//! Portable SpaceTerm Client state.
//!
//! Owns per-server connections, authoritative replicas, Workspace Attachments,
//! personal arrangements, semantic content projections, and pending exact-grant
//! releases and self-revocations. Native credentials, clipboard, presentation,
//! and window systems belong to injected desktop or other native adapters.
//! Connection workers use Tokio; no async implementation is present yet.

#![forbid(unsafe_code)]
