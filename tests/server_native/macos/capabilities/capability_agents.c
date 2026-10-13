#include "capability_agents.h"
#include "capability_internal.h"
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

static int nonzero(const uint8_t value[16]) {
    unsigned any = 0; for (size_t i = 0; i < 16; ++i) any |= value[i]; return any != 0;
}
static int valid(const st_capability_agent_session *session) {
    return session->uid == getuid() && nonzero(session->account) && nonzero(session->session) &&
        session->home[0] == '/' && memchr(session->home, 0, sizeof(session->home));
}
static int line(int fd, uint64_t deadline, int *okay) {
    uint8_t bytes[1025] = {0}; size_t used = 0;
    int result = 0;
    while (used < sizeof(bytes) - 1 && st_capability_transfer(fd, bytes + used, 1, 0, deadline)) {
        if (bytes[used++] == '\n') {
            *okay = used >= 3 && memcmp(bytes, "OK", 2) == 0 && (bytes[2] == '\n' || bytes[2] == ' ' || bytes[2] == '\r');
            result = used >= 4 && memcmp(bytes, "ERR ", 4) == 0 ? -1 : 1; break;
        }
    }
    st_capability_discard_owned(bytes, sizeof(bytes)); return result;
}
st_native_result st_capability_gpg_agent(const st_capability_agent_peer *peer,
    uint64_t deadline, st_capability_observation *out) {
    if (!peer || !peer->observe || !out) return ST_PROBE_INVALID;
    st_native_result result = ST_PROBE_UNAVAILABLE;
    st_capability_agent_session before = {0}, after = {0};
    char directory[4096] = {0}, path[sizeof(((struct sockaddr_un *)0)->sun_path)] = {0};
    int fd = -1;
    uint64_t now = st_capability_monotonic_ns();
    if (!now || now >= deadline) { result = ST_PROBE_TIMED_OUT; goto done; }
    if (deadline - now > UINT64_C(5000000000)) { result = ST_PROBE_LIMIT; goto done; }
    if (!peer->observe(peer->context, &before) || !valid(&before)) goto done;
    int length = snprintf(directory, sizeof(directory), "%s/.gnupg", before.home);
    if (length < 0 || (size_t)length >= sizeof(directory)) goto done;
    length = snprintf(path, sizeof(path), "%s/S.gpg-agent", directory);
    if (length < 0 || (size_t)length >= sizeof(path)) goto done;
    struct stat parent, socket_before, socket_after;
    if (lstat(directory, &parent) != 0 || !S_ISDIR(parent.st_mode) || parent.st_uid != before.uid ||
        parent.st_mode & (S_IWGRP | S_IWOTH) || lstat(path, &socket_before) != 0 ||
        !S_ISSOCK(socket_before.st_mode) || socket_before.st_uid != before.uid) goto done;
    fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0 || fcntl(fd, F_SETFD, FD_CLOEXEC) != 0 || fcntl(fd, F_SETFL, O_NONBLOCK) != 0) goto done;
    struct sockaddr_un address = {0}; address.sun_family = AF_UNIX; strcpy(address.sun_path, path);
    if (connect(fd, (struct sockaddr *)&address, sizeof(address)) != 0) {
        if (errno != EINPROGRESS || !st_capability_wait_fd(fd, POLLOUT, deadline)) goto done;
        int error = 0; socklen_t size = sizeof(error);
        if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &size) != 0 || error) goto done;
    }
    uid_t uid; gid_t gid;
    if (getpeereid(fd, &uid, &gid) != 0 || uid != before.uid ||
        !peer->observe(peer->context, &after) || !valid(&after) || memcmp(&before, &after, sizeof(before))) goto done;
    out->preflight_class = ST_PREFLIGHT_ALLOWED;
    int okay = 0, success = 0;
    for (unsigned i = 0; i < 4 && !okay; ++i) if (line(fd, deadline, &okay) <= 0) break;
    char query[] = "GETINFO version\n";
    if (okay && st_capability_transfer(fd, query, sizeof(query) - 1, 1, deadline)) {
        okay = 0;
        for (unsigned i = 0; i < 8 && !okay; ++i) if (line(fd, deadline, &okay) <= 0) break;
        success = okay;
    }
    if (success && peer->observe(peer->context, &after) && valid(&after) &&
        memcmp(&before, &after, sizeof(before)) == 0 && lstat(path, &socket_after) == 0 &&
        socket_before.st_dev == socket_after.st_dev && socket_before.st_ino == socket_after.st_ino &&
        st_capability_monotonic_ns() < deadline) {
        out->effect_class = ST_EFFECT_VERIFIED; out->outcome = ST_OUTCOME_PASSED; result = ST_PROBE_OK;
    }
done:
    if (fd >= 0) close(fd);
    st_capability_discard_owned(&before, sizeof(before)); st_capability_discard_owned(&after, sizeof(after));
    st_capability_discard_owned(directory, sizeof(directory)); st_capability_discard_owned(path, sizeof(path));
    if (result != ST_PROBE_OK) {
        if (st_capability_monotonic_ns() >= deadline) result = ST_PROBE_TIMED_OUT;
        out->outcome = result == ST_PROBE_TIMED_OUT ? ST_OUTCOME_TIMED_OUT : ST_OUTCOME_UNAVAILABLE;
        out->effect_class = ST_EFFECT_UNOBSERVED;
        if (out->preflight_class != ST_PREFLIGHT_ALLOWED) out->preflight_class = ST_PREFLIGHT_UNAVAILABLE;
    }
    return result;
}
