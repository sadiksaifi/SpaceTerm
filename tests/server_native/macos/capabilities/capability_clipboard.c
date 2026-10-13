#include "capability_clipboard.h"
#include "capability_targets.h"
#include <string.h>

void st_capability_clipboard_read(const st_capability_clipboard_peer *peer, const uint8_t run[16],
    uint64_t deadline, st_capability_observation *out) {
    if (!out) return;
    out->preflight_class = ST_PREFLIGHT_UNAVAILABLE;
    out->effect_class = ST_EFFECT_UNOBSERVED;
    out->cleanup_class = ST_CLEANUP_NOT_NEEDED;
    out->outcome = ST_OUTCOME_UNAVAILABLE;
    if (!peer || !run || !peer->now || !peer->read_text || !peer->change_count) return;
    if (peer->now(peer->context) >= deadline) { out->outcome = ST_OUTCOME_TIMED_OUT; return; }
    char expected[ST_COMPANION_MARKER_LENGTH + 1];
    uint8_t bytes[256] = {0};
    if (st_capability_target_marker(run, expected) != ST_PROBE_OK) return;
    uint64_t before, after;
    size_t length = 0;
    int read = peer->change_count(peer->context, &before) &&
        peer->read_text(peer->context, bytes, sizeof(bytes), &length) && length <= sizeof(bytes) &&
        peer->change_count(peer->context, &after) && before == after;
    int matched = read && length == ST_COMPANION_MARKER_LENGTH &&
        memcmp(bytes, expected, ST_COMPANION_MARKER_LENGTH) == 0;
    st_capability_discard_owned(bytes, sizeof(bytes));
    if (peer->now(peer->context) >= deadline) out->outcome = ST_OUTCOME_TIMED_OUT;
    else if (matched) {
        out->preflight_class = ST_PREFLIGHT_ALLOWED; out->effect_class = ST_EFFECT_VERIFIED;
        out->outcome = ST_OUTCOME_PASSED;
    } else if (read) out->outcome = ST_OUTCOME_UNRUN;
}
