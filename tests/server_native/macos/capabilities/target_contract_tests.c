#include "capability_targets.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

int main(void) {
    st_companion_ledger ledger;
    st_companion_id run = {{1}}, nonce = {{2}}, received;
    st_companion_ledger_init(&ledger);
    assert(st_companion_ledger_bind(&ledger, &run));
    assert(st_companion_ledger_arm(&ledger, ST_COMPANION_TARGET_URL, &nonce, 1000, 0, 0) == ST_COMPANION_ARM_OK);
    char url[ST_COMPANION_URL_LENGTH + 1];
    assert(st_capability_target_url(run.bytes, nonce.bytes, url) == ST_PROBE_OK);
    assert(st_companion_ledger_receive_url(&ledger, url, strlen(url), 1, &received) == ST_COMPANION_RECEIPT_ACCEPTED);
    assert(st_companion_id_equal(&received, &nonce));
    assert(st_companion_ledger_receive_url(&ledger, url, strlen(url), 2, &received) == ST_COMPANION_RECEIPT_REPLAYED);
    puts("passed native_url_format_matches_the_retained_target_and_rejects_replay");
    nonce.bytes[0] = 3;
    assert(st_companion_ledger_arm(&ledger, ST_COMPANION_TARGET_ACCESSIBILITY, &nonce, 1000, 0, 0) == ST_COMPANION_ARM_OK);
    char marker[ST_COMPANION_MARKER_LENGTH + 1];
    assert(st_capability_target_marker(nonce.bytes, marker) == ST_PROBE_OK);
    assert(st_companion_ledger_receive_accessibility(&ledger, marker, strlen(marker), 1, &received) == ST_COMPANION_RECEIPT_ACCEPTED);
    assert(st_companion_id_equal(&received, &nonce));
    puts("passed native_accessibility_marker_matches_the_retained_set_only_target");
    nonce.bytes[0] = 4;
    assert(st_companion_ledger_arm(&ledger, ST_COMPANION_TARGET_CLIPBOARD, &nonce, 1000, 0, 8) == ST_COMPANION_ARM_OK);
    assert(st_capability_target_marker(nonce.bytes, marker) == ST_PROBE_OK);
    assert(st_companion_ledger_receive_clipboard(&ledger, (uint8_t *)marker, strlen(marker), 1, &received) == ST_COMPANION_RECEIPT_ACCEPTED);
    assert(st_companion_id_equal(&received, &nonce));
    puts("passed native_clipboard_marker_matches_the_retained_paste_target");
    puts("executed=3 proof=synthetic native-effects=0");
}
