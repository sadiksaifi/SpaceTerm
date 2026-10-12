#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>
#ifdef __linux__
#include <sys/prctl.h>
#endif

/* Only bounded numeric process facts leave this injected ordinary program. It never receives
 * credential bytes or native custody handles. The high descriptor is an unrelated /dev/null
 * descriptor intentionally leaked into the pre-secret exec by the test composition. */
static void report_hangup(int signal_number) {
    (void)signal_number;
    (void)write(STDOUT_FILENO, "H", 1);
}

int main(int argc, char **argv) {
    if (argc > 1 && (strcmp(argv[1], "background") == 0 || strcmp(argv[1], "background-short") == 0)) {
        pid_t leader = getpid();
        pid_t child = fork();
        if (child < 0) return 71;
        if (child > 0) return 0;
        usleep(80000);
        printf("pid=%d leader=%d\n", getpid(), leader);
        fflush(stdout);
        if (strcmp(argv[1], "background-short") == 0) { usleep(600000); return 0; }
        for (;;) pause();
    }
    if (argc > 2 && strcmp(argv[1], "linger") == 0 && strcmp(argv[2], "ignore-hup") == 0) signal(SIGHUP, SIG_IGN);
    if (argc > 2 && strcmp(argv[1], "linger") == 0 && strcmp(argv[2], "report-hup") == 0) signal(SIGHUP, report_hangup);
    if (argc > 1 && strcmp(argv[1], "job-control") == 0) {
        int gate[2];
        if (pipe(gate) != 0) return 76;
        pid_t job = fork();
        if (job < 0) return 77;
        if (job == 0) {
            close(gate[0]);
            if (setpgid(0, 0) != 0 || write(gate[1], "R", 1) != 1) return 78;
            close(gate[1]);
            for (;;) pause();
        }
        close(gate[1]);
        char ready;
        if (read(gate[0], &ready, 1) != 1) return 79;
        close(gate[0]);
        printf("job=%d\n", job);
    }
    struct stat cwd;
    struct winsize size;
    memset(&size, 0, sizeof(size));
    if (stat(".", &cwd) != 0) return 72;
    (void)ioctl(0, TIOCGWINSZ, &size);
    int no_new_privs = 0, seccomp = 0;
#ifdef __linux__
    no_new_privs = prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0);
    seccomp = prctl(PR_GET_SECCOMP, 0, 0, 0, 0);
#endif
    int open = 0;
    for (int fd = 3; fd < 1100; fd++) {
        if (fcntl(fd, F_GETFD) != -1 || errno != EBADF) open++;
    }
    printf("pid=%d parent=%d uid=%u gid=%u nnp=%d seccomp=%d open=%d inode=%llu rows=%u cols=%u\n",
           getpid(), getppid(), getuid(), getgid(), no_new_privs, seccomp, open,
           (unsigned long long)cwd.st_ino, size.ws_row, size.ws_col);
    gid_t groups[256];
    int count = getgroups(256, groups);
    if (count < 0) return 73;
    printf("groups");
    for (int index = 0; index < count; index++) printf(" %u", groups[index]);
    printf("\n");
    fflush(stdout);
    if (argc > 1 && (strcmp(argv[1], "linger") == 0 || strcmp(argv[1], "job-control") == 0)) {
        for (;;) pause();
    }
    if (argc > 1 && strcmp(argv[1], "close-stdout") == 0) {
        close(STDOUT_FILENO);
        for (;;) pause();
    }
    if (argc > 1 && strcmp(argv[1], "delayed-exit") == 0) {
        usleep(300000);
        return 0;
    }
    if (argc > 1 && strcmp(argv[1], "signal") == 0) {
        raise(SIGTERM);
        return 74;
    }
    if (argc > 1 && (unsigned char)argv[1][0] == 0xff) {
        const char *value = getenv("ORDINARY_NATIVE_BYTES");
        return value && (unsigned char)argv[1][1] == 0xfe && (unsigned char)value[0] == 0xfd ? 0 : 75;
    }
    return argc > 1 ? atoi(argv[1]) : 0;
}
