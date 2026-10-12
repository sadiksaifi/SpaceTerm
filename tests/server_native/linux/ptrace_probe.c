#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/prctl.h>
#include <sys/ptrace.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

int main(int argc, char **argv) {
    char *end = NULL;
    if (argc != 2 || getuid() == 0) {
        return 2;
    }
    long target = strtol(argv[1], &end, 10);
    if (*end != '\0' || target <= 1 || target > 2147483647) {
        return 2;
    }
    int ready[2];
    if (pipe(ready) != 0) {
        return 2;
    }
    pid_t control = fork();
    if (control == 0) {
        close(ready[0]);
        if (prctl(PR_SET_DUMPABLE, 1, 0, 0, 0) != 0 || write(ready[1], "1", 1) != 1) {
            _exit(2);
        }
        close(ready[1]);
        for (;;) {
            pause();
        }
    }
    close(ready[1]);
    char byte;
    int status;
    int passed = control > 0 && read(ready[0], &byte, 1) == 1 &&
                 ptrace(PTRACE_ATTACH, control, NULL, NULL) == 0 &&
                 waitpid(control, &status, 0) == control && WIFSTOPPED(status) &&
                 ptrace(PTRACE_DETACH, control, NULL, NULL) == 0;
    close(ready[0]);
    if (control > 0) {
        kill(control, SIGKILL);
        waitpid(control, &status, 0);
    }
    if (!passed) {
        puts("scope=linux-launcher-prototype case=ptrace-control result=failed");
        return 1;
    }
    errno = 0;
    long result = ptrace(PTRACE_ATTACH, (pid_t)target, NULL, NULL);
    int denied = result == -1 && errno == EPERM;
    if (result == 0) {
        waitpid((pid_t)target, &status, 0);
        ptrace(PTRACE_DETACH, (pid_t)target, NULL, NULL);
    }
    printf("scope=linux-launcher-prototype case=ptrace-control result=passed\n");
    printf("scope=linux-launcher-prototype case=ptrace-server result=%s\n", denied ? "denied" : "failed");
    return denied ? 0 : 1;
}
