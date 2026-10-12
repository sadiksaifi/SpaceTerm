#include "capability_internal.h"
#include <assert.h>
#include <stdio.h>

static unsigned url_calls, receipt_calls, automation_calls;
st_native_result st_capability_open_url(uint64_t deadline) { (void)deadline; ++url_calls; return 0; }
st_native_result st_capability_url_receipt(uint64_t deadline) { (void)deadline; ++receipt_calls; return 0; }
st_native_result st_capability_apple_event_receipt(uint64_t deadline) {
    (void)deadline; ++automation_calls; return ST_PROBE_AUTHORIZATION_DENIED;
}
static void url_receipt_is_independent_of_automation_denial(void) {
    st_capability_observation url = {1, ST_CAP_URL, ST_CONTEXT_GUI, ST_OUTCOME_UNAVAILABLE,
        ST_RESPONSIBLE_UNKNOWN, ST_PREFLIGHT_UNKNOWN, ST_EFFECT_UNOBSERVED, ST_CLEANUP_NOT_NEEDED};
    st_capability_events(&url, 1, 1);
    assert(url.outcome == ST_OUTCOME_PASSED && url.effect_class == ST_EFFECT_VERIFIED);
    assert(url_calls == 1 && receipt_calls == 1 && automation_calls == 0);
    st_capability_observation events = {1, ST_CAP_APPLE_EVENTS, ST_CONTEXT_GUI, ST_OUTCOME_UNAVAILABLE,
        ST_RESPONSIBLE_UNKNOWN, ST_PREFLIGHT_UNKNOWN, ST_EFFECT_UNOBSERVED, ST_CLEANUP_NOT_NEEDED};
    st_capability_events(&events, 1, 0);
    assert(events.outcome == ST_OUTCOME_DENIED && events.preflight_class == ST_PREFLIGHT_DENIED);
    assert(url_calls == 1 && receipt_calls == 1 && automation_calls == 1);
}
int main(void) {
    url_receipt_is_independent_of_automation_denial();
    puts("passed url_receipt_is_independent_of_automation_denial");
    puts("executed=1 proof=synthetic native-effects=0");
    return 0;
}
