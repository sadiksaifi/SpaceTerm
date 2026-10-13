//! Shared account-service management for native clients. Discovery is read-only.
//! The host owns registration, original request IDs, assessments and supervision.

pub mod artifacts;

use artifacts::{ArtifactTransport, ServerArtifacts};
use std::sync::Arc;

use spaceterm_client::management_cli::{
    AssessmentToken, ManagementClientKind, ManagementFailure, ManagementFuture, StatusBackend,
    StatusManagement, StatusOnlyManagement,
};
use spaceterm_host::{
    account_service::{
        ConfirmedServiceAction, LingerAuthorization, ServiceAction, ServiceAssessment,
        ServiceFailure, ServiceFuture, ServiceProgress, ServiceStatus, ServiceSupervisor,
        SupervisorState,
    },
    native_deployment::contracts::NativeRequestId,
    native_management::SudoMode,
    startup::{AccountBuildKey, DesiredServerState},
};

use crate::cli::{
    arguments::ConfirmationMode,
    bindings::{
        NativeCliCommand, NativeCommandBindings, NativeObservation, ProcessObservation,
        RegistrationAvailability,
    },
    output::{CliOutput, hex},
    runner::CliIo,
};

/// One compiled account/build provider. It cannot select a caller-supplied home,
/// executable, service label or server identity.
#[derive(Clone)]
pub struct NativeClientProvider {
    management: Arc<NativeServiceManagement>,
    companion: Option<Arc<dyn StatusManagement>>,
}
impl NativeClientProvider {
    /// Read-only production construction. Linux install requires a separately
    /// injected linger authorization adapter; no constructor prompts or installs.
    pub fn current() -> Result<Self, ManagementFailure> {
        Self::with_authorization(None)
    }

    /// Native composition selects CLI sudo, desktop pkexec or scoped SSH
    /// authorization. This capability enables only the fixed linger operation.
    pub fn with_authorization(
        authorization: Option<Arc<dyn LingerAuthorization>>,
    ) -> Result<Self, ManagementFailure> {
        ServiceSupervisor::current(authorization)
            .map(Self::from_supervisor)
            .map_err(classify_service)
    }

    /// Shares the retained lifecycle owner instead of creating another journal.
    pub fn from_supervisor(supervisor: ServiceSupervisor) -> Self {
        Self {
            management: Arc::new(NativeServiceManagement {
                supervisor,
                artifacts: Arc::new(ServerArtifacts::default()),
            }),
            companion: None,
        }
    }

    /// Replaces only external downloads. Installation still requires the host's
    /// compiled trust root and explicit verified-artifact admission.
    pub fn with_artifact_transport(mut self, transport: Arc<dyn ArtifactTransport>) -> Self {
        self.management = Arc::new(NativeServiceManagement {
            supervisor: self.management.supervisor.clone(),
            artifacts: Arc::new(ServerArtifacts::new(transport)),
        });
        self
    }

    pub fn key(&self) -> AccountBuildKey {
        self.management.key()
    }

    pub fn management(&self) -> Arc<NativeServiceManagement> {
        Arc::clone(&self.management)
    }

    pub fn inspect(&self, key: AccountBuildKey) -> ManagementFuture<'_, NativeObservation> {
        self.management.inspect(key)
    }

    /// Pairing composition supplies the worker for the companion's own saved,
    /// server-issued additional Client identity. Desktop and CLI kinds are refused.
    /// StatusOnlyManagement rechecks that identity on every read and subscription.
    pub fn with_companion_backend<B: StatusBackend + 'static>(
        mut self,
        saved: spaceterm_client::journal::SavedBinding,
        verifier: &dyn spaceterm_client::journal::ReceiptVerifier,
        backend: B,
    ) -> Result<Self, ManagementFailure> {
        saved
            .validate(verifier)
            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
        if !matches!(
            saved.proof().parts().claims,
            spaceterm_core::pairing::PairingReceiptBody::ClientBinding { .. }
        ) {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        let binding = saved
            .binding()
            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
        let pin = binding.target().pin();
        let backend = SavedCompanionBackend { saved, backend };
        match backend.context() {
            Ok(_) | Err(ManagementFailure::Offline) => {}
            Err(failure) => return Err(failure),
        }
        self.companion = Some(Arc::new(NativeCompanionStatus {
            native: Arc::clone(&self.management),
            pin,
            status: StatusOnlyManagement::new(backend),
        }));
        Ok(self)
    }

    /// Missing saved identity is an enrollment prerequisite, not an offline or
    /// stopped server. This accessor never enrolls, starts or borrows an identity.
    pub fn companion_status(&self) -> Result<Arc<dyn StatusManagement>, ManagementFailure> {
        self.companion
            .as_ref()
            .map(Arc::clone)
            .ok_or(ManagementFailure::PairingRequired)
    }
}

struct SavedCompanionBackend<B> {
    saved: spaceterm_client::journal::SavedBinding,
    backend: B,
}
impl<B: StatusBackend> StatusBackend for SavedCompanionBackend<B> {
    fn context(
        &self,
    ) -> Result<spaceterm_client::management_cli::AuthenticatedManagementContext, ManagementFailure>
    {
        let context = self.backend.context()?;
        let binding = self
            .saved
            .binding()
            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
        if context.kind() != ManagementClientKind::StatusCompanion
            || context.stream() != self.saved.streams().parts().ordinary
            || context.device() != binding.target().device()
        {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        Ok(context)
    }
    fn status(&self) -> ManagementFuture<'_, spaceterm_core::management::ServerStatus> {
        self.backend.status()
    }
    fn subscribe_status(
        &self,
    ) -> ManagementFuture<'_, spaceterm_client::management_cli::StatusSubscription> {
        self.backend.subscribe_status()
    }
}

struct NativeCompanionStatus<B> {
    native: Arc<NativeServiceManagement>,
    pin: spaceterm_protocol::authentication::ServerPin,
    status: StatusOnlyManagement<SavedCompanionBackend<B>>,
}
impl<B: StatusBackend> NativeCompanionStatus<B> {
    async fn ready(&self) -> Result<(), ManagementFailure> {
        let status = self.native.status().await.map_err(classify_service)?;
        if status.server.is_none()
            || status.desired != Some(DesiredServerState::Running)
            || matches!(
                status.supervisor,
                SupervisorState::NotInstalled
                    | SupervisorState::Suppressed
                    | SupervisorState::Disabled
            )
        {
            return Err(ManagementFailure::NativeUnavailable);
        }
        if status.server.map(|server| server.bytes()) != Some(self.pin.identity)
            || self
                .native
                .supervisor
                .discover()
                .map_err(classify_service)?
                .is_none_or(|registration| registration.server_pin() != self.pin)
        {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        Ok(())
    }
}
impl<B: StatusBackend> StatusManagement for NativeCompanionStatus<B> {
    fn status(&self) -> ManagementFuture<'_, spaceterm_core::management::ServerStatus> {
        Box::pin(async {
            self.ready().await?;
            self.status.status().await
        })
    }
    fn subscribe_status(
        &self,
    ) -> ManagementFuture<'_, spaceterm_client::management_cli::StatusSubscription> {
        Box::pin(async {
            self.ready().await?;
            self.status.subscribe_status().await
        })
    }
}

/// Narrow native lifecycle surface shared by CLI and desktop Settings.
/// Methods retain the host's original assessment, request and completion record.
pub struct NativeServiceManagement {
    supervisor: ServiceSupervisor,
    artifacts: Arc<ServerArtifacts>,
}
impl NativeServiceManagement {
    pub fn key(&self) -> AccountBuildKey {
        self.supervisor.key()
    }

    pub fn supervisor(&self) -> &ServiceSupervisor {
        &self.supervisor
    }

    pub fn status(&self) -> ServiceFuture<'_, ServiceStatus> {
        self.supervisor.status()
    }

    /// Fetches signed metadata only. The result grants no startup or installation authority.
    pub fn check_installation(
        &self,
        version: Option<crate::cli::arguments::StableVersion>,
    ) -> ManagementFuture<'_, spaceterm_distribution::VerifiedAccountServerFeed> {
        let artifacts = Arc::clone(&self.artifacts);
        let build = self.key().build_identity;
        Box::pin(async move {
            tokio::task::spawn_blocking(move || artifacts.check(build, version.as_ref()))
                .await
                .map_err(|_| ManagementFailure::NativeUnavailable)?
        })
    }

    /// Checks the compiled channel's signed feed against approved code. This
    /// reads registration without contacting a manager or changing Stopped intent.
    pub fn check_update(
        &self,
    ) -> ManagementFuture<'_, Option<spaceterm_distribution::BuildVersion>> {
        Box::pin(async move {
            let registration = self.supervisor.discover().map_err(classify_service)?;
            let feed = self.check_installation(None).await?;
            let candidate = &feed.document().version;
            let current = registration
                .as_ref()
                .and_then(|registration| registration.approved_installation())
                .map(|approved| &approved.version);
            Ok(newer_version(candidate, current).then(|| candidate.clone()))
        })
    }

    /// Downloads and verifies one signed artifact on the blocking pool. It never
    /// creates identity, service configuration, credentials or registration.
    pub fn download_installation(
        &self,
        version: Option<crate::cli::arguments::StableVersion>,
    ) -> ManagementFuture<'_, spaceterm_distribution::VerifiedAccountServerArtifact> {
        let artifacts = Arc::clone(&self.artifacts);
        let build = self.key().build_identity;
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                artifacts.download_artifact(build, version.as_ref())
            })
            .await
            .map_err(|_| ManagementFailure::NativeUnavailable)?
        })
    }

    pub fn start(&self, request: NativeRequestId) -> ServiceFuture<'_, ServiceProgress> {
        self.supervisor.start(request)
    }

    /// Allocates once and captures the original runtime scope and expiry.
    pub fn assess(&self, action: ServiceAction) -> ServiceFuture<'_, ServiceAssessment> {
        Box::pin(async move {
            let request = self.supervisor.allocate_request()?;
            self.supervisor.assess(request, action).await
        })
    }

    /// Restoring cannot refresh expiry, choose a new action or allocate an ID.
    pub fn restore_assessment(
        &self,
        action: ServiceAction,
        token: AssessmentToken,
    ) -> ManagementFuture<'_, ServiceAssessment> {
        Box::pin(async move {
            let (request, comparison) = decode_token(&token)?;
            let assessment = self
                .supervisor
                .restore_assessment(request, comparison)
                .await
                .map_err(classify_service)?;
            if assessment.action() != action {
                return Err(ManagementFailure::StaleAssessment);
            }
            Ok(assessment)
        })
    }

    pub fn stop(&self, action: ConfirmedServiceAction) -> ServiceFuture<'_, ServiceProgress> {
        self.supervisor.stop(action)
    }

    pub fn restart(&self, action: ConfirmedServiceAction) -> ServiceFuture<'_, ServiceProgress> {
        self.supervisor.restart(action)
    }

    pub fn uninstall(&self, action: ConfirmedServiceAction) -> ServiceFuture<'_, ServiceProgress> {
        self.supervisor.uninstall(action)
    }

    pub fn resolve(&self, request: NativeRequestId) -> ServiceFuture<'_, ServiceProgress> {
        self.supervisor.resolve(request)
    }

    /// The caller must durably retain the original result before releasing its
    /// host-owned capacity. Writing CLI output alone is not an acknowledgement.
    pub fn acknowledge_request(&self, request: NativeRequestId) -> Result<(), ServiceFailure> {
        self.supervisor.acknowledge_request(request)
    }

    fn check_key(&self, key: AccountBuildKey) -> Result<(), ManagementFailure> {
        if key != self.key() {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        Ok(())
    }
}

/// Content-free all-client confirmation facts. A displayed token is a lookup
/// reference only. The host still revalidates the original exact assessment.
#[derive(Clone, Debug)]
pub struct NativeActionPrompt {
    pub request: NativeRequestId,
    pub action: ServiceAction,
    pub affected_sessions: u32,
    pub token: AssessmentToken,
}
impl NativeActionPrompt {
    pub fn from_assessment(assessment: &ServiceAssessment) -> Result<Self, ManagementFailure> {
        Ok(Self {
            request: assessment.request(),
            action: assessment.action(),
            affected_sessions: assessment.affected_sessions(),
            token: AssessmentToken::new(format!(
                "native_{}_{}",
                hex(assessment.request().as_bytes()),
                hex(&assessment.token()),
            ))?,
        })
    }

    pub fn description(&self) -> String {
        let effect = match self.action {
            ServiceAction::Stop => "Stop the server and suspend automatic startup",
            ServiceAction::Restart => "Restart the running server using its approved version",
            ServiceAction::Uninstall => {
                "Uninstall the service and verified code, retaining server data and pairing identities"
            }
            ServiceAction::Activate(_) => "Activate the exact staged server version",
        };
        format!(
            "{effect}. End {} Terminal Sessions for every client. Keep repository files.",
            self.affected_sessions
        )
    }
}

fn decode_token(token: &AssessmentToken) -> Result<(NativeRequestId, [u8; 32]), ManagementFailure> {
    let body = token
        .as_str()
        .strip_prefix("native_")
        .ok_or(ManagementFailure::StaleAssessment)?;
    let (request, comparison) = body
        .split_once('_')
        .ok_or(ManagementFailure::StaleAssessment)?;
    let request = crate::cli::arguments::parse_hex(request)
        .map_err(|_| ManagementFailure::StaleAssessment)?;
    let comparison = crate::cli::arguments::parse_hex(comparison)
        .map_err(|_| ManagementFailure::StaleAssessment)?;
    let request = NativeRequestId::new(request).map_err(|_| ManagementFailure::StaleAssessment)?;
    Ok((request, comparison))
}

impl NativeCommandBindings for NativeServiceManagement {
    fn inspect(&self, key: AccountBuildKey) -> ManagementFuture<'_, NativeObservation> {
        Box::pin(async move {
            self.check_key(key)?;
            let status = self.status().await.map_err(classify_service)?;
            if status.key != key {
                return Err(ManagementFailure::UnsafeNativeState);
            }
            if status.server.is_none()
                || matches!(
                    status.supervisor,
                    SupervisorState::NotInstalled | SupervisorState::Suppressed
                )
                || (status.supervisor == SupervisorState::Disabled
                    && status.desired != Some(DesiredServerState::Stopped))
            {
                return Err(ManagementFailure::NativeUnavailable);
            }
            Ok(NativeObservation {
                availability: RegistrationAvailability::Registered,
                desired: status.desired,
                process: if status.supervisor == SupervisorState::Running {
                    ProcessObservation::Observed
                } else {
                    ProcessObservation::Unknown
                },
                // Native manager observation is not protocol authentication or
                // a positive process-absence certificate.
                verified: false,
                approved: status
                    .approved
                    .map(|installation| installation.installation),
                staged: status.staged.map(|installation| installation.installation),
            })
        })
    }

    fn run<'a>(
        &'a self,
        key: AccountBuildKey,
        command: NativeCliCommand,
        confirmation: ConfirmationMode,
        mode: SudoMode,
        io: &'a mut dyn CliIo,
    ) -> ManagementFuture<'a, CliOutput> {
        Box::pin(async move {
            self.check_key(key)?;
            if let NativeCliCommand::Resolve { request } = command {
                return self
                    .resolve(request)
                    .await
                    .map(CliOutput::ServiceProgress)
                    .map_err(classify_service);
            }
            if command == NativeCliCommand::UpdateCheck {
                return Ok(CliOutput::Update {
                    available_version: self
                        .check_update()
                        .await?
                        .map(|version| version.as_str().to_owned()),
                });
            }
            if command == NativeCliCommand::Start {
                let request = self
                    .supervisor
                    .allocate_request()
                    .map_err(classify_service)?;
                crate::cli::output::emit_native_request(request, io)?;
                return self
                    .start(request)
                    .await
                    .map(CliOutput::ServiceProgress)
                    .map_err(classify_service);
            }
            let action = match command {
                NativeCliCommand::Stop => ServiceAction::Stop,
                NativeCliCommand::Restart => ServiceAction::Restart,
                NativeCliCommand::Uninstall => ServiceAction::Uninstall,
                NativeCliCommand::UpdateActivate { installation } => {
                    ServiceAction::Activate(installation)
                }
                _ => return Err(ManagementFailure::NativeUnavailable),
            };
            if mode == SudoMode::NonInteractive && confirmation == ConfirmationMode::Interactive {
                return Err(ManagementFailure::ConfirmationRequired);
            }
            let assessment = match &confirmation {
                ConfirmationMode::Token(token) => {
                    self.restore_assessment(action, token.clone()).await?
                }
                _ => {
                    let request = self
                        .supervisor
                        .allocate_request()
                        .map_err(classify_service)?;
                    crate::cli::output::emit_native_request(request, io)?;
                    self.supervisor
                        .assess(request, action)
                        .await
                        .map_err(classify_service)?
                }
            };
            let prompt = NativeActionPrompt::from_assessment(&assessment)?;
            match confirmation {
                ConfirmationMode::Assess => return Ok(CliOutput::ServiceAssessment(prompt)),
                ConfirmationMode::Interactive => io.confirm_native_action(&prompt)?,
                ConfirmationMode::Token(_) => {}
            }
            let confirmed = assessment.confirm().map_err(classify_service)?;
            let progress = match action {
                ServiceAction::Stop => self.stop(confirmed).await,
                ServiceAction::Restart => self.restart(confirmed).await,
                ServiceAction::Uninstall => self.uninstall(confirmed).await,
                ServiceAction::Activate(_) => self.supervisor.activate(confirmed).await,
            }
            .map_err(classify_service)?;
            Ok(CliOutput::ServiceProgress(progress))
        })
    }
}

fn newer_version(
    candidate: &spaceterm_distribution::BuildVersion,
    current: Option<&spaceterm_distribution::BuildVersion>,
) -> bool {
    let Some(current) = current else { return true };
    match spaceterm_distribution::StableVersion::parse(candidate.as_str()) {
        Ok(candidate) if spaceterm_distribution::StableVersion::parse(current.as_str()).is_ok() => {
            candidate.supersedes(current.as_str())
        }
        _ => candidate != current,
    }
}

/// Typed native classification. Raw OS failures, paths and utility output never
/// cross into diagnostics, CLI output or Settings state.
pub fn classify_service(failure: ServiceFailure) -> ManagementFailure {
    match failure {
        ServiceFailure::RootAccount
        | ServiceFailure::AuthorizationDenied
        | ServiceFailure::AuthorizationCancelled => ManagementFailure::AuthorizationDenied,
        ServiceFailure::UnsupportedHost => ManagementFailure::UnsupportedHost,
        ServiceFailure::Missing
        | ServiceFailure::SupervisorUnavailable
        | ServiceFailure::LoginSessionUnavailable
        | ServiceFailure::Suppressed => ManagementFailure::NativeUnavailable,
        ServiceFailure::StorageLocked => ManagementFailure::StorageLocked,
        ServiceFailure::StorageUnavailable => ManagementFailure::StorageUnavailable,
        ServiceFailure::UnsafeStorage
        | ServiceFailure::InvalidRecord
        | ServiceFailure::UnsupportedSchema
        | ServiceFailure::IdentityMismatch
        | ServiceFailure::ArtifactRejected => ManagementFailure::UnsafeNativeState,
        ServiceFailure::IncompatibleStorage => ManagementFailure::Incompatible,
        ServiceFailure::UnknownRequest
        | ServiceFailure::RequestConflict
        | ServiceFailure::RequestRetired
        | ServiceFailure::StaleAssessment => ManagementFailure::StaleAssessment,
        ServiceFailure::ConfirmationRequired => ManagementFailure::ConfirmationRequired,
        ServiceFailure::AuthorizationUnavailable => ManagementFailure::AuthorizationUnavailable,
        ServiceFailure::LimitExceeded => ManagementFailure::LimitExceeded,
        ServiceFailure::Pending | ServiceFailure::CleanupUnresolved => {
            ManagementFailure::OutcomePending
        }
        ServiceFailure::Indeterminate => ManagementFailure::Indeterminate,
    }
}

#[cfg(test)]
mod tests {
    use super::newer_version;
    use spaceterm_distribution::BuildVersion;

    #[test]
    fn signed_update_candidate_compares_numeric_versions_and_never_offers_a_stable_downgrade() {
        for (candidate, current, expected) in [
            ("1.10.0", Some("1.9.9"), true),
            ("2.0.0", Some("1.99.99"), true),
            ("1.2.3", Some("1.2.3"), false),
            ("1.2.2", Some("1.2.3"), false),
            ("1.9.0", Some("1.10.0"), false),
            ("1.2.3", None, true),
            ("dev.123456789abc", Some("dev.123456789abc"), false),
            ("dev.123456789abc", Some("dev.abcdef123456"), true),
            ("dev.123456789abc.dirty", Some("dev.123456789abc"), true),
        ] {
            let candidate = BuildVersion::parse(candidate).unwrap();
            let current = current.map(|current| BuildVersion::parse(current).unwrap());
            assert_eq!(newer_version(&candidate, current.as_ref()), expected);
        }
    }
}
