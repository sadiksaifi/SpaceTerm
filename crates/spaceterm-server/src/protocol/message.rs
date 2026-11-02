//! Messages each peer sends after admission. Each variant travels as its own frame kind, and its
//! payload is postcard-encoded.
use super::limits;
use super::operation::{Operation, ReplyBody, Takeover};
use super::screen::ScreenUpdate;
use crate::pane::{
    FocusTarget, Key, LineState, PaneSize, PasteOutcome, Pointer, Wheel, accessibility, effects,
};
use serde::{Deserialize, Serialize};

pub use crate::pane::Refusal;

/// Identifies one request among those a client has in flight. A client may reuse an id only
/// after its reply arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub u32);

/// Identifies one claimed Pane on one connection. The client assigns channels in `Claim`. Channel
/// numbers strictly increase and are never reused within a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Channel(pub u32);

#[derive(Clone, Debug)]
pub enum ClientMessage {
    /// Exactly one `Reply` answers it, possibly deferred.
    Request(Box<Request>),
    /// Fire-and-forget terminal input. No reply; frames report the serials they reflect.
    Input(Input),
    FrameAck(FrameAck),
    AccessibilityAck(AccessibilityAck),
    View(View),
    EffectDone(EffectDone),
    /// Retires a resolved Workspace edit. No reply.
    Acknowledge(Acknowledge),
    /// Abandons a read. Its reply still arrives, refused as `Stale` if the server skipped it.
    Cancel(Cancel),
    Ping(Ping),
    Pong(Pong),
}

#[derive(Clone, Debug)]
pub enum ServerMessage {
    Reply(Box<Reply>),
    Frame(Box<Frame>),
    Accessibility(Box<AccessibilityMessage>),
    Pane(PaneMessage),
    /// The latest Workspace inventory. A newer revision replaces one not yet written.
    Inventory(Box<InventoryRevision>),
    Ping(Ping),
    Pong(Pong),
    /// The server closes the connection after this message.
    Goodbye(Fatal),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: RequestId,
    pub operation: Operation,
}

/// One ordered batch of terminal input for one channel, applied or discarded as a whole.
///
/// Serials start at 1 on each channel and increase by exactly 1 per message. A gap or repeat is a
/// protocol violation. Every event except `Focus` and `Resize` is accepted only under the server's
/// current focus generation, which equals `generation` once a leading `Focus` event applies.
/// `Resize` is never fenced by focus.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Input {
    pub channel: Channel,
    pub serial: u32,
    pub generation: u64,
    pub events: Vec<InputEvent>,
}

/// Terminal input. Input is never replayed after a reconnect or takeover.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum InputEvent {
    /// Moves the channel to `Input::generation`. Allowed only as the first event of a message.
    Focus(FocusTarget),
    Key(Key),
    Pointer(Pointer),
    Wheel(Wheel),
    /// One chunk of a paste. The server settles the paste after the chunk with `last` set and
    /// reports the outcome as `PaneEvent::Input` for that message. A discarded or missing chunk
    /// aborts the whole paste, and nothing partial reaches the PTY. A paste over
    /// [`limits::MAX_PASTE_BYTES`] settles as `PasteRejection::TooLarge`.
    Paste {
        text: String,
        last: bool,
    },
    ResolvePaste {
        id: u64,
        confirm: bool,
    },
    CopyOrForward {
        modifiers: u16,
    },
    Resize(PaneSize),
}

/// Acknowledges a frame after the client decoded and applied it, before rendering it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameAck {
    pub channel: Channel,
    pub frame: u64,
}

/// Acknowledges a semantic revision after the client applied its last message. It returns the
/// channel's semantic credit and makes `revision` the base of the generation's next update.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessibilityAck {
    pub channel: Channel,
    pub demand_generation: u64,
    pub revision: u64,
}

/// Whether the client presents a channel, and the frame rate it presents at. A hidden channel
/// receives no frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub channel: Channel,
    pub visible: bool,
    /// One of [`limits::FRAME_RATES`].
    pub max_hz: u8,
}

/// Completes the native effect request `effect` raised under focus generation `generation`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectDone {
    pub channel: Channel,
    pub generation: u64,
    pub effect: u64,
    pub completion: effects::Completion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acknowledge {
    pub sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cancel {
    pub id: RequestId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ping {
    pub nonce: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pong {
    pub nonce: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reply {
    pub id: RequestId,
    pub result: Result<ReplyBody, Refusal>,
}

/// One screen update for one channel. Each channel has at most one unacknowledged frame.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Frame {
    pub channel: Channel,
    /// Increases by at least 1 per frame on a channel.
    pub frame: u64,
    /// The acknowledged frame this update applies to. `None` is a keyframe.
    pub base: Option<u64>,
    /// The highest input serial written to the PTY before this state was captured.
    pub input: u32,
    pub update: ScreenUpdate,
}

/// One message of a semantic revision for one channel. Each channel has at most one
/// unacknowledged revision.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccessibilityMessage {
    pub channel: Channel,
    pub update: accessibility::Update,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneMessage {
    pub channel: Channel,
    pub event: PaneEvent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PaneEvent {
    /// Another client controls the Pane now. The channel is closed, and later input on it is
    /// discarded.
    Takeover(Takeover),
    /// The shell exited with code zero, which closed its Pane. The channel is closed.
    Ended,
    /// The Terminal Session ended and its Pane stays: End All Sessions ended it, or its shell
    /// exited with a nonzero code or after a signal. The channel is closed, and a new claim says
    /// why.
    SessionEnded,
    /// The Terminal Session failed. The channel is closed.
    Failed,
    /// The Terminal Session could not start in its directory. The channel is closed, and a new
    /// claim offers directory recovery.
    DirectoryUnavailable,
    /// A native effect request. The client answers with `EffectDone`.
    Effect(effects::Request),
    /// The server discarded the Input message `serial`, or settled the paste it completed.
    Input { serial: u32, outcome: InputOutcome },
    /// The PTY's line mode changed. The server sends the current state first on each channel,
    /// then each change it reads after input, after output and after focus changes.
    Line(LineState),
    /// The highest input serial written to the PTY and the PTY output byte count, sent at most
    /// once per frame interval when either advanced without a frame.
    Progress { input: u32, output_bytes: u64 },
    /// The channel's run settled. Boxed to keep events small; postcard encodes it as the bare
    /// receipt.
    RunFinished(Box<super::operation::RunReceipt>),
}

impl PaneEvent {
    /// Whether this is its channel's last event: `Ended`, `SessionEnded`, `Failed`,
    /// `DirectoryUnavailable` or `Takeover`.
    pub fn last(&self) -> bool {
        matches!(
            self,
            Self::Ended
                | Self::SessionEnded
                | Self::Failed
                | Self::DirectoryUnavailable
                | Self::Takeover(_)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputOutcome {
    Discarded(Refusal),
    Paste(PasteOutcome),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InventoryRevision {
    pub inventory: crate::hierarchy::Inventory,
    pub next_sequence: u64,
}

/// Why a peer closes a connection. Values are classifications only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum Fatal {
    #[error("The peer broke the SpaceTerm Protocol.")]
    ProtocolViolation,
    #[error("The connection reached a capacity limit.")]
    Capacity,
    #[error("The connection stopped making progress.")]
    Stalled,
    #[error("This device's access was revoked.")]
    Revoked,
    #[error("The server is stopping.")]
    Stopping,
    #[error("The peers have no compatible protocol.")]
    Incompatible,
    /// The sender failed on its own, for example a frame it could not encode. Not the
    /// receiver's fault.
    #[error("The peer failed.")]
    Internal,
}

impl ClientMessage {
    /// Checks the semantic bounds postcard cannot express.
    pub fn validate(&self) -> Result<(), Fatal> {
        match self {
            Self::Request(request) => request.operation.validate(),
            Self::Input(input) => input.validate(),
            Self::View(view) => check(limits::FRAME_RATES.contains(&view.max_hz)),
            Self::EffectDone(done) => match &done.completion {
                effects::Completion::Text(text) => {
                    check(text.len() <= effects::MAX_CLIPBOARD_BYTES)
                }
                _ => Ok(()),
            },
            Self::FrameAck(_)
            | Self::AccessibilityAck(_)
            | Self::Acknowledge(_)
            | Self::Cancel(_)
            | Self::Ping(_)
            | Self::Pong(_) => Ok(()),
        }
    }
}

impl Input {
    pub fn validate(&self) -> Result<(), Fatal> {
        check(self.serial != 0 && !self.events.is_empty())?;
        check(self.events.len() <= limits::MAX_INPUT_EVENTS)?;
        for (index, event) in self.events.iter().enumerate() {
            match event {
                InputEvent::Focus(_) => check(index == 0)?,
                InputEvent::Key(key) => check(
                    key.logical.len() <= crate::pane::MAX_KEY_TEXT_BYTES
                        && key
                            .text
                            .as_ref()
                            .is_none_or(|text| text.len() <= crate::pane::MAX_KEY_TEXT_BYTES),
                )?,
                InputEvent::Paste { text, .. } => {
                    check(text.len() <= limits::MAX_PASTE_CHUNK_BYTES)?
                }
                InputEvent::Resize(size) => check(size.validate().is_ok())?,
                InputEvent::Pointer(_)
                | InputEvent::Wheel(_)
                | InputEvent::ResolvePaste { .. }
                | InputEvent::CopyOrForward { .. } => {}
            }
        }
        Ok(())
    }
}

impl ServerMessage {
    /// Checks the semantic bounds postcard cannot express.
    pub fn validate(&self) -> Result<(), Fatal> {
        match self {
            Self::Frame(frame) => {
                check(frame.base.is_none_or(|base| base < frame.frame))?;
                frame.update.validate()
            }
            Self::Accessibility(message) => message.update.validate(),
            Self::Reply(reply) => match &reply.result {
                Ok(body) => body.validate(),
                Err(_) => Ok(()),
            },
            Self::Inventory(revision) => {
                let panes = revision
                    .inventory
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.tabs)
                    .flat_map(|tab| &tab.panes);
                let mut count = 0;
                for pane in panes {
                    count += 1;
                    check(count <= crate::hierarchy::MAX_PANES)?;
                    check(pane.observers.names.len() <= crate::hierarchy::MAX_OBSERVER_NAMES)?;
                    check(!pane.observers.names.is_empty() || pane.observers.others == 0)?;
                    if let Some(snapshot) = &pane.presentation {
                        check(snapshot.validate().is_ok())?;
                    }
                }
                Ok(())
            }
            Self::Pane(_) | Self::Ping(_) | Self::Pong(_) | Self::Goodbye(_) => Ok(()),
        }
    }
}

pub(crate) fn check(condition: bool) -> Result<(), Fatal> {
    if condition {
        Ok(())
    } else {
        Err(Fatal::ProtocolViolation)
    }
}
