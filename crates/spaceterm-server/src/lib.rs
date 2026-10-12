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

pub mod api;

pub mod failure;

pub mod service;

pub mod listeners;

pub mod connection;

pub mod protocol_dispatch;

pub mod control;

pub mod startup_intents;

pub mod runtime_registry;

pub mod recovery;

pub mod shutdown;

pub mod publication;

pub mod egress;

pub mod extensions;

pub mod pairing;

pub mod git;

pub mod adapters;

pub mod contracts;
