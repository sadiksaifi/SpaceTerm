//! Account-local normal launch through the approved retained service runtime.
use crate::{AuthenticatedStatus, CoreError, MachineConnection, service::Service};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Why a client launches the account's SpaceTerm Core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchIntent {
    /// The person opened the desktop. Launch starts a SpaceTerm Core they stopped, as explicit
    /// Start does.
    Foreground,
    /// Login, reconnecting, or a window reopened in the running desktop. A deliberate Stop
    /// stays in force.
    Passive,
}

/// The observed result of normal launch. Only explicit Start and a foreground launch reverse
/// Stop.
pub enum StartupOutcome {
    Ready {
        core: MachineConnection,
        status: AuthenticatedStatus,
    },
    Stopped,
    AuthorizationRequired,
    /// Another service operation, such as Update, Restart or Stop, holds the service. Present
    /// it, call [`LocalStartup::wait_for_service`], then run launch again.
    ServiceBusy,
    Cancelled,
}

/// Coordinates service reuse, first installation and owning-account client admission.
/// Construction only resolves paths; it never installs or starts a service.
#[derive(Clone)]
pub struct LocalStartup {
    service: Service,
    core: MachineConnection,
}

impl LocalStartup {
    pub fn for_identity(identity: &str) -> Result<Self, CoreError> {
        Self::with_service(Service::for_identity(identity)?)
    }

    pub(crate) fn with_service(service: Service) -> Result<Self, CoreError> {
        let core = service.local_core()?;
        Ok(Self { service, core })
    }

    /// Reuse retained service policy, or provision one exact verified retained artifact.
    ///
    /// `provision` runs only when no service policy exists. It returns an account-retained
    /// installation path; Service verifies that artifact before registration. It must not call
    /// service controls. Cancellation is observed while waiting for the launch lock and before
    /// and after provisioning. Once registration/start begins, this operation reports its result
    /// without rolling it back. A cancelled provisioning callback may leave a staged artifact.
    ///
    /// AuthorizationRequired never opens an authorization prompt. Resume by explicitly calling
    /// Service::enable_linger with the chosen authorization surface, then running launch again.
    pub fn run(
        &self,
        provision: impl FnOnce() -> Result<PathBuf, CoreError>,
        cancelled: impl Fn() -> bool,
    ) -> Result<StartupOutcome, CoreError> {
        self.launch(LaunchIntent::Passive, provision, cancelled, &|| {})
    }

    /// [`Self::run`] for `intent`. `starting` runs once, right before this launch installs or
    /// starts the server, so the client can say SpaceTerm Core is starting.
    pub fn launch(
        &self,
        intent: LaunchIntent,
        provision: impl FnOnce() -> Result<PathBuf, CoreError>,
        cancelled: impl Fn() -> bool,
        starting: &dyn Fn(),
    ) -> Result<StartupOutcome, CoreError> {
        let result = (|| {
            let Some(status) = self
                .service
                .launch(intent, provision, &cancelled, starting)?
            else {
                return Ok(StartupOutcome::Cancelled);
            };
            if status.stopped {
                return Ok(StartupOutcome::Stopped);
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            // Launch has admitted the service operation. Observe its result even if the
            // initiating presentation has since closed; cancellation cannot undo that Start.
            let status = loop {
                let observed = match self.core.refresh() {
                    Err(CoreError::Capacity) => self.core.refresh_cancellable(&cancelled),
                    result => result,
                };
                match observed {
                    Err(CoreError::ServiceBusy) if cancelled() => {
                        return Ok(StartupOutcome::Cancelled);
                    }
                    Err(CoreError::NotRunning) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    result => break result?,
                }
            };
            Ok(StartupOutcome::Ready {
                core: self.core.clone(),
                status,
            })
        })();
        match result {
            Err(CoreError::Stopped) => Ok(StartupOutcome::Stopped),
            Err(CoreError::AuthorizationRequired) => Ok(StartupOutcome::AuthorizationRequired),
            Err(CoreError::ServiceBusy) => Ok(StartupOutcome::ServiceBusy),
            other => other,
        }
    }

    /// Wait for the service operation that made launch report [`StartupOutcome::ServiceBusy`].
    /// The wait wakes when the operation ends, never on a polling interval. A stuck operation
    /// fails with ServiceBusy after a bounded wait. Cancellation returns at once.
    pub fn wait_for_service(&self, cancelled: impl Fn() -> bool) -> Result<(), CoreError> {
        self.service.wait_for_operation(cancelled)
    }
}
