#include "capability_terminal.h"
#include "capability_internal.h"
#include <assert.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <unistd.h>
#include <util.h>

static volatile sig_atomic_t interrupted;
static void interrupt(int signal_number) { (void)signal_number; interrupted = 1; }
static void read_marker(int fd, const char *expected, uint64_t deadline) {
    char line[128] = {0}; size_t used = 0;
    while (used + 1 < sizeof(line)) {
        assert(st_capability_transfer(fd, line + used, 1, 0, deadline));
        if (line[used++] == '\n') {
            if (strstr(line, expected)) return;
            used = 0; memset(line, 0, sizeof(line));
        }
    }
    assert(0);
}
static void isolated_sudo_preexec_has_foreground_tty_input_and_signal_delivery(void) {
    int readiness[2]; assert(pipe(readiness) == 0);
    int master, slave;
    assert(openpty(&master, &slave, NULL, NULL, NULL) == 0);
    assert(fflush(stdout) == 0);
    pid_t owner = fork(); assert(owner >= 0);
    if (!owner) {
        close(master); close(readiness[0]);
        assert(setsid() > 0 && ioctl(slave, TIOCSCTTY, 0) == 0);
        struct termios original, restored;
        assert(tcgetattr(slave, &original) == 0);
        st_capability_terminal terminal;
        assert(st_capability_terminal_prepare(slave, &terminal));
        pid_t child = fork(); assert(child >= 0);
        if (!child) {
            assert(st_capability_terminal_child_setup(&terminal));
            int tty = open("/dev/tty", O_RDWR | O_NOCTTY);
            char has_tty = tty >= 0 ? '1' : '0';
            assert(write(readiness[1], &has_tty, 1) == 1); close(readiness[1]);
            assert(tty >= 0);
            assert(getsid(0) == getpid() && tcgetpgrp(tty) == getpgrp());
            struct sigaction action = {0};
            action.sa_handler = interrupt; sigemptyset(&action.sa_mask);
            assert(sigaction(SIGINT, &action, NULL) == 0);
            assert(write(tty, "ready\n", 6) == 6);
            char input[32] = {0};
            assert(read(tty, input, sizeof(input)) == 17);
            assert(memcmp(input, "terminal-fixture\n", 17) == 0);
            assert(write(tty, "input-ok\n", 9) == 9);
            uint64_t deadline = st_capability_monotonic_ns() + UINT64_C(2000000000);
            while (!interrupted && st_capability_monotonic_ns() < deadline) usleep(1000);
            assert(interrupted);
            assert(write(tty, "signal-ok\n", 10) == 10);
            _exit(0);
        }
        close(readiness[1]);
        int status;
        assert(st_capability_terminal_wait(&terminal, child,
            st_capability_monotonic_ns() + UINT64_C(4000000000), &status));
        assert(st_capability_terminal_finish(&terminal));
        assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
        assert(tcgetattr(slave, &restored) == 0);
        assert(original.c_iflag == restored.c_iflag && original.c_oflag == restored.c_oflag &&
            (original.c_lflag & ~PENDIN) == (restored.c_lflag & ~PENDIN) && original.c_cflag == restored.c_cflag &&
            memcmp(original.c_cc, restored.c_cc, sizeof(original.c_cc)) == 0);
        assert(tcgetpgrp(slave) == getpgrp());
        _exit(0);
    }
    close(slave); close(readiness[1]);
    uint64_t deadline = st_capability_monotonic_ns() + UINT64_C(4000000000);
    char has_tty;
    assert(st_capability_transfer(readiness[0], &has_tty, 1, 0, deadline)); close(readiness[0]);
    assert(has_tty == '1');
    read_marker(master, "ready", deadline);
    assert(st_capability_transfer(master, "terminal-fixture\n", 17, 1, deadline));
    read_marker(master, "input-ok", deadline);
    char control = 3;
    assert(st_capability_transfer(master, &control, 1, 1, deadline));
    read_marker(master, "signal-ok", deadline);
    int status; assert(waitpid(owner, &status, 0) == owner);
    close(master);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
}
int main(void) {
    isolated_sudo_preexec_has_foreground_tty_input_and_signal_delivery();
    puts("passed isolated_sudo_preexec_has_foreground_tty_input_and_signal_delivery");
    puts("executed=1 proof=synthetic native-effects=0");
    return 0;
}
