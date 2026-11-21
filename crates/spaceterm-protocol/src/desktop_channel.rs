//! The account-local channel from `spaceterm` to the desktop on the same machine and account.
//!
//! One connection carries four length-prefixed JSON messages, each at most 16 KiB with unknown
//! fields refused. The client sends a [`ChannelHello`] with the Desktop Channel versions it
//! speaks, its current and previous major, and the desktop answers a [`ChannelOffer`] that
//! selects one with [`crate::version::negotiate`] or none. Hello and Offer are frozen: every later major reads
//! and writes them unchanged, so a client and a desktop that share no major still learn which
//! one is older. Only after a selected version does the client send its one request, encoded at
//! that version, and half-close; the desktop answers once and closes. Both ends require the
//! peer to run as their own account. Requests
//! carry exact server, Workspace, Tab, Pane and Terminal Session IDs as fences, never authority:
//! the desktop decides what it presents, and only a Local Pane may open a local file. A
//! Diagnostics request only reads the desktop's Terminal Diagnostics counters.
//!
use crate::hierarchy::Id;
use crate::version::{self as versions, ProtocolVersion};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::PathBuf;

/// The largest message, which bounds a request's local path of 4 KiB with its envelope.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;
/// The Terminal Session `spaceterm` runs in, from its Session context.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionContext {
    pub core: Id,
    pub pane: Id,
    pub session: Id,
}

impl std::fmt::Debug for SessionContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SessionContext { .. }")
    }
}

/// What the client asks of the desktop.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Opens an absolute local file with its default application, or presents its File
    /// Preview, for the Session's Pane.
    Open {
        session: SessionContext,
        path: PathBuf,
        preview: bool,
    },
    /// Brings the desktop's own view to a Workspace, and to a Tab and Pane within it when given.
    Show {
        core: Id,
        workspace: Id,
        tab: Option<Id>,
        pane: Option<Id>,
    },
    /// Reads the desktop's Terminal Diagnostics counters. It changes nothing and needs no
    /// presented Workspace, Pane or server connection.
    Diagnostics,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Open { preview: false, .. } => "Request::Open { .. }",
            Self::Open { preview: true, .. } => "Request::Preview { .. }",
            Self::Show { .. } => "Request::Show { .. }",
            Self::Diagnostics => "Request::Diagnostics",
        })
    }
}

/// The desktop's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Done,
    Refused(Refusal),
    /// The desktop's Terminal Diagnostics counters, totals since it started.
    Diagnostics(crate::diagnostics::TerminalCounterSnapshot),
}

/// Why the desktop refused a request. Each names a fixed classification only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// This desktop presents no such live Terminal Session, Workspace, Tab or Pane.
    NotShown,
    /// The Session's Pane is Remote, so its paths grant no local file access.
    RemoteSession,
    /// The path names no regular file that this account can read.
    FileUnavailable,
    /// This desktop has no File Preview.
    PreviewUnavailable,
    /// The desktop is still checking as many earlier files as it checks at once.
    Busy,
    /// The desktop belongs to another application identity.
    OtherIdentity,
}

/// The client's first message. Frozen.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelHello {
    pub identity: String,
    /// The client's current version and, when one exists, its previous major at that major's
    /// last minor. 1 or 2 entries, highest first.
    pub versions: Vec<ProtocolVersion>,
}

/// The desktop's answer to the Hello. Frozen.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelOffer {
    pub identity: String,
    /// `negotiate(hello.versions, desktop)`. None when no major is shared or the identities
    /// differ; the desktop then closes the connection.
    pub selected: Option<ProtocolVersion>,
    pub desktop: ProtocolVersion,
}

/// The client's request, only after the Offer selected a version, encoded at that version.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope<T> {
    /// When the client stops waiting, in nanoseconds on the host's monotonic clock.
    pub expires_at: u64,
    pub body: T,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplyEnvelope {
    pub body: Reply,
}

/// The Desktop Channel versions this build speaks as a caller: the current one, then the
/// previous major when one exists.
pub fn offered() -> Vec<ProtocolVersion> {
    std::iter::once(versions::DESKTOP_CHANNEL)
        .chain(versions::PREVIOUS_DESKTOP_CHANNEL)
        .collect()
}

/// A Desktop Channel message cannot be encoded or decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid Desktop Channel message")]
pub struct InvalidMessage;

/// One message: its JSON with a big-endian `u32` length before it, at most
/// [`MAX_MESSAGE_BYTES`] of JSON.
pub fn encode(value: &impl Serialize) -> Result<Vec<u8>, InvalidMessage> {
    let json = serde_json::to_vec(value).map_err(|_| InvalidMessage)?;
    if json.len() > MAX_MESSAGE_BYTES {
        return Err(InvalidMessage);
    }
    let mut message = Vec::with_capacity(4 + json.len());
    message.extend_from_slice(&(json.len() as u32).to_be_bytes());
    message.extend_from_slice(&json);
    Ok(message)
}

pub fn decode<T: DeserializeOwned>(json: &[u8]) -> Result<T, InvalidMessage> {
    serde_json::from_slice(json).map_err(|_| InvalidMessage)
}
