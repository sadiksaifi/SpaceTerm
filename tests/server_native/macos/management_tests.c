#include "../../../crates/spaceterm-host/native/macos/management.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

#if defined(__APPLE__)
#include <Security/AuthorizationDB.h>
#include <Security/SecStaticCode.h>
#include <Security/SecRequirement.h>
#include <pthread.h>
#include <time.h>
#include <unistd.h>

/* Real private Mach ports, with code identity and privileged UID observations
 * injected. No launchd service, authorization prompt, or root process is used. */
static pthread_mutex_t roundtrip_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t roundtrip_changed = PTHREAD_COND_INITIALIZER;
static _Thread_local int roundtrip_helper;
static mach_port_t roundtrip_server;
static int helper_returned, validation_started, validation_allowed;
enum { VALID_ACK, SUBSTITUTED_ACK_REQUEST, SUBSTITUTED_ACK_CALLER, REJECTED_HELPER_CODE };
static unsigned roundtrip_mode;
static st_management_result helper_result, client_result;
static uint8_t roundtrip_request[160], client_reply[ST_MANAGEMENT_REPLY_BYTES];
static size_t client_length;
static const uint8_t completed_reply[] = "progress=01000000000000000000000000000000,7,2\n";
static uint32_t roundtrip_uid(void) { return roundtrip_helper ? 0 : getuid(); }
static kern_return_t roundtrip_check_in(mach_port_t bootstrap, const char *name, mach_port_t *port) {
    (void)bootstrap;
    assert(strcmp(name, "io.github.sadiksaifi.spaceterm-development.server.manager.demand") == 0);
    *port = roundtrip_server; return KERN_SUCCESS;
}
static kern_return_t roundtrip_look_up(mach_port_t bootstrap, const char *name, mach_port_t *port) {
    (void)bootstrap; assert(strstr(name, ".manager.demand") != NULL);
    *port = roundtrip_server;
    return mach_port_insert_right(mach_task_self(), *port, *port, MACH_MSG_TYPE_MAKE_SEND);
}
static mach_msg_return_t roundtrip_message(mach_msg_header_t *header, mach_msg_option_t options,
    mach_msg_size_t send, mach_msg_size_t size, mach_port_name_t port,
    mach_msg_timeout_t timeout, mach_port_name_t notify) {
    if ((options & MACH_SEND_MSG) != 0 && header->msgh_id == 0x53544d41
        && roundtrip_mode == SUBSTITUTED_ACK_REQUEST) ((uint8_t *)(header + 1))[0] ^= 1;
    mach_msg_return_t result = mach_msg(header, options, send, size, port, timeout, notify);
    if (result == MACH_MSG_SUCCESS && (options & MACH_RCV_MSG) != 0
        && header->msgh_id == 0x53544d41 && roundtrip_mode == SUBSTITUTED_ACK_CALLER) {
        mach_msg_audit_trailer_t *trailer = (mach_msg_audit_trailer_t *)((uint8_t *)header + header->msgh_size);
        trailer->msgh_audit.val[7] ^= 1;
    }
    if (result == MACH_MSG_SUCCESS && (options & MACH_RCV_MSG) != 0
        && header->msgh_id == 0x53544d52) {
        mach_msg_audit_trailer_t *trailer = (mach_msg_audit_trailer_t *)((uint8_t *)header + header->msgh_size);
        /* Only the helper's privileged observation is injected. Mach carries
         * the actual sender incarnation and transferred send-once rights. */
        trailer->msgh_audit.val[1] = 0;
    }
    return result;
}
static OSStatus roundtrip_guest(SecCodeRef host, CFDictionaryRef attributes, SecCSFlags flags, SecCodeRef *code) {
    (void)host; (void)flags;
    const audit_token_t *token = (const audit_token_t *)CFDataGetBytePtr(
        CFDictionaryGetValue(attributes, kSecGuestAttributeAudit));
    assert(token->val[5] == (uint32_t)getpid() && token->val[7] != 0);
    pthread_mutex_lock(&roundtrip_lock);
    if (!roundtrip_helper) {
        assert(token->val[1] == 0);
        validation_started = 1; pthread_cond_broadcast(&roundtrip_changed);
        while (!validation_allowed) pthread_cond_wait(&roundtrip_changed, &roundtrip_lock);
    } else assert(token->val[1] == getuid());
    int alive = roundtrip_helper || !helper_returned;
    pthread_mutex_unlock(&roundtrip_lock);
    *code = alive ? (SecCodeRef)CFRetain(CFSTR("injected-live-manager-code")) : NULL;
    return alive ? 0 : errSecCSNoSuchCode;
}
static OSStatus roundtrip_validate(SecCodeRef code, SecCSFlags flags, SecRequirementRef requirement) {
    assert(code != NULL && flags == kSecCSStrictValidate);
    if (requirement != NULL) {
        CFStringRef text = NULL;
        assert(SecRequirementCopyString(requirement, 0, &text) == 0 && text != NULL);
        assert(CFStringFind(text, CFSTR("spaceterm-development.server.manager"), 0).location != kCFNotFound);
        CFRelease(text);
        if (roundtrip_mode == REJECTED_HELPER_CODE) return errSecCSReqFailed;
    }
    return 0;
}
static kern_return_t roundtrip_deallocate(ipc_space_t task, mach_port_name_t name) {
    /* Check-in's receive right is released by the test after both threads join. */
    return name == roundtrip_server ? KERN_SUCCESS : mach_port_deallocate(task, name);
}
static const st_management_demand_test_calls roundtrip_peers = {
    .real_uid = roundtrip_uid, .effective_uid = roundtrip_uid,
    .check_in = roundtrip_check_in, .look_up = roundtrip_look_up,
    .receive = roundtrip_message, .guest = roundtrip_guest, .validate = roundtrip_validate,
    .deallocate = roundtrip_deallocate,
};
static void *roundtrip_helper_thread(void *unused) {
    (void)unused; roundtrip_helper = 1;
    st_management_demand_message *message = NULL;
    assert(st_management_receive_gui_demand(3, 1000, &message) == 0);
    helper_result = st_management_demand_reply(message, completed_reply, sizeof(completed_reply) - 1, 1500);
    st_management_demand_release(&message);
    pthread_mutex_lock(&roundtrip_lock);
    helper_returned = 1; pthread_cond_broadcast(&roundtrip_changed);
    pthread_mutex_unlock(&roundtrip_lock);
    return NULL;
}
static void *roundtrip_client_thread(void *unused) {
    (void)unused;
    client_result = st_management_send_gui_demand(3, roundtrip_request, client_reply, &client_length, 1500);
    return NULL;
}
static void gui_reply_case(unsigned mode) {
    roundtrip_mode = mode;
    assert(getuid() != 0);
    st_management_test_set_calls(NULL);
    st_management_demand_test_set_calls(&roundtrip_peers);
    helper_returned = validation_started = validation_allowed = 0;
    assert(mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &roundtrip_server) == KERN_SUCCESS);
    memset(roundtrip_request, 0, sizeof(roundtrip_request));
    roundtrip_request[6] = 3; roundtrip_request[16] = 1;
    uint32_t uid = getuid();
    for (unsigned i = 0; i < 4; i++) roundtrip_request[12 + i] = (uint8_t)(uid >> (24 - 8 * i));
    pthread_t helper, client;
    assert(pthread_create(&helper, NULL, roundtrip_helper_thread, NULL) == 0);
    assert(pthread_create(&client, NULL, roundtrip_client_thread, NULL) == 0);
    pthread_mutex_lock(&roundtrip_lock);
    struct timespec until;
    assert(clock_gettime(CLOCK_REALTIME, &until) == 0); until.tv_sec += 2;
    while (!validation_started)
        assert(pthread_cond_timedwait(&roundtrip_changed, &roundtrip_lock, &until) == 0);
    /* The reply is delivered, but its sender has not yet been authenticated.
     * Give an unacknowledged sender time to retire while validation is held. */
    assert(clock_gettime(CLOCK_REALTIME, &until) == 0);
    until.tv_nsec += 100000000;
    if (until.tv_nsec >= 1000000000) { until.tv_sec++; until.tv_nsec -= 1000000000; }
    while (!helper_returned) {
        if (pthread_cond_timedwait(&roundtrip_changed, &roundtrip_lock, &until) != 0) break;
    }
    assert(!helper_returned);
    validation_allowed = 1; pthread_cond_broadcast(&roundtrip_changed);
    pthread_mutex_unlock(&roundtrip_lock);
    assert(pthread_join(client, NULL) == 0 && pthread_join(helper, NULL) == 0);
    assert(helper_returned);
    if (mode == REJECTED_HELPER_CODE) {
        assert(client_result == ST_MANAGEMENT_AUTHORIZATION_DENIED && client_length == 0);
        for (size_t i = 0; i < sizeof(client_reply); i++) assert(client_reply[i] == 0);
    } else {
        assert(client_result == 0 && client_length == sizeof(completed_reply) - 1);
        assert(memcmp(client_reply, completed_reply, client_length) == 0);
    }
    assert(helper_result == (mode == VALID_ACK ? 0 : ST_MANAGEMENT_IDENTITY_MISMATCH));
    assert(mach_port_mod_refs(mach_task_self(), roundtrip_server, MACH_PORT_RIGHT_RECEIVE, -1) == KERN_SUCCESS);
    st_management_demand_test_set_calls(NULL);
}
static void gui_reply_keeps_exact_helper_alive_until_delayed_client_authentication_and_ack(void) {
    gui_reply_case(VALID_ACK);
}
static void gui_reply_acknowledgement_rejects_another_request_or_caller_incarnation(void) {
    gui_reply_case(SUBSTITUTED_ACK_REQUEST);
    gui_reply_case(SUBSTITUTED_ACK_CALLER);
}
static void gui_reply_failed_helper_authentication_discards_output_and_retires_helper(void) {
    gui_reply_case(REJECTED_HELPER_CODE);
}

static uint32_t demand_uid = 501;
static int demand_trailer = 1, demand_alive = 1;
static unsigned demand_guests, demand_releases, session_joins;
static int session_failure;
static uint32_t fake_root_uid(void) { return 0; }
static kern_return_t fake_check_in(mach_port_t bootstrap, const char *name, mach_port_t *port) {
    (void)bootstrap;
    assert(strcmp(name, "io.github.sadiksaifi.spaceterm-development.server.manager.demand") == 0);
    *port = 100; return KERN_SUCCESS;
}
static mach_msg_return_t fake_receive(mach_msg_header_t *header, mach_msg_option_t options,
    mach_msg_size_t send, mach_msg_size_t size, mach_port_name_t port,
    mach_msg_timeout_t timeout, mach_port_name_t notify) {
    (void)send; (void)size; (void)notify;
    assert((options & MACH_RCV_MSG) != 0 && timeout <= 2000 && timeout > 0);
    typedef struct { mach_msg_header_t header; uint8_t request[160]; uint8_t external[32]; } wire;
    wire *message = (wire *)header;
    header->msgh_size = sizeof(*message); header->msgh_id = 0x53544d47;
    header->msgh_local_port = port; header->msgh_remote_port = 101;
    header->msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_PORT_SEND_ONCE, 0);
    message->request[6] = 3;
    message->request[12] = 0; message->request[13] = 0;
    message->request[14] = 1; message->request[15] = 245;
    mach_msg_audit_trailer_t *trailer = (mach_msg_audit_trailer_t *)((uint8_t *)message + sizeof(*message));
    trailer->msgh_trailer_type = MACH_MSG_TRAILER_FORMAT_0;
    trailer->msgh_trailer_size = demand_trailer ? sizeof(*trailer) : sizeof(mach_msg_trailer_t);
    trailer->msgh_audit.val[0] = demand_uid;
    trailer->msgh_audit.val[1] = demand_uid;
    trailer->msgh_audit.val[6] = 123;
    trailer->msgh_audit.val[5] = 700;
    trailer->msgh_audit.val[7] = 800;
    return MACH_MSG_SUCCESS;
}
static OSStatus fake_guest(SecCodeRef host, CFDictionaryRef attributes, SecCSFlags flags, SecCodeRef *code) {
    (void)host; (void)flags;
    const audit_token_t *token = (const audit_token_t *)CFDataGetBytePtr(CFDictionaryGetValue(attributes, kSecGuestAttributeAudit));
    assert(token->val[1] == demand_uid && token->val[5] == 700 && token->val[7] == 800);
    demand_guests++;
    *code = NULL;
    if (!demand_alive) return -1;
    *code = (SecCodeRef)CFRetain(CFSTR("synthetic-native-code")); return 0;
}
static OSStatus fake_validate(SecCodeRef code, SecCSFlags flags, SecRequirementRef requirement) {
    (void)flags; assert(code != NULL && requirement == NULL); return demand_alive ? 0 : -1;
}
static kern_return_t fake_deallocate(ipc_space_t task, mach_port_name_t name) {
    (void)task; assert(name == 100 || name == 101 || name == 102); demand_releases++; return KERN_SUCCESS;
}
static int fake_session_port(au_asid_t asid, mach_port_name_t *port) {
    assert(asid == 123); *port = session_failure == 1 ? MACH_PORT_NULL : 102;
    return session_failure == 1 ? -1 : 0;
}
static au_asid_t fake_session_join(mach_port_name_t port) {
    assert(port == 102); session_joins++; return session_failure == 2 ? -1 : 123;
}
static const st_management_demand_test_calls demand_peers = {
    .real_uid = fake_root_uid, .effective_uid = fake_root_uid, .check_in = fake_check_in,
    .receive = fake_receive, .guest = fake_guest, .validate = fake_validate,
    .deallocate = fake_deallocate, .session_port = fake_session_port, .session_join = fake_session_join,
};
static void local_authorization_joins_only_the_authenticated_callers_audit_session(void) {
    st_management_demand_test_set_calls(&demand_peers);
    demand_uid = 501; demand_alive = demand_trailer = 1;
    for (session_failure = 0; session_failure <= 2; session_failure++) {
        session_joins = 0;
        st_management_demand_message *message = NULL;
        assert(st_management_receive_gui_demand(3, 1000, &message) == 0);
        assert(st_management_demand_join_session(message) == (session_failure == 0 ? 0 : 18));
        assert(session_joins == (session_failure == 1 ? 0u : 1u));
        demand_alive = 0;
        assert(st_management_demand_join_session(message) == 18);
        st_management_demand_release(&message); demand_alive = 1;
    }
    st_management_demand_test_set_calls(NULL);
}
static void gui_demand_uses_kernel_audit_token_not_request_uid_and_rechecks_pid_version(void) {
    st_management_demand_test_set_calls(&demand_peers);
    demand_uid = 501; demand_alive = 1; demand_trailer = 1; demand_guests = demand_releases = 0;
    st_management_demand_message *message = NULL;
    assert(st_management_receive_gui_demand(3, 1000, &message) == 0);
    assert(message != NULL && demand_guests == 1);
    uint32_t uid = 0; assert(st_management_demand_caller_uid(message, &uid) == 0 && uid == 501);
    assert(st_management_demand_recheck(message) == 0 && demand_guests == 2);
    demand_alive = 0;
    assert(st_management_demand_recheck(message) == 18);
    st_management_demand_release(&message);
    assert(message == NULL && demand_releases == 2);
}
static void gui_demand_refuses_another_sender_and_missing_audit_trailer(void) {
    st_management_demand_test_set_calls(&demand_peers);
    demand_uid = 502; demand_alive = 1; demand_trailer = 1; demand_guests = demand_releases = 0;
    st_management_demand_message *message = NULL;
    assert(st_management_receive_gui_demand(3, 1000, &message) == 12);
    assert(message == NULL && demand_guests == 0 && demand_releases == 2);
    demand_uid = 501; demand_trailer = 0;
    assert(st_management_receive_gui_demand(3, 1000, &message) == 18);
    assert(message == NULL && demand_guests == 0);
    st_management_demand_test_set_calls(NULL);
}
static OSStatus authorization_status;
static unsigned calls, frees, item_frees;
static AuthorizationFlags last_flags, create_flags;
static int credential_aged, destroyed;
static uint64_t clock_ns;
static st_management_gui_authorization *lease;
static const uint8_t request_id[16] = { 1 };
struct st_management_admission {
    uint8_t request[160];
    uint8_t external[32];
    uint64_t original_deadline;
    int consumed;
};
static struct st_management_admission admission;
static uint8_t submitted_request[160];
st_management_result st_management_admission_recheck(const st_management_admission *scope,
    const uint8_t request[160], const uint8_t external[32]) {
    assert(scope == &admission);
    if (clock_ns >= scope->original_deadline) return ST_MANAGEMENT_STALE_ASSESSMENT;
    if (scope->consumed) return ST_MANAGEMENT_AUTHORIZATION_DENIED;
    if (memcmp(scope->request, request, 160) != 0 || memcmp(scope->external, external, 32) != 0)
        return ST_MANAGEMENT_AUTHORIZATION_DENIED;
    return ST_MANAGEMENT_OK;
}
st_management_result st_management_admission_consume(const st_management_admission *scope,
    const uint8_t request[160], const uint8_t external[32]) {
    st_management_result result = st_management_admission_recheck(scope, request, external);
    if (result == 0) admission.consumed = 1;
    return result;
}
static st_management_result verify_right(uint32_t build, uint32_t operation,
    const uint8_t external[32], st_management_right_observation *out) {
    return st_management_verify_gui_right_bound(&admission, build, operation,
        submitted_request, external, out);
}

static uint64_t fake_monotonic(void) { return clock_ns; }
static st_management_result request_right(uint32_t build, uint32_t operation, uint8_t external[32]) {
    memset(external, 0, 32);
    st_management_result result = st_management_request_gui_right(build, operation, request_id, &lease);
    if (result != 0) return result;
    result = st_management_gui_right_export(lease, request_id, external);
    if (result == 0) {
        /* Inject a trusted native original assessment. This is not a GUI assertion. */
        memset(&admission, 0, sizeof(admission));
        admission.request[5] = (uint8_t)operation; admission.request[6] = (uint8_t)build;
        memcpy(admission.request + 16, request_id, 16);
        memcpy(submitted_request, admission.request, 160);
        memcpy(admission.external, external, 32);
        admission.original_deadline = clock_ns + UINT64_C(30000000000);
    }
    return result;
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
static int missing_ack;
static mach_msg_return_t deadline_reply_message(mach_msg_header_t *header, mach_msg_option_t options,
    mach_msg_size_t send, mach_msg_size_t size, mach_port_name_t port,
    mach_msg_timeout_t timeout, mach_port_name_t notify) {
    if ((options & MACH_SEND_MSG) != 0) {
        assert(header->msgh_id == 0x53544d52 && timeout == 1000);
        clock_ns += UINT64_C(900000000); return MACH_MSG_SUCCESS;
    }
    if (port == 100) return fake_receive(header, options, send, size, port, timeout, notify);
    /* Sending used 900 ms of the original one-second reply budget. Neither a
     * missing nor a just-delivered acknowledgement gets a fresh second. */
    assert(timeout == 100);
    clock_ns += UINT64_C(100000000);
    if (missing_ack) return MACH_RCV_TIMED_OUT;
    memset(header, 0, size);
    header->msgh_size = sizeof(*header) + 16; header->msgh_id = 0x53544d41;
    header->msgh_local_port = port;
    mach_msg_audit_trailer_t *trailer = (mach_msg_audit_trailer_t *)((uint8_t *)header + header->msgh_size);
    trailer->msgh_trailer_type = MACH_MSG_TRAILER_FORMAT_0; trailer->msgh_trailer_size = sizeof(*trailer);
    trailer->msgh_audit.val[0] = trailer->msgh_audit.val[1] = 501;
    trailer->msgh_audit.val[5] = 700; trailer->msgh_audit.val[6] = 123; trailer->msgh_audit.val[7] = 800;
    return MACH_MSG_SUCCESS;
}
static void gui_reply_missing_or_late_ack_cannot_extend_original_deadline(void) {
    st_management_demand_test_calls peers = demand_peers;
    peers.receive = deadline_reply_message;
    for (missing_ack = 0; missing_ack <= 1; missing_ack++) {
        reset(); st_management_demand_test_set_calls(&peers);
        demand_uid = 501; demand_alive = demand_trailer = 1;
        st_management_demand_message *message = NULL;
        assert(st_management_receive_gui_demand(3, 1000, &message) == 0);
        assert(st_management_demand_reply(message, completed_reply, sizeof(completed_reply) - 1, 1000)
            == ST_MANAGEMENT_TIMED_OUT);
        st_management_demand_release(&message);
    }
    st_management_demand_test_set_calls(NULL);
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
    assert(verify_right(2, 7, external, &out) == 0);
    assert(strcmp(requested_right, "io.github.sadiksaifi.spaceterm-preflight.server.manage.uninstall") == 0);
    assert((last_flags & kAuthorizationFlagInteractionAllowed) == 0);
    assert((last_flags & kAuthorizationFlagExtendRights) == 0);
    assert(out.schema == 1 && out.right_granted == 1 && out.interaction_allowed == 0);
    assert(frees == 1 && item_frees == 1);
    reset(); assert(request_right(2, 7, external) == 0); wrong_right = 1;
    assert(verify_right(2, 7, external, &out) == 16);
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
    assert(verify_right(3, 4, external, &out) == 0);
    assert(out.right_granted == 1);
    assert((last_flags & kAuthorizationFlagExtendRights) == 0);
    assert(verify_right(3, 5, external, &out) == 16);
    assert(out.right_granted == 0);
    assert(verify_right(2, 4, external, &out) == 16);
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
    assert(verify_right(3, 4, external, &out) == 0);
    clock_ns += UINT64_C(30000000000);
    assert(st_management_gui_right_export(lease, request_id, external) == 14);
    for (size_t i = 0; i < 32; i++) assert(external[i] == 0);
    assert(st_management_gui_right_release(&lease) == 0 && lease == NULL && destroyed);
    assert(verify_right(3, 4, external, &out) == 14);
    assert(out.right_granted == 0);
    assert(st_management_gui_right_release(&lease) == 0);
}
static void raw_form_cannot_authorize_without_native_original_scope(void) {
    reset(); uint8_t external[32]; st_management_right_observation out;
    assert(request_right(3, 4, external) == 0);
    assert(st_management_verify_gui_right(3, 4, external, &out) == 18);
    assert(out.right_granted == 0);
}
static void saved_form_cannot_substitute_request_or_restart_the_assessment_clock(void) {
    reset(); uint8_t external[32]; st_management_right_observation out;
    assert(request_right(3, 4, external) == 0);
    submitted_request[16] ^= 3;
    assert(verify_right(3, 4, external, &out) == 16);
    assert(out.right_granted == 0 && admission.consumed == 0);
    submitted_request[16] ^= 3;
    /* The assessment existed before GUI authorization began. */
    admission.original_deadline = clock_ns;
    assert(verify_right(3, 4, external, &out) == 14);
    assert(out.right_granted == 0 && admission.consumed == 0);
}
static void saved_unconsumed_form_is_rejected_after_original_expiry(void) {
    reset(); uint8_t external[32]; st_management_right_observation out;
    assert(request_right(3, 4, external) == 0);
    /* Keep the original form and its owner alive. Do not export again or consume it. */
    clock_ns += UINT64_C(30000000000);
    assert(verify_right(3, 4, external, &out) != 0);
    assert(out.right_granted == 0);
}
static void verified_gui_right_is_consumed_before_another_request(void) {
    reset(); uint8_t external[32]; st_management_right_observation out;
    assert(request_right(3, 4, external) == 0);
    assert(verify_right(3, 4, external, &out) == 0);
    assert(verify_right(3, 4, external, &out) == 16);
    assert(out.right_granted == 0);
}
static void local_right_binds_original_request_and_expiry_without_an_external_form(void) {
    reset(); st_management_demand_test_set_calls(&demand_peers);
    uint8_t request[160] = { 0 };
    memcpy(request, "STMG", 4); request[4] = 1; request[5] = 4; request[6] = 3;
    request[11] = 160; request[14] = 1; request[15] = 245; request[16] = 1;
    st_management_local_authorization *local = NULL;
    assert(st_management_request_local_right(request, clock_ns, 501, &local) == 0);
    assert(local != NULL && calls == 1);
    assert(st_management_local_right_recheck(local, request) == 0);
    request[88] ^= 1;
    assert(st_management_local_right_recheck(local, request) == ST_MANAGEMENT_IDENTITY_MISMATCH);
    assert(st_management_local_right_release(&local) == 0 && local == NULL && destroyed);
    reset();
    request[88] ^= 1;
    assert(st_management_request_local_right(request, clock_ns, 501, &local) == 0);
    clock_ns += UINT64_C(30000000000);
    assert(st_management_local_right_recheck(local, request) == ST_MANAGEMENT_STALE_ASSESSMENT);
    assert(destroyed);
    assert(st_management_local_right_release(&local) == 0 && local == NULL);
    st_management_demand_test_set_calls(NULL);
}
static void local_right_denials_and_wrong_caller_leave_no_live_authority(void) {
    reset(); st_management_demand_test_set_calls(&demand_peers);
    uint8_t request[160] = { 0 };
    memcpy(request, "STMG", 4); request[4] = 1; request[5] = 5; request[6] = 3;
    request[11] = 160; request[14] = 1; request[15] = 245; request[16] = 1;
    st_management_local_authorization *local = NULL;
    assert(st_management_request_local_right(request, clock_ns, 502, &local) == 12);
    assert(local == NULL && calls == 0);
    assert(st_management_request_local_right(request, clock_ns - 1, 0, &local) == 12);
    assert(local == NULL && calls == 0);
    const OSStatus statuses[] = { errAuthorizationDenied, errAuthorizationCanceled, errAuthorizationInternal };
    const uint32_t expected[] = { 16, 17, 18 };
    for (size_t i = 0; i < 3; i++) {
        reset(); authorization_status = statuses[i];
        assert(st_management_request_local_right(request, clock_ns, 501, &local) == expected[i]);
        assert(local == NULL && destroyed && calls == 1);
    }
    reset(); clock_ns += UINT64_C(30000000000);
    assert(st_management_request_local_right(request, UINT64_C(1000000000), 501, &local) == 14);
    assert(local == NULL && calls == 0);
    st_management_demand_test_set_calls(NULL);
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
static uint32_t background_status;
static unsigned background_calls;
static st_management_result observe_background(const char *plist, uint32_t *out_status) {
    assert(strcmp(plist, "io.github.sadiksaifi.spaceterm-development.server.501.plist") == 0);
    background_calls++;
    *out_status = background_status;
    return 0;
}
static void background_items_reads_only_the_fixed_owner_and_rejects_unknown_native_status(void) {
    uint32_t status = 99;
    st_management_test_set_background_observer(observe_background);
    for (background_status = 0; background_status <= 3; background_status++) {
        assert(st_management_observe_background_items(3, 501, &status) == 0);
        assert(status == background_status);
    }
    unsigned before = background_calls;
    assert(st_management_observe_background_items(3, 0, &status) == 9 && status == UINT32_MAX);
    assert(st_management_observe_background_items(0, 501, &status) == 9);
    assert(st_management_observe_background_items(3, 501, NULL) == 9);
    assert(background_calls == before);
    background_status = 999;
    assert(st_management_observe_background_items(3, 501, &status) == 2 && status == UINT32_MAX);
    st_management_test_set_background_observer(NULL);
}
#define RUN(test) do { test(); tests++; puts("passed=" #test); } while (0)
int main(void) {
    unsigned tests = 0;
    RUN(fixed_rights_reject_unknown_operation_and_small_buffer);
    RUN(native_error_text_is_not_relayed);
    RUN(background_items_suppression_is_not_missing_installation);
    RUN(background_items_reads_only_the_fixed_owner_and_rejects_unknown_native_status);
#if defined(__APPLE__)
    RUN(gui_reply_keeps_exact_helper_alive_until_delayed_client_authentication_and_ack);
    RUN(gui_reply_acknowledgement_rejects_another_request_or_caller_incarnation);
    RUN(gui_reply_failed_helper_authentication_discards_output_and_retires_helper);
    RUN(gui_reply_missing_or_late_ack_cannot_extend_original_deadline);
    RUN(local_authorization_joins_only_the_authenticated_callers_audit_session);
    RUN(gui_demand_uses_kernel_audit_token_not_request_uid_and_rechecks_pid_version);
    RUN(gui_demand_refuses_another_sender_and_missing_audit_trailer);
    RUN(authorization_denied_cancelled_or_unavailable_has_no_effect);
    RUN(helper_reconstructs_right_and_never_prompts);
    RUN(weakened_and_alternate_authorization_policies_fail_closed);
    RUN(full_authd_readback_accepts_only_the_fixed_user_policy);
    RUN(normalized_right_policy_rejects_session_owner_bypass_and_wrong_tries);
    RUN(exact_operation_right_survives_elapsed_gui_helper_handoff);
    RUN(gui_token_is_request_bound_expiring_and_destroyed_on_release);
    RUN(raw_form_cannot_authorize_without_native_original_scope);
    RUN(saved_form_cannot_substitute_request_or_restart_the_assessment_clock);
    RUN(saved_unconsumed_form_is_rejected_after_original_expiry);
    RUN(verified_gui_right_is_consumed_before_another_request);
    RUN(gui_authorization_is_not_account_or_caller_authority);
    RUN(local_right_binds_original_request_and_expiry_without_an_external_form);
    RUN(local_right_denials_and_wrong_caller_leave_no_live_authority);
    assert(st_management_gui_right_release(&lease) == 0);
#else
    assert(st_management_authorize_request() == 18);
#endif
    printf("tests=%u proof=synthetic\n", tests); return 0;
}
