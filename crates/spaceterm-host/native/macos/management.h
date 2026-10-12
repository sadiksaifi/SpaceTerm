#ifndef SPACETERM_MANAGEMENT_H
#define SPACETERM_MANAGEMENT_H

#include <stddef.h>
#include <stdint.h>

/* Values are ND520/1 NativeFailure's ABI. None carry native error text. */
typedef uint32_t st_management_result;
#define ST_MANAGEMENT_OK UINT32_C(0)
#define ST_MANAGEMENT_NATIVE_UNAVAILABLE UINT32_C(2)
#define ST_MANAGEMENT_INVALID_RECORD UINT32_C(9)
#define ST_MANAGEMENT_AUTHORIZATION_DENIED UINT32_C(16)
#define ST_MANAGEMENT_AUTHORIZATION_CANCELLED UINT32_C(17)
#define ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE UINT32_C(18)
#define ST_MANAGEMENT_SUPERVISOR_UNAVAILABLE UINT32_C(24)
#define ST_MANAGEMENT_BACKGROUND_SUPPRESSED UINT32_C(25)
#define ST_MANAGEMENT_STALE_ASSESSMENT UINT32_C(14)
/* AuthorizationPriv.h defines this SPI as bit 5. The public macOS SDK omits
 * its name. Native acceptance is a required evidence gate, not assumed here. */
#define ST_MANAGEMENT_LEAST_PRIVILEGED_FLAG UINT32_C(32)
#define ST_MANAGEMENT_RIGHT_BYTES 160u
#define ST_MANAGEMENT_EXTERNAL_BYTES 32u

/* Tags are comparison data corresponding to the frozen NativeOperation enum. */
st_management_result st_management_format_right(
    uint32_t build, uint32_t operation, char *destination, size_t capacity);
st_management_result st_management_classify_authorization(int32_t native_status);
st_management_result st_management_classify_background_items(uint32_t native_status);

/* Own the single exact right through request admission or cancellation. A
 * least-privileged token carries no reusable authenticated-user credential.
 * Its external form is sensitive authority and must not enter diagnostics. */
typedef struct st_management_gui_authorization st_management_gui_authorization;
st_management_result st_management_request_gui_right(
    uint32_t build, uint32_t operation, const uint8_t request_id[16],
    st_management_gui_authorization **out_owned_authorization);
st_management_result st_management_gui_right_export(
    const st_management_gui_authorization *authorization,
    const uint8_t request_id[16], uint8_t out_external_form[ST_MANAGEMENT_EXTERNAL_BYTES]);
/* Destroy the token's rights, wipe its form, and null the owned pointer. */
st_management_result st_management_gui_right_release(
    st_management_gui_authorization **owned_authorization);

/* A granted right is only an observation, not caller/account/native authority.
 * The helper reconstructs its right without interaction or extension and
 * destroys the verified token rights before reporting success. The common native
 * caller/account adapter must independently bind the peer before any effect. */
typedef struct {
    uint32_t schema;
    uint32_t right_granted;
    uint32_t interaction_allowed;
    uint32_t reserved;
} st_management_right_observation;
st_management_result st_management_verify_gui_right(
    uint32_t build, uint32_t operation,
    const uint8_t external_form[ST_MANAGEMENT_EXTERNAL_BYTES],
    st_management_right_observation *out_observation);

/* No caller fabrication API. Until native binding is integrated this always
 * returns unavailable, even if a separate right observation was successful. */
st_management_result st_management_authorize_request(void);

#if defined(ST_MANAGEMENT_TESTING) && defined(__APPLE__)
#include <Security/Authorization.h>
#include <CoreFoundation/CoreFoundation.h>
/* Compiled only into ordinary tests. These callbacks never enter production. */
typedef struct {
    OSStatus (*create)(const AuthorizationRights *, const AuthorizationEnvironment *,
        AuthorizationFlags, AuthorizationRef *);
    OSStatus (*internalize)(const AuthorizationExternalForm *, AuthorizationRef *);
    OSStatus (*copy_rights)(AuthorizationRef, const AuthorizationRights *,
        const AuthorizationEnvironment *, AuthorizationFlags, AuthorizationRights **);
    OSStatus (*externalize)(AuthorizationRef, AuthorizationExternalForm *);
    OSStatus (*free_authorization)(AuthorizationRef, AuthorizationFlags);
    OSStatus (*free_items)(AuthorizationItemSet *);
    OSStatus (*right_get)(const char *, CFDictionaryRef *);
    uint64_t (*monotonic_ns)(void);
} st_management_test_calls;
void st_management_test_set_calls(const st_management_test_calls *calls);
#endif
#endif
