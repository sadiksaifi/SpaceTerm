#define __STDC_WANT_LIB_EXT1__ 1
#define _DARWIN_C_SOURCE 1
#include "native_admission.h"
#include <limits.h>
#include <libproc.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include <sys/proc_info.h>
#include <unistd.h>

_Static_assert(sizeof(st_native_process_identity) == 16, "native process ABI size");
_Static_assert(_Alignof(st_native_process_identity) == 8, "native process ABI alignment");
_Static_assert(sizeof(st_native_pending_facts) == 312, "native facts ABI size");
_Static_assert(_Alignof(st_native_pending_facts) == 8, "native facts ABI alignment");
_Static_assert(offsetof(st_native_pending_facts, registration_generation) == 24, "generation ABI");
_Static_assert(offsetof(st_native_pending_facts, server_id) == 56, "server identity ABI");
_Static_assert(offsetof(st_native_pending_facts, original_process) == 264, "original process ABI");
_Static_assert(offsetof(st_native_pending_facts, retriever_process) == 280, "retriever process ABI");
_Static_assert(offsetof(st_native_pending_facts, journal_request) == 296, "journal ABI");

struct st_native_post_exec_proof { uint32_t unused; };

static bool nonzero(const uint8_t *bytes, size_t length) {
    uint8_t combined = 0;
    for (size_t i = 0; i < length; ++i) combined |= bytes[i];
    return combined != 0;
}
static bool valid_process(st_native_process_identity process) {
    return process.pid > 0 && process.pid <= INT32_MAX && process.start_microseconds < 1000000;
}
static bool same_process(st_native_process_identity a, st_native_process_identity b) {
    return a.pid == b.pid && a.start_seconds == b.start_seconds &&
           a.start_microseconds == b.start_microseconds;
}

st_native_result st_native_validate_pending_facts(const st_native_pending_facts *facts) {
    if (facts == NULL || facts->abi_version != ST_NATIVE_ABI_VERSION ||
        facts->struct_size != sizeof(*facts) || facts->reserved != 0 ||
        facts->uid == 0 || facts->uid == UINT32_MAX || facts->gid == UINT32_MAX ||
        facts->build < 1 || facts->build > 3 || facts->registration_generation == 0 ||
        facts->lifecycle_generation == 0 || facts->data_generation == 0 ||
        facts->installation_id == 0 || !valid_process(facts->original_process) ||
        !valid_process(facts->retriever_process) ||
        facts->original_process.pid == facts->retriever_process.pid ||
        !nonzero(facts->server_id, 16) || !nonzero(facts->account_binding, 32) ||
        !nonzero(facts->installation_binding, 32) || !nonzero(facts->server_artifact, 32) ||
        !nonzero(facts->manifest, 32) || !nonzero(facts->credential_binding, 32) ||
        !nonzero(facts->startup_nonce, 32)) return 9; /* InvalidRecord */
    return 0;
}

st_native_result st_native_observe_process(uint32_t pid, st_native_privilege_observation *out) {
    if (out == NULL) return 9;
    memset(out, 0, sizeof(*out));
    if (pid == 0 || pid > INT32_MAX) return 26;
    struct proc_bsdinfo info;
    memset(&info, 0, sizeof(info));
    if (proc_pidinfo((int)pid, PROC_PIDTBSDINFO, 0, &info, sizeof(info)) != sizeof(info) ||
        info.pbi_pid != pid || info.pbi_start_tvusec >= 1000000) {
        (void)memset_s(&info, sizeof(info), 0, sizeof(info));
        return 26;
    }
    out->process = (st_native_process_identity){ pid, (uint32_t)info.pbi_start_tvusec, info.pbi_start_tvsec };
    out->parent_pid = info.pbi_ppid;
    out->real_uid = info.pbi_ruid;
    out->effective_uid = info.pbi_uid;
    out->saved_uid = info.pbi_svuid;
    out->real_gid = info.pbi_rgid;
    out->effective_gid = info.pbi_gid;
    out->saved_gid = info.pbi_svgid;
    out->saved_ids_observed = 1;
    (void)memset_s(&info, sizeof(info), 0, sizeof(info));
    /* getgroups is a native observation only for this process. Never substitute
     * the retriever's own groups for the remote post-exec parent's groups.
     * Darwin does not expose a proved full remote supplementary-group snapshot
     * through this adapter yet. Remote admission therefore remains unavailable. */
    if (pid == (uint32_t)getpid()) {
        gid_t groups[256];
        memset(groups, 0, sizeof(groups));
        int count = getgroups(256, groups);
        if (count < 1 || count > 256) {
            (void)memset_s(groups, sizeof(groups), 0, sizeof(groups));
            (void)memset_s(out, sizeof(*out), 0, sizeof(*out));
            return 2;
        }
        out->groups_observed = 1;
        out->group_count = (uint32_t)count;
        for (int i = 0; i < count; ++i) out->groups[i] = (uint32_t)groups[i];
        (void)memset_s(groups, sizeof(groups), 0, sizeof(groups));
    }
    return 0;
}

st_native_result st_native_validate_privileges(
    const st_native_pending_facts *facts, const st_native_privilege_observation *observed) {
    st_native_result result = st_native_validate_pending_facts(facts);
    if (result != 0 || observed == NULL) return result != 0 ? result : 9;
    if (!same_process(facts->original_process, observed->process)) return 26;
    if (observed->saved_ids_observed != 1 || observed->groups_observed != 1) return 2;
    /* Darwin's native group list includes the effective GID, even after
     * setgroups(0, NULL). Only that registered primary entry may remain. */
    if (observed->group_count != 1 || observed->groups[0] != facts->gid ||
        observed->real_uid != facts->uid || observed->effective_uid != facts->uid ||
        observed->saved_uid != facts->uid || observed->real_gid != facts->gid ||
        observed->effective_gid != facts->gid || observed->saved_gid != facts->gid) return 27;
    return 0;
}

st_native_result st_native_observe_post_exec(
    const st_native_pending *pending, st_native_post_exec_proof **out_proof) {
    if (out_proof == NULL || *out_proof != NULL) return 9;
    st_native_pending_facts facts;
    st_native_privilege_observation observed;
    memset(&facts, 0, sizeof(facts));
    memset(&observed, 0, sizeof(observed));
    st_native_result result = st_native_pending_copy_facts(pending, &facts);
    if (result == 0) result = st_native_validate_pending_facts(&facts);
    if (result == 0) result = st_native_observe_process(facts.original_process.pid, &observed);
    if (result == 0) result = st_native_validate_privileges(&facts, &observed);
    /* A native privilege snapshot does not authenticate current main-executable
     * bytes, runtime protection, sealed resources or the retained closure. Until
     * the common pending owner binds those observations, no proof is issued. */
    if (result == 0) result = 28; /* ExecutionBindingUnproven */
    (void)memset_s(&facts, sizeof(facts), 0, sizeof(facts));
    (void)memset_s(&observed, sizeof(observed), 0, sizeof(observed));
    return result;
}

void st_native_post_exec_proof_release(st_native_post_exec_proof **owned) {
    if (owned == NULL || *owned == NULL) return;
    (void)memset_s(*owned, sizeof(**owned), 0, sizeof(**owned));
    free(*owned);
    *owned = NULL;
}
