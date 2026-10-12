/* The ordinary fixture uses the real item adapter algorithm with native item
 * calls replaced at compile time. This translation unit is never installed.
 */
#ifndef ST_KEYCHAIN_TESTING
#error "This fixture requires the private injected composition"
#endif
#include "../../../crates/spaceterm-host/native/macos/keychain_items.c"
