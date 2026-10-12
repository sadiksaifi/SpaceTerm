#ifndef SPACETERM_KEYCHAIN_ITEMS_H
#define SPACETERM_KEYCHAIN_ITEMS_H

#include <stdint.h>

/* Forward declarations match spaceterm_native.h. This header needs no UI or
 * framework types. Binding and permit handles have no public constructor.
 */
typedef uint32_t st_native_result;
typedef struct st_native_release_permit st_native_release_permit;
typedef struct st_native_locked_secret st_native_locked_secret;
typedef struct st_keychain_item_binding st_keychain_item_binding;

typedef struct {
    uint32_t schema;
    uint32_t policy_class;
    uint32_t interaction_class;
    uint32_t account_binding_matches;
    uint32_t code_binding_matches;
    uint32_t auxiliary_acl_class;
} st_keychain_policy_observation;

/* Inspect only attributes, persistent identity, owner, and every ACL. No secret
 * read, prompt, alternate Keychain, or caller-selected item is permitted.
 * Failure clears the observation. Facts alone do not authorize retrieval.
 */
st_native_result st_keychain_inspect_fixed_item(
    const st_keychain_item_binding *binding,
    st_keychain_policy_observation *out_observation);

/* The permit must retain the exact admitted binding and release fence. No
 * selector is accepted. Retrieval prelocks private staging memory before the
 * secret read and erases framework allocations before policy reinspection.
 * Staging is wiped before unlock on every exit. Failure wipes the destination,
 * including locking or cleanup failure. No prompt or pageable fallback exists.
 */
st_native_result st_keychain_retrieve_for_startup(
    const st_native_release_permit *permit,
    st_native_locked_secret *destination);

#endif
