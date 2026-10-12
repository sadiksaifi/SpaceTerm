#include "prototype.h"

#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Same public file-based Keychain/CSSM deprecation exception as prototype.h.
 * These tests use only in-memory requirements and access objects. They neither
 * enroll items nor prove native credential retrieval. Never install an ASan
 * build as a secret-bearing executable.
 */
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"

#if defined(ST_POLICY_TEST_ALLOCATIONS)
static size_t allocation_budget = SIZE_MAX;

void *st_policy_test_calloc(size_t count, size_t size)
{
    if (allocation_budget == 0) return NULL;
    if (allocation_budget != SIZE_MAX) --allocation_budget;
    return calloc(count, size);
}
#endif

static const uint8_t legacy_hash[ST_POLICY_LEGACY_HASH_BYTES] = {
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a,
    0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14
};
static const char documentary_path[] = "/synthetic/spaceterm-policy-fixture";

/* The certificate fingerprint is synthetic requirement text, not an identity.
 * No certificate, private key, signature, or executable is created or checked.
 */
#define APPROVED_REQUIREMENT \
    "identifier \"io.github.sadiksaifi.spaceterm-development.server.launcher\" " \
    "and certificate leaf = H\"00112233445566778899aabbccddeeff00112233\""
#define WRONG_REQUIREMENT \
    "identifier \"io.github.sadiksaifi.spaceterm-development.other\" " \
    "and certificate leaf = H\"00112233445566778899aabbccddeeff00112233\""

_Static_assert(sizeof documentary_path <= ST_POLICY_MAX_PATH_BYTES,
               "fixture path must fit the fixed budget");
_Static_assert(sizeof ST_POLICY_ENTRY_TAG <= sizeof(CSSM_STRING),
               "fixture tag must fit the public CSSM tag");

typedef struct {
    SecRequirementRef expected;
    CFDataRef expected_data;
    CFDataRef subject_data;
    CFDataRef wrong_data;
} requirements;

typedef struct {
    CSSM_LIST list;
    CSSM_LIST_ELEMENT nodes[5];
    uint8_t hash[ST_POLICY_LEGACY_HASH_BYTES];
    uint8_t path[ST_POLICY_MAX_PATH_BYTES + 1];
    uint8_t requirement[ST_POLICY_MAX_REQUIREMENT_BYTES + 1];
} signature_subject;

typedef struct {
    CSSM_LIST threshold;
    CSSM_LIST_ELEMENT threshold_nodes[5];
    CSSM_LIST_ELEMENT process_nodes[2];
    CSSM_ACL_PROCESS_SUBJECT_SELECTOR selector;
    signature_subject signature;
} composite_subject;

typedef struct {
    composite_subject owner_subject;
    composite_subject decrypt_subject;
    CSSM_ACL_OWNER_PROTOTYPE owner;
    CSSM_ACL_ENTRY_INFO entries[2];
    CSSM_ACL_AUTHORIZATION_TAG authorizations[2];
    CSSM_LIST_ELEMENT scratch[ST_POLICY_MAX_NODES + 1];
    CSSM_ACL_KEYCHAIN_PROMPT_SELECTOR prompt_selector;
    uint8_t extra_byte;
    st_policy_view view;
} borrowed_fixture;

typedef struct {
    st_policy_result actual;
    bool passed;
} test_result;

static void requirements_release(requirements *value)
{
    if (value->expected != NULL) CFRelease(value->expected);
    if (value->expected_data != NULL) CFRelease(value->expected_data);
    if (value->subject_data != NULL) CFRelease(value->subject_data);
    if (value->wrong_data != NULL) CFRelease(value->wrong_data);
    *value = (requirements){0};
}

static bool compile_data(CFStringRef text, CFDataRef *out_data)
{
    SecRequirementRef requirement = NULL;
    *out_data = NULL;
    OSStatus status = SecRequirementCreateWithString(text, kSecCSDefaultFlags,
                                                    &requirement);
    bool ok = status == errSecSuccess && requirement != NULL;
    if (ok) {
        status = SecRequirementCopyData(requirement, kSecCSDefaultFlags, out_data);
        ok = status == errSecSuccess && *out_data != NULL;
    }
    if (requirement != NULL) CFRelease(requirement);
    return ok;
}

static bool requirements_create(requirements *value)
{
    *value = (requirements){0};
    OSStatus status = SecRequirementCreateWithString(CFSTR(APPROVED_REQUIREMENT),
                                                    kSecCSDefaultFlags,
                                                    &value->expected);
    if (status != errSecSuccess || value->expected == NULL) return false;
    status = SecRequirementCopyData(value->expected, kSecCSDefaultFlags,
                                    &value->expected_data);
    if (status != errSecSuccess || value->expected_data == NULL) return false;

    /* Compile the borrowed subject independently of the expected requirement.
     * Never derive expected identity from the candidate subject itself.
     */
    if (!compile_data(CFSTR(APPROVED_REQUIREMENT), &value->subject_data) ||
        !compile_data(CFSTR(WRONG_REQUIREMENT), &value->wrong_data)) return false;
    CFIndex subject_length = CFDataGetLength(value->subject_data);
    CFIndex wrong_length = CFDataGetLength(value->wrong_data);
    return subject_length > 0 && subject_length <= ST_POLICY_MAX_REQUIREMENT_BYTES &&
           wrong_length > 0 && wrong_length <= ST_POLICY_MAX_REQUIREMENT_BYTES &&
           CFEqual(value->subject_data, value->expected_data) &&
           !CFEqual(value->wrong_data, value->expected_data);
}

static void word(CSSM_LIST_ELEMENT *node, CSSM_WORDID_TYPE value)
{
    *node = (CSSM_LIST_ELEMENT){
        .WordID = value,
        .ElementType = CSSM_LIST_ELEMENT_WORDID
    };
}

static void datum(CSSM_LIST_ELEMENT *node, void *data, size_t length)
{
    *node = (CSSM_LIST_ELEMENT){
        .ElementType = CSSM_LIST_ELEMENT_DATUM,
        .Element.Word = {.Length = length, .Data = data}
    };
}

static void sublist(CSSM_LIST_ELEMENT *node, CSSM_LIST list)
{
    *node = (CSSM_LIST_ELEMENT){
        .ElementType = CSSM_LIST_ELEMENT_SUBLIST,
        .Element.Sublist = list
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

static void signature_create(signature_subject *subject, CFDataRef data)
{
    memcpy(subject->hash, legacy_hash, sizeof legacy_hash);
    memcpy(subject->path, documentary_path, sizeof documentary_path);
    size_t length = (size_t)CFDataGetLength(data);
    memcpy(subject->requirement, CFDataGetBytePtr(data), length);
    word(&subject->nodes[0], CSSM_ACL_SUBJECT_TYPE_CODE_SIGNATURE);
    word(&subject->nodes[1], CSSM_ACL_CODE_SIGNATURE_OSX);
    datum(&subject->nodes[2], subject->hash, sizeof subject->hash);
    datum(&subject->nodes[3], subject->path, sizeof documentary_path);
    datum(&subject->nodes[4], subject->requirement, length);
    subject->list = linked_list(subject->nodes, 5);
}

static void composite_create(composite_subject *subject, CFDataRef data)
{
    subject->selector = (CSSM_ACL_PROCESS_SUBJECT_SELECTOR){
        .version = CSSM_ACL_PROCESS_SELECTOR_CURRENT_VERSION,
        .mask = CSSM_ACL_MATCH_UID,
        .uid = 0,
        .gid = 0
    };
    word(&subject->process_nodes[0], CSSM_ACL_SUBJECT_TYPE_PROCESS);
    datum(&subject->process_nodes[1], &subject->selector, sizeof subject->selector);
    CSSM_LIST process = linked_list(subject->process_nodes, 2);
    signature_create(&subject->signature, data);
    word(&subject->threshold_nodes[0], CSSM_ACL_SUBJECT_TYPE_THRESHOLD);
    word(&subject->threshold_nodes[1], 2);
    word(&subject->threshold_nodes[2], 2);
    sublist(&subject->threshold_nodes[3], process);
    sublist(&subject->threshold_nodes[4], subject->signature.list);
    subject->threshold = linked_list(subject->threshold_nodes, 5);
}

static void fixture_create(borrowed_fixture *fixture, const requirements *value)
{
    memset(fixture, 0, sizeof *fixture);
    composite_create(&fixture->owner_subject, value->subject_data);
    composite_create(&fixture->decrypt_subject, value->subject_data);
    fixture->owner.TypedSubject = fixture->owner_subject.threshold;
    fixture->owner.Delegate = CSSM_FALSE;
    fixture->authorizations[0] = CSSM_ACL_AUTHORIZATION_DECRYPT;
    fixture->entries[0].EntryPublicInfo.TypedSubject = fixture->decrypt_subject.threshold;
    fixture->entries[0].EntryPublicInfo.Delegate = CSSM_FALSE;
    fixture->entries[0].EntryPublicInfo.Authorization = (CSSM_AUTHORIZATIONGROUP){
        .NumberOfAuthTags = 1,
        .AuthTags = fixture->authorizations
    };
    memcpy(fixture->entries[0].EntryPublicInfo.EntryTag, ST_POLICY_ENTRY_TAG,
           sizeof ST_POLICY_ENTRY_TAG);
    fixture->entries[0].EntryHandle = ST_POLICY_ENTRY_HANDLE;
    fixture->view = (st_policy_view){
        .owner = &fixture->owner,
        .entry_count = 1,
        .entries = fixture->entries
    };
}

/* Independent literal assertions for a native round trip. These bounded reads
 * do not replace the module validator or certify a persisted Keychain policy.
 */
static bool exact_list(const CSSM_LIST *list, size_t count,
                       const CSSM_LIST_ELEMENT **nodes)
{
    if (list == NULL || list->ListType != CSSM_LIST_TYPE_UNKNOWN) return false;
    const CSSM_LIST_ELEMENT *node = list->Head;
    for (size_t i = 0; i < count; ++i) {
        if (node == NULL) return false;
        for (size_t j = 0; j < i; ++j) {
            if (nodes[j] == node) return false;
        }
        nodes[i] = node;
        node = node->NextElement;
    }
    return node == NULL && list->Tail == nodes[count - 1];
}

static bool is_word(const CSSM_LIST_ELEMENT *node, CSSM_WORDID_TYPE value)
{
    return node->ElementType == CSSM_LIST_ELEMENT_WORDID && node->WordID == value;
}

static bool is_data(const CSSM_LIST_ELEMENT *node, const void *data, size_t length)
{
    return node->ElementType == CSSM_LIST_ELEMENT_DATUM &&
           node->Element.Word.Length == length && node->Element.Word.Data != NULL &&
           memcmp(node->Element.Word.Data, data, length) == 0;
}

static bool process_matches(const CSSM_LIST *list)
{
    const CSSM_LIST_ELEMENT *nodes[2];
    if (!exact_list(list, 2, nodes) ||
        !is_word(nodes[0], CSSM_ACL_SUBJECT_TYPE_PROCESS) ||
        nodes[1]->ElementType != CSSM_LIST_ELEMENT_DATUM ||
        nodes[1]->Element.Word.Length != sizeof(CSSM_ACL_PROCESS_SUBJECT_SELECTOR) ||
        nodes[1]->Element.Word.Data == NULL) return false;
    CSSM_ACL_PROCESS_SUBJECT_SELECTOR selector;
    memcpy(&selector, nodes[1]->Element.Word.Data, sizeof selector);
    return selector.version == CSSM_ACL_PROCESS_SELECTOR_CURRENT_VERSION &&
           selector.mask == CSSM_ACL_MATCH_UID && selector.uid == 0 && selector.gid == 0;
}

static bool signature_matches(const CSSM_LIST *list, const requirements *value)
{
    const CSSM_LIST_ELEMENT *nodes[5];
    return exact_list(list, 5, nodes) &&
           is_word(nodes[0], CSSM_ACL_SUBJECT_TYPE_CODE_SIGNATURE) &&
           is_word(nodes[1], CSSM_ACL_CODE_SIGNATURE_OSX) &&
           is_data(nodes[2], legacy_hash, sizeof legacy_hash) &&
           is_data(nodes[3], documentary_path, sizeof documentary_path) &&
           is_data(nodes[4], CFDataGetBytePtr(value->expected_data),
                   (size_t)CFDataGetLength(value->expected_data));
}

static bool threshold_matches(const CSSM_LIST *list, const requirements *value)
{
    const CSSM_LIST_ELEMENT *nodes[5];
    return exact_list(list, 5, nodes) &&
           is_word(nodes[0], CSSM_ACL_SUBJECT_TYPE_THRESHOLD) &&
           is_word(nodes[1], 2) && is_word(nodes[2], 2) &&
           nodes[3]->ElementType == CSSM_LIST_ELEMENT_SUBLIST &&
           nodes[4]->ElementType == CSSM_LIST_ELEMENT_SUBLIST &&
           process_matches(&nodes[3]->Element.Sublist) &&
           signature_matches(&nodes[4]->Element.Sublist, value);
}

static bool policy_matches(const st_policy_view *view, const requirements *value)
{
    if (view->owner == NULL || view->entries == NULL || view->entry_count != 1) return false;
    const CSSM_ACL_ENTRY_INFO *entry = &view->entries[0];
    const CSSM_ACL_ENTRY_PROTOTYPE *policy = &entry->EntryPublicInfo;
    return view->owner->Delegate == CSSM_FALSE && policy->Delegate == CSSM_FALSE &&
           policy->Authorization.NumberOfAuthTags == 1 &&
           policy->Authorization.AuthTags != NULL &&
           policy->Authorization.AuthTags[0] == CSSM_ACL_AUTHORIZATION_DECRYPT &&
           policy->TimeRange.StartDate.Length == 0 && policy->TimeRange.StartDate.Data == NULL &&
           policy->TimeRange.EndDate.Length == 0 && policy->TimeRange.EndDate.Data == NULL &&
           entry->EntryHandle == ST_POLICY_ENTRY_HANDLE &&
           memcmp(policy->EntryTag, ST_POLICY_ENTRY_TAG, sizeof ST_POLICY_ENTRY_TAG) == 0 &&
           threshold_matches(&view->owner->TypedSubject, value) &&
           threshold_matches(&policy->TypedSubject, value);
}

/* Twelve list nodes and four active DATUM buffers per admitted subject.
 * Never read the inactive union of WORDID nodes or treat Tail as an allocation.
 */
static bool subject_storage(const CSSM_LIST *list, const void **storage)
{
    const CSSM_LIST_ELEMENT *threshold[5], *process[2], *signature[5];
    if (!exact_list(list, 5, threshold) ||
        threshold[3]->ElementType != CSSM_LIST_ELEMENT_SUBLIST ||
        threshold[4]->ElementType != CSSM_LIST_ELEMENT_SUBLIST ||
        !exact_list(&threshold[3]->Element.Sublist, 2, process) ||
        !exact_list(&threshold[4]->Element.Sublist, 5, signature) ||
        process[1]->ElementType != CSSM_LIST_ELEMENT_DATUM) return false;
    size_t count = 0;
    for (size_t i = 0; i < 5; ++i) storage[count++] = threshold[i];
    for (size_t i = 0; i < 2; ++i) storage[count++] = process[i];
    for (size_t i = 0; i < 5; ++i) storage[count++] = signature[i];
    storage[count++] = process[1]->Element.Word.Data;
    for (size_t i = 2; i < 5; ++i) {
        if (signature[i]->ElementType != CSSM_LIST_ELEMENT_DATUM) return false;
        storage[count++] = signature[i]->Element.Word.Data;
    }
    return count == 16;
}

static bool subjects_disjoint(const CSSM_LIST *left, const CSSM_LIST *right)
{
    const void *a[16], *b[16];
    if (!subject_storage(left, a) || !subject_storage(right, b)) return false;
    for (size_t i = 0; i < 16; ++i) {
        if (a[i] == NULL || b[i] == NULL) return false;
        for (size_t j = 0; j < 16; ++j) {
            if (a[i] == b[j]) return false;
        }
    }
    return true;
}

static test_result inspect_snapshot(st_policy_snapshot *snapshot,
                                    const requirements *value, st_policy_view *view)
{
    *view = (st_policy_view){0};
    st_policy_result result = st_policy_snapshot_view(snapshot, view);
    if (result != ST_POLICY_OK) return (test_result){result, false};
    result = st_policy_validate(view, value->expected);
    bool matches = result == ST_POLICY_OK && policy_matches(view, value) &&
                   subjects_disjoint(&view->owner->TypedSubject,
                                     &view->entries[0].EntryPublicInfo.TypedSubject);
    return (test_result){result, matches};
}

static bool release_snapshot(st_policy_snapshot **snapshot)
{
    st_policy_snapshot_release(snapshot);
    bool cleared = *snapshot == NULL;
    st_policy_snapshot_release(snapshot);
    st_policy_snapshot_release(NULL);
    return cleared && *snapshot == NULL;
}

static test_result roundtrips(const requirements *value, size_t repetitions)
{
    for (size_t i = 0; i < repetitions; ++i) {
        borrowed_fixture fixture;
        fixture_create(&fixture, value);
        SecAccessRef access = NULL;
        st_policy_snapshot *snapshot = NULL;
        st_policy_result result = st_policy_access_create(&fixture.decrypt_subject.signature.list,
                                                         value->expected, &access);
        test_result outcome = {result, false};
        if (result == ST_POLICY_OK && access != NULL) {
            result = st_policy_snapshot_copy(access, &snapshot);
            outcome.actual = result;
            if (result == ST_POLICY_OK && snapshot != NULL) {
                st_policy_view view;
                outcome = inspect_snapshot(snapshot, value, &view);
            }
        }
        bool cleared = release_snapshot(&snapshot);
        if (access != NULL) CFRelease(access);
        outcome.passed = outcome.passed && cleared;
        if (!outcome.passed) return outcome;
    }
    return (test_result){ST_POLICY_OK, true};
}

static test_result constructor_roundtrip(const requirements *value)
{
    return roundtrips(value, 1);
}

static test_result repeated_roundtrip(const requirements *value)
{
    return roundtrips(value, 16);
}

static test_result borrowed_policy(const requirements *value)
{
    borrowed_fixture fixture;
    fixture_create(&fixture, value);
    st_policy_result result = st_policy_validate(&fixture.view, value->expected);
    return (test_result){result, result == ST_POLICY_OK};
}

static test_result shared_datum_storage(const requirements *value)
{
    borrowed_fixture fixture;
    fixture_create(&fixture, value);
    for (size_t i = 2; i < 5; ++i) {
        fixture.decrypt_subject.signature.nodes[i].Element.Word =
            fixture.owner_subject.signature.nodes[i].Element.Word;
    }
    fixture.decrypt_subject.process_nodes[1].Element.Word =
        fixture.owner_subject.process_nodes[1].Element.Word;
    st_policy_result result = st_policy_validate(&fixture.view, value->expected);
    return (test_result){result, result == ST_POLICY_OK};
}

static test_result unaligned_selector(const requirements *value)
{
    borrowed_fixture fixture;
    fixture_create(&fixture, value);
    _Alignas(CSSM_ACL_PROCESS_SUBJECT_SELECTOR)
        uint8_t bytes[sizeof(CSSM_ACL_PROCESS_SUBJECT_SELECTOR) + 1];
    memcpy(bytes + 1, &fixture.owner_subject.selector, sizeof fixture.owner_subject.selector);
    fixture.owner_subject.process_nodes[1].Element.Word.Data = bytes + 1;
    st_policy_result result = st_policy_validate(&fixture.view, value->expected);
    return (test_result){result, result == ST_POLICY_OK};
}

static bool metadata_matches(const CSSM_LIST *list, const signature_subject *subject)
{
    const CSSM_LIST_ELEMENT *threshold[5], *signature[5];
    return exact_list(list, 5, threshold) &&
           threshold[4]->ElementType == CSSM_LIST_ELEMENT_SUBLIST &&
           exact_list(&threshold[4]->Element.Sublist, 5, signature) &&
           is_data(signature[2], subject->hash, ST_POLICY_LEGACY_HASH_BYTES) &&
           is_data(signature[3], subject->path, subject->nodes[3].Element.Word.Length);
}

static test_result documentary_metadata_boundaries(const requirements *value)
{
    const size_t lengths[] = {1, ST_POLICY_MAX_PATH_BYTES};
    for (size_t i = 0; i < sizeof lengths / sizeof lengths[0]; ++i) {
        borrowed_fixture fixture;
        fixture_create(&fixture, value);
        signature_subject *subject = &fixture.decrypt_subject.signature;
        memset(subject->hash, 0xff, sizeof subject->hash);
        memset(subject->path, 'p', lengths[i]);
        subject->path[lengths[i] - 1] = 0;
        subject->nodes[3].Element.Word.Length = lengths[i];
        SecAccessRef access = NULL;
        st_policy_snapshot *snapshot = NULL;
        st_policy_result result = st_policy_access_create(&subject->list, value->expected, &access);
        test_result outcome = {result, false};
        if (result == ST_POLICY_OK && access != NULL) {
            result = st_policy_snapshot_copy(access, &snapshot);
            outcome.actual = result;
            if (result == ST_POLICY_OK && snapshot != NULL) {
                st_policy_view view;
                result = st_policy_snapshot_view(snapshot, &view);
                if (result == ST_POLICY_OK) result = st_policy_validate(&view, value->expected);
                outcome.actual = result;
                outcome.passed = result == ST_POLICY_OK &&
                    metadata_matches(&view.owner->TypedSubject, subject) &&
                    metadata_matches(&view.entries[0].EntryPublicInfo.TypedSubject, subject) &&
                    subjects_disjoint(&view.owner->TypedSubject,
                                      &view.entries[0].EntryPublicInfo.TypedSubject);
            }
        }
        bool cleared = release_snapshot(&snapshot);
        if (access != NULL) CFRelease(access);
        outcome.passed = outcome.passed && cleared;
        if (!outcome.passed) return outcome;
    }
    return (test_result){ST_POLICY_OK, true};
}

static test_result null_argument_contract(const requirements *value)
{
    borrowed_fixture fixture;
    fixture_create(&fixture, value);
    SecAccessRef access = (SecAccessRef)value->expected;
    st_policy_result result = st_policy_access_create(NULL, value->expected, &access);
    bool passed = result == ST_POLICY_UNSAFE && access == NULL;
    access = (SecAccessRef)value->expected;
    result = st_policy_access_create(&fixture.decrypt_subject.signature.list, NULL, &access);
    passed = passed && result == ST_POLICY_UNSAFE && access == NULL;
    result = st_policy_access_create(&fixture.decrypt_subject.signature.list, value->expected, NULL);
    passed = passed && result == ST_POLICY_UNSAFE;
    result = st_policy_validate(NULL, value->expected);
    passed = passed && result == ST_POLICY_UNSAFE;
    result = st_policy_validate(&fixture.view, NULL);
    passed = passed && result == ST_POLICY_UNSAFE;
    st_policy_snapshot *snapshot = (st_policy_snapshot *)(void *)&fixture;
    result = st_policy_snapshot_copy(NULL, &snapshot);
    passed = passed && result == ST_POLICY_UNSAFE && snapshot == NULL;
    result = st_policy_snapshot_copy(NULL, NULL);
    passed = passed && result == ST_POLICY_UNSAFE;
    st_policy_view view = fixture.view;
    result = st_policy_snapshot_view(NULL, &view);
    passed = passed && result == ST_POLICY_UNSAFE && view.owner == NULL &&
             view.entry_count == 0 && view.entries == NULL;
    result = st_policy_snapshot_view(NULL, NULL);
    passed = passed && result == ST_POLICY_UNSAFE;
    return (test_result){result, passed};
}

static test_result borrowed_lifetime(const requirements *value)
{
    borrowed_fixture fixture;
    fixture_create(&fixture, value);
    SecAccessRef access = NULL;
    st_policy_snapshot *snapshot = NULL;
    st_policy_result result = st_policy_access_create(&fixture.decrypt_subject.signature.list,
                                                     value->expected, &access);
    test_result outcome = {result, false};
    if (result == ST_POLICY_OK && access != NULL) {
        /* Destroy caller storage, not framework-owned memory. */
        memset(&fixture, 0, sizeof fixture);
        result = st_policy_snapshot_copy(access, &snapshot);
        outcome.actual = result;
        CFRelease(access);
        access = NULL;
        if (result == ST_POLICY_OK && snapshot != NULL) {
            st_policy_view view;
            outcome = inspect_snapshot(snapshot, value, &view);
        }
    }
    bool cleared = release_snapshot(&snapshot);
    if (access != NULL) CFRelease(access);
    outcome.passed = outcome.passed && cleared;
    return outcome;
}

static test_result snapshot_independence(const requirements *value)
{
    borrowed_fixture fixture;
    fixture_create(&fixture, value);
    SecAccessRef access = NULL;
    st_policy_snapshot *first = NULL, *second = NULL;
    st_policy_view a = {0}, b = {0};
    st_policy_result result = st_policy_access_create(&fixture.decrypt_subject.signature.list,
                                                     value->expected, &access);
    test_result outcome = {result, false};
    if (result != ST_POLICY_OK || access == NULL) goto cleanup;
    result = st_policy_snapshot_copy(access, &first);
    outcome.actual = result;
    if (result != ST_POLICY_OK || first == NULL) goto cleanup;
    result = st_policy_snapshot_copy(access, &second);
    outcome.actual = result;
    if (result != ST_POLICY_OK || second == NULL) goto cleanup;
    CFRelease(access);
    access = NULL;
    outcome = inspect_snapshot(first, value, &a);
    if (!outcome.passed) goto cleanup;
    outcome = inspect_snapshot(second, value, &b);
    if (!outcome.passed) goto cleanup;
    bool disjoint = a.owner != b.owner && a.entries != b.entries && first != second &&
                    subjects_disjoint(&a.owner->TypedSubject, &b.owner->TypedSubject) &&
                    subjects_disjoint(&a.owner->TypedSubject,
                                      &b.entries[0].EntryPublicInfo.TypedSubject) &&
                    subjects_disjoint(&a.entries[0].EntryPublicInfo.TypedSubject,
                                      &b.owner->TypedSubject) &&
                    subjects_disjoint(&a.entries[0].EntryPublicInfo.TypedSubject,
                                      &b.entries[0].EntryPublicInfo.TypedSubject);
    bool first_cleared = release_snapshot(&first);
    a = (st_policy_view){0};
    outcome = inspect_snapshot(second, value, &b);
    outcome.passed = outcome.passed && disjoint && first_cleared;
cleanup:
    {
        bool first_cleared_at_exit = release_snapshot(&first);
        bool second_cleared_at_exit = release_snapshot(&second);
        if (access != NULL) CFRelease(access);
        outcome.passed = outcome.passed && first_cleared_at_exit && second_cleared_at_exit;
    }
    return outcome;
}

#if defined(ST_POLICY_TEST_ALLOCATIONS)
static test_result allocation_failures(const requirements *value)
{
    borrowed_fixture fixture;
    fixture_create(&fixture, value);
    bool passed = true;
    st_policy_result result = ST_POLICY_OK;
    for (size_t allowed = 0; allowed < 2; ++allowed) {
        SecAccessRef access = (SecAccessRef)value->expected;
        allocation_budget = allowed;
        result = st_policy_access_create(&fixture.decrypt_subject.signature.list,
                                         value->expected, &access);
        allocation_budget = SIZE_MAX;
        passed = passed && result == ST_POLICY_NATIVE_UNAVAILABLE && access == NULL;
        if (access != NULL && access != (SecAccessRef)value->expected) CFRelease(access);
    }
    SecAccessRef access = NULL;
    st_policy_snapshot *first = NULL, *second = NULL;
    result = st_policy_access_create(&fixture.decrypt_subject.signature.list,
                                     value->expected, &access);
    if (result != ST_POLICY_OK || access == NULL) {
        if (access != NULL) CFRelease(access);
        return (test_result){result, false};
    }
    result = st_policy_snapshot_copy(access, &first);
    if (result != ST_POLICY_OK || first == NULL) {
        (void)release_snapshot(&first);
        CFRelease(access);
        return (test_result){result, false};
    }
    allocation_budget = 0;
    result = st_policy_snapshot_copy(access, &second);
    allocation_budget = SIZE_MAX;
    passed = passed && result == ST_POLICY_NATIVE_UNAVAILABLE && second == NULL;
    st_policy_view view;
    test_result existing = inspect_snapshot(first, value, &view);
    passed = passed && existing.passed;
    bool first_cleared = release_snapshot(&first);
    bool second_cleared = release_snapshot(&second);
    CFRelease(access);
    test_result recovered = roundtrips(value, 1);
    return (test_result){result, passed && first_cleared && second_cleared && recovered.passed};
}
#endif

typedef enum {
    WEAK_THRESHOLD, WRONG_UID, WRONG_MASK, WRONG_REQUIREMENT_DATA,
    WRONG_PROCESS_VERSION, WRONG_PROCESS_GID, PROMPT_ALTERNATIVE,
    SECOND_DECRYPT, ANY_AUTHORIZATION, DELEGATION, MALFORMED_ELEMENT_KIND,
    CYCLIC_NEXT, CYCLIC_SUBLIST, OVERBOUND_NODES, OVERBOUND_DEPTH,
    OVERBOUND_REQUIREMENT, OVERBOUND_PATH, MISSING_REQUIREMENT, HASH_ONLY,
    MALFORMED_REQUIREMENT, UNTERMINATED_PATH, WRONG_HASH_LENGTH,
    MISSING_DATUM_STORAGE, INCONSISTENT_TAIL, NONEMPTY_VALIDITY,
    OTHER_AUTHORIZATION, EMPTY_AUTHORIZATION, PROCESS_ONLY_OWNER,
    EXTRA_SIGNATURE_ELEMENT, SHARED_SUBJECT_NODES, UNKNOWN_ELEMENT_KIND,
    UNKNOWN_LIST_TYPE, MISSING_HEAD, EMBEDDED_PATH_NUL, EMPTY_REQUIREMENT,
    TRAILING_REQUIREMENT_DATA, WRONG_ENTRY_TAG, WRONG_ENTRY_HANDLE,
    NONEMPTY_END_VALIDITY
} mutation;

typedef struct {
    const char *name;
    mutation change;
    bool owner;
} negative_case;

/* Mutations keep all non-NULL pointers readable. Every overbound buffer and node
 * chain has real backing storage. This is not a forged native allocation ledger.
 */
static bool mutate(borrowed_fixture *fixture, const requirements *value,
                   const negative_case *test)
{
    composite_subject *subject = test->owner ? &fixture->owner_subject : &fixture->decrypt_subject;
    CSSM_LIST *root = test->owner ? &fixture->owner.TypedSubject :
                                 &fixture->entries[0].EntryPublicInfo.TypedSubject;
    signature_subject *signature = &subject->signature;
    bool bad_signature = false;
    switch (test->change) {
    case WEAK_THRESHOLD:
        subject->threshold_nodes[1].WordID = 1;
        break;
    case WRONG_UID:
        subject->selector.uid = 1000;
        break;
    case WRONG_MASK:
        subject->selector.mask = test->owner ? CSSM_ACL_MATCH_UID | CSSM_ACL_MATCH_HONOR_ROOT :
                                              CSSM_ACL_MATCH_GID;
        break;
    case WRONG_REQUIREMENT_DATA: {
        size_t length = (size_t)CFDataGetLength(value->wrong_data);
        memcpy(signature->requirement, CFDataGetBytePtr(value->wrong_data), length);
        signature->nodes[4].Element.Word.Length = length;
        bad_signature = true;
        break;
    }
    case WRONG_PROCESS_VERSION:
        subject->selector.version = 0;
        break;
    case WRONG_PROCESS_GID:
        subject->selector.gid = 1000;
        break;
    case PROMPT_ALTERNATIVE: {
        fixture->prompt_selector.version = CSSM_ACL_KEYCHAIN_PROMPT_CURRENT_VERSION;
        word(&fixture->scratch[1], CSSM_ACL_SUBJECT_TYPE_KEYCHAIN_PROMPT);
        datum(&fixture->scratch[2], &fixture->prompt_selector, sizeof fixture->prompt_selector);
        CSSM_LIST prompt = linked_list(&fixture->scratch[1], 2);
        sublist(&fixture->scratch[0], prompt);
        subject->threshold_nodes[2].WordID = 3;
        subject->threshold_nodes[4].NextElement = &fixture->scratch[0];
        root->Tail = &fixture->scratch[0];
        break;
    }
    case SECOND_DECRYPT:
        /* Deliberately borrowed aliases, never passed to snapshot_release. */
        fixture->entries[1] = fixture->entries[0];
        fixture->entries[1].EntryHandle = 2;
        fixture->view.entry_count = 2;
        break;
    case ANY_AUTHORIZATION:
        fixture->authorizations[0] = CSSM_ACL_AUTHORIZATION_ANY;
        break;
    case DELEGATION:
        if (test->owner) fixture->owner.Delegate = CSSM_TRUE;
        else fixture->entries[0].EntryPublicInfo.Delegate = CSSM_TRUE;
        break;
    case MALFORMED_ELEMENT_KIND:
        subject->threshold_nodes[1].ElementType = CSSM_LIST_ELEMENT_DATUM;
        break;
    case CYCLIC_NEXT:
        signature->nodes[4].NextElement = &signature->nodes[0];
        bad_signature = true;
        break;
    case CYCLIC_SUBLIST:
        sublist(&signature->nodes[4], signature->list);
        bad_signature = true;
        break;
    case OVERBOUND_NODES:
        for (size_t i = 0; i < ST_POLICY_MAX_NODES + 1; ++i) word(&fixture->scratch[i], 0);
        (void)linked_list(fixture->scratch, ST_POLICY_MAX_NODES + 1);
        signature->nodes[4].NextElement = &fixture->scratch[0];
        signature->list.Tail = &fixture->scratch[ST_POLICY_MAX_NODES];
        bad_signature = true;
        break;
    case OVERBOUND_DEPTH: {
        CSSM_LIST nested = {0};
        for (size_t i = ST_POLICY_MAX_DEPTH + 1; i > 0; --i) {
            if (i == ST_POLICY_MAX_DEPTH + 1) word(&fixture->scratch[i - 1], CSSM_ACL_SUBJECT_TYPE_ANY);
            else sublist(&fixture->scratch[i - 1], nested);
            nested = linked_list(&fixture->scratch[i - 1], 1);
        }
        subject->threshold_nodes[3].Element.Sublist = nested;
        break;
    }
    case OVERBOUND_REQUIREMENT:
        signature->nodes[4].Element.Word.Length = sizeof signature->requirement;
        bad_signature = true;
        break;
    case OVERBOUND_PATH:
        memset(signature->path, 'p', sizeof signature->path);
        signature->path[sizeof signature->path - 1] = 0;
        signature->nodes[3].Element.Word.Length = sizeof signature->path;
        bad_signature = true;
        break;
    case MISSING_REQUIREMENT:
        signature->nodes[3].NextElement = NULL;
        signature->list.Tail = &signature->nodes[3];
        bad_signature = true;
        break;
    case HASH_ONLY:
        signature->nodes[2].NextElement = NULL;
        signature->list.Tail = &signature->nodes[2];
        bad_signature = true;
        break;
    case MALFORMED_REQUIREMENT:
        signature->requirement[0] ^= 0xff;
        bad_signature = true;
        break;
    case UNTERMINATED_PATH:
        signature->path[sizeof documentary_path - 1] = 'x';
        bad_signature = true;
        break;
    case WRONG_HASH_LENGTH:
        signature->nodes[2].Element.Word.Length = ST_POLICY_LEGACY_HASH_BYTES - 1;
        bad_signature = true;
        break;
    case MISSING_DATUM_STORAGE:
        signature->nodes[4].Element.Word.Data = NULL;
        bad_signature = true;
        break;
    case INCONSISTENT_TAIL:
        signature->list.Tail = &signature->nodes[3];
        bad_signature = true;
        break;
    case NONEMPTY_VALIDITY:
        fixture->entries[0].EntryPublicInfo.TimeRange.StartDate = (CSSM_DATA){
            .Length = 1, .Data = &fixture->extra_byte
        };
        break;
    case OTHER_AUTHORIZATION:
        fixture->authorizations[1] = CSSM_ACL_AUTHORIZATION_ENCRYPT;
        fixture->entries[0].EntryPublicInfo.Authorization.NumberOfAuthTags = 2;
        break;
    case EMPTY_AUTHORIZATION:
        fixture->entries[0].EntryPublicInfo.Authorization.NumberOfAuthTags = 0;
        fixture->entries[0].EntryPublicInfo.Authorization.AuthTags = NULL;
        break;
    case PROCESS_ONLY_OWNER:
        *root = linked_list(subject->process_nodes, 2);
        break;
    case EXTRA_SIGNATURE_ELEMENT:
        datum(&fixture->scratch[0], &fixture->extra_byte, 1);
        signature->nodes[4].NextElement = &fixture->scratch[0];
        signature->list.Tail = &fixture->scratch[0];
        bad_signature = true;
        break;
    case SHARED_SUBJECT_NODES:
        fixture->entries[0].EntryPublicInfo.TypedSubject = fixture->owner.TypedSubject;
        break;
    case UNKNOWN_ELEMENT_KIND:
        signature->nodes[4].ElementType = UINT32_MAX;
        bad_signature = true;
        break;
    case UNKNOWN_LIST_TYPE:
        signature->list.ListType = CSSM_LIST_TYPE_CUSTOM;
        bad_signature = true;
        break;
    case MISSING_HEAD:
        signature->list.Head = NULL;
        bad_signature = true;
        break;
    case EMBEDDED_PATH_NUL:
        signature->path[1] = 0;
        bad_signature = true;
        break;
    case EMPTY_REQUIREMENT:
        signature->nodes[4].Element.Word.Length = 0;
        bad_signature = true;
        break;
    case TRAILING_REQUIREMENT_DATA: {
        size_t length = signature->nodes[4].Element.Word.Length;
        signature->requirement[length] = 0;
        signature->nodes[4].Element.Word.Length = length + 1;
        bad_signature = true;
        break;
    }
    case WRONG_ENTRY_TAG:
        fixture->entries[0].EntryPublicInfo.EntryTag[0] = 'x';
        break;
    case WRONG_ENTRY_HANDLE:
        fixture->entries[0].EntryHandle = ST_POLICY_ENTRY_HANDLE + 1;
        break;
    case NONEMPTY_END_VALIDITY:
        fixture->entries[0].EntryPublicInfo.TimeRange.EndDate = (CSSM_DATA){
            .Length = 1, .Data = &fixture->extra_byte
        };
        break;
    }
    subject->threshold_nodes[4].Element.Sublist = signature->list;
    return bad_signature;
}

static test_result reject_policy(const requirements *value, const negative_case *test)
{
    borrowed_fixture fixture;
    fixture_create(&fixture, value);
    bool bad_signature = mutate(&fixture, value, test);
    st_policy_result result = st_policy_validate(&fixture.view, value->expected);
    test_result outcome = {result, result == ST_POLICY_UNSAFE};
    if (bad_signature) {
        const CSSM_LIST *candidate = test->owner ? &fixture.owner_subject.signature.list :
                                                &fixture.decrypt_subject.signature.list;
        SecAccessRef access = NULL;
        st_policy_result created = st_policy_access_create(candidate, value->expected, &access);
        bool rejected = created == ST_POLICY_UNSAFE && access == NULL;
        if (outcome.passed && !rejected) outcome.actual = created;
        outcome.passed = outcome.passed && rejected;
        if (access != NULL) CFRelease(access);
    }
    return outcome;
}

static const negative_case negative_cases[] = {
    {"owner-threshold-one-of-two", WEAK_THRESHOLD, true},
    {"decrypt-threshold-one-of-two", WEAK_THRESHOLD, false},
    {"owner-wrong-uid", WRONG_UID, true},
    {"decrypt-wrong-uid", WRONG_UID, false},
    {"owner-wrong-mask", WRONG_MASK, true},
    {"decrypt-wrong-mask", WRONG_MASK, false},
    {"owner-wrong-requirement", WRONG_REQUIREMENT_DATA, true},
    {"decrypt-wrong-requirement", WRONG_REQUIREMENT_DATA, false},
    {"owner-process-version", WRONG_PROCESS_VERSION, true},
    {"decrypt-process-gid", WRONG_PROCESS_GID, false},
    {"prompt-alternative", PROMPT_ALTERNATIVE, false},
    {"second-decrypt-entry", SECOND_DECRYPT, false},
    {"any-authorization", ANY_AUTHORIZATION, false},
    {"owner-delegation", DELEGATION, true},
    {"decrypt-delegation", DELEGATION, false},
    {"malformed-element-kind", MALFORMED_ELEMENT_KIND, false},
    {"cyclic-next-pointer", CYCLIC_NEXT, false},
    {"cyclic-sublist", CYCLIC_SUBLIST, true},
    {"overbound-node-count", OVERBOUND_NODES, false},
    {"overbound-nesting-depth", OVERBOUND_DEPTH, true},
    {"overbound-requirement", OVERBOUND_REQUIREMENT, false},
    {"overbound-documentary-path", OVERBOUND_PATH, false},
    {"missing-serialized-requirement", MISSING_REQUIREMENT, false},
    {"hash-only-code-signature", HASH_ONLY, true},
    {"malformed-serialized-requirement", MALFORMED_REQUIREMENT, false},
    {"unterminated-documentary-path", UNTERMINATED_PATH, true},
    {"wrong-legacy-hash-length", WRONG_HASH_LENGTH, false},
    {"missing-datum-storage", MISSING_DATUM_STORAGE, false},
    {"inconsistent-tail", INCONSISTENT_TAIL, true},
    {"nonempty-validity", NONEMPTY_VALIDITY, false},
    {"other-authorization", OTHER_AUTHORIZATION, false},
    {"empty-authorization", EMPTY_AUTHORIZATION, false},
    {"owner-process-only", PROCESS_ONLY_OWNER, true},
    {"extra-signature-element", EXTRA_SIGNATURE_ELEMENT, false},
    {"shared-owner-decrypt-nodes", SHARED_SUBJECT_NODES, false},
    {"unknown-element-kind", UNKNOWN_ELEMENT_KIND, false},
    {"unknown-signature-list-type", UNKNOWN_LIST_TYPE, false},
    {"missing-signature-head", MISSING_HEAD, true},
    {"embedded-documentary-path-nul", EMBEDDED_PATH_NUL, false},
    {"empty-serialized-requirement", EMPTY_REQUIREMENT, true},
    {"trailing-serialized-requirement-data", TRAILING_REQUIREMENT_DATA, false},
    {"wrong-entry-tag", WRONG_ENTRY_TAG, false},
    {"wrong-entry-handle", WRONG_ENTRY_HANDLE, false},
    {"nonempty-end-validity", NONEMPTY_END_VALIDITY, false}
};

typedef test_result (*positive_test)(const requirements *);
static const struct {
    const char *name;
    positive_test run;
} positive_cases[] = {
    {"valid-borrowed-policy", borrowed_policy},
    {"shared-immutable-datum-storage", shared_datum_storage},
    {"unaligned-process-selector", unaligned_selector},
    {"documentary-metadata-boundaries", documentary_metadata_boundaries},
    {"constructor-native-roundtrip", constructor_roundtrip},
    {"borrowed-signature-lifetime", borrowed_lifetime},
    {"snapshot-copy-independence", snapshot_independence},
    {"repeated-native-roundtrip", repeated_roundtrip}
};

static const char *classification(st_policy_result result)
{
    switch (result) {
    case ST_POLICY_OK: return "ok";
    case ST_POLICY_UNSAFE: return "unsafe";
    case ST_POLICY_NATIVE_UNAVAILABLE: return "native-unavailable";
    }
    return "invalid-result";
}

static void emit_case(const char *name, st_policy_result expected, test_result result)
{
    /* All strings are fixed literals, never native error text or subject data. */
    printf("{\"schema\":1,\"scope\":\"in-memory-policy\",\"case\":\"%s\","
           "\"expected\":\"%s\",\"classification\":\"%s\",\"passed\":%s}\n",
           name, classification(expected), classification(result.actual),
           result.passed ? "true" : "false");
}

int main(void)
{
    requirements value;
    if (!requirements_create(&value)) {
        emit_case("requirement-fixture-setup", ST_POLICY_OK,
                  (test_result){ST_POLICY_NATIVE_UNAVAILABLE, false});
        requirements_release(&value);
        return EXIT_FAILURE;
    }
    test_result contract = null_argument_contract(&value);
    emit_case("null-argument-output-contract", ST_POLICY_UNSAFE, contract);
    size_t count = 1, failed = contract.passed ? 0 : 1;
#if defined(ST_POLICY_TEST_ALLOCATIONS)
    test_result allocation = allocation_failures(&value);
    emit_case("module-allocation-failure-cleanup", ST_POLICY_NATIVE_UNAVAILABLE, allocation);
    ++count;
    if (!allocation.passed) ++failed;
#endif
    for (size_t i = 0; i < sizeof positive_cases / sizeof positive_cases[0]; ++i) {
        test_result result = positive_cases[i].run(&value);
        emit_case(positive_cases[i].name, ST_POLICY_OK, result);
        ++count;
        if (!result.passed) ++failed;
    }
    for (size_t i = 0; i < sizeof negative_cases / sizeof negative_cases[0]; ++i) {
        test_result result = reject_policy(&value, &negative_cases[i]);
        emit_case(negative_cases[i].name, ST_POLICY_UNSAFE, result);
        ++count;
        if (!result.passed) ++failed;
    }
    requirements_release(&value);
    printf("{\"schema\":1,\"scope\":\"in-memory-policy\",\"summary\":true,"
           "\"cases\":%zu,\"failed\":%zu}\n", count, failed);
    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}

#pragma clang diagnostic pop
