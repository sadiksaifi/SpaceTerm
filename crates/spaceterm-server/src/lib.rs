//! Headless SpaceTerm Server composition.
//!
//! Owns authenticated transports, runtime worker composition, publication,
//! recovery, management CLI, and orderly shutdown. Tokio drives transports and
//! composition; terminal workers remain OS threads. Native host adapters supply
//! credentials and process authority. Production startup must remain closed
//! until native credential delivery exists, with no listener or test authority.

#![forbid(unsafe_code)]

/// Bounded production startup classification, without native error details.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupFailure {
    /// The native startup channel cannot yet deliver credentials.
    NativeCredentialDeliveryUnavailable,
}

/// Validate production startup prerequisites before creating any listeners.
pub fn production_startup() -> Result<(), StartupFailure> {
    Err(StartupFailure::NativeCredentialDeliveryUnavailable)
}

/// Safe native receiver adapter supplied by native-handoff B2.
pub mod native_startup;
