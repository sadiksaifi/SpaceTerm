#include "../../../crates/spaceterm-host/native/macos/management.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

#if defined(__APPLE__)
#include <Security/AuthorizationDB.h>
static OSStatus authorization_status;
static unsigned calls, frees, item_frees;
static AuthorizationFlags last_flags, create_flags;
static int credential_aged, destroyed;
static uint64_t clock_ns;
static st_management_gui_authorization *lease;
static const uint8_t request_id[16] = { 1 };
static uint64_t fake_monotonic(void) { return clock_ns; }
static st_management_result request_right(uint32_t build, uint32_t operation, uint8_t external[32]) {
    memset(external, 0, 32);
    st_management_result result = st_management_request_gui_right(build, operation, request_id, &lease);
    return result == 0 ? st_management_gui_right_export(lease, request_id, external) : result;
}
static char requested_right[ST_MANAGEMENT_RIGHT_BYTES], held_right[ST_MANAGEMENT_RIGHT_BYTES];
static int wrong_right, weak_policy, extra_rule, shared_policy, session_owner_policy, wrong_tries;
static AuthorizationItem granted_item;
static AuthorizationRights granted_rights;

static OSStatus fake_create(const AuthorizationRights *rights,
    const AuthorizationEnvironment *environment, AuthorizationFlags flags, AuthorizationRef *out) {
    assert(rights == NULL && environment == NULL && flags == ST_MANAGEMENT_LEAST_PRIVILEGED_FLAG);
    create_flags = flags;
    *out = (AuthorizationRef)(uintptr_t)1;
    return 0;
}
static OSStatus fake_internalize(const AuthorizationExternalForm *form, AuthorizationRef *out) {
    assert(form != NULL);
    if (destroyed) return errAuthorizationDenied;
    *out = (AuthorizationRef)(uintptr_t)1; return 0;
}
static OSStatus fake_copy(AuthorizationRef authorization, const AuthorizationRights *rights,
    const AuthorizationEnvironment *environment, AuthorizationFlags flags, AuthorizationRights **out) {
    assert(authorization != NULL && rights->count == 1 && environment == NULL);
    calls++; last_flags = flags;
    snprintf(requested_right, sizeof(requested_right), "%s", rights->items[0].name);
    *out = NULL;
    if (authorization_status != 0) return authorization_status;
    /* authd ages ordinary UID credentials under timeout=0. Exact right
     * credentials in a least-privileged token are evaluated separately. */
    if (credential_aged && !(create_flags & (1u << 5)))
        return errAuthorizationDenied;
    if (!(flags & kAuthorizationFlagExtendRights)
        && strcmp(held_right, requested_right) != 0)
        return errAuthorizationDenied;
    if (flags & kAuthorizationFlagExtendRights)
        snprintf(held_right, sizeof(held_right), "%s", requested_right);
    granted_item = (AuthorizationItem){ .name = wrong_right ? "other.manage.start" : requested_right };
    granted_rights = (AuthorizationRights){ .count = 1, .items = &granted_item };
    *out = &granted_rights;
    return 0;
}
static OSStatus fake_externalize(AuthorizationRef authorization, AuthorizationExternalForm *out) {
    assert(authorization != NULL); memset(out, 7, sizeof(*out)); return 0;
}
static OSStatus fake_free(AuthorizationRef authorization, AuthorizationFlags flags) {
    assert(authorization != NULL);
    assert(flags == kAuthorizationFlagDefaults || flags == kAuthorizationFlagDestroyRights);
    if (flags == kAuthorizationFlagDestroyRights) destroyed = 1;
    frees++; return 0;
}
static OSStatus fake_item_free(AuthorizationItemSet *set) { assert(set != NULL); item_frees++; return 0; }
static OSStatus fake_policy(const char *right, CFDictionaryRef *out) {
    assert(strstr(right, ".manage.") != NULL);
    CFMutableDictionaryRef definition = CFDictionaryCreateMutable(NULL, 0,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    assert(definition != NULL);
    int timeout = weak_policy ? 300 : 0;
    CFNumberRef number = CFNumberCreate(NULL, kCFNumberIntType, &timeout);
    CFDictionarySetValue(definition, CFSTR("class"), CFSTR("user"));
    CFDictionarySetValue(definition, CFSTR("group"), CFSTR("admin"));
    CFDictionarySetValue(definition, CFSTR("authenticate-user"), kCFBooleanTrue);
    CFDictionarySetValue(definition, CFSTR("allow-root"), kCFBooleanFalse);
    CFDictionarySetValue(definition, CFSTR("shared"), shared_policy ? kCFBooleanTrue : kCFBooleanFalse);
    CFDictionarySetValue(definition, CFSTR("timeout"), number);
    if (extra_rule) CFDictionarySetValue(definition, CFSTR("rule"), CFSTR("allow"));
    /* Complete normalized authd readback, not the input installation plist. */
    int tries = wrong_tries ? 10000 : 3;
    CFNumberRef tries_number = CFNumberCreate(NULL, kCFNumberIntType, &tries);
    CFDictionarySetValue(definition, CFSTR("tries"), tries_number);
    CFDictionarySetValue(definition, CFSTR("session-owner"),
        session_owner_policy ? kCFBooleanTrue : kCFBooleanFalse);
    CFDictionarySetValue(definition, CFSTR("comment"), CFSTR("SpaceTerm fixed right"));
    CFDictionarySetValue(definition, CFSTR("version"), tries_number);
    CFDictionarySetValue(definition, CFSTR("created"), number);
    CFDictionarySetValue(definition, CFSTR("modified"), number);
    CFRelease(tries_number);
    CFRelease(number); *out = definition; return 0;
}
static const st_management_test_calls fake_calls = {
    .create = fake_create, .internalize = fake_internalize, .copy_rights = fake_copy,
    .externalize = fake_externalize, .free_authorization = fake_free,
    .free_items = fake_item_free, .right_get = fake_policy, .monotonic_ns = fake_monotonic
};
static void reset(void) {
    assert(st_management_gui_right_release(&lease) == 0);
    destroyed = 0; clock_ns = 1000000000;
    authorization_status = 0; calls = 0; frees = 0; item_frees = 0; last_flags = 0;
    wrong_right = weak_policy = extra_rule = shared_policy = session_owner_policy = wrong_tries = 0;
    create_flags = 0; credential_aged = 0;
    memset(requested_right, 0, sizeof(requested_right));
    memset(held_right, 0, sizeof(held_right));
    st_management_test_set_calls(&fake_calls);
}
static void authorization_denied_cancelled_or_unavailable_has_no_effect(void) {
    const OSStatus statuses[] = { errAuthorizationDenied, errAuthorizationCanceled,
        errAuthorizationInteractionNotAllowed, errAuthorizationInternal };
    const st_management_result expected[] = { 16, 17, 18, 18 };
    for (size_t i = 0; i < 4; i++) {
        reset(); authorization_status = statuses[i];
        uint8_t external[32]; memset(external, 99, sizeof(external));
        assert(request_right(3, 5, external) == expected[i]);
        for (size_t j = 0; j < sizeof(external); j++) assert(external[j] == 0);
        assert(calls == 1 && frees == 1 && item_frees == 0);
    }
}
static void helper_reconstructs_right_and_never_prompts(void) {
    reset(); uint8_t external[32]; st_management_right_observation out;
    assert(request_right(2, 7, external) == 0);
    calls = 0; item_frees = 0;
    assert(st_management_verify_gui_right(2, 7, external, &out) == 0);
    assert(strcmp(requested_right, "io.github.sadiksaifi.spaceterm-preflight.server.manage.uninstall") == 0);
    assert((last_flags & kAuthorizationFlagInteractionAllowed) == 0);
    assert((last_flags & kAuthorizationFlagExtendRights) == 0);
    assert(out.schema == 1 && out.right_granted == 1 && out.interaction_allowed == 0);
    assert(frees == 1 && item_frees == 1);
    reset(); assert(request_right(2, 7, external) == 0); wrong_right = 1;
    assert(st_management_verify_gui_right(2, 7, external, &out) == 16);
    assert(out.schema == 0 && out.right_granted == 0);
}
static void weakened_and_alternate_authorization_policies_fail_closed(void) {
    uint8_t external[32];
    reset(); weak_policy = 1;
    assert(request_right(1, 1, external) == 16); assert(calls == 0);
    reset(); extra_rule = 1;
    assert(request_right(1, 1, external) == 16); assert(calls == 0);
    reset(); shared_policy = 1;
    assert(request_right(1, 1, external) == 16); assert(calls == 0);
}
static void normalized_right_policy_rejects_session_owner_bypass_and_wrong_tries(void) {
    uint8_t external[32];
    reset(); session_owner_policy = 1;
    assert(request_right(1, 1, external) == 16); assert(calls == 0);
    reset(); wrong_tries = 1;
    assert(request_right(1, 1, external) == 16); assert(calls == 0);
}
static void full_authd_readback_accepts_only_the_fixed_user_policy(void) {
    reset(); uint8_t external[32];
    assert(request_right(3, 4, external) == 0);
    assert(calls == 1);
}
static void exact_operation_right_survives_elapsed_gui_helper_handoff(void) {
    reset(); uint8_t external[32]; st_management_right_observation out;
    assert(request_right(3, 4, external) == 0);
    credential_aged = 1; clock_ns += UINT64_C(1000000000);
    assert(st_management_verify_gui_right(3, 4, external, &out) == 0);
    assert(out.right_granted == 1);
    assert((last_flags & kAuthorizationFlagExtendRights) == 0);
    assert(st_management_verify_gui_right(3, 5, external, &out) == 16);
    assert(out.right_granted == 0);
    assert(st_management_verify_gui_right(2, 4, external, &out) == 16);
    assert(out.right_granted == 0);
    assert((last_flags & (kAuthorizationFlagExtendRights | kAuthorizationFlagInteractionAllowed)) == 0);
}
static void gui_token_is_request_bound_expiring_and_destroyed_on_release(void) {
    reset(); uint8_t external[32], wrong_request[16] = { 2 };
    assert(request_right(3, 4, external) == 0);
    assert(st_management_gui_right_export(lease, wrong_request, external) == 9);
    for (size_t i = 0; i < 32; i++) assert(external[i] == 0);
    assert(st_management_gui_right_export(lease, request_id, external) == 0);
    st_management_right_observation out;
    assert(st_management_verify_gui_right(3, 4, external, &out) == 0);
    clock_ns += UINT64_C(30000000000);
    assert(st_management_gui_right_export(lease, request_id, external) == 14);
    for (size_t i = 0; i < 32; i++) assert(external[i] == 0);
    assert(st_management_gui_right_release(&lease) == 0 && lease == NULL && destroyed);
    assert(st_management_verify_gui_right(3, 4, external, &out) == 16);
    assert(out.right_granted == 0);
    assert(st_management_gui_right_release(&lease) == 0);
}
static void verified_gui_right_is_consumed_before_another_request(void) {
    reset(); uint8_t external[32]; st_management_right_observation out;
    assert(request_right(3, 4, external) == 0);
    assert(st_management_verify_gui_right(3, 4, external, &out) == 0);
    assert(st_management_verify_gui_right(3, 4, external, &out) == 16);
    assert(out.right_granted == 0);
}
static void gui_authorization_is_not_account_or_caller_authority(void) {
    reset(); uint8_t external[32];
    assert(request_right(3, 4, external) == 0);
    assert((last_flags & kAuthorizationFlagInteractionAllowed) != 0);
    assert(st_management_authorize_request() == 18);
}
#endif
static void fixed_rights_reject_unknown_operation_and_small_buffer(void) {
    char right[ST_MANAGEMENT_RIGHT_BYTES];
    assert(st_management_format_right(1, 10, right, sizeof(right)) == 0);
    assert(strcmp(right, "io.github.sadiksaifi.spaceterm.server.manage.remote-access") == 0);
    assert(st_management_format_right(0, 4, right, sizeof(right)) == 9 && right[0] == 0);
    assert(st_management_format_right(1, 0, right, sizeof(right)) == 9);
    assert(st_management_format_right(1, 12, right, sizeof(right)) == 9);
    assert(st_management_format_right(1, 4, right, 1) == 9 && right[0] == 0);
}
static void native_error_text_is_not_relayed(void) {
    assert(st_management_classify_authorization(-2147483647) == 18);
    assert(st_management_classify_authorization(-60005) == 16);
    assert(st_management_classify_authorization(-60006) == 17);
    assert(st_management_classify_authorization(-60007) == 18);
}
static void background_items_suppression_is_not_missing_installation(void) {
    assert(st_management_classify_background_items(1) == 0);
    assert(st_management_classify_background_items(2) == 25);
    assert(st_management_classify_background_items(0) == 24);
    assert(st_management_classify_background_items(3) == 24);
    assert(st_management_classify_background_items(99) == 2);
}
#define RUN(test) do { test(); tests++; puts("passed=" #test); } while (0)
int main(void) {
    unsigned tests = 0;
    RUN(fixed_rights_reject_unknown_operation_and_small_buffer);
    RUN(native_error_text_is_not_relayed);
    RUN(background_items_suppression_is_not_missing_installation);
#if defined(__APPLE__)
    RUN(authorization_denied_cancelled_or_unavailable_has_no_effect);
    RUN(helper_reconstructs_right_and_never_prompts);
    RUN(weakened_and_alternate_authorization_policies_fail_closed);
    RUN(full_authd_readback_accepts_only_the_fixed_user_policy);
    RUN(normalized_right_policy_rejects_session_owner_bypass_and_wrong_tries);
    RUN(exact_operation_right_survives_elapsed_gui_helper_handoff);
    RUN(gui_token_is_request_bound_expiring_and_destroyed_on_release);
    RUN(verified_gui_right_is_consumed_before_another_request);
    RUN(gui_authorization_is_not_account_or_caller_authority);
    assert(st_management_gui_right_release(&lease) == 0);
#else
    assert(st_management_authorize_request() == 18);
#endif
    printf("tests=%u proof=synthetic\n", tests); return 0;
}
