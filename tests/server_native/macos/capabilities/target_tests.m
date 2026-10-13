#include "capability_internal.h"
#include "capability_agents.h"
#include "capability_environment.h"
#import <Foundation/Foundation.h>
#import <AppKit/AppKit.h>
#include <assert.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <libproc.h>
#include <mach/mach.h>
#include <stdlib.h>
#include <sys/sysctl.h>
#include <stdio.h>
#include <string.h>
#include <signal.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <unistd.h>
#include <util.h>

static int audit_parent_acquisition, parent_procargs_calls, initial_parent_value_acquired;
int sysctl(int *name, u_int count, void *bytes, size_t *length, void *input, size_t input_length) {
    typedef int (*native_sysctl)(int *, u_int, void *, size_t *, void *, size_t);
    native_sysctl native = (native_sysctl)dlsym(RTLD_NEXT, "sysctl");
    if (!native) return -1;
    int result = native(name, count, bytes, length, input, input_length);
    if (audit_parent_acquisition && count == 3 && name[0] == CTL_KERN && name[1] == KERN_PROCARGS2) {
        ++parent_procargs_calls;
        static const char marker[] = "synthetic-only-parent-value";
        if (result == 0 && bytes && length && *length >= sizeof(marker)) {
            for (size_t i = 0; i + sizeof(marker) <= *length; ++i) {
                if (!memcmp((uint8_t *)bytes + i, marker, sizeof(marker))) initial_parent_value_acquired = 1;
            }
        }
    }
    return result;
}

typedef struct {
    mach_msg_header_t header;
    mach_msg_body_t body;
    mach_msg_ool_descriptor_t data;
} clipboard_message;
@interface STOversizedClipboard : NSObject
@property(nonatomic) BOOL materialized;
@property(nonatomic) mach_port_t source;
- (void)prepare;
@end
@implementation STOversizedClipboard
- (void)prepare {
    mach_port_t port;
    assert(mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &port) == KERN_SUCCESS);
    self.source = port;
    vm_address_t bytes = 0; vm_size_t size = 64 * 1024 * 1024;
    assert(vm_allocate(mach_task_self(), &bytes, size, VM_FLAGS_ANYWHERE) == KERN_SUCCESS);
    clipboard_message message = {0};
    message.header.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_MAKE_SEND, 0) | MACH_MSGH_BITS_COMPLEX;
    message.header.msgh_remote_port = port; message.header.msgh_size = sizeof(message);
    message.body.msgh_descriptor_count = 1;
    message.data.address = (void *)bytes; message.data.size = (mach_msg_size_t)size;
    message.data.deallocate = TRUE; message.data.copy = MACH_MSG_VIRTUAL_COPY;
    message.data.type = MACH_MSG_OOL_DESCRIPTOR;
    assert(mach_msg(&message.header, MACH_SEND_MSG, sizeof(message), 0, MACH_PORT_NULL, 0, MACH_PORT_NULL) == KERN_SUCCESS);
}
- (NSData *)dataForType:(NSString *)type {
    (void)type;
    struct { clipboard_message message; mach_msg_max_trailer_t trailer; } received = {0};
    assert(mach_msg(&received.message.header, MACH_RCV_MSG | MACH_RCV_TIMEOUT, 0, sizeof(received),
        self.source, 1000, MACH_PORT_NULL) == KERN_SUCCESS);
    self.materialized = YES;
    return [[NSData alloc] initWithBytesNoCopy:received.message.data.address length:received.message.data.size
        deallocator:^(void *bytes, NSUInteger length) { vm_deallocate(mach_task_self(), (vm_address_t)bytes, length); }];
}
@end

/* Input recognition and an isolated synthetic PTY only. Never native worker
 * execution, signing, Keychain, privacy, clipboard, sudo, or supervision APIs. */
static st_capability_targets valid_targets(void) {
    st_capability_targets targets = {0};
    targets.schema = 2;
    targets.run_id[0] = 1;
    targets.target_nonce[0] = 1;
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
static void targets_reject_unknown_schema_invalid_ids_and_traversal(void) {
    st_capability_targets targets = valid_targets();
    assert(st_capability_bind_targets(&targets) == 0);
    targets.schema = 1;
    assert(st_capability_bind_targets(&targets) == ST_PROBE_INVALID);
    targets = valid_targets(); targets.controller_pid = UINT32_MAX;
    assert(st_capability_bind_targets(&targets) == ST_PROBE_INVALID);
    targets = valid_targets(); memset(targets.target_nonce, 0, sizeof(targets.target_nonce));
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
        struct termios original, restored;
        assert(tcgetattr(slave, &original) == 0);
        int config[2], start[2], closed[2];
        assert(pipe(config) == 0 && pipe(start) == 0 && pipe(closed) == 0);
        pid_t worker = fork(); assert(worker >= 0);
        if (worker == 0) {
            close(config[1]); close(start[1]); close(closed[0]);
            assert(setpgid(0, 0) == 0);
            char byte;
            assert(read(start[0], &byte, 1) == 1);
            struct termios raw = original; cfmakeraw(&raw);
            assert(tcsetattr(slave, TCSANOW, &raw) == 0);
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
        assert(tcgetattr(slave, &restored) == 0);
        assert((original.c_lflag & ~PENDIN) == (restored.c_lflag & ~PENDIN));
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
static void native_clipboard_budget_precedes_materialization(void) {
    assert(fflush(stdout) == 0);
    pid_t child = fork(); assert(child >= 0);
    if (!child) {
        @autoreleasepool {
            STOversizedClipboard *board = [STOversizedClipboard new]; [board prepare];
            uint8_t bytes[256] = {0}; size_t length = 0;
            assert(!st_capability_clipboard_text((__bridge void *)board, bytes, sizeof(bytes), &length));
            assert(!board.materialized);
            _exit(0);
        }
    }
    int status; assert(waitpid(child, &status, 0) == child);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
}
static int shell_boundary_child(int fd) {
    assert(getenv("ST_E1_BOUNDARY_FIXTURE") == NULL);
    uint64_t deadline = st_capability_monotonic_ns() + UINT64_C(2000000000);
    assert(st_capability_bind_shell_mode(fd, deadline) == ST_PROBE_OK); close(fd);
    st_capability_environment_difference difference;
    audit_parent_acquisition = 1;
    st_native_result result = st_capability_observe_shell_environment(getppid(), ST_CONTEXT_GUI, &difference);
    audit_parent_acquisition = 0;
    /* Only a normalized status crosses the test's observation pipe. */
    return (result != ST_PROBE_OK ? 4 : 0) | (parent_procargs_calls ? 1 : 0) | (initial_parent_value_acquired ? 2 : 0);
}
static void native_shell_mode_boundary_never_acquires_parent_environment(void) {
    char executable[PROC_PIDPATHINFO_MAXSIZE];
    assert(proc_pidpath(getpid(), executable, sizeof(executable)) > 0);
    char directory[] = "/private/tmp/st-shell-boundary-XXXXXX";
    assert(mkdtemp(directory));
    char zdotdir[4096]; assert(snprintf(zdotdir, sizeof(zdotdir), "ZDOTDIR=%s", directory) > 0);
    int mode[2], result[2], master, slave;
    assert(pipe(mode) == 0 && pipe(result) == 0 && openpty(&master, &slave, NULL, NULL, NULL) == 0);
    char command[8192];
    assert(snprintf(command, sizeof(command),
        "unset ST_E1_BOUNDARY_FIXTURE; if [[ -o login && -o interactive && -z $ZSH_EXECUTION_STRING ]]; then printf 11 >&%d; else printf 00 >&%d; fi; exec %d>&-; '%s' --shell-boundary-child %d; code=$?; printf '%%s' $code >&%d; exit",
        mode[1], mode[1], mode[1], executable, mode[0], result[1]) > 0);
    char startup[4096]; assert(snprintf(startup, sizeof(startup), "%s/.zlogin", directory) > 0);
    int startup_fd = open(startup, O_CREAT | O_EXCL | O_WRONLY, 0600); assert(startup_fd >= 0);
    assert(write(startup_fd, command, strlen(command)) == (ssize_t)strlen(command)); close(startup_fd);
    assert(fflush(stdout) == 0);
    pid_t shell = fork(); assert(shell >= 0);
    if (!shell) {
        close(master); close(result[0]);
        assert(setsid() > 0 && ioctl(slave, TIOCSCTTY, 0) == 0);
        assert(dup2(slave, STDIN_FILENO) >= 0);
        int quiet = open("/dev/null", O_WRONLY);
        assert(quiet >= 0 && dup2(quiet, STDOUT_FILENO) >= 0 && dup2(quiet, STDERR_FILENO) >= 0);
        char *arguments[] = {"-zsh", "-l", "-i", NULL};
        char *environment[] = {"PATH=/usr/bin:/bin:/usr/sbin:/sbin", "LANG=C", zdotdir,
            "ST_E1_BOUNDARY_FIXTURE=synthetic-only-parent-value", NULL};
        execve("/bin/zsh", arguments, environment); _exit(126);
    }
    close(slave); close(mode[0]); close(mode[1]); close(result[1]);
    uint64_t deadline = st_capability_monotonic_ns() + UINT64_C(4000000000);
    char status_code[2] = {0};
    assert(st_capability_transfer(result[0], status_code, 1, 0, deadline));
    int status; assert(waitpid(shell, &status, 0) == shell);
    close(master); close(result[0]); assert(unlink(startup) == 0 && rmdir(directory) == 0);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(status_code[0] == '0');
}
int main(int argc, char **argv) {
    if (argc == 3 && !strcmp(argv[1], "--shell-boundary-child")) return shell_boundary_child(atoi(argv[2]));
    @autoreleasepool {
        targets_reject_unbound_companion_window();
        puts("passed targets_reject_unbound_companion_window");
        targets_reject_hidden_bytes_after_volume_terminator();
        puts("passed targets_reject_hidden_bytes_after_volume_terminator");
        targets_reject_unknown_schema_invalid_ids_and_traversal();
        puts("passed targets_reject_unknown_schema_invalid_ids_and_traversal");
        closed_configuration_pipe_restores_actual_foreground_pty();
        puts("passed closed_configuration_pipe_restores_actual_foreground_pty");
        native_responsible_process_observation_is_not_ancestry_assumption();
        puts("passed native_responsible_process_observation_is_not_ancestry_assumption");
        native_agent_account_and_session_observation_is_stable();
        puts("passed native_agent_account_and_session_observation_is_stable");
        native_clipboard_budget_precedes_materialization();
        puts("passed native_clipboard_budget_precedes_materialization");
        native_shell_mode_boundary_never_acquires_parent_environment();
        puts("passed native_shell_mode_boundary_never_acquires_parent_environment");
        puts("executed=8 proof=synthetic native-effects=0");
        return 0;
    }
}
