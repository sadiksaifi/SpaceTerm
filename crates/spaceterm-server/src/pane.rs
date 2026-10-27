//! Authenticated terminal operations. Native requests never grant local file authority.
pub mod accessibility;
pub mod control;
pub mod directories;
pub mod effects;
pub mod graphics;
pub mod metadata;
pub mod presentation;
pub mod reading;
#[cfg(feature = "client")]
mod transport;
use crate::ServerError;
use serde::{Deserialize, Serialize};

// One supported grid budget shared by desktop geometry, emulator, PTY and wire validation.
pub const MAX_COLUMNS: u16 = 512;
pub const MAX_ROWS: u16 = 256;
/// Maximum number of ordered key events accepted by one focused batch.
pub const MAX_KEY_BATCH: usize = 32;
pub const MAX_KEY_TEXT_BYTES: usize = 4096;
// Includes a full grid of 64-codepoint cells plus bounded links and graphics metadata.
// Image pixels continue to travel in independently bounded GraphicsChunk messages.
pub const MAX_TERMINAL_FRAME: usize = 96 * 1024 * 1024;

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
    Direct(crate::direct::Management),
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
    SelectionChunk {
        lease: Lease,
        generation: u64,
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
    /// `Unchanged` accepts the batch. `Screen` or `ScreenChanges` accepts it and carries the
    /// Pane's next publication, encoded exactly as the next `PollChanges` would have.
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
    /// One output check, answered by `Reply::Cycle`. `effects` names the acknowledged focus
    /// generation whose native effects the check also takes. `reading` asks whether the
    /// accepted reading operation has completed.
    PollChanges {
        lease: Lease,
        base: Option<u64>,
        effects: Option<u64>,
        reading: bool,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Reply {
    Direct(Result<crate::direct::ManagementReply, crate::direct::DirectError>),
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
    SelectionChunk {
        generation: u64,
        offset: usize,
        total: usize,
        bytes: Vec<u8>,
    },
    SelectionPending,
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
        key_input_batches: bool,
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
        directory_creation: bool,
        #[serde(default)]
        directory_matching: bool,
        #[serde(default)]
        worktree_writes: bool,
        #[serde(default)]
        end_all_sessions: bool,
        #[serde(default)]
        workspace_policy: bool,
        #[serde(default)]
        presentation_changes: bool,
    },
    Restored {
        /// This running server created the Pane; no previous shell ended in a server restart.
        #[serde(default)]
        fresh: bool,
        /// End All Sessions cleared this Pane in the current running server.
        #[serde(default)]
        session_ended: bool,
        #[serde(default)]
        recovery: Option<directories::Target>,
        #[serde(default)]
        directory_unavailable: bool,
        server: [u8; 32],
        workspace: [u8; 32],
        tab: [u8; 32],
        pane: [u8; 32],
    },
    /// This client does not control the Pane. `controller` says who does, for presentation only.
    Takeover {
        #[serde(default)]
        session_ended: bool,
        target: PaneTarget,
        controller: control::Controller,
    },
    Attached {
        lease: Lease,
    },
    Screen(Box<Screen>),
    ScreenChanges(Box<presentation::RowChanges>),
    Cycle(Box<presentation::Cycle>),
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color {
    #[default]
    Default,
    Palette(u8),
    Rgb([u8; 3]),
}
impl Color {
    fn is_default(&self) -> bool {
        *self == Self::Default
    }
}
fn is_zero<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}
/// Fields with their default value are omitted, so a plain cell carries only its text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyperlink: Option<u16>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Color::is_default")]
    pub foreground: Color,
    #[serde(default, skip_serializing_if = "Color::is_default")]
    pub background: Color,
    #[serde(default, skip_serializing_if = "Color::is_default")]
    pub underline_color: Color,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub flags: u16,
    #[serde(default, skip_serializing_if = "is_zero")]
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
    /// Records the Client Name an admitted client supplied, before any of its requests.
    fn admitted(&mut self, _client: [u8; 32], _name: control::ClientName) {}
    /// Permanently releases this paired identity's live and dormant control grants.
    fn revoked(&mut self, _client: [u8; 32]) {}
    fn request(&mut self, device: [u8; 32], connection: [u8; 32], request: Request) -> Reply;
    /// Applies a request in arrival order. A `Later` reply waits without the host's service
    /// lock while the connection keeps reading, then `finish` completes it under that lock.
    fn handle(&mut self, device: [u8; 32], connection: [u8; 32], request: Request) -> Handled {
        Handled::Now(self.request(device, connection, request))
    }
    /// Completes a `Handled::Later` reply after its wait, under the host's service lock.
    fn finish(&mut self, _device: [u8; 32], _connection: [u8; 32], _finish: u64) -> Reply {
        Reply::Refused(Refusal::Unsupported)
    }
    fn disconnected(&mut self, connection: [u8; 32]);
    fn shutdown(&mut self) -> Result<(), ServerError>;
}
/// A reply completed after the service lock is released.
pub trait PendingReply: Send {
    /// Blocks without any service lock until the Pane publishes or `deadline` passes.
    fn wait(&mut self, deadline: std::time::Instant);
}
#[allow(
    clippy::large_enum_variant,
    reason = "moved once per request, like the Reply that `request` already returns by value"
)]
pub enum Handled {
    Now(Reply),
    /// The host drops this reply, and its connection, if the connection ends or loses
    /// authority first. The client then fences the request as unconfirmed.
    Later {
        pending: Box<dyn PendingReply>,
        deadline: std::time::Instant,
        finish: u64,
    },
}

#[cfg(feature = "client")]
pub struct PaneConnection {
    transport: std::sync::Arc<PaneTransport>,
    last_exchange: Option<ExchangeTiming>,
}
/// Local bounded numeric evidence; no terminal data or operation contents are retained.
#[cfg(feature = "client")]
#[derive(Clone, Copy, Debug)]
pub struct ExchangeTiming {
    pub queue_wait: std::time::Duration,
    pub exchange: std::time::Duration,
}
#[cfg(feature = "client")]
struct PaneTransport {
    measurement_id: u64,
    measurement_sequence: std::sync::atomic::AtomicU64,
    exchanges: transport::Exchanges,
    presentation_changes: bool,
    takeover: bool,
    interactive_input: bool,
    key_input_batches: bool,
    native_effects: bool,
    reading: bool,
    graphics: bool,
    accessibility: bool,
    directory_recovery: bool,
    directory_creation: bool,
    directory_matching: bool,
    worktree_writes: bool,
    end_all_sessions: bool,
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
            && !transport.exchanges.fenced()
        {
            return Ok(PaneConnection {
                transport,
                last_exchange: None,
            });
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
    pub fn supports_end_all_sessions(&self) -> bool {
        self.transport.end_all_sessions
    }
    pub fn worktree_writes(&self) -> bool {
        self.transport.worktree_writes
    }

    pub(crate) fn negotiate(
        mut stream: impl crate::transport::Transport + 'static,
    ) -> Result<Self, ServerError> {
        let mut wire = crate::protocol::Wire::terminal(&mut stream)?;
        match wire.call(0, &Request::Capabilities)? {
            Reply::Capabilities {
                development_pane: true,
                pane_takeover,
                interactive_input,
                key_input_batches,
                native_effects,
                reading,
                graphics,
                accessibility,
                directory_recovery,
                directory_creation,
                directory_matching,
                worktree_writes,
                end_all_sessions,
                workspace_policy: true,
                presentation_changes,
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
                    .end_all_sessions = end_all_sessions;
                std::sync::Arc::get_mut(&mut connection.transport)
                    .ok_or(ServerError::Protocol)?
                    .interactive_input = interactive_input;
                std::sync::Arc::get_mut(&mut connection.transport)
                    .ok_or(ServerError::Protocol)?
                    .key_input_batches = key_input_batches;
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
                std::sync::Arc::get_mut(&mut connection.transport)
                    .ok_or(ServerError::Protocol)?
                    .directory_creation = directory_creation;
                std::sync::Arc::get_mut(&mut connection.transport)
                    .ok_or(ServerError::Protocol)?
                    .directory_matching = directory_matching;
                std::sync::Arc::get_mut(&mut connection.transport)
                    .ok_or(ServerError::Protocol)?
                    .presentation_changes = presentation_changes;
                Ok(connection)
            }
            Reply::Capabilities {
                development_pane: true,
                workspace_policy: false,
                ..
            } => Err(ServerError::MissingRequiredCapability),
            Reply::Refused(reason) => Err(reason.into()),
            _ => Err(ServerError::Unsupported),
        }
    }
    pub(crate) fn new(stream: impl crate::transport::Transport + 'static, takeover: bool) -> Self {
        Self {
            transport: std::sync::Arc::new(PaneTransport {
                measurement_id: crate::measurement::scope(),
                measurement_sequence: std::sync::atomic::AtomicU64::new(0),
                exchanges: transport::Exchanges::new(Box::new(stream)),
                presentation_changes: false,
                takeover,
                interactive_input: false,
                key_input_batches: false,
                native_effects: false,
                reading: false,
                graphics: false,
                accessibility: false,
                directory_recovery: false,
                directory_creation: false,
                directory_matching: false,
                worktree_writes: false,
                end_all_sessions: false,
            }),
            last_exchange: None,
        }
    }
    pub fn supports_directory_matching(&self) -> bool {
        self.transport.directory_matching
    }
    pub fn supports_directory_creation(&self) -> bool {
        self.transport.directory_creation
    }
    pub fn supports_directory_recovery(&self) -> bool {
        self.transport.directory_recovery
    }
    pub fn supports_presentation_changes(&self) -> bool {
        self.transport.presentation_changes
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
    pub fn supports_key_input_batches(&self) -> bool {
        self.transport.key_input_batches
    }
    pub fn supports_interactive_input(&self) -> bool {
        self.transport.interactive_input
    }
    pub fn exchange(&mut self, request: &Request) -> Result<Reply, ServerError> {
        use crate::measurement::{self, Event};
        let operation = measurement::Operation::of(request);
        let connection = self.transport.measurement_id;
        measurement::record(Event::RpcQueued {
            connection,
            operation,
        });
        let queued = std::time::Instant::now();
        let turn = self.transport.exchanges.admit()?;
        let admitted = std::time::Instant::now();
        let sequence = if connection == 0 {
            0
        } else {
            self.transport
                .measurement_sequence
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1
        };
        measurement::record(Event::RpcAdmitted {
            connection,
            sequence,
            operation,
        });
        let result = (|| {
            // The writer is released here; other callers' requests overlap this reply.
            let pending = turn.write(request)?;
            measurement::record(Event::RequestWritten {
                connection,
                sequence,
                operation,
            });
            let reply: Reply =
                serde_json::from_slice(&pending.reply()?).map_err(|_| ServerError::Protocol)?;
            measurement::record(Event::ReplyRead {
                connection,
                sequence,
                operation,
            });
            if let Reply::Inventory { inventory, .. } = &reply {
                let panes: Vec<_> = inventory
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.tabs)
                    .flat_map(|tab| &tab.panes)
                    .collect();
                if panes.len() > crate::hierarchy::MAX_PANES {
                    return Err(ServerError::Protocol);
                }
                for pane in panes {
                    if let Some(snapshot) = &pane.presentation {
                        snapshot.validate()?;
                    }
                }
            }
            Ok(reply)
        })();
        // An uncertain exchange fences every subscriber of this transport, including every
        // outstanding request. A new connection gets a fresh host identity; no request is
        // replayed here.
        if let Err(error) = result {
            self.transport.exchanges.fence(error);
        }
        self.last_exchange = Some(ExchangeTiming {
            queue_wait: admitted.duration_since(queued),
            exchange: admitted.elapsed(),
        });
        match result? {
            Reply::Refused(reason) => Err(reason.into()),
            reply => Ok(reply),
        }
    }
    pub fn last_exchange_timing(&self) -> Option<ExchangeTiming> {
        self.last_exchange
    }
}

#[cfg(all(test, feature = "client"))]
mod transport_tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn a_retained_runtime_without_workspace_policy_is_refused_before_operations() {
        for capability in [None, Some(false)] {
            let (client, mut server) = UnixStream::pair().unwrap();
            let peer = std::thread::spawn(move || {
                let mut wire = crate::protocol::Wire::terminal(&mut server).unwrap();
                let (id, request) = wire.receive::<Request>().unwrap();
                assert!(matches!(request, Request::Capabilities));
                let mut reply = serde_json::json!({ "Capabilities": { "development_pane": true } });
                if let Some(capability) = capability {
                    reply["Capabilities"]["workspace_policy"] = capability.into();
                }
                wire.send(id, &reply).unwrap();
            });
            assert!(matches!(
                PaneConnection::negotiate(client),
                Err(ServerError::MissingRequiredCapability)
            ));
            peer.join().unwrap();
        }
    }

    #[test]
    fn absent_optional_capabilities_keep_the_required_connection_available() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            let mut wire = crate::protocol::Wire::terminal(&mut server).unwrap();
            let (id, request) = wire.receive::<Request>().unwrap();
            assert!(matches!(request, Request::Capabilities));
            wire.send(
                id,
                &serde_json::json!({
                    "Capabilities": { "development_pane": true, "workspace_policy": true }
                }),
            )
            .unwrap();
        });
        let connection = PaneConnection::negotiate(client).unwrap();
        assert!(!connection.supports_graphics());
        assert!(!connection.supports_interactive_input());
        assert!(!connection.supports_key_input_batches());
        assert!(!connection.supports_reading());
        assert!(!connection.supports_end_all_sessions());
        assert!(!connection.supports_directory_creation());
        assert!(!connection.supports_directory_matching());
        peer.join().unwrap();
    }

    #[test]
    fn key_batches_require_an_explicit_optional_capability() {
        for supported in [None, Some(false), Some(true)] {
            let (client, mut server) = UnixStream::pair().unwrap();
            let peer = std::thread::spawn(move || {
                let mut wire = crate::protocol::Wire::terminal(&mut server).unwrap();
                let (id, request) = wire.receive::<Request>().unwrap();
                assert!(matches!(request, Request::Capabilities));
                let mut reply = serde_json::json!({
                    "Capabilities": { "development_pane": true, "workspace_policy": true,
                        "interactive_input": true }
                });
                if let Some(supported) = supported {
                    reply["Capabilities"]["key_input_batches"] = supported.into();
                }
                wire.send(id, &reply).unwrap();
            });
            let connection = PaneConnection::negotiate(client).unwrap();
            assert_eq!(
                connection.supports_key_input_batches(),
                supported == Some(true)
            );
            peer.join().unwrap();
        }
    }

    #[test]
    fn directory_matching_requires_an_explicit_optional_capability() {
        for supported in [None, Some(false), Some(true)] {
            let (client, mut server) = UnixStream::pair().unwrap();
            let peer = std::thread::spawn(move || {
                let mut wire = crate::protocol::Wire::terminal(&mut server).unwrap();
                let (id, request) = wire.receive::<Request>().unwrap();
                assert!(matches!(request, Request::Capabilities));
                let mut reply = serde_json::json!({
                    "Capabilities": { "development_pane": true, "workspace_policy": true }
                });
                if let Some(supported) = supported {
                    reply["Capabilities"]["directory_matching"] = supported.into();
                }
                wire.send(id, &reply).unwrap();
            });
            let connection = PaneConnection::negotiate(client).unwrap();
            assert_eq!(
                connection.supports_directory_matching(),
                supported == Some(true)
            );
            peer.join().unwrap();
        }
    }

    #[test]
    fn directory_creation_requires_an_explicit_optional_capability() {
        for supported in [false, true] {
            let (client, mut server) = UnixStream::pair().unwrap();
            let peer = std::thread::spawn(move || {
                let mut wire = crate::protocol::Wire::terminal(&mut server).unwrap();
                let (id, request) = wire.receive::<Request>().unwrap();
                assert!(matches!(request, Request::Capabilities));
                wire.send(
                    id,
                    &serde_json::json!({
                        "Capabilities": {
                            "development_pane": true,
                            "workspace_policy": true,
                            "directory_creation": supported
                        }
                    }),
                )
                .unwrap();
            });
            assert_eq!(
                PaneConnection::negotiate(client)
                    .unwrap()
                    .supports_directory_creation(),
                supported
            );
            peer.join().unwrap();
        }
    }

    #[test]
    fn end_all_sessions_support_requires_an_explicit_optional_capability() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            let mut wire = crate::protocol::Wire::terminal(&mut server).unwrap();
            let (id, request) = wire.receive::<Request>().unwrap();
            assert!(matches!(request, Request::Capabilities));
            wire.send(id, &serde_json::json!({
                "Capabilities": { "development_pane": true, "workspace_policy": true, "end_all_sessions": true }
            })).unwrap();
        });
        assert!(
            PaneConnection::negotiate(client)
                .unwrap()
                .supports_end_all_sessions()
        );
        peer.join().unwrap();
    }

    const CALLERS: u8 = 4;

    fn indexed(index: u8) -> Lease {
        Lease {
            server: [index; 32],
            session: [index; 32],
            control: [index; 32],
            connection: [index; 32],
        }
    }

    // Each caller polls with its own lease on one shared transport.
    fn concurrent_polls(
        shared: &SharedPaneConnection,
    ) -> Vec<std::thread::JoinHandle<(Lease, Result<Reply, ServerError>)>> {
        (0..CALLERS)
            .map(|index| {
                let mut connection = shared
                    .connect(|| panic!("callers must share one transport"))
                    .unwrap();
                std::thread::spawn(move || {
                    let lease = indexed(index);
                    (lease, connection.exchange(&Request::Poll { lease }))
                })
            })
            .collect()
    }

    #[test]
    fn pipelined_exchanges_overlap_and_return_replies_by_request_id() {
        let shared = SharedPaneConnection::default();
        let (client, mut server) = UnixStream::pair().unwrap();
        let _owner = shared
            .connect(|| Ok(PaneConnection::new(client, false)))
            .unwrap();
        let peer = std::thread::spawn(move || {
            let mut wire = crate::protocol::Wire::terminal(&mut server).unwrap();
            // Every request arrives before any reply, so no round trip waits for another.
            let mut requests = Vec::new();
            for _ in 0..CALLERS {
                let (id, request) = wire.receive::<Request>().unwrap();
                let Request::Poll { lease } = request else {
                    panic!("unexpected request");
                };
                requests.push((id, lease));
            }
            // Reverse order is what a late first reply produces.
            for (id, lease) in requests.into_iter().rev() {
                wire.send(id, &Reply::Attached { lease }).unwrap();
            }
        });
        for caller in concurrent_polls(&shared) {
            let (lease, reply) = caller.join().unwrap();
            assert!(matches!(reply, Ok(Reply::Attached { lease: replied }) if replied == lease));
        }
        peer.join().unwrap();
    }

    #[test]
    fn losing_the_connection_fences_every_outstanding_exchange() {
        let shared = SharedPaneConnection::default();
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut owner = shared
            .connect(|| Ok(PaneConnection::new(client, false)))
            .unwrap();
        let peer = std::thread::spawn(move || {
            let mut wire = crate::protocol::Wire::terminal(&mut server).unwrap();
            for _ in 0..CALLERS {
                wire.receive::<Request>().unwrap();
            }
            // Every request is outstanding when the connection ends.
        });
        for caller in concurrent_polls(&shared) {
            // Each request may have been applied, so each fails as unconfirmed.
            let (_, reply) = caller.join().unwrap();
            assert!(matches!(reply, Err(ServerError::Protocol)));
        }
        peer.join().unwrap();
        assert!(matches!(
            owner.exchange(&Request::Inventory),
            Err(ServerError::NotRunning)
        ));
        let (client, _server) = UnixStream::pair().unwrap();
        let mut reconnected = false;
        shared
            .connect(|| {
                reconnected = true;
                Ok(PaneConnection::new(client, false))
            })
            .unwrap();
        assert!(reconnected);
    }

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
            let (_, request) = wire.receive::<Request>().unwrap();
            assert!(matches!(request, Request::Inventory));
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
            let (id, request) = wire.receive::<Request>().unwrap();
            assert!(matches!(request, Request::Capabilities));
            wire.send(id, &Reply::Unchanged).unwrap();
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
