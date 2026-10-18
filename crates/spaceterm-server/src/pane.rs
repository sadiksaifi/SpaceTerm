//! Authenticated terminal operations. Native requests never grant local file authority.
pub mod accessibility;
pub mod directories;
pub mod effects;
pub mod graphics;
pub mod reading;
use crate::ServerError;
use serde::{Deserialize, Serialize};

pub const MAX_COLUMNS: u16 = 240;
pub const MAX_ROWS: u16 = 100;
pub const MAX_TERMINAL_FRAME: usize = 16 * 1024 * 1024;

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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
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
    Capabilities,
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
    Key {
        lease: Lease,
        key: Key,
    },
    Resize {
        lease: Lease,
        size: PaneSize,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Reply {
    Accessibility(Result<accessibility::Chunk, accessibility::Error>),
    AccessibilityRevision {
        revision: u64,
    },
    WorktreeRead(Result<crate::worktree::ReadReply, crate::worktree::Error>),
    MutationPending,
    DirectoryRead(Result<directories::Progress, directories::Error>),
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
    Capabilities {
        development_pane: bool,
        #[serde(default)]
        pane_takeover: bool,
        #[serde(default)]
        interactive_input: bool,
        #[serde(default)]
        native_effects: bool,
        #[serde(default)]
        reading: bool,
        #[serde(default)]
        graphics: bool,
        #[serde(default)]
        accessibility: bool,
        #[serde(default)]
        directory_recovery: bool,
        #[serde(default)]
        worktree_writes: bool,
    },
    Restored {
        #[serde(default)]
        recovery: Option<directories::Target>,
        #[serde(default)]
        directory_unavailable: bool,
        server: [u8; 32],
        workspace: [u8; 32],
        tab: [u8; 32],
        pane: [u8; 32],
    },
    Takeover {
        target: PaneTarget,
    },
    Attached {
        lease: Lease,
    },
    Screen(Box<Screen>),
    Unchanged,
    Ended,
    Closed,
    Refused(Refusal),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Color {
    Default,
    Palette(u8),
    Rgb([u8; 3]),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

/// The host supplies runtime ownership; the protocol supplies authenticated device/connection facts.
pub trait PaneService: Send {
    fn restore(
        &mut self,
        _store: crate::organization::OrganizationStore,
    ) -> Result<(), ServerError> {
        Ok(())
    }
    fn tick(&mut self) {}
    fn request(&mut self, device: [u8; 32], connection: [u8; 32], request: Request) -> Reply;
    fn disconnected(&mut self, connection: [u8; 32]);
    fn shutdown(&mut self) -> Result<(), ServerError>;
}

#[cfg(feature = "client")]
pub struct PaneConnection {
    transport: std::sync::Arc<PaneTransport>,
}
#[cfg(feature = "client")]
struct PaneTransport {
    stream: std::sync::Mutex<Option<std::os::unix::net::UnixStream>>,
    takeover: bool,
    interactive_input: bool,
    native_effects: bool,
    reading: bool,
    graphics: bool,
    accessibility: bool,
    directory_recovery: bool,
    worktree_writes: bool,
}
/// One transport for a desktop's inventory and Pane workers. It retains no credential.
#[cfg(feature = "client")]
#[derive(Default)]
pub(crate) struct SharedPaneConnection(std::sync::Mutex<std::sync::Weak<PaneTransport>>);
#[cfg(feature = "client")]
impl SharedPaneConnection {
    pub(crate) fn connect(
        &self,
        open: impl FnOnce() -> Result<PaneConnection, ServerError>,
    ) -> Result<PaneConnection, ServerError> {
        let mut saved = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(transport) = saved.upgrade()
            && transport
                .stream
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
        {
            return Ok(PaneConnection { transport });
        }
        // Serialize authentication as well as publication, so simultaneous Pane
        // workers never briefly allocate a separate host connection each.
        let connection = open()?;
        *saved = std::sync::Arc::downgrade(&connection.transport);
        Ok(connection)
    }
}
#[cfg(feature = "client")]
impl PaneConnection {
    pub fn worktree_writes(&self) -> bool {
        self.transport.worktree_writes
    }

    pub(crate) fn negotiate(
        mut stream: std::os::unix::net::UnixStream,
    ) -> Result<Self, ServerError> {
        let mut wire = crate::protocol::Wire::terminal(&mut stream)?;
        wire.write(&Request::Capabilities)?;
        match wire.read()? {
            Reply::Capabilities {
                development_pane: true,
                pane_takeover,
                interactive_input,
                native_effects,
                reading,
                graphics,
                accessibility,
                directory_recovery,
                worktree_writes,
            } => {
                let mut connection = Self::new(stream, pane_takeover);
                std::sync::Arc::get_mut(&mut connection.transport)
                    .ok_or(ServerError::Protocol)?
                    .accessibility = accessibility;
                std::sync::Arc::get_mut(&mut connection.transport)
                    .ok_or(ServerError::Protocol)?
                    .worktree_writes = worktree_writes;
                std::sync::Arc::get_mut(&mut connection.transport)
                    .ok_or(ServerError::Protocol)?
                    .interactive_input = interactive_input;
                std::sync::Arc::get_mut(&mut connection.transport)
                    .ok_or(ServerError::Protocol)?
                    .native_effects = native_effects;
                std::sync::Arc::get_mut(&mut connection.transport)
                    .ok_or(ServerError::Protocol)?
                    .reading = reading;
                std::sync::Arc::get_mut(&mut connection.transport)
                    .ok_or(ServerError::Protocol)?
                    .graphics = graphics;
                std::sync::Arc::get_mut(&mut connection.transport)
                    .ok_or(ServerError::Protocol)?
                    .directory_recovery = directory_recovery;
                Ok(connection)
            }
            Reply::Refused(reason) => Err(reason.into()),
            _ => Err(ServerError::Unsupported),
        }
    }
    pub(crate) fn new(stream: std::os::unix::net::UnixStream, takeover: bool) -> Self {
        Self {
            transport: std::sync::Arc::new(PaneTransport {
                stream: std::sync::Mutex::new(Some(stream)),
                takeover,
                interactive_input: false,
                native_effects: false,
                reading: false,
                graphics: false,
                accessibility: false,
                directory_recovery: false,
                worktree_writes: false,
            }),
        }
    }
    pub fn supports_directory_recovery(&self) -> bool {
        self.transport.directory_recovery
    }
    pub fn supports_native_effects(&self) -> bool {
        self.transport.native_effects
    }
    pub fn supports_graphics(&self) -> bool {
        self.transport.graphics
    }
    pub fn supports_accessibility(&self) -> bool {
        self.transport.accessibility
    }
    pub fn supports_reading(&self) -> bool {
        self.transport.reading
    }
    pub fn supports_takeover(&self) -> bool {
        self.transport.takeover
    }
    pub fn supports_interactive_input(&self) -> bool {
        self.transport.interactive_input
    }
    pub fn exchange(&mut self, request: &Request) -> Result<Reply, ServerError> {
        let mut stream = self
            .transport
            .stream
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let result = (|| {
            let mut wire =
                crate::protocol::Wire::terminal(stream.as_mut().ok_or(ServerError::NotRunning)?)?;
            wire.write(request)?;
            wire.read()
        })();
        // An uncertain exchange fences every subscriber of this transport. A new
        // connection gets a fresh host identity; no request is replayed here.
        if result.is_err() {
            *stream = None;
        }
        match result? {
            Reply::Refused(reason) => Err(reason.into()),
            reply => Ok(reply),
        }
    }
}

#[cfg(all(test, feature = "client"))]
mod transport_tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn uncertain_exchange_fences_all_subscribers_before_reconnecting() {
        let shared = SharedPaneConnection::default();
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut inventory = shared
            .connect(|| Ok(PaneConnection::new(client, true)))
            .unwrap();
        let mut pane = shared
            .connect(|| panic!("Pane must reuse inventory transport"))
            .unwrap();
        let peer = std::thread::spawn(move || {
            let mut wire = crate::protocol::Wire::terminal(&mut server).unwrap();
            assert!(matches!(
                wire.read::<Request>().unwrap(),
                Request::Inventory
            ));
            // A lost reply leaves the operation uncertain. The transport never retries it.
        });
        assert!(inventory.exchange(&Request::Inventory).is_err());
        peer.join().unwrap();
        assert!(matches!(
            pane.exchange(&Request::Capabilities),
            Err(ServerError::NotRunning)
        ));

        let (client, mut server) = UnixStream::pair().unwrap();
        let mut replacement = shared
            .connect(|| Ok(PaneConnection::new(client, false)))
            .unwrap();
        let peer = std::thread::spawn(move || {
            let mut wire = crate::protocol::Wire::terminal(&mut server).unwrap();
            assert!(matches!(
                wire.read::<Request>().unwrap(),
                Request::Capabilities
            ));
            wire.write(&Reply::Unchanged).unwrap();
        });
        assert!(matches!(
            replacement.exchange(&Request::Capabilities).unwrap(),
            Reply::Unchanged
        ));
        assert!(matches!(
            pane.exchange(&Request::Inventory),
            Err(ServerError::NotRunning)
        ));
        peer.join().unwrap();
    }
}
