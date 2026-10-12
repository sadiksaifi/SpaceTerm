#include "capability_internal.h"
#include "capability_environment.h"
#include <errno.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int decimal(const char *text, uint64_t *out) {
    if (!text || !text[0] || (text[0] == '0' && text[1])) return 0;
    uint64_t value = 0;
    for (size_t i = 0; text[i]; ++i) {
        if (i >= 20 || text[i] < '0' || text[i] > '9' || value > (UINT64_MAX - (unsigned)(text[i] - '0')) / 10) return 0;
        value = value * 10 + (unsigned)(text[i] - '0');
    }
    *out = value; return 1;
}
static void emit(void *context, const st_capability_observation *observation) {
    int *failed = context;
    uint8_t bytes[32];
    if (st_capability_encode(observation, bytes) || fwrite(bytes, 1, sizeof(bytes), stdout) != sizeof(bytes)) *failed = 1;
}
static int read_targets(int fd, uint64_t deadline, st_capability_targets *out) {
    uint8_t bytes[sizeof(*out) + 1]; size_t used = 0;
    while (used < sizeof(bytes) && st_capability_monotonic_ns() < deadline) {
        struct pollfd item = {fd, POLLIN, 0};
        int ready = poll(&item, 1, 1);
        if (ready < 0 && errno != EINTR) return 0;
        if (ready <= 0) continue;
        ssize_t count = read(fd, bytes + used, sizeof(bytes) - used);
        if (count > 0) used += (size_t)count;
        else if (count == 0) {
            if (used != sizeof(*out)) return 0;
            memcpy(out, bytes, sizeof(*out)); return 1;
        } else if (errno != EAGAIN && errno != EINTR) return 0;
    }
    return 0;
}
int main(int argc, char **argv) {
    /* The default has no effect and emits no host data. --worker is an internal
     * re-exec entry, not a bypass: it independently checks code and ancestry. */
    if (argc == 5 && strcmp(argv[1], "--worker") == 0) return st_capability_worker_entry(argc, argv);
    if (argc == 2 && strcmp(argv[1], "--environment") == 0) {
        st_capability_environment observation;
        int locale = getenv("LC_ALL") != NULL || getenv("LC_CTYPE") != NULL || getenv("LANG") != NULL;
        uint8_t bytes[16];
        if (st_capability_normalize_environment(getenv("PATH"), locale, &observation) ||
            st_capability_environment_encode(&observation, bytes)) return 1;
        return fwrite(bytes, 1, sizeof(bytes), stdout) == sizeof(bytes) && fflush(stdout) == 0 ? 0 : 1;
    }
    if (argc == 5 && strcmp(argv[1], "--environment-channel") == 0) {
        uint64_t context, deadline, fd;
        if (!decimal(argv[2], &context) || context < 1 || context > 2 || !decimal(argv[3], &deadline) ||
            !decimal(argv[4], &fd) || fd < 3 || fd > INT32_MAX) return 1;
        st_native_result result = st_capability_environment_send_current((int)fd, (uint32_t)context, deadline);
        close((int)fd);
        return result == ST_PROBE_OK ? 0 : 1;
    }
    if (argc == 5 && strcmp(argv[1], "--environment-pair") == 0) {
        uint64_t deadline, gui, daemon;
        if (!decimal(argv[2], &deadline) || !decimal(argv[3], &gui) || !decimal(argv[4], &daemon) ||
            gui < 3 || daemon < 3 || gui > INT32_MAX || daemon > INT32_MAX || gui == daemon) return 1;
        st_capability_environment_difference difference;
        st_native_result result = st_capability_environment_compare_channels((int)gui, (int)daemon, deadline, &difference);
        close((int)gui); close((int)daemon);
        uint8_t bytes[20];
        if (result != ST_PROBE_OK || st_capability_environment_difference_encode(&difference, bytes)) return 1;
        return fwrite(bytes, 1, sizeof(bytes), stdout) == sizeof(bytes) && fflush(stdout) == 0 ? 0 : 1;
    }
    uint64_t capability, context, deadline, fd;
    if (argc != 6 || strcmp(argv[1], "--run") || !decimal(argv[2], &capability) ||
        !decimal(argv[3], &context) || !decimal(argv[4], &deadline) || !decimal(argv[5], &fd) ||
        capability == 0 || capability > 13 || context == 0 || context > 2 || fd < 3 || fd > INT32_MAX) return 1;
    st_capability_targets targets;
    int okay = read_targets((int)fd, deadline, &targets);
    close((int)fd);
    if (!okay || st_capability_bind_targets(&targets) != 0) return 1;
    int failed = 0;
    st_native_result result = st_capability_run((uint32_t)capability, (uint32_t)context, deadline, emit, &failed);
    /* A denied/unavailable observation is still a valid outcome, not Passed.
     * Transport/cleanup errors retain their failure exit and bounded record. */
    if (fflush(stdout) != 0 || failed) return 1;
    return result == ST_PROBE_OK || result == ST_PROBE_UNAVAILABLE ? 0 : 1;
}
