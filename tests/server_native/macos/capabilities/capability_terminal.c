#include "capability_terminal.h"
#include "capability_internal.h"
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <util.h>

int st_capability_terminal_prepare(int owner, st_capability_terminal *out) {
    if (!out || !isatty(owner) || tcgetpgrp(owner) != getpgrp()) return 0;
    *out = (st_capability_terminal){.owner = owner, .master = -1, .slave = -1};
    struct winsize size;
    if (tcgetattr(owner, &out->original) != 0 || ioctl(owner, TIOCGWINSZ, &size) != 0 ||
        openpty(&out->master, &out->slave, NULL, &out->original, &size) != 0) return 0;
    if (fcntl(out->master, F_SETFD, FD_CLOEXEC) != 0 || fcntl(out->slave, F_SETFD, FD_CLOEXEC) != 0 ||
        fcntl(out->master, F_SETFL, O_NONBLOCK) != 0) {
        st_capability_terminal_finish(out); return 0;
    }
    out->forwarding = out->original;
    cfmakeraw(&out->forwarding);
    return 1;
}
int st_capability_terminal_child_setup(st_capability_terminal *terminal) {
    close(terminal->master);
    if (setsid() < 0 || ioctl(terminal->slave, TIOCSCTTY, 0) != 0 ||
        tcsetpgrp(terminal->slave, getpgrp()) != 0 ||
        dup2(terminal->slave, STDIN_FILENO) < 0 || dup2(terminal->slave, STDOUT_FILENO) < 0 ||
        dup2(terminal->slave, STDERR_FILENO) < 0) return 0;
    if (terminal->slave > STDERR_FILENO) close(terminal->slave);
    if (terminal->owner > STDERR_FILENO && terminal->owner != terminal->slave) close(terminal->owner);
    return 1;
}
static int forward(int source, int destination, uint64_t deadline) {
    /* This is opaque PTY forwarding, not an application authentication prompt.
     * No password parser, response object, cache, logging or AskPass exists. */
    uint8_t bytes[256];
    ssize_t count = read(source, bytes, sizeof(bytes));
    int result = count > 0 ? st_capability_transfer(destination, bytes, (size_t)count, 1, deadline) :
        count == 0 || (count < 0 && (errno == EINTR || errno == EAGAIN || errno == EIO));
    st_capability_discard_owned(bytes, sizeof(bytes));
    return result;
}
int st_capability_terminal_wait(st_capability_terminal *terminal, pid_t child, uint64_t deadline, int *status) {
    close(terminal->slave); terminal->slave = -1;
    if (tcsetattr(terminal->owner, TCSANOW, &terminal->forwarding) == 0) {
        terminal->raw = 1;
        while (st_capability_monotonic_ns() < deadline) {
            struct pollfd descriptors[] = {
                {terminal->master, POLLIN, 0}, {terminal->owner, POLLIN, 0}
            };
            if (poll(descriptors, 2, 10) < 0 && errno != EINTR) break;
            /* Drain child output before observing its exit. All data goes only
             * to the owner's terminal, never the capability record pipe. */
            if ((descriptors[0].revents & POLLIN) && !forward(terminal->master, terminal->owner, deadline)) break;
            pid_t result = waitpid(child, status, WNOHANG);
            if (result == child) return 1;
            if (result < 0 && errno != EINTR) return 0;
            if ((descriptors[1].revents & POLLIN) && !forward(terminal->owner, terminal->master, deadline)) break;
            if (descriptors[0].revents & POLLNVAL) break;
            if (descriptors[0].revents & (POLLERR | POLLHUP)) {
                struct timespec pause = {0, 1000000}; nanosleep(&pause, NULL);
            }
        }
    }
    /* An unreaped direct child's PID cannot be recycled. This attempt is not
     * descendant cleanup; the retained uniqueid tree scan remains mandatory. */
    (void)kill(child, SIGTERM);
    uint64_t retirement = st_capability_monotonic_ns() + UINT64_C(250000000);
    while (st_capability_monotonic_ns() < retirement) {
        pid_t result = waitpid(child, status, WNOHANG);
        if (result == child || (result < 0 && errno != EINTR)) break;
        struct timespec pause = {0, 1000000}; nanosleep(&pause, NULL);
    }
    return 0;
}
static int same_modes(const struct termios *left, const struct termios *right) {
    return left->c_iflag == right->c_iflag && left->c_oflag == right->c_oflag &&
        left->c_cflag == right->c_cflag && (left->c_lflag & ~PENDIN) == (right->c_lflag & ~PENDIN) &&
        cfgetispeed(left) == cfgetispeed(right) && cfgetospeed(left) == cfgetospeed(right) &&
        memcmp(left->c_cc, right->c_cc, sizeof(left->c_cc)) == 0;
}
int st_capability_terminal_restore_owner(int owner, const struct termios *original) {
    struct termios current, raw = *original;
    cfmakeraw(&raw);
    if (tcgetpgrp(owner) != getpgrp() || tcgetattr(owner, &current) != 0) return 0;
    if (same_modes(&current, original)) return 1;
    return same_modes(&current, &raw) && tcsetattr(owner, TCSANOW, original) == 0;
}
int st_capability_terminal_finish(st_capability_terminal *terminal) {
    int restored = 1;
    if (terminal->raw) {
        restored = st_capability_terminal_restore_owner(terminal->owner, &terminal->original);
    }
    if (terminal->master >= 0) close(terminal->master);
    if (terminal->slave >= 0) close(terminal->slave);
    terminal->master = terminal->slave = -1; terminal->raw = 0;
    return restored;
}
