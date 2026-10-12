#define _GNU_SOURCE 1
#define _DARWIN_C_SOURCE 1
#include "spaceterm_native.h"
#include <fcntl.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>
#ifdef __linux__
#include <sys/prctl.h>
#endif
st_native_result st_native_receiver_prepare(void);

static bool entry_case(unsigned fault) {
    pid_t child = fork();
    if (child < 0) return false;
    if (child == 0) {
        int last = 7;
#ifdef __APPLE__
        last = 8;
#endif
        /* Stabilize every source above the receiver's fixed descriptor range. */
        for (int target = 3; target <= last; ++target) {
            int pair[2];
            if (pipe(pair) != 0) _exit(1);
            bool writing = target == 4 || target == 6 || target == 8;
            if (fault == 1 && target == 5) writing = true;
            int source = fcntl(pair[writing ? 1 : 0], F_DUPFD_CLOEXEC, 20);
            (void)close(pair[0]);
            (void)close(pair[1]);
            if (source < 0 || dup2(source, target) != target) _exit(1);
            (void)close(source);
        }
        if (fault == 2) (void)close(5);
#ifdef __linux__
        if (prctl(PR_SET_DUMPABLE, fault == 3 ? 1 : 0, 0, 0, 0) != 0) _exit(1);
#endif
        st_native_result result = st_native_receiver_prepare();
        st_native_result expected = fault == 0 ? 0 : fault == 1 ? 8 : fault == 2 ? 34 : 51;
        if (result != expected) _exit(1);
        for (int fd = 3; fd <= last; ++fd) {
            int flags = fcntl(fd, F_GETFD);
            if (fault == 0 ? flags < 0 || !(flags & FD_CLOEXEC) : flags != -1) _exit(1);
        }
        _exit(0);
    }
    int status = 0;
    return waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}
int main(void) {
    const char *names[] = {"entry-protects-all-descriptors", "entry-wrong-direction-closes-all", "entry-missing-descriptor-closes-all", "entry-dumpable-one-rejects"};
    unsigned count = 3;
#ifdef __linux__
    count = 4;
#endif
    bool passed = true;
    for (unsigned i = 0; i < count; ++i) {
        bool result = entry_case(i);
        passed = passed && result;
        printf("{\"schema\":1,\"scope\":\"startup-exchange-composition\",\"case\":\"%s\",\"result\":\"%s\"}\n", names[i], result ? "pass" : "red");
    }
    return passed ? 0 : 1;
}
