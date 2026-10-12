#ifndef SPACETERM_MANAGEMENT_POLKIT_H
#define SPACETERM_MANAGEMENT_POLKIT_H
#include <stdint.h>

typedef struct st_management_polkit_authorization st_management_polkit_authorization;
/* The retained kernel sender supplies PID/start/UID. Only the fixed management
 * action is sent to the native system authority. No external grant is exported. */
uint32_t st_management_polkit_check(const uint8_t request[160], uint32_t native_uid,
    uint32_t native_pid, uint64_t native_start_ticks, uint64_t timeout_us,
    st_management_polkit_authorization **out);
uint32_t st_management_polkit_recheck(const st_management_polkit_authorization *authorization,
    const uint8_t request[160], uint32_t native_uid, uint32_t native_pid, uint64_t native_start_ticks);
void st_management_polkit_release(st_management_polkit_authorization **owned);
#endif
