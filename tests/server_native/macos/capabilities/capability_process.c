#include "capability_process.h"
#include <errno.h>
#include <libproc.h>
#include <mach/message.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

/* XNU's proc_info_private.h marks this 56-byte structure as API. The macOS
 * SDK omits the declaration, not the flavor. Keep the ABI exact and fail closed
 * when the running kernel does not return the complete record. */
typedef struct {
    uint8_t uuid[16];
    uint64_t uniqueid, parent_uniqueid;
    int32_t pidversion, original_parent_pidversion;
    uint64_t reserved[2];
} native_identity;
_Static_assert(sizeof(native_identity) == 56, "Darwin process identity ABI");

int st_capability_process_observe(pid_t pid, st_capability_process_identity *out) {
    native_identity info;
    if (!out || pid <= 0 || proc_pidinfo(pid, 17, 1, &info, sizeof(info)) != sizeof(info) ||
        !info.uniqueid || info.pidversion <= 0) return 0;
    *out = (st_capability_process_identity){pid, (uint32_t)info.pidversion,
        info.uniqueid & ~(UINT64_C(1) << 63), info.parent_uniqueid & ~(UINT64_C(1) << 63)};
    return 1;
}
int st_capability_process_sentinel(uint64_t *uniqueid) {
    int gate[2];
    if (!uniqueid || pipe(gate) != 0) return 0;
    pid_t sentinel = fork();
    if (!sentinel) {
        close(gate[1]); char byte;
        struct sigaction guard = {0}; guard.sa_handler = SIG_DFL;
        sigemptyset(&guard.sa_mask);
        if (sigaction(SIGALRM, &guard, NULL) != 0) _exit(1);
        alarm(1);
        while (read(gate[0], &byte, 1) < 0 && errno == EINTR) {}
        close(gate[0]); _exit(0);
    }
    close(gate[0]);
    st_capability_process_identity identity;
    int observed = sentinel > 0 && st_capability_process_observe(sentinel, &identity);
    close(gate[1]);
    int status = 0; pid_t waited = 0;
    if (sentinel <= 0) return 0;
    uint64_t deadline = st_capability_monotonic_ns() + UINT64_C(250000000);
    do {
        waited = waitpid(sentinel, &status, WNOHANG);
        if (waited == sentinel || (waited < 0 && errno != EINTR)) break;
        struct timespec pause = {0, 1000000}; nanosleep(&pause, NULL);
    } while (st_capability_monotonic_ns() < deadline);
    if (waited != sentinel && observed) {
        audit_token_t token = {{0}}; token.val[5] = (uint32_t)sentinel; token.val[7] = identity.pidversion;
        (void)proc_signal_with_audittoken(&token, SIGKILL);
        deadline = st_capability_monotonic_ns() + UINT64_C(250000000);
        do {
            waited = waitpid(sentinel, &status, WNOHANG);
            if (waited == sentinel || (waited < 0 && errno != EINTR)) break;
            struct timespec pause = {0, 1000000}; nanosleep(&pause, NULL);
        } while (st_capability_monotonic_ns() < deadline);
        return 0; /* A nontrivial sentinel retirement cannot supply U_hi. */
    }
    if (!observed || waited != sentinel || !WIFEXITED(status) || WEXITSTATUS(status)) return 0;
    *uniqueid = identity.uniqueid;
    return 1;
}
static int list(void *unused, st_capability_process_identity *out, size_t capacity, size_t *count) {
    (void)unused;
    if (capacity > 16384) return 0;
    pid_t *pids = calloc(capacity + 1, sizeof(*pids));
    if (!pids) return 0;
    int listed = proc_listallpids(pids, (int)((capacity + 1) * sizeof(*pids)));
    int okay = listed > 0 && (size_t)listed <= capacity;
    *count = 0;
    for (int i = 0; okay && i < listed; ++i) {
        if (pids[i] <= 0) continue;
        struct proc_bsdshortinfo state;
        if (proc_pidinfo(pids[i], PROC_PIDT_SHORTBSDINFO, 1, &state, sizeof(state)) == sizeof(state) &&
            state.pbsi_status == 5) continue; /* SZOMB is not a live process. */
        if (!st_capability_process_observe(pids[i], &out[*count])) {
            /* An exited PID is absent. A live but unreadable process cannot be
             * assumed foreign, especially a root-owned sudo descendant. */
            if (kill(pids[i], 0) != 0 && errno == ESRCH) continue;
            okay = 0; break;
        }
        ++*count;
    }
    free(pids); return okay;
}
static int signal_member(void *unused, const st_capability_process_identity *identity) {
    (void)unused;
    st_capability_process_identity current;
    if (!st_capability_process_observe(identity->pid, &current))
        return kill(identity->pid, 0) != 0 && errno == ESRCH;
    if (current.uniqueid != identity->uniqueid || current.pidversion != identity->pidversion)
        return 0;
    audit_token_t token = {{0}};
    token.val[5] = (uint32_t)identity->pid;
    token.val[7] = identity->pidversion;
    /* The kernel compares pidversion atomically with signalling. The preceding
     * uniqueid check alone would leave a read/kill PID-reuse race. */
    int result = proc_signal_with_audittoken(&token, SIGTERM);
    return result == 0 || result == ESRCH;
}
static uint64_t now(void *unused) { (void)unused; return st_capability_monotonic_ns(); }
static void pause_scan(void *unused) {
    (void)unused;
    struct timespec duration = {0, 2000000}; nanosleep(&duration, NULL);
}
st_native_result st_capability_tree_retire_native(uint64_t root) {
    uint64_t hi;
    if (!st_capability_process_sentinel(&hi)) return ST_PROBE_CLEANUP_UNRESOLVED;
    st_capability_tree_peer peer = {NULL, list, signal_member, now, pause_scan};
    return st_capability_tree_retire(&peer, root, hi,
        st_capability_monotonic_ns() + UINT64_C(250000000));
}
