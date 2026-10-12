#ifndef SPACETERM_NATIVE_MACOS_ADMISSION_H
#define SPACETERM_NATIVE_MACOS_ADMISSION_H
#include "launcher.h"

/* Comparison observations, not authority. Native callers must retain the
 * actual execution binding across observation and credential delivery. */
typedef struct {
    st_native_process_identity process;
    st_native_process_identity kernel_process;
    uint32_t parent_pid;
    uint32_t real_uid, effective_uid, saved_uid;
    uint32_t real_gid, effective_gid, saved_gid;
    uint32_t groups_observed, group_count;
    uint32_t groups[256];
    uint32_t saved_ids_observed;
} st_native_privilege_observation;

st_native_result st_native_observe_process(
    uint32_t pid, st_native_privilege_observation *out_observation);
st_native_result st_native_validate_privileges(
    const st_native_pending_facts *expected,
    const st_native_privilege_observation *observed);
st_native_result st_native_validate_pending_facts(const st_native_pending_facts *facts);

/* The authority owner supplies this retained port. It keeps authenticated main
 * code, hardened-runtime policy, the full closure and protected publication live.
 * recheck binds current execution to those objects and this exact startup, not
 * to paths, digests or receiver declarations alone. There is no default issuer. */
typedef struct st_native_execution_binding st_native_execution_binding;
/* Bound native account observation, retained by the pending issuer. Ordinary
 * Darwin account groups are preserved; no group may carry GID zero. */
st_native_result st_native_pending_copy_account_groups(
    const st_native_pending *pending, uint32_t *groups,
    uint32_t capacity, uint32_t *out_count);

st_native_result st_native_pending_retain_execution(
    const st_native_pending *pending, st_native_execution_binding **out_binding);
st_native_result st_native_execution_binding_recheck(
    const st_native_execution_binding *binding, const st_native_pending_facts *facts);
/* Copy the receiver's bounded getgroups report only after authenticating its
 * exact-startup channel and process against the retained execution binding. */
st_native_result st_native_execution_binding_copy_receiver_groups(
    const st_native_execution_binding *binding, const st_native_pending_facts *facts,
    uint32_t *groups, uint32_t capacity, uint32_t *out_count);
void st_native_execution_binding_release(st_native_execution_binding **owned_binding);

st_native_result st_native_post_exec_proof_recheck(
    const st_native_post_exec_proof *proof, const st_native_pending *pending);
/* Evidence consumes the checked self-report. This copy grants no authority. */
st_native_result st_native_post_exec_proof_copy_groups(
    const st_native_post_exec_proof *proof,
    uint32_t *groups, uint32_t capacity, uint32_t *out_count);
#endif
