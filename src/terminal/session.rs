use std::io::{ErrorKind, Read, Write};
use std::sync::mpsc::{self, Receiver as CommandReceiver, Sender as CommandSender};
use std::thread::{self, JoinHandle};

use portable_pty::PtySize;
use thiserror::Error;

use crate::platform::macos_pty::{PtyError, SpawnedPty, spawn_user_shell};
use crate::terminal::emulator::{ScreenSnapshot, TerminalEmulator};

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
            pixel_width: self.cell_width_px,
            pixel_height: self.cell_height_px,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum SessionEvent {
    Screen(ScreenSnapshot),
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
        let (event_tx, event_rx) = async_channel::unbounded();
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

    let mut emulator = match TerminalEmulator::new(initial_size.cols, initial_size.rows) {
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

    while let Ok(command) = commands.recv() {
        let keep_running = match command {
            Command::Input(bytes) => write_pty(&mut pty.writer, &bytes, &events),
            Command::Output(bytes) => {
                emulator.feed(&bytes);
                let responses = emulator.take_pty_responses();
                (responses.is_empty() || write_pty(&mut pty.writer, &responses, &events))
                    && publish_screen(&mut emulator, &events)
            }
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
                    Err(message) => send_error(&events, message),
                }
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
                let _ = events.send_blocking(SessionEvent::Exited(status));
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
        Ok(snapshot) => events.send_blocking(SessionEvent::Screen(snapshot)).is_ok(),
        Err(error) => send_error(
            events,
            format!("failed to produce terminal screen snapshot: {error}"),
        ),
    }
}

fn send_error(events: &async_channel::Sender<SessionEvent>, message: String) -> bool {
    events.send_blocking(SessionEvent::Error(message)).is_ok()
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
                    saw_red_x = screen.rows.iter().flatten().any(|cell| {
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
