use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver as CommandReceiver, Sender as CommandSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Error as AnyError;
#[cfg(test)]
use portable_pty::ExitStatus;
use portable_pty::PtySize;
use thiserror::Error;

use crate::platform::macos_pty::{
    PtyError, PtyTerminator, ShellExit, ShutdownDisposition, SpawnedPty, spawn_user_shell,
    user_shell,
};
use crate::terminal::emulator::{EmulatorAction, ScreenSnapshot, TerminalEmulator};
use crate::terminal::geometry::TerminalGeometry;
#[cfg(test)]
use crate::terminal::key::OptionAsAltPolicy;
use crate::terminal::key::{InputModifiers, KeyInput};

const FINAL_CHILD_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const PTY_READ_BUFFER_SIZE: usize = 16 * 1024;
const PTY_OUTPUT_QUEUE_CAPACITY: usize = 8;

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

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PointerInput {
    pub(crate) phase: PointerPhase,
    pub(crate) button: Option<PointerButton>,
    pub(crate) position: SurfacePosition,
    pub(crate) modifiers: InputModifiers,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WheelInput {
    pub(crate) steps: i32,
    pub(crate) position: SurfacePosition,
    pub(crate) modifiers: InputModifiers,
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

pub(crate) trait TerminalSessionHandle {
    fn key(&self, input: KeyInput);
    fn resize(&self, geometry: TerminalGeometry);
    fn pointer(&self, input: PointerInput);
    fn wheel(&self, input: WheelInput);
    fn scroll_to(&self, offset_rows: u64);
    fn paste(&self, text: String);
    fn request_selection_text(&self) -> async_channel::Receiver<Result<Option<String>, String>>;
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

pub(crate) struct TerminalSession {
    commands: Option<CommandSender<Command>>,
    worker: Option<JoinHandle<()>>,
    terminator: Option<Box<dyn SessionPtyTerminator>>,
    resizes: ResizeMailbox,
}

trait SessionPty: Write + Send {
    fn take_reader(&mut self) -> std::io::Result<Box<dyn Read + Send>>;
    fn resize(&self, size: PtySize) -> Result<(), AnyError>;
    fn wait_for_child(&mut self, timeout: Duration) -> std::io::Result<ShellExit>;
}

trait SessionPtyTerminator: Send + Sync {
    fn terminate(&self) -> std::io::Result<()>;
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
        let (command_tx, command_rx) = mpsc::channel();
        let reader_transport = ReaderTransport::new(command_tx.clone());
        let resizes = ResizeMailbox::default();
        let worker_resizes = resizes.clone();
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
                    geometry,
                    command_rx,
                    reader_transport,
                    worker_resizes,
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

        let worker = thread::Builder::new()
            .name("spaceterm-terminal".to_owned())
            .spawn(move || {
                TerminalWorker::run(
                    pty,
                    geometry,
                    command_rx,
                    reader_transport,
                    worker_resizes,
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

    pub(crate) fn scroll_to(&self, offset_rows: u64) {
        if let Some(commands) = &self.commands
            && commands.send(Command::ScrollTo(offset_rows)).is_err()
        {
            eprintln!("terminal scrollbar input was dropped because the worker has stopped");
        }
    }

    pub(crate) fn paste(&self, text: String) {
        if let Some(commands) = &self.commands
            && commands.send(Command::Paste(text)).is_err()
        {
            eprintln!("terminal paste was dropped because the worker has stopped");
        }
    }

    pub(crate) fn request_selection_text(
        &self,
    ) -> async_channel::Receiver<Result<Option<String>, String>> {
        let (reply, receiver) = async_channel::bounded(1);
        let sent = self
            .commands
            .as_ref()
            .is_some_and(|commands| commands.send(Command::SelectionText(reply.clone())).is_ok());
        if !sent {
            let _ = reply.try_send(Err(
                "terminal selection could not be read because the worker has stopped".to_owned(),
            ));
        }
        receiver
    }

    fn shutdown(&mut self) {
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
        // Dropping a JoinHandle detaches the worker. It still owns the PTY and reader
        // cleanup, but a close operation must never block its GPUI caller on either thread.
        drop(self.worker.take());
    }
}

impl TerminalSessionHandle for TerminalSession {
    fn key(&self, input: KeyInput) {
        Self::key(self, input);
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

    fn scroll_to(&self, offset_rows: u64) {
        Self::scroll_to(self, offset_rows);
    }

    fn paste(&self, text: String) {
        Self::paste(self, text);
    }

    fn request_selection_text(&self) -> async_channel::Receiver<Result<Option<String>, String>> {
        Self::request_selection_text(self)
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
    Resize,
    Pointer(PointerInput),
    Wheel(WheelInput),
    ScrollTo(u64),
    Paste(String),
    SelectionText(async_channel::Sender<Result<Option<String>, String>>),
    ReaderReady,
    Shutdown,
}

struct TerminalWorker {
    pty: Box<dyn SessionPty>,
    emulator: TerminalEmulator,
    commands: CommandReceiver<Command>,
    reader_events: mpsc::Receiver<ReaderEvent>,
    reader_thread: JoinHandle<()>,
    events: async_channel::Sender<SessionEvent>,
    resizes: ResizeMailbox,
    pending_command: Option<Command>,
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
        initial_geometry: TerminalGeometry,
        commands: CommandReceiver<Command>,
        reader_transport: ReaderTransport,
        resizes: ResizeMailbox,
        events: async_channel::Sender<SessionEvent>,
        startup: StartupReporter,
    ) {
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

        let emulator = match TerminalEmulator::new(initial_geometry) {
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
            pending_command: None,
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
            let command = match self.pending_command.take() {
                Some(command) => command,
                None => match self.commands.recv() {
                    Ok(command) => command,
                    Err(_) => break,
                },
            };

            if !self.process_command(command) {
                break;
            }
        }
    }

    fn process_command(&mut self, command: Command) -> bool {
        match command {
            Command::Key(input) => match self.emulator.key(input) {
                Ok(action) => self.apply_emulator_action(action),
                Err(message) => {
                    self.send_runtime_failure(message);
                    false
                }
            },
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
                    Ok(()) => self.apply_emulator_action(EmulatorAction::screen_changed()),
                    Err(message) => {
                        self.send_runtime_failure(message);
                        false
                    }
                }
            }
            Command::Pointer(input) => match self.emulator.pointer(input) {
                Ok(action) => self.apply_emulator_action(action),
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
            Command::ScrollTo(offset_rows) => {
                let action = self.emulator.scroll_to(offset_rows);
                self.apply_emulator_action(action)
            }
            Command::Paste(text) => match self.emulator.paste(text) {
                Ok(action) => self.apply_emulator_action(action),
                Err(message) => {
                    self.send_runtime_failure(message);
                    false
                }
            },
            Command::SelectionText(reply) => {
                let _ = reply.try_send(self.emulator.selection_text());
                true
            }
            Command::Shutdown => false,
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
            let event = classify_reader_stop(
                read_error,
                self.pty.wait_for_child(FINAL_CHILD_WAIT_TIMEOUT),
            );
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
        let received_output = !chunks.is_empty();
        for bytes in chunks {
            self.emulator.feed(&bytes);
        }

        if received_output && (!self.write_pending_pty_responses() || !self.publish_screen()) {
            return false;
        }
        true
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
        } = self;
        // SpawnedPty's Drop terminates and reaps a live shell for the native Adapter.
        drop(reader_events);
        drop(pty);
        join_reader(reader_thread);
    }
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
    use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize};
    use crate::terminal::key::{KeyAction, PhysicalKey};

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
            pending_command: None,
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
        session.paste("later".to_owned());
        let state = records.wait_for("the later terminal input", |state| {
            state.written.ends_with(b"later")
        });

        assert_eq!(state.written, b"\x1b[48;4;20;72;160tlater");
        session.shutdown();
    }

    #[test]
    fn write_failure_should_emit_a_runtime_failure_and_stop_the_worker() {
        let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
            write_error: Some("write unavailable".to_owned()),
            ..ScriptedPtyOptions::default()
        });
        let (mut session, events) = result.unwrap();

        session.paste("input".to_owned());
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
        };

        let result = session.request_selection_text().try_recv().unwrap();
        assert!(matches!(result, Err(message) if message.contains("worker has stopped")));
    }

    #[test]
    fn real_shell_output_round_trips_through_the_pty_and_emulator() {
        let size = test_geometry();
        let StartedTerminalSession {
            handle: session,
            events,
        } = NativeTerminalSessionFactory
            .start(size, &std::env::current_dir().unwrap())
            .unwrap();

        // The command renders a red X. The echoed command contains an X too, but
        // only the shell's output passes through the SGR sequence and becomes red.
        session.paste("printf '\\033[31mX\\033[0m\\n'\n".to_owned());

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_red_x = false;
        while Instant::now() < deadline && !saw_red_x {
            match events.try_recv() {
                Ok(SessionEvent::Screen(screen)) => {
                    saw_red_x = screen.rows.iter().flat_map(|row| row.iter()).any(|cell| {
                        cell.text == "X"
                            && cell.foreground == crate::theme::ACTIVE_THEME.terminal_normal()[1]
                    });
                }
                Ok(SessionEvent::Failed(failure)) => panic!("terminal session failed: {failure}"),
                Ok(SessionEvent::Exited(status)) => panic!("shell exited early: {status}"),
                Err(async_channel::TryRecvError::Empty) => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(async_channel::TryRecvError::Closed) => break,
            }
        }

        drop(session);
        assert!(
            saw_red_x,
            "did not receive colored output from the real shell"
        );
    }

    #[test]
    fn real_shell_exit_command_emits_an_exited_event() {
        let size = test_geometry();
        let StartedTerminalSession {
            handle: session,
            events,
        } = NativeTerminalSessionFactory
            .start(size, &std::env::current_dir().unwrap())
            .unwrap();

        session.paste("exit\n".to_owned());

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exit_status = None;
        while Instant::now() < deadline && exit_status.is_none() {
            match events.try_recv() {
                Ok(SessionEvent::Screen(_)) => {}
                Ok(SessionEvent::Exited(status)) => exit_status = Some(status),
                Ok(SessionEvent::Failed(failure)) => panic!("terminal session failed: {failure}"),
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
}
