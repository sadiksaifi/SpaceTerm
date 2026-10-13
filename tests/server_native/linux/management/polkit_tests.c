#include "../../../../crates/spaceterm-host/native/linux/management/polkit_test.h"
#include <assert.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>

#if defined(__APPLE__)
/* This test does not exercise saved Authorization Services forms. */
struct st_management_admission;
uint32_t st_management_admission_recheck(const struct st_management_admission *scope,
    const uint8_t request[160], const uint8_t external[32]) {
    (void)scope; (void)request; (void)external; return 18;
}
uint32_t st_management_admission_consume(const struct st_management_admission *scope,
    const uint8_t request[160], const uint8_t external[32]) {
    (void)scope; (void)request; (void)external; return 18;
}
#endif
static int bus_object, reply_object, owner_object;
static unsigned queries, authorization_checks, messages_retired, buses_retired, detail;
static int authorized, challenge, dismissed, authority_uid, end, fail_call, unrelated_sender;
static int bus_new(sd_bus **out) { *out = (sd_bus *)&bus_object; return 0; }
static int address(sd_bus *bus, const char *value) { assert(bus != NULL && strcmp(value, "unix:path=/run/dbus/system_bus_socket") == 0); return 0; }
static int client(sd_bus *bus, int value) { assert(bus != NULL && value == 1); return 0; }
static int start(sd_bus *bus) { assert(bus != NULL); return 0; }
static int timeout(sd_bus *bus, uint64_t value) { assert(bus != NULL && value > 0 && value <= UINT64_C(300000000)); return 0; }
static int call(sd_bus *bus, const char *destination, const char *path, const char *interface,
    const char *member, sd_bus_error *error, sd_bus_message **out, const char *types, ...) {
    assert(bus != NULL && error != NULL); queries++;
    va_list values; va_start(values, types);
    if (strcmp(member, "CheckAuthorization") == 0) {
        authorization_checks++;
        assert(strcmp(destination, "org.freedesktop.PolicyKit1") == 0);
        assert(strcmp(path, "/org/freedesktop/PolicyKit1/Authority") == 0);
        assert(strcmp(interface, "org.freedesktop.PolicyKit1.Authority") == 0);
        assert(strcmp(types, "(sa{sv})sa{ss}us") == 0);
        assert(strcmp(va_arg(values, const char *), "unix-process") == 0);
        assert(va_arg(values, int) == 3);
        assert(strcmp(va_arg(values, const char *), "pid") == 0);
        assert(strcmp(va_arg(values, const char *), "u") == 0);
        assert(va_arg(values, uint32_t) == 777);
        assert(strcmp(va_arg(values, const char *), "start-time") == 0);
        assert(strcmp(va_arg(values, const char *), "t") == 0);
        assert(va_arg(values, uint64_t) == 42);
        assert(strcmp(va_arg(values, const char *), "uid") == 0);
        assert(strcmp(va_arg(values, const char *), "i") == 0);
        assert(va_arg(values, int32_t) == 501);
        assert(strcmp(va_arg(values, const char *), "io.github.sadiksaifi.spaceterm-development.server.manage.stop") == 0);
        assert(va_arg(values, int) == 0);
        assert(va_arg(values, uint32_t) == 1);
        assert(strcmp(va_arg(values, const char *), "") == 0);
        *out = fail_call ? NULL : (sd_bus_message *)&reply_object;
    } else {
        assert(strcmp(destination, "org.freedesktop.DBus") == 0);
        assert(strcmp(member, "GetNameOwner") == 0 && strcmp(types, "s") == 0);
        assert(strcmp(va_arg(values, const char *), "org.freedesktop.PolicyKit1") == 0);
        *out = (sd_bus_message *)&owner_object;
    }
    va_end(values); return fail_call ? -1 : 1;
}
static int enter(sd_bus_message *reply, char type, const char *signature) {
    assert(reply == (sd_bus_message *)&reply_object);
    assert((type == 'r' && strcmp(signature, "bba{ss}") == 0) || (type == 'a' && strcmp(signature, "{ss}") == 0)); return 1;
}
static int exit_container(sd_bus_message *reply) { assert(reply != NULL); return 1; }
static int read_message(sd_bus_message *reply, const char *types, ...) {
    va_list values; va_start(values, types); int result = 1;
    if (strcmp(types, "s") == 0) {
        assert(reply == (sd_bus_message *)&owner_object);
        assert(authority_uid == 0 || authority_uid == 997);
        *va_arg(values, const char **) = ":1.77";
    } else if (strcmp(types, "bb") == 0) {
        *va_arg(values, int *) = authorized; *va_arg(values, int *) = challenge;
    } else {
        assert(strcmp(types, "{ss}") == 0);
        if (detail++ == 0 && dismissed) {
            *va_arg(values, const char **) = "polkit.dismissed";
            *va_arg(values, const char **) = "true";
        } else result = 0;
    }
    va_end(values); return result;
}
static int at_end(sd_bus_message *reply, int complete) { assert(reply != NULL && complete == 1); return end; }
static const char *sender(sd_bus_message *reply) { assert(reply != NULL); return unrelated_sender ? ":1.88" : ":1.77"; }
static sd_bus_message *message_unref(sd_bus_message *reply) { assert(reply != NULL); messages_retired++; return NULL; }
static sd_bus *bus_unref(sd_bus *bus) { assert(bus != NULL); buses_retired++; return NULL; }
static void error_free(sd_bus_error *error) { assert(error != NULL); }
static const st_management_bus_api native = {
    NULL, bus_new, address, client, start, timeout, call, enter, exit_container, read_message,
    at_end, sender, message_unref, bus_unref, error_free
};
static void reset(void) {
    queries = authorization_checks = messages_retired = buses_retired = detail = 0;
    authorized = 1; challenge = dismissed = authority_uid = fail_call = unrelated_sender = 0; end = 1;
    st_management_polkit_test_set_api(&native);
}
void st_management_polkit_composition_mode(unsigned mode) {
    reset();
    switch (mode) {
        case 0: break;
        case 1: authorized = 0; break;
        case 2: authorized = 0; dismissed = 1; break;
        case 3: authorized = 0; challenge = 1; break;
        case 4: challenge = 1; break;
        case 5: unrelated_sender = 1; break;
        case 6: fail_call = 1; break;
        case 7: end = 0; break;
        case 8: authority_uid = 997; break;
        case 9: authority_uid = 997; authorized = 0; break;
        case 10: authority_uid = 997; authorized = 0; dismissed = 1; break;
        default: assert(0);
    }
}
unsigned st_management_polkit_composition_retired(void) { return buses_retired; }
unsigned st_management_polkit_composition_checks(void) { return authorization_checks; }
#if !defined(ST_MANAGEMENT_COMPOSITION_FIXTURE)
static void frame(uint8_t request[160]) {
    memset(request, 0, 160); memcpy(request, "STMG", 4); request[4] = 1;
    request[5] = 5; request[6] = 3; request[11] = 160; request[14] = 1; request[15] = 245;
    request[16] = 1; request[88] = 1;
}
static void native_grant_is_subject_action_and_request_bound(void) {
    reset(); uint8_t request[160]; frame(request);
    st_management_polkit_authorization *lease = NULL;
    assert(st_management_polkit_check(request, 501, 777, 42, 5000000, &lease) == 0);
    assert(lease != NULL && queries == 2 && messages_retired == 1);
    assert(st_management_polkit_recheck(lease, request, 501, 777, 42) == 0);
    request[88] ^= 1;
    assert(st_management_polkit_recheck(lease, request, 501, 777, 42) == 12);
    request[88] ^= 1;
    assert(st_management_polkit_recheck(lease, request, 501, 778, 42) == 12);
    assert(st_management_polkit_recheck(lease, request, 501, 777, 43) == 12);
    st_management_polkit_release(&lease);
    assert(lease == NULL && messages_retired == 2 && buses_retired == 1);
}
static void native_denial_cancellation_missing_agent_and_bad_authority_have_no_grant(void) {
    for (unsigned mode = 0; mode < 7; mode++) {
        reset(); uint8_t request[160]; frame(request);
        uint32_t expected = 18;
        switch (mode) {
            case 0: authorized = 0; expected = 16; break;
            case 1: authorized = 0; dismissed = 1; expected = 17; break;
            case 2: authorized = 0; challenge = 1; break;
            case 3: challenge = 1; break;
            case 4: unrelated_sender = 1; break;
            case 5: fail_call = 1; break;
            case 6: end = 0; break;
        }
        st_management_polkit_authorization *lease = NULL;
        assert(st_management_polkit_check(request, 501, 777, 42, 5000000, &lease) == expected);
        assert(lease == NULL && buses_retired == 1);
        assert(messages_retired == (fail_call ? 0u : 2u));
    }
}
static void invalid_subject_and_request_are_rejected_before_native_authority(void) {
    reset(); uint8_t request[160]; frame(request); st_management_polkit_authorization *lease = NULL;
    assert(st_management_polkit_check(request, 502, 777, 42, 5000000, &lease) == 12);
    assert(st_management_polkit_check(request, 501, 0, 42, 5000000, &lease) == 9);
    assert(st_management_polkit_check(request, 501, 777, 0, 5000000, &lease) == 9);
    request[5] = 12;
    assert(st_management_polkit_check(request, 501, 777, 42, 5000000, &lease) == 9);
    assert(lease == NULL && queries == 0 && messages_retired == 0);
}
static void nonroot_authority_preserves_grant_denial_and_cancellation(void) {
    for (unsigned outcome = 0; outcome < 3; outcome++) {
        reset(); uint8_t request[160]; frame(request);
        authority_uid = 997;
        authorized = outcome == 0; dismissed = outcome == 2;
        uint32_t expected = outcome == 0 ? 0 : outcome == 1 ? 16 : 17;
        st_management_polkit_authorization *lease = NULL;
        assert(st_management_polkit_check(request, 501, 777, 42, 5000000, &lease) == expected);
        assert((lease != NULL) == (outcome == 0));
        st_management_polkit_release(&lease);
        assert(buses_retired == 1);
    }
}
int main(void) {
    nonroot_authority_preserves_grant_denial_and_cancellation();
    native_grant_is_subject_action_and_request_bound();
    native_denial_cancellation_missing_agent_and_bad_authority_have_no_grant();
    invalid_subject_and_request_are_rejected_before_native_authority();
    puts("tests=4 proof=injected-native-polkit"); return 0;
}

#endif
