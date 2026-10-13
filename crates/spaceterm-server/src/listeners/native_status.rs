//! Native facts are observations, never lifecycle or artifact mutation authority.

use std::sync::Arc;

use spaceterm_core::{identity::ServerId, management::*};
use spaceterm_host::account_service::{
    LingerState, ServiceFailure, ServicePhase, ServiceRequestState, ServiceSupervisor,
    SupervisorState,
};

use crate::{contracts::*, failure::failure};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeStatusObservation {
    pub approved: Option<ServerVersion>,
    pub staged: Option<ServerVersion>,
    pub verification: NativeVerification,
    pub capabilities: Vec<CapabilityStatus>,
    pub progress: Vec<ManagementProgress>,
}
impl Default for NativeStatusObservation {
    fn default() -> Self {
        Self {
            approved: None,
            staged: None,
            verification: NativeVerification::Unverified,
            capabilities: Vec::new(),
            progress: Vec::new(),
        }
    }
}

impl NativeStatusObservation {
    pub(crate) fn unavailable() -> Self {
        Self {
            capabilities: [
                CAPABILITY_NATIVE_LIFECYCLE,
                CAPABILITY_SIGNED_SERVER_INSTALLATION,
            ]
            .into_iter()
            .map(|capability| CapabilityStatus {
                capability,
                availability: CapabilityAvailability::Unavailable,
            })
            .collect(),
            ..Self::default()
        }
    }
}

/// Constructor-injected native supervision and signed installation observations.
pub trait NativeStatusObserver: Send + Sync {
    fn observe(
        &self,
        server: ServerId,
    ) -> ServerFuture<'_, Result<NativeStatusObservation, ServerFailure>>;
}

pub struct AccountNativeStatusObserver(Arc<ServiceSupervisor>);
impl AccountNativeStatusObserver {
    pub fn new(supervisor: Arc<ServiceSupervisor>) -> Arc<Self> {
        Arc::new(Self(supervisor))
    }
}
impl NativeStatusObserver for AccountNativeStatusObserver {
    fn observe(
        &self,
        server: ServerId,
    ) -> ServerFuture<'_, Result<NativeStatusObservation, ServerFailure>> {
        Box::pin(async move {
            let facts = self.0.status().await.map_err(native_failure)?;
            if facts.server != Some(server) {
                return Err(failure(ServerFailureClass::ScopeMismatch));
            }
            let lifecycle = match (facts.supervisor, facts.linger) {
                (SupervisorState::Suppressed, _) | (_, Some(LingerState::Unavailable)) => {
                    CapabilityAvailability::Unavailable
                }
                (_, Some(LingerState::Disabled)) => CapabilityAvailability::AuthorizationRequired,
                _ => CapabilityAvailability::Available,
            };
            let supervisor = self.0.clone();
            let selected =
                tokio::task::spawn_blocking(move || supervisor.verified_installation_selection())
                    .await
                    .map_err(|_| failure(ServerFailureClass::NativeUnavailable))?;
            let mut observation = NativeStatusObservation {
                capabilities: vec![CapabilityStatus {
                    capability: CAPABILITY_NATIVE_LIFECYCLE,
                    availability: lifecycle,
                }],
                ..NativeStatusObservation::default()
            };
            let availability = match selected {
                Ok(selected) => {
                    // Compare the separate native supervision snapshot before publishing.
                    if facts.approved.as_ref() != Some(&selected.approved)
                        || facts.staged != selected.staged
                    {
                        return Err(failure(ServerFailureClass::StaleAuthority));
                    }
                    observation.approved = Some(version(selected.approved.version)?);
                    observation.staged = selected
                        .staged
                        .map(|value| version(value.version))
                        .transpose()?;
                    observation.verification = NativeVerification::Verified;
                    CapabilityAvailability::Available
                }
                Err(ServiceFailure::UnsupportedHost) => CapabilityAvailability::Unsupported,
                Err(_) => CapabilityAvailability::Unavailable,
            };
            observation.capabilities.push(CapabilityStatus {
                capability: CAPABILITY_SIGNED_SERVER_INSTALLATION,
                availability,
            });
            if let Some(request) = facts.pending {
                let progress = self.0.request_progress(request).map_err(native_failure)?;
                let state = match progress.state {
                    ServiceRequestState::Completed(_) => ManagementProgressState::Completed,
                    ServiceRequestState::Indeterminate => ManagementProgressState::RecoveryRequired,
                    ServiceRequestState::NotApplied(_) => ManagementProgressState::Unavailable,
                    ServiceRequestState::Pending => match progress.phase {
                        ServicePhase::Quiescing => ManagementProgressState::Draining,
                        _ => ManagementProgressState::Pending,
                    },
                };
                observation.progress.push(ManagementProgress {
                    request: ManagementRequestReference::Native(*request.as_bytes()),
                    state,
                    completed_units: u32::from(state == ManagementProgressState::Completed),
                    total_units: 1,
                });
            }
            Ok(observation)
        })
    }
}
fn version(value: spaceterm_distribution::BuildVersion) -> Result<ServerVersion, ServerFailure> {
    ServerVersion::new(value.as_str().to_owned())
        .map_err(|_| failure(ServerFailureClass::ScopeMismatch))
}
fn native_failure(value: ServiceFailure) -> ServerFailure {
    failure(match value {
        ServiceFailure::IdentityMismatch => ServerFailureClass::ScopeMismatch,
        ServiceFailure::StorageLocked | ServiceFailure::StorageUnavailable => {
            ServerFailureClass::StorageUnavailable
        }
        _ => ServerFailureClass::NativeUnavailable,
    })
}
