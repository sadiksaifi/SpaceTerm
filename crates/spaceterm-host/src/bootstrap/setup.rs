//! Fixed unprivileged account setup operations for deliberate SSH Bootstrap.
//!
//! This endpoint delegates identity creation, installation and publication to the
//! existing account-service owner. Lookup never enters its resumptive lifecycle APIs.

use spaceterm_core::{identity::ServerId, operations::RequestDigest, pairing::EnrollmentProposal};
use spaceterm_distribution::{BuildVersion, VerifiedAccountServerArtifact};
use spaceterm_protocol::{authentication::ServerPin, pairing_adapter::enrollment_proposal_digest};

use crate::{
    account_service::{
        ConfirmedServiceInstall, LingerState, ServiceFailure, ServiceFuture,
        ServiceInstallAssessment, ServiceOutcome, ServicePhase, ServiceProgress,
        ServiceRequestState, ServiceSupervisor, SupervisorState,
    },
    native_deployment::contracts::NativeRequestId,
    startup::{AccountBuildKey, DesiredServerState},
};

/// Public registration observations. Missing is distinct from locked or unreadable storage.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub enum BootstrapAccountRegistration {
    /// No established registration exists for this actual account and compiled build.
    Missing,
    /// Deliberate Stop forbids automatic start and owner enrollment during Bootstrap.
    Stopped {
        /// Original retained public server identity.
        #[serde(with = "pin_wire")]
        pin: ServerPin,
        /// Exact registration comparison generation.
        generation: u64,
    },
    /// Exact approved signed installation and independently observed running service.
    Running {
        /// Original retained public server identity.
        #[serde(with = "pin_wire")]
        pin: ServerPin,
        /// Exact registration comparison generation.
        generation: u64,
        /// Signed approved archive digest, not the latest staged archive.
        artifact_identity: [u8; 32],
        /// Signed approved build version.
        version: BuildVersion,
        /// Protocol majors authenticated by the approved artifact manifest.
        protocol_majors: Vec<u16>,
    },
}

/// Bounded native observations. There are no executable paths or service labels in this DTO.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapAccountFacts {
    /// Actual native account and compiled build identity.
    pub key: AccountBuildKey,
    /// Exact registration observation, with unavailable storage returned as an error.
    pub registration: BootstrapAccountRegistration,
    /// Independently observed native manager state.
    pub supervisor: SupervisorState,
    /// Independently observed Linux account linger; absent on macOS.
    pub linger: Option<LingerState>,
}

/// One actual-account service provider, shared by setup and original-request lookup.
#[derive(Clone)]
pub struct AccountBootstrapSetup {
    supervisor: ServiceSupervisor,
}
impl AccountBootstrapSetup {
    /// Reuses the existing lifecycle owner and its selected native authorization adapter.
    pub fn new(supervisor: ServiceSupervisor) -> Self {
        Self { supervisor }
    }
    /// Returns the actual native account and compiled build selected by the lifecycle owner.
    pub fn key(&self) -> AccountBuildKey {
        self.supervisor.key()
    }
    /// Read-only discovery. This method never initializes native storage or prompts.
    pub fn inspect(&self) -> ServiceFuture<'_, BootstrapAccountFacts> {
        Box::pin(async move {
            let before = self.supervisor.discover()?;
            let status = self.supervisor.status().await?;
            let registration = match before.as_ref() {
                None => BootstrapAccountRegistration::Missing,
                Some(registration)
                    if registration.desired_state() == DesiredServerState::Stopped =>
                {
                    BootstrapAccountRegistration::Stopped {
                        pin: registration.server_pin(),
                        generation: registration.generation(),
                    }
                }
                Some(registration) => {
                    if status.supervisor == SupervisorState::Suppressed {
                        return Err(ServiceFailure::Suppressed);
                    }
                    if status.supervisor != SupervisorState::Running {
                        return Err(ServiceFailure::Pending);
                    }
                    let approved = registration
                        .approved_installation()
                        .ok_or(ServiceFailure::Missing)?;
                    let protocol_majors = self.supervisor.approved_protocol_majors()?;
                    BootstrapAccountRegistration::Running {
                        pin: registration.server_pin(),
                        generation: registration.generation(),
                        artifact_identity: approved.artifact_sha256,
                        version: approved.version.clone(),
                        protocol_majors,
                    }
                }
            };
            let after = self.supervisor.discover()?;
            let snapshot = |registration: &crate::account_service::RegisteredServer| {
                (
                    registration.server_pin(),
                    registration.generation(),
                    registration.desired_state(),
                    registration.approved_installation().cloned(),
                )
            };
            if before.as_ref().map(snapshot) != after.as_ref().map(snapshot)
                || status.key != self.key()
                || status.server.map(|id| id.bytes())
                    != after.as_ref().map(|r| r.server_pin().identity)
                || status.desired != after.as_ref().map(|r| r.desired_state())
                || status.approved
                    != after
                        .as_ref()
                        .and_then(|r| r.approved_installation().cloned())
            {
                return Err(ServiceFailure::StaleAssessment);
            }
            Ok(BootstrapAccountFacts {
                key: status.key,
                registration,
                supervisor: status.supervisor,
                linger: status.linger,
            })
        })
    }
    /// Reserves one durable native request before remote publication. It starts no code.
    pub fn reserve_request(&self) -> Result<NativeRequestId, ServiceFailure> {
        self.supervisor.allocate_request()
    }
    /// Authenticates the transferred artifact with the production trust root and native roots.
    pub fn assess_install(
        &self,
        artifact: VerifiedAccountServerArtifact,
    ) -> Result<ServiceInstallAssessment, ServiceFailure> {
        self.supervisor.assess_install(artifact)
    }
    /// Fixed explicit initial-install entry. No caller executable, shell command or service action is accepted.
    pub fn install(
        &self,
        request: NativeRequestId,
        confirmation: ConfirmedServiceInstall,
    ) -> ServiceFuture<'_, ServiceProgress> {
        self.supervisor.install(request, confirmation)
    }
    /// Installs once, then observes the same original within one overall startup deadline.
    /// The fifteen-second deadline starts before the native install wait. Timeout drops
    /// only this observer; the admitted native worker and retained request remain owned.
    pub fn install_and_observe(
        &self,
        request: NativeRequestId,
        confirmation: ConfirmedServiceInstall,
        artifact_identity: [u8; 32],
    ) -> ServiceFuture<'_, BootstrapAccountFacts> {
        Box::pin(async move {
            if artifact_identity == [0; 32] {
                return Err(ServiceFailure::InvalidRecord);
            }
            let until = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
            tokio::time::timeout_at(until, async {
                let progress = self.install(request, confirmation).await?;
                match progress.state {
                    ServiceRequestState::NotApplied(failure) => return Err(failure),
                    ServiceRequestState::Indeterminate => {
                        return Err(ServiceFailure::Indeterminate);
                    }
                    _ => {}
                }
                loop {
                    match self.resolve(request, artifact_identity).await {
                        Err(ServiceFailure::Pending) => {
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                        result => return result,
                    }
                }
            })
            .await
            .unwrap_or(Err(ServiceFailure::Pending))
        })
    }
    /// Records only this exact owner claim on the previously observed running registration.
    /// The server's pairing owner remains responsible for issuing the single-use reservation.
    pub fn authorize_owner<'a>(
        &'a self,
        request: NativeRequestId,
        expected: &'a BootstrapAccountRegistration,
        proposal: &'a EnrollmentProposal,
    ) -> ServiceFuture<'a, RequestDigest> {
        Box::pin(async move {
            let observed = self.inspect().await?;
            if &observed.registration != expected {
                return Err(ServiceFailure::StaleAssessment);
            }
            let BootstrapAccountRegistration::Running { pin, .. } = expected else {
                return Err(ServiceFailure::ConfirmationRequired);
            };
            if proposal.parts().server.bytes() != pin.identity {
                return Err(ServiceFailure::IdentityMismatch);
            }
            let claim =
                enrollment_proposal_digest(proposal).map_err(|_| ServiceFailure::InvalidRecord)?;
            let server = ServerId::new(pin.identity);
            let digest = self
                .supervisor
                .owner_enrollment_digest(request, server, claim)?;
            self.supervisor
                .record_owner_proposal(request, proposal.clone())?;
            Ok(digest)
        })
    }
    /// Resolves an original install or owner request after a lost SSH reply or provider restart.
    /// Only an admitted Starting original may record its observed completion. Reserved
    /// and earlier phases never resume effects. No lookup allocates, reinstalls or Starts.
    pub fn resolve(
        &self,
        request: NativeRequestId,
        artifact_identity: [u8; 32],
    ) -> ServiceFuture<'_, BootstrapAccountFacts> {
        Box::pin(async move {
            if artifact_identity == [0; 32] {
                return Err(ServiceFailure::InvalidRecord);
            }
            let (expected_server, expected_pin) =
                match self.supervisor.inspect_install_request(request) {
                    Ok((mut progress, original_artifact)) => {
                        if original_artifact != artifact_identity {
                            return Err(ServiceFailure::RequestConflict);
                        }
                        if progress.phase == ServicePhase::Starting
                            && progress.state == ServiceRequestState::Pending
                        {
                            progress = self
                                .supervisor
                                .observe_install_start(request, artifact_identity)
                                .await?;
                        }
                        match progress.state {
                            ServiceRequestState::Completed(ServiceOutcome::Installed {
                                server,
                                ..
                            }) => (server.identity, Some(server)),
                            ServiceRequestState::NotApplied(failure) => return Err(failure),
                            ServiceRequestState::Pending => return Err(ServiceFailure::Pending),
                            ServiceRequestState::Indeterminate => {
                                return Err(ServiceFailure::Indeterminate);
                            }
                            _ => return Err(ServiceFailure::RequestConflict),
                        }
                    }
                    Err(ServiceFailure::RequestConflict | ServiceFailure::RequestRetired) => (
                        self.supervisor
                            .inspect_owner_request(request)?
                            .ok_or(ServiceFailure::UnknownRequest)?
                            .0
                            .bytes(),
                        None,
                    ),
                    Err(failure) => return Err(failure),
                };
            let observed = self.inspect().await?;
            match &observed.registration {
                BootstrapAccountRegistration::Running {
                    pin,
                    artifact_identity: actual,
                    ..
                } if pin.identity == expected_server
                    && expected_pin.is_none_or(|original| original == *pin)
                    && actual == &artifact_identity => {}
                BootstrapAccountRegistration::Stopped { pin, .. }
                    if pin.identity == expected_server
                        && expected_pin.is_none_or(|original| original == *pin) => {}
                _ => return Err(ServiceFailure::IdentityMismatch),
            }
            Ok(observed)
        })
    }
}

mod pin_wire {
    use serde::{Deserialize, Serialize};
    use spaceterm_protocol::authentication::ServerPin;
    pub fn serialize<S: serde::Serializer>(
        pin: &ServerPin,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        (pin.identity, pin.public_key).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ServerPin, D::Error> {
        let (identity, public_key) = <([u8; 16], [u8; 32])>::deserialize(deserializer)?;
        if identity == [0; 16] || public_key == [0; 32] {
            return Err(serde::de::Error::custom("invalid public server pin"));
        }
        Ok(ServerPin {
            identity,
            public_key,
        })
    }
}
