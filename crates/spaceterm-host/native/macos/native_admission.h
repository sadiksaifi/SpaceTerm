#ifndef SPACETERM_NATIVE_MACOS_ADMISSION_H
#define SPACETERM_NATIVE_MACOS_ADMISSION_H
#include "launcher.h"

/* Comparison observations, not authority. Native callers must retain the
 * actual execution binding across observation and credential delivery. */
typedef struct {
    st_native_process_identity process;
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
#endif
