/* Read-only native feasibility experiment. No sudo, TCC or credential effects.
 * A direct-child exit and an empty process group cannot authorize scope cleanup.
 * If Darwin cannot report the escaped descendant's identity, return unavailable,
 * not success. This executable is not part of the credited capability matrix. */
#include <assert.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <sys/event.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    alarm(3);
    int start[2], release[2], witness[2];
    assert(pipe(start) == 0 && pipe(release) == 0 && pipe(witness) == 0);
    pid_t child = fork(); assert(child >= 0);
    if (!child) {
        close(start[1]); close(release[1]); close(witness[0]);
        char byte; assert(read(start[0], &byte, 1) == 1); close(start[0]);
        pid_t descendant = fork(); assert(descendant >= 0);
        if (!descendant) {
            assert(setsid() > 0);
            assert(write(witness[1], "r", 1) == 1);
            (void)read(release[0], &byte, 1);
            close(release[0]); close(witness[1]); _exit(0);
        }
        close(release[0]); close(witness[1]); _exit(0);
    }
    close(start[0]); close(release[0]); close(witness[1]);
    int queue = kqueue(); assert(queue >= 0);
    struct kevent watch;
    /* The SDK states that NOTE_TRACK/NOTE_CHILD have not been supported since
     * macOS 10.5. Install before the child may fork, so polling races cannot
     * explain missing descendant evidence in this experiment. */
    EV_SET(&watch, child, EVFILT_PROC, EV_ADD | EV_ENABLE,
        NOTE_FORK | NOTE_EXIT | NOTE_TRACK, 0, NULL);
    int accepted = kevent(queue, &watch, 1, NULL, 0, NULL) == 0;
    assert(write(start[1], "g", 1) == 1); close(start[1]);
    char byte; assert(read(witness[0], &byte, 1) == 1);
    int direct_exit = 0, child_identity = 0, tracking_error = !accepted;
    for (unsigned attempt = 0; accepted && attempt < 10; ++attempt) {
        struct kevent events[16]; struct timespec bound = {0, 20000000};
        int count = kevent(queue, NULL, 0, events, 16, &bound); assert(count >= 0);
        for (int i = 0; i < count; ++i) {
            if (events[i].ident == (uintptr_t)child && (events[i].fflags & NOTE_EXIT)) direct_exit = 1;
            if (events[i].ident != (uintptr_t)child && (events[i].fflags & NOTE_CHILD)) child_identity = 1;
            if (events[i].fflags & NOTE_TRACKERR) tracking_error = 1;
        }
    }
    struct pollfd live = {witness[0], POLLIN, 0};
    int descendant_alive = poll(&live, 1, 0) == 0;
    close(release[1]);
    struct pollfd retired = {witness[0], POLLIN, 0};
    assert(poll(&retired, 1, 1000) == 1 && read(witness[0], &byte, 1) == 0);
    close(witness[0]); close(queue);
    int status; assert(waitpid(child, &status, 0) == child && WIFEXITED(status));
    printf("direct-exit-observed=%s descendant-alive=%s descendant-identity-bound=%s tracking-error=%s fixture-retired=true\n",
        direct_exit ? "true" : "false", descendant_alive ? "true" : "false",
        child_identity ? "true" : "false", tracking_error ? "true" : "false");
    return accepted && direct_exit && descendant_alive && child_identity && !tracking_error ? 0 : 2;
}
