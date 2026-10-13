#include "capability_targets.h"
#include <stdio.h>

static int id(const uint8_t value[16], char text[33]) {
    if (!value) return 0;
    static const char hex[] = "0123456789abcdef";
    unsigned nonzero = 0;
    for (size_t i = 0; i < 16; ++i) {
        nonzero |= value[i]; text[i * 2] = hex[value[i] >> 4]; text[i * 2 + 1] = hex[value[i] & 15];
    }
    text[32] = 0; return nonzero != 0;
}
st_native_result st_capability_target_marker(const uint8_t nonce[16], char out[ST_COMPANION_MARKER_LENGTH + 1]) {
    char text[33];
    if (!out || !id(nonce, text)) return ST_PROBE_INVALID;
    int size = snprintf(out, ST_COMPANION_MARKER_LENGTH + 1, ST_COMPANION_MARKER_PREFIX "%s", text);
    return size == ST_COMPANION_MARKER_LENGTH ? ST_PROBE_OK : ST_PROBE_INVALID;
}
st_native_result st_capability_target_url(const uint8_t run[16], const uint8_t nonce[16], char out[ST_COMPANION_URL_LENGTH + 1]) {
    char run_text[33], nonce_text[33];
    if (!out || !id(run, run_text) || !id(nonce, nonce_text)) return ST_PROBE_INVALID;
    int size = snprintf(out, ST_COMPANION_URL_LENGTH + 1,
        ST_COMPANION_URL_SCHEME "://receipt?run=%s&nonce=%s", run_text, nonce_text);
    return size == ST_COMPANION_URL_LENGTH ? ST_PROBE_OK : ST_PROBE_INVALID;
}
