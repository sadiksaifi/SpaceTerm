#include "capability_clipboard.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

typedef struct {
    uint64_t count, time;
    unsigned reads;
    int changed;
    char text[51];
    uint8_t rich_format[4];
} fixture;
static int count(void *context, uint64_t *out) { *out = ((fixture *)context)->count; return 1; }
static uint64_t now(void *context) { return ((fixture *)context)->time; }
static int read_text(void *context, uint8_t *out, size_t capacity, size_t *size) {
    fixture *f = context;
    ++f->reads; assert(capacity >= sizeof(f->text));
    memcpy(out, f->text, sizeof(f->text)); *size = sizeof(f->text);
    if (f->changed) ++f->count;
    return 1;
}
static st_capability_observation observe(fixture *f, uint64_t deadline) {
    uint8_t run[16] = {1};
    st_capability_clipboard_peer peer = {f, count, read_text, now};
    st_capability_observation out = {1, ST_CAP_CLIPBOARD, ST_CONTEXT_GUI, ST_OUTCOME_UNAVAILABLE,
        ST_RESPONSIBLE_UNKNOWN, ST_PREFLIGHT_UNKNOWN, ST_EFFECT_UNOBSERVED, ST_CLEANUP_NOT_NEEDED};
    st_capability_clipboard_read(&peer, run, deadline, &out);
    return out;
}
static fixture prepared(void) {
    fixture f = {.count = 8, .time = 1, .rich_format = {1, 2, 3, 4}};
    memcpy(f.text, "spaceterm-evidence-01000000000000000000000000000000", sizeof(f.text));
    return f;
}
int main(void) {
    fixture f = prepared();
    st_capability_observation out = observe(&f, 10);
    assert(out.outcome == ST_OUTCOME_PASSED && out.effect_class == ST_EFFECT_VERIFIED);
    assert(out.cleanup_class == ST_CLEANUP_NOT_NEEDED && f.count == 8 && f.reads == 1);
    assert(memcmp(f.rich_format, (uint8_t[]){1, 2, 3, 4}, 4) == 0);
    puts("passed general_clipboard_synthetic_read_preserves_every_format");
    f = prepared(); f.changed = 1; out = observe(&f, 10);
    assert(out.outcome == ST_OUTCOME_UNAVAILABLE && out.effect_class == ST_EFFECT_UNOBSERVED && f.count == 9);
    puts("passed intervening_clipboard_change_is_never_restored_or_credited");
    f = prepared(); out = observe(&f, 1);
    assert(out.outcome == ST_OUTCOME_TIMED_OUT && f.count == 8 && f.reads == 0);
    puts("passed cancelled_or_expired_clipboard_probe_has_no_effect");
    f = prepared(); f.text[0] = 'x'; out = observe(&f, 10);
    assert(out.outcome == ST_OUTCOME_UNRUN && out.effect_class == ST_EFFECT_UNOBSERVED && f.count == 8);
    puts("passed unprepared_clipboard_is_not_a_synthetic_effect");
    puts("executed=4 proof=synthetic native-effects=0");
}
