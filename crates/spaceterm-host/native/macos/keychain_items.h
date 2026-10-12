#ifndef SPACETERM_KEYCHAIN_ITEMS_H
#define SPACETERM_KEYCHAIN_ITEMS_H

#include <stddef.h>
#include <stdint.h>

/* Forward declarations match spaceterm_native.h. This header needs no UI or
 * framework types. Binding and permit handles have no public constructor.
 */
typedef uint32_t st_native_result;
typedef struct st_native_release_permit st_native_release_permit;
typedef struct st_native_locked_secret st_native_locked_secret;
typedef struct st_keychain_item_binding st_keychain_item_binding;
typedef struct st_native_keychain_scope st_native_keychain_scope;

enum {
    ST_KEYCHAIN_INSPECT = 0, ST_KEYCHAIN_RELEASE = 1,
    ST_KEYCHAIN_ENROLL = 2, ST_KEYCHAIN_REMOVE = 3
};

/* The installation owner issues this borrowed scope under its transaction or
 * release fence. This snapshot is comparison data, not a scope constructor.
 * Enrollment slots are durably reserved, including after failed creation.
 * Removal scopes retain the exact protected item receipt, not a label lookup.
 */
typedef struct {
    uint32_t uid, gid, build, operation;
    uint8_t native_account[16], server_id[16], slot[32], account_binding[32];
    const uint8_t *persistent_identity;
    size_t persistent_identity_length;
} st_keychain_scope_snapshot;

/* Producer-owned callbacks. Missing linkage fails before any Keychain call.
 * copy_snapshot and recheck enforce the production evidence gate and original
 * live installation/release authority. Evidence fixtures use a separately
 * composed authorized owner, never a runtime override. The scope binds the
 * compiled retriever requirement to the protected profile and account. secret
 * borrows exactly 32 locked bytes.
 * record_identity commits the persistent reference before further effects;
 * uncertain preserves the original slot and pending receipt for operator recovery.
 * These callbacks have no caller-selected Keychain, pathname or command.
 */
st_native_result st_native_keychain_scope_copy_snapshot(
    const st_native_keychain_scope *, st_keychain_scope_snapshot *);
st_native_result st_native_keychain_scope_recheck(const st_native_keychain_scope *, uint32_t operation);
st_native_result st_native_keychain_scope_reserve_slot(const st_native_keychain_scope *);
st_native_result st_native_keychain_scope_secret(const st_native_keychain_scope *, const uint8_t **, size_t *);
st_native_result st_native_keychain_scope_record_identity(const st_native_keychain_scope *, const uint8_t *, size_t);
st_native_result st_native_keychain_scope_mark_removed(const st_native_keychain_scope *);
void st_native_keychain_scope_uncertain(const st_native_keychain_scope *);
/* These two owner issuers return owned scopes, including any partial output on
 * failure. The Rust bridge keeps common permit/lease objects opaque to C. Its
 * installation callback accepts only the actual borrowed NativeTransactionLease.
 */
st_native_result st_native_keychain_inspection_scope(const uint8_t binding_digest[32], st_native_keychain_scope **);
st_native_result st_native_keychain_installation_scope(const void *transaction_lease, uint32_t operation, st_native_keychain_scope **);
void st_native_keychain_scope_release(st_native_keychain_scope **);

/* This lookup borrows the exact scope already retained by the live permit. */
st_native_result st_native_keychain_release_scope(
    const st_native_release_permit *, const st_native_keychain_scope **);

/* Copies bounded comparison data from the owner's already-issued scope. The
 * scope remains borrowed and live until release_binding; no facts can issue it.
 */
st_native_result st_keychain_retain_scope(const st_native_keychain_scope *, st_keychain_item_binding **);
void st_keychain_release_binding(st_keychain_item_binding **);

/* Enrollment never overwrites an existing item or reuses a reserved slot.
 * Removal resolves the receipt's persistent reference and retains that native
 * item through deletion. Partial enrollment keeps its exact protected identity.
 */
st_native_result st_keychain_enroll_owned_item(st_keychain_item_binding *);
st_native_result st_keychain_remove_owned_item(const st_keychain_item_binding *);

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
