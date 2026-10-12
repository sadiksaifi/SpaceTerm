//! Desktop and headless CLI share login Keychain custody and slot policy.
#[allow(dead_code, reason = "desktop production enrollment composition is coordinator-owned")]
pub(crate) fn native() -> spaceterm_client_native::credentials::adapter::DeviceCredentials {
    spaceterm_client_native::credentials::adapter::DeviceCredentials::native()
}
