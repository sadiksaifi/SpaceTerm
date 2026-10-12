use spaceterm_client::pairing::credentials::{CredentialNamespace, CredentialPurpose, CredentialScope, CredentialScopeParts};
use spaceterm_client_native::credentials::adapter::DeviceCredentials;
use spaceterm_core::{identity::ServerId, pairing::CredentialReference};

#[test]
fn credential_namespace_isolated_by_build_server_purpose_and_slot() {
    let scope = |build: &str, server: Option<u8>, slot: u8| CredentialScope::new(CredentialScopeParts { namespace: CredentialNamespace::new(build.into()).unwrap(), purpose: server.map_or(CredentialPurpose::DeviceIdentity,|server| CredentialPurpose::PairingAuthentication { server: ServerId::new([server;16]) }), reference: CredentialReference::new([slot;16]) }).unwrap();
    let scopes = [scope("dev.spaceterm.release",Some(1),1),scope("dev.spaceterm.preflight",Some(1),1),scope("dev.spaceterm.development",Some(1),1),scope("dev.spaceterm.release",Some(2),1),scope("dev.spaceterm.release",None,1),scope("dev.spaceterm.release",Some(1),2)];
    let names: std::collections::HashSet<_> = scopes.iter().map(DeviceCredentials::locator).collect();
    assert_eq!(names.len(),scopes.len());
    for scope in scopes { assert_eq!(format!("{scope:?}"),"CredentialScope([redacted])"); }
}
#[test]
fn desktop_credential_wrapper_constructs_no_native_item_or_fallback() {
    // Construction performs no credential I/O. Ordinary tests cannot mutate a
    // Keychain or Secret Service collection merely by selecting the provider.
    #[cfg(target_os = "macos")]
    let _native = super::macos::native();
    #[cfg(target_os = "linux")]
    let _native = super::linux::native();
}
