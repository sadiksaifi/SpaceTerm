//! Native launch and retained-cleanup seams around the frozen runtime ports.
//! Providers retain filesystem, geometry, process and launch authority privately.

use crate::contracts::{RuntimeFactoryPort, RuntimeOwnerPort, ServerFailure, ServerFuture};
use spaceterm_core::{
    observations::*, retirement::SessionCleanupObservation, runtime_authority::RuntimeLifecycle,
};
use spaceterm_terminal::runtime_contracts::RuntimeStartRequest;
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
