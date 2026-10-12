//! Uninstalled Rust/C composition with injected native item and authority peers.

use spaceterm_host::{
    boot_credentials::{BootCredentialAdapter, CredentialPolicyFacts},
    native_deployment::{
        contracts::{BootCredentialBinding, NativeFailure},
        macos_boot_credentials::MacosBootCredentialAdapter,
    },
};
use std::ffi::{CString, c_char, c_int, c_void};

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(library: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

type Prepare = unsafe extern "C" fn(*const u8, u32) -> u32;
type Finish = unsafe extern "C" fn();

#[test]
fn actual_host_adapter_consumes_native_policy_and_rejects_substitution() {
    let path = CString::new(std::env::var("SPACETERM_KEYCHAIN_TEST_PEER").unwrap()).unwrap();
    // SAFETY: the task built this uninstalled peer in its private temporary directory.
    // RTLD_NOW | RTLD_GLOBAL lets the production adapter resolve its static symbols.
    let library = unsafe { dlopen(path.as_ptr(), 2 | 8) };
    assert!(!library.is_null());
    // SAFETY: these two symbols have the peer's documented exact ABI. Keep the
    // library loaded until process exit so the adapter's pointers cannot expire.
    let prepare = unsafe { dlsym(library, c"st_keychain_fixture_prepare".as_ptr()) };
    let finish = unsafe { dlsym(library, c"st_keychain_fixture_finish".as_ptr()) };
    assert!(!prepare.is_null() && !finish.is_null());
    // SAFETY: both nonnull function symbols have the exact test peer ABI.
    let prepare: Prepare = unsafe { std::mem::transmute(prepare) };
    let finish: Finish = unsafe { std::mem::transmute(finish) };
    let binding = BootCredentialBinding::macos([1; 32], [2; 32], [3; 32], [4; 16]).unwrap();
    let digest = binding.sha256().unwrap();
    for (scenario, expected) in [
        (
            0,
            Ok(CredentialPolicyFacts {
                schema: 1,
                account_and_code_bound: true,
                interaction_disabled: true,
            }),
        ),
        (1, Err(NativeFailure::CredentialPolicyRejected)),
        (2, Err(NativeFailure::IdentityMismatch)),
        (3, Err(NativeFailure::ExecutionBindingUnproven)),
    ] {
        // SAFETY: the peer copies the bounded digest and owns its synthetic fixture.
        assert_eq!(unsafe { prepare(digest.as_ptr(), scenario) }, 0);
        assert_eq!(
            MacosBootCredentialAdapter.inspect_policy(&binding),
            expected
        );
        // SAFETY: discard any remaining peer scope after this isolated scenario.
        unsafe { finish() };
    }
    let substituted = BootCredentialBinding::macos([5; 32], [2; 32], [3; 32], [4; 16]).unwrap();
    // SAFETY: the peer binds only the original digest, never the substituted profile.
    assert_eq!(unsafe { prepare(digest.as_ptr(), 0) }, 0);
    assert_eq!(
        MacosBootCredentialAdapter.inspect_policy(&substituted),
        Err(NativeFailure::IdentityMismatch)
    );
    // SAFETY: this test owns the last synthetic fixture.
    unsafe { finish() };
}
