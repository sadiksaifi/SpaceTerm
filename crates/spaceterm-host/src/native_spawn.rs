//! Account programs are created by one pre-secret, exec-separated process.
//!
//! The launcher must start the protected spawner before retrieving credentials, drop it to the
//! account with its ordinary groups, and pass one anonymous channel to the server. Connecting
//! that channel does not spawn a process or grant native startup authority. Server composition
//! must share this connection between the PTY factory and repository runner. Desktop composition
//! keeps its ordinary adapters.

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod client;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod protocol;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod pty;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod repository;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod service;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use client::CredentialFreeSpawner;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use pty::CredentialFreePtyAdapterFactory;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use repository::CredentialFreeRepositoryProgramRunner;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use service::run_inherited;
