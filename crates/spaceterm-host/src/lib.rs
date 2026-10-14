//! Native host adapters for the headless SpaceTerm Server.
//!
//! Account-service supervision, account-private credentials, PTYs, storage and
//! transports are composed by `spaceterm-server`. Portable behavior and policy
//! belong to `spaceterm-core`.

#![deny(missing_docs)]

// Legacy source-included tests must use the real host contracts for additive ports.
extern crate self as spaceterm_host;

pub mod git;
pub mod native_registration;
pub mod protected_directory;
pub mod root_protection;
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
pub mod server_machine_facts;

pub mod bootstrap;

pub mod linux_custody;
pub mod linux_launcher;
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
