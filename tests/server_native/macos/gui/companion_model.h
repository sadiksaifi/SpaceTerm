#ifndef SPACETERM_EVIDENCE_COMPANION_MODEL_H
#define SPACETERM_EVIDENCE_COMPANION_MODEL_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/*
 * Portable policy for the temporary native evidence companion.
 *
 * The companion is a GUI-session target and explanation surface for issue
 * #520's capability matrix. It is not the shipped status companion, it grants
 * nothing, and it contains no privileged execution. All text here is fixed
 * literal copy; nothing carries terminal, clipboard, environment, path, or
 * native error content.
 */

/* Codes equal st_capability_case in the capability probe contract (1 to 13). */
typedef enum {
    ST_COMPANION_CAPABILITY_PROTECTED_FOLDERS = 1,
    ST_COMPANION_CAPABILITY_SELECTED_VOLUME = 2,
    ST_COMPANION_CAPABILITY_MICROPHONE = 3,
    ST_COMPANION_CAPABILITY_SCREEN_RECORDING = 4,
    ST_COMPANION_CAPABILITY_ACCESSIBILITY = 5,
    ST_COMPANION_CAPABILITY_CLIPBOARD_TOOLS = 6,
    ST_COMPANION_CAPABILITY_URL_OPENING = 7,
    ST_COMPANION_CAPABILITY_APPLE_EVENTS = 8,
    ST_COMPANION_CAPABILITY_LOGIN_KEYCHAIN = 9,
    ST_COMPANION_CAPABILITY_SSH_AGENT = 10,
    ST_COMPANION_CAPABILITY_GPG_AGENT = 11,
    ST_COMPANION_CAPABILITY_TOUCH_ID_SUDO = 12,
    ST_COMPANION_CAPABILITY_SHELL_ENVIRONMENT = 13
} st_companion_capability_code;

enum { ST_COMPANION_CAPABILITY_COUNT = 13 };

/* The two probe contexts. Their results are never coalesced. */
typedef enum {
    ST_COMPANION_CONTEXT_GUI_PARENTED = 0,
    ST_COMPANION_CONTEXT_DAEMON_PARENTED = 1
} st_companion_context;

enum { ST_COMPANION_CONTEXT_COUNT = 2 };

/* Responsible-process classes from the capability contract, plus NONE for a
 * case that involves no System Permission request at all. */
typedef enum {
    ST_COMPANION_PROCESS_UNKNOWN = 0,
    ST_COMPANION_PROCESS_DESKTOP = 1,
    ST_COMPANION_PROCESS_SERVER = 2,
    ST_COMPANION_PROCESS_PROBE = 3,
    ST_COMPANION_PROCESS_COMPANION = 4,
    ST_COMPANION_PROCESS_OTHER = 5,
    ST_COMPANION_PROCESS_NONE = 6
} st_companion_process;

/* Synthetic targets this companion provides to probes. */
typedef enum {
    ST_COMPANION_TARGET_NONE = 0,
    ST_COMPANION_TARGET_URL = 1,
    ST_COMPANION_TARGET_APPLE_EVENT = 2,
    ST_COMPANION_TARGET_ACCESSIBILITY = 3,
    ST_COMPANION_TARGET_CLIPBOARD = 4,
    ST_COMPANION_TARGET_CAPTURE = 5
} st_companion_target;

enum { ST_COMPANION_TARGET_COUNT = 6 };

typedef struct {
    st_companion_process process;
    /* The noun shown as "Who asks", for example "SpaceTerm Server". */
    const char *requester;
    /* One or two sentences that name the requester when one exists. */
    const char *explanation;
} st_companion_context_copy;

typedef struct {
    uint32_t code;
    const char *name;
    /* What the probe does with synthetic inputs only. */
    const char *summary;
    st_companion_context_copy contexts[ST_COMPANION_CONTEXT_COUNT];
    /* True when macOS keeps a per-app System Permission for this case. */
    bool system_permission;
    /* Privacy & Security anchor, or NULL when no pane applies. */
    const char *settings_anchor;
    /* The capability contract's probe wait: 5 seconds, or 60 for a person. */
    uint32_t wait_seconds;
    /* A person must decide or act while this probe runs. */
    bool needs_person;
    st_companion_target target;
} st_companion_capability;

/* Returns NULL for codes outside 1 to 13. */
const st_companion_capability *st_companion_capability_find(uint32_t code);

/* Why permissions granted to the SpaceTerm desktop app do not apply to the
 * server. Names both requesting processes. */
const char *st_companion_permission_scope(void);

/* The fixed display noun for a responsible-process class. */
const char *st_companion_process_name(st_companion_process process);

/* Evidence results, matching the native evidence schema's closed values. */
typedef enum {
    ST_COMPANION_RESULT_UNRUN = 0,
    ST_COMPANION_RESULT_PASSED = 1,
    ST_COMPANION_RESULT_DENIED = 2,
    ST_COMPANION_RESULT_CANCELLED = 3,
    ST_COMPANION_RESULT_UNAVAILABLE = 4,
    ST_COMPANION_RESULT_TIMED_OUT = 5,
    ST_COMPANION_RESULT_FAILED = 6,
    ST_COMPANION_RESULT_PENDING = 7,
    ST_COMPANION_RESULT_INDETERMINATE = 8,
    ST_COMPANION_RESULT_CLEANUP_UNRESOLVED = 9
} st_companion_result;

enum { ST_COMPANION_RESULT_COUNT = 10 };

/* How a status is drawn. Text always carries the meaning; tone only picks the
 * symbol's color. */
typedef enum {
    ST_COMPANION_TONE_NEUTRAL = 0,
    ST_COMPANION_TONE_POSITIVE = 1,
    ST_COMPANION_TONE_NEGATIVE = 2,
    ST_COMPANION_TONE_CAUTION = 3,
    ST_COMPANION_TONE_PROGRESS = 4
} st_companion_tone;

typedef struct {
    /* Short label, for example "Denied". */
    const char *title;
    /* Full sentence shown beside the label and read by VoiceOver. */
    const char *detail;
    /* SF Symbol name for the status image. */
    const char *symbol;
    st_companion_tone tone;
} st_companion_status_copy;

const char *st_companion_result_token(st_companion_result result);
bool st_companion_result_parse(const char *token, size_t length, st_companion_result *out);
st_companion_status_copy st_companion_result_copy(st_companion_result result);

/* State of the evidence run as the controller reports it. The companion
 * never infers stopped or cleaned-up state on its own. */
typedef enum {
    ST_COMPANION_RUN_DISCONNECTED = 0,
    ST_COMPANION_RUN_CONNECTING = 1,
    ST_COMPANION_RUN_ACTIVE = 2,
    ST_COMPANION_RUN_STOP_REQUESTED = 3,
    ST_COMPANION_RUN_STOPPING = 4,
    ST_COMPANION_RUN_STOPPED = 5,
    ST_COMPANION_RUN_CLEANUP_UNRESOLVED = 6,
    ST_COMPANION_RUN_FAILED = 7,
    ST_COMPANION_RUN_CONTROLLER_LOST = 8,
    ST_COMPANION_RUN_PROTOCOL_ERROR = 9,
    ST_COMPANION_RUN_REJECTED = 10
} st_companion_run_state;

st_companion_status_copy st_companion_run_copy(st_companion_run_state state);
/* Probes, authorization, and Stop Evidence Run are available only here. */
bool st_companion_run_accepts_requests(st_companion_run_state state);

/* Every action a person can invoke. The companion performs nothing on its
 * own except recording receipts and presenting controller reports. */
typedef enum {
    ST_COMPANION_ACTION_REQUEST_PROBE = 1,
    ST_COMPANION_ACTION_OPEN_PRIVACY_SETTINGS = 2,
    ST_COMPANION_ACTION_REQUEST_AUTHORIZATION = 3,
    ST_COMPANION_ACTION_STOP_RUN = 4,
    ST_COMPANION_ACTION_SHOW_COMPANION = 5,
    ST_COMPANION_ACTION_SHOW_TARGETS = 6,
    ST_COMPANION_ACTION_PASTE_MARKER = 7,
    ST_COMPANION_ACTION_QUIT = 8
} st_companion_action;

enum { ST_COMPANION_ACTION_COUNT = 8 };

/* The only effects an action may have. There is deliberately no grant,
 * privacy reset, PAM, Background Items, or forced-cleanup effect. */
typedef enum {
    ST_COMPANION_EFFECT_PRESENTATION = 1,
    ST_COMPANION_EFFECT_CONTROLLER_REQUEST = 2,
    ST_COMPANION_EFFECT_MANAGEMENT_AUTHORIZATION = 3,
    ST_COMPANION_EFFECT_OPEN_SETTINGS_PANE = 4,
    ST_COMPANION_EFFECT_READ_PASTED_MARKER = 5,
    ST_COMPANION_EFFECT_QUIT_COMPANION = 6
} st_companion_effect;

typedef struct {
    st_companion_action action;
    const char *title;
    st_companion_effect effect;
    /* Asks the person to confirm consequences before any effect. */
    bool confirms_first;
} st_companion_action_info;

/* Returns NULL for unknown actions. */
const st_companion_action_info *st_companion_action_find(uint32_t action);

/* Quit never stops the run or claims cleanup. */
typedef struct {
    bool confirm;
    const char *title;
    const char *message;
} st_companion_quit_plan;

st_companion_quit_plan st_companion_plan_quit(st_companion_run_state state);

/* Fixed names of NativeFailure's repr(u32) values 1 to 34, in kebab case.
 * Returns NULL for other values. */
const char *st_companion_failure_name(uint32_t failure);

#endif
