#ifndef SPACETERM_NATIVE_MACOS_LAUNCHER_H
#define SPACETERM_NATIVE_MACOS_LAUNCHER_H
#include "spaceterm_native.h"

typedef struct st_native_launcher_adapters st_native_launcher_adapters;

/* The default composition never accepts a caller-supplied path or environment. */
const st_native_launcher_adapters *st_native_launcher_default_adapters(void);
st_native_result st_native_launch_one_shot(
    st_native_pending **owned_pending,
    const st_native_launcher_adapters *adapters);
st_native_result st_native_observe_post_exec(
    const st_native_pending *pending,
    st_native_post_exec_proof **out_proof);
void st_native_post_exec_proof_release(st_native_post_exec_proof **owned_proof);

/* Common pending owner supplies this destructor. It is required for consuming
 * pending ownership even on failure. No test implementation ships in the host. */
void st_native_pending_release(st_native_pending **owned_pending);

/* Sets ordinary account groups and drops all three UID/GID identities using the retained pending
 * account, then independently checks native saved IDs. Does not execute code or
 * authorize release. Production composition may call it only after retaining
 * the authenticated execution binding and before same-PID exec. */
st_native_result st_native_drop_account_privileges(const st_native_pending *pending);
#endif
