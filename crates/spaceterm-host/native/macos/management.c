#include "management.h"

#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <string.h>

#if defined(__APPLE__)
#include <Security/Authorization.h>
#include <Security/AuthorizationDB.h>
#include <CoreFoundation/CoreFoundation.h>
#endif

static void wipe(void *memory, size_t length) {
    volatile uint8_t *bytes = memory;
    while (length-- != 0) *bytes++ = 0;
}

st_management_result st_management_format_right(
    uint32_t build, uint32_t operation, char *destination, size_t capacity) {
    static const char *const namespaces[] = {
        "io.github.sadiksaifi.spaceterm.server",
        "io.github.sadiksaifi.spaceterm-preflight.server",
        "io.github.sadiksaifi.spaceterm-development.server"
    };
    static const char *const suffixes[] = {
        "install", "stage", "activate", "start", "stop", "restart", "uninstall",
        "repair", "collect", "remote-access", "owner-enrollment"
    };
    if (destination == NULL || capacity == 0) return ST_MANAGEMENT_INVALID_RECORD;
    destination[0] = '\0';
    if (build < 1 || build > 3 || operation < 1 || operation > 11)
        return ST_MANAGEMENT_INVALID_RECORD;
    int written = snprintf(destination, capacity, "%s.manage.%s",
        namespaces[build - 1], suffixes[operation - 1]);
    if (written < 0 || (size_t)written >= capacity) {
        destination[0] = '\0';
        return ST_MANAGEMENT_INVALID_RECORD;
    }
    return ST_MANAGEMENT_OK;
}

st_management_result st_management_classify_authorization(int32_t status) {
    switch (status) {
        case 0: return ST_MANAGEMENT_OK;
        case -60005: return ST_MANAGEMENT_AUTHORIZATION_DENIED;
        case -60006: return ST_MANAGEMENT_AUTHORIZATION_CANCELLED;
        default: return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    }
}

st_management_result st_management_classify_background_items(uint32_t status) {
    /* SMAppServiceStatus, observed by a headless native adapter. Not registration. */
    switch (status) {
        case 1: return ST_MANAGEMENT_OK; /* Enabled */
        case 2: return ST_MANAGEMENT_BACKGROUND_SUPPRESSED; /* RequiresApproval */
        case 0: case 3: return ST_MANAGEMENT_SUPERVISOR_UNAVAILABLE;
        default: return ST_MANAGEMENT_NATIVE_UNAVAILABLE;
    }
}

#if defined(__APPLE__)
_Static_assert(sizeof(AuthorizationExternalForm) == ST_MANAGEMENT_EXTERNAL_BYTES,
    "AuthorizationExternalForm ABI");
_Static_assert(errAuthorizationDenied == -60005, "authorization denied ABI");
_Static_assert(errAuthorizationCanceled == -60006, "authorization cancelled ABI");

#if defined(ST_MANAGEMENT_TESTING)
static const st_management_test_calls *test_calls;
void st_management_test_set_calls(const st_management_test_calls *calls) { test_calls = calls; }
#define NATIVE_CALL(name, fallback) (test_calls != NULL ? test_calls->name : fallback)
#else
#define NATIVE_CALL(name, fallback) fallback
#endif

static int boolean_value(CFDictionaryRef dictionary, CFStringRef key, int expected) {
    CFTypeRef value = CFDictionaryGetValue(dictionary, key);
    return value != NULL && CFGetTypeID(value) == CFBooleanGetTypeID()
        && (CFBooleanGetValue((CFBooleanRef)value) != 0) == expected;
}

static int string_value(CFDictionaryRef dictionary, CFStringRef key, CFStringRef expected) {
    CFTypeRef value = CFDictionaryGetValue(dictionary, key);
    return value != NULL && CFGetTypeID(value) == CFStringGetTypeID() && CFEqual(value, expected);
}

static st_management_result inspect_right_policy(const char *right) {
    CFDictionaryRef definition = NULL;
    OSStatus status = NATIVE_CALL(right_get, AuthorizationRightGet)(right, &definition);
    if (status != errAuthorizationSuccess || definition == NULL) {
        if (definition != NULL) CFRelease(definition);
        return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    }
    if (CFGetTypeID(definition) != CFDictionaryGetTypeID()) {
        CFRelease(definition);
        return ST_MANAGEMENT_AUTHORIZATION_DENIED;
    }
    CFTypeRef timeout = CFDictionaryGetValue(definition, CFSTR("timeout"));
    double seconds = -1;
    int accepted = string_value(definition, CFSTR("class"), CFSTR("user"))
        && string_value(definition, CFSTR("group"), CFSTR("admin"))
        && boolean_value(definition, CFSTR("authenticate-user"), 1)
        && boolean_value(definition, CFSTR("shared"), 0)
        && boolean_value(definition, CFSTR("allow-root"), 0)
        && timeout != NULL && CFGetTypeID(timeout) == CFNumberGetTypeID()
        && CFNumberGetValue((CFNumberRef)timeout, kCFNumberDoubleType, &seconds)
        && seconds == 0;
    CFTypeRef tries = CFDictionaryGetValue(definition, CFSTR("tries"));
    int64_t attempts = 0;
    accepted = accepted && boolean_value(definition, CFSTR("session-owner"), 0)
        && tries != NULL && CFGetTypeID(tries) == CFNumberGetTypeID()
        && CFNumberGetValue((CFNumberRef)tries, kCFNumberSInt64Type, &attempts)
        && attempts == 3;
    /* Alternate rules and unknown policy keys can change the effective grant.
     * Metadata added by Authorization Services is permitted, not interpreted. */
    static const char *const keys[] = {
        "class", "group", "authenticate-user", "shared", "allow-root", "timeout",
        "tries", "session-owner", "comment", "created", "modified", "version",
        "identifier", "requirement"
    };
    CFIndex count = CFDictionaryGetCount(definition);
    if (count < 8 || count > 14) accepted = 0;
    if (accepted) {
        const void *actual_keys[14];
        CFDictionaryGetKeysAndValues(definition, actual_keys, NULL);
        for (CFIndex i = 0; i < count; i++) {
            int found = 0;
            if (CFGetTypeID(actual_keys[i]) != CFStringGetTypeID()) { accepted = 0; break; }
            for (size_t j = 0; j < sizeof(keys) / sizeof(keys[0]); j++) {
                CFStringRef key = CFStringCreateWithCString(NULL, keys[j], kCFStringEncodingASCII);
                if (key != NULL) { found |= CFEqual(actual_keys[i], key); CFRelease(key); }
            }
            if (!found) { accepted = 0; break; }
        }
    }
    CFRelease(definition);
    return accepted ? ST_MANAGEMENT_OK : ST_MANAGEMENT_AUTHORIZATION_DENIED;
}

static st_management_result copy_exact_right(
    AuthorizationRef authorization, const char *right, int interaction_allowed) {
    AuthorizationItem item = { .name = right, .valueLength = 0, .value = NULL, .flags = 0 };
    AuthorizationRights requested = { .count = 1, .items = &item };
    AuthorizationRights *granted = NULL;
    AuthorizationFlags flags = kAuthorizationFlagDefaults;
    if (interaction_allowed)
        flags = kAuthorizationFlagExtendRights | kAuthorizationFlagInteractionAllowed;
    OSStatus status = NATIVE_CALL(copy_rights, AuthorizationCopyRights)(
        authorization, &requested, kAuthorizationEmptyEnvironment, flags, &granted);
    st_management_result result = st_management_classify_authorization(status);
    if (result == ST_MANAGEMENT_OK && (granted == NULL || granted->count != 1
        || granted->items == NULL || granted->items[0].name == NULL
        || strcmp(granted->items[0].name, right) != 0
        || granted->items[0].valueLength != 0 || granted->items[0].value != NULL))
        result = ST_MANAGEMENT_AUTHORIZATION_DENIED;
    if (granted != NULL && NATIVE_CALL(free_items, AuthorizationFreeItemSet)(granted) != 0)
        result = ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    return result;
}

struct st_management_gui_authorization {
    AuthorizationRef authorization;
    AuthorizationExternalForm external;
    uint8_t request_id[16];
    uint64_t deadline_ns;
};

static uint64_t monotonic_ns(void) {
#if defined(ST_MANAGEMENT_TESTING)
    if (test_calls != NULL) return test_calls->monotonic_ns();
#endif
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) return 0;
    return (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
}

st_management_result st_management_gui_right_release(
    st_management_gui_authorization **owned) {
    if (owned == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    st_management_gui_authorization *lease = *owned;
    *owned = NULL;
    if (lease == NULL) return ST_MANAGEMENT_OK;
    OSStatus status = NATIVE_CALL(free_authorization, AuthorizationFree)(
        lease->authorization, kAuthorizationFlagDestroyRights);
    wipe(lease, sizeof(*lease));
    free(lease);
    return status == 0 ? ST_MANAGEMENT_OK : ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}

st_management_result st_management_request_gui_right(
    uint32_t build, uint32_t operation, const uint8_t request_id[16],
    st_management_gui_authorization **out_owned) {
    if (out_owned == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    *out_owned = NULL;
    if (request_id == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    uint8_t nonzero = 0;
    for (size_t i = 0; i < 16; i++) nonzero |= request_id[i];
    if (nonzero == 0) return ST_MANAGEMENT_INVALID_RECORD;
    uint64_t started = monotonic_ns();
    if (started == 0 || started > UINT64_MAX - UINT64_C(30000000000))
        return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    char right[ST_MANAGEMENT_RIGHT_BYTES];
    st_management_result result = st_management_format_right(build, operation, right, sizeof(right));
    if (result != ST_MANAGEMENT_OK) return result;
    result = inspect_right_policy(right);
    if (result != ST_MANAGEMENT_OK) return result;
    st_management_gui_authorization *lease = calloc(1, sizeof(*lease));
    if (lease == NULL) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    OSStatus status = NATIVE_CALL(create, AuthorizationCreate)(NULL,
        kAuthorizationEmptyEnvironment, ST_MANAGEMENT_LEAST_PRIVILEGED_FLAG,
        &lease->authorization);
    result = st_management_classify_authorization(status);
    if (result == ST_MANAGEMENT_OK && lease->authorization == NULL)
        result = ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    if (result == ST_MANAGEMENT_OK) result = copy_exact_right(lease->authorization, right, 1);
    if (result == ST_MANAGEMENT_OK) {
        status = NATIVE_CALL(externalize, AuthorizationMakeExternalForm)(
            lease->authorization, &lease->external);
        result = st_management_classify_authorization(status);
    }
    uint64_t now = monotonic_ns();
    if (result == ST_MANAGEMENT_OK && (now == 0 || now < started))
        result = ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    if (result == ST_MANAGEMENT_OK && now - started >= UINT64_C(30000000000))
        result = ST_MANAGEMENT_STALE_ASSESSMENT;
    if (result != ST_MANAGEMENT_OK) {
        if (lease->authorization != NULL) {
            if (st_management_gui_right_release(&lease) != 0)
                result = ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
        } else {
            wipe(lease, sizeof(*lease)); free(lease);
        }
        return result;
    }
    memcpy(lease->request_id, request_id, 16);
    lease->deadline_ns = started + UINT64_C(30000000000);
    *out_owned = lease;
    return ST_MANAGEMENT_OK;
}

st_management_result st_management_gui_right_export(
    const st_management_gui_authorization *lease, const uint8_t request_id[16],
    uint8_t external[ST_MANAGEMENT_EXTERNAL_BYTES]) {
    if (external == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    wipe(external, ST_MANAGEMENT_EXTERNAL_BYTES);
    if (lease == NULL || request_id == NULL || memcmp(lease->request_id, request_id, 16) != 0)
        return ST_MANAGEMENT_INVALID_RECORD;
    uint64_t now = monotonic_ns();
    if (now == 0) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    if (now >= lease->deadline_ns) return ST_MANAGEMENT_STALE_ASSESSMENT;
    memcpy(external, &lease->external, ST_MANAGEMENT_EXTERNAL_BYTES);
    return ST_MANAGEMENT_OK;
}

st_management_result st_management_verify_gui_right(uint32_t build, uint32_t operation,
    const uint8_t external[ST_MANAGEMENT_EXTERNAL_BYTES], st_management_right_observation *out) {
    if (out == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    memset(out, 0, sizeof(*out));
    if (external == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    char right[ST_MANAGEMENT_RIGHT_BYTES];
    st_management_result result = st_management_format_right(build, operation, right, sizeof(right));
    if (result != ST_MANAGEMENT_OK) return result;
    result = inspect_right_policy(right);
    if (result != ST_MANAGEMENT_OK) return result;
    AuthorizationExternalForm form;
    memcpy(&form, external, sizeof(form));
    AuthorizationRef authorization = NULL;
    OSStatus status = NATIVE_CALL(internalize, AuthorizationCreateFromExternalForm)(&form, &authorization);
    wipe(&form, sizeof(form));
    result = st_management_classify_authorization(status);
    if (result == ST_MANAGEMENT_OK && authorization == NULL)
        result = ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    if (result == ST_MANAGEMENT_OK) result = copy_exact_right(authorization, right, 0);
    /* A verified exact right is consumed, not cached for another request. The
     * GUI owner still releases its own reference on admission or cancellation. */
    AuthorizationFlags release_flags = result == ST_MANAGEMENT_OK
        ? kAuthorizationFlagDestroyRights : kAuthorizationFlagDefaults;
    if (authorization != NULL && NATIVE_CALL(free_authorization, AuthorizationFree)(
        authorization, release_flags) != 0)
        result = ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    if (result == ST_MANAGEMENT_OK) {
        out->schema = 1;
        out->right_granted = 1;
    }
    return result;
}
#else
st_management_result st_management_request_gui_right(uint32_t build, uint32_t operation,
    const uint8_t request_id[16], st_management_gui_authorization **out) {
    (void)build; (void)operation; (void)request_id;
    if (out == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    *out = NULL;
    return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}
st_management_result st_management_gui_right_export(
    const st_management_gui_authorization *lease, const uint8_t request_id[16],
    uint8_t external[ST_MANAGEMENT_EXTERNAL_BYTES]) {
    (void)lease; (void)request_id;
    if (external == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    wipe(external, ST_MANAGEMENT_EXTERNAL_BYTES);
    return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}
st_management_result st_management_gui_right_release(st_management_gui_authorization **owned) {
    if (owned == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    if (*owned != NULL) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    return ST_MANAGEMENT_OK;
}
st_management_result st_management_verify_gui_right(uint32_t build, uint32_t operation,
    const uint8_t external[ST_MANAGEMENT_EXTERNAL_BYTES], st_management_right_observation *out) {
    (void)build; (void)operation;
    if (out == NULL || external == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    memset(out, 0, sizeof(*out));
    return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}
#endif

st_management_result st_management_authorize_request(void) {
    return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}
