#include "capability_process.h"
#include <stdlib.h>

#define TREE_LIMIT 16384u
#define SHADOW_BIT (UINT64_C(1) << 63)

static int order(const void *a, const void *b) {
    uint64_t left = ((const st_capability_process_identity *)a)->uniqueid;
    uint64_t right = ((const st_capability_process_identity *)b)->uniqueid;
    return (left > right) - (left < right);
}
static const st_capability_process_identity *find(const st_capability_process_identity *history,
    size_t count, uint64_t uniqueid) {
    st_capability_process_identity key = {.uniqueid = uniqueid};
    return bsearch(&key, history, count, sizeof(*history), order);
}
/* The root has been reaped before the sentinel brackets the fixed command.
 * Historical fork-parent IDs survive reparenting. Missing intermediate parents
 * inside that window are uncertainty, never evidence of absence. */
static int membership(const st_capability_process_identity *history, size_t count,
    uint64_t uniqueid, uint64_t root, uint64_t hi) {
    for (unsigned depth = 0; depth < 128; ++depth) {
        if (uniqueid == root) return 1;
        if (uniqueid < root) return 0;
        const st_capability_process_identity *node = find(history, count, uniqueid);
        if (!node) return uniqueid < hi ? -1 : 0;
        if (node->parent_uniqueid == uniqueid) return -1;
        uniqueid = node->parent_uniqueid;
    }
    return -1;
}
st_native_result st_capability_tree_retire(const st_capability_tree_peer *peer,
    uint64_t root, uint64_t hi, uint64_t deadline) {
    root &= ~SHADOW_BIT; hi &= ~SHADOW_BIT;
    if (!peer || !peer->list || !peer->signal || !peer->now || !peer->pause || !root || hi <= root)
        return ST_PROBE_CLEANUP_UNRESOLVED;
    st_capability_process_identity *live = calloc(TREE_LIMIT, sizeof(*live));
    st_capability_process_identity *history = calloc(TREE_LIMIT, sizeof(*history));
    st_native_result result = ST_PROBE_CLEANUP_UNRESOLVED;
    if (!live || !history) goto done;
    size_t retained = 0;
    unsigned empty_scans = 0;
    for (unsigned iteration = 0; iteration < 64 && peer->now(peer->context) < deadline; ++iteration) {
        size_t count = 0;
        if (!peer->list(peer->context, live, TREE_LIMIT, &count) || count > TREE_LIMIT) break;
        int uncertain = 0, members = 0;
        /* Insert complete snapshots before classifying any process. A live
         * unrelated parent must not be missed merely because of PID ordering. */
        for (size_t i = 0; i < count; ++i) {
            live[i].uniqueid &= ~SHADOW_BIT;
            live[i].parent_uniqueid &= ~SHADOW_BIT;
            if (!live[i].uniqueid || live[i].pid <= 0 || !live[i].pidversion) { uncertain = 1; break; }
            const st_capability_process_identity *known = find(history, retained, live[i].uniqueid);
            if (known) {
                if (known->parent_uniqueid != live[i].parent_uniqueid) { uncertain = 1; break; }
            } else {
                if (retained == TREE_LIMIT) { uncertain = 1; break; }
                history[retained++] = live[i];
                qsort(history, retained, sizeof(*history), order);
            }
            if (peer->now(peer->context) >= deadline) { uncertain = 1; break; }
        }
        if (uncertain) break;
        for (size_t i = 0; i < count; ++i) {
            int classification = membership(history, retained, live[i].uniqueid, root, hi);
            if (classification < 0) uncertain = 1;
            else if (classification == 1) {
                members = 1;
                /* The native peer rereads uniqueid then uses the kernel's
                 * audit-token pidversion check. Numeric kill is not a fallback. */
                if (!peer->signal(peer->context, &live[i])) uncertain = 1;
            }
            if (peer->now(peer->context) >= deadline) { uncertain = 1; break; }
        }
        if (!members && !uncertain) {
            if (++empty_scans == 2) { result = ST_PROBE_OK; break; }
        } else empty_scans = 0;
        peer->pause(peer->context);
    }
done:
    free(live); free(history);
    return result;
}
