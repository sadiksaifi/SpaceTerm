//! Native-discovered guest consumer for the closed stopped account grammar.

use std::sync::Arc;

use spaceterm_core::{identity::ServerId, organization::ServerDirectory};
use spaceterm_distribution::VerifiedAccountServerArtifact;
use spaceterm_host::account_service::{
    ServiceFailure, ServiceSupervisor, StoppedNativeAccountReply, StoppedNativeAccountRequest,
    StoppedNativeRequest,
};

use super::{AccountNativeMaintenanceExecutor, StoppedNativeTarget};

/// A Channel must transition its exact stream into the ordinary handshake.
/// A native reply cannot carry the executor or import its frozen admission.
pub enum StoppedNativeAccountOutcome {
    /// Bounded comparison or already-accepted original progress.
    Reply(StoppedNativeAccountReply),
    /// Original capture and real frozen paired authentication composition.
    Channel {
        /// The producer sends this exact original before the handshake.
        original: StoppedNativeRequest,
        /// Keeps stopped custody until ordinary authentication and final admission.
        executor: Box<AccountNativeMaintenanceExecutor>,
    },
}

/// Checks native identity and original scope before consuming declared package
/// bytes. The account envelope supplies no home, paths, keys, or commands.
/// A Resolve package is allowed only for a signed already-Accepted Stage original.
pub fn validate_artifact_input(
    supervisor: &ServiceSupervisor,
    request: &StoppedNativeAccountRequest,
) -> Result<(), ServiceFailure> {
    request.validate()?;
    let (key, pin) = request.expected_identity();
    supervisor.reconcile_stopped_native_storage(key, pin, request.request())?;
    let registration = supervisor.discover()?.ok_or(ServiceFailure::Missing)?;
    if registration.key() != key || registration.server_pin() != pin {
        return Err(ServiceFailure::IdentityMismatch);
    }
    match request {
        StoppedNativeAccountRequest::Channel { capture, artifact } => {
            let original = supervisor.capture_stopped_native_expected(
                capture.request,
                capture.action,
                key,
                pin,
                capture.registration_generation.get(),
            )?;
            if let Some(artifact) = artifact {
                supervisor.check_stopped_native_stage_transfer(
                    original,
                    artifact.artifact_sha256,
                    &artifact.version,
                    false,
                )?;
            }
        }
        StoppedNativeAccountRequest::Resolve {
            artifact: Some(artifact),
            request,
            ..
        } => {
            let original = supervisor.restore_stopped_native(*request)?;
            supervisor.check_stopped_native_stage_transfer(
                original,
                artifact.artifact_sha256,
                &artifact.version,
                true,
            )?;
        }
        _ => {}
    }
    Ok(())
}

/// Handles one closed account request using native-discovered home and actual
/// providers. Verified packages must come from the existing bounded receiver.
/// No decoded reference or saved-principal comparison can accept native effects.
pub async fn handle_account_request(
    supervisor: Arc<ServiceSupervisor>,
    home: ServerDirectory,
    request: &StoppedNativeAccountRequest,
    artifact: Option<VerifiedAccountServerArtifact>,
) -> StoppedNativeAccountOutcome {
    match perform(supervisor, home, request, artifact).await {
        Ok(StoppedNativeAccountOutcome::Reply(reply)) => match reply.validate() {
            Ok(()) => StoppedNativeAccountOutcome::Reply(reply),
            Err(error) => {
                StoppedNativeAccountOutcome::Reply(StoppedNativeAccountReply::Refused(error))
            }
        },
        Ok(channel) => channel,
        Err(error) => StoppedNativeAccountOutcome::Reply(StoppedNativeAccountReply::Refused(error)),
    }
}

async fn perform(
    supervisor: Arc<ServiceSupervisor>,
    home: ServerDirectory,
    request: &StoppedNativeAccountRequest,
    artifact: Option<VerifiedAccountServerArtifact>,
) -> Result<StoppedNativeAccountOutcome, ServiceFailure> {
    validate_artifact_input(&supervisor, request)?;
    let (key, pin) = request.expected_identity();
    if home.server() != ServerId::new(pin.identity) {
        return Err(ServiceFailure::IdentityMismatch);
    }
    match (request.artifact(), artifact.as_ref()) {
        (Some(expected), Some(artifact)) => expected.matches(artifact)?,
        (None, None) => {}
        _ => return Err(ServiceFailure::ArtifactRejected),
    }
    let reply = match request {
        StoppedNativeAccountRequest::Channel { capture, .. } => {
            let original = supervisor.capture_stopped_native_expected(
                capture.request,
                capture.action,
                key,
                pin,
                capture.registration_generation.get(),
            )?;
            let executor = AccountNativeMaintenanceExecutor::open(
                supervisor,
                home,
                StoppedNativeTarget {
                    key,
                    pin,
                    principal: capture.principal,
                },
                original,
                artifact,
            )
            .await?;
            return Ok(StoppedNativeAccountOutcome::Channel {
                original,
                executor: Box::new(executor),
            });
        }
        StoppedNativeAccountRequest::Restore {
            request, action, ..
        } => {
            let original = supervisor.restore_stopped_native(*request)?;
            if original.action != *action {
                return Err(ServiceFailure::RequestConflict);
            }
            let assessment = match supervisor.restore_stopped_native_reference(original) {
                Ok(reference) => Some(reference),
                Err(ServiceFailure::Pending) => None,
                Err(error) => return Err(error),
            };
            StoppedNativeAccountReply::Restored {
                original,
                assessment,
            }
        }
        StoppedNativeAccountRequest::Resolve { request, .. } => {
            let original = supervisor.restore_stopped_native(*request)?;
            // Require signed origin even for Reserved progress. Public captures
            // and reachable native accounts cannot promote an unassessed request.
            supervisor.restore_stopped_native_reference(original)?;
            let progress = supervisor
                .resolve_stopped_native_with_artifact_exact(original, artifact.as_ref())
                .await?;
            StoppedNativeAccountReply::Progress(progress)
        }
        StoppedNativeAccountRequest::Acknowledge { progress, .. } => {
            supervisor.acknowledge_stopped_native_exact(progress)?;
            StoppedNativeAccountReply::Acknowledged {
                progress: progress.clone(),
            }
        }
    };
    Ok(StoppedNativeAccountOutcome::Reply(reply))
}
