#include "capability_process.h"
#include <assert.h>
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

/* Only throwaway, unprivileged children. The released negative control is
 * retired before assertions, so a red result cannot strand an escaped child. */
static void escaped(int triple) {
    int start[2], release[2], witness[2];
    assert(pipe(start) == 0 && pipe(release) == 0 && pipe(witness) == 0);
    pid_t root = fork(); assert(root >= 0);
    if (!root) {
        close(start[1]); close(release[1]); close(witness[0]);
        char byte; assert(read(start[0], &byte, 1) == 1); close(start[0]);
        pid_t child = fork(); assert(child >= 0);
        if (!child) {
            if (triple) {
                pid_t grandchild = fork(); assert(grandchild >= 0);
                if (grandchild) _exit(0);
            }
            assert(setsid() > 0);
            pid_t pid = getpid(); assert(write(witness[1], &pid, sizeof(pid)) == sizeof(pid));
            while (read(release[0], &byte, 1) < 0 && errno == EINTR) {}
            close(release[0]); close(witness[1]); _exit(0);
        }
        if (triple) { int status; assert(waitpid(child, &status, 0) == child); }
        close(release[0]); close(witness[1]); _exit(0);
    }
    close(start[0]); close(release[0]); close(witness[1]);
    st_capability_process_identity identity;
    assert(st_capability_process_observe(root, &identity));
    assert(write(start[1], "g", 1) == 1); close(start[1]);
    pid_t escaped_pid; assert(read(witness[0], &escaped_pid, sizeof(escaped_pid)) == sizeof(escaped_pid));
    int status; assert(waitpid(root, &status, 0) == root);
    st_native_result result = st_capability_tree_retire_native(identity.uniqueid);
    int stayed_alive = kill(escaped_pid, 0) == 0;
    close(release[1]);
    char byte; struct pollfd retired = {witness[0], POLLIN | POLLHUP, 0};
    assert(poll(&retired, 1, 1000) == 1 && read(witness[0], &byte, 1) == 0); close(witness[0]);
    if (triple) assert(result == ST_PROBE_CLEANUP_UNRESOLVED && stayed_alive);
    else assert(result == ST_PROBE_OK);
}
int main(void) {
    alarm(5);
    escaped(0); puts("passed native_reparented_member_is_identity_retired");
    escaped(1); puts("passed native_triple_fork_uncertainty_is_preserved");
    puts("executed=2 proof=synthetic native-effects=0");
}
