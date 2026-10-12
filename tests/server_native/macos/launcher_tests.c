#define _DARWIN_C_SOURCE 1
#include <sys/types.h>
#include "launcher_test.h"
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <poll.h>
#include <signal.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/param.h>
#include <sys/sysctl.h>
#include <sys/wait.h>
#include <unistd.h>

struct st_native_pending { st_native_pending_facts facts; };
static st_native_pending pending;
static unsigned release_calls, steps, failed_step;
static bool release_unresolved, missing_saved, missing_groups, wrong_saved, wrong_start;
struct st_native_execution_binding { st_native_pending_facts facts; };
static bool execution_available;
static bool execution_replaced;
static unsigned execution_releases;
static unsigned report_fault;

st_native_result st_native_pending_copy_account_groups(
    const st_native_pending *p, uint32_t *groups, uint32_t capacity, uint32_t *count) {
    if (p != &pending || groups == NULL || count == NULL) return 9;
    gid_t native[256];
    int length = getgroups(256, native);
    if (length < 1 || (uint32_t)length > capacity) return 30;
    for (int i = 0; i < length; ++i) groups[i] = native[i];
    *count = (uint32_t)length;
    return 0;
}
st_native_result st_native_pending_retain_execution(
    const st_native_pending *p, st_native_execution_binding **out) {
    if (!execution_available) return 28;
    if (p != &pending || out == NULL || *out != NULL) return 9;
    *out = calloc(1, sizeof(**out));
    if (*out == NULL) return 2;
    (*out)->facts = p->facts;
    return 0;
}
st_native_result st_native_execution_binding_recheck(
    const st_native_execution_binding *binding, const st_native_pending_facts *facts) {
    if (binding == NULL || facts == NULL) return 9;
    return execution_replaced || memcmp(&binding->facts, facts, sizeof(*facts)) != 0 ? 28 : 0;
}
st_native_result st_native_execution_binding_copy_receiver_groups(
    const st_native_execution_binding *binding, const st_native_pending_facts *facts,
    uint32_t *groups, uint32_t capacity, uint32_t *count) {
    st_native_result result = st_native_execution_binding_recheck(binding, facts);
    if (result != 0) return result;
    gid_t native[256];
    int length = getgroups(256, native);
    if (length < 1 || (uint32_t)length > capacity) return 30;
    for (int i = 0; i < length; ++i) groups[i] = native[i];
    *count = (uint32_t)length;
    if (report_fault == 1) groups[0] = 0;
    if (report_fault == 2) { groups[0] = facts->gid + 1; *count = 1; }
    if (report_fault == 3) *count = 257;
    return 0;
}
void st_native_execution_binding_release(st_native_execution_binding **owned) {
    if (owned == NULL || *owned == NULL) return;
    ++execution_releases;
    free(*owned);
    *owned = NULL;
}
static bool cleanup_unresolved;
static bool forced_cleanup_uncertainty;
static volatile sig_atomic_t cancellation_probe;
static int retirement_fd = -1;
static volatile sig_atomic_t first_signal;
static pid_t owned_child = -1;

static void cancel(int number) {
    if (first_signal == 0) {
        first_signal = number;
        if (cancellation_probe) (void)write(STDOUT_FILENO, "s", 1);
    }
}
static bool install_cancellation(void) {
    struct sigaction action = {.sa_handler = cancel};
    return sigemptyset(&action.sa_mask) == 0 &&
        sigaction(SIGTERM, &action, NULL) == 0 && sigaction(SIGINT, &action, NULL) == 0;
}
static pid_t owned_fork(void) {
    sigset_t blocked, old;
    if (sigemptyset(&blocked) != 0 || sigaddset(&blocked, SIGTERM) != 0 ||
        sigaddset(&blocked, SIGINT) != 0 || sigprocmask(SIG_BLOCK, &blocked, &old) != 0) return -1;
    if (first_signal != 0) {
        (void)sigprocmask(SIG_SETMASK, &old, NULL);
        return -1;
    }
    pid_t pid = fork();
    if (pid == 0) {
        struct sigaction action = {.sa_handler = SIG_DFL};
        if (sigemptyset(&action.sa_mask) != 0 || sigaction(SIGTERM, &action, NULL) != 0 ||
            sigaction(SIGINT, &action, NULL) != 0 || sigprocmask(SIG_SETMASK, &old, NULL) != 0) _exit(2);
    } else {
        if (pid > 0) owned_child = pid;
        if (sigprocmask(SIG_SETMASK, &old, NULL) != 0) cleanup_unresolved = true;
    }
    return pid;
}

st_native_result st_native_pending_copy_facts(const st_native_pending *p, st_native_pending_facts *out) {
    if (p == NULL || out == NULL) return 9;
    *out = p->facts;
    return 0;
}
void st_native_pending_release(st_native_pending **owned) {
    ++release_calls;
    if (!release_unresolved) *owned = NULL;
}

static st_native_result fake_observe(uint32_t pid, st_native_privilege_observation *out) {
    (void)pid;
    memset(out, 0, sizeof(*out));
    out->process = pending.facts.original_process;
    out->kernel_process = pending.facts.original_process;
    out->real_uid = out->effective_uid = out->saved_uid = pending.facts.uid;
    out->real_gid = out->effective_gid = out->saved_gid = pending.facts.gid;
    out->saved_ids_observed = missing_saved ? 0 : 1;
    out->groups_observed = missing_groups ? 0 : 1;
    out->group_count = 1;
    out->groups[0] = pending.facts.gid;
    if (wrong_saved) out->saved_uid = 0;
    if (wrong_start) ++out->process.start_microseconds;
    return 0;
}
static int fake_groups(int count, const gid_t *groups) {
    gid_t native[256];
    int length = getgroups(256, native);
    if (count != length || groups == NULL || memcmp(groups, native, (size_t)count * sizeof(gid_t)) != 0) return -1;
    return ++steps == failed_step ? -1 : 0;
}
static int fake_gid(gid_t gid) {
    if (gid != pending.facts.gid || steps != 1) return -1;
    return ++steps == failed_step ? -1 : 0;
}
static int fake_uid(uid_t uid) {
    if (uid != pending.facts.uid || steps != 2) return -1;
    return ++steps == failed_step ? -1 : 0;
}
static bool reset(void) {
    memset(&pending, 0, sizeof(pending));
    st_native_pending_facts *f = &pending.facts;
    f->abi_version = 2; f->struct_size = sizeof(*f);
    f->uid = (uint32_t)getuid(); f->gid = (uint32_t)getgid(); f->build = 3;
    f->registration_generation = 1; f->lifecycle_generation = 2;
    f->data_generation = 3; f->installation_id = 4;
    memset(f->server_id, 1, sizeof(f->server_id));
    memset(f->account_binding, 2, sizeof(f->account_binding));
    memset(f->installation_binding, 3, sizeof(f->installation_binding));
    memset(f->server_artifact, 4, sizeof(f->server_artifact));
    memset(f->manifest, 5, sizeof(f->manifest));
    memset(f->credential_binding, 6, sizeof(f->credential_binding));
    memset(f->startup_nonce, 7, sizeof(f->startup_nonce));
    st_native_privilege_observation native;
    if (st_native_observe_process((uint32_t)getpid(), &native) != 0) return false;
    f->original_process = native.process;
    f->retriever_process = (st_native_process_identity){ (uint32_t)getpid() + 1, 1, 1 };
    release_calls = steps = failed_step = 0;
    release_unresolved = missing_saved = missing_groups = wrong_saved = wrong_start = false;
    execution_available = execution_replaced = false;
    execution_releases = report_fault = 0;
    st_launcher_test_calls calls = {fake_groups, fake_gid, fake_uid, fake_observe};
    st_launcher_test_inject(&calls);
    return true;
}

static bool drop_order(void) {
    return st_native_drop_account_privileges(&pending) == 0 && steps == 3;
}
static bool primary_group_only(void) {
    st_native_privilege_observation observed;
    if (fake_observe(pending.facts.original_process.pid, &observed) != 0) return false;
    return st_native_validate_privileges(&pending.facts, &observed) == 0;
}
static bool ordinary_groups(void) {
    st_native_privilege_observation observed;
    if (fake_observe(pending.facts.original_process.pid, &observed) != 0) return false;
    observed.groups[1] = pending.facts.gid + 1;
    observed.group_count = 2;
    return st_native_validate_privileges(&pending.facts, &observed) == 0;
}
static bool root_group(void) {
    st_native_privilege_observation observed;
    if (fake_observe(pending.facts.original_process.pid, &observed) != 0) return false;
    observed.groups[1] = 0;
    observed.group_count = 2;
    return st_native_validate_privileges(&pending.facts, &observed) == 27;
}
static bool wrong_primary_group(void) {
    st_native_privilege_observation observed;
    if (fake_observe(pending.facts.original_process.pid, &observed) != 0) return false;
    observed.groups[0] = pending.facts.gid + 1;
    return st_native_validate_privileges(&pending.facts, &observed) == 27;
}
static bool drop_failure(void) {
    for (unsigned i = 1; i <= 3; ++i) {
        steps = 0; failed_step = i;
        if (st_native_drop_account_privileges(&pending) != 27 || steps != i) return false;
    }
    return true;
}
static bool saved_missing(void) {
    missing_saved = true;
    return st_native_drop_account_privileges(&pending) == 2;
}
static bool groups_missing(void) {
    missing_groups = true;
    return st_native_drop_account_privileges(&pending) == 2;
}
static bool saved_root(void) {
    wrong_saved = true;
    return st_native_drop_account_privileges(&pending) == 27;
}
static bool pid_reused(void) {
    wrong_start = true;
    return st_native_drop_account_privileges(&pending) == 26;
}
static bool stale_before_drop(void) {
    ++pending.facts.original_process.start_seconds;
    return st_native_drop_account_privileges(&pending) == 26 && steps == 0;
}
static bool consumed_pending(void) {
    st_native_pending *owned = &pending;
    return st_native_launch_one_shot(&owned, st_native_launcher_default_adapters()) == 28 &&
           owned == NULL && release_calls == 1 && steps == 0;
}
static bool unresolved_pending(void) {
    release_unresolved = true;
    st_native_pending *owned = &pending;
    return st_native_launch_one_shot(&owned, st_native_launcher_default_adapters()) == 33 &&
           owned == NULL && release_calls == 1;
}
static bool no_wire_proof(void) {
    st_native_post_exec_proof *proof = NULL;
    st_native_result result = st_native_observe_post_exec(&pending, &proof);
    st_native_post_exec_proof_release(&proof);
    return result != 0 && proof == NULL;
}
static bool retained_post_exec(void) {
    execution_available = true;
    st_native_post_exec_proof *proof = NULL;
    st_native_result result = st_native_observe_post_exec(&pending, &proof);
    bool issued = result == 0 && proof != NULL;
    st_native_post_exec_proof_release(&proof);
    return issued && proof == NULL && execution_releases == 1;
}
static bool privilege_snapshot_refusals(void) {
    st_native_privilege_observation observed;
    if (fake_observe(pending.facts.original_process.pid, &observed) != 0) return false;
    ++observed.kernel_process.start_seconds;
    if (st_native_validate_privileges(&pending.facts, &observed) != 26) return false;
    observed.kernel_process = observed.process;
    uint32_t *ids[] = {&observed.real_uid, &observed.effective_uid, &observed.saved_uid,
        &observed.real_gid, &observed.effective_gid, &observed.saved_gid};
    for (size_t i = 0; i < sizeof(ids) / sizeof(ids[0]); ++i) {
        uint32_t original = *ids[i];
        *ids[i] = 0;
        st_native_result result = st_native_validate_privileges(&pending.facts, &observed);
        *ids[i] = original;
        if (result != 27) return false;
    }
    observed.group_count = 0;
    return st_native_validate_privileges(&pending.facts, &observed) == 27;
}
static bool retained_proof_rechecks(void) {
    execution_available = true;
    st_native_post_exec_proof *proof = NULL;
    if (st_native_observe_post_exec(&pending, &proof) != 0) return false;
    bool valid = st_native_post_exec_proof_recheck(proof, &pending) == 0;
    uint32_t report[256], count = 0;
    gid_t groups[256];
    int length = getgroups(256, groups);
    valid = valid && length > 0 &&
        st_native_post_exec_proof_copy_groups(proof, report, 256, &count) == 0 &&
        count == (uint32_t)length;
    for (uint32_t i = 0; valid && i < count; ++i) valid = report[i] == (uint32_t)groups[i];
    ++pending.facts.lifecycle_generation;
    valid = valid && st_native_post_exec_proof_recheck(proof, &pending) == 26;
    --pending.facts.lifecycle_generation;
    execution_replaced = true;
    valid = valid && st_native_post_exec_proof_recheck(proof, &pending) == 28;
    st_native_post_exec_proof_release(&proof);
    return valid && proof == NULL && execution_releases == 1;
}
static bool inconsistent_receiver_report(void) {
    execution_available = true;
    for (report_fault = 1; report_fault <= 3; ++report_fault) {
        st_native_post_exec_proof *proof = NULL;
        st_native_result result = st_native_observe_post_exec(&pending, &proof);
        bool valid = result == 27 && proof == NULL && execution_releases == report_fault;
        st_native_post_exec_proof_release(&proof);
        if (!valid) return false;
    }
    return true;
}
static bool native_post_exec_authority(void) {
    return reset() && no_wire_proof() &&
        reset() && retained_post_exec() &&
        reset() && privilege_snapshot_refusals() &&
        reset() && root_group() &&
        reset() && retained_proof_rechecks() &&
        reset() && inconsistent_receiver_report();
}
static bool invalid_abi(void) {
    pending.facts.abi_version = 1;
    return st_native_drop_account_privileges(&pending) == 9 && steps == 0;
}

static bool wait_child(pid_t pid) {
    if (pid <= 0 || pid != owned_child) { cleanup_unresolved = true; return false; }
    bool signaled = false;
    for (unsigned i = 0; i < 3000; ++i) {
        if (first_signal != 0 && !signaled) {
            if (kill(pid, SIGTERM) != 0 && errno != ESRCH) cleanup_unresolved = true;
            signaled = true;
        }
        int status = 0;
        pid_t result = waitpid(pid, &status, WNOHANG);
        if (result == pid) {
            owned_child = -1;
            if (forced_cleanup_uncertainty) cleanup_unresolved = true;
            return first_signal != 0 || (WIFEXITED(status) && WEXITSTATUS(status) == 0);
        }
        if (result < 0 && errno != EINTR) { cleanup_unresolved = true; return false; }
        usleep(1000);
    }
    cleanup_unresolved = true;
    return false;
}

static const char *self_path;
static bool exec_identity(void) {
    int channel[2];
    if (pipe(channel) != 0) return false;
    pid_t pid = owned_fork();
    if (pid == 0) {
        alarm(2);
        close(channel[0]);
        st_native_privilege_observation before;
        if (st_native_observe_process((uint32_t)getpid(), &before) != 0 ||
            write(channel[1], &before.process, sizeof(before.process)) != sizeof(before.process)) _exit(2);
        char fd[16];
        (void)snprintf(fd, sizeof(fd), "%d", channel[1]);
        char *const argv[] = {(char *)self_path, "--exec-child", fd, NULL};
        char *const env[] = {"PATH=/usr/bin:/bin", cancellation_probe ? "ST_LAUNCHER_CANCEL_PROBE=1" : NULL, NULL};
        execve(self_path, argv, env);
        _exit(2);
    }
    close(channel[1]);
    if (retirement_fd >= 0) { close(retirement_fd); retirement_fd = -1; }
    if (pid < 0) { close(channel[0]); return false; }
    st_native_process_identity observations[2];
    size_t used = 0;
    while (used < sizeof(observations)) {
        struct pollfd pollfd = {channel[0], POLLIN, 0};
        if (poll(&pollfd, 1, 2000) <= 0) break;
        ssize_t count = read(channel[0], ((uint8_t *)observations) + used, sizeof(observations) - used);
        if (count <= 0) break;
        used += (size_t)count;
    }
    close(channel[0]);
    if (cancellation_probe) { putchar('r'); fflush(stdout); }
    bool retired = wait_child(pid);
    if (!retired) return false;
    return used == sizeof(observations) && memcmp(&observations[0], &observations[1], sizeof(observations[0])) == 0 && observations[0].pid == (uint32_t)pid;
}

static bool descriptor_exec(void) {
    int descriptors[2];
    if (pipe(descriptors) != 0) return false;
    if (fcntl(descriptors[0], F_SETFD, FD_CLOEXEC) != 0 || fcntl(descriptors[1], F_SETFD, FD_CLOEXEC) != 0) {
        close(descriptors[0]); close(descriptors[1]); return false;
    }
    pid_t pid = owned_fork();
    if (pid == 0) {
        alarm(2);
        char first[16], second[16];
        (void)snprintf(first, sizeof(first), "%d", descriptors[0]);
        (void)snprintf(second, sizeof(second), "%d", descriptors[1]);
        char *const argv[] = {(char *)self_path, "--descriptor-child", first, second, NULL};
        char *const env[] = {"PATH=/usr/bin:/bin", cancellation_probe ? "ST_LAUNCHER_CANCEL_PROBE=1" : NULL, NULL};
        execve(self_path, argv, env);
        _exit(2);
    }
    close(descriptors[0]); close(descriptors[1]);
    if (retirement_fd >= 0) { close(retirement_fd); retirement_fd = -1; }
    if (cancellation_probe) { putchar('r'); fflush(stdout); }
    return pid > 0 && wait_child(pid);
}

static bool remote_credentials(void) {
    int lease[2];
    if (pipe(lease) != 0) return false;
    pid_t pid = owned_fork();
    if (pid == 0) {
        alarm(3); close(lease[1]);
        char byte; (void)read(lease[0], &byte, 1); _exit(0);
    }
    close(lease[0]);
    st_native_privilege_observation observed;
    gid_t groups[256];
    int count = getgroups(256, groups);
    bool valid = pid > 0 && count > 0 &&
        st_native_observe_process((uint32_t)pid, &observed) == 0 &&
        observed.groups_observed == 1 && observed.saved_ids_observed == 1 &&
        observed.group_count >= 1 && observed.group_count <= (uint32_t)count &&
        observed.groups[0] == getgid() &&
        memcmp(&observed.process, &observed.kernel_process, sizeof(observed.process)) == 0 &&
        observed.real_uid == getuid() &&
        observed.effective_uid == geteuid() && observed.saved_uid == geteuid() &&
        observed.real_gid == getgid() && observed.effective_gid == getegid() &&
        observed.saved_gid == getegid();
    close(lease[1]);
    bool retired = pid > 0 && wait_child(pid);
    return valid && retired;
}

typedef struct { const char *name; bool (*run)(void); } test_case;
static const test_case cases[] = {
    {"drop-groups-gid-uid-in-order", drop_order},
    {"darwin-primary-group-only-passes", primary_group_only},
    {"darwin-ordinary-groups-pass", ordinary_groups},
    {"darwin-wrong-primary-group-rejects", wrong_primary_group},
    {"privilege-drop-stops-at-first-failure", drop_failure},
    {"missing-native-saved-id-proof-is-unavailable-not-success", saved_missing},
    {"missing-remote-group-proof-is-unavailable-not-success", groups_missing},
    {"saved-root-identity-rejects-drop", saved_root},
    {"pid-reuse-rejects-privilege-proof", pid_reused},
    {"stale-original-process-rejects-before-drop", stale_before_drop},
    {"pending-consumed-before-unavailable-launch", consumed_pending},
    {"unresolved-release-overrides-launch-failure", unresolved_pending},
    {"native-proof-not-readiness-bits-authorizes-release", native_post_exec_authority},
    {"unknown-abi-rejects-before-effects", invalid_abi},
    {"tracked-launcher-exec-preserves-original-pid-identity", exec_identity},
    {"terminal-child-inherits-no-exchange-descriptor", descriptor_exec},
};

int main(int argc, char **argv) {
    struct rlimit core = {0, 0};
    sigset_t mask;
    struct sigaction lifetime = {.sa_handler = SIG_DFL};
    if (sigemptyset(&mask) != 0 || sigprocmask(SIG_SETMASK, &mask, NULL) != 0 ||
        sigemptyset(&lifetime.sa_mask) != 0 || sigaction(SIGALRM, &lifetime, NULL) != 0 ||
        setrlimit(RLIMIT_CORE, &core) != 0 || getuid() == 0 || geteuid() == 0) return 2;
    if (argc == 3 && strcmp(argv[1], "--exec-child") == 0) {
        int fd = atoi(argv[2]);
        if (fd < 3 || fcntl(fd, F_SETFD, FD_CLOEXEC) != 0) return 2;
        st_native_privilege_observation after;
        if (st_native_observe_process((uint32_t)getpid(), &after) != 0) return 2;
        bool wrote = write(fd, &after.process, sizeof(after.process)) == sizeof(after.process);
        if (getenv("ST_LAUNCHER_CANCEL_PROBE") != NULL) pause();
        return wrote ? 0 : 2;
    }
    if (argc == 4 && strcmp(argv[1], "--descriptor-child") == 0) {
        for (int i = 2; i < 4; ++i) {
            int fd = atoi(argv[i]);
            if (fd < 3 || fcntl(fd, F_GETFD) != -1 || errno != EBADF) return 1;
        }
        if (getenv("ST_LAUNCHER_CANCEL_PROBE") != NULL) pause();
        return 0;
    }
    if (argc == 4 && strcmp(argv[1], "--cancellation-probe") == 0) {
        cancellation_probe = true;
        forced_cleanup_uncertainty = strcmp(argv[2], "unresolved") == 0;
        retirement_fd = atoi(argv[3]);
        self_path = argv[0];
        if (retirement_fd < 3 || !install_cancellation() || !reset()) return 2;
        bool passed = strcmp(argv[2], "descriptor") == 0 ? descriptor_exec() : exec_identity();
        return cleanup_unresolved ? 125 : first_signal != 0 ? 128 + first_signal : passed ? 0 : 1;
    }
    if (argc == 2 && strcmp(argv[1], "--group-snapshot-probe") == 0) {
        struct kinfo_proc kernel;
        memset(&kernel, 0, sizeof(kernel));
        size_t length = sizeof(kernel);
        int mib[] = {CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()};
        gid_t groups[256];
        int count = getgroups(256, groups);
        if (count < 1 || count > 256 ||
            sysctl(mib, 4, &kernel, &length, NULL, 0) != 0 || length != sizeof(kernel) ||
            kernel.kp_eproc.e_ucred.cr_ngroups < 1 || kernel.kp_eproc.e_ucred.cr_ngroups > NGROUPS) return 2;
        unsigned kernel_count = (unsigned)kernel.kp_eproc.e_ucred.cr_ngroups;
        printf("{\"schema\":1,\"scope\":\"darwin-group-proof\",\"case\":\"kernel-snapshot\",\"result\":\"%s\",\"kernel_count\":%u,\"native_count\":%u}\n",
            kernel_count < (unsigned)count ? "incomplete" : "not-demonstrated", kernel_count, (unsigned)count);
        return 0;
    }
    if (argc == 2 && strcmp(argv[1], "--remote-credentials") == 0) {
        if (!install_cancellation()) return 2;
        bool valid = remote_credentials();
        printf("{\"schema\":1,\"scope\":\"launcher-native-mechanics\",\"case\":\"remote-kernel-privilege-observation\",\"result\":\"%s\"}\n", valid ? "pass" : "red");
        return cleanup_unresolved ? 125 : valid ? 0 : 1;
    }
    if (argc != 1 || !install_cancellation()) return 2;
    self_path = argv[0];
    int status = 0;
    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); ++i) {
        bool passed = reset() && cases[i].run();
        printf("{\"schema\":1,\"scope\":\"launcher-native-mechanics\",\"case\":\"%s\",\"result\":\"%s\"}\n", cases[i].name, cleanup_unresolved ? "cleanup-unresolved" : passed ? "pass" : "red");
        fflush(stdout);
        if (!passed) status = 1;
        if (first_signal != 0 || cleanup_unresolved) break;
    }
    return cleanup_unresolved ? 125 : first_signal != 0 ? 128 + first_signal : status;
}
