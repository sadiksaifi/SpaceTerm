//! Desktop and headless CLI share explicit encrypted Secret Service access.
//! The provider never selects a portal/file backend or a plain-session fallback.
#[allow(dead_code, reason = "desktop production enrollment composition is coordinator-owned")]
pub(crate) fn native() -> spaceterm_client_native::credentials::adapter::DeviceCredentials {
    spaceterm_client_native::credentials::adapter::DeviceCredentials::native()
}
