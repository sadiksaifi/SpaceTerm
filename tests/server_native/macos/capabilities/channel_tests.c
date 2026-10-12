#include "capability_environment.h"
#include "capability_internal.h"
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

static pid_t creator, sources[2];
static int duplicate_holder, restored_owner, private_reads;
static int source_channel, restore_barrier[2], holder_release[2], private_witness[2];
static uint64_t source_deadline;

/* Only code policy is injected. Socket I/O and LOCAL_PEERPID use the real kernel.
 * Retain these link peers so the regression also exercises the former SCA2 path. */
int st_capability_channel_code_allowed(pid_t pid) {
    return pid == creator || pid == sources[0] || pid == sources[1];
}
static void inject_frame(int fd, uint32_t context, uint64_t deadline) {
    const char *locales[ST_CAPABILITY_LOCALE_COUNT] = {"C"};
    st_capability_retained_environment *value = NULL;
    assert(st_capability_environment_retain(context, "/synthetic/a", locales, &value) == ST_PROBE_OK);
    assert(st_capability_environment_send_private(fd, value, deadline) == ST_PROBE_OK);
    st_capability_environment_release(&value);
}
static void restore_last_owner(int fd) {
    uint8_t byte = 0;
    assert(fcntl(fd, F_SETNOSIGPIPE, 1) == 0);
    assert(write(fd, &byte, 1) == -1 && errno == EPIPE);
}
static void wait_release(int fd) {
    uint8_t byte;
    struct pollfd release = {fd, POLLIN, 0};
    assert(poll(&release, 1, 2000) == 1 && read(fd, &byte, 1) == 0);
    close(fd);
}
st_native_result st_capability_observe_environment(st_probe_context context,
    st_capability_retained_environment **out) {
    ++private_reads;
    assert(write(private_witness[1], "p", 1) == 1);
    if (duplicate_holder && context == ST_CONTEXT_GUI) {
        /* Keep both holders. Only the unapproved child sends SCE1, then shuts
         * down writing for the shared endpoint. The approved holder stays alive. */
        pid_t injector = fork(); assert(injector >= 0);
        if (!injector) {
            assert(!st_capability_channel_code_allowed(getpid()));
            close(holder_release[0]);
            inject_frame(source_channel, context, source_deadline);
            close(source_channel); _exit(0);
        }
        int status; assert(waitpid(injector, &status, 0) == injector);
        assert(WIFEXITED(status) && !WEXITSTATUS(status));
        restore_last_owner(source_channel);
        restored_owner = 1;
    } else if (duplicate_holder && context == ST_CONTEXT_DAEMON) {
        /* Delay the second frame until the approved GUI holder has restored
         * attribution. Final validation cannot win a scheduling race. */
        uint8_t byte;
        assert(st_capability_transfer(restore_barrier[0], &byte, 1, 0, source_deadline) && byte == 1);
    }
    const char *locales[ST_CAPABILITY_LOCALE_COUNT] = {"C"};
    return st_capability_environment_retain(context, "/synthetic/a", locales, out);
}
static st_native_result pair(int attack, unsigned *private_events) {
    creator = getpid(); duplicate_holder = attack; restored_owner = private_reads = 0;
    int channels[2][2];
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, channels[0]) == 0);
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, channels[1]) == 0);
    assert(pipe(private_witness) == 0);
    if (attack) assert(pipe(restore_barrier) == 0 && pipe(holder_release) == 0);
    uint8_t run[16] = {1};
    uint64_t deadline = st_capability_monotonic_ns() + UINT64_C(700000000);
    for (unsigned i = 0; i < 2; ++i) {
        pid_t child = fork(); assert(child >= 0);
        if (!child) {
            sources[i] = getpid();
            close(channels[0][0]); close(channels[1][0]); close(channels[1 - i][1]);
            close(private_witness[0]);
            source_channel = channels[i][1]; source_deadline = deadline;
            if (attack) {
                close(holder_release[1]); close(restore_barrier[i == 0 ? 0 : 1]);
                if (i != 0) close(holder_release[0]);
            }
            usleep(40000);
            st_native_result result = st_capability_environment_send_current(source_channel, i + 1, deadline, run);
            if (attack && i == 0) {
                if (restored_owner) {
                    uint8_t byte = 1;
                    assert(write(restore_barrier[1], &byte, 1) == 1);
                }
                close(restore_barrier[1]);
                /* Keep PID/start/code identity and the descriptor through the
                 * collector's final checks, even after our own write failed. */
                wait_release(holder_release[0]);
            } else if (attack) close(restore_barrier[0]);
            close(source_channel); close(private_witness[1]);
            _exit(result == ST_PROBE_OK ? 0 : 2);
        }
        sources[i] = child;
    }
    close(channels[0][1]); close(channels[1][1]); close(private_witness[1]);
    if (attack) {
        close(restore_barrier[0]); close(restore_barrier[1]); close(holder_release[0]);
    }
    st_capability_environment_difference difference = {0};
    st_native_result result = st_capability_environment_compare_channels(channels[0][0], channels[1][0],
        deadline, run, &difference);
    close(channels[0][0]); close(channels[1][0]);
    if (attack) close(holder_release[1]);
    for (unsigned i = 0; i < 2; ++i) {
        int status; assert(waitpid(sources[i], &status, 0) == sources[i]);
        assert(WIFEXITED(status) && (WEXITSTATUS(status) == 0 || WEXITSTATUS(status) == 2));
    }
    *private_events = 0;
    char byte; ssize_t count;
    while ((count = read(private_witness[0], &byte, 1)) == 1) ++*private_events;
    assert(count == 0); close(private_witness[0]);
    if (result == ST_PROBE_OK) assert(difference.path_difference == ST_PATH_EQUAL);
    else assert(difference.schema == 0);
    return result;
}

/* Independent negative control keeps exercising the native duplicate-holder
 * boundary even while the public adapter refuses before the handshake. No code
 * policy or last-owner query authenticates the synthetic values in this control. */
static void mutable_owner_control(void) {
    int channel[2], ready[2], release[2];
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, channel) == 0);
    assert(pipe(ready) == 0 && pipe(release) == 0);
    uint64_t deadline = st_capability_monotonic_ns() + UINT64_C(700000000);
    pid_t holder = fork(); assert(holder >= 0);
    if (!holder) {
        close(channel[0]); close(ready[0]); close(release[1]);
        pid_t injector = fork(); assert(injector >= 0);
        if (!injector) {
            close(release[0]); inject_frame(channel[1], ST_CONTEXT_GUI, deadline);
            close(channel[1]); close(ready[1]); _exit(0);
        }
        int status; assert(waitpid(injector, &status, 0) == injector);
        assert(WIFEXITED(status) && !WEXITSTATUS(status));
        restore_last_owner(channel[1]);
        assert(write(ready[1], "r", 1) == 1); close(ready[1]);
        wait_release(release[0]); close(channel[1]); _exit(0);
    }
    close(channel[1]); close(ready[1]); close(release[0]);
    uint8_t byte;
    assert(st_capability_transfer(ready[0], &byte, 1, 0, deadline) && byte == 'r'); close(ready[0]);
    st_capability_retained_environment *value = NULL;
    assert(st_capability_environment_receive_private(channel[0], ST_CONTEXT_GUI, deadline, &value) == ST_PROBE_OK);
    uid_t uid; gid_t gid; pid_t attributed = -1; socklen_t size = sizeof(attributed);
    assert(getpeereid(channel[0], &uid, &gid) == 0 && uid == getuid());
    assert(getsockopt(channel[0], SOL_LOCAL, LOCAL_PEERPID, &attributed, &size) == 0);
    assert(size == sizeof(attributed) && attributed == holder);
    st_capability_environment_release(&value);
    close(channel[0]); close(release[1]);
    int status; assert(waitpid(holder, &status, 0) == holder && WIFEXITED(status) && !WEXITSTATUS(status));
}
static void no_grant_or_acquisition(void) {
    int channels[2][2];
    for (unsigned i = 0; i < 2; ++i) assert(socketpair(AF_UNIX, SOCK_STREAM, 0, channels[i]) == 0);
    uint8_t run[16] = {1}, byte;
    uint64_t deadline = st_capability_monotonic_ns() + UINT64_C(100000000);
    st_capability_environment_difference sentinel = {17, 18, 19, 20, 21}, out = sentinel;
    private_reads = 0;
    assert(st_capability_environment_send_current(channels[0][1], ST_CONTEXT_GUI, deadline, run) == ST_PROBE_UNAVAILABLE);
    assert(st_capability_environment_compare_channels(channels[0][0], channels[1][0], deadline, run, &out) == ST_PROBE_UNAVAILABLE);
    assert(!memcmp(&out, &sentinel, sizeof(out)) && !private_reads);
    for (unsigned i = 0; i < 2; ++i) for (unsigned j = 0; j < 2; ++j) {
        assert(recv(channels[i][j], &byte, 1, MSG_PEEK | MSG_DONTWAIT) == -1 && errno == EAGAIN);
    }
    for (unsigned i = 0; i < 2; ++i) { close(channels[i][0]); close(channels[i][1]); }
}
int main(void) {
    alarm(10);
    unsigned private_events;
    assert(pair(1, &private_events) == ST_PROBE_UNAVAILABLE);
    assert(private_events == 0);
    puts("passed private_frame_cannot_borrow_restored_approved_holder_identity");
    mutable_owner_control();
    puts("passed socket_last_owner_can_restore_after_unapproved_frame");
    assert(pair(0, &private_events) == ST_PROBE_UNAVAILABLE && private_events == 0);
    no_grant_or_acquisition();
    puts("passed environment_channels_without_sender_binding_never_grant_or_observe_private_values");
    puts("executed=3 proof=synthetic native-effects=0");
    return 0;
}
