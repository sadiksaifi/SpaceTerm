use std::io::{ErrorKind, Read, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver as CommandReceiver, Sender as CommandSender, TryRecvError};
use std::thread::{self, JoinHandle};

use portable_pty::PtySize;
use thiserror::Error;

use crate::platform::macos_pty::{PtyError, SpawnedPty, spawn_user_shell};
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

#[allow(
    dead_code,
    reason = "worker-side interaction contract is not wired to the UI in this milestone"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PointerButton {
    Left,
    Middle,
    Right,
}

#[allow(
    dead_code,
    reason = "worker-side interaction contract is not wired to the UI in this milestone"
)]
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

pub(crate) struct TerminalSession {
    commands: Option<CommandSender<Command>>,
    worker: Option<JoinHandle<()>>,
}

impl TerminalSession {
    pub(crate) fn start(
        size: GridSize,
    ) -> Result<(Self, async_channel::Receiver<SessionEvent>), SessionError> {
        let pty = spawn_user_shell(size.pty_size())?;
        let (command_tx, command_rx) = mpsc::channel();
        // Two slots retain the latest screen and a final lifecycle event without
        // allowing sustained PTY output to build an unbounded UI backlog.
        let (event_tx, event_rx) = async_channel::bounded(2);
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);

        let reader_commands = command_tx.clone();
        let worker = thread::Builder::new()
            .name("termspace-terminal".to_owned())
            .spawn(move || run_worker(pty, size, command_rx, reader_commands, event_tx, startup_tx))
            .map_err(SessionError::SpawnWorker)?;

        match startup_rx.recv() {
            Ok(Ok(())) => Ok((
                Self {
                    commands: Some(command_tx),
                    worker: Some(worker),
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

    pub(crate) fn send_input(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        if let Some(commands) = &self.commands
            && commands.send(Command::Input(bytes)).is_err()
        {
            eprintln!("terminal input was dropped because the worker has stopped");
        }
    }

    pub(crate) fn resize(&self, size: GridSize) {
        if let Some(commands) = &self.commands
            && commands.send(Command::Resize(size)).is_err()
        {
            eprintln!("terminal resize was dropped because the worker has stopped");
        }
    }

    #[allow(
        dead_code,
        reason = "worker-side interaction contract is not wired to the UI in this milestone"
    )]
    pub(crate) fn pointer(&self, input: PointerInput) {
        if let Some(commands) = &self.commands
            && commands.send(Command::Pointer(input)).is_err()
        {
            eprintln!("terminal pointer input was dropped because the worker has stopped");
        }
    }

    #[allow(
        dead_code,
        reason = "worker-side interaction contract is not wired to the UI in this milestone"
    )]
    pub(crate) fn wheel(&self, input: WheelInput) {
        if let Some(commands) = &self.commands
            && commands.send(Command::Wheel(input)).is_err()
        {
            eprintln!("terminal wheel input was dropped because the worker has stopped");
        }
    }

    #[allow(
        dead_code,
        reason = "worker-side interaction contract is not wired to the UI in this milestone"
    )]
    pub(crate) fn paste(&self, text: String) {
        if let Some(commands) = &self.commands
            && commands.send(Command::Paste(text)).is_err()
        {
            eprintln!("terminal paste was dropped because the worker has stopped");
        }
    }

    #[allow(
        dead_code,
        reason = "worker-side interaction contract is not wired to the UI in this milestone"
    )]
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
        if let Some(worker) = self.worker.take() {
            join_worker(worker);
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Debug)]
enum Command {
    Input(Vec<u8>),
    Output(Vec<u8>),
    Resize(GridSize),
    #[allow(
        dead_code,
        reason = "worker-side interaction contract is not wired to the UI in this milestone"
    )]
    Pointer(PointerInput),
    #[allow(
        dead_code,
        reason = "worker-side interaction contract is not wired to the UI in this milestone"
    )]
    Wheel(WheelInput),
    #[allow(
        dead_code,
        reason = "worker-side interaction contract is not wired to the UI in this milestone"
    )]
    Paste(String),
    #[allow(
        dead_code,
        reason = "worker-side interaction contract is not wired to the UI in this milestone"
    )]
    SelectionText(async_channel::Sender<Result<Option<String>, String>>),
    ReaderStopped(Option<String>),
    Shutdown,
}

fn run_worker(
    mut pty: SpawnedPty,
    initial_size: GridSize,
    commands: CommandReceiver<Command>,
    command_tx: CommandSender<Command>,
    events: async_channel::Sender<SessionEvent>,
    startup: mpsc::SyncSender<Result<(), String>>,
) {
    let Some(reader) = pty.reader.take() else {
        let _ = startup.send(Err("PTY reader was already taken".to_owned()));
        return;
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
            Command::Input(bytes) => write_pty(&mut pty.writer, &bytes, &events),
            Command::Output(bytes) => process_output(
                bytes,
                &commands,
                &mut pending_command,
                &mut emulator,
                &mut pty.writer,
                &events,
            ),
            Command::Resize(size) => {
                let pty_resized = pty.master.resize(size.pty_size()).map_err(|error| {
                    format!("failed to resize the macOS pseudo-terminal: {error:#}")
                });
                let emulator_resized = emulator
                    .resize(
                        size.cols,
                        size.rows,
                        u32::from(size.cell_width_px),
                        u32::from(size.cell_height_px),
                    )
                    .map_err(|error| format!("failed to resize terminal state: {error}"));

                match pty_resized.and(emulator_resized) {
                    Ok(()) => publish_screen(&mut emulator, &events),
                    Err(message) => {
                        send_error(&events, message);
                        false
                    }
                }
            }
            Command::Pointer(input) => match emulator.pointer(input) {
                Ok(action) => {
                    apply_emulator_action(action, &mut emulator, &mut pty.writer, &events)
                }
                Err(message) => {
                    send_error(&events, message);
                    false
                }
            },
            Command::Wheel(input) => match emulator.wheel(input) {
                Ok(action) => {
                    apply_emulator_action(action, &mut emulator, &mut pty.writer, &events)
                }
                Err(message) => {
                    send_error(&events, message);
                    false
                }
            },
            Command::Paste(text) => match emulator.paste(text) {
                Ok(action) => {
                    apply_emulator_action(action, &mut emulator, &mut pty.writer, &events)
                }
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
                let status = match pty.child.wait() {
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

    // SpawnedPty::drop terminates and reaps a shell that is still running.
    drop(pty);
    join_reader(reader_thread);
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    commands: CommandSender<Command>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("termspace-pty-reader".to_owned())
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
    writer: &mut Box<dyn Write + Send>,
    events: &async_channel::Sender<SessionEvent>,
) -> bool {
    emulator.feed(&first);
    let commands_open = loop {
        match commands.try_recv() {
            Ok(Command::Output(bytes)) => emulator.feed(&bytes),
            Ok(command) => {
                *pending_command = Some(command);
                break true;
            }
            Err(TryRecvError::Empty) => break true,
            Err(TryRecvError::Disconnected) => break false,
        }
    };

    let responses = emulator.take_pty_responses();
    (responses.is_empty() || write_pty(writer, &responses, events))
        && publish_screen(emulator, events)
        && commands_open
}

fn apply_emulator_action(
    action: EmulatorAction,
    emulator: &mut TerminalEmulator,
    writer: &mut Box<dyn Write + Send>,
    events: &async_channel::Sender<SessionEvent>,
) -> bool {
    (action.bytes.is_empty() || write_pty(writer, &action.bytes, events))
        && (!action.screen_changed || publish_screen(emulator, events))
}

fn write_pty(
    writer: &mut Box<dyn Write + Send>,
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
    use std::time::{Duration, Instant};

    use super::*;

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
    fn stopped_session_returns_an_error_for_selection_requests() {
        let session = TerminalSession {
            commands: None,
            worker: None,
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
        let (session, events) = TerminalSession::start(size).unwrap();

        // The command renders a red X. The echoed command contains an X too, but
        // only the shell's output passes through the SGR sequence and becomes red.
        session.send_input(b"printf '\\033[31mX\\033[0m\\n'\r".to_vec());

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
}
