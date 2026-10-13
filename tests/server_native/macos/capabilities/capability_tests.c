#define _POSIX_C_SOURCE 200809L
#include "capability_internal.h"
#include "capability_environment.h"
#include <assert.h>
#include <errno.h>
#include <signal.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

static unsigned test_count;
static unsigned mode;
static st_capability_observation emitted;
static unsigned emit_count;
static pid_t last_child;
static int escaped_control[2], escaped_status[2];
static unsigned finish_count, spawn_count, native_count;

static void emit(void *context, const st_capability_observation *observation) {
    assert(context == &emitted);
    emitted = *observation;
    ++emit_count;
}

/* Test-only link adapter. It owns real pipe/process behavior without calling
 * privacy APIs, launchd, sudo, Keychain, microphone, capture, or signing tools. */
st_native_result st_capability_spawn(st_capability_case capability, st_probe_context context,
    uint64_t deadline, pid_t *owned_child, int *owned_output) {
    (void)deadline;
    ++spawn_count;
    if (mode == 7) return ST_PROBE_UNAVAILABLE;
    int descriptors[2];
    assert(pipe(descriptors) == 0);
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        close(descriptors[0]);
        assert(setpgid(0, 0) == 0);
        st_capability_observation record = {1, capability, context, ST_OUTCOME_PASSED,
            ST_RESPONSIBLE_UNKNOWN, ST_PREFLIGHT_ALLOWED, ST_EFFECT_VERIFIED, ST_CLEANUP_NOT_NEEDED};
        uint8_t bytes[32];
        assert(st_capability_encode(&record, bytes) == ST_PROBE_OK);
        if (mode == 10) {
            close(escaped_control[1]); close(escaped_status[0]);
            pid_t escaped = fork();
            assert(escaped >= 0);
            if (escaped == 0) {
                close(descriptors[1]);
                assert(setsid() > 0);
                assert(write(escaped_status[1], "r", 1) == 1);
                char byte;
                while (read(escaped_control[0], &byte, 1) < 0 && errno == EINTR) {}
                close(escaped_status[1]);
                _exit(0);
            }
            close(escaped_control[0]); close(escaped_status[1]);
            (void)write(descriptors[1], bytes, 32);
        } else if (mode == 11) {
            close(descriptors[1]);
            _exit(1);
        } else if (mode == 9) {
            signal(SIGINT, SIG_DFL);
            raise(SIGINT);
            _exit(1);
        } else if (mode == 1) {
            struct timespec pause = {0, 40000000};
            for (size_t i = 0; i < sizeof(bytes); ++i) {
                if (write(descriptors[1], bytes + i, 1) != 1) _exit(1);
                nanosleep(&pause, NULL);
            }
        } else if (mode == 2) {
            const char *raw = "PATH=/Users/private/bin native error text";
            (void)write(descriptors[1], raw, strlen(raw));
        } else if (mode == 3) {
            (void)write(descriptors[1], bytes, 31);
        } else if (mode == 4) {
            bytes[23] = ST_PREFLIGHT_ALLOWED;
            bytes[27] = ST_EFFECT_UNOBSERVED;
            (void)write(descriptors[1], bytes, 32);
        } else if (mode == 5) {
            bytes[11] = context == ST_CONTEXT_GUI ? ST_CONTEXT_DAEMON : ST_CONTEXT_GUI;
            (void)write(descriptors[1], bytes, 32);
        } else if (mode == 6) {
            (void)write(descriptors[1], bytes, 32);
            (void)write(descriptors[1], "x", 1);
        } else if (mode == 8) {
            record.outcome = ST_OUTCOME_CLEANUP_UNRESOLVED;
            record.cleanup_class = ST_CLEANUP_UNRESOLVED;
            assert(st_capability_encode(&record, bytes) == 0);
            (void)write(descriptors[1], bytes, 32);
        } else {
            (void)write(descriptors[1], bytes, 32);
        }
        close(descriptors[1]);
        _exit(0);
    }
    close(descriptors[1]);
    if (mode == 11) {
        int config[2]; assert(pipe(config) == 0);
        close(config[0]);
        char byte = 0;
        assert(!st_capability_transfer(config[1], &byte, 1, 1, deadline));
        close(config[1]);
    }
    /* Establish the same retained process group the production spawn provides. */
    if (setpgid(child, child) != 0) assert(errno == EACCES || errno == ESRCH);
    *owned_child = child;
    *owned_output = descriptors[0];
    last_child = child;
    return ST_PROBE_OK;
}

st_native_result st_capability_finish_spawn(pid_t child, int retired, int root_reaped) {
    assert(child == last_child && retired && root_reaped);
    ++finish_count;
    return mode == 10 ? ST_PROBE_CLEANUP_UNRESOLVED : ST_PROBE_OK;
}

st_native_result st_capability_native_worker(st_capability_case capability, st_probe_context context,
    uint64_t deadline, st_capability_observation *out) {
    (void)deadline; ++native_count;
    if (mode == 7) return ST_PROBE_UNAVAILABLE;
    *out = (st_capability_observation){1, capability, context, ST_OUTCOME_PASSED,
        ST_RESPONSIBLE_UNKNOWN, ST_PREFLIGHT_ALLOWED, ST_EFFECT_VERIFIED, ST_CLEANUP_NOT_NEEDED};
    return ST_PROBE_OK;
}

static st_native_result run(unsigned selected_mode, uint32_t capability, uint32_t context, uint64_t budget) {
    mode = selected_mode;
    emit_count = 0;
    st_native_result result = st_capability_run(capability, context,
        st_capability_monotonic_ns() + budget, emit, &emitted);
    assert(emit_count == 1);
    if (mode != 7 && capability != ST_CAP_LOGIN_SHELL) {
        int status;
        assert(waitpid(last_child, &status, WNOHANG) == -1 && errno == ECHILD);
    }
    return result;
}

#define TEST(name) static void name(void)
#define RUN(name) do { name(); ++test_count; printf("passed %s\n", #name); } while (0)

TEST(probe_timeout_does_not_reset_after_partial_output) {
    uint64_t before = st_capability_monotonic_ns();
    assert(run(1, ST_CAP_MICROPHONE, ST_CONTEXT_GUI, UINT64_C(100000000)) == ST_PROBE_TIMED_OUT);
    assert(st_capability_monotonic_ns() - before < UINT64_C(500000000));
    assert(emitted.outcome == ST_OUTCOME_TIMED_OUT);
}
TEST(probe_output_rejects_paths_environment_values_and_native_errors) {
    assert(run(2, ST_CAP_MICROPHONE, ST_CONTEXT_GUI, UINT64_C(1000000000)) == ST_PROBE_INVALID);
    assert(emitted.outcome == ST_OUTCOME_FAILED);
    uint8_t encoded[32];
    assert(st_capability_encode(&emitted, encoded) == 0);
    for (size_t i = 0; i < sizeof(encoded); ++i) assert(encoded[i] < 14);
}
TEST(preflight_success_without_effect_is_not_passed) {
    assert(run(4, ST_CAP_MICROPHONE, ST_CONTEXT_GUI, UINT64_C(1000000000)) == ST_PROBE_INVALID);
    assert(emitted.outcome == ST_OUTCOME_FAILED);
}
TEST(gui_and_daemon_results_are_never_coalesced) {
    assert(run(0, ST_CAP_MICROPHONE, ST_CONTEXT_GUI, UINT64_C(1000000000)) == 0);
    assert(emitted.context == ST_CONTEXT_GUI);
    assert(run(0, ST_CAP_MICROPHONE, ST_CONTEXT_DAEMON, UINT64_C(1000000000)) == 0);
    assert(emitted.context == ST_CONTEXT_DAEMON);
    assert(run(5, ST_CAP_MICROPHONE, ST_CONTEXT_GUI, UINT64_C(1000000000)) == ST_PROBE_INVALID);
    assert(emitted.context == ST_CONTEXT_GUI);
}
TEST(missing_volume_and_agent_are_unrun_or_unavailable) {
    for (uint32_t capability = 1; capability <= 13; ++capability) {
        assert(run(7, capability, ST_CONTEXT_GUI, UINT64_C(1000000000)) == ST_PROBE_UNAVAILABLE);
        assert(emitted.outcome == ST_OUTCOME_UNAVAILABLE);
    }
}
TEST(capture_pixels_and_microphone_samples_are_discarded) {
    uint8_t pixels[17], samples[13];
    memset(pixels, 0xab, sizeof(pixels));
    memset(samples, 0xcd, sizeof(samples));
    st_capability_discard_owned(pixels, sizeof(pixels));
    st_capability_discard_owned(samples, sizeof(samples));
    for (size_t i = 0; i < sizeof(pixels); ++i) assert(pixels[i] == 0);
    for (size_t i = 0; i < sizeof(samples); ++i) assert(samples[i] == 0);
}
TEST(touch_id_case_executes_only_usr_bin_true) {
    const char *const *arguments = st_capability_sudo_argv();
    assert(strcmp(arguments[0], "/usr/bin/sudo") == 0);
    assert(strcmp(arguments[1], "/usr/bin/true") == 0);
    assert(arguments[2] == NULL);
}
TEST(completed_fixed_tree_is_not_unconditionally_cleanup_unresolved) {
    assert(run(0, ST_CAP_TOUCH_ID_SUDO, ST_CONTEXT_GUI, UINT64_C(1000000000)) == ST_PROBE_OK);
    assert(emitted.outcome == ST_OUTCOME_PASSED);
}
TEST(short_and_extra_output_reject_before_success) {
    assert(run(3, ST_CAP_MICROPHONE, ST_CONTEXT_GUI, UINT64_C(1000000000)) == ST_PROBE_INVALID);
    assert(run(6, ST_CAP_MICROPHONE, ST_CONTEXT_GUI, UINT64_C(1000000000)) == ST_PROBE_INVALID);
}
TEST(cleanup_uncertainty_overrides_effect_success) {
    assert(run(8, ST_CAP_CLIPBOARD, ST_CONTEXT_GUI, UINT64_C(1000000000)) == ST_PROBE_CLEANUP_UNRESOLVED);
    assert(emitted.outcome == ST_OUTCOME_CLEANUP_UNRESOLVED);
}
TEST(resource_worker_timeout_does_not_claim_cleanup) {
    assert(run(1, ST_CAP_LOGIN_KEYCHAIN, ST_CONTEXT_GUI, UINT64_C(100000000)) == ST_PROBE_CLEANUP_UNRESOLVED);
    assert(emitted.cleanup_class == ST_CLEANUP_UNRESOLVED);
}
TEST(unknown_codes_and_reserved_schema_reject) {
    st_capability_observation observation = {1, 1, 1, 1, 6, 1, 1, 0};
    uint8_t bytes[32];
    assert(st_capability_encode(&observation, bytes) == 0);
    st_capability_observation decoded;
    for (size_t field = 0; field < 8; ++field) {
        uint8_t bad[32];
        memcpy(bad, bytes, sizeof(bad));
        memset(bad + field * 4, 0xff, 4);
        assert(st_capability_decode(bad, sizeof(bad), &decoded) == ST_PROBE_INVALID);
    }
    assert(st_capability_decode(bytes, 31, &decoded) == ST_PROBE_INVALID);
}

TEST(owned_worker_cancellation_is_not_denial_or_success) {
    assert(run(9, ST_CAP_MICROPHONE, ST_CONTEXT_GUI, UINT64_C(1000000000)) == ST_PROBE_OK);
    assert(emitted.outcome == ST_OUTCOME_CANCELLED);
    assert(emitted.effect_class == ST_EFFECT_UNOBSERVED);
    assert(run(9, ST_CAP_CLIPBOARD, ST_CONTEXT_GUI, UINT64_C(1000000000)) == ST_PROBE_OK);
    assert(emitted.outcome == ST_OUTCOME_CANCELLED && emitted.cleanup_class == ST_CLEANUP_NOT_NEEDED);
    assert(run(9, ST_CAP_TOUCH_ID_SUDO, ST_CONTEXT_GUI, UINT64_C(1000000000)) == ST_PROBE_OK);
    assert(emitted.outcome == ST_OUTCOME_CANCELLED && emitted.cleanup_class == ST_CLEANUP_VERIFIED);
}

TEST(normalized_environment_discloses_counts_not_values) {
    st_capability_environment observation;
    assert(st_capability_normalize_environment("/private/synthetic:/usr/bin:/private/synthetic:", 1, &observation) == 0);
    assert(observation.path_components == 4 && observation.duplicate_path_components == 1 && observation.locale_present == 1);
    uint8_t encoded[16];
    assert(st_capability_environment_encode(&observation, encoded) == 0);
    for (size_t i = 0; i < sizeof(encoded); ++i) assert(encoded[i] <= 4);
    assert(st_capability_normalize_environment(NULL, 0, &observation) == 0 && observation.path_components == 0);
    char oversize[4098]; memset(oversize, 'a', sizeof(oversize)); oversize[4097] = 0;
    assert(st_capability_normalize_environment(oversize, 0, &observation) == ST_PROBE_LIMIT);
}

TEST(escaped_descendant_never_counts_as_complete_cleanup) {
    assert(pipe(escaped_control) == 0 && pipe(escaped_status) == 0);
    st_native_result result = run(10, ST_CAP_TOUCH_ID_SUDO, ST_CONTEXT_GUI, UINT64_C(1000000000));
    close(escaped_control[0]); close(escaped_status[1]);
    char byte;
    assert(read(escaped_status[0], &byte, 1) == 1 && byte == 'r');
    /* The setsid descendant is still alive after the worker was reaped. Release
     * it before asserting so the red regression cannot strand the fixture. */
    close(escaped_control[1]);
    assert(read(escaped_status[0], &byte, 1) == 0);
    close(escaped_status[0]);
    assert(result == ST_PROBE_CLEANUP_UNRESOLVED);
    assert(emitted.cleanup_class == ST_CLEANUP_UNRESOLVED);
}

TEST(closed_configuration_reader_emits_failure_and_retires_worker) {
    /* Use a real process with default SIGPIPE, so a process-wide ignored signal
     * inherited from a test runner cannot make this regression pass. */
    pid_t owner = fork(); assert(owner >= 0);
    if (owner == 0) {
        assert(signal(SIGPIPE, SIG_DFL) != SIG_ERR);
        int socket_fds[2]; assert(socketpair(AF_UNIX, SOCK_STREAM, 0, socket_fds) == 0);
        close(socket_fds[1]); char byte = 0;
        assert(!st_capability_transfer(socket_fds[0], &byte, 1, 1,
            st_capability_monotonic_ns() + UINT64_C(1000000000)));
        close(socket_fds[0]);
        struct sigaction disposition; assert(sigaction(SIGPIPE, NULL, &disposition) == 0);
        assert(disposition.sa_handler == SIG_DFL);
        finish_count = 0;
        assert(run(11, ST_CAP_URL, ST_CONTEXT_GUI, UINT64_C(1000000000)) == ST_PROBE_INVALID);
        assert(emitted.outcome == ST_OUTCOME_FAILED && emitted.cleanup_class == ST_CLEANUP_NOT_NEEDED);
        assert(finish_count == 1);
        _exit(0);
    }
    int status; assert(waitpid(owner, &status, 0) == owner);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
}

TEST(login_shell_values_never_cross_a_worker_process) {
    mode = 0; emit_count = spawn_count = native_count = 0;
    assert(st_capability_run(ST_CAP_LOGIN_SHELL, ST_CONTEXT_GUI,
        st_capability_monotonic_ns() + UINT64_C(1000000000), emit, &emitted) == ST_PROBE_OK);
    assert(emit_count == 1 && spawn_count == 0 && native_count == 1 && emitted.outcome == ST_OUTCOME_PASSED);
}

TEST(local_environment_comparison_detects_values_and_order_without_export) {
    const char *locale[ST_CAPABILITY_LOCALE_COUNT] = {"C"};
    st_capability_environment_difference original, order, changed;
    assert(st_capability_environment_compare_local(ST_CONTEXT_GUI, "/usr/bin:/bin:/usr/sbin:/sbin", locale, &original) == 0);
    assert(original.path_difference == ST_PATH_EQUAL && original.locale_difference == ST_LOCALE_EQUAL);
    assert(st_capability_environment_compare_local(ST_CONTEXT_DAEMON, "/bin:/usr/bin:/usr/sbin:/sbin", locale, &order) == 0);
    assert(order.path_difference == ST_PATH_ORDER_CHANGED && order.context == ST_CONTEXT_DAEMON);
    locale[0] = "en_US.UTF-8";
    assert(st_capability_environment_compare_local(ST_CONTEXT_GUI, "/usr/bin:/bin:/usr/sbin:/synthetic", locale, &changed) == 0);
    assert(changed.path_difference == ST_PATH_COMPONENTS_CHANGED && changed.locale_difference == ST_LOCALE_VALUES_CHANGED);
    uint8_t bytes[20]; assert(st_capability_environment_difference_encode(&changed, bytes) == 0);
    for (size_t i = 0; i < sizeof(bytes); ++i) assert(bytes[i] <= 4);
    assert(bytes[3] == 2 && bytes[7] == ST_CONTEXT_GUI && bytes[11] == 1);
    locale[1] = "C";
    assert(st_capability_environment_compare_local(ST_CONTEXT_GUI, NULL, locale, &changed) == 0);
    assert(changed.path_difference == ST_PATH_PRESENCE_CHANGED && changed.locale_difference == ST_LOCALE_PRESENCE_CHANGED);
    char oversized[4098]; memset(oversized, 'x', sizeof(oversized)); oversized[4097] = 0;
    assert(st_capability_environment_compare_local(ST_CONTEXT_GUI, oversized, locale, &changed) == ST_PROBE_LIMIT);
    locale[0] = oversized;
    assert(st_capability_environment_compare_local(ST_CONTEXT_GUI, NULL, locale, &changed) == ST_PROBE_LIMIT);
}

TEST(public_shell_mode_metadata_is_bounded_and_never_contains_argv) {
    const char *inputs[] = {"11", "00", "10", "01", "111", "1x", "-l -i", "1"};
    for (size_t i = 0; i < sizeof(inputs) / sizeof(inputs[0]); ++i) {
        int descriptors[2]; assert(pipe(descriptors) == 0);
        assert(write(descriptors[1], inputs[i], strlen(inputs[i])) == (ssize_t)strlen(inputs[i]));
        close(descriptors[1]);
        uint32_t mode = UINT32_MAX;
        st_native_result result = st_capability_read_shell_mode(descriptors[0],
            st_capability_monotonic_ns() + UINT64_C(1000000000), &mode);
        close(descriptors[0]);
        if (i < 4) { assert(result == ST_PROBE_OK); assert((mode == 3) == (i == 0)); }
        else assert(result != ST_PROBE_OK);
    }
}

TEST(sigpipe_suppression_handles_sockets_and_preserves_existing_pending_signal) {
    sigset_t blocked, original, pending;
    sigemptyset(&blocked); sigaddset(&blocked, SIGPIPE);
    assert(sigprocmask(SIG_BLOCK, &blocked, &original) == 0);
    assert(raise(SIGPIPE) == 0);
    int channels[2]; assert(socketpair(AF_UNIX, SOCK_STREAM, 0, channels) == 0);
    close(channels[1]);
    char byte = 0;
    assert(!st_capability_transfer(channels[0], &byte, 1, 1,
        st_capability_monotonic_ns() + UINT64_C(1000000000)));
    assert(sigpending(&pending) == 0 && sigismember(&pending, SIGPIPE));
    int received; assert(sigwait(&blocked, &received) == 0 && received == SIGPIPE);
    assert(sigprocmask(SIG_SETMASK, &original, NULL) == 0);
    close(channels[0]);
}

#ifdef __APPLE__
static pthread_mutex_t mask_mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t mask_condition = PTHREAD_COND_INITIALIZER;
static int mask_phase;
static void *distinct_mask(void *unused) {
    (void)unused;
    sigset_t blocked, pending;
    sigemptyset(&blocked); sigaddset(&blocked, SIGUSR1);
    assert(pthread_sigmask(SIG_BLOCK, &blocked, NULL) == 0);
    assert(pthread_kill(pthread_self(), SIGUSR1) == 0);
    assert(pthread_mutex_lock(&mask_mutex) == 0);
    mask_phase = 1; assert(pthread_cond_signal(&mask_condition) == 0);
    while (mask_phase != 2) assert(pthread_cond_wait(&mask_condition, &mask_mutex) == 0);
    assert(pthread_mutex_unlock(&mask_mutex) == 0);
    assert(pthread_sigmask(SIG_SETMASK, NULL, &blocked) == 0);
    assert(sigismember(&blocked, SIGUSR1));
    assert(sigpending(&pending) == 0 && sigismember(&pending, SIGUSR1));
    int signal_number; assert(sigwait(&blocked, &signal_number) == 0 && signal_number == SIGUSR1);
    return NULL;
}
TEST(descriptor_sigpipe_preserves_other_thread_mask_and_pending_signal) {
    pid_t owner = fork(); assert(owner >= 0);
    if (!owner) {
        assert(signal(SIGPIPE, SIG_DFL) != SIG_ERR);
        assert(signal(SIGUSR1, SIG_DFL) != SIG_ERR);
        sigset_t empty; sigemptyset(&empty); assert(pthread_sigmask(SIG_SETMASK, &empty, NULL) == 0);
        pthread_t worker; assert(pthread_create(&worker, NULL, distinct_mask, NULL) == 0);
        assert(pthread_mutex_lock(&mask_mutex) == 0);
        while (mask_phase != 1) assert(pthread_cond_wait(&mask_condition, &mask_mutex) == 0);
        assert(pthread_mutex_unlock(&mask_mutex) == 0);
        for (int socket = 0; socket < 2; ++socket) {
            int fds[2]; assert((socket ? socketpair(AF_UNIX, SOCK_STREAM, 0, fds) : pipe(fds)) == 0);
            int writing = socket ? fds[0] : fds[1], reading = socket ? fds[1] : fds[0];
            close(reading); char byte = 0;
            assert(!st_capability_transfer(writing, &byte, 1, 1,
                st_capability_monotonic_ns() + UINT64_C(1000000000)));
            close(writing);
        }
        assert(pthread_mutex_lock(&mask_mutex) == 0);
        mask_phase = 2; assert(pthread_cond_signal(&mask_condition) == 0);
        assert(pthread_mutex_unlock(&mask_mutex) == 0);
        assert(pthread_join(worker, NULL) == 0);
        assert(pthread_sigmask(SIG_SETMASK, NULL, &empty) == 0 && !sigismember(&empty, SIGUSR1));
        _exit(0);
    }
    int status; assert(waitpid(owner, &status, 0) == owner);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
}
#endif

int main(void) {
#ifdef __APPLE__
    RUN(descriptor_sigpipe_preserves_other_thread_mask_and_pending_signal);
#endif
    RUN(probe_timeout_does_not_reset_after_partial_output);
    RUN(probe_output_rejects_paths_environment_values_and_native_errors);
    RUN(preflight_success_without_effect_is_not_passed);
    RUN(gui_and_daemon_results_are_never_coalesced);
    RUN(missing_volume_and_agent_are_unrun_or_unavailable);
    RUN(capture_pixels_and_microphone_samples_are_discarded);
    RUN(touch_id_case_executes_only_usr_bin_true);
    RUN(completed_fixed_tree_is_not_unconditionally_cleanup_unresolved);
    RUN(short_and_extra_output_reject_before_success);
    RUN(cleanup_uncertainty_overrides_effect_success);
    RUN(resource_worker_timeout_does_not_claim_cleanup);
    RUN(unknown_codes_and_reserved_schema_reject);
    RUN(normalized_environment_discloses_counts_not_values);
    RUN(owned_worker_cancellation_is_not_denial_or_success);
    RUN(escaped_descendant_never_counts_as_complete_cleanup);
    RUN(closed_configuration_reader_emits_failure_and_retires_worker);
    RUN(login_shell_values_never_cross_a_worker_process);
    RUN(local_environment_comparison_detects_values_and_order_without_export);
    RUN(public_shell_mode_metadata_is_bounded_and_never_contains_argv);
    RUN(sigpipe_suppression_handles_sockets_and_preserves_existing_pending_signal);
    printf("executed=%u proof=synthetic native-effects=0\n", test_count);
    #ifdef __APPLE__
    return test_count == 21 ? 0 : 1;
#else
    return test_count == 20 ? 0 : 1;
#endif
}
