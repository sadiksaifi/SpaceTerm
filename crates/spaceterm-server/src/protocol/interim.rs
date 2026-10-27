//! Request-and-poll operations carried over the push framing while #670 lands in parts.
//!
//! The transport cutover maps each remaining polled request here, so every merge keeps the app
//! working. Each variant names the change that deletes it. Nothing in this module survives #670.
use super::message::Channel;
use crate::pane::reading;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Interim {
    /// Deleted by scrollback, which reads the client's Scrollback cache.
    Read {
        channel: Channel,
        operation: reading::Operation,
    },
    /// Deleted by scrollback.
    ReadingStatus { channel: Channel },
    /// Deleted by scrollback.
    SelectionChunk {
        channel: Channel,
        generation: u64,
        offset: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum InterimReply {
    /// Deleted by scrollback.
    Reading { accepted: bool },
    /// Deleted by scrollback.
    ReadingReady {
        generation: u64,
        selection_revision: u64,
        selection_present: bool,
    },
    /// Deleted by scrollback.
    SelectionChunk {
        generation: u64,
        offset: u64,
        total: u64,
        bytes: Vec<u8>,
    },
    /// Deleted by scrollback.
    SelectionPending,
}

impl InterimReply {
    pub(super) fn bulk(&self) -> bool {
        matches!(self, Self::SelectionChunk { .. })
    }
}
