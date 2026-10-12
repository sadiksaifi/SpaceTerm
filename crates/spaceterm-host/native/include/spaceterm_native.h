#ifndef SPACETERM_NATIVE_H
#define SPACETERM_NATIVE_H

#include <stddef.h>
#include <stdint.h>

#define ST_NATIVE_ABI_VERSION UINT32_C(2)
#define ST_NATIVE_SECRET_BYTES 32u
#define ST_NATIVE_BINDING_BYTES 32u

typedef uint32_t st_native_result;
/* 0 means success; other values equal the Rust NativeFailure discriminants. */
typedef struct st_native_pending st_native_pending;
typedef struct st_native_release_permit st_native_release_permit;
typedef struct st_native_post_exec_proof st_native_post_exec_proof;
typedef struct st_native_locked_secret st_native_locked_secret;
typedef struct st_native_authorized_request st_native_authorized_request;
typedef struct st_native_caller st_native_caller;

typedef struct {
    uint32_t pid;
    uint32_t start_microseconds;
    uint64_t start_seconds;
} st_native_process_identity;

typedef struct {
    uint32_t abi_version;
    uint32_t struct_size;
    uint32_t uid;
    uint32_t gid;
    uint32_t build;
    uint32_t reserved;
    uint64_t registration_generation;
    uint64_t lifecycle_generation;
    uint64_t data_generation;
    uint64_t installation_id;
    uint8_t server_id[16];
    uint8_t account_binding[32];
    uint8_t installation_binding[32];
    uint8_t server_artifact[32];
    uint8_t manifest[32];
    uint8_t credential_binding[32];
    uint8_t startup_nonce[32];
    st_native_process_identity original_process;
    st_native_process_identity retriever_process;
    uint8_t journal_request[16];
} st_native_pending_facts;

/* This layout is an in-process ABI, never a serialized wire format. */
_Static_assert(sizeof(st_native_process_identity) == 16, "process ABI size");
_Static_assert(_Alignof(st_native_process_identity) == 8, "process ABI alignment");
_Static_assert(offsetof(st_native_process_identity, start_seconds) == 8, "process ABI offset");
_Static_assert(sizeof(st_native_pending_facts) == 312, "pending ABI size");
_Static_assert(_Alignof(st_native_pending_facts) == 8, "pending ABI alignment");
_Static_assert(offsetof(st_native_pending_facts, registration_generation) == 24, "generation ABI offset");
_Static_assert(offsetof(st_native_pending_facts, server_id) == 56, "server ABI offset");
_Static_assert(offsetof(st_native_pending_facts, account_binding) == 72, "account ABI offset");
_Static_assert(offsetof(st_native_pending_facts, installation_binding) == 104, "installation ABI offset");
_Static_assert(offsetof(st_native_pending_facts, server_artifact) == 136, "artifact ABI offset");
_Static_assert(offsetof(st_native_pending_facts, manifest) == 168, "manifest ABI offset");
_Static_assert(offsetof(st_native_pending_facts, credential_binding) == 200, "credential ABI offset");
_Static_assert(offsetof(st_native_pending_facts, startup_nonce) == 232, "nonce ABI offset");
_Static_assert(offsetof(st_native_pending_facts, original_process) == 264, "parent ABI offset");
_Static_assert(offsetof(st_native_pending_facts, retriever_process) == 280, "retriever ABI offset");
_Static_assert(offsetof(st_native_pending_facts, journal_request) == 296, "journal ABI offset");

st_native_result st_native_pending_copy_facts(
    const st_native_pending *pending, st_native_pending_facts *out_facts);
st_native_result st_native_release_permit_recheck(
    const st_native_release_permit *permit);
st_native_result st_native_locked_secret_write(
    st_native_locked_secret *destination, const uint8_t *bytes, size_t length);
void st_native_locked_secret_wipe(st_native_locked_secret *secret);

#endif
