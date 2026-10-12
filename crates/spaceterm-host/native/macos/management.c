#include "management.h"

#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <string.h>
#include <unistd.h>

#if defined(__APPLE__)
#include <Security/Authorization.h>
#include <Security/AuthorizationDB.h>
#include <CoreFoundation/CoreFoundation.h>
#include <Security/SecCode.h>
#include <Security/SecBase.h>
#include <Security/SecStaticCode.h>
#include <Security/SecRequirement.h>
#include <mach/mach.h>
#include <bsm/audit.h>
#include <servers/bootstrap.h>
#include <dlfcn.h>
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

#if defined(ST_MANAGEMENT_TESTING)
static const st_management_demand_test_calls *demand_calls;
void st_management_demand_test_set_calls(const st_management_demand_test_calls *calls) {
    demand_calls = calls;
}
#define DEMAND_CALL(name, fallback) (demand_calls != NULL ? demand_calls->name : fallback)
#else
#define DEMAND_CALL(name, fallback) fallback
#endif

struct st_management_demand_message {
    uint8_t request[160];
    uint8_t external[32];
    audit_token_t audit;
    SecCodeRef code;
    mach_port_t receiver;
    mach_port_t reply;
};
typedef struct {
    mach_msg_header_t header;
    uint8_t request[160];
    uint8_t external[32];
} st_management_demand_wire;

static st_management_result demand_guest(const audit_token_t *audit, SecCodeRef *out_code) {
    *out_code = NULL;
    CFDataRef token = CFDataCreate(NULL, (const UInt8 *)audit, sizeof(*audit));
    if (token == NULL) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    const void *keys[] = { kSecGuestAttributeAudit };
    const void *values[] = { token };
    CFDictionaryRef attributes = CFDictionaryCreate(NULL, keys, values, 1,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFRelease(token);
    if (attributes == NULL) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    OSStatus status = DEMAND_CALL(guest, SecCodeCopyGuestWithAttributes)(NULL, attributes,
        kSecCSDefaultFlags, out_code);
    CFRelease(attributes);
    if (status != errSecSuccess || *out_code == NULL) {
        if (*out_code != NULL) CFRelease(*out_code);
        *out_code = NULL;
        return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    }
    status = DEMAND_CALL(validate, SecCodeCheckValidity)(*out_code, kSecCSStrictValidate, NULL);
    if (status != errSecSuccess) {
        CFRelease(*out_code); *out_code = NULL;
        return ST_MANAGEMENT_AUTHORIZATION_DENIED;
    }
    return ST_MANAGEMENT_OK;
}

void st_management_demand_release(st_management_demand_message **owned) {
    if (owned == NULL || *owned == NULL) return;
    st_management_demand_message *message = *owned;
    *owned = NULL;
    if (message->code != NULL) CFRelease(message->code);
    if (message->reply != MACH_PORT_NULL)
        (void)DEMAND_CALL(deallocate, mach_port_deallocate)(mach_task_self(), message->reply);
    if (message->receiver != MACH_PORT_NULL)
        (void)DEMAND_CALL(deallocate, mach_port_deallocate)(mach_task_self(), message->receiver);
    wipe(message, sizeof(*message)); free(message);
}

st_management_result st_management_receive_gui_demand(uint32_t build,
    uint32_t remaining_ms, st_management_demand_message **out) {
    if (out == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    *out = NULL;
    if (build < 1 || build > 3 || remaining_ms == 0 || remaining_ms > 2000)
        return ST_MANAGEMENT_INVALID_RECORD;
    if (DEMAND_CALL(real_uid, getuid)() != 0 || DEMAND_CALL(effective_uid, geteuid)() != 0)
        return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    char endpoint[ST_MANAGEMENT_RIGHT_BYTES];
    st_management_result result = st_management_format_right(build, 4, endpoint, sizeof(endpoint));
    if (result != 0) return result;
    char *suffix = strstr(endpoint, ".manage.start");
    if (suffix == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    memcpy(suffix, ".manager.demand", sizeof(".manager.demand"));
    st_management_demand_message *message = calloc(1, sizeof(*message));
    if (message == NULL) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    if (DEMAND_CALL(check_in, bootstrap_check_in)(bootstrap_port, endpoint, &message->receiver) != KERN_SUCCESS
        || message->receiver == MACH_PORT_NULL) {
        st_management_demand_release(&message); return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    }
    struct { st_management_demand_wire wire; mach_msg_max_trailer_t trailer; } incoming;
    memset(&incoming, 0, sizeof(incoming));
    mach_msg_option_t options = MACH_RCV_MSG | MACH_RCV_TIMEOUT
        | MACH_RCV_TRAILER_TYPE(MACH_MSG_TRAILER_FORMAT_0)
        | MACH_RCV_TRAILER_ELEMENTS(MACH_RCV_TRAILER_AUDIT);
    mach_msg_return_t received = DEMAND_CALL(receive, mach_msg)(&incoming.wire.header, options, 0,
        sizeof(incoming), message->receiver, remaining_ms, MACH_PORT_NULL);
    if (received != MACH_MSG_SUCCESS) {
        st_management_demand_release(&message);
        return received == MACH_RCV_TIMED_OUT ? ST_MANAGEMENT_TIMED_OUT : ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    }
    mach_msg_header_t *header = &incoming.wire.header;
    if (header->msgh_size != sizeof(st_management_demand_wire)
        || header->msgh_id != 0x53544d47
        || (header->msgh_bits & MACH_MSGH_BITS_COMPLEX) != 0
        || header->msgh_local_port != message->receiver
        || MACH_MSGH_BITS_REMOTE(header->msgh_bits) != MACH_MSG_TYPE_PORT_SEND_ONCE
        || header->msgh_remote_port == MACH_PORT_NULL) {
        /* Preserve our receive right while destroying any transferred message resources. */
        header->msgh_local_port = MACH_PORT_NULL;
        mach_msg_destroy(header);
        wipe(&incoming, sizeof(incoming));
        st_management_demand_release(&message); return ST_MANAGEMENT_INVALID_RECORD;
    }
    message->reply = header->msgh_remote_port;
    const mach_msg_audit_trailer_t *trailer = (const mach_msg_audit_trailer_t *)&incoming.trailer;
    if (trailer->msgh_trailer_type != MACH_MSG_TRAILER_FORMAT_0
        || trailer->msgh_trailer_size < sizeof(*trailer)
        || trailer->msgh_trailer_size > sizeof(incoming.trailer)) {
        wipe(&incoming, sizeof(incoming));
        st_management_demand_release(&message); return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    }
    message->audit = trailer->msgh_audit;
    /* Darwin's kernel audit ABI: effective UID at word 1, PID at 5, PID version at 7. */
    uint32_t uid = message->audit.val[1];
    uint32_t request_uid = ((uint32_t)incoming.wire.request[12] << 24)
        | ((uint32_t)incoming.wire.request[13] << 16)
        | ((uint32_t)incoming.wire.request[14] << 8) | incoming.wire.request[15];
    if (uid == 0 || uid == UINT32_MAX || message->audit.val[5] == 0 || message->audit.val[7] == 0
        || uid != request_uid || incoming.wire.request[6] != build) {
        wipe(&incoming, sizeof(incoming));
        st_management_demand_release(&message); return ST_MANAGEMENT_IDENTITY_MISMATCH;
    }
    memcpy(message->request, incoming.wire.request, sizeof(message->request));
    memcpy(message->external, incoming.wire.external, sizeof(message->external));
    wipe(&incoming, sizeof(incoming));
    result = demand_guest(&message->audit, &message->code);
    if (result != 0) { st_management_demand_release(&message); return result; }
    *out = message; return ST_MANAGEMENT_OK;
}
st_management_result st_management_demand_request(const st_management_demand_message *message,
    uint8_t out[160]) {
    if (out == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    memset(out, 0, 160);
    if (message == NULL || message->code == NULL) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    memcpy(out, message->request, 160); return ST_MANAGEMENT_OK;
}
st_management_result st_management_demand_external(const st_management_demand_message *message, uint8_t out[32]) {
    if (out == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    wipe(out, 32);
    if (message == NULL || message->code == NULL) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    memcpy(out, message->external, 32); return ST_MANAGEMENT_OK;
}
st_management_result st_management_demand_caller_uid(const st_management_demand_message *message,
    uint32_t *out_uid) {
    if (out_uid == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    *out_uid = 0;
    if (message == NULL || message->code == NULL) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    *out_uid = message->audit.val[1]; return ST_MANAGEMENT_OK;
}
st_management_result st_management_demand_recheck(const st_management_demand_message *message) {
    if (message == NULL || message->code == NULL) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    SecCodeRef current = NULL;
    st_management_result result = demand_guest(&message->audit, &current);
    if (current != NULL) CFRelease(current);
    return result;
}

st_management_result st_management_demand_join_session(const st_management_demand_message *message) {
    st_management_result result = st_management_demand_recheck(message);
    if (result != 0) return result;
    if (DEMAND_CALL(real_uid, getuid)() != 0 || DEMAND_CALL(effective_uid, geteuid)() != 0
        || message->audit.val[0] != message->audit.val[1]
        || message->audit.val[6] == 0 || message->audit.val[6] > INT32_MAX)
        return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    /* authd locates its GUI agent by audit session. A launchd system helper must
     * join the authenticated sender's session, never a request-selected session.
     * This helper handles one request and exits; no later caller shares it. */
    au_asid_t asid = (au_asid_t)message->audit.val[6];
    mach_port_name_t session = MACH_PORT_NULL;
    if (DEMAND_CALL(session_port, audit_session_port)(asid, &session) != 0
        || session == MACH_PORT_NULL) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    au_asid_t joined = DEMAND_CALL(session_join, audit_session_join)(session);
    kern_return_t released = DEMAND_CALL(deallocate, mach_port_deallocate)(mach_task_self(), session);
    if (joined != asid || released != KERN_SUCCESS) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    return st_management_demand_recheck(message);
}

typedef struct {
    mach_msg_header_t header;
    uint32_t length;
    uint8_t bytes[ST_MANAGEMENT_REPLY_BYTES];
} st_management_reply_wire;
typedef struct {
    mach_msg_header_t header;
    uint8_t request_id[16];
} st_management_ack_wire;

static uint64_t monotonic_ns(void);
typedef struct { uint64_t started, expires; } demand_deadline;
static st_management_result demand_begin_wait(uint32_t remaining_ms, demand_deadline *deadline) {
    uint64_t now = monotonic_ns();
    uint64_t budget = (uint64_t)remaining_ms * UINT64_C(1000000);
    if (now == 0 || now > UINT64_MAX - budget) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    deadline->started = now; deadline->expires = now + budget;
    return ST_MANAGEMENT_OK;
}
static st_management_result demand_remaining(demand_deadline deadline, uint32_t *remaining_ms) {
    uint64_t now = monotonic_ns();
    if (now == 0 || now < deadline.started) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    if (now >= deadline.expires) return ST_MANAGEMENT_TIMED_OUT;
    uint64_t milliseconds = (deadline.expires - now) / UINT64_C(1000000);
    *remaining_ms = milliseconds == 0 ? 1 : (uint32_t)milliseconds;
    return ST_MANAGEMENT_OK;
}
static st_management_result demand_message_result(mach_msg_return_t status) {
    if (status == MACH_MSG_SUCCESS) return ST_MANAGEMENT_OK;
    if (status == MACH_RCV_TIMED_OUT || status == MACH_SEND_TIMED_OUT) return ST_MANAGEMENT_TIMED_OUT;
    return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}

st_management_result st_management_demand_reply(st_management_demand_message *message,
    const uint8_t *reply, size_t length, uint32_t remaining_ms) {
    if (message == NULL || reply == NULL || length == 0 || length > ST_MANAGEMENT_REPLY_BYTES
        || remaining_ms == 0 || message->reply == MACH_PORT_NULL) return ST_MANAGEMENT_INVALID_RECORD;
    demand_deadline deadline;
    st_management_result result = demand_begin_wait(remaining_ms, &deadline);
    if (result != 0) return result;
    /* The reply transfers a private send-once acknowledgement right. A queued
     * reply is not yet authenticated: keep this exact helper alive until the
     * original sender has validated and copied it, or the original budget ends. */
    mach_port_t ack = MACH_PORT_NULL;
    if (mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &ack) != KERN_SUCCESS)
        return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    st_management_reply_wire outgoing;
    memset(&outgoing, 0, sizeof(outgoing));
    outgoing.header.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_MOVE_SEND_ONCE, MACH_MSG_TYPE_MAKE_SEND_ONCE);
    outgoing.header.msgh_remote_port = message->reply; outgoing.header.msgh_local_port = ack;
    outgoing.header.msgh_size = sizeof(outgoing); outgoing.header.msgh_id = 0x53544d52;
    outgoing.length = (uint32_t)length; memcpy(outgoing.bytes, reply, length);
    result = demand_remaining(deadline, &remaining_ms);
    if (result == 0) {
        mach_msg_return_t status = DEMAND_CALL(receive, mach_msg)(&outgoing.header,
            MACH_SEND_MSG | MACH_SEND_TIMEOUT, sizeof(outgoing), 0, MACH_PORT_NULL,
            remaining_ms, MACH_PORT_NULL);
        result = demand_message_result(status);
        if (status == MACH_MSG_SUCCESS) message->reply = MACH_PORT_NULL;
    }
    wipe(&outgoing, sizeof(outgoing));
    struct { st_management_ack_wire wire; mach_msg_max_trailer_t trailer; } incoming;
    memset(&incoming, 0, sizeof(incoming));
    if (result == 0) result = demand_remaining(deadline, &remaining_ms);
    if (result == 0) {
        mach_msg_return_t status = DEMAND_CALL(receive, mach_msg)(&incoming.wire.header,
            MACH_RCV_MSG | MACH_RCV_TIMEOUT | MACH_RCV_TRAILER_TYPE(MACH_MSG_TRAILER_FORMAT_0)
                | MACH_RCV_TRAILER_ELEMENTS(MACH_RCV_TRAILER_AUDIT),
            0, sizeof(incoming), ack, remaining_ms, MACH_PORT_NULL);
        result = demand_message_result(status);
        if (status == MACH_MSG_SUCCESS) {
            const mach_msg_audit_trailer_t *audit = (const mach_msg_audit_trailer_t *)&incoming.trailer;
            mach_msg_header_t *header = &incoming.wire.header;
            if (header->msgh_size != sizeof(incoming.wire) || header->msgh_id != 0x53544d41
                || (header->msgh_bits & MACH_MSGH_BITS_COMPLEX) != 0
                || header->msgh_remote_port != MACH_PORT_NULL || header->msgh_local_port != ack
                || audit->msgh_trailer_type != MACH_MSG_TRAILER_FORMAT_0
                || audit->msgh_trailer_size < sizeof(*audit) || audit->msgh_trailer_size > sizeof(incoming.trailer)
                || memcmp(&audit->msgh_audit, &message->audit, sizeof(message->audit)) != 0
                || memcmp(incoming.wire.request_id, message->request + 16, 16) != 0)
                result = ST_MANAGEMENT_IDENTITY_MISMATCH;
            header->msgh_local_port = MACH_PORT_NULL; mach_msg_destroy(header);
        }
    }
    if (result == 0) result = demand_remaining(deadline, &remaining_ms);
    wipe(&incoming, sizeof(incoming));
    if (mach_port_mod_refs(mach_task_self(), ack, MACH_PORT_RIGHT_RECEIVE, -1) != KERN_SUCCESS)
        result = ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    return result;
}

st_management_result st_management_send_gui_demand(uint32_t build, const uint8_t request[160],
    uint8_t out[ST_MANAGEMENT_REPLY_BYTES], size_t *out_length, uint32_t remaining_ms) {
    if (out == NULL || out_length == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    memset(out, 0, ST_MANAGEMENT_REPLY_BYTES); *out_length = 0;
    if (request == NULL || build < 1 || build > 3 || request[6] != build || remaining_ms == 0)
        return ST_MANAGEMENT_INVALID_RECORD;
    uint32_t uid = ((uint32_t)request[12] << 24) | ((uint32_t)request[13] << 16)
        | ((uint32_t)request[14] << 8) | request[15];
    if (uid == 0 || uid != DEMAND_CALL(real_uid, getuid)()
        || uid != DEMAND_CALL(effective_uid, geteuid)()) return ST_MANAGEMENT_IDENTITY_MISMATCH;
    demand_deadline deadline;
    st_management_result result = demand_begin_wait(remaining_ms, &deadline);
    if (result != 0) return result;
    char endpoint[ST_MANAGEMENT_RIGHT_BYTES];
    result = st_management_format_right(build, 4, endpoint, sizeof(endpoint));
    if (result != 0) return result;
    char *suffix = strstr(endpoint, ".manage.start");
    if (suffix == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    memcpy(suffix, ".manager.demand", sizeof(".manager.demand"));
    mach_port_t server = MACH_PORT_NULL, reply = MACH_PORT_NULL;
    if (DEMAND_CALL(look_up, bootstrap_look_up)(bootstrap_port, endpoint, &server) != KERN_SUCCESS
        || mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &reply) != KERN_SUCCESS) {
        if (server != MACH_PORT_NULL) (void)mach_port_deallocate(mach_task_self(), server);
        return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    }
    st_management_demand_wire outgoing;
    memset(&outgoing, 0, sizeof(outgoing));
    outgoing.header.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND_ONCE);
    outgoing.header.msgh_remote_port = server; outgoing.header.msgh_local_port = reply;
    outgoing.header.msgh_size = sizeof(outgoing); outgoing.header.msgh_id = 0x53544d47;
    memcpy(outgoing.request, request, sizeof(outgoing.request));
    result = demand_remaining(deadline, &remaining_ms);
    if (result == 0) result = demand_message_result(DEMAND_CALL(receive, mach_msg)(
        &outgoing.header, MACH_SEND_MSG | MACH_SEND_TIMEOUT,
        sizeof(outgoing), 0, MACH_PORT_NULL, remaining_ms, MACH_PORT_NULL));
    wipe(&outgoing, sizeof(outgoing));
    struct { st_management_reply_wire wire; mach_msg_max_trailer_t trailer; } incoming;
    memset(&incoming, 0, sizeof(incoming));
    if (result == 0) result = demand_remaining(deadline, &remaining_ms);
    if (result == 0) result = demand_message_result(DEMAND_CALL(receive, mach_msg)(&incoming.wire.header,
        MACH_RCV_MSG | MACH_RCV_TIMEOUT | MACH_RCV_TRAILER_TYPE(MACH_MSG_TRAILER_FORMAT_0)
            | MACH_RCV_TRAILER_ELEMENTS(MACH_RCV_TRAILER_AUDIT),
        0, sizeof(incoming), reply, remaining_ms, MACH_PORT_NULL));
    if (result == 0) {
        result = ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
        const mach_msg_audit_trailer_t *audit = (const mach_msg_audit_trailer_t *)&incoming.trailer;
        mach_msg_header_t *header = &incoming.wire.header;
        if (header->msgh_size == sizeof(incoming.wire) && header->msgh_id == 0x53544d52
            && (header->msgh_bits & MACH_MSGH_BITS_COMPLEX) == 0
            && MACH_MSGH_BITS_REMOTE(header->msgh_bits) == MACH_MSG_TYPE_PORT_SEND_ONCE
            && header->msgh_remote_port != MACH_PORT_NULL && header->msgh_local_port == reply
            && audit->msgh_trailer_type == MACH_MSG_TRAILER_FORMAT_0
            && audit->msgh_trailer_size >= sizeof(*audit) && audit->msgh_trailer_size <= sizeof(incoming.trailer)
            && audit->msgh_audit.val[1] == 0 && incoming.wire.length > 0
            && incoming.wire.length <= ST_MANAGEMENT_REPLY_BYTES) {
            SecCodeRef code = NULL; SecRequirementRef requirement = NULL;
            result = demand_guest(&audit->msgh_audit, &code);
            char name[ST_MANAGEMENT_RIGHT_BYTES];
            if (result == 0) {
                /* Same pinned signing identity as the native credential boundary. */
                const char *base = build == 1 ? "io.github.sadiksaifi.spaceterm.server.manager"
                    : build == 2 ? "io.github.sadiksaifi.spaceterm-preflight.server.manager"
                    : "io.github.sadiksaifi.spaceterm-development.server.manager";
                int size = snprintf(name, sizeof(name), "identifier \"%s\" and certificate leaf = H\"9e07cd5c15c5c862a21b93ea55d4303bc19b7366\"", base);
                CFStringRef text = size > 0 && (size_t)size < sizeof(name)
                    ? CFStringCreateWithCString(NULL, name, kCFStringEncodingASCII) : NULL;
                if (text == NULL || SecRequirementCreateWithString(text, 0, &requirement) != errSecSuccess)
                    result = ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
                if (text != NULL) CFRelease(text);
                if (result == 0 && DEMAND_CALL(validate, SecCodeCheckValidity)(code, kSecCSStrictValidate, requirement) != errSecSuccess)
                    result = ST_MANAGEMENT_AUTHORIZATION_DENIED;
            }
            if (requirement != NULL) CFRelease(requirement);
            if (code != NULL) CFRelease(code);
            if (result == 0) result = demand_remaining(deadline, &remaining_ms);
            if (result == 0) {
                *out_length = incoming.wire.length; memcpy(out, incoming.wire.bytes, *out_length);
                st_management_ack_wire ack;
                memset(&ack, 0, sizeof(ack));
                ack.header.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_MOVE_SEND_ONCE, 0);
                ack.header.msgh_remote_port = header->msgh_remote_port;
                ack.header.msgh_size = sizeof(ack); ack.header.msgh_id = 0x53544d41;
                memcpy(ack.request_id, request + 16, 16);
                mach_msg_return_t sent = DEMAND_CALL(receive, mach_msg)(&ack.header,
                    MACH_SEND_MSG | MACH_SEND_TIMEOUT, sizeof(ack), 0, MACH_PORT_NULL,
                    remaining_ms, MACH_PORT_NULL);
                result = demand_message_result(sent);
                if (sent == MACH_MSG_SUCCESS) header->msgh_remote_port = MACH_PORT_NULL;
                if (result == 0) result = demand_remaining(deadline, &remaining_ms);
                wipe(&ack, sizeof(ack));
            }
        }
        header->msgh_local_port = MACH_PORT_NULL; mach_msg_destroy(header);
    }
    if (result != 0) { memset(out, 0, ST_MANAGEMENT_REPLY_BYTES); *out_length = 0; }
    wipe(&incoming, sizeof(incoming));
    (void)mach_port_deallocate(mach_task_self(), server);
    (void)mach_port_mod_refs(mach_task_self(), reply, MACH_PORT_RIGHT_RECEIVE, -1);
    return result;
}

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

st_management_result st_management_verify_gui_right_bound(
    const st_management_admission *scope, uint32_t build, uint32_t operation,
    const uint8_t request[160], const uint8_t external[ST_MANAGEMENT_EXTERNAL_BYTES],
    st_management_right_observation *out) {
    if (out == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    memset(out, 0, sizeof(*out));
    if (external == NULL || request == NULL || scope == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    st_management_result scoped = st_management_admission_recheck(scope, request, external);
    if (scoped != ST_MANAGEMENT_OK) return scoped;
    /* Wire tags must agree even after the trusted scope has been checked. */
    if (request[5] != operation || request[6] != build) return ST_MANAGEMENT_INVALID_RECORD;
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
    if (result == ST_MANAGEMENT_OK)
        result = st_management_admission_consume(scope, request, external);
    /* Consumption is atomic in the original protected scope. The copied authd
     * reference is destroyed; a saved form cannot establish another scope. */
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
struct st_management_local_authorization {
    AuthorizationRef authorization;
    uint8_t request[160];
    uint64_t issued_ns;
};

static st_management_result local_expiry(uint64_t issued) {
    uint64_t now = monotonic_ns();
    if (issued == 0 || now == 0 || now < issued) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    return now - issued >= UINT64_C(30000000000) ? ST_MANAGEMENT_STALE_ASSESSMENT : ST_MANAGEMENT_OK;
}

st_management_result st_management_local_right_release(st_management_local_authorization **owned) {
    if (owned == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    st_management_local_authorization *lease = *owned;
    *owned = NULL;
    if (lease == NULL) return ST_MANAGEMENT_OK;
    OSStatus status = errAuthorizationSuccess;
    if (lease->authorization != NULL)
        status = NATIVE_CALL(free_authorization, AuthorizationFree)(lease->authorization,
            kAuthorizationFlagDestroyRights);
    wipe(lease, sizeof(*lease)); free(lease);
    return st_management_classify_authorization(status);
}

st_management_result st_management_request_local_right(const uint8_t request[160],
    uint64_t issued, uint32_t caller_uid, st_management_local_authorization **out) {
    if (out == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    *out = NULL;
    if (request == NULL || memcmp(request, "STMG", 4) != 0 || request[4] != 1)
        return ST_MANAGEMENT_INVALID_RECORD;
    uint32_t uid = ((uint32_t)request[12] << 24) | ((uint32_t)request[13] << 16)
        | ((uint32_t)request[14] << 8) | request[15];
    if (uid == 0 || uid == UINT32_MAX || caller_uid != uid) return ST_MANAGEMENT_IDENTITY_MISMATCH;
    if (DEMAND_CALL(real_uid, getuid)() != 0 || DEMAND_CALL(effective_uid, geteuid)() != 0)
        return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    st_management_result result = local_expiry(issued);
    if (result != 0) return result;
    char right[ST_MANAGEMENT_RIGHT_BYTES];
    result = st_management_format_right(request[6], request[5], right, sizeof(right));
    if (result == 0) result = inspect_right_policy(right);
    if (result != 0) return result;
    st_management_local_authorization *lease = calloc(1, sizeof(*lease));
    if (lease == NULL) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    result = st_management_classify_authorization(NATIVE_CALL(create, AuthorizationCreate)(NULL,
        kAuthorizationEmptyEnvironment, ST_MANAGEMENT_LEAST_PRIVILEGED_FLAG, &lease->authorization));
    if (result == 0 && lease->authorization == NULL) result = ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    if (result == 0) result = copy_exact_right(lease->authorization, right, 1);
    if (result == 0) result = local_expiry(issued);
    if (result != 0) {
        st_management_result retired = st_management_local_right_release(&lease);
        return retired == 0 ? result : ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    }
    memcpy(lease->request, request, sizeof(lease->request)); lease->issued_ns = issued;
    *out = lease; return ST_MANAGEMENT_OK;
}

st_management_result st_management_local_right_recheck(st_management_local_authorization *lease,
    const uint8_t request[160]) {
    if (lease == NULL || request == NULL || lease->authorization == NULL)
        return ST_MANAGEMENT_AUTHORIZATION_DENIED;
    st_management_result result = memcmp(lease->request, request, 160) == 0
        ? local_expiry(lease->issued_ns) : ST_MANAGEMENT_IDENTITY_MISMATCH;
    char right[ST_MANAGEMENT_RIGHT_BYTES];
    if (result == 0) result = st_management_format_right(request[6], request[5], right, sizeof(right));
    if (result == 0) result = inspect_right_policy(right);
    if (result == 0) result = copy_exact_right(lease->authorization, right, 0);
    if (result == 0) result = local_expiry(lease->issued_ns);
    if (result != 0) {
        OSStatus retired = NATIVE_CALL(free_authorization, AuthorizationFree)(lease->authorization,
            kAuthorizationFlagDestroyRights);
        lease->authorization = NULL;
        if (retired != 0) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    }
    return result;
}
#else
st_management_result st_management_receive_gui_demand(uint32_t build, uint32_t remaining,
    st_management_demand_message **out) {
    (void)build; (void)remaining;
    if (out == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    *out = NULL; return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}
st_management_result st_management_demand_request(const st_management_demand_message *message, uint8_t out[160]) {
    (void)message; if (out == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    memset(out, 0, 160); return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}
st_management_result st_management_demand_external(const st_management_demand_message *message, uint8_t out[32]) {
    (void)message; if (out == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    wipe(out, 32); return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}
st_management_result st_management_demand_caller_uid(const st_management_demand_message *message, uint32_t *out) {
    (void)message; if (out == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    *out = 0; return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}
st_management_result st_management_demand_recheck(const st_management_demand_message *message) {
    (void)message; return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}
st_management_result st_management_demand_join_session(const st_management_demand_message *message) {
    (void)message; return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}
void st_management_demand_release(st_management_demand_message **owned) { if (owned != NULL) *owned = NULL; }
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
st_management_result st_management_verify_gui_right_bound(
    const st_management_admission *scope, uint32_t build, uint32_t operation,
    const uint8_t request[160], const uint8_t external[ST_MANAGEMENT_EXTERNAL_BYTES],
    st_management_right_observation *out) {
    (void)scope; (void)request; (void)build; (void)operation;
    if (out == NULL || external == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    memset(out, 0, sizeof(*out));
    return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}
#endif

#if !defined(__APPLE__)
st_management_result st_management_request_local_right(const uint8_t request[160],
    uint64_t issued, uint32_t caller_uid, st_management_local_authorization **out) {
    (void)request; (void)issued; (void)caller_uid;
    if (out == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    *out = NULL; return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}
st_management_result st_management_local_right_recheck(st_management_local_authorization *lease,
    const uint8_t request[160]) {
    (void)lease; (void)request; return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}
st_management_result st_management_local_right_release(st_management_local_authorization **owned) {
    if (owned == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    *owned = NULL; return ST_MANAGEMENT_OK;
}

#endif

st_management_result st_management_authorize_request(void) {
    return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}

st_management_result st_management_verify_gui_right(uint32_t build, uint32_t operation,
    const uint8_t external[ST_MANAGEMENT_EXTERNAL_BYTES], st_management_right_observation *out) {
    (void)build; (void)operation;
    if (out == NULL || external == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    memset(out, 0, sizeof(*out));
    return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}

#if defined(ST_MANAGEMENT_TESTING)
static st_management_background_observer background_observer;
void st_management_test_set_background_observer(st_management_background_observer observer) {
    background_observer = observer;
}
#endif
#if defined(__APPLE__)
static int background_symbol(const char *name, void *destination, size_t size) {
    void *symbol = dlsym(RTLD_DEFAULT, name);
    if (symbol == NULL || size != sizeof(symbol)) return 0;
    memcpy(destination, &symbol, size);
    return 1;
}
static st_management_result native_background_status(const char *plist, uint32_t *out_status) {
    /* The sealed system framework is the only library opened. The admitted
     * helper bundle owns the daemon plist; no account path or plugin is loaded. */
    void *framework = dlopen("/System/Library/Frameworks/ServiceManagement.framework/ServiceManagement",
        RTLD_NOW | RTLD_LOCAL);
    if (framework == NULL) return ST_MANAGEMENT_NATIVE_UNAVAILABLE;
    void *(*get_class)(const char *) = NULL;
    void *(*selector)(const char *) = NULL;
    void *(*pool_push)(void) = NULL;
    void (*pool_pop)(void *) = NULL;
    void *(*message_object)(void *, void *, CFStringRef) = NULL;
    uintptr_t (*message_status)(void *, void *) = NULL;
    if (!background_symbol("objc_getClass", &get_class, sizeof(get_class)) ||
        !background_symbol("sel_registerName", &selector, sizeof(selector)) ||
        !background_symbol("objc_autoreleasePoolPush", &pool_push, sizeof(pool_push)) ||
        !background_symbol("objc_autoreleasePoolPop", &pool_pop, sizeof(pool_pop)) ||
        !background_symbol("objc_msgSend", &message_object, sizeof(message_object)) ||
        !background_symbol("objc_msgSend", &message_status, sizeof(message_status))) {
        dlclose(framework); return ST_MANAGEMENT_NATIVE_UNAVAILABLE;
    }
    void *pool = pool_push();
    CFStringRef name = CFStringCreateWithCString(NULL, plist, kCFStringEncodingUTF8);
    void *class = get_class("SMAppService");
    void *service = class != NULL && name != NULL ?
        message_object(class, selector("daemonServiceWithPlistName:"), name) : NULL;
    uintptr_t status = service != NULL ? message_status(service, selector("status")) : UINTPTR_MAX;
    if (name != NULL) CFRelease(name);
    pool_pop(pool);
    dlclose(framework);
    if (status > 3) return ST_MANAGEMENT_NATIVE_UNAVAILABLE;
    *out_status = (uint32_t)status;
    return ST_MANAGEMENT_OK;
}
#endif
st_management_result st_management_observe_background_items(
    uint32_t build, uint32_t account_uid, uint32_t *out_status) {
    if (out_status != NULL) *out_status = UINT32_MAX;
    if (out_status == NULL || build < 1 || build > 3 || account_uid == 0 || account_uid == UINT32_MAX)
        return ST_MANAGEMENT_INVALID_RECORD;
    static const char *const identifiers[] = {
        "io.github.sadiksaifi.spaceterm.server",
        "io.github.sadiksaifi.spaceterm-preflight.server",
        "io.github.sadiksaifi.spaceterm-development.server"
    };
    char plist[160];
    int written = snprintf(plist, sizeof(plist), "%s.%u.plist", identifiers[build - 1], account_uid);
    if (written < 0 || (size_t)written >= sizeof(plist)) return ST_MANAGEMENT_INVALID_RECORD;
    uint32_t status = UINT32_MAX;
    st_management_result result;
#if defined(ST_MANAGEMENT_TESTING)
    if (background_observer != NULL) result = background_observer(plist, &status);
    else
#endif
#if defined(__APPLE__)
        result = native_background_status(plist, &status);
#else
        result = ST_MANAGEMENT_NATIVE_UNAVAILABLE;
#endif
    if (result != ST_MANAGEMENT_OK) return result;
    if (status > 3) return ST_MANAGEMENT_NATIVE_UNAVAILABLE;
    *out_status = status;
    return ST_MANAGEMENT_OK;
}
