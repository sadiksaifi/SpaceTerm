/* Test composition only. Production requires the coordinator's common header. */
#ifndef ST_NATIVE_TESTING
#error "The missing common native contract must not fall back to a test contract"
#endif
#ifndef SPACETERM_NATIVE_H
#define SPACETERM_NATIVE_H
#include <stddef.h>
#include <stdint.h>
#define ST_NATIVE_ABI_VERSION UINT32_C(2)
#define ST_NATIVE_SECRET_BYTES 32u
#define ST_NATIVE_BINDING_BYTES 32u
typedef uint32_t st_native_result;
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
    uint32_t abi_version, struct_size, uid, gid, build, reserved;
    uint64_t registration_generation, lifecycle_generation, data_generation, installation_id;
    uint8_t server_id[16], account_binding[32], installation_binding[32];
    uint8_t server_artifact[32], manifest[32], credential_binding[32], startup_nonce[32];
    st_native_process_identity original_process, retriever_process;
    uint8_t journal_request[16];
} st_native_pending_facts;
st_native_result st_native_pending_copy_facts(const st_native_pending *, st_native_pending_facts *);
st_native_result st_native_release_permit_recheck(const st_native_release_permit *);
st_native_result st_native_locked_secret_write(st_native_locked_secret *, const uint8_t *, size_t);
void st_native_locked_secret_wipe(st_native_locked_secret *);
#endif
