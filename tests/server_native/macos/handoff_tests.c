#define __STDC_WANT_LIB_EXT1__ 1
#define _DARWIN_C_SOURCE 1
#include "handoff.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <signal.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifdef ST_HANDOFF_PRIVATE_REAP_FAULT
#include <stdlib.h>
#include <sys/stat.h>
#endif

/* Wire-candidate tests, not signed/root/Keychain/launcher evidence. Expected
 * packets use the documented literal grammar, never a production encoder.
 * Each case runs in an owned process group with a bounded private result pipe.
 * Writer children own real anonymous pipes and are reaped by their exact PID.
 * Only fixed classifications reach the supervisor's stdout. Workers have no
 * stdout/stderr channel, and no fixture bytes are arguments, environment, or
 * files. Core files are disabled in the test harness, not asserted as native
 * server protection. No privileged drop, exec, secret retrieval, or service runs.
 */
_Static_assert(ST_HANDOFF_READINESS_BYTES == 192 &&
               ST_HANDOFF_CONFIRMATION_BYTES == 256 &&
               ST_HANDOFF_MAX_FRAME_BYTES == 256,
               "version-one fixture uses fixed literal wire sizes");

#define RECEIVE_BUDGET_NS UINT64_C(120000000)
#define CASE_BUDGET_NS UINT64_C(1200000000)
#define SUITE_BUDGET_NS UINT64_C(30000000000)
#define REAP_BUDGET_NS UINT64_C(500000000)

typedef enum {
    CASE_PASS = 0,
    CASE_RED = 1,
    CASE_FIXTURE_ERROR = 2,
    CASE_DEADLINE = 3,
    CASE_GATED = 4,
    CASE_CLEANUP_UNRESOLVED = 5
} case_result;

typedef enum {
    READY_EXACT, CONFIRM_EXACT, READY_FRAGMENTED, CONFIRM_FRAGMENTED,
    EMPTY_CLOSED, ARBITRARY_BYTE, READY_SHORT, CONFIRM_SHORT,
    TRAILING_BYTE, CONFIRM_TRAILING, DELAYED_TRAILING,
    UNKNOWN_TYPE, UNKNOWN_VERSION, WRONG_MAGIC, WRONG_DECLARED_SIZE,
    OVERSIZED_DECLARED_SIZE, HEADER_RESERVED, FRAME_RESERVED,
    MISSING_GROUP_DROP, MISSING_GID_DROP, MISSING_UID_DROP,
    MISSING_CORE_PROTECTION, MISSING_MEMORY_PROTECTION, MISSING_POST_EXEC,
    EXTRA_OBLIGATION, WRONG_UID, WRONG_GID, WRONG_BUILD, WRONG_GENERATION,
    WRONG_INSTALLATION, WRONG_ARTIFACT, WRONG_NONCE, WRONG_PID,
    SAME_PID_WRONG_START, WRONG_START_FRACTION, ROOT_TARGET,
    ZERO_GENERATION, ZERO_NONCE, ZERO_INSTALLATION, ZERO_ARTIFACT,
    INVALID_BUILD, INVALID_PID, INVALID_START_FRACTION,
    INVALID_EXPECTED_KIND, INVALID_EXPECTATION_OPEN,
    READY_EXPECTED_CHALLENGE, READY_EXPECTED_MAC,
    CONFIRM_NOT_READY, READY_NOT_CONFIRM, WRONG_CHALLENGE, WRONG_MAC,
    CONFIRM_STALE_GENERATION, CONFIRM_WRONG_NONCE, CONFIRM_OBLIGATIONS,
    OPEN_EMPTY, OPEN_PARTIAL, OPEN_READY, OPEN_CONFIRM,
    INTERRUPTED_WAIT, EXPIRED_DEADLINE,
    ZERO_DEADLINE, UNBOUNDED_DEADLINE, MISSING_CLOEXEC,
    WRITE_ENDPOINT, NONPIPE_ENDPOINT, SOCKET_ENDPOINT, NEGATIVE_ENDPOINT,
    NULL_EXPECTATION, NULL_OUTPUT, NULL_ENDPOINT, CONSUMED_ENDPOINT,
    SIGNAL_SEMANTICS, NATIVE_GATE_UNAVAILABLE
#ifdef ST_HANDOFF_PRIVATE_REAP_FAULT
    , PRIVATE_CANCELLATION_REAP_FAULT
#endif
} scenario;

typedef struct {
    const char *name;
    scenario scenario;
} test_case;

static const test_case cases[] = {
    {"readiness-exact", READY_EXACT},
    {"confirmation-exact", CONFIRM_EXACT},
    {"readiness-fragmented", READY_FRAGMENTED},
    {"confirmation-fragmented", CONFIRM_FRAGMENTED},
    {"empty-closed", EMPTY_CLOSED},
    {"arbitrary-byte", ARBITRARY_BYTE},
    {"readiness-truncated", READY_SHORT},
    {"confirmation-truncated", CONFIRM_SHORT},
    {"readiness-trailing", TRAILING_BYTE},
    {"confirmation-trailing", CONFIRM_TRAILING},
    {"delayed-trailing", DELAYED_TRAILING},
    {"unknown-type", UNKNOWN_TYPE},
    {"unknown-version", UNKNOWN_VERSION},
    {"wrong-magic", WRONG_MAGIC},
    {"wrong-declared-size", WRONG_DECLARED_SIZE},
    {"oversized-declared-size", OVERSIZED_DECLARED_SIZE},
    {"header-reserved", HEADER_RESERVED},
    {"frame-reserved", FRAME_RESERVED},
    {"missing-group-drop", MISSING_GROUP_DROP},
    {"missing-gid-drop", MISSING_GID_DROP},
    {"missing-uid-drop", MISSING_UID_DROP},
    {"missing-core-protection", MISSING_CORE_PROTECTION},
    {"missing-memory-protection", MISSING_MEMORY_PROTECTION},
    {"missing-post-exec", MISSING_POST_EXEC},
    {"extra-obligation", EXTRA_OBLIGATION},
    {"wrong-uid", WRONG_UID},
    {"wrong-gid", WRONG_GID},
    {"wrong-build", WRONG_BUILD},
    {"wrong-generation", WRONG_GENERATION},
    {"wrong-installation", WRONG_INSTALLATION},
    {"wrong-server-artifact", WRONG_ARTIFACT},
    {"wrong-nonce", WRONG_NONCE},
    {"wrong-peer-pid", WRONG_PID},
    {"same-pid-wrong-start", SAME_PID_WRONG_START},
    {"wrong-start-fraction", WRONG_START_FRACTION},
    {"root-target", ROOT_TARGET},
    {"zero-generation", ZERO_GENERATION},
    {"zero-nonce", ZERO_NONCE},
    {"zero-installation", ZERO_INSTALLATION},
    {"zero-artifact", ZERO_ARTIFACT},
    {"invalid-build", INVALID_BUILD},
    {"invalid-pid", INVALID_PID},
    {"invalid-start-fraction", INVALID_START_FRACTION},
    {"invalid-expected-kind", INVALID_EXPECTED_KIND},
    {"invalid-expectation-open", INVALID_EXPECTATION_OPEN},
    {"readiness-expected-challenge", READY_EXPECTED_CHALLENGE},
    {"readiness-expected-mac", READY_EXPECTED_MAC},
    {"confirmation-not-readiness", CONFIRM_NOT_READY},
    {"readiness-not-confirmation", READY_NOT_CONFIRM},
    {"wrong-confirmation-challenge", WRONG_CHALLENGE},
    {"wrong-confirmation-mac", WRONG_MAC},
    {"confirmation-stale-generation", CONFIRM_STALE_GENERATION},
    {"confirmation-wrong-nonce", CONFIRM_WRONG_NONCE},
    {"confirmation-obligations", CONFIRM_OBLIGATIONS},
    {"open-empty-deadline", OPEN_EMPTY},
    {"open-partial-deadline", OPEN_PARTIAL},
    {"open-readiness-deadline", OPEN_READY},
    {"open-confirmation-deadline", OPEN_CONFIRM},
    {"interrupted-wait", INTERRUPTED_WAIT},
    {"expired-deadline", EXPIRED_DEADLINE},
    {"zero-deadline", ZERO_DEADLINE},
    {"unbounded-deadline", UNBOUNDED_DEADLINE},
    {"missing-cloexec", MISSING_CLOEXEC},
    {"write-endpoint", WRITE_ENDPOINT},
    {"nonpipe-endpoint", NONPIPE_ENDPOINT},
    {"socket-endpoint", SOCKET_ENDPOINT},
    {"negative-endpoint", NEGATIVE_ENDPOINT},
    {"null-expectation", NULL_EXPECTATION},
    {"null-output", NULL_OUTPUT},
    {"null-endpoint", NULL_ENDPOINT},
    {"consumed-endpoint", CONSUMED_ENDPOINT},
    {"signal-semantics", SIGNAL_SEMANTICS},
    {"native-gate-unavailable", NATIVE_GATE_UNAVAILABLE}
};

static int worker_result_fd = -1;
static volatile sig_atomic_t interruptions;
static volatile sig_atomic_t stop_signal;

#ifdef ST_HANDOFF_PRIVATE_REAP_FAULT
/* Coordinator-built private fixture, absent from normal/ASan wire runs. There
 * is no injected case_result, wait status, PID, or native admission result.
 * The real supervised case has finite/pipe leases and never forks a writer.
 * Its cleanup wait is delayed past its actual deadline once. A separate,
 * fault-only bounded wait reaps the killed case before the RED assertion while
 * preserving the original failed wait. This is fixture containment, not a fix
 * for main's cancellation precedence or the Python retirement boundary.
 */
static bool private_reap_fixture;
static bool private_reap_fault_used;
static int private_lease_fd = -1;
static int private_outer_lease_fd = -1;
static int private_ready_fd = -1;
static int private_audit_fd = -1;

static int private_pipe_argument(const char *text, bool read_end)
{
    size_t length = strlen(text);
    if (length == 0 || length > 10) return -1;
    for (size_t i = 0; i < length; ++i) {
        if (text[i] < '0' || text[i] > '9') return -1;
    }
    errno = 0;
    char *end = NULL;
    long value = strtol(text, &end, 10);
    if (errno != 0 || end == NULL || *end != '\0' ||
        value <= STDERR_FILENO || value > INT_MAX) return -1;
    int descriptor = (int)value;
    struct stat metadata;
    int flags = fcntl(descriptor, F_GETFL);
    int wanted = read_end ? O_RDONLY : O_WRONLY;
    if (flags < 0 || (flags & O_ACCMODE) != wanted ||
        fstat(descriptor, &metadata) != 0 || !S_ISFIFO(metadata.st_mode) ||
        fcntl(descriptor, F_SETFD, FD_CLOEXEC) != 0) return -1;
    return descriptor;
}

static bool private_arguments(int argc, char **argv)
{
    if (argc != 6 || strcmp(argv[1], "--private-cancellation-reap-fault") != 0) {
        return false;
    }
    int descriptors[4];
    for (size_t i = 0; i < 4; ++i) {
        descriptors[i] = private_pipe_argument(argv[i + 2], i < 2);
        if (descriptors[i] < 0) return false;
        for (size_t j = 0; j < i; ++j) {
            if (descriptors[j] == descriptors[i]) return false;
        }
    }
    /* The Python fixture blocks signals only while publishing its actual child.
     * Undo that inherited fixture mask before publishing case readiness.
     */
    sigset_t unblocked;
    if (sigemptyset(&unblocked) != 0 ||
        sigprocmask(SIG_SETMASK, &unblocked, NULL) != 0) return false;
    private_lease_fd = descriptors[0];
    private_outer_lease_fd = descriptors[1];
    private_ready_fd = descriptors[2];
    private_audit_fd = descriptors[3];
    private_reap_fixture = true;
    return true;
}
#endif

static void request_stop(int number)
{
    /* Both terminating signals are masked during this handler. */
    if (stop_signal == 0) stop_signal = number;
}

static bool ordinary_stop_signals(void)
{
    struct sigaction normal = {.sa_handler = SIG_DFL};
    return sigemptyset(&normal.sa_mask) == 0 &&
           sigaction(SIGINT, &normal, NULL) == 0 &&
           sigaction(SIGTERM, &normal, NULL) == 0;
}

static uint64_t monotonic_ns(void)
{
    struct timespec value;
    if (clock_gettime(CLOCK_MONOTONIC, &value) != 0) return 0;
    return (uint64_t)value.tv_sec * UINT64_C(1000000000) +
           (uint64_t)value.tv_nsec;
}

static uint64_t cpu_ns(void)
{
    struct timespec value;
    if (clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &value) != 0) return 0;
    return (uint64_t)value.tv_sec * UINT64_C(1000000000) +
           (uint64_t)value.tv_nsec;
}

static bool pause_ns(uint64_t duration)
{
    struct timespec remaining = {
        .tv_sec = (time_t)(duration / UINT64_C(1000000000)),
        .tv_nsec = (long)(duration % UINT64_C(1000000000))
    };
    for (unsigned int attempt = 0; attempt < 64; ++attempt) {
        if (nanosleep(&remaining, &remaining) == 0) return true;
        if (errno != EINTR) return false;
    }
    return false;
}

static bool owned_pipe(int descriptors[2])
{
    if (pipe(descriptors) != 0) return false;
    if (fcntl(descriptors[0], F_SETFD, FD_CLOEXEC) == 0 &&
        fcntl(descriptors[1], F_SETFD, FD_CLOEXEC) == 0) return true;
    (void)close(descriptors[0]);
    (void)close(descriptors[1]);
    descriptors[0] = descriptors[1] = -1;
    return false;
}

static bool closed_descriptor(int descriptor)
{
    errno = 0;
    return fcntl(descriptor, F_GETFD) == -1 && errno == EBADF;
}

static void put32(uint8_t *destination, uint32_t value)
{
    for (size_t i = 0; i < 4; ++i) {
        destination[i] = (uint8_t)(value >> (24 - 8 * i));
    }
}

static void put64(uint8_t *destination, uint64_t value)
{
    for (size_t i = 0; i < 8; ++i) {
        destination[i] = (uint8_t)(value >> (56 - 8 * i));
    }
}

static st_handoff_frame_expectation expectation(st_handoff_frame_kind kind)
{
    st_handoff_frame_expectation value = {
        .pending = {
            .uid = 501,
            .gid = 20,
            .build = ST_HANDOFF_BUILD_DEVELOPMENT,
            .registration_generation = 9,
            .original_process = {
                .pid = (uint32_t)getpid(),
                /* Synthetic start values exercise tuple matching, not kernel
                 * process authentication or a real PID-reuse experiment.
                 */
                .start_seconds = UINT64_C(1700000000),
                .start_microseconds = 123456
            }
        },
        .kind = kind
    };
    memset(value.pending.approved_installation, 0x31, 32);
    memset(value.pending.server_artifact, 0x42, 32);
    memset(value.pending.startup_nonce, 0x53, 32);
    if (kind == ST_HANDOFF_FRAME_CONFIRMATION) {
        memset(value.challenge, 0x64, 32);
        memset(value.confirmation_mac, 0x75, 32);
    }
    return value;
}

static size_t packet(const st_handoff_frame_expectation *expected,
                     uint8_t bytes[ST_HANDOFF_MAX_FRAME_BYTES + 1])
{
    /* Encode literal version-one positions independently of the module. The
     * module does not provide an encoder or a helper that computes expectations.
     */
    memset(bytes, 0, ST_HANDOFF_MAX_FRAME_BYTES + 1);
    bytes[0] = 'S'; bytes[1] = 'T'; bytes[2] = 'H'; bytes[3] = '1';
    bytes[4] = 1;
    bytes[5] = (uint8_t)expected->kind;
    size_t size = expected->kind == ST_HANDOFF_FRAME_READINESS ? 192 : 256;
    put32(bytes + 8, (uint32_t)size);
    put32(bytes + 16, expected->pending.uid);
    put32(bytes + 20, expected->pending.gid);
    put32(bytes + 24, (uint32_t)expected->pending.build);
    put32(bytes + 28, expected->kind == ST_HANDOFF_FRAME_READINESS ? 0x3f : 0);
    put64(bytes + 32, expected->pending.registration_generation);
    memcpy(bytes + 40, expected->pending.approved_installation, 32);
    memcpy(bytes + 72, expected->pending.server_artifact, 32);
    memcpy(bytes + 104, expected->pending.startup_nonce, 32);
    put64(bytes + 136, expected->pending.original_process.pid);
    put64(bytes + 144, expected->pending.original_process.start_seconds);
    put32(bytes + 152, expected->pending.original_process.start_microseconds);
    if (expected->kind == ST_HANDOFF_FRAME_CONFIRMATION) {
        memcpy(bytes + 192, expected->challenge, 32);
        memcpy(bytes + 224, expected->confirmation_mac, 32);
    }
    return size;
}

typedef enum { WRITE_COMPLETE, WRITE_FRAGMENTED, WRITE_TRAILING_LATE,
               HOLD_EMPTY, HOLD_PARTIAL, HOLD_COMPLETE } writer_script;
typedef enum { WRITE_SENT, WRITE_CLOSED, WRITE_FAILED } write_result;

static write_result write_bytes(int descriptor, const uint8_t *bytes, size_t size)
{
    size_t offset = 0;
    unsigned int interrupted = 0;
    while (offset < size) {
        ssize_t count = write(descriptor, bytes + offset, size - offset);
        if (count > 0) offset += (size_t)count;
        else if (count < 0 && errno == EPIPE) return WRITE_CLOSED;
        else if (count < 0 && errno == EINTR && ++interrupted <= 64) continue;
        else return WRITE_FAILED;
    }
    return WRITE_SENT;
}

static int write_script(int descriptor, const uint8_t *bytes, size_t size,
                        writer_script script)
{
    /* The writer alone ignores SIGPIPE: early receiver rejection is expected.
     * The receive process retains its ordinary signal dispositions and mask.
     */
    struct sigaction ignored = {.sa_handler = SIG_IGN};
    if (sigemptyset(&ignored.sa_mask) != 0 ||
        sigaction(SIGPIPE, &ignored, NULL) != 0) return 2;
    if (script == HOLD_EMPTY) {
        return pause_ns(UINT64_C(240000000)) ? 0 : 2;
    }
    if (script == WRITE_FRAGMENTED) {
        for (size_t offset = 0; offset < size;) {
            size_t chunk = size - offset;
            if (chunk > 17) chunk = 17;
            write_result result = write_bytes(descriptor, bytes + offset, chunk);
            if (result == WRITE_CLOSED) return 0;
            if (result == WRITE_FAILED) return 2;
            offset += chunk;
            if (offset < size && !pause_ns(UINT64_C(2000000))) return 2;
        }
        return 0;
    }
    size_t sent = script == HOLD_PARTIAL ? 31 : size;
    write_result result = write_bytes(descriptor, bytes, sent);
    if (result == WRITE_CLOSED) return 0;
    if (result == WRITE_FAILED) return 2;
    if (script == HOLD_PARTIAL || script == HOLD_COMPLETE) {
        return pause_ns(UINT64_C(240000000)) ? 0 : 2;
    }
    if (script == WRITE_TRAILING_LATE) {
        if (!pause_ns(UINT64_C(20000000))) return 2;
        const uint8_t extra = 0x86;
        result = write_bytes(descriptor, &extra, 1);
        if (result == WRITE_FAILED) return 2;
    }
    return 0;
}

static bool reap_writer(pid_t child)
{
    uint64_t start = monotonic_ns();
    if (start == 0) {
        (void)kill(child, SIGKILL);
        return false;
    }
    uint64_t deadline = start + REAP_BUDGET_NS;
    for (unsigned int attempt = 0; attempt < 512; ++attempt) {
        uint64_t now = monotonic_ns();
        if (now == 0 || now >= deadline) break;
        int status = 0;
        pid_t result = waitpid(child, &status, WNOHANG);
        if (result == child) return WIFEXITED(status) && WEXITSTATUS(status) == 0;
        if (result < 0 && errno != EINTR) return false;
        if (!pause_ns(UINT64_C(2000000))) break;
    }
    /* The supervisor owns this worker's entire process group and will clean it
     * if a writer cannot be reaped. Never waitpid(-1) or block indefinitely.
     */
    (void)kill(child, SIGKILL);
    return false;
}

static void count_signal(int number)
{
    (void)number;
    if (interruptions < 1000) ++interruptions;
}

static bool same_mask(const sigset_t *left, const sigset_t *right)
{
    for (int number = 1; number < NSIG; ++number) {
        if (sigismember(left, number) != sigismember(right, number)) return false;
    }
    return true;
}

static bool same_action(const struct sigaction *left,
                        const struct sigaction *right)
{
    return left->sa_handler == right->sa_handler &&
           left->sa_flags == right->sa_flags &&
           same_mask(&left->sa_mask, &right->sa_mask);
}

#ifdef ST_HANDOFF_PRIVATE_REAP_FAULT
static case_result private_hold_case(void)
{
    uint64_t start = monotonic_ns();
    const uint8_t ready = 'r';
    if (start == 0 || write_bytes(private_ready_fd, &ready, 1) != WRITE_SENT) {
        return CASE_FIXTURE_ERROR;
    }
    (void)close(private_ready_fd);
    private_ready_fd = -1;
    uint64_t deadline = start + UINT64_C(6000000000);
    /* Retain the audit pipe without writing so EOF also covers case death if
     * the outer supervisor itself is unexpectedly killed. No orphan can keep
     * running past the independent lease/deadline.
     */
    for (unsigned int attempt = 0; attempt < 512; ++attempt) {
        uint64_t now = monotonic_ns();
        if (now == 0 || now >= deadline) break;
        struct pollfd leases[2] = {
            {.fd = private_lease_fd, .events = POLLIN},
            {.fd = private_outer_lease_fd, .events = POLLIN}
        };
        int polled = poll(leases, 2, 20);
        if (polled < 0 && errno == EINTR) continue;
        if (polled < 0) break;
        for (size_t i = 0; i < 2; ++i) {
            if ((leases[i].revents & (POLLIN | POLLHUP | POLLERR | POLLNVAL)) != 0) {
                return CASE_FIXTURE_ERROR;
            }
        }
    }
    return CASE_FIXTURE_ERROR;
}
#endif

static case_result exercise(scenario selected)
{
#ifdef ST_HANDOFF_PRIVATE_REAP_FAULT
    if (selected == PRIVATE_CANCELLATION_REAP_FAULT) return private_hold_case();
#endif
    st_handoff_frame_kind kind = ST_HANDOFF_FRAME_READINESS;
    switch (selected) {
    case CONFIRM_EXACT: case CONFIRM_FRAGMENTED: case CONFIRM_SHORT:
    case CONFIRM_TRAILING: case READY_NOT_CONFIRM: case WRONG_CHALLENGE:
    case WRONG_MAC: case CONFIRM_STALE_GENERATION: case CONFIRM_WRONG_NONCE:
    case CONFIRM_OBLIGATIONS: case OPEN_CONFIRM:
        kind = ST_HANDOFF_FRAME_CONFIRMATION;
        break;
    default: break;
    }
    st_handoff_frame_expectation expected = expectation(kind);
    uint8_t bytes[ST_HANDOFF_MAX_FRAME_BYTES + 1];
    size_t size = packet(&expected, bytes);
    st_handoff_result wanted = ST_HANDOFF_REJECTED;
    writer_script script = WRITE_COMPLETE;
    switch (selected) {
    case READY_EXACT: case CONFIRM_EXACT: case CONSUMED_ENDPOINT:
    case SIGNAL_SEMANTICS:
        wanted = ST_HANDOFF_CANDIDATE_MATCH;
        break;
    case READY_FRAGMENTED: case CONFIRM_FRAGMENTED:
        wanted = ST_HANDOFF_CANDIDATE_MATCH;
        script = WRITE_FRAGMENTED;
        break;
    case EMPTY_CLOSED: size = 0; wanted = ST_HANDOFF_PEER_CLOSED; break;
    case ARBITRARY_BYTE: bytes[0] = 'R'; size = 1; break;
    case READY_SHORT: case CONFIRM_SHORT: --size; break;
    case TRAILING_BYTE: case CONFIRM_TRAILING: bytes[size++] = 0x86; break;
    case DELAYED_TRAILING: script = WRITE_TRAILING_LATE; break;
    case UNKNOWN_TYPE: bytes[5] = 3; break;
    case UNKNOWN_VERSION: bytes[4] = 2; break;
    case WRONG_MAGIC: bytes[0] = 'X'; break;
    case WRONG_DECLARED_SIZE: put32(bytes + 8, 191); break;
    case OVERSIZED_DECLARED_SIZE: put32(bytes + 8, UINT32_MAX); break;
    case HEADER_RESERVED: bytes[6] = 1; break;
    case FRAME_RESERVED: bytes[191] = 1; break;
    case MISSING_GROUP_DROP: put32(bytes + 28, 0x3e); break;
    case MISSING_GID_DROP: put32(bytes + 28, 0x3d); break;
    case MISSING_UID_DROP: put32(bytes + 28, 0x3b); break;
    case MISSING_CORE_PROTECTION: put32(bytes + 28, 0x37); break;
    case MISSING_MEMORY_PROTECTION: put32(bytes + 28, 0x2f); break;
    case MISSING_POST_EXEC: put32(bytes + 28, 0x1f); break;
    case EXTRA_OBLIGATION: put32(bytes + 28, 0x7f); break;
    case WRONG_UID: put32(bytes + 16, 502); break;
    case WRONG_GID: put32(bytes + 20, 21); break;
    case WRONG_BUILD: put32(bytes + 24, 1); break;
    case WRONG_GENERATION: case CONFIRM_STALE_GENERATION:
        put64(bytes + 32, 8); break;
    case WRONG_INSTALLATION: bytes[40] ^= 1; break;
    case WRONG_ARTIFACT: bytes[72] ^= 1; break;
    case WRONG_NONCE: case CONFIRM_WRONG_NONCE: bytes[104] ^= 1; break;
    case WRONG_PID: put64(bytes + 136, expected.pending.original_process.pid + 1); break;
    case SAME_PID_WRONG_START:
        put64(bytes + 144, expected.pending.original_process.start_seconds + 1); break;
    case WRONG_START_FRACTION:
        put32(bytes + 152, expected.pending.original_process.start_microseconds + 1); break;
    case ROOT_TARGET: expected.pending.uid = 0; size = packet(&expected, bytes); break;
    case ZERO_GENERATION:
        expected.pending.registration_generation = 0; size = packet(&expected, bytes); break;
    case ZERO_NONCE:
        memset(expected.pending.startup_nonce, 0, 32); size = packet(&expected, bytes); break;
    case ZERO_INSTALLATION:
        memset(expected.pending.approved_installation, 0, 32); size = packet(&expected, bytes); break;
    case ZERO_ARTIFACT:
        memset(expected.pending.server_artifact, 0, 32); size = packet(&expected, bytes); break;
    case INVALID_BUILD: expected.pending.build = (st_handoff_build)99;
        size = packet(&expected, bytes); break;
    case INVALID_PID: expected.pending.original_process.pid = UINT32_MAX;
        size = packet(&expected, bytes); break;
    case INVALID_START_FRACTION:
        expected.pending.original_process.start_microseconds = 1000000;
        size = packet(&expected, bytes); break;
    case INVALID_EXPECTED_KIND:
        expected.kind = (st_handoff_frame_kind)99;
        size = packet(&expected, bytes); break;
    case INVALID_EXPECTATION_OPEN:
        /* A valid wire packet cannot repair invalid caller expectations. Reject
         * admission without waiting on this empty, still-open writer.
         */
        expected.pending.uid = 0; script = HOLD_EMPTY; break;
    case READY_EXPECTED_CHALLENGE: expected.challenge[0] = 0x64; break;
    case READY_EXPECTED_MAC: expected.confirmation_mac[0] = 0x75; break;
    case CONFIRM_NOT_READY: {
        st_handoff_frame_expectation other = expectation(ST_HANDOFF_FRAME_CONFIRMATION);
        size = packet(&other, bytes);
        (void)memset_s(&other, sizeof other, 0, sizeof other);
        break;
    }
    case READY_NOT_CONFIRM: {
        st_handoff_frame_expectation other = expectation(ST_HANDOFF_FRAME_READINESS);
        size = packet(&other, bytes);
        (void)memset_s(&other, sizeof other, 0, sizeof other);
        break;
    }
    case WRONG_CHALLENGE: bytes[192] ^= 1; break;
    case WRONG_MAC: bytes[224] ^= 1; break;
    case CONFIRM_OBLIGATIONS: put32(bytes + 28, 0x3f); break;
    case OPEN_EMPTY: case INTERRUPTED_WAIT:
        script = HOLD_EMPTY; wanted = ST_HANDOFF_TIMED_OUT; break;
    case OPEN_PARTIAL: script = HOLD_PARTIAL; wanted = ST_HANDOFF_TIMED_OUT; break;
    case OPEN_READY: case OPEN_CONFIRM:
        script = HOLD_COMPLETE; wanted = ST_HANDOFF_TIMED_OUT; break;
    case EXPIRED_DEADLINE: wanted = ST_HANDOFF_TIMED_OUT; break;
    default: break;
    }

    if (selected == NATIVE_GATE_UNAVAILABLE) {
        st_handoff_result actual = st_handoff_native_admit(&expected.pending);
        (void)memset_s(&expected, sizeof expected, 0, sizeof expected);
        (void)memset_s(bytes, sizeof bytes, 0, sizeof bytes);
        return actual == ST_HANDOFF_NATIVE_UNAVAILABLE ? CASE_GATED : CASE_RED;
    }

    int descriptors[2] = {-1, -1};
    int endpoint = -1;
    pid_t writer = -1;
    if (selected == NONPIPE_ENDPOINT) {
        endpoint = open("/dev/null", O_RDONLY | O_CLOEXEC);
        if (endpoint < 0) return CASE_FIXTURE_ERROR;
    } else if (selected == SOCKET_ENDPOINT) {
        if (socketpair(AF_UNIX, SOCK_STREAM, 0, descriptors) != 0) return CASE_FIXTURE_ERROR;
        endpoint = descriptors[0];
        if (fcntl(endpoint, F_SETFD, FD_CLOEXEC) != 0) {
            (void)close(descriptors[0]); (void)close(descriptors[1]);
            return CASE_FIXTURE_ERROR;
        }
        (void)close(descriptors[1]);
    } else if (selected != NEGATIVE_ENDPOINT && selected != NULL_ENDPOINT) {
        if (!owned_pipe(descriptors)) return CASE_FIXTURE_ERROR;
        if (selected == WRITE_ENDPOINT) {
            endpoint = descriptors[1];
            (void)close(descriptors[0]);
        } else {
            endpoint = descriptors[0];
            if (selected == MISSING_CLOEXEC && fcntl(endpoint, F_SETFD, 0) != 0) {
                (void)close(descriptors[0]); (void)close(descriptors[1]);
                return CASE_FIXTURE_ERROR;
            }
            writer = fork();
            if (writer == 0) {
                (void)close(descriptors[0]);
                if (worker_result_fd >= 0) (void)close(worker_result_fd);
                int result = write_script(descriptors[1], bytes, size, script);
                (void)close(descriptors[1]);
                (void)memset_s(&expected, sizeof expected, 0, sizeof expected);
                (void)memset_s(bytes, sizeof bytes, 0, sizeof bytes);
                _exit(result);
            }
            (void)close(descriptors[1]);
            if (writer < 0) {
                (void)close(descriptors[0]);
                return CASE_FIXTURE_ERROR;
            }
        }
    }

    if (selected == INTERRUPTED_WAIT || selected == SIGNAL_SEMANTICS) {
        struct sigaction action = {.sa_handler = count_signal};
        if (sigemptyset(&action.sa_mask) != 0 ||
            sigaction(selected == INTERRUPTED_WAIT ? SIGALRM : SIGPIPE,
                      &action, NULL) != 0) return CASE_FIXTURE_ERROR;
        if (selected == SIGNAL_SEMANTICS) {
            sigset_t additional;
            if (sigemptyset(&additional) != 0 || sigaddset(&additional, SIGUSR1) != 0 ||
                sigprocmask(SIG_BLOCK, &additional, NULL) != 0) return CASE_FIXTURE_ERROR;
        } else {
            interruptions = 0;
            struct itimerval timer = {
                .it_interval = {.tv_sec = 0, .tv_usec = 5000},
                .it_value = {.tv_sec = 0, .tv_usec = 5000}
            };
            if (setitimer(ITIMER_REAL, &timer, NULL) != 0) return CASE_FIXTURE_ERROR;
        }
    }

    struct sigaction pipe_before, alarm_before, pipe_after, alarm_after;
    sigset_t mask_before, mask_after;
    if (sigaction(SIGPIPE, NULL, &pipe_before) != 0 ||
        sigaction(SIGALRM, NULL, &alarm_before) != 0 ||
        sigprocmask(SIG_SETMASK, NULL, &mask_before) != 0) return CASE_FIXTURE_ERROR;
    uint64_t start = monotonic_ns();
    uint64_t cpu_start = cpu_ns();
    if (start == 0 || cpu_start == 0) return CASE_FIXTURE_ERROR;
    uint64_t deadline = start + RECEIVE_BUDGET_NS;
    if (selected == EXPIRED_DEADLINE) deadline = start - 1;
    if (selected == ZERO_DEADLINE) deadline = 0;
    if (selected == UNBOUNDED_DEADLINE) deadline = UINT64_MAX;

    int original = endpoint;
    st_handoff_candidate output = {.kind = ST_HANDOFF_FRAME_CONFIRMATION};
    st_handoff_result actual = st_handoff_receive_candidate(
        selected == NULL_EXPECTATION ? NULL : &expected,
        selected == NULL_ENDPOINT ? NULL : &endpoint,
        deadline, selected == NULL_OUTPUT ? NULL : &output);
    uint64_t end = monotonic_ns();
    uint64_t cpu_end = cpu_ns();
    bool signal_queries = sigaction(SIGPIPE, NULL, &pipe_after) == 0 &&
                          sigaction(SIGALRM, NULL, &alarm_after) == 0 &&
                          sigprocmask(SIG_SETMASK, NULL, &mask_after) == 0;
    if (selected == INTERRUPTED_WAIT) {
        struct itimerval stopped = {0};
        if (setitimer(ITIMER_REAL, &stopped, NULL) != 0) return CASE_FIXTURE_ERROR;
    }

    bool passed = actual == wanted && signal_queries &&
                  same_action(&pipe_before, &pipe_after) &&
                  same_action(&alarm_before, &alarm_after) &&
                  same_mask(&mask_before, &mask_after) &&
                  end >= start && end - start < UINT64_C(600000000) &&
                  cpu_end >= cpu_start;
    if (selected != NULL_ENDPOINT) {
        passed = passed && endpoint == -1 && (original < 0 || closed_descriptor(original));
    }
    if (selected != NULL_OUTPUT) {
        st_handoff_frame_kind wanted_kind = wanted == ST_HANDOFF_CANDIDATE_MATCH ?
                                          expected.kind : ST_HANDOFF_FRAME_NONE;
        passed = passed && output.kind == wanted_kind;
    }
    if (selected == OPEN_EMPTY || selected == OPEN_PARTIAL || selected == OPEN_READY ||
        selected == OPEN_CONFIRM || selected == INTERRUPTED_WAIT) {
        /* Detect an immediate timeout and a CPU-consuming spin as well as hangs.
         * The supervisor provides the hard outer bound if receive never returns.
         */
        passed = passed && end >= deadline && cpu_end - cpu_start < UINT64_C(50000000);
    }
    if (selected == INVALID_EXPECTATION_OPEN) passed = passed && end < deadline;
    if (selected == INTERRUPTED_WAIT) passed = passed && interruptions > 0;
    if (selected == CONSUMED_ENDPOINT) {
        output.kind = ST_HANDOFF_FRAME_CONFIRMATION;
        st_handoff_result repeated = st_handoff_receive_candidate(
            &expected, &endpoint, monotonic_ns() + RECEIVE_BUDGET_NS, &output);
        passed = passed && repeated == ST_HANDOFF_REJECTED && endpoint == -1 &&
                 output.kind == ST_HANDOFF_FRAME_NONE;
    }
    /* Recover a descriptor if a broken implementation did not consume it, but
     * only while the original handle remains open in this single-threaded case.
     */
    if (original >= 0 && !closed_descriptor(original)) (void)close(original);
    bool writer_ok = writer < 0 || reap_writer(writer);
    (void)memset_s(&expected, sizeof expected, 0, sizeof expected);
    (void)memset_s(bytes, sizeof bytes, 0, sizeof bytes);
    if (!writer_ok || !signal_queries || end == 0 || cpu_end == 0) return CASE_FIXTURE_ERROR;
    return passed ? CASE_PASS : CASE_RED;
}

static bool private_stdio(void)
{
    int descriptor = open("/dev/null", O_RDWR | O_CLOEXEC);
    if (descriptor < 0) return false;
    bool ok = dup2(descriptor, STDIN_FILENO) >= 0 &&
              dup2(descriptor, STDOUT_FILENO) >= 0 &&
              dup2(descriptor, STDERR_FILENO) >= 0;
    if (descriptor > STDERR_FILENO) (void)close(descriptor);
    return ok;
}

static bool bounded_reap(pid_t child, uint64_t deadline, int *status)
{
#ifdef ST_HANDOFF_PRIVATE_REAP_FAULT
    bool inject = private_reap_fixture && stop_signal != 0 && !private_reap_fault_used;
    bool exhausted = false;
    if (inject) {
        private_reap_fault_used = true;
        /* Model a scheduling delay at the native wait boundary. The ordinary
         * loop below still decides that its real deadline was exhausted before
         * observing any child status. Never fabricate a waitpid result.
         */
        for (unsigned int attempt = 0; attempt < 1024; ++attempt) {
            uint64_t now = monotonic_ns();
            if (now == 0) break;
            if (now >= deadline) { exhausted = true; break; }
            if (!pause_ns(UINT64_C(2000000))) break;
        }
    }
#endif
    for (unsigned int attempt = 0; attempt < 1024; ++attempt) {
        uint64_t now = monotonic_ns();
        if (now == 0 || now >= deadline) break;
        pid_t result = waitpid(child, status, WNOHANG);
        if (result == child) return true;
        if (result < 0 && errno != EINTR) return false;
        if (!pause_ns(UINT64_C(2000000))) return false;
    }
#ifdef ST_HANDOFF_PRIVATE_REAP_FAULT
    if (inject && exhausted) {
        const uint8_t uncertain = 'u';
        (void)write_bytes(private_audit_fd, &uncertain, 1);
        /* Fault-only containment: the killed case is still our exact unreaped
         * child. Reap it within a fresh finite bound, but keep this operation's
         * original false result. Main must still handle CASE_CLEANUP_UNRESOLVED.
         */
        uint64_t now = monotonic_ns();
        bool retired = now != 0 && bounded_reap(child, now + REAP_BUDGET_NS, status);
        const uint8_t observation = retired ? 'r' : 'f';
        (void)write_bytes(private_audit_fd, &observation, 1);
    }
#endif
    return false;
}

static case_result supervised(scenario selected)
{
    int descriptors[2];
    if (!owned_pipe(descriptors)) return CASE_FIXTURE_ERROR;
    uint64_t start = monotonic_ns();
    if (start == 0) {
        (void)close(descriptors[0]); (void)close(descriptors[1]);
        return CASE_FIXTURE_ERROR;
    }
    uint64_t deadline = start + CASE_BUDGET_NS;
    pid_t child = fork();
    if (child == 0) {
        (void)close(descriptors[0]);
        worker_result_fd = descriptors[1];
        case_result result = CASE_FIXTURE_ERROR;
        stop_signal = 0;
        if (setpgid(0, 0) == 0 && ordinary_stop_signals() && private_stdio()) {
            result = exercise(selected);
        }
        uint8_t classification = (uint8_t)result;
        write_result written = write_bytes(descriptors[1], &classification, 1);
        (void)close(descriptors[1]);
        _exit(written == WRITE_SENT ? 0 : 2);
    }
    (void)close(descriptors[1]);
#ifdef ST_HANDOFF_PRIVATE_REAP_FAULT
    if (private_reap_fixture && private_ready_fd >= 0) {
        (void)close(private_ready_fd);
        private_ready_fd = -1;
    }
#endif
    if (child < 0) {
        (void)close(descriptors[0]);
        return CASE_FIXTURE_ERROR;
    }
    /* Both sides establish the owned process group before any writer forks.
     * EACCES/ESRCH is not used to authorize a different process/group.
     */
    if (setpgid(child, child) != 0 && errno != EACCES && errno != ESRCH) {
        (void)kill(-child, SIGKILL);
        (void)kill(child, SIGKILL);
        int status = 0;
        bool reaped = bounded_reap(child, deadline, &status);
        (void)close(descriptors[0]);
        return reaped ? CASE_FIXTURE_ERROR : CASE_CLEANUP_UNRESOLVED;
    }
    case_result result = CASE_DEADLINE;
    uint8_t bytes[2] = {0};
    size_t received = 0;
    bool eof = false;
    unsigned int interrupted = 0;
    while (received < sizeof bytes && stop_signal == 0 && monotonic_ns() < deadline) {
        uint64_t now = monotonic_ns();
        if (now == 0 || now >= deadline) break;
        uint64_t remaining_ms = (deadline - now + UINT64_C(999999)) / UINT64_C(1000000);
        struct pollfd ready = {.fd = descriptors[0], .events = POLLIN};
        int polled = poll(&ready, 1, (int)remaining_ms);
        if (polled < 0 && errno == EINTR && ++interrupted <= 64) continue;
        if (polled < 0) { result = CASE_FIXTURE_ERROR; break; }
        if (polled == 0) break;
        ssize_t count = read(descriptors[0], bytes + received, sizeof bytes - received);
        if (count == 0) { eof = true; break; }
        if (count < 0 && errno == EINTR && ++interrupted <= 64) continue;
        if (count < 0) { result = CASE_FIXTURE_ERROR; break; }
        received += (size_t)count;
        if (received == sizeof bytes) { result = CASE_FIXTURE_ERROR; break; }
    }
    (void)close(descriptors[0]);
    int status = 0;
    bool valid = eof && received == 1 && bytes[0] <= CASE_CLEANUP_UNRESOLVED;
    if (eof && (!valid || bytes[0] == CASE_FIXTURE_ERROR)) {
        /* A setup failure may leave a writer. Clean the owned group before
         * reaping its leader, while the leader's ID is still reserved to us.
         */
        (void)kill(-child, SIGKILL);
        (void)kill(child, SIGKILL);
        bool cleaned = bounded_reap(child, monotonic_ns() + REAP_BUDGET_NS, &status);
        return cleaned ? CASE_FIXTURE_ERROR : CASE_CLEANUP_UNRESOLVED;
    }
    bool reaped = valid && bounded_reap(child, deadline, &status);
    if (!reaped) {
        /* The leader is still our unreaped child, so its ID cannot be reused.
         * Kill only this owned group and its exact leader, then bound the reap.
         */
        (void)kill(-child, SIGKILL);
        (void)kill(child, SIGKILL);
        bool cleaned = bounded_reap(child, monotonic_ns() + REAP_BUDGET_NS, &status);
        return cleaned ? result : CASE_CLEANUP_UNRESOLVED;
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) return CASE_FIXTURE_ERROR;
    /* A normal case has reaped its sole writer before sending its result. */
    return (case_result)bytes[0];
}

static void emit(const char *name, const char *result)
{
    printf("{\"schema\":1,\"scope\":\"handoff-wire-candidate\","
           "\"case\":\"%s\",\"result\":\"%s\"}\n", name, result);
    (void)fflush(stdout);
}

int main(int argc, char **argv)
{
    (void)argv;
    bool arguments_ok = argc == 1;
#ifdef ST_HANDOFF_PRIVATE_REAP_FAULT
    if (!arguments_ok) arguments_ok = private_arguments(argc, argv);
#endif
    if (!arguments_ok) {
        emit("arguments", "invalid-arguments");
        return 2;
    }
    if (getuid() == 0 || geteuid() == 0) {
        emit("driver", "privileged-run-refused");
        return 2;
    }
    struct sigaction child_action;
    struct sigaction stopping = {.sa_handler = request_stop};
    if (sigaction(SIGCHLD, NULL, &child_action) != 0 ||
        child_action.sa_handler == SIG_IGN || (child_action.sa_flags & SA_NOCLDWAIT) != 0 ||
        sigemptyset(&stopping.sa_mask) != 0 ||
        sigaddset(&stopping.sa_mask, SIGINT) != 0 ||
        sigaddset(&stopping.sa_mask, SIGTERM) != 0 ||
        sigaction(SIGINT, &stopping, NULL) != 0 ||
        sigaction(SIGTERM, &stopping, NULL) != 0) {
        emit("driver", "fixture-error");
        return 2;
    }
    struct rlimit no_core = {.rlim_cur = 0, .rlim_max = 0};
    if (setrlimit(RLIMIT_CORE, &no_core) != 0) {
        emit("driver", "fixture-error");
        return 2;
    }
    uint64_t start = monotonic_ns();
    if (start == 0) {
        emit("driver", "fixture-error");
        return 2;
    }
    const test_case *selected_cases = cases;
    size_t count = sizeof cases / sizeof cases[0];
#ifdef ST_HANDOFF_PRIVATE_REAP_FAULT
    static const test_case fault_case[] = {
        {"private-cancellation-reap-fault", PRIVATE_CANCELLATION_REAP_FAULT}
    };
    if (private_reap_fixture) {
        selected_cases = fault_case;
        count = 1;
        /* Already-reaped regressions reach Python's actual wait through EOF.
         * Cleanup precedence must use a fixed supervisor outcome, not depend on
         * a native JSON record that cancellation can bypass or never read.
         */
        (void)close(STDOUT_FILENO);
    }
#endif
    bool red = false, failed = false, cleanup_unresolved = false;
    for (size_t i = 0; i < count && stop_signal == 0; ++i) {
        case_result result = monotonic_ns() - start >= SUITE_BUDGET_NS ?
                             CASE_DEADLINE : supervised(selected_cases[i].scenario);
        /* Preserve the owned case's failed retirement before interruption can
         * discard its record or restore ordinary terminating-signal semantics.
         */
        if (result == CASE_CLEANUP_UNRESOLVED) cleanup_unresolved = true;
        if (stop_signal != 0) break;
        switch (result) {
        case CASE_PASS: emit(selected_cases[i].name, "pass"); break;
        case CASE_RED: emit(selected_cases[i].name, "red"); red = true; break;
        case CASE_GATED: emit(selected_cases[i].name, "gated"); break;
        case CASE_FIXTURE_ERROR: emit(selected_cases[i].name, "fixture-error"); failed = true; break;
        case CASE_DEADLINE: emit(selected_cases[i].name, "deadline"); failed = true; break;
        case CASE_CLEANUP_UNRESOLVED:
            emit(selected_cases[i].name, "cleanup-unresolved"); failed = true; break;
        }
    }
    if (cleanup_unresolved) return ST_HANDOFF_TEST_CLEANUP_UNRESOLVED_EXIT;
    if (stop_signal != 0) {
        int number = stop_signal;
        /* supervised cleaned and bounded the reap before restoring the normal
         * terminating signal. This also lets the outer task cancel a case group.
         */
        (void)ordinary_stop_signals();
        sigset_t unblocked;
        (void)sigemptyset(&unblocked);
        (void)sigaddset(&unblocked, number);
        (void)sigprocmask(SIG_UNBLOCK, &unblocked, NULL);
        (void)raise(number);
        return 128 + number;
    }
    return failed ? 2 : red ? 1 : 0;
}
