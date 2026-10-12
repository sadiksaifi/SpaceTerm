#include "companion_protocol.h"

#include <stdio.h>
#include <string.h>

typedef struct {
    const char *token;
    uint32_t value;
} token_value;

static const token_value target_tokens[] = {
    {"url", ST_COMPANION_TARGET_URL},
    {"apple-event", ST_COMPANION_TARGET_APPLE_EVENT},
    {"accessibility", ST_COMPANION_TARGET_ACCESSIBILITY},
    {"clipboard", ST_COMPANION_TARGET_CLIPBOARD},
    {"capture", ST_COMPANION_TARGET_CAPTURE},
};

static const token_value context_tokens[] = {
    {"gui-parented", ST_COMPANION_CONTEXT_GUI_PARENTED},
    {"daemon-parented", ST_COMPANION_CONTEXT_DAEMON_PARENTED},
};

/* The controller reports only states it established itself. */
static const token_value run_tokens[] = {
    {"active", ST_COMPANION_RUN_ACTIVE},
    {"stopping", ST_COMPANION_RUN_STOPPING},
    {"stopped", ST_COMPANION_RUN_STOPPED},
    {"cleanup-unresolved", ST_COMPANION_RUN_CLEANUP_UNRESOLVED},
    {"failed", ST_COMPANION_RUN_FAILED},
};

static const token_value phase_tokens[] = {
    {"queued", ST_COMPANION_PHASE_QUEUED},
    {"running", ST_COMPANION_PHASE_RUNNING},
    {"waiting-for-person", ST_COMPANION_PHASE_WAITING_FOR_PERSON},
};

static const token_value process_tokens[] = {
    {"unknown", ST_COMPANION_PROCESS_UNKNOWN}, {"desktop", ST_COMPANION_PROCESS_DESKTOP},
    {"server", ST_COMPANION_PROCESS_SERVER},   {"probe", ST_COMPANION_PROCESS_PROBE},
    {"companion", ST_COMPANION_PROCESS_COMPANION}, {"other", ST_COMPANION_PROCESS_OTHER},
};

static const token_value preflight_tokens[] = {
    {"unknown", ST_COMPANION_PREFLIGHT_UNKNOWN},
    {"granted", ST_COMPANION_PREFLIGHT_GRANTED},
    {"denied", ST_COMPANION_PREFLIGHT_DENIED},
    {"not-determined", ST_COMPANION_PREFLIGHT_NOT_DETERMINED},
    {"restricted", ST_COMPANION_PREFLIGHT_RESTRICTED},
    {"not-applicable", ST_COMPANION_PREFLIGHT_NOT_APPLICABLE},
    {"unavailable", ST_COMPANION_PREFLIGHT_UNAVAILABLE},
};

static const token_value observed_tokens[] = {
    {"unknown", ST_COMPANION_OBSERVED_UNKNOWN},
    {"observed", ST_COMPANION_OBSERVED_EFFECT},
    {"blocked", ST_COMPANION_OBSERVED_BLOCKED},
    {"absent", ST_COMPANION_OBSERVED_ABSENT},
    {"not-attempted", ST_COMPANION_OBSERVED_NOT_ATTEMPTED},
};

static const token_value cleanup_tokens[] = {
    {"unknown", ST_COMPANION_CLEANUP_UNKNOWN},
    {"complete", ST_COMPANION_CLEANUP_COMPLETE},
    {"not-needed", ST_COMPANION_CLEANUP_NOT_NEEDED},
    {"unresolved", ST_COMPANION_CLEANUP_UNRESOLVED},
};

static const token_value ack_tokens[] = {
    {"accepted", ST_COMPANION_ACK_ACCEPTED},
    {"busy", ST_COMPANION_ACK_BUSY},
    {"stopping", ST_COMPANION_ACK_STOPPING},
    {"unsupported", ST_COMPANION_ACK_UNSUPPORTED},
};

static const token_value refusal_tokens[] = {
    {"inactive", ST_COMPANION_REFUSAL_INACTIVE},
    {"duplicate", ST_COMPANION_REFUSAL_DUPLICATE},
    {"full", ST_COMPANION_REFUSAL_FULL},
    {"limit", ST_COMPANION_REFUSAL_LIMIT},
};

#define COUNT(table) (sizeof(table) / sizeof((table)[0]))

static const char *token_for(const token_value *table, size_t count, uint32_t value)
{
    for (size_t i = 0; i < count; ++i) {
        if (table[i].value == value) return table[i].token;
    }
    return NULL;
}

const char *st_companion_target_token(st_companion_target target)
{
    return token_for(target_tokens, COUNT(target_tokens), (uint32_t)target);
}

const char *st_companion_context_token(st_companion_context context)
{
    return token_for(context_tokens, COUNT(context_tokens), (uint32_t)context);
}

const char *st_companion_preflight_token(st_companion_preflight preflight)
{
    return token_for(preflight_tokens, COUNT(preflight_tokens), (uint32_t)preflight);
}

const char *st_companion_observed_token(st_companion_observed observed)
{
    return token_for(observed_tokens, COUNT(observed_tokens), (uint32_t)observed);
}

const char *st_companion_cleanup_token(st_companion_cleanup cleanup)
{
    return token_for(cleanup_tokens, COUNT(cleanup_tokens), (uint32_t)cleanup);
}

const char *st_companion_process_token(st_companion_process process)
{
    return token_for(process_tokens, COUNT(process_tokens), (uint32_t)process);
}

const char *st_companion_refusal_token(st_companion_refusal refusal)
{
    return token_for(refusal_tokens, COUNT(refusal_tokens), (uint32_t)refusal);
}

st_companion_refusal st_companion_refusal_for_arm(st_companion_arm_status status)
{
    switch (status) {
    case ST_COMPANION_ARM_DUPLICATE: return ST_COMPANION_REFUSAL_DUPLICATE;
    case ST_COMPANION_ARM_FULL: return ST_COMPANION_REFUSAL_FULL;
    case ST_COMPANION_ARM_LIMIT: return ST_COMPANION_REFUSAL_LIMIT;
    case ST_COMPANION_ARM_OK:
    case ST_COMPANION_ARM_NO_RUN:
    case ST_COMPANION_ARM_INVALID: break;
    }
    return (st_companion_refusal)0;
}

/* --- Parsing --------------------------------------------------------------- */

typedef struct {
    const char *cursor;
    const char *end;
} scanner;

typedef struct {
    const char *start;
    size_t length;
} span;

static bool value_byte(char c)
{
    return (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '-';
}

/* Reads " key=value" (or the leading kind when key is NULL). */
static bool field(scanner *scan, const char *key, span *value)
{
    if (key != NULL) {
        size_t key_length = strlen(key);
        if (scan->cursor == scan->end || *scan->cursor != ' ') return false;
        ++scan->cursor;
        if ((size_t)(scan->end - scan->cursor) <= key_length ||
            memcmp(scan->cursor, key, key_length) != 0 || scan->cursor[key_length] != '=') {
            return false;
        }
        scan->cursor += key_length + 1;
    }
    const char *start = scan->cursor;
    while (scan->cursor < scan->end && value_byte(*scan->cursor)) ++scan->cursor;
    if (scan->cursor == start) return false;
    if (scan->cursor < scan->end && *scan->cursor != ' ') return false;
    value->start = start;
    value->length = (size_t)(scan->cursor - start);
    return true;
}

static bool finished(const scanner *scan)
{
    return scan->cursor == scan->end;
}

static bool equals(span value, const char *text)
{
    return value.length == strlen(text) && memcmp(value.start, text, value.length) == 0;
}

static bool lookup(span value, const token_value *table, size_t count, uint32_t *out)
{
    for (size_t i = 0; i < count; ++i) {
        if (equals(value, table[i].token)) {
            *out = table[i].value;
            return true;
        }
    }
    return false;
}

/* Canonical decimal from minimum to maximum: no sign, no leading zero. */
static bool number(span value, uint32_t minimum, uint32_t maximum, uint32_t *out)
{
    if (value.length == 0 || value.length > 7) return false;
    if (value.length > 1 && value.start[0] == '0') return false;
    uint32_t result = 0;
    for (size_t i = 0; i < value.length; ++i) {
        char c = value.start[i];
        if (c < '0' || c > '9') return false;
        result = result * 10u + (uint32_t)(c - '0');
    }
    if (result < minimum || result > maximum) return false;
    *out = result;
    return true;
}

static bool identifier(span value, st_companion_id *out)
{
    return st_companion_id_parse(value.start, value.length, out);
}

static bool parse_welcome(scanner *scan, st_companion_inbound *out)
{
    span schema, run;
    uint32_t version = 0;
    return field(scan, "schema", &schema) && number(schema, 1, 1, &version) &&
           field(scan, "run", &run) && identifier(run, &out->value.welcome.run) &&
           finished(scan);
}

static bool parse_run(scanner *scan, st_companion_inbound *out)
{
    span state;
    uint32_t value = 0;
    if (!field(scan, "state", &state) || !lookup(state, run_tokens, COUNT(run_tokens), &value) ||
        !finished(scan)) {
        return false;
    }
    out->value.run.state = (st_companion_run_state)value;
    return true;
}

static bool parse_expect(scanner *scan, st_companion_inbound *out)
{
    span target, nonce, deadline;
    uint32_t target_value = 0;
    if (!field(scan, "target", &target) ||
        !lookup(target, target_tokens, COUNT(target_tokens), &target_value) ||
        !field(scan, "nonce", &nonce) || !identifier(nonce, &out->value.expect.nonce) ||
        !field(scan, "deadline-ms", &deadline) ||
        !number(deadline, 1, ST_COMPANION_MAX_DEADLINE_MS, &out->value.expect.deadline_ms) ||
        !finished(scan)) {
        return false;
    }
    out->value.expect.target = (st_companion_target)target_value;
    return true;
}

static bool parse_capability_context(scanner *scan, uint32_t *capability,
                                     st_companion_context *context)
{
    span code, place;
    uint32_t context_value = 0;
    if (!field(scan, "capability", &code) ||
        !number(code, 1, ST_COMPANION_CAPABILITY_COUNT, capability) ||
        !field(scan, "context", &place) ||
        !lookup(place, context_tokens, COUNT(context_tokens), &context_value)) {
        return false;
    }
    *context = (st_companion_context)context_value;
    return true;
}

static bool parse_progress(scanner *scan, st_companion_inbound *out)
{
    span phase;
    uint32_t phase_value = 0;
    if (!parse_capability_context(scan, &out->value.progress.capability,
                                  &out->value.progress.context) ||
        !field(scan, "phase", &phase) ||
        !lookup(phase, phase_tokens, COUNT(phase_tokens), &phase_value) || !finished(scan)) {
        return false;
    }
    out->value.progress.phase = (st_companion_phase)phase_value;
    return true;
}

static bool parse_result(scanner *scan, st_companion_inbound *out)
{
    span result, responsible, preflight, effect, cleanup;
    uint32_t responsible_value = 0, preflight_value = 0, effect_value = 0, cleanup_value = 0;
    st_companion_observation *observation = &out->value.result.observation;
    memset(observation, 0, sizeof *observation);
    if (!parse_capability_context(scan, &out->value.result.capability,
                                  &out->value.result.context) ||
        !field(scan, "result", &result) ||
        !st_companion_result_parse(result.start, result.length, &observation->result) ||
        !field(scan, "responsible", &responsible) ||
        !lookup(responsible, process_tokens, COUNT(process_tokens), &responsible_value) ||
        !field(scan, "preflight", &preflight) ||
        !lookup(preflight, preflight_tokens, COUNT(preflight_tokens), &preflight_value) ||
        !field(scan, "effect", &effect) ||
        !lookup(effect, observed_tokens, COUNT(observed_tokens), &effect_value) ||
        !field(scan, "cleanup", &cleanup) ||
        !lookup(cleanup, cleanup_tokens, COUNT(cleanup_tokens), &cleanup_value) ||
        !finished(scan)) {
        return false;
    }
    observation->responsible = (st_companion_process)responsible_value;
    observation->preflight = (st_companion_preflight)preflight_value;
    observation->effect = (st_companion_observed)effect_value;
    observation->cleanup = (st_companion_cleanup)cleanup_value;
    observation->reported = true;
    /* Passed requires an observed effect; preflight alone is never a pass. */
    return observation->result != ST_COMPANION_RESULT_PASSED ||
           observation->effect == ST_COMPANION_OBSERVED_EFFECT;
}

static bool parse_ack(scanner *scan, st_companion_inbound *out)
{
    span sequence, disposition;
    uint32_t disposition_value = 0;
    if (!field(scan, "seq", &sequence) ||
        !number(sequence, 1, ST_COMPANION_MAX_SEQUENCE, &out->value.ack.sequence) ||
        !field(scan, "disposition", &disposition) ||
        !lookup(disposition, ack_tokens, COUNT(ack_tokens), &disposition_value) ||
        !finished(scan)) {
        return false;
    }
    out->value.ack.disposition = (st_companion_ack)disposition_value;
    return true;
}

bool st_companion_protocol_parse(const char *line, size_t length, st_companion_inbound *out)
{
    if (line == NULL || out == NULL || length == 0 || length >= ST_COMPANION_MAX_RECORD) {
        return false;
    }
    st_companion_inbound parsed;
    memset(&parsed, 0, sizeof parsed);
    scanner scan = {line, line + length};
    span kind;
    if (!field(&scan, NULL, &kind)) return false;
    bool valid = false;
    if (equals(kind, "welcome")) {
        parsed.kind = ST_COMPANION_INBOUND_WELCOME;
        valid = parse_welcome(&scan, &parsed);
    } else if (equals(kind, "run")) {
        parsed.kind = ST_COMPANION_INBOUND_RUN;
        valid = parse_run(&scan, &parsed);
    } else if (equals(kind, "expect")) {
        parsed.kind = ST_COMPANION_INBOUND_EXPECT;
        valid = parse_expect(&scan, &parsed);
    } else if (equals(kind, "progress")) {
        parsed.kind = ST_COMPANION_INBOUND_PROGRESS;
        valid = parse_progress(&scan, &parsed);
    } else if (equals(kind, "result")) {
        parsed.kind = ST_COMPANION_INBOUND_RESULT;
        valid = parse_result(&scan, &parsed);
    } else if (equals(kind, "ack")) {
        parsed.kind = ST_COMPANION_INBOUND_ACK;
        valid = parse_ack(&scan, &parsed);
    }
    if (!valid) return false;
    *out = parsed;
    return true;
}

/* --- Encoding -------------------------------------------------------------- */

static size_t finish(char *out, size_t capacity, int written)
{
    if (written <= 0 || (size_t)written >= capacity || (size_t)written >= ST_COMPANION_MAX_RECORD) {
        if (out != NULL && capacity > 0) out[0] = '\0';
        return 0;
    }
    return (size_t)written;
}

size_t st_companion_encode_hello(char *out, size_t capacity, const st_companion_id *run,
                                 uint32_t pid)
{
    if (out == NULL || run == NULL || pid == 0) return finish(out, capacity, 0);
    char hex[ST_COMPANION_ID_HEX + 1];
    st_companion_id_format(run, hex);
    return finish(out, capacity,
                  snprintf(out, capacity, "hello schema=%d run=%s pid=%u\n",
                           ST_COMPANION_PROTOCOL_SCHEMA, hex, pid));
}

size_t st_companion_encode_targets(char *out, size_t capacity, uint32_t capture_window)
{
    if (out == NULL || capture_window == 0) return finish(out, capacity, 0);
    return finish(out, capacity,
                  snprintf(out, capacity, "targets capture-window=%u\n", capture_window));
}

size_t st_companion_encode_probe_request(char *out, size_t capacity, uint32_t sequence,
                                         uint32_t capability)
{
    if (out == NULL || sequence == 0 || sequence > ST_COMPANION_MAX_SEQUENCE ||
        st_companion_capability_find(capability) == NULL) {
        return finish(out, capacity, 0);
    }
    return finish(out, capacity,
                  snprintf(out, capacity, "request seq=%u kind=probe capability=%u\n", sequence,
                           capability));
}

size_t st_companion_encode_stop_request(char *out, size_t capacity, uint32_t sequence)
{
    if (out == NULL || sequence == 0 || sequence > ST_COMPANION_MAX_SEQUENCE) {
        return finish(out, capacity, 0);
    }
    return finish(out, capacity, snprintf(out, capacity, "request seq=%u kind=stop-run\n", sequence));
}

size_t st_companion_encode_armed(char *out, size_t capacity, st_companion_target target,
                                 const st_companion_id *nonce)
{
    const char *target_token = st_companion_target_token(target);
    if (out == NULL || target_token == NULL || nonce == NULL) return finish(out, capacity, 0);
    char hex[ST_COMPANION_ID_HEX + 1];
    st_companion_id_format(nonce, hex);
    return finish(out, capacity,
                  snprintf(out, capacity, "armed target=%s nonce=%s\n", target_token, hex));
}

size_t st_companion_encode_refused(char *out, size_t capacity, st_companion_target target,
                                   const st_companion_id *nonce, st_companion_refusal refusal)
{
    const char *target_token = st_companion_target_token(target);
    const char *reason = st_companion_refusal_token(refusal);
    if (out == NULL || target_token == NULL || reason == NULL || nonce == NULL) {
        return finish(out, capacity, 0);
    }
    char hex[ST_COMPANION_ID_HEX + 1];
    st_companion_id_format(nonce, hex);
    return finish(out, capacity,
                  snprintf(out, capacity, "refused target=%s nonce=%s reason=%s\n",
                           target_token, hex, reason));
}

size_t st_companion_encode_receipt(char *out, size_t capacity, st_companion_target target,
                                   st_companion_receipt_status status,
                                   const st_companion_id *nonce)
{
    const char *target_token = st_companion_target_token(target);
    if (out == NULL || target_token == NULL || target == ST_COMPANION_TARGET_CAPTURE ||
        (unsigned)status >= ST_COMPANION_RECEIPT_STATUS_COUNT) {
        return finish(out, capacity, 0);
    }
    const char *status_token = st_companion_receipt_token(status);
    if (!st_companion_receipt_has_nonce(status)) {
        return finish(out, capacity,
                      snprintf(out, capacity, "receipt target=%s status=%s\n", target_token,
                               status_token));
    }
    if (nonce == NULL) return finish(out, capacity, 0);
    char hex[ST_COMPANION_ID_HEX + 1];
    st_companion_id_format(nonce, hex);
    return finish(out, capacity,
                  snprintf(out, capacity, "receipt target=%s status=%s nonce=%s\n", target_token,
                           status_token, hex));
}

size_t st_companion_encode_authorization(char *out, size_t capacity,
                                         st_companion_operation operation,
                                         st_companion_auth_outcome outcome)
{
    if (out == NULL || st_companion_operation_token(operation) == NULL ||
        outcome == ST_COMPANION_AUTH_OUTCOME_NONE ||
        (unsigned)outcome >= ST_COMPANION_AUTH_OUTCOME_COUNT) {
        return finish(out, capacity, 0);
    }
    return finish(out, capacity,
                  snprintf(out, capacity, "authorization operation=%s outcome=%s\n",
                           st_companion_operation_token(operation),
                           st_companion_auth_outcome_token(outcome)));
}

size_t st_companion_encode_closing(char *out, size_t capacity)
{
    if (out == NULL) return 0;
    return finish(out, capacity, snprintf(out, capacity, "closing reason=quit\n"));
}

/* --- Framing --------------------------------------------------------------- */

void st_companion_framer_init(st_companion_framer *framer)
{
    memset(framer, 0, sizeof *framer);
}

st_companion_frame_status st_companion_framer_push(st_companion_framer *framer,
                                                   const uint8_t *bytes,
                                                   size_t length,
                                                   size_t *consumed,
                                                   const char **record,
                                                   size_t *record_length)
{
    *consumed = 0;
    if (framer->poisoned) return ST_COMPANION_FRAME_POISONED;
    /* A record returned by the previous call has been handed out; start over. */
    if (framer->length > 0 && framer->buffer[framer->length - 1] == '\n') framer->length = 0;
    for (size_t i = 0; i < length; ++i) {
        uint8_t byte = bytes[i];
        *consumed = i + 1;
        if (byte == '\n') {
            size_t line = framer->length;
            framer->buffer[framer->length++] = '\n';
            *record = framer->buffer;
            *record_length = line;
            return ST_COMPANION_FRAME_RECORD;
        }
        /* Leaves room for the newline, so a record never exceeds the bound. */
        if (byte < 0x20 || byte > 0x7e || framer->length >= ST_COMPANION_MAX_RECORD - 1) {
            framer->poisoned = true;
            memset(framer->buffer, 0, sizeof framer->buffer);
            framer->length = 0;
            return ST_COMPANION_FRAME_POISONED;
        }
        framer->buffer[framer->length++] = (char)byte;
    }
    return ST_COMPANION_FRAME_NEED_MORE;
}
