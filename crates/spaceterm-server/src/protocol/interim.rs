//! Request-and-poll operations carried over the push framing while #670 lands in parts.
//!
//! The transport cutover maps each remaining polled request here, so every merge keeps the app
//! working. Each variant names the change that deletes it. Nothing in this module survives #670.
use super::message::Channel;
use crate::pane::{effects, presentation, reading};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Interim {
    /// Deleted by push, which sends `ServerMessage::Frame` and Pane events instead.
    Poll { channel: Channel },
    /// Deleted by push.
    PollChanges { channel: Channel, base: Option<u64> },
    /// Deleted by push, which sends `PaneEvent::Effect` and `PaneEvent::HiddenInput`.
    NativeEffects { channel: Channel, generation: u64 },
    /// Deleted by push, which uses `ClientMessage::View`.
    PresentationDemand { channel: Channel, visible: bool },
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
    /// Deleted by push and encoding.
    Screen(Box<crate::pane::Screen>),
    /// Deleted by push and encoding.
    ScreenChanges(Box<presentation::RowChanges>),
    /// Deleted by push.
    Unchanged,
    /// Deleted by push, which sends `PaneEvent::Ended`.
    Ended,
    /// Deleted by push, which sends `PaneEvent::Accessibility`.
    AccessibilityRevision { revision: u64 },
    /// Deleted by push.
    NativeEffects {
        request: Option<effects::Request>,
        hidden_input: bool,
    },
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
