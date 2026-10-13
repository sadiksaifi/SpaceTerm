//! Fixed, deliberate account-owner channel. It never creates an ordinary principal.
//! The first frame binds a previously retained native request to verified candidate
//! keys. Remaining frames reuse the frozen enrollment handshake and both majors.

use std::{sync::Arc, time::Duration};

use prost::Message as _;
use serde::{Deserialize, Serialize};
use spaceterm_core::pairing::{
    EnrollmentProposal, EnrollmentReservationState, PairingFailure, PairingOutcome,
};
use spaceterm_host::{
    account_service::{ServiceFailure, ServiceSupervisor},
    ipc::{IpcEndpoint, IpcListener, IpcTransport, PrivateIpcDirectory},
    native_deployment::contracts::NativeRequestId,
    startup::DesiredServerState,
    transport::{AdmissionLimits, MessageLimit, MessageTransport, TransportError, TransportRoute},
};
use spaceterm_protocol::pairing_adapter::VerifiedDeviceClaim;
use tokio::{
    sync::watch,
    task::{JoinHandle, JoinSet},
};
use zeroize::Zeroizing;

use crate::{account_owner_enrollment::AccountOwnerRequestExecutor, api::ServerHandle};

pub const OWNER_FRAME_LIMIT: usize = 64 * 1024;
const OWNER_DEADLINE: Duration = Duration::from_secs(60);
const OWNER_FRAME_COUNT: usize = 16;

/// Public proof material only. A provider-issued ID is retained before submission.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerEnrollmentRequest {
    pub request: NativeRequestId,
    pub proposal: EnrollmentProposal,
    pub device_proof: Vec<u8>,
    pub authentication_proof: Vec<u8>,
}
impl OwnerEnrollmentRequest {
    pub fn verify(&self) -> Result<VerifiedDeviceClaim, OwnerEndpointFailure> {
        if self.device_proof.len() != 64 || self.authentication_proof.len() != 64 {
            return Err(OwnerEndpointFailure::InvalidRequest);
        }
        VerifiedDeviceClaim::verify(
            self.proposal.clone(),
            &self.device_proof,
            &self.authentication_proof,
        )
        .map_err(|_| OwnerEndpointFailure::InvalidRequest)
    }
}

/// Issuance replay cannot recover a bearer. Only completed enrollment has a
/// historical receipt available through the candidate's restricted handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerReplyDisposition {
    AuthorizationDelivered,
    AuthorizationReplyLost,
    AuthorizationInvalidated,
    ReceiptAvailable,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerEnrollmentReply {
    pub schema: u16,
    pub disposition: OwnerReplyDisposition,
    pub outcome: PairingOutcome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerEndpointFailure {
    InvalidRequest,
    Account(ServiceFailure),
    Pairing(PairingFailure),
    Transport,
    Cleanup,
}
impl OwnerEndpointFailure {
    pub const fn classification(self) -> &'static str {
        match self {
            Self::InvalidRequest => "owner request invalid",
            Self::Account(_) => "owner account unavailable",
            Self::Pairing(_) => "owner enrollment unavailable",
            Self::Transport => "owner channel unavailable",
            Self::Cleanup => "owner cleanup incomplete",
        }
    }
}
impl From<TransportError> for OwnerEndpointFailure {
    fn from(_: TransportError) -> Self {
        Self::Transport
    }
}
impl From<PairingFailure> for OwnerEndpointFailure {
    fn from(value: PairingFailure) -> Self {
        Self::Pairing(value)
    }
}

fn endpoint(supervisor: &ServiceSupervisor) -> Result<IpcEndpoint, OwnerEndpointFailure> {
    let registration = supervisor
        .discover()
        .map_err(OwnerEndpointFailure::Account)?
        .ok_or(OwnerEndpointFailure::Account(ServiceFailure::Missing))?;
    let ordinary = registration
        .endpoint()
        .map_err(OwnerEndpointFailure::Account)?;
    let directory = ordinary
        .path()
        .parent()
        .ok_or(OwnerEndpointFailure::InvalidRequest)?;
    PrivateIpcDirectory::open(directory)?
        .endpoint("owner")
        .map_err(Into::into)
}

/// The account-owner CLI selects the registered native route, never caller paths.
/// Running must use that process's actor. A failed connect never opens its database.
pub async fn execute(
    supervisor: Arc<ServiceSupervisor>,
    channel: &dyn MessageTransport,
) -> Result<(), OwnerEndpointFailure> {
    let peer = channel.peer_facts();
    if peer.route != TransportRoute::PrivateIpc
        || peer.local_uid != Some(supervisor.key().account_uid)
    {
        return Err(OwnerEndpointFailure::InvalidRequest);
    }
    let initial = receive(channel)
        .await?
        .ok_or(OwnerEndpointFailure::InvalidRequest)?;
    let request: OwnerEnrollmentRequest =
        serde_json::from_slice(&initial).map_err(|_| OwnerEndpointFailure::InvalidRequest)?;
    let claim = request.verify()?;
    let registration = supervisor
        .discover()
        .map_err(OwnerEndpointFailure::Account)?
        .ok_or(OwnerEndpointFailure::Account(ServiceFailure::Missing))?;
    if registration.desired_state() == DesiredServerState::Running {
        let transport = IpcTransport::connect(
            &endpoint(&supervisor)?,
            MessageLimit::new(OWNER_FRAME_LIMIT)?,
        )
        .await?;
        let result = forward(channel, &transport, &initial).await;
        let closed = transport.close().await;
        return result.and(closed.map_err(Into::into));
    }
    let home = spaceterm_host::account_service::TrustedAccountFacts::current()
        .map_err(OwnerEndpointFailure::Account)?
        .home()
        .to_owned();
    let executor =
        crate::account_startup::production_stopped_owner(supervisor, home, request.request, &claim)
            .await?;
    let result = serve_executor(&executor, channel, request.request, claim).await;
    let report = executor
        .close()
        .await
        .map_err(|_| OwnerEndpointFailure::Cleanup)?;
    if report.is_some_and(|report| {
        !report.cleanup_confirmed
            || !report.listeners_closed
            || !report.started_writes_finished
            || report.recovery_required
    }) {
        return Err(OwnerEndpointFailure::Cleanup);
    }
    result
}

async fn receive(
    channel: &dyn MessageTransport,
) -> Result<Option<Zeroizing<Vec<u8>>>, OwnerEndpointFailure> {
    let bytes = channel.recv().await?;
    if bytes
        .as_ref()
        .is_some_and(|bytes| bytes.is_empty() || bytes.len() > OWNER_FRAME_LIMIT)
    {
        return Err(OwnerEndpointFailure::InvalidRequest);
    }
    Ok(bytes.map(Zeroizing::new))
}

async fn forward(
    local: &dyn MessageTransport,
    remote: &dyn MessageTransport,
    initial: &[u8],
) -> Result<(), OwnerEndpointFailure> {
    remote.send(initial).await?;
    // One public result and one frozen delivery/public reservation precede the
    // alternating encrypted enrollment handshake and restricted frame exchange.
    for _ in 0..2 {
        let reply = receive(remote)
            .await?
            .ok_or(OwnerEndpointFailure::Transport)?;
        local.send(&reply).await?;
    }
    for _ in 0..(OWNER_FRAME_COUNT + 2) {
        let Some(frame) = receive(local).await? else {
            return Ok(());
        };
        remote.send(&frame).await?;
        let reply = receive(remote)
            .await?
            .ok_or(OwnerEndpointFailure::Transport)?;
        local.send(&reply).await?;
    }
    Err(OwnerEndpointFailure::InvalidRequest)
}

pub async fn serve_executor(
    executor: &AccountOwnerRequestExecutor,
    channel: &dyn MessageTransport,
    request: NativeRequestId,
    claim: VerifiedDeviceClaim,
) -> Result<(), OwnerEndpointFailure> {
    let execution = executor.issue(request, claim).await?;
    let PairingOutcome::AuthorizationReserved { ref ticket, .. } = execution.outcome else {
        return Err(OwnerEndpointFailure::InvalidRequest);
    };
    let reservation = executor.retained_reservation(ticket)?;
    let disposition = if execution.authorization.is_some() {
        OwnerReplyDisposition::AuthorizationDelivered
    } else {
        match reservation.parts().state {
            EnrollmentReservationState::Consumed { .. } => OwnerReplyDisposition::ReceiptAvailable,
            EnrollmentReservationState::Reserved => OwnerReplyDisposition::AuthorizationReplyLost,
            EnrollmentReservationState::Invalidated => {
                OwnerReplyDisposition::AuthorizationInvalidated
            }
        }
    };
    let public = executor
        .enrollment_reservation(ticket)
        .map_err(|_| OwnerEndpointFailure::InvalidRequest)?;
    let reply = serde_json::to_vec(&OwnerEnrollmentReply {
        schema: 1,
        disposition,
        outcome: execution.outcome,
    })
    .map_err(|_| OwnerEndpointFailure::InvalidRequest)?;
    channel.send(&reply).await?;
    if let Some(authorization) = execution.authorization {
        let delivery = authorization
            .delivery_message(&reservation)
            .map_err(|_| OwnerEndpointFailure::InvalidRequest)?;
        channel.send(&delivery).await?;
    } else {
        channel.send(&public.to_wire().encode_to_vec()).await?;
    }
    let Some(hello) = receive(channel).await? else {
        return Ok(());
    };
    let (handshake, reply) = executor
        .accept_enrollment(&hello)
        .map_err(|_| OwnerEndpointFailure::InvalidRequest)?;
    channel.send(&reply).await?;
    let proof = receive(channel)
        .await?
        .ok_or(OwnerEndpointFailure::InvalidRequest)?;
    let (mut connection, reply) = handshake
        .receive_proof(&proof)
        .map_err(|_| OwnerEndpointFailure::InvalidRequest)?;
    channel.send(&reply).await?;
    for _ in 0..OWNER_FRAME_COUNT {
        let Some(frame) = receive(channel).await? else {
            return Ok(());
        };
        let reply = Zeroizing::new(
            executor
                .enrollment_frame(&mut connection, &frame)
                .await
                .map_err(|_| OwnerEndpointFailure::InvalidRequest)?,
        );
        channel.send(&reply).await?;
    }
    Err(OwnerEndpointFailure::InvalidRequest)
}

/// A separate account-only route owns bounded enrollment sessions. It does not
/// alter ordinary transport authentication or acquire another store authority.
pub struct OwnerEndpoint {
    stop: watch::Sender<bool>,
    task: JoinHandle<Result<(), OwnerEndpointFailure>>,
}
impl OwnerEndpoint {
    pub fn start(
        supervisor: Arc<ServiceSupervisor>,
        handle: ServerHandle,
    ) -> Result<Self, OwnerEndpointFailure> {
        let listener = IpcListener::bind(
            endpoint(&supervisor)?,
            MessageLimit::new(OWNER_FRAME_LIMIT)?,
            AdmissionLimits::new(8, OWNER_DEADLINE)?,
        )?;
        let (stop, mut stopped) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut sessions = JoinSet::new();
            let mut cleanup_failed = false;
            let result = loop {
                tokio::select! {
                    biased;
                    _ = stopped.changed() => break Ok(()),
                    Some(result) = sessions.join_next(), if !sessions.is_empty() => {
                        cleanup_failed |= matches!(result, Err(_) | Ok(Err(OwnerEndpointFailure::Cleanup)));
                    },
                    accepted = listener.accept(), if sessions.len() < 8 => {
                        let pending = match accepted { Ok(value) => value, Err(_) => break Err(OwnerEndpointFailure::Transport) };
                        let supervisor = supervisor.clone();
                        let handle = handle.clone();
                        sessions.spawn(async move {
                            // Never finish ordinary authentication. Admission and
                            // its deadline remain retained for the entire owner session.
                            let result = async {
                                let initial = receive(&pending).await?.ok_or(OwnerEndpointFailure::InvalidRequest)?;
                                let request: OwnerEnrollmentRequest = serde_json::from_slice(&initial)
                                    .map_err(|_| OwnerEndpointFailure::InvalidRequest)?;
                                let claim = request.verify()?;
                                let executor = AccountOwnerRequestExecutor::running(supervisor, handle)?;
                                let result = serve_executor(&executor, &pending, request.request, claim).await;
                                executor.close().await.map_err(|_| OwnerEndpointFailure::Cleanup)?;
                                result
                            }.await;
                            let close = pending.close_joinable().await;
                            close.map_err(|_| OwnerEndpointFailure::Cleanup)?;
                            result
                        });
                    }
                }
            };
            let close = listener.close();
            while let Some(result) = sessions.join_next().await {
                cleanup_failed |= matches!(result, Err(_) | Ok(Err(OwnerEndpointFailure::Cleanup)));
            }
            close.map_err(|_| OwnerEndpointFailure::Cleanup)?;
            if cleanup_failed {
                return Err(OwnerEndpointFailure::Cleanup);
            }
            result
        });
        Ok(Self { stop, task })
    }
    pub async fn close(self) -> Result<(), OwnerEndpointFailure> {
        let _ = self.stop.send(true);
        self.task.await.map_err(|_| OwnerEndpointFailure::Cleanup)?
    }
}
