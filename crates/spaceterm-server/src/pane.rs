//! Authenticated terminal operations. Native requests never grant local file authority.
pub mod accessibility;
pub mod control;
pub mod directories;
pub mod effects;
pub mod graphics;
pub mod metadata;
pub mod presentation;
pub mod reading;
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

/// A host's in-process command vocabulary for one connection and its leases. It never travels on
/// the wire: the transport maps each protocol [`crate::protocol::Operation`] and
/// [`crate::protocol::InputEvent`] to these commands on the server, and the desktop's Pane worker
/// maps them back to operations on its channel. Variants disappear as the polled semantics they
/// carry are replaced by pushed presentation, inventory and Scrollback. It derives serde only
/// for test fixtures that cross a process boundary.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    WorktreeRead(crate::worktree::Read),
    DirectoryRead(directories::Request),
    DirectoryCreate(directories::CreationRequest),
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
    Read {
        lease: Lease,
        operation: reading::Operation,
    },
    ReadingStatus {
        lease: Lease,
    },
    SelectionChunk {
        lease: Lease,
        generation: u64,
        offset: usize,
    },
    NativeEffects {
        lease: Lease,
        generation: u64,
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
    Poll {
        lease: Lease,
    },
    PollChanges {
        lease: Lease,
        base: Option<u64>,
    },
    PresentationDemand {
        lease: Lease,
        visible: bool,
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Reply {
    Accessibility(Result<accessibility::Chunk, accessibility::Error>),
    AccessibilityRevision {
        revision: u64,
    },
    WorktreeRead(Result<crate::worktree::ReadReply, crate::worktree::Error>),
    MutationPending,
    DirectoryRead(Result<directories::Progress, directories::Error>),
    DirectoryCreate(Result<directories::Progress, directories::Error>),
    GraphicsChunk(Result<graphics::Chunk, graphics::Error>),
    Reading {
        accepted: bool,
    },
    ReadingReady {
        generation: u64,
        selection_revision: u64,
        selection_present: bool,
    },
    SelectionChunk {
        generation: u64,
        offset: usize,
        total: usize,
        bytes: Vec<u8>,
    },
    SelectionPending,
    NativeEffects {
        request: Option<effects::Request>,
        hidden_input: bool,
    },
    Paste(PasteOutcome),
    Inventory {
        inventory: crate::hierarchy::Inventory,
        next_sequence: u64,
    },
    Outcome(crate::hierarchy::RetainedOutcome),
    Restored {
        /// This running server created the Pane; no previous shell ended in a server restart.
        fresh: bool,
        /// End All Sessions cleared this Pane in the current running server.
        session_ended: bool,
        recovery: Option<directories::Target>,
        directory_unavailable: bool,
        server: [u8; 32],
        workspace: [u8; 32],
        tab: [u8; 32],
        pane: [u8; 32],
    },
    /// This client does not control the Pane. `controller` says who does, for presentation only.
    Takeover {
        session_ended: bool,
        target: PaneTarget,
        controller: control::Controller,
    },
    Attached {
        lease: Lease,
    },
    Screen(Box<Screen>),
    ScreenChanges(Box<presentation::RowChanges>),
    Unchanged,
    Ended,
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
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Color {
    Default,
    Palette(u8),
    Rgb([u8; 3]),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    #[serde(default)]
    pub hyperlink: Option<u16>,
    pub text: String,
    pub foreground: Color,
    pub background: Color,
    pub underline_color: Color,
    pub flags: u16,
    pub underline: u8,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Screen {
    #[serde(default)]
    pub accessibility_revision: u64,
    #[serde(default)]
    pub graphics: Option<Result<graphics::Snapshot, graphics::Error>>,
    #[serde(default)]
    pub cell_pixels: Option<(u16, u16)>,
    #[serde(default)]
    pub reading: Option<reading::Snapshot>,
    /// Per-screen targets; cells refer to indices in this bounded table.
    #[serde(default)]
    pub hyperlinks: Vec<effects::Hyperlink>,
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub mouse_tracking: bool,
    #[serde(default)]
    pub alternate_scroll: bool,
    pub columns: u16,
    pub rows: Vec<Vec<Cell>>,
    pub wrapped: Vec<bool>,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub palette: Vec<[u8; 3]>,
    pub reversed: bool,
    pub foreground_overridden: bool,
    pub background_overridden: bool,
    pub cursor_overridden: bool,
    pub palette_overrides: Vec<bool>,
    pub cursor: Option<(u16, u16, u8)>,
    pub cursor_visible: bool,
    pub cursor_blinking: bool,
    pub cursor_shape: u8,
    pub cursor_color: [u8; 3],
    pub cursor_text: [u8; 3],
    pub alternate: bool,
}
