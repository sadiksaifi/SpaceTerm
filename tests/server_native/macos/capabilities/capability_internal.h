#ifndef SPACETERM_CAPABILITY_INTERNAL_H
#define SPACETERM_CAPABILITY_INTERNAL_H
#include "capability_probe.h"
#include <sys/types.h>

/* A private link seam. The ordinary harness links its own native spawn adapter.
 * The signed fixture links capability_native.m. There is no production switch
 * selecting a test adapter. */
st_native_result st_capability_spawn(st_capability_case, st_probe_context,
    uint64_t deadline_ns, pid_t *owned_child, int *owned_output);
/* Transfer only the current owning PTY to an already retained worker. */
int st_capability_begin_foreground(pid_t owned_child);
st_native_result st_capability_finish_spawn(pid_t owned_child, int group_absent, int root_reaped);
st_native_result st_capability_native_worker(st_capability_case, st_probe_context,
    uint64_t deadline_ns, st_capability_observation *);
int st_capability_worker_entry(int argc, char **argv);
/* Read-only OS responsibility observation for this exact effect process. */
uint32_t st_capability_responsible_process(void);

/* Native bounded text acquisition, also exercised with a native allocation peer. */
int st_capability_clipboard_text(void *board, uint8_t *out, size_t capacity, size_t *length);

/* Separate controlled startup-order experiment, not actual case-13 evidence. */
void st_capability_controlled_shell_order(st_capability_observation *, uint64_t deadline_ns);

/* Closed outcomes at the Launch Services, companion IPC, and Automation
 * boundaries. Ordinary tests replace only these external native calls. */
st_native_result st_capability_open_url(uint64_t deadline_ns);
st_native_result st_capability_url_receipt(uint64_t deadline_ns);
st_native_result st_capability_apple_event_receipt(uint64_t deadline_ns);
void st_capability_events(st_capability_observation *, uint64_t deadline_ns, int opening_url);

/* Owned descriptor I/O shares the caller's absolute deadline. */
int st_capability_wait_fd(int fd, short events, uint64_t deadline_ns);
int st_capability_transfer(int fd, void *bytes, size_t size, int writing, uint64_t deadline_ns);

#define ST_CAPABILITY_IDENTIFIER "io.github.sadiksaifi.spaceterm-development.server.native-evidence.probe"
#define ST_CAPABILITY_INSTALLED_PATH "/Library/Application Support/SpaceTerm Native Evidence 520/capability-probe"
#define ST_CAPABILITY_COMPANION_IDENTIFIER "io.github.sadiksaifi.spaceterm-development.server.native-evidence.companion"
#define ST_CAPABILITY_CONTROLLER_IDENTIFIER "io.github.sadiksaifi.spaceterm-development.server.native-evidence.client"
#define ST_CAPABILITY_CERTIFICATE "9e07cd5c15c5c862a21b93ea55d4303bc19b7366"

/* Target inputs are NONAUTHORIZING evidence facts. The native worker authenticates
 * the companion before IPC or capture. Folder markers use native folder discovery
 * and a fixed run-bound leaf. The explicit volume directory never enters output. */
typedef struct {
    uint32_t schema;
    uint32_t companion_pid;
    uint32_t companion_window;
    uint32_t controller_pid;
    uint8_t run_id[16];
    uint8_t target_nonce[16];
    char selected_volume[4096]; /* Empty means no explicitly selected volume. */
} st_capability_targets;

_Static_assert(sizeof(st_capability_targets) == 4144, "target ABI size");
_Static_assert(offsetof(st_capability_targets, selected_volume) == 48, "target ABI offset");

/* Single-threaded evidence fixture binding, not a shipped server API. */
st_native_result st_capability_bind_targets(const st_capability_targets *targets);

#endif
