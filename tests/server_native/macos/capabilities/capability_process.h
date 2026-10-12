#ifndef SPACETERM_CAPABILITY_PROCESS_H
#define SPACETERM_CAPABILITY_PROCESS_H
#include "capability_probe.h"
#include <sys/types.h>

/* Native identities never enter evidence. Exec shadows share the same ancestry. */
typedef struct {
    pid_t pid;
    uint32_t pidversion;
    uint64_t uniqueid;
    uint64_t parent_uniqueid;
} st_capability_process_identity;

/* Read-only OS boundaries and identity-checked signalling. The test composition
 * supplies peers; the native fixture always links the Darwin implementation. */
typedef struct {
    void *context;
    int (*list)(void *, st_capability_process_identity *, size_t, size_t *);
    int (*signal)(void *, const st_capability_process_identity *);
    uint64_t (*now)(void *);
    void (*pause)(void *);
} st_capability_tree_peer;

st_native_result st_capability_tree_retire(const st_capability_tree_peer *,
    uint64_t root_uniqueid, uint64_t sentinel_uniqueid, uint64_t deadline_ns);
int st_capability_process_observe(pid_t, st_capability_process_identity *);
int st_capability_process_sentinel(uint64_t *uniqueid);
st_native_result st_capability_tree_retire_native(uint64_t root_uniqueid);
#endif
