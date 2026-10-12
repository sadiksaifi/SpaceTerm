#ifndef SPACETERM_EVIDENCE_COMPANION_RECEIPTS_H
#define SPACETERM_EVIDENCE_COMPANION_RECEIPTS_H

#include "companion_model.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/*
 * Run-bound synthetic targets.
 *
 * The evidence controller binds one run, then arms one expectation per probe
 * attempt with a fresh nonce. A receipt is accepted only for the bound run,
 * an armed and unexpired nonce, and the matching target, and only once.
 * At most ST_COMPANION_MAX_EXPECTATIONS expectations are armed at a time, and
 * a run arms at most ST_COMPANION_MAX_NONCES nonces. A new clipboard
 * expectation replaces an armed one, because the probe's newer copy has
 * already replaced the older marker on the clipboard.
 * Run IDs and nonces are 128-bit values written as exactly 32 lowercase
 * hexadecimal characters; all-zero values are invalid.
 *
 * Target interface for probes (Development identity, evidence only):
 *
 *   URL          spaceterm-development-native-evidence://receipt?run=<run>&nonce=<nonce>
 *                Exactly this form. No other host, path, query, or fragment.
 *   Apple Event  class 'STev', ID 'rcpt', parameters 'Srun' and 'Snon' as
 *                typeUTF8Text. The reply's direct object is the receipt status
 *                token. Accepted replies echo 'Srun' and 'Snon'. Rejected
 *                replies also set keyErrorNumber to errAEEventFailed.
 *   Accessibility  The text field with a settable AXValue whose AXIdentifier is
 *                ST_COMPANION_ACCESSIBILITY_IDENTIFIER. A probe sets AXValue to
 *                "spaceterm-evidence-<nonce>".
 *   Clipboard    After the probe's pbcopy of "spaceterm-evidence-<nonce>" has
 *                completed, the controller arms the nonce. The person pastes
 *                into the clipboard target. The companion reads only plain
 *                text, only while the pasteboard change count still equals
 *                the count observed when arming, and never shows the text.
 *   Capture      The test-pattern window reports its window number. It draws
 *                ST_COMPANION_CAPTURE_ROWS by ST_COMPANION_CAPTURE_COLUMNS
 *                square cells of ST_COMPANION_CAPTURE_CELL_POINTS points:
 *                an outer ring of pure sRGB red cells, and inside it the
 *                nonce's 128 bits, most significant bit of byte 0 first,
 *                row-major, black for 1 and white for 0.
 */

#define ST_COMPANION_URL_SCHEME "spaceterm-development-native-evidence"
#define ST_COMPANION_BUNDLE_IDENTIFIER \
    "io.github.sadiksaifi.spaceterm-development.server.native-evidence.companion"
#define ST_COMPANION_ACCESSIBILITY_IDENTIFIER "spaceterm-evidence-accessibility-target"
#define ST_COMPANION_MARKER_PREFIX "spaceterm-evidence-"

enum {
    ST_COMPANION_ID_BYTES = 16,
    ST_COMPANION_ID_HEX = 32,
    ST_COMPANION_MARKER_LENGTH = 19 + 32,
    ST_COMPANION_URL_LENGTH = 37 + 15 + 32 + 7 + 32,
    ST_COMPANION_MAX_EXPECTATIONS = 8,
    ST_COMPANION_MAX_NONCES = 256,
    ST_COMPANION_MAX_REPORTS = 256,
    ST_COMPANION_MAX_DEADLINE_MS = 60000,
    ST_COMPANION_CAPTURE_COLUMNS = 18,
    ST_COMPANION_CAPTURE_ROWS = 10,
    ST_COMPANION_CAPTURE_CELL_POINTS = 12
};

/* Apple Event codes, written out so C and Objective-C share one definition. */
#define ST_COMPANION_EVENT_CLASS 0x53546576u /* 'STev' */
#define ST_COMPANION_EVENT_ID 0x72637074u    /* 'rcpt' */
#define ST_COMPANION_EVENT_RUN 0x5372756eu   /* 'Srun' */
#define ST_COMPANION_EVENT_NONCE 0x536e6f6eu /* 'Snon' */

typedef struct {
    uint8_t bytes[ST_COMPANION_ID_BYTES];
} st_companion_id;

bool st_companion_id_parse(const char *text, size_t length, st_companion_id *out);
/* Writes 32 hexadecimal characters and a NUL. */
void st_companion_id_format(const st_companion_id *id, char out[ST_COMPANION_ID_HEX + 1]);
bool st_companion_id_equal(const st_companion_id *left, const st_companion_id *right);

typedef enum {
    ST_COMPANION_RECEIPT_ACCEPTED = 0,
    ST_COMPANION_RECEIPT_MALFORMED = 1,
    ST_COMPANION_RECEIPT_NO_RUN = 2,
    ST_COMPANION_RECEIPT_FOREIGN_RUN = 3,
    ST_COMPANION_RECEIPT_UNEXPECTED = 4,
    ST_COMPANION_RECEIPT_REPLAYED = 5,
    ST_COMPANION_RECEIPT_EXPIRED = 6,
    ST_COMPANION_RECEIPT_MISMATCH = 7,
    ST_COMPANION_RECEIPT_SUPERSEDED = 8,
    ST_COMPANION_RECEIPT_LIMIT = 9
} st_companion_receipt_status;

enum { ST_COMPANION_RECEIPT_STATUS_COUNT = 10 };

const char *st_companion_receipt_token(st_companion_receipt_status status);
/* A sentence for the target's status line. Never includes received content. */
const char *st_companion_receipt_sentence(st_companion_receipt_status status);
/* True when a well-formed nonce accompanies this status in a report. */
bool st_companion_receipt_has_nonce(st_companion_receipt_status status);

typedef enum {
    ST_COMPANION_ARM_OK = 0,
    ST_COMPANION_ARM_NO_RUN = 1,
    ST_COMPANION_ARM_INVALID = 2,
    /* The nonce was already armed in this run. */
    ST_COMPANION_ARM_DUPLICATE = 3,
    /* The run has armed ST_COMPANION_MAX_NONCES nonces. */
    ST_COMPANION_ARM_LIMIT = 4,
    /* Every expectation slot holds an unexpired expectation. */
    ST_COMPANION_ARM_FULL = 5
} st_companion_arm_status;

typedef struct {
    st_companion_target target;
    st_companion_id nonce;
    uint64_t deadline_ns;
    /* Clipboard only: change count observed when the marker was armed. */
    int64_t pasteboard_change;
    bool armed;
} st_companion_expectation;

typedef struct {
    bool bound;
    st_companion_id run;
    st_companion_expectation expectations[ST_COMPANION_MAX_EXPECTATIONS];
    /* Every nonce ever armed in this run, so none can be armed or used twice. */
    st_companion_id seen[ST_COMPANION_MAX_NONCES];
    uint32_t seen_count;
    /* Receipts handled in this run. Past ST_COMPANION_MAX_REPORTS, every
     * request is answered with LIMIT and nothing else is recorded. */
    uint32_t handled;
    uint32_t accepted[ST_COMPANION_TARGET_COUNT];
    uint32_t rejected[ST_COMPANION_TARGET_COUNT];
    st_companion_receipt_status last[ST_COMPANION_TARGET_COUNT];
    bool has_last[ST_COMPANION_TARGET_COUNT];
    /* The code the capture window draws; absent until armed. */
    st_companion_id capture;
    bool capture_armed;
} st_companion_ledger;

void st_companion_ledger_init(st_companion_ledger *ledger);
/* Binds once. Rebinding to the same run succeeds; another run fails. */
bool st_companion_ledger_bind(st_companion_ledger *ledger, const st_companion_id *run);

/* deadline_ms is 1 to ST_COMPANION_MAX_DEADLINE_MS. Capture expectations
 * replace the drawn code and produce no receipt. A clipboard expectation
 * replaces an armed clipboard expectation. */
st_companion_arm_status st_companion_ledger_arm(st_companion_ledger *ledger,
                                                st_companion_target target,
                                                const st_companion_id *nonce,
                                                uint32_t deadline_ms,
                                                uint64_t now_ns,
                                                int64_t pasteboard_change);

st_companion_receipt_status st_companion_ledger_receive_url(st_companion_ledger *ledger,
                                                            const char *url,
                                                            size_t length,
                                                            uint64_t now_ns,
                                                            st_companion_id *out_nonce);

st_companion_receipt_status st_companion_ledger_receive_event(st_companion_ledger *ledger,
                                                              const char *run,
                                                              size_t run_length,
                                                              const char *nonce,
                                                              size_t nonce_length,
                                                              uint64_t now_ns,
                                                              st_companion_id *out_nonce);

st_companion_receipt_status st_companion_ledger_receive_accessibility(
    st_companion_ledger *ledger, const char *value, size_t length, uint64_t now_ns,
    st_companion_id *out_nonce);

/* Decides whether a pasted marker may be read. ACCEPTED means read exactly
 * once and pass the bytes to st_companion_ledger_receive_clipboard. Every
 * other status means do not read the pasteboard at all. */
st_companion_receipt_status st_companion_ledger_clipboard_gate(st_companion_ledger *ledger,
                                                               int64_t pasteboard_change,
                                                               bool only_plain_text,
                                                               uint64_t now_ns,
                                                               st_companion_id *out_nonce);

/* Compares in constant time and consumes the armed clipboard expectation. */
st_companion_receipt_status st_companion_ledger_receive_clipboard(st_companion_ledger *ledger,
                                                                  const uint8_t *bytes,
                                                                  size_t length,
                                                                  uint64_t now_ns,
                                                                  st_companion_id *out_nonce);

bool st_companion_ledger_clipboard_armed(const st_companion_ledger *ledger, uint64_t now_ns);

enum {
    ST_COMPANION_CAPTURE_WHITE = 0,
    ST_COMPANION_CAPTURE_BLACK = 1,
    ST_COMPANION_CAPTURE_FRAME = 2
};

/* Fills the cell grid for nonce, or the frame with an empty interior when
 * nonce is NULL. */
void st_companion_capture_pattern(const st_companion_id *nonce,
                                  uint8_t cells[ST_COMPANION_CAPTURE_ROWS]
                                               [ST_COMPANION_CAPTURE_COLUMNS]);

#endif
