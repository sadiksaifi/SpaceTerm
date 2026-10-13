#include "capability_process.h"
#include <assert.h>
#include <errno.h>
#include <poll.h>
#include <libproc.h>
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
static void replaced_identity(void) {
    int gate[2]; assert(pipe(gate) == 0);
    pid_t child = fork(); assert(child >= 0);
    if (!child) {
        close(gate[1]); char byte; while (read(gate[0], &byte, 1) < 0 && errno == EINTR) {}
        _exit(0);
    }
    close(gate[0]);
    st_capability_process_identity identity;
    assert(st_capability_process_observe(child, &identity));
    st_capability_process_identity mismatch = identity; ++mismatch.uniqueid;
    int unique_rejected = !st_capability_process_signal(&mismatch) && kill(child, 0) == 0;
    mismatch = identity; ++mismatch.pidversion;
    int version_rejected = !st_capability_process_signal(&mismatch) && kill(child, 0) == 0;
    close(gate[1]); int status; assert(waitpid(child, &status, 0) == child);
    assert(unique_rejected && version_rejected && WIFEXITED(status) && WEXITSTATUS(status) == 0);
}
int main(void) {
    alarm(5);
    escaped(0); puts("passed native_reparented_member_is_identity_retired");
    escaped(1); puts("passed native_triple_fork_uncertainty_is_preserved");
    struct proc_bsdshortinfo root;
    st_capability_process_identity identity;
    assert(proc_pidinfo(1, PROC_PIDT_SHORTBSDINFO, 0, &root, sizeof(root)) == sizeof(root) && root.pbsi_uid == 0);
    assert(st_capability_process_observe(1, &identity) && identity.uniqueid > 0);
    puts("passed unprivileged_process_identity_observer_reads_cross_uid_ancestry");
    replaced_identity(); puts("passed native_signal_rechecks_uniqueid_and_pidversion");
    puts("executed=4 proof=synthetic native-effects=0");
}
