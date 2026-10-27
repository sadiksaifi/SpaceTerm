//! Client requests and their reply bodies.
//!
//! Operations reuse the payload types of the request-and-poll exchange where postcard encodes them.
//! Operations that the push protocol replaces live in [`super::interim::Interim`] until their
//! replacement merges.
use super::interim::{Interim, InterimReply};
use super::message::{Channel, check};
use super::screen::Page;
use super::{Fatal, limits};
use crate::pane::{
    AttachmentClaim, Lease, PaneTarget, PresentationStyle, accessibility, control, directories,
    graphics,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Operation {
    /// Selects, starts, repairs or takes over a Pane and binds it to `Claim::channel`.
    Claim(Claim),
    /// Retires an attachment generation the client detached.
    ReleaseAttachment {
        attachment: crate::attachments::Attachment,
    },
    /// Gives up control of the channel's Pane. The channel closes.
    Release {
        channel: Channel,
    },
    /// Closes the channel's Pane and ends its Terminal Session.
    Close {
        channel: Channel,
    },
    /// Clears only this Pane's screen and Scrollback.
    ClearScrollback {
        channel: Channel,
    },
    Style {
        channel: Channel,
        style: PresentationStyle,
    },
    /// A shared Workspace edit, or its Original Operation dispatched again with the same
    /// identity. The server replays an outcome it already recorded. The reply carries the
    /// validated outcome after the inventory revision that contains it, once that revision is
    /// retained, and waits for a Worktree write the outcome depends on.
    Mutate {
        sequence: u64,
        mutation: crate::hierarchy::Mutation,
    },
    /// The recorded outcome of an Original Operation, without dispatching it. The reply waits
    /// like `Mutate`'s, and refuses `Missing` when the server recorded no such operation.
    Resolve {
        sequence: u64,
    },
    /// Reads `rows` Scrollback rows from `start`, counted from the oldest retained row of
    /// history `epoch`.
    ScrollbackPage {
        channel: Channel,
        epoch: u64,
        start: u64,
        rows: u16,
    },
    Graphics {
        channel: Channel,
        image: graphics::ImageKey,
        offset: u64,
    },
    Accessibility {
        channel: Channel,
        revision: Option<u64>,
        offset: u64,
        base: Option<u64>,
    },
    AccessibilitySelection {
        channel: Channel,
        focus_generation: u64,
        selection: Option<accessibility::SelectionRefs>,
    },
    EndAccessibility {
        channel: Channel,
    },
    /// Replied when the read settles. `Cancel` abandons it.
    DirectoryRead(directories::Operation),
    /// Replied when the write settles. `Cancel` prevents the write if it has not started;
    /// directories it already created stay.
    DirectoryCreate(directories::Creation),
    /// Replied when the read settles. `Cancel` abandons it. `Read::Poll` is refused.
    WorktreeRead(crate::worktree::Read),
    /// Direct-network administration. Paired connections may send only what the host allows.
    Direct(crate::direct::Management),
    /// Stops the server. Account-local connections only.
    Shutdown,
    /// A request-and-poll operation awaiting its push replacement.
    Interim(Interim),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Claim {
    /// A channel number greater than every channel this connection used before.
    pub channel: Channel,
    pub attachment: crate::attachments::Attachment,
    pub pane: crate::hierarchy::Id,
    pub claim: AttachmentClaim,
    pub size: crate::pane::PaneSize,
    pub style: Option<PresentationStyle>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ReplyBody {
    /// The operation completed and carries no value.
    Done,
    Claim(ClaimReply),
    Outcome(crate::hierarchy::RetainedOutcome),
    Page(Page),
    Graphics(Result<graphics::Chunk, graphics::Error>),
    Accessibility(Result<accessibility::Chunk, accessibility::Error>),
    Directory(Result<directories::Value, directories::Error>),
    Worktree(Result<crate::worktree::ReadReply, crate::worktree::Error>),
    Direct(Result<crate::direct::ManagementReply, crate::direct::DirectError>),
    Interim(InterimReply),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ClaimReply {
    /// The channel is bound to this lease. Frames follow while the channel is visible.
    Attached { lease: Lease },
    /// Another client controls the Pane. The channel stays unbound.
    Takeover(Takeover),
    /// The Pane has no running shell yet. The channel stays unbound.
    Restored(Box<Restored>),
}

/// The Takeover Screen facts. `controller` is presentation only.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Takeover {
    pub session_ended: bool,
    pub target: PaneTarget,
    pub controller: control::Controller,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Restored {
    /// This running server created the Pane; no previous shell ended in a server restart.
    pub fresh: bool,
    /// End All Sessions cleared this Pane in the current running server.
    pub session_ended: bool,
    pub recovery: Option<directories::Target>,
    pub directory_unavailable: bool,
    pub server: [u8; 32],
    pub workspace: [u8; 32],
    pub tab: [u8; 32],
    pub pane: [u8; 32],
}

impl Operation {
    /// Checks the semantic bounds postcard cannot express.
    pub fn validate(&self) -> Result<(), Fatal> {
        match self {
            Self::Claim(claim) => check(claim.size.validate().is_ok()),
            Self::ScrollbackPage { rows, .. } => check((1..=limits::MAX_PAGE_ROWS).contains(rows)),
            _ => Ok(()),
        }
    }

    /// Whether the server may skip this operation after `Cancel`.
    pub fn cancellable(&self) -> bool {
        matches!(
            self,
            Self::ScrollbackPage { .. }
                | Self::Graphics { .. }
                | Self::Accessibility { .. }
                | Self::DirectoryRead(_)
                | Self::DirectoryCreate(_)
                | Self::WorktreeRead(_)
        )
    }
}

impl ReplyBody {
    /// Bulk replies travel at L2 in chunks of at most [`limits::BULK_CHUNK_BYTES`].
    pub fn bulk(&self) -> bool {
        matches!(
            self,
            Self::Page(_) | Self::Graphics(_) | Self::Accessibility(_)
        ) || matches!(self, Self::Interim(reply) if reply.bulk())
    }

    pub fn validate(&self) -> Result<(), Fatal> {
        match self {
            Self::Page(page) => page.validate(),
            _ => Ok(()),
        }
    }
}
