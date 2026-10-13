//! Native launch and retained-cleanup seams around the frozen runtime ports.
//! Providers retain filesystem, geometry, process and launch authority privately.

use crate::contracts::{RuntimeFactoryPort, RuntimeOwnerPort, ServerFailure, ServerFuture};
use spaceterm_core::{
    observations::*, retirement::SessionCleanupObservation, runtime_authority::RuntimeLifecycle,
};
use spaceterm_host::{
    git::filesystem::RetainedDirectory,
    pty::{
        NativePtyCloseHandle, NativePtyOutputSink, NativePtyOwner, NativePtySize,
        UnixNativePtyAdapterFactory,
    },
    server_directories::native::NativeDirectoryProvider,
    shell_launch::{PreparedShellLaunch, ShellLaunchPlanner},
};
use spaceterm_terminal::{metadata::PromptOwner, runtime_contracts::RuntimeStartRequest};
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

/// Comparison gate at the native process-creation boundary. Cancellation wins
/// until that boundary is crossed. An admitted process then belongs to the
/// server, never to the connection that supplied its original measurement.
pub struct LaunchGate(AtomicU8);
impl Default for LaunchGate {
    fn default() -> Self {
        Self(AtomicU8::new(0))
    }
}
impl LaunchGate {
    pub fn admit_process(&self) -> Result<(), ServerFailure> {
        self.0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| {
                crate::failure::failure(crate::contracts::ServerFailureClass::StaleAuthority)
            })
    }
    pub fn process_admitted(&self) -> bool {
        self.0.load(Ordering::Acquire) == 1
    }
    pub fn cancel(&self) {
        let _ = self
            .0
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
    }
}
/// Direct account PTY preparation for the runtime provider. The actor must reserve
/// aggregate grid residency and commit StartupReserved before invoking this path.
/// This value is neither a protocol request nor a replacement runtime owner.
pub struct AccountShellLaunch {
    launch: PreparedShellLaunch,
    directory: Arc<dyn RetainedDirectory>,
    size: NativePtySize,
}
impl std::fmt::Debug for AccountShellLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccountShellLaunch([redacted])")
    }
}
impl AccountShellLaunch {
    /// The runtime supplies its fresh per-Session owner and retains the same value
    /// in terminal metadata. It is host-only launch authority, never request data.
    pub fn prepare(
        planner: &ShellLaunchPlanner,
        directories: &NativeDirectoryProvider,
        request: &RuntimeStartRequest,
        prompt_owner: &PromptOwner,
    ) -> Result<Self, ServerFailure> {
        let grid = request.reservation().grid();
        let directory = directories.retain(request.directory()).map_err(|error| {
            use spaceterm_core::server_directories::DirectoryFailure;
            crate::failure::failure(match error {
                DirectoryFailure::WrongServer => {
                    crate::contracts::ServerFailureClass::ScopeMismatch
                }
                DirectoryFailure::Capacity => crate::contracts::ServerFailureClass::Capacity,
                _ => crate::contracts::ServerFailureClass::RuntimeUnavailable,
            })
        })?;
        let launch = planner
            .local(directory.path())
            .map_err(|_| {
                crate::failure::failure(crate::contracts::ServerFailureClass::RuntimeUnavailable)
            })?
            .with_environment_value(PromptOwner::ENVIRONMENT, prompt_owner.value());
        let (pixel_width, pixel_height) = grid.pty_pixels();
        Ok(Self {
            launch,
            directory,
            size: NativePtySize {
                rows: grid.rows(),
                columns: grid.columns(),
                pixel_width,
                pixel_height,
            },
        })
    }

    pub fn size(&self) -> NativePtySize {
        self.size
    }

    /// Runs on the runtime's blocking worker, never a transport task. The existing
    /// Unix PTY adapter preserves ordinary account groups, shell integration and
    /// the process descriptor guard. The returned owner joins native cleanup.
    pub fn start(
        self,
        gate: &LaunchGate,
        output: Arc<dyn NativePtyOutputSink>,
        close: &NativePtyCloseHandle,
    ) -> Result<NativePtyOwner, ServerFailure> {
        self.directory.revalidate().map_err(|_| {
            crate::failure::failure(crate::contracts::ServerFailureClass::RuntimeUnavailable)
        })?;
        gate.admit_process()?;
        #[cfg(target_os = "macos")]
        let host = spaceterm_host::pty::macos_pty_host::MacosPtyHost;
        #[cfg(target_os = "linux")]
        let host = spaceterm_host::pty::linux_pty_host::LinuxPtyHost;
        let factory = UnixNativePtyAdapterFactory::new(Arc::new(host));
        NativePtyOwner::start(&factory, self.launch, self.size, output, close).map_err(|_| {
            crate::failure::failure(crate::contracts::ServerFailureClass::RuntimeUnavailable)
        })
    }
}

/// Returned only after owning-filesystem validation and aggregate geometry
/// reservation. It is consumed at the final launch boundary and is never wire data.
/// The provider must admit the gate before creating a process and release its
/// preparation resources before returning an error. Post-admission failures must
/// retain and join native cleanup; an error alone is not cleanup confirmation.
pub trait PreparedLaunchPort: Send {
    fn start(
        self: Box<Self>,
        factory: Arc<dyn RuntimeFactoryPort>,
        request: RuntimeStartRequest,
        gate: Arc<LaunchGate>,
    ) -> ServerFuture<'static, Result<Box<dyn RuntimeOwnerPort>, ServerFailure>>;
}
pub trait LaunchPreparationPort: Send + Sync {
    fn prepare<'a>(
        &'a self,
        request: &'a RuntimeStartRequest,
    ) -> ServerFuture<
        'a,
        Result<Box<dyn PreparedLaunchPort>, spaceterm_core::operations::OperationFailure>,
    >;
}
/// The runtime's retained supervisor handles unexpected actor loss and uncertain
/// durability without fabricated NativeManagementRequestRef or OperationId.
/// Completion must join the owner, reader and process cleanup. No default exists.
pub trait RuntimeCleanupPort: Send + Sync {
    fn abandon(
        &self,
        owner: Box<dyn RuntimeOwnerPort>,
    ) -> ServerFuture<'static, Result<SessionCleanupObservation, ServerFailure>>;
}

#[derive(Clone)]
pub struct RetainedRuntimeObservations {
    pub lifecycle: RuntimeLifecycle,
    pub confirmed_directory: Option<ConfirmedDirectoryObservation>,
    pub display_directory: Option<DisplayDirectoryObservation>,
    pub close_work: Option<CloseWorkObservation>,
    pub cleanup: Option<SessionCleanupObservation>,
}
impl RetainedRuntimeObservations {
    pub fn capture(owner: &dyn RuntimeOwnerPort) -> Self {
        Self {
            lifecycle: owner.lifecycle_snapshot(),
            confirmed_directory: owner.confirmed_directory_snapshot(),
            display_directory: owner.display_directory_snapshot(),
            close_work: owner.close_work_snapshot(),
            cleanup: owner.cleanup_snapshot(),
        }
    }
}

mod native_runtime;
pub use native_runtime::{
    AccountLaunchFacts, AccountLaunchProvider, NativeRuntime, NativeRuntimeClock,
};
