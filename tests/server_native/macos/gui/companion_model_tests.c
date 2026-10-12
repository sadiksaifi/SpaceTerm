#include "companion_authorization.h"
#include "companion_model.h"
#include "companion_protocol.h"
#include "companion_receipts.h"

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Portable tests for the evidence companion's policy, receipts, controller
 * records, and authorization flow. They use synthetic IDs and fake ports only.
 * Nothing here touches AppKit, the pasteboard, LaunchServices, Apple Events,
 * Authorization Services, or any System Permission.
 */

#define CHECK(condition)                                                      \
    do {                                                                      \
        if (!(condition)) {                                                   \
            fprintf(stderr, "check failed at line %d\n", __LINE__);           \
            return false;                                                     \
        }                                                                     \
    } while (0)

static const uint64_t second_ns = UINT64_C(1000000000);

static st_companion_id synthetic_id(uint8_t seed)
{
    st_companion_id id;
    for (size_t i = 0; i < ST_COMPANION_ID_BYTES; ++i) id.bytes[i] = (uint8_t)(seed + i * 17u);
    return id;
}

static void format_id(uint8_t seed, char out[ST_COMPANION_ID_HEX + 1])
{
    st_companion_id id = synthetic_id(seed);
    st_companion_id_format(&id, out);
}

static bool contains(const char *text, const char *needle)
{
    return text != NULL && needle != NULL && strstr(text, needle) != NULL;
}

/* Case-insensitive, so "Capability probe" matches "the capability probe". */
static bool mentions(const char *text, const char *name)
{
    if (text == NULL || name == NULL || name[0] == '\0') return false;
    size_t length = strlen(name);
    for (const char *start = text; *start; ++start) {
        size_t i = 0;
        while (i < length && start[i] != '\0') {
            char left = start[i], right = name[i];
            if (left >= 'A' && left <= 'Z') left = (char)(left - 'A' + 'a');
            if (right >= 'A' && right <= 'Z') right = (char)(right - 'A' + 'a');
            if (left != right) break;
            ++i;
        }
        if (i == length) return true;
    }
    return false;
}

static size_t make_url(char *out, size_t capacity, uint8_t run, uint8_t nonce)
{
    char run_hex[ST_COMPANION_ID_HEX + 1];
    char nonce_hex[ST_COMPANION_ID_HEX + 1];
    format_id(run, run_hex);
    format_id(nonce, nonce_hex);
    int written = snprintf(out, capacity, "%s://receipt?run=%s&nonce=%s", ST_COMPANION_URL_SCHEME,
                           run_hex, nonce_hex);
    return written < 0 ? 0 : (size_t)written;
}

static void bound_ledger(st_companion_ledger *ledger, uint8_t run)
{
    st_companion_id id = synthetic_id(run);
    st_companion_ledger_init(ledger);
    (void)st_companion_ledger_bind(ledger, &id);
}

static st_companion_arm_status arm(st_companion_ledger *ledger, st_companion_target target,
                                   uint8_t nonce, uint32_t deadline_ms, uint64_t now_ns)
{
    st_companion_id id = synthetic_id(nonce);
    return st_companion_ledger_arm(ledger, target, &id, deadline_ms, now_ns, 41);
}

/* --- Catalog and copy ------------------------------------------------------ */

static bool capability_catalog_covers_codes(void)
{
    CHECK(st_companion_capability_find(0) == NULL);
    CHECK(st_companion_capability_find(ST_COMPANION_CAPABILITY_COUNT + 1) == NULL);
    for (uint32_t code = 1; code <= ST_COMPANION_CAPABILITY_COUNT; ++code) {
        const st_companion_capability *capability = st_companion_capability_find(code);
        CHECK(capability != NULL);
        CHECK(capability->code == code);
        CHECK(capability->name != NULL && capability->name[0] != '\0');
        CHECK(capability->summary != NULL && capability->summary[0] != '\0');
        CHECK(capability->wait_seconds == 5 || capability->wait_seconds == 60);
        CHECK(capability->needs_person == (capability->wait_seconds == 60));
        for (uint32_t other = 1; other < code; ++other) {
            CHECK(strcmp(st_companion_capability_find(other)->name, capability->name) != 0);
        }
    }
    CHECK(st_companion_capability_find(ST_COMPANION_CAPABILITY_URL_OPENING)->target ==
          ST_COMPANION_TARGET_URL);
    CHECK(st_companion_capability_find(ST_COMPANION_CAPABILITY_APPLE_EVENTS)->target ==
          ST_COMPANION_TARGET_APPLE_EVENT);
    CHECK(st_companion_capability_find(ST_COMPANION_CAPABILITY_ACCESSIBILITY)->target ==
          ST_COMPANION_TARGET_ACCESSIBILITY);
    CHECK(st_companion_capability_find(ST_COMPANION_CAPABILITY_CLIPBOARD_TOOLS)->target ==
          ST_COMPANION_TARGET_CLIPBOARD);
    CHECK(st_companion_capability_find(ST_COMPANION_CAPABILITY_SCREEN_RECORDING)->target ==
          ST_COMPANION_TARGET_CAPTURE);
    return true;
}

static bool explanations_name_requesting_process(void)
{
    for (uint32_t code = 1; code <= ST_COMPANION_CAPABILITY_COUNT; ++code) {
        const st_companion_capability *capability = st_companion_capability_find(code);
        for (size_t context = 0; context < ST_COMPANION_CONTEXT_COUNT; ++context) {
            const st_companion_context_copy *copy = &capability->contexts[context];
            CHECK(copy->requester != NULL && copy->requester[0] != '\0');
            CHECK(copy->explanation != NULL && copy->explanation[0] != '\0');
            if (copy->process != ST_COMPANION_PROCESS_NONE) {
                CHECK(mentions(copy->explanation, copy->requester));
            }
        }
        if (capability->system_permission) {
            const st_companion_context_copy *gui =
                &capability->contexts[ST_COMPANION_CONTEXT_GUI_PARENTED];
            const st_companion_context_copy *daemon =
                &capability->contexts[ST_COMPANION_CONTEXT_DAEMON_PARENTED];
            CHECK(gui->process == ST_COMPANION_PROCESS_DESKTOP);
            CHECK(daemon->process == ST_COMPANION_PROCESS_SERVER);
            CHECK(contains(gui->explanation, "SpaceTerm app"));
            CHECK(contains(daemon->explanation, "SpaceTerm Server"));
            CHECK(!contains(gui->explanation, "SpaceTerm Server"));
        }
    }
    return true;
}

/* macOS never asks the process that writes the clipboard, and a person's own
 * Edit > Paste reads it without a prompt. No context names a requester. */
static bool clipboard_explanation_names_no_requester(void)
{
    const st_companion_capability *clipboard =
        st_companion_capability_find(ST_COMPANION_CAPABILITY_CLIPBOARD_TOOLS);
    CHECK(clipboard != NULL);
    CHECK(!clipboard->system_permission);
    CHECK(clipboard->settings_anchor == NULL);
    CHECK(clipboard->needs_person);
    for (size_t context = 0; context < ST_COMPANION_CONTEXT_COUNT; ++context) {
        const st_companion_context_copy *copy = &clipboard->contexts[context];
        CHECK(copy->process == ST_COMPANION_PROCESS_NONE);
        CHECK(!contains(copy->explanation, "asks about"));
        CHECK(contains(copy->explanation, "needs no System Permission"));
        CHECK(contains(copy->explanation, "does not ask the process that copies"));
    }
    CHECK(contains(clipboard->contexts[ST_COMPANION_CONTEXT_GUI_PARENTED].explanation,
                   "Edit > Paste"));
    CHECK(contains(clipboard->contexts[ST_COMPANION_CONTEXT_DAEMON_PARENTED].explanation,
                   "server session"));
    return true;
}

static bool permission_scope_names_both_processes(void)
{
    const char *scope = st_companion_permission_scope();
    CHECK(contains(scope, "SpaceTerm app"));
    CHECK(contains(scope, "SpaceTerm Server"));
    CHECK(contains(scope, "do not apply"));
    CHECK(strcmp(st_companion_process_name(ST_COMPANION_PROCESS_DESKTOP), "SpaceTerm app") == 0);
    CHECK(strcmp(st_companion_process_name(ST_COMPANION_PROCESS_SERVER), "SpaceTerm Server") == 0);
    CHECK(st_companion_process_name((st_companion_process)99) != NULL);
    return true;
}

static bool settings_anchors_are_allowlisted(void)
{
    static const char *const allowed[] = {"Privacy_FilesAndFolders", "Privacy_Microphone",
                                          "Privacy_ScreenCapture", "Privacy_Accessibility",
                                          "Privacy_Automation"};
    for (uint32_t code = 1; code <= ST_COMPANION_CAPABILITY_COUNT; ++code) {
        const st_companion_capability *capability = st_companion_capability_find(code);
        if (capability->settings_anchor == NULL) continue;
        CHECK(capability->system_permission);
        bool found = false;
        for (size_t i = 0; i < sizeof allowed / sizeof allowed[0]; ++i) {
            if (strcmp(allowed[i], capability->settings_anchor) == 0) found = true;
        }
        CHECK(found);
    }
    CHECK(st_companion_capability_find(ST_COMPANION_CAPABILITY_URL_OPENING)->settings_anchor ==
          NULL);
    CHECK(st_companion_capability_find(ST_COMPANION_CAPABILITY_SHELL_ENVIRONMENT)
              ->settings_anchor == NULL);
    return true;
}

static bool results_have_distinct_tokens_and_copy(void)
{
    for (int result = 0; result < ST_COMPANION_RESULT_COUNT; ++result) {
        const char *token = st_companion_result_token((st_companion_result)result);
        st_companion_status_copy copy = st_companion_result_copy((st_companion_result)result);
        CHECK(token != NULL && copy.title != NULL && copy.detail != NULL && copy.symbol != NULL);
        st_companion_result parsed = ST_COMPANION_RESULT_FAILED;
        CHECK(st_companion_result_parse(token, strlen(token), &parsed));
        CHECK(parsed == (st_companion_result)result);
        for (int other = 0; other < result; ++other) {
            st_companion_status_copy prior = st_companion_result_copy((st_companion_result)other);
            CHECK(strcmp(prior.title, copy.title) != 0);
            CHECK(strcmp(prior.detail, copy.detail) != 0);
            CHECK(strcmp(st_companion_result_token((st_companion_result)other), token) != 0);
        }
    }
    return true;
}

static bool result_parse_rejects_unknown_tokens(void)
{
    st_companion_result parsed = ST_COMPANION_RESULT_PASSED;
    CHECK(!st_companion_result_parse("pass", 4, &parsed));
    CHECK(!st_companion_result_parse("passed ", 7, &parsed));
    CHECK(!st_companion_result_parse("Passed", 6, &parsed));
    CHECK(!st_companion_result_parse("", 0, &parsed));
    CHECK(!st_companion_result_parse(NULL, 0, &parsed));
    CHECK(st_companion_result_parse("timed-out", 9, &parsed));
    CHECK(parsed == ST_COMPANION_RESULT_TIMED_OUT);
    return true;
}

static bool run_requests_only_while_active(void)
{
    for (int state = 0; state <= ST_COMPANION_RUN_REJECTED; ++state) {
        bool accepts = st_companion_run_accepts_requests((st_companion_run_state)state);
        CHECK(accepts == (state == ST_COMPANION_RUN_ACTIVE));
        st_companion_status_copy copy = st_companion_run_copy((st_companion_run_state)state);
        CHECK(copy.title != NULL && copy.detail != NULL && copy.symbol != NULL);
    }
    CHECK(contains(st_companion_run_copy(ST_COMPANION_RUN_CONTROLLER_LOST).detail,
                   "cannot tell"));
    return true;
}

static bool action_effects_exclude_grants_and_resets(void)
{
    CHECK(st_companion_action_find(0) == NULL);
    CHECK(st_companion_action_find(ST_COMPANION_ACTION_COUNT + 1) == NULL);
    for (uint32_t action = 1; action <= ST_COMPANION_ACTION_COUNT; ++action) {
        const st_companion_action_info *info = st_companion_action_find(action);
        CHECK(info != NULL && info->action == (st_companion_action)action);
        CHECK(info->effect >= ST_COMPANION_EFFECT_PRESENTATION &&
              info->effect <= ST_COMPANION_EFFECT_QUIT_COMPANION);
    }
    CHECK(st_companion_action_find(ST_COMPANION_ACTION_STOP_RUN)->effect ==
          ST_COMPANION_EFFECT_CONTROLLER_REQUEST);
    CHECK(st_companion_action_find(ST_COMPANION_ACTION_STOP_RUN)->confirms_first);
    CHECK(st_companion_action_find(ST_COMPANION_ACTION_REQUEST_AUTHORIZATION)->effect ==
          ST_COMPANION_EFFECT_MANAGEMENT_AUTHORIZATION);
    CHECK(st_companion_action_find(ST_COMPANION_ACTION_OPEN_PRIVACY_SETTINGS)->effect ==
          ST_COMPANION_EFFECT_OPEN_SETTINGS_PANE);
    return true;
}

static bool quit_plan_never_claims_stop_or_cleanup(void)
{
    for (int state = 0; state <= ST_COMPANION_RUN_REJECTED; ++state) {
        st_companion_quit_plan plan = st_companion_plan_quit((st_companion_run_state)state);
        CHECK(plan.title != NULL && plan.message != NULL);
        CHECK(contains(plan.message, "does not stop"));
        CHECK(!contains(plan.message, "cleaned up."));
        CHECK(!contains(plan.message, "stopped the"));
    }
    CHECK(st_companion_plan_quit(ST_COMPANION_RUN_ACTIVE).confirm);
    CHECK(st_companion_plan_quit(ST_COMPANION_RUN_STOPPING).confirm);
    CHECK(!st_companion_plan_quit(ST_COMPANION_RUN_DISCONNECTED).confirm);
    CHECK(!st_companion_plan_quit(ST_COMPANION_RUN_STOPPED).confirm);
    return true;
}

static bool failure_names_cover_native_failures(void)
{
    CHECK(st_companion_failure_name(0) == NULL);
    CHECK(st_companion_failure_name(35) == NULL);
    for (uint32_t failure = 1; failure <= 34; ++failure) {
        const char *name = st_companion_failure_name(failure);
        CHECK(name != NULL && name[0] != '\0');
        for (const char *c = name; *c; ++c) CHECK((*c >= 'a' && *c <= 'z') || *c == '-');
    }
    CHECK(strcmp(st_companion_failure_name(2), "native-unavailable") == 0);
    CHECK(strcmp(st_companion_failure_name(16), "authorization-denied") == 0);
    CHECK(strcmp(st_companion_failure_name(17), "authorization-cancelled") == 0);
    CHECK(strcmp(st_companion_failure_name(18), "authorization-unavailable") == 0);
    CHECK(strcmp(st_companion_failure_name(25), "background-items-suppressed") == 0);
    CHECK(strcmp(st_companion_failure_name(34), "io-unavailable") == 0);
    return true;
}

/* --- Identifiers and receipts --------------------------------------------- */

static bool identifier_parse_is_strict(void)
{
    char hex[ST_COMPANION_ID_HEX + 1];
    format_id(3, hex);
    st_companion_id parsed;
    st_companion_id expected = synthetic_id(3);
    CHECK(st_companion_id_parse(hex, ST_COMPANION_ID_HEX, &parsed));
    CHECK(st_companion_id_equal(&parsed, &expected));
    CHECK(!st_companion_id_parse(hex, ST_COMPANION_ID_HEX - 1, &parsed));
    char upper[ST_COMPANION_ID_HEX + 1];
    memcpy(upper, hex, sizeof upper);
    for (size_t i = 0; i < ST_COMPANION_ID_HEX; ++i) {
        if (upper[i] >= 'a' && upper[i] <= 'f') {
            upper[i] = (char)(upper[i] - 'a' + 'A');
            break;
        }
    }
    CHECK(!st_companion_id_parse(upper, ST_COMPANION_ID_HEX, &parsed));
    CHECK(!st_companion_id_parse("00000000000000000000000000000000", 32, &parsed));
    CHECK(!st_companion_id_parse("0123456789abcdef0123456789abcdeg", 32, &parsed));
    return true;
}

static bool url_receipt_requires_exact_form(void)
{
    st_companion_ledger ledger;
    bound_ledger(&ledger, 1);
    CHECK(arm(&ledger, ST_COMPANION_TARGET_URL, 2, 5000, 0) == ST_COMPANION_ARM_OK);
    char url[256];
    size_t length = make_url(url, sizeof url, 1, 2);
    CHECK(length == ST_COMPANION_URL_LENGTH);
    st_companion_id nonce;

    char variant[300];
    /* Trailing fragment, extra query, other host, and uppercase scheme. */
    snprintf(variant, sizeof variant, "%s#x", url);
    CHECK(st_companion_ledger_receive_url(&ledger, variant, strlen(variant), 1, &nonce) ==
          ST_COMPANION_RECEIPT_MALFORMED);
    snprintf(variant, sizeof variant, "%s&x=1", url);
    CHECK(st_companion_ledger_receive_url(&ledger, variant, strlen(variant), 1, &nonce) ==
          ST_COMPANION_RECEIPT_MALFORMED);
    memcpy(variant, url, length + 1);
    memcpy(variant + strlen(ST_COMPANION_URL_SCHEME) + 3, "receiqt", 7);
    CHECK(st_companion_ledger_receive_url(&ledger, variant, length, 1, &nonce) ==
          ST_COMPANION_RECEIPT_MALFORMED);
    memcpy(variant, url, length + 1);
    variant[0] = 'S';
    CHECK(st_companion_ledger_receive_url(&ledger, variant, length, 1, &nonce) ==
          ST_COMPANION_RECEIPT_MALFORMED);
    CHECK(st_companion_ledger_receive_url(&ledger, url, length - 1, 1, &nonce) ==
          ST_COMPANION_RECEIPT_MALFORMED);
    CHECK(st_companion_ledger_receive_url(&ledger, NULL, 0, 1, &nonce) ==
          ST_COMPANION_RECEIPT_MALFORMED);
    /* Malformed requests never consume the armed expectation. */
    CHECK(st_companion_ledger_receive_url(&ledger, url, length, 1, &nonce) ==
          ST_COMPANION_RECEIPT_ACCEPTED);
    return true;
}

static bool url_receipt_binds_run_and_nonce(void)
{
    st_companion_ledger ledger;
    st_companion_ledger_init(&ledger);
    char url[256];
    st_companion_id nonce;
    size_t length = make_url(url, sizeof url, 1, 2);
    CHECK(st_companion_ledger_receive_url(&ledger, url, length, 0, &nonce) ==
          ST_COMPANION_RECEIPT_NO_RUN);

    bound_ledger(&ledger, 1);
    st_companion_id other_run = synthetic_id(9);
    CHECK(!st_companion_ledger_bind(&ledger, &other_run));
    CHECK(arm(&ledger, ST_COMPANION_TARGET_URL, 2, 5000, 0) == ST_COMPANION_ARM_OK);
    length = make_url(url, sizeof url, 9, 2);
    CHECK(st_companion_ledger_receive_url(&ledger, url, length, 1, &nonce) ==
          ST_COMPANION_RECEIPT_FOREIGN_RUN);
    length = make_url(url, sizeof url, 1, 3);
    CHECK(st_companion_ledger_receive_url(&ledger, url, length, 1, &nonce) ==
          ST_COMPANION_RECEIPT_UNEXPECTED);
    length = make_url(url, sizeof url, 1, 2);
    CHECK(st_companion_ledger_receive_url(&ledger, url, length, 1, &nonce) ==
          ST_COMPANION_RECEIPT_ACCEPTED);
    st_companion_id expected = synthetic_id(2);
    CHECK(st_companion_id_equal(&nonce, &expected));
    CHECK(ledger.accepted[ST_COMPANION_TARGET_URL] == 1);
    CHECK(ledger.rejected[ST_COMPANION_TARGET_URL] == 2);
    return true;
}

static bool apple_event_receipt_binds_run_and_nonce(void)
{
    st_companion_ledger ledger;
    bound_ledger(&ledger, 4);
    char run[ST_COMPANION_ID_HEX + 1];
    char foreign[ST_COMPANION_ID_HEX + 1];
    char nonce_hex[ST_COMPANION_ID_HEX + 1];
    format_id(4, run);
    format_id(5, foreign);
    format_id(6, nonce_hex);
    st_companion_id nonce;
    CHECK(arm(&ledger, ST_COMPANION_TARGET_APPLE_EVENT, 6, 60000, 0) == ST_COMPANION_ARM_OK);
    CHECK(st_companion_ledger_receive_event(&ledger, run, 31, nonce_hex, 32, 1, &nonce) ==
          ST_COMPANION_RECEIPT_MALFORMED);
    CHECK(st_companion_ledger_receive_event(&ledger, foreign, 32, nonce_hex, 32, 1, &nonce) ==
          ST_COMPANION_RECEIPT_FOREIGN_RUN);
    /* A nonce armed for the Apple Event target cannot be spent on a URL. */
    char url[256];
    size_t length = make_url(url, sizeof url, 4, 6);
    CHECK(st_companion_ledger_receive_url(&ledger, url, length, 1, &nonce) ==
          ST_COMPANION_RECEIPT_UNEXPECTED);
    CHECK(st_companion_ledger_receive_event(&ledger, run, 32, nonce_hex, 32, 1, &nonce) ==
          ST_COMPANION_RECEIPT_ACCEPTED);
    CHECK(ledger.accepted[ST_COMPANION_TARGET_APPLE_EVENT] == 1);
    return true;
}

static bool receipts_reject_replay_and_expiry(void)
{
    st_companion_ledger ledger;
    bound_ledger(&ledger, 1);
    char url[256];
    st_companion_id nonce;
    size_t length = make_url(url, sizeof url, 1, 2);
    CHECK(arm(&ledger, ST_COMPANION_TARGET_URL, 2, 1000, 0) == ST_COMPANION_ARM_OK);
    CHECK(arm(&ledger, ST_COMPANION_TARGET_URL, 2, 1000, 0) == ST_COMPANION_ARM_DUPLICATE);
    CHECK(st_companion_ledger_receive_url(&ledger, url, length, 1, &nonce) ==
          ST_COMPANION_RECEIPT_ACCEPTED);
    CHECK(st_companion_ledger_receive_url(&ledger, url, length, 2, &nonce) ==
          ST_COMPANION_RECEIPT_REPLAYED);
    /* A spent nonce cannot be armed again. */
    CHECK(arm(&ledger, ST_COMPANION_TARGET_URL, 2, 1000, 0) == ST_COMPANION_ARM_DUPLICATE);

    length = make_url(url, sizeof url, 1, 3);
    CHECK(arm(&ledger, ST_COMPANION_TARGET_URL, 3, 1000, 0) == ST_COMPANION_ARM_OK);
    CHECK(st_companion_ledger_receive_url(&ledger, url, length, second_ns + 1, &nonce) ==
          ST_COMPANION_RECEIPT_EXPIRED);
    CHECK(st_companion_ledger_receive_url(&ledger, url, length, second_ns + 2, &nonce) ==
          ST_COMPANION_RECEIPT_REPLAYED);
    CHECK(arm(&ledger, ST_COMPANION_TARGET_URL, 4, 0, 0) == ST_COMPANION_ARM_INVALID);
    CHECK(arm(&ledger, ST_COMPANION_TARGET_URL, 4, ST_COMPANION_MAX_DEADLINE_MS + 1, 0) ==
          ST_COMPANION_ARM_INVALID);
    CHECK(arm(&ledger, ST_COMPANION_TARGET_NONE, 4, 1000, 0) == ST_COMPANION_ARM_INVALID);
    return true;
}

static bool accessibility_receipt_accepts_only_armed_marker(void)
{
    st_companion_ledger ledger;
    bound_ledger(&ledger, 1);
    char marker[80];
    char nonce_hex[ST_COMPANION_ID_HEX + 1];
    format_id(7, nonce_hex);
    snprintf(marker, sizeof marker, "%s%s", ST_COMPANION_MARKER_PREFIX, nonce_hex);
    CHECK(strlen(marker) == ST_COMPANION_MARKER_LENGTH);
    st_companion_id nonce;
    CHECK(st_companion_ledger_receive_accessibility(&ledger, marker, strlen(marker), 0, &nonce) ==
          ST_COMPANION_RECEIPT_UNEXPECTED);
    CHECK(arm(&ledger, ST_COMPANION_TARGET_ACCESSIBILITY, 7, 5000, 0) == ST_COMPANION_ARM_OK);
    CHECK(st_companion_ledger_receive_accessibility(&ledger, "hello", 5, 0, &nonce) ==
          ST_COMPANION_RECEIPT_MALFORMED);
    CHECK(st_companion_ledger_receive_accessibility(&ledger, marker, strlen(marker) - 1, 0,
                                                    &nonce) == ST_COMPANION_RECEIPT_MALFORMED);
    CHECK(st_companion_ledger_receive_accessibility(&ledger, marker, strlen(marker), 0, &nonce) ==
          ST_COMPANION_RECEIPT_ACCEPTED);
    CHECK(st_companion_ledger_receive_accessibility(&ledger, marker, strlen(marker), 0, &nonce) ==
          ST_COMPANION_RECEIPT_REPLAYED);
    return true;
}

static bool clipboard_gate_refuses_unarmed_superseded_and_rich(void)
{
    st_companion_ledger ledger;
    st_companion_id nonce;
    st_companion_ledger_init(&ledger);
    CHECK(st_companion_ledger_clipboard_gate(&ledger, 41, true, 0, &nonce) ==
          ST_COMPANION_RECEIPT_NO_RUN);
    bound_ledger(&ledger, 1);
    CHECK(!st_companion_ledger_clipboard_armed(&ledger, 0));
    CHECK(st_companion_ledger_clipboard_gate(&ledger, 41, true, 0, &nonce) ==
          ST_COMPANION_RECEIPT_UNEXPECTED);

    CHECK(arm(&ledger, ST_COMPANION_TARGET_CLIPBOARD, 8, 60000, 0) == ST_COMPANION_ARM_OK);
    CHECK(st_companion_ledger_clipboard_armed(&ledger, 0));
    /* A newer marker replaces the armed one. */
    CHECK(arm(&ledger, ST_COMPANION_TARGET_CLIPBOARD, 9, 60000, 0) == ST_COMPANION_ARM_OK);
    CHECK(st_companion_ledger_clipboard_gate(&ledger, 41, false, 0, &nonce) ==
          ST_COMPANION_RECEIPT_MISMATCH);

    CHECK(arm(&ledger, ST_COMPANION_TARGET_CLIPBOARD, 10, 60000, 0) == ST_COMPANION_ARM_OK);
    CHECK(st_companion_ledger_clipboard_gate(&ledger, 42, true, 0, &nonce) ==
          ST_COMPANION_RECEIPT_SUPERSEDED);
    CHECK(!st_companion_ledger_clipboard_armed(&ledger, 0));

    CHECK(arm(&ledger, ST_COMPANION_TARGET_CLIPBOARD, 11, 1000, 0) == ST_COMPANION_ARM_OK);
    CHECK(!st_companion_ledger_clipboard_armed(&ledger, 2 * second_ns));
    CHECK(st_companion_ledger_clipboard_gate(&ledger, 41, true, 2 * second_ns, &nonce) ==
          ST_COMPANION_RECEIPT_EXPIRED);
    return true;
}

static bool clipboard_marker_comparison_consumes_expectation(void)
{
    st_companion_ledger ledger;
    bound_ledger(&ledger, 1);
    st_companion_id nonce;
    char marker[80];
    char nonce_hex[ST_COMPANION_ID_HEX + 1];
    format_id(12, nonce_hex);
    snprintf(marker, sizeof marker, "%s%s", ST_COMPANION_MARKER_PREFIX, nonce_hex);

    CHECK(arm(&ledger, ST_COMPANION_TARGET_CLIPBOARD, 12, 60000, 0) == ST_COMPANION_ARM_OK);
    CHECK(st_companion_ledger_clipboard_gate(&ledger, 41, true, 0, &nonce) ==
          ST_COMPANION_RECEIPT_ACCEPTED);
    /* The gate alone does not consume the expectation. */
    CHECK(st_companion_ledger_clipboard_armed(&ledger, 0));
    CHECK(st_companion_ledger_receive_clipboard(&ledger, (const uint8_t *)marker, strlen(marker),
                                                0, &nonce) == ST_COMPANION_RECEIPT_ACCEPTED);
    CHECK(!st_companion_ledger_clipboard_armed(&ledger, 0));
    CHECK(st_companion_ledger_receive_clipboard(&ledger, (const uint8_t *)marker, strlen(marker),
                                                0, &nonce) == ST_COMPANION_RECEIPT_UNEXPECTED);

    format_id(13, nonce_hex);
    CHECK(arm(&ledger, ST_COMPANION_TARGET_CLIPBOARD, 13, 60000, 0) == ST_COMPANION_ARM_OK);
    CHECK(st_companion_ledger_clipboard_gate(&ledger, 41, true, 0, &nonce) ==
          ST_COMPANION_RECEIPT_ACCEPTED);
    /* One wrong value ends the attempt; the person cannot probe for the marker. */
    CHECK(st_companion_ledger_receive_clipboard(&ledger, (const uint8_t *)"other text", 10, 0,
                                                &nonce) == ST_COMPANION_RECEIPT_MISMATCH);
    CHECK(!st_companion_ledger_clipboard_armed(&ledger, 0));
    return true;
}

/* Each clipboard probe copies a new marker, which supersedes the previous one.
 * Arming the new marker replaces the older expectation instead of refusing. */
static bool clipboard_expectation_replaces_older_marker(void)
{
    st_companion_ledger ledger;
    bound_ledger(&ledger, 1);
    st_companion_id first = synthetic_id(30);
    st_companion_id second = synthetic_id(31);
    st_companion_id nonce;
    CHECK(st_companion_ledger_arm(&ledger, ST_COMPANION_TARGET_CLIPBOARD, &first, 60000, 0, 41) ==
          ST_COMPANION_ARM_OK);
    CHECK(st_companion_ledger_arm(&ledger, ST_COMPANION_TARGET_CLIPBOARD, &second, 60000, 0,
                                  42) == ST_COMPANION_ARM_OK);
    size_t clipboard_slots = 0;
    for (size_t i = 0; i < ST_COMPANION_MAX_EXPECTATIONS; ++i) {
        if (ledger.expectations[i].armed &&
            ledger.expectations[i].target == ST_COMPANION_TARGET_CLIPBOARD) {
            ++clipboard_slots;
        }
    }
    CHECK(clipboard_slots == 1);
    /* The newer marker is the one a paste may read. */
    CHECK(st_companion_ledger_clipboard_gate(&ledger, 42, true, 0, &nonce) ==
          ST_COMPANION_RECEIPT_ACCEPTED);
    CHECK(st_companion_id_equal(&nonce, &second));
    CHECK(st_companion_ledger_clipboard_gate(&ledger, 41, true, 0, &nonce) ==
          ST_COMPANION_RECEIPT_SUPERSEDED);
    /* The replaced nonce stays used. */
    CHECK(st_companion_ledger_arm(&ledger, ST_COMPANION_TARGET_CLIPBOARD, &first, 60000, 0, 43) ==
          ST_COMPANION_ARM_DUPLICATE);
    return true;
}

static bool ledger_bounds_expectations_and_receipts(void)
{
    st_companion_ledger ledger;
    bound_ledger(&ledger, 1);
    for (uint8_t i = 0; i < ST_COMPANION_MAX_EXPECTATIONS; ++i) {
        CHECK(arm(&ledger, ST_COMPANION_TARGET_URL, (uint8_t)(20 + i), 60000, 0) ==
              ST_COMPANION_ARM_OK);
    }
    CHECK(arm(&ledger, ST_COMPANION_TARGET_URL, 40, 60000, 0) == ST_COMPANION_ARM_FULL);
    /* Expired expectations make room. */
    CHECK(arm(&ledger, ST_COMPANION_TARGET_URL, 40, 60000, 61 * second_ns) == ST_COMPANION_ARM_OK);

    st_companion_ledger fresh;
    bound_ledger(&fresh, 1);
    uint32_t armed = 0;
    st_companion_arm_status last = ST_COMPANION_ARM_OK;
    for (uint32_t i = 0; i < ST_COMPANION_MAX_NONCES + 4; ++i) {
        uint64_t now = (uint64_t)i * 2 * second_ns;
        st_companion_id id = synthetic_id(1);
        id.bytes[0] = (uint8_t)(i & 0xff);
        id.bytes[1] = (uint8_t)(i >> 8);
        last = st_companion_ledger_arm(&fresh, ST_COMPANION_TARGET_URL, &id, 1000, now, 0);
        if (last == ST_COMPANION_ARM_OK) ++armed;
    }
    CHECK(armed == ST_COMPANION_MAX_NONCES);
    CHECK(last == ST_COMPANION_ARM_LIMIT);

    st_companion_ledger flooded;
    bound_ledger(&flooded, 1);
    char url[256];
    st_companion_id nonce;
    size_t length = make_url(url, sizeof url, 1, 99);
    st_companion_receipt_status status = ST_COMPANION_RECEIPT_ACCEPTED;
    for (uint32_t i = 0; i < ST_COMPANION_MAX_REPORTS + 1; ++i) {
        status = st_companion_ledger_receive_url(&flooded, url, length, 0, &nonce);
    }
    CHECK(status == ST_COMPANION_RECEIPT_LIMIT);
    return true;
}

static bool capture_pattern_encodes_nonce(void)
{
    uint8_t cells[ST_COMPANION_CAPTURE_ROWS][ST_COMPANION_CAPTURE_COLUMNS];
    st_companion_id nonce = synthetic_id(5);
    st_companion_capture_pattern(&nonce, cells);
    for (size_t row = 0; row < ST_COMPANION_CAPTURE_ROWS; ++row) {
        for (size_t column = 0; column < ST_COMPANION_CAPTURE_COLUMNS; ++column) {
            bool edge = row == 0 || column == 0 || row == ST_COMPANION_CAPTURE_ROWS - 1 ||
                        column == ST_COMPANION_CAPTURE_COLUMNS - 1;
            if (edge) {
                CHECK(cells[row][column] == ST_COMPANION_CAPTURE_FRAME);
                continue;
            }
            size_t bit = (row - 1) * (ST_COMPANION_CAPTURE_COLUMNS - 2) + (column - 1);
            bool set = (nonce.bytes[bit / 8] >> (7 - bit % 8)) & 1u;
            CHECK(cells[row][column] ==
                  (set ? ST_COMPANION_CAPTURE_BLACK : ST_COMPANION_CAPTURE_WHITE));
        }
    }
    st_companion_capture_pattern(NULL, cells);
    CHECK(cells[0][0] == ST_COMPANION_CAPTURE_FRAME);
    CHECK(cells[4][8] == ST_COMPANION_CAPTURE_WHITE);

    st_companion_ledger ledger;
    bound_ledger(&ledger, 1);
    CHECK(!ledger.capture_armed);
    CHECK(arm(&ledger, ST_COMPANION_TARGET_CAPTURE, 5, 5000, 0) == ST_COMPANION_ARM_OK);
    CHECK(ledger.capture_armed && st_companion_id_equal(&ledger.capture, &nonce));
    return true;
}

/* --- Controller records --------------------------------------------------- */

static bool parse_text(const char *line, st_companion_inbound *out)
{
    return st_companion_protocol_parse(line, strlen(line), out);
}

static bool protocol_parses_each_inbound_record(void)
{
    char run[ST_COMPANION_ID_HEX + 1];
    char line[ST_COMPANION_MAX_RECORD];
    st_companion_inbound record;
    format_id(1, run);

    snprintf(line, sizeof line, "welcome schema=1 run=%s", run);
    CHECK(parse_text(line, &record) && record.kind == ST_COMPANION_INBOUND_WELCOME);
    st_companion_id expected = synthetic_id(1);
    CHECK(st_companion_id_equal(&record.value.welcome.run, &expected));

    CHECK(parse_text("run state=cleanup-unresolved", &record));
    CHECK(record.kind == ST_COMPANION_INBOUND_RUN);
    CHECK(record.value.run.state == ST_COMPANION_RUN_CLEANUP_UNRESOLVED);

    snprintf(line, sizeof line, "expect target=apple-event nonce=%s deadline-ms=60000", run);
    CHECK(parse_text(line, &record) && record.kind == ST_COMPANION_INBOUND_EXPECT);
    CHECK(record.value.expect.target == ST_COMPANION_TARGET_APPLE_EVENT);
    CHECK(record.value.expect.deadline_ms == 60000);

    CHECK(parse_text("progress capability=4 context=daemon-parented phase=waiting-for-person",
                     &record));
    CHECK(record.kind == ST_COMPANION_INBOUND_PROGRESS);
    CHECK(record.value.progress.capability == 4);
    CHECK(record.value.progress.context == ST_COMPANION_CONTEXT_DAEMON_PARENTED);
    CHECK(record.value.progress.phase == ST_COMPANION_PHASE_WAITING_FOR_PERSON);

    CHECK(parse_text("result capability=5 context=gui-parented result=denied responsible=desktop "
                     "preflight=denied effect=blocked cleanup=not-needed",
                     &record));
    CHECK(record.kind == ST_COMPANION_INBOUND_RESULT);
    CHECK(record.value.result.capability == 5);
    CHECK(record.value.result.context == ST_COMPANION_CONTEXT_GUI_PARENTED);
    CHECK(record.value.result.observation.result == ST_COMPANION_RESULT_DENIED);
    CHECK(record.value.result.observation.responsible == ST_COMPANION_PROCESS_DESKTOP);
    CHECK(record.value.result.observation.preflight == ST_COMPANION_PREFLIGHT_DENIED);
    CHECK(record.value.result.observation.effect == ST_COMPANION_OBSERVED_BLOCKED);
    CHECK(record.value.result.observation.cleanup == ST_COMPANION_CLEANUP_NOT_NEEDED);

    CHECK(parse_text("ack seq=12 disposition=busy", &record));
    CHECK(record.kind == ST_COMPANION_INBOUND_ACK);
    CHECK(record.value.ack.sequence == 12 && record.value.ack.disposition == ST_COMPANION_ACK_BUSY);
    return true;
}

static bool protocol_rejects_noncanonical_records(void)
{
    char run[ST_COMPANION_ID_HEX + 1];
    char line[ST_COMPANION_MAX_RECORD + 8];
    st_companion_inbound record;
    format_id(1, run);
    static const char *const rejected[] = {
        "",
        "run",
        "run state=active ",
        " run state=active",
        "run  state=active",
        "run state=Active",
        "run state=active extra=1",
        "run state=paused",
        "run state=stop-requested",
        "ack disposition=busy seq=12",
        "ack seq=012 disposition=busy",
        "ack seq=0 disposition=busy",
        "ack seq=1000001 disposition=busy",
        "progress capability=14 context=gui-parented phase=queued",
        "progress capability=4 context=headless phase=queued",
        ("result capability=5 context=gui-parented result=granted responsible=desktop "
         "preflight=denied effect=blocked cleanup=not-needed"),
        "hello schema=1",
        "welcome schema=2 run=0123456789abcdef0123456789abcdef",
        "expect target=window nonce=0123456789abcdef0123456789abcdef deadline-ms=5",
        "expect target=url nonce=0123456789abcdef0123456789abcdef deadline-ms=60001",
        "expect target=url nonce=0123456789abcdef0123456789abcdef deadline-ms=0",
        "run state=active\r",
    };
    for (size_t i = 0; i < sizeof rejected / sizeof rejected[0]; ++i) {
        CHECK(!parse_text(rejected[i], &record));
    }
    memset(line, 'a', sizeof line);
    memcpy(line, "run state=", 10);
    CHECK(!st_companion_protocol_parse(line, ST_COMPANION_MAX_RECORD, &record));
    snprintf(line, sizeof line, "welcome schema=1 run=%s", run);
    CHECK(!st_companion_protocol_parse(line, strlen(line), NULL));
    CHECK(!st_companion_protocol_parse(NULL, 0, &record));
    return true;
}

static bool protocol_passed_requires_observed_effect(void)
{
    st_companion_inbound record;
    CHECK(!parse_text("result capability=7 context=daemon-parented result=passed "
                      "responsible=unknown preflight=not-applicable effect=absent cleanup=complete",
                      &record));
    CHECK(!parse_text("result capability=7 context=daemon-parented result=passed "
                      "responsible=unknown preflight=not-applicable effect=unknown cleanup=complete",
                      &record));
    CHECK(parse_text("result capability=7 context=daemon-parented result=passed "
                     "responsible=unknown preflight=not-applicable effect=observed cleanup=complete",
                     &record));
    CHECK(record.value.result.observation.effect == ST_COMPANION_OBSERVED_EFFECT);
    return true;
}

static bool protocol_encoders_emit_fixed_records(void)
{
    char out[ST_COMPANION_MAX_RECORD];
    char run[ST_COMPANION_ID_HEX + 1];
    char expected[ST_COMPANION_MAX_RECORD];
    st_companion_id run_id = synthetic_id(1);
    format_id(1, run);

    snprintf(expected, sizeof expected, "hello schema=1 run=%s pid=4242\n", run);
    CHECK(st_companion_encode_hello(out, sizeof out, &run_id, 4242) == strlen(expected));
    CHECK(strcmp(out, expected) == 0);
    CHECK(st_companion_encode_targets(out, sizeof out, 77) == strlen("targets capture-window=77\n"));
    CHECK(strcmp(out, "targets capture-window=77\n") == 0);
    CHECK(st_companion_encode_targets(out, sizeof out, 0) == 0);
    CHECK(st_companion_encode_probe_request(out, sizeof out, 3, 8) > 0);
    CHECK(strcmp(out, "request seq=3 kind=probe capability=8\n") == 0);
    CHECK(st_companion_encode_probe_request(out, sizeof out, 3, 14) == 0);
    CHECK(st_companion_encode_probe_request(out, sizeof out, 0, 8) == 0);
    CHECK(st_companion_encode_stop_request(out, sizeof out, 4) > 0);
    CHECK(strcmp(out, "request seq=4 kind=stop-run\n") == 0);

    st_companion_id nonce = synthetic_id(2);
    char nonce_hex[ST_COMPANION_ID_HEX + 1];
    format_id(2, nonce_hex);
    snprintf(expected, sizeof expected, "receipt target=url status=accepted nonce=%s\n", nonce_hex);
    CHECK(st_companion_encode_receipt(out, sizeof out, ST_COMPANION_TARGET_URL,
                                      ST_COMPANION_RECEIPT_ACCEPTED, &nonce) > 0);
    CHECK(strcmp(out, expected) == 0);
    /* Statuses for unrecognized requests never echo a sender-chosen value. */
    CHECK(st_companion_encode_receipt(out, sizeof out, ST_COMPANION_TARGET_APPLE_EVENT,
                                      ST_COMPANION_RECEIPT_FOREIGN_RUN, &nonce) > 0);
    CHECK(strcmp(out, "receipt target=apple-event status=foreign-run\n") == 0);
    CHECK(st_companion_encode_receipt(out, sizeof out, ST_COMPANION_TARGET_URL,
                                      ST_COMPANION_RECEIPT_ACCEPTED, NULL) == 0);
    snprintf(expected, sizeof expected, "armed target=capture nonce=%s\n", nonce_hex);
    CHECK(st_companion_encode_armed(out, sizeof out, ST_COMPANION_TARGET_CAPTURE, &nonce) ==
          strlen(expected));
    CHECK(strcmp(out, expected) == 0);
    CHECK(st_companion_encode_armed(out, sizeof out, ST_COMPANION_TARGET_NONE, &nonce) == 0);
    CHECK(st_companion_encode_armed(out, sizeof out, ST_COMPANION_TARGET_URL, NULL) == 0);

    /* A refusal names the target, the controller's own nonce, and a reason. */
    snprintf(expected, sizeof expected, "refused target=clipboard nonce=%s reason=full\n",
             nonce_hex);
    CHECK(st_companion_encode_refused(out, sizeof out, ST_COMPANION_TARGET_CLIPBOARD, &nonce,
                                      ST_COMPANION_REFUSAL_FULL) == strlen(expected));
    CHECK(strcmp(out, expected) == 0);
    static const struct {
        st_companion_arm_status status;
        st_companion_refusal refusal;
        const char *token;
    } refusals[] = {
        {ST_COMPANION_ARM_DUPLICATE, ST_COMPANION_REFUSAL_DUPLICATE, "duplicate"},
        {ST_COMPANION_ARM_FULL, ST_COMPANION_REFUSAL_FULL, "full"},
        {ST_COMPANION_ARM_LIMIT, ST_COMPANION_REFUSAL_LIMIT, "limit"},
    };
    for (size_t i = 0; i < sizeof refusals / sizeof refusals[0]; ++i) {
        CHECK(st_companion_refusal_for_arm(refusals[i].status) == refusals[i].refusal);
        CHECK(strcmp(st_companion_refusal_token(refusals[i].refusal), refusals[i].token) == 0);
    }
    CHECK(strcmp(st_companion_refusal_token(ST_COMPANION_REFUSAL_INACTIVE), "inactive") == 0);
    /* OK needs no refusal; NO_RUN and INVALID only follow a protocol violation. */
    CHECK(st_companion_refusal_for_arm(ST_COMPANION_ARM_OK) == 0);
    CHECK(st_companion_refusal_for_arm(ST_COMPANION_ARM_NO_RUN) == 0);
    CHECK(st_companion_refusal_for_arm(ST_COMPANION_ARM_INVALID) == 0);
    CHECK(st_companion_encode_refused(out, sizeof out, ST_COMPANION_TARGET_NONE, &nonce,
                                      ST_COMPANION_REFUSAL_FULL) == 0);
    CHECK(st_companion_encode_refused(out, sizeof out, ST_COMPANION_TARGET_URL, NULL,
                                      ST_COMPANION_REFUSAL_FULL) == 0);
    CHECK(st_companion_encode_refused(out, sizeof out, ST_COMPANION_TARGET_URL, &nonce,
                                      (st_companion_refusal)0) == 0);
    CHECK(st_companion_encode_authorization(out, sizeof out, ST_COMPANION_OPERATION_STOP,
                                            ST_COMPANION_AUTH_OUTCOME_CANCELLED) > 0);
    CHECK(strcmp(out, "authorization operation=stop outcome=cancelled\n") == 0);
    CHECK(st_companion_encode_closing(out, sizeof out) > 0);
    CHECK(strcmp(out, "closing reason=quit\n") == 0);
    CHECK(st_companion_encode_closing(out, 5) == 0);
    return true;
}

static bool framer_splits_and_poisons(void)
{
    st_companion_framer framer;
    st_companion_framer_init(&framer);
    const char *stream = "run state=active\nack seq=1 disposition=accepted\nrun st";
    const uint8_t *bytes = (const uint8_t *)stream;
    size_t remaining = strlen(stream);
    size_t consumed = 0;
    const char *record = NULL;
    size_t length = 0;

    CHECK(st_companion_framer_push(&framer, bytes, remaining, &consumed, &record, &length) ==
          ST_COMPANION_FRAME_RECORD);
    CHECK(length == 16 && memcmp(record, "run state=active", 16) == 0);
    bytes += consumed;
    remaining -= consumed;
    CHECK(st_companion_framer_push(&framer, bytes, remaining, &consumed, &record, &length) ==
          ST_COMPANION_FRAME_RECORD);
    CHECK(length == strlen("ack seq=1 disposition=accepted"));
    bytes += consumed;
    remaining -= consumed;
    CHECK(st_companion_framer_push(&framer, bytes, remaining, &consumed, &record, &length) ==
          ST_COMPANION_FRAME_NEED_MORE);
    CHECK(consumed == remaining);
    CHECK(st_companion_framer_push(&framer, (const uint8_t *)"ate=stopped\n", 12, &consumed,
                                   &record, &length) == ST_COMPANION_FRAME_RECORD);
    CHECK(length == strlen("run state=stopped"));

    uint8_t overlong[ST_COMPANION_MAX_RECORD + 1];
    memset(overlong, 'a', sizeof overlong);
    st_companion_framer_init(&framer);
    CHECK(st_companion_framer_push(&framer, overlong, sizeof overlong, &consumed, &record,
                                   &length) == ST_COMPANION_FRAME_POISONED);
    CHECK(st_companion_framer_push(&framer, (const uint8_t *)"\n", 1, &consumed, &record,
                                   &length) == ST_COMPANION_FRAME_POISONED);

    st_companion_framer_init(&framer);
    CHECK(st_companion_framer_push(&framer, (const uint8_t *)"run\0x\n", 6, &consumed, &record,
                                   &length) == ST_COMPANION_FRAME_POISONED);
    st_companion_framer_init(&framer);
    CHECK(st_companion_framer_push(&framer, (const uint8_t *)"run\r\n", 5, &consumed, &record,
                                   &length) == ST_COMPANION_FRAME_POISONED);
    st_companion_framer_init(&framer);
    CHECK(st_companion_framer_push(&framer, (const uint8_t *)"r\xc3\xa9\n", 4, &consumed, &record,
                                   &length) == ST_COMPANION_FRAME_POISONED);
    return true;
}

/* --- Authorization flow ---------------------------------------------------- */

typedef struct {
    uint32_t assess_calls;
    uint32_t execute_calls;
    uint32_t assess_result;
    uint32_t execute_result;
    uint32_t sessions;
    uint32_t writes;
    uint32_t completion;
    uint8_t generation;
    uint8_t executed_binding;
} fake_port_state;

static uint32_t fake_assess(void *context, uint32_t operation, st_companion_assessment *out)
{
    fake_port_state *state = context;
    ++state->assess_calls;
    if (state->assess_result != 0) return state->assess_result;
    memset(out, 0, sizeof *out);
    out->operation = operation;
    out->terminal_sessions = state->sessions;
    out->started_writes = state->writes;
    out->binding[0] = ++state->generation;
    return 0;
}

static uint32_t fake_execute(void *context, const st_companion_assessment *confirmed,
                             st_companion_execution *out)
{
    fake_port_state *state = context;
    ++state->execute_calls;
    state->executed_binding = confirmed->binding[0];
    if (state->execute_result != 0) return state->execute_result;
    memset(out, 0, sizeof *out);
    out->completion = state->completion;
    out->request[0] = 0x5a;
    return 0;
}

static st_companion_authorization_port fake_port(fake_port_state *state)
{
    return (st_companion_authorization_port){state, fake_assess, fake_execute};
}

static bool authorization_start_skips_confirmation_only(void)
{
    fake_port_state state = {.completion = ST_COMPANION_COMPLETION_RUNNING};
    st_companion_authorization_port port = fake_port(&state);
    st_companion_auth_flow flow;
    st_companion_auth_init(&flow);
    CHECK(st_companion_auth_begin(&flow, &port, ST_COMPANION_OPERATION_START, 0) ==
          ST_COMPANION_AUTH_AUTHORIZING);
    CHECK(state.assess_calls == 1 && state.execute_calls == 0);
    CHECK(st_companion_auth_confirmation_message(&flow, (char[64]){0}, 64) == 0);
    CHECK(st_companion_auth_execute(&flow, &port, second_ns) == ST_COMPANION_AUTH_FINISHED);
    CHECK(state.execute_calls == 1);
    CHECK(flow.outcome == ST_COMPANION_AUTH_OUTCOME_COMPLETED_RUNNING);
    CHECK(flow.execution.request[0] == 0x5a);
    /* A finished flow cannot execute again. */
    CHECK(st_companion_auth_execute(&flow, &port, second_ns) == ST_COMPANION_AUTH_FINISHED);
    CHECK(state.execute_calls == 1);
    return true;
}

static bool authorization_destructive_confirmation_precedes_execute(void)
{
    fake_port_state state = {.sessions = 3, .writes = 1,
                             .completion = ST_COMPANION_COMPLETION_STOPPED};
    st_companion_authorization_port port = fake_port(&state);
    st_companion_auth_flow flow;
    st_companion_auth_init(&flow);
    CHECK(st_companion_auth_begin(&flow, &port, ST_COMPANION_OPERATION_STOP, 0) ==
          ST_COMPANION_AUTH_CONFIRMING);
    /* Execution is impossible until the companion's confirmation is answered. */
    CHECK(st_companion_auth_execute(&flow, &port, 0) == ST_COMPANION_AUTH_CONFIRMING);
    CHECK(state.execute_calls == 0);
    /* A second request cannot replace a flow that is waiting for a person. */
    CHECK(st_companion_auth_begin(&flow, &port, ST_COMPANION_OPERATION_RESTART, 0) ==
          ST_COMPANION_AUTH_CONFIRMING);
    CHECK(flow.operation == ST_COMPANION_OPERATION_STOP && state.assess_calls == 1);
    CHECK(st_companion_auth_confirm(&flow, &port, true, second_ns) ==
          ST_COMPANION_AUTH_AUTHORIZING);
    CHECK(state.execute_calls == 0);
    CHECK(st_companion_auth_execute(&flow, &port, second_ns) == ST_COMPANION_AUTH_FINISHED);
    CHECK(state.execute_calls == 1 && state.executed_binding == 1);
    CHECK(flow.outcome == ST_COMPANION_AUTH_OUTCOME_COMPLETED_STOPPED);
    CHECK(st_companion_operation_is_destructive(ST_COMPANION_OPERATION_RESTART));
    CHECK(!st_companion_operation_is_destructive(ST_COMPANION_OPERATION_START));
    return true;
}

static bool authorization_declined_never_executes(void)
{
    fake_port_state state = {.sessions = 1};
    st_companion_authorization_port port = fake_port(&state);
    st_companion_auth_flow flow;
    st_companion_auth_init(&flow);
    CHECK(st_companion_auth_begin(&flow, &port, ST_COMPANION_OPERATION_RESTART, 0) ==
          ST_COMPANION_AUTH_CONFIRMING);
    CHECK(st_companion_auth_confirm(&flow, &port, false, 0) == ST_COMPANION_AUTH_FINISHED);
    CHECK(flow.outcome == ST_COMPANION_AUTH_OUTCOME_DECLINED);
    CHECK(st_companion_auth_execute(&flow, &port, 0) == ST_COMPANION_AUTH_FINISHED);
    CHECK(state.execute_calls == 0);
    st_companion_status_copy declined = st_companion_auth_outcome_copy(flow.outcome);
    st_companion_status_copy cancelled =
        st_companion_auth_outcome_copy(ST_COMPANION_AUTH_OUTCOME_CANCELLED);
    CHECK(strcmp(declined.title, cancelled.title) != 0);
    CHECK(contains(declined.detail, "not asked"));
    return true;
}

static bool authorization_stale_confirmation_reassesses(void)
{
    fake_port_state state = {.sessions = 2, .completion = ST_COMPANION_COMPLETION_STOPPED};
    st_companion_authorization_port port = fake_port(&state);
    st_companion_auth_flow flow;
    st_companion_auth_init(&flow);
    CHECK(st_companion_auth_begin(&flow, &port, ST_COMPANION_OPERATION_STOP, 0) ==
          ST_COMPANION_AUTH_CONFIRMING);
    state.sessions = 5;
    uint64_t late = (uint64_t)(ST_COMPANION_ASSESSMENT_SECONDS + 1) * second_ns;
    CHECK(st_companion_auth_confirm(&flow, &port, true, late) == ST_COMPANION_AUTH_CONFIRMING);
    CHECK(state.assess_calls == 2 && state.execute_calls == 0);
    CHECK(flow.assessment.terminal_sessions == 5 && flow.reassessments == 1);
    char message[ST_COMPANION_MAX_CONFIRMATION];
    CHECK(st_companion_auth_confirmation_message(&flow, message, sizeof message) > 0);
    CHECK(contains(message, "5 Terminal Sessions"));
    CHECK(st_companion_auth_confirm(&flow, &port, true, late + second_ns) ==
          ST_COMPANION_AUTH_AUTHORIZING);
    CHECK(st_companion_auth_execute(&flow, &port, late + second_ns) == ST_COMPANION_AUTH_FINISHED);
    CHECK(state.executed_binding == 2);

    /* Start re-assesses silently when its assessment expired before execute. */
    fake_port_state start = {.completion = ST_COMPANION_COMPLETION_RUNNING};
    port = fake_port(&start);
    st_companion_auth_init(&flow);
    CHECK(st_companion_auth_begin(&flow, &port, ST_COMPANION_OPERATION_START, 0) ==
          ST_COMPANION_AUTH_AUTHORIZING);
    CHECK(st_companion_auth_execute(&flow, &port, late) == ST_COMPANION_AUTH_FINISHED);
    CHECK(start.assess_calls == 2 && start.executed_binding == 2);

    /* Repeated expiry stops instead of looping. */
    fake_port_state slow = {.sessions = 1};
    port = fake_port(&slow);
    st_companion_auth_init(&flow);
    CHECK(st_companion_auth_begin(&flow, &port, ST_COMPANION_OPERATION_STOP, 0) ==
          ST_COMPANION_AUTH_CONFIRMING);
    st_companion_auth_phase phase = ST_COMPANION_AUTH_CONFIRMING;
    for (uint32_t i = 1; i <= ST_COMPANION_MAX_REASSESSMENTS + 1; ++i) {
        phase = st_companion_auth_confirm(&flow, &port, true, (uint64_t)i * late);
    }
    CHECK(phase == ST_COMPANION_AUTH_FINISHED);
    CHECK(flow.outcome == ST_COMPANION_AUTH_OUTCOME_STALE && slow.execute_calls == 0);
    return true;
}

static bool authorization_failures_map_to_distinct_outcomes(void)
{
    static const struct {
        uint32_t failure;
        st_companion_auth_outcome outcome;
    } mapping[] = {
        {2, ST_COMPANION_AUTH_OUTCOME_NATIVE_UNAVAILABLE},
        {14, ST_COMPANION_AUTH_OUTCOME_STALE},
        {16, ST_COMPANION_AUTH_OUTCOME_DENIED},
        {17, ST_COMPANION_AUTH_OUTCOME_CANCELLED},
        {18, ST_COMPANION_AUTH_OUTCOME_UNAVAILABLE},
        {25, ST_COMPANION_AUTH_OUTCOME_BACKGROUND_ITEMS_SUPPRESSED},
        {29, ST_COMPANION_AUTH_OUTCOME_TIMED_OUT},
        {31, ST_COMPANION_AUTH_OUTCOME_PENDING},
        {32, ST_COMPANION_AUTH_OUTCOME_INDETERMINATE},
        {33, ST_COMPANION_AUTH_OUTCOME_CLEANUP_UNRESOLVED},
        {20, ST_COMPANION_AUTH_OUTCOME_FAILED},
        {99, ST_COMPANION_AUTH_OUTCOME_FAILED},
    };
    for (size_t i = 0; i < sizeof mapping / sizeof mapping[0]; ++i) {
        CHECK(st_companion_auth_outcome_for_failure(mapping[i].failure) == mapping[i].outcome);
        fake_port_state state = {.execute_result = mapping[i].failure};
        st_companion_authorization_port port = fake_port(&state);
        st_companion_auth_flow flow;
        st_companion_auth_init(&flow);
        CHECK(st_companion_auth_begin(&flow, &port, ST_COMPANION_OPERATION_START, 0) ==
              ST_COMPANION_AUTH_AUTHORIZING);
        CHECK(st_companion_auth_execute(&flow, &port, 0) == ST_COMPANION_AUTH_FINISHED);
        CHECK(flow.outcome == mapping[i].outcome && flow.failure == mapping[i].failure);
    }
    for (int outcome = 0; outcome < ST_COMPANION_AUTH_OUTCOME_COUNT; ++outcome) {
        st_companion_status_copy copy =
            st_companion_auth_outcome_copy((st_companion_auth_outcome)outcome);
        const char *token = st_companion_auth_outcome_token((st_companion_auth_outcome)outcome);
        CHECK(copy.title != NULL && copy.detail != NULL && copy.symbol != NULL && token != NULL);
        for (int other = 0; other < outcome; ++other) {
            CHECK(strcmp(st_companion_auth_outcome_copy((st_companion_auth_outcome)other).title,
                         copy.title) != 0);
            CHECK(strcmp(st_companion_auth_outcome_token((st_companion_auth_outcome)other),
                         token) != 0);
        }
    }

    /* A failed assessment finishes without confirmation or execution. */
    fake_port_state state = {.assess_result = 6};
    st_companion_authorization_port port = fake_port(&state);
    st_companion_auth_flow flow;
    st_companion_auth_init(&flow);
    CHECK(st_companion_auth_begin(&flow, &port, ST_COMPANION_OPERATION_STOP, 0) ==
          ST_COMPANION_AUTH_FINISHED);
    CHECK(flow.outcome == ST_COMPANION_AUTH_OUTCOME_FAILED && flow.failure == 6);
    CHECK(state.execute_calls == 0);

    /* Admission without completion is pending, not success. */
    fake_port_state pending = {.completion = ST_COMPANION_COMPLETION_PENDING};
    port = fake_port(&pending);
    st_companion_auth_init(&flow);
    (void)st_companion_auth_begin(&flow, &port, ST_COMPANION_OPERATION_START, 0);
    (void)st_companion_auth_execute(&flow, &port, 0);
    CHECK(flow.outcome == ST_COMPANION_AUTH_OUTCOME_PENDING);

    fake_port_state wrong = {.completion = ST_COMPANION_COMPLETION_STOPPED};
    port = fake_port(&wrong);
    st_companion_auth_init(&flow);
    (void)st_companion_auth_begin(&flow, &port, ST_COMPANION_OPERATION_START, 0);
    (void)st_companion_auth_execute(&flow, &port, 0);
    CHECK(flow.outcome == ST_COMPANION_AUTH_OUTCOME_INDETERMINATE);
    return true;
}

/* NativeUnavailable from a linked adapter means macOS lacks a service. Only
 * the composition, not a failure code, says that no adapter is linked. */
static bool authorization_native_unavailable_is_not_missing_adapter(void)
{
    for (int step = 0; step < 2; ++step) {
        fake_port_state state = {.completion = ST_COMPANION_COMPLETION_RUNNING};
        if (step == 0) {
            state.assess_result = 2;
        } else {
            state.execute_result = 2;
        }
        st_companion_authorization_port port = fake_port(&state);
        st_companion_auth_flow flow;
        st_companion_auth_init(&flow);
        (void)st_companion_auth_begin(&flow, &port, ST_COMPANION_OPERATION_START, 0);
        (void)st_companion_auth_execute(&flow, &port, 0);
        CHECK(flow.phase == ST_COMPANION_AUTH_FINISHED);
        CHECK(state.assess_calls == 1);
        CHECK(flow.failure == 2);
        CHECK(flow.outcome != ST_COMPANION_AUTH_OUTCOME_ADAPTER_UNAVAILABLE);
        CHECK(strcmp(st_companion_auth_outcome_token(flow.outcome), "adapter-unavailable") != 0);
        st_companion_status_copy copy = st_companion_auth_outcome_copy(flow.outcome);
        CHECK(!mentions(copy.title, "adapter"));
        CHECK(!contains(copy.detail, "adapter is not available"));
        CHECK(contains(copy.title, "macOS"));
    }
    CHECK(st_companion_auth_outcome_for_failure(2) == ST_COMPANION_AUTH_OUTCOME_NATIVE_UNAVAILABLE);
    /* No failure code maps to a missing adapter. */
    for (uint32_t failure = 1; failure <= 64; ++failure) {
        CHECK(st_companion_auth_outcome_for_failure(failure) !=
              ST_COMPANION_AUTH_OUTCOME_ADAPTER_UNAVAILABLE);
    }

    /* A port with a missing function is no adapter. */
    fake_port_state state = {0};
    st_companion_authorization_port partial = {&state, fake_assess, NULL};
    st_companion_auth_flow flow;
    st_companion_auth_init(&flow);
    CHECK(st_companion_auth_begin(&flow, &partial, ST_COMPANION_OPERATION_START, 0) ==
          ST_COMPANION_AUTH_FINISHED);
    CHECK(flow.outcome == ST_COMPANION_AUTH_OUTCOME_ADAPTER_UNAVAILABLE);
    CHECK(state.assess_calls == 0);
    return true;
}

static bool authorization_unavailable_port_reports_adapter(void)
{
    const st_companion_authorization_port *port = st_companion_authorization_unavailable();
    CHECK(port != NULL && port->assess != NULL && port->execute != NULL);
    st_companion_auth_flow flow;
    st_companion_auth_init(&flow);
    CHECK(st_companion_auth_begin(&flow, port, ST_COMPANION_OPERATION_START, 0) ==
          ST_COMPANION_AUTH_FINISHED);
    CHECK(flow.outcome == ST_COMPANION_AUTH_OUTCOME_ADAPTER_UNAVAILABLE);
    CHECK(flow.failure == 0);
    CHECK(strcmp(st_companion_auth_outcome_token(flow.outcome), "adapter-unavailable") == 0);
    st_companion_execution execution;
    CHECK(port->execute(port->context, &flow.assessment, &execution) == 2);
    CHECK(st_companion_auth_begin(&flow, NULL, ST_COMPANION_OPERATION_START, 0) ==
          ST_COMPANION_AUTH_FINISHED);
    CHECK(flow.outcome == ST_COMPANION_AUTH_OUTCOME_ADAPTER_UNAVAILABLE);
    return true;
}

static bool authorization_confirmation_states_counts_and_separation(void)
{
    fake_port_state state = {.sessions = 1, .writes = 2};
    st_companion_authorization_port port = fake_port(&state);
    st_companion_auth_flow flow;
    st_companion_auth_init(&flow);
    CHECK(st_companion_auth_begin(&flow, &port, ST_COMPANION_OPERATION_STOP, 0) ==
          ST_COMPANION_AUTH_CONFIRMING);
    char message[ST_COMPANION_MAX_CONFIRMATION];
    CHECK(st_companion_auth_confirmation_message(&flow, message, sizeof message) > 0);
    CHECK(contains(message, "1 Terminal Session will end"));
    CHECK(contains(message, "2 started Git writes finish first"));
    CHECK(contains(message, "does not authorize"));
    CHECK(strcmp(st_companion_auth_confirmation_title(&flow), "Stop Server?") == 0);
    CHECK(strcmp(st_companion_auth_confirmation_button(&flow), "Stop Server") == 0);
    CHECK(st_companion_auth_confirmation_message(&flow, message, 8) == 0);

    fake_port_state none = {0};
    port = fake_port(&none);
    st_companion_auth_init(&flow);
    (void)st_companion_auth_begin(&flow, &port, ST_COMPANION_OPERATION_RESTART, 0);
    CHECK(st_companion_auth_confirmation_message(&flow, message, sizeof message) > 0);
    CHECK(contains(message, "No Terminal Sessions will end"));
    CHECK(!contains(message, "Git write"));
    CHECK(contains(message, "starts again"));
    return true;
}

typedef struct {
    const char *name;
    bool (*run)(void);
} test_case;

static const test_case cases[] = {
    {"capability-catalog-covers-codes-1-to-13", capability_catalog_covers_codes},
    {"system-permission-explanations-name-requesting-process",
     explanations_name_requesting_process},
    {"permission-scope-names-desktop-and-server", permission_scope_names_both_processes},
    {"settings-anchors-are-allowlisted", settings_anchors_are_allowlisted},
    {"clipboard-explanation-names-no-requester", clipboard_explanation_names_no_requester},
    {"results-have-distinct-tokens-and-copy", results_have_distinct_tokens_and_copy},
    {"result-parse-rejects-unknown-tokens", result_parse_rejects_unknown_tokens},
    {"run-requests-only-while-active", run_requests_only_while_active},
    {"action-effects-exclude-grants-and-resets", action_effects_exclude_grants_and_resets},
    {"quit-plan-never-claims-stop-or-cleanup", quit_plan_never_claims_stop_or_cleanup},
    {"failure-names-cover-native-failures", failure_names_cover_native_failures},
    {"identifier-parse-is-strict", identifier_parse_is_strict},
    {"url-receipt-requires-exact-form", url_receipt_requires_exact_form},
    {"url-receipt-binds-run-and-nonce", url_receipt_binds_run_and_nonce},
    {"apple-event-receipt-binds-run-and-nonce", apple_event_receipt_binds_run_and_nonce},
    {"receipts-reject-replay-and-expiry", receipts_reject_replay_and_expiry},
    {"accessibility-receipt-accepts-only-armed-marker",
     accessibility_receipt_accepts_only_armed_marker},
    {"clipboard-gate-refuses-unarmed-superseded-and-rich",
     clipboard_gate_refuses_unarmed_superseded_and_rich},
    {"clipboard-marker-comparison-consumes-expectation",
     clipboard_marker_comparison_consumes_expectation},
    {"clipboard-expectation-replaces-older-marker", clipboard_expectation_replaces_older_marker},
    {"ledger-bounds-expectations-and-receipts", ledger_bounds_expectations_and_receipts},
    {"capture-pattern-encodes-nonce", capture_pattern_encodes_nonce},
    {"protocol-parses-each-inbound-record", protocol_parses_each_inbound_record},
    {"protocol-rejects-noncanonical-records", protocol_rejects_noncanonical_records},
    {"protocol-passed-requires-observed-effect", protocol_passed_requires_observed_effect},
    {"protocol-encoders-emit-fixed-records", protocol_encoders_emit_fixed_records},
    {"framer-splits-and-poisons", framer_splits_and_poisons},
    {"authorization-start-skips-confirmation-only", authorization_start_skips_confirmation_only},
    {"authorization-destructive-confirmation-precedes-execute",
     authorization_destructive_confirmation_precedes_execute},
    {"authorization-declined-never-executes", authorization_declined_never_executes},
    {"authorization-stale-confirmation-reassesses", authorization_stale_confirmation_reassesses},
    {"authorization-failures-map-to-distinct-outcomes",
     authorization_failures_map_to_distinct_outcomes},
    {"authorization-native-unavailable-is-not-missing-adapter",
     authorization_native_unavailable_is_not_missing_adapter},
    {"authorization-unavailable-port-reports-adapter",
     authorization_unavailable_port_reports_adapter},
    {"authorization-confirmation-states-counts-and-separation",
     authorization_confirmation_states_counts_and_separation},
};

int main(void)
{
    size_t failed = 0;
    size_t count = sizeof cases / sizeof cases[0];
    for (size_t i = 0; i < count; ++i) {
        bool passed = cases[i].run();
        /* All strings are fixed literals. */
        printf("{\"schema\":1,\"scope\":\"companion-model\",\"case\":\"%s\",\"passed\":%s}\n",
               cases[i].name, passed ? "true" : "false");
        if (!passed) ++failed;
    }
    printf("{\"schema\":1,\"scope\":\"companion-model\",\"summary\":true,"
           "\"cases\":%zu,\"failed\":%zu}\n", count, failed);
    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
