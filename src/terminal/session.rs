use std::collections::VecDeque;
use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver as CommandReceiver, Sender as CommandSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Error as AnyError;
#[cfg(test)]
use portable_pty::ExitStatus;
use portable_pty::PtySize;
use thiserror::Error;

use crate::platform::macos_pty::{
    PtyError, PtyTerminator, ShellExit, ShutdownDisposition, SpawnedPty, spawn_user_shell,
    user_shell,
};
use crate::platform::shell_integration::resource_root;
use crate::terminal::attention::AttentionEvent;
#[cfg(test)]
use crate::terminal::emulator::MAX_SYNCHRONIZED_OUTPUT_DURATION;
use crate::terminal::emulator::{
    EmulatorAction, PresentationGeneration, ScreenSnapshot, TerminalEmulator,
};
use crate::terminal::geometry::TerminalGeometry;
use crate::terminal::identity;
#[cfg(test)]
use crate::terminal::key::OptionAsAltPolicy;
use crate::terminal::key::{InputModifiers, KeyInput};
#[cfg(test)]
use crate::terminal::osc52::Osc52ClipboardError;
use crate::terminal::osc52::{
    MAX_OSC52_CONTENT_BYTES, Osc52AccessPolicy, Osc52AuthorizationDecision, Osc52AuthorizationId,
    Osc52AuthorizationPolicy, Osc52AuthorizationRequest, Osc52Clipboard, Osc52Effect, Osc52Filter,
    Osc52Operation,
};
use crate::terminal::paste::{
    PasteConfirmationId, PasteDecision, PasteRejection, PasteRequestOutcome, PasteResolution,
    PreparedPaste,
};
use crate::terminal::selection::{SelectionCopy, SelectionCopyOptions};
use crate::terminal::{FindDirection, FindQueryGeneration};

const FINAL_CHILD_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const PTY_READ_BUFFER_SIZE: usize = 16 * 1024;
const PTY_OUTPUT_QUEUE_CAPACITY: usize = 8;
const TERMIOS_POLL_INTERVAL: Duration = Duration::from_millis(200);
const PASTE_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(30);
const OSC52_AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct SurfacePosition {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PointerButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PointerPhase {
    Press,
    Motion,
    Release,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShiftSelectionPolicy {
    OverrideApplicationMouse,
    ReportToApplication,
}

impl ShiftSelectionPolicy {
    pub(crate) const fn from_selection_override(enabled: bool) -> Self {
        if enabled {
            Self::OverrideApplicationMouse
        } else {
            Self::ReportToApplication
        }
    }
}

impl Default for ShiftSelectionPolicy {
    fn default() -> Self {
        Self::from_selection_override(true)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PointerInput {
    pub(crate) generation: PresentationGeneration,
    pub(crate) phase: PointerPhase,
    pub(crate) button: Option<PointerButton>,
    pub(crate) position: SurfacePosition,
    pub(crate) modifiers: InputModifiers,
    pub(crate) shift_selection: ShiftSelectionPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WheelInput {
    pub(crate) generation: PresentationGeneration,
    pub(crate) horizontal_steps: i32,
    pub(crate) vertical_steps: i32,
    pub(crate) phase: WheelPhase,
    pub(crate) position: SurfacePosition,
    pub(crate) modifiers: InputModifiers,
    pub(crate) shift_selection: ShiftSelectionPolicy,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum WheelPhase {
    GestureStarted,
    #[default]
    GestureChanged,
    GestureEnded,
    GestureCancelled,
    MomentumStarted,
    MomentumChanged,
    MomentumEnded,
    MomentumCancelled,
}

fn pty_size(geometry: TerminalGeometry) -> PtySize {
    let grid = geometry.grid();
    let backing = geometry.backing_grid_size();
    PtySize {
        rows: grid.rows,
        cols: grid.cols,
        pixel_width: backing.width.min(u32::from(u16::MAX)) as u16,
        pixel_height: backing.height.min(u32::from(u16::MAX)) as u16,
    }
}

// Screen events may supersede older screens. Failed and Exited are final events,
// so the worker must not publish another screen after either one.
#[derive(Clone, Debug)]
pub(crate) enum SessionEvent {
    Screen(Arc<ScreenSnapshot>),
    Attention(AttentionEvent),
    HiddenInputChanged(bool),
    Osc52Authorization(Osc52AuthorizationRequest),
    Osc52AuthorizationExpired(Osc52AuthorizationId),
    Exited(SessionExit),
    Failed(SessionFailure),
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
    #[cfg(test)]
    #[error(transparent)]
    Pty(#[from] PtyError),
    #[error("failed to start the terminal worker thread: {0}")]
    SpawnWorker(#[source] std::io::Error),
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SelectionCopyError {
    Formatting,
    WorkerStopped,
}

pub(crate) trait TerminalSessionHandle {
    fn key(&self, input: KeyInput);
    fn focus(&self, focused: bool);
    fn resize(&self, geometry: TerminalGeometry);
    fn pointer(&self, input: PointerInput);
    fn wheel(&self, input: WheelInput);
    fn scroll_to(&self, offset_rows: u64, generation: PresentationGeneration);
    fn set_find_query(&self, generation: FindQueryGeneration, query: String);
    fn navigate_find(&self, generation: FindQueryGeneration, direction: FindDirection);
    fn end_find(&self, generation: FindQueryGeneration);
    fn request_paste(
        &self,
        text: String,
    ) -> async_channel::Receiver<Result<PasteRequestOutcome, String>>;
    fn resolve_paste(
        &self,
        id: PasteConfirmationId,
        decision: PasteDecision,
    ) -> async_channel::Receiver<Result<PasteResolution, String>>;
    fn resolve_osc52_authorization(
        &self,
        id: Osc52AuthorizationId,
        decision: Osc52AuthorizationDecision,
    );
    fn copy_selection(&self) -> Result<Option<SelectionCopy>, SelectionCopyError>;
}

pub(crate) trait TerminalSessionFactory {
    fn start(
        &self,
        geometry: TerminalGeometry,
        working_directory: &Path,
    ) -> Result<StartedTerminalSession, SessionError>;

    fn fallback_title(&self) -> String {
        "Terminal".to_owned()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NativeTerminalSessionFactory;

impl TerminalSessionFactory for NativeTerminalSessionFactory {
    fn start(
        &self,
        geometry: TerminalGeometry,
        working_directory: &Path,
    ) -> Result<StartedTerminalSession, SessionError> {
        let (session, events) = TerminalSession::start(geometry, working_directory)?;
        Ok(StartedTerminalSession {
            handle: Box::new(session),
            events,
        })
    }

    fn fallback_title(&self) -> String {
        let shell = user_shell();
        Path::new(&shell)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(&shell)
            .to_owned()
    }
}

#[derive(Clone, Default)]
struct ResizeMailbox {
    pending: Arc<Mutex<Option<TerminalGeometry>>>,
}

impl ResizeMailbox {
    fn replace(&self, geometry: TerminalGeometry) -> bool {
        let mut pending = self.lock();
        let should_notify = pending.is_none();
        *pending = Some(geometry);
        should_notify
    }

    fn take(&self) -> Option<TerminalGeometry> {
        self.lock().take()
    }

    fn lock(&self) -> MutexGuard<'_, Option<TerminalGeometry>> {
        self.pending.lock().unwrap_or_else(|poisoned| {
            eprintln!("terminal resize mailbox recovered after a worker panic");
            poisoned.into_inner()
        })
    }
}

#[derive(Debug)]
enum FindQueryUpdate {
    Set(FindQueryGeneration, String),
    End(FindQueryGeneration),
}

#[derive(Clone, Default)]
struct FindQueryMailbox {
    pending: Arc<Mutex<Option<FindQueryUpdate>>>,
}

impl FindQueryMailbox {
    fn replace(&self, update: FindQueryUpdate) -> bool {
        let mut pending = self.lock();
        let should_notify = pending.is_none();
        *pending = Some(update);
        should_notify
    }

    fn take(&self) -> Option<FindQueryUpdate> {
        self.lock().take()
    }

    fn lock(&self) -> MutexGuard<'_, Option<FindQueryUpdate>> {
        self.pending.lock().unwrap_or_else(|poisoned| {
            eprintln!("terminal Find mailbox recovered after a worker panic");
            poisoned.into_inner()
        })
    }
}

pub(crate) struct TerminalSession {
    commands: Option<CommandSender<Command>>,
    worker: Option<JoinHandle<()>>,
    terminator: Option<Box<dyn SessionPtyTerminator>>,
    resizes: ResizeMailbox,
    find_queries: FindQueryMailbox,
}

trait SessionPty: Write + Send {
    fn take_reader(&mut self) -> std::io::Result<Box<dyn Read + Send>>;
    fn resize(&self, size: PtySize) -> Result<(), AnyError>;
    fn wait_for_child(&mut self, timeout: Duration) -> std::io::Result<ShellExit>;
    fn hidden_input(&self) -> std::io::Result<bool> {
        Ok(false)
    }
}

trait SessionPtyTerminator: Send + Sync {
    fn terminate(&self) -> std::io::Result<()>;
}

#[cfg(test)]
#[derive(Default)]
struct UnavailableOsc52Clipboard;

#[cfg(test)]
impl Osc52Clipboard for UnavailableOsc52Clipboard {
    fn read(
        &mut self,
        _target: crate::terminal::Osc52Target,
    ) -> Result<String, Osc52ClipboardError> {
        Err(Osc52ClipboardError::Unavailable)
    }

    fn write(
        &mut self,
        _target: crate::terminal::Osc52Target,
        _text: &str,
    ) -> Result<(), Osc52ClipboardError> {
        Err(Osc52ClipboardError::Unavailable)
    }
}

fn native_osc52_clipboard() -> Box<dyn Osc52Clipboard> {
    #[cfg(test)]
    {
        Box::<UnavailableOsc52Clipboard>::default()
    }
    #[cfg(not(test))]
    {
        Box::<crate::platform::macos_pasteboard::MacosOsc52Clipboard>::default()
    }
}

struct StartedSessionPty {
    pty: Box<dyn SessionPty>,
    terminator: Box<dyn SessionPtyTerminator>,
}

#[derive(Clone, Default)]
struct DeferredPtyTerminator {
    state: Arc<Mutex<DeferredPtyTerminationState>>,
}

#[derive(Default)]
struct DeferredPtyTerminationState {
    requested: bool,
    terminator: Option<Box<dyn SessionPtyTerminator>>,
}

impl DeferredPtyTerminator {
    fn install(&self, terminator: Box<dyn SessionPtyTerminator>) -> std::io::Result<()> {
        let mut state = self.lock_state();
        if state.requested {
            drop(state);
            terminator.terminate()
        } else {
            state.terminator = Some(terminator);
            Ok(())
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, DeferredPtyTerminationState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                eprintln!("deferred PTY termination lock was poisoned; recovering");
                poisoned.into_inner()
            }
        }
    }
}

impl SessionPtyTerminator for DeferredPtyTerminator {
    fn terminate(&self) -> std::io::Result<()> {
        let terminator = {
            let mut state = self.lock_state();
            state.requested = true;
            state.terminator.take()
        };
        match terminator {
            Some(terminator) => terminator.terminate(),
            None => Ok(()),
        }
    }
}

impl SessionPty for SpawnedPty {
    fn take_reader(&mut self) -> std::io::Result<Box<dyn Read + Send>> {
        SpawnedPty::take_reader(self)
    }

    fn resize(&self, size: PtySize) -> Result<(), AnyError> {
        SpawnedPty::resize(self, size)
    }

    fn wait_for_child(&mut self, timeout: Duration) -> std::io::Result<ShellExit> {
        SpawnedPty::wait_for_child(self, timeout)
    }

    fn hidden_input(&self) -> std::io::Result<bool> {
        SpawnedPty::hidden_input(self)
    }
}

impl SessionPtyTerminator for PtyTerminator {
    fn terminate(&self) -> std::io::Result<()> {
        PtyTerminator::terminate(self)
    }
}

fn spawn_native_session_pty(
    size: PtySize,
    working_directory: &Path,
) -> Result<StartedSessionPty, PtyError> {
    let (pty, terminator) = spawn_user_shell(size, working_directory)?;
    Ok(StartedSessionPty {
        pty: Box::new(pty),
        terminator: Box::new(terminator),
    })
}

impl TerminalSession {
    pub(crate) fn start(
        geometry: TerminalGeometry,
        working_directory: &Path,
    ) -> Result<(Self, async_channel::Receiver<SessionEvent>), SessionError> {
        Self::start_deferred_with(geometry, working_directory, spawn_native_session_pty)
    }

    fn start_deferred_with(
        geometry: TerminalGeometry,
        working_directory: &Path,
        spawn_pty: impl FnOnce(PtySize, &Path) -> Result<StartedSessionPty, PtyError> + Send + 'static,
    ) -> Result<(Self, async_channel::Receiver<SessionEvent>), SessionError> {
        let working_directory = PathBuf::from(working_directory);
        let fallback_title = shell_fallback_title();
        let terminal_name = identity::launch_identity(&resource_root()).term;
        let (command_tx, command_rx) = mpsc::channel();
        let reader_transport = ReaderTransport::new(command_tx.clone());
        let resizes = ResizeMailbox::default();
        let worker_resizes = resizes.clone();
        let find_queries = FindQueryMailbox::default();
        let worker_find_queries = find_queries.clone();
        let (event_tx, event_rx) = async_channel::bounded(2);
        let deferred_terminator = DeferredPtyTerminator::default();
        let worker_terminator = deferred_terminator.clone();
        let worker_events = event_tx.clone();

        let worker = thread::Builder::new()
            .name("spaceterm-terminal".to_owned())
            .spawn(move || {
                let StartedSessionPty { pty, terminator } =
                    match spawn_pty(pty_size(geometry), &working_directory) {
                        Ok(started) => started,
                        Err(error) => {
                            send_session_event(
                                &worker_events,
                                SessionEvent::Failed(SessionFailure::Startup {
                                    stage: SessionStartupStage::Pty,
                                    message: error.to_string(),
                                }),
                            );
                            return;
                        }
                    };
                if let Err(error) = worker_terminator.install(terminator) {
                    eprintln!("failed to terminate a PTY closed during startup: {error}");
                }
                TerminalWorker::run(
                    pty,
                    TerminalWorkerContext {
                        initial_geometry: geometry,
                        initial_directory: working_directory,
                        fallback_title,
                        terminal_name,
                    },
                    command_rx,
                    reader_transport,
                    TerminalWorkerMailboxes {
                        resizes: worker_resizes,
                        find_queries: worker_find_queries,
                    },
                    event_tx,
                    StartupReporter::Events(worker_events),
                );
            })
            .map_err(SessionError::SpawnWorker)?;

        Ok((
            Self {
                commands: Some(command_tx),
                worker: Some(worker),
                terminator: Some(Box::new(deferred_terminator)),
                resizes,
                find_queries,
            },
            event_rx,
        ))
    }

    #[cfg(test)]
    fn start_with(
        geometry: TerminalGeometry,
        working_directory: &Path,
        spawn_pty: impl FnOnce(PtySize, &Path) -> Result<StartedSessionPty, PtyError>,
    ) -> Result<(Self, async_channel::Receiver<SessionEvent>), SessionError> {
        let worker_directory = working_directory.to_owned();
        let terminal_name = identity::launch_identity(&resource_root()).term;
        let StartedSessionPty { pty, terminator } =
            spawn_pty(pty_size(geometry), working_directory)?;
        let (command_tx, command_rx) = mpsc::channel();
        let reader_transport = ReaderTransport::new(command_tx.clone());
        // Two slots retain the latest screen and a final lifecycle event without
        // allowing sustained PTY output to build an unbounded UI backlog.
        let (event_tx, event_rx) = async_channel::bounded(2);
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let resizes = ResizeMailbox::default();
        let worker_resizes = resizes.clone();
        let find_queries = FindQueryMailbox::default();
        let worker_find_queries = find_queries.clone();

        let worker = thread::Builder::new()
            .name("spaceterm-terminal".to_owned())
            .spawn(move || {
                TerminalWorker::run(
                    pty,
                    TerminalWorkerContext {
                        initial_geometry: geometry,
                        initial_directory: worker_directory,
                        fallback_title: "Terminal".to_owned(),
                        terminal_name,
                    },
                    command_rx,
                    reader_transport,
                    TerminalWorkerMailboxes {
                        resizes: worker_resizes,
                        find_queries: worker_find_queries,
                    },
                    event_tx,
                    StartupReporter::Blocking(startup_tx),
                )
            })
            .map_err(SessionError::SpawnWorker)?;

        match startup_rx.recv() {
            Ok(Ok(())) => Ok((
                Self {
                    commands: Some(command_tx),
                    worker: Some(worker),
                    terminator: Some(terminator),
                    resizes,
                    find_queries,
                },
                event_rx,
            )),
            Ok(Err(message)) => {
                join_worker(worker);
                Err(SessionError::EmulatorStartup(message))
            }
            Err(_) => {
                join_worker(worker);
                Err(SessionError::StartupChannelClosed)
            }
        }
    }

    pub(crate) fn key(&self, input: KeyInput) {
        if let Some(commands) = &self.commands
            && commands.send(Command::Key(input)).is_err()
        {
            eprintln!("terminal key input was dropped because the worker has stopped");
        }
    }

    pub(crate) fn focus(&self, focused: bool) {
        if let Some(commands) = &self.commands
            && commands.send(Command::Focus(focused)).is_err()
        {
            eprintln!("terminal focus input was dropped because the worker has stopped");
        }
    }

    pub(crate) fn resize(&self, geometry: TerminalGeometry) {
        if let Some(commands) = &self.commands
            && self.resizes.replace(geometry)
            && commands.send(Command::Resize).is_err()
        {
            eprintln!("terminal resize was dropped because the worker has stopped");
        }
    }

    pub(crate) fn pointer(&self, input: PointerInput) {
        if let Some(commands) = &self.commands
            && commands.send(Command::Pointer(input)).is_err()
        {
            eprintln!("terminal pointer input was dropped because the worker has stopped");
        }
    }

    pub(crate) fn wheel(&self, input: WheelInput) {
        if let Some(commands) = &self.commands
            && commands.send(Command::Wheel(input)).is_err()
        {
            eprintln!("terminal wheel input was dropped because the worker has stopped");
        }
    }

    pub(crate) fn scroll_to(&self, offset_rows: u64, generation: PresentationGeneration) {
        if let Some(commands) = &self.commands
            && commands
                .send(Command::ScrollTo(offset_rows, generation))
                .is_err()
        {
            eprintln!("terminal scrollbar input was dropped because the worker has stopped");
        }
    }

    pub(crate) fn set_find_query(&self, generation: FindQueryGeneration, query: String) {
        if let Some(commands) = &self.commands
            && self
                .find_queries
                .replace(FindQueryUpdate::Set(generation, query))
            && commands.send(Command::FindQueryChanged).is_err()
        {
            eprintln!("terminal Find query was dropped because the worker has stopped");
        }
    }

    pub(crate) fn navigate_find(&self, generation: FindQueryGeneration, direction: FindDirection) {
        if let Some(commands) = &self.commands
            && commands
                .send(Command::NavigateFind(generation, direction))
                .is_err()
        {
            eprintln!("terminal Find navigation was dropped because the worker has stopped");
        }
    }

    pub(crate) fn end_find(&self, generation: FindQueryGeneration) {
        if let Some(commands) = &self.commands
            && self.find_queries.replace(FindQueryUpdate::End(generation))
            && commands.send(Command::FindQueryChanged).is_err()
        {
            eprintln!("terminal Find close was dropped because the worker has stopped");
        }
    }

    pub(crate) fn request_paste(
        &self,
        text: String,
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

    pub(crate) fn resolve_paste(
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

    pub(crate) fn resolve_osc52_authorization(
        &self,
        id: Osc52AuthorizationId,
        decision: Osc52AuthorizationDecision,
    ) {
        if let Some(commands) = &self.commands
            && commands
                .send(Command::ResolveOsc52Authorization(id, decision))
                .is_err()
        {
            eprintln!("OSC 52 authorization reply was dropped because the worker has stopped");
        }
    }

    pub(crate) fn copy_selection(&self) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        let Some(commands) = &self.commands else {
            return Err(SelectionCopyError::WorkerStopped);
        };
        let (reply, receiver) = mpsc::sync_channel(1);
        commands
            .send(Command::SelectionCopy(reply))
            .map_err(|_| SelectionCopyError::WorkerStopped)?;
        receiver
            .recv()
            .map_err(|_| SelectionCopyError::WorkerStopped)?
    }

    fn request_shutdown(&mut self) {
        // Request termination before transferring sole responsibility to off-thread PTY cleanup.
        if let Some(terminator) = self.terminator.take()
            && let Err(error) = terminator.terminate()
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
    fn key(&self, input: KeyInput) {
        Self::key(self, input);
    }

    fn focus(&self, focused: bool) {
        Self::focus(self, focused);
    }

    fn resize(&self, geometry: TerminalGeometry) {
        Self::resize(self, geometry);
    }

    fn pointer(&self, input: PointerInput) {
        Self::pointer(self, input);
    }

    fn wheel(&self, input: WheelInput) {
        Self::wheel(self, input);
    }

    fn scroll_to(&self, offset_rows: u64, generation: PresentationGeneration) {
        Self::scroll_to(self, offset_rows, generation);
    }

    fn set_find_query(&self, generation: FindQueryGeneration, query: String) {
        Self::set_find_query(self, generation, query);
    }

    fn navigate_find(&self, generation: FindQueryGeneration, direction: FindDirection) {
        Self::navigate_find(self, generation, direction);
    }

    fn end_find(&self, generation: FindQueryGeneration) {
        Self::end_find(self, generation);
    }

    fn request_paste(
        &self,
        text: String,
    ) -> async_channel::Receiver<Result<PasteRequestOutcome, String>> {
        Self::request_paste(self, text)
    }

    fn resolve_paste(
        &self,
        id: PasteConfirmationId,
        decision: PasteDecision,
    ) -> async_channel::Receiver<Result<PasteResolution, String>> {
        Self::resolve_paste(self, id, decision)
    }

    fn resolve_osc52_authorization(
        &self,
        id: Osc52AuthorizationId,
        decision: Osc52AuthorizationDecision,
    ) {
        Self::resolve_osc52_authorization(self, id, decision);
    }

    fn copy_selection(&self) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        Self::copy_selection(self)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

enum ReaderEvent {
    Output(Vec<u8>),
    Stopped(Option<String>),
}

struct ReaderTransport {
    commands: CommandSender<Command>,
    events: mpsc::SyncSender<ReaderEvent>,
    event_rx: mpsc::Receiver<ReaderEvent>,
}

impl ReaderTransport {
    fn new(commands: CommandSender<Command>) -> Self {
        let (events, event_rx) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
        Self {
            commands,
            events,
            event_rx,
        }
    }
}

struct ReaderEventBatch {
    chunks: Vec<Vec<u8>>,
    reader_stopped: Option<Option<String>>,
}

#[derive(Debug)]
enum Command {
    Key(KeyInput),
    Focus(bool),
    Resize,
    Pointer(PointerInput),
    Wheel(WheelInput),
    ScrollTo(u64, PresentationGeneration),
    FindQueryChanged,
    NavigateFind(FindQueryGeneration, FindDirection),
    RequestPaste(
        String,
        async_channel::Sender<Result<PasteRequestOutcome, String>>,
    ),
    ResolvePaste(
        PasteConfirmationId,
        PasteDecision,
        async_channel::Sender<Result<PasteResolution, String>>,
    ),
    PasteConfirmationExpired,
    ResolveOsc52Authorization(Osc52AuthorizationId, Osc52AuthorizationDecision),
    Osc52AuthorizationExpired(Osc52AuthorizationId),
    ResumeOsc52Output,
    SelectionCopy(mpsc::SyncSender<Result<Option<SelectionCopy>, SelectionCopyError>>),
    SelectionAutoscrollTick(PresentationGeneration),
    ReaderReady,
    Shutdown,
    PollHiddenInput,
}

struct TerminalWorker {
    pty: Box<dyn SessionPty>,
    emulator: TerminalEmulator,
    commands: CommandReceiver<Command>,
    reader_events: mpsc::Receiver<ReaderEvent>,
    reader_thread: JoinHandle<()>,
    events: async_channel::Sender<SessionEvent>,
    resizes: ResizeMailbox,
    find_queries: FindQueryMailbox,
    pending_command: Option<Command>,
    terminal_input_focused: bool,
    focus_reporting_enabled: bool,
    held_keys: HeldKeys,
    selection_autoscroll: SelectionAutoscrollSchedule,
    paste_confirmations: PasteConfirmationSchedule,
    osc52_filter: Osc52Filter,
    osc52_policy: Osc52AuthorizationPolicy,
    osc52_clipboard: Box<dyn Osc52Clipboard>,
    osc52_authorization: Osc52AuthorizationSchedule,
    deferred_osc52_effects: VecDeque<Osc52Effect>,
    deferred_output_chunks: VecDeque<Vec<u8>>,
    deferred_reader_ready: bool,
    hidden_input: HiddenInputSchedule,
}

struct TerminalWorkerContext {
    initial_geometry: TerminalGeometry,
    initial_directory: PathBuf,
    fallback_title: String,
    terminal_name: &'static str,
}

struct TerminalWorkerMailboxes {
    resizes: ResizeMailbox,
    find_queries: FindQueryMailbox,
}

struct HiddenInputSchedule {
    active: bool,
    deadline: Instant,
}

impl HiddenInputSchedule {
    fn new(now: Instant) -> Self {
        Self {
            active: false,
            deadline: now,
        }
    }

    fn update(&mut self, now: Instant, result: std::io::Result<bool>) -> Option<bool> {
        self.deadline = now + TERMIOS_POLL_INTERVAL;
        let active = match result {
            Ok(active) => active,
            Err(error) => {
                eprintln!(
                    "failed to inspect PTY hidden-input state; releasing secure input: {error}"
                );
                false
            }
        };
        if self.active == active {
            None
        } else {
            self.active = active;
            Some(active)
        }
    }
}

struct PendingOsc52Authorization {
    id: Osc52AuthorizationId,
    operation: Osc52Operation,
    deadline: Instant,
}

#[derive(Default)]
struct Osc52AuthorizationSchedule {
    next_id: u64,
    pending: Option<PendingOsc52Authorization>,
}

impl Osc52AuthorizationSchedule {
    fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|pending| pending.deadline)
    }

    fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    fn create(
        &mut self,
        operation: Osc52Operation,
        now: Instant,
    ) -> Option<Osc52AuthorizationRequest> {
        if self.pending.is_some() {
            return None;
        }
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let id = Osc52AuthorizationId::from_counter(self.next_id);
        let request = Osc52AuthorizationRequest {
            id,
            access: operation.access(),
            target: operation.target(),
            byte_len: operation.byte_len(),
        };
        self.pending = Some(PendingOsc52Authorization {
            id,
            operation,
            deadline: now + OSC52_AUTHORIZATION_TIMEOUT,
        });
        Some(request)
    }

    fn take(&mut self, id: Osc52AuthorizationId, now: Instant) -> Option<Osc52Operation> {
        let pending = self.pending.as_ref()?;
        if pending.id != id || now >= pending.deadline {
            return None;
        }
        self.pending.take().map(|pending| pending.operation)
    }

    fn expire(&mut self, now: Instant) -> Option<Osc52AuthorizationId> {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.pending.take().map(|pending| pending.id)
        } else {
            None
        }
    }

    fn cancel(&mut self) {
        self.pending = None;
    }
}

struct PendingPaste {
    id: PasteConfirmationId,
    payload: PreparedPaste,
    deadline: Instant,
}

#[derive(Default)]
struct PasteConfirmationSchedule {
    next_id: u64,
    pending: Option<PendingPaste>,
}

impl PasteConfirmationSchedule {
    fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|pending| pending.deadline)
    }

    fn create(
        &mut self,
        payload: PreparedPaste,
        now: Instant,
    ) -> Option<crate::terminal::PasteConfirmation> {
        if self.pending.is_some() {
            return None;
        }
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let id = PasteConfirmationId::new(self.next_id);
        let confirmation = payload.confirmation(id);
        self.pending = Some(PendingPaste {
            id,
            payload,
            deadline: now + PASTE_CONFIRMATION_TIMEOUT,
        });
        Some(confirmation)
    }

    fn take(&mut self, id: PasteConfirmationId, now: Instant) -> Option<PreparedPaste> {
        let pending = self.pending.take()?;
        if pending.id == id && now < pending.deadline {
            Some(pending.payload)
        } else {
            None
        }
    }

    fn expire(&mut self, now: Instant) -> bool {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.pending = None;
            true
        } else {
            false
        }
    }

    fn cancel(&mut self) {
        self.pending = None;
    }
}

#[derive(Default)]
struct SelectionAutoscrollSchedule {
    deadline: Option<Instant>,
    generation: PresentationGeneration,
}

impl SelectionAutoscrollSchedule {
    fn update(
        &mut self,
        now: Instant,
        interval: Option<Duration>,
        generation: PresentationGeneration,
    ) {
        self.deadline = interval.map(|interval| now + interval);
        self.generation = generation;
    }

    fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn take_due(&mut self, now: Instant) -> Option<PresentationGeneration> {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
            Some(self.generation)
        } else {
            None
        }
    }
}

#[derive(Default)]
struct HeldKeys(Vec<KeyInput>);

impl HeldKeys {
    fn route(&mut self, input: &KeyInput) {
        if input.is_text_input() || input.is_input_method_commit() {
            return;
        }
        match input.action {
            crate::terminal::key::KeyAction::Press | crate::terminal::key::KeyAction::Repeat => {
                if let Some(held) = self
                    .0
                    .iter_mut()
                    .find(|held| held.physical_key == input.physical_key)
                {
                    *held = input.clone();
                } else {
                    self.0.push(input.clone());
                }
            }
            crate::terminal::key::KeyAction::Release => self
                .0
                .retain(|held| held.physical_key != input.physical_key),
        }
    }

    fn take_releases(&mut self) -> Vec<KeyInput> {
        std::mem::take(&mut self.0)
            .into_iter()
            .map(|mut input| {
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
        mut pty: Box<dyn SessionPty>,
        context: TerminalWorkerContext,
        commands: CommandReceiver<Command>,
        reader_transport: ReaderTransport,
        mailboxes: TerminalWorkerMailboxes,
        events: async_channel::Sender<SessionEvent>,
        startup: StartupReporter,
    ) {
        let TerminalWorkerContext {
            initial_geometry,
            initial_directory,
            fallback_title,
            terminal_name,
        } = context;
        let TerminalWorkerMailboxes {
            resizes,
            find_queries,
        } = mailboxes;
        let ReaderTransport {
            commands: reader_commands,
            events: reader_events,
            event_rx: reader_event_rx,
        } = reader_transport;
        let reader = match pty.take_reader() {
            Ok(reader) => reader,
            Err(error) => {
                startup.failed(SessionStartupStage::Reader, error.to_string());
                return;
            }
        };

        let reader_thread = match spawn_reader(reader, reader_events, reader_commands) {
            Ok(thread) => thread,
            Err(error) => {
                startup.failed(
                    SessionStartupStage::ReaderThread,
                    format!("failed to start PTY reader thread: {error}"),
                );
                return;
            }
        };

        let local_hostname = crate::terminal::metadata::local_hostname();
        let emulator = match TerminalEmulator::new_with_metadata(
            initial_geometry,
            &initial_directory.to_string_lossy(),
            &fallback_title,
            local_hostname.as_deref(),
            terminal_name,
            Instant::now(),
        ) {
            Ok(emulator) => emulator,
            Err(error) => {
                startup.failed(SessionStartupStage::Emulator, error.to_string());
                drop(reader_event_rx);
                drop(pty);
                join_reader(reader_thread);
                return;
            }
        };

        let mut worker = Self {
            pty,
            emulator,
            commands,
            reader_events: reader_event_rx,
            reader_thread,
            events,
            resizes,
            find_queries,
            pending_command: None,
            terminal_input_focused: true,
            focus_reporting_enabled: false,
            held_keys: HeldKeys::default(),
            selection_autoscroll: SelectionAutoscrollSchedule::default(),
            paste_confirmations: PasteConfirmationSchedule::default(),
            osc52_filter: Osc52Filter::default(),
            osc52_policy: Osc52AuthorizationPolicy::default(),
            osc52_clipboard: native_osc52_clipboard(),
            osc52_authorization: Osc52AuthorizationSchedule::default(),
            deferred_osc52_effects: VecDeque::new(),
            deferred_output_chunks: VecDeque::new(),
            deferred_reader_ready: false,
            hidden_input: HiddenInputSchedule::new(Instant::now()),
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
        if let Some(command) = self.pending_command.take() {
            return Some(command);
        }
        if !self.osc52_authorization.is_pending()
            && (!self.deferred_osc52_effects.is_empty() || !self.deferred_output_chunks.is_empty())
        {
            return Some(Command::ResumeOsc52Output);
        }
        if !self.osc52_authorization.is_pending() && self.deferred_reader_ready {
            self.deferred_reader_ready = false;
            return Some(Command::ReaderReady);
        }

        loop {
            let synchronized_output_deadline = self.emulator.synchronized_output_deadline();
            let autoscroll_deadline = self.selection_autoscroll.deadline();
            let deadline = [
                synchronized_output_deadline,
                autoscroll_deadline,
                self.paste_confirmations.deadline(),
                self.osc52_authorization.deadline(),
                Some(self.hidden_input.deadline),
            ]
            .into_iter()
            .flatten()
            .min();
            let Some(deadline) = deadline else {
                return self.commands.recv().ok();
            };
            let timeout = deadline.saturating_duration_since(Instant::now());
            match self.commands.recv_timeout(timeout) {
                Ok(command) => return Some(command),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let now = Instant::now();
                    if let Some(generation) = self.selection_autoscroll.take_due(now) {
                        return Some(Command::SelectionAutoscrollTick(generation));
                    }
                    if self.paste_confirmations.expire(now) {
                        return Some(Command::PasteConfirmationExpired);
                    }
                    if let Some(id) = self.osc52_authorization.expire(now) {
                        return Some(Command::Osc52AuthorizationExpired(id));
                    }
                    if now >= self.hidden_input.deadline {
                        return Some(Command::PollHiddenInput);
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

    fn process_command(&mut self, command: Command) -> bool {
        match command {
            Command::Key(input) => self.process_key(input),
            Command::Focus(focused) => self.process_focus(focused),
            Command::ReaderReady if self.osc52_authorization.is_pending() => {
                self.deferred_reader_ready = true;
                true
            }
            Command::ReaderReady => self.process_reader_events(),
            Command::Resize => {
                let Some(geometry) = self.resizes.take() else {
                    return true;
                };
                let result = self
                    .pty
                    .resize(pty_size(geometry))
                    .map_err(|error| {
                        format!("failed to resize the macOS pseudo-terminal: {error:#}")
                    })
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
            Command::Wheel(input) => match self.emulator.wheel(input) {
                Ok(action) => self.apply_emulator_action(action),
                Err(message) => {
                    self.send_runtime_failure(message);
                    false
                }
            },
            Command::ScrollTo(offset_rows, generation) => {
                let action = self.emulator.scroll_to_at(offset_rows, generation);
                self.apply_emulator_action(action)
            }
            Command::FindQueryChanged => {
                let Some(update) = self.find_queries.take() else {
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
            Command::ResolveOsc52Authorization(id, decision) => {
                self.process_osc52_authorization(id, decision)
            }
            Command::Osc52AuthorizationExpired(id) => {
                let _ = self.send_terminal_event(SessionEvent::Osc52AuthorizationExpired(id));
                true
            }
            Command::ResumeOsc52Output => self.resume_osc52_output(),
            Command::SelectionCopy(reply) => {
                let _ = reply.send(
                    self.emulator
                        .selection_copy(SelectionCopyOptions::default())
                        .map_err(|_| SelectionCopyError::Formatting),
                );
                true
            }
            Command::SelectionAutoscrollTick(generation) => {
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
            Command::Shutdown => false,
            Command::PollHiddenInput => {
                if let Some(active) = self
                    .hidden_input
                    .update(Instant::now(), self.pty.hidden_input())
                {
                    send_session_event(&self.events, SessionEvent::HiddenInputChanged(active))
                } else {
                    true
                }
            }
        }
    }

    fn process_paste_request(
        &mut self,
        text: String,
        reply: async_channel::Sender<Result<PasteRequestOutcome, String>>,
    ) -> bool {
        if !self.terminal_input_focused {
            let _ = reply.try_send(Ok(PasteRequestOutcome::Rejected(
                PasteRejection::TerminalUnfocused,
            )));
            return true;
        }
        let payload = match PreparedPaste::prepare(text) {
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
                .paste_confirmations
                .create(payload, Instant::now())
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
        let Some(payload) = self.paste_confirmations.take(id, Instant::now()) else {
            let _ = reply.try_send(Ok(PasteResolution::Stale));
            return true;
        };
        if decision == PasteDecision::Cancel || !self.terminal_input_focused {
            let _ = reply.try_send(Ok(PasteResolution::Cancelled));
            return true;
        }

        match self.emulator.paste(payload.into_text()) {
            Ok(action) => {
                let applied = self.apply_emulator_action(action);
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
                let applied = self.apply_emulator_action(action);
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
                self.selection_autoscroll.update(
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
            if !self.flush_synchronized_output() {
                return false;
            }
            let event = classify_reader_stop(
                read_error,
                self.pty.wait_for_child(FINAL_CHILD_WAIT_TIMEOUT),
            );
            self.emulator.mark_metadata_stale();
            if !self.publish_screen() {
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
                Ok(ReaderEvent::Output(bytes)) => batch.chunks.push(bytes),
                Ok(ReaderEvent::Stopped(read_error)) => {
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
        self.process_output_queue(chunks.into())
    }

    fn process_output_queue(&mut self, mut chunks: VecDeque<Vec<u8>>) -> bool {
        let received_output = !chunks.is_empty() || !self.deferred_osc52_effects.is_empty();
        let mut focus_reports = Vec::new();
        let mut effects = mem::take(&mut self.deferred_osc52_effects);

        loop {
            if effects.is_empty() {
                let Some(bytes) = chunks.pop_front() else {
                    break;
                };
                effects.extend(self.osc52_filter.feed(&bytes));
            }
            let Some(effect) = effects.pop_front() else {
                continue;
            };
            match effect {
                Osc52Effect::Terminal(bytes) => {
                    if !self.feed_terminal_output(&bytes, &mut focus_reports) {
                        return false;
                    }
                }
                Osc52Effect::Rejected(_rejection) => {}
                Osc52Effect::Operation(operation) => {
                    if !self.flush_ordered_terminal_replies(&mut focus_reports) {
                        return false;
                    }
                    match self.osc52_policy.for_access(operation.access()) {
                        Osc52AccessPolicy::Deny => {}
                        Osc52AccessPolicy::Allow => {
                            if !self.perform_osc52_operation(operation) {
                                return false;
                            }
                        }
                        Osc52AccessPolicy::Ask => {
                            let Some(request) =
                                self.osc52_authorization.create(operation, Instant::now())
                            else {
                                continue;
                            };
                            self.deferred_osc52_effects = effects;
                            self.deferred_output_chunks = chunks;
                            if !self.send_terminal_event(SessionEvent::Osc52Authorization(request))
                            {
                                return false;
                            }
                            return !received_output || self.publish_screen();
                        }
                    }
                }
            }
        }

        if received_output
            && (!self.flush_ordered_terminal_replies(&mut focus_reports) || !self.publish_screen())
        {
            return false;
        }
        true
    }

    fn feed_terminal_output(&mut self, bytes: &[u8], focus_reports: &mut Vec<u8>) -> bool {
        self.emulator.feed(bytes);
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

    fn process_osc52_authorization(
        &mut self,
        id: Osc52AuthorizationId,
        decision: Osc52AuthorizationDecision,
    ) -> bool {
        let Some(operation) = self.osc52_authorization.take(id, Instant::now()) else {
            return true;
        };
        if decision == Osc52AuthorizationDecision::Allow && !self.perform_osc52_operation(operation)
        {
            return false;
        }
        self.resume_osc52_output()
    }

    fn resume_osc52_output(&mut self) -> bool {
        let chunks = mem::take(&mut self.deferred_output_chunks);
        self.process_output_queue(chunks)
    }

    fn perform_osc52_operation(&mut self, operation: Osc52Operation) -> bool {
        match &operation {
            Osc52Operation::Write { target, text } => {
                let _ = self.osc52_clipboard.write(*target, text);
                true
            }
            Osc52Operation::Read { target, .. } => {
                let Ok(text) = self.osc52_clipboard.read(*target) else {
                    return true;
                };
                if text.len() > MAX_OSC52_CONTENT_BYTES {
                    return true;
                }
                operation
                    .read_reply(&text)
                    .is_none_or(|reply| self.write_pty(&reply))
            }
        }
    }

    fn process_key(&mut self, input: KeyInput) -> bool {
        self.held_keys.route(&input);
        match self.emulator.key(input) {
            Ok(action) => self.apply_emulator_action(action),
            Err(message) => {
                self.send_runtime_failure(message);
                false
            }
        }
    }

    fn process_focus(&mut self, focused: bool) -> bool {
        if self.terminal_input_focused == focused {
            return true;
        }

        if !focused {
            self.paste_confirmations.cancel();
            self.osc52_authorization.cancel();
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
            && (!action.screen_changed || self.publish_screen())
    }

    fn write_pending_pty_responses(&mut self) -> bool {
        let responses = self.emulator.take_pty_responses();
        responses.is_empty() || self.write_pty(&responses)
    }

    fn write_pty(&mut self, bytes: &[u8]) -> bool {
        if let Err(error) = self.pty.write_all(bytes).and_then(|()| self.pty.flush()) {
            let _ = self.send_runtime_failure(format!("failed to write to the shell PTY: {error}"));
            return false;
        }
        true
    }

    fn publish_screen(&mut self) -> bool {
        match self.emulator.snapshot() {
            Ok(Some(snapshot)) => self
                .events
                .force_send(SessionEvent::Screen(snapshot))
                .is_ok(),
            Ok(None) => true,
            Err(error) => {
                self.send_runtime_failure(format!(
                    "failed to produce terminal screen snapshot: {error}"
                ));
                false
            }
        }
    }

    fn release_synchronized_output_if_due(&mut self, now: Instant) -> bool {
        match self.emulator.expire_synchronized_output(now) {
            Ok(true) => self.publish_screen(),
            Ok(false) => true,
            Err(error) => {
                self.send_runtime_failure(format!(
                    "failed to release synchronized terminal output: {error}"
                ));
                false
            }
        }
    }

    fn flush_synchronized_output(&mut self) -> bool {
        match self.emulator.end_synchronized_output() {
            Ok(true) => self.publish_screen(),
            Ok(false) => true,
            Err(error) => {
                self.send_runtime_failure(format!(
                    "failed to flush synchronized terminal output: {error}"
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
            pty,
            emulator: _emulator,
            commands: _commands,
            reader_events,
            reader_thread,
            events: _events,
            resizes: _resizes,
            pending_command: _pending_command,
            terminal_input_focused: _terminal_input_focused,
            focus_reporting_enabled: _focus_reporting_enabled,
            held_keys: _held_keys,
            selection_autoscroll: _selection_autoscroll,
            paste_confirmations: _paste_confirmations,
            ..
        } = self;
        // SpawnedPty's Drop terminates and reaps a live shell for the native Adapter.
        drop(reader_events);
        drop(pty);
        join_reader(reader_thread);
    }
}

fn shell_fallback_title() -> String {
    let shell = user_shell();
    Path::new(&shell)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(&shell)
        .to_owned()
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    events: mpsc::SyncSender<ReaderEvent>,
    commands: CommandSender<Command>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("spaceterm-pty-reader".to_owned())
        .spawn(move || {
            let mut buffer = [0_u8; PTY_READ_BUFFER_SIZE];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        let _ = send_reader_event(ReaderEvent::Stopped(None), &events, &commands);
                        break;
                    }
                    Ok(read) => {
                        if !send_reader_event(
                            ReaderEvent::Output(buffer[..read].to_vec()),
                            &events,
                            &commands,
                        ) {
                            break;
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(error) => {
                        let _ = send_reader_event(
                            ReaderEvent::Stopped(Some(error.to_string())),
                            &events,
                            &commands,
                        );
                        break;
                    }
                }
            }
        })
}

fn send_reader_event(
    event: ReaderEvent,
    events: &mpsc::SyncSender<ReaderEvent>,
    commands: &CommandSender<Command>,
) -> bool {
    // ReaderReady is the publication point that orders this private event against control
    // Commands. Controls may overtake a producer still blocked on bounded output capacity.
    events.send(event).is_ok() && commands.send(Command::ReaderReady).is_ok()
}

fn classify_reader_stop(
    read_error: Option<String>,
    wait_result: std::io::Result<ShellExit>,
) -> SessionEvent {
    match (read_error, wait_result) {
        (None, Ok(exit)) => SessionEvent::Exited(classify_shell_exit(exit)),
        (Some(read_error), Ok(exit)) => SessionEvent::Failed(SessionFailure::PtyRead {
            read_error,
            exit_status: classify_shell_exit(exit).to_string(),
        }),
        (read_error, Err(wait_error)) => SessionEvent::Failed(SessionFailure::ShellWait {
            read_error,
            wait_error: wait_error.to_string(),
        }),
    }
}

fn classify_shell_exit(exit: ShellExit) -> SessionExit {
    match exit.shutdown {
        ShutdownDisposition::Graceful => SessionExit::GracefulShutdown,
        ShutdownDisposition::Forced => SessionExit::ForcedShutdown,
        ShutdownDisposition::NotRequested => match exit.status.signal() {
            Some(signal) => SessionExit::Signal(signal.to_owned()),
            None if exit.status.success() => SessionExit::Success,
            None => SessionExit::ExitCode(exit.status.exit_code()),
        },
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

fn join_reader(reader: JoinHandle<()>) {
    if reader.join().is_err() {
        eprintln!("PTY reader thread panicked");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn hidden_input_polling_emits_only_transitions_and_fails_closed() {
        let start = Instant::now();
        let mut schedule = HiddenInputSchedule::new(start);

        assert_eq!(schedule.update(start, Ok(false)), None);
        assert_eq!(schedule.update(start, Ok(true)), Some(true));
        assert_eq!(schedule.update(start, Ok(true)), None);
        assert_eq!(
            schedule.update(start, Err(io::Error::other("descriptor closed"))),
            Some(false)
        );
        assert_eq!(schedule.deadline, start + TERMIOS_POLL_INTERVAL);
    }
    use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize};
    use crate::terminal::key::{KeyAction, PhysicalKey};

    struct JoinedRealPtySession(TerminalSession);

    impl std::ops::Deref for JoinedRealPtySession {
        type Target = TerminalSession;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl Drop for JoinedRealPtySession {
        fn drop(&mut self) {
            self.0.shutdown_and_join();
        }
    }

    fn geometry(cols: u16, rows: u16, cell_width: f32, cell_height: f32) -> TerminalGeometry {
        TerminalGeometry::from_grid(
            CellGridSize::new(cols, rows),
            LogicalCellSize::new(cell_width, cell_height),
            BackingScale::ONE,
        )
    }

    fn test_geometry() -> TerminalGeometry {
        geometry(80, 24, 8.0, 20.0)
    }

    fn text_key(action: KeyAction) -> KeyInput {
        KeyInput {
            action,
            physical_key: PhysicalKey::A,
            native_key_code: Some(0),
            logical_key: "a".to_owned(),
            text: Some("a".to_owned()),
            unshifted_codepoint: Some('a'),
            modifiers: InputModifiers::default(),
            consumed_modifiers: InputModifiers::default(),
            option_as_alt: OptionAsAltPolicy::default(),
        }
    }

    fn modifier_key(action: KeyAction) -> KeyInput {
        KeyInput {
            action,
            physical_key: PhysicalKey::ShiftLeft,
            native_key_code: Some(56),
            logical_key: "shift".to_owned(),
            text: None,
            unshifted_codepoint: None,
            modifiers: InputModifiers {
                shift: action != KeyAction::Release,
                ..InputModifiers::default()
            },
            consumed_modifiers: InputModifiers::default(),
            option_as_alt: OptionAsAltPolicy::default(),
        }
    }

    #[test]
    fn input_method_commits_never_enter_the_held_key_set() {
        let mut held = HeldKeys::default();

        held.route(&KeyInput::input_method_commit("한"));

        assert!(held.take_releases().is_empty());
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum LifecycleStep {
        TerminationRequested,
        PtyDropped,
    }

    #[derive(Clone, Debug, Default)]
    struct ScriptedPtyState {
        take_reader_calls: usize,
        write_attempts: usize,
        written: Vec<u8>,
        flushes: usize,
        resizes: Vec<PtySize>,
        waits: usize,
        terminations: usize,
        pty_drops: usize,
        reader_drops: usize,
        terminator_drops: usize,
        lifecycle: Vec<LifecycleStep>,
    }

    #[derive(Clone, Default)]
    struct ScriptedPtyRecords {
        state: Arc<(Mutex<ScriptedPtyState>, Condvar)>,
    }

    impl ScriptedPtyRecords {
        fn update(&self, update: impl FnOnce(&mut ScriptedPtyState)) {
            let (state, changed) = &*self.state;
            update(&mut state.lock().unwrap());
            changed.notify_all();
        }

        fn snapshot(&self) -> ScriptedPtyState {
            self.state.0.lock().unwrap().clone()
        }

        fn wait_for(
            &self,
            description: &str,
            predicate: impl Fn(&ScriptedPtyState) -> bool,
        ) -> ScriptedPtyState {
            let (state, changed) = &*self.state;
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut state = state.lock().unwrap();
            while !predicate(&state) {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "timed out waiting for {description}");
                let (next_state, timeout) = changed.wait_timeout(state, remaining).unwrap();
                state = next_state;
                assert!(
                    !timeout.timed_out() || predicate(&state),
                    "timed out waiting for {description}"
                );
            }
            state.clone()
        }
    }

    enum ReaderStep {
        Bytes(Vec<u8>),
        Error(String),
        Eof,
    }

    struct ScriptedReader {
        steps: mpsc::Receiver<ReaderStep>,
        pending: VecDeque<u8>,
        records: ScriptedPtyRecords,
    }

    impl Read for ScriptedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            while self.pending.is_empty() {
                match self.steps.recv() {
                    Ok(ReaderStep::Bytes(bytes)) => self.pending.extend(bytes),
                    Ok(ReaderStep::Error(message)) => return Err(io::Error::other(message)),
                    Ok(ReaderStep::Eof) | Err(_) => return Ok(0),
                }
            }

            let read = buffer.len().min(self.pending.len());
            for destination in &mut buffer[..read] {
                let Some(byte) = self.pending.pop_front() else {
                    unreachable!("the scripted reader length was checked before draining")
                };
                *destination = byte;
            }
            Ok(read)
        }
    }

    impl Drop for ScriptedReader {
        fn drop(&mut self) {
            self.records.update(|state| state.reader_drops += 1);
        }
    }

    struct ScriptedPtyOptions {
        reader_error: Option<String>,
        resize_error: Option<String>,
        write_error: Option<String>,
        wait_error: Option<String>,
        wait_times_out: bool,
        exit_code: u32,
        termination_error: Option<String>,
        termination_releases_reader: bool,
    }

    impl Default for ScriptedPtyOptions {
        fn default() -> Self {
            Self {
                reader_error: None,
                resize_error: None,
                write_error: None,
                wait_error: None,
                wait_times_out: false,
                exit_code: 0,
                termination_error: None,
                termination_releases_reader: true,
            }
        }
    }

    struct ScriptedPty {
        reader: Option<Box<dyn Read + Send>>,
        records: ScriptedPtyRecords,
        reader_error: Option<String>,
        resize_error: Option<String>,
        write_error: Option<String>,
        wait_error: Option<String>,
        wait_times_out: bool,
        exit_code: u32,
    }

    impl Write for ScriptedPty {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.records.update(|state| state.write_attempts += 1);
            if let Some(message) = &self.write_error {
                return Err(io::Error::new(ErrorKind::BrokenPipe, message.clone()));
            }
            self.records
                .update(|state| state.written.extend_from_slice(bytes));
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.records.update(|state| state.flushes += 1);
            Ok(())
        }
    }

    impl SessionPty for ScriptedPty {
        fn take_reader(&mut self) -> io::Result<Box<dyn Read + Send>> {
            self.records.update(|state| state.take_reader_calls += 1);
            if let Some(message) = self.reader_error.take() {
                return Err(io::Error::other(message));
            }
            self.reader.take().ok_or_else(|| {
                io::Error::new(ErrorKind::NotFound, "scripted PTY reader was already taken")
            })
        }

        fn resize(&self, size: PtySize) -> Result<(), AnyError> {
            self.records.update(|state| state.resizes.push(size));
            match &self.resize_error {
                Some(message) => Err(AnyError::msg(message.clone())),
                None => Ok(()),
            }
        }

        fn wait_for_child(&mut self, timeout: Duration) -> io::Result<ShellExit> {
            self.records.update(|state| state.waits += 1);
            if self.wait_times_out {
                return Err(io::Error::new(
                    ErrorKind::TimedOut,
                    format!(
                        "timed out after {} ms waiting for the scripted shell process to exit",
                        timeout.as_millis()
                    ),
                ));
            }
            match &self.wait_error {
                Some(message) => Err(io::Error::other(message.clone())),
                None => Ok(ShellExit {
                    status: ExitStatus::with_exit_code(self.exit_code),
                    shutdown: ShutdownDisposition::NotRequested,
                }),
            }
        }
    }

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    struct Osc52ClipboardState {
        read_text: String,
        reads: Vec<crate::terminal::Osc52Target>,
        writes: Vec<(crate::terminal::Osc52Target, String)>,
    }

    #[derive(Clone, Default)]
    struct RecordingOsc52Clipboard {
        state: Arc<Mutex<Osc52ClipboardState>>,
    }

    impl RecordingOsc52Clipboard {
        fn with_read_text(text: &str) -> Self {
            Self {
                state: Arc::new(Mutex::new(Osc52ClipboardState {
                    read_text: text.to_owned(),
                    ..Osc52ClipboardState::default()
                })),
            }
        }

        fn snapshot(&self) -> Osc52ClipboardState {
            self.state.lock().unwrap().clone()
        }
    }

    impl Osc52Clipboard for RecordingOsc52Clipboard {
        fn read(
            &mut self,
            target: crate::terminal::Osc52Target,
        ) -> Result<String, Osc52ClipboardError> {
            let mut state = self.state.lock().unwrap();
            state.reads.push(target);
            Ok(state.read_text.clone())
        }

        fn write(
            &mut self,
            target: crate::terminal::Osc52Target,
            text: &str,
        ) -> Result<(), Osc52ClipboardError> {
            self.state
                .lock()
                .unwrap()
                .writes
                .push((target, text.to_owned()));
            Ok(())
        }
    }

    fn osc52_worker(
        policy: Osc52AuthorizationPolicy,
        clipboard: RecordingOsc52Clipboard,
    ) -> (
        TerminalWorker,
        async_channel::Receiver<SessionEvent>,
        ScriptedPtyRecords,
    ) {
        let (_command_tx, commands) = mpsc::channel();
        let (_reader_events, reader_event_rx) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
        let records = ScriptedPtyRecords::default();
        let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
        let worker = TerminalWorker {
            pty: Box::new(ScriptedPty {
                reader: None,
                records: records.clone(),
                reader_error: None,
                resize_error: None,
                write_error: None,
                wait_error: None,
                wait_times_out: false,
                exit_code: 0,
            }),
            emulator: TerminalEmulator::new(test_geometry()).unwrap(),
            commands,
            reader_events: reader_event_rx,
            reader_thread: thread::spawn(|| {}),
            events,
            resizes: ResizeMailbox::default(),
            find_queries: FindQueryMailbox::default(),
            pending_command: None,
            terminal_input_focused: true,
            focus_reporting_enabled: false,
            held_keys: HeldKeys::default(),
            selection_autoscroll: SelectionAutoscrollSchedule::default(),
            paste_confirmations: PasteConfirmationSchedule::default(),
            osc52_filter: Osc52Filter::default(),
            osc52_policy: policy,
            osc52_clipboard: Box::new(clipboard),
            osc52_authorization: Osc52AuthorizationSchedule::default(),
            deferred_osc52_effects: VecDeque::new(),
            deferred_output_chunks: VecDeque::new(),
            deferred_reader_ready: false,
            hidden_input: HiddenInputSchedule::new(Instant::now()),
        };
        (worker, receiver, records)
    }

    #[test]
    fn osc52_denial_is_quiet_and_never_accesses_the_clipboard() {
        let clipboard = RecordingOsc52Clipboard::with_read_text("secret");
        let (mut worker, _events, records) =
            osc52_worker(Osc52AuthorizationPolicy::default(), clipboard.clone());

        assert!(
            worker.process_output_chunks(vec![b"\x1b]52;c;?\x07\x1b]52;c;d3JpdGU=\x07".to_vec()])
        );

        assert_eq!(
            clipboard.snapshot(),
            Osc52ClipboardState {
                read_text: "secret".to_owned(),
                ..Osc52ClipboardState::default()
            }
        );
        assert!(records.snapshot().written.is_empty());
    }

    #[test]
    fn allowed_osc52_read_reply_stays_ordered_between_terminal_protocol_replies() {
        let clipboard = RecordingOsc52Clipboard::with_read_text("hello");
        let (mut worker, _events, records) = osc52_worker(
            Osc52AuthorizationPolicy {
                read: Osc52AccessPolicy::Allow,
                write: Osc52AccessPolicy::Allow,
            },
            clipboard.clone(),
        );

        assert!(worker.process_output_chunks(vec![b"\x1b[6n\x1b]52;c;?\x07\x1b[6n".to_vec()]));

        assert_eq!(
            clipboard.snapshot().reads,
            [crate::terminal::Osc52Target::Standard]
        );
        assert_eq!(
            records.snapshot().written,
            b"\x1b[1;1R\x1b]52;c;aGVsbG8=\x07\x1b[1;1R"
        );
    }

    #[test]
    fn asked_osc52_write_retains_only_metadata_and_defers_later_output() {
        let clipboard = RecordingOsc52Clipboard::default();
        let (mut worker, events, records) = osc52_worker(
            Osc52AuthorizationPolicy {
                read: Osc52AccessPolicy::Ask,
                write: Osc52AccessPolicy::Ask,
            },
            clipboard.clone(),
        );

        assert!(worker.process_output_chunks(vec![b"\x1b]52;c;c2VjcmV0\x07\x1b[6n".to_vec()]));
        let SessionEvent::Osc52Authorization(request) = events.try_recv().unwrap() else {
            panic!("ask policy must publish bounded authorization metadata")
        };
        assert_eq!(
            (request.access, request.target, request.byte_len),
            (
                crate::terminal::Osc52Access::Write,
                crate::terminal::Osc52Target::Standard,
                6,
            )
        );
        assert!(clipboard.snapshot().writes.is_empty());
        assert!(records.snapshot().written.is_empty());
        assert!(worker.osc52_authorization.is_pending());

        assert!(worker.process_osc52_authorization(request.id, Osc52AuthorizationDecision::Allow,));
        assert_eq!(
            clipboard.snapshot().writes,
            [(crate::terminal::Osc52Target::Standard, "secret".to_owned())]
        );
        assert_eq!(records.snapshot().written, b"\x1b[1;1R");
    }

    #[test]
    fn osc52_authorization_allows_one_pending_request_and_rejects_stale_ids() {
        let now = Instant::now();
        let mut schedule = Osc52AuthorizationSchedule::default();
        let operation = Osc52Operation::Read {
            target: crate::terminal::Osc52Target::Standard,
            terminator: crate::terminal::osc52::Osc52Terminator::StringTerminator,
        };
        let request = schedule.create(operation.clone(), now).unwrap();

        assert!(schedule.create(operation, now).is_none());
        assert_eq!(schedule.take(Osc52AuthorizationId::new(999), now), None);
        assert!(schedule.is_pending());
        assert_eq!(
            schedule.expire(now + OSC52_AUTHORIZATION_TIMEOUT),
            Some(request.id)
        );
        assert!(!schedule.is_pending());
    }

    impl Drop for ScriptedPty {
        fn drop(&mut self) {
            self.records.update(|state| {
                state.pty_drops += 1;
                state.lifecycle.push(LifecycleStep::PtyDropped);
            });
        }
    }

    struct ScriptedPtyTerminator {
        records: ScriptedPtyRecords,
        reader_steps: mpsc::Sender<ReaderStep>,
        error: Option<String>,
        releases_reader: bool,
    }

    impl SessionPtyTerminator for ScriptedPtyTerminator {
        fn terminate(&self) -> io::Result<()> {
            self.records.update(|state| {
                state.terminations += 1;
                state.lifecycle.push(LifecycleStep::TerminationRequested);
            });
            if self.releases_reader {
                let _ = self.reader_steps.send(ReaderStep::Eof);
            }
            match &self.error {
                Some(message) => Err(io::Error::other(message.clone())),
                None => Ok(()),
            }
        }
    }

    impl Drop for ScriptedPtyTerminator {
        fn drop(&mut self) {
            self.records.update(|state| state.terminator_drops += 1);
        }
    }

    type ScriptedStart =
        Result<(TerminalSession, async_channel::Receiver<SessionEvent>), SessionError>;

    fn start_scripted_session(
        options: ScriptedPtyOptions,
    ) -> (ScriptedStart, mpsc::Sender<ReaderStep>, ScriptedPtyRecords) {
        let records = ScriptedPtyRecords::default();
        let records_for_pty = records.clone();
        let records_for_terminator = records.clone();
        let (reader_steps, steps) = mpsc::channel();
        let terminator_steps = reader_steps.clone();
        let ScriptedPtyOptions {
            reader_error,
            resize_error,
            write_error,
            wait_error,
            wait_times_out,
            exit_code,
            termination_error,
            termination_releases_reader,
        } = options;

        let result = TerminalSession::start_with(
            test_geometry(),
            Path::new("/scripted"),
            move |size, working_directory| {
                assert_eq!(size, pty_size(test_geometry()));
                assert_eq!(working_directory, Path::new("/scripted"));
                Ok(StartedSessionPty {
                    pty: Box::new(ScriptedPty {
                        reader: Some(Box::new(ScriptedReader {
                            steps,
                            pending: VecDeque::new(),
                            records: records_for_pty.clone(),
                        })),
                        records: records_for_pty,
                        reader_error,
                        resize_error,
                        write_error,
                        wait_error,
                        wait_times_out,
                        exit_code,
                    }),
                    terminator: Box::new(ScriptedPtyTerminator {
                        records: records_for_terminator,
                        reader_steps: terminator_steps,
                        error: termination_error,
                        releases_reader: termination_releases_reader,
                    }),
                })
            },
        );

        (result, reader_steps, records)
    }

    fn receive_event(
        events: &async_channel::Receiver<SessionEvent>,
        description: &str,
        predicate: impl Fn(&SessionEvent) -> bool + Send + 'static,
    ) -> SessionEvent {
        let events = events.clone();
        let (matched, result) = mpsc::sync_channel(1);
        let waiter = thread::spawn(move || {
            loop {
                match events.recv_blocking() {
                    Ok(event) if predicate(&event) => {
                        let _ = matched.send(Some(event));
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => {
                        let _ = matched.send(None);
                        break;
                    }
                }
            }
        });

        match result.recv_timeout(Duration::from_secs(1)) {
            Ok(Some(event)) => {
                waiter.join().unwrap();
                event
            }
            Ok(None) => {
                waiter.join().unwrap();
                panic!("session events closed while waiting for {description}")
            }
            Err(error) => {
                drop(waiter);
                panic!("timed out waiting for {description}: {error}")
            }
        }
    }

    fn screen_text(screen: &ScreenSnapshot) -> String {
        screen
            .rows
            .iter()
            .flat_map(|row| row.iter())
            .map(|cell| cell.text.as_str())
            .collect()
    }

    #[test]
    fn shell_exit_should_preserve_normal_signal_and_shutdown_classifications() {
        let classify = |status, shutdown| classify_shell_exit(ShellExit { status, shutdown });

        assert_eq!(
            classify(
                ExitStatus::with_exit_code(0),
                ShutdownDisposition::NotRequested
            ),
            SessionExit::Success
        );
        assert_eq!(
            classify(
                ExitStatus::with_exit_code(17),
                ShutdownDisposition::NotRequested
            ),
            SessionExit::ExitCode(17)
        );
        assert_eq!(
            classify(
                ExitStatus::with_signal("Hangup"),
                ShutdownDisposition::NotRequested
            ),
            SessionExit::Signal("Hangup".to_owned())
        );
        assert_eq!(
            classify(
                ExitStatus::with_signal("Hangup"),
                ShutdownDisposition::Graceful
            ),
            SessionExit::GracefulShutdown
        );
        assert_eq!(
            classify(
                ExitStatus::with_signal("Killed"),
                ShutdownDisposition::Forced
            ),
            SessionExit::ForcedShutdown
        );
    }

    #[test]
    fn scripted_output_and_exit_should_preserve_the_latest_screen_before_the_final_event() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, events) = result.unwrap();

        for index in 0..32 {
            reader_steps
                .send(ReaderStep::Bytes(
                    format!("bounded line {index}\r\n").into_bytes(),
                ))
                .unwrap();
        }
        reader_steps.send(ReaderStep::Eof).unwrap();
        records.wait_for("the scripted worker to finish", |state| {
            state.pty_drops == 1
        });

        assert_eq!(events.len(), 2);
        let first = events.try_recv().unwrap();
        let second = events.try_recv().unwrap();
        let result = match (first, second) {
            (SessionEvent::Screen(screen), SessionEvent::Exited(status)) => (
                screen_text(&screen).contains("bounded line 31"),
                status == SessionExit::Success,
                events.try_recv().is_err(),
            ),
            events => panic!("expected the latest Screen followed by Exited, got {events:?}"),
        };

        assert_eq!(result, (true, true, true));
        session.shutdown();
    }

    #[test]
    fn shell_exit_should_flush_a_pending_synchronized_output_transaction() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, events) = result.unwrap();

        reader_steps
            .send(ReaderStep::Bytes(b"\x1b[?2026hfinal output".to_vec()))
            .unwrap();
        reader_steps.send(ReaderStep::Eof).unwrap();
        records.wait_for("the synchronized-output worker to finish", |state| {
            state.pty_drops == 1
        });

        assert_eq!(events.len(), 2);
        let screen = events.try_recv().unwrap();
        let exited = events.try_recv().unwrap();
        assert!(matches!(
            screen,
            SessionEvent::Screen(screen) if screen_text(&screen).contains("final output")
        ));
        assert!(matches!(exited, SessionEvent::Exited(SessionExit::Success)));
        session.shutdown();
    }

    #[test]
    fn session_snapshots_reuse_rows_unchanged_by_later_output() {
        let (result, reader_steps, _records) =
            start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, events) = result.unwrap();

        reader_steps
            .send(ReaderStep::Bytes(b"first row".to_vec()))
            .unwrap();
        let first = receive_event(
            &events,
            "the first row snapshot",
            |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains("first row")),
        );
        reader_steps
            .send(ReaderStep::Bytes(b"\r\nsecond row".to_vec()))
            .unwrap();
        let second = receive_event(
            &events,
            "the second row snapshot",
            |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains("second row")),
        );

        let (SessionEvent::Screen(first), SessionEvent::Screen(second)) = (first, second) else {
            unreachable!("the event predicates accept only terminal screens")
        };
        assert!(Arc::ptr_eq(&first.rows[0], &second.rows[0]));
        assert!(!Arc::ptr_eq(&first.rows[1], &second.rows[1]));

        session.shutdown();
    }

    #[test]
    fn session_failure_display_should_explain_each_classification() {
        let failures = [
            SessionFailure::Startup {
                stage: SessionStartupStage::Pty,
                message: "open unavailable".to_owned(),
            },
            SessionFailure::Runtime("write unavailable".to_owned()),
            SessionFailure::PtyRead {
                read_error: "read unavailable".to_owned(),
                exit_status: "exit code 7".to_owned(),
            },
            SessionFailure::ShellWait {
                read_error: Some("read unavailable".to_owned()),
                wait_error: "wait unavailable".to_owned(),
            },
            SessionFailure::ShellWait {
                read_error: None,
                wait_error: "wait unavailable".to_owned(),
            },
        ];

        let statuses = failures.each_ref().map(ToString::to_string);

        assert_eq!(
            statuses,
            [
                "Terminal Session startup failed during PTY creation: open unavailable",
                "Terminal runtime failed: write unavailable",
                "Shell output failed: read unavailable; shell exited (exit code 7)",
                "Shell output failed: read unavailable; waiting for the shell also failed: wait unavailable",
                "Shell output ended, but waiting for the shell failed: wait unavailable",
            ]
        );
        let _: &dyn std::error::Error = &failures[0];
    }

    #[test]
    fn deferred_start_should_return_before_pty_spawn_and_publish_a_typed_failure() {
        let (spawn_entered, entered) = mpsc::sync_channel(1);
        let (release_spawn, release) = mpsc::sync_channel(1);
        let started_at = Instant::now();

        let (mut session, events) = TerminalSession::start_deferred_with(
            test_geometry(),
            Path::new("/scripted"),
            move |size, working_directory| {
                assert_eq!(size, pty_size(test_geometry()));
                assert_eq!(working_directory, Path::new("/scripted"));
                spawn_entered.send(()).unwrap();
                release.recv().unwrap();
                Err(PtyError::Open(AnyError::msg("scripted spawn unavailable")))
            },
        )
        .unwrap();

        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            started_at.elapsed() < Duration::from_millis(250),
            "Terminal Session start waited for PTY spawn"
        );
        release_spawn.send(()).unwrap();
        let event = receive_event(&events, "the deferred PTY spawn failure", |event| {
            matches!(
                event,
                SessionEvent::Failed(SessionFailure::Startup {
                    stage: SessionStartupStage::Pty,
                    ..
                })
            )
        });

        let SessionEvent::Failed(SessionFailure::Startup { message, .. }) = event else {
            unreachable!("the event predicate accepts only typed startup failures")
        };
        assert!(message.contains("scripted spawn unavailable"));
        session.shutdown();
    }

    #[test]
    fn native_factory_should_report_pty_spawn_failures_through_session_events() {
        let StartedTerminalSession {
            handle: session,
            events,
        } = NativeTerminalSessionFactory
            .start(
                test_geometry(),
                Path::new("/private/tmp/spaceterm-missing-session-workspace"),
            )
            .unwrap();

        let event = receive_event(&events, "the native PTY startup failure", |event| {
            matches!(
                event,
                SessionEvent::Failed(SessionFailure::Startup {
                    stage: SessionStartupStage::Pty,
                    ..
                })
            )
        });

        let SessionEvent::Failed(SessionFailure::Startup { message, .. }) = event else {
            unreachable!("the event predicate accepts only typed PTY startup failures")
        };
        assert!(message.contains("Workspace working directory"));
        drop(session);
    }

    #[test]
    fn reader_acquisition_failure_should_fail_startup_and_drop_the_pty_once() {
        let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
            reader_error: Some("reader unavailable".to_owned()),
            ..ScriptedPtyOptions::default()
        });

        let error = match result {
            Ok(_) => panic!("a missing PTY reader must fail startup"),
            Err(error) => error,
        };
        let state = records.snapshot();

        assert!(matches!(
            error,
            SessionError::EmulatorStartup(message) if message == "reader unavailable"
        ));
        assert_eq!(
            (
                state.take_reader_calls,
                state.pty_drops,
                state.terminations,
                state.terminator_drops,
            ),
            (1, 1, 0, 1)
        );
    }

    #[test]
    fn scripted_output_should_reach_the_terminal_screen() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, events) = result.unwrap();

        reader_steps
            .send(ReaderStep::Bytes(b"scripted output".to_vec()))
            .unwrap();
        let event = receive_event(
            &events,
            "scripted terminal output",
            |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains("scripted output")),
        );

        let SessionEvent::Screen(screen) = event else {
            unreachable!("the event predicate accepts only terminal screens")
        };
        assert!(screen_text(&screen).contains("scripted output"));

        session.shutdown();
        let state = records.wait_for("the scripted session owners to be released", |state| {
            state.pty_drops == 1 && state.reader_drops == 1
        });
        assert_eq!(
            (
                state.terminations,
                state.pty_drops,
                state.reader_drops,
                state.terminator_drops,
            ),
            (1, 1, 1, 1)
        );
    }

    #[test]
    fn bounded_output_should_preserve_the_control_lane_and_unblock_the_producer() {
        let (command_tx, commands) = mpsc::channel();
        let (reader_events, reader_event_rx) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
        let (attempted, attempts) = mpsc::channel();
        let (completed, completions) = mpsc::channel();
        let producer_commands = command_tx.clone();

        let producer = thread::spawn(move || {
            for index in 0..=PTY_OUTPUT_QUEUE_CAPACITY {
                attempted.send(index).unwrap();
                let sent = send_reader_event(
                    ReaderEvent::Output(vec![index as u8; PTY_READ_BUFFER_SIZE]),
                    &reader_events,
                    &producer_commands,
                );
                completed
                    .send(sent.then_some(PTY_READ_BUFFER_SIZE))
                    .unwrap();
                if !sent {
                    break;
                }
            }
        });

        let mut queued_bytes = 0;
        for index in 0..PTY_OUTPUT_QUEUE_CAPACITY {
            assert_eq!(attempts.recv().unwrap(), index);
            queued_bytes += completions.recv().unwrap().unwrap();
        }
        assert_eq!(attempts.recv().unwrap(), PTY_OUTPUT_QUEUE_CAPACITY);
        assert!(matches!(
            completions.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(
            queued_bytes,
            PTY_OUTPUT_QUEUE_CAPACITY * PTY_READ_BUFFER_SIZE
        );

        command_tx.send(Command::Shutdown).unwrap();
        for _ in 0..PTY_OUTPUT_QUEUE_CAPACITY {
            assert!(matches!(commands.recv().unwrap(), Command::ReaderReady));
        }
        assert!(matches!(commands.recv().unwrap(), Command::Shutdown));
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        drop(reader_event_rx);
        assert_eq!(completions.recv().unwrap(), None);
        producer.join().unwrap();
    }

    #[test]
    fn key_actions_should_retain_fifo_order_in_the_reliable_command_lane() {
        let (commands, receiver) = mpsc::channel();
        for action in [KeyAction::Press, KeyAction::Repeat, KeyAction::Release] {
            commands
                .send(Command::Key(KeyInput {
                    action,
                    physical_key: PhysicalKey::A,
                    native_key_code: Some(0),
                    logical_key: "a".to_owned(),
                    text: Some("a".to_owned()),
                    unshifted_codepoint: Some('a'),
                    modifiers: InputModifiers::default(),
                    consumed_modifiers: InputModifiers::default(),
                    option_as_alt: OptionAsAltPolicy::default(),
                }))
                .unwrap();
        }

        let actions = [receiver.recv(), receiver.recv(), receiver.recv()].map(|command| {
            let Command::Key(input) = command.unwrap() else {
                panic!("the command lane should contain only typed key input")
            };
            input.action
        });

        assert_eq!(
            actions,
            [KeyAction::Press, KeyAction::Repeat, KeyAction::Release]
        );
    }

    #[test]
    fn held_keys_track_only_terminal_routed_keys_and_modifiers() {
        let mut held = HeldKeys::default();
        held.route(&text_key(KeyAction::Press));
        held.route(&modifier_key(KeyAction::Press));
        held.route(&text_key(KeyAction::Release));

        let releases = held.take_releases();

        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].physical_key, PhysicalKey::ShiftLeft);
        assert_eq!(releases[0].action, KeyAction::Release);
        assert!(held.take_releases().is_empty());

        let application_shortcut_never_routed = HeldKeys::default();
        assert!(application_shortcut_never_routed.0.is_empty());
    }

    #[test]
    fn enabling_focus_reporting_emits_current_state_and_deduplicates_edges() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, _events) = result.unwrap();
        session.focus(false);
        reader_steps
            .send(ReaderStep::Bytes(b"\x1b[?1004h".to_vec()))
            .unwrap();

        let enabled = records.wait_for("the current focus-out report", |state| {
            state.written == b"\x1b[O"
        });
        assert_eq!(enabled.written, b"\x1b[O");

        session.focus(false);
        session.resize(test_geometry());
        records.wait_for("the duplicate focus barrier", |state| {
            !state.resizes.is_empty()
        });
        assert_eq!(records.snapshot().written, b"\x1b[O");

        session.focus(true);
        records.wait_for("the focus-in edge", |state| {
            state.written == b"\x1b[O\x1b[I"
        });
        session.focus(true);
        session.resize(geometry(81, 24, 8.0, 20.0));
        records.wait_for("the duplicate focus-in barrier", |state| {
            state.resizes.len() == 2
        });
        assert_eq!(records.snapshot().written, b"\x1b[O\x1b[I");

        reader_steps
            .send(ReaderStep::Bytes(b"\x1b[?1004l".to_vec()))
            .unwrap();
        session.resize(geometry(82, 24, 8.0, 20.0));
        records.wait_for("the focus-reporting disable barrier", |state| {
            state.resizes.len() == 3
        });
        session.focus(false);
        session.resize(geometry(83, 24, 8.0, 20.0));
        records.wait_for("the disabled focus edge barrier", |state| {
            state.resizes.len() == 4
        });
        assert_eq!(records.snapshot().written, b"\x1b[O\x1b[I");

        session.shutdown();
    }

    #[test]
    fn focus_loss_releases_terminal_held_keys_once_before_focus_out() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, _events) = result.unwrap();
        reader_steps
            .send(ReaderStep::Bytes(b"\x1b[>11u\x1b[?1004h".to_vec()))
            .unwrap();
        records.wait_for("the current focus-in report", |state| {
            state.written == b"\x1b[I"
        });

        session.key(text_key(KeyAction::Press));
        session.focus(false);
        records.wait_for("held release before focus-out", |state| {
            state.written.ends_with(b"\x1b[97;1:3u\x1b[O")
        });
        let once = records.snapshot().written;

        session.focus(false);
        session.resize(test_geometry());
        records.wait_for("the duplicate focus-out barrier", |state| {
            !state.resizes.is_empty()
        });
        assert_eq!(records.snapshot().written, once);

        session.shutdown();
    }

    #[test]
    fn consecutive_output_chunks_should_publish_one_ordered_coalesced_screen() {
        let (command_tx, commands) = mpsc::channel();
        let (reader_events, reader_event_rx) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
        assert!(send_reader_event(
            ReaderEvent::Output(b"first".to_vec()),
            &reader_events,
            &command_tx,
        ));
        assert!(send_reader_event(
            ReaderEvent::Output(b" second".to_vec()),
            &reader_events,
            &command_tx,
        ));
        assert!(matches!(commands.recv().unwrap(), Command::ReaderReady));

        let records = ScriptedPtyRecords::default();
        let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
        let mut worker = TerminalWorker {
            pty: Box::new(ScriptedPty {
                reader: None,
                records,
                reader_error: None,
                resize_error: None,
                write_error: None,
                wait_error: None,
                wait_times_out: false,
                exit_code: 0,
            }),
            emulator: TerminalEmulator::new(test_geometry()).unwrap(),
            commands,
            reader_events: reader_event_rx,
            reader_thread: thread::spawn(|| {}),
            events,
            resizes: ResizeMailbox::default(),
            find_queries: FindQueryMailbox::default(),
            pending_command: None,
            terminal_input_focused: true,
            focus_reporting_enabled: false,
            held_keys: HeldKeys::default(),
            selection_autoscroll: SelectionAutoscrollSchedule::default(),
            paste_confirmations: PasteConfirmationSchedule::default(),
            osc52_filter: Osc52Filter::default(),
            osc52_policy: Osc52AuthorizationPolicy::default(),
            osc52_clipboard: Box::<UnavailableOsc52Clipboard>::default(),
            osc52_authorization: Osc52AuthorizationSchedule::default(),
            deferred_osc52_effects: VecDeque::new(),
            deferred_output_chunks: VecDeque::new(),
            deferred_reader_ready: false,
            hidden_input: HiddenInputSchedule::new(Instant::now()),
        };

        assert!(worker.process_reader_events());
        assert!(worker.pending_command.is_none());
        let SessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
            panic!("coalesced output must publish a terminal screen")
        };
        assert!(screen_text(&screen).contains("first second"));
        assert!(receiver.try_recv().is_err());
        assert!(matches!(
            worker.commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        worker.finish();
    }

    #[test]
    fn synchronized_output_deadline_should_publish_the_pending_screen() {
        let (_command_tx, commands) = mpsc::channel();
        let (_reader_events, reader_event_rx) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
        let records = ScriptedPtyRecords::default();
        let (events, receiver) = async_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY);
        let mut worker = TerminalWorker {
            pty: Box::new(ScriptedPty {
                reader: None,
                records,
                reader_error: None,
                resize_error: None,
                write_error: None,
                wait_error: None,
                wait_times_out: false,
                exit_code: 0,
            }),
            emulator: TerminalEmulator::new(test_geometry()).unwrap(),
            commands,
            reader_events: reader_event_rx,
            reader_thread: thread::spawn(|| {}),
            events,
            resizes: ResizeMailbox::default(),
            find_queries: FindQueryMailbox::default(),
            pending_command: None,
            terminal_input_focused: true,
            focus_reporting_enabled: false,
            held_keys: HeldKeys::default(),
            selection_autoscroll: SelectionAutoscrollSchedule::default(),
            paste_confirmations: PasteConfirmationSchedule::default(),
            osc52_filter: Osc52Filter::default(),
            osc52_policy: Osc52AuthorizationPolicy::default(),
            osc52_clipboard: Box::<UnavailableOsc52Clipboard>::default(),
            osc52_authorization: Osc52AuthorizationSchedule::default(),
            deferred_osc52_effects: VecDeque::new(),
            deferred_output_chunks: VecDeque::new(),
            deferred_reader_ready: false,
            hidden_input: HiddenInputSchedule::new(Instant::now()),
        };
        assert!(worker.publish_screen());
        let _ = receiver.try_recv().unwrap();

        let started = Instant::now();
        worker.emulator.feed_at(b"\x1b[?2026hstalled", started);
        assert!(worker.publish_screen());
        assert!(receiver.try_recv().is_err());

        assert!(
            worker.release_synchronized_output_if_due(started + MAX_SYNCHRONIZED_OUTPUT_DURATION)
        );
        let SessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
            panic!("the synchronized-output deadline must publish a screen")
        };
        assert!(screen_text(&screen).contains("stalled"));
        worker.finish();
    }

    #[test]
    fn output_control_output_should_preserve_screen_order_through_the_session_interface() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, events) = result.unwrap();

        reader_steps
            .send(ReaderStep::Bytes(b"first".to_vec()))
            .unwrap();
        let first = receive_event(
            &events,
            "the first output Screen",
            |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains("first")),
        );
        session.resize(test_geometry());
        records.wait_for("the control between output chunks", |state| {
            state.resizes.len() == 1
        });
        reader_steps
            .send(ReaderStep::Bytes(b" second".to_vec()))
            .unwrap();
        let second = receive_event(
            &events,
            "the second output Screen",
            |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains("first second")),
        );

        let (SessionEvent::Screen(first), SessionEvent::Screen(second)) = (first, second) else {
            unreachable!("the event predicates accept only terminal screens")
        };
        assert_eq!(
            (
                screen_text(&first).contains("second"),
                screen_text(&second).contains("first second"),
                records.snapshot().resizes,
            ),
            (false, true, vec![pty_size(test_geometry())])
        );
        session.shutdown();
    }

    #[test]
    fn copy_selection_should_observe_every_preceding_pointer_event() {
        let (result, reader_steps, _records) =
            start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, events) = result.unwrap();
        reader_steps
            .send(ReaderStep::Bytes(b"selected".to_vec()))
            .unwrap();
        let SessionEvent::Screen(screen) = receive_event(
            &events,
            "the selectable terminal output",
            |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains("selected")),
        ) else {
            unreachable!()
        };
        let pointer = |phase, position| PointerInput {
            generation: screen.generation,
            phase,
            button: (phase != PointerPhase::Motion).then_some(PointerButton::Left),
            position,
            modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::default(),
        };

        session.pointer(pointer(
            PointerPhase::Press,
            SurfacePosition { x: 1.0, y: 1.0 },
        ));
        session.pointer(pointer(
            PointerPhase::Motion,
            SurfacePosition { x: 63.0, y: 1.0 },
        ));
        session.pointer(pointer(
            PointerPhase::Release,
            SurfacePosition { x: 63.0, y: 1.0 },
        ));
        let copy = session.copy_selection().unwrap().unwrap();

        assert_eq!(copy.plain_text, "selected");
        session.shutdown();
    }

    #[test]
    fn resize_should_reach_the_pty_with_pixel_dimensions() {
        let (result, _reader_steps, records) =
            start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, _events) = result.unwrap();
        let resized = geometry(100, 30, 9.0, 21.0);

        session.resize(resized);
        let state = records.wait_for("the scripted PTY resize", |state| state.resizes.len() == 1);

        assert_eq!(state.resizes, vec![pty_size(resized)]);
        session.shutdown();
    }

    #[test]
    fn pixel_only_resize_should_reach_the_pty_without_publishing_a_grid_screen() {
        let (result, _reader_steps, records) =
            start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, events) = result.unwrap();
        let initial = receive_event(&events, "the initial terminal screen", |event| {
            matches!(event, SessionEvent::Screen(_))
        });
        let SessionEvent::Screen(initial) = initial else {
            unreachable!()
        };
        let grid = test_geometry().grid();
        let resized = geometry(grid.cols, grid.rows, 9.0, 21.0);

        session.resize(resized);
        assert!(session.copy_selection().is_ok());

        assert_eq!(records.snapshot().resizes, vec![pty_size(resized)]);
        assert!(events.try_recv().is_err());
        assert_eq!(initial.size.cols, grid.cols);
        assert_eq!(initial.size.rows, grid.rows);
        session.shutdown();
    }

    #[test]
    fn fractional_backing_geometry_should_reach_the_pty_without_per_cell_rounding() {
        let (result, _reader_steps, records) =
            start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, _events) = result.unwrap();
        let resized = TerminalGeometry::from_grid(
            CellGridSize::new(10, 2),
            LogicalCellSize::new(7.5, 20.0),
            BackingScale::new(1.5).unwrap(),
        );

        session.resize(resized);
        let state = records.wait_for("the fractional scripted PTY resize", |state| {
            state.resizes.len() == 1
        });

        assert_eq!(
            state.resizes,
            vec![PtySize {
                rows: 2,
                cols: 10,
                pixel_width: 113,
                pixel_height: 60,
            }]
        );
        session.shutdown();
    }

    #[test]
    fn rapid_resizes_should_queue_one_notification_and_retain_only_the_latest_geometry() {
        let (commands, receiver) = mpsc::channel();
        let resizes = ResizeMailbox::default();
        let mut session = TerminalSession {
            commands: Some(commands),
            worker: None,
            terminator: None,
            resizes: resizes.clone(),
            find_queries: FindQueryMailbox::default(),
        };
        let pixel_only = TerminalGeometry::from_grid(
            CellGridSize::new(80, 24),
            LogicalCellSize::new(7.5, 20.0),
            BackingScale::new(1.5).unwrap(),
        );
        let latest = geometry(100, 30, 9.0, 21.0);

        session.resize(pixel_only);
        session.resize(latest);

        assert_eq!(
            (
                matches!(receiver.try_recv(), Ok(Command::Resize)),
                receiver.try_recv().is_err(),
                resizes.take(),
            ),
            (true, true, Some(latest))
        );
        session.shutdown();
    }

    #[test]
    fn rapid_find_queries_should_queue_one_notification_and_retain_only_the_latest_query() {
        let (commands, receiver) = mpsc::channel();
        let find_queries = FindQueryMailbox::default();
        let mut session = TerminalSession {
            commands: Some(commands),
            worker: None,
            terminator: None,
            resizes: ResizeMailbox::default(),
            find_queries: find_queries.clone(),
        };

        session.set_find_query(FindQueryGeneration::test(1), "n".to_owned());
        session.set_find_query(FindQueryGeneration::test(2), "needle".to_owned());

        assert!(matches!(receiver.try_recv(), Ok(Command::FindQueryChanged)));
        assert!(matches!(
            find_queries.take(),
            Some(FindQueryUpdate::Set(generation, query))
                if generation == FindQueryGeneration::test(2) && query == "needle"
        ));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        session.shutdown();
    }

    #[test]
    fn find_close_should_supersede_a_pending_query_update() {
        let (commands, receiver) = mpsc::channel();
        let find_queries = FindQueryMailbox::default();
        let mut session = TerminalSession {
            commands: Some(commands),
            worker: None,
            terminator: None,
            resizes: ResizeMailbox::default(),
            find_queries: find_queries.clone(),
        };

        session.set_find_query(FindQueryGeneration::test(1), "needle".to_owned());
        session.end_find(FindQueryGeneration::test(2));

        assert!(matches!(receiver.try_recv(), Ok(Command::FindQueryChanged)));
        assert!(matches!(
            find_queries.take(),
            Some(FindQueryUpdate::End(generation))
                if generation == FindQueryGeneration::test(2)
        ));
        session.shutdown();
    }

    #[test]
    fn pending_pty_responses_should_precede_later_input_through_the_session_interface() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, events) = result.unwrap();
        let resized = geometry(20, 4, 8.0, 18.0);

        reader_steps
            .send(ReaderStep::Bytes(b"\x1b[?2048hX".to_vec()))
            .unwrap();
        receive_event(
            &events,
            "the mode-setting terminal output",
            |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains('X')),
        );
        session.resize(resized);
        records.wait_for("the in-band terminal resize response", |state| {
            state.written == b"\x1b[48;4;20;72;160t"
        });
        assert_eq!(
            session
                .request_paste("later".to_owned())
                .recv_blocking()
                .unwrap(),
            Ok(PasteRequestOutcome::Written)
        );
        let state = records.wait_for("the later terminal input", |state| {
            state.written.ends_with(b"later")
        });

        assert_eq!(state.written, b"\x1b[48;4;20;72;160tlater");
        session.shutdown();
    }

    #[test]
    fn bracketed_multiline_paste_is_written_without_confirmation() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, events) = result.unwrap();

        reader_steps
            .send(ReaderStep::Bytes(b"\x1b[?2004hX".to_vec()))
            .unwrap();
        receive_event(
            &events,
            "bracketed-paste mode activation",
            |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains('X')),
        );

        assert_eq!(
            session
                .request_paste("one\ntwo".to_owned())
                .recv_blocking()
                .unwrap(),
            Ok(PasteRequestOutcome::Written)
        );
        let state = records.wait_for("the bracketed multiline paste", |state| {
            state.written.ends_with(b"\x1b[200~one\ntwo\x1b[201~")
        });
        assert_eq!(state.written, b"\x1b[200~one\ntwo\x1b[201~");
        session.shutdown();
    }

    #[test]
    fn control_bearing_paste_is_sanitized_without_confirmation() {
        let (result, _reader_steps, records) =
            start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, _events) = result.unwrap();

        assert_eq!(
            session
                .request_paste("one\x03two".to_owned())
                .recv_blocking()
                .unwrap(),
            Ok(PasteRequestOutcome::Written)
        );
        let state = records.wait_for("the sanitized paste", |state| state.written == b"one two");
        assert_eq!(state.written, b"one two");
        session.shutdown();
    }

    #[test]
    fn bracketed_paste_with_a_closing_fence_still_requires_confirmation() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, events) = result.unwrap();

        reader_steps
            .send(ReaderStep::Bytes(b"\x1b[?2004hX".to_vec()))
            .unwrap();
        receive_event(
            &events,
            "bracketed-paste mode activation",
            |event| matches!(event, SessionEvent::Screen(screen) if screen_text(screen).contains('X')),
        );

        let outcome = session
            .request_paste("one\x1b[201~two".to_owned())
            .recv_blocking()
            .unwrap()
            .unwrap();
        let PasteRequestOutcome::ConfirmationRequired(confirmation) = outcome else {
            panic!("an embedded closing fence must require confirmation")
        };

        assert!(confirmation.risk.closing_fence);
        assert!(records.snapshot().written.is_empty());
        session.shutdown();
    }

    #[test]
    fn unsafe_paste_is_immutable_until_confirmation_and_uses_exact_unbracketed_bytes() {
        let (result, _reader_steps, records) =
            start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, _events) = result.unwrap();
        let mut caller_copy = "one\r\ntw\x03o".to_owned();

        let outcome = session
            .request_paste(caller_copy.clone())
            .recv_blocking()
            .unwrap()
            .unwrap();
        caller_copy.clear();
        let PasteRequestOutcome::ConfirmationRequired(confirmation) = outcome else {
            panic!("multiline, control-bearing paste must require confirmation")
        };
        assert!(records.snapshot().written.is_empty());

        assert_eq!(
            session
                .resolve_paste(confirmation.id, PasteDecision::Confirm)
                .recv_blocking()
                .unwrap(),
            Ok(PasteResolution::Written)
        );
        assert_eq!(records.snapshot().written, b"one\rtw o");
        session.shutdown();
    }

    #[test]
    fn cancelled_paste_writes_no_pty_bytes() {
        let (result, _reader_steps, records) =
            start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, _events) = result.unwrap();
        let outcome = session
            .request_paste("first\nsecond".to_owned())
            .recv_blocking()
            .unwrap()
            .unwrap();
        let PasteRequestOutcome::ConfirmationRequired(confirmation) = outcome else {
            panic!("multiline paste must require confirmation")
        };

        assert_eq!(
            session
                .resolve_paste(confirmation.id, PasteDecision::Cancel)
                .recv_blocking()
                .unwrap(),
            Ok(PasteResolution::Cancelled)
        );
        assert!(records.snapshot().written.is_empty());
        session.shutdown();
    }

    #[test]
    fn focus_loss_invalidates_pending_paste_before_confirmation() {
        let (result, _reader_steps, records) =
            start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, _events) = result.unwrap();
        let outcome = session
            .request_paste("first\nsecond".to_owned())
            .recv_blocking()
            .unwrap()
            .unwrap();
        let PasteRequestOutcome::ConfirmationRequired(confirmation) = outcome else {
            panic!("multiline paste must require confirmation")
        };

        session.focus(false);
        let _ = session.copy_selection();
        assert_eq!(
            session
                .resolve_paste(confirmation.id, PasteDecision::Confirm)
                .recv_blocking()
                .unwrap(),
            Ok(PasteResolution::Stale)
        );
        assert!(records.snapshot().written.is_empty());
        session.shutdown();
    }

    #[test]
    fn only_one_unsafe_paste_can_await_confirmation() {
        let (result, _reader_steps, records) =
            start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, _events) = result.unwrap();
        let first = session
            .request_paste("first\ncommand".to_owned())
            .recv_blocking()
            .unwrap()
            .unwrap();
        assert!(matches!(
            first,
            PasteRequestOutcome::ConfirmationRequired(_)
        ));

        assert_eq!(
            session
                .request_paste("second\ncommand".to_owned())
                .recv_blocking()
                .unwrap(),
            Ok(PasteRequestOutcome::Rejected(
                PasteRejection::ConfirmationPending
            ))
        );
        assert!(records.snapshot().written.is_empty());
        session.shutdown();
    }

    #[test]
    fn paste_confirmation_schedule_expires_without_exposing_payload() {
        let now = Instant::now();
        let mut schedule = PasteConfirmationSchedule::default();
        let payload = PreparedPaste::prepare("first\nsecond".to_owned()).unwrap();
        let confirmation = schedule.create(payload, now).unwrap();

        assert!(schedule.expire(now + PASTE_CONFIRMATION_TIMEOUT));
        assert_eq!(schedule.take(confirmation.id, now), None);
    }

    #[test]
    fn write_failure_should_emit_a_runtime_failure_and_stop_the_worker() {
        let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
            write_error: Some("write unavailable".to_owned()),
            ..ScriptedPtyOptions::default()
        });
        let (mut session, events) = result.unwrap();

        let _ = session.request_paste("input".to_owned()).recv_blocking();
        let event = receive_event(&events, "the PTY write failure", |event| {
            matches!(event, SessionEvent::Failed(_))
        });

        let SessionEvent::Failed(failure) = event else {
            unreachable!("the event predicate accepts only terminal failures")
        };
        assert_eq!(
            failure,
            SessionFailure::Runtime(
                "failed to write to the shell PTY: write unavailable".to_owned()
            )
        );
        let state = records.wait_for("the failed PTY worker to release ownership", |state| {
            state.pty_drops == 1
        });
        assert_eq!((state.write_attempts, state.written.len()), (1, 0));

        session.shutdown();
        let state = records.snapshot();
        assert_eq!(
            (state.terminations, state.pty_drops, state.terminator_drops),
            (1, 1, 1)
        );
    }

    #[test]
    fn reader_error_with_successful_wait_should_emit_a_pty_read_failure() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
            exit_code: 7,
            ..ScriptedPtyOptions::default()
        });
        let (mut session, events) = result.unwrap();

        reader_steps
            .send(ReaderStep::Error("read unavailable".to_owned()))
            .unwrap();
        let event = receive_event(&events, "the PTY read failure", |event| {
            matches!(event, SessionEvent::Failed(_))
        });

        let SessionEvent::Failed(SessionFailure::PtyRead {
            read_error,
            exit_status,
        }) = event
        else {
            panic!("a read error followed by a successful wait must be classified as PtyRead")
        };
        assert_eq!(read_error, "read unavailable");
        assert_eq!(exit_status, "Shell exited with code 7");
        assert_eq!(records.snapshot().waits, 1);

        session.shutdown();
    }

    #[test]
    fn reader_error_with_wait_failure_should_preserve_both_errors() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
            wait_error: Some("wait unavailable".to_owned()),
            ..ScriptedPtyOptions::default()
        });
        let (mut session, events) = result.unwrap();

        reader_steps
            .send(ReaderStep::Error("read unavailable".to_owned()))
            .unwrap();
        let event = receive_event(&events, "the PTY read and wait failure", |event| {
            matches!(event, SessionEvent::Failed(_))
        });

        let SessionEvent::Failed(failure) = event else {
            unreachable!("the event predicate accepts only terminal failures")
        };
        assert_eq!(
            failure,
            SessionFailure::ShellWait {
                read_error: Some("read unavailable".to_owned()),
                wait_error: "wait unavailable".to_owned(),
            }
        );
        assert_eq!(records.snapshot().waits, 1);

        session.shutdown();
    }

    #[test]
    fn reader_eof_with_wait_failure_should_emit_a_shell_wait_failure() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
            wait_error: Some("wait unavailable".to_owned()),
            ..ScriptedPtyOptions::default()
        });
        let (mut session, events) = result.unwrap();

        reader_steps.send(ReaderStep::Eof).unwrap();
        let event = receive_event(&events, "the shell wait failure", |event| {
            matches!(event, SessionEvent::Failed(_))
        });

        let SessionEvent::Failed(failure) = event else {
            unreachable!("the event predicate accepts only terminal failures")
        };
        assert_eq!(
            failure,
            SessionFailure::ShellWait {
                read_error: None,
                wait_error: "wait unavailable".to_owned(),
            }
        );
        assert_eq!(records.snapshot().waits, 1);

        session.shutdown();
    }

    #[test]
    fn child_wait_timeout_should_emit_a_shell_wait_failure() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
            wait_times_out: true,
            ..ScriptedPtyOptions::default()
        });
        let (mut session, events) = result.unwrap();

        reader_steps.send(ReaderStep::Eof).unwrap();
        let event = receive_event(&events, "the bounded shell wait failure", |event| {
            matches!(event, SessionEvent::Failed(_))
        });

        let SessionEvent::Failed(SessionFailure::ShellWait {
            read_error,
            wait_error,
        }) = event
        else {
            panic!("a child wait timeout must be classified as ShellWait")
        };
        assert_eq!(read_error, None);
        assert_eq!(
            wait_error,
            "timed out after 2000 ms waiting for the scripted shell process to exit"
        );
        let state = records.wait_for("the timed-out PTY worker to release ownership", |state| {
            state.pty_drops == 1 && state.reader_drops == 1
        });
        assert_eq!(
            (state.waits, state.pty_drops, state.reader_drops),
            (1, 1, 1)
        );

        session.shutdown();
    }

    #[test]
    fn reader_eof_should_wait_for_the_child_and_emit_exit() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
            exit_code: 7,
            ..ScriptedPtyOptions::default()
        });
        let (mut session, events) = result.unwrap();

        reader_steps.send(ReaderStep::Eof).unwrap();
        let event = receive_event(&events, "the scripted shell exit", |event| {
            matches!(event, SessionEvent::Exited(_))
        });

        assert!(matches!(
            event,
            SessionEvent::Exited(SessionExit::ExitCode(7))
        ));
        let state = records.wait_for("the exited PTY worker to release ownership", |state| {
            state.pty_drops == 1
        });
        assert_eq!((state.waits, state.pty_drops), (1, 1));

        session.shutdown();
    }

    #[test]
    fn repeated_shutdown_should_return_before_a_blocked_reader_finishes() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
            termination_releases_reader: false,
            ..ScriptedPtyOptions::default()
        });
        let (mut session, _events) = result.unwrap();
        let (completed, completion) = mpsc::sync_channel(1);

        let shutdown_thread = thread::spawn(move || {
            session.shutdown();
            session.shutdown();
            completed.send(session).unwrap();
        });
        let session = match completion.recv_timeout(Duration::from_millis(250)) {
            Ok(session) => session,
            Err(error) => {
                reader_steps.send(ReaderStep::Eof).unwrap();
                shutdown_thread.join().unwrap();
                panic!("shutdown waited for the blocked reader thread: {error}");
            }
        };
        shutdown_thread.join().unwrap();
        let state = records.wait_for("the detached worker to drop its PTY", |state| {
            state.pty_drops == 1
        });
        assert_eq!(
            (
                state.terminations,
                state.pty_drops,
                state.reader_drops,
                state.terminator_drops,
                session.commands.is_none(),
                session.worker.is_none(),
                session.terminator.is_none(),
            ),
            (1, 1, 0, 1, true, true, true)
        );
        assert_eq!(
            state.lifecycle,
            vec![
                LifecycleStep::TerminationRequested,
                LifecycleStep::PtyDropped,
            ]
        );

        reader_steps.send(ReaderStep::Eof).unwrap();
        let state = records.wait_for("the released reader ownership", |state| {
            state.reader_drops == 1
        });
        assert_eq!(
            (
                state.terminations,
                state.pty_drops,
                state.reader_drops,
                state.terminator_drops,
            ),
            (1, 1, 1, 1)
        );
    }

    #[test]
    fn drop_should_return_after_termination_fails_with_a_blocked_reader() {
        let (result, reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
            termination_error: Some("termination unavailable".to_owned()),
            termination_releases_reader: false,
            ..ScriptedPtyOptions::default()
        });
        let (session, _events) = result.unwrap();
        let (completed, completion) = mpsc::sync_channel(1);

        let drop_thread = thread::spawn(move || {
            drop(session);
            completed.send(()).unwrap();
        });
        if let Err(error) = completion.recv_timeout(Duration::from_millis(250)) {
            reader_steps.send(ReaderStep::Eof).unwrap();
            drop_thread.join().unwrap();
            panic!("Drop waited after termination failed: {error}");
        }
        drop_thread.join().unwrap();
        let state = records.wait_for("the detached worker to drop its PTY", |state| {
            state.pty_drops == 1
        });
        assert_eq!(
            (
                state.terminations,
                state.pty_drops,
                state.reader_drops,
                state.terminator_drops,
            ),
            (1, 1, 0, 1)
        );

        reader_steps.send(ReaderStep::Eof).unwrap();
        let state = records.wait_for("the reader to release after termination failure", |state| {
            state.reader_drops == 1
        });
        assert_eq!(
            (
                state.terminations,
                state.pty_drops,
                state.reader_drops,
                state.terminator_drops,
            ),
            (1, 1, 1, 1)
        );
    }

    #[test]
    fn stopped_session_returns_an_error_for_selection_requests() {
        let session = TerminalSession {
            commands: None,
            worker: None,
            terminator: None,
            resizes: ResizeMailbox::default(),
            find_queries: FindQueryMailbox::default(),
        };

        assert_eq!(
            session.copy_selection(),
            Err(SelectionCopyError::WorkerStopped)
        );
    }

    #[test]
    fn real_shell_output_round_trips_through_the_pty_and_emulator() {
        let _isolation = crate::platform::macos_pty::lock_real_pty_test();
        let size = test_geometry();
        let (session, events) =
            TerminalSession::start(size, &std::env::current_dir().unwrap()).unwrap();
        let session = JoinedRealPtySession(session);

        // The command renders a red X. The echoed command contains an X too, but
        // only the shell's output passes through the SGR sequence and becomes red.
        let request = session
            .request_paste("printf '\\033[31mX\\033[0m\\n'\n".to_owned())
            .recv_blocking()
            .unwrap()
            .unwrap();
        let PasteRequestOutcome::ConfirmationRequired(confirmation) = request else {
            panic!("multiline paste must require confirmation")
        };
        assert_eq!(
            session
                .resolve_paste(confirmation.id, PasteDecision::Confirm)
                .recv_blocking()
                .unwrap(),
            Ok(PasteResolution::Written)
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_red_x = false;
        while Instant::now() < deadline && !saw_red_x {
            match events.try_recv() {
                Ok(SessionEvent::Screen(screen)) => {
                    saw_red_x = screen.rows.iter().flat_map(|row| row.iter()).any(|cell| {
                        cell.text == "X"
                            && cell.foreground_source == crate::terminal::TerminalColor::Palette(1)
                    });
                }
                Ok(SessionEvent::Failed(failure)) => panic!("terminal session failed: {failure}"),
                Ok(SessionEvent::Exited(status)) => panic!("shell exited early: {status}"),
                Ok(
                    SessionEvent::Osc52Authorization(_)
                    | SessionEvent::Osc52AuthorizationExpired(_),
                ) => {}
                Ok(SessionEvent::HiddenInputChanged(_) | SessionEvent::Attention(_)) => {}
                Err(async_channel::TryRecvError::Empty) => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(async_channel::TryRecvError::Closed) => break,
            }
        }

        assert!(
            saw_red_x,
            "did not receive colored output from the real shell"
        );
    }

    #[test]
    fn real_shell_exit_command_emits_an_exited_event() {
        let _isolation = crate::platform::macos_pty::lock_real_pty_test();
        let size = test_geometry();
        let (session, events) =
            TerminalSession::start(size, &std::env::current_dir().unwrap()).unwrap();
        let session = JoinedRealPtySession(session);

        let request = session
            .request_paste("exit\n".to_owned())
            .recv_blocking()
            .unwrap()
            .unwrap();
        let PasteRequestOutcome::ConfirmationRequired(confirmation) = request else {
            panic!("multiline paste must require confirmation")
        };
        let _ = session
            .resolve_paste(confirmation.id, PasteDecision::Confirm)
            .recv_blocking();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exit_status = None;
        while Instant::now() < deadline && exit_status.is_none() {
            match events.try_recv() {
                Ok(SessionEvent::Screen(_)) => {}
                Ok(SessionEvent::Exited(status)) => exit_status = Some(status),
                Ok(SessionEvent::Failed(failure)) => panic!("terminal session failed: {failure}"),
                Ok(
                    SessionEvent::Osc52Authorization(_)
                    | SessionEvent::Osc52AuthorizationExpired(_),
                ) => {}
                Ok(SessionEvent::HiddenInputChanged(_) | SessionEvent::Attention(_)) => {}
                Err(async_channel::TryRecvError::Empty) => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(async_channel::TryRecvError::Closed) => break,
            }
        }

        drop(session);
        assert!(
            exit_status.is_some(),
            "shell exit did not produce a terminal lifecycle event"
        );
    }

    #[test]
    fn selection_autoscroll_schedule_uses_an_injected_monotonic_now() {
        let epoch = Instant::now();
        let generation = PresentationGeneration::default();
        let mut schedule = SelectionAutoscrollSchedule::default();

        schedule.update(epoch, Some(Duration::from_millis(100)), generation);

        assert_eq!(schedule.take_due(epoch + Duration::from_millis(99)), None);
        assert_eq!(
            schedule.take_due(epoch + Duration::from_millis(100)),
            Some(generation)
        );
        assert_eq!(schedule.take_due(epoch + Duration::from_secs(1)), None);

        schedule.update(epoch, Some(Duration::from_millis(25)), generation);
        schedule.update(epoch, None, generation);
        assert_eq!(schedule.take_due(epoch + Duration::from_secs(1)), None);
    }

    #[test]
    fn worker_autoscroll_ticks_publish_scrollback_without_more_pointer_motion() {
        let (result, reader_steps, _records) =
            start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, events) = result.unwrap();
        let mut output = Vec::new();
        for row in 0..30 {
            output.extend_from_slice(format!("row {row:02}\r\n").as_bytes());
        }
        reader_steps.send(ReaderStep::Bytes(output)).unwrap();
        let SessionEvent::Screen(bottom) = receive_event(
            &events,
            "scrollback at the bottom",
            |event| matches!(event, SessionEvent::Screen(screen) if screen.scrollbar.total_rows > screen.scrollbar.visible_rows),
        ) else {
            unreachable!()
        };
        let bottom_offset = bottom.scrollbar.offset_rows;
        let pointer = |phase, position, generation| PointerInput {
            generation,
            phase,
            button: (phase != PointerPhase::Motion).then_some(PointerButton::Left),
            position,
            modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::default(),
        };
        session.pointer(pointer(
            PointerPhase::Press,
            SurfacePosition { x: 1.0, y: 470.0 },
            bottom.generation,
        ));
        session.pointer(pointer(
            PointerPhase::Motion,
            SurfacePosition { x: 1.0, y: -1.0 },
            bottom.generation,
        ));

        let SessionEvent::Screen(autoscrolled) = receive_event(
            &events,
            "worker-driven selection autoscroll",
            move |event| matches!(event, SessionEvent::Screen(screen) if screen.scrollbar.offset_rows < bottom_offset),
        ) else {
            unreachable!()
        };
        session.pointer(pointer(
            PointerPhase::Release,
            SurfacePosition { x: 1.0, y: -1.0 },
            autoscrolled.generation,
        ));
        session.shutdown();
    }
}
