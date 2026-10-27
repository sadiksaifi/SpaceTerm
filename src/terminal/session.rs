use super::failure::{FailureReason, TerminalFailure};
use super::native_services::PastePayload;
use crate::terminal::key::InputModifiers;
pub(crate) mod clipboard;
mod input;
mod publication;
pub(crate) use publication::DEFAULT_FRAME_RATE;
mod server_reading;
mod subscriptions;
pub use clipboard::ClipboardCompletion;
pub use clipboard::{ClientEffectGrant, ClientEffectRequest, ClipboardRequest};
use clipboard::{ClipboardAuthority, WorkerClipboard};
use input::PtyInput;
pub(crate) use subscriptions::SubscriptionNotifier;
pub use subscriptions::TerminalSubscription;
use subscriptions::{TerminalEventPublisher, WorkerEventLifetime};
mod schedules;
use schedules::{FindQueryUpdate, ScheduleInput, WorkerSchedules};
mod launch;
use super::pointer_input::*;
use crate::platform::local_filesystem::LocalFilesystemAuthority;
pub use launch::{LocalTerminalLaunchPlan, NativeTerminalSessionFactory};
use std::fmt;
#[cfg(test)]
use std::io::Write;
use std::mem;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver as CommandReceiver, Sender as CommandSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::appearance::{AppearanceGeneration, ResolvedTerminalAppearance};
use crate::platform::native_pty::{
    NativePtyAdapterFactory, NativePtyCloseHandle, NativePtyExit, NativePtyOperationFailure,
    NativePtyOutput, NativePtyOutputSink, NativePtyOwner, NativePtySize, NativePtyStartupFailure,
    NativePtyStartupStage, NativePtyWaitFailure,
};
use crate::platform::shell_launch::{PreparedShellLaunch, ShellLaunchPlanner};
use crate::terminal::accessibility::AccessibilitySelectionRefs;
use crate::terminal::accessibility::AccessibilitySelectionRequest;
use crate::terminal::accessibility::TerminalAccessibilityModel;
use crate::terminal::attention::AttentionEvent;
#[cfg(test)]
use crate::terminal::emulator::MAX_SYNCHRONIZED_OUTPUT_DURATION;
use crate::terminal::emulator::{
    EmulatorAction, PresentationGeneration, ScreenSnapshot, ScrollbackMovement, TerminalEmulator,
};
use crate::terminal::geometry::TerminalGeometry;
#[cfg(test)]
use crate::terminal::identity;
#[cfg(test)]
use crate::terminal::key::OptionAsAltPolicy;
use crate::terminal::key::{KeyInput, PhysicalKey};
use crate::terminal::metadata::{LocalMachine, TerminalMetadataContext, TerminalMetadataSnapshot};
use crate::terminal::osc52::{Osc52Effect, Osc52Filter};
use crate::terminal::paste::{
    PasteConfirmationId, PasteDecision, PasteRejection, PasteRequestOutcome, PasteResolution,
    PreparedPaste,
};
use crate::terminal::permission_request::{PermissionRequest, PermissionRequestFilter};
use crate::terminal::selection::{SelectionCopy, SelectionCopyOptions};
use crate::terminal::{FindDirection, FindQueryGeneration};

const FINAL_CHILD_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const PTY_OUTPUT_QUEUE_CAPACITY: usize = 8;

fn pty_size(geometry: TerminalGeometry) -> NativePtySize {
    let grid = geometry.grid();
    let cell = geometry.backing_cell_size();
    // Clients such as icat infer integer cell dimensions from the PTY extent.
    // Match the engine's pixel grid while retaining fractional geometry in the UI.
    // Zero means unavailable if the extent cannot fit the PTY's pixel field.
    NativePtySize {
        rows: grid.rows,
        columns: grid.cols,
        pixel_width: u16::try_from(u64::from(grid.cols) * u64::from(cell.width)).unwrap_or(0),
        pixel_height: u16::try_from(u64::from(grid.rows) * u64::from(cell.height)).unwrap_or(0),
    }
}

// Screen events may supersede older screens. Failed and Exited are final events,
// so the worker must not publish another screen after either one.
#[derive(Debug)]
pub enum TerminalSessionEvent {
    Screen(Arc<ScreenSnapshot>),
    /// Retained Terminal Metadata changed; read it from the retained snapshot.
    MetadataChanged(MetadataWakeup),
    Attention(AttentionEvent),
    ClientEffect(ClientEffectRequest),
    ClientEffectsReady,
    /// A complete asynchronous pointer selection is ready for the initiating client.
    SelectionCopyReady,
    HiddenInputChanged(bool),
    /// A paste may have been written before the connection lost its acknowledgement.
    PasteUnconfirmed,
    /// A program asked for a Permission Setup.
    PermissionRequested(PermissionRequest),
    Exited(TerminalSessionExit),
    Failed(TerminalSessionFailure),
}

#[derive(Debug)]
pub struct MetadataWakeup {
    queued: Arc<AtomicBool>,
}

impl MetadataWakeup {
    fn new(queued: Arc<AtomicBool>) -> Self {
        Self { queued }
    }
}

impl Drop for MetadataWakeup {
    fn drop(&mut self) {
        self.queued.store(false, Ordering::Release);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalAppearanceUpdate {
    pub generation: AppearanceGeneration,
    pub appearance: Arc<ResolvedTerminalAppearance>,
}

#[cfg(test)]
pub(crate) fn test_terminal_appearance_update() -> TerminalAppearanceUpdate {
    TerminalAppearanceUpdate::builtin().expect("built-in terminal appearance must resolve")
}

impl TerminalAppearanceUpdate {
    /// Resolves the built-in appearance without consulting a desktop or installed fonts.
    pub fn builtin() -> Result<Self, TerminalFailure> {
        use crate::appearance::{
            AppearancePreferences, AvailableFonts, SystemAppearance, ThemeCatalog,
        };
        let resolved = ThemeCatalog::default()
            .resolve(
                AppearanceGeneration::INITIAL,
                &AppearancePreferences::default(),
                SystemAppearance::unavailable(),
                &AvailableFonts::default(),
            )
            .map_err(|_| TerminalFailure::emulator("resolve-builtin-terminal-appearance"))?;
        Ok(Self::new(resolved.generation, resolved.terminal))
    }

    pub fn new(
        generation: AppearanceGeneration,
        appearance: Arc<ResolvedTerminalAppearance>,
    ) -> Self {
        Self {
            generation,
            appearance,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalSessionExit {
    Success,
    ExitCode(u32),
    Signal(String),
    GracefulShutdown,
    ForcedShutdown,
}

impl fmt::Display for TerminalSessionExit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Success => formatter.write_str("Shell exited successfully"),
            Self::ExitCode(code) => write!(formatter, "Shell exited with code {code}"),
            Self::Signal(signal) => write!(formatter, "Shell exited after signal {signal}"),
            Self::GracefulShutdown => formatter.write_str("Shell shut down"),
            Self::ForcedShutdown => formatter.write_str("Shell shutdown forced"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalSessionFailure {
    Startup {
        stage: TerminalSessionStartupStage,
        message: String,
    },
    Runtime(TerminalFailure),
    PtyRead {
        read_error: String,
        exit_status: String,
    },
    ShellWait {
        read_error: Option<String>,
        wait_error: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalSessionStartupStage {
    Directory,
    Pty,
    Reader,
    ReaderThread,
    Emulator,
}

impl fmt::Display for TerminalSessionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup { stage, message } => {
                write!(
                    formatter,
                    "Terminal Session startup failed during {stage}: {message}"
                )
            }
            Self::Runtime(failure) => failure.fmt(formatter),
            Self::PtyRead {
                read_error,
                exit_status,
            } => write!(
                formatter,
                "Shell output failed: {read_error}; shell exited ({exit_status})"
            ),
            Self::ShellWait {
                read_error: Some(read_error),
                wait_error,
            } => write!(
                formatter,
                "Shell output failed: {read_error}; waiting for the shell also failed: {wait_error}"
            ),
            Self::ShellWait {
                read_error: None,
                wait_error,
            } => write!(
                formatter,
                "Shell output ended, but waiting for the shell failed: {wait_error}"
            ),
        }
    }
}

impl std::error::Error for TerminalSessionFailure {}

impl fmt::Display for TerminalSessionStartupStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Directory => formatter.write_str("directory validation"),
            Self::Pty => formatter.write_str("PTY creation"),
            Self::Reader => formatter.write_str("PTY reader acquisition"),
            Self::ReaderThread => formatter.write_str("PTY reader thread creation"),
            Self::Emulator => formatter.write_str("Terminal Emulator creation"),
        }
    }
}

#[derive(Debug, Error)]
pub enum TerminalSessionError {
    #[error("failed to start the terminal worker thread: {0:?}")]
    SpawnWorker(std::io::ErrorKind),
    #[cfg(test)]
    #[error("terminal emulator initialization failed: {0}")]
    EmulatorStartup(String),
}

impl From<std::io::Error> for TerminalSessionError {
    fn from(error: std::io::Error) -> Self {
        Self::SpawnWorker(error.kind())
    }
}

pub struct StartedTerminalSession {
    pub handle: Box<dyn TerminalSessionHandle>,
    pub events: async_channel::Receiver<TerminalSessionEvent>,
    pub accessibility: async_channel::Receiver<Arc<TerminalAccessibilityModel>>,
    pub clipboard: async_channel::Receiver<ClipboardRequest>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionCopyError {
    Formatting,
    WorkerStopped,
}

/// Server-backed delivery for native accessibility requests, which the server Pane's
/// connection fences by its own epoch and focus generation.
#[cfg(feature = "desktop")]
pub(crate) type ServerAccessibilitySelection =
    Arc<dyn Fn(AccessibilitySelectionRequest) + Send + Sync>;
#[cfg(feature = "desktop")]
pub(crate) type ServerAccessibilityDemand = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
enum AccessibilitySelectionTarget {
    Worker(CommandSender<Command>),
    #[cfg(feature = "desktop")]
    Server(ServerAccessibilitySelection),
}

#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only a native accessibility Adapter requests accessibility work"
    )
)]
#[derive(Clone)]
pub struct AccessibilitySelectionSender {
    target: AccessibilitySelectionTarget,
}

impl fmt::Debug for AccessibilitySelectionSender {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessibilitySelectionSender")
    }
}

#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only a native accessibility Adapter requests accessibility work"
    )
)]
impl AccessibilitySelectionSender {
    #[cfg(test)]
    pub fn recording_channel() -> (Self, RecordingAccessibilitySelectionReceiver) {
        let (commands, receiver) = mpsc::channel();
        (
            Self {
                target: AccessibilitySelectionTarget::Worker(commands),
            },
            RecordingAccessibilitySelectionReceiver(receiver),
        )
    }

    #[cfg(feature = "desktop")]
    pub(crate) fn server(deliver: ServerAccessibilitySelection) -> Self {
        Self {
            target: AccessibilitySelectionTarget::Server(deliver),
        }
    }

    pub fn request(&self, request: AccessibilitySelectionRequest) {
        match &self.target {
            AccessibilitySelectionTarget::Worker(commands) => {
                if commands
                    .send(Command::AccessibilitySelection(request))
                    .is_err()
                {
                    eprintln!(
                        "terminal accessibility selection was dropped because the worker has stopped"
                    );
                }
            }
            #[cfg(feature = "desktop")]
            AccessibilitySelectionTarget::Server(deliver) => deliver(request),
        }
    }
}

#[derive(Clone)]
enum AccessibilityDemandTarget {
    Worker {
        commands: CommandSender<Command>,
        schedule_input: ScheduleInput,
    },
    #[cfg(feature = "desktop")]
    Server(ServerAccessibilityDemand),
}

#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only a native accessibility Adapter requests accessibility work"
    )
)]
#[derive(Clone)]
pub struct AccessibilityDemandSender {
    target: AccessibilityDemandTarget,
}

impl fmt::Debug for AccessibilityDemandSender {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessibilityDemandSender")
    }
}

#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only a native accessibility Adapter requests accessibility work"
    )
)]
impl AccessibilityDemandSender {
    #[cfg(feature = "desktop")]
    pub(crate) fn server(demand: ServerAccessibilityDemand) -> Self {
        Self {
            target: AccessibilityDemandTarget::Server(demand),
        }
    }

    pub fn request(&self) {
        self.request_at(Instant::now());
    }

    fn request_at(&self, requested_at: Instant) {
        match &self.target {
            AccessibilityDemandTarget::Worker {
                commands,
                schedule_input,
            } => {
                if schedule_input.enqueue_accessibility_demand(requested_at)
                    && commands.send(Command::AccessibilityDemand).is_err()
                {
                    eprintln!(
                        "terminal accessibility demand was dropped because the worker has stopped"
                    );
                }
            }
            #[cfg(feature = "desktop")]
            AccessibilityDemandTarget::Server(demand) => demand(),
        }
    }
}

#[cfg(test)]
pub struct RecordingAccessibilitySelectionReceiver(CommandReceiver<Command>);

#[cfg(test)]
impl RecordingAccessibilitySelectionReceiver {
    pub fn drain(&self) -> Vec<AccessibilitySelectionRequest> {
        self.0
            .try_iter()
            .map(|command| match command {
                Command::AccessibilitySelection(request) => request,
                _ => unreachable!("Selection authority only sends Selection commands"),
            })
            .collect()
    }
}

/// The newest Terminal Metadata, retained apart from Screen presentation.
///
/// Presentation events may evict any earlier queue entry, and a hidden Pane receives no Screens at
/// all, yet its Pane and Workspace still need current presentation and close-confirmation facts.
#[derive(Clone, Default)]
struct TerminalSessionMetadataState(Arc<Mutex<Option<Arc<TerminalMetadataSnapshot>>>>);

impl TerminalSessionMetadataState {
    fn snapshot(&self) -> Option<Arc<TerminalMetadataSnapshot>> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn publish(&self, metadata: Arc<TerminalMetadataSnapshot>) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = Some(metadata);
    }
}

/// Requests are a bounded set of supported permissions, retained independently of UI wakeups.
#[derive(Clone, Default)]
struct TerminalSessionPermissionRequestState(Arc<Mutex<Option<PermissionRequest>>>);

impl TerminalSessionPermissionRequestState {
    fn snapshot(&self) -> Option<PermissionRequest> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn publish(&self, request: &PermissionRequest) {
        let mut retained = self.0.lock().unwrap_or_else(|error| error.into_inner());
        match retained.as_mut() {
            Some(retained) => retained.merge(request.clone()),
            None => *retained = Some(request.clone()),
        }
    }
}

/// What a Terminal Session's current connection has answered about interactive terminal input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InteractiveInput {
    /// The connection has not answered yet, so neither acceptance nor refusal is known.
    Pending,
    Accepted,
    /// The connection answered that it carries no interactive terminal input.
    Refused,
}

/// Bounds one host commit and the text retained while its transport completes.
pub const MAX_INPUT_METHOD_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum InputMethodRefusal {
    #[error("Input method text is over 65,536 bytes.")]
    TooLarge,
    #[error("Couldn't send input method text. You can still edit it.")]
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputMethodOutcome {
    Written,
    /// Some text may have reached the terminal. Never replay this commit automatically.
    Interrupted,
}

pub trait TerminalSessionHandle {
    /// Drains bounded native requests independently of replaceable presentation wakeups.
    fn take_client_effects(&self) -> Vec<ClientEffectRequest> {
        Vec::new()
    }

    /// Retained host hidden-input state when presentation notifications can coalesce.
    fn hidden_input(&self) -> Option<bool> {
        None
    }

    /// The newest Terminal Metadata the Terminal Session retained, presentable or not.
    fn metadata_snapshot(&self) -> Option<Arc<TerminalMetadataSnapshot>> {
        None
    }

    /// The permissions requested by this Terminal Session, retained across event replacement.
    fn permission_request(&self) -> Option<PermissionRequest> {
        None
    }

    fn key(&self, input: KeyInput);

    /// Accepts the entire commit as one ordered unit, or refuses before consuming its text.
    fn commit_input_method(
        &self,
        text: &str,
    ) -> Result<async_channel::Receiver<InputMethodOutcome>, InputMethodRefusal> {
        if text.len() > MAX_INPUT_METHOD_BYTES {
            return Err(InputMethodRefusal::TooLarge);
        }
        if !self.interactive_input_available() {
            return Err(InputMethodRefusal::Unavailable);
        }
        self.key(KeyInput::input_method_commit(text));
        let (sender, receiver) = async_channel::bounded(1);
        let _ = sender.try_send(InputMethodOutcome::Written);
        Ok(receiver)
    }
    fn clear_screen_and_scrollback(&self);
    fn focus(&self, focused: bool);

    /// Supplies the client facts for terminal input and host-addressed Input Method Text.
    fn interactive_focus(&self, terminal_input: bool, _input_method: bool) {
        self.focus(terminal_input);
    }

    /// What the current connection answered about interactive terminal operations.
    fn interactive_input(&self) -> InteractiveInput {
        InteractiveInput::Accepted
    }

    /// Whether the current connection can carry interactive terminal operations.
    fn interactive_input_available(&self) -> bool {
        self.interactive_input() == InteractiveInput::Accepted
    }
    fn reading_available(&self) -> bool {
        true
    }
    fn selection_copy_ready(&self) -> bool {
        true
    }

    /// Changes whenever a connection or control grant can no longer authorize delayed input.
    fn input_generation(&self) -> u64 {
        0
    }

    /// Consumes a retained uncertain-paste notice after control has been reconciled.
    fn take_paste_unconfirmed(&self) -> bool {
        false
    }

    /// Transfers Terminal Input Focus to this Terminal Session's Paste Confirmation.
    fn focus_paste_confirmation(&self, _id: PasteConfirmationId) {
        self.focus(false);
    }
    fn resize(&self, geometry: TerminalGeometry);
    fn pointer(&self, input: PointerInput);
    fn pointer_and_copy_selection(
        &self,
        input: PointerInput,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError>;
    fn wheel(&self, input: WheelInput);
    fn scroll_to(&self, offset_rows: u64, generation: PresentationGeneration);
    fn scroll_scrollback(&self, movement: ScrollbackMovement);
    fn set_find_query(&self, generation: FindQueryGeneration, query: String);
    fn navigate_find(&self, generation: FindQueryGeneration, direction: FindDirection);
    fn end_find(&self, generation: FindQueryGeneration);
    fn request_paste(
        &self,
        text: PastePayload,
    ) -> async_channel::Receiver<Result<PasteRequestOutcome, String>>;
    fn resolve_paste(
        &self,
        id: PasteConfirmationId,
        decision: PasteDecision,
    ) -> async_channel::Receiver<Result<PasteResolution, String>>;
    fn copy_selection(&self) -> Result<Option<SelectionCopy>, SelectionCopyError>;
    fn take_prepared_pointer_copy(&self) -> Option<SelectionCopy> {
        None
    }
    /// Copy the Selection, or forward the host Copy chord to enhanced keyboard applications.
    fn copy_or_forward(
        &self,
        _: InputModifiers,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        self.copy_selection()
    }
    fn copy_selection_at(
        &self,
        generation: PresentationGeneration,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError>;
    fn accessibility_selection_sender(&self) -> Option<AccessibilitySelectionSender> {
        None
    }
    fn accessibility_demand_sender(&self) -> Option<AccessibilityDemandSender> {
        None
    }
    fn set_presentable(&self, _presentable: bool) {}
    /// Requests current screen rows for bounded hidden CPU preparation. This grants no focus
    /// or drawing authority and does not change the hidden output-check cadence.
    fn set_preparation_demand(&self, _prepare: bool) {}
    fn update_appearance(&self, update: TerminalAppearanceUpdate);
}

/// Starts a server-owned Terminal Session or its client presentation.
pub trait TerminalSessionFactory {
    /// Native runtimes consume the local launch authority. Server presentations use their
    /// server-owned Pane and grant no client-local directory authority from this plan.
    fn start(
        &self,
        geometry: TerminalGeometry,
        launch_plan: LocalTerminalLaunchPlan,
        initial_appearance: TerminalAppearanceUpdate,
    ) -> Result<StartedTerminalSession, TerminalSessionError>;

    fn fallback_title(&self) -> String {
        "Terminal".to_owned()
    }

    /// Explicit client file intake is allowed only by same-machine composition.
    /// Server-supplied paths never acquire this authority.
    fn allows_local_file_insertion(&self) -> bool {
        false
    }

    /// Client filesystem authority supplied only for an authenticated same-machine server route.
    /// Received paths cannot supply or replace this adapter.
    fn local_file_authority(&self) -> Option<&LocalFilesystemAuthority> {
        None
    }

    /// Whether the shell runs in a SpaceTerm Server rather than this application.
    fn is_server_backed(&self) -> bool {
        false
    }
}

pub struct TerminalRuntime {
    metadata_state: TerminalSessionMetadataState,
    permission_request_state: TerminalSessionPermissionRequestState,
    commands: Option<CommandSender<Command>>,
    worker: Option<JoinHandle<()>>,
    native_pty_close: Option<NativePtyCloseHandle>,
    cleanup_confirmation: Option<NativePtyCloseHandle>,
    schedule_input: ScheduleInput,
    event_publisher: TerminalEventPublisher,
    clipboard_authority: Arc<ClipboardAuthority>,
}

#[cfg(test)]
type StartedTerminalSessionParts = (
    TerminalRuntime,
    async_channel::Receiver<TerminalSessionEvent>,
    async_channel::Receiver<Arc<TerminalAccessibilityModel>>,
);

#[cfg(test)]
fn test_launch_planner() -> ShellLaunchPlanner {
    ShellLaunchPlanner::for_test(
        "/fixture/zsh".into(),
        PathBuf::from("/fixture/missing-resources"),
    )
    .with_environment(
        crate::platform::shell_integration::ShellIntegrationMode::Disabled,
        crate::platform::shell_integration::ShellEnvironment::default(),
    )
}

impl TerminalRuntime {
    pub(crate) fn forward_copy_shortcut(&self, modifiers: InputModifiers) {
        let (reply, _receiver) = mpsc::sync_channel(1);
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::CopyOrForward(modifiers, reply));
        }
    }
    pub(crate) fn reading_pointer(&self, input: PointerInput) -> mpsc::Receiver<bool> {
        let (reply, completion) = mpsc::sync_channel(1);
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::ReadingPointer(input, reply));
        }
        completion
    }
    pub(crate) fn reading_wheel(&self, input: WheelInput) -> mpsc::Receiver<bool> {
        let (reply, completion) = mpsc::sync_channel(1);
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::ReadingWheel(input, reply));
        }
        completion
    }
    pub(crate) fn reading_barrier(&self) -> mpsc::Receiver<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::ReadingBarrier(tx));
        }
        rx
    }
    pub(crate) fn reset_reading(&self) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::ResetReading);
        }
    }
    pub(crate) fn prepare_server_selection(
        &self,
        generation: u64,
    ) -> mpsc::Receiver<server_reading::PreparedSelection> {
        let (tx, rx) = mpsc::sync_channel(1);
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::PrepareServerSelection(generation, tx));
        }
        rx
    }
    /// Applies a server client's accessible Selection endpoints on the worker.
    ///
    /// The receiver yields `true` only when the Selection changed; stale endpoints, mismatched
    /// screens, synchronized output and a stopped worker yield `false` or disconnect.
    pub(crate) fn set_server_accessibility_selection(
        &self,
        selection: Option<AccessibilitySelectionRefs>,
    ) -> mpsc::Receiver<bool> {
        let (tx, rx) = mpsc::sync_channel(1);
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::ServerAccessibilitySelection(selection, tx));
        }
        rx
    }
    /// Ends server client accessibility demand without changing visual Screen publication.
    /// A later `AccessibilityDemandSender::request` reactivates semantic publication.
    pub(crate) fn end_server_accessibility_demand(&self) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::EndAccessibilityDemand(Instant::now()));
        }
    }
    /// Sends application mouse input without starting a server-owned Selection.
    /// Adapter snapshot numbers fence starts; accepted gestures retain their starting snapshot.
    pub fn application_pointer(&self, input: PointerInput, snapshot: u64, current_snapshot: u64) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::ApplicationPointer(
                input,
                snapshot,
                current_snapshot,
            ));
        }
    }
    /// Balances a rejected continuation at the last accepted application pointer position.
    pub fn cancel_application_pointer(&self) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::CancelApplicationPointer);
        }
    }
    /// Sends application wheel input without moving server-owned Scrollback.
    pub fn application_wheel(&self, input: WheelInput) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::ApplicationWheel(input));
        }
    }
    /// Routes native requests through the authenticated controlling client.
    pub(crate) fn route_client_effects(&self) {
        self.clipboard_authority.route();
        self.schedule_input.set_accessibility_demand_enabled(true);
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::RouteClientEffects);
        }
    }
    pub(crate) fn client_effect_focus(&self, focused: Option<bool>) {
        self.clipboard_authority.client_focus(focused);
    }
    /// Sets the frame rate the presenting client declared, in hertz. Publication coalesces
    /// continued output to at most one screen per frame at this rate.
    pub(crate) fn set_frame_rate(&self, hz: u8) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::SetFrameRate(hz));
        }
    }

    /// Marks the end of client Input `serial`: call it after the runtime calls that apply that
    /// Input. Screens captured once its bytes reach the PTY carry it as `input_serial`.
    pub(crate) fn note_input_serial(&self, serial: u32) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::InputSerial(serial));
        }
    }

    /// Opens one presentation subscription, replacing the previous subscription.
    /// Dropping its receivers leaves this runtime and its PTY alive.
    pub fn subscribe(&self) -> TerminalSubscription {
        self.subscribe_notifying(None)
    }

    /// Opens a subscription like [`Self::subscribe`] that calls `notifier` whenever it has
    /// something to receive, so its owner waits for that instead of polling.
    pub(crate) fn subscribe_notifying(
        &self,
        notifier: Option<SubscriptionNotifier>,
    ) -> TerminalSubscription {
        let (events, event_sender) = self.event_publisher.subscribe(notifier);
        self.clipboard_authority.observe_subscription(event_sender);
        let (accessibility_sender, accessibility) = async_channel::bounded(1);
        let (clipboard_sender, clipboard) = async_channel::bounded(1);
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::Subscribe {
                accessibility: accessibility_sender,
                clipboard: clipboard_sender,
            });
        }
        TerminalSubscription {
            events,
            accessibility,
            clipboard,
            state: self.event_publisher.clone(),
        }
    }

    fn copy_selection_query(
        &self,
        generation: Option<PresentationGeneration>,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        let Some(commands) = &self.commands else {
            return Err(SelectionCopyError::WorkerStopped);
        };
        let (reply, receiver) = mpsc::sync_channel(1);
        commands
            .send(Command::SelectionCopy(generation, reply))
            .map_err(|_| SelectionCopyError::WorkerStopped)?;
        receiver
            .recv()
            .map_err(|_| SelectionCopyError::WorkerStopped)?
    }

    fn request_shutdown(&mut self) {
        self.clipboard_authority.focus(false);
        // Request termination before transferring sole responsibility to off-thread PTY cleanup.
        if let Some(close_handle) = self.native_pty_close.take()
            && let Err(error) = close_handle.request_close()
        {
            eprintln!("failed to terminate shell while shutting down terminal worker: {error}");
        }
        if let Some(commands) = self.commands.take()
            && commands.send(Command::Shutdown).is_err()
        {
            // The worker already stopped, so there is nothing left to signal.
        }
    }

    pub fn shutdown(&mut self) {
        self.request_shutdown();
        // Dropping a JoinHandle detaches the worker. It still owns the PTY and reader
        // cleanup, but a close operation must never block its GPUI caller on either thread.
        drop(self.worker.take());
    }

    /// Headless owners await worker cleanup and require proof that owned process groups ended.
    pub fn shutdown_and_wait(&mut self, timeout: Duration) -> Result<(), TerminalFailure> {
        if self.cleanup_confirmation.is_none() {
            self.cleanup_confirmation = self.native_pty_close.clone();
        }
        self.request_shutdown();
        let deadline = Instant::now() + timeout;
        if let Some(worker) = &self.worker {
            while !worker.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if !worker.is_finished() {
                return Err(TerminalFailure::pty("cleanup-unresolved"));
            }
        }
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            return Err(TerminalFailure::pty("cleanup-unresolved"));
        }
        if self
            .cleanup_confirmation
            .as_ref()
            .is_some_and(|close| !close.cleanup_confirmed())
        {
            return Err(TerminalFailure::pty("cleanup-unresolved"));
        }
        Ok(())
    }

    #[cfg(test)]
    fn shutdown_and_join(&mut self) {
        self.request_shutdown();
        if let Some(worker) = self.worker.take() {
            join_worker(worker);
        }
    }
}

impl TerminalSessionHandle for TerminalRuntime {
    fn metadata_snapshot(&self) -> Option<Arc<TerminalMetadataSnapshot>> {
        self.metadata_state.snapshot()
    }

    fn permission_request(&self) -> Option<PermissionRequest> {
        self.permission_request_state.snapshot()
    }

    fn key(&self, input: KeyInput) {
        if let Some(commands) = &self.commands
            && commands.send(Command::Key(input)).is_err()
        {
            eprintln!("terminal key input was dropped because the worker has stopped");
        }
    }

    fn clear_screen_and_scrollback(&self) {
        if let Some(commands) = &self.commands
            && commands.send(Command::ClearScreenAndScrollback).is_err()
        {
            eprintln!("terminal clear was dropped because the worker has stopped");
        }
    }

    fn focus(&self, focused: bool) {
        self.clipboard_authority.focus(focused);
        if let Some(commands) = &self.commands
            && commands.send(Command::Focus(focused)).is_err()
        {
            eprintln!("terminal focus input was dropped because the worker has stopped");
        }
    }

    fn focus_paste_confirmation(&self, id: PasteConfirmationId) {
        self.clipboard_authority.focus(false);
        if let Some(commands) = &self.commands
            && commands.send(Command::FocusPasteConfirmation(id)).is_err()
        {
            eprintln!(
                "terminal paste confirmation focus was dropped because the worker has stopped"
            );
        }
    }

    fn resize(&self, geometry: TerminalGeometry) {
        if let Some(commands) = &self.commands {
            let should_notify = self.schedule_input.enqueue_resize(geometry);
            let notification_delivered = should_notify && commands.send(Command::Resize).is_ok();
            if should_notify && !notification_delivered {
                eprintln!("terminal resize was dropped because the worker has stopped");
            }
        }
    }

    fn pointer(&self, input: PointerInput) {
        if let Some(commands) = &self.commands
            && commands.send(Command::Pointer(input)).is_err()
        {
            eprintln!("terminal pointer input was dropped because the worker has stopped");
        }
    }

    fn pointer_and_copy_selection(
        &self,
        input: PointerInput,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        let Some(commands) = &self.commands else {
            return Err(SelectionCopyError::WorkerStopped);
        };
        let (reply, receiver) = mpsc::sync_channel(1);
        commands
            .send(Command::PointerAndCopySelection(input, reply))
            .map_err(|_| SelectionCopyError::WorkerStopped)?;
        receiver
            .recv()
            .map_err(|_| SelectionCopyError::WorkerStopped)?
    }

    fn wheel(&self, input: WheelInput) {
        if let Some(commands) = &self.commands
            && commands.send(Command::Wheel(input)).is_err()
        {
            eprintln!("terminal wheel input was dropped because the worker has stopped");
        }
    }

    fn scroll_to(&self, offset_rows: u64, generation: PresentationGeneration) {
        if let Some(commands) = &self.commands
            && commands
                .send(Command::ScrollTo(offset_rows, generation))
                .is_err()
        {
            eprintln!("terminal scrollbar input was dropped because the worker has stopped");
        }
    }

    fn scroll_scrollback(&self, movement: ScrollbackMovement) {
        if let Some(commands) = &self.commands
            && commands.send(Command::ScrollScrollback(movement)).is_err()
        {
            eprintln!("terminal scroll was dropped because the worker has stopped");
        }
    }

    fn set_find_query(&self, generation: FindQueryGeneration, query: String) {
        if let Some(commands) = &self.commands
            && self.schedule_input.enqueue_find_query(generation, query)
            && commands.send(Command::FindQueryChanged).is_err()
        {
            eprintln!("terminal Find query was dropped because the worker has stopped");
        }
    }

    fn navigate_find(&self, generation: FindQueryGeneration, direction: FindDirection) {
        if let Some(commands) = &self.commands
            && commands
                .send(Command::NavigateFind(generation, direction))
                .is_err()
        {
            eprintln!("terminal Find navigation was dropped because the worker has stopped");
        }
    }

    fn end_find(&self, generation: FindQueryGeneration) {
        if let Some(commands) = &self.commands
            && self.schedule_input.enqueue_find_end(generation)
            && commands.send(Command::FindQueryChanged).is_err()
        {
            eprintln!("terminal Find close was dropped because the worker has stopped");
        }
    }

    fn request_paste(
        &self,
        text: PastePayload,
    ) -> async_channel::Receiver<Result<PasteRequestOutcome, String>> {
        let (reply, receiver) = async_channel::bounded(1);
        let sent = self.commands.as_ref().is_some_and(|commands| {
            commands
                .send(Command::RequestPaste(text, reply.clone()))
                .is_ok()
        });
        if !sent {
            let _ = reply.try_send(Err(
                "terminal paste could not be requested because the worker has stopped".to_owned(),
            ));
        }
        receiver
    }

    fn resolve_paste(
        &self,
        id: PasteConfirmationId,
        decision: PasteDecision,
    ) -> async_channel::Receiver<Result<PasteResolution, String>> {
        let (reply, receiver) = async_channel::bounded(1);
        let sent = self.commands.as_ref().is_some_and(|commands| {
            commands
                .send(Command::ResolvePaste(id, decision, reply.clone()))
                .is_ok()
        });
        if !sent {
            let _ = reply.try_send(Err(
                "terminal paste confirmation was lost because the worker has stopped".to_owned(),
            ));
        }
        receiver
    }

    fn copy_selection(&self) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        self.copy_selection_query(None)
    }

    fn copy_or_forward(
        &self,
        modifiers: InputModifiers,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        let commands = self
            .commands
            .as_ref()
            .ok_or(SelectionCopyError::WorkerStopped)?;
        let (reply, receiver) = mpsc::sync_channel(1);
        commands
            .send(Command::CopyOrForward(modifiers, reply))
            .map_err(|_| SelectionCopyError::WorkerStopped)?;
        receiver
            .recv()
            .map_err(|_| SelectionCopyError::WorkerStopped)?
    }

    fn copy_selection_at(
        &self,
        generation: PresentationGeneration,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        self.copy_selection_query(Some(generation))
    }

    fn accessibility_selection_sender(&self) -> Option<AccessibilitySelectionSender> {
        self.commands
            .as_ref()
            .map(|commands| AccessibilitySelectionSender {
                target: AccessibilitySelectionTarget::Worker(commands.clone()),
            })
    }

    fn accessibility_demand_sender(&self) -> Option<AccessibilityDemandSender> {
        self.commands
            .as_ref()
            .map(|commands| AccessibilityDemandSender {
                target: AccessibilityDemandTarget::Worker {
                    commands: commands.clone(),
                    schedule_input: self.schedule_input.clone(),
                },
            })
    }

    fn set_presentable(&self, presentable: bool) {
        if !self.clipboard_authority.routed() {
            self.schedule_input
                .set_accessibility_demand_enabled(presentable);
        }
        if let Some(commands) = &self.commands
            && commands.send(Command::SetPresentable(presentable)).is_err()
        {
            eprintln!("terminal presentation state was dropped because the worker has stopped");
        }
    }

    fn update_appearance(&self, update: TerminalAppearanceUpdate) {
        let Some(commands) = &self.commands else {
            return;
        };
        if self.schedule_input.enqueue_terminal_appearance(update)
            && commands.send(Command::AppearanceChanged).is_err()
        {
            eprintln!("terminal appearance update was dropped because the worker has stopped");
        }
    }
}

impl Drop for TerminalRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct ReaderTransport {
    output: Arc<TerminalSessionNativePtyOutputSink>,
    event_rx: mpsc::Receiver<NativePtyOutput>,
}

impl ReaderTransport {
    fn new(commands: CommandSender<Command>, clipboard_authority: Arc<ClipboardAuthority>) -> Self {
        let (events, event_rx) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
        Self {
            output: Arc::new(TerminalSessionNativePtyOutputSink {
                commands,
                events,
                clipboard_authority,
            }),
            event_rx,
        }
    }

    fn output_sink(&self) -> Arc<dyn NativePtyOutputSink> {
        self.output.clone()
    }
}

struct TerminalSessionNativePtyOutputSink {
    clipboard_authority: Arc<ClipboardAuthority>,
    commands: CommandSender<Command>,
    events: mpsc::SyncSender<NativePtyOutput>,
}

impl NativePtyOutputSink for TerminalSessionNativePtyOutputSink {
    fn publish(&self, output: NativePtyOutput) -> bool {
        // ReaderReady orders bounded PTY output against the reliable control command lane.
        let epoch = self.clipboard_authority.output_epoch();
        self.events.send(output).is_ok() && self.commands.send(Command::ReaderReady(epoch)).is_ok()
    }
}

struct ReaderEventBatch {
    chunks: Vec<(Vec<u8>, Option<u64>)>,
    reader_stopped: Option<Option<crate::platform::native_pty::NativePtyReadFailure>>,
}

enum Command {
    ReadingPointer(PointerInput, mpsc::SyncSender<bool>),
    ReadingWheel(WheelInput, mpsc::SyncSender<bool>),
    ResetReading,
    ReadingBarrier(mpsc::SyncSender<()>),
    PrepareServerSelection(u64, mpsc::SyncSender<server_reading::PreparedSelection>),
    ServerAccessibilitySelection(Option<AccessibilitySelectionRefs>, mpsc::SyncSender<bool>),
    EndAccessibilityDemand(Instant),
    RouteClientEffects,
    Subscribe {
        accessibility: async_channel::Sender<Arc<TerminalAccessibilityModel>>,
        clipboard: async_channel::Sender<ClipboardRequest>,
    },
    Key(KeyInput),
    ClearScreenAndScrollback,
    Focus(bool),
    FocusPasteConfirmation(PasteConfirmationId),
    Resize,
    Pointer(PointerInput),
    ApplicationPointer(PointerInput, u64, u64),
    CancelApplicationPointer,
    ApplicationWheel(WheelInput),
    PointerAndCopySelection(
        PointerInput,
        mpsc::SyncSender<Result<Option<SelectionCopy>, SelectionCopyError>>,
    ),
    Wheel(WheelInput),
    ScrollTo(u64, PresentationGeneration),
    ScrollScrollback(ScrollbackMovement),
    FindQueryChanged,
    NavigateFind(FindQueryGeneration, FindDirection),
    RequestPaste(
        PastePayload,
        async_channel::Sender<Result<PasteRequestOutcome, String>>,
    ),
    ResolvePaste(
        PasteConfirmationId,
        PasteDecision,
        async_channel::Sender<Result<PasteResolution, String>>,
    ),
    PasteConfirmationExpired,
    SelectionCopy(
        Option<PresentationGeneration>,
        mpsc::SyncSender<Result<Option<SelectionCopy>, SelectionCopyError>>,
    ),
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only a native accessibility Adapter requests accessibility work"
        )
    )]
    AccessibilitySelection(AccessibilitySelectionRequest),
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only a native accessibility Adapter requests accessibility work"
        )
    )]
    AccessibilityDemand,
    AccessibilityContinue,
    PublishAccessibility,
    SelectionAutoscrollTick,
    PublishPendingScreen,
    MetadataStatusTick,
    GraphicsAnimationTick,
    GraphicsBudgetAvailable,
    SetPresentable(bool),
    /// The presenting client's frame rate, in hertz.
    SetFrameRate(u8),
    /// Every byte of client Input `serial` has been enqueued for the PTY.
    InputSerial(u32),
    AppearanceChanged,
    ReaderReady(Option<u64>),
    CompressScrollback,
    Shutdown,
    PollHiddenInput,
    CopyOrForward(
        InputModifiers,
        mpsc::SyncSender<Result<Option<SelectionCopy>, SelectionCopyError>>,
    ),
    CompleteClipboard(u64, ClipboardCompletion),
    ClipboardExpired,
    ResumeOutput(Option<u64>),
    FlushInput,
}

impl Command {
    fn requires_presentation_barrier(&self) -> bool {
        matches!(
            self,
            Self::Pointer(..)
                | Self::ReadingPointer(..)
                | Self::ReadingWheel(..)
                | Self::ApplicationPointer(..)
                | Self::PointerAndCopySelection(..)
                | Self::Wheel(..)
                | Self::ScrollTo(..)
                | Self::SelectionCopy(Some(_), _)
                | Self::PrepareServerSelection(..)
                | Self::ServerAccessibilitySelection(..)
                | Self::ReadingBarrier(_)
                | Self::AccessibilitySelection(..)
                | Self::AccessibilityContinue
                | Self::PublishAccessibility
        )
    }
}

impl fmt::Debug for Command {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::ReadingPointer(..) => "ReadingPointer",
            Self::ReadingWheel(..) => "ReadingWheel",
            Self::ResetReading => "ResetReading",
            Self::ReadingBarrier(_) => "ReadingBarrier",
            Self::PrepareServerSelection(..) => "PrepareServerSelection",
            Self::ServerAccessibilitySelection(..) => "ServerAccessibilitySelection",
            Self::EndAccessibilityDemand(..) => "EndAccessibilityDemand",
            Self::RouteClientEffects => "RouteClientEffects",
            Self::Subscribe { .. } => "Subscribe",
            Self::Key(..) => "Key",
            Self::ClearScreenAndScrollback => "ClearScreenAndScrollback",
            Self::Focus(..) => "Focus",
            Self::FocusPasteConfirmation(..) => "FocusPasteConfirmation",
            Self::Resize => "Resize",
            Self::Pointer(..) => "Pointer",
            Self::ApplicationPointer(..) => "ApplicationPointer",
            Self::CancelApplicationPointer => "CancelApplicationPointer",
            Self::ApplicationWheel(..) => "ApplicationWheel",
            Self::PointerAndCopySelection(..) => "PointerAndCopySelection",
            Self::Wheel(..) => "Wheel",
            Self::ScrollTo(..) => "ScrollTo",
            Self::ScrollScrollback(..) => "ScrollScrollback",
            Self::FindQueryChanged => "FindQueryChanged",
            Self::NavigateFind(..) => "NavigateFind",
            Self::RequestPaste(..) => "RequestPaste",
            Self::ResolvePaste(..) => "ResolvePaste",
            Self::PasteConfirmationExpired => "PasteConfirmationExpired",
            Self::SelectionCopy(..) => "SelectionCopy",
            Self::AccessibilitySelection(..) => "AccessibilitySelection",
            Self::AccessibilityDemand => "AccessibilityDemand",
            Self::AccessibilityContinue => "AccessibilityContinue",
            Self::PublishAccessibility => "PublishAccessibility",
            Self::SelectionAutoscrollTick => "SelectionAutoscrollTick",
            Self::PublishPendingScreen => "PublishPendingScreen",
            Self::MetadataStatusTick => "MetadataStatusTick",
            Self::GraphicsAnimationTick => "GraphicsAnimationTick",
            Self::GraphicsBudgetAvailable => "GraphicsBudgetAvailable",
            Self::SetPresentable(..) => "SetPresentable",
            Self::SetFrameRate(..) => "SetFrameRate",
            Self::InputSerial(..) => "InputSerial",
            Self::AppearanceChanged => "AppearanceChanged",
            Self::ReaderReady(_) => "ReaderReady",
            Self::CompressScrollback => "CompressScrollback",
            Self::Shutdown => "Shutdown",
            Self::PollHiddenInput => "PollHiddenInput",
            Self::CopyOrForward(..) => "CopyOrForward",
            Self::CompleteClipboard(..) => "CompleteClipboard",
            Self::ClipboardExpired => "ClipboardExpired",
            Self::ResumeOutput(_) => "ResumeOutput",
            Self::FlushInput => "FlushInput",
        };
        formatter.write_str(name)
    }
}

struct TerminalWorker {
    metadata_state: TerminalSessionMetadataState,
    permission_request_state: TerminalSessionPermissionRequestState,
    native_pty: NativePtyOwner,
    input: PtyInput,
    emulator: TerminalEmulator,
    server_reading: Option<server_reading::ServerReading>,
    commands: CommandReceiver<Command>,
    reader_events: mpsc::Receiver<NativePtyOutput>,
    events: TerminalEventPublisher,
    accessibility: async_channel::Sender<Arc<TerminalAccessibilityModel>>,
    pending_command: Option<Command>,
    terminal_input_focused: bool,
    focus_reporting_enabled: bool,
    held_keys: HeldKeys,
    schedules: WorkerSchedules,
    osc52_filter: Osc52Filter<Option<u64>>,
    permission_requests: PermissionRequestFilter,
    clipboard: WorkerClipboard,
}

enum TerminalFocusTarget {
    Terminal,
    Unfocused,
    PasteConfirmation(PasteConfirmationId),
}

struct TerminalWorkerContext {
    local_filesystem: LocalFilesystemAuthority,
    initial_geometry: TerminalGeometry,
    metadata_context: TerminalMetadataContext,
    fallback_title: String,
    terminal_name: &'static str,
    restricted: bool,
    initial_appearance: TerminalAppearanceUpdate,
}

struct TerminalWorkerPublishers {
    clipboard: WorkerClipboard,
    metadata_state: TerminalSessionMetadataState,
    permission_request_state: TerminalSessionPermissionRequestState,
    events: TerminalEventPublisher,
    accessibility: async_channel::Sender<Arc<TerminalAccessibilityModel>>,
}

#[derive(Default)]
struct HeldKeys {
    held: Vec<KeyInput>,
    suppressed_releases: Vec<PhysicalKey>,
}

impl HeldKeys {
    fn route(&mut self, input: &KeyInput) -> bool {
        if input.is_text_input() || input.is_input_method_commit() {
            return true;
        }
        match input.action {
            crate::terminal::key::KeyAction::Press => {
                self.suppressed_releases
                    .retain(|key| *key != input.physical_key);
                if let Some(held) = self
                    .held
                    .iter_mut()
                    .find(|held| held.physical_key == input.physical_key)
                {
                    *held = input.clone();
                } else {
                    self.held.push(input.clone());
                }
                true
            }
            crate::terminal::key::KeyAction::Repeat => {
                if self.suppressed_releases.contains(&input.physical_key) {
                    return false;
                }
                if let Some(held) = self
                    .held
                    .iter_mut()
                    .find(|held| held.physical_key == input.physical_key)
                {
                    *held = input.clone();
                } else {
                    self.held.push(input.clone());
                }
                true
            }
            crate::terminal::key::KeyAction::Release => {
                if self
                    .held
                    .iter()
                    .any(|held| held.physical_key == input.physical_key)
                {
                    self.held
                        .retain(|held| held.physical_key != input.physical_key);
                    return true;
                }
                let Some(index) = self
                    .suppressed_releases
                    .iter()
                    .position(|key| *key == input.physical_key)
                else {
                    return true;
                };
                self.suppressed_releases.swap_remove(index);
                false
            }
        }
    }

    fn suppress_shortcut_release(&mut self, key: PhysicalKey) {
        if !self.suppressed_releases.contains(&key) {
            self.suppressed_releases.push(key);
        }
    }

    fn take_releases(&mut self) -> Vec<KeyInput> {
        std::mem::take(&mut self.held)
            .into_iter()
            .map(|mut input| {
                if !self.suppressed_releases.contains(&input.physical_key) {
                    self.suppressed_releases.push(input.physical_key);
                }
                input.action = crate::terminal::key::KeyAction::Release;
                input
            })
            .collect()
    }
}

impl TerminalWorker {
    fn run(
        native_pty: NativePtyOwner,
        context: TerminalWorkerContext,
        commands: CommandReceiver<Command>,
        reader_transport: ReaderTransport,
        schedule_input: ScheduleInput,
        publishers: TerminalWorkerPublishers,
    ) {
        let TerminalWorkerContext {
            initial_geometry,
            metadata_context,
            fallback_title,
            terminal_name,
            restricted,
            local_filesystem,
            initial_appearance,
        } = context;
        let TerminalWorkerPublishers {
            clipboard,
            metadata_state,
            permission_request_state,
            events,
            accessibility,
        } = publishers;
        let ReaderTransport {
            output,
            event_rx: reader_event_rx,
        } = reader_transport;

        let mut emulator = match TerminalEmulator::new_with_local_filesystem(
            initial_geometry,
            metadata_context,
            &fallback_title,
            terminal_name,
            Instant::now(),
            local_filesystem,
            initial_appearance,
        ) {
            Ok(emulator) => emulator,
            Err(error) => {
                send_session_event(
                    &events,
                    TerminalSessionEvent::Failed(TerminalSessionFailure::Startup {
                        stage: TerminalSessionStartupStage::Emulator,
                        message: error.to_string(),
                    }),
                );
                drop(reader_event_rx);
                drop(native_pty);
                return;
            }
        };

        if restricted && emulator.restrict_client_effects().is_err() {
            send_session_event(
                &events,
                TerminalSessionEvent::Failed(TerminalSessionFailure::Runtime(
                    TerminalFailure::emulator("restrict-server-terminal"),
                )),
            );
            return;
        }
        let graphics_commands = output.commands.clone();
        emulator.set_graphics_budget_wakeup(move || {
            let _ = graphics_commands.send(Command::GraphicsBudgetAvailable);
        });

        let mut worker = Self {
            metadata_state,
            permission_request_state,
            native_pty,
            input: PtyInput::default(),
            emulator,
            server_reading: None,
            commands,
            reader_events: reader_event_rx,
            events,
            accessibility,
            pending_command: None,
            terminal_input_focused: true,
            focus_reporting_enabled: false,
            held_keys: HeldKeys::default(),
            schedules: WorkerSchedules::new(Instant::now(), schedule_input),
            osc52_filter: Osc52Filter::default(),
            permission_requests: PermissionRequestFilter::default(),
            clipboard,
        };

        worker.run_commands();
        worker.finish();
    }

    fn run_commands(&mut self) {
        if !self.publish_screen() {
            return;
        }
        self.prime_compression_activity();

        loop {
            let Some(command) = self.receive_next_command() else {
                break;
            };
            let postpone_compression = matches!(
                &command,
                Command::Key(..)
                    | Command::ReaderReady(_)
                    | Command::RequestPaste(..)
                    | Command::ResolvePaste(..)
                    | Command::Focus(..)
                    | Command::FocusPasteConfirmation(..)
            );
            let compression_step = matches!(&command, Command::CompressScrollback);
            if !self.process_command(command) {
                break;
            }
            if !compression_step {
                self.observe_compression_activity(Instant::now(), postpone_compression);
            }
        }
    }

    fn prime_compression_activity(&mut self) {
        match self.emulator.compression_activity() {
            Ok(activity) => self.schedules.prime_compression(activity),
            Err(_) => self.schedules.disable_compression(),
        }
    }

    fn observe_compression_activity(&mut self, now: Instant, postpone: bool) {
        match self.emulator.compression_activity() {
            Ok(activity) => self.schedules.observe_compression(now, activity, postpone),
            Err(_) => self.schedules.disable_compression(),
        }
    }

    fn receive_next_command(&mut self) -> Option<Command> {
        // Presentation deadlines must advance even when PTY readers keep the queue nonempty.
        // The emulator separately enforces the prompt cap and synchronized-output inactivity.
        if !self.release_synchronized_output_if_due(Instant::now()) {
            return None;
        }
        if self
            .input
            .deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Some(Command::FlushInput);
        }
        if self.clipboard.expired() {
            return Some(Command::ClipboardExpired);
        }
        // A command removed from the shared queue while batching PTY output owns this
        // single slot. Run it before schedules can install barrier work into the slot.
        if let Some(command) = self.pending_command.take() {
            return Some(self.note_normal_command(command));
        }
        if !self.clipboard.pending()
            && let Some(epoch) = self.clipboard.deferred_readers.pop_front()
        {
            return Some(Command::ResumeOutput(epoch));
        }
        if self.schedules.must_continue_accessibility() {
            return self.take_accessibility_continuation();
        }
        let now = Instant::now();
        if self
            .emulator
            .metadata_status_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            return Some(self.note_normal_command(Command::MetadataStatusTick));
        }
        if let Some(command) = self.schedules.take_due(now) {
            return Some(self.note_normal_command(command));
        }
        if self.schedules.accessibility_pending() {
            return match self.commands.try_recv() {
                Ok(command) => Some(self.note_normal_command(command)),
                Err(mpsc::TryRecvError::Empty) => self.take_accessibility_continuation(),
                Err(mpsc::TryRecvError::Disconnected) => None,
            };
        }
        if self.schedules.compression_due(now) {
            match self.commands.try_recv() {
                Ok(command) => return Some(self.note_normal_command(command)),
                Err(mpsc::TryRecvError::Disconnected) => return None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if self.schedules.take_compression_due(now) {
                return Some(Command::CompressScrollback);
            }
        }

        loop {
            let synchronized_output_deadline = self.emulator.synchronized_output_deadline();
            let deadline = [
                self.schedules.deadline(synchronized_output_deadline),
                self.emulator.metadata_status_deadline(),
                self.clipboard.deadline(),
                self.input.deadline(),
            ]
            .into_iter()
            .flatten()
            .min();
            let Some(deadline) = deadline else {
                let command = self.commands.recv().ok()?;
                return Some(self.note_normal_command(command));
            };
            let timeout = deadline.saturating_duration_since(Instant::now());
            match self.commands.recv_timeout(timeout) {
                Ok(command) => return Some(self.note_normal_command(command)),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if self
                        .input
                        .deadline()
                        .is_some_and(|deadline| Instant::now() >= deadline)
                    {
                        return Some(Command::FlushInput);
                    }
                    if self.clipboard.expired() {
                        return Some(Command::ClipboardExpired);
                    }
                    let now = Instant::now();
                    if self
                        .emulator
                        .metadata_status_deadline()
                        .is_some_and(|deadline| now >= deadline)
                    {
                        return Some(self.note_normal_command(Command::MetadataStatusTick));
                    }
                    if let Some(command) = self.schedules.take_due(now) {
                        return Some(self.note_normal_command(command));
                    }
                    if synchronized_output_deadline.is_some()
                        && !self.release_synchronized_output_if_due(now)
                    {
                        return None;
                    }
                    match self.commands.try_recv() {
                        Ok(command) => return Some(self.note_normal_command(command)),
                        Err(mpsc::TryRecvError::Disconnected) => return None,
                        Err(mpsc::TryRecvError::Empty) => {}
                    }
                    if self.schedules.take_compression_due(now) {
                        return Some(Command::CompressScrollback);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    fn note_normal_command(&mut self, command: Command) -> Command {
        if !matches!(&command, Command::AccessibilityContinue) {
            self.schedules.note_normal_command();
        }
        self.order_presentation_barrier(command)
    }

    fn order_presentation_barrier(&mut self, command: Command) -> Command {
        if self.server_reading.is_some()
            && !self.schedules.is_presentable()
            && matches!(
                &command,
                Command::AccessibilityContinue | Command::PublishAccessibility
            )
        {
            return command;
        }
        if command.requires_presentation_barrier() && self.schedules.take_presentation_barrier() {
            self.pending_command = Some(if matches!(&command, Command::AccessibilityContinue) {
                Command::PublishAccessibility
            } else {
                command
            });
            Command::PublishPendingScreen
        } else {
            command
        }
    }

    fn take_accessibility_continuation(&mut self) -> Option<Command> {
        self.schedules
            .take_accessibility_continuation()
            .then(|| self.order_presentation_barrier(Command::AccessibilityContinue))
    }

    fn process_command(&mut self, command: Command) -> bool {
        match command {
            Command::ReadingPointer(input, reply) => match self.emulator.reading_pointer(input) {
                Ok(action) => {
                    let applied = action.is_some();
                    let success = action.is_none_or(|action| self.apply_emulator_action(action))
                        && self.refresh_selection_autoscroll();
                    let _ = reply.send(applied && success);
                    // The server waits on this reply before it drains the session again.
                    self.events.notify();
                    success
                }
                Err(error) => {
                    self.send_runtime_failure(error);
                    false
                }
            },
            Command::ReadingWheel(input, reply) => match self.emulator.reading_wheel(input) {
                Ok(action) => {
                    let applied = action.is_some();
                    let success = action.is_none_or(|action| self.apply_emulator_action(action));
                    let _ = reply.send(applied && success);
                    self.events.notify();
                    success
                }
                Err(error) => {
                    self.send_runtime_failure(error);
                    false
                }
            },
            Command::ReadingBarrier(reply) => {
                let _ = reply.send(());
                // The server holds back draining the session until this barrier passes.
                self.events.notify();
                true
            }
            Command::ResetReading => match self.emulator.reset_reading() {
                Ok(action) => self.apply_emulator_action(action),
                Err(error) => {
                    self.send_runtime_failure(error);
                    false
                }
            },
            Command::PrepareServerSelection(generation, reply) => {
                let selection = if self.emulator.synchronized_output_deadline().is_some() {
                    Ok(None)
                } else {
                    self.server_reading
                        .as_ref()
                        .map_or(Ok(None), |reading| reading.prepared(generation))
                };
                let _ = reply.send(selection);
                true
            }
            Command::ServerAccessibilitySelection(selection, reply) => {
                match self.emulator.set_accessibility_selection_refs(selection) {
                    Ok(changed) => {
                        let _ = reply.send(changed);
                        !changed
                            || (self.apply_emulator_action(EmulatorAction::screen_changed())
                                && self.refresh_selection_autoscroll())
                    }
                    Err(message) => {
                        let _ = reply.send(false);
                        self.send_runtime_failure(message);
                        false
                    }
                }
            }
            Command::EndAccessibilityDemand(ended_at) => {
                self.schedules.end_accessibility_demand(ended_at);
                true
            }
            Command::RouteClientEffects => {
                self.emulator.enable_client_clipboard();
                self.emulator.enable_server_reading();
                self.server_reading = Some(server_reading::ServerReading::default());
                self.schedules
                    .set_server_presentable(self.schedules.is_presentable(), Instant::now());
                true
            }
            Command::Subscribe {
                accessibility,
                clipboard,
            } => {
                self.accessibility.close();
                self.accessibility = accessibility;
                self.clipboard.replace_requests(clipboard);
                if !self.process_focus(false) || !self.cancel_pointer_drag() {
                    return false;
                }
                let now = Instant::now();
                self.schedules.subscribe(now);
                self.emulator.reset_accessibility_presentation();
                if !self.publish_screen() {
                    return false;
                }
                self.events.repeat_screen();
                self.schedules.note_screen_published();
                self.send_terminal_event(TerminalSessionEvent::HiddenInputChanged(
                    self.events.hidden_input(),
                ))
            }
            Command::Key(input) => self.process_key(input),
            Command::ClearScreenAndScrollback => {
                let action = self.emulator.clear_screen_and_scrollback();
                self.apply_emulator_action(action)
            }
            Command::Focus(focused) => self.process_focus(focused),
            Command::FocusPasteConfirmation(id) => {
                self.process_focus_target(TerminalFocusTarget::PasteConfirmation(id))
            }
            Command::ReaderReady(epoch) if self.clipboard.pending() => {
                self.clipboard.deferred_readers.push_back(epoch);
                true
            }
            Command::ReaderReady(epoch) => self.process_reader_events(epoch),
            Command::ResumeOutput(epoch) => self.process_reader_events_limit(1, epoch),
            Command::CompleteClipboard(id, completion) => {
                self.complete_clipboard(Some(id), completion)
            }
            Command::ClipboardExpired => self.complete_clipboard(None, ClipboardCompletion::Denied),
            Command::FlushInput => self.flush_input(),
            Command::CompressScrollback => {
                match self.emulator.compress_scrollback() {
                    Ok(result) => self.schedules.complete_compression(Instant::now(), result),
                    Err(_) => self.schedules.disable_compression(),
                }
                true
            }
            Command::Resize => {
                let Some(geometry) = self.schedules.take_resize() else {
                    return true;
                };
                if geometry != self.emulator.geometry() {
                    match self.emulator.cancel_application_pointer() {
                        Ok(action) => {
                            if !self.apply_emulator_action(action) {
                                return false;
                            }
                        }
                        Err(message) => {
                            self.send_runtime_failure(message);
                            return false;
                        }
                    }
                }
                let size = pty_size(geometry);
                let winsize_changed = size != pty_size(self.emulator.geometry());
                let result = (|| {
                    self.native_pty.resize(size).map_err(|error| {
                        TerminalFailure::pty("resize-pty")
                            .with_reason(FailureReason::Io(error.kind()))
                    })?;
                    self.emulator
                        .resize_for_pty(geometry, winsize_changed)
                        .map_err(|error| {
                            TerminalFailure::emulator_error("resize-terminal-state", error)
                        })
                })();

                match result {
                    Ok(()) => {
                        self.apply_emulator_action(EmulatorAction::screen_changed())
                            && self.refresh_selection_autoscroll()
                    }
                    Err(message) => {
                        self.send_runtime_failure(message);
                        false
                    }
                }
            }
            Command::ApplicationPointer(input, snapshot, current_snapshot) => match self
                .emulator
                .application_pointer(input, snapshot, current_snapshot)
            {
                Ok(action) => self.apply_emulator_action(action),
                Err(message) => {
                    self.send_runtime_failure(message);
                    false
                }
            },
            Command::CancelApplicationPointer => match self.emulator.cancel_application_pointer() {
                Ok(action) => self.apply_emulator_action(action),
                Err(message) => {
                    self.send_runtime_failure(message);
                    false
                }
            },
            Command::ApplicationWheel(input) => match self.emulator.application_wheel(input) {
                Ok(action) => self.apply_emulator_action(action),
                Err(message) => {
                    self.send_runtime_failure(message);
                    false
                }
            },
            Command::Pointer(input) => match self.emulator.pointer(input) {
                Ok(action) => {
                    self.apply_emulator_action(action) && self.refresh_selection_autoscroll()
                }
                Err(message) => {
                    self.send_runtime_failure(message);
                    false
                }
            },
            Command::PointerAndCopySelection(input, reply) => match self.emulator.pointer(input) {
                Ok(action) => {
                    let copy = if action.selection_completed {
                        self.emulator
                            .selection_copy(SelectionCopyOptions::default())
                            .map_err(|_| SelectionCopyError::Formatting)
                    } else {
                        Ok(None)
                    };
                    let _ = reply.send(copy);
                    self.apply_emulator_action(action) && self.refresh_selection_autoscroll()
                }
                Err(message) => {
                    let _ = reply.send(Ok(None));
                    self.send_runtime_failure(message);
                    false
                }
            },
            Command::Wheel(input) => match self.emulator.wheel(input) {
                Ok(action) => self.apply_emulator_action(action),
                Err(message) => {
                    self.send_runtime_failure(message);
                    false
                }
            },
            Command::ScrollTo(offset_rows, generation) => {
                if self.emulator.synchronized_output_deadline().is_some() {
                    return true;
                }
                let action = self.emulator.scroll_to_at(offset_rows, generation);
                self.apply_emulator_action(action)
            }
            Command::ScrollScrollback(movement) => {
                if self.emulator.synchronized_output_deadline().is_some() {
                    return true;
                }
                let action = self.emulator.scroll_scrollback(movement);
                self.apply_emulator_action(action)
            }
            Command::FindQueryChanged => {
                let Some(update) = self.schedules.take_find_query() else {
                    return true;
                };
                let action = match update {
                    FindQueryUpdate::Set(generation, query) => {
                        self.emulator.set_find_query(generation, query)
                    }
                    FindQueryUpdate::End(generation) => self.emulator.end_find(generation),
                };
                self.apply_emulator_action(action)
            }
            Command::NavigateFind(generation, direction) => {
                match self.emulator.navigate_find(generation, direction) {
                    Ok(action) => self.apply_emulator_action(action),
                    Err(message) => {
                        self.send_runtime_failure(message);
                        false
                    }
                }
            }
            Command::RequestPaste(text, reply) => self.process_paste_request(text, reply),
            Command::ResolvePaste(id, decision, reply) => {
                self.process_paste_resolution(id, decision, reply)
            }
            Command::PasteConfirmationExpired => true,
            Command::CopyOrForward(modifiers, reply) => {
                let selection = self
                    .emulator
                    .selection_copy(SelectionCopyOptions::default())
                    .map_err(|_| SelectionCopyError::Formatting);
                let forward = matches!(selection, Ok(None))
                    && self.terminal_input_focused
                    && self.emulator.enhanced_keyboard_active();
                let keep_running = !forward
                    || self.process_shortcut(KeyInput {
                        action: crate::terminal::key::KeyAction::Press,
                        physical_key: crate::terminal::key::PhysicalKey::C,
                        native_key_code: None,
                        logical_key: "c".into(),
                        text: None,
                        unshifted_codepoint: Some('c'),
                        modifiers,
                        consumed_modifiers: InputModifiers::default(),
                        option_as_alt: crate::terminal::key::OptionAsAltPolicy::None,
                    });
                let _ = reply.send(selection);
                keep_running
            }
            Command::SelectionCopy(generation, reply) => {
                let selection = if generation.is_some_and(|generation| {
                    generation != self.emulator.presentation_generation()
                        || self.emulator.synchronized_output_deadline().is_some()
                }) {
                    Ok(None)
                } else {
                    self.emulator
                        .selection_copy(SelectionCopyOptions::default())
                        .map_err(|_| SelectionCopyError::Formatting)
                };
                let _ = reply.send(selection);
                true
            }
            Command::AccessibilitySelection(request) => {
                match self.emulator.set_accessibility_selection(request) {
                    Ok(action) => {
                        self.apply_emulator_action(action) && self.refresh_selection_autoscroll()
                    }
                    Err(message) => {
                        self.send_runtime_failure(message);
                        false
                    }
                }
            }
            Command::AccessibilityDemand => {
                self.schedules.accessibility_demand_received(Instant::now());
                true
            }
            Command::AccessibilityContinue => self.publish_accessibility(false),
            Command::PublishAccessibility => self.publish_accessibility(true),
            Command::SelectionAutoscrollTick => {
                if self.emulator.synchronized_output_deadline().is_some() {
                    return self.refresh_selection_autoscroll();
                }
                match self.emulator.selection_autoscroll_tick() {
                    Ok(action) => {
                        self.apply_emulator_action(action) && self.refresh_selection_autoscroll()
                    }
                    Err(message) => {
                        self.send_runtime_failure(message);
                        false
                    }
                }
            }
            Command::PublishPendingScreen => self.publish_screen(),
            Command::MetadataStatusTick => self.advance_metadata_status(),
            Command::GraphicsAnimationTick => {
                self.emulator.synchronized_output_deadline().is_some()
                    || self.request_presentation()
            }
            Command::GraphicsBudgetAvailable => {
                !self.emulator.take_graphics_budget_wakeup()
                    || self.emulator.synchronized_output_deadline().is_some()
                    || self.request_presentation()
            }
            Command::SetPresentable(presentable) => {
                let now = Instant::now();
                if self.server_reading.is_some() {
                    self.schedules.set_server_presentable(presentable, now);
                } else {
                    self.schedules.set_presentable(presentable, now);
                }
                if !presentable && !self.cancel_pointer_drag() {
                    return false;
                }
                if !presentable && !self.publish_metadata_changed() {
                    false
                } else if self.schedules.presentation_due(now) {
                    self.publish_screen()
                } else {
                    true
                }
            }
            Command::SetFrameRate(hz) => {
                self.schedules.set_frame_rate(hz);
                true
            }
            Command::InputSerial(serial) => {
                self.input.note_serial(serial);
                true
            }
            Command::AppearanceChanged => {
                let Some(update) = self.schedules.take_terminal_appearance() else {
                    return true;
                };
                match self.emulator.apply_appearance(update) {
                    Ok(()) => {
                        if !self.write_pending_pty_responses() {
                            return false;
                        }
                        self.publish_screen()
                    }
                    Err(error) => {
                        self.send_runtime_failure(TerminalFailure::emulator_error(
                            "apply-terminal-appearance",
                            error,
                        ));
                        false
                    }
                }
            }
            Command::Shutdown => false,
            Command::PollHiddenInput => self.poll_hidden_input(),
        }
    }

    fn poll_hidden_input(&mut self) -> bool {
        if let Some(active) = self
            .schedules
            .update_hidden_input(Instant::now(), self.native_pty.hidden_input())
        {
            send_session_event(
                &self.events,
                TerminalSessionEvent::HiddenInputChanged(active),
            )
        } else {
            true
        }
    }

    fn hidden_input_transition(&mut self) -> bool {
        self.schedules.hidden_input_transition(Instant::now());
        self.poll_hidden_input()
    }

    fn process_paste_request(
        &mut self,
        text: PastePayload,
        reply: async_channel::Sender<Result<PasteRequestOutcome, String>>,
    ) -> bool {
        if !self.terminal_input_focused {
            let _ = reply.try_send(Ok(PasteRequestOutcome::Rejected(
                PasteRejection::TerminalUnfocused,
            )));
            return true;
        }
        let payload = match text.prepare() {
            Ok(payload) => payload,
            Err(rejection) => {
                let _ = reply.try_send(Ok(PasteRequestOutcome::Rejected(rejection)));
                return true;
            }
        };
        let bracketed_paste = match self.emulator.bracketed_paste_mode() {
            Ok(bracketed_paste) => bracketed_paste,
            Err(message) => {
                let _ =
                    reply.try_send(Err("terminal paste mode could not be determined".to_owned()));
                self.send_runtime_failure(message);
                return false;
            }
        };
        if payload.requires_confirmation(bracketed_paste) {
            let outcome = self
                .schedules
                .request_paste_confirmation(payload, Instant::now())
                .map(PasteRequestOutcome::ConfirmationRequired)
                .unwrap_or(PasteRequestOutcome::Rejected(
                    PasteRejection::ConfirmationPending,
                ));
            let _ = reply.try_send(Ok(outcome));
            return true;
        }

        self.write_prepared_paste(payload, reply, PasteRequestOutcome::Written)
    }

    fn process_paste_resolution(
        &mut self,
        id: PasteConfirmationId,
        decision: PasteDecision,
        reply: async_channel::Sender<Result<PasteResolution, String>>,
    ) -> bool {
        let confirmation_focused = self.schedules.paste_confirmation_has_focus(id);
        let Some(payload) = self
            .schedules
            .resolve_paste_confirmation(id, Instant::now())
        else {
            let _ = reply.try_send(Ok(PasteResolution::Stale));
            return true;
        };
        if decision == PasteDecision::Cancel
            || (!self.terminal_input_focused && !confirmation_focused)
        {
            let _ = reply.try_send(Ok(PasteResolution::Cancelled));
            return true;
        }

        match self.emulator.paste(payload.into_text()) {
            Ok(action) => {
                let applied = self.apply_terminal_input_action(action);
                if applied {
                    let _ = reply.try_send(Ok(PasteResolution::Written));
                }
                applied
            }
            Err(message) => {
                let _ = reply.try_send(Err("terminal paste encoding failed".to_owned()));
                self.send_runtime_failure(message);
                false
            }
        }
    }

    fn write_prepared_paste(
        &mut self,
        payload: PreparedPaste,
        reply: async_channel::Sender<Result<PasteRequestOutcome, String>>,
        outcome: PasteRequestOutcome,
    ) -> bool {
        match self.emulator.paste(payload.into_text()) {
            Ok(action) => {
                let applied = self.apply_terminal_input_action(action);
                if applied {
                    let _ = reply.try_send(Ok(outcome));
                }
                applied
            }
            Err(message) => {
                let _ = reply.try_send(Err("terminal paste encoding failed".to_owned()));
                self.send_runtime_failure(message);
                false
            }
        }
    }

    fn refresh_selection_autoscroll(&mut self) -> bool {
        match self.emulator.selection_autoscroll_interval() {
            Ok(interval) => {
                self.schedules
                    .update_selection_autoscroll(Instant::now(), interval);
                true
            }
            Err(message) => {
                self.send_runtime_failure(message);
                false
            }
        }
    }

    fn process_reader_events(&mut self, epoch: Option<u64>) -> bool {
        self.process_reader_events_limit(PTY_OUTPUT_QUEUE_CAPACITY, epoch)
    }

    fn process_reader_events_limit(&mut self, limit: usize, epoch: Option<u64>) -> bool {
        let (batch, commands_open) = match self.receive_reader_batch(limit, epoch) {
            Ok(batch) => batch,
            Err(message) => {
                self.send_runtime_failure(message);
                return false;
            }
        };
        let ReaderEventBatch {
            chunks,
            reader_stopped,
        } = batch;
        if !self.process_output_chunks(chunks) {
            return false;
        }

        if let Some(read_error) = reader_stopped {
            if self.clipboard.pending() {
                self.clipboard.reader_stop = Some(read_error);
                true
            } else {
                self.finish_reader_output(read_error)
            }
        } else {
            commands_open
        }
    }

    fn finish_reader_output(
        &mut self,
        read_error: Option<crate::platform::native_pty::NativePtyReadFailure>,
    ) -> bool {
        if !self.end_synchronized_output() {
            return false;
        }
        let event = classify_reader_stop(
            read_error,
            self.native_pty.wait_for_exit(FINAL_CHILD_WAIT_TIMEOUT),
        );
        self.emulator.mark_metadata_stale();
        if !self.publish_screen_snapshot() || !self.publish_final_accessibility() {
            return false;
        }
        self.send_terminal_event(event);
        false
    }

    fn receive_reader_batch(
        &mut self,
        limit: usize,
        mut epoch: Option<u64>,
    ) -> Result<(ReaderEventBatch, bool), TerminalFailure> {
        let mut batch = ReaderEventBatch {
            chunks: Vec::with_capacity(limit),
            reader_stopped: None,
        };
        let mut commands_open = true;

        for index in 0..limit {
            match self.reader_events.recv() {
                Ok(NativePtyOutput::Bytes(bytes)) => batch.chunks.push((bytes, epoch)),
                Ok(NativePtyOutput::Stopped(read_error)) => {
                    batch.reader_stopped = Some(read_error);
                    break;
                }
                Err(_) => {
                    return Err(TerminalFailure::pty("receive-shell-output")
                        .with_reason(FailureReason::ChannelClosed));
                }
            }

            if index + 1 == limit {
                break;
            }
            match self.commands.try_recv() {
                Ok(Command::ReaderReady(next_epoch)) => {
                    epoch = next_epoch;
                }
                Ok(command) => {
                    self.pending_command = Some(command);
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    commands_open = false;
                    break;
                }
            }
        }

        Ok((batch, commands_open))
    }

    fn process_output_chunks(&mut self, chunks: Vec<(Vec<u8>, Option<u64>)>) -> bool {
        let previous_metadata = self.emulator.metadata();
        let received_output = !chunks.is_empty() || !self.clipboard.effects.is_empty();
        for (bytes, epoch) in chunks {
            self.clipboard
                .enqueue(self.osc52_filter.feed_with_context(&bytes, epoch));
        }
        let mut focus_reports = Vec::new();
        // Every Permission Request in these chunks becomes one event, so a program that floods
        // requests cannot crowd other events out of the bounded queue.
        let mut requested: Option<(PermissionRequest, Option<u64>)> = None;
        while !self.clipboard.pending() {
            let Some((effect, epoch)) = self.clipboard.effects.pop_front() else {
                break;
            };
            let continued = match effect {
                Osc52Effect::Terminal(bytes) => {
                    self.feed_terminal_output(&bytes, epoch, &mut requested, &mut focus_reports)
                }
                Osc52Effect::Operation(operation) => {
                    if !self.flush_ordered_terminal_replies(&mut focus_reports) {
                        return false;
                    }
                    let denied = self.clipboard.begin(operation, epoch);
                    if self.clipboard.pending() {
                        self.events.notify();
                    }
                    denied.is_none_or(|bytes| self.write_pty(&bytes))
                }
                Osc52Effect::Rejected(_) => true,
            };
            if !continued {
                return false;
            }
        }
        if let Some((request, epoch)) = requested {
            if !self.clipboard.authority.routed() {
                self.permission_request_state.publish(&request);
            }
            let effect = spaceterm_server::pane::effects::Effect::PermissionSetup {
                screen_recording: request.permissions().contains(
                    &crate::platform::permission_access::SystemPermission::ScreenRecording,
                ),
                accessibility: request
                    .permissions()
                    .contains(&crate::platform::permission_access::SystemPermission::Accessibility),
            };
            if !self.send_client_effect(
                TerminalSessionEvent::PermissionRequested(request),
                effect,
                epoch,
            ) {
                return false;
            }
        }

        if received_output {
            if self.server_reading.is_some() {
                self.schedules.note_hidden_server_content_changed();
            }
            let metadata = self.emulator.metadata();
            let metadata_revised = metadata.revision != previous_metadata.revision;
            if metadata_revised {
                self.metadata_state.publish(metadata);
                self.schedules.note_metadata_changed();
            }
            if !self.flush_ordered_terminal_replies(&mut focus_reports)
                || !self.hidden_input_transition()
            {
                return false;
            }
            // A presentable Terminal Session's next Screen carries the metadata wakeup. A separate
            // event first can evict lossless attention when that Screen replaces an older queue
            // entry. Hidden Terminal Sessions receive no Screens, so they still need the wakeup.
            if metadata_revised
                && !self.schedules.is_presentable()
                && !self.publish_metadata_changed()
            {
                return false;
            }
            if self.emulator.synchronized_output_deadline().is_none()
                && !self.request_pty_presentation()
            {
                return false;
            }
        }
        true
    }

    fn complete_clipboard(&mut self, id: Option<u64>, completion: ClipboardCompletion) -> bool {
        let (completed, response) = self.clipboard.complete(id, completion);
        if !completed {
            return true;
        }
        if response.is_some_and(|bytes| !self.write_pty(&bytes)) {
            return false;
        }
        if !self.process_output_chunks(Vec::new()) {
            return false;
        }
        if !self.clipboard.pending()
            && let Some(stop) = self.clipboard.reader_stop.take()
        {
            return self.finish_reader_output(stop);
        }
        true
    }

    /// Feeds output to the emulator and adds the Permission Requests it carries to `requested`.
    fn feed_terminal_output(
        &mut self,
        bytes: &[u8],
        epoch: Option<u64>,
        requested: &mut Option<(PermissionRequest, Option<u64>)>,
        focus_reports: &mut Vec<u8>,
    ) -> bool {
        let emulator = &mut self.emulator;
        self.permission_requests.feed_with_context(
            bytes,
            epoch,
            |bytes| emulator.feed(bytes),
            |request, epoch| match requested {
                Some((requested, old_epoch)) => {
                    requested.merge(request);
                    if *old_epoch != epoch {
                        *old_epoch = None;
                    }
                }
                None => *requested = Some((request, epoch)),
            },
        );
        if let Some(error) = self.emulator.graphics_failure() {
            self.send_runtime_failure(TerminalFailure::emulator_error(
                "update-terminal-graphics-storage",
                error,
            ));
            return false;
        }
        for event in self.emulator.take_attention_events() {
            let effect = match event {
                AttentionEvent::Bell => spaceterm_server::pane::effects::Effect::Bell,
                AttentionEvent::CommandFinished {
                    exit_status,
                    duration,
                } => spaceterm_server::pane::effects::Effect::CommandFinished {
                    exit_status,
                    duration_ms: duration.as_millis().min(u128::from(u64::MAX)) as u64,
                },
            };
            if !self.send_client_effect(TerminalSessionEvent::Attention(event), effect, epoch) {
                return false;
            }
        }
        let focus_reporting_enabled = match self.emulator.focus_reporting_enabled() {
            Ok(enabled) => enabled,
            Err(message) => {
                self.send_runtime_failure(message);
                return false;
            }
        };
        if focus_reporting_enabled && !self.focus_reporting_enabled {
            match self.emulator.focus(self.terminal_input_focused) {
                Ok(action) => focus_reports.extend(action.bytes),
                Err(message) => {
                    self.send_runtime_failure(message);
                    return false;
                }
            }
        }
        self.focus_reporting_enabled = focus_reporting_enabled;
        true
    }

    fn flush_ordered_terminal_replies(&mut self, focus_reports: &mut Vec<u8>) -> bool {
        self.write_pending_pty_responses()
            && (focus_reports.is_empty() || self.write_pty(&mem::take(focus_reports)))
    }

    fn process_key(&mut self, input: KeyInput) -> bool {
        if !self.held_keys.route(&input) {
            return true;
        }
        match self.emulator.key(input) {
            Ok(action) => self.apply_terminal_input_action(action),
            // A key that cannot be encoded is discarded. It says nothing about the Terminal
            // Session, which keeps running.
            Err(failure)
                if failure.reason()
                    == Some(crate::terminal::failure::FailureReason::InvalidInput) =>
            {
                true
            }
            Err(message) => {
                self.send_runtime_failure(message);
                false
            }
        }
    }

    fn process_shortcut(&mut self, mut input: KeyInput) -> bool {
        // Menu Shortcuts may never deliver a physical key-up event.
        // Complete the semantic gesture and suppress any later physical release.
        if !self.process_key(input.clone()) {
            return false;
        }
        input.action = crate::terminal::key::KeyAction::Release;
        let key = input.physical_key;
        if !self.process_key(input) {
            return false;
        }
        self.held_keys.suppress_shortcut_release(key);
        true
    }

    fn cancel_pointer_drag(&mut self) -> bool {
        match self.emulator.cancel_pointer_drag() {
            Ok(action) => self.apply_emulator_action(action) && self.refresh_selection_autoscroll(),
            Err(message) => {
                self.send_runtime_failure(message);
                false
            }
        }
    }

    fn process_focus(&mut self, focused: bool) -> bool {
        self.process_focus_target(if focused {
            TerminalFocusTarget::Terminal
        } else {
            TerminalFocusTarget::Unfocused
        })
    }

    fn process_focus_target(&mut self, target: TerminalFocusTarget) -> bool {
        let focused = matches!(target, TerminalFocusTarget::Terminal);
        match target {
            TerminalFocusTarget::PasteConfirmation(id) if self.terminal_input_focused => {
                if !self.schedules.focus_paste_confirmation(id) {
                    self.schedules.cancel_paste_confirmation();
                }
            }
            TerminalFocusTarget::Terminal => {}
            TerminalFocusTarget::Unfocused | TerminalFocusTarget::PasteConfirmation(_) => {
                self.schedules.cancel_paste_confirmation();
            }
        }
        if !focused && !self.cancel_pointer_drag() {
            return false;
        }
        if !self.hidden_input_transition() {
            return false;
        }
        if self.terminal_input_focused == focused {
            return true;
        }

        if !focused {
            for input in self.held_keys.take_releases() {
                match self.emulator.key(input) {
                    Ok(action) => {
                        if !self.apply_emulator_action(action) {
                            return false;
                        }
                    }
                    Err(message) => {
                        self.send_runtime_failure(message);
                        return false;
                    }
                }
            }
        }
        self.terminal_input_focused = focused;

        match self.emulator.focus(focused) {
            Ok(action) => self.apply_emulator_action(action),
            Err(message) => {
                self.send_runtime_failure(message);
                false
            }
        }
    }

    fn apply_emulator_action(&mut self, action: EmulatorAction) -> bool {
        self.write_pending_pty_responses()
            && (action.bytes.is_empty() || self.write_pty(&action.bytes))
            && (!action.screen_changed || self.publish_visible_screen_change())
    }

    fn apply_terminal_input_action(&mut self, action: EmulatorAction) -> bool {
        if !self.write_pending_pty_responses() {
            return false;
        }
        if !action.bytes.is_empty() && !self.write_pty(&action.bytes) {
            return false;
        }
        if !action.bytes.is_empty() && !self.hidden_input_transition() {
            return false;
        }
        !action.screen_changed || self.publish_visible_screen_change()
    }

    fn publish_visible_screen_change(&mut self) -> bool {
        if self.server_reading.is_some() {
            self.schedules.note_hidden_server_content_changed();
        }
        self.schedules.request_presentation();
        if self.schedules.take_visible_presentation() {
            self.publish_screen()
        } else {
            true
        }
    }

    fn write_pending_pty_responses(&mut self) -> bool {
        let responses = self.emulator.take_pty_responses();
        responses.is_empty() || self.write_pty(&responses)
    }

    fn write_pty(&mut self, bytes: &[u8]) -> bool {
        if self.input.enqueue(bytes).is_err() {
            let _ = self.send_runtime_failure(
                TerminalFailure::pty("queue-shell-input")
                    .with_reason(FailureReason::CapacityExceeded),
            );
            return false;
        }
        self.flush_input()
    }

    fn flush_input(&mut self) -> bool {
        if let Err(error) = self.input.drain(&mut self.native_pty) {
            let _ = self.send_runtime_failure(
                TerminalFailure::pty("write-shell-input")
                    .with_reason(FailureReason::Io(error.kind())),
            );
            return false;
        }
        true
    }

    fn publish_screen(&mut self) -> bool {
        self.metadata_state.publish(self.emulator.metadata());
        if self.events.is_closed() {
            if self.server_reading.is_some() {
                self.schedules.set_server_presentable(false, Instant::now());
            } else {
                self.schedules.set_presentable(false, Instant::now());
            }
            return true;
        }

        self.publish_screen_snapshot()
    }

    fn publish_screen_snapshot(&mut self) -> bool {
        self.metadata_state.publish(self.emulator.metadata());
        if let Some(reading) = &mut self.server_reading {
            reading.update(&mut self.emulator);
        }
        let semantic_content_changed = self.emulator.accessibility_content_changed();
        self.emulator.set_input_serial(self.input.written_serial());
        let snapshot = self.emulator.snapshot();
        self.schedules
            .update_graphics_animation(self.emulator.graphics_animation_deadline());
        match snapshot {
            Ok(Some(snapshot)) => {
                let semantics_changed = semantic_content_changed
                    || !matches!(
                        snapshot.damage.content,
                        super::emulator::ContentDamageSnapshot::Clean
                    )
                    || !matches!(
                        snapshot.damage.cursor,
                        super::emulator::ContentDamageSnapshot::Clean
                    )
                    || snapshot.damage.selection_presence
                    || snapshot.damage.viewport
                    || snapshot.damage.scrollbar
                    || snapshot.damage.active_screen
                    || snapshot.damage.resize;
                let result = self
                    .events
                    .force_send(TerminalSessionEvent::Screen(snapshot))
                    .is_ok()
                    || self.events.is_closed();
                if result {
                    let now = self.schedules.now();
                    self.schedules.metadata_presented();
                    self.schedules.mark_presented(now);
                    if self.accessibility.is_closed() {
                        self.schedules.disable_accessibility();
                    } else if semantics_changed {
                        self.schedules.update_accessibility(false);
                        self.schedules.note_screen_published();
                    }
                }
                result
            }
            Ok(None) => {
                if semantic_content_changed && !self.accessibility.is_closed() {
                    self.schedules.note_screen_published();
                }
                true
            }
            Err(error) => {
                self.send_runtime_failure(TerminalFailure::emulator_error(
                    "produce-terminal-screen-snapshot",
                    error,
                ));
                false
            }
        }
    }

    fn request_presentation(&mut self) -> bool {
        self.request_presentation_at(self.schedules.now())
    }

    fn request_pty_presentation(&mut self) -> bool {
        let now = self.schedules.now();
        self.schedules.request_pty_presentation(now);
        if self.schedules.presentation_due(now) {
            self.publish_screen()
        } else {
            true
        }
    }

    fn publish_metadata_changed(&mut self) -> bool {
        let Some(wakeup) = self.schedules.take_metadata_presentation() else {
            return true;
        };
        match self
            .events
            .try_send(TerminalSessionEvent::MetadataChanged(wakeup))
        {
            Ok(()) | Err(async_channel::TrySendError::Full(_)) => true,
            Err(async_channel::TrySendError::Closed(_)) => true,
        }
    }

    fn advance_metadata_status(&mut self) -> bool {
        let now = Instant::now();
        if !self.emulator.advance_metadata_status(now) {
            return true;
        }
        self.metadata_state.publish(self.emulator.metadata());
        self.schedules.note_metadata_changed();
        if self.schedules.is_presentable() {
            self.schedules.request_presentation();
            if self.emulator.synchronized_output_deadline().is_none()
                && self.schedules.presentation_due(now)
            {
                self.publish_screen()
            } else {
                true
            }
        } else {
            self.publish_metadata_changed()
        }
    }

    fn request_presentation_at(&mut self, now: Instant) -> bool {
        self.schedules.request_presentation();
        if self.schedules.presentation_due(now) {
            self.publish_screen()
        } else {
            true
        }
    }

    fn publish_accessibility(&mut self, bind_current_presentation: bool) -> bool {
        if self.accessibility.is_closed() {
            self.schedules.disable_accessibility();
            return true;
        }
        if self.emulator.synchronized_output_deadline().is_some() {
            return true;
        }
        let update = if bind_current_presentation {
            self.emulator
                .accessibility_snapshot_for_current_presentation()
        } else {
            self.emulator.accessibility_snapshot(false)
        };
        let (accessibility, more) = match update {
            Ok(update) => update,
            Err(error) => {
                self.send_runtime_failure(error);
                return false;
            }
        };
        self.schedules.update_accessibility(more);
        self.schedules
            .mark_accessibility_presented(Instant::now(), !more);
        if let Some(accessibility) = accessibility {
            // Accessibility is an independent best-effort presentation lane. Losing its
            // receiver must not stop shell IO or lifecycle delivery on the event lane.
            let _ = self.accessibility.force_send(accessibility);
            self.events.notify();
        }
        true
    }

    fn publish_final_accessibility(&mut self) -> bool {
        let mut bind_current_presentation = true;
        loop {
            if !self.publish_accessibility(bind_current_presentation) {
                return false;
            }
            bind_current_presentation = false;
            if !self.schedules.take_accessibility_continuation() {
                return true;
            }
        }
    }

    fn release_synchronized_output_if_due(&mut self, now: Instant) -> bool {
        match self.emulator.expire_synchronized_output(now) {
            Ok(true) => self.request_presentation_at(now),
            Ok(false) => true,
            Err(error) => {
                self.send_runtime_failure(TerminalFailure::emulator_error(
                    "release-synchronized-terminal-output",
                    error,
                ));
                false
            }
        }
    }

    fn end_synchronized_output(&mut self) -> bool {
        match self.emulator.end_synchronized_output() {
            Ok(_) => true,
            Err(error) => {
                self.send_runtime_failure(TerminalFailure::emulator_error(
                    "end-synchronized-terminal-output",
                    error,
                ));
                false
            }
        }
    }

    fn send_runtime_failure(&self, failure: TerminalFailure) -> bool {
        self.send_terminal_event(TerminalSessionEvent::Failed(
            TerminalSessionFailure::Runtime(failure),
        ))
    }

    fn send_client_effect(
        &self,
        direct: TerminalSessionEvent,
        effect: spaceterm_server::pane::effects::Effect,
        epoch: Option<u64>,
    ) -> bool {
        if !self.clipboard.authority.routed() {
            return self.send_terminal_event(direct);
        }
        if self.clipboard.authority.native_request(epoch) {
            self.events.client_effect(ClientEffectRequest::new(
                effect,
                Arc::clone(&self.clipboard.authority),
                epoch.unwrap_or_default(),
                Duration::from_secs(1),
            ));
            true
        } else {
            true
        }
    }

    fn send_terminal_event(&self, event: TerminalSessionEvent) -> bool {
        send_session_event(&self.events, event)
    }

    fn finish(self) {
        let Self {
            native_pty,
            emulator: _emulator,
            commands: _commands,
            reader_events,
            events: _events,
            pending_command: _pending_command,
            terminal_input_focused: _terminal_input_focused,
            focus_reporting_enabled: _focus_reporting_enabled,
            held_keys: _held_keys,
            ..
        } = self;
        // The Native PTY Owner performs termination, waiting, and reaping off the GPUI thread.
        drop(reader_events);
        drop(native_pty);
    }
}

fn classify_reader_stop(
    read_error: Option<crate::platform::native_pty::NativePtyReadFailure>,
    wait_result: Result<NativePtyExit, NativePtyWaitFailure>,
) -> TerminalSessionEvent {
    match (read_error, wait_result) {
        (None, Ok(exit)) => TerminalSessionEvent::Exited(classify_native_pty_exit(exit)),
        (Some(read_error), Ok(exit)) => {
            TerminalSessionEvent::Failed(TerminalSessionFailure::PtyRead {
                read_error: read_error.to_string(),
                exit_status: classify_native_pty_exit(exit).to_string(),
            })
        }
        (read_error, Err(wait_error)) => {
            TerminalSessionEvent::Failed(TerminalSessionFailure::ShellWait {
                read_error: read_error.map(|error| error.to_string()),
                wait_error: wait_error.to_string(),
            })
        }
    }
}

fn classify_native_pty_exit(exit: NativePtyExit) -> TerminalSessionExit {
    match exit {
        NativePtyExit::Success => TerminalSessionExit::Success,
        NativePtyExit::ExitCode(code) => TerminalSessionExit::ExitCode(code),
        NativePtyExit::Signal(signal) => TerminalSessionExit::Signal(signal),
        NativePtyExit::GracefulShutdown => TerminalSessionExit::GracefulShutdown,
        NativePtyExit::ForcedShutdown => TerminalSessionExit::ForcedShutdown,
    }
}

fn send_session_event(events: &TerminalEventPublisher, event: TerminalSessionEvent) -> bool {
    let finished = matches!(
        event,
        TerminalSessionEvent::Exited(_) | TerminalSessionEvent::Failed(_)
    );
    let sent = match events.try_send(event) {
        Ok(()) => true,
        Err(async_channel::TrySendError::Full(event)) => {
            events.force_send(event).is_ok() || events.is_closed()
        }
        Err(async_channel::TrySendError::Closed(_)) => true,
    };
    if finished {
        events.close();
    }
    sent
}

#[cfg(test)]
fn join_worker(worker: JoinHandle<()>) {
    if worker.join().is_err() {
        eprintln!("terminal worker thread panicked");
    }
}

#[cfg(test)]
#[path = "session/tests.rs"]
pub(super) mod tests;

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
#[path = "../platform/unix_adapter_tests/session.rs"]
mod unix_adapter_tests;
