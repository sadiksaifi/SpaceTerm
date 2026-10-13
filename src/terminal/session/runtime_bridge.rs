//! Maps owning native PTY parts into the identity-free legacy runtime.
use super::*;
use spaceterm_core::organization::FailureClass;
use spaceterm_terminal::runtime::legacy::{LegacyPty, LegacyPtyOutput, LegacyRuntimeContext};
use spaceterm_terminal::runtime::ports::{
    PtyExit, PtyParts, PtyProcess, PtySize, PtyTermination, RuntimeFailure, RuntimeStage,
};
use std::io::{self, Read, Write};

pub(super) fn runtime_size(size: NativePtySize) -> PtySize {
    PtySize {
        rows: size.rows,
        columns: size.columns,
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    }
}
fn native_size(size: PtySize) -> NativePtySize {
    NativePtySize {
        rows: size.rows,
        columns: size.columns,
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    }
}
fn failure(stage: RuntimeStage) -> RuntimeFailure {
    RuntimeFailure::new(FailureClass::RuntimeFault, stage)
}
struct HostProcess {
    adapter: Box<dyn crate::platform::native_pty::NativePtyAdapter>,
    exit: Arc<Mutex<Option<Result<NativePtyExit, NativePtyWaitFailure>>>>,
}
impl Write for HostProcess {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.adapter.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.adapter.flush()
    }
}
impl PtyProcess for HostProcess {
    fn take_reader(&mut self) -> Result<Box<dyn Read + Send>, RuntimeFailure> {
        self.adapter
            .take_reader()
            .map_err(|_| failure(RuntimeStage::Reader))
    }
    fn resize(&self, size: PtySize) -> Result<(), RuntimeFailure> {
        self.adapter
            .resize(native_size(size))
            .map_err(|_| failure(RuntimeStage::Resize))
    }
    fn hidden_input(&self) -> Result<bool, RuntimeFailure> {
        self.adapter
            .hidden_input()
            .map_err(|_| failure(RuntimeStage::Input))
    }
    fn wait_for_exit(&mut self, timeout: Duration) -> Result<PtyExit, RuntimeFailure> {
        let retained = self
            .exit
            .lock()
            .map_err(|_| failure(RuntimeStage::Cleanup))?
            .clone();
        let exit = match retained {
            Some(Ok(exit)) => exit,
            _ => {
                let result = self.adapter.wait_for_exit(timeout);
                *self
                    .exit
                    .lock()
                    .map_err(|_| failure(RuntimeStage::Cleanup))? = Some(result.clone());
                result.map_err(|_| failure(RuntimeStage::Cleanup))?
            }
        };
        let status = match exit {
            NativePtyExit::Success => Some(0),
            NativePtyExit::ExitCode(code) => i32::try_from(code).ok(),
            _ => None,
        };
        Ok(PtyExit::Exited { status })
    }
}
struct HostTermination(Arc<dyn crate::platform::native_pty::NativePtyTermination>);
impl PtyTermination for HostTermination {
    fn request_termination(&self) -> Result<(), RuntimeFailure> {
        self.0
            .request_termination()
            .map_err(|_| failure(RuntimeStage::Cleanup))?;
        // The native port only acknowledges signalling. Its descendant settlement
        // runs in Drop and exposes no classified receipt. Keep the launch quarantined
        // until the host provider supplies joinable process-session cleanup evidence.
        Err(RuntimeFailure::new(
            FailureClass::RuntimeUnavailable,
            RuntimeStage::Cleanup,
        ))
    }
}

pub(super) enum CompatibilityPty {
    Runtime {
        pty: LegacyPty<Option<u64>>,
        exit: Arc<Mutex<Option<Result<NativePtyExit, NativePtyWaitFailure>>>>,
    },
    #[cfg(test)]
    Fixture(NativePtyOwner),
}
#[derive(Clone, Debug)]
pub(super) enum ReadFailure {
    Runtime(RuntimeFailure),
}
impl fmt::Display for ReadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => error.fmt(f),
        }
    }
}
impl CompatibilityPty {
    pub(super) fn start(
        context: &LegacyRuntimeContext,
        factory: &dyn NativePtyAdapterFactory,
        launch: PreparedShellLaunch,
        commands: CommandSender<Command>,
        authority: Arc<ClipboardAuthority>,
    ) -> Result<Self, RuntimeFailure> {
        let exit = Arc::new(Mutex::new(None));
        let process_exit = Arc::clone(&exit);
        let pty = context.launch(
            |size| {
                let parts = factory
                    .create(launch, native_size(size))
                    .map_err(|_| failure(RuntimeStage::Launch))?;
                Ok(PtyParts {
                    process: Box::new(HostProcess {
                        adapter: parts.adapter,
                        exit: process_exit,
                    }),
                    termination: Arc::new(HostTermination(parts.termination)),
                })
            },
            move || Ok(authority.grant()),
            move || {
                let _ = commands.send(Command::ReaderReady(None));
            },
        )?;
        Ok(Self::Runtime { pty, exit })
    }
    /// Returns false only for a capacity rejection before either resize effect.
    pub(super) fn resize(
        &mut self,
        geometry: TerminalGeometry,
        emulator: &mut TerminalEmulator,
    ) -> Result<bool, TerminalFailure> {
        let size = pty_size(geometry);
        let winsize_changed = size != pty_size(emulator.geometry());
        let mut engine_failure = None;
        let resize_engine = || {
            emulator
                .resize_for_pty(geometry, winsize_changed)
                .map_err(|error| {
                    engine_failure = Some(TerminalFailure::emulator_error(
                        "resize-terminal-state",
                        error,
                    ));
                    failure(RuntimeStage::Resize)
                })
        };
        let result = match self {
            Self::Runtime { pty, .. } => pty.resize_with(runtime_size(size), resize_engine),
            #[cfg(test)]
            Self::Fixture(pty) => pty
                .resize(size)
                .map_err(|_| failure(RuntimeStage::Resize))
                .and_then(|()| {
                    let mut resize_engine = resize_engine;
                    resize_engine()
                }),
        };
        match result {
            Ok(()) => Ok(true),
            Err(error) if error.class == FailureClass::ResourceLimit => Ok(false),
            Err(_) => Err(engine_failure.unwrap_or_else(|| {
                TerminalFailure::pty("resize-pty")
                    .with_reason(FailureReason::Io(io::ErrorKind::Other))
            })),
        }
    }
    pub(super) fn hidden_input(&self) -> Result<bool, NativePtyOperationFailure> {
        match self {
            Self::Runtime { pty, .. } => pty
                .hidden_input()
                .map_err(|_| NativePtyOperationFailure::new(io::ErrorKind::Other)),
            #[cfg(test)]
            Self::Fixture(pty) => pty.hidden_input(),
        }
    }
    pub(super) fn wait_for_exit(
        &mut self,
        timeout: Duration,
    ) -> Result<NativePtyExit, NativePtyWaitFailure> {
        match self {
            Self::Runtime { pty, exit } => {
                let _ = pty.wait_for_exit(timeout);
                exit.lock()
                    .map_err(|_| NativePtyWaitFailure::new(io::ErrorKind::Other))?
                    .clone()
                    .ok_or_else(|| NativePtyWaitFailure::new(io::ErrorKind::Other))?
            }
            #[cfg(test)]
            Self::Fixture(pty) => pty.wait_for_exit(timeout),
        }
    }
    pub(super) fn receive_output(
        &mut self,
        _epoch: Option<u64>,
        #[cfg(test)] _fixture_events: &mpsc::Receiver<NativePtyOutput>,
    ) -> Result<LegacyPtyOutput<Option<u64>>, TerminalFailure> {
        match self {
            Self::Runtime { pty, .. } => pty
                .try_output()
                .map_err(|_| TerminalFailure::pty("receive-shell-output"))?
                .ok_or_else(|| {
                    TerminalFailure::pty("receive-shell-output")
                        .with_reason(FailureReason::ChannelClosed)
                }),
            #[cfg(test)]
            Self::Fixture(_) => match _fixture_events.recv() {
                Ok(NativePtyOutput::Bytes(bytes)) => Ok(LegacyPtyOutput::Bytes(bytes, _epoch)),
                Ok(NativePtyOutput::Stopped(error)) => Ok(LegacyPtyOutput::Stopped(
                    error.map_or(Ok(()), |_| Err(failure(RuntimeStage::Reader))),
                )),
                Err(_) => Err(TerminalFailure::pty("receive-shell-output")
                    .with_reason(FailureReason::ChannelClosed)),
            },
        }
    }
}
impl Write for CompatibilityPty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Runtime { pty, .. } => pty.write(bytes),
            #[cfg(test)]
            Self::Fixture(pty) => pty.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Runtime { pty, .. } => pty.flush(),
            #[cfg(test)]
            Self::Fixture(pty) => pty.flush(),
        }
    }
}
#[cfg(test)]
impl From<NativePtyOwner> for CompatibilityPty {
    fn from(pty: NativePtyOwner) -> Self {
        Self::Fixture(pty)
    }
}
