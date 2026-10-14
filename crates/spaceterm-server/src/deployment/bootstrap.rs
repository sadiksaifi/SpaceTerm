//! Deliberate, framed SSH account endpoint. Native account discovery selects every root.

use super::owner::OwnerEndpointFailure;
use spaceterm_core::identity::ServerId;
use spaceterm_distribution::{BuildChannel, ServerPlatform, server_feed::MAX_ARCHIVE_BYTES};
use spaceterm_host::{
    account_service::{ServiceFailure, ServiceSupervisor, StoppedServerExpectation},
    bootstrap::{
        artifact::receive_artifact,
        setup::{AccountBootstrapSetup, BootstrapAccountFacts, BootstrapAccountRegistration},
        wire::*,
    },
    startup::{BuildIdentity, DesiredServerState},
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
    let result = execute_foreground(&supervisor, channel).await;
    // EOF, malformed input and broken borrowed output all retire admission. Keep
    // this helper's runtime alive until its already spawned native workers finish.
    // This is outside the reply deadline and never resumes a retained request.
    supervisor.finish_install_execution().await;
    result
}

async fn execute_foreground(
    supervisor: &Arc<ServiceSupervisor>,
    channel: &dyn MessageTransport,
) -> Result<(), OwnerEndpointFailure> {
    let setup = AccountBootstrapSetup::new((**supervisor).clone());
    // The SSH owner closes stdin to retire this endpoint. No ordinary server starts here.
    for _ in 0..4096 {
        let Some(bytes) = channel.recv().await? else {
            return Ok(());
        };
        let request: AccountRequest =
            serde_json::from_slice(&bytes).map_err(|_| OwnerEndpointFailure::InvalidRequest)?;
        if matches!(request, AccountRequest::OwnerChannel) {
            // Keep owner::execute awaited so its Running IPC close and Stopped
            // executor cleanup finish even when the native channel retires.
            return super::owner::execute(Arc::clone(supervisor), channel).await;
        }
        let reply = match handle(&setup, supervisor, channel, request).await {
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
    observation_from_facts(supervisor, setup.inspect().await?)
}

fn observation_from_facts(
    supervisor: &ServiceSupervisor,
    facts: BootstrapAccountFacts,
) -> Result<AccountObservation, ServiceFailure> {
    let registration = supervisor.discover()?;
    let matches = match (&facts.registration, registration.as_ref()) {
        (BootstrapAccountRegistration::Missing, None) => true,
        (BootstrapAccountRegistration::Stopped { pin, generation }, Some(registered)) => {
            registered.server_pin() == *pin
                && registered.generation() == *generation
                && registered.desired_state() == DesiredServerState::Stopped
        }
        (
            BootstrapAccountRegistration::Running {
                pin,
                generation,
                artifact_identity,
                version,
                ..
            },
            Some(registered),
        ) => {
            registered.server_pin() == *pin
                && registered.generation() == *generation
                && registered.desired_state() == DesiredServerState::Running
                && registered.approved_installation().is_some_and(|approved| {
                    approved.artifact_sha256 == *artifact_identity && approved.version == *version
                })
        }
        _ => false,
    };
    if facts.key != supervisor.key() || !matches {
        return Err(ServiceFailure::StaleAssessment);
    }
    let remote_socket = registration
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
            let artifact_identity = artifact.artifact_sha256();
            let confirmation = setup.assess_install(artifact)?.confirm();
            let facts = setup
                .install_and_observe(request, confirmation, artifact_identity)
                .await?;
            observation_from_facts(supervisor, facts).map(AccountReply::Observation)
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
            let facts = setup.resolve(request, artifact).await?;
            observation_from_facts(supervisor, facts).map(AccountReply::Observation)
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
            if facts.registration != expected {
                return Err(ServiceFailure::StaleAssessment);
            }
            let BootstrapAccountRegistration::Stopped { pin, generation } = expected else {
                return Err(ServiceFailure::StaleAssessment);
            };
            let registration = supervisor
                .discover()?
                .ok_or(ServiceFailure::StaleAssessment)?;
            if registration.server_pin() != pin
                || registration.generation() != generation
                || registration.desired_state() != DesiredServerState::Stopped
            {
                return Err(ServiceFailure::StaleAssessment);
            }
            let expected = StoppedServerExpectation {
                pin,
                generation,
                approved: registration
                    .approved_installation()
                    .cloned()
                    .ok_or(ServiceFailure::Missing)?,
            };
            // Start is a separate deliberate entry. Install, resolve and owner never call it.
            let request = supervisor.allocate_request()?;
            supervisor.start_stopped(request, expected).await?;
            Ok(AccountReply::Observation(
                observation(setup, supervisor).await?,
            ))
        }
        AccountRequest::OwnerChannel => Err(ServiceFailure::InvalidRecord),
    }
}
