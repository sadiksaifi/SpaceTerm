#define __STDC_WANT_LIB_EXT1__ 1
#define _DARWIN_C_SOURCE 1
#include "handoff.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <stdbool.h>
#include <stddef.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#define NANOSECONDS_PER_SECOND UINT64_C(1000000000)
#define NANOSECONDS_PER_MILLISECOND UINT64_C(1000000)

typedef struct {
    uint64_t deadline_ns;
    unsigned int interruptions;
} receive_wait;

static bool all_zero(const uint8_t *bytes, size_t size)
{
    uint8_t combined = 0;
    for (size_t i = 0; i < size; ++i) combined |= bytes[i];
    return combined == 0;
}

static bool same_bytes(const uint8_t *left, const uint8_t *right, size_t size)
{
    uint8_t difference = 0;
    for (size_t i = 0; i < size; ++i) difference |= left[i] ^ right[i];
    return difference == 0;
}

static uint32_t read32(const uint8_t *bytes)
{
    uint32_t value = 0;
    for (size_t i = 0; i < 4; ++i) value = (value << 8) | bytes[i];
    return value;
}

static uint64_t read64(const uint8_t *bytes)
{
    uint64_t value = 0;
    for (size_t i = 0; i < 8; ++i) value = (value << 8) | bytes[i];
    return value;
}

static bool valid_expectation(const st_handoff_frame_expectation *expected)
{
    const st_handoff_pending_context *pending = &expected->pending;
    if (expected->kind != ST_HANDOFF_FRAME_READINESS &&
        expected->kind != ST_HANDOFF_FRAME_CONFIRMATION) return false;
    if (pending->uid == 0 || pending->uid == UINT32_MAX ||
        pending->gid == UINT32_MAX || pending->registration_generation == 0 ||
        pending->original_process.pid == 0 ||
        pending->original_process.pid > INT32_MAX ||
        pending->original_process.start_seconds == 0 ||
        pending->original_process.start_microseconds >= 1000000) return false;
    switch (pending->build) {
    case ST_HANDOFF_BUILD_RELEASE:
    case ST_HANDOFF_BUILD_PREFLIGHT:
    case ST_HANDOFF_BUILD_DEVELOPMENT:
        break;
    default:
        return false;
    }
    if (all_zero(pending->approved_installation, ST_HANDOFF_BINDING_BYTES) ||
        all_zero(pending->server_artifact, ST_HANDOFF_BINDING_BYTES) ||
        all_zero(pending->startup_nonce, ST_HANDOFF_BINDING_BYTES)) return false;
    return expected->kind != ST_HANDOFF_FRAME_READINESS ||
           (all_zero(expected->challenge, ST_HANDOFF_BINDING_BYTES) &&
            all_zero(expected->confirmation_mac, ST_HANDOFF_BINDING_BYTES));
}

static bool frame_matches(const st_handoff_frame_expectation *expected,
                          const uint8_t *bytes, size_t size)
{
    const st_handoff_pending_context *pending = &expected->pending;
    uint32_t obligations = expected->kind == ST_HANDOFF_FRAME_READINESS ?
                           ST_HANDOFF_POST_EXEC_OBLIGATIONS : 0;
    if (bytes[0] != 'S' || bytes[1] != 'T' || bytes[2] != 'H' || bytes[3] != '1' ||
        bytes[4] != 1 || bytes[5] != (uint8_t)expected->kind ||
        !all_zero(bytes + 6, 2) || read32(bytes + 8) != size ||
        !all_zero(bytes + 12, 4) ||
        read32(bytes + 16) != pending->uid ||
        read32(bytes + 20) != pending->gid ||
        read32(bytes + 24) != (uint32_t)pending->build ||
        read32(bytes + 28) != obligations ||
        read64(bytes + 32) != pending->registration_generation ||
        !same_bytes(bytes + 40, pending->approved_installation, ST_HANDOFF_BINDING_BYTES) ||
        !same_bytes(bytes + 72, pending->server_artifact, ST_HANDOFF_BINDING_BYTES) ||
        !same_bytes(bytes + 104, pending->startup_nonce, ST_HANDOFF_BINDING_BYTES) ||
        read64(bytes + 136) != pending->original_process.pid ||
        read64(bytes + 144) != pending->original_process.start_seconds ||
        read32(bytes + 152) != pending->original_process.start_microseconds ||
        !all_zero(bytes + 156, 36)) return false;
    return expected->kind != ST_HANDOFF_FRAME_CONFIRMATION ||
           (same_bytes(bytes + 192, expected->challenge, ST_HANDOFF_BINDING_BYTES) &&
            same_bytes(bytes + 224, expected->confirmation_mac, ST_HANDOFF_BINDING_BYTES));
}

static bool monotonic_ns(uint64_t *out_now)
{
    struct timespec value;
    if (clock_gettime(CLOCK_MONOTONIC, &value) != 0 || value.tv_sec < 0 ||
        value.tv_nsec < 0 || value.tv_nsec >= (long)NANOSECONDS_PER_SECOND) return false;
    uint64_t seconds = (uint64_t)value.tv_sec;
    uint64_t fraction = (uint64_t)value.tv_nsec;
    if (seconds > (UINT64_MAX - fraction) / NANOSECONDS_PER_SECOND) return false;
    *out_now = seconds * NANOSECONDS_PER_SECOND + fraction;
    return true;
}

static bool remaining_wait(const receive_wait *wait, int *out_timeout_ms,
                           st_handoff_result *failure)
{
    uint64_t now;
    if (!monotonic_ns(&now)) {
        *failure = ST_HANDOFF_REJECTED;
        return false;
    }
    if (now >= wait->deadline_ns) {
        *failure = ST_HANDOFF_TIMED_OUT;
        return false;
    }
    if (out_timeout_ms != NULL) {
        /* Admission bounds this difference to two seconds. Round up for poll,
         * then check the same absolute deadline again before reading.
         */
        uint64_t remaining = wait->deadline_ns - now;
        *out_timeout_ms = (int)((remaining + NANOSECONDS_PER_MILLISECOND - 1) /
                               NANOSECONDS_PER_MILLISECOND);
    }
    return true;
}

static bool retry_interrupted(receive_wait *wait, int error,
                              st_handoff_result *failure)
{
    if (error != EINTR) {
        *failure = ST_HANDOFF_REJECTED;
        return false;
    }
    if (wait->interruptions >= ST_HANDOFF_MAX_INTERRUPTS) {
        *failure = ST_HANDOFF_INTERRUPTED;
        return false;
    }
    ++wait->interruptions;
    return remaining_wait(wait, NULL, failure);
}

static bool valid_endpoint(int descriptor, receive_wait *wait,
                           st_handoff_result *failure)
{
    *failure = ST_HANDOFF_REJECTED;
    struct stat metadata;
    while (fstat(descriptor, &metadata) != 0) {
        if (!retry_interrupted(wait, errno, failure)) return false;
    }
    if (!S_ISFIFO(metadata.st_mode)) return false;
    int access_flags;
    while ((access_flags = fcntl(descriptor, F_GETFL)) < 0) {
        if (!retry_interrupted(wait, errno, failure)) return false;
    }
    if ((access_flags & O_ACCMODE) != O_RDONLY) return false;
    int descriptor_flags;
    while ((descriptor_flags = fcntl(descriptor, F_GETFD)) < 0) {
        if (!retry_interrupted(wait, errno, failure)) return false;
    }
    return (descriptor_flags & FD_CLOEXEC) != 0;
}

static st_handoff_result receive_frame(int descriptor, receive_wait *wait,
                                      const st_handoff_frame_expectation *expected,
                                      uint8_t *bytes, size_t size)
{
    size_t received = 0;
    st_handoff_result result = ST_HANDOFF_REJECTED;
    for (;;) {
        int timeout_ms;
        if (!remaining_wait(wait, &timeout_ms, &result)) return result;
        struct pollfd ready = {.fd = descriptor, .events = POLLIN};
        int polled = poll(&ready, 1, timeout_ms);
        if (polled < 0) {
            if (!retry_interrupted(wait, errno, &result)) return result;
            continue;
        }
        if (polled == 0) continue;
        if ((ready.revents & (POLLERR | POLLNVAL)) != 0 ||
            (ready.revents & (POLLIN | POLLHUP)) == 0) return ST_HANDOFF_REJECTED;
        if (!remaining_wait(wait, NULL, &result)) return result;

        /* Only this call owns the reader, so another reader cannot drain poll's
         * readiness. Do not change open-file-description flags or signal state.
         * HUP can accompany unread data: read until EOF, not merely until HUP.
         */
        ssize_t count = read(descriptor, bytes + received, size + 1 - received);
        if (count < 0) {
            if (!retry_interrupted(wait, errno, &result)) return result;
            continue;
        }
        if (count == 0) {
            if (received == 0) return ST_HANDOFF_PEER_CLOSED;
            if (received != size || !frame_matches(expected, bytes, size)) {
                return ST_HANDOFF_REJECTED;
            }
            if (!remaining_wait(wait, NULL, &result)) return result;
            return ST_HANDOFF_CANDIDATE_MATCH;
        }
        received += (size_t)count;
        if (received > size) return ST_HANDOFF_REJECTED;
        /* A complete frame still needs EOF. The next read probes at most one
         * trailing byte, with the original deadline and interruption budget.
         */
    }
}

st_handoff_result st_handoff_receive_candidate(
    const st_handoff_frame_expectation *expected,
    int *owned_read_end,
    uint64_t monotonic_deadline_ns,
    st_handoff_candidate *out_candidate)
{
    int descriptor = -1;
    if (owned_read_end != NULL) {
        descriptor = *owned_read_end;
        *owned_read_end = -1;
    }
    if (out_candidate != NULL) {
        *out_candidate = (st_handoff_candidate){.kind = ST_HANDOFF_FRAME_NONE};
    }
    uint8_t bytes[ST_HANDOFF_MAX_FRAME_BYTES + 1] = {0};
    st_handoff_result result = ST_HANDOFF_REJECTED;
    if (descriptor < 0 || expected == NULL || out_candidate == NULL ||
        monotonic_deadline_ns == 0 || !valid_expectation(expected)) goto cleanup;
    uint64_t now;
    if (!monotonic_ns(&now) ||
        (monotonic_deadline_ns > now &&
         monotonic_deadline_ns - now > ST_HANDOFF_MAX_WAIT_NS)) goto cleanup;
    receive_wait wait = {.deadline_ns = monotonic_deadline_ns};
    if (!valid_endpoint(descriptor, &wait, &result)) goto cleanup;
    size_t size = expected->kind == ST_HANDOFF_FRAME_READINESS ?
                  ST_HANDOFF_READINESS_BYTES : ST_HANDOFF_CONFIRMATION_BYTES;
    result = receive_frame(descriptor, &wait, expected, bytes, size);
    if (result == ST_HANDOFF_CANDIDATE_MATCH) out_candidate->kind = expected->kind;

cleanup:
    /* The expectation stays borrowed. This is the only mutable frame buffer,
     * including confirmation bytes and the trailing probe; retain no copies.
     */
    (void)memset_s(bytes, sizeof bytes, 0, sizeof bytes);
    /* Never retry close after EINTR: the descriptor number could be reused.
     * The caller's owning handle was already cleared on every return path.
     */
    if (descriptor >= 0) (void)close(descriptor);
    return result;
}

st_handoff_result st_handoff_native_admit(
    const st_handoff_pending_context *pending)
{
    (void)pending;
    /* No policy/item, code identity, installation, registration, privilege, or
     * protection operations are performed. Wire equality is not admission.
     */
    return ST_HANDOFF_NATIVE_UNAVAILABLE;
}
