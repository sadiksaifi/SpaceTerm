#include "capability_internal.h"
#include "capability_environment.h"

static st_native_result validate(uint64_t deadline, const uint8_t run[16]) {
    uint64_t now = st_capability_monotonic_ns();
    unsigned nonzero = 0;
    if (!run) return ST_PROBE_INVALID;
    for (size_t i = 0; i < 16; ++i) nonzero |= run[i];
    if (!nonzero) return ST_PROBE_INVALID;
    if (!now || deadline <= now) return ST_PROBE_TIMED_OUT;
    if (deadline - now > UINT64_C(5000000000)) return ST_PROBE_LIMIT;
    return ST_PROBE_OK;
}

/* Darwin LOCAL_PEERPID names the endpoint's latest I/O owner, not the sender of
 * a received frame. Another holder's read or failed write can restore an approved
 * PID after an unapproved holder supplied SCE1. SCA2, start-time/code rechecks and
 * EOF therefore cannot authorize private transfer or comparison. Refuse before
 * any handshake, grant or private acquisition until a reviewed frame-bound native
 * transport replaces this stream interface. There is no fallback or test switch. */
st_native_result st_capability_environment_send_current(int channel, st_probe_context context,
    uint64_t deadline, const uint8_t run[16]) {
    if (channel < 0 || context < 1 || context > 2) return ST_PROBE_INVALID;
    st_native_result result = validate(deadline, run);
    return result == ST_PROBE_OK ? ST_PROBE_UNAVAILABLE : result;
}

st_native_result st_capability_environment_compare_channels(int gui_channel, int daemon_channel,
    uint64_t deadline, const uint8_t run[16], st_capability_environment_difference *out) {
    if (!out || gui_channel < 0 || daemon_channel < 0 || gui_channel == daemon_channel)
        return ST_PROBE_INVALID;
    st_native_result result = validate(deadline, run);
    return result == ST_PROBE_OK ? ST_PROBE_UNAVAILABLE : result;
}
