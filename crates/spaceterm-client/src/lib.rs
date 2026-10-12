//! Portable SpaceTerm Client state.
//!
//! Owns per-server connections, authoritative replicas, Workspace Attachments,
//! personal arrangements, semantic content projections, and pending exact-grant
//! releases and self-revocations. Native credentials, clipboard, presentation,
//! and window systems belong to injected desktop or other native adapters.
//! Connection workers use Tokio; no async implementation is present yet.

#![forbid(unsafe_code)]

pub mod api;

pub mod failure;

pub mod journal;

pub mod limits;

pub mod operations;

pub mod pane_channel;

pub mod pending;

pub mod ports;

pub mod projection;

pub mod registry;

pub mod replica;

pub mod view;

pub mod pairing;

pub mod git;

pub mod directories;

pub mod management_cli;

mod connection;

mod content_cache;

mod protocol_adapter;

#[cfg(feature = "test-support")]
pub mod testing;
