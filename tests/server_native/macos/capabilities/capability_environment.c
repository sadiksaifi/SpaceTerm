#include "capability_environment.h"
#include "capability_internal.h"
#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>
#include <string.h>
#include <stdlib.h>

st_native_result st_capability_normalize_environment(const char *path, int locale_present,
    st_capability_environment *out) {
    if (!out || (locale_present != 0 && locale_present != 1)) return ST_PROBE_INVALID;
    if (!path) { *out = (st_capability_environment){1, 0, 0, (uint32_t)locale_present}; return 0; }
    size_t length = 0;
    while (length <= 4096 && path[length]) ++length;
    if (length > 4096) return ST_PROBE_LIMIT;
    size_t starts[256], lengths[256];
    uint32_t count = 0, duplicates = 0;
    size_t start = 0;
    for (size_t i = 0; i <= length; ++i) {
        if (i != length && path[i] != ':') continue;
        if (count == 256) return ST_PROBE_LIMIT;
        size_t size = i - start;
        for (uint32_t j = 0; j < count; ++j) {
            if (lengths[j] == size && memcmp(path + starts[j], path + start, size) == 0) {
                ++duplicates; break;
            }
        }
        starts[count] = start; lengths[count] = size; ++count;
        start = i + 1;
    }
    *out = (st_capability_environment){1, count, duplicates, (uint32_t)locale_present};
    return 0;
}

st_native_result st_capability_environment_encode(const st_capability_environment *value, uint8_t out[16]) {
    if (!value || !out || value->schema != 1 || value->path_components > 256 ||
        value->duplicate_path_components > value->path_components || value->locale_present > 1) return ST_PROBE_INVALID;
    uint32_t fields[] = {value->schema, value->path_components, value->duplicate_path_components, value->locale_present};
    for (size_t i = 0; i < 4; ++i) {
        out[i * 4] = (uint8_t)(fields[i] >> 24); out[i * 4 + 1] = (uint8_t)(fields[i] >> 16);
        out[i * 4 + 2] = (uint8_t)(fields[i] >> 8); out[i * 4 + 3] = (uint8_t)fields[i];
    }
    return 0;
}

const char *const st_capability_locale_names[ST_CAPABILITY_LOCALE_COUNT] = {
    "LANG", "LC_ALL", "LC_CTYPE", "LC_NUMERIC", "LC_TIME", "LC_COLLATE", "LC_MONETARY", "LC_MESSAGES",
    "LC_PAPER", "LC_NAME", "LC_ADDRESS", "LC_TELEPHONE", "LC_MEASUREMENT", "LC_IDENTIFICATION"
};

struct st_capability_retained_environment {
    uint32_t context;
    char *path;
    char *locales[ST_CAPABILITY_LOCALE_COUNT];
};
static char *retain_value(const char *value) {
    if (!value) return NULL;
    size_t size = strlen(value) + 1;
    char *copy = malloc(size);
    if (copy) memcpy(copy, value, size);
    return copy;
}
void st_capability_environment_release(st_capability_retained_environment **owned) {
    if (!owned || !*owned) return;
    st_capability_retained_environment *value = *owned;
    if (value->path) { st_capability_discard_owned(value->path, strlen(value->path)); free(value->path); }
    for (size_t i = 0; i < ST_CAPABILITY_LOCALE_COUNT; ++i) if (value->locales[i]) {
        st_capability_discard_owned(value->locales[i], strlen(value->locales[i])); free(value->locales[i]);
    }
    st_capability_discard_owned(value, sizeof(*value)); free(value); *owned = NULL;
}
st_native_result st_capability_environment_retain(st_probe_context context,
    const char *path, const char *const locales[ST_CAPABILITY_LOCALE_COUNT], st_capability_retained_environment **out) {
    if (!out || *out || !locales || context < 1 || context > 2) return ST_PROBE_INVALID;
    st_capability_environment counts;
    st_native_result result = st_capability_normalize_environment(path, 0, &counts);
    if (result) return result;
    for (size_t i = 0; i < ST_CAPABILITY_LOCALE_COUNT; ++i) {
        size_t length = 0;
        if (locales[i]) while (length <= 4096 && locales[i][length]) ++length;
        if (length > 4096) return ST_PROBE_LIMIT;
    }
    st_capability_retained_environment *value = calloc(1, sizeof(*value));
    if (!value) return ST_PROBE_IO_UNAVAILABLE;
    value->context = context; value->path = retain_value(path);
    int okay = !path || value->path;
    for (size_t i = 0; i < ST_CAPABILITY_LOCALE_COUNT; ++i) {
        value->locales[i] = retain_value(locales[i]);
        okay = okay && (!locales[i] || value->locales[i]);
    }
    if (!okay) { st_capability_environment_release(&value); return ST_PROBE_IO_UNAVAILABLE; }
    *out = value; return 0;
}
typedef struct { const char *bytes; size_t length; } component;
static size_t components(const char *path, component out[256]) {
    size_t count = 0, start = 0;
    for (size_t i = 0;; ++i) {
        if (path[i] && path[i] != ':') continue;
        out[count++] = (component){path + start, i - start};
        if (!path[i]) return count;
        start = i + 1;
    }
}
static int same_components(const char *left, const char *right) {
    component a[256], b[256];
    size_t count = components(left, a);
    if (components(right, b) != count) return 0;
    int matched[256] = {0};
    for (size_t i = 0; i < count; ++i) {
        size_t j = 0;
        while (j < count && (matched[j] || a[i].length != b[j].length ||
            memcmp(a[i].bytes, b[j].bytes, a[i].length))) ++j;
        if (j == count) return 0;
        matched[j] = 1;
    }
    return 1;
}
st_native_result st_capability_environment_compare(const st_capability_retained_environment *gui,
    const st_capability_retained_environment *daemon, st_capability_environment_difference *out) {
    if (!gui || !daemon || !out || gui->context != ST_CONTEXT_GUI || daemon->context != ST_CONTEXT_DAEMON)
        return ST_PROBE_INVALID;
    *out = (st_capability_environment_difference){1, ST_CONTEXT_GUI, ST_CONTEXT_DAEMON,
        ST_PATH_EQUAL, ST_LOCALE_EQUAL};
    if ((!gui->path) != (!daemon->path)) out->path_difference = ST_PATH_PRESENCE_CHANGED;
    else if (gui->path && strcmp(gui->path, daemon->path))
        out->path_difference = same_components(gui->path, daemon->path) ? ST_PATH_ORDER_CHANGED : ST_PATH_COMPONENTS_CHANGED;
    for (size_t i = 0; i < ST_CAPABILITY_LOCALE_COUNT; ++i) {
        if ((!gui->locales[i]) != (!daemon->locales[i])) out->locale_difference = ST_LOCALE_PRESENCE_CHANGED;
        else if (gui->locales[i] && strcmp(gui->locales[i], daemon->locales[i]) &&
            out->locale_difference != ST_LOCALE_PRESENCE_CHANGED) out->locale_difference = ST_LOCALE_VALUES_CHANGED;
    }
    return 0;
}
st_native_result st_capability_environment_difference_encode(
    const st_capability_environment_difference *value, uint8_t out[20]) {
    if (!value || !out || value->schema != 1 || value->gui_context != ST_CONTEXT_GUI ||
        value->daemon_context != ST_CONTEXT_DAEMON || value->path_difference < 1 || value->path_difference > 4 ||
        value->locale_difference < 1 || value->locale_difference > 3) return ST_PROBE_INVALID;
    uint32_t fields[] = {value->schema, value->gui_context, value->daemon_context,
        value->path_difference, value->locale_difference};
    for (size_t i = 0; i < 5; ++i) {
        out[i * 4] = (uint8_t)(fields[i] >> 24); out[i * 4 + 1] = (uint8_t)(fields[i] >> 16);
        out[i * 4 + 2] = (uint8_t)(fields[i] >> 8); out[i * 4 + 3] = (uint8_t)fields[i];
    }
    return 0;
}
int st_capability_shell_invocation(const char *const argv[], size_t count, int has_pty) {
    if (!argv || count == 0 || count > 32 || !argv[0] || !has_pty) return 0;
    int login = argv[0][0] == '-', interactive = 0;
    for (size_t i = 1; i < count; ++i) {
        const char *value = argv[i];
        if (!value || value[0] != '-' || !value[1]) return 0;
        if (!strcmp(value, "--login")) { login = 1; continue; }
        for (size_t j = 1; value[j]; ++j) {
            if (value[j] == 'l') login = 1;
            else if (value[j] == 'i') interactive = 1;
            else return 0;
        }
    }
    return login && interactive;
}

/* SCE1 is a private codec, not sender evidence. Actual values require native
 * frame-bound sender evidence, not endpoint last-owner PID/code checks. No current
 * native adapter supplies it; only injected transport tests use these routines. */
static void put_u32(uint8_t *bytes, uint32_t value) {
    bytes[0] = (uint8_t)(value >> 24); bytes[1] = (uint8_t)(value >> 16);
    bytes[2] = (uint8_t)(value >> 8); bytes[3] = (uint8_t)value;
}
static uint32_t get_u32(const uint8_t *bytes) {
    return (uint32_t)bytes[0] << 24 | (uint32_t)bytes[1] << 16 | (uint32_t)bytes[2] << 8 | bytes[3];
}
st_native_result st_capability_environment_send_private(int fd,
    const st_capability_retained_environment *value, uint64_t deadline) {
    if (!value) return ST_PROBE_INVALID;
    uint8_t header[8 + (ST_CAPABILITY_LOCALE_COUNT + 1) * 4] = {'S', 'C', 'E', '1'};
    put_u32(header + 4, value->context);
    const char *values[ST_CAPABILITY_LOCALE_COUNT + 1] = {value->path};
    for (size_t i = 0; i < ST_CAPABILITY_LOCALE_COUNT; ++i) values[i + 1] = value->locales[i];
    for (size_t i = 0; i < ST_CAPABILITY_LOCALE_COUNT + 1; ++i)
        put_u32(header + 8 + i * 4, values[i] ? (uint32_t)strlen(values[i]) : UINT32_MAX);
    int okay = st_capability_transfer(fd, header, sizeof(header), 1, deadline);
    for (size_t i = 0; okay && i < ST_CAPABILITY_LOCALE_COUNT + 1; ++i)
        if (values[i]) okay = st_capability_transfer(fd, (void *)values[i], strlen(values[i]), 1, deadline);
    if (okay) okay = shutdown(fd, SHUT_WR) == 0;
    return okay ? ST_PROBE_OK : ST_PROBE_IO_UNAVAILABLE;
}
st_native_result st_capability_environment_receive_private(int fd, st_probe_context context,
    uint64_t deadline, st_capability_retained_environment **out) {
    if (!out || *out || context < 1 || context > 2) return ST_PROBE_INVALID;
    uint8_t header[8 + (ST_CAPABILITY_LOCALE_COUNT + 1) * 4];
    if (!st_capability_transfer(fd, header, sizeof(header), 0, deadline) ||
        memcmp(header, "SCE1", 4) || get_u32(header + 4) != context) return ST_PROBE_INVALID;
    char *values[ST_CAPABILITY_LOCALE_COUNT + 1] = {0};
    st_native_result result = ST_PROBE_OK;
    for (size_t i = 0; i < ST_CAPABILITY_LOCALE_COUNT + 1; ++i) {
        uint32_t length = get_u32(header + 8 + i * 4);
        if (length == UINT32_MAX) continue;
        if (length > 4096) { result = ST_PROBE_LIMIT; break; }
        values[i] = calloc((size_t)length + 1, 1);
        if (!values[i]) { result = ST_PROBE_IO_UNAVAILABLE; break; }
        if (!st_capability_transfer(fd, values[i], length, 0, deadline) || memchr(values[i], 0, length)) {
            result = ST_PROBE_INVALID; break;
        }
    }
    uint8_t extra;
    if (result == ST_PROBE_OK && (!st_capability_wait_fd(fd, POLLIN, deadline) || read(fd, &extra, 1) != 0))
        result = ST_PROBE_INVALID;
    if (result == ST_PROBE_OK) result = st_capability_environment_retain(context, values[0],
        (const char *const *)(values + 1), out);
    for (size_t i = 0; i < ST_CAPABILITY_LOCALE_COUNT + 1; ++i) if (values[i]) {
        uint32_t length = get_u32(header + 8 + i * 4);
        st_capability_discard_owned(values[i], (size_t)length + 1); free(values[i]);
    }
    return result;
}
