#ifndef SPACETERM_EVIDENCE_COMPANION_PROTOCOL_H
#define SPACETERM_EVIDENCE_COMPANION_PROTOCOL_H

#include "companion_authorization.h"
#include "companion_model.h"
#include "companion_receipts.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/*
 * Controller channel, schema 1.
 *
 * The unprivileged evidence controller creates a fresh private directory
 * (owned by the account, mode 0700), listens on a Unix stream socket inside
 * it, and launches the companion through LaunchServices with:
 *
 *   --run-id <32 lowercase hex> --controller-socket <absolute socket path>
 *
 * The companion verifies the directory and socket ownership and the peer's
 * UID, then exchanges newline-terminated ASCII records. Each record is a kind
 * followed by its keys in the fixed order below, separated by single spaces,
 * at most ST_COMPANION_MAX_RECORD bytes including the newline. Values use
 * only [a-z0-9-]. Unknown kinds, keys, values, orders, or extra fields close
 * the channel. Records carry public IDs, codes, and classes only.
 *
 * Companion to controller:
 *   hello schema=1 run=<id> pid=<n>
 *   targets capture-window=<n>
 *   request seq=<n> kind=probe capability=<1-13>
 *   request seq=<n> kind=stop-run
 *   armed target=<target> nonce=<id>
 *   refused target=<target> nonce=<id> reason=<inactive|duplicate|full|limit>
 *   receipt target=<target> status=<status> [nonce=<id>]
 *   authorization operation=<operation> outcome=<outcome>
 *   closing reason=<quit>
 *
 * Controller to companion:
 *   welcome schema=1 run=<id>
 *   run state=<active|stopping|stopped|cleanup-unresolved|failed>
 *   expect target=<target> nonce=<id> deadline-ms=<1-60000>
 *   progress capability=<n> context=<context> phase=<queued|running|waiting-for-person>
 *   result capability=<n> context=<context> result=<result> responsible=<process>
 *          preflight=<preflight> effect=<effect> cleanup=<cleanup>
 *   ack seq=<n> disposition=<accepted|busy|stopping|unsupported>
 *
 * "armed" follows an accepted "expect" after the target window is presented
 * and, for capture, after the pattern for that nonce has been drawn. The
 * controller starts a probe for that target only after it reads "armed".
 *
 * An "expect" the companion cannot arm gets "refused" and the channel stays
 * open. The controller must not start that probe. Reasons:
 *   inactive   the run is not active or stop-requested
 *   duplicate  the nonce was already armed in this run
 *   full       ST_COMPANION_MAX_EXPECTATIONS (8) expectations are armed and
 *              unexpired; one frees when it is used or its deadline passes
 *   limit      the run already armed ST_COMPANION_MAX_NONCES (256) nonces
 * A clipboard "expect" replaces an armed clipboard expectation instead of
 * being refused, because the probe's newer copy supersedes the older marker.
 * Only malformed or out-of-order records close the channel.
 * The result record is one line. A passed result requires effect=observed.
 * "closing" only says the companion is exiting. It never claims that the run
 * stopped or that anything was cleaned up.
 */

enum {
    ST_COMPANION_PROTOCOL_SCHEMA = 1,
    ST_COMPANION_MAX_RECORD = 256,
    ST_COMPANION_MAX_SEQUENCE = 1000000
};

typedef enum {
    ST_COMPANION_PHASE_NONE = 0,
    ST_COMPANION_PHASE_REQUESTED = 1,
    ST_COMPANION_PHASE_QUEUED = 2,
    ST_COMPANION_PHASE_RUNNING = 3,
    ST_COMPANION_PHASE_WAITING_FOR_PERSON = 4
} st_companion_phase;

typedef enum {
    ST_COMPANION_PREFLIGHT_UNKNOWN = 0,
    ST_COMPANION_PREFLIGHT_GRANTED = 1,
    ST_COMPANION_PREFLIGHT_DENIED = 2,
    ST_COMPANION_PREFLIGHT_NOT_DETERMINED = 3,
    ST_COMPANION_PREFLIGHT_RESTRICTED = 4,
    ST_COMPANION_PREFLIGHT_NOT_APPLICABLE = 5,
    ST_COMPANION_PREFLIGHT_UNAVAILABLE = 6
} st_companion_preflight;

typedef enum {
    ST_COMPANION_OBSERVED_UNKNOWN = 0,
    ST_COMPANION_OBSERVED_EFFECT = 1,
    ST_COMPANION_OBSERVED_BLOCKED = 2,
    ST_COMPANION_OBSERVED_ABSENT = 3,
    ST_COMPANION_OBSERVED_NOT_ATTEMPTED = 4
} st_companion_observed;

typedef enum {
    ST_COMPANION_CLEANUP_UNKNOWN = 0,
    ST_COMPANION_CLEANUP_COMPLETE = 1,
    ST_COMPANION_CLEANUP_NOT_NEEDED = 2,
    ST_COMPANION_CLEANUP_UNRESOLVED = 3
} st_companion_cleanup;

typedef enum {
    ST_COMPANION_ACK_ACCEPTED = 1,
    ST_COMPANION_ACK_BUSY = 2,
    ST_COMPANION_ACK_STOPPING = 3,
    ST_COMPANION_ACK_UNSUPPORTED = 4
} st_companion_ack;

typedef enum {
    ST_COMPANION_REFUSAL_INACTIVE = 1,
    ST_COMPANION_REFUSAL_DUPLICATE = 2,
    ST_COMPANION_REFUSAL_FULL = 3,
    ST_COMPANION_REFUSAL_LIMIT = 4
} st_companion_refusal;

/* The refusal for an arm status the controller can cause, or 0 for OK and
 * for statuses that only a protocol violation can produce. */
st_companion_refusal st_companion_refusal_for_arm(st_companion_arm_status status);
const char *st_companion_refusal_token(st_companion_refusal refusal);

typedef struct {
    st_companion_result result;
    st_companion_process responsible;
    st_companion_preflight preflight;
    st_companion_observed effect;
    st_companion_cleanup cleanup;
    st_companion_phase phase;
    bool reported;
} st_companion_observation;

typedef enum {
    ST_COMPANION_INBOUND_WELCOME = 1,
    ST_COMPANION_INBOUND_RUN = 2,
    ST_COMPANION_INBOUND_EXPECT = 3,
    ST_COMPANION_INBOUND_PROGRESS = 4,
    ST_COMPANION_INBOUND_RESULT = 5,
    ST_COMPANION_INBOUND_ACK = 6
} st_companion_inbound_kind;

typedef struct {
    st_companion_inbound_kind kind;
    union {
        struct {
            st_companion_id run;
        } welcome;
        struct {
            st_companion_run_state state;
        } run;
        struct {
            st_companion_target target;
            st_companion_id nonce;
            uint32_t deadline_ms;
        } expect;
        struct {
            uint32_t capability;
            st_companion_context context;
            st_companion_phase phase;
        } progress;
        struct {
            uint32_t capability;
            st_companion_context context;
            st_companion_observation observation;
        } result;
        struct {
            uint32_t sequence;
            st_companion_ack disposition;
        } ack;
    } value;
} st_companion_inbound;

/* line excludes the newline. Returns false for anything not exactly valid. */
bool st_companion_protocol_parse(const char *line, size_t length, st_companion_inbound *out);

/* Encoders return the record length including the newline, or 0 when an
 * argument is invalid or capacity is short. Output is NUL-terminated. */
size_t st_companion_encode_hello(char *out, size_t capacity, const st_companion_id *run,
                                 uint32_t pid);
size_t st_companion_encode_targets(char *out, size_t capacity, uint32_t capture_window);
size_t st_companion_encode_probe_request(char *out, size_t capacity, uint32_t sequence,
                                         uint32_t capability);
size_t st_companion_encode_stop_request(char *out, size_t capacity, uint32_t sequence);
size_t st_companion_encode_armed(char *out, size_t capacity, st_companion_target target,
                                 const st_companion_id *nonce);
size_t st_companion_encode_refused(char *out, size_t capacity, st_companion_target target,
                                   const st_companion_id *nonce, st_companion_refusal refusal);
size_t st_companion_encode_receipt(char *out, size_t capacity, st_companion_target target,
                                   st_companion_receipt_status status,
                                   const st_companion_id *nonce);
size_t st_companion_encode_authorization(char *out, size_t capacity,
                                         st_companion_operation operation,
                                         st_companion_auth_outcome outcome);
size_t st_companion_encode_closing(char *out, size_t capacity);

const char *st_companion_target_token(st_companion_target target);
const char *st_companion_context_token(st_companion_context context);
const char *st_companion_preflight_token(st_companion_preflight preflight);
const char *st_companion_observed_token(st_companion_observed observed);
const char *st_companion_cleanup_token(st_companion_cleanup cleanup);
const char *st_companion_process_token(st_companion_process process);

/* Splits a byte stream into records. A record longer than the bound, a NUL,
 * or a carriage return poisons the framer permanently. */
typedef struct {
    char buffer[ST_COMPANION_MAX_RECORD];
    size_t length;
    bool poisoned;
} st_companion_framer;

typedef enum {
    ST_COMPANION_FRAME_NEED_MORE = 0,
    ST_COMPANION_FRAME_RECORD = 1,
    ST_COMPANION_FRAME_POISONED = 2
} st_companion_frame_status;

void st_companion_framer_init(st_companion_framer *framer);
/* Consumes bytes up to and including the first newline. *consumed reports how
 * many bytes were used. On RECORD, *record and *record_length describe the
 * line without its newline; they stay valid until the next call. */
st_companion_frame_status st_companion_framer_push(st_companion_framer *framer,
                                                   const uint8_t *bytes,
                                                   size_t length,
                                                   size_t *consumed,
                                                   const char **record,
                                                   size_t *record_length);

#endif
