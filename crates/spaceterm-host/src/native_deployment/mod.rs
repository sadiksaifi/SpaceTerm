//! Bounded deployment facts. Decoding these records never grants native authority.

pub mod account;
pub(crate) mod bindings;
pub mod contracts;
pub mod profile;

pub mod authority;
pub mod authority_ffi;
pub mod failures;
pub mod ports;
pub mod production_gate;
pub mod publication;

#[cfg(target_os = "macos")]
#[path = "../boot_credentials/macos.rs"]
pub mod macos_boot_credentials;
