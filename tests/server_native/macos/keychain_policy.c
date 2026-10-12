#include "prototype.h"

#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

/* Same public file-based Keychain/CSSM deprecation exception as prototype.h. */
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"

enum {
    ST_POLICY_MAX_ALLOCATIONS = 2 * ST_POLICY_MAX_NODES +
                                3 * ST_POLICY_MAX_ENTRIES + 2
};

typedef struct {
    const CSSM_LIST_ELEMENT *nodes[ST_POLICY_MAX_NODES];
    size_t node_count;
    size_t datum_bytes;
} graph_budget;

typedef struct {
    const CSSM_DATA *hash;
    const CSSM_DATA *path;
    const CSSM_DATA *requirement;
} signature_parts;

typedef struct {
    CSSM_LIST threshold;
    CSSM_LIST_ELEMENT nodes[12];
    CSSM_ACL_PROCESS_SUBJECT_SELECTOR selector;
    uint8_t hash[ST_POLICY_LEGACY_HASH_BYTES];
    uint8_t path[ST_POLICY_MAX_PATH_BYTES];
    uint8_t requirement[ST_POLICY_MAX_REQUIREMENT_BYTES];
} owned_subject;

_Static_assert(24 <= ST_POLICY_MAX_NODES && 2 <= ST_POLICY_MAX_DEPTH,
               "both constructed subjects must fit the graph budgets");
_Static_assert(2 * (sizeof(CSSM_ACL_PROCESS_SUBJECT_SELECTOR) +
                   ST_POLICY_LEGACY_HASH_BYTES + ST_POLICY_MAX_PATH_BYTES +
                   ST_POLICY_MAX_REQUIREMENT_BYTES) <= ST_POLICY_MAX_DATUM_BYTES,
               "both constructed subjects must fit the datum budget");

struct st_policy_snapshot {
    CSSM_ACL_OWNER_PROTOTYPE *owner;
    uint32_t entry_count;
    CSSM_ACL_ENTRY_INFO *entries;
    void *allocations[ST_POLICY_MAX_ALLOCATIONS];
    size_t allocation_count;
};

static void *policy_calloc(size_t count, size_t size)
{
#if defined(ST_POLICY_TEST_ALLOCATIONS)
    return st_policy_test_calloc(count, size);
#else
    return calloc(count, size);
#endif
}

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

st_policy_result st_policy_validate(const st_policy_view *view,
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

static void word(CSSM_LIST_ELEMENT *node, CSSM_WORDID_TYPE value)
{
    *node = (CSSM_LIST_ELEMENT){
        .WordID = value,
        .ElementType = CSSM_LIST_ELEMENT_WORDID
    };
}

static void datum(CSSM_LIST_ELEMENT *node, void *bytes, size_t length)
{
    *node = (CSSM_LIST_ELEMENT){
        .ElementType = CSSM_LIST_ELEMENT_DATUM,
        .Element.Word = {.Length = length, .Data = bytes}
    };
}

static CSSM_LIST linked_list(CSSM_LIST_ELEMENT *nodes, size_t count)
{
    for (size_t i = 0; i < count; ++i) {
        nodes[i].NextElement = i + 1 < count ? &nodes[i + 1] : NULL;
    }
    return (CSSM_LIST){
        .ListType = CSSM_LIST_TYPE_UNKNOWN,
        .Head = &nodes[0],
        .Tail = &nodes[count - 1]
    };
}

static owned_subject *subject_copy(const signature_parts *parts)
{
    owned_subject *subject = policy_calloc(1, sizeof *subject);
    if (subject == NULL) return NULL;
    subject->selector = (CSSM_ACL_PROCESS_SUBJECT_SELECTOR){
        .version = CSSM_ACL_PROCESS_SELECTOR_CURRENT_VERSION,
        .mask = CSSM_ACL_MATCH_UID,
        .uid = 0,
        .gid = 0
    };
    memcpy(subject->hash, parts->hash->Data, parts->hash->Length);
    memcpy(subject->path, parts->path->Data, parts->path->Length);
    memcpy(subject->requirement, parts->requirement->Data, parts->requirement->Length);
    CSSM_LIST_ELEMENT *threshold = &subject->nodes[0];
    CSSM_LIST_ELEMENT *process = &subject->nodes[5];
    CSSM_LIST_ELEMENT *signature = &subject->nodes[7];
    word(process, CSSM_ACL_SUBJECT_TYPE_PROCESS);
    datum(process + 1, &subject->selector, sizeof subject->selector);
    word(signature, CSSM_ACL_SUBJECT_TYPE_CODE_SIGNATURE);
    word(signature + 1, CSSM_ACL_CODE_SIGNATURE_OSX);
    datum(signature + 2, subject->hash, parts->hash->Length);
    datum(signature + 3, subject->path, parts->path->Length);
    datum(signature + 4, subject->requirement, parts->requirement->Length);
    word(threshold, CSSM_ACL_SUBJECT_TYPE_THRESHOLD);
    word(threshold + 1, 2);
    word(threshold + 2, 2);
    threshold[3] = (CSSM_LIST_ELEMENT){
        .ElementType = CSSM_LIST_ELEMENT_SUBLIST,
        .Element.Sublist = linked_list(process, 2)
    };
    threshold[4] = (CSSM_LIST_ELEMENT){
        .ElementType = CSSM_LIST_ELEMENT_SUBLIST,
        .Element.Sublist = linked_list(signature, 5)
    };
    subject->threshold = linked_list(threshold, 5);
    return subject;
}

st_policy_result st_policy_access_create(const CSSM_LIST *code_signature,
                                        SecRequirementRef expected_requirement,
                                        SecAccessRef *out_access)
{
    if (out_access == NULL) return ST_POLICY_UNSAFE;
    *out_access = NULL;
    graph_budget budget = {0};
    signature_parts parts;
    if (!graph_list(&budget, code_signature, 1) ||
        !signature_shape(code_signature, &parts)) return ST_POLICY_UNSAFE;
    st_policy_result result = requirement_matches(parts.requirement, expected_requirement);
    if (result != ST_POLICY_OK) return result;
    owned_subject *owner_subject = subject_copy(&parts);
    owned_subject *decrypt_subject = owner_subject != NULL ? subject_copy(&parts) : NULL;
    if (owner_subject == NULL || decrypt_subject == NULL) {
        free(decrypt_subject);
        free(owner_subject);
        return ST_POLICY_NATIVE_UNAVAILABLE;
    }
    /* Each local tree is one owned allocation containing its nodes and bytes.
     * Only its base is freed. The native constructor copies both trees.
     */
    CSSM_ACL_OWNER_PROTOTYPE owner = {
        .TypedSubject = owner_subject->threshold,
        .Delegate = CSSM_FALSE
    };
    CSSM_ACL_AUTHORIZATION_TAG authorization = CSSM_ACL_AUTHORIZATION_DECRYPT;
    CSSM_ACL_ENTRY_INFO entry = {
        .EntryPublicInfo = {
            .TypedSubject = decrypt_subject->threshold,
            .Delegate = CSSM_FALSE,
            .Authorization = {.NumberOfAuthTags = 1, .AuthTags = &authorization},
            .EntryTag = ST_POLICY_ENTRY_TAG
        },
        .EntryHandle = ST_POLICY_ENTRY_HANDLE
    };
    SecAccessRef access = NULL;
    OSStatus status = SecAccessCreateFromOwnerAndACL(&owner, 1, &entry, &access);
    free(decrypt_subject);
    free(owner_subject);
    if (status != errSecSuccess || access == NULL) {
        if (access != NULL) CFRelease(access);
        return ST_POLICY_NATIVE_UNAVAILABLE;
    }
    *out_access = access;
    return ST_POLICY_OK;
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

void st_policy_snapshot_release(st_policy_snapshot **snapshot)
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

st_policy_result st_policy_snapshot_copy(SecAccessRef access,
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
    st_policy_snapshot *snapshot = policy_calloc(1, sizeof *snapshot);
    if (snapshot == NULL) return ST_POLICY_NATIVE_UNAVAILABLE;
    status = SecAccessGetOwnerAndACL(access, &snapshot->owner,
                                    &snapshot->entry_count, &snapshot->entries);
    bool bounded = snapshot_collect(snapshot);
    if (status != errSecSuccess || !bounded) {
        st_policy_snapshot_release(&snapshot);
        return status != errSecSuccess ? ST_POLICY_NATIVE_UNAVAILABLE : ST_POLICY_UNSAFE;
    }
    *out_snapshot = snapshot;
    return ST_POLICY_OK;
}

st_policy_result st_policy_snapshot_view(const st_policy_snapshot *snapshot,
                                        st_policy_view *out_view)
{
    if (out_view == NULL) return ST_POLICY_UNSAFE;
    *out_view = (st_policy_view){0};
    if (snapshot == NULL) return ST_POLICY_UNSAFE;
    *out_view = (st_policy_view){
        .owner = snapshot->owner,
        .entry_count = snapshot->entry_count,
        .entries = snapshot->entries
    };
    return ST_POLICY_OK;
}

#pragma clang diagnostic pop
