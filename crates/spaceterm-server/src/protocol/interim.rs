//! Request-and-poll operations carried over the push framing while #670 lands in parts.
//!
//! The transport cutover maps each remaining polled request here, so every merge keeps the app
//! working. Each variant names the change that deletes it. Nothing in this module survives #670.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Interim {
    /// Deleted by control-plane, which pushes `ServerMessage::Inventory`.
    Inventory,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[allow(
    clippy::large_enum_variant,
    reason = "control-plane deletes this enum with the inventory poll"
)]
pub enum InterimReply {
    /// Deleted by control-plane.
    Inventory {
        inventory: crate::hierarchy::Inventory,
        next_sequence: u64,
    },
    /// Deleted by control-plane, which defers the `Mutate` and `Resolve` replies instead.
    MutationPending,
}
