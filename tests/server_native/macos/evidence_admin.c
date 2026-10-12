/* Evidence command admission only. Native installation and protected receipt
 * composition are not present in the dispatch. Never turn parsing into authority. */
#include "../../../crates/spaceterm-host/native/macos/management.h"
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static int canonical_account_uid(const char *text) {
    if (text == NULL || text[0] < '1' || text[0] > '9') return 0;
    uint64_t value = 0;
    size_t length = 0;
    for (const char *cursor = text; *cursor != '\0'; cursor++) {
        if (*cursor < '0' || *cursor > '9' || ++length > 10) return 0;
        value = value * 10 + (uint64_t)(*cursor - '0');
        if (value >= UINT32_MAX) return 0;
    }
    return value != 0;
}
int main(int argc, char **argv) {
    int valid = argc == 2 && strcmp(argv[1], "uninstall") == 0;
    if (argc == 6 && strcmp(argv[1], "install") == 0
        && strcmp(argv[2], "--account-uid") == 0 && canonical_account_uid(argv[3])
        && strcmp(argv[4], "--candidate") == 0
        && strcmp(argv[5], "/private/tmp/spaceterm-native-520-candidate") == 0) valid = 1;
    if (!valid) {
        puts("classification=invalid-record"); return ST_MANAGEMENT_INVALID_RECORD;
    }
    /* No receipt, authenticated copies, account binding, or effect provider can
     * be established here. No filesystem, Keychain, launchd, or sudo call follows. */
    puts("classification=native-unavailable"); return 2;
}
