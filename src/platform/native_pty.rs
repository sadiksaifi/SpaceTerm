use std::fmt;
use std::io::{self, Read, Write};
#[cfg(test)]
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use spaceterm_server::diagnostics::TerminalCounters;
use spaceterm_server::pane::LineMode;
use thiserror::Error;

use super::shell_launch::{PreparedShellLaunch, ShellLaunchFailure};

const READ_BUFFER_SIZE: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NativePtySize {
    pub rows: u16,
    pub columns: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativePtyExit {
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
#[error("Native PTY read failed ({kind:?})")]
pub struct NativePtyReadFailure {
    kind: io::ErrorKind,
}

impl NativePtyReadFailure {
    fn new(kind: io::ErrorKind) -> Self {
        Self { kind }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("Native PTY wait failed ({kind:?})")]
pub struct NativePtyWaitFailure {
    kind: io::ErrorKind,
}

impl NativePtyWaitFailure {
    pub fn new(kind: io::ErrorKind) -> Self {
        Self { kind }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("Native PTY operation failed ({kind:?})")]
pub struct NativePtyOperationFailure {
    kind: io::ErrorKind,
}

impl NativePtyOperationFailure {
    pub fn new(kind: io::ErrorKind) -> Self {
        Self { kind }
    }

    pub const fn kind(&self) -> io::ErrorKind {
        self.kind
    }
}

#[derive(Debug, Error)]
pub enum NativePtyStartupFailure {
    #[error(transparent)]
    Launch(#[from] ShellLaunchFailure),
    #[error(transparent)]
    Adapter(#[from] NativePtyAdapterConstructionFailure),
    #[error("Native PTY lifecycle coordination could not be started")]
    LifecycleCoordination,
    #[error("Native PTY reader acquisition failed ({0:?})")]
    Reader(io::ErrorKind),
    #[error("Native PTY reader thread creation failed ({0:?})")]
    ReaderThread(io::ErrorKind),
}

impl NativePtyStartupFailure {
    pub const fn stage(&self) -> NativePtyStartupStage {
        match self {
            Self::Launch(_) | Self::Adapter(_) | Self::LifecycleCoordination => {
                NativePtyStartupStage::Adapter
            }
            Self::Reader(_) => NativePtyStartupStage::Reader,
            Self::ReaderThread(_) => NativePtyStartupStage::ReaderThread,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativePtyStartupStage {
    Adapter,
    Reader,
    ReaderThread,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativePtyOutput {
    Bytes(Vec<u8>),
    Stopped(Option<NativePtyReadFailure>),
}

pub trait NativePtyOutputSink: Send + Sync + 'static {
    fn publish(&self, output: NativePtyOutput) -> bool;
}

/// Input writes must be nonblocking and report WouldBlock under PTY backpressure.
pub trait NativePtyAdapter: Write + Send {
    fn take_reader(&mut self) -> io::Result<Box<dyn Read + Send>>;
    fn resize(&self, size: NativePtySize) -> Result<(), NativePtyOperationFailure>;
    /// The line discipline's flags and whether the shell owns the foreground. Each read costs a
    /// few microseconds, so callers read it after input, output and focus changes.
    fn line_mode(&self) -> Result<LineMode, NativePtyOperationFailure> {
        Ok(LineMode::UNKNOWN)
    }
    fn wait_for_exit(&mut self, timeout: Duration) -> Result<NativePtyExit, NativePtyWaitFailure>;
}

/// Constructs the platform-neutral parts owned by one Native PTY Owner.
///
/// Application composition selects the Operating-System-specific implementation. The Interface
/// deliberately exposes no descriptor, terminal attribute, signal, or process-group mechanism.
pub trait NativePtyAdapterFactory: Send + Sync {
    fn create(
        &self,
        launch: PreparedShellLaunch,
        size: NativePtySize,
    ) -> Result<NativePtyAdapterParts, NativePtyAdapterConstructionFailure>;
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
/// A closed construction failure classification at the Operating-System Adapter boundary.
pub enum NativePtyAdapterConstructionFailure {
    /// The account environment lookup failed before PTY or process construction began.
    #[error("Account service environment is unavailable")]
    EnvironmentUnavailable,
    /// The PTY could not be opened, so no process was started.
    #[error("Native PTY resources could not be created")]
    ResourceCreationFailed,
    /// The PTY could not be configured before the shell was spawned, so no process was started.
    #[error("Native PTY resources could not be configured")]
    ResourceConfigurationFailed,
    /// The shell could not be spawned, for example because its directory disappeared after
    /// validation. A spawn that fails before `exec` reaps its child, so no process remains.
    #[error("Shell Process could not be started")]
    ProcessStartFailed,
    /// The shell was spawned before the failure, so its cleanup stays unconfirmed.
    #[error("Shell Process ownership could not be established")]
    ProcessOwnershipFailed,
    /// The shell was spawned before the failure, so its cleanup stays unconfirmed.
    #[error("Native PTY input/output could not be acquired")]
    InputOutputUnavailable,
}

impl NativePtyAdapterConstructionFailure {
    /// Whether construction ended before any process existed, so none needs cleanup.
    const fn started_no_process(self) -> bool {
        matches!(
            self,
            Self::EnvironmentUnavailable
                | Self::ResourceCreationFailed
                | Self::ResourceConfigurationFailed
                | Self::ProcessStartFailed
        )
    }
}

pub trait NativePtyTermination: Send + Sync {
    fn request_termination(&self) -> io::Result<()>;
    fn cleanup_confirmed(&self) -> bool {
        false
    }
}

pub struct NativePtyAdapterParts {
    pub adapter: Box<dyn NativePtyAdapter>,
    pub termination: Arc<dyn NativePtyTermination>,
}

#[derive(Clone, Default)]
pub struct NativePtyCloseHandle {
    state: Arc<Mutex<NativePtyCloseState>>,
}

#[derive(Default)]
struct NativePtyCloseState {
    requested: bool,
    cleanup_confirmed: bool,
    supervisor: Option<Sender<TerminationSupervisorCommand>>,
}

impl NativePtyCloseHandle {
    /// Preparation or adapter construction ended before any process could need cleanup.
    pub(crate) fn confirm_unstarted(&self) {
        let mut state = self.lock_state();
        debug_assert!(state.supervisor.is_none());
        state.cleanup_confirmed = true;
    }

    pub fn cleanup_confirmed(&self) -> bool {
        self.lock_state().cleanup_confirmed
    }

    pub fn request_close(&self) -> io::Result<()> {
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
                        eprintln!(
                            "failed to terminate shell while shutting down Native PTY Owner: {:?}",
                            error.kind()
                        );
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

pub struct NativePtyOwner {
    adapter: Option<Box<dyn NativePtyAdapter>>,
    termination_supervisor: Option<NativePtyTerminationSupervisor>,
    reader_thread: Option<JoinHandle<()>>,
    cleanup_probe: Option<Arc<dyn NativePtyTermination>>,
    close_state: Weak<Mutex<NativePtyCloseState>>,
}

impl NativePtyOwner {
    pub fn start(
        adapter_factory: &dyn NativePtyAdapterFactory,
        launch: PreparedShellLaunch,
        size: NativePtySize,
        output: Arc<dyn NativePtyOutputSink>,
        close_handle: &NativePtyCloseHandle,
    ) -> Result<Self, NativePtyStartupFailure> {
        let parts = adapter_factory
            .create(launch, size)
            .inspect_err(|failure| {
                if failure.started_no_process() {
                    close_handle.confirm_unstarted();
                }
            })
            .map_err(NativePtyStartupFailure::Adapter)?;
        Self::install_adapter_parts(parts, output, close_handle)
    }

    fn install_adapter_parts(
        mut parts: NativePtyAdapterParts,
        output: Arc<dyn NativePtyOutputSink>,
        close_handle: &NativePtyCloseHandle,
    ) -> Result<Self, NativePtyStartupFailure> {
        let termination_supervisor = close_handle
            .install(Arc::clone(&parts.termination))
            .map_err(|_| NativePtyStartupFailure::LifecycleCoordination)?;
        let reader = parts
            .adapter
            .take_reader()
            .map_err(|error| NativePtyStartupFailure::Reader(error.kind()))?;
        let reader_thread = spawn_reader(reader, output)
            .map_err(|error| NativePtyStartupFailure::ReaderThread(error.kind()))?;
        Ok(Self {
            adapter: Some(parts.adapter),
            termination_supervisor: Some(termination_supervisor),
            reader_thread: Some(reader_thread),
            cleanup_probe: Some(parts.termination),
            close_state: Arc::downgrade(&close_handle.state),
        })
    }

    pub fn resize(&self, size: NativePtySize) -> Result<(), NativePtyOperationFailure> {
        self.adapter().resize(size)
    }

    pub fn line_mode(&self) -> Result<LineMode, NativePtyOperationFailure> {
        TerminalCounters::process().record_line_mode_read();
        self.adapter().line_mode()
    }

    pub fn wait_for_exit(
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
        let confirmed = self
            .cleanup_probe
            .as_ref()
            .is_some_and(|probe| probe.cleanup_confirmed());
        drop(self.cleanup_probe.take());
        if let Some(state) = self.close_state.upgrade() {
            state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cleanup_confirmed = confirmed;
        }
        if let Some(reader_thread) = self.reader_thread.take()
            && reader_thread.join().is_err()
        {
            eprintln!("PTY reader thread panicked");
        }
    }
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
                    Ok(read) => {
                        TerminalCounters::process().record_pty_read(read);
                        NativePtyOutput::Bytes(buffer[..read].to_vec())
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        NativePtyOutput::Stopped(Some(NativePtyReadFailure::new(error.kind())))
                    }
                };
                let stopped = matches!(event, NativePtyOutput::Stopped(_));
                if !output.publish(event) || stopped {
                    break;
                }
            }
        })
}

#[cfg(all(test, feature = "desktop"))]
pub fn conformance_initialization() -> Result<Option<(PathBuf, NativePtySize)>, String> {
    tests::observe_initialization()
}

#[cfg(all(test, feature = "desktop"))]
pub struct ShutdownObservation {
    pub after_factory_drop: usize,
    pub after_owner_drop: usize,
    pub after_repeated_close: usize,
    pub termination_completed: bool,
    pub dropped_before_termination: bool,
}

#[cfg(all(test, feature = "desktop"))]
pub fn conformance_shutdown() -> Result<ShutdownObservation, String> {
    let (after_factory_drop, after_owner_drop) = tests::observe_retention(false)?;
    let (_, after_repeated_close) = tests::observe_retention(true)?;
    let (termination_completed, dropped_before_termination) = tests::observe_drop_order()?;
    Ok(ShutdownObservation {
        after_factory_drop,
        after_owner_drop,
        after_repeated_close,
        termination_completed,
        dropped_before_termination,
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

        fn line_mode(&self) -> Result<LineMode, NativePtyOperationFailure> {
            Ok(LineMode {
                canonical: true,
                ..LineMode::UNKNOWN
            })
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

    struct OneShotAdapterFactory(Mutex<Option<NativePtyAdapterParts>>);

    impl NativePtyAdapterFactory for OneShotAdapterFactory {
        fn create(
            &self,
            _launch: PreparedShellLaunch,
            _size: NativePtySize,
        ) -> Result<NativePtyAdapterParts, NativePtyAdapterConstructionFailure> {
            self.0
                .lock()
                .expect("adapter parts should remain available")
                .take()
                .ok_or(NativePtyAdapterConstructionFailure::ResourceCreationFailed)
        }
    }

    impl NativePtyAdapterFactory for RecordingAdapterFactory {
        fn create(
            &self,
            launch: PreparedShellLaunch,
            size: NativePtySize,
        ) -> Result<NativePtyAdapterParts, NativePtyAdapterConstructionFailure> {
            let working_directory = launch.working_directory().to_owned();
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

    pub(super) fn observe_initialization() -> Result<Option<(PathBuf, NativePtySize)>, String> {
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
            PreparedShellLaunch::for_test(working_directory.clone()),
            size,
            Arc::new(DiscardOutput),
            &NativePtyCloseHandle::default(),
        )
        .map_err(|error| format!("step=start field=owner expected=started observed={error}"))?;

        let observed = construction.lock().unwrap().clone();
        drop(owner);
        Ok(observed)
    }

    pub(super) fn observe_retention(repeat_close: bool) -> Result<(usize, usize), String> {
        let termination_count = Arc::new(AtomicUsize::new(0));
        let factory = RecordingAdapterFactory {
            construction: Arc::new(Mutex::new(None)),
            adapter_observation: Arc::new(Mutex::new(AdapterObservation::default())),
            termination_count: Arc::clone(&termination_count),
        };
        let close_handle = NativePtyCloseHandle::default();
        let owner = NativePtyOwner::start(
            &factory,
            PreparedShellLaunch::for_test(PathBuf::from("/project")),
            NativePtySize::default(),
            Arc::new(DiscardOutput),
            &close_handle,
        )
        .map_err(|error| format!("step=start field=owner expected=started observed={error}"))?;

        drop(factory);
        let before_owner_drop = termination_count.load(Ordering::Acquire);
        if repeat_close {
            close_handle
                .request_close()
                .map_err(|error| error.to_string())?;
            close_handle
                .request_close()
                .map_err(|error| error.to_string())?;
        }
        drop(owner);
        Ok((before_owner_drop, termination_count.load(Ordering::Acquire)))
    }

    #[test]
    fn owner_forwards_exact_launch_and_geometry_to_the_injected_adapter_factory() {
        assert_eq!(
            observe_initialization().unwrap(),
            Some((
                PathBuf::from("/exact/spelling/../project"),
                NativePtySize {
                    rows: 31,
                    columns: 97,
                    pixel_width: 1_164,
                    pixel_height: 620
                },
            ))
        );
    }

    #[test]
    fn owner_retains_factory_parts_until_owner_destruction() {
        assert_eq!(observe_retention(false).unwrap(), (0, 1));
    }

    #[test]
    fn repeated_close_and_owner_drop_request_termination_once() {
        assert_eq!(observe_retention(true).unwrap(), (0, 1));
    }

    struct FailingAdapterFactory;

    impl NativePtyAdapterFactory for FailingAdapterFactory {
        fn create(
            &self,
            _launch: PreparedShellLaunch,
            _size: NativePtySize,
        ) -> Result<NativePtyAdapterParts, NativePtyAdapterConstructionFailure> {
            Err(NativePtyAdapterConstructionFailure::ResourceCreationFailed)
        }
    }

    #[test]
    fn owner_maps_adapter_factory_failure_to_the_adapter_startup_stage() {
        let error = NativePtyOwner::start(
            &FailingAdapterFactory,
            PreparedShellLaunch::for_test(PathBuf::from("/project")),
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
                "Native PTY resources could not be created".to_owned(),
            )
        );
    }

    fn observed_owner(
        exit: NativePtyExit,
        close_handle: &NativePtyCloseHandle,
        termination: Arc<dyn NativePtyTermination>,
    ) -> (NativePtyOwner, Arc<Mutex<AdapterObservation>>) {
        let observation = Arc::new(Mutex::new(AdapterObservation::default()));
        let factory = OneShotAdapterFactory(Mutex::new(Some(NativePtyAdapterParts {
            adapter: Box::new(ObservedAdapter {
                observation: Arc::clone(&observation),
                exit,
            }),
            termination,
        })));
        let owner = NativePtyOwner::start(
            &factory,
            PreparedShellLaunch::for_test(PathBuf::from("/project")),
            NativePtySize::default(),
            Arc::new(DiscardOutput),
            close_handle,
        )
        .expect("fake Native PTY Owner should start");
        (owner, observation)
    }

    #[test]
    fn owner_delegates_io_geometry_line_mode_and_exit_outcomes() {
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
                    .line_mode()
                    .expect("line mode should be delegated")
                    .hidden_input()
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

    /// Returns `reads` reads of `bytes` bytes each, then end of file.
    struct ChunkedReader {
        reads: usize,
        bytes: usize,
    }

    impl Read for ChunkedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.reads == 0 {
                return Ok(0);
            }
            self.reads -= 1;
            buffer[..self.bytes].fill(b'y');
            Ok(self.bytes)
        }
    }

    // Other tests in this process record into the same counters, so these check lower bounds
    // large enough that concurrent recording cannot supply them.
    #[test]
    fn each_pty_read_counts_one_read_and_its_bytes() {
        let before = TerminalCounters::process().snapshot();

        spawn_reader(
            Box::new(ChunkedReader {
                reads: 300,
                bytes: 1_001,
            }),
            Arc::new(DiscardOutput),
        )
        .unwrap()
        .join()
        .unwrap();

        let after = TerminalCounters::process().snapshot();
        assert!(after.pty_reads - before.pty_reads >= 300);
        assert!(after.pty_read_bytes - before.pty_read_bytes >= 300_300);
    }

    #[test]
    fn each_line_mode_read_is_counted() {
        let (owner, _observation) = observed_owner(
            NativePtyExit::Success,
            &NativePtyCloseHandle::default(),
            Arc::new(CountingTermination(Arc::new(AtomicUsize::new(0)))),
        );
        let before = TerminalCounters::process().snapshot();

        for _ in 0..500 {
            owner.line_mode().unwrap();
        }

        let after = TerminalCounters::process().snapshot();
        assert!(after.line_mode_reads - before.line_mode_reads >= 500);
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

    pub(super) fn observe_drop_order() -> Result<(bool, bool), String> {
        let termination_completed = Arc::new(AtomicBool::new(false));
        let dropped_before_termination = Arc::new(AtomicBool::new(false));
        let factory = OneShotAdapterFactory(Mutex::new(Some(NativePtyAdapterParts {
            adapter: Box::new(DropOrderAdapter {
                termination_completed: Arc::clone(&termination_completed),
                dropped_before_termination: Arc::clone(&dropped_before_termination),
            }),
            termination: Arc::new(CompletionTermination(Arc::clone(&termination_completed))),
        })));
        let owner = NativePtyOwner::start(
            &factory,
            PreparedShellLaunch::for_test(PathBuf::from("/project")),
            NativePtySize::default(),
            Arc::new(DiscardOutput),
            &NativePtyCloseHandle::default(),
        )
        .map_err(|error| format!("step=start field=owner expected=started observed={error}"))?;

        drop(owner);

        Ok((
            termination_completed.load(Ordering::Acquire),
            dropped_before_termination.load(Ordering::Acquire),
        ))
    }

    #[test]
    fn owner_completes_the_termination_request_before_dropping_the_adapter() {
        assert_eq!(observe_drop_order().unwrap(), (true, false));
    }
}
