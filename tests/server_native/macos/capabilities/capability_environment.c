#include "capability_environment.h"
#include "capability_internal.h"
#include <string.h>
#include <poll.h>
#include <unistd.h>

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
st_native_result st_capability_environment_compare_local(st_probe_context context, const char *path,
    const char *const locales[ST_CAPABILITY_LOCALE_COUNT], st_capability_environment_difference *out) {
    if (!out || !locales || context < 1 || context > 2) return ST_PROBE_INVALID;
    st_capability_environment counts;
    st_native_result result = st_capability_normalize_environment(path, 0, &counts);
    if (result) return result;
    for (size_t i = 0; i < ST_CAPABILITY_LOCALE_COUNT; ++i) {
        size_t length = 0;
        if (locales[i]) while (length <= 4096 && locales[i][length]) ++length;
        if (length > 4096) return ST_PROBE_LIMIT;
    }
    static const char baseline[] = "/usr/bin:/bin:/usr/sbin:/sbin";
    st_capability_environment_difference difference = {2, context, 1, ST_PATH_EQUAL, ST_LOCALE_EQUAL};
    if (!path) difference.path_difference = ST_PATH_PRESENCE_CHANGED;
    else if (strcmp(path, baseline)) difference.path_difference =
        same_components(path, baseline) ? ST_PATH_ORDER_CHANGED : ST_PATH_COMPONENTS_CHANGED;
    for (size_t i = 0; i < ST_CAPABILITY_LOCALE_COUNT; ++i) {
        const char *reference = i == 0 ? "C" : NULL;
        if ((!reference) != (!locales[i])) difference.locale_difference = ST_LOCALE_PRESENCE_CHANGED;
        else if (reference && strcmp(reference, locales[i]) && difference.locale_difference != ST_LOCALE_PRESENCE_CHANGED)
            difference.locale_difference = ST_LOCALE_VALUES_CHANGED;
    }
    *out = difference; return ST_PROBE_OK;
}
st_native_result st_capability_environment_difference_encode(
    const st_capability_environment_difference *value, uint8_t out[20]) {
    if (!value || !out || value->schema != 2 || value->context < 1 || value->context > 2 ||
        value->reference != 1 || value->path_difference < 1 || value->path_difference > 4 ||
        value->locale_difference < 1 || value->locale_difference > 3) return ST_PROBE_INVALID;
    uint32_t fields[] = {value->schema, value->context, value->reference,
        value->path_difference, value->locale_difference};
    for (size_t i = 0; i < 5; ++i) {
        out[i * 4] = (uint8_t)(fields[i] >> 24); out[i * 4 + 1] = (uint8_t)(fields[i] >> 16);
        out[i * 4 + 2] = (uint8_t)(fields[i] >> 8); out[i * 4 + 3] = (uint8_t)fields[i];
    }
    return 0;
}
st_native_result st_capability_read_shell_mode(int fd, uint64_t deadline, uint32_t *out) {
    if (fd < 0 || !out) return ST_PROBE_INVALID;
    if (st_capability_monotonic_ns() >= deadline) return ST_PROBE_TIMED_OUT;
    uint8_t bytes[2], extra;
    if (!st_capability_transfer(fd, bytes, sizeof(bytes), 0, deadline) ||
        !st_capability_wait_fd(fd, POLLIN, deadline) || read(fd, &extra, 1) != 0) return ST_PROBE_IO_UNAVAILABLE;
    if ((bytes[0] != '0' && bytes[0] != '1') || (bytes[1] != '0' && bytes[1] != '1')) return ST_PROBE_INVALID;
    *out = (uint32_t)(bytes[0] - '0') * 2 + (uint32_t)(bytes[1] - '0');
    return ST_PROBE_OK;
}
