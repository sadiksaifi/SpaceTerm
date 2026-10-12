#include "keychain_items.h"
#include "prototype.h"

#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>

/* All policy and requirement objects are in memory. No item API or Keychain
 * mutation is allowed in this binary. The certificate is requirement text,
 * not signing material. Never install this binary as secret-bearing code.
 */
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"

#define RETRIEVER_REQUIREMENT \
    "identifier \"io.github.sadiksaifi.spaceterm-development.server.launcher\" " \
    "and certificate leaf = H\"9e07cd5c15c5c862a21b93ea55d4303bc19b7366\""
#define WRONG_REQUIREMENT \
    "identifier \"io.github.sadiksaifi.spaceterm-development.server\" " \
    "and certificate leaf = H\"9e07cd5c15c5c862a21b93ea55d4303bc19b7366\""

struct st_native_release_permit { bool live; };
struct st_native_locked_secret { uint8_t bytes[32]; };

/* This is the test composition's owned-memory operation, not a native permit
 * producer. The production object refers to the common boundary's implementation.
 */
void st_native_locked_secret_wipe(st_native_locked_secret *destination)
{
    if (destination != NULL) memset(destination, 0, sizeof *destination);
}

typedef struct {
    CSSM_LIST subject;
    CSSM_LIST_ELEMENT threshold[5], process[2], signature[5];
    CSSM_ACL_PROCESS_SUBJECT_SELECTOR selector;
    uint8_t hash[20];
    char path[32];
} subject;

typedef struct {
    st_keychain_test_binding_facts facts;
    uint8_t identity[24];
    st_keychain_test_attributes attributes;
    SecAccessRef access;
    st_keychain_item_binding *binding;
    unsigned guards, disables, opens, resolves, attributes_copied, secrets, rechecks;
    unsigned attributes_released, items_released, keychains_released, allocations_released;
    bool secret_wiped, destination_wiped, interactions_disabled;
    bool framework_allocation_live, framework_live_during_reinspection;
    uint8_t *staging;
    unsigned locks, unlocks, writes;
    bool staging_locked, locked_before_read, locked_during_reinspection, staging_wiped;
    bool write_used_locked_staging;
    bool change_policy_on_read, change_identity_on_read;
    st_native_result guard_result, secret_result, free_result, write_result, lock_result, unlock_result;
    size_t secret_length;
    unsigned invalidate_at_recheck;
} fixture;

static void word(CSSM_LIST_ELEMENT *node, CSSM_WORDID_TYPE value)
{
    *node = (CSSM_LIST_ELEMENT){.ElementType = CSSM_LIST_ELEMENT_WORDID, .WordID = value};
}
static void datum(CSSM_LIST_ELEMENT *node, void *bytes, size_t length)
{
    *node = (CSSM_LIST_ELEMENT){.ElementType = CSSM_LIST_ELEMENT_DATUM,
                              .Element.Word = {.Length = length, .Data = bytes}};
}
static CSSM_LIST list(CSSM_LIST_ELEMENT *nodes, size_t count)
{
    for (size_t i = 0; i < count; ++i) nodes[i].NextElement = i + 1 < count ? nodes + i + 1 : NULL;
    return (CSSM_LIST){.ListType = CSSM_LIST_TYPE_UNKNOWN, .Head = nodes, .Tail = nodes + count - 1};
}
static void subject_create(subject *value, CFDataRef requirement)
{
    memset(value, 0, sizeof *value);
    value->selector = (CSSM_ACL_PROCESS_SUBJECT_SELECTOR){
        .version = CSSM_ACL_PROCESS_SELECTOR_CURRENT_VERSION, .mask = CSSM_ACL_MATCH_UID};
    strcpy(value->path, "/synthetic/launcher");
    word(value->process, CSSM_ACL_SUBJECT_TYPE_PROCESS);
    datum(value->process + 1, &value->selector, sizeof value->selector);
    word(value->signature, CSSM_ACL_SUBJECT_TYPE_CODE_SIGNATURE);
    word(value->signature + 1, CSSM_ACL_CODE_SIGNATURE_OSX);
    datum(value->signature + 2, value->hash, sizeof value->hash);
    datum(value->signature + 3, value->path, strlen(value->path) + 1);
    datum(value->signature + 4, (void *)CFDataGetBytePtr(requirement), (size_t)CFDataGetLength(requirement));
    word(value->threshold, CSSM_ACL_SUBJECT_TYPE_THRESHOLD);
    word(value->threshold + 1, 2); word(value->threshold + 2, 2);
    value->threshold[3] = (CSSM_LIST_ELEMENT){.ElementType = CSSM_LIST_ELEMENT_SUBLIST,
                                            .Element.Sublist = list(value->process, 2)};
    value->threshold[4] = (CSSM_LIST_ELEMENT){.ElementType = CSSM_LIST_ELEMENT_SUBLIST,
                                            .Element.Sublist = list(value->signature, 5)};
    value->subject = list(value->threshold, 5);
}

typedef enum { VALID, ALTERNATE_DECRYPT, PROMPT, WRONG_CODE, AUXILIARY, WEAK_OWNER, DELEGATE } policy_kind;
static bool access_create(fixture *value, policy_kind kind)
{
    SecRequirementRef requirement = NULL;
    CFDataRef data = NULL;
    CFStringRef text = kind == WRONG_CODE ? CFSTR(WRONG_REQUIREMENT) : CFSTR(RETRIEVER_REQUIREMENT);
    bool ok = SecRequirementCreateWithString(text, 0, &requirement) == errSecSuccess && requirement != NULL &&
              SecRequirementCopyData(requirement, 0, &data) == errSecSuccess && data != NULL;
    if (ok) {
        subject owner, decrypt, other;
        subject_create(&owner, data); subject_create(&decrypt, data); subject_create(&other, data);
        CSSM_ACL_OWNER_PROTOTYPE owner_policy = {.TypedSubject = owner.subject};
        CSSM_ACL_AUTHORIZATION_TAG decrypt_tag = CSSM_ACL_AUTHORIZATION_DECRYPT;
        CSSM_ACL_AUTHORIZATION_TAG aux_tag = CSSM_ACL_AUTHORIZATION_CHANGE_ACL;
        CSSM_ACL_ENTRY_INFO entries[2] = {
            {.EntryPublicInfo = {.TypedSubject = decrypt.subject,
                                 .Authorization = {1, &decrypt_tag},
                                 .EntryTag = ST_POLICY_ENTRY_TAG}, .EntryHandle = ST_POLICY_ENTRY_HANDLE},
            {.EntryPublicInfo = {.TypedSubject = other.subject,
                                 .Authorization = {1, &aux_tag},
                                 .EntryTag = "unsupported-auxiliary"}, .EntryHandle = 2}
        };
        CSSM_LIST_ELEMENT prompt[3];
        CSSM_ACL_KEYCHAIN_PROMPT_SELECTOR prompt_selector = {
            .version = CSSM_ACL_KEYCHAIN_PROMPT_CURRENT_VERSION
        };
        word(prompt + 1, CSSM_ACL_SUBJECT_TYPE_KEYCHAIN_PROMPT);
        datum(prompt + 2, &prompt_selector, sizeof prompt_selector);
        prompt[0] = (CSSM_LIST_ELEMENT){.ElementType = CSSM_LIST_ELEMENT_SUBLIST,
                                       .Element.Sublist = list(prompt + 1, 2)};
        if (kind == PROMPT) {
            decrypt.threshold[2].WordID = 3;
            decrypt.threshold[4].NextElement = prompt;
            prompt[0].NextElement = NULL;
            entries[0].EntryPublicInfo.TypedSubject.Tail = prompt;
        }
        if (kind == WEAK_OWNER) owner_policy.TypedSubject = list(owner.process, 2);
        if (kind == DELEGATE) entries[0].EntryPublicInfo.Delegate = CSSM_TRUE;
        if (kind == ALTERNATE_DECRYPT) entries[1].EntryPublicInfo.Authorization.AuthTags = &decrypt_tag;
        if (value->access != NULL) CFRelease(value->access);
        value->access = NULL;
        ok = SecAccessCreateFromOwnerAndACL(&owner_policy,
                                           kind == ALTERNATE_DECRYPT || kind == AUXILIARY ? 2 : 1,
                                           entries, &value->access) == errSecSuccess && value->access != NULL;
    }
    if (data != NULL) CFRelease(data);
    if (requirement != NULL) CFRelease(requirement);
    return ok;
}

static st_native_result guard(void *context)
{
    fixture *value = context; ++value->guards; return value->guard_result;
}
static st_native_result disable(void *context)
{
    fixture *value = context; ++value->disables; value->interactions_disabled = true; return ST_KC_OK;
}
static st_native_result open_system(void *context, void **out)
{
    fixture *value = context; ++value->opens;
    if (!value->interactions_disabled) return ST_KC_INTERACTION_REQUIRED;
    *out = value; return ST_KC_OK;
}
static st_native_result resolve(void *context, void *keychain, const uint8_t *identity,
                                size_t length, void **out)
{
    fixture *value = context; ++value->resolves;
    if (keychain != value || length != sizeof value->identity ||
        memcmp(identity, value->identity, length) != 0) return ST_KC_IDENTITY_MISMATCH;
    *out = value; return ST_KC_OK;
}
static st_native_result attributes(void *context, void *item, st_keychain_test_attributes *out)
{
    fixture *value = context; ++value->attributes_copied;
    if (value->secrets != 0) {
        if (value->framework_allocation_live) value->framework_live_during_reinspection = true;
        value->locked_during_reinspection = value->staging_locked && value->staging != NULL &&
                                            value->staging[0] == 0xa7;
    }
    if (item != value) return ST_KC_NATIVE_UNAVAILABLE;
    *out = value->attributes; return ST_KC_OK;
}
static void release_attributes(void *context, st_keychain_test_attributes *attributes_value)
{
    fixture *value = context; ++value->attributes_released; *attributes_value = (st_keychain_test_attributes){0};
}
static st_native_result copy_access(void *context, void *item, SecAccessRef *out)
{
    fixture *value = context;
    if (item != value) return ST_KC_NATIVE_UNAVAILABLE;
    *out = (SecAccessRef)CFRetain(value->access); return ST_KC_OK;
}
static st_native_result secret(void *context, void *item, uint8_t **out, size_t *length)
{
    fixture *value = context; ++value->secrets;
    value->locked_before_read = value->staging_locked;
    if (item != value || !value->interactions_disabled) return ST_KC_NATIVE_UNAVAILABLE;
    *length = value->secret_length;
    *out = malloc(*length);
    if (*out == NULL) return ST_KC_NATIVE_UNAVAILABLE;
    value->framework_allocation_live = true;
    memset(*out, 0xa7, *length);
    if (value->change_policy_on_read && !access_create(value, ALTERNATE_DECRYPT)) return ST_KC_NATIVE_UNAVAILABLE;
    if (value->change_identity_on_read) value->attributes.generic[16] ^= 1;
    return value->secret_result;
}
static st_native_result release_secret(void *context, uint8_t *bytes, size_t length)
{
    fixture *value = context;
    if (bytes != NULL) {
        ++value->allocations_released;
        value->secret_wiped = true;
        for (size_t i = 0; i < length; ++i) if (bytes[i] != 0) value->secret_wiped = false;
        free(bytes);
        value->framework_allocation_live = false;
    }
    return value->free_result;
}
static void release_item(void *context, void *item)
{
    fixture *value = context; if (item != NULL) ++value->items_released;
}
static void release_keychain(void *context, void *keychain)
{
    fixture *value = context; if (keychain != NULL) ++value->keychains_released;
}
static st_native_result recheck(void *context, const st_native_release_permit *permit)
{
    fixture *value = context; ++value->rechecks;
    return permit != NULL && permit->live && value->rechecks != value->invalidate_at_recheck ?
           ST_KC_OK : ST_KC_EXECUTION_UNPROVEN;
}
static st_native_result write_secret(void *context, st_native_locked_secret *destination,
                                     const uint8_t *bytes, size_t length)
{
    fixture *value = context; ++value->writes;
    value->write_used_locked_staging = value->staging_locked && bytes == value->staging &&
                                       !value->framework_allocation_live && value->rechecks == 3;
    if (length != sizeof destination->bytes) return ST_KC_INVALID_RECORD;
    memcpy(destination->bytes, bytes, length); return value->write_result;
}
static void wipe_destination(void *context, st_native_locked_secret *destination)
{
    fixture *value = context; value->destination_wiped = true; memset(destination, 0, sizeof *destination);
}
static st_native_result lock_staging(void *context, uint8_t *bytes, size_t length)
{
    fixture *value = context; ++value->locks;
    if (value->lock_result != ST_KC_OK) return value->lock_result;
    if (length != 32 || mlock(bytes, length) != 0) return ST_KC_NATIVE_UNAVAILABLE;
    value->staging = bytes; value->staging_locked = true;
    return ST_KC_OK;
}
static st_native_result unlock_staging(void *context, uint8_t *bytes, size_t length)
{
    fixture *value = context; ++value->unlocks;
    value->staging_wiped = length == 32 && value->staging == bytes && value->staging_locked;
    for (size_t i = 0; i < length; ++i) if (bytes[i] != 0) value->staging_wiped = false;
    value->staging_locked = false; value->staging = NULL;
    if (munlock(bytes, length) != 0) return ST_KC_CLEANUP_UNRESOLVED;
    return value->unlock_result;
}
static const st_keychain_test_calls calls = {
    guard, disable, open_system, resolve, attributes, release_attributes,
    copy_access, secret, release_secret, release_item, release_keychain,
    recheck, write_secret, wipe_destination, lock_staging, unlock_staging
};

static void put_u32(uint8_t *out, uint32_t value)
{
    for (unsigned i = 0; i < 4; ++i) out[i] = (uint8_t)(value >> (8 * i));
}
static bool fixture_create(fixture *value)
{
    *value = (fixture){.secret_length = 32};
    value->facts = (st_keychain_test_binding_facts){.uid = 501, .gid = 20, .build = 3,
        .persistent_identity = value->identity, .persistent_identity_length = sizeof value->identity};
    memset(value->identity, 0x16, sizeof value->identity);
    memset(value->facts.native_account, 0x2a, 16);
    memset(value->facts.server_id, 0x31, 16);
    memset(value->facts.slot, 0x45, 16);
    memset(value->facts.account_binding, 0x63, 32);
    value->attributes.item_class = kSecGenericPasswordItemClass;
    value->attributes.service = (const uint8_t *)"io.github.sadiksaifi.spaceterm-development.server.boot.v1";
    value->attributes.service_length = strlen((const char *)value->attributes.service);
    value->attributes.label = (const uint8_t *)"SpaceTerm server boot key";
    value->attributes.label_length = strlen((const char *)value->attributes.label);
    value->attributes.persistent_identity = value->identity;
    value->attributes.persistent_identity_length = sizeof value->identity;
    for (unsigned i = 0; i < 16; ++i) { value->attributes.account[2*i] = '4'; value->attributes.account[2*i+1] = '5'; }
    uint8_t *generic = value->attributes.generic;
    memcpy(generic, "STKB", 4); generic[4] = 1; generic[6] = 3;
    put_u32(generic + 8, 501); put_u32(generic + 12, 20);
    memcpy(generic + 16, value->facts.native_account, 16);
    memcpy(generic + 32, value->facts.server_id, 16);
    memcpy(generic + 48, value->facts.slot, 16);
    memcpy(generic + 64, value->facts.account_binding, 32);
    return access_create(value, VALID) &&
           st_keychain_test_binding_create(&value->facts, &calls, value, &value->binding) == ST_KC_OK;
}
static void fixture_release(fixture *value)
{
    st_keychain_test_binding_release(&value->binding);
    if (value->access != NULL) CFRelease(value->access);
}
static bool all_zero(const void *bytes, size_t length)
{
    const uint8_t *value = bytes;
    for (size_t i = 0; i < length; ++i) if (value[i] != 0) return false;
    return true;
}
static bool inspect(fixture *value, st_native_result expected)
{
    st_keychain_policy_observation observation;
    memset(&observation, 0xee, sizeof observation);
    st_native_result result = st_keychain_inspect_fixed_item(value->binding, &observation);
    bool metadata_only = value->secrets == 0 && value->attributes_released == value->attributes_copied &&
                         value->items_released == value->resolves && value->keychains_released == value->opens;
    bool output = expected != ST_KC_OK ? all_zero(&observation, sizeof observation) :
                  observation.schema == 1 && observation.policy_class == 1 && observation.interaction_class == 1 &&
                  observation.account_binding_matches == 1 && observation.code_binding_matches == 1 &&
                  observation.auxiliary_acl_class == 1;
    return result == expected && metadata_only && output;
}
static bool policy_case(policy_kind kind)
{
    fixture value;
    if (!fixture_create(&value)) { fixture_release(&value); return false; }
    bool ok = access_create(&value, kind) && inspect(&value, ST_KC_POLICY_REJECTED);
    fixture_release(&value); return ok;
}
static bool alternate(void) { return policy_case(ALTERNATE_DECRYPT); }
static bool prompt(void) { return policy_case(PROMPT); }
static bool wrong_code(void) { return policy_case(WRONG_CODE); }
static bool auxiliary(void) { return policy_case(AUXILIARY); }
static bool weak_owner(void) { return policy_case(WEAK_OWNER); }
static bool delegate(void) { return policy_case(DELEGATE); }
static bool inspection(void)
{
    fixture value;
    if (!fixture_create(&value)) { fixture_release(&value); return false; }
    bool ok = inspect(&value, ST_KC_OK) && value.guards != 0 && value.disables == 1;
    fixture_release(&value); return ok;
}
static bool wrong_account(void)
{
    bool ok = true;
    const size_t offsets[] = {6, 8, 12, 16, 32, 48, 64};
    for (size_t i = 0; i < sizeof offsets / sizeof offsets[0]; ++i) {
        fixture value;
        if (!fixture_create(&value)) { fixture_release(&value); return false; }
        value.attributes.generic[offsets[i]] ^= 1;
        ok = inspect(&value, ST_KC_IDENTITY_MISMATCH) && ok;
        fixture_release(&value);
    }
    return ok;
}
static bool persistent_identity(void)
{
    fixture value;
    if (!fixture_create(&value)) { fixture_release(&value); return false; }
    uint8_t wrong[24] = {0};
    value.attributes.persistent_identity = wrong;
    bool ok = inspect(&value, ST_KC_IDENTITY_MISMATCH);
    fixture_release(&value); return ok;
}
static bool guard_rejects(void)
{
    fixture value;
    if (!fixture_create(&value)) { fixture_release(&value); return false; }
    value.guard_result = ST_KC_EXECUTION_UNPROVEN;
    bool ok = inspect(&value, ST_KC_EXECUTION_UNPROVEN) && value.opens == 0 && value.disables == 0;
    fixture_release(&value); return ok;
}
static bool retrieval(unsigned invalidation, st_native_result secret_result,
                      size_t secret_length, st_native_result free_result,
                      st_native_result write_result, st_native_result expected)
{
    fixture value;
    if (!fixture_create(&value)) { fixture_release(&value); return false; }
    value.invalidate_at_recheck = invalidation; value.secret_result = secret_result;
    value.secret_length = secret_length; value.free_result = free_result; value.write_result = write_result;
    st_native_release_permit permit = {.live = true};
    st_native_locked_secret destination;
    memset(&destination, 0xee, sizeof destination);
    st_native_result result = st_keychain_test_retrieve(value.binding, &permit, &destination);
    bool output = expected == ST_KC_OK ? destination.bytes[0] == 0xa7 && !value.destination_wiped &&
                                        value.write_used_locked_staging :
                  value.destination_wiped && all_zero(&destination, sizeof destination);
    bool ok = result == expected && output && (value.secrets == 0 ||
              (value.secrets == 1 && value.secret_wiped && value.allocations_released == 1 &&
               value.locked_before_read && !value.framework_allocation_live &&
               !value.framework_live_during_reinspection));
    if (value.locks != 0) ok = ok && value.locks == 1 && value.unlocks == 1 && value.staging_wiped;
    if (invalidation == 1) ok = ok && value.opens == 0 && value.secrets == 0;
    if (free_result != ST_KC_OK) ok = ok && value.attributes_copied == 1 && value.writes == 0;
    if (secret_result == ST_KC_INTERACTION_REQUIRED) ok = ok && value.secrets == 1 && value.disables == 1;
    fixture_release(&value); return ok;
}
static bool live_permit(void) { return retrieval(1, ST_KC_OK, 32, ST_KC_OK, ST_KC_OK, ST_KC_EXECUTION_UNPROVEN); }
static bool stale_permit(void) { return retrieval(3, ST_KC_OK, 32, ST_KC_OK, ST_KC_OK, ST_KC_EXECUTION_UNPROVEN); }
static bool interaction(void) { return retrieval(0, ST_KC_INTERACTION_REQUIRED, 32, ST_KC_OK, ST_KC_OK, ST_KC_INTERACTION_REQUIRED); }
static bool allocation_cleanup(void)
{
    return retrieval(0, ST_KC_OK, 32, ST_KC_OK, ST_KC_OK, ST_KC_OK) &&
           retrieval(2, ST_KC_OK, 32, ST_KC_OK, ST_KC_OK, ST_KC_EXECUTION_UNPROVEN) &&
           retrieval(0, ST_KC_CREDENTIAL_UNAVAILABLE, 32, ST_KC_OK, ST_KC_OK, ST_KC_CREDENTIAL_UNAVAILABLE) &&
           retrieval(0, ST_KC_OK, 31, ST_KC_OK, ST_KC_OK, ST_KC_CREDENTIAL_UNAVAILABLE) &&
           retrieval(0, ST_KC_OK, 33, ST_KC_OK, ST_KC_OK, ST_KC_CREDENTIAL_UNAVAILABLE) &&
           retrieval(0, ST_KC_OK, 32, ST_KC_CLEANUP_UNRESOLVED, ST_KC_OK, ST_KC_CLEANUP_UNRESOLVED) &&
           retrieval(0, ST_KC_OK, 32, ST_KC_OK, ST_KC_NATIVE_UNAVAILABLE, ST_KC_NATIVE_UNAVAILABLE);
}
static bool policy_and_identity_rechecked_after_read(void)
{
    bool ok = true;
    for (unsigned i = 0; i < 2; ++i) {
        fixture value;
        if (!fixture_create(&value)) { fixture_release(&value); return false; }
        value.change_policy_on_read = i == 0;
        value.change_identity_on_read = i == 1;
        st_native_release_permit permit = {.live = true};
        st_native_locked_secret destination;
        memset(&destination, 0xee, sizeof destination);
        st_native_result result = st_keychain_test_retrieve(value.binding, &permit, &destination);
        ok = ok && result == (i == 0 ? ST_KC_POLICY_REJECTED : ST_KC_IDENTITY_MISMATCH) &&
             value.secrets == 1 && value.secret_wiped && value.allocations_released == 1 &&
             value.locked_during_reinspection && !value.framework_live_during_reinspection &&
             value.unlocks == 1 && value.staging_wiped && value.writes == 0 &&
             value.destination_wiped && all_zero(&destination, sizeof destination);
        fixture_release(&value);
    }
    return ok;
}
static bool released_before_reinspection(void)
{
    fixture value;
    if (!fixture_create(&value)) { fixture_release(&value); return false; }
    st_native_release_permit permit = {.live = true};
    st_native_locked_secret destination;
    memset(&destination, 0xee, sizeof destination);
    st_native_result result = st_keychain_test_retrieve(value.binding, &permit, &destination);
    bool ok = result == ST_KC_OK && value.attributes_copied == 2 &&
              !value.framework_live_during_reinspection && !value.framework_allocation_live &&
              value.allocations_released == 1 && value.secret_wiped && destination.bytes[0] == 0xa7 &&
              value.locked_before_read && value.locked_during_reinspection &&
              value.locks == 1 && value.unlocks == 1 && value.staging_wiped && value.write_used_locked_staging;
    fixture_release(&value); return ok;
}
static bool locking_failure(void)
{
    fixture value;
    if (!fixture_create(&value)) { fixture_release(&value); return false; }
    value.lock_result = ST_KC_NATIVE_UNAVAILABLE;
    st_native_release_permit permit = {.live = true};
    st_native_locked_secret destination;
    memset(&destination, 0xee, sizeof destination);
    st_native_result result = st_keychain_test_retrieve(value.binding, &permit, &destination);
    bool ok = result == ST_KC_NATIVE_UNAVAILABLE && value.locks == 1 && value.unlocks == 0 &&
              value.secrets == 0 && value.allocations_released == 0 &&
              value.destination_wiped && all_zero(&destination, sizeof destination);
    fixture_release(&value); return ok;
}
static bool unlock_failure(void)
{
    fixture value;
    if (!fixture_create(&value)) { fixture_release(&value); return false; }
    value.unlock_result = ST_KC_CLEANUP_UNRESOLVED;
    st_native_release_permit permit = {.live = true};
    st_native_locked_secret destination;
    memset(&destination, 0xee, sizeof destination);
    st_native_result result = st_keychain_test_retrieve(value.binding, &permit, &destination);
    bool ok = result == ST_KC_CLEANUP_UNRESOLVED && value.locks == 1 && value.unlocks == 1 &&
              value.staging_wiped && value.secret_wiped && value.allocations_released == 1 &&
              value.destination_wiped && all_zero(&destination, sizeof destination);
    fixture_release(&value); return ok;
}
static bool fixed_attributes(void)
{
    bool ok = true;
    for (unsigned i = 0; i < 4; ++i) {
        fixture value;
        if (!fixture_create(&value)) { fixture_release(&value); return false; }
        switch (i) {
        case 0: value.attributes.account[0] ^= 1; break;
        case 1: value.attributes.service = (const uint8_t *)"other"; value.attributes.service_length = 5; break;
        case 2: value.attributes.label_length -= 1; break;
        case 3: value.attributes.item_class = kSecInternetPasswordItemClass; break;
        }
        ok = inspect(&value, ST_KC_IDENTITY_MISMATCH) && ok;
        fixture_release(&value);
    }
    return ok;
}
static bool no_native_calls(void)
{
    /* Missing native or locking operations reject without any fallback. */
    fixture value;
    if (!fixture_create(&value)) { fixture_release(&value); return false; }
    bool ok = true;
    for (unsigned i = 0; i < 3; ++i) {
        st_keychain_test_calls incomplete = calls;
        switch (i) {
        case 0: incomplete.open_system = NULL; break;
        case 1: incomplete.lock_staging = NULL; break;
        case 2: incomplete.unlock_staging = NULL; break;
        }
        st_keychain_item_binding *binding = NULL;
        st_native_result result = st_keychain_test_binding_create(&value.facts, &incomplete, &value, &binding);
        ok = ok && result == ST_KC_INVALID_RECORD && binding == NULL && value.opens == 0 && value.locks == 0;
        st_keychain_test_binding_release(&binding);
    }
    fixture_release(&value); return ok;
}

static bool unavailable_public_retrieval(void)
{
    st_native_release_permit permit = {.live = true};
    st_native_locked_secret destination;
    memset(&destination, 0xee, sizeof destination);
    return st_keychain_retrieve_for_startup(&permit, &destination) == ST_KC_NATIVE_UNAVAILABLE &&
           all_zero(&destination, sizeof destination);
}

int main(void)
{
    static const struct { const char *name; bool (*run)(void); } cases[] = {
        {"persisted_policy_rejects_alternate_decrypt_grant", alternate},
        {"persisted_policy_rejects_prompt_fallback", prompt},
        {"persisted_policy_rejects_wrong_retriever_requirement", wrong_code},
        {"persisted_item_rejects_wrong_account_incarnation_and_slot", wrong_account},
        {"unsupported_auxiliary_acl_is_not_ignored", auxiliary},
        {"interaction_required_retrieval_has_no_prompt_retry", interaction},
        {"policy_inspection_never_reads_secret", inspection},
        {"retrieval_requires_live_release_permit", live_permit},
        {"framework_secret_allocation_is_wiped_on_every_exit", allocation_cleanup},
        {"ordinary_tests_never_open_or_mutate_system_keychain", no_native_calls},
        {"persisted_owner_rejects_root_without_code_conjunction", weak_owner},
        {"persisted_policy_rejects_delegate_grants", delegate},
        {"persistent_identity_is_not_a_matching_label", persistent_identity},
        {"native_guard_precedes_all_keychain_calls", guard_rejects},
        {"release_rechecked_after_retrieval_before_copy", stale_permit},
        {"policy_and_identity_rechecked_after_secret_read", policy_and_identity_rechecked_after_read},
        {"framework_plaintext_is_released_before_reinspection", released_before_reinspection},
        {"staging_lock_failure_prevents_secret_read", locking_failure},
        {"staging_unlock_failure_wipes_destination", unlock_failure},
        {"fixed_item_rejects_alternate_attributes_and_class", fixed_attributes},
        {"unavailable_public_retrieval_wipes_destination", unavailable_public_retrieval}
    };
    size_t failed = 0;
    for (size_t i = 0; i < sizeof cases / sizeof cases[0]; ++i) {
        bool ok = cases[i].run();
        if (!ok) ++failed;
        printf("{\"schema\":1,\"scope\":\"injected-keychain\",\"case\":\"%s\",\"passed\":%s}\n",
               cases[i].name, ok ? "true" : "false");
    }
    printf("{\"schema\":1,\"scope\":\"injected-keychain\",\"summary\":true,\"cases\":%zu,\"failed\":%zu}\n",
           sizeof cases / sizeof cases[0], failed);
    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}

#pragma clang diagnostic pop
