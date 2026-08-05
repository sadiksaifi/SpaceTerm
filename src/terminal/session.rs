use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver as CommandReceiver, Sender as CommandSender, TryRecvError};
use std::thread::{self, JoinHandle};

use anyhow::Error as AnyError;
use portable_pty::{ExitStatus, PtySize};
use thiserror::Error;

use crate::platform::macos_pty::{
    PtyError, PtyTerminator, SpawnedPty, spawn_user_shell, user_shell,
};
use crate::terminal::emulator::{EmulatorAction, ScreenSnapshot, TerminalEmulator};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct SurfacePosition {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct InputModifiers {
    pub(crate) shift: bool,
    pub(crate) alt: bool,
    pub(crate) control: bool,
    pub(crate) platform: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeyCode {
    Character(char),
    Enter,
    Backspace,
    Tab,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KeyInput {
    pub(crate) code: KeyCode,
    pub(crate) text: Option<String>,
    pub(crate) modifiers: InputModifiers,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GridSize {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) cell_width_px: u16,
    pub(crate) cell_height_px: u16,
}

impl GridSize {
    fn pty_size(self) -> PtySize {
        PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: self.cols.saturating_mul(self.cell_width_px),
            pixel_height: self.rows.saturating_mul(self.cell_height_px),
        }
    }
}

// Screen events may supersede older screens. Error and Exited are final events,
// so the worker must not publish another screen after either one.
#[derive(Clone, Debug)]
pub(crate) enum SessionEvent {
    Screen(Arc<ScreenSnapshot>),
    Exited(String),
    Error(String),
}

#[derive(Debug, Error)]
pub(crate) enum SessionError {
    #[error(transparent)]
    Pty(#[from] PtyError),
    #[error("failed to start the terminal worker thread: {0}")]
    SpawnWorker(#[source] std::io::Error),
    #[error("terminal worker stopped before initialization completed")]
    StartupChannelClosed,
    #[error("terminal emulator initialization failed: {0}")]
    EmulatorStartup(String),
}

pub(crate) struct StartedTerminalSession {
    pub(crate) handle: Box<dyn TerminalSessionHandle>,
    pub(crate) events: async_channel::Receiver<SessionEvent>,
}

pub(crate) trait TerminalSessionHandle {
    fn key(&self, input: KeyInput);
    fn resize(&self, size: GridSize);
    fn pointer(&self, input: PointerInput);
    fn wheel(&self, input: WheelInput);
    fn scroll_to(&self, offset_rows: u64);
    fn paste(&self, text: String);
    fn request_selection_text(&self) -> async_channel::Receiver<Result<Option<String>, String>>;
}

pub(crate) trait TerminalSessionFactory {
    fn start(
        &self,
        size: GridSize,
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
        size: GridSize,
        working_directory: &Path,
    ) -> Result<StartedTerminalSession, SessionError> {
        let (session, events) = TerminalSession::start(size, working_directory)?;
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

pub(crate) struct TerminalSession {
    commands: Option<CommandSender<Command>>,
    worker: Option<JoinHandle<()>>,
    terminator: Option<Box<dyn SessionPtyTerminator>>,
}

trait SessionPty: Write + Send {
    fn take_reader(&mut self) -> std::io::Result<Box<dyn Read + Send>>;
    fn resize(&self, size: PtySize) -> Result<(), AnyError>;
    fn wait_for_child(&mut self) -> std::io::Result<ExitStatus>;
}

trait SessionPtyTerminator: Send + Sync {
    fn terminate(&self) -> std::io::Result<()>;
}

struct StartedSessionPty {
    pty: Box<dyn SessionPty>,
    terminator: Box<dyn SessionPtyTerminator>,
}

struct NativeSessionPty(SpawnedPty);

impl Write for NativeSessionPty {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl SessionPty for NativeSessionPty {
    fn take_reader(&mut self) -> std::io::Result<Box<dyn Read + Send>> {
        self.0.take_reader()
    }

    fn resize(&self, size: PtySize) -> Result<(), AnyError> {
        self.0.resize(size)
    }

    fn wait_for_child(&mut self) -> std::io::Result<ExitStatus> {
        self.0.wait_for_child()
    }
}

struct NativeSessionPtyTerminator(PtyTerminator);

impl SessionPtyTerminator for NativeSessionPtyTerminator {
    fn terminate(&self) -> std::io::Result<()> {
        self.0.terminate()
    }
}

fn spawn_native_session_pty(
    size: PtySize,
    working_directory: &Path,
) -> Result<StartedSessionPty, PtyError> {
    let (pty, terminator) = spawn_user_shell(size, working_directory)?;
    Ok(StartedSessionPty {
        pty: Box::new(NativeSessionPty(pty)),
        terminator: Box::new(NativeSessionPtyTerminator(terminator)),
    })
}

impl TerminalSession {
    pub(crate) fn start(
        size: GridSize,
        working_directory: &Path,
    ) -> Result<(Self, async_channel::Receiver<SessionEvent>), SessionError> {
        Self::start_with(size, working_directory, spawn_native_session_pty)
    }

    fn start_with(
        size: GridSize,
        working_directory: &Path,
        spawn_pty: impl FnOnce(PtySize, &Path) -> Result<StartedSessionPty, PtyError>,
    ) -> Result<(Self, async_channel::Receiver<SessionEvent>), SessionError> {
        let StartedSessionPty { pty, terminator } = spawn_pty(size.pty_size(), working_directory)?;
        let (command_tx, command_rx) = mpsc::channel();
        // Two slots retain the latest screen and a final lifecycle event without
        // allowing sustained PTY output to build an unbounded UI backlog.
        let (event_tx, event_rx) = async_channel::bounded(2);
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);

        let reader_commands = command_tx.clone();
        let worker = thread::Builder::new()
            .name("spaceterm-terminal".to_owned())
            .spawn(move || run_worker(pty, size, command_rx, reader_commands, event_tx, startup_tx))
            .map_err(SessionError::SpawnWorker)?;

        match startup_rx.recv() {
            Ok(Ok(())) => Ok((
                Self {
                    commands: Some(command_tx),
                    worker: Some(worker),
                    terminator: Some(terminator),
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

    pub(crate) fn resize(&self, size: GridSize) {
        if let Some(commands) = &self.commands
            && commands.send(Command::Resize(size)).is_err()
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
        if let Some(commands) = self.commands.take()
            && commands.send(Command::Shutdown).is_err()
        {
            // The worker already stopped, so there is nothing left to signal.
        }
        if let Some(terminator) = self.terminator.take()
            && let Err(error) = terminator.terminate()
        {
            eprintln!("failed to terminate shell while shutting down terminal worker: {error}");
        }
        if let Some(worker) = self.worker.take() {
            join_worker(worker);
        }
    }
}

impl TerminalSessionHandle for TerminalSession {
    fn key(&self, input: KeyInput) {
        Self::key(self, input);
    }

    fn resize(&self, size: GridSize) {
        Self::resize(self, size);
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

#[derive(Debug)]
enum Command {
    Key(KeyInput),
    Output(Vec<u8>),
    Resize(GridSize),
    Pointer(PointerInput),
    Wheel(WheelInput),
    ScrollTo(u64),
    Paste(String),
    SelectionText(async_channel::Sender<Result<Option<String>, String>>),
    ReaderStopped(Option<String>),
    Shutdown,
}

fn run_worker(
    mut pty: Box<dyn SessionPty>,
    initial_size: GridSize,
    commands: CommandReceiver<Command>,
    command_tx: CommandSender<Command>,
    events: async_channel::Sender<SessionEvent>,
    startup: mpsc::SyncSender<Result<(), String>>,
) {
    let reader = match pty.take_reader() {
        Ok(reader) => reader,
        Err(error) => {
            let _ = startup.send(Err(error.to_string()));
            return;
        }
    };

    let reader_thread = match spawn_reader(reader, command_tx) {
        Ok(thread) => thread,
        Err(error) => {
            let _ = startup.send(Err(format!("failed to start PTY reader thread: {error}")));
            return;
        }
    };

    let mut emulator = match TerminalEmulator::new(
        initial_size.cols,
        initial_size.rows,
        u32::from(initial_size.cell_width_px),
        u32::from(initial_size.cell_height_px),
    ) {
        Ok(emulator) => emulator,
        Err(error) => {
            let _ = startup.send(Err(error.to_string()));
            drop(pty);
            join_reader(reader_thread);
            return;
        }
    };

    if startup.send(Ok(())).is_err() {
        drop(pty);
        join_reader(reader_thread);
        return;
    }

    if !publish_screen(&mut emulator, &events) {
        drop(pty);
        join_reader(reader_thread);
        return;
    }

    let mut pending_command = None;
    loop {
        let command = match pending_command.take() {
            Some(command) => command,
            None => match commands.recv() {
                Ok(command) => command,
                Err(_) => break,
            },
        };

        let keep_running = match command {
            Command::Key(input) => match emulator.key(input) {
                Ok(action) => apply_emulator_action(action, &mut emulator, &mut pty, &events),
                Err(message) => {
                    send_error(&events, message);
                    false
                }
            },
            Command::Output(bytes) => process_output(
                bytes,
                &commands,
                &mut pending_command,
                &mut emulator,
                &mut pty,
                &events,
            ),
            Command::Resize(size) => {
                let result = pty
                    .resize(size.pty_size())
                    .map_err(|error| {
                        format!("failed to resize the macOS pseudo-terminal: {error:#}")
                    })
                    .and_then(|()| {
                        emulator
                            .resize(
                                size.cols,
                                size.rows,
                                u32::from(size.cell_width_px),
                                u32::from(size.cell_height_px),
                            )
                            .map_err(|error| format!("failed to resize terminal state: {error}"))
                    });

                match result {
                    Ok(()) => apply_emulator_action(
                        EmulatorAction::screen_changed(),
                        &mut emulator,
                        &mut pty,
                        &events,
                    ),
                    Err(message) => {
                        send_error(&events, message);
                        false
                    }
                }
            }
            Command::Pointer(input) => match emulator.pointer(input) {
                Ok(action) => apply_emulator_action(action, &mut emulator, &mut pty, &events),
                Err(message) => {
                    send_error(&events, message);
                    false
                }
            },
            Command::Wheel(input) => match emulator.wheel(input) {
                Ok(action) => apply_emulator_action(action, &mut emulator, &mut pty, &events),
                Err(message) => {
                    send_error(&events, message);
                    false
                }
            },
            Command::ScrollTo(offset_rows) => apply_emulator_action(
                emulator.scroll_to(offset_rows),
                &mut emulator,
                &mut pty,
                &events,
            ),
            Command::Paste(text) => match emulator.paste(text) {
                Ok(action) => apply_emulator_action(action, &mut emulator, &mut pty, &events),
                Err(message) => {
                    send_error(&events, message);
                    false
                }
            },
            Command::SelectionText(reply) => {
                let _ = reply.try_send(emulator.selection_text());
                true
            }
            Command::ReaderStopped(read_error) => {
                let status = match pty.wait_for_child() {
                    Ok(status) => format!("Shell exited ({status:?})"),
                    Err(wait_error) => match read_error {
                        Some(read_error) => format!(
                            "Shell output stopped: {read_error}; process wait failed: {wait_error}"
                        ),
                        None => format!("Shell output stopped; process wait failed: {wait_error}"),
                    },
                };
                send_terminal_event(&events, SessionEvent::Exited(status));
                false
            }
            Command::Shutdown => false,
        };

        if !keep_running {
            break;
        }
    }

    // The native SessionPty owns SpawnedPty, whose Drop terminates and reaps a live shell.
    drop(pty);
    join_reader(reader_thread);
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    commands: CommandSender<Command>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("spaceterm-pty-reader".to_owned())
        .spawn(move || {
            let mut buffer = [0_u8; 16 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        let _ = commands.send(Command::ReaderStopped(None));
                        break;
                    }
                    Ok(read) => {
                        if commands
                            .send(Command::Output(buffer[..read].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(error) => {
                        let _ = commands.send(Command::ReaderStopped(Some(error.to_string())));
                        break;
                    }
                }
            }
        })
}

fn process_output(
    first: Vec<u8>,
    commands: &CommandReceiver<Command>,
    pending_command: &mut Option<Command>,
    emulator: &mut TerminalEmulator,
    writer: &mut dyn Write,
    events: &async_channel::Sender<SessionEvent>,
) -> bool {
    emulator.feed(&first);
    if !write_pending_pty_responses(emulator, writer, events) {
        return false;
    }

    let commands_open = loop {
        match commands.try_recv() {
            Ok(Command::Output(bytes)) => {
                emulator.feed(&bytes);
                if !write_pending_pty_responses(emulator, writer, events) {
                    return false;
                }
            }
            Ok(command) => {
                *pending_command = Some(command);
                break true;
            }
            Err(TryRecvError::Empty) => break true,
            Err(TryRecvError::Disconnected) => break false,
        }
    };

    publish_screen(emulator, events) && commands_open
}

fn apply_emulator_action(
    action: EmulatorAction,
    emulator: &mut TerminalEmulator,
    writer: &mut dyn Write,
    events: &async_channel::Sender<SessionEvent>,
) -> bool {
    write_pending_pty_responses(emulator, writer, events)
        && (action.bytes.is_empty() || write_pty(writer, &action.bytes, events))
        && (!action.screen_changed || publish_screen(emulator, events))
}

fn write_pending_pty_responses(
    emulator: &TerminalEmulator,
    writer: &mut dyn Write,
    events: &async_channel::Sender<SessionEvent>,
) -> bool {
    let responses = emulator.take_pty_responses();
    responses.is_empty() || write_pty(writer, &responses, events)
}

fn write_pty(
    writer: &mut dyn Write,
    bytes: &[u8],
    events: &async_channel::Sender<SessionEvent>,
) -> bool {
    if let Err(error) = writer.write_all(bytes).and_then(|()| writer.flush()) {
        let _ = send_error(events, format!("failed to write to the shell PTY: {error}"));
        return false;
    }
    true
}

fn publish_screen(
    emulator: &mut TerminalEmulator,
    events: &async_channel::Sender<SessionEvent>,
) -> bool {
    match emulator.snapshot() {
        Ok(Some(snapshot)) => events.force_send(SessionEvent::Screen(snapshot)).is_ok(),
        Ok(None) => true,
        Err(error) => {
            send_error(
                events,
                format!("failed to produce terminal screen snapshot: {error}"),
            );
            false
        }
    }
}

fn send_error(events: &async_channel::Sender<SessionEvent>, message: String) -> bool {
    send_terminal_event(events, SessionEvent::Error(message))
}

fn send_terminal_event(events: &async_channel::Sender<SessionEvent>, event: SessionEvent) -> bool {
    match events.try_send(event) {
        Ok(()) => true,
        Err(async_channel::TrySendError::Full(event)) => events.force_send(event).is_ok(),
        Err(async_channel::TrySendError::Closed(_)) => false,
    }
}

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

    const TEST_SIZE: GridSize = GridSize {
        cols: 80,
        rows: 24,
        cell_width_px: 8,
        cell_height_px: 20,
    };

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
        terminator_drops: usize,
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
        Eof,
    }

    struct ScriptedReader {
        steps: mpsc::Receiver<ReaderStep>,
        pending: VecDeque<u8>,
    }

    impl Read for ScriptedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            while self.pending.is_empty() {
                match self.steps.recv() {
                    Ok(ReaderStep::Bytes(bytes)) => self.pending.extend(bytes),
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

    #[derive(Default)]
    struct ScriptedPtyOptions {
        reader_error: Option<String>,
        resize_error: Option<String>,
        write_error: Option<String>,
        wait_error: Option<String>,
        exit_code: u32,
    }

    struct ScriptedPty {
        reader: Option<Box<dyn Read + Send>>,
        records: ScriptedPtyRecords,
        reader_error: Option<String>,
        resize_error: Option<String>,
        write_error: Option<String>,
        wait_error: Option<String>,
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

        fn wait_for_child(&mut self) -> io::Result<ExitStatus> {
            self.records.update(|state| state.waits += 1);
            match &self.wait_error {
                Some(message) => Err(io::Error::other(message.clone())),
                None => Ok(ExitStatus::with_exit_code(self.exit_code)),
            }
        }
    }

    impl Drop for ScriptedPty {
        fn drop(&mut self) {
            self.records.update(|state| state.pty_drops += 1);
        }
    }

    struct ScriptedPtyTerminator {
        records: ScriptedPtyRecords,
        reader_steps: mpsc::Sender<ReaderStep>,
    }

    impl SessionPtyTerminator for ScriptedPtyTerminator {
        fn terminate(&self) -> io::Result<()> {
            self.records.update(|state| state.terminations += 1);
            let _ = self.reader_steps.send(ReaderStep::Eof);
            Ok(())
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
            exit_code,
        } = options;

        let result = TerminalSession::start_with(
            TEST_SIZE,
            Path::new("/scripted"),
            move |size, working_directory| {
                assert_eq!(size, TEST_SIZE.pty_size());
                assert_eq!(working_directory, Path::new("/scripted"));
                Ok(StartedSessionPty {
                    pty: Box::new(ScriptedPty {
                        reader: Some(Box::new(ScriptedReader {
                            steps,
                            pending: VecDeque::new(),
                        })),
                        records: records_for_pty,
                        reader_error,
                        resize_error,
                        write_error,
                        wait_error,
                        exit_code,
                    }),
                    terminator: Box::new(ScriptedPtyTerminator {
                        records: records_for_terminator,
                        reader_steps: terminator_steps,
                    }),
                })
            },
        );

        (result, reader_steps, records)
    }

    fn receive_event(
        events: &async_channel::Receiver<SessionEvent>,
        description: &str,
        predicate: impl Fn(&SessionEvent) -> bool,
    ) -> SessionEvent {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match events.try_recv() {
                Ok(event) if predicate(&event) => return event,
                Ok(_) | Err(async_channel::TryRecvError::Empty) => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for {description}"
                    );
                    thread::sleep(Duration::from_millis(1));
                }
                Err(async_channel::TryRecvError::Closed) => {
                    panic!("session events closed while waiting for {description}")
                }
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

    #[derive(Clone, Debug)]
    struct RecordingWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for RecordingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn terminal_events_preserve_the_latest_screen() {
        let (events, receiver) = async_channel::bounded(2);
        let first = ScreenSnapshot::empty();
        let second = ScreenSnapshot::empty();

        events
            .force_send(SessionEvent::Screen(Arc::clone(&first)))
            .unwrap();
        events
            .force_send(SessionEvent::Screen(Arc::clone(&second)))
            .unwrap();
        assert!(send_terminal_event(
            &events,
            SessionEvent::Exited("done".to_owned())
        ));

        match receiver.try_recv().unwrap() {
            SessionEvent::Screen(screen) => assert!(Arc::ptr_eq(&screen, &second)),
            event => panic!("expected latest screen, got {event:?}"),
        }
        assert!(matches!(
            receiver.try_recv().unwrap(),
            SessionEvent::Exited(status) if status == "done"
        ));
        assert!(receiver.try_recv().is_err());
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
        let state = records.snapshot();
        assert_eq!(
            (state.terminations, state.pty_drops, state.terminator_drops),
            (1, 1, 1)
        );
    }

    #[test]
    fn consecutive_output_commands_should_publish_one_coalesced_screen() {
        let mut emulator = TerminalEmulator::new(80, 24, 8, 20).unwrap();
        let (command_tx, commands) = mpsc::channel();
        command_tx
            .send(Command::Output(b" second".to_vec()))
            .unwrap();
        let (reply, _reply_receiver) = async_channel::bounded(1);
        command_tx.send(Command::SelectionText(reply)).unwrap();
        let mut pending_command = None;
        let mut writer = io::sink();
        let (events, receiver) = async_channel::bounded(2);

        assert!(process_output(
            b"first".to_vec(),
            &commands,
            &mut pending_command,
            &mut emulator,
            &mut writer,
            &events,
        ));

        let SessionEvent::Screen(screen) = receiver.try_recv().unwrap() else {
            panic!("coalesced output must publish a terminal screen")
        };
        assert!(screen_text(&screen).contains("first second"));
        assert!(receiver.try_recv().is_err());
        assert!(matches!(pending_command, Some(Command::SelectionText(_))));
    }

    #[test]
    fn resize_should_reach_the_pty_with_pixel_dimensions() {
        let (result, _reader_steps, records) =
            start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, _events) = result.unwrap();
        let resized = GridSize {
            cols: 100,
            rows: 30,
            cell_width_px: 9,
            cell_height_px: 21,
        };

        session.resize(resized);
        let state = records.wait_for("the scripted PTY resize", |state| state.resizes.len() == 1);

        assert_eq!(state.resizes, vec![resized.pty_size()]);
        session.shutdown();
    }

    #[test]
    fn pending_pty_responses_are_written_before_action_input() {
        let mut emulator = TerminalEmulator::new(10, 2, 10, 20).unwrap();
        emulator.feed(b"\x1b[?2048h");
        emulator.resize(20, 4, 8, 18).unwrap();
        let written = Arc::new(Mutex::new(Vec::new()));
        let mut writer: Box<dyn Write + Send> = Box::new(RecordingWriter {
            bytes: Arc::clone(&written),
        });
        let (events, _receiver) = async_channel::bounded(2);

        assert!(apply_emulator_action(
            EmulatorAction {
                bytes: b"later".to_vec(),
                screen_changed: false,
            },
            &mut emulator,
            &mut writer,
            &events,
        ));

        assert_eq!(
            written.lock().unwrap().as_slice(),
            b"\x1b[48;4;20;72;160tlater"
        );
    }

    #[test]
    fn write_failure_should_emit_the_existing_error_event_and_stop_the_worker() {
        let (result, _reader_steps, records) = start_scripted_session(ScriptedPtyOptions {
            write_error: Some("write unavailable".to_owned()),
            ..ScriptedPtyOptions::default()
        });
        let (mut session, events) = result.unwrap();

        session.paste("input".to_owned());
        let event = receive_event(&events, "the PTY write failure", |event| {
            matches!(event, SessionEvent::Error(_))
        });

        assert!(matches!(
            event,
            SessionEvent::Error(message)
                if message == "failed to write to the shell PTY: write unavailable"
        ));
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
            SessionEvent::Exited(status) if status.starts_with("Shell exited")
        ));
        let state = records.wait_for("the exited PTY worker to release ownership", |state| {
            state.pty_drops == 1
        });
        assert_eq!((state.waits, state.pty_drops), (1, 1));

        session.shutdown();
    }

    #[test]
    fn repeated_shutdown_should_terminate_and_release_each_owner_once() {
        let (result, _reader_steps, records) =
            start_scripted_session(ScriptedPtyOptions::default());
        let (mut session, _events) = result.unwrap();

        session.shutdown();
        session.shutdown();

        let state = records.snapshot();
        assert_eq!(
            (
                state.terminations,
                state.pty_drops,
                state.terminator_drops,
                session.commands.is_none(),
                session.worker.is_none(),
                session.terminator.is_none(),
            ),
            (1, 1, 1, true, true, true)
        );
    }

    #[test]
    fn stopped_session_returns_an_error_for_selection_requests() {
        let session = TerminalSession {
            commands: None,
            worker: None,
            terminator: None,
        };

        let result = session.request_selection_text().try_recv().unwrap();
        assert!(matches!(result, Err(message) if message.contains("worker has stopped")));
    }

    #[test]
    fn real_shell_output_round_trips_through_the_pty_and_emulator() {
        let size = GridSize {
            cols: 80,
            rows: 24,
            cell_width_px: 8,
            cell_height_px: 20,
        };
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
                Ok(SessionEvent::Error(error)) => panic!("terminal session failed: {error}"),
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
        let size = GridSize {
            cols: 80,
            rows: 24,
            cell_width_px: 8,
            cell_height_px: 20,
        };
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
                Ok(SessionEvent::Error(error)) => panic!("terminal session failed: {error}"),
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
