#include "../../native/macos/management.h"
#include <Security/AuthorizationDB.h>
#include <assert.h>
#include <string.h>

static unsigned mode, copies, retired;
static char right[ST_MANAGEMENT_RIGHT_BYTES];
static uint64_t now_ns = UINT64_C(1000000000);
static AuthorizationItem granted_item;
static AuthorizationRights granted;
static uint32_t root_uid(void) { return 0; }
static uint64_t clock_ns(void) { return now_ns; }
static OSStatus create(const AuthorizationRights *rights, const AuthorizationEnvironment *environment,
    AuthorizationFlags flags, AuthorizationRef *out) {
    assert(rights == NULL && environment == NULL && flags == ST_MANAGEMENT_LEAST_PRIVILEGED_FLAG);
    *out = (AuthorizationRef)(uintptr_t)1; return 0;
}
static OSStatus copy(AuthorizationRef ref, const AuthorizationRights *rights,
    const AuthorizationEnvironment *environment, AuthorizationFlags flags, AuthorizationRights **out) {
    assert(ref != NULL && rights->count == 1 && environment == NULL); copies++;
    *out = NULL;
    if (mode == 1) return errAuthorizationDenied;
    if (mode == 2) return errAuthorizationCanceled;
    if (mode == 3) return errAuthorizationInternal;
    if (flags & kAuthorizationFlagExtendRights) {
        assert((flags & kAuthorizationFlagInteractionAllowed) != 0);
        assert(strlen(rights->items[0].name) < sizeof(right));
        strcpy(right, rights->items[0].name);
        if (mode == 5) now_ns += UINT64_C(30000000000);
    } else assert(strcmp(right, rights->items[0].name) == 0);
    granted_item = (AuthorizationItem){ .name = mode == 4 ? "other.manage.start" : right };
    granted = (AuthorizationRights){ .count = 1, .items = &granted_item };
    *out = &granted; return 0;
}
static OSStatus release(AuthorizationRef ref, AuthorizationFlags flags) {
    assert(ref != NULL && flags == kAuthorizationFlagDestroyRights); retired++; return 0;
}
static OSStatus free_items(AuthorizationItemSet *items) { assert(items != NULL); return 0; }
static OSStatus policy(const char *name, CFDictionaryRef *out) {
    assert(strstr(name, ".manage.") != NULL);
    CFMutableDictionaryRef definition = CFDictionaryCreateMutable(NULL, 0,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    int zero = 0, three = 3;
    CFNumberRef timeout = CFNumberCreate(NULL, kCFNumberIntType, &zero);
    CFNumberRef tries = CFNumberCreate(NULL, kCFNumberIntType, &three);
    CFDictionarySetValue(definition, CFSTR("class"), CFSTR("user"));
    CFDictionarySetValue(definition, CFSTR("group"), CFSTR("admin"));
    CFDictionarySetValue(definition, CFSTR("authenticate-user"), kCFBooleanTrue);
    CFDictionarySetValue(definition, CFSTR("shared"), kCFBooleanFalse);
    CFDictionarySetValue(definition, CFSTR("allow-root"), mode == 6 ? kCFBooleanTrue : kCFBooleanFalse);
    CFDictionarySetValue(definition, CFSTR("session-owner"), kCFBooleanFalse);
    CFDictionarySetValue(definition, CFSTR("timeout"), timeout);
    CFDictionarySetValue(definition, CFSTR("tries"), tries);
    CFRelease(timeout); CFRelease(tries); *out = definition; return 0;
}
static const st_management_test_calls calls = {
    .create = create, .copy_rights = copy, .free_authorization = release,
    .free_items = free_items, .right_get = policy, .monotonic_ns = clock_ns,
};
static const st_management_demand_test_calls callers = { .real_uid = root_uid, .effective_uid = root_uid };
void st_management_composition_mode(unsigned value) {
    mode = value; copies = retired = 0; now_ns = UINT64_C(1000000000);
    st_management_test_set_calls(&calls); st_management_demand_test_set_calls(&callers);
}
unsigned st_management_composition_retired(void) { return retired; }
/* The local-right route cannot use saved external forms. */
st_management_result st_management_admission_recheck(const st_management_admission *scope,
    const uint8_t request[160], const uint8_t external[32]) {
    (void)scope; (void)request; (void)external; return ST_MANAGEMENT_AUTHORIZATION_DENIED;
}
st_management_result st_management_admission_consume(const st_management_admission *scope,
    const uint8_t request[160], const uint8_t external[32]) {
    (void)scope; (void)request; (void)external; return ST_MANAGEMENT_AUTHORIZATION_DENIED;
}
