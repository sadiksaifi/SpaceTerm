//! Native lifecycle comparisons use the same pinned, paired protocol as every
//! other Client. Missing saved credentials never authorize enrollment or Start.

use std::{path::PathBuf, sync::Arc, time::Duration};

use spaceterm_client::{
    api::PairedServerBinding,
    failure::{ClientFailure, ClientFailureClass},
    journal::{JournalController, JournalLimits, ReceiptVerifier},
    pairing::{
        credentials::NativeDeviceCredentials,
        journal::{JournalPairingStore, LocalPairingStore},
    },
    ports::ConnectionJournal,
};
use spaceterm_core::{
    authority::{ServerEpoch, SessionKey},
    native_management::{NativeManagementRequestRef, NativeRetirementAction},
    retirement::SessionCleanupObservation,
};
use spaceterm_host::{
    account_service::{
        RegisteredServer, RuntimeAssessment, ServiceFailure, ServiceFuture, ServiceProcesses,
        runtime_inventory::{
            AccountRuntimeCall, AccountRuntimeReply, RUNTIME_MESSAGE_LIMIT,
            RetainedRuntimeProcesses,
        },
    },
    ipc::IpcTransport,
    transport::{MessageLimit, MessageTransport},
};
use spaceterm_protocol::{
    authentication::{
        ClientHandshake, Established, HandshakeConfig, Principal, Purpose, ServerPin, SigningKey,
    },
    codec::Payload,
    negotiation::CAPABILITY_ACCOUNT_RUNTIME,
    wire::{common, v1, v2},
};

use crate::{
    credentials::adapter::DeviceCredentials, journal::NativeClientJournal,
    route::LocalRouteAuthority,
};

const CONNECT_DEADLINE: Duration = Duration::from_secs(5);
const CLEANUP_DEADLINE: Duration = Duration::from_secs(30);

/// One verified saved Client identity. The lease remains tied to the canonical
/// journal, so native credential acquisition cannot revive a revoked binding.
pub struct RuntimeClientAccess {
    pub binding: PairedServerBinding,
    pub signer: Arc<dyn SigningKey>,
    pub journal: Arc<dyn ConnectionJournal>,
}
/// Native journal/credential boundary, never a wire-provided credential or path.
pub trait RuntimeClientIdentity: Send + Sync {
    fn access<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, RuntimeClientAccess>;
}

/// Lazily opens the account/build native-management Client journal. It never
/// provisions directories, keys or a replacement identity on missing reads.
pub struct NativeRuntimeIdentity {
    journal: Option<Arc<JournalController>>,
    credentials: Arc<dyn NativeDeviceCredentials>,
}
impl Default for NativeRuntimeIdentity {
    fn default() -> Self {
        Self {
            journal: None,
            credentials: Arc::new(DeviceCredentials::native()),
        }
    }
}
impl NativeRuntimeIdentity {
    /// Existing desktop/Bootstrap composition supplies its own saved additional
    /// Client journal. No second journal is created for that application.
    pub fn from_journal(
        journal: Arc<JournalController>,
        credentials: Arc<dyn NativeDeviceCredentials>,
    ) -> Self {
        Self {
            journal: Some(journal),
            credentials,
        }
    }
    /// The enrollment owner can provision this explicit native Client directory.
    /// Ordinary lifecycle observation only opens it if it already exists.
    pub fn directory(registration: &RegisteredServer) -> Result<PathBuf, ServiceFailure> {
        let account = spaceterm_host::account_service::TrustedAccountFacts::current()?;
        if account.uid() != registration.key().account_uid {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let build = match registration.key().build_identity {
            spaceterm_host::startup::BuildIdentity::Release => "release",
            spaceterm_host::startup::BuildIdentity::Preflight => "preflight",
            spaceterm_host::startup::BuildIdentity::Development => "development",
        };
        Ok(account
            .home()
            .join(".spaceterm-services")
            .join(build)
            .join("client-connections")
            .join("management"))
    }
    async fn journal(
        &self,
        registration: &RegisteredServer,
    ) -> Result<Arc<JournalController>, ServiceFailure> {
        if let Some(journal) = &self.journal {
            return Ok(Arc::clone(journal));
        }
        let codec = Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec);
        let verifier: Arc<dyn ReceiptVerifier> = Arc::new(PinnedReceipts);
        let limits = JournalLimits::default();
        let storage = NativeClientJournal::open_existing(
            Self::directory(registration)?,
            codec.clone(),
            verifier.clone(),
            limits,
        )
        .await
        .map_err(client_failure)?;
        JournalController::recover(Arc::new(storage), codec, verifier, limits)
            .await
            .map(Arc::new)
            .map_err(client_failure)
    }
}
impl RuntimeClientIdentity for NativeRuntimeIdentity {
    fn access<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, RuntimeClientAccess> {
        Box::pin(async move {
            let journal = self.journal(registration).await?;
            let bindings = journal.recover_bindings().await.map_err(client_failure)?;
            let binding = bindings
                .into_iter()
                .find(|binding| binding.target().pin() == registration.server_pin())
                .ok_or(ServiceFailure::Missing)?;
            let key = binding.target().local_pairing().map_err(client_failure)?;
            let store = JournalPairingStore::new(Arc::clone(&journal));
            let snapshot = store
                .recover()
                .await
                .map_err(|_| ServiceFailure::IdentityMismatch)?;
            let saved = snapshot
                .active(&key)
                .ok_or(ServiceFailure::IdentityMismatch)?;
            let authority = store
                .ordinary_authority(&key)
                .await
                .map_err(|_| ServiceFailure::AuthorizationDenied)?;
            let signer = self
                .credentials
                .signing_key(saved.credential(), &authority.lease())
                .await
                .map_err(|_| ServiceFailure::AuthorizationUnavailable)?;
            Ok(RuntimeClientAccess {
                binding,
                signer,
                journal,
            })
        })
    }
}
struct PinnedReceipts;
impl ReceiptVerifier for PinnedReceipts {
    fn verify(
        &self,
        pin: ServerPin,
        receipt: &spaceterm_core::pairing::SignedPairingReceipt,
    ) -> Result<(), ClientFailure> {
        spaceterm_protocol::pairing_adapter::ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidStorage))
    }
}

/// Real paired cross-process inventory and native signed cleanup reconciliation.
/// NativeServiceProcesses still independently checks the approved root process
/// and registered endpoint before claiming Stop or Start absence.
pub struct RuntimeServiceProcesses {
    identity: Arc<dyn RuntimeClientIdentity>,
}
impl Default for RuntimeServiceProcesses {
    fn default() -> Self {
        Self::new(Arc::new(NativeRuntimeIdentity::default()))
    }
}
impl RuntimeServiceProcesses {
    pub fn new(identity: Arc<dyn RuntimeClientIdentity>) -> Self {
        Self { identity }
    }
    async fn call(
        &self,
        registration: &RegisteredServer,
        call: AccountRuntimeCall,
    ) -> Result<AccountRuntimeReply, ServiceFailure> {
        let access = self.identity.access(registration).await?;
        let target = access.binding.target();
        let local = LocalRouteAuthority::new(registration.endpoint()?).map_err(client_failure)?;
        if target.pin() != registration.server_pin() || target.route() != local.description() {
            return Err(ServiceFailure::IdentityMismatch);
        }
        if !access
            .journal
            .ordinary_use_allowed(&access.binding)
            .await
            .map_err(client_failure)?
        {
            return Err(ServiceFailure::AuthorizationDenied);
        }
        let transport = tokio::time::timeout(
            CONNECT_DEADLINE,
            IpcTransport::connect(&registration.endpoint()?, MessageLimit::envelope()),
        )
        .await
        .map_err(|_| ServiceFailure::SupervisorUnavailable)?
        .map_err(|_| ServiceFailure::SupervisorUnavailable)?;
        let result = tokio::time::timeout(CONNECT_DEADLINE, async {
            let local_id = |n: u64| {
                let mut bytes = [0; 16];
                bytes[8..].copy_from_slice(&n.to_be_bytes());
                bytes
            };
            let principal = Principal {
                server: target.server().bytes(),
                device: target.device().bytes(),
                pairing: local_id(target.pairing().local()),
                client: local_id(target.client().local()),
            };
            let config = HandshakeConfig {
                capabilities: vec![CAPABILITY_ACCOUNT_RUNTIME],
                ..HandshakeConfig::default()
            };
            let (handshake, hello) = ClientHandshake::begin(
                target.pin(),
                principal,
                Purpose::Ordinary,
                config,
                access.signer,
            )
            .map_err(|_| ServiceFailure::IdentityMismatch)?;
            transport
                .send(&hello)
                .await
                .map_err(|_| ServiceFailure::SupervisorUnavailable)?;
            let reply = transport
                .recv()
                .await
                .map_err(|_| ServiceFailure::SupervisorUnavailable)?
                .ok_or(ServiceFailure::SupervisorUnavailable)?;
            let (ready, proof) = handshake
                .receive_server(&reply)
                .map_err(|_| ServiceFailure::IdentityMismatch)?;
            transport
                .send(&proof)
                .await
                .map_err(|_| ServiceFailure::SupervisorUnavailable)?;
            let reply = transport
                .recv()
                .await
                .map_err(|_| ServiceFailure::SupervisorUnavailable)?
                .ok_or(ServiceFailure::SupervisorUnavailable)?;
            let Established::Ordinary(mut codec) = ready
                .receive_ready(&reply)
                .map_err(|_| ServiceFailure::IdentityMismatch)?
            else {
                return Err(ServiceFailure::AuthorizationDenied);
            };
            if !codec.capabilities().contains(&CAPABILITY_ACCOUNT_RUNTIME) {
                return Err(ServiceFailure::UnsupportedSchema);
            }
            if !access
                .journal
                .ordinary_use_allowed(&access.binding)
                .await
                .map_err(client_failure)?
            {
                return Err(ServiceFailure::AuthorizationDenied);
            }
            let body = serde_json::to_vec(&call).map_err(|_| ServiceFailure::InvalidRecord)?;
            if body.len() > RUNTIME_MESSAGE_LIMIT {
                return Err(ServiceFailure::LimitExceeded);
            }
            let message = common::AccountRuntimeMessage { request: 1, body };
            let payload = match codec.major() {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::AccountRuntimeRequest(message)),
                }),
                2 => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::AccountRuntimeRequest(message)),
                }),
                _ => return Err(ServiceFailure::UnsupportedSchema),
            };
            let bytes = codec
                .encode(&payload)
                .map_err(|_| ServiceFailure::InvalidRecord)?;
            transport
                .send(&bytes)
                .await
                .map_err(|_| ServiceFailure::SupervisorUnavailable)?;
            let bytes = transport
                .recv()
                .await
                .map_err(|_| ServiceFailure::SupervisorUnavailable)?
                .ok_or(ServiceFailure::SupervisorUnavailable)?;
            let response = codec
                .decode(&bytes)
                .map_err(|_| ServiceFailure::IdentityMismatch)?;
            let response = match response {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::AccountRuntimeReply(value)),
                }) => value,
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::AccountRuntimeReply(value)),
                }) => value,
                _ => return Err(ServiceFailure::InvalidRecord),
            };
            if response.request != 1 {
                return Err(ServiceFailure::IdentityMismatch);
            }
            let reply: AccountRuntimeReply = serde_json::from_slice(&response.body)
                .map_err(|_| ServiceFailure::InvalidRecord)?;
            if let AccountRuntimeReply::Failed(failure) = reply {
                return Err(failure);
            }
            Ok(reply)
        })
        .await
        .map_err(|_| ServiceFailure::SupervisorUnavailable)?;
        let _ = transport.close().await;
        result
    }
}
impl ServiceProcesses for RuntimeServiceProcesses {
    fn assess<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, RuntimeAssessment> {
        Box::pin(async move {
            if RetainedRuntimeProcesses::is_clean(registration)? {
                return RetainedRuntimeProcesses.assess(registration).await;
            }
            match self.call(registration, AccountRuntimeCall::Assess).await? {
                AccountRuntimeReply::Assessment(assessment) => Ok(assessment),
                _ => Err(ServiceFailure::InvalidRecord),
            }
        })
    }
    fn revalidate<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            if assessment.epoch.is_none() {
                return RetainedRuntimeProcesses
                    .revalidate(registration, assessment)
                    .await;
            }
            match self
                .call(
                    registration,
                    AccountRuntimeCall::Reserve(assessment.clone()),
                )
                .await?
            {
                AccountRuntimeReply::Reserved => Ok(()),
                _ => Err(ServiceFailure::InvalidRecord),
            }
        })
    }
    fn abort_reservation<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            if assessment.epoch.is_none() {
                return RetainedRuntimeProcesses
                    .abort_reservation(registration, assessment)
                    .await;
            }
            match self
                .call(registration, AccountRuntimeCall::Abort(assessment.clone()))
                .await?
            {
                AccountRuntimeReply::Aborted => Ok(()),
                _ => Err(ServiceFailure::InvalidRecord),
            }
        })
    }
    fn quiesce<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        request: NativeManagementRequestRef,
        action: NativeRetirementAction,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move {
            if assessment.epoch.is_none() {
                return RetainedRuntimeProcesses
                    .quiesce(registration, request, action, assessment)
                    .await;
            }
            if let Ok(cleanup) =
                RetainedRuntimeProcesses::cleanup(registration, request, action, assessment)
            {
                return Ok(cleanup);
            }
            let result = self
                .call(
                    registration,
                    AccountRuntimeCall::Quiesce {
                        request,
                        action,
                        assessment: assessment.clone(),
                    },
                )
                .await;
            match result {
                Ok(AccountRuntimeReply::QuiescenceAccepted)
                | Err(ServiceFailure::SupervisorUnavailable) => {}
                Err(failure) => return Err(failure),
                _ => return Err(ServiceFailure::InvalidRecord),
            }
            let deadline = tokio::time::Instant::now() + CLEANUP_DEADLINE;
            loop {
                match RetainedRuntimeProcesses::cleanup(registration, request, action, assessment) {
                    Ok(observations) => return Ok(observations),
                    Err(ServiceFailure::CleanupUnresolved) => {}
                    Err(failure) => return Err(failure),
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(ServiceFailure::Pending);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
    }
    fn verify_absent<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        RetainedRuntimeProcesses.verify_absent(registration)
    }
    fn previous_instance_cleanup<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        epoch: ServerEpoch,
        sessions: &'a [SessionKey],
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        RetainedRuntimeProcesses.previous_instance_cleanup(registration, epoch, sessions)
    }
}
fn client_failure(failure: ClientFailure) -> ServiceFailure {
    match failure.class() {
        ClientFailureClass::UnsafeStorage | ClientFailureClass::InvalidStorage => {
            ServiceFailure::UnsafeStorage
        }
        ClientFailureClass::AuthorizationDenied => ServiceFailure::AuthorizationDenied,
        _ => ServiceFailure::StorageUnavailable,
    }
}
