#include "keychain_items.h"

#include <CoreFoundation/CoreFoundation.h>
#include <Security/Security.h>
#include <Security/cssmapple.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

/* The common native boundary owns these opaque handles. */
extern st_native_result st_native_release_permit_recheck(const st_native_release_permit *permit) __attribute__((weak_import));
extern st_native_result st_native_locked_secret_write(st_native_locked_secret *destination,
                                                      const uint8_t *bytes, size_t length) __attribute__((weak_import));
extern void st_native_locked_secret_wipe(st_native_locked_secret *destination) __attribute__((weak_import));
extern st_native_result st_native_keychain_release_scope(const st_native_release_permit *, const st_native_keychain_scope **) __attribute__((weak_import));

/* File-based Keychain APIs expose the complete composite owner/ACL subjects.
 * SecAccessControl does not represent root UID AND the pinned code requirement.
 * This module never treats an in-memory access object as persisted authority.
 */
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"

#if defined(ST_KEYCHAIN_TESTING)
#include "../../../../tests/server_native/macos/keychain_items.h"
typedef st_keychain_test_binding_facts binding_facts;
typedef st_keychain_test_attributes item_attributes;
typedef st_keychain_test_calls item_calls;
#else
typedef st_keychain_scope_snapshot binding_facts;
typedef struct {
    uint32_t item_class;
    uint8_t account[64], generic[112];
    const uint8_t *service; size_t service_length;
    const uint8_t *label; size_t label_length;
    const uint8_t *persistent_identity; size_t persistent_identity_length;
    void *native_attributes;
    CFDataRef native_identity;
} item_attributes;
typedef struct {
    st_native_result (*guard)(void *);
    st_native_result (*disable_interaction)(void *);
    st_native_result (*open_system)(void *, void **);
    st_native_result (*resolve_item)(void *, void *, const uint8_t *, size_t, void **);
    st_native_result (*copy_attributes)(void *, void *, item_attributes *);
    void (*release_attributes)(void *, item_attributes *);
    st_native_result (*copy_access)(void *, void *, SecAccessRef *);
    st_native_result (*copy_secret)(void *, void *, uint8_t **, size_t *);
    st_native_result (*release_secret)(void *, uint8_t *, size_t);
    void (*release_item)(void *, void *);
    void (*release_keychain)(void *, void *);
    st_native_result (*recheck_permit)(void *, const st_native_release_permit *);
    st_native_result (*write_secret)(void *, st_native_locked_secret *, const uint8_t *, size_t);
    void (*wipe_destination)(void *, st_native_locked_secret *);
    st_native_result (*lock_staging)(void *, uint8_t *, size_t);
    st_native_result (*unlock_staging)(void *, uint8_t *, size_t);
    st_native_result (*collision)(void *, void *, const uint8_t *, size_t, const uint8_t *, size_t);
    st_native_result (*reserve_slot)(void *);
    st_native_result (*enrollment_secret)(void *, const uint8_t **, size_t *);
    st_native_result (*create_item)(void *, void *, const item_attributes *, SecAccessRef,
                                    const uint8_t *, size_t, void **);
    st_native_result (*record_identity)(void *, const uint8_t *, size_t);
    st_native_result (*delete_item)(void *, void *);
    st_native_result (*mark_removed)(void *);
    void (*uncertain)(void *);
} item_calls;
#endif

enum {
    KC_OK = 0, KC_NATIVE_UNAVAILABLE = 2, KC_INVALID_RECORD = 9,
    KC_IDENTITY_MISMATCH = 12, KC_CREDENTIAL_UNAVAILABLE = 21,
    KC_POLICY_REJECTED = 22, KC_INTERACTION_REQUIRED = 23,
    KC_CLEANUP_UNRESOLVED = 33,
    KC_ATTRIBUTE_BYTES = 112, KC_PERSISTENT_BYTES = 1024, KC_SECRET_BYTES = 32
};

typedef enum { ST_POLICY_OK, ST_POLICY_UNSAFE, ST_POLICY_NATIVE_UNAVAILABLE } st_policy_result;
enum {
    ST_POLICY_MAX_ENTRIES = 1, ST_POLICY_MAX_NODES = 64, ST_POLICY_MAX_DEPTH = 4,
    ST_POLICY_MAX_REQUIREMENT_BYTES = 4096, ST_POLICY_MAX_PATH_BYTES = 1024,
    ST_POLICY_MAX_DATUM_BYTES = 16384, ST_POLICY_LEGACY_HASH_BYTES = 20,
    ST_POLICY_MAX_ALLOCATIONS = 2 * ST_POLICY_MAX_NODES + 3 * ST_POLICY_MAX_ENTRIES + 2
};
#define ST_POLICY_ENTRY_TAG "spaceterm-policy-prototype"
#define ST_POLICY_ENTRY_HANDLE ((CSSM_ACL_HANDLE)1)

typedef struct {
    const CSSM_ACL_OWNER_PROTOTYPE *owner;
    uint32_t entry_count;
    const CSSM_ACL_ENTRY_INFO *entries;
} st_policy_view;
typedef struct {
    const CSSM_LIST_ELEMENT *nodes[ST_POLICY_MAX_NODES];
    size_t node_count, datum_bytes;
} graph_budget;
typedef struct { const CSSM_DATA *hash, *path, *requirement; } signature_parts;
typedef struct {
    CSSM_ACL_OWNER_PROTOTYPE *owner;
    uint32_t entry_count;
    CSSM_ACL_ENTRY_INFO *entries;
    void *allocations[ST_POLICY_MAX_ALLOCATIONS];
    size_t allocation_count;
} st_policy_snapshot;

_Static_assert(sizeof(st_keychain_policy_observation) == 24, "policy observation ABI size");
_Static_assert(_Alignof(st_keychain_policy_observation) == 4, "policy observation ABI alignment");
_Static_assert(offsetof(st_keychain_policy_observation, auxiliary_acl_class) == 20, "policy observation ABI offset");

/* These graph rules intentionally match the in-memory constructor, including
 * its one-entry limit. Native auxiliary entries are NOT silently admitted.
 * Persisted policies with any additional ACL require separately established
 * semantics and signed evidence before this limit can change.
 */
static bool graph_list(graph_budget *budget, const CSSM_LIST *list, size_t depth)
{
    if (list == NULL || depth > ST_POLICY_MAX_DEPTH ||
        list->ListType != CSSM_LIST_TYPE_UNKNOWN ||
        (list->Head == NULL) != (list->Tail == NULL)) return false;
    const CSSM_LIST_ELEMENT *last = NULL;
    for (const CSSM_LIST_ELEMENT *node = list->Head; node != NULL;
         node = node->NextElement) {
        if (budget->node_count == ST_POLICY_MAX_NODES) return false;
        for (size_t i = 0; i < budget->node_count; ++i) {
            if (budget->nodes[i] == node) return false;
        }
        budget->nodes[budget->node_count++] = node;
        switch (node->ElementType) {
        case CSSM_LIST_ELEMENT_WORDID:
            /* WORDID has no active union member. Native snapshots need not
             * initialize the bytes of that inactive union.
             */
            break;
        case CSSM_LIST_ELEMENT_DATUM: {
            const CSSM_DATA *data = &node->Element.Word;
            if (data->Length == 0 || data->Data == NULL ||
                data->Length > ST_POLICY_MAX_DATUM_BYTES - budget->datum_bytes) return false;
            budget->datum_bytes += data->Length;
            break;
        }
        case CSSM_LIST_ELEMENT_SUBLIST:
            if (!graph_list(budget, &node->Element.Sublist, depth + 1)) return false;
            break;
        default:
            return false;
        }
        last = node;
    }
    return list->Tail == last;
}

/* Called only after the whole graph has passed its bounds and link checks. */
static bool list_nodes(const CSSM_LIST *list, size_t count,
                       const CSSM_LIST_ELEMENT **nodes)
{
    const CSSM_LIST_ELEMENT *node = list->Head;
    for (size_t i = 0; i < count; ++i) {
        if (node == NULL) return false;
        nodes[i] = node;
        node = node->NextElement;
    }
    return node == NULL && list->Tail == nodes[count - 1];
}

static bool is_word(const CSSM_LIST_ELEMENT *node, CSSM_WORDID_TYPE value)
{
    return node->ElementType == CSSM_LIST_ELEMENT_WORDID && node->WordID == value;
}

static bool signature_shape(const CSSM_LIST *list, signature_parts *parts)
{
    const CSSM_LIST_ELEMENT *nodes[5];
    if (!list_nodes(list, 5, nodes) ||
        !is_word(nodes[0], CSSM_ACL_SUBJECT_TYPE_CODE_SIGNATURE) ||
        !is_word(nodes[1], CSSM_ACL_CODE_SIGNATURE_OSX) ||
        nodes[2]->ElementType != CSSM_LIST_ELEMENT_DATUM ||
        nodes[3]->ElementType != CSSM_LIST_ELEMENT_DATUM ||
        nodes[4]->ElementType != CSSM_LIST_ELEMENT_DATUM) return false;
    *parts = (signature_parts){
        .hash = &nodes[2]->Element.Word,
        .path = &nodes[3]->Element.Word,
        .requirement = &nodes[4]->Element.Word
    };
    if (parts->hash->Length != ST_POLICY_LEGACY_HASH_BYTES ||
        parts->path->Length == 0 || parts->path->Length > ST_POLICY_MAX_PATH_BYTES ||
        parts->requirement->Length == 0 ||
        parts->requirement->Length > ST_POLICY_MAX_REQUIREMENT_BYTES) return false;
    /* The path is documentary bytes. Require its first NUL to be its last byte;
     * never resolve it, and never authorize through the legacy hash.
     */
    return memchr(parts->path->Data, 0, parts->path->Length) ==
           parts->path->Data + parts->path->Length - 1;
}

static bool process_shape(const CSSM_LIST *list)
{
    const CSSM_LIST_ELEMENT *nodes[2];
    if (!list_nodes(list, 2, nodes) ||
        !is_word(nodes[0], CSSM_ACL_SUBJECT_TYPE_PROCESS) ||
        nodes[1]->ElementType != CSSM_LIST_ELEMENT_DATUM ||
        nodes[1]->Element.Word.Length != sizeof(CSSM_ACL_PROCESS_SUBJECT_SELECTOR)) return false;
    CSSM_ACL_PROCESS_SUBJECT_SELECTOR selector;
    /* Borrowed DATUM bytes need not have the selector's alignment. */
    memcpy(&selector, nodes[1]->Element.Word.Data, sizeof selector);
    return selector.version == CSSM_ACL_PROCESS_SELECTOR_CURRENT_VERSION &&
           selector.mask == CSSM_ACL_MATCH_UID && selector.uid == 0 && selector.gid == 0;
}

static bool threshold_shape(const CSSM_LIST *list, signature_parts *parts)
{
    const CSSM_LIST_ELEMENT *nodes[5];
    return list_nodes(list, 5, nodes) &&
           is_word(nodes[0], CSSM_ACL_SUBJECT_TYPE_THRESHOLD) &&
           is_word(nodes[1], 2) && is_word(nodes[2], 2) &&
           nodes[3]->ElementType == CSSM_LIST_ELEMENT_SUBLIST &&
           nodes[4]->ElementType == CSSM_LIST_ELEMENT_SUBLIST &&
           process_shape(&nodes[3]->Element.Sublist) &&
           signature_shape(&nodes[4]->Element.Sublist, parts);
}

static st_policy_result requirement_matches(const CSSM_DATA *candidate,
                                            SecRequirementRef expected)
{
    if (expected == NULL || CFGetTypeID(expected) != SecRequirementGetTypeID()) return ST_POLICY_UNSAFE;
    CFDataRef input = NULL, expected_data = NULL, decoded_data = NULL;
    CFStringRef expected_text = NULL, decoded_text = NULL;
    SecRequirementRef decoded = NULL;
    st_policy_result result = ST_POLICY_NATIVE_UNAVAILABLE;
    OSStatus status = SecRequirementCopyData(expected, kSecCSDefaultFlags, &expected_data);
    if (status != errSecSuccess || expected_data == NULL) goto cleanup;
    CFIndex expected_length = CFDataGetLength(expected_data);
    if (expected_length <= 0 || expected_length > ST_POLICY_MAX_REQUIREMENT_BYTES) {
        result = ST_POLICY_UNSAFE;
        goto cleanup;
    }
    input = CFDataCreate(kCFAllocatorDefault, candidate->Data, (CFIndex)candidate->Length);
    if (input == NULL) goto cleanup;
    status = SecRequirementCreateWithData(input, kSecCSDefaultFlags, &decoded);
    if (status != errSecSuccess || decoded == NULL) {
        result = status == errSecAllocate ? ST_POLICY_NATIVE_UNAVAILABLE : ST_POLICY_UNSAFE;
        goto cleanup;
    }
    status = SecRequirementCopyData(decoded, kSecCSDefaultFlags, &decoded_data);
    if (status != errSecSuccess || decoded_data == NULL) goto cleanup;
    status = SecRequirementCopyString(expected, kSecCSDefaultFlags, &expected_text);
    if (status != errSecSuccess || expected_text == NULL) goto cleanup;
    status = SecRequirementCopyString(decoded, kSecCSDefaultFlags, &decoded_text);
    if (status != errSecSuccess || decoded_text == NULL) goto cleanup;
    /* Compare both the original bytes and their decoded form against the
     * independently supplied expectation. Parsing must not normalize extra
     * serialized bytes into an accepted requirement.
     */
    result = CFEqual(input, expected_data) && CFEqual(decoded_data, expected_data) &&
             CFEqual(decoded_text, expected_text) ? ST_POLICY_OK : ST_POLICY_UNSAFE;
cleanup:
    if (decoded_text != NULL) CFRelease(decoded_text);
    if (expected_text != NULL) CFRelease(expected_text);
    if (decoded_data != NULL) CFRelease(decoded_data);
    if (decoded != NULL) CFRelease(decoded);
    if (expected_data != NULL) CFRelease(expected_data);
    if (input != NULL) CFRelease(input);
    return result;
}

static st_policy_result policy_validate(const st_policy_view *view,
                                   SecRequirementRef expected_requirement)
{
    static const CSSM_STRING entry_tag = ST_POLICY_ENTRY_TAG;
    if (view == NULL || view->owner == NULL || view->entry_count != ST_POLICY_MAX_ENTRIES ||
        view->entries == NULL || expected_requirement == NULL) return ST_POLICY_UNSAFE;
    const CSSM_ACL_ENTRY_INFO *entry = &view->entries[0];
    const CSSM_ACL_ENTRY_PROTOTYPE *policy = &entry->EntryPublicInfo;
    if (view->owner->Delegate != CSSM_FALSE || policy->Delegate != CSSM_FALSE ||
        entry->EntryHandle != ST_POLICY_ENTRY_HANDLE ||
        memcmp(policy->EntryTag, entry_tag, sizeof entry_tag) != 0 ||
        policy->Authorization.NumberOfAuthTags != 1 || policy->Authorization.AuthTags == NULL ||
        policy->Authorization.AuthTags[0] != CSSM_ACL_AUTHORIZATION_DECRYPT ||
        policy->TimeRange.StartDate.Length != 0 || policy->TimeRange.StartDate.Data != NULL ||
        policy->TimeRange.EndDate.Length != 0 || policy->TimeRange.EndDate.Data != NULL) return ST_POLICY_UNSAFE;
    graph_budget budget = {0};
    signature_parts owner, decrypt;
    if (!graph_list(&budget, &view->owner->TypedSubject, 1) ||
        !graph_list(&budget, &policy->TypedSubject, 1) ||
        !threshold_shape(&view->owner->TypedSubject, &owner) ||
        !threshold_shape(&policy->TypedSubject, &decrypt)) return ST_POLICY_UNSAFE;
    st_policy_result result = requirement_matches(owner.requirement, expected_requirement);
    return result == ST_POLICY_OK ? requirement_matches(decrypt.requirement, expected_requirement) : result;
}

static bool allocation_add(st_policy_snapshot *snapshot, void *allocation)
{
    if (allocation == NULL) return true;
    for (size_t i = 0; i < snapshot->allocation_count; ++i) {
        if (snapshot->allocations[i] == allocation) return true;
    }
    if (snapshot->allocation_count == ST_POLICY_MAX_ALLOCATIONS) return false;
    snapshot->allocations[snapshot->allocation_count++] = allocation;
    return true;
}

/* This collector is only for framework copy-outs, never borrowed input. Apple
 * copyOwnerAndAcl uses Allocator::standard(), whose normal allocator is
 * malloc/free. Its ChunkCopyWalker separately allocates each node and active
 * DATUM, and relinks Tail without allocating it. No allocator-size queries or
 * assumptions about inactive union bytes are needed.
 */
static bool snapshot_collect(st_policy_snapshot *snapshot)
{
    typedef struct {
        CSSM_LIST list;
        size_t depth;
    } pending_list;
    pending_list pending[ST_POLICY_MAX_NODES + 2];
    size_t pending_count = 0;
    graph_budget budget = {0};
    bool bounded = true;
    (void)allocation_add(snapshot, snapshot->owner);
    (void)allocation_add(snapshot, snapshot->entries);
    if (snapshot->owner != NULL) {
        pending[pending_count++] = (pending_list){snapshot->owner->TypedSubject, 1};
    }
    if (snapshot->entry_count > ST_POLICY_MAX_ENTRIES ||
        (snapshot->entry_count != 0 && snapshot->entries == NULL)) bounded = false;
    size_t entries = snapshot->entries == NULL ? 0 : snapshot->entry_count;
    if (entries > ST_POLICY_MAX_ENTRIES) entries = ST_POLICY_MAX_ENTRIES;
    for (size_t i = 0; i < entries; ++i) {
        CSSM_ACL_ENTRY_PROTOTYPE *entry = &snapshot->entries[i].EntryPublicInfo;
        bounded = allocation_add(snapshot, entry->Authorization.AuthTags) && bounded;
        bounded = allocation_add(snapshot, entry->TimeRange.StartDate.Data) && bounded;
        bounded = allocation_add(snapshot, entry->TimeRange.EndDate.Data) && bounded;
        if (entry->Authorization.NumberOfAuthTags > 1 ||
            entry->TimeRange.StartDate.Length != 0 || entry->TimeRange.EndDate.Length != 0) bounded = false;
        pending[pending_count++] = (pending_list){entry->TypedSubject, 1};
    }
    while (pending_count != 0) {
        pending_list work = pending[--pending_count];
        if (work.depth > ST_POLICY_MAX_DEPTH || work.list.ListType != CSSM_LIST_TYPE_UNKNOWN ||
            (work.list.Head == NULL) != (work.list.Tail == NULL)) bounded = false;
        CSSM_LIST_ELEMENT *last = NULL;
        for (CSSM_LIST_ELEMENT *node = work.list.Head; node != NULL; node = node->NextElement) {
            bool seen = false;
            for (size_t i = 0; i < budget.node_count; ++i) {
                if (budget.nodes[i] == node) seen = true;
            }
            if (seen || budget.node_count == ST_POLICY_MAX_NODES) {
                bounded = false;
                break;
            }
            budget.nodes[budget.node_count++] = node;
            bounded = allocation_add(snapshot, node) && bounded;
            switch (node->ElementType) {
            case CSSM_LIST_ELEMENT_WORDID:
                break;
            case CSSM_LIST_ELEMENT_DATUM: {
                CSSM_DATA *data = &node->Element.Word;
                bounded = allocation_add(snapshot, data->Data) && bounded;
                if (data->Length == 0 || data->Data == NULL ||
                    data->Length > ST_POLICY_MAX_DATUM_BYTES - budget.datum_bytes) bounded = false;
                else budget.datum_bytes += data->Length;
                break;
            }
            case CSSM_LIST_ELEMENT_SUBLIST:
                if (pending_count == sizeof pending / sizeof pending[0]) bounded = false;
                else pending[pending_count++] = (pending_list){node->Element.Sublist, work.depth + 1};
                break;
            default:
                bounded = false;
                break;
            }
            last = node;
        }
        if (work.list.Tail != last) bounded = false;
    }
    return bounded && snapshot->owner != NULL && snapshot->entry_count == 1 && snapshot->entries != NULL;
}

static void snapshot_release(st_policy_snapshot **snapshot)
{
    if (snapshot == NULL || *snapshot == NULL) return;
    st_policy_snapshot *owned = *snapshot;
    *snapshot = NULL;
    /* All unique allocation addresses were collected before any free. Release
     * uses that ledger, not a second walk through potentially changed view data.
     * Entries and embedded CSSM_LIST values are not separate allocations.
     */
    for (size_t i = 0; i < owned->allocation_count; ++i) free(owned->allocations[i]);
    free(owned);
}

static st_policy_result snapshot_copy(SecAccessRef access,
                                        st_policy_snapshot **out_snapshot)
{
    if (out_snapshot == NULL) return ST_POLICY_UNSAFE;
    *out_snapshot = NULL;
    if (access == NULL || CFGetTypeID(access) != SecAccessGetTypeID()) return ST_POLICY_UNSAFE;
    /* Reject an excess entry count using CF-owned metadata before requesting
     * malloc-owned native graphs. The input access must not be concurrently
     * modified while either public copy operation is in progress.
     */
    CFArrayRef acls = NULL;
    OSStatus status = SecAccessCopyACLList(access, &acls);
    if (status != errSecSuccess || acls == NULL) {
        if (acls != NULL) CFRelease(acls);
        return ST_POLICY_NATIVE_UNAVAILABLE;
    }
    /* copySecACLs includes the owner; copyOwnerAndAcl returns it separately. */
    bool one_entry = CFArrayGetCount(acls) == ST_POLICY_MAX_ENTRIES + 1;
    CFRelease(acls);
    if (!one_entry) return ST_POLICY_UNSAFE;
    st_policy_snapshot *snapshot = calloc(1, sizeof *snapshot);
    if (snapshot == NULL) return ST_POLICY_NATIVE_UNAVAILABLE;
    status = SecAccessGetOwnerAndACL(access, &snapshot->owner,
                                    &snapshot->entry_count, &snapshot->entries);
    bool bounded = snapshot_collect(snapshot);
    if (status != errSecSuccess || !bounded) {
        snapshot_release(&snapshot);
        return status != errSecSuccess ? ST_POLICY_NATIVE_UNAVAILABLE : ST_POLICY_UNSAFE;
    }
    *out_snapshot = snapshot;
    return ST_POLICY_OK;
}

struct st_keychain_item_binding {
    binding_facts facts;
    uint8_t persistent_identity[KC_PERSISTENT_BYTES];
    const item_calls *calls;
    void *context;
    const st_native_keychain_scope *scope;
};

static const char *const services[] = {
    NULL,
    "io.github.sadiksaifi.spaceterm.server.boot.v1",
    "io.github.sadiksaifi.spaceterm-preflight.server.boot.v1",
    "io.github.sadiksaifi.spaceterm-development.server.boot.v1"
};
static const CFStringRef requirements[] = {
    NULL,
    CFSTR("identifier \"io.github.sadiksaifi.spaceterm.server.launcher\" and certificate leaf = H\"9e07cd5c15c5c862a21b93ea55d4303bc19b7366\""),
    CFSTR("identifier \"io.github.sadiksaifi.spaceterm-preflight.server.launcher\" and certificate leaf = H\"9e07cd5c15c5c862a21b93ea55d4303bc19b7366\""),
    CFSTR("identifier \"io.github.sadiksaifi.spaceterm-development.server.launcher\" and certificate leaf = H\"9e07cd5c15c5c862a21b93ea55d4303bc19b7366\"")
};
static const char item_label[] = "SpaceTerm server boot key";

static bool nonzero(const uint8_t *bytes, size_t length)
{
    uint8_t bits = 0;
    for (size_t i = 0; i < length; ++i) bits |= bytes[i];
    return bits != 0;
}
static void wipe(uint8_t *bytes, size_t length)
{
    volatile uint8_t *cursor = bytes;
    while (length-- != 0) *cursor++ = 0;
}
static bool facts_valid(const binding_facts *facts)
{
    return facts != NULL && facts->uid != 0 && facts->uid != UINT32_MAX &&
           facts->gid != UINT32_MAX && facts->build >= 1 && facts->build <= 3 &&
           nonzero(facts->native_account, 16) && nonzero(facts->server_id, 16) &&
           facts->operation <= ST_KEYCHAIN_REMOVE && nonzero(facts->slot, 32) && nonzero(facts->account_binding, 32) &&
           ((facts->operation == ST_KEYCHAIN_ENROLL && facts->persistent_identity_length == 0) ||
            (facts->persistent_identity != NULL && facts->persistent_identity_length != 0 &&
           facts->persistent_identity_length <= KC_PERSISTENT_BYTES &&
           nonzero(facts->persistent_identity, facts->persistent_identity_length)));
}
static bool calls_complete(const item_calls *calls)
{
    return calls != NULL && calls->guard != NULL && calls->disable_interaction != NULL &&
           calls->open_system != NULL && calls->resolve_item != NULL &&
           calls->copy_attributes != NULL && calls->release_attributes != NULL &&
           calls->copy_access != NULL && calls->copy_secret != NULL &&
           calls->release_secret != NULL && calls->release_item != NULL &&
           calls->release_keychain != NULL && calls->recheck_permit != NULL &&
           calls->write_secret != NULL && calls->wipe_destination != NULL &&
           calls->lock_staging != NULL && calls->unlock_staging != NULL;
}
static void put_u32(uint8_t *bytes, uint32_t value)
{
    for (unsigned i = 0; i < 4; ++i) bytes[i] = (uint8_t)(value >> (8 * i));
}
static void expected_attributes(const binding_facts *facts, item_attributes *out)
{
    *out = (item_attributes){.item_class = kSecGenericPasswordItemClass,
        .service = (const uint8_t *)services[facts->build],
        .service_length = strlen(services[facts->build]),
        .label = (const uint8_t *)item_label, .label_length = sizeof item_label - 1};
    static const char hex[] = "0123456789abcdef";
    memcpy(out->generic, "STKB", 4);
    out->generic[4] = 2; /* u16 little-endian schema */
    out->generic[6] = (uint8_t)facts->build;
    put_u32(out->generic + 8, facts->uid); put_u32(out->generic + 12, facts->gid);
    memcpy(out->generic + 16, facts->native_account, 16);
    memcpy(out->generic + 32, facts->server_id, 16);
    memcpy(out->generic + 48, facts->slot, 32);
    memcpy(out->generic + 80, facts->account_binding, 32);
    for (unsigned i = 0; i < 32; ++i) {
        out->account[2 * i] = (uint8_t)hex[facts->slot[i] >> 4];
        out->account[2 * i + 1] = (uint8_t)hex[facts->slot[i] & 15];
    }
 }

static st_native_result attributes_match(const st_keychain_item_binding *binding,
                                         const item_attributes *attributes)
{
    const binding_facts *facts = &binding->facts;
    const char *service = services[facts->build];
    uint8_t expected[KC_ATTRIBUTE_BYTES] = {0};
    uint8_t slot[64];
    item_attributes selected;
    expected_attributes(facts, &selected);
    memcpy(expected, selected.generic, sizeof expected);
    memcpy(slot, selected.account, sizeof slot);
    if (attributes->item_class != kSecGenericPasswordItemClass ||
        attributes->service == NULL || attributes->service_length != strlen(service) ||
        memcmp(attributes->service, service, attributes->service_length) != 0 ||
        attributes->label == NULL || attributes->label_length != sizeof item_label - 1 ||
        memcmp(attributes->label, item_label, sizeof item_label - 1) != 0 ||
        attributes->persistent_identity == NULL ||
        attributes->persistent_identity_length != facts->persistent_identity_length ||
        memcmp(attributes->persistent_identity, facts->persistent_identity, facts->persistent_identity_length) != 0 ||
        memcmp(attributes->generic, expected, sizeof expected) != 0 ||
        memcmp(attributes->account, slot, sizeof slot) != 0) return KC_IDENTITY_MISMATCH;
    return KC_OK;
}

static st_native_result inspect_retained_item(const st_keychain_item_binding *binding, void *item)
{
    const item_calls *calls = binding->calls;
    item_attributes attributes = {0};
    SecAccessRef access = NULL;
    SecRequirementRef requirement = NULL;
    st_policy_snapshot *snapshot = NULL;
    st_native_result result = calls->copy_attributes(binding->context, item, &attributes);
    if (result == KC_OK) result = attributes_match(binding, &attributes);
    calls->release_attributes(binding->context, &attributes);
    if (result != KC_OK) goto cleanup;
    result = calls->copy_access(binding->context, item, &access);
    if (result != KC_OK) goto cleanup;
    if (access == NULL) { result = KC_NATIVE_UNAVAILABLE; goto cleanup; }
    OSStatus status = SecRequirementCreateWithString(requirements[binding->facts.build], 0, &requirement);
    if (status != errSecSuccess || requirement == NULL) { result = KC_NATIVE_UNAVAILABLE; goto cleanup; }
    st_policy_result policy = snapshot_copy(access, &snapshot);
    if (policy == ST_POLICY_OK) {
        st_policy_view view = {snapshot->owner, snapshot->entry_count, snapshot->entries};
        policy = policy_validate(&view, requirement);
    }
    result = policy == ST_POLICY_OK ? KC_OK :
             policy == ST_POLICY_UNSAFE ? KC_POLICY_REJECTED : KC_NATIVE_UNAVAILABLE;
cleanup:
    snapshot_release(&snapshot);
    if (requirement != NULL) CFRelease(requirement);
    if (access != NULL) CFRelease(access);
    return result;
}

/* These native operations have no item-selector input. The only keychain path
 * is the native System Keychain; resolution uses the retained persistent item
 * identity, then proves its keychain and exact attributes.
 */
#if !defined(ST_KEYCHAIN_TESTING)
extern st_native_result st_native_keychain_scope_copy_snapshot(const st_native_keychain_scope *, st_keychain_scope_snapshot *) __attribute__((weak_import));
extern st_native_result st_native_keychain_scope_recheck(const st_native_keychain_scope *, uint32_t) __attribute__((weak_import));
extern st_native_result st_native_keychain_scope_reserve_slot(const st_native_keychain_scope *) __attribute__((weak_import));
extern st_native_result st_native_keychain_scope_secret(const st_native_keychain_scope *, const uint8_t **, size_t *) __attribute__((weak_import));
extern st_native_result st_native_keychain_scope_record_identity(const st_native_keychain_scope *, const uint8_t *, size_t) __attribute__((weak_import));
extern st_native_result st_native_keychain_scope_mark_removed(const st_native_keychain_scope *) __attribute__((weak_import));
extern void st_native_keychain_scope_uncertain(const st_native_keychain_scope *) __attribute__((weak_import));
extern st_native_result st_native_keychain_release_scope(const st_native_release_permit *, const st_native_keychain_scope **) __attribute__((weak_import));
static st_native_result native_status(OSStatus status)
{
    if (status == errSecSuccess) return KC_OK;
    if (status == errSecInteractionNotAllowed || status == errSecInteractionRequired ||
        status == errSecUserCanceled) return KC_INTERACTION_REQUIRED;
    return KC_CREDENTIAL_UNAVAILABLE;
}
static st_native_result native_guard(void *context)
{
    const st_keychain_item_binding *binding = context;
    if (binding == NULL || binding->scope == NULL || getuid() != 0 || geteuid() != 0 ||
        st_native_keychain_scope_recheck == NULL) return KC_NATIVE_UNAVAILABLE;
    return st_native_keychain_scope_recheck(binding->scope, binding->facts.operation);
}
static st_native_result native_disable(void *context)
{
    (void)context;
    return native_status(SecKeychainSetUserInteractionAllowed(false));
}
static st_native_result native_open(void *context, void **out_keychain)
{
    (void)context;
    SecKeychainRef keychain = NULL;
    OSStatus status = SecKeychainOpen("/Library/Keychains/System.keychain", &keychain);
    *out_keychain = keychain;
    return native_status(status);
}
static st_native_result native_resolve(void *context, void *keychain,
                                      const uint8_t *identity, size_t length, void **out_item)
{
    (void)context;
    CFDataRef reference = CFDataCreate(NULL, identity, (CFIndex)length);
    if (reference == NULL) return KC_NATIVE_UNAVAILABLE;
    SecKeychainItemRef item = NULL;
    OSStatus status = SecKeychainItemCopyFromPersistentReference(reference, &item);
    CFRelease(reference);
    *out_item = item;
    if (status != errSecSuccess || item == NULL) return native_status(status == errSecSuccess ? errSecItemNotFound : status);
    SecKeychainRef actual = NULL;
    status = SecKeychainItemCopyKeychain(item, &actual);
    bool matches = status == errSecSuccess && actual != NULL && CFEqual(actual, keychain);
    if (actual != NULL) CFRelease(actual);
    return matches ? KC_OK : KC_IDENTITY_MISMATCH;
}
static st_native_result native_attributes(void *context, void *item, item_attributes *out)
{
    (void)context;
    UInt32 tags[] = {kSecServiceItemAttr, kSecAccountItemAttr, kSecGenericItemAttr, kSecLabelItemAttr};
    UInt32 formats[] = {CSSM_DB_ATTRIBUTE_FORMAT_BLOB, CSSM_DB_ATTRIBUTE_FORMAT_BLOB,
                        CSSM_DB_ATTRIBUTE_FORMAT_BLOB, CSSM_DB_ATTRIBUTE_FORMAT_BLOB};
    SecKeychainAttributeInfo info = {4, tags, formats};
    SecKeychainAttributeList *attributes = NULL;
    SecItemClass item_class = 0;
    /* NULL data and length are essential: inspection never reads the secret. */
    OSStatus status = SecKeychainItemCopyAttributesAndData(item, &info, &item_class, &attributes, NULL, NULL);
    out->native_attributes = attributes;
    out->item_class = item_class;
    if (status != errSecSuccess) return native_status(status);
    if (attributes == NULL || attributes->count != 4 || attributes->attr == NULL) return KC_IDENTITY_MISMATCH;
    unsigned seen = 0;
    for (unsigned i = 0; i < attributes->count; ++i) {
        const SecKeychainAttribute *attribute = attributes->attr + i;
        if (attribute->data == NULL) return KC_IDENTITY_MISMATCH;
        unsigned bit = 0;
        switch (attribute->tag) {
        case kSecServiceItemAttr:
            bit = 1; out->service = attribute->data; out->service_length = attribute->length; break;
        case kSecAccountItemAttr:
            bit = 2;
            if (attribute->length != sizeof out->account) return KC_IDENTITY_MISMATCH;
            memcpy(out->account, attribute->data, sizeof out->account); break;
        case kSecGenericItemAttr:
            bit = 4;
            if (attribute->length != sizeof out->generic) return KC_IDENTITY_MISMATCH;
            memcpy(out->generic, attribute->data, sizeof out->generic); break;
        case kSecLabelItemAttr:
            bit = 8; out->label = attribute->data; out->label_length = attribute->length; break;
        default: return KC_IDENTITY_MISMATCH;
        }
        if ((seen & bit) != 0) return KC_IDENTITY_MISMATCH;
        seen |= bit;
    }
    if (seen != 15) return KC_IDENTITY_MISMATCH;
    status = SecKeychainItemCreatePersistentReference(item, &out->native_identity);
    if (status != errSecSuccess || out->native_identity == NULL) return native_status(status == errSecSuccess ? errSecAllocate : status);
    CFIndex length = CFDataGetLength(out->native_identity);
    if (length <= 0 || length > KC_PERSISTENT_BYTES) return KC_IDENTITY_MISMATCH;
    out->persistent_identity = CFDataGetBytePtr(out->native_identity);
    out->persistent_identity_length = (size_t)length;
    return KC_OK;
}
static void native_release_attributes(void *context, item_attributes *attributes)
{
    (void)context;
    if (attributes->native_attributes != NULL) SecKeychainItemFreeAttributesAndData(attributes->native_attributes, NULL);
    if (attributes->native_identity != NULL) CFRelease(attributes->native_identity);
    *attributes = (item_attributes){0};
}
static st_native_result native_access(void *context, void *item, SecAccessRef *out_access)
{
    (void)context; return native_status(SecKeychainItemCopyAccess(item, out_access));
}
static st_native_result native_secret(void *context, void *item, uint8_t **out_bytes, size_t *out_length)
{
    (void)context;
    UInt32 length = 0;
    void *bytes = NULL;
    OSStatus status = SecKeychainItemCopyAttributesAndData(item, NULL, NULL, NULL, &length, &bytes);
    *out_bytes = bytes; *out_length = length;
    return native_status(status);
}
static st_native_result native_release_secret(void *context, uint8_t *bytes, size_t length)
{
    (void)context; (void)length;
    return bytes == NULL || SecKeychainItemFreeAttributesAndData(NULL, bytes) == errSecSuccess ? KC_OK : KC_CLEANUP_UNRESOLVED;
}
static void native_release_item(void *context, void *item)
{
    (void)context; if (item != NULL) CFRelease(item);
}
static void native_release_keychain(void *context, void *keychain)
{
    (void)context; if (keychain != NULL) CFRelease(keychain);
}
static st_native_result native_recheck(void *context, const st_native_release_permit *permit)
{
    (void)context;
    return st_native_release_permit_recheck != NULL ? st_native_release_permit_recheck(permit) : KC_NATIVE_UNAVAILABLE;
}
static st_native_result native_write(void *context, st_native_locked_secret *destination,
                                     const uint8_t *bytes, size_t length)
{
    (void)context;
    return st_native_locked_secret_write != NULL ? st_native_locked_secret_write(destination, bytes, length) : KC_NATIVE_UNAVAILABLE;
}
static void native_wipe_destination(void *context, st_native_locked_secret *destination)
{
    (void)context; if (st_native_locked_secret_wipe != NULL) st_native_locked_secret_wipe(destination);
}
static st_native_result native_lock_staging(void *context, uint8_t *bytes, size_t length)
{
    (void)context;
    return mlock(bytes, length) == 0 ? KC_OK : KC_NATIVE_UNAVAILABLE;
}
static st_native_result native_unlock_staging(void *context, uint8_t *bytes, size_t length)
{
    (void)context;
    return munlock(bytes, length) == 0 ? KC_OK : KC_CLEANUP_UNRESOLVED;
}
static st_native_result native_collision(void *context, void *keychain, const uint8_t *service,
                                          size_t service_length, const uint8_t *account, size_t account_length)
{
    (void)context;
    SecKeychainAttribute attributes[] = {
        {kSecServiceItemAttr, (UInt32)service_length, (void *)service},
        {kSecAccountItemAttr, (UInt32)account_length, (void *)account}};
    SecKeychainAttributeList list = {2, attributes};
    SecKeychainSearchRef search = NULL;
    SecKeychainItemRef item = NULL;
    OSStatus status = SecKeychainSearchCreateFromAttributes(keychain, kSecGenericPasswordItemClass, &list, &search);
    if (status == errSecSuccess && search != NULL) status = SecKeychainSearchCopyNext(search, &item);
    else if (status == errSecSuccess) status = errSecAllocate;
    if (item != NULL) CFRelease(item);
    if (search != NULL) CFRelease(search);
    return status == errSecItemNotFound ? KC_OK : status == errSecSuccess ? KC_IDENTITY_MISMATCH : native_status(status);
}
static st_native_result native_reserve(void *context)
{
    const st_keychain_item_binding *binding = context;
    return st_native_keychain_scope_reserve_slot != NULL ? st_native_keychain_scope_reserve_slot(binding->scope) : KC_NATIVE_UNAVAILABLE;
}
static st_native_result native_enrollment_secret(void *context, const uint8_t **bytes, size_t *length)
{
    const st_keychain_item_binding *binding = context;
    return st_native_keychain_scope_secret != NULL ? st_native_keychain_scope_secret(binding->scope, bytes, length) : KC_NATIVE_UNAVAILABLE;
}
static st_native_result native_create(void *context, void *keychain, const item_attributes *expected,
                                     SecAccessRef access, const uint8_t *bytes, size_t length, void **out_item)
{
    (void)context;
    SecKeychainAttribute attributes[] = {
        {kSecServiceItemAttr, (UInt32)expected->service_length, (void *)expected->service},
        {kSecAccountItemAttr, sizeof expected->account, (void *)expected->account},
        {kSecGenericItemAttr, sizeof expected->generic, (void *)expected->generic},
        {kSecLabelItemAttr, (UInt32)expected->label_length, (void *)expected->label}};
    SecKeychainAttributeList list = {4, attributes};
    SecKeychainItemRef item = NULL;
    OSStatus status = SecKeychainItemCreateFromContent(kSecGenericPasswordItemClass, &list, (UInt32)length,
                                                     bytes, keychain, access, &item);
    *out_item = item;
    if (status == errSecSuccess) return KC_OK;
    if (status == errSecDuplicateItem) return KC_IDENTITY_MISMATCH;
    st_native_result classified = native_status(status);
    return classified == KC_INTERACTION_REQUIRED ? classified : KC_CLEANUP_UNRESOLVED;
}
static st_native_result native_record(void *context, const uint8_t *identity, size_t length)
{
    const st_keychain_item_binding *binding = context;
    return st_native_keychain_scope_record_identity != NULL ? st_native_keychain_scope_record_identity(binding->scope, identity, length) : KC_NATIVE_UNAVAILABLE;
}
static st_native_result native_delete(void *context, void *item)
{
    (void)context; return native_status(SecKeychainItemDelete(item));
}
static st_native_result native_removed(void *context)
{
    const st_keychain_item_binding *binding = context;
    return st_native_keychain_scope_mark_removed != NULL ? st_native_keychain_scope_mark_removed(binding->scope) : KC_NATIVE_UNAVAILABLE;
}
static void native_uncertain(void *context)
{
    const st_keychain_item_binding *binding = context;
    if (st_native_keychain_scope_uncertain != NULL) st_native_keychain_scope_uncertain(binding->scope);
}
static const item_calls native_calls = {
    native_guard, native_disable, native_open, native_resolve, native_attributes,
    native_release_attributes, native_access, native_secret, native_release_secret,
    native_release_item, native_release_keychain, native_recheck, native_write, native_wipe_destination,
    native_lock_staging, native_unlock_staging, native_collision, native_reserve,
    native_enrollment_secret, native_create, native_record, native_delete, native_removed, native_uncertain
};
#endif

st_native_result st_keychain_retain_scope(const st_native_keychain_scope *scope, st_keychain_item_binding **out)
{
    if (out == NULL) return KC_INVALID_RECORD;
    *out = NULL;
#if defined(ST_KEYCHAIN_TESTING)
    extern st_native_result st_keychain_test_scope_binding(const st_native_keychain_scope *, st_keychain_item_binding **);
    return st_keychain_test_scope_binding(scope, out);
#else
    if (scope == NULL || st_native_keychain_scope_copy_snapshot == NULL ||
        st_native_keychain_scope_recheck == NULL) return KC_NATIVE_UNAVAILABLE;
    st_keychain_scope_snapshot snapshot = {0};
    st_native_result result = st_native_keychain_scope_copy_snapshot(scope, &snapshot);
    if (result != KC_OK) return result;
    binding_facts facts = {.uid = snapshot.uid, .gid = snapshot.gid, .build = snapshot.build,
        .operation = snapshot.operation, .persistent_identity = snapshot.persistent_identity,
        .persistent_identity_length = snapshot.persistent_identity_length};
    memcpy(facts.native_account, snapshot.native_account, sizeof facts.native_account);
    memcpy(facts.server_id, snapshot.server_id, sizeof facts.server_id);
    memcpy(facts.slot, snapshot.slot, sizeof facts.slot);
    memcpy(facts.account_binding, snapshot.account_binding, sizeof facts.account_binding);
    if (!facts_valid(&facts)) return KC_INVALID_RECORD;
    if (facts.operation == ST_KEYCHAIN_ENROLL &&
        (facts.persistent_identity_length != 0 || st_native_keychain_scope_reserve_slot == NULL ||
         st_native_keychain_scope_secret == NULL || st_native_keychain_scope_record_identity == NULL ||
         st_native_keychain_scope_uncertain == NULL)) return KC_NATIVE_UNAVAILABLE;
    if (facts.operation == ST_KEYCHAIN_REMOVE &&
        (st_native_keychain_scope_mark_removed == NULL || st_native_keychain_scope_uncertain == NULL)) return KC_NATIVE_UNAVAILABLE;
    st_keychain_item_binding *binding = calloc(1, sizeof *binding);
    if (binding == NULL) return KC_NATIVE_UNAVAILABLE;
    binding->facts = facts;
    if (facts.persistent_identity_length != 0)
        memcpy(binding->persistent_identity, facts.persistent_identity, facts.persistent_identity_length);
    binding->facts.persistent_identity = binding->persistent_identity;
    binding->scope = scope; binding->calls = &native_calls; binding->context = binding;
    result = native_guard(binding);
    if (result != KC_OK) { st_keychain_release_binding(&binding); return result; }
    *out = binding; return KC_OK;
#endif
}
void st_keychain_release_binding(st_keychain_item_binding **owned_binding)
{
    if (owned_binding == NULL || *owned_binding == NULL) return;
    st_keychain_item_binding *binding = *owned_binding;
    *owned_binding = NULL;
    wipe((uint8_t *)binding, sizeof *binding); free(binding);
}

static st_native_result open_admitted_item(const st_keychain_item_binding *binding,
                                          void **out_keychain, void **out_item)
{
    *out_keychain = NULL; *out_item = NULL;
    if (binding == NULL || !facts_valid(&binding->facts) || !calls_complete(binding->calls)) return KC_INVALID_RECORD;
#if !defined(ST_KEYCHAIN_TESTING)
    /* Production has no injectable table or caller-defined authority. */
    if (binding->calls != &native_calls) return KC_NATIVE_UNAVAILABLE;
#endif
    const item_calls *calls = binding->calls;
    st_native_result result = calls->guard(binding->context);
    if (result != KC_OK) return result;
    result = calls->disable_interaction(binding->context);
    if (result != KC_OK) return result;
    result = calls->open_system(binding->context, out_keychain);
    if (result != KC_OK) return result;
    if (*out_keychain == NULL) return KC_NATIVE_UNAVAILABLE;
    result = calls->resolve_item(binding->context, *out_keychain,
                                 binding->facts.persistent_identity,
                                 binding->facts.persistent_identity_length, out_item);
    if (result != KC_OK) return result;
    return *out_item != NULL ? KC_OK : KC_NATIVE_UNAVAILABLE;
}

static void policy_word(CSSM_LIST_ELEMENT *node, CSSM_WORDID_TYPE value)
{
    *node = (CSSM_LIST_ELEMENT){.ElementType = CSSM_LIST_ELEMENT_WORDID, .WordID = value};
}
static void policy_datum(CSSM_LIST_ELEMENT *node, void *bytes, size_t length)
{
    *node = (CSSM_LIST_ELEMENT){.ElementType = CSSM_LIST_ELEMENT_DATUM,
        .Element.Word = {.Length = length, .Data = bytes}};
}
static CSSM_LIST policy_list(CSSM_LIST_ELEMENT *nodes, size_t count)
{
    for (size_t i = 0; i < count; ++i) nodes[i].NextElement = i + 1 < count ? nodes + i + 1 : NULL;
    return (CSSM_LIST){.ListType = CSSM_LIST_TYPE_UNKNOWN, .Head = nodes, .Tail = nodes + count - 1};
}
static st_native_result enrollment_access(uint32_t build, SecAccessRef *out)
{
    SecRequirementRef requirement = NULL;
    CFDataRef data = NULL;
    st_native_result result = KC_NATIVE_UNAVAILABLE;
    *out = NULL;
    if (SecRequirementCreateWithString(requirements[build], 0, &requirement) != errSecSuccess ||
        requirement == NULL || SecRequirementCopyData(requirement, 0, &data) != errSecSuccess || data == NULL) goto cleanup;
    uint8_t hash[20] = {0};
    char path[] = "spaceterm-launcher"; /* documentary only; never used to authorize */
    CSSM_ACL_PROCESS_SUBJECT_SELECTOR selector = {.version = CSSM_ACL_PROCESS_SELECTOR_CURRENT_VERSION,
        .mask = CSSM_ACL_MATCH_UID, .uid = 0, .gid = 0};
    CSSM_LIST_ELEMENT process[2], signature[5], threshold[5];
    policy_word(process, CSSM_ACL_SUBJECT_TYPE_PROCESS);
    policy_datum(process + 1, &selector, sizeof selector);
    policy_word(signature, CSSM_ACL_SUBJECT_TYPE_CODE_SIGNATURE);
    policy_word(signature + 1, CSSM_ACL_CODE_SIGNATURE_OSX);
    policy_datum(signature + 2, hash, sizeof hash); policy_datum(signature + 3, path, sizeof path);
    policy_datum(signature + 4, (void *)CFDataGetBytePtr(data), (size_t)CFDataGetLength(data));
    policy_word(threshold, CSSM_ACL_SUBJECT_TYPE_THRESHOLD);
    policy_word(threshold + 1, 2); policy_word(threshold + 2, 2);
    threshold[3] = (CSSM_LIST_ELEMENT){.ElementType = CSSM_LIST_ELEMENT_SUBLIST, .Element.Sublist = policy_list(process, 2)};
    threshold[4] = (CSSM_LIST_ELEMENT){.ElementType = CSSM_LIST_ELEMENT_SUBLIST, .Element.Sublist = policy_list(signature, 5)};
    CSSM_LIST subject = policy_list(threshold, 5);
    CSSM_ACL_OWNER_PROTOTYPE owner = {.TypedSubject = subject};
    CSSM_ACL_AUTHORIZATION_TAG decrypt = CSSM_ACL_AUTHORIZATION_DECRYPT;
    CSSM_ACL_ENTRY_INFO entry = {.EntryPublicInfo = {.TypedSubject = subject, .Authorization = {1, &decrypt},
        .EntryTag = ST_POLICY_ENTRY_TAG}, .EntryHandle = ST_POLICY_ENTRY_HANDLE};
    if (SecAccessCreateFromOwnerAndACL(&owner, 1, &entry, out) == errSecSuccess && *out != NULL) result = KC_OK;
cleanup:
    if (data != NULL) CFRelease(data);
    if (requirement != NULL) CFRelease(requirement);
    return result;
}

st_native_result st_keychain_enroll_owned_item(st_keychain_item_binding *binding)
{
    if (binding == NULL || !facts_valid(&binding->facts) || binding->facts.operation != ST_KEYCHAIN_ENROLL ||
        !calls_complete(binding->calls)) return KC_INVALID_RECORD;
    const item_calls *calls = binding->calls;
    if (calls->collision == NULL || calls->reserve_slot == NULL || calls->enrollment_secret == NULL ||
        calls->create_item == NULL || calls->record_identity == NULL || calls->uncertain == NULL) return KC_NATIVE_UNAVAILABLE;
#if !defined(ST_KEYCHAIN_TESTING)
    if (calls != &native_calls) return KC_NATIVE_UNAVAILABLE;
#endif
    void *keychain = NULL, *item = NULL;
    SecAccessRef access = NULL;
    item_attributes expected, observed = {0};
    expected_attributes(&binding->facts, &expected);
    st_native_result result = calls->guard(binding->context);
    if (result == KC_OK) result = calls->disable_interaction(binding->context);
    if (result == KC_OK) result = calls->open_system(binding->context, &keychain);
    if (result == KC_OK && keychain == NULL) result = KC_NATIVE_UNAVAILABLE;
    if (result == KC_OK) result = calls->collision(binding->context, keychain, expected.service,
                                                  expected.service_length, expected.account, sizeof expected.account);
    if (result == KC_OK) result = enrollment_access(binding->facts.build, &access);
    if (result == KC_OK) result = calls->guard(binding->context);
    if (result == KC_OK) result = calls->reserve_slot(binding->context);
    const uint8_t *secret = NULL;
    size_t length = 0;
    if (result == KC_OK) result = calls->enrollment_secret(binding->context, &secret, &length);
    if (result == KC_OK && (secret == NULL || length != KC_SECRET_BYTES)) result = KC_CREDENTIAL_UNAVAILABLE;
    if (result == KC_OK) result = calls->guard(binding->context);
    if (result == KC_OK) result = calls->create_item(binding->context, keychain, &expected, access, secret, length, &item);
    if (item != NULL) {
        /* Record the exact created object before full persisted-policy inspection.
         * Never delete by selector on a partial failure. A failed identity write
         * keeps the pending slot and item for reviewed operator recovery.
         */
        st_native_result captured = calls->copy_attributes(binding->context, item, &observed);
        if (captured == KC_OK && observed.persistent_identity != NULL && observed.persistent_identity_length != 0 &&
            observed.persistent_identity_length <= KC_PERSISTENT_BYTES &&
            nonzero(observed.persistent_identity, observed.persistent_identity_length)) {
            memcpy(binding->persistent_identity, observed.persistent_identity, observed.persistent_identity_length);
            binding->facts.persistent_identity_length = observed.persistent_identity_length;
            captured = calls->record_identity(binding->context, binding->persistent_identity,
                                               binding->facts.persistent_identity_length);
        } else captured = KC_CLEANUP_UNRESOLVED;
        calls->release_attributes(binding->context, &observed);
        if (captured != KC_OK) { calls->uncertain(binding->context); result = KC_CLEANUP_UNRESOLVED; }
        if (result == KC_OK) result = inspect_retained_item(binding, item);
        if (result == KC_OK) result = calls->guard(binding->context);
    } else if (result == KC_OK || result == KC_CLEANUP_UNRESOLVED) {
        calls->uncertain(binding->context); result = KC_CLEANUP_UNRESOLVED;
    }
    if (access != NULL) CFRelease(access);
    calls->release_item(binding->context, item); calls->release_keychain(binding->context, keychain);
    return result;
}

st_native_result st_keychain_remove_owned_item(const st_keychain_item_binding *binding)
{
    if (binding == NULL || !facts_valid(&binding->facts) || binding->facts.operation != ST_KEYCHAIN_REMOVE ||
        !calls_complete(binding->calls)) return KC_INVALID_RECORD;
    const item_calls *calls = binding->calls;
    if (calls->delete_item == NULL || calls->mark_removed == NULL || calls->uncertain == NULL) return KC_NATIVE_UNAVAILABLE;
    void *keychain = NULL, *item = NULL;
    item_attributes attributes = {0};
    st_native_result result = open_admitted_item(binding, &keychain, &item);
    if (result == KC_OK) result = calls->copy_attributes(binding->context, item, &attributes);
    if (result == KC_OK) result = attributes_match(binding, &attributes);
    calls->release_attributes(binding->context, &attributes);
    /* Removal needs exact receipt identity and attributes, not a safe DECRYPT
     * policy. Otherwise a partially created item with unsafe auxiliary ACLs
     * could never be cleaned up. Retain this same native item through deletion.
     */
    if (result == KC_OK) result = calls->guard(binding->context);
    if (result == KC_OK) {
        result = calls->delete_item(binding->context, item);
        if (result == KC_OK) result = calls->mark_removed(binding->context);
        if (result != KC_OK) { calls->uncertain(binding->context); result = KC_CLEANUP_UNRESOLVED; }
    }
    calls->release_item(binding->context, item); calls->release_keychain(binding->context, keychain);
    return result;
}

st_native_result st_keychain_inspect_fixed_item(const st_keychain_item_binding *binding,
                                               st_keychain_policy_observation *out_observation)
{
    if (out_observation == NULL) return KC_INVALID_RECORD;
    *out_observation = (st_keychain_policy_observation){0};
    void *keychain = NULL, *item = NULL;
    st_native_result result = open_admitted_item(binding, &keychain, &item);
    if (result == KC_OK) result = inspect_retained_item(binding, item);
    if (binding != NULL && (item != NULL || keychain != NULL) && calls_complete(binding->calls)) {
        binding->calls->release_item(binding->context, item);
        binding->calls->release_keychain(binding->context, keychain);
    }
    if (result == KC_OK) *out_observation = (st_keychain_policy_observation){1, 1, 1, 1, 1, 1};
    return result;
}

typedef struct {
    uint8_t *bytes;
    bool locked;
} locked_staging;

static st_native_result staging_create(const st_keychain_item_binding *binding,
                                       locked_staging *staging)
{
    /* A private mapping owns the whole page. Locking a malloc or stack buffer
     * could overlap another secret's page and later unlock it during cleanup.
     */
    void *bytes = mmap(NULL, KC_SECRET_BYTES, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANON, -1, 0);
    if (bytes == MAP_FAILED) return KC_NATIVE_UNAVAILABLE;
    staging->bytes = bytes;
    st_native_result result = binding->calls->lock_staging(binding->context, bytes, KC_SECRET_BYTES);
    staging->locked = result == KC_OK;
    return result;
}

static st_native_result staging_release(const st_keychain_item_binding *binding,
                                        locked_staging *staging)
{
    if (staging->bytes == NULL) return KC_OK;
    wipe(staging->bytes, KC_SECRET_BYTES);
    st_native_result result = KC_OK;
    if (staging->locked && binding->calls->unlock_staging(binding->context, staging->bytes,
                                                        KC_SECRET_BYTES) != KC_OK) {
        result = KC_CLEANUP_UNRESOLVED;
    }
    if (munmap(staging->bytes, KC_SECRET_BYTES) != 0) result = KC_CLEANUP_UNRESOLVED;
    *staging = (locked_staging){0};
    return result;
}

static st_native_result retrieve_fixed_item(const st_keychain_item_binding *binding,
                                           const st_native_release_permit *permit,
                                           st_native_locked_secret *destination)
{
    if (binding == NULL || !facts_valid(&binding->facts) || binding->facts.operation != ST_KEYCHAIN_RELEASE || !calls_complete(binding->calls) || destination == NULL) return KC_INVALID_RECORD;
#if !defined(ST_KEYCHAIN_TESTING)
    if (binding->calls != &native_calls) {
        st_native_locked_secret_wipe(destination);
        return KC_NATIVE_UNAVAILABLE;
    }
#endif
    const item_calls *calls = binding->calls;
    void *keychain = NULL, *item = NULL;
    locked_staging staging = {0};
    st_native_result result = calls->recheck_permit(binding->context, permit);
    if (result != KC_OK) goto cleanup;
    result = open_admitted_item(binding, &keychain, &item);
    if (result != KC_OK) goto cleanup;
    result = inspect_retained_item(binding, item);
    if (result != KC_OK) goto cleanup;
    result = staging_create(binding, &staging);
    if (result != KC_OK) goto cleanup;
    result = calls->recheck_permit(binding->context, permit);
    if (result != KC_OK) goto cleanup;
    uint8_t *bytes = NULL;
    size_t length = 0;
    result = calls->copy_secret(binding->context, item, &bytes, &length);
    if (result == KC_OK) {
        if (bytes == NULL || length != KC_SECRET_BYTES) result = KC_CREDENTIAL_UNAVAILABLE;
        else memcpy(staging.bytes, bytes, KC_SECRET_BYTES);
    }
    /* Framework copy-out cannot target locked memory. Move valid bytes into the
     * prelocked mapping, then erase and release the copy-out before any native
     * policy or permit call. Failure allocations follow the same immediate path.
     */
    if (bytes != NULL) wipe(bytes, length);
    if (calls->release_secret(binding->context, bytes, length) != KC_OK) result = KC_CLEANUP_UNRESOLVED;
    if (result != KC_OK) goto cleanup;
    /* Retain the same item and inspect its full policy again. Neither a label
     * match nor inspection of a different newly searched item closes this race.
     * Only the locked staging mapping holds plaintext while inspection runs.
     */
    result = inspect_retained_item(binding, item);
    if (result != KC_OK) goto cleanup;
    result = calls->recheck_permit(binding->context, permit);
    if (result != KC_OK) goto cleanup;
    result = calls->write_secret(binding->context, destination, staging.bytes, KC_SECRET_BYTES);
cleanup:
    if (staging_release(binding, &staging) != KC_OK) result = KC_CLEANUP_UNRESOLVED;
    calls->release_item(binding->context, item);
    calls->release_keychain(binding->context, keychain);
    if (result != KC_OK) calls->wipe_destination(binding->context, destination);
    return result;
}

st_native_result st_keychain_retrieve_for_startup(const st_native_release_permit *permit,
                                                 st_native_locked_secret *destination)
{
    if (destination == NULL) return KC_INVALID_RECORD;
    if (permit == NULL) {
        if (st_native_locked_secret_wipe != NULL) st_native_locked_secret_wipe(destination);
        return KC_INVALID_RECORD;
    }
    st_native_result result = KC_NATIVE_UNAVAILABLE;
    st_keychain_item_binding *binding = NULL;
    const st_native_keychain_scope *scope = NULL;
    if (st_native_release_permit_recheck != NULL && st_native_keychain_release_scope != NULL &&
        st_native_locked_secret_write != NULL && st_native_locked_secret_wipe != NULL) {
        result = st_native_release_permit_recheck(permit);
        if (result == KC_OK) result = st_native_keychain_release_scope(permit, &scope);
        if (result == KC_OK) result = st_keychain_retain_scope(scope, &binding);
        if (result == KC_OK) result = retrieve_fixed_item(binding, permit, destination);
    }
    st_keychain_release_binding(&binding);
    if (result != KC_OK && st_native_locked_secret_wipe != NULL) st_native_locked_secret_wipe(destination);
    return result;
}

#if defined(ST_KEYCHAIN_TESTING)
st_native_result st_keychain_test_binding_create(const binding_facts *facts,
                                                 const item_calls *calls, void *context,
                                                 st_keychain_item_binding **out_binding)
{
    if (out_binding == NULL) return KC_INVALID_RECORD;
    *out_binding = NULL;
    if (!facts_valid(facts) || !calls_complete(calls)) return KC_INVALID_RECORD;
    st_keychain_item_binding *binding = calloc(1, sizeof *binding);
    if (binding == NULL) return KC_NATIVE_UNAVAILABLE;
    binding->facts = *facts;
    if (facts->persistent_identity_length != 0)
        memcpy(binding->persistent_identity, facts->persistent_identity, facts->persistent_identity_length);
    binding->facts.persistent_identity = binding->persistent_identity;
    binding->calls = calls; binding->context = context;
    *out_binding = binding;
    return KC_OK;
}
void st_keychain_test_binding_release(st_keychain_item_binding **owned_binding)
{
    st_keychain_release_binding(owned_binding);
}
st_native_result st_keychain_test_retrieve(const st_keychain_item_binding *binding,
                                          const st_native_release_permit *permit,
                                          st_native_locked_secret *destination)
{
    return retrieve_fixed_item(binding, permit, destination);
}
#endif

#pragma clang diagnostic pop
