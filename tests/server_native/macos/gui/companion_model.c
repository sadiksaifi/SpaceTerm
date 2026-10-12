#include "companion_model.h"

#include <string.h>

#define APP "SpaceTerm app"
#define SERVER "SpaceTerm Server"
#define PROBE "Capability probe"
#define NOBODY "No one"

/* Display copy. Sentences are literal and name the process macOS attributes
 * the request to. They describe the evidence design, not observed results;
 * observed results arrive separately from the evidence controller. */
static const st_companion_capability capabilities[ST_COMPANION_CAPABILITY_COUNT] = {
    {ST_COMPANION_CAPABILITY_PROTECTED_FOLDERS,
     "Protected folders",
     "Opens a synthetic marker file in a protected folder, such as Documents.",
     {{ST_COMPANION_PROCESS_DESKTOP, APP,
       "macOS asks whether the " APP " may open this folder, because the shell runs in a "
       "SpaceTerm window."},
      {ST_COMPANION_PROCESS_SERVER, SERVER,
       "macOS asks whether " SERVER " may open this folder. Access you gave the SpaceTerm "
       "app does not apply."}},
     true, "Privacy_FilesAndFolders", 60, true, ST_COMPANION_TARGET_NONE},
    {ST_COMPANION_CAPABILITY_SELECTED_VOLUME,
     "Selected volume",
     "Opens a synthetic marker file on a removable or network volume you choose.",
     {{ST_COMPANION_PROCESS_DESKTOP, APP,
       "macOS asks whether the " APP " may open files on this volume."},
      {ST_COMPANION_PROCESS_SERVER, SERVER,
       "macOS asks whether " SERVER " may open files on this volume. Access you gave the "
       "SpaceTerm app does not apply."}},
     true, "Privacy_FilesAndFolders", 60, true, ST_COMPANION_TARGET_NONE},
    {ST_COMPANION_CAPABILITY_MICROPHONE,
     "Microphone",
     "Records a short sample from the microphone and discards it.",
     {{ST_COMPANION_PROCESS_DESKTOP, APP,
       "macOS asks whether the " APP " may use the microphone."},
      {ST_COMPANION_PROCESS_SERVER, SERVER,
       "macOS asks whether " SERVER " may use the microphone. Microphone access you gave "
       "the SpaceTerm app does not apply."}},
     true, "Privacy_Microphone", 60, true, ST_COMPANION_TARGET_NONE},
    {ST_COMPANION_CAPABILITY_SCREEN_RECORDING,
     "Screen Recording",
     "Captures the companion's test pattern window and discards the pixels.",
     {{ST_COMPANION_PROCESS_DESKTOP, APP,
       "macOS asks whether the " APP " may record the screen."},
      {ST_COMPANION_PROCESS_SERVER, SERVER,
       "macOS asks whether " SERVER " may record the screen. Screen Recording you allowed "
       "for the SpaceTerm app does not apply."}},
     true, "Privacy_ScreenCapture", 60, true, ST_COMPANION_TARGET_CAPTURE},
    {ST_COMPANION_CAPABILITY_ACCESSIBILITY,
     "Accessibility",
     "Sets the value of the companion's Accessibility test field.",
     {{ST_COMPANION_PROCESS_DESKTOP, APP,
       "macOS asks whether the " APP " may control this Mac through Accessibility."},
      {ST_COMPANION_PROCESS_SERVER, SERVER,
       "macOS asks whether " SERVER " may control this Mac through Accessibility. "
       "Accessibility you allowed for the SpaceTerm app does not apply."}},
     true, "Privacy_Accessibility", 60, true, ST_COMPANION_TARGET_ACCESSIBILITY},
    {ST_COMPANION_CAPABILITY_CLIPBOARD_TOOLS,
     "Clipboard tools",
     "Copies a synthetic marker with pbcopy. You paste it into the companion's clipboard "
     "target.",
     {{ST_COMPANION_PROCESS_NONE, NOBODY,
       "Copying to the clipboard needs no System Permission, and macOS does not ask the "
       "process that copies. You paste the marker yourself with Edit > Paste, so macOS "
       "does not ask the companion either."},
      {ST_COMPANION_PROCESS_NONE, NOBODY,
       "Copying to the clipboard needs no System Permission, and macOS does not ask the "
       "process that copies. This case checks whether a server session can reach your "
       "clipboard at all."}},
     false, NULL, 60, true, ST_COMPANION_TARGET_CLIPBOARD},
    {ST_COMPANION_CAPABILITY_URL_OPENING,
     "URL opening",
     "Opens the companion's test URL. No browser or network is involved.",
     {{ST_COMPANION_PROCESS_NONE, NOBODY,
       "Opening a URL needs no System Permission. macOS delivers the URL to the companion "
       "in your login session."},
      {ST_COMPANION_PROCESS_NONE, NOBODY,
       "Opening a URL needs no System Permission. This case checks whether a server "
       "session can reach your login session at all."}},
     false, NULL, 5, false, ST_COMPANION_TARGET_URL},
    {ST_COMPANION_CAPABILITY_APPLE_EVENTS,
     "Apple Events",
     "Sends one Apple Event to the companion and checks the reply.",
     {{ST_COMPANION_PROCESS_DESKTOP, APP,
       "macOS asks whether the " APP " may control SpaceTerm Evidence Companion."},
      {ST_COMPANION_PROCESS_SERVER, SERVER,
       "macOS asks whether " SERVER " may control SpaceTerm Evidence Companion. Automation "
       "you allowed for the SpaceTerm app does not apply."}},
     true, "Privacy_Automation", 60, true, ST_COMPANION_TARGET_APPLE_EVENT},
    {ST_COMPANION_CAPABILITY_LOGIN_KEYCHAIN,
     "Login keychain",
     "Creates, reads, and deletes a synthetic item in your login keychain.",
     {{ST_COMPANION_PROCESS_PROBE, PROBE,
       "Keychain access is decided per item. The capability probe created the synthetic "
       "item, so the item's access list names the capability probe, not the SpaceTerm app."},
      {ST_COMPANION_PROCESS_PROBE, PROBE,
       "The same item access list names the capability probe. A server session may find "
       "the login keychain locked or unavailable."}},
     false, NULL, 5, false, ST_COMPANION_TARGET_NONE},
    {ST_COMPANION_CAPABILITY_SSH_AGENT,
     "SSH agent",
     "Checks whether the SSH agent answers, without reporting any key.",
     {{ST_COMPANION_PROCESS_NONE, NOBODY,
       "The SSH agent needs no System Permission. A shell in a SpaceTerm window inherits "
       "the agent from your login session."},
      {ST_COMPANION_PROCESS_NONE, NOBODY,
       "The SSH agent needs no System Permission. A server session reaches the agent only "
       "if the server provides the agent's socket."}},
     false, NULL, 5, false, ST_COMPANION_TARGET_NONE},
    {ST_COMPANION_CAPABILITY_GPG_AGENT,
     "GPG agent",
     "Checks whether the GPG agent answers, without reporting any key.",
     {{ST_COMPANION_PROCESS_NONE, NOBODY,
       "The GPG agent needs no System Permission. A shell in a SpaceTerm window finds the "
       "agent your login session started."},
      {ST_COMPANION_PROCESS_NONE, NOBODY,
       "The GPG agent needs no System Permission. A server session may start or find a "
       "different agent, or none."}},
     false, NULL, 5, false, ST_COMPANION_TARGET_NONE},
    {ST_COMPANION_CAPABILITY_TOUCH_ID_SUDO,
     "Touch ID for sudo",
     "Runs sudo with /usr/bin/true. You approve or cancel the request yourself.",
     {{ST_COMPANION_PROCESS_OTHER, "sudo",
       "The sudo command decides through its own authentication settings. Touch ID may be "
       "offered in a SpaceTerm window if this Mac enables it for sudo. The companion never "
       "changes sudo or PAM settings."},
      {ST_COMPANION_PROCESS_OTHER, "sudo",
       "The sudo command decides through its own authentication settings. Touch ID needs "
       "your login session, so a server session may ask for a password or fail instead."}},
     false, NULL, 60, true, ST_COMPANION_TARGET_NONE},
    {ST_COMPANION_CAPABILITY_SHELL_ENVIRONMENT,
     "Shell environment",
     "Compares PATH, locale, and login-shell results between the two sessions without "
     "recording their values.",
     {{ST_COMPANION_PROCESS_NONE, NOBODY,
       "No System Permission is involved. The shell starts from the SpaceTerm app's "
       "environment."},
      {ST_COMPANION_PROCESS_NONE, NOBODY,
       "No System Permission is involved. The shell starts from the server's environment, "
       "which can differ from a SpaceTerm window."}},
     false, NULL, 5, false, ST_COMPANION_TARGET_NONE},
};

const st_companion_capability *st_companion_capability_find(uint32_t code)
{
    if (code < 1 || code > ST_COMPANION_CAPABILITY_COUNT) return NULL;
    return &capabilities[code - 1];
}

const char *st_companion_permission_scope(void)
{
    return "macOS keeps System Permissions for each app separately. A shell in a SpaceTerm "
           "window uses the " APP "'s permissions. A Terminal Session started by " SERVER
           " uses the server's own permissions. Permissions you gave the " APP " do not "
           "apply to " SERVER ".";
}

const char *st_companion_process_name(st_companion_process process)
{
    switch (process) {
    case ST_COMPANION_PROCESS_DESKTOP: return APP;
    case ST_COMPANION_PROCESS_SERVER: return SERVER;
    case ST_COMPANION_PROCESS_PROBE: return PROBE;
    case ST_COMPANION_PROCESS_COMPANION: return "SpaceTerm Evidence Companion";
    case ST_COMPANION_PROCESS_OTHER: return "Another process";
    case ST_COMPANION_PROCESS_NONE: return NOBODY;
    case ST_COMPANION_PROCESS_UNKNOWN: break;
    }
    return "Unknown";
}

static const char *const result_tokens[ST_COMPANION_RESULT_COUNT] = {
    "unrun",  "passed",  "denied",        "cancelled",    "unavailable",
    "timed-out", "failed", "pending", "indeterminate", "cleanup-unresolved",
};

static const st_companion_status_copy result_copy[ST_COMPANION_RESULT_COUNT] = {
    {"Not run", "This case has not run in this evidence run yet.", "circle.dashed",
     ST_COMPANION_TONE_NEUTRAL},
    {"Passed", "The probe observed the effect.", "checkmark.circle", ST_COMPANION_TONE_POSITIVE},
    {"Denied", "Access was refused. The companion does not change permissions.",
     "xmark.octagon", ST_COMPANION_TONE_NEGATIVE},
    {"Cancelled", "The request was cancelled before anyone decided.", "minus.circle",
     ST_COMPANION_TONE_CAUTION},
    {"Unavailable",
     "This case cannot run in this session, for example without a login session or "
     "hardware.",
     "slash.circle", ST_COMPANION_TONE_CAUTION},
    {"Timed out", "No decision or effect arrived before the wait ended.",
     "clock.badge.exclamationmark", ST_COMPANION_TONE_CAUTION},
    {"Failed", "The probe failed for a reason other than a permission decision.",
     "exclamationmark.triangle", ST_COMPANION_TONE_NEGATIVE},
    {"In progress", "The probe is running or waiting for a person.", "hourglass",
     ST_COMPANION_TONE_PROGRESS},
    {"Indeterminate", "The outcome could not be established. Treat this case as not passed.",
     "questionmark.circle", ST_COMPANION_TONE_CAUTION},
    {"Cleanup unresolved",
     "The probe's synthetic resources may remain. The evidence run keeps its cleanup receipt.",
     "exclamationmark.arrow.circlepath", ST_COMPANION_TONE_NEGATIVE},
};

const char *st_companion_result_token(st_companion_result result)
{
    if ((unsigned)result >= ST_COMPANION_RESULT_COUNT) return "failed";
    return result_tokens[result];
}

bool st_companion_result_parse(const char *token, size_t length, st_companion_result *out)
{
    if (token == NULL || out == NULL) return false;
    for (size_t i = 0; i < ST_COMPANION_RESULT_COUNT; ++i) {
        if (strlen(result_tokens[i]) == length && memcmp(result_tokens[i], token, length) == 0) {
            *out = (st_companion_result)i;
            return true;
        }
    }
    return false;
}

st_companion_status_copy st_companion_result_copy(st_companion_result result)
{
    if ((unsigned)result >= ST_COMPANION_RESULT_COUNT) return result_copy[ST_COMPANION_RESULT_FAILED];
    return result_copy[result];
}

st_companion_status_copy st_companion_run_copy(st_companion_run_state state)
{
    switch (state) {
    case ST_COMPANION_RUN_DISCONNECTED:
        return (st_companion_status_copy){
            "Not connected",
            "No evidence controller is connected, so probes and authorization are unavailable.",
            "bolt.horizontal.circle", ST_COMPANION_TONE_NEUTRAL};
    case ST_COMPANION_RUN_CONNECTING:
        return (st_companion_status_copy){"Connecting",
                                          "Connecting to the evidence controller.",
                                          "hourglass", ST_COMPANION_TONE_PROGRESS};
    case ST_COMPANION_RUN_ACTIVE:
        return (st_companion_status_copy){
            "Evidence run active",
            "You can request probes, request authorization, or stop the evidence run.",
            "play.circle", ST_COMPANION_TONE_POSITIVE};
    case ST_COMPANION_RUN_STOP_REQUESTED:
        return (st_companion_status_copy){
            "Stop requested",
            "The companion asked the evidence controller to stop. The controller performs "
            "cleanup and reports the result.",
            "hourglass", ST_COMPANION_TONE_PROGRESS};
    case ST_COMPANION_RUN_STOPPING:
        return (st_companion_status_copy){
            "Stopping",
            "The evidence controller is stopping the run and removing its recorded resources.",
            "hourglass", ST_COMPANION_TONE_PROGRESS};
    case ST_COMPANION_RUN_STOPPED:
        return (st_companion_status_copy){
            "Stopped",
            "The evidence controller reported that the run stopped and its cleanup completed.",
            "stop.circle", ST_COMPANION_TONE_NEUTRAL};
    case ST_COMPANION_RUN_CLEANUP_UNRESOLVED:
        return (st_companion_status_copy){
            "Cleanup unresolved",
            "The evidence controller could not confirm cleanup. Its receipt and protected "
            "resources remain for recovery.",
            "exclamationmark.arrow.circlepath", ST_COMPANION_TONE_NEGATIVE};
    case ST_COMPANION_RUN_FAILED:
        return (st_companion_status_copy){
            "Run failed",
            "The evidence controller reported a failed run. Its own report states the "
            "cleanup result.",
            "exclamationmark.triangle", ST_COMPANION_TONE_NEGATIVE};
    case ST_COMPANION_RUN_CONTROLLER_LOST:
        return (st_companion_status_copy){
            "Controller disconnected",
            "The connection ended. The companion cannot tell whether the run stopped or "
            "was cleaned up.",
            "bolt.horizontal.circle", ST_COMPANION_TONE_CAUTION};
    case ST_COMPANION_RUN_PROTOCOL_ERROR:
        return (st_companion_status_copy){
            "Connection closed",
            "The controller sent a record the companion does not accept, so the companion "
            "closed the connection. The run's state is unknown.",
            "exclamationmark.triangle", ST_COMPANION_TONE_NEGATIVE};
    case ST_COMPANION_RUN_REJECTED:
        return (st_companion_status_copy){
            "Connection refused",
            "The controller's socket failed the ownership checks, so the companion did not "
            "connect.",
            "xmark.shield", ST_COMPANION_TONE_NEGATIVE};
    }
    return st_companion_run_copy(ST_COMPANION_RUN_PROTOCOL_ERROR);
}

bool st_companion_run_accepts_requests(st_companion_run_state state)
{
    return state == ST_COMPANION_RUN_ACTIVE;
}

static const st_companion_action_info actions[ST_COMPANION_ACTION_COUNT] = {
    {ST_COMPANION_ACTION_REQUEST_PROBE, "Request Probe", ST_COMPANION_EFFECT_CONTROLLER_REQUEST,
     false},
    {ST_COMPANION_ACTION_OPEN_PRIVACY_SETTINGS, "Open Privacy Settings",
     ST_COMPANION_EFFECT_OPEN_SETTINGS_PANE, false},
    {ST_COMPANION_ACTION_REQUEST_AUTHORIZATION, "Request Authorization\xE2\x80\xA6",
     ST_COMPANION_EFFECT_MANAGEMENT_AUTHORIZATION, true},
    {ST_COMPANION_ACTION_STOP_RUN, "Stop Evidence Run\xE2\x80\xA6",
     ST_COMPANION_EFFECT_CONTROLLER_REQUEST, true},
    {ST_COMPANION_ACTION_SHOW_COMPANION, "Companion", ST_COMPANION_EFFECT_PRESENTATION, false},
    {ST_COMPANION_ACTION_SHOW_TARGETS, "Show Test Targets", ST_COMPANION_EFFECT_PRESENTATION,
     false},
    {ST_COMPANION_ACTION_PASTE_MARKER, "Paste", ST_COMPANION_EFFECT_READ_PASTED_MARKER, false},
    {ST_COMPANION_ACTION_QUIT, "Quit SpaceTerm Evidence Companion",
     ST_COMPANION_EFFECT_QUIT_COMPANION, true},
};

const st_companion_action_info *st_companion_action_find(uint32_t action)
{
    if (action < 1 || action > ST_COMPANION_ACTION_COUNT) return NULL;
    return &actions[action - 1];
}

st_companion_quit_plan st_companion_plan_quit(st_companion_run_state state)
{
    static const char title[] = "Quit SpaceTerm Evidence Companion?";
    switch (state) {
    case ST_COMPANION_RUN_ACTIVE:
    case ST_COMPANION_RUN_STOP_REQUESTED:
    case ST_COMPANION_RUN_STOPPING:
        return (st_companion_quit_plan){
            true, title,
            "Quitting does not stop the evidence run or clean it up. The evidence controller "
            "and SpaceTerm Server keep running, and the controller still owns cleanup. "
            "Probes that need this companion's test targets will fail or time out."};
    default:
        return (st_companion_quit_plan){
            false, title,
            "Quitting does not stop any evidence run or clean anything up."};
    }
}

static const char *const failure_names[34] = {
    "unsupported-host",
    "native-unavailable",
    "custody-unavailable",
    "account-identity-unavailable",
    "registration-missing",
    "storage-locked",
    "storage-unavailable",
    "unsafe-native-state",
    "invalid-record",
    "unsupported-schema",
    "legacy-registration-requires-migration",
    "identity-mismatch",
    "generation-mismatch",
    "stale-assessment",
    "confirmation-required",
    "authorization-denied",
    "authorization-cancelled",
    "authorization-unavailable",
    "installation-unavailable",
    "artifact-rejected",
    "credential-unavailable",
    "credential-policy-rejected",
    "interaction-required",
    "supervisor-unavailable",
    "background-items-suppressed",
    "process-identity-mismatch",
    "privilege-drop-unproven",
    "execution-binding-unproven",
    "timed-out",
    "limit-exceeded",
    "outcome-pending",
    "indeterminate",
    "cleanup-unresolved",
    "io-unavailable",
};

const char *st_companion_failure_name(uint32_t failure)
{
    if (failure < 1 || failure > 34) return NULL;
    return failure_names[failure - 1];
}
