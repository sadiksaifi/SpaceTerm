#ifndef SPACETERM_CAPABILITY_TARGETS_H
#define SPACETERM_CAPABILITY_TARGETS_H
#include "capability_probe.h"
#include "../gui/companion_receipts.h"

/* Render only public synthetic IDs. These strings are not receipt or execution authority. */
st_native_result st_capability_target_marker(const uint8_t nonce[16], char out[ST_COMPANION_MARKER_LENGTH + 1]);
st_native_result st_capability_target_url(const uint8_t run[16], const uint8_t nonce[16], char out[ST_COMPANION_URL_LENGTH + 1]);
#endif
