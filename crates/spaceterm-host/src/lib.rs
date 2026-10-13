//! Headless native boundaries for SpaceTerm Server startup.
//!
//! Protected directory authority retains every ancestor handle on macOS and Linux.
//! [`root_protection::NativeRootProtection`] supplies macOS fixed-root filesystem
//! and ACL inspection. Linux requires a private, independently issued protection
//! lease covering selected-mount mapping and every inode's account-write-authorizing
//! aliases. Native query adapters and lease validation exist, but no production
//! issuer does. Uncontextualized Linux protection and fixed registration reads fail
//! closed. Optional newer mount queries do not raise the Linux 5.15/glibc 2.35/systemd
//! 249 baseline; old layouts without genuine provenance remain unavailable.
//! Filesystem protection is separate from signed-code authentication and execution
//! approval. No privileged or credential admission is supplied by these checks.
//! Startup admission validates an injected native registration and retains only its
//! exact approved installation. These boundaries neither load credentials nor start
//! processes.

#![deny(missing_docs)]

// Legacy source-included tests must use the real host contracts for additive ports.
extern crate self as spaceterm_host;

pub mod git;
pub mod native_registration;
pub mod protected_directory;
pub mod root_protection;
pub mod session_capabilities;
pub mod startup;
pub mod storage;

pub mod process;
pub mod pty;
pub mod shell_integration;
pub mod shell_launch;
pub mod transport;
pub mod wss;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod ipc;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod peer_credentials;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod ssh_tunnel;

pub mod activation_journal;
pub mod approved_installation;
pub mod boot_credentials;
pub mod installation;
pub mod native_deployment;
pub mod organization_store;

pub mod server_directories;

pub mod bootstrap;

// Native phase 2 producers compile against the same headless host surface.
pub mod linux_custody;
pub mod linux_launcher;
pub mod native_evidence;
pub mod native_management;
pub mod native_receipts;
pub mod native_receiver;
pub mod native_refusals;
pub mod native_spawn;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod startup_exchange;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod account_service;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod private_filesystem;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod server_keys;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod server_installation;
