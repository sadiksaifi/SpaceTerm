#include "capability_internal.h"
#include "capability_agents.h"
#import <Foundation/Foundation.h>
#include <assert.h>
#include <stdio.h>
#include <string.h>
#include <signal.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <unistd.h>
#include <util.h>

/* Input recognition and an isolated synthetic PTY only. Never native worker
 * execution, signing, Keychain, privacy, clipboard, sudo, or supervision APIs. */
static st_capability_targets valid_targets(void) {
    st_capability_targets targets = {0};
    targets.schema = 1;
    targets.run_id[0] = 1;
    return targets;
}
static void targets_reject_unbound_companion_window(void) {
    st_capability_targets targets = valid_targets();
    targets.companion_window = 1;
    assert(st_capability_bind_targets(&targets) == ST_PROBE_INVALID);
}
static void targets_reject_hidden_bytes_after_volume_terminator(void) {
    st_capability_targets targets = valid_targets();
    targets.selected_volume[1] = 'x';
    assert(st_capability_bind_targets(&targets) == ST_PROBE_INVALID);
}
static void targets_reject_unknown_schema_reserved_and_traversal(void) {
    st_capability_targets targets = valid_targets();
    assert(st_capability_bind_targets(&targets) == 0);
    targets.schema = 2;
    assert(st_capability_bind_targets(&targets) == ST_PROBE_INVALID);
    targets = valid_targets(); targets.reserved = 1;
    assert(st_capability_bind_targets(&targets) == ST_PROBE_INVALID);
    targets = valid_targets(); memset(targets.run_id, 0, sizeof(targets.run_id));
    assert(st_capability_bind_targets(&targets) == ST_PROBE_INVALID);
    targets = valid_targets(); strcpy(targets.selected_volume, "/synthetic/../volume");
    assert(st_capability_bind_targets(&targets) == ST_PROBE_INVALID);
    targets = valid_targets(); strcpy(targets.selected_volume, "/synthetic/volume");
    assert(st_capability_bind_targets(&targets) == 0);
}
static void closed_configuration_pipe_restores_actual_foreground_pty(void) {
    int master, slave;
    assert(openpty(&master, &slave, NULL, NULL, NULL) == 0);
    assert(fflush(stdout) == 0);
    pid_t owner = fork(); assert(owner >= 0);
    if (owner == 0) {
        close(master);
        assert(setsid() > 0 && ioctl(slave, TIOCSCTTY, 0) == 0);
        assert(signal(SIGPIPE, SIG_DFL) != SIG_ERR);
        int config[2], start[2], closed[2];
        assert(pipe(config) == 0 && pipe(start) == 0 && pipe(closed) == 0);
        pid_t worker = fork(); assert(worker >= 0);
        if (worker == 0) {
            close(config[1]); close(start[1]); close(closed[0]);
            assert(setpgid(0, 0) == 0);
            char byte;
            assert(read(start[0], &byte, 1) == 1);
            close(config[0]);
            assert(write(closed[1], "r", 1) == 1);
            _exit(1);
        }
        close(config[0]); close(start[0]); close(closed[1]);
        assert(setpgid(worker, worker) == 0);
        assert(tcgetpgrp(slave) == getpgrp());
        assert(st_capability_begin_foreground(worker));
        assert(tcgetpgrp(slave) == worker);
        assert(write(start[1], "r", 1) == 1); close(start[1]);
        char byte;
        assert(read(closed[0], &byte, 1) == 1); close(closed[0]);
        assert(!st_capability_transfer(config[1], &byte, 1, 1,
            st_capability_monotonic_ns() + UINT64_C(1000000000)));
        close(config[1]);
        int status; assert(waitpid(worker, &status, 0) == worker);
        assert(st_capability_finish_spawn(worker, 1, 1) == ST_PROBE_OK);
        assert(tcgetpgrp(slave) == getpgrp());
        close(slave); _exit(0);
    }
    close(slave);
    int status; assert(waitpid(owner, &status, 0) == owner);
    close(master);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
}
static void native_responsible_process_observation_is_not_ancestry_assumption(void) {
    uint32_t observed = st_capability_responsible_process();
    assert(observed >= ST_RESPONSIBLE_DESKTOP && observed < ST_RESPONSIBLE_UNKNOWN);
}
static void native_agent_account_and_session_observation_is_stable(void) {
    st_capability_agent_session before = {0}, after = {0};
    assert(st_capability_native_agent_session(&before));
    assert(st_capability_native_agent_session(&after));
    assert(before.uid == getuid() && memcmp(&before, &after, sizeof(before)) == 0);
    st_capability_discard_owned(&before, sizeof(before));
    st_capability_discard_owned(&after, sizeof(after));
}
int main(void) {
    @autoreleasepool {
        targets_reject_unbound_companion_window();
        puts("passed targets_reject_unbound_companion_window");
        targets_reject_hidden_bytes_after_volume_terminator();
        puts("passed targets_reject_hidden_bytes_after_volume_terminator");
        targets_reject_unknown_schema_reserved_and_traversal();
        puts("passed targets_reject_unknown_schema_reserved_and_traversal");
        closed_configuration_pipe_restores_actual_foreground_pty();
        puts("passed closed_configuration_pipe_restores_actual_foreground_pty");
        native_responsible_process_observation_is_not_ancestry_assumption();
        puts("passed native_responsible_process_observation_is_not_ancestry_assumption");
        native_agent_account_and_session_observation_is_stable();
        puts("passed native_agent_account_and_session_observation_is_stable");
        puts("executed=6 proof=synthetic native-effects=0");
        return 0;
    }
}
