//! Deliberate, framed SSH account endpoint. Native account discovery selects every root.

use super::owner::OwnerEndpointFailure;
use spaceterm_core::identity::ServerId;
use spaceterm_distribution::{BuildChannel, ServerPlatform, server_feed::MAX_ARCHIVE_BYTES};
use spaceterm_host::{
    account_service::{ServiceFailure, ServiceRequestState, ServiceSupervisor},
    bootstrap::{
        artifact::receive_artifact,
        setup::{AccountBootstrapSetup, BootstrapAccountRegistration},
        wire::*,
    },
    startup::BuildIdentity,
    transport::MessageTransport,
};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

pub async fn execute(
    supervisor: Arc<ServiceSupervisor>,
    channel: &dyn MessageTransport,
) -> Result<(), OwnerEndpointFailure> {
    let setup = AccountBootstrapSetup::new((*supervisor).clone());
    // The SSH owner closes stdin to retire this endpoint. No ordinary server starts here.
    for _ in 0..4096 {
        let Some(bytes) = channel.recv().await? else {
            return Ok(());
        };
        let request: AccountRequest =
            serde_json::from_slice(&bytes).map_err(|_| OwnerEndpointFailure::InvalidRequest)?;
        if matches!(request, AccountRequest::OwnerChannel) {
            return super::owner::execute(supervisor, channel).await;
        }
        let reply = match handle(&setup, &supervisor, channel, request).await {
            Ok(reply) => reply,
            Err(failure) => AccountReply::Failure(failure),
        };
        let bytes = serde_json::to_vec(&reply).map_err(|_| OwnerEndpointFailure::InvalidRequest)?;
        channel.send(&bytes).await?;
    }
    Err(OwnerEndpointFailure::InvalidRequest)
}

async fn observation(
    setup: &AccountBootstrapSetup,
    supervisor: &ServiceSupervisor,
) -> Result<AccountObservation, ServiceFailure> {
    let facts = setup.inspect().await?;
    let remote_socket = supervisor
        .discover()?
        .map(|registration| {
            registration
                .endpoint()?
                .path()
                .to_str()
                .map(str::to_owned)
                .ok_or(ServiceFailure::InvalidRecord)
        })
        .transpose()?;
    Ok(AccountObservation {
        facts,
        remote_socket,
    })
}

async fn handle(
    setup: &AccountBootstrapSetup,
    supervisor: &ServiceSupervisor,
    channel: &dyn MessageTransport,
    request: AccountRequest,
) -> Result<AccountReply, ServiceFailure> {
    match request {
        AccountRequest::Inspect => Ok(AccountReply::Observation(
            observation(setup, supervisor).await?,
        )),
        AccountRequest::Reserve => Ok(AccountReply::Reserved(setup.reserve_request()?)),
        AccountRequest::ReserveOwner { proposal } => {
            let facts = setup.inspect().await?;
            let BootstrapAccountRegistration::Running { pin, .. } = facts.registration else {
                return Err(ServiceFailure::ConfirmationRequired);
            };
            if pin.identity != proposal.parts().server.bytes() {
                return Err(ServiceFailure::IdentityMismatch);
            }
            let request = match supervisor
                .retained_owner_proposal(proposal.parts().server, &proposal.parts().keys)?
            {
                Some((request, original)) if original == proposal => request,
                Some(_) => return Err(ServiceFailure::RequestConflict),
                None => {
                    let request = setup.reserve_request()?;
                    supervisor.record_owner_proposal(request, proposal)?;
                    request
                }
            };
            Ok(AccountReply::Reserved(request))
        }
        AccountRequest::Install {
            request,
            artifact,
            transfer_bytes,
        } => {
            // Revalidate Missing before receiving or interpreting any signed payload.
            if setup.inspect().await?.registration != BootstrapAccountRegistration::Missing {
                return Err(ServiceFailure::StaleAssessment);
            }
            if transfer_bytes == 0 || transfer_bytes > MAX_ARCHIVE_BYTES + 128 * 1024 {
                return Err(ServiceFailure::LimitExceeded);
            }
            let mut bytes = Vec::new();
            while (bytes.len() as u64) < transfer_bytes {
                let chunk = channel
                    .recv()
                    .await
                    .map_err(|_| ServiceFailure::Pending)?
                    .ok_or(ServiceFailure::Pending)?;
                if chunk.is_empty()
                    || chunk.len() > TRANSFER_CHUNK
                    || chunk.len() as u64 > transfer_bytes - bytes.len() as u64
                {
                    return Err(ServiceFailure::InvalidRecord);
                }
                bytes.extend_from_slice(&chunk);
            }
            let build = match setup.key().build_identity {
                BuildIdentity::Release => BuildChannel::Release,
                BuildIdentity::Preflight => BuildChannel::Preflight,
                BuildIdentity::Development => BuildChannel::Development,
            };
            let platform = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                ServerPlatform::DarwinArm64
            } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
                ServerPlatform::LinuxX86_64
            } else {
                return Err(ServiceFailure::UnsupportedHost);
            };
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| ServiceFailure::SupervisorUnavailable)?
                .as_secs();
            let artifact = receive_artifact(&mut bytes.as_slice(), build, platform, artifact, now)
                .map_err(|_| ServiceFailure::ArtifactRejected)?;
            let confirmation = setup.assess_install(artifact)?.confirm();
            let progress = setup.install(request, confirmation).await?;
            if !matches!(progress.state, ServiceRequestState::Completed(_)) {
                return Err(ServiceFailure::Pending);
            }
            // A started service may need one read-only observation retry. Never Start again.
            let until = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
            loop {
                match observation(setup, supervisor).await {
                    Err(ServiceFailure::Pending) if tokio::time::Instant::now() < until => {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await
                    }
                    result => return result.map(AccountReply::Observation),
                }
            }
        }
        AccountRequest::AuthorizeOwner {
            request,
            expected,
            proposal,
        } => {
            let digest = setup.authorize_owner(request, &expected, &proposal).await?;
            Ok(AccountReply::Owner {
                observation: observation(setup, supervisor).await?,
                digest,
            })
        }
        AccountRequest::OwnerBinding { request, proposal } => {
            let claim = spaceterm_protocol::pairing_adapter::enrollment_proposal_digest(&proposal)
                .map_err(|_| ServiceFailure::InvalidRecord)?;
            let digest = supervisor
                .retained_owner_enrollment_digest(request, proposal.parts().server, claim)?
                .ok_or(ServiceFailure::UnknownRequest)?;
            Ok(AccountReply::Owner {
                observation: observation(setup, supervisor).await?,
                digest,
            })
        }
        AccountRequest::Resolve { request, artifact } => {
            setup.resolve(request, artifact).await?;
            Ok(AccountReply::Observation(
                observation(setup, supervisor).await?,
            ))
        }
        AccountRequest::RetainedOwner { keys } => {
            let facts = setup.inspect().await?;
            let BootstrapAccountRegistration::Running { pin, .. } = facts.registration else {
                return Err(ServiceFailure::ConfirmationRequired);
            };
            Ok(AccountReply::RetainedOwner(
                supervisor.retained_owner_proposal(ServerId::new(pin.identity), &keys)?,
            ))
        }
        AccountRequest::Start { expected } => {
            let facts = setup.inspect().await?;
            if facts.registration != expected
                || !matches!(expected, BootstrapAccountRegistration::Stopped { .. })
            {
                return Err(ServiceFailure::StaleAssessment);
            }
            // Start is a separate deliberate entry. Install, resolve and owner never call it.
            let request = supervisor.allocate_request()?;
            supervisor.start(request).await?;
            Ok(AccountReply::Observation(
                observation(setup, supervisor).await?,
            ))
        }
        AccountRequest::OwnerChannel => Err(ServiceFailure::InvalidRecord),
    }
}
