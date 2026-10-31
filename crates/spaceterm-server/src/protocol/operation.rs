//! Client requests and their reply bodies.
//!
//! Operations reuse the `pane`, `hierarchy`, `worktree` and `direct` payload types where postcard
//! encodes them.
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
    /// A Workspace edit, or its Original Operation dispatched again with the same
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
    /// Reads `rows` Scrollback rows from position `start` of history `epoch` (see
    /// [`super::screen::History`]). The page leaves out rows the server already discarded, so it
    /// may start later. An ended epoch, or history hidden by the alternate screen, is refused as
    /// `Refusal::Stale`.
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
    /// Replied when the read settles. `Cancel` abandons it.
    WorktreeRead(crate::worktree::Read),
    /// Direct-network administration. Paired connections may send only what the host allows.
    Direct(crate::direct::Management),
    /// Stops the server. Account-local connections only.
    Shutdown,
    /// Opens an observer channel on any Pane. It never claims, resizes, starts a shell, focuses
    /// or receives effects. `session` fences the observed Terminal Session when given.
    Observe {
        channel: Channel,
        pane: crate::hierarchy::Id,
        session: Option<crate::hierarchy::Id>,
    },
    /// Runs one command at the prompt of the channel's Session, as one bracketed paste.
    /// `command` is at most [`MAX_RUN_COMMAND_BYTES`] and holds no C0 or C1 control character.
    Run {
        channel: Channel,
        run: RunId,
        command: String,
        session: crate::hierarchy::Id,
    },
    /// The receipt of a run, without resending its input.
    RunStatus {
        run: RunId,
    },
    /// Retires a settled run's receipt.
    RunAcknowledge {
        run: RunId,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Claim {
    /// A channel number greater than every channel this connection used before.
    pub channel: Channel,
    pub attachment: crate::attachments::Attachment,
    pub pane: crate::hierarchy::Id,
    pub claim: AttachmentClaim,
    /// `None` keeps the running Session's size; a fresh Session starts at the server default.
    pub size: Option<crate::pane::PaneSize>,
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
    Observe(ObserveReply),
    Run(RunReply),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ObserveReply {
    /// The channel observes this Session. Frames follow while the channel is visible.
    Observing {
        session: crate::hierarchy::Id,
        history: super::screen::History,
    },
    /// The Pane has no running Session. The channel stays unbound.
    NoSession { state: SessionState },
}

/// Why a Pane has no running Terminal Session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    NotStarted,
    Ended(crate::pane::SessionEnd),
}

/// The longest command `Operation::Run` accepts, in bytes.
pub const MAX_RUN_COMMAND_BYTES: usize = 16 * 1024;

/// Names one run. The client chooses it before dispatch, so a lost reply can be resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RunId(pub [u8; 16]);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunReply {
    /// The command was written at `start`.
    Admitted {
        start: HistoryPosition,
    },
    Refused(RunRefusal),
    Receipt(RunReceipt),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunRefusal {
    NotAtPrompt,
    InputPending,
    NoIntegration {
        /// The launch installed integration, but its first owner-marked prompt has not arrived.
        first_prompt_pending: bool,
    },
    NoBracketedPaste,
    SessionChanged,
    RunPending,
    Capacity,
}

/// One history row: `row` of history `epoch` (see [`super::screen::History`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPosition {
    pub epoch: u64,
    pub row: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunReceipt {
    pub run: RunId,
    pub session: crate::hierarchy::Id,
    pub state: RunState,
    /// Where the finish was observed, once it was.
    pub end: Option<HistoryPosition>,
}

/// A run's state. `Finished` without an exit status never means success.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunState {
    Admitted,
    Running,
    Finished {
        exit_status: Option<i32>,
        duration_ms: u64,
    },
    Unconfirmed(RunDoubt),
    SessionEnded,
    TakenOver,
}

/// Why the server cannot bind a finish to its run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunDoubt {
    NoExitStatus,
    NoCommandStart,
    CmdlineMismatch,
    IntegrationLost,
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
    /// Why the Pane's Session ended, if it did. Then Take Over starts a fresh one.
    pub session_ended: Option<crate::pane::SessionEnd>,
    pub target: PaneTarget,
    pub controller: control::Controller,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Restored {
    /// This running server created the Pane; no previous shell ended in a server restart.
    pub fresh: bool,
    /// Why this Pane's Session ended in the current running server, if one did.
    pub session_ended: Option<crate::pane::SessionEnd>,
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
            Self::Claim(claim) => check(claim.size.is_none_or(|size| size.validate().is_ok())),
            Self::Run { command, .. } => check(
                command.len() <= MAX_RUN_COMMAND_BYTES && !command.chars().any(|c| c.is_control()),
            ),
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
        )
    }

    pub fn validate(&self) -> Result<(), Fatal> {
        match self {
            Self::Page(page) => page.validate(),
            _ => Ok(()),
        }
    }
}
