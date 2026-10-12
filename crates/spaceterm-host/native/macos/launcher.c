#define __STDC_WANT_LIB_EXT1__ 1
#define _DARWIN_C_SOURCE 1
#include "native_admission.h"
#include <grp.h>
#include <string.h>
#include <unistd.h>

struct st_native_launcher_adapters { uint32_t abi; };
static const st_native_launcher_adapters native_adapters = { ST_NATIVE_ABI_VERSION };

#ifdef ST_NATIVE_TESTING
#include "launcher_test.h"
static st_launcher_test_calls test_calls;
void st_launcher_test_inject(const st_launcher_test_calls *calls) {
    memset(&test_calls, 0, sizeof(test_calls));
    if (calls != NULL) test_calls = *calls;
}
#endif

static st_native_result set_account_groups(const st_native_pending *pending, uint32_t gid) {
    uint32_t observed[256];
    gid_t groups[256];
    uint32_t count = 0;
    st_native_result result = st_native_pending_copy_account_groups(pending, observed, 256, &count);
    if (result == 0 && (count == 0 || count > 256 || observed[0] != gid)) result = 27;
    for (uint32_t i = 0; i < count && result == 0; ++i) {
        if (observed[i] == 0) result = 27;
        groups[i] = (gid_t)observed[i];
    }
    if (result == 0) {
        int changed;
#ifdef ST_NATIVE_TESTING
        if (test_calls.groups != NULL) changed = test_calls.groups((int)count, groups);
        else
#endif
        changed = setgroups((int)count, groups);
        if (changed != 0) result = 27;
    }
    (void)memset_s(observed, sizeof(observed), 0, sizeof(observed));
    (void)memset_s(groups, sizeof(groups), 0, sizeof(groups));
    return result;
}
static int set_gid(gid_t gid) {
#ifdef ST_NATIVE_TESTING
    if (test_calls.gid != NULL) return test_calls.gid(gid);
#endif
    return setgid(gid);
}
static int set_uid(uid_t uid) {
#ifdef ST_NATIVE_TESTING
    if (test_calls.uid != NULL) return test_calls.uid(uid);
#endif
    return setuid(uid);
}
static st_native_result observe(uint32_t pid, st_native_privilege_observation *out) {
#ifdef ST_NATIVE_TESTING
    if (test_calls.observe != NULL) return test_calls.observe(pid, out);
#endif
    return st_native_observe_process(pid, out);
}

const st_native_launcher_adapters *st_native_launcher_default_adapters(void) { return &native_adapters; }

st_native_result st_native_drop_account_privileges(const st_native_pending *pending) {
    st_native_pending_facts facts;
    st_native_privilege_observation observed;
    memset(&facts, 0, sizeof(facts));
    memset(&observed, 0, sizeof(observed));
    st_native_result result = st_native_pending_copy_facts(pending, &facts);
    if (result == 0) result = st_native_validate_pending_facts(&facts);
    if (result == 0 && facts.original_process.pid != (uint32_t)getpid()) result = 26;
    if (result == 0) result = st_native_observe_process(facts.original_process.pid, &observed);
    if (result == 0 && (facts.original_process.start_seconds != observed.process.start_seconds ||
        facts.original_process.start_microseconds != observed.process.start_microseconds)) result = 26;
    if (result == 0) result = set_account_groups(pending, facts.gid);
    if (result == 0 && set_gid(facts.gid) != 0) result = 27;
    if (result == 0 && set_uid(facts.uid) != 0) result = 27;
    if (result == 0) result = observe(facts.original_process.pid, &observed);
    if (result == 0) result = st_native_validate_privileges(&facts, &observed);
    (void)memset_s(&facts, sizeof(facts), 0, sizeof(facts));
    (void)memset_s(&observed, sizeof(observed), 0, sizeof(observed));
    return result;
}

st_native_result st_native_launch_one_shot(
    st_native_pending **owned_pending, const st_native_launcher_adapters *adapters) {
    if (owned_pending == NULL) return 9;
    st_native_pending *pending = *owned_pending;
    *owned_pending = NULL;
    if (pending == NULL) return 9;
    st_native_pending_facts facts;
    st_native_privilege_observation observed;
    memset(&facts, 0, sizeof(facts));
    memset(&observed, 0, sizeof(observed));
    st_native_result result = adapters == &native_adapters ? 0 : 2;
    if (result == 0) result = st_native_pending_copy_facts(pending, &facts);
    if (result == 0) result = st_native_validate_pending_facts(&facts);
    if (result == 0 && facts.original_process.pid != (uint32_t)getpid()) result = 26;
    if (result == 0) result = st_native_observe_process(facts.original_process.pid, &observed);
    if (result == 0 && (facts.original_process.start_seconds != observed.process.start_seconds ||
        facts.original_process.start_microseconds != observed.process.start_microseconds)) result = 26;
    /* Native authority has not supplied the owned execution/launch binding or
     * the C-to-Rust pending/release bridge. Keep production fail-closed rather
     * than treating comparison facts as authority to fork or execute code. The
     * receiver and guarded retriever mechanics live in native_receiver. */
    if (result == 0) result = 28;
    (void)memset_s(&facts, sizeof(facts), 0, sizeof(facts));
    (void)memset_s(&observed, sizeof(observed), 0, sizeof(observed));
    st_native_pending_release(&pending);
    return pending == NULL ? result : 33;
}
