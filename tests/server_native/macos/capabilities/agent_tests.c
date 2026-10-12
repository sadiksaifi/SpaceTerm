#include "capability_agents.h"
#include "capability_internal.h"
#include <assert.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

static st_capability_agent_session session;
static unsigned observations;
static int invalidate, queried;
static int observe(void *unused, st_capability_agent_session *out) {
    (void)unused;
    ++observations; *out = session;
    if (invalidate && observations > 1) out->session[0] = 2;
    return 1;
}
static void *agent(void *context) {
    int listener = *(int *)context;
    struct pollfd waiting = {listener, POLLIN, 0};
    if (poll(&waiting, 1, 300) == 1) {
        int fd = accept(listener, NULL, NULL); assert(fd >= 0);
        uint64_t deadline = st_capability_monotonic_ns() + UINT64_C(1000000000);
        char greeting[] = "OK fixture\n", query[16] = {0};
        if (st_capability_transfer(fd, greeting, sizeof(greeting) - 1, 1, deadline) &&
            st_capability_transfer(fd, query, sizeof(query), 0, deadline)) {
            assert(memcmp(query, "GETINFO version\n", sizeof(query)) == 0);
            queried = 1;
            char response[] = "D synthetic\nOK\n";
            assert(st_capability_transfer(fd, response, sizeof(response) - 1, 1, deadline));
        }
        close(fd);
    }
    return NULL;
}
static void modern(int replacement) {
    char root[] = "/private/tmp/st-gpg-XXXXXX";
    assert(mkdtemp(root));
    char directory[108], socket_path[108];
    assert(snprintf(directory, sizeof(directory), "%s/.gnupg", root) > 0);
    assert(mkdir(directory, 0700) == 0);
    assert(snprintf(socket_path, sizeof(socket_path), "%s/S.gpg-agent", directory) > 0);
    int listener = socket(AF_UNIX, SOCK_STREAM, 0); assert(listener >= 0);
    struct sockaddr_un address = {0}; address.sun_family = AF_UNIX; strcpy(address.sun_path, socket_path);
    assert(bind(listener, (struct sockaddr *)&address, sizeof(address)) == 0 && listen(listener, 1) == 0);
    memset(&session, 0, sizeof(session)); session.uid = getuid();
    session.account[0] = session.session[0] = 1; strcpy(session.home, root);
    observations = queried = 0; invalidate = replacement;
    pthread_t thread; assert(pthread_create(&thread, NULL, agent, &listener) == 0);
    st_capability_agent_peer peer = {NULL, observe};
    st_capability_observation out = {1, ST_CAP_GPG_AGENT, ST_CONTEXT_GUI, ST_OUTCOME_UNAVAILABLE,
        ST_RESPONSIBLE_UNKNOWN, ST_PREFLIGHT_UNKNOWN, ST_EFFECT_UNOBSERVED, ST_CLEANUP_NOT_NEEDED};
    st_native_result result = st_capability_gpg_agent(&peer,
        st_capability_monotonic_ns() + UINT64_C(1000000000), &out);
    assert(pthread_join(thread, NULL) == 0);
    close(listener); assert(unlink(socket_path) == 0 && rmdir(directory) == 0 && rmdir(root) == 0);
    if (replacement) assert(result == ST_PROBE_UNAVAILABLE && out.outcome != ST_OUTCOME_PASSED);
    else assert(result == ST_PROBE_OK && out.outcome == ST_OUTCOME_PASSED && queried);
}
static void expired_agent_lease_performs_no_discovery_or_query(void) {
    observations = 0;
    st_capability_agent_peer peer = {NULL, observe};
    st_capability_observation out = {0};
    assert(st_capability_gpg_agent(&peer, st_capability_monotonic_ns() - 1, &out) == ST_PROBE_TIMED_OUT);
    assert(observations == 0 && out.outcome == ST_OUTCOME_TIMED_OUT);
}
int main(void) {
    modern(0); puts("passed modern_gpg_agent_is_queried_without_legacy_environment");
    modern(1); puts("passed replacement_session_refuses_modern_agent_completion");
    expired_agent_lease_performs_no_discovery_or_query();
    puts("passed expired_agent_lease_performs_no_discovery_or_query");
    puts("executed=3 proof=synthetic native-effects=0");
}
