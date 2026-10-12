//! Actual host credential types. Ordinary tests load no native Keychain peer.

#[cfg(target_os = "macos")]
use spaceterm_host::{
    boot_credentials::BootCredentialAdapter,
    native_deployment::{
        contracts::{BootCredentialBinding, NativeFailure},
        macos_boot_credentials::MacosBootCredentialAdapter,
    },
};

#[cfg(target_os = "macos")]
#[test]
fn comparison_binding_without_native_owner_never_opens_system_keychain() {
    let binding = BootCredentialBinding::macos([1; 32], [2; 32], [3; 32], [4; 16]).unwrap();
    assert_eq!(
        MacosBootCredentialAdapter.inspect_policy(&binding),
        Err(NativeFailure::NativeUnavailable)
    );
}
