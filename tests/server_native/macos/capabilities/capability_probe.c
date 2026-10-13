#define _POSIX_C_SOURCE 200809L
#include "capability_internal.h"
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <signal.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

_Static_assert(sizeof(st_capability_observation) == 32, "capability ABI size");
_Static_assert(offsetof(st_capability_observation, cleanup_class) == 28, "capability ABI offset");

uint64_t st_capability_monotonic_ns(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) return 0;
    return (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
}

static int valid(const st_capability_observation *value) {
    return value && value->schema == 1 && value->capability >= 1 && value->capability <= 13 &&
        value->context >= 1 && value->context <= 2 && value->outcome >= 1 && value->outcome <= 10 &&
        value->responsible_process >= 1 && value->responsible_process <= 6 &&
        value->preflight_class <= 4 && value->effect_class <= 5 && value->cleanup_class <= 2 &&
        (value->outcome != ST_OUTCOME_PASSED ||
            (value->preflight_class == ST_PREFLIGHT_ALLOWED && value->effect_class == ST_EFFECT_VERIFIED)) &&
        ((value->cleanup_class == ST_CLEANUP_UNRESOLVED) ==
            (value->outcome == ST_OUTCOME_CLEANUP_UNRESOLVED)) &&
        (value->outcome != ST_OUTCOME_UNRUN || value->effect_class == ST_EFFECT_UNOBSERVED);
}

st_native_result st_capability_encode(const st_capability_observation *value, uint8_t out[32]) {
    if (!out || !valid(value)) return ST_PROBE_INVALID;
    const uint32_t fields[8] = {value->schema, value->capability, value->context, value->outcome,
        value->responsible_process, value->preflight_class, value->effect_class, value->cleanup_class};
    for (size_t i = 0; i < 8; ++i) {
        out[i * 4] = (uint8_t)(fields[i] >> 24);
        out[i * 4 + 1] = (uint8_t)(fields[i] >> 16);
        out[i * 4 + 2] = (uint8_t)(fields[i] >> 8);
        out[i * 4 + 3] = (uint8_t)fields[i];
    }
    return ST_PROBE_OK;
}

st_native_result st_capability_decode(const uint8_t *bytes, size_t length, st_capability_observation *out) {
    if (!bytes || !out || length != 32) return ST_PROBE_INVALID;
    uint32_t fields[8];
    for (size_t i = 0; i < 8; ++i) {
        fields[i] = (uint32_t)bytes[i * 4] << 24 | (uint32_t)bytes[i * 4 + 1] << 16 |
            (uint32_t)bytes[i * 4 + 2] << 8 | bytes[i * 4 + 3];
    }
    st_capability_observation result = {fields[0], fields[1], fields[2], fields[3],
        fields[4], fields[5], fields[6], fields[7]};
    if (!valid(&result)) return ST_PROBE_INVALID;
    *out = result;
    return ST_PROBE_OK;
}

void st_capability_discard_owned(void *bytes, size_t length) {
    if (!bytes) return;
    volatile uint8_t *destination = bytes;
    while (length--) *destination++ = 0;
}

const char *const *st_capability_sudo_argv(void) {
    static const char *const arguments[] = {"/usr/bin/sudo", "/usr/bin/true", NULL};
    return arguments;
}

static int remaining_ms(uint64_t deadline) {
    uint64_t now = st_capability_monotonic_ns();
    if (now == 0 || now >= deadline) return 0;
    uint64_t ms = (deadline - now + UINT64_C(999999)) / UINT64_C(1000000);
    return ms > INT_MAX ? INT_MAX : (int)ms;
}

/* Group retirement covers ordinary workers only. The fixed sudo tree also
 * requires the native finish port's uniqueid ancestry proof. */
static int retire(pid_t child, int *reaped) {
    int already_reaped = *reaped;
    int status;
    pid_t waited = 0;
    if (already_reaped && kill(-child, 0) != 0 && errno == ESRCH) return 1;
    /* Retain the direct child until signaling: reaping releases its PID and
     * permits reuse. Never signal a numeric scope after its anchor was reaped.
     * A zombie may refuse the signal; only final absence establishes cleanup. */
    if (!already_reaped) (void)kill(-child, SIGTERM);
    uint64_t deadline = st_capability_monotonic_ns() + UINT64_C(250000000);
    do {
        if (!already_reaped) {
            waited = waitpid(child, &status, WNOHANG);
            if (waited == child) *reaped = already_reaped = 1;
            else if (waited < 0 && errno != EINTR) return 0;
        }
        if (already_reaped && kill(-child, 0) != 0 && errno == ESRCH) return 1;
        struct timespec pause = {0, 1000000};
        nanosleep(&pause, NULL);
    } while (remaining_ms(deadline));
    return 0;
}

st_native_result st_capability_run(st_capability_case capability, st_probe_context context,
    uint64_t deadline, st_capability_emit_fn emit, void *emit_context) {
    if (!emit || capability < 1 || capability > 13 || context < 1 || context > 2) return ST_PROBE_INVALID;
    st_capability_observation observation = {1, capability, context, ST_OUTCOME_UNAVAILABLE,
        ST_RESPONSIBLE_UNKNOWN, ST_PREFLIGHT_UNKNOWN, ST_EFFECT_UNOBSERVED, ST_CLEANUP_NOT_NEEDED};
    uint64_t now = st_capability_monotonic_ns();
    uint64_t limit = capability == ST_CAP_TOUCH_ID_SUDO ? UINT64_C(60000000000) : UINT64_C(5000000000);
    st_native_result result = ST_PROBE_OK;
    if (now == 0 || deadline <= now) {
        observation.outcome = ST_OUTCOME_TIMED_OUT;
        emit(emit_context, &observation);
        return ST_PROBE_TIMED_OUT;
    }
    if (deadline - now > limit) return ST_PROBE_LIMIT;
    if (capability == ST_CAP_LOGIN_SHELL) {
        /* Compare inherited private values here. Re-exec would send them across
         * a process boundary and require an authenticated private-value channel. */
        result = st_capability_native_worker(capability, context, deadline, &observation);
        if (!valid(&observation) || observation.capability != capability || observation.context != context)
            return ST_PROBE_INVALID;
        if (st_capability_monotonic_ns() >= deadline) {
            observation.outcome = ST_OUTCOME_TIMED_OUT; observation.effect_class = ST_EFFECT_UNOBSERVED;
            result = ST_PROBE_TIMED_OUT;
        }
        emit(emit_context, &observation); return result;
    }
    pid_t child = -1;
    int output = -1;
    result = st_capability_spawn(capability, context, deadline, &child, &output);
    if (result != ST_PROBE_OK) { emit(emit_context, &observation); return result; }
    if (child <= 0 || output < 0) {
        observation.outcome = ST_OUTCOME_CLEANUP_UNRESOLVED;
        observation.cleanup_class = ST_CLEANUP_UNRESOLVED;
        emit(emit_context, &observation);
        return ST_PROBE_CLEANUP_UNRESOLVED;
    }
    int flags = fcntl(output, F_GETFL);
    if (flags < 0 || fcntl(output, F_SETFL, flags | O_NONBLOCK) != 0) result = ST_PROBE_IO_UNAVAILABLE;
    uint8_t bytes[33] = {0};
    size_t used = 0;
    int eof = 0, reaped = 0, status = 0;
    while (result == ST_PROBE_OK && (!eof || !reaped)) {
        int ms = remaining_ms(deadline);
        if (!ms) { result = ST_PROBE_TIMED_OUT; break; }
        struct pollfd poll_fd = {output, POLLIN | POLLHUP, 0};
        int ready = poll(&poll_fd, 1, ms < 10 ? ms : 10);
        if (ready < 0 && errno != EINTR) { result = ST_PROBE_IO_UNAVAILABLE; break; }
        if (ready > 0 && (poll_fd.revents & (POLLIN | POLLHUP))) {
            ssize_t received = read(output, bytes + used, sizeof(bytes) - used);
            if (received > 0) {
                used += (size_t)received;
                if (used > 32) { result = ST_PROBE_INVALID; break; }
            } else if (received == 0) eof = 1;
            else if (errno != EAGAIN && errno != EINTR) result = ST_PROBE_IO_UNAVAILABLE;
        }
        if (ready > 0 && (poll_fd.revents & (POLLERR | POLLNVAL))) result = ST_PROBE_IO_UNAVAILABLE;
        if (!reaped) {
            pid_t waited = waitpid(child, &status, WNOHANG);
            if (waited == child) reaped = 1;
            else if (waited < 0 && errno != EINTR) result = ST_PROBE_CLEANUP_UNRESOLVED;
        }
    }
    close(output);
    if (result == ST_PROBE_OK && st_capability_monotonic_ns() >= deadline) result = ST_PROBE_TIMED_OUT;
    int cancelled = result == ST_PROBE_OK && reaped && WIFSIGNALED(status) &&
        (WTERMSIG(status) == SIGINT || WTERMSIG(status) == SIGTERM) &&
        st_capability_monotonic_ns() < deadline;
    if (cancelled) {
        observation.outcome = ST_OUTCOME_CANCELLED;
        if (capability == ST_CAP_LOGIN_KEYCHAIN)
            observation.cleanup_class = ST_CLEANUP_UNRESOLVED;
    } else if (result == ST_PROBE_OK) {
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0 ||
            st_capability_decode(bytes, used, &observation) != ST_PROBE_OK ||
            observation.capability != capability || observation.context != context) result = ST_PROBE_INVALID;
    }
    st_capability_discard_owned(bytes, sizeof(bytes));
    if (result != ST_PROBE_OK) {
        observation = (st_capability_observation){1, capability, context,
            result == ST_PROBE_TIMED_OUT ? ST_OUTCOME_TIMED_OUT : ST_OUTCOME_FAILED,
            ST_RESPONSIBLE_UNKNOWN, ST_PREFLIGHT_UNKNOWN, ST_EFFECT_UNOBSERVED, ST_CLEANUP_NOT_NEEDED};
        /* Resource-bearing worker death cannot establish restoration. */
        if (capability == ST_CAP_LOGIN_KEYCHAIN)
            observation.cleanup_class = ST_CLEANUP_UNRESOLVED;
    }
    int retired = retire(child, &reaped);
    st_native_result finished = st_capability_finish_spawn(child, retired, reaped);
    if ((!retired && capability != ST_CAP_TOUCH_ID_SUDO) || finished != ST_PROBE_OK) observation.cleanup_class = ST_CLEANUP_UNRESOLVED;
    else if (capability == ST_CAP_TOUCH_ID_SUDO && observation.cleanup_class != ST_CLEANUP_UNRESOLVED)
        observation.cleanup_class = ST_CLEANUP_VERIFIED;
    /* The native finish port proves the fixed sudo tree with uniqueid ancestry,
     * a reaped sentinel and identity-checked rescans. Group emptiness alone does
     * not authorize that port's success. */
    if (observation.cleanup_class == ST_CLEANUP_UNRESOLVED) {
        observation.outcome = ST_OUTCOME_CLEANUP_UNRESOLVED;
        result = ST_PROBE_CLEANUP_UNRESOLVED;
    }
    emit(emit_context, &observation);
    return result;
}
