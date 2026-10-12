#include "companion_receipts.h"

#include <string.h>

static const char url_prefix[] = ST_COMPANION_URL_SCHEME "://receipt?run=";
static const char url_nonce[] = "&nonce=";
static const char marker_prefix[] = ST_COMPANION_MARKER_PREFIX;

_Static_assert(sizeof url_prefix - 1 + ST_COMPANION_ID_HEX + sizeof url_nonce - 1 +
                       ST_COMPANION_ID_HEX ==
                   ST_COMPANION_URL_LENGTH,
               "URL length must match the fixed form");
_Static_assert(sizeof marker_prefix - 1 + ST_COMPANION_ID_HEX == ST_COMPANION_MARKER_LENGTH,
               "marker length must match the fixed form");

static int hex_value(char c)
{
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    return -1;
}

bool st_companion_id_parse(const char *text, size_t length, st_companion_id *out)
{
    if (text == NULL || out == NULL || length != ST_COMPANION_ID_HEX) return false;
    st_companion_id parsed;
    uint8_t any = 0;
    for (size_t i = 0; i < ST_COMPANION_ID_BYTES; ++i) {
        int high = hex_value(text[2 * i]);
        int low = hex_value(text[2 * i + 1]);
        if (high < 0 || low < 0) return false;
        parsed.bytes[i] = (uint8_t)((high << 4) | low);
        any |= parsed.bytes[i];
    }
    if (any == 0) return false;
    *out = parsed;
    return true;
}

void st_companion_id_format(const st_companion_id *id, char out[ST_COMPANION_ID_HEX + 1])
{
    static const char digits[] = "0123456789abcdef";
    for (size_t i = 0; i < ST_COMPANION_ID_BYTES; ++i) {
        out[2 * i] = digits[id->bytes[i] >> 4];
        out[2 * i + 1] = digits[id->bytes[i] & 0x0f];
    }
    out[ST_COMPANION_ID_HEX] = '\0';
}

bool st_companion_id_equal(const st_companion_id *left, const st_companion_id *right)
{
    uint8_t difference = 0;
    for (size_t i = 0; i < ST_COMPANION_ID_BYTES; ++i) {
        difference |= (uint8_t)(left->bytes[i] ^ right->bytes[i]);
    }
    return difference == 0;
}

static bool id_is_zero(const st_companion_id *id)
{
    uint8_t any = 0;
    for (size_t i = 0; i < ST_COMPANION_ID_BYTES; ++i) any |= id->bytes[i];
    return any == 0;
}

static const char *const receipt_tokens[ST_COMPANION_RECEIPT_STATUS_COUNT] = {
    "accepted", "malformed", "no-run", "foreign-run", "unexpected",
    "replayed", "expired",   "mismatch", "superseded", "limit",
};

static const char *const receipt_sentences[ST_COMPANION_RECEIPT_STATUS_COUNT] = {
    "Receipt accepted for this evidence run.",
    "Ignored a request that does not match the test format.",
    "Ignored a request because no evidence run is connected.",
    "Ignored a request for a different evidence run.",
    "Ignored a request that this evidence run did not expect.",
    "Ignored a repeated request.",
    "Ignored a request that arrived after its deadline.",
    "Did not accept a value that differs from the expected marker.",
    "Did not read the clipboard, because it changed after the probe copied its marker.",
    "Ignored a request because this run reached its receipt limit.",
};

const char *st_companion_receipt_token(st_companion_receipt_status status)
{
    if ((unsigned)status >= ST_COMPANION_RECEIPT_STATUS_COUNT) return "malformed";
    return receipt_tokens[status];
}

const char *st_companion_receipt_sentence(st_companion_receipt_status status)
{
    if ((unsigned)status >= ST_COMPANION_RECEIPT_STATUS_COUNT) {
        return receipt_sentences[ST_COMPANION_RECEIPT_MALFORMED];
    }
    return receipt_sentences[status];
}

bool st_companion_receipt_has_nonce(st_companion_receipt_status status)
{
    /* Only nonces the controller armed are echoed; others are sender-chosen. */
    return status == ST_COMPANION_RECEIPT_ACCEPTED || status == ST_COMPANION_RECEIPT_REPLAYED ||
           status == ST_COMPANION_RECEIPT_EXPIRED || status == ST_COMPANION_RECEIPT_MISMATCH ||
           status == ST_COMPANION_RECEIPT_SUPERSEDED;
}

void st_companion_ledger_init(st_companion_ledger *ledger)
{
    memset(ledger, 0, sizeof *ledger);
}

bool st_companion_ledger_bind(st_companion_ledger *ledger, const st_companion_id *run)
{
    if (ledger == NULL || run == NULL || id_is_zero(run)) return false;
    if (ledger->bound) return st_companion_id_equal(&ledger->run, run);
    ledger->run = *run;
    ledger->bound = true;
    return true;
}

static bool seen_before(const st_companion_ledger *ledger, const st_companion_id *nonce)
{
    bool found = false;
    for (uint32_t i = 0; i < ledger->seen_count; ++i) {
        found |= st_companion_id_equal(&ledger->seen[i], nonce);
    }
    return found;
}

static bool expired(const st_companion_expectation *expectation, uint64_t now_ns)
{
    return now_ns > expectation->deadline_ns;
}

static bool valid_target(st_companion_target target)
{
    return target >= ST_COMPANION_TARGET_URL && target <= ST_COMPANION_TARGET_CAPTURE;
}

st_companion_arm_status st_companion_ledger_arm(st_companion_ledger *ledger,
                                                st_companion_target target,
                                                const st_companion_id *nonce,
                                                uint32_t deadline_ms,
                                                uint64_t now_ns,
                                                int64_t pasteboard_change)
{
    if (ledger == NULL || !ledger->bound) return ST_COMPANION_ARM_NO_RUN;
    if (nonce == NULL || id_is_zero(nonce) || !valid_target(target) || deadline_ms == 0 ||
        deadline_ms > ST_COMPANION_MAX_DEADLINE_MS) {
        return ST_COMPANION_ARM_INVALID;
    }
    if (seen_before(ledger, nonce)) return ST_COMPANION_ARM_DUPLICATE;
    if (ledger->seen_count >= ST_COMPANION_MAX_NONCES) return ST_COMPANION_ARM_LIMIT;

    if (target == ST_COMPANION_TARGET_CAPTURE) {
        ledger->seen[ledger->seen_count++] = *nonce;
        ledger->capture = *nonce;
        ledger->capture_armed = true;
        return ST_COMPANION_ARM_OK;
    }

    st_companion_expectation *slot = NULL;
    for (size_t i = 0; i < ST_COMPANION_MAX_EXPECTATIONS; ++i) {
        st_companion_expectation *candidate = &ledger->expectations[i];
        if (candidate->armed && expired(candidate, now_ns)) candidate->armed = false;
        if (candidate->armed && target == ST_COMPANION_TARGET_CLIPBOARD &&
            candidate->target == ST_COMPANION_TARGET_CLIPBOARD) {
            /* The probe's newer copy superseded this marker; its nonce stays used. */
            candidate->armed = false;
        }
        if (!candidate->armed && slot == NULL) slot = candidate;
    }
    if (slot == NULL) return ST_COMPANION_ARM_FULL;

    ledger->seen[ledger->seen_count++] = *nonce;
    slot->target = target;
    slot->nonce = *nonce;
    slot->deadline_ns = now_ns + (uint64_t)deadline_ms * UINT64_C(1000000);
    slot->pasteboard_change = pasteboard_change;
    slot->armed = true;
    return ST_COMPANION_ARM_OK;
}

static st_companion_receipt_status record(st_companion_ledger *ledger,
                                          st_companion_target target,
                                          st_companion_receipt_status status)
{
    if (status == ST_COMPANION_RECEIPT_ACCEPTED) {
        ++ledger->accepted[target];
    } else {
        ++ledger->rejected[target];
    }
    ledger->last[target] = status;
    ledger->has_last[target] = true;
    return status;
}

/* Admits one more handled receipt, or reports that the bound is reached. */
static bool admit(st_companion_ledger *ledger)
{
    if (ledger->handled >= ST_COMPANION_MAX_REPORTS) return false;
    ++ledger->handled;
    return true;
}

static st_companion_expectation *find_expectation(st_companion_ledger *ledger,
                                                  st_companion_target target,
                                                  const st_companion_id *nonce)
{
    st_companion_expectation *match = NULL;
    for (size_t i = 0; i < ST_COMPANION_MAX_EXPECTATIONS; ++i) {
        st_companion_expectation *candidate = &ledger->expectations[i];
        if (candidate->armed && candidate->target == target &&
            st_companion_id_equal(&candidate->nonce, nonce)) {
            match = candidate;
        }
    }
    return match;
}

/* Settles a receipt whose run and nonce are already well formed. */
static st_companion_receipt_status settle(st_companion_ledger *ledger,
                                          st_companion_target target,
                                          const st_companion_id *run,
                                          const st_companion_id *nonce,
                                          uint64_t now_ns,
                                          st_companion_id *out_nonce)
{
    if (run != NULL && !st_companion_id_equal(run, &ledger->run)) {
        return record(ledger, target, ST_COMPANION_RECEIPT_FOREIGN_RUN);
    }
    st_companion_expectation *expectation = find_expectation(ledger, target, nonce);
    if (expectation != NULL) {
        expectation->armed = false;
        if (out_nonce != NULL) *out_nonce = *nonce;
        if (expired(expectation, now_ns)) {
            return record(ledger, target, ST_COMPANION_RECEIPT_EXPIRED);
        }
        return record(ledger, target, ST_COMPANION_RECEIPT_ACCEPTED);
    }
    bool armed_elsewhere = false;
    for (size_t i = 0; i < ST_COMPANION_MAX_EXPECTATIONS; ++i) {
        const st_companion_expectation *candidate = &ledger->expectations[i];
        armed_elsewhere |= candidate->armed && st_companion_id_equal(&candidate->nonce, nonce);
    }
    if (!armed_elsewhere && seen_before(ledger, nonce)) {
        if (out_nonce != NULL) *out_nonce = *nonce;
        return record(ledger, target, ST_COMPANION_RECEIPT_REPLAYED);
    }
    return record(ledger, target, ST_COMPANION_RECEIPT_UNEXPECTED);
}

st_companion_receipt_status st_companion_ledger_receive_url(st_companion_ledger *ledger,
                                                            const char *url,
                                                            size_t length,
                                                            uint64_t now_ns,
                                                            st_companion_id *out_nonce)
{
    const st_companion_target target = ST_COMPANION_TARGET_URL;
    if (!admit(ledger)) return ST_COMPANION_RECEIPT_LIMIT;
    st_companion_id run;
    st_companion_id nonce;
    const size_t prefix = sizeof url_prefix - 1;
    const size_t separator = sizeof url_nonce - 1;
    if (url == NULL || length != ST_COMPANION_URL_LENGTH ||
        memcmp(url, url_prefix, prefix) != 0 ||
        !st_companion_id_parse(url + prefix, ST_COMPANION_ID_HEX, &run) ||
        memcmp(url + prefix + ST_COMPANION_ID_HEX, url_nonce, separator) != 0 ||
        !st_companion_id_parse(url + prefix + ST_COMPANION_ID_HEX + separator,
                               ST_COMPANION_ID_HEX, &nonce)) {
        return record(ledger, target, ST_COMPANION_RECEIPT_MALFORMED);
    }
    if (!ledger->bound) return record(ledger, target, ST_COMPANION_RECEIPT_NO_RUN);
    return settle(ledger, target, &run, &nonce, now_ns, out_nonce);
}

st_companion_receipt_status st_companion_ledger_receive_event(st_companion_ledger *ledger,
                                                              const char *run_text,
                                                              size_t run_length,
                                                              const char *nonce_text,
                                                              size_t nonce_length,
                                                              uint64_t now_ns,
                                                              st_companion_id *out_nonce)
{
    const st_companion_target target = ST_COMPANION_TARGET_APPLE_EVENT;
    if (!admit(ledger)) return ST_COMPANION_RECEIPT_LIMIT;
    st_companion_id run;
    st_companion_id nonce;
    if (!st_companion_id_parse(run_text, run_length, &run) ||
        !st_companion_id_parse(nonce_text, nonce_length, &nonce)) {
        return record(ledger, target, ST_COMPANION_RECEIPT_MALFORMED);
    }
    if (!ledger->bound) return record(ledger, target, ST_COMPANION_RECEIPT_NO_RUN);
    return settle(ledger, target, &run, &nonce, now_ns, out_nonce);
}

static bool parse_marker(const char *value, size_t length, st_companion_id *nonce)
{
    const size_t prefix = sizeof marker_prefix - 1;
    return value != NULL && length == ST_COMPANION_MARKER_LENGTH &&
           memcmp(value, marker_prefix, prefix) == 0 &&
           st_companion_id_parse(value + prefix, ST_COMPANION_ID_HEX, nonce);
}

st_companion_receipt_status st_companion_ledger_receive_accessibility(
    st_companion_ledger *ledger, const char *value, size_t length, uint64_t now_ns,
    st_companion_id *out_nonce)
{
    const st_companion_target target = ST_COMPANION_TARGET_ACCESSIBILITY;
    if (!admit(ledger)) return ST_COMPANION_RECEIPT_LIMIT;
    st_companion_id nonce;
    if (!parse_marker(value, length, &nonce)) {
        return record(ledger, target, ST_COMPANION_RECEIPT_MALFORMED);
    }
    if (!ledger->bound) return record(ledger, target, ST_COMPANION_RECEIPT_NO_RUN);
    /* The marker carries no run: only this run's controller can arm the nonce. */
    return settle(ledger, target, NULL, &nonce, now_ns, out_nonce);
}

static st_companion_expectation *clipboard_expectation(st_companion_ledger *ledger)
{
    for (size_t i = 0; i < ST_COMPANION_MAX_EXPECTATIONS; ++i) {
        st_companion_expectation *candidate = &ledger->expectations[i];
        if (candidate->armed && candidate->target == ST_COMPANION_TARGET_CLIPBOARD) {
            return candidate;
        }
    }
    return NULL;
}

bool st_companion_ledger_clipboard_armed(const st_companion_ledger *ledger, uint64_t now_ns)
{
    for (size_t i = 0; i < ST_COMPANION_MAX_EXPECTATIONS; ++i) {
        const st_companion_expectation *candidate = &ledger->expectations[i];
        if (candidate->armed && candidate->target == ST_COMPANION_TARGET_CLIPBOARD &&
            !expired(candidate, now_ns)) {
            return true;
        }
    }
    return false;
}

st_companion_receipt_status st_companion_ledger_clipboard_gate(st_companion_ledger *ledger,
                                                               int64_t pasteboard_change,
                                                               bool only_plain_text,
                                                               uint64_t now_ns,
                                                               st_companion_id *out_nonce)
{
    const st_companion_target target = ST_COMPANION_TARGET_CLIPBOARD;
    if (!admit(ledger)) return ST_COMPANION_RECEIPT_LIMIT;
    if (!ledger->bound) return record(ledger, target, ST_COMPANION_RECEIPT_NO_RUN);
    st_companion_expectation *expectation = clipboard_expectation(ledger);
    if (expectation == NULL) return record(ledger, target, ST_COMPANION_RECEIPT_UNEXPECTED);
    if (out_nonce != NULL) *out_nonce = expectation->nonce;
    if (expired(expectation, now_ns)) {
        expectation->armed = false;
        return record(ledger, target, ST_COMPANION_RECEIPT_EXPIRED);
    }
    if (pasteboard_change != expectation->pasteboard_change) {
        expectation->armed = false;
        return record(ledger, target, ST_COMPANION_RECEIPT_SUPERSEDED);
    }
    if (!only_plain_text) {
        expectation->armed = false;
        return record(ledger, target, ST_COMPANION_RECEIPT_MISMATCH);
    }
    /* Permission to read once; the comparison consumes the expectation. Not
     * recorded as a receipt until the comparison settles. */
    --ledger->handled;
    return ST_COMPANION_RECEIPT_ACCEPTED;
}

st_companion_receipt_status st_companion_ledger_receive_clipboard(st_companion_ledger *ledger,
                                                                  const uint8_t *bytes,
                                                                  size_t length,
                                                                  uint64_t now_ns,
                                                                  st_companion_id *out_nonce)
{
    const st_companion_target target = ST_COMPANION_TARGET_CLIPBOARD;
    if (!admit(ledger)) return ST_COMPANION_RECEIPT_LIMIT;
    if (!ledger->bound) return record(ledger, target, ST_COMPANION_RECEIPT_NO_RUN);
    st_companion_expectation *expectation = clipboard_expectation(ledger);
    if (expectation == NULL) return record(ledger, target, ST_COMPANION_RECEIPT_UNEXPECTED);
    expectation->armed = false;
    if (out_nonce != NULL) *out_nonce = expectation->nonce;
    if (expired(expectation, now_ns)) return record(ledger, target, ST_COMPANION_RECEIPT_EXPIRED);

    char expected[ST_COMPANION_MARKER_LENGTH];
    char hex[ST_COMPANION_ID_HEX + 1];
    st_companion_id_format(&expectation->nonce, hex);
    memcpy(expected, marker_prefix, sizeof marker_prefix - 1);
    memcpy(expected + sizeof marker_prefix - 1, hex, ST_COMPANION_ID_HEX);

    /* The length is public (the marker's fixed form); the bytes are compared
     * in constant time so timing reveals nothing about the pasted value. */
    uint8_t difference = bytes == NULL || length != ST_COMPANION_MARKER_LENGTH ? 1u : 0u;
    if (difference == 0) {
        for (size_t i = 0; i < ST_COMPANION_MARKER_LENGTH; ++i) {
            difference |= (uint8_t)(bytes[i] ^ (uint8_t)expected[i]);
        }
    }
    memset(expected, 0, sizeof expected);
    return record(ledger, target,
                  difference == 0 ? ST_COMPANION_RECEIPT_ACCEPTED : ST_COMPANION_RECEIPT_MISMATCH);
}

void st_companion_capture_pattern(const st_companion_id *nonce,
                                  uint8_t cells[ST_COMPANION_CAPTURE_ROWS]
                                               [ST_COMPANION_CAPTURE_COLUMNS])
{
    const size_t interior = ST_COMPANION_CAPTURE_COLUMNS - 2;
    for (size_t row = 0; row < ST_COMPANION_CAPTURE_ROWS; ++row) {
        for (size_t column = 0; column < ST_COMPANION_CAPTURE_COLUMNS; ++column) {
            if (row == 0 || column == 0 || row == ST_COMPANION_CAPTURE_ROWS - 1 ||
                column == ST_COMPANION_CAPTURE_COLUMNS - 1) {
                cells[row][column] = ST_COMPANION_CAPTURE_FRAME;
                continue;
            }
            size_t bit = (row - 1) * interior + (column - 1);
            bool set = nonce != NULL && ((nonce->bytes[bit / 8] >> (7 - bit % 8)) & 1u);
            cells[row][column] = set ? ST_COMPANION_CAPTURE_BLACK : ST_COMPANION_CAPTURE_WHITE;
        }
    }
}

_Static_assert((ST_COMPANION_CAPTURE_ROWS - 2) * (ST_COMPANION_CAPTURE_COLUMNS - 2) ==
                   ST_COMPANION_ID_BYTES * 8,
               "capture interior must hold exactly one nonce");
