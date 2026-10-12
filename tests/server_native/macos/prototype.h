#ifndef SPACETERM_MACOS_POLICY_PROTOTYPE_H
#define SPACETERM_MACOS_POLICY_PROTOTYPE_H

#include <CoreFoundation/CoreFoundation.h>
#include <Security/SecAccess.h>
#include <Security/SecRequirement.h>
#include <Security/cssmapple.h>
#include <stddef.h>
#include <stdint.h>

/*
 * Deprecation exception: the public file-based Keychain/CSSM APIs expose the
 * composite owner and ACL representation needed by this in-memory prototype.
 * The newer UID/GID owner helpers cannot represent this conjunction. No item,
 * Keychain, executable, signing identity, or service is opened by this module.
 */
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"

typedef enum {
    ST_POLICY_OK = 0,
    ST_POLICY_UNSAFE,
    ST_POLICY_NATIVE_UNAVAILABLE
} st_policy_result;

/* Fixed admission budgets, not caller-selected limits. Counts cover the owner
 * and all entries together. Depth counts the outer subject list as level one.
 * Path bytes include the sole, final NUL. Requirement bytes are serialized data.
 */
enum {
    ST_POLICY_MAX_ENTRIES = 1,
    ST_POLICY_MAX_NODES = 64,
    ST_POLICY_MAX_DEPTH = 4,
    ST_POLICY_MAX_REQUIREMENT_BYTES = 4096,
    ST_POLICY_MAX_PATH_BYTES = 1024,
    ST_POLICY_MAX_DATUM_BYTES = 16384,
    ST_POLICY_LEGACY_HASH_BYTES = 20
};

#define ST_POLICY_ENTRY_TAG "spaceterm-policy-prototype"
#define ST_POLICY_ENTRY_HANDLE ((CSSM_ACL_HANDLE)1)

typedef struct {
    const CSSM_ACL_OWNER_PROTOTYPE *owner;
    uint32_t entry_count;
    const CSSM_ACL_ENTRY_INFO *entries;
} st_policy_view;

typedef struct st_policy_snapshot st_policy_snapshot;

/*
 * All CSSM inputs are borrowed, readable native graphs, including their active
 * DATUM storage. They are data, never launch authority. Invalid pointers cannot
 * be made safe by a graph validator; callers must supply readable storage.
 * Cycles, node aliases, inconsistent links, excess budgets, and invalid shapes
 * must be rejected before invoking the public native constructor. Disjoint
 * borrowed trees may share immutable DATUM bytes. Inputs must remain unchanged
 * throughout an operation.
 *
 * CODE_SIGNATURE must contain exactly five elements: its type WORDID, OSX
 * signature-type WORDID, a 20-byte legacy-hash DATUM, a bounded NUL-terminated
 * documentary-path DATUM, and a bounded serialized-requirement DATUM. Never
 * resolve the path or use the hash as authorization. Decode the requirement
 * with SecRequirementCreateWithData and compare its serialized and canonical
 * representations with the independently supplied expected requirement.
 *
 * Construct separate owned trees for the owner and sole DECRYPT entry. Each is
 * exactly 2-of-2 PROCESS and the admitted CODE_SIGNATURE. PROCESS is version
 * CURRENT, mask MATCH_UID only, uid 0, gid 0. Both Delegate values are false.
 * The entry has only DECRYPT, empty validity dates, the fixed tag and handle.
 * Reject prompts, ANY, alternate authorizations, and requirement-less subjects.
 *
 * Success returns an owned SecAccessRef for later, separately guarded enrollment.
 * It does not prove native credential enforcement or authorize enrollment.
 * The caller releases it with CFRelease. Every failure sets *out_access to NULL.
 * A NULL output pointer is rejected without dereferencing it.
 */
st_policy_result st_policy_access_create(const CSSM_LIST *code_signature,
                                        SecRequirementRef expected_requirement,
                                        SecAccessRef *out_access);

/* Validate the entire borrowed in-memory policy, not a selected decrypt entry.
 * Exactly one entry is admitted at this stage; persisted native auxiliary ACLs
 * are out of scope. Enforce the same owner, subject, requirement, and budgets as
 * the constructor. Malformed or weakened data is UNSAFE, never a fallback.
 */
st_policy_result st_policy_validate(const st_policy_view *view,
                                   SecRequirementRef expected_requirement);

/*
 * Copy an unmodified access object returned by st_policy_access_create using
 * the public SecAccessGetOwnerAndACL representation. The caller retains the
 * access and must not modify it before or during copying. Arbitrary access
 * objects and persisted-item ACLs are outside this operation's contract.
 * Preflight checks do not establish provenance or bound framework allocation.
 * Only this operation creates snapshots. A successful snapshot owns its entire
 * graph independently of the access and every other snapshot. Every failure
 * sets *out_snapshot to NULL. NULL outputs are rejected.
 *
 * The framework allocates before returning its graph. This module cleans all
 * accessible allocations for supported inputs and its own allocation failures.
 * Framework-internal failed allocations that never reach outputs are inaccessible.
 * Fixed traversal budgets cannot completely clean an arbitrary overbound graph.
 *
 * Framework snapshot storage uses malloc/free, not CFRelease. Collect unique
 * allocation addresses before freeing anything, with fixed node, depth, entry,
 * and byte budgets. Visit only the active union, never free Tail separately, and
 * never free an entry embedded in the top-level entry array. Borrowed fixtures
 * and caller-created CSSM graphs must never be released here.
 */
st_policy_result st_policy_snapshot_copy(SecAccessRef access,
                                        st_policy_snapshot **out_snapshot);

/* A view borrows read-only storage from its snapshot until release. This
 * operation does not certify policy safety. Failure clears all view fields.
 * NULL outputs are rejected.
 */
st_policy_result st_policy_snapshot_view(const st_policy_snapshot *snapshot,
                                        st_policy_view *out_view);

/* Release only a snapshot returned by snapshot_copy; NULL is a no-op. Clear the
 * caller's handle so repeated release of that handle is safe.
 */
void st_policy_snapshot_release(st_policy_snapshot **snapshot);

#if defined(ST_POLICY_TEST_ALLOCATIONS)
/* The optional in-memory test build replaces only this module's calloc calls.
 * Security.framework and CoreFoundation keep their own allocators.
 */
void *st_policy_test_calloc(size_t count, size_t size);
#endif

#pragma clang diagnostic pop

#endif
