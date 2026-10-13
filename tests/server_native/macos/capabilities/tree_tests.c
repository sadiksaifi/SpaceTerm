#include "capability_process.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

typedef struct {
    st_capability_process_identity live[8];
    size_t count;
    unsigned scans, signals;
    uint64_t time;
    int denied, reused, forever;
} fixture;
static int list(void *context, st_capability_process_identity *out, size_t capacity, size_t *count) {
    fixture *f = context;
    assert(capacity >= f->count);
    memcpy(out, f->live, f->count * sizeof(*out)); *count = f->count; ++f->scans;
    return 1;
}
static int signal_member(void *context, const st_capability_process_identity *member) {
    fixture *f = context;
    /* A replacement observed between scan and signalling never receives a
     * numeric signal. The native peer enforces this again inside the kernel. */
    if (f->reused) { f->live[0] = (st_capability_process_identity){member->pid, 8, 400, 2}; f->reused = 0; return 0; }
    ++f->signals;
    if (f->denied) return 0;
    if (!f->forever) f->count = 0;
    return 1;
}
static uint64_t now(void *context) { return ((fixture *)context)->time; }
static void pause_scan(void *context) { ((fixture *)context)->time += 1; }
static st_native_result retire(fixture *f) {
    st_capability_tree_peer peer = {f, list, signal_member, now, pause_scan};
    return st_capability_tree_retire(&peer, 100, 200, 20);
}
static fixture member(void) {
    fixture f = {.count = 1}; f.live[0] = (st_capability_process_identity){30, 7, 110, 100}; return f;
}
int main(void) {
    fixture f = member();
    assert(retire(&f) == ST_PROBE_OK && f.signals == 1 && f.scans == 3);
    puts("passed retained_fork_parent_identity_finds_reparented_member");
    f = member(); f.live[0].parent_uniqueid = 105;
    assert(retire(&f) == ST_PROBE_CLEANUP_UNRESOLVED && f.signals == 0 && f.scans == 20);
    puts("passed triple_fork_unknown_window_parent_remains_unresolved");
    f = member(); f.reused = 1;
    assert(retire(&f) == ST_PROBE_OK && f.signals == 0);
    puts("passed pid_reuse_never_signals_the_replacement");
    f = member(); f.live[0].parent_uniqueid = 2;
    assert(retire(&f) == ST_PROBE_OK && f.signals == 0);
    puts("passed unrelated_window_activity_is_not_a_member");
    f = member(); f.live[0].uniqueid |= UINT64_C(1) << 63; f.live[0].parent_uniqueid |= UINT64_C(1) << 63;
    assert(retire(&f) == ST_PROBE_OK && f.signals == 1);
    puts("passed exec_shadow_bit_does_not_break_ancestry");
    f = member(); f.denied = 1;
    assert(retire(&f) == ST_PROBE_CLEANUP_UNRESOLVED && f.signals > 0);
    puts("passed denied_retirement_never_becomes_complete_cleanup");
    f = member(); f.forever = 1;
    assert(retire(&f) == ST_PROBE_CLEANUP_UNRESOLVED && f.scans == 20);
    puts("passed retirement_has_one_absolute_deadline");
    f = member(); f.live[0].uniqueid = 220; f.live[0].parent_uniqueid = 210;
    assert(retire(&f) == ST_PROBE_OK && f.signals == 0);
    puts("passed post_sentinel_foreign_activity_is_outside_the_fixed_command_window");
    puts("executed=8 proof=synthetic native-effects=0");
}
