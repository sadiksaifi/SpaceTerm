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
#define ST_MANAGEMENT_IDENTITY_MISMATCH UINT32_C(12)
#define ST_MANAGEMENT_TIMED_OUT UINT32_C(29)
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

/* A raw external form has no request or original expiry binding. This legacy
 * observation entry refuses authority even when authd still holds the right. */
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

/* The root Rust issuer owns this live scope. It is never serialized, decoded,
 * created from a request, or supplied by the GUI. Its protected original
 * assessment binds the actual caller, account incarnation, exact 160-byte
 * request, enrolled external form, and original boot-bound deadline. */
typedef struct st_management_admission st_management_admission;
st_management_result st_management_admission_recheck(
    const st_management_admission *scope, const uint8_t request[160],
    const uint8_t external[ST_MANAGEMENT_EXTERNAL_BYTES]);
st_management_result st_management_admission_consume(
    const st_management_admission *scope, const uint8_t request[160],
    const uint8_t external[ST_MANAGEMENT_EXTERNAL_BYTES]);
st_management_result st_management_verify_gui_right_bound(
    const st_management_admission *scope, uint32_t build, uint32_t operation,
    const uint8_t request[160], const uint8_t external[ST_MANAGEMENT_EXTERNAL_BYTES],
    st_management_right_observation *out_observation);

/* A one-request launchd endpoint receives a simple fixed Mach message plus the
 * kernel audit trailer. The opaque object retains the exact dynamic SecCode
 * guest and audit-token principal. No request UID or client bytes construct it. */
typedef struct st_management_demand_message st_management_demand_message;
st_management_result st_management_receive_gui_demand(uint32_t build,
    uint32_t remaining_ms, st_management_demand_message **out_message);
st_management_result st_management_demand_request(const st_management_demand_message *message,
    uint8_t out_request[160]);
st_management_result st_management_demand_external(const st_management_demand_message *message,
    uint8_t out_external[32]);
st_management_result st_management_demand_caller_uid(const st_management_demand_message *message,
    uint32_t *out_uid);
st_management_result st_management_demand_recheck(const st_management_demand_message *message);
void st_management_demand_release(st_management_demand_message **owned_message);

/* Unbound legacy entry has no native caller scope and cannot authorize effects. */
st_management_result st_management_authorize_request(void);

#if defined(ST_MANAGEMENT_TESTING) && defined(__APPLE__)
#include <Security/Authorization.h>
#include <CoreFoundation/CoreFoundation.h>
#include <Security/SecCode.h>
#include <mach/mach.h>
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
typedef struct {
    uint32_t (*real_uid)(void);
    uint32_t (*effective_uid)(void);
    kern_return_t (*check_in)(mach_port_t, const char *, mach_port_t *);
    mach_msg_return_t (*receive)(mach_msg_header_t *, mach_msg_option_t,
        mach_msg_size_t, mach_msg_size_t, mach_port_name_t, mach_msg_timeout_t, mach_port_name_t);
    OSStatus (*guest)(SecCodeRef, CFDictionaryRef, SecCSFlags, SecCodeRef *);
    OSStatus (*validate)(SecCodeRef, SecCSFlags, SecRequirementRef);
    kern_return_t (*deallocate)(ipc_space_t, mach_port_name_t);
} st_management_demand_test_calls;
void st_management_demand_test_set_calls(const st_management_demand_test_calls *calls);
#endif
#endif
