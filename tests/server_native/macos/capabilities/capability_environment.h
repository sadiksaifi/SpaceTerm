#ifndef SPACETERM_CAPABILITY_ENVIRONMENT_H
#define SPACETERM_CAPABILITY_ENVIRONMENT_H
#include "capability_probe.h"

/* Never contains PATH components, locale values, shell paths, or their hashes.
 * Counts/presence compare normalization, not exact environment equality. */
typedef struct {
    uint32_t schema;
    uint32_t path_components;
    uint32_t duplicate_path_components;
    uint32_t locale_present;
} st_capability_environment;

st_native_result st_capability_normalize_environment(const char *path, int locale_present,
    st_capability_environment *out);
st_native_result st_capability_environment_encode(const st_capability_environment *, uint8_t out[16]);

/* Private retained source values. No public value/hash accessor exists.
 * Only authenticated owned IPC may transfer them; never stdout or evidence.
 * The native adapter captures them from the actual PTY login shell. */
#define ST_CAPABILITY_LOCALE_COUNT 14u
extern const char *const st_capability_locale_names[ST_CAPABILITY_LOCALE_COUNT];
typedef struct st_capability_retained_environment st_capability_retained_environment;
enum { ST_PATH_EQUAL = 1, ST_PATH_PRESENCE_CHANGED = 2,
       ST_PATH_COMPONENTS_CHANGED = 3, ST_PATH_ORDER_CHANGED = 4 };
enum { ST_LOCALE_EQUAL = 1, ST_LOCALE_PRESENCE_CHANGED = 2, ST_LOCALE_VALUES_CHANGED = 3 };
typedef struct {
    uint32_t schema;
    uint32_t gui_context;
    uint32_t daemon_context;
    uint32_t path_difference;
    uint32_t locale_difference;
} st_capability_environment_difference;

st_native_result st_capability_environment_retain(st_probe_context context,
    const char *path, const char *const locales[ST_CAPABILITY_LOCALE_COUNT], st_capability_retained_environment **out);
st_native_result st_capability_environment_compare(const st_capability_retained_environment *gui,
    const st_capability_retained_environment *daemon, st_capability_environment_difference *out);
void st_capability_environment_release(st_capability_retained_environment **owned);
st_native_result st_capability_environment_difference_encode(
    const st_capability_environment_difference *, uint8_t out[20]);
/* Evidence-only native acquisition. Does not launch a shell or replace startup
 * files. Unavailable if actual login/interactive/PTY context cannot be proved. */
st_native_result st_capability_observe_environment(st_probe_context context,
    st_capability_retained_environment **out);
/* Evidence-only stream entries remain unavailable without frame-bound native
 * sender evidence. They grant no private transfer and leave output unchanged.
 * LOCAL_PEERPID and a nonsecret handshake cannot supply that binding. */
st_native_result st_capability_environment_send_current(int owned_channel, st_probe_context,
    uint64_t deadline_ns, const uint8_t run_id[16]);
st_native_result st_capability_environment_compare_channels(int gui_channel, int daemon_channel,
    uint64_t deadline_ns, const uint8_t run_id[16], st_capability_environment_difference *out);

/* Recognize an explicit interactive login shell on a PTY, never a -c command
 * or script pretending to be an observed account shell. */
int st_capability_shell_invocation(const char *const argv[], size_t count, int has_pty);
#endif
