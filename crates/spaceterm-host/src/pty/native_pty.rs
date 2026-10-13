//! Platform-neutral PTY ownership, ordered reader delivery, and termination supervision.

use std::fmt;
use std::io::{self, Read, Write};
#[cfg(any(test, feature = "test-support"))]
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use thiserror::Error;

use crate::shell_launch::{PreparedShellLaunch, ShellLaunchFailure};

const READ_BUFFER_SIZE: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// PTY grid dimensions and its total pixel extent.
pub struct NativePtySize {
    /// Terminal rows.
    pub rows: u16,
    /// Terminal columns.
    pub columns: u16,
    /// Total grid width in pixels.
    pub pixel_width: u16,
    /// Total grid height in pixels.
    pub pixel_height: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Shell exit result, including owner-requested retirement.
pub enum NativePtyExit {
    /// The shell exited with status zero.
    Success,
    /// The shell exited with a nonzero status.
    ExitCode(u32),
    /// The shell exited after a classified native signal.
    Signal(String),
    /// Owner-requested graceful shutdown completed.
    GracefulShutdown,
    /// Owner-requested shutdown required escalation.
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
/// Bounded PTY reader failure classification.
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
/// Bounded child wait failure classification.
pub struct NativePtyWaitFailure {
    kind: io::ErrorKind,
}

impl NativePtyWaitFailure {
    /// Construct a failure without retaining the native error or its message.
    pub fn new(kind: io::ErrorKind) -> Self {
        Self { kind }
    }

    /// Return the recoverable native error category.
    pub const fn kind(&self) -> io::ErrorKind {
        self.kind
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("Native PTY operation failed ({kind:?})")]
/// Bounded resize or terminal-attribute failure classification.
pub struct NativePtyOperationFailure {
    kind: io::ErrorKind,
}

impl NativePtyOperationFailure {
    /// Construct a failure without retaining the native error or its message.
    pub fn new(kind: io::ErrorKind) -> Self {
        Self { kind }
    }

    /// Return the recoverable native error category.
    pub const fn kind(&self) -> io::ErrorKind {
        self.kind
    }
}

#[derive(Debug, Error)]
/// Classified failure while installing an owner and its reader.
pub enum NativePtyStartupFailure {
    #[error(transparent)]
    /// Launch policy rejected the requested shell.
    Launch(#[from] ShellLaunchFailure),
    #[error(transparent)]
    /// Native adapter construction failed.
    Adapter(#[from] NativePtyAdapterConstructionFailure),
    #[error("Native PTY lifecycle coordination could not be started")]
    /// Termination supervision could not be installed.
    LifecycleCoordination,
    #[error("Native PTY reader acquisition failed ({0:?})")]
    /// Reader acquisition failed.
    Reader(io::ErrorKind),
    #[error("Native PTY reader thread creation failed ({0:?})")]
    /// Reader worker creation failed.
    ReaderThread(io::ErrorKind),
}

impl NativePtyStartupFailure {
    /// Return the failed owner startup stage.
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
/// The stage of owner startup that failed.
pub enum NativePtyStartupStage {
    /// Launch preparation, native construction, or termination coordination.
    Adapter,
    /// Acquisition of a terminal-output reader.
    Reader,
    /// Starting the terminal-output reader thread.
    ReaderThread,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Ordered terminal output or the reader completion event.
pub enum NativePtyOutput {
    /// One chunk of at most 16 KiB.
    Bytes(Vec<u8>),
    /// EOF, or a bounded read failure.
    Stopped(Option<NativePtyReadFailure>),
}

/// Output admission selected by runtime composition.
pub trait NativePtyOutputSink: Send + Sync + 'static {
    /// Accept ordered output; false cancels further reader delivery.
    fn publish(&self, output: NativePtyOutput) -> bool;
}

/// Input writes must be nonblocking and report WouldBlock under PTY backpressure.
pub trait NativePtyAdapter: Write + Send {
    /// Consume this adapter's output reader once.
    fn take_reader(&mut self) -> io::Result<Box<dyn Read + Send>>;
    /// Apply admitted grid and pixel dimensions.
    fn resize(&self, size: NativePtySize) -> Result<(), NativePtyOperationFailure>;
    /// Whether canonical input currently has echo disabled.
    fn hidden_input(&self) -> Result<bool, NativePtyOperationFailure> {
        Ok(false)
    }
    /// Wait up to the supplied timeout for the shell exit result.
    /// A zero timeout polls without waiting for coordination locks or initiating session settlement.
    /// Return a typed `TimedOut` failure when the child is running or its state is contended.
    fn wait_for_exit(&mut self, timeout: Duration) -> Result<NativePtyExit, NativePtyWaitFailure>;
}

/// Constructs the platform-neutral parts owned by one Native PTY Owner.
///
/// Application composition selects the Operating-System-specific implementation. The Interface
/// deliberately exposes no descriptor, terminal attribute, signal, or process-group mechanism.
pub trait NativePtyAdapterFactory: Send + Sync {
    /// Consume one prepared launch and construct its owned native resources.
    fn create(
        &self,
        launch: PreparedShellLaunch,
        size: NativePtySize,
    ) -> Result<NativePtyAdapterParts, NativePtyAdapterConstructionFailure>;
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
/// A closed construction failure classification at the Operating-System Adapter boundary.
pub enum NativePtyAdapterConstructionFailure {
    #[error("Native PTY resources could not be created")]
    /// Native resource creation failed.
    ResourceCreationFailed,
    #[error("Native PTY resources could not be configured")]
    /// Initial PTY configuration failed.
    ResourceConfigurationFailed,
    #[error("Shell Process could not be started")]
    /// Starting the shell failed.
    ProcessStartFailed,
    #[error("Shell Process ownership could not be established")]
    /// A stable isolated process session could not be established.
    ProcessOwnershipFailed,
    #[error("Native PTY input/output could not be acquired")]
    /// Acquiring the PTY reader or writer failed.
    InputOutputUnavailable,
}

/// Independent evidence of native process-session cleanup, not a shell exit result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativePtySettlement {
    /// The leader was reaped and no process-session members remain.
    Settled,
    /// Process-session members remain after bounded signal escalation.
    StillRunning,
    /// Ownership, process inspection, signalling, or reaping could not be confirmed.
    Unavailable,
}

/// Idempotent native process retirement selected by composition.
pub trait NativePtyTermination: Send + Sync {
    /// Request bounded native process shutdown. Success alone is not settlement evidence.
    fn request_termination(&self) -> io::Result<()>;

    /// Attempt cleanup on a blocking thread. Only `Settled` is permanent positive evidence.
    /// Providers without independent evidence must keep cleanup unconfirmed.
    ///
    /// Retain this same authority after adapter cleanup when a result is inconclusive. POSIX
    /// providers keep the original process-session identity and perform a fresh attempt on a later
    /// call. macOS serializes complete attempts process-wide because its proof spawns fence children.
    /// macOS lock contention is bounded by the attempt deadline and returns `Unavailable`.
    /// Dropping this authority is not settlement evidence and must not release quarantined capacity.
    fn settle(&self) -> NativePtySettlement {
        NativePtySettlement::Unavailable
    }
}

/// Adapter and retirement authority retained by one owner.
pub struct NativePtyAdapterParts {
    /// Input, output-reader acquisition, size, and child wait operations.
    pub adapter: Box<dyn NativePtyAdapter>,
    /// Process retirement independent of adapter command handling.
    pub termination: Arc<dyn NativePtyTermination>,
}

#[derive(Clone, Default)]
/// Cloneable, idempotent, nonblocking close request for one owner.
pub struct NativePtyCloseHandle {
    state: Arc<Mutex<NativePtyCloseState>>,
}

#[derive(Default)]
struct NativePtyCloseState {
    requested: bool,
    supervisor: Option<Sender<TerminationSupervisorCommand>>,
}

impl NativePtyCloseHandle {
    /// Enqueue close without waiting for native shutdown work.
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

/// Retains a PTY, termination supervisor, and 16 KiB reader worker.
/// Drop completes termination, drops the adapter, then joins the reader.
pub struct NativePtyOwner {
    adapter: Option<Box<dyn NativePtyAdapter>>,
    termination_supervisor: Option<NativePtyTerminationSupervisor>,
    reader_thread: Option<JoinHandle<()>>,
}

impl NativePtyOwner {
    /// Consume a prepared launch and install native lifecycle supervision.
    pub fn start(
        adapter_factory: &dyn NativePtyAdapterFactory,
        launch: PreparedShellLaunch,
        size: NativePtySize,
        output: Arc<dyn NativePtyOutputSink>,
        close_handle: &NativePtyCloseHandle,
    ) -> Result<Self, NativePtyStartupFailure> {
        let parts = adapter_factory
            .create(launch, size)
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
        })
    }

    /// Apply an admitted terminal size through the adapter.
    pub fn resize(&self, size: NativePtySize) -> Result<(), NativePtyOperationFailure> {
        self.adapter().resize(size)
    }

    /// Read the adapter's hidden-input classification.
    pub fn hidden_input(&self) -> Result<bool, NativePtyOperationFailure> {
        self.adapter().hidden_input()
    }

    /// Wait for the shell's classified exit result.
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
                    Ok(read) => NativePtyOutput::Bytes(buffer[..read].to_vec()),
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

#[cfg(any(test, feature = "test-support"))]
/// Observe injected launch forwarding for desktop conformance tests.
pub fn conformance_initialization() -> Result<Option<(PathBuf, NativePtySize)>, String> {
    tests::observe_initialization()
}

#[cfg(any(test, feature = "test-support"))]
/// Lifecycle observations from injected conformance adapters.
pub struct ShutdownObservation {
    /// Terminations after the factory is dropped.
    pub after_factory_drop: usize,
    /// Terminations after the owner is dropped.
    pub after_owner_drop: usize,
    /// Terminations after repeated close and owner drop.
    pub after_repeated_close: usize,
    /// Whether retirement finished before destruction returned.
    pub termination_completed: bool,
    /// Whether the adapter was destroyed before retirement completed.
    pub dropped_before_termination: bool,
}

#[cfg(any(test, feature = "test-support"))]
/// Observe retention, close idempotence, and cleanup order through injected adapters.
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

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use std::io::Cursor;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    #[cfg(test)]
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

        /// Whether canonical input currently has echo disabled.
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

    struct OneShotAdapterFactory(Mutex<Option<NativePtyAdapterParts>>);

    impl NativePtyAdapterFactory for OneShotAdapterFactory {
        /// Consume one prepared launch and construct its owned native resources.
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
        /// Consume one prepared launch and construct its owned native resources.
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

    #[test]
    fn owner_reader_preserves_order_in_chunks_of_at_most_16_kib() {
        struct ReaderAdapter(Option<Cursor<Vec<u8>>>);
        impl Write for ReaderAdapter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl NativePtyAdapter for ReaderAdapter {
            fn take_reader(&mut self) -> io::Result<Box<dyn Read + Send>> {
                Ok(Box::new(self.0.take().unwrap()))
            }
            fn resize(&self, _: NativePtySize) -> Result<(), NativePtyOperationFailure> {
                Ok(())
            }
            fn wait_for_exit(
                &mut self,
                _: Duration,
            ) -> Result<NativePtyExit, NativePtyWaitFailure> {
                Ok(NativePtyExit::Success)
            }
        }
        struct ChannelOutput(mpsc::Sender<NativePtyOutput>);
        impl NativePtyOutputSink for ChannelOutput {
            fn publish(&self, output: NativePtyOutput) -> bool {
                self.0.send(output).is_ok()
            }
        }
        let bytes: Vec<u8> = (0..32_769).map(|index| (index % 251) as u8).collect();
        let factory = OneShotAdapterFactory(Mutex::new(Some(NativePtyAdapterParts {
            adapter: Box::new(ReaderAdapter(Some(Cursor::new(bytes.clone())))),
            termination: Arc::new(CountingTermination(Arc::new(AtomicUsize::new(0)))),
        })));
        let (output, receiver) = mpsc::channel();
        let owner = NativePtyOwner::start(
            &factory,
            PreparedShellLaunch::for_test(std::env::temp_dir()),
            NativePtySize::default(),
            Arc::new(ChannelOutput(output)),
            &NativePtyCloseHandle::default(),
        )
        .unwrap();
        let mut observed = Vec::new();
        let mut lengths = Vec::new();
        loop {
            match receiver.recv_timeout(Duration::from_secs(1)).unwrap() {
                NativePtyOutput::Bytes(chunk) => {
                    lengths.push(chunk.len());
                    observed.extend(chunk);
                }
                NativePtyOutput::Stopped(error) => {
                    assert!(error.is_none());
                    break;
                }
            }
        }
        drop(owner);
        assert_eq!(lengths, [16_384, 16_384, 1]);
        assert_eq!(observed, bytes);
    }

    #[cfg(test)]
    struct FailingAdapterFactory;

    #[cfg(test)]
    impl NativePtyAdapterFactory for FailingAdapterFactory {
        /// Consume one prepared launch and construct its owned native resources.
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

    #[cfg(test)]
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

    #[cfg(test)]
    struct BlockingTermination {
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    #[cfg(test)]
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
