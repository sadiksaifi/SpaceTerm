#ifndef SPACETERM_CAPABILITY_PROBE_H
#define SPACETERM_CAPABILITY_PROBE_H

#include <stddef.h>
#include <stdint.h>

/* The contract seed is absent at b107b1d. This alias matches ND520/1.
 * Integration may include spaceterm_native.h instead; no authority handle is
 * defined or fabricated by this evidence-only interface. */
typedef uint32_t st_native_result;
typedef uint32_t st_capability_case;
typedef uint32_t st_probe_context;
typedef uint32_t st_probe_outcome;

enum { ST_CAP_PROTECTED_FOLDERS = 1, ST_CAP_SELECTED_VOLUME = 2,
       ST_CAP_MICROPHONE = 3, ST_CAP_SCREEN_RECORDING = 4,
       ST_CAP_ACCESSIBILITY = 5, ST_CAP_CLIPBOARD = 6,
       ST_CAP_URL = 7, ST_CAP_APPLE_EVENTS = 8,
       ST_CAP_LOGIN_KEYCHAIN = 9, ST_CAP_SSH_AGENT = 10,
       ST_CAP_GPG_AGENT = 11, ST_CAP_TOUCH_ID_SUDO = 12,
       ST_CAP_LOGIN_SHELL = 13 };
enum { ST_CONTEXT_GUI = 1, ST_CONTEXT_DAEMON = 2 };
enum { ST_OUTCOME_PASSED = 1, ST_OUTCOME_DENIED = 2,
       ST_OUTCOME_CANCELLED = 3, ST_OUTCOME_UNAVAILABLE = 4,
       ST_OUTCOME_TIMED_OUT = 5, ST_OUTCOME_UNRUN = 6,
       ST_OUTCOME_FAILED = 7, ST_OUTCOME_PENDING = 8,
       ST_OUTCOME_INDETERMINATE = 9, ST_OUTCOME_CLEANUP_UNRESOLVED = 10 };
enum { ST_RESPONSIBLE_DESKTOP = 1, ST_RESPONSIBLE_SERVER = 2,
       ST_RESPONSIBLE_PROBE = 3, ST_RESPONSIBLE_COMPANION = 4,
       ST_RESPONSIBLE_OTHER = 5, ST_RESPONSIBLE_UNKNOWN = 6 };
enum { ST_PREFLIGHT_UNKNOWN = 0, ST_PREFLIGHT_ALLOWED = 1,
       ST_PREFLIGHT_DENIED = 2, ST_PREFLIGHT_NOT_DETERMINED = 3,
       ST_PREFLIGHT_UNAVAILABLE = 4 };
enum { ST_EFFECT_UNOBSERVED = 0, ST_EFFECT_VERIFIED = 1,
       ST_EFFECT_DENIED = 2, ST_EFFECT_UNAVAILABLE = 3,
       ST_EFFECT_FAILED = 4, ST_EFFECT_COMPLETED_UNATTRIBUTED = 5 };
enum { ST_CLEANUP_NOT_NEEDED = 0, ST_CLEANUP_VERIFIED = 1,
       ST_CLEANUP_UNRESOLVED = 2 };
/* Result codes equal the common NativeFailure repr(u32), not native errors. */
enum { ST_PROBE_OK = 0, ST_PROBE_UNAVAILABLE = 2, ST_PROBE_INVALID = 9,
       ST_PROBE_AUTHORIZATION_DENIED = 16, ST_PROBE_TIMED_OUT = 29, ST_PROBE_LIMIT = 30,
       ST_PROBE_CLEANUP_UNRESOLVED = 33, ST_PROBE_IO_UNAVAILABLE = 34 };

typedef struct {
    uint32_t schema;
    uint32_t capability;
    uint32_t context;
    uint32_t outcome;
    uint32_t responsible_process;
    uint32_t preflight_class;
    uint32_t effect_class;
    uint32_t cleanup_class;
} st_capability_observation;

typedef void (*st_capability_emit_fn)(void *, const st_capability_observation *);

/* Emits exactly one bounded observation on every valid invocation. The one
 * effect deadline covers worker startup and output; a separate 250 ms guard
 * bounds retirement without extending the effect deadline. Ordinary budgets
 * are at most 5 s; explicit sudo waits are at most 60 s. The outer controller
 * must also bound synchronous native preflight. No raw output is forwarded. */
st_native_result st_capability_run(st_capability_case capability,
    st_probe_context context, uint64_t monotonic_deadline_ns,
    st_capability_emit_fn emit, void *emit_context);

/* Fixed big-endian wire representation; the in-process C struct is not wire. */
st_native_result st_capability_encode(const st_capability_observation *, uint8_t out[32]);
st_native_result st_capability_decode(const uint8_t *, size_t, st_capability_observation *);
uint64_t st_capability_monotonic_ns(void);

/* Discard owned captured data. Never log, retain, or emit sample/pixel bytes. */
void st_capability_discard_owned(void *bytes, size_t length);

/* Fixed argv consumed by the evidence fixture. Authentication method remains
 * Unknown; sudo exit alone cannot produce Passed for Touch ID. */
const char *const *st_capability_sudo_argv(void);

#endif
