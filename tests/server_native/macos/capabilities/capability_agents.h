#ifndef SPACETERM_CAPABILITY_AGENTS_H
#define SPACETERM_CAPABILITY_AGENTS_H
#include "capability_probe.h"
#include <sys/types.h>

/* Private, native observations. Neither paths nor bindings enter evidence. */
typedef struct {
    uid_t uid;
    uint8_t account[16], session[16];
    char home[4096];
} st_capability_agent_session;
typedef struct {
    void *context;
    int (*observe)(void *, st_capability_agent_session *);
} st_capability_agent_peer;
st_native_result st_capability_gpg_agent(const st_capability_agent_peer *,
    uint64_t deadline_ns, st_capability_observation *);
/* Native read-only account/session observation, with no agent or privacy effects. */
int st_capability_native_agent_session(st_capability_agent_session *);
#endif
