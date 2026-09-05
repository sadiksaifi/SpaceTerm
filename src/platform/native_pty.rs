use std::env;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use thiserror::Error;

use crate::ssh::command::SshCommandSpec;

const READ_BUFFER_SIZE: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NativePtySize {
    pub(crate) rows: u16,
    pub(crate) columns: u16,
    pub(crate) pixel_width: u16,
    pub(crate) pixel_height: u16,
}

pub(crate) enum NativePtyLaunch {
    Local {
        working_directory: PathBuf,
    },
    Remote {
        local_home: PathBuf,
        command: SshCommandSpec,
    },
}

impl fmt::Debug for NativePtyLaunch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct(match self {
                Self::Local { .. } => "LocalNativePtyLaunch",
                Self::Remote { .. } => "RemoteNativePtyLaunch",
            })
            .finish_non_exhaustive()
    }
}

impl NativePtyLaunch {
    pub(crate) fn local(working_directory: PathBuf) -> Self {
        Self::Local { working_directory }
    }

    pub(crate) fn remote(local_home: PathBuf, command: SshCommandSpec) -> Self {
        Self::Remote {
            local_home,
            command,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NativePtyExit {
    Success,
    ExitCode(u32),
    Signal(String),
    GracefulShutdown,
    ForcedShutdown,
}

impl fmt::Display for NativePtyExit {
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

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub(crate) struct NativePtyReadFailure {
    message: String,
}

impl NativePtyReadFailure {
    fn new(message: String) -> Self {
        Self { message }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub(crate) struct NativePtyWaitFailure {
    message: String,
}

impl NativePtyWaitFailure {
    pub(crate) fn new(message: String) -> Self {
        Self { message }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub(crate) struct NativePtyOperationFailure {
    message: String,
}

impl NativePtyOperationFailure {
    pub(crate) fn new(message: String) -> Self {
        Self { message }
    }
}

#[derive(Debug, Error)]
pub(crate) enum NativePtyStartupFailure {
    #[error("{0}")]
    Adapter(String),
    #[error("{0}")]
    Reader(String),
    #[error("failed to start PTY reader thread: {0}")]
    ReaderThread(#[source] io::Error),
}

impl NativePtyStartupFailure {
    pub(crate) const fn stage(&self) -> NativePtyStartupStage {
        match self {
            Self::Adapter(_) => NativePtyStartupStage::Adapter,
            Self::Reader(_) => NativePtyStartupStage::Reader,
            Self::ReaderThread(_) => NativePtyStartupStage::ReaderThread,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativePtyStartupStage {
    Adapter,
    Reader,
    ReaderThread,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NativePtyOutput {
    Bytes(Vec<u8>),
    Stopped(Option<NativePtyReadFailure>),
}

pub(crate) trait NativePtyOutputSink: Send + Sync + 'static {
    fn publish(&self, output: NativePtyOutput) -> bool;
}

pub(crate) trait NativePtyAdapter: Write + Send {
    fn take_reader(&mut self) -> io::Result<Box<dyn Read + Send>>;
    fn resize(&self, size: NativePtySize) -> Result<(), NativePtyOperationFailure>;
    fn hidden_input(&self) -> Result<bool, NativePtyOperationFailure> {
        Ok(false)
    }
    fn wait_for_exit(&mut self, timeout: Duration) -> Result<NativePtyExit, NativePtyWaitFailure>;
}

/// Constructs the platform-neutral parts owned by one Native PTY Owner.
///
/// Application composition selects the Operating-System-specific implementation. The Interface
/// deliberately exposes no descriptor, terminal attribute, signal, or process-group mechanism.
pub(crate) trait NativePtyAdapterFactory: Send + Sync {
    fn create(
        &self,
        launch: NativePtyLaunch,
        size: NativePtySize,
    ) -> Result<NativePtyAdapterParts, NativePtyAdapterConstructionFailure>;
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
/// A construction failure erased at the Operating-System Adapter boundary.
pub(crate) struct NativePtyAdapterConstructionFailure {
    message: String,
}

impl NativePtyAdapterConstructionFailure {
    pub(crate) fn new(message: String) -> Self {
        Self { message }
    }
}

pub(crate) trait NativePtyTermination: Send + Sync {
    fn request_termination(&self) -> io::Result<()>;
}

pub(crate) struct NativePtyAdapterParts {
    pub(crate) adapter: Box<dyn NativePtyAdapter>,
    pub(crate) termination: Arc<dyn NativePtyTermination>,
}

#[derive(Clone, Default)]
pub(crate) struct NativePtyCloseHandle {
    state: Arc<Mutex<NativePtyCloseState>>,
}

#[derive(Default)]
struct NativePtyCloseState {
    requested: bool,
    supervisor: Option<Sender<TerminationSupervisorCommand>>,
}

impl NativePtyCloseHandle {
    pub(crate) fn request_close(&self) -> io::Result<()> {
        let supervisor = {
            let mut state = self.lock_state();
            if state.requested {
                return Ok(());
            }
            state.requested = true;
            state.supervisor.take()
        };
        match supervisor {
            Some(supervisor) => {
                // A disconnected supervisor means owner-side cleanup already consumed or
                // superseded this idempotent close request.
                let _ = supervisor.send(TerminationSupervisorCommand::Close);
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn install(
        &self,
        termination: Arc<dyn NativePtyTermination>,
    ) -> io::Result<NativePtyTerminationSupervisor> {
        let (commands, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("spaceterm-pty-termination".to_owned())
            .spawn(move || match receiver.recv() {
                Ok(TerminationSupervisorCommand::Close) => {
                    if let Err(error) = termination.request_termination() {
                        eprintln!("failed to terminate shell while shutting down Native PTY Owner: {error}");
                    }
                }
                Ok(TerminationSupervisorCommand::Stop) | Err(_) => {}
            })?;

        let mut state = self.lock_state();
        if state.requested {
            commands
                .send(TerminationSupervisorCommand::Close)
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "Native PTY termination supervisor stopped",
                    )
                })?;
        } else {
            state.supervisor = Some(commands.clone());
        }
        Ok(NativePtyTerminationSupervisor {
            commands,
            close_state: Arc::downgrade(&self.state),
            thread: Some(thread),
        })
    }

    fn lock_state(&self) -> MutexGuard<'_, NativePtyCloseState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                eprintln!("Native PTY close coordination lock was poisoned; recovering");
                poisoned.into_inner()
            }
        }
    }
}

#[derive(Clone, Copy)]
enum TerminationSupervisorCommand {
    Close,
    Stop,
}

struct NativePtyTerminationSupervisor {
    commands: Sender<TerminationSupervisorCommand>,
    close_state: Weak<Mutex<NativePtyCloseState>>,
    thread: Option<JoinHandle<()>>,
}

impl NativePtyTerminationSupervisor {
    fn request_close(&self) {
        let _ = self.commands.send(TerminationSupervisorCommand::Close);
    }
}

impl Drop for NativePtyTerminationSupervisor {
    fn drop(&mut self) {
        let _ = self.commands.send(TerminationSupervisorCommand::Stop);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            eprintln!("Native PTY termination supervisor panicked");
        }
        if let Some(state) = self.close_state.upgrade() {
            match state.lock() {
                Ok(mut state) => state.supervisor = None,
                Err(poisoned) => poisoned.into_inner().supervisor = None,
            }
        }
    }
}

pub(crate) struct NativePtyOwner {
    adapter: Option<Box<dyn NativePtyAdapter>>,
    termination_supervisor: Option<NativePtyTerminationSupervisor>,
    reader_thread: Option<JoinHandle<()>>,
}

impl NativePtyOwner {
    pub(crate) fn start(
        adapter_factory: &dyn NativePtyAdapterFactory,
        launch: NativePtyLaunch,
        size: NativePtySize,
        output: Arc<dyn NativePtyOutputSink>,
        close_handle: &NativePtyCloseHandle,
    ) -> Result<Self, NativePtyStartupFailure> {
        let parts = adapter_factory
            .create(launch, size)
            .map_err(|error| NativePtyStartupFailure::Adapter(error.to_string()))?;
        Self::install_adapter_parts(parts, output, close_handle)
    }

    #[cfg(test)]
    pub(crate) fn from_adapter_parts(
        parts: NativePtyAdapterParts,
        output: Arc<dyn NativePtyOutputSink>,
        close_handle: &NativePtyCloseHandle,
    ) -> Result<Self, NativePtyStartupFailure> {
        Self::install_adapter_parts(parts, output, close_handle)
    }

    fn install_adapter_parts(
        mut parts: NativePtyAdapterParts,
        output: Arc<dyn NativePtyOutputSink>,
        close_handle: &NativePtyCloseHandle,
    ) -> Result<Self, NativePtyStartupFailure> {
        let termination_supervisor = close_handle
            .install(Arc::clone(&parts.termination))
            .map_err(|error| NativePtyStartupFailure::Adapter(error.to_string()))?;
        let reader = parts
            .adapter
            .take_reader()
            .map_err(|error| NativePtyStartupFailure::Reader(error.to_string()))?;
        let reader_thread =
            spawn_reader(reader, output).map_err(NativePtyStartupFailure::ReaderThread)?;
        Ok(Self {
            adapter: Some(parts.adapter),
            termination_supervisor: Some(termination_supervisor),
            reader_thread: Some(reader_thread),
        })
    }

    pub(crate) fn resize(&self, size: NativePtySize) -> Result<(), NativePtyOperationFailure> {
        self.adapter().resize(size)
    }

    pub(crate) fn hidden_input(&self) -> Result<bool, NativePtyOperationFailure> {
        self.adapter().hidden_input()
    }

    pub(crate) fn wait_for_exit(
        &mut self,
        timeout: Duration,
    ) -> Result<NativePtyExit, NativePtyWaitFailure> {
        self.adapter_mut().wait_for_exit(timeout)
    }

    fn adapter(&self) -> &dyn NativePtyAdapter {
        self.adapter
            .as_deref()
            .expect("Native PTY adapter exists until owner destruction")
    }

    fn adapter_mut(&mut self) -> &mut dyn NativePtyAdapter {
        self.adapter
            .as_deref_mut()
            .expect("Native PTY adapter exists until owner destruction")
    }
}

impl Write for NativePtyOwner {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.adapter_mut().write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.adapter_mut().flush()
    }
}

impl Drop for NativePtyOwner {
    fn drop(&mut self) {
        if let Some(supervisor) = &self.termination_supervisor {
            supervisor.request_close();
        }
        drop(self.termination_supervisor.take());
        drop(self.adapter.take());
        if let Some(reader_thread) = self.reader_thread.take()
            && reader_thread.join().is_err()
        {
            eprintln!("PTY reader thread panicked");
        }
    }
}

pub(crate) fn shell_fallback_title() -> String {
    let shell = user_shell();
    std::path::Path::new(&shell)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(&shell)
        .to_owned()
}

pub(super) fn user_shell() -> String {
    env::var("SHELL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_owned())
}

#[cfg(test)]
pub(crate) fn lock_real_pty_test() -> std::sync::MutexGuard<'static, ()> {
    crate::platform::macos_pty::lock_real_pty_test()
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    output: Arc<dyn NativePtyOutputSink>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("spaceterm-pty-reader".to_owned())
        .spawn(move || {
            let mut buffer = [0_u8; READ_BUFFER_SIZE];
            loop {
                let event = match reader.read(&mut buffer) {
                    Ok(0) => NativePtyOutput::Stopped(None),
                    Ok(read) => NativePtyOutput::Bytes(buffer[..read].to_vec()),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        NativePtyOutput::Stopped(Some(NativePtyReadFailure::new(error.to_string())))
                    }
                };
                let stopped = matches!(event, NativePtyOutput::Stopped(_));
                if !output.publish(event) || stopped {
                    break;
                }
            }
        })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;

    use super::*;

    #[derive(Default)]
    struct AdapterObservation {
        writes: Vec<u8>,
        size: Option<NativePtySize>,
    }

    struct ObservedAdapter {
        observation: Arc<Mutex<AdapterObservation>>,
        exit: NativePtyExit,
    }

    impl Read for ObservedAdapter {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for ObservedAdapter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.observation
                .lock()
                .expect("adapter observation should remain available")
                .writes
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl NativePtyAdapter for ObservedAdapter {
        fn take_reader(&mut self) -> io::Result<Box<dyn Read + Send>> {
            Ok(Box::new(Cursor::new(Vec::<u8>::new())))
        }

        fn resize(&self, size: NativePtySize) -> Result<(), NativePtyOperationFailure> {
            self.observation
                .lock()
                .expect("adapter observation should remain available")
                .size = Some(size);
            Ok(())
        }

        fn hidden_input(&self) -> Result<bool, NativePtyOperationFailure> {
            Ok(true)
        }

        fn wait_for_exit(
            &mut self,
            _timeout: Duration,
        ) -> Result<NativePtyExit, NativePtyWaitFailure> {
            Ok(self.exit.clone())
        }
    }

    struct CountingTermination(Arc<AtomicUsize>);

    impl NativePtyTermination for CountingTermination {
        fn request_termination(&self) -> io::Result<()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    struct DiscardOutput;

    impl NativePtyOutputSink for DiscardOutput {
        fn publish(&self, _output: NativePtyOutput) -> bool {
            true
        }
    }

    struct RecordingAdapterFactory {
        construction: Arc<Mutex<Option<(PathBuf, NativePtySize)>>>,
        adapter_observation: Arc<Mutex<AdapterObservation>>,
        termination_count: Arc<AtomicUsize>,
    }

    impl NativePtyAdapterFactory for RecordingAdapterFactory {
        fn create(
            &self,
            launch: NativePtyLaunch,
            size: NativePtySize,
        ) -> Result<NativePtyAdapterParts, NativePtyAdapterConstructionFailure> {
            let NativePtyLaunch::Local { working_directory } = launch else {
                panic!("test constructor expected a Local launch")
            };
            *self
                .construction
                .lock()
                .expect("construction observation should remain available") =
                Some((working_directory, size));
            Ok(NativePtyAdapterParts {
                adapter: Box::new(ObservedAdapter {
                    observation: Arc::clone(&self.adapter_observation),
                    exit: NativePtyExit::Success,
                }),
                termination: Arc::new(CountingTermination(Arc::clone(&self.termination_count))),
            })
        }
    }

    #[test]
    fn owner_forwards_exact_launch_and_geometry_to_the_injected_adapter_factory() {
        let construction = Arc::new(Mutex::new(None));
        let factory = RecordingAdapterFactory {
            construction: Arc::clone(&construction),
            adapter_observation: Arc::new(Mutex::new(AdapterObservation::default())),
            termination_count: Arc::new(AtomicUsize::new(0)),
        };
        let size = NativePtySize {
            rows: 31,
            columns: 97,
            pixel_width: 1_164,
            pixel_height: 620,
        };
        let working_directory = PathBuf::from("/exact/spelling/../project");

        let owner = NativePtyOwner::start(
            &factory,
            NativePtyLaunch::local(working_directory.clone()),
            size,
            Arc::new(DiscardOutput),
            &NativePtyCloseHandle::default(),
        )
        .expect("fake Native PTY Owner should start");

        assert_eq!(
            *construction
                .lock()
                .expect("construction observation should remain available"),
            Some((working_directory, size))
        );
        drop(owner);
    }

    #[test]
    fn owner_retains_factory_parts_until_owner_destruction() {
        let termination_count = Arc::new(AtomicUsize::new(0));
        let factory = RecordingAdapterFactory {
            construction: Arc::new(Mutex::new(None)),
            adapter_observation: Arc::new(Mutex::new(AdapterObservation::default())),
            termination_count: Arc::clone(&termination_count),
        };
        let owner = NativePtyOwner::start(
            &factory,
            NativePtyLaunch::local(PathBuf::from("/project")),
            NativePtySize::default(),
            Arc::new(DiscardOutput),
            &NativePtyCloseHandle::default(),
        )
        .expect("fake Native PTY Owner should start");

        drop(factory);
        let before_owner_drop = termination_count.load(Ordering::Acquire);
        drop(owner);
        assert_eq!(
            (before_owner_drop, termination_count.load(Ordering::Acquire),),
            (0, 1)
        );
    }

    struct FailingAdapterFactory;

    impl NativePtyAdapterFactory for FailingAdapterFactory {
        fn create(
            &self,
            _launch: NativePtyLaunch,
            _size: NativePtySize,
        ) -> Result<NativePtyAdapterParts, NativePtyAdapterConstructionFailure> {
            Err(NativePtyAdapterConstructionFailure::new(
                "adapter construction unavailable".to_owned(),
            ))
        }
    }

    #[test]
    fn owner_maps_adapter_factory_failure_to_the_adapter_startup_stage() {
        let error = NativePtyOwner::start(
            &FailingAdapterFactory,
            NativePtyLaunch::local(PathBuf::from("/project")),
            NativePtySize::default(),
            Arc::new(DiscardOutput),
            &NativePtyCloseHandle::default(),
        )
        .err()
        .expect("adapter construction should fail");

        assert_eq!(
            (error.stage(), error.to_string()),
            (
                NativePtyStartupStage::Adapter,
                "adapter construction unavailable".to_owned(),
            )
        );
    }

    fn observed_owner(
        exit: NativePtyExit,
        close_handle: &NativePtyCloseHandle,
        termination: Arc<dyn NativePtyTermination>,
    ) -> (NativePtyOwner, Arc<Mutex<AdapterObservation>>) {
        let observation = Arc::new(Mutex::new(AdapterObservation::default()));
        let owner = NativePtyOwner::from_adapter_parts(
            NativePtyAdapterParts {
                adapter: Box::new(ObservedAdapter {
                    observation: Arc::clone(&observation),
                    exit,
                }),
                termination,
            },
            Arc::new(DiscardOutput),
            close_handle,
        )
        .expect("fake Native PTY Owner should start");
        (owner, observation)
    }

    #[test]
    fn owner_delegates_io_geometry_hidden_input_and_exit_outcomes() {
        let exits = [
            NativePtyExit::Success,
            NativePtyExit::ExitCode(23),
            NativePtyExit::Signal("TERM".to_owned()),
            NativePtyExit::GracefulShutdown,
            NativePtyExit::ForcedShutdown,
        ];
        for expected_exit in exits {
            let close_handle = NativePtyCloseHandle::default();
            let termination_count = Arc::new(AtomicUsize::new(0));
            let (mut owner, observation) = observed_owner(
                expected_exit.clone(),
                &close_handle,
                Arc::new(CountingTermination(Arc::clone(&termination_count))),
            );
            let size = NativePtySize {
                rows: 31,
                columns: 97,
                pixel_width: 1_164,
                pixel_height: 620,
            };

            owner.resize(size).expect("resize should be delegated");
            assert!(
                owner
                    .hidden_input()
                    .expect("hidden input should be delegated")
            );
            owner
                .write_all(b"input")
                .expect("writes should be delegated");
            assert_eq!(
                owner
                    .wait_for_exit(Duration::from_millis(1))
                    .expect("wait should be delegated"),
                expected_exit
            );

            let observation = observation
                .lock()
                .expect("adapter observation should remain available");
            assert_eq!(observation.size, Some(size));
            assert_eq!(observation.writes, b"input");
            assert_eq!(termination_count.load(Ordering::Relaxed), 0);
        }
    }

    struct BlockingTermination {
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl NativePtyTermination for BlockingTermination {
        fn request_termination(&self) -> io::Result<()> {
            let _ = self.entered.send(());
            let _ = self
                .release
                .lock()
                .expect("release receiver should remain available")
                .recv();
            Ok(())
        }
    }

    #[test]
    fn close_returns_without_waiting_for_termination_work() {
        let close_handle = NativePtyCloseHandle::default();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (owner, _observation) = observed_owner(
            NativePtyExit::GracefulShutdown,
            &close_handle,
            Arc::new(BlockingTermination {
                entered: entered_tx,
                release: Mutex::new(release_rx),
            }),
        );
        let (returned_tx, returned_rx) = mpsc::channel();
        let close_thread = thread::spawn(move || {
            let result = close_handle.request_close();
            let _ = returned_tx.send(result);
        });

        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("termination supervisor should receive close");
        let returned = returned_rx.recv_timeout(Duration::from_millis(100));
        let _ = release_tx.send(());
        close_thread.join().expect("close caller should not panic");
        assert!(
            returned
                .expect("close should return while termination remains blocked")
                .is_ok()
        );
        drop(owner);
    }

    struct DropOrderAdapter {
        termination_completed: Arc<AtomicBool>,
        dropped_before_termination: Arc<AtomicBool>,
    }

    impl Write for DropOrderAdapter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl NativePtyAdapter for DropOrderAdapter {
        fn take_reader(&mut self) -> io::Result<Box<dyn Read + Send>> {
            Ok(Box::new(Cursor::new(Vec::<u8>::new())))
        }

        fn resize(&self, _size: NativePtySize) -> Result<(), NativePtyOperationFailure> {
            Ok(())
        }

        fn wait_for_exit(
            &mut self,
            _timeout: Duration,
        ) -> Result<NativePtyExit, NativePtyWaitFailure> {
            Ok(NativePtyExit::Success)
        }
    }

    impl Drop for DropOrderAdapter {
        fn drop(&mut self) {
            if !self.termination_completed.load(Ordering::Acquire) {
                self.dropped_before_termination
                    .store(true, Ordering::Release);
            }
        }
    }

    struct CompletionTermination(Arc<AtomicBool>);

    impl NativePtyTermination for CompletionTermination {
        fn request_termination(&self) -> io::Result<()> {
            self.0.store(true, Ordering::Release);
            Ok(())
        }
    }

    #[test]
    fn owner_completes_the_termination_request_before_dropping_the_adapter() {
        let termination_completed = Arc::new(AtomicBool::new(false));
        let dropped_before_termination = Arc::new(AtomicBool::new(false));
        let owner = NativePtyOwner::from_adapter_parts(
            NativePtyAdapterParts {
                adapter: Box::new(DropOrderAdapter {
                    termination_completed: Arc::clone(&termination_completed),
                    dropped_before_termination: Arc::clone(&dropped_before_termination),
                }),
                termination: Arc::new(CompletionTermination(Arc::clone(&termination_completed))),
            },
            Arc::new(DiscardOutput),
            &NativePtyCloseHandle::default(),
        )
        .expect("fake Native PTY Owner should start");

        drop(owner);

        assert!(termination_completed.load(Ordering::Acquire));
        assert!(!dropped_before_termination.load(Ordering::Acquire));
    }
}
