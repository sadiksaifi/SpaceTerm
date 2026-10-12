#ifndef SPACETERM_KEYCHAIN_ITEMS_TEST_H
#define SPACETERM_KEYCHAIN_ITEMS_TEST_H

#if !defined(ST_KEYCHAIN_TESTING)
#error "Injected Keychain calls are available only in the private test composition"
#endif

#include "../../../crates/spaceterm-host/native/macos/keychain_items.h"
#include <CoreFoundation/CoreFoundation.h>
#include <Security/Security.h>
#include <stddef.h>

#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"

enum {
    ST_KC_OK = 0,
    ST_KC_NATIVE_UNAVAILABLE = 2,
    ST_KC_IDENTITY_MISMATCH = 12,
    ST_KC_CREDENTIAL_UNAVAILABLE = 21,
    ST_KC_POLICY_REJECTED = 22,
    ST_KC_INTERACTION_REQUIRED = 23,
    ST_KC_EXECUTION_UNPROVEN = 28,
    ST_KC_LIMIT_EXCEEDED = 30,
    ST_KC_CLEANUP_UNRESOLVED = 33,
    ST_KC_INVALID_RECORD = 9,
    ST_KC_ATTRIBUTE_BYTES = 96,
    ST_KC_PERSISTENT_BYTES = 1024
};

typedef struct {
    uint32_t uid;
    uint32_t gid;
    uint32_t build;
    uint8_t native_account[16];
    uint8_t server_id[16];
    uint8_t slot[16];
    uint8_t account_binding[32];
    const uint8_t *persistent_identity;
    size_t persistent_identity_length;
} st_keychain_test_binding_facts;

typedef struct {
    uint32_t item_class;
    uint8_t account[32]; /* lowercase hexadecimal slot */
    uint8_t generic[ST_KC_ATTRIBUTE_BYTES];
    const uint8_t *service;
    size_t service_length;
    const uint8_t *label;
    size_t label_length;
    const uint8_t *persistent_identity;
    size_t persistent_identity_length;
    void *native_attributes;
    CFDataRef native_identity;
} st_keychain_test_attributes;

/* All copy callbacks own their outputs even when returning failure. release
 * callbacks must accept NULL. Ordinary tests supply every callback; they never
 * fall through to Security.framework's item or Keychain operations.
 */
typedef struct {
    st_native_result (*guard)(void *context);
    st_native_result (*disable_interaction)(void *context);
    st_native_result (*open_system)(void *context, void **out_keychain);
    st_native_result (*resolve_item)(void *context, void *keychain,
                                   const uint8_t *identity, size_t length,
                                   void **out_item);
    st_native_result (*copy_attributes)(void *context, void *item,
                                       st_keychain_test_attributes *out_attributes);
    void (*release_attributes)(void *context, st_keychain_test_attributes *attributes);
    st_native_result (*copy_access)(void *context, void *item, SecAccessRef *out_access);
    st_native_result (*copy_secret)(void *context, void *item,
                                  uint8_t **out_bytes, size_t *out_length);
    st_native_result (*release_secret)(void *context, uint8_t *bytes, size_t length);
    void (*release_item)(void *context, void *item);
    void (*release_keychain)(void *context, void *keychain);
    st_native_result (*recheck_permit)(void *context,
                                     const st_native_release_permit *permit);
    st_native_result (*write_secret)(void *context, st_native_locked_secret *destination,
                                    const uint8_t *bytes, size_t length);
    void (*wipe_destination)(void *context, st_native_locked_secret *destination);
    st_native_result (*lock_staging)(void *context, uint8_t *bytes, size_t length);
    st_native_result (*unlock_staging)(void *context, uint8_t *bytes, size_t length);
} st_keychain_test_calls;

/* Test-only binding construction. No production issuer or native authority is
 * supplied by these functions. The same guarded inspection/retrieval algorithm
 * consumes the injected native outputs.
 */
st_native_result st_keychain_test_binding_create(
    const st_keychain_test_binding_facts *facts,
    const st_keychain_test_calls *calls, void *context,
    st_keychain_item_binding **out_binding);
void st_keychain_test_binding_release(st_keychain_item_binding **owned_binding);
st_native_result st_keychain_test_retrieve(
    const st_keychain_item_binding *binding,
    const st_native_release_permit *permit,
    st_native_locked_secret *destination);

#pragma clang diagnostic pop
#endif
