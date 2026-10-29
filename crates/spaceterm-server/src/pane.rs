//! Authenticated terminal operations. Native requests never grant local file authority.
pub mod accessibility;
pub mod control;
pub mod directories;
pub mod effects;
pub mod graphics;
pub mod metadata;
pub mod keys;
pub mod run;
use crate::ServerError;
use serde::{Deserialize, Serialize};

// One supported grid budget shared by desktop geometry, emulator, PTY and wire validation.
pub const MAX_COLUMNS: u16 = 512;
pub const MAX_ROWS: u16 = 256;
/// Maximum number of ordered key events accepted by one focused batch.
pub const MAX_KEY_BATCH: usize = 32;
pub const MAX_KEY_TEXT_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSize {
    pub columns: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}
impl PaneSize {
    pub fn validate(self) -> Result<Self, ServerError> {
        if !(2..=MAX_COLUMNS).contains(&self.columns)
            || !(2..=MAX_ROWS).contains(&self.rows)
            || !(1..=256).contains(&self.cell_width)
            || !(1..=256).contains(&self.cell_height)
        {
            return Err(ServerError::Unsupported);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub server: [u8; 32],
    pub session: [u8; 32],
    pub control: [u8; 32],
    pub connection: [u8; 32],
}

/// How the PTY's line discipline treats typed input, and whether the shell reads it. The server
/// reads these flags from the PTY master; they carry no terminal contents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LineMode {
    /// ICANON: the kernel collects a whole line before the program reads it.
    pub canonical: bool,
    /// ECHO: the kernel echoes typed input.
    pub echo: bool,
    /// ISIG: the kernel turns the interrupt, quit and suspend keys into signals.
    pub signals: bool,
    /// The shell's process group owns the terminal's foreground.
    pub shell_foreground: bool,
}

impl LineMode {
    /// The mode reported when the PTY cannot be read: no hidden input and no line editor.
    pub const UNKNOWN: Self = Self {
        canonical: false,
        echo: false,
        signals: false,
        shell_foreground: false,
    };

    /// Whether the terminal hides typed input, as at a password prompt.
    pub fn hidden_input(self) -> bool {
        self.canonical && !self.echo
    }

    /// Whether the shell's own line editor reads typed input: zsh's ZLE, bash's readline or
    /// dash at its prompt. Only then does a printable key normally echo at the cursor.
    pub fn line_editor(self) -> bool {
        !self.canonical && !self.echo && self.signals && self.shell_foreground
    }
}

/// A Terminal Session's line mode with the epoch it was read in. The epoch increases by 1 each
/// time the server reads a mode that differs from the previous one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LineState {
    pub epoch: u64,
    pub mode: LineMode,
}

/// Why a Pane the running server keeps has no Terminal Session. It carries bounded facts only,
/// never the shell's output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionEnd {
    /// End All Sessions ended the Session.
    EndAllSessions,
    /// The shell exited on its own with a nonzero code. A zero exit closes the Pane instead.
    Exited { code: u32 },
    /// A signal ended the shell.
    Signaled,
}

/// Identifies a running Pane without granting access to its terminal content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneTarget {
    pub server: [u8; 32],
    pub session: [u8; 32],
}
impl From<Lease> for PaneTarget {
    fn from(lease: Lease) -> Self {
        Self {
            server: lease.server,
            session: lease.session,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Key {
    pub physical: u32,
    pub action: u8,
    pub logical: String,
    pub text: Option<String>,
    pub unshifted: Option<char>,
    pub modifiers: u16,
    pub consumed: u16,
    pub option_as_alt: bool,
}

/// Eligibility supplied by the controlling client's current local focus generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FocusTarget {
    Unfocused,
    FocusedPane,
    Terminal,
    PasteConfirmation { id: u64 },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum PointerPhase {
    Press,
    Motion,
    Release,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum PointerButton {
    Left,
    Middle,
    Right,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum WheelPhase {
    GestureStarted,
    GestureChanged,
    GestureEnded,
    GestureCancelled,
    MomentumStarted,
    MomentumChanged,
    MomentumEnded,
    MomentumCancelled,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pointer {
    pub screen_generation: u64,
    pub phase: PointerPhase,
    pub button: Option<PointerButton>,
    pub x: f32,
    pub y: f32,
    pub modifiers: u16,
    pub shift_selection: bool,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wheel {
    pub screen_generation: u64,
    pub horizontal_steps: i32,
    pub vertical_steps: i32,
    pub phase: WheelPhase,
    pub x: f32,
    pub y: f32,
    pub modifiers: u16,
    pub shift_selection: bool,
}

/// Input is never replayed or accepted outside its exact lease and focus generation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum InteractiveInput {
    Key(Key),
    CopyOrForward { modifiers: u16 },
    Pointer(Pointer),
    Wheel(Wheel),
    Paste { text: String },
    ResolvePaste { id: u64, confirm: bool },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasteConfirmation {
    pub id: u64,
    pub byte_len: u32,
    pub line_count: u32,
    pub multiline: bool,
    pub control_bytes: bool,
    pub closing_fence: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PasteRejection {
    Empty,
    TooLarge { limit: u32 },
    ConfirmationPending,
    TerminalUnfocused,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PasteOutcome {
    Written,
    ConfirmationRequired(PasteConfirmation),
    Rejected(PasteRejection),
    Cancelled,
    Stale,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PresentationStyle {
    pub light: bool,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub palette: [[u8; 3]; 16],
    pub cursor: [u8; 3],
    pub cursor_text: Option<[u8; 3]>,
    pub bold_as_bright: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AttachmentClaim {
    Select {
        explicit: bool,
    },
    Start {
        server: [u8; 32],
    },
    Repair {
        target: directories::Target,
        replacement: Option<std::path::PathBuf>,
    },
    TakeOver {
        target: PaneTarget,
    },
}

/// The in-process command set of a host and of the desktop's Pane worker, for one connection and
/// its leases. It never travels on the wire. On the server, the connection service maps each
/// protocol [`crate::protocol::Operation`] and [`crate::protocol::InputEvent`] to these commands.
/// On the desktop, the Pane worker maps them to operations and input on its channel.
#[derive(Clone, Debug)]
pub enum Request {
    EndAccessibilityDemand {
        lease: Lease,
    },
    Accessibility {
        lease: Lease,
        revision: Option<u64>,
        offset: usize,
        base: Option<u64>,
    },
    AccessibilitySelection {
        lease: Lease,
        focus_generation: u64,
        selection: Option<accessibility::SelectionRefs>,
    },
    RepairDirectory {
        target: directories::Target,
        replacement: Option<std::path::PathBuf>,
        size: PaneSize,
        style: Option<PresentationStyle>,
    },
    GraphicsChunk {
        lease: Lease,
        image: graphics::ImageKey,
        offset: usize,
    },
    CompleteClipboard {
        lease: Lease,
        generation: u64,
        id: u64,
        completion: effects::Completion,
    },
    Focus {
        lease: Lease,
        generation: u64,
        target: FocusTarget,
    },
    Input {
        lease: Lease,
        generation: u64,
        input: InteractiveInput,
    },
    /// Ordered key events accepted under one exact lease and focus generation.
    /// A lost reply never permits replay. Acceptance does not imply PTY execution.
    Keys {
        lease: Lease,
        generation: u64,
        keys: Vec<Key>,
    },
    ClaimPane {
        attachment: crate::attachments::Attachment,
        pane: crate::hierarchy::Id,
        claim: AttachmentClaim,
        size: PaneSize,
        style: Option<PresentationStyle>,
    },
    ReleaseAttachment {
        attachment: crate::attachments::Attachment,
    },
    /// What a connection receives pushed: the inventory and this client's next sequence.
    Inventory,
    Mutate {
        sequence: u64,
        mutation: crate::hierarchy::Mutation,
    },
    Resolve {
        sequence: u64,
    },
    Acknowledge {
        sequence: u64,
    },
    SelectPane {
        pane: crate::hierarchy::Id,
        explicit: bool,
        size: PaneSize,
        style: Option<PresentationStyle>,
    },
    Attach {
        create: bool,
        size: PaneSize,
        resume: Option<Lease>,
        style: Option<PresentationStyle>,
    },
    /// Selection acquires an unclaimed Pane, but never takes it from another client.
    Select {
        create: bool,
        size: PaneSize,
        resume: Option<PaneTarget>,
        style: Option<PresentationStyle>,
    },
    /// Explicit first use of a retained Pane, fenced by stable identity and current server boot.
    StartRestored {
        server: [u8; 32],
        pane: [u8; 32],
        size: PaneSize,
        style: Option<PresentationStyle>,
    },
    TakeOver {
        target: PaneTarget,
        size: PaneSize,
        style: Option<PresentationStyle>,
    },
    Release {
        lease: Lease,
    },
    /// Whether the client presents the channel bound to `lease`, and at what frame rate. The
    /// desktop's Pane worker sends it as `ClientMessage::View`; it never reaches a host.
    View {
        lease: Lease,
        visible: bool,
        max_hz: u8,
    },
    Key {
        lease: Lease,
        key: Key,
    },
    Resize {
        lease: Lease,
        size: PaneSize,
    },
    /// Clear only this Pane's terminal content under its exact control and connection lease.
    ClearScreenAndScrollback {
        lease: Lease,
    },
    Style {
        lease: Lease,
        style: PresentationStyle,
    },
    Close {
        lease: Lease,
    },
    Shutdown,
}

/// The reply to one [`Request`], in process only.
#[derive(Clone, Debug)]
pub enum Reply {
    Accessibility(Result<accessibility::Chunk, accessibility::Error>),
    MutationPending,
    GraphicsChunk(Result<graphics::Chunk, graphics::Error>),
    Paste(PasteOutcome),
    Inventory {
        inventory: crate::hierarchy::Inventory,
        next_sequence: u64,
    },
    Outcome(crate::hierarchy::RetainedOutcome),
    Restored {
        /// This running server created the Pane; no previous shell ended in a server restart.
        fresh: bool,
        /// Why this Pane's Session ended in the current running server, if one did.
        session_ended: Option<SessionEnd>,
        recovery: Option<directories::Target>,
        directory_unavailable: bool,
        server: [u8; 32],
        workspace: [u8; 32],
        tab: [u8; 32],
        pane: [u8; 32],
    },
    /// This client does not control the Pane. `controller` says who does, for presentation only.
    Takeover {
        session_ended: Option<SessionEnd>,
        target: PaneTarget,
        controller: control::Controller,
    },
    Attached {
        lease: Lease,
    },
    Unchanged,
    Closed,
    Refused(Refusal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    Controlled,
    Missing,
    Stale,
    Unsupported,
    Failed,
    CleanupUnresolved,
    DirectoryUnavailable,
    StorageUnavailable,
    Capacity,
    /// A service operation is about to end every Terminal Session, so none may start.
    ServiceBusy,
}
impl From<Refusal> for ServerError {
    fn from(value: Refusal) -> Self {
        match value {
            Refusal::Capacity => Self::Capacity,
            Refusal::Controlled => Self::Controlled,
            Refusal::Missing => Self::SessionUnavailable,
            Refusal::Stale => Self::StaleControl,
            Refusal::Unsupported => Self::Unsupported,
            Refusal::Failed => Self::StartupFailed,
            Refusal::DirectoryUnavailable => Self::DirectoryUnavailable,
            Refusal::StorageUnavailable => Self::StorageUnavailable,
            Refusal::CleanupUnresolved => Self::CleanupUnresolved,
            Refusal::ServiceBusy => Self::ServiceBusy,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Color {
    Default,
    Palette(u8),
    Rgb([u8; 3]),
}
