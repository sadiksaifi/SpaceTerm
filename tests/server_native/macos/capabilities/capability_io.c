#ifdef __APPLE__
#define _DARWIN_C_SOURCE
#endif
#define _POSIX_C_SOURCE 200809L
#include "capability_internal.h"
#include <errno.h>
#include <poll.h>
#include <fcntl.h>
#include <signal.h>
#include <pthread.h>
#include <sys/socket.h>
#include <unistd.h>

static int expired(uint64_t deadline) { return st_capability_monotonic_ns() >= deadline; }
int st_capability_wait_fd(int fd, short events, uint64_t deadline) {
    for (;;) {
        uint64_t now = st_capability_monotonic_ns();
        if (!now || now >= deadline) return 0;
        uint64_t remaining = deadline - now;
        int ms = (int)(remaining / UINT64_C(1000000));
        if (ms > 100) ms = 100;
        struct pollfd item = {fd, events, 0};
        int result = poll(&item, 1, ms > 0 ? ms : 1);
        if (result > 0) return (item.revents & (events | POLLHUP)) != 0;
        if (result < 0 && errno != EINTR) return 0;
    }
    return 0;
}
/* Darwin sigprocmask changes all threads. Owned descriptor suppression cannot
 * alter another thread's distinct mask or consume its pending signals. */
static ssize_t write_owned(int fd, const void *bytes, size_t size) {
#ifdef __APPLE__
    if (fcntl(fd, F_SETNOSIGPIPE, 1) != 0) {
        int enabled = 1;
        if (setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &enabled, sizeof(enabled)) != 0) return -1;
    }
    return write(fd, bytes, size);
#else
    sigset_t pipe_signal, original, pending;
    sigemptyset(&pipe_signal); sigaddset(&pipe_signal, SIGPIPE);
    if (pthread_sigmask(SIG_BLOCK, &pipe_signal, &original) != 0) return -1;
    if (sigpending(&pending) != 0) {
        (void)pthread_sigmask(SIG_SETMASK, &original, NULL);
        return -1;
    }
    int already_pending = sigismember(&pending, SIGPIPE);
    ssize_t count = write(fd, bytes, size);
    int saved = errno;
    if (count < 0 && saved == EPIPE && !already_pending &&
        sigpending(&pending) == 0 && sigismember(&pending, SIGPIPE)) {
        int received;
        if (sigwait(&pipe_signal, &received) != 0) count = -1;
    }
    if (pthread_sigmask(SIG_SETMASK, &original, NULL) != 0) return -1;
    errno = saved;
    return count;
#endif
}

int st_capability_transfer(int fd, void *bytes, size_t size, int writing, uint64_t deadline) {
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0 || fcntl(fd, F_SETFL, flags | O_NONBLOCK) != 0) return 0;
    size_t offset = 0;
    while (offset < size && !expired(deadline)) {
        if (!st_capability_wait_fd(fd, writing ? POLLOUT : POLLIN, deadline)) return 0;
        ssize_t count = writing ? write_owned(fd, (uint8_t *)bytes + offset, size - offset)
            : read(fd, (uint8_t *)bytes + offset, size - offset);
        if (count > 0) offset += (size_t)count;
        else if (count == 0 || (errno != EINTR && errno != EAGAIN)) return 0;
    }
    return offset == size;
}
