//! Selected-server native management. Registration and confirmation authority stay
//! with the owning host; Clients retain only exact public comparisons.

use std::{num::NonZeroU64, sync::Arc};

use spaceterm_client::{
    api::PairedServerBinding,
    management_cli::{AssessmentToken, ManagementFailure, ManagementFuture},
};
use spaceterm_core::authority::ServerEpoch;
use spaceterm_distribution::VerifiedAccountServerArtifact;
use spaceterm_host::{
    account_service::{ServiceAction, ServiceProgress, ServiceStatus},
    native_deployment::contracts::NativeRequestId,
};

use super::{NativeActionPrompt, NativeServiceManagement, classify_service};

/// Consistent native facts about this provider's exact selected Server.
pub struct HostServiceObservation {
    pub status: ServiceStatus,
    pub generation: NonZeroU64,
}

/// A comparison to one retained native assessment, not decoded confirmation authority.
#[derive(Clone, Debug)]
pub struct HostServiceAssessment {
    pub prompt: NativeActionPrompt,
    pub runtime_epoch: Option<ServerEpoch>,
}

/// One exact selected-server native capability. An SSH route or IPC pathname alone
/// cannot construct this capability. Implementations own registration verification,
/// original request retention, and host-native lifecycle completion.
pub trait HostServiceManagement: Send + Sync {
    fn binding(&self) -> &PairedServerBinding;
    fn inspect(&self) -> ManagementFuture<'_, HostServiceObservation>;
    fn reserve_request(&self) -> ManagementFuture<'_, NativeRequestId>;
    fn assess(
        &self,
        request: NativeRequestId,
        action: ServiceAction,
    ) -> ManagementFuture<'_, HostServiceAssessment>;
    fn restore_assessment(
        &self,
        action: ServiceAction,
        reference: AssessmentToken,
    ) -> ManagementFuture<'_, HostServiceAssessment>;
    fn submit(&self, original: HostServiceAssessment) -> ManagementFuture<'_, ServiceProgress>;
    fn start(&self, request: NativeRequestId) -> ManagementFuture<'_, ServiceProgress>;
    fn resolve(&self, request: NativeRequestId) -> ManagementFuture<'_, ServiceProgress>;
    fn retained_requests(&self) -> ManagementFuture<'_, Vec<ServiceProgress>>;
    /// Staging selects signed code only. It cannot activate it, enable listening,
    /// or change deliberately Stopped intent.
    fn stage(
        &self,
        request: NativeRequestId,
        artifact: VerifiedAccountServerArtifact,
    ) -> ManagementFuture<'_, ServiceProgress>;
}

/// The existing account-local provider bound to one selected Client identity.
/// Remote providers implement the same surface without exposing guest paths or a
/// guest ServiceSupervisor to desktop callers.
pub struct LocalHostServices {
    management: Arc<NativeServiceManagement>,
    binding: PairedServerBinding,
}
impl LocalHostServices {
    pub fn new(
        management: Arc<NativeServiceManagement>,
        binding: PairedServerBinding,
    ) -> Result<Self, ManagementFailure> {
        let provider = Self {
            management,
            binding,
        };
        provider.registration()?;
        Ok(provider)
    }

    fn registration(
        &self,
    ) -> Result<spaceterm_host::account_service::RegisteredServer, ManagementFailure> {
        let registration = self
            .management
            .supervisor()
            .discover()
            .map_err(classify_service)?
            .ok_or(ManagementFailure::AuthorizationDenied)?;
        if registration.key() != self.management.key()
            || registration.server_pin() != self.binding.target().pin()
        {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        let local = crate::route::LocalRouteAuthority::new(
            registration.endpoint().map_err(classify_service)?,
        )
        .map_err(|_| ManagementFailure::UnsafeNativeState)?;
        if local.description() != self.binding.target().route() {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        Ok(registration)
    }
}
impl HostServiceManagement for LocalHostServices {
    fn binding(&self) -> &PairedServerBinding {
        &self.binding
    }

    fn inspect(&self) -> ManagementFuture<'_, HostServiceObservation> {
        Box::pin(async move {
            let status = self.management.status().await.map_err(classify_service)?;
            let registration = self.registration()?;
            if status.key != self.management.key()
                || status.server != Some(self.binding.target().server())
                || status.desired != Some(registration.desired_state())
                || status.approved.as_ref() != registration.approved_installation()
                || status.staged.as_ref() != registration.staged_installation()
            {
                return Err(ManagementFailure::StaleAssessment);
            }
            Ok(HostServiceObservation {
                status,
                generation: NonZeroU64::new(registration.generation())
                    .ok_or(ManagementFailure::UnsafeNativeState)?,
            })
        })
    }

    fn reserve_request(&self) -> ManagementFuture<'_, NativeRequestId> {
        Box::pin(async move {
            self.registration()?;
            self.management
                .supervisor()
                .allocate_request()
                .map_err(classify_service)
        })
    }

    fn assess(
        &self,
        request: NativeRequestId,
        action: ServiceAction,
    ) -> ManagementFuture<'_, HostServiceAssessment> {
        Box::pin(async move {
            self.registration()?;
            let original = self
                .management
                .supervisor()
                .assess(request, action)
                .await
                .map_err(classify_service)?;
            Ok(HostServiceAssessment {
                prompt: NativeActionPrompt::from_assessment(&original)?,
                runtime_epoch: original.runtime_epoch(),
            })
        })
    }

    fn restore_assessment(
        &self,
        action: ServiceAction,
        reference: AssessmentToken,
    ) -> ManagementFuture<'_, HostServiceAssessment> {
        Box::pin(async move {
            self.registration()?;
            let original = self
                .management
                .restore_assessment(action, reference)
                .await?;
            Ok(HostServiceAssessment {
                prompt: NativeActionPrompt::from_assessment(&original)?,
                runtime_epoch: original.runtime_epoch(),
            })
        })
    }

    fn submit(&self, original: HostServiceAssessment) -> ManagementFuture<'_, ServiceProgress> {
        Box::pin(async move {
            self.registration()?;
            let retained = self
                .management
                .restore_assessment(original.prompt.action, original.prompt.token.clone())
                .await?;
            if retained.request() != original.prompt.request
                || retained.runtime_epoch() != original.runtime_epoch
                || retained.affected_sessions() != original.prompt.affected_sessions
            {
                return Err(ManagementFailure::StaleAssessment);
            }
            let confirmed = retained.confirm().map_err(classify_service)?;
            let progress = match original.prompt.action {
                ServiceAction::Stop => self.management.stop(confirmed).await,
                ServiceAction::Restart => self.management.restart(confirmed).await,
                ServiceAction::Uninstall => self.management.uninstall(confirmed).await,
                ServiceAction::Activate(_) => {
                    self.management.supervisor().activate(confirmed).await
                }
            };
            progress.map_err(classify_service)
        })
    }

    fn start(&self, request: NativeRequestId) -> ManagementFuture<'_, ServiceProgress> {
        Box::pin(async move {
            self.registration()?;
            self.management
                .start(request)
                .await
                .map_err(classify_service)
        })
    }

    fn resolve(&self, request: NativeRequestId) -> ManagementFuture<'_, ServiceProgress> {
        Box::pin(async move {
            self.registration()?;
            self.management
                .resolve(request)
                .await
                .map_err(classify_service)
        })
    }

    fn retained_requests(&self) -> ManagementFuture<'_, Vec<ServiceProgress>> {
        Box::pin(async move {
            self.registration()?;
            self.management
                .supervisor()
                .retained_service_requests()
                .map_err(classify_service)
        })
    }

    fn stage(
        &self,
        request: NativeRequestId,
        artifact: VerifiedAccountServerArtifact,
    ) -> ManagementFuture<'_, ServiceProgress> {
        Box::pin(async move {
            self.registration()?;
            let original = self
                .management
                .supervisor()
                .assess_install(artifact)
                .map_err(classify_service)?;
            self.management
                .supervisor()
                .stage_installation(request, original.confirm())
                .await
                .map_err(classify_service)
        })
    }
}
