use super::native_services::PastePayload;
mod schedules;
use schedules::{FindQueryUpdate, ScheduleInput, WorkerSchedules};
mod launch;
use super::pointer_input::*;
use crate::platform::local_filesystem::LocalFilesystemAuthority;
pub(crate) use launch::{
    LocalTerminalLaunchPlan, NativeTerminalSessionFactory, RemoteTerminalLaunchPlan,
    TerminalLaunchPlan,
};
use std::fmt;
use std::io::Write;
use std::mem;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
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
use crate::terminal::accessibility::AccessibilitySelectionRequest;
use crate::terminal::accessibility::TerminalAccessibilityModel;
use crate::terminal::attention::AttentionEvent;
#[cfg(test)]
use crate::terminal::emulator::MAX_SYNCHRONIZED_OUTPUT_DURATION;
use crate::terminal::emulator::{
    EmulatorAction, PresentationGeneration, ScreenSnapshot, TerminalEmulator,
};
use crate::terminal::geometry::TerminalGeometry;
#[cfg(test)]
use crate::terminal::identity;
#[cfg(test)]
use crate::terminal::key::InputModifiers;
#[cfg(test)]
use crate::terminal::key::OptionAsAltPolicy;
use crate::terminal::key::{KeyInput, PhysicalKey};
use crate::terminal::metadata::{
    LocalMachine, RemoteTerminalMetadataContext, TerminalMetadataContext,
};
use crate::terminal::osc52::{Osc52Effect, Osc52Filter};
use crate::terminal::paste::{
    PasteConfirmationId, PasteDecision, PasteRejection, PasteRequestOutcome, PasteResolution,
    PreparedPaste,
};
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
#[derive(Clone, Debug)]
pub(crate) enum SessionEvent {
    Screen(Arc<ScreenSnapshot>),
    CurrentDirectoryChanged,
    Attention(AttentionEvent),
    HiddenInputChanged(bool),
    Exited(SessionExit),
    Failed(SessionFailure),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TerminalAppearanceUpdate {
    pub(crate) generation: AppearanceGeneration,
    pub(crate) appearance: Arc<ResolvedTerminalAppearance>,
}

#[cfg(test)]
pub(crate) fn test_terminal_appearance_update() -> TerminalAppearanceUpdate {
    use crate::appearance::{
        AppearancePreferences, AvailableFonts, SchemeCatalog, SystemAppearance,
    };

    let resolved = SchemeCatalog::default()
        .resolve(
            AppearanceGeneration::INITIAL,
            &AppearancePreferences::default(),
            SystemAppearance::unavailable(),
            &AvailableFonts::default(),
        )
        .expect("built-in terminal appearance must resolve");
    TerminalAppearanceUpdate::new(resolved.generation, resolved.terminal)
}

impl TerminalAppearanceUpdate {
    pub(crate) fn new(
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
pub(crate) enum SessionExit {
    Success,
    ExitCode(u32),
    Signal(String),
    GracefulShutdown,
    ForcedShutdown,
}

impl fmt::Display for SessionExit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Success => formatter.write_str("Shell exited successfully"),
            Self::ExitCode(code) => write!(formatter, "Shell exited with code {code}"),
            Self::Signal(signal) => write!(formatter, "Shell exited after signal {signal}"),
            Self::GracefulShutdown => formatter.write_str("Shell shut down gracefully"),
            Self::ForcedShutdown => formatter.write_str("Shell shutdown was forced"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SessionFailure {
    Startup {
        stage: SessionStartupStage,
        message: String,
    },
    Runtime(String),
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
pub(crate) enum SessionStartupStage {
    Pty,
    Reader,
    ReaderThread,
    Emulator,
}

impl fmt::Display for SessionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup { stage, message } => {
                write!(
                    formatter,
                    "Terminal Session startup failed during {stage}: {message}"
                )
            }
            Self::Runtime(message) => write!(formatter, "Terminal runtime failed: {message}"),
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

impl std::error::Error for SessionFailure {}

impl fmt::Display for SessionStartupStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pty => formatter.write_str("PTY creation"),
            Self::Reader => formatter.write_str("PTY reader acquisition"),
            Self::ReaderThread => formatter.write_str("PTY reader thread creation"),
            Self::Emulator => formatter.write_str("Terminal Emulator creation"),
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum SessionError {
    #[error("failed to start the terminal worker thread: {0}")]
    SpawnWorker(#[source] std::io::Error),
    #[error(transparent)]
    PreparedSshPaneChannel(#[from] crate::ssh::command::PreparedSshPaneChannelError),
    #[cfg(test)]
    #[error("terminal worker stopped before initialization completed")]
    StartupChannelClosed,
    #[cfg(test)]
    #[error("terminal emulator initialization failed: {0}")]
    EmulatorStartup(String),
}

pub(crate) struct StartedTerminalSession {
    pub(crate) handle: Box<dyn TerminalSessionHandle>,
    pub(crate) events: async_channel::Receiver<SessionEvent>,
    pub(crate) accessibility: async_channel::Receiver<Arc<TerminalAccessibilityModel>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SelectionCopyError {
    Formatting,
    WorkerStopped,
}

#[derive(Clone, Debug)]
pub(crate) struct AccessibilitySelectionSender {
    commands: CommandSender<Command>,
}

impl AccessibilitySelectionSender {
    #[cfg(test)]
    pub(crate) fn recording_channel() -> (Self, RecordingAccessibilitySelectionReceiver) {
        let (commands, receiver) = mpsc::channel();
        (
            Self { commands },
            RecordingAccessibilitySelectionReceiver(receiver),
        )
    }

    pub(crate) fn request(&self, request: AccessibilitySelectionRequest) {
        if self
            .commands
            .send(Command::AccessibilitySelection(request))
            .is_err()
        {
            eprintln!(
                "terminal accessibility selection was dropped because the worker has stopped"
            );
        }
    }
}

#[derive(Clone)]
pub(crate) struct AccessibilityDemandSender {
    commands: CommandSender<Command>,
    schedule_input: ScheduleInput,
}

impl fmt::Debug for AccessibilityDemandSender {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessibilityDemandSender")
    }
}

impl AccessibilityDemandSender {
    pub(crate) fn request(&self) {
        self.request_at(Instant::now());
    }

    fn request_at(&self, requested_at: Instant) {
        if self
            .schedule_input
            .enqueue_accessibility_demand(requested_at)
            && self.commands.send(Command::AccessibilityDemand).is_err()
        {
            eprintln!("terminal accessibility demand was dropped because the worker has stopped");
        }
    }
}

#[cfg(test)]
pub(crate) struct RecordingAccessibilitySelectionReceiver(CommandReceiver<Command>);

#[cfg(test)]
impl RecordingAccessibilitySelectionReceiver {
    pub(crate) fn drain(&self) -> Vec<AccessibilitySelectionRequest> {
        self.0
            .try_iter()
            .map(|command| match command {
                Command::AccessibilitySelection(request) => request,
                _ => unreachable!("Selection authority only sends Selection commands"),
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SessionDirectorySnapshot {
    pub(crate) revision: u64,
    pub(crate) current: Option<crate::domain::CurrentDirectory>,
}

// Retained separately because presentation events may evict any earlier queue entry.
#[derive(Clone, Default)]
struct SessionDirectoryState(Arc<Mutex<Option<SessionDirectorySnapshot>>>);

impl SessionDirectoryState {
    fn snapshot(&self) -> Option<SessionDirectorySnapshot> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn publish(&self, metadata: &crate::terminal::metadata::TerminalMetadataSnapshot) {
        let current = (metadata.freshness == crate::terminal::metadata::MetadataFreshness::Live)
            .then(|| metadata.context.current_directory(&metadata.directory.path))
            .flatten();
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        *state = Some(SessionDirectorySnapshot {
            revision: metadata.revision,
            current,
        });
    }
}

pub(crate) trait TerminalSessionHandle {
    fn directory_snapshot(&self) -> Option<SessionDirectorySnapshot> {
        None
    }

    fn key(&self, input: KeyInput);
    fn focus(&self, focused: bool);
    fn resize(&self, geometry: TerminalGeometry);
    fn pointer(&self, input: PointerInput);
    fn pointer_and_copy_selection(
        &self,
        input: PointerInput,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError>;
    fn wheel(&self, input: WheelInput);
    fn scroll_to(&self, offset_rows: u64, generation: PresentationGeneration);
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
    fn update_appearance(&self, update: TerminalAppearanceUpdate);
}

/// Starts one Terminal Session by consuming typed Local or Remote launch authority.
pub(crate) trait TerminalSessionFactory {
    /// Starts a session without reparsing or weakening the supplied launch plan.
    ///
    /// Remote implementations consume the prepared OpenSSH command exactly once and use only the
    /// plan's validated local home as local process working-directory authority.
    fn start(
        &self,
        geometry: TerminalGeometry,
        launch_plan: TerminalLaunchPlan,
        initial_appearance: TerminalAppearanceUpdate,
    ) -> Result<StartedTerminalSession, SessionError>;

    fn fallback_title(&self) -> String {
        "Terminal".to_owned()
    }
}

pub(crate) struct TerminalSession {
    directory_state: SessionDirectoryState,
    commands: Option<CommandSender<Command>>,
    worker: Option<JoinHandle<()>>,
    native_pty_close: Option<NativePtyCloseHandle>,
    schedule_input: ScheduleInput,
}

type StartedSession = (
    TerminalSession,
    async_channel::Receiver<SessionEvent>,
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

impl TerminalSession {
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

    fn shutdown(&mut self) {
        self.request_shutdown();
        // Dropping a JoinHandle detaches the worker. It still owns the PTY and reader
        // cleanup, but a close operation must never block its GPUI caller on either thread.
        drop(self.worker.take());
    }

    #[cfg(test)]
    fn shutdown_and_join(&mut self) {
        self.request_shutdown();
        if let Some(worker) = self.worker.take() {
            join_worker(worker);
        }
    }
}

impl TerminalSessionHandle for TerminalSession {
    fn directory_snapshot(&self) -> Option<SessionDirectorySnapshot> {
        self.directory_state.snapshot()
    }

    fn key(&self, input: KeyInput) {
        if let Some(commands) = &self.commands
            && commands.send(Command::Key(input)).is_err()
        {
            eprintln!("terminal key input was dropped because the worker has stopped");
        }
    }

    fn focus(&self, focused: bool) {
        if let Some(commands) = &self.commands
            && commands.send(Command::Focus(focused)).is_err()
        {
            eprintln!("terminal focus input was dropped because the worker has stopped");
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
                commands: commands.clone(),
            })
    }

    fn accessibility_demand_sender(&self) -> Option<AccessibilityDemandSender> {
        self.commands
            .as_ref()
            .map(|commands| AccessibilityDemandSender {
                commands: commands.clone(),
                schedule_input: self.schedule_input.clone(),
            })
    }

    fn set_presentable(&self, presentable: bool) {
        self.schedule_input
            .set_accessibility_demand_enabled(presentable);
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

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct ReaderTransport {
    output: Arc<SessionNativePtyOutputSink>,
    event_rx: mpsc::Receiver<NativePtyOutput>,
}

impl ReaderTransport {
    fn new(commands: CommandSender<Command>) -> Self {
        let (events, event_rx) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
        Self {
            output: Arc::new(SessionNativePtyOutputSink { commands, events }),
            event_rx,
        }
    }

    fn output_sink(&self) -> Arc<dyn NativePtyOutputSink> {
        self.output.clone()
    }
}

struct SessionNativePtyOutputSink {
    commands: CommandSender<Command>,
    events: mpsc::SyncSender<NativePtyOutput>,
}

impl NativePtyOutputSink for SessionNativePtyOutputSink {
    fn publish(&self, output: NativePtyOutput) -> bool {
        // ReaderReady orders bounded PTY output against the reliable control command lane.
        self.events.send(output).is_ok() && self.commands.send(Command::ReaderReady).is_ok()
    }
}

struct ReaderEventBatch {
    chunks: Vec<Vec<u8>>,
    reader_stopped: Option<Option<crate::platform::native_pty::NativePtyReadFailure>>,
}

enum Command {
    Key(KeyInput),
    Focus(bool),
    Resize,
    Pointer(PointerInput),
    PointerAndCopySelection(
        PointerInput,
        mpsc::SyncSender<Result<Option<SelectionCopy>, SelectionCopyError>>,
    ),
    Wheel(WheelInput),
    ScrollTo(u64, PresentationGeneration),
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
    AccessibilitySelection(AccessibilitySelectionRequest),
    AccessibilityDemand,
    AccessibilityContinue,
    PublishAccessibility,
    SelectionAutoscrollTick(PresentationGeneration),
    PublishPendingScreen,
    GraphicsAnimationTick,
    GraphicsBudgetAvailable,
    SetPresentable(bool),
    AppearanceChanged,
    ReaderReady,
    Shutdown,
    PollHiddenInput,
}

impl Command {
    fn requires_presentation_barrier(&self) -> bool {
        matches!(
            self,
            Self::Pointer(..)
                | Self::PointerAndCopySelection(..)
                | Self::Wheel(..)
                | Self::ScrollTo(..)
                | Self::SelectionCopy(Some(_), _)
                | Self::AccessibilitySelection(..)
                | Self::AccessibilityContinue
                | Self::PublishAccessibility
                | Self::SelectionAutoscrollTick(..)
        )
    }
}

impl fmt::Debug for Command {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Key(..) => "Key",
            Self::Focus(..) => "Focus",
            Self::Resize => "Resize",
            Self::Pointer(..) => "Pointer",
            Self::PointerAndCopySelection(..) => "PointerAndCopySelection",
            Self::Wheel(..) => "Wheel",
            Self::ScrollTo(..) => "ScrollTo",
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
            Self::SelectionAutoscrollTick(..) => "SelectionAutoscrollTick",
            Self::PublishPendingScreen => "PublishPendingScreen",
            Self::GraphicsAnimationTick => "GraphicsAnimationTick",
            Self::GraphicsBudgetAvailable => "GraphicsBudgetAvailable",
            Self::SetPresentable(..) => "SetPresentable",
            Self::AppearanceChanged => "AppearanceChanged",
            Self::ReaderReady => "ReaderReady",
            Self::Shutdown => "Shutdown",
            Self::PollHiddenInput => "PollHiddenInput",
        };
        formatter.write_str(name)
    }
}

struct TerminalWorker {
    directory_state: SessionDirectoryState,
    native_pty: NativePtyOwner,
    emulator: TerminalEmulator,
    commands: CommandReceiver<Command>,
    reader_events: mpsc::Receiver<NativePtyOutput>,
    events: async_channel::Sender<SessionEvent>,
    accessibility: async_channel::Sender<Arc<TerminalAccessibilityModel>>,
    pending_command: Option<Command>,
    terminal_input_focused: bool,
    focus_reporting_enabled: bool,
    held_keys: HeldKeys,
    schedules: WorkerSchedules,
    osc52_filter: Osc52Filter,
}

struct TerminalWorkerContext {
    local_filesystem: LocalFilesystemAuthority,
    initial_geometry: TerminalGeometry,
    metadata_context: TerminalMetadataContext,
    fallback_title: String,
    terminal_name: &'static str,
    initial_appearance: TerminalAppearanceUpdate,
}

struct TerminalWorkerPublishers {
    directory_state: SessionDirectoryState,
    events: async_channel::Sender<SessionEvent>,
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

enum StartupReporter {
    #[cfg(test)]
    Blocking(mpsc::SyncSender<Result<(), String>>),
    Events(async_channel::Sender<SessionEvent>),
}

impl StartupReporter {
    fn failed(&self, stage: SessionStartupStage, message: String) {
        match self {
            #[cfg(test)]
            Self::Blocking(startup) => {
                let _ = startup.send(Err(message));
            }
            Self::Events(events) => {
                send_session_event(
                    events,
                    SessionEvent::Failed(SessionFailure::Startup { stage, message }),
                );
            }
        }
    }

    fn succeeded(&self) -> bool {
        match self {
            #[cfg(test)]
            Self::Blocking(startup) => startup.send(Ok(())).is_ok(),
            Self::Events(_) => true,
        }
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
        startup: StartupReporter,
    ) {
        let TerminalWorkerContext {
            initial_geometry,
            metadata_context,
            fallback_title,
            terminal_name,
            local_filesystem,
            initial_appearance,
        } = context;
        let TerminalWorkerPublishers {
            directory_state,
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
                startup.failed(SessionStartupStage::Emulator, error.to_string());
                drop(reader_event_rx);
                drop(native_pty);
                return;
            }
        };

        let graphics_commands = output.commands.clone();
        emulator.set_graphics_budget_wakeup(move || {
            let _ = graphics_commands.send(Command::GraphicsBudgetAvailable);
        });

        let mut worker = Self {
            directory_state,
            native_pty,
            emulator,
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
        };

        if !startup.succeeded() {
            worker.finish();
            return;
        }

        worker.run_commands();
        worker.finish();
    }

    fn run_commands(&mut self) {
        if !self.publish_screen() {
            return;
        }

        loop {
            let Some(command) = self.receive_next_command() else {
                break;
            };

            if !self.process_command(command) {
                break;
            }
        }
    }

    fn receive_next_command(&mut self) -> Option<Command> {
        // A command removed from the shared queue while batching PTY output owns this
        // single slot. Run it before schedules can install barrier work into the slot.
        if let Some(command) = self.pending_command.take() {
            return Some(self.note_normal_command(command));
        }
        if self.schedules.must_continue_accessibility() {
            return self.take_accessibility_continuation();
        }
        if let Some(command) = self.schedules.take_due(Instant::now()) {
            return Some(self.note_normal_command(command));
        }
        if self.schedules.accessibility_pending() {
            return match self.commands.try_recv() {
                Ok(command) => Some(self.note_normal_command(command)),
                Err(mpsc::TryRecvError::Empty) => self.take_accessibility_continuation(),
                Err(mpsc::TryRecvError::Disconnected) => None,
            };
        }

        loop {
            let synchronized_output_deadline = self.emulator.synchronized_output_deadline();
            let deadline = self.schedules.deadline(synchronized_output_deadline);
            let Some(deadline) = deadline else {
                let command = self.commands.recv().ok()?;
                return Some(self.note_normal_command(command));
            };
            let timeout = deadline.saturating_duration_since(Instant::now());
            match self.commands.recv_timeout(timeout) {
                Ok(command) => return Some(self.note_normal_command(command)),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let now = Instant::now();
                    if let Some(command) = self.schedules.take_due(now) {
                        return Some(self.note_normal_command(command));
                    }
                    if synchronized_output_deadline.is_some()
                        && !self.release_synchronized_output_if_due(now)
                    {
                        return None;
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
            Command::Key(input) => self.process_key(input),
            Command::Focus(focused) => self.process_focus(focused),
            Command::ReaderReady => self.process_reader_events(),
            Command::Resize => {
                let Some(geometry) = self.schedules.take_resize() else {
                    return true;
                };
                let result = self
                    .native_pty
                    .resize(pty_size(geometry))
                    .map_err(|error| format!("failed to resize the native PTY: {error}"))
                    .and_then(|()| {
                        self.emulator
                            .resize(geometry)
                            .map_err(|error| format!("failed to resize terminal state: {error}"))
                    });

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
            Command::SelectionAutoscrollTick(generation) => {
                if self.emulator.synchronized_output_deadline().is_some() {
                    return true;
                }
                match self.emulator.selection_autoscroll_tick(generation) {
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
                self.schedules.set_presentable(presentable, now);
                if self.schedules.presentation_due(now) {
                    self.publish_screen()
                } else {
                    true
                }
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
                        self.send_runtime_failure(format!(
                            "failed to apply terminal appearance: {error}"
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
            send_session_event(&self.events, SessionEvent::HiddenInputChanged(active))
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
        let Some(payload) = self
            .schedules
            .resolve_paste_confirmation(id, Instant::now())
        else {
            let _ = reply.try_send(Ok(PasteResolution::Stale));
            return true;
        };
        if decision == PasteDecision::Cancel || !self.terminal_input_focused {
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
                self.schedules.update_selection_autoscroll(
                    Instant::now(),
                    interval,
                    self.emulator.presentation_generation(),
                );
                true
            }
            Err(message) => {
                self.send_runtime_failure(message);
                false
            }
        }
    }

    fn process_reader_events(&mut self) -> bool {
        let (batch, commands_open) = match self.receive_reader_batch(PTY_OUTPUT_QUEUE_CAPACITY) {
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
            if !self.end_synchronized_output() {
                return false;
            }
            let event = classify_reader_stop(
                read_error,
                self.native_pty.wait_for_exit(FINAL_CHILD_WAIT_TIMEOUT),
            );
            self.emulator.mark_metadata_stale();
            if !self.publish_screen() {
                return false;
            }
            if !self.publish_final_accessibility() {
                return false;
            }
            self.send_terminal_event(event);
            false
        } else {
            commands_open
        }
    }

    fn receive_reader_batch(&mut self, limit: usize) -> Result<(ReaderEventBatch, bool), String> {
        let mut batch = ReaderEventBatch {
            chunks: Vec::with_capacity(limit),
            reader_stopped: None,
        };
        let mut commands_open = true;

        for index in 0..limit {
            match self.reader_events.recv() {
                Ok(NativePtyOutput::Bytes(bytes)) => batch.chunks.push(bytes),
                Ok(NativePtyOutput::Stopped(read_error)) => {
                    batch.reader_stopped = Some(read_error);
                    break;
                }
                Err(_) => {
                    return Err(
                        "PTY reader notification arrived after its event channel closed".to_owned(),
                    );
                }
            }

            if index + 1 == limit {
                break;
            }
            match self.commands.try_recv() {
                Ok(Command::ReaderReady) => {}
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

    fn process_output_chunks(&mut self, chunks: Vec<Vec<u8>>) -> bool {
        let previous_metadata = self.emulator.metadata();
        let received_output = !chunks.is_empty();
        let mut focus_reports = Vec::new();
        for bytes in chunks {
            for effect in self.osc52_filter.feed(&bytes) {
                let continued = match effect {
                    Osc52Effect::Terminal(bytes) => {
                        self.feed_terminal_output(&bytes, &mut focus_reports)
                    }
                    // Denial still separates earlier replies from later terminal output.
                    Osc52Effect::Operation(_) => {
                        self.flush_ordered_terminal_replies(&mut focus_reports)
                    }
                    Osc52Effect::Rejected(_) => true,
                };
                if !continued {
                    return false;
                }
            }
        }

        if received_output {
            let metadata = self.emulator.metadata();
            if metadata.directory != previous_metadata.directory {
                self.directory_state.publish(&metadata);
                if !self.send_terminal_event(SessionEvent::CurrentDirectoryChanged) {
                    return false;
                }
            }
            if !self.flush_ordered_terminal_replies(&mut focus_reports)
                || !self.hidden_input_transition()
            {
                return false;
            }
            if self.emulator.synchronized_output_deadline().is_none()
                && !self.request_presentation()
            {
                return false;
            }
        }
        true
    }

    fn feed_terminal_output(&mut self, bytes: &[u8], focus_reports: &mut Vec<u8>) -> bool {
        self.emulator.feed(bytes);
        if let Some(error) = self.emulator.graphics_failure() {
            self.send_runtime_failure(format!(
                "failed to update terminal graphics storage: {error}"
            ));
            return false;
        }
        for event in self.emulator.take_attention_events() {
            if !self.send_terminal_event(SessionEvent::Attention(event)) {
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
            Err(message) => {
                self.send_runtime_failure(message);
                false
            }
        }
    }

    fn process_focus(&mut self, focused: bool) -> bool {
        if !self.hidden_input_transition() {
            return false;
        }
        if self.terminal_input_focused == focused {
            return true;
        }

        if !focused {
            self.schedules.cancel_paste_confirmation();
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
        if let Err(error) = self
            .native_pty
            .write_all(bytes)
            .and_then(|()| self.native_pty.flush())
        {
            let _ = self.send_runtime_failure(format!("failed to write to the shell PTY: {error}"));
            return false;
        }
        true
    }

    fn publish_screen(&mut self) -> bool {
        self.directory_state.publish(&self.emulator.metadata());
        if self.events.is_closed() {
            return false;
        }

        let snapshot = self.emulator.snapshot();
        self.schedules
            .update_graphics_animation(self.emulator.graphics_animation_deadline());
        match snapshot {
            Ok(Some(snapshot)) => {
                let result = self
                    .events
                    .force_send(SessionEvent::Screen(snapshot))
                    .is_ok();
                if result {
                    let now = Instant::now();
                    self.schedules.mark_presented(now);
                    self.schedules.update_accessibility(false);
                    if !self.accessibility.is_closed() {
                        self.schedules.note_screen_published();
                    } else {
                        self.schedules.disable_accessibility();
                    }
                }
                result
            }
            Ok(None) => true,
            Err(error) => {
                self.send_runtime_failure(format!(
                    "failed to produce terminal screen snapshot: {error}"
                ));
                false
            }
        }
    }

    fn request_presentation(&mut self) -> bool {
        self.request_presentation_at(Instant::now())
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
                self.send_runtime_failure(format!(
                    "failed to produce terminal accessibility snapshot: {error}"
                ));
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
                self.send_runtime_failure(format!(
                    "failed to release synchronized terminal output: {error}"
                ));
                false
            }
        }
    }

    fn end_synchronized_output(&mut self) -> bool {
        match self.emulator.end_synchronized_output() {
            Ok(_) => true,
            Err(error) => {
                self.send_runtime_failure(format!(
                    "failed to end synchronized terminal output: {error}"
                ));
                false
            }
        }
    }

    fn send_runtime_failure(&self, message: String) -> bool {
        self.send_terminal_event(SessionEvent::Failed(SessionFailure::Runtime(message)))
    }

    fn send_terminal_event(&self, event: SessionEvent) -> bool {
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
) -> SessionEvent {
    match (read_error, wait_result) {
        (None, Ok(exit)) => SessionEvent::Exited(classify_native_pty_exit(exit)),
        (Some(read_error), Ok(exit)) => SessionEvent::Failed(SessionFailure::PtyRead {
            read_error: read_error.to_string(),
            exit_status: classify_native_pty_exit(exit).to_string(),
        }),
        (read_error, Err(wait_error)) => SessionEvent::Failed(SessionFailure::ShellWait {
            read_error: read_error.map(|error| error.to_string()),
            wait_error: wait_error.to_string(),
        }),
    }
}

fn classify_native_pty_exit(exit: NativePtyExit) -> SessionExit {
    match exit {
        NativePtyExit::Success => SessionExit::Success,
        NativePtyExit::ExitCode(code) => SessionExit::ExitCode(code),
        NativePtyExit::Signal(signal) => SessionExit::Signal(signal),
        NativePtyExit::GracefulShutdown => SessionExit::GracefulShutdown,
        NativePtyExit::ForcedShutdown => SessionExit::ForcedShutdown,
    }
}

fn send_session_event(events: &async_channel::Sender<SessionEvent>, event: SessionEvent) -> bool {
    match events.try_send(event) {
        Ok(()) => true,
        Err(async_channel::TrySendError::Full(event)) => events.force_send(event).is_ok(),
        Err(async_channel::TrySendError::Closed(_)) => false,
    }
}

#[cfg(test)]
fn join_worker(worker: JoinHandle<()>) {
    if worker.join().is_err() {
        eprintln!("terminal worker thread panicked");
    }
}

#[cfg(test)]
#[path = "session/tests.rs"]
mod tests;

#[cfg(all(test, target_os = "macos", feature = "macos-native-tests"))]
#[path = "../platform/macos_adapter_tests/session.rs"]
mod macos_adapter_tests;
