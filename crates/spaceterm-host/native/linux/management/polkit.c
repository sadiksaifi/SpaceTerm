#define _GNU_SOURCE
#include "polkit.h"
#include "../../macos/management.h"
#include <dlfcn.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* sd-bus is in the supported systemd 249 runtime. The admitted manager closure
 * must include this OS library and its loader/configuration dependencies. */
#include "polkit_native.h"
typedef st_management_bus_api bus_api;
#if defined(ST_MANAGEMENT_TESTING)
#include "polkit_test.h"
static const bus_api *test_api;
void st_management_polkit_test_set_api(const bus_api *api) { test_api = api; }
#endif
struct st_management_polkit_authorization {
    bus_api api;
    sd_bus *bus;
    sd_bus_message *reply;
    uint8_t request[160];
    uint32_t uid, pid;
    uint64_t start_ticks;
};
static void wipe(void *memory, size_t length) {
    volatile uint8_t *bytes = memory;
    while (length-- != 0) *bytes++ = 0;
}
void st_management_polkit_release(st_management_polkit_authorization **owned) {
    if (owned == NULL || *owned == NULL) return;
    st_management_polkit_authorization *lease = *owned; *owned = NULL;
    if (lease->reply != NULL) lease->api.message_unref(lease->reply);
    if (lease->bus != NULL) lease->api.bus_unref(lease->bus);
    if (lease->api.library != NULL) dlclose(lease->api.library);
    wipe(lease, sizeof(*lease)); free(lease);
}
static int load_api(bus_api *api) {
#if defined(ST_MANAGEMENT_TESTING)
    if (test_api != NULL) { *api = *test_api; return 1; }
#endif
    /* Fixed supported OS anchor, not LD_LIBRARY_PATH or a caller-selected path.
     * Native publication separately proves custody of this complete closure. */
    api->library = dlopen("/lib/x86_64-linux-gnu/libsystemd.so.0", RTLD_NOW | RTLD_LOCAL);
    if (api->library == NULL) return 0;
#define LOAD(field, name) do { \
    void *symbol = dlsym(api->library, name); \
    if (symbol == NULL || sizeof(api->field) != sizeof(symbol)) return 0; \
    memcpy(&api->field, &symbol, sizeof(symbol)); \
} while (0)
    LOAD(bus_new, "sd_bus_new"); LOAD(set_address, "sd_bus_set_address");
    LOAD(set_client, "sd_bus_set_bus_client"); LOAD(start, "sd_bus_start");
    LOAD(set_timeout, "sd_bus_set_method_call_timeout"); LOAD(call, "sd_bus_call_method");
    LOAD(enter, "sd_bus_message_enter_container"); LOAD(exit, "sd_bus_message_exit_container");
    LOAD(read, "sd_bus_message_read"); LOAD(at_end, "sd_bus_message_at_end");
    LOAD(sender, "sd_bus_message_get_sender"); LOAD(message_unref, "sd_bus_message_unref");
    LOAD(bus_unref, "sd_bus_unref"); LOAD(error_free, "sd_bus_error_free");
#undef LOAD
    return 1;
}
static uint32_t decode_result(st_management_polkit_authorization *lease) {
    int authorized = 0, challenge = 0, dismissed = 0;
    bus_api *api = &lease->api;
    if (api->enter(lease->reply, 'r', "bba{ss}") <= 0
        || api->read(lease->reply, "bb", &authorized, &challenge) <= 0
        || api->enter(lease->reply, 'a', "{ss}") <= 0) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    for (unsigned count = 0;; count++) {
        const char *key = NULL, *value = NULL;
        int next = api->read(lease->reply, "{ss}", &key, &value);
        if (next < 0) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
        if (next == 0) break;
        if (count == 32 || key == NULL || value == NULL || strnlen(key, 257) > 256
            || strnlen(value, 4097) > 4096) return UINT32_C(30);
        if (strcmp(key, "polkit.dismissed") == 0) dismissed = strcmp(value, "true") == 0;
    }
    if (api->exit(lease->reply) < 0 || api->exit(lease->reply) < 0
        || api->at_end(lease->reply, 1) <= 0 || (authorized && challenge)
        || (authorized && dismissed)) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    if (dismissed) return ST_MANAGEMENT_AUTHORIZATION_CANCELLED;
    if (authorized) return ST_MANAGEMENT_OK;
    return challenge ? ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE : ST_MANAGEMENT_AUTHORIZATION_DENIED;
}
uint32_t st_management_polkit_check(const uint8_t request[160], uint32_t uid,
    uint32_t pid, uint64_t start_ticks, uint64_t timeout_us,
    st_management_polkit_authorization **out) {
    if (out == NULL) return ST_MANAGEMENT_INVALID_RECORD;
    *out = NULL;
    if (request == NULL || memcmp(request, "STMG", 4) != 0 || request[4] != 1
        || pid == 0 || pid > INT32_MAX || uid == 0 || uid == UINT32_MAX
        || start_ticks == 0 || timeout_us == 0) return ST_MANAGEMENT_INVALID_RECORD;
    uint32_t request_uid = ((uint32_t)request[12] << 24) | ((uint32_t)request[13] << 16)
        | ((uint32_t)request[14] << 8) | request[15];
    if (request_uid != uid) return ST_MANAGEMENT_IDENTITY_MISMATCH;
#if defined(ST_MANAGEMENT_TESTING)
    if (test_api == NULL && (getuid() != 0 || geteuid() != 0)) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
#else
    if (getuid() != 0 || geteuid() != 0) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
#endif
    char action[ST_MANAGEMENT_RIGHT_BYTES];
    uint32_t result = st_management_format_right(request[6], request[5], action, sizeof(action));
    if (result != 0) return result;
    st_management_polkit_authorization *lease = calloc(1, sizeof(*lease));
    if (lease == NULL) return ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    result = ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
    sd_bus_error error = { 0 };
    sd_bus_message *identity = NULL;
    if (!load_api(&lease->api) || lease->api.bus_new(&lease->bus) < 0
        || lease->api.set_address(lease->bus, "unix:path=/run/dbus/system_bus_socket") < 0
        || lease->api.set_client(lease->bus, 1) < 0
        || lease->api.set_timeout(lease->bus, timeout_us) < 0 || lease->api.start(lease->bus) < 0) goto done;
    /* Root asks the native authority about the retained original account process,
     * not about this root helper or PKEXEC_UID. AllowUserInteraction is one-shot. */
    if (lease->api.call(lease->bus, "org.freedesktop.PolicyKit1", "/org/freedesktop/PolicyKit1/Authority",
        "org.freedesktop.PolicyKit1.Authority", "CheckAuthorization", &error, &lease->reply,
        "(sa{sv})sa{ss}us", "unix-process", 3, "pid", "u", pid,
        "start-time", "t", start_ticks, "uid", "i", (int32_t)uid, action, 0, UINT32_C(1), "") < 0) goto done;
    const char *sender = lease->api.sender(lease->reply);
    if (sender == NULL || sender[0] != ':' || strnlen(sender, 257) > 256) goto done;
    /* The protected system-bus policy owns this well-known name. Supported polkitd
     * runs as root or its dedicated service account; a UID is not its bus identity.
     * Compare the native reply with the unique owner on this same retained bus. */
    if (lease->api.call(lease->bus, "org.freedesktop.DBus", "/org/freedesktop/DBus",
        "org.freedesktop.DBus", "GetNameOwner", &error, &identity, "s", "org.freedesktop.PolicyKit1") < 0) goto done;
    const char *owner = NULL;
    if (lease->api.read(identity, "s", &owner) <= 0 || owner == NULL || owner[0] != ':'
        || strnlen(owner, 257) > 256 || lease->api.at_end(identity, 1) <= 0
        || strcmp(sender, owner) != 0) goto done;
    result = decode_result(lease);
done:
    if (identity != NULL) lease->api.message_unref(identity);
    if (lease->api.error_free != NULL) lease->api.error_free(&error);
    if (result != 0) { st_management_polkit_release(&lease); return result; }
    memcpy(lease->request, request, sizeof(lease->request));
    lease->uid = uid; lease->pid = pid; lease->start_ticks = start_ticks;
    *out = lease; return ST_MANAGEMENT_OK;
}
uint32_t st_management_polkit_recheck(const st_management_polkit_authorization *lease,
    const uint8_t request[160], uint32_t uid, uint32_t pid, uint64_t start_ticks) {
    if (lease == NULL || lease->reply == NULL || request == NULL)
        return ST_MANAGEMENT_AUTHORIZATION_DENIED;
    if (lease->uid != uid || lease->pid != pid || lease->start_ticks != start_ticks
        || memcmp(lease->request, request, 160) != 0) return ST_MANAGEMENT_IDENTITY_MISMATCH;
#if defined(ST_MANAGEMENT_TESTING)
    if (test_api != NULL) return ST_MANAGEMENT_OK;
#endif
    return getuid() == 0 && geteuid() == 0 ? ST_MANAGEMENT_OK : ST_MANAGEMENT_AUTHORIZATION_UNAVAILABLE;
}
