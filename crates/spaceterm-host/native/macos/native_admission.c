#define __STDC_WANT_LIB_EXT1__ 1
#define _DARWIN_C_SOURCE 1
#include "native_admission.h"
#include <limits.h>
#include <libproc.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include <sys/param.h>
#include <sys/proc_info.h>
#include <sys/sysctl.h>
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

struct st_native_post_exec_proof {
    st_native_execution_binding *execution;
    st_native_pending_facts facts;
    uint32_t group_count;
    uint32_t groups[256];
};

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
static bool same_pending(const st_native_pending_facts *a, const st_native_pending_facts *b) {
    return a->abi_version == b->abi_version && a->struct_size == b->struct_size &&
        a->uid == b->uid && a->gid == b->gid && a->build == b->build &&
        a->reserved == b->reserved && a->registration_generation == b->registration_generation &&
        a->lifecycle_generation == b->lifecycle_generation && a->data_generation == b->data_generation &&
        a->installation_id == b->installation_id &&
        memcmp(a->server_id, b->server_id, sizeof(a->server_id)) == 0 &&
        memcmp(a->account_binding, b->account_binding, sizeof(a->account_binding)) == 0 &&
        memcmp(a->installation_binding, b->installation_binding, sizeof(a->installation_binding)) == 0 &&
        memcmp(a->server_artifact, b->server_artifact, sizeof(a->server_artifact)) == 0 &&
        memcmp(a->manifest, b->manifest, sizeof(a->manifest)) == 0 &&
        memcmp(a->credential_binding, b->credential_binding, sizeof(a->credential_binding)) == 0 &&
        memcmp(a->startup_nonce, b->startup_nonce, sizeof(a->startup_nonce)) == 0 &&
        same_process(a->original_process, b->original_process) &&
        same_process(a->retriever_process, b->retriever_process) &&
        memcmp(a->journal_request, b->journal_request, sizeof(a->journal_request)) == 0;
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
    struct kinfo_proc kernel;
    memset(&info, 0, sizeof(info));
    memset(&kernel, 0, sizeof(kernel));
    size_t length = sizeof(kernel);
    int mib[] = {CTL_KERN, KERN_PROC, KERN_PROC_PID, (int)pid};
    st_native_result result = 0;
    if (proc_pidinfo((int)pid, PROC_PIDTBSDINFO, 0, &info, sizeof(info)) != sizeof(info) ||
        info.pbi_pid != pid || info.pbi_start_tvusec >= 1000000 ||
        sysctl(mib, 4, &kernel, &length, NULL, 0) != 0 || length != sizeof(kernel) ||
        kernel.kp_proc.p_pid != (pid_t)pid || kernel.kp_proc.p_starttime.tv_sec < 0 ||
        kernel.kp_proc.p_starttime.tv_usec < 0 || kernel.kp_proc.p_starttime.tv_usec >= 1000000) {
        result = 26;
    } else {
        out->process = (st_native_process_identity){pid, (uint32_t)info.pbi_start_tvusec, info.pbi_start_tvsec};
        out->kernel_process = (st_native_process_identity){pid,
            (uint32_t)kernel.kp_proc.p_starttime.tv_usec, (uint64_t)kernel.kp_proc.p_starttime.tv_sec};
        if (!same_process(out->process, out->kernel_process)) result = 26;
        out->parent_pid = info.pbi_ppid;
        out->real_uid = info.pbi_ruid;
        out->effective_uid = info.pbi_uid;
        out->saved_uid = info.pbi_svuid;
        out->real_gid = info.pbi_rgid;
        out->effective_gid = info.pbi_gid;
        out->saved_gid = info.pbi_svgid;
        out->saved_ids_observed = 1;
        int count = kernel.kp_eproc.e_ucred.cr_ngroups;
        if (count < 1 || count > NGROUPS || count > 256) result = 2;
        else {
            out->groups_observed = 1;
            out->group_count = (uint32_t)count;
            for (int i = 0; i < count; ++i) out->groups[i] = kernel.kp_eproc.e_ucred.cr_groups[i];
        }
    }
    (void)memset_s(&info, sizeof(info), 0, sizeof(info));
    (void)memset_s(&kernel, sizeof(kernel), 0, sizeof(kernel));
    if (result != 0) (void)memset_s(out, sizeof(*out), 0, sizeof(*out));
    return result;
}

st_native_result st_native_validate_privileges(
    const st_native_pending_facts *facts, const st_native_privilege_observation *observed) {
    st_native_result result = st_native_validate_pending_facts(facts);
    if (result != 0 || observed == NULL) return result != 0 ? result : 9;
    if (!same_process(facts->original_process, observed->process) ||
        !same_process(facts->original_process, observed->kernel_process)) return 26;
    if (observed->saved_ids_observed != 1 || observed->groups_observed != 1) return 2;
    /* KERN_PROC_PID exposes at most NGROUPS entries. Decision 6 proves the
     * privilege drop, not a complete directory-membership snapshot. */
    if (facts->gid == 0 || observed->group_count < 1 || observed->group_count > 256 ||
        observed->groups[0] != facts->gid ||
        observed->real_uid != facts->uid || observed->effective_uid != facts->uid ||
        observed->saved_uid != facts->uid || observed->real_gid != facts->gid ||
        observed->effective_gid != facts->gid || observed->saved_gid != facts->gid) return 27;
    for (uint32_t i = 0; i < observed->group_count; ++i) if (observed->groups[i] == 0) return 27;
    return 0;
}

static st_native_result check_execution(const st_native_post_exec_proof *proof) {
    st_native_privilege_observation observed;
    memset(&observed, 0, sizeof(observed));
    st_native_result result = st_native_execution_binding_recheck(proof->execution, &proof->facts);
    if (result == 0) result = st_native_observe_process(proof->facts.original_process.pid, &observed);
    if (result == 0) result = st_native_validate_privileges(&proof->facts, &observed);
    if (result == 0 && (proof->group_count < 1 || proof->group_count > 256)) result = 27;
    if (result == 0) {
        for (uint32_t i = 0; i < proof->group_count; ++i) if (proof->groups[i] == 0) result = 27;
        for (uint32_t i = 0; i < observed.group_count; ++i) {
            bool found = false;
            for (uint32_t j = 0; j < proof->group_count; ++j)
                if (observed.groups[i] == proof->groups[j]) found = true;
            if (!found) result = 27;
        }
    }
    if (result == 0) result = st_native_execution_binding_recheck(proof->execution, &proof->facts);
    (void)memset_s(&observed, sizeof(observed), 0, sizeof(observed));
    return result;
}

st_native_result st_native_observe_post_exec(
    const st_native_pending *pending, st_native_post_exec_proof **out_proof) {
    if (out_proof == NULL || *out_proof != NULL) return 9;
    st_native_post_exec_proof *proof = calloc(1, sizeof(*proof));
    if (proof == NULL) return 2;
    st_native_result result = st_native_pending_copy_facts(pending, &proof->facts);
    if (result == 0) result = st_native_validate_pending_facts(&proof->facts);
    if (result == 0) result = st_native_pending_retain_execution(pending, &proof->execution);
    if (result == 0 && proof->execution == NULL) result = 28;
    if (result == 0) result = st_native_execution_binding_copy_receiver_groups(
        proof->execution, &proof->facts, proof->groups, 256, &proof->group_count);
    if (result == 0) result = check_execution(proof);
    if (result == 0) *out_proof = proof;
    else st_native_post_exec_proof_release(&proof);
    return result;
}

st_native_result st_native_post_exec_proof_recheck(
    const st_native_post_exec_proof *proof, const st_native_pending *pending) {
    if (proof == NULL || proof->execution == NULL) return 9;
    st_native_pending_facts facts;
    memset(&facts, 0, sizeof(facts));
    st_native_result result = st_native_pending_copy_facts(pending, &facts);
    if (result == 0) result = st_native_validate_pending_facts(&facts);
    if (result == 0 && !same_pending(&proof->facts, &facts)) result = 26;
    (void)memset_s(&facts, sizeof(facts), 0, sizeof(facts));
    /* check_execution only observes immutable retained state. */
    return result == 0 ? check_execution(proof) : result;
}

st_native_result st_native_post_exec_proof_copy_groups(
    const st_native_post_exec_proof *proof, uint32_t *groups, uint32_t capacity, uint32_t *count) {
    if (count == NULL) return 9;
    *count = 0;
    if (proof == NULL || groups == NULL || proof->group_count < 1 || proof->group_count > 256) return 9;
    if (capacity < proof->group_count) return 30;
    memcpy(groups, proof->groups, proof->group_count * sizeof(*groups));
    *count = proof->group_count;
    return 0;
}

void st_native_post_exec_proof_release(st_native_post_exec_proof **owned) {
    if (owned == NULL || *owned == NULL) return;
    st_native_execution_binding_release(&(*owned)->execution);
    (void)memset_s(*owned, sizeof(**owned), 0, sizeof(**owned));
    free(*owned);
    *owned = NULL;
}
