//! Account-local normal launch through the approved retained service runtime.
use crate::{AuthenticatedStatus, LocalServer, ServerError, service::Service};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The observed result of normal launch. Only explicit Start can reverse Stop.
pub enum StartupOutcome {
    Ready {
        server: LocalServer,
        status: AuthenticatedStatus,
    },
    Stopped,
    AuthorizationRequired,
    Cancelled,
}

/// Coordinates service reuse, first installation and owning-account client admission.
/// Construction only resolves paths; it never installs or starts a service.
#[derive(Clone)]
pub struct LocalStartup {
    service: Service,
    server: LocalServer,
}

impl LocalStartup {
    pub fn for_identity(identity: &str) -> Result<Self, ServerError> {
        Self::with_service(Service::for_identity(identity)?)
    }

    pub(crate) fn with_service(service: Service) -> Result<Self, ServerError> {
        let server = service.local_server()?;
        Ok(Self { service, server })
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
        provision: impl FnOnce() -> Result<PathBuf, ServerError>,
        cancelled: impl Fn() -> bool,
    ) -> Result<StartupOutcome, ServerError> {
        let result = (|| {
            let Some(status) = self.service.launch(provision, cancelled)? else {
                return Ok(StartupOutcome::Cancelled);
            };
            if status.stopped {
                return Ok(StartupOutcome::Stopped);
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            let status = loop {
                match self.server.refresh() {
                    Err(ServerError::NotRunning) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    result => break result?,
                }
            };
            Ok(StartupOutcome::Ready {
                server: self.server.clone(),
                status,
            })
        })();
        match result {
            Err(ServerError::Stopped) => Ok(StartupOutcome::Stopped),
            Err(ServerError::AuthorizationRequired) => Ok(StartupOutcome::AuthorizationRequired),
            other => other,
        }
    }
}
