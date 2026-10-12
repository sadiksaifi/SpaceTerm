#ifndef SPACETERM_MACOS_HANDOFF_PROTOTYPE_H
#define SPACETERM_MACOS_HANDOFF_PROTOTYPE_H

#include <stdint.h>

/*
 * Wire-candidate prototype: recognize one context-bound frame on an owned pipe.
 * CANDIDATE_MATCH means wire conformance only, never readiness, admission,
 * execution approval, credential delivery, or native enforcement evidence.
 * There is no executable path, command, credential query, or launch operation.
 * No Keychain API or policy snapshot operation belongs to this interface.
 */
typedef enum {
    ST_HANDOFF_CANDIDATE_MATCH = 0,
    ST_HANDOFF_REJECTED,
    ST_HANDOFF_TIMED_OUT,
    ST_HANDOFF_PEER_CLOSED,
    ST_HANDOFF_INTERRUPTED,
    ST_HANDOFF_NATIVE_UNAVAILABLE
} st_handoff_result;

typedef enum {
    ST_HANDOFF_FRAME_NONE = 0,
    ST_HANDOFF_FRAME_READINESS = 1,
    ST_HANDOFF_FRAME_CONFIRMATION = 2
} st_handoff_frame_kind;

typedef enum {
    ST_HANDOFF_BUILD_RELEASE = 1,
    ST_HANDOFF_BUILD_PREFLIGHT = 2,
    ST_HANDOFF_BUILD_DEVELOPMENT = 3
} st_handoff_build;

enum {
    ST_HANDOFF_BINDING_BYTES = 32,
    ST_HANDOFF_READINESS_BYTES = 192,
    ST_HANDOFF_CONFIRMATION_BYTES = 256,
    ST_HANDOFF_MAX_FRAME_BYTES = 256,
    ST_HANDOFF_MAX_INTERRUPTS = 64
};

#define ST_HANDOFF_MAX_WAIT_NS UINT64_C(2000000000)

/* Test-supervisor termination interface only, never a native admission result.
 * The Python task maps this distinct status to cleanup-unresolved before signal
 * precedence, including after a consuming wait and without any JSON record.
 * Ordinary wire failures keep exit 2; successful cancellation keeps its signal.
 */
#define ST_HANDOFF_TEST_CLEANUP_UNRESOLVED_EXIT 125

/* These are mandatory post-exec declarations in the readiness frame, not proof
 * that the process performed them. Native admission must independently check
 * groups, real/effective/saved GID/UID, current code, and available protection
 * evidence. Pre-exec mlock/core settings cannot discharge post-exec obligations.
 */
enum {
    ST_HANDOFF_GROUPS_CLEARED = 1u << 0,
    ST_HANDOFF_GID_DROPPED = 1u << 1,
    ST_HANDOFF_UID_DROPPED = 1u << 2,
    ST_HANDOFF_CORE_DISABLED = 1u << 3,
    ST_HANDOFF_SECRET_BUFFER_LOCKED = 1u << 4,
    ST_HANDOFF_POST_EXEC = 1u << 5,
    ST_HANDOFF_POST_EXEC_OBLIGATIONS = 0x3fu
};

typedef struct {
    uint32_t pid;
    uint64_t start_seconds;
    uint32_t start_microseconds;
} st_handoff_process_identity;

/* Borrowed, immutable pending-startup expectations. A PID alone is not an
 * identity. Every field is bound by the wire check, including the original
 * service-tracked launcher's PID/start tuple across exec. A forked replacement
 * server must not match it. Nonces and generations are not reused.
 *
 * Tests supply synthetic registration bindings. Production must obtain these
 * values from protected account/build registration and native identity capture,
 * never environment values, SUDO_UID, path discovery, or an untrusted client.
 * These bytes are data, not an AdmissionTicket or an installation approval.
 * Filesystem snapshots/timestamps cannot substitute for authenticated copies,
 * root-owned immutable approved installations, or serialized mutation checks.
 */
typedef struct {
    uint32_t uid;
    uint32_t gid;
    st_handoff_build build;
    uint64_t registration_generation;
    uint8_t approved_installation[ST_HANDOFF_BINDING_BYTES];
    uint8_t server_artifact[ST_HANDOFF_BINDING_BYTES];
    uint8_t startup_nonce[ST_HANDOFF_BINDING_BYTES];
    st_handoff_process_identity original_process;
} st_handoff_pending_context;

typedef struct {
    st_handoff_pending_context pending;
    st_handoff_frame_kind kind;
    /* Zero for readiness. For confirmation, these private expected bytes must
     * come from the original delivery transcript. Tests use synthetic bytes.
     * Comparing supplied bytes here is not a cryptographic/native delivery gate.
     * The caller wipes application-owned challenge/MAC storage after use.
     */
    uint8_t challenge[ST_HANDOFF_BINDING_BYTES];
    uint8_t confirmation_mac[ST_HANDOFF_BINDING_BYTES];
} st_handoff_frame_expectation;

/* A bounded, nonauthorizing observation. No context, payload, challenge, MAC,
 * path, errno, or native message escapes in a result. Failure sets kind to NONE.
 */
typedef struct {
    st_handoff_frame_kind kind;
} st_handoff_candidate;

/*
 * Wire version 1 uses octets, not a serialized C struct. All integers are
 * unsigned big-endian. No variable strings, lengths, extensions, or other frame
 * kinds are admitted. Both frames have this 192-byte prefix:
 *
 *   0..3    literal ASCII STH1
 *   4       version 1
 *   5       type: 1 readiness, 2 confirmation
 *   6..7    zero
 *   8..11   exact frame size: 192 or 256
 *   12..15  zero
 *   16..19  owning UID (nonzero, not UINT32_MAX)
 *   20..23  registered primary GID (not UINT32_MAX)
 *   24..27  build: 1 Release, 2 Preflight, 3 Development
 *   28..31  readiness obligations exactly 0x3f; zero for confirmation
 *   32..39  registration generation (nonzero)
 *   40..71  approved installation identity (not all zero)
 *   72..103 approved server artifact identity (not all zero)
 *   104..135 startup nonce (not all zero)
 *   136..143 original process PID (1..INT32_MAX)
 *   144..151 original process start seconds (nonzero)
 *   152..155 original process start microseconds (less than 1000000)
 *   156..191 zero
 *
 * Confirmation adds the private challenge at 192..223 and MAC at 224..255.
 * Match the exact expected kind, every context field, challenge, and MAC. A
 * readiness expectation has zero challenge/MAC. Caller expectations must also
 * obey the context bounds. A truncated, extra, unknown, or differently typed
 * frame is REJECTED, not readiness. An empty closed pipe is PEER_CLOSED.
 *
 * The writer sends exactly one frame and closes its endpoint. Recognition
 * requires EOF, so delayed trailing bytes cannot be mistaken for readiness.
 * Partial/open pipes time out against the original absolute CLOCK_MONOTONIC
 * deadline. No relative-deadline reset on EINTR. At most MAX_INTERRUPTS retries;
 * exhaustion is INTERRUPTED. An expired deadline is TIMED_OUT; zero, overflow,
 * or a deadline more than MAX_WAIT_NS ahead is REJECTED.
 *
 * owned_read_end points to the sole owned read endpoint of an anonymous pipe
 * created with pipe(), with FD_CLOEXEC already set. Ownership is transferred on
 * every call, including failure: clear the caller's descriptor to -1 and close
 * it exactly once. No API duplicates or exports an endpoint. The caller must
 * not dup, copy, concurrently use, or retain aliases of the descriptor. C/POSIX
 * cannot enforce that ownership precondition against an arbitrary caller.
 * Inspect fstat, read-only access mode, and FD_CLOEXEC before reading. Reject a
 * missing/negative descriptor, a write endpoint, a non-pipe descriptor, or missing
 * CLOEXEC. Never change file-status flags, duplicate the endpoint, or retry close.
 * Anonymous-pipe provenance is a caller precondition, not something fstat alone
 * proves. The owning launcher must create the pipes and close all unrelated
 * endpoints before exec; this decoder does not launch it.
 *
 * Validate expectations and the original deadline before consuming frame bytes.
 * expected/out_candidate must be readable/writable, respectively, and remain
 * valid for the call. NULL arguments are rejected but still consume a supplied
 * descriptor. On every failure clear out_candidate, if supplied. No signal
 * handlers/masks/dispositions may be changed. No child may be spawned or reaped.
 * Buffer reads are bounded by MAX_FRAME_BYTES plus one trailing-byte probe.
 * Never emit or persist frame bytes. Wipe mutable frame/challenge/MAC copies.
 */
st_handoff_result st_handoff_receive_candidate(
    const st_handoff_frame_expectation *expected,
    int *owned_read_end,
    uint64_t monotonic_deadline_ns,
    st_handoff_candidate *out_candidate);

/* Reserved native boundary, deliberately unavailable in this source-only
 * increment. Never promote a candidate to native readiness. Future admission
 * needs protected registration/Stopped checks before loading and before writing,
 * live original-parent identity before and after code validation, exact signed
 * artifact/runtime/entitlements, post-exec protection, and group/GID/UID checks.
 * The tracked launcher must exec the server, not fork it and exit. Delivery,
 * replay/stale-completion fencing, cryptographic confirmation, wiping, and
 * retriever reaping need a separate owned native exchange. No positive native
 * result or secret operation is exposed here. Root/signed evidence is gated.
 */
st_handoff_result st_handoff_native_admit(
    const st_handoff_pending_context *pending);

#endif
