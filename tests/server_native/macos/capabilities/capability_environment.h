#ifndef SPACETERM_CAPABILITY_ENVIRONMENT_H
#define SPACETERM_CAPABILITY_ENVIRONMENT_H
#include "capability_probe.h"
#include <sys/types.h>

/* Counts/presence cannot establish exact GUI-versus-daemon equality. */
typedef struct {
    uint32_t schema;
    uint32_t path_components;
    uint32_t duplicate_path_components;
    uint32_t locale_present;
} st_capability_environment;
st_native_result st_capability_normalize_environment(const char *path, int locale_present,
    st_capability_environment *out);
st_native_result st_capability_environment_encode(const st_capability_environment *, uint8_t out[16]);

#define ST_CAPABILITY_LOCALE_COUNT 14u
extern const char *const st_capability_locale_names[ST_CAPABILITY_LOCALE_COUNT];
enum { ST_PATH_EQUAL = 1, ST_PATH_PRESENCE_CHANGED = 2,
       ST_PATH_COMPONENTS_CHANGED = 3, ST_PATH_ORDER_CHANGED = 4 };
enum { ST_LOCALE_EQUAL = 1, ST_LOCALE_PRESENCE_CHANGED = 2, ST_LOCALE_VALUES_CHANGED = 3 };
/* Schema 2 compares each process locally against reference 1: PATH is
 * /usr/bin:/bin:/usr/sbin:/sbin, LANG is C, and the thirteen LC_* values are unset.
 * Equal means equal to that public baseline, never equal to another process. */
typedef struct {
    uint32_t schema;
    uint32_t context;
    uint32_t reference;
    uint32_t path_difference;
    uint32_t locale_difference;
} st_capability_environment_difference;
st_native_result st_capability_environment_compare_local(st_probe_context, const char *path,
    const char *const locales[ST_CAPABILITY_LOCALE_COUNT], st_capability_environment_difference *out);
st_native_result st_capability_environment_difference_encode(
    const st_capability_environment_difference *, uint8_t out[20]);
/* Native acquisition verifies the actual interactive login-shell/PTY context.
 * No private value, digest, or environment channel leaves this process. */
st_native_result st_capability_observe_environment(st_probe_context, st_capability_environment_difference *out);
/* The shell emits only login/interactive booleans after excluding command/script
 * execution. Exactly two ASCII bits and EOF; no argv or environment is acquired. */
st_native_result st_capability_read_shell_mode(int fd, uint64_t deadline, uint32_t *out);
st_native_result st_capability_bind_shell_mode(int fd, uint64_t deadline);
/* Native parent/PTY observation and local comparison, after signed context admission. */
st_native_result st_capability_observe_shell_environment(pid_t shell, st_probe_context context,
    st_capability_environment_difference *out);
#endif
