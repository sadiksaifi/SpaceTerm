#include "capability_internal.h"

void st_capability_events(st_capability_observation *out, uint64_t deadline, int opening_url) {
    st_native_result result = ST_PROBE_OK;
    if (opening_url) result = st_capability_open_url(deadline);
    if (result == ST_PROBE_OK) result = opening_url ? st_capability_url_receipt(deadline)
        : st_capability_apple_event_receipt(deadline);
    if (result == ST_PROBE_OK) {
        out->preflight_class = ST_PREFLIGHT_ALLOWED;
        out->effect_class = ST_EFFECT_VERIFIED;
        out->outcome = ST_OUTCOME_PASSED;
    } else if (result == ST_PROBE_AUTHORIZATION_DENIED) {
        out->preflight_class = ST_PREFLIGHT_DENIED;
        out->outcome = ST_OUTCOME_DENIED;
    } else if (result == ST_PROBE_TIMED_OUT) out->outcome = ST_OUTCOME_TIMED_OUT;
    else {
        out->preflight_class = ST_PREFLIGHT_UNAVAILABLE;
        out->outcome = ST_OUTCOME_UNAVAILABLE;
    }
}
