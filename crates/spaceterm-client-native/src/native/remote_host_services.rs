//! Management of one remote account's already paired Server. SSH owns native
//! reachability; the existing authenticated worker owns live runtime admission.

use std::{num::NonZeroU64, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use spaceterm_client::{
    api::{PairedServerBinding, ServerQueries},
    failure::{ClientFailure, ClientFailureClass},
    journal::{JournalController, MAX_REMOTE_NATIVE_BYTES, RemoteNativeRequest},
    management_cli::{AssessmentToken, ManagementFailure, ManagementFuture},
};
use spaceterm_core::{
    authority::ServerEpoch,
    identity::ClientId,
    operations::RequestDigest,
    pairing::{LocalPairingKey, PairingFailure},
};
use spaceterm_host::{
    account_service::{
        NativeLifecycleAssessmentRef, NativeLifecycleRequest, NativeStageAccountReply,
        NativeStageAccountRequest, NativeStageAssessmentRef, NativeStageRequest, ServiceAction,
        ServiceOutcome, ServicePhase, ServiceProgress, ServiceRequestState, StoppedNativeAction,
        StoppedNativeAssessmentRef, StoppedNativeCall, StoppedNativeReply, StoppedNativeRequest,
        remote_wire::{NativeLifecycleAccountReply, NativeLifecycleAccountRequest},
        runtime_inventory::{AccountRuntimeCall, AccountRuntimeReply, RUNTIME_MESSAGE_LIMIT},
    },
    native_deployment::contracts::NativeRequestId,
    startup::AccountBuildKey,
    transport::MessageTransport,
};
use spaceterm_protocol::{
    authentication::{
        ClientHandshake, Established, HandshakeConfig, Principal, Purpose, ServerPin,
    },
    codec::Payload,
    connection::AuthenticatedConnection,
    negotiation::{CAPABILITY_ACCOUNT_RUNTIME, CAPABILITY_REMOTE_NATIVE_LIFECYCLE},
    wire::{common, v1, v2},
};

use crate::pairing_runtime::NativePairingRuntime;
use spaceterm_distribution::{BuildVersion, VerifiedAccountServerArtifact};
use spaceterm_host::startup::DesiredServerState;

const STOPPED_EXCHANGE_DEADLINE: Duration = Duration::from_secs(5);
use super::{
    NativeActionPrompt, classify_service,
    host_services::{HostServiceAssessment, HostServiceManagement, HostServiceObservation},
};

/// External SSH/native-channel boundary for the original saved Bootstrap route.
///
/// Implementations retain the original SSH master and captured configuration.
/// They revalidate its host, account and capture before sending and after receiving,
/// enforce the native wire bounds, and never install, enroll or replace a pairing.
/// Dropping a foreground future does not cancel already accepted native work.
/// No default implementation or provider-unavailable fallback exists.
pub trait RemoteNativeTransport: Send + Sync {
    /// Rechecks the original SSH owner, host, account and captured configuration.
    fn revalidate(&self, key: AccountBuildKey, pin: ServerPin) -> ManagementFuture<'_, ()>;
    /// Exchanges only the closed native account grammar, with owner checks on both sides.
    /// Capture establishes guest-owned original completion custody before replying.
    /// Paired Accepted only publishes admission; the SSH owner completes that
    /// original independently of foreground cancellation or response delivery.
    fn account_call(
        &self,
        request: NativeLifecycleAccountRequest,
    ) -> ManagementFuture<'_, NativeLifecycleAccountReply>;
    /// Transfers an exact verified package only for this original Stage request.
    /// Re-supply resumes accepted work without another assessment or admission.
    fn stage_call<'a>(
        &'a self,
        request: NativeStageAccountRequest,
        artifact: Option<&'a VerifiedAccountServerArtifact>,
    ) -> ManagementFuture<'a, NativeStageAccountReply>;
    /// Captures the same original request and opens its frozen stopped executor.
    /// The returned channel performs ordinary pinned authentication with the Mac
    /// signer. This operation never accepts effects or opens a live listener.
    fn stopped_channel(
        &self,
        request: RemoteStoppedRequest,
        artifact: Option<VerifiedAccountServerArtifact>,
    ) -> ManagementFuture<'_, (StoppedNativeRequest, Arc<dyn MessageTransport>)>;
    /// Reads the retained signed original without refreshing its expiry or opening SQLite.
    fn restore_stopped(
        &self,
        key: AccountBuildKey,
        pin: ServerPin,
        request: NativeRequestId,
        action: StoppedNativeAction,
    ) -> ManagementFuture<'_, Option<StoppedNativeAssessmentRef>>;
    /// Resolves only already admitted stopped work, never replaying confirmation.
    fn resolve_stopped<'a>(
        &'a self,
        key: AccountBuildKey,
        pin: ServerPin,
        request: NativeRequestId,
        artifact: Option<&'a VerifiedAccountServerArtifact>,
    ) -> ManagementFuture<'a, ServiceProgress>;
    /// Acknowledges the exact immutable native result, including a lost-ACK retry.
    fn acknowledge_stopped(
        &self,
        key: AccountBuildKey,
        pin: ServerPin,
        progress: ServiceProgress,
    ) -> ManagementFuture<'_, ServiceProgress>;
}

/// Closed native stopped-capture comparisons. None grants admission authority.
/// The guest checks the exact observed generation under its capture lease.
pub use spaceterm_host::account_service::StoppedNativeChannelRequest as RemoteStoppedRequest;

/// Exact selected-server capability. It owns the Mac's saved Client runtime and
/// retains native comparisons in that runtime's canonical journal.
pub struct RemoteHostServices {
    runtime: Arc<NativePairingRuntime>,
    local_pairing: LocalPairingKey,
    client: ClientId,
    binding: PairedServerBinding,
    journal: Arc<JournalController>,
    queries: ServerQueries,
    key: AccountBuildKey,
    transport: Arc<dyn RemoteNativeTransport>,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginalScope {
    key: AccountBuildKey,
    server: [u8; 16],
    public_key: [u8; 32],
    request: NativeRequestId,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum RequestIntent {
    Lifecycle(ServiceAction),
    Stopped {
        action: StoppedNativeAction,
        generation: NonZeroU64,
        #[serde(default)]
        stage_version: Option<BuildVersion>,
    },
    Stage {
        artifact_sha256: [u8; 32],
        version: BuildVersion,
    },
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "authority", content = "reference", deny_unknown_fields)]
enum RetainedAssessment {
    Live(NativeLifecycleAssessmentRef),
    Stopped(StoppedNativeAssessmentRef),
    Stage(NativeStageAssessmentRef),
}

fn live_reference(bytes: &[u8]) -> Result<NativeLifecycleAssessmentRef, ManagementFailure> {
    match decode(bytes)? {
        RetainedAssessment::Live(reference) => Ok(reference),
        RetainedAssessment::Stopped(_) | RetainedAssessment::Stage(_) => {
            Err(ManagementFailure::StaleAssessment)
        }
    }
}

impl RemoteHostServices {
    /// Reads the account/build comparison of exact submitted canonical originals.
    /// Unsubmitted records and records without a retained assessment are not evidence.
    /// Conflicting keys or malformed retained originals are refused. This lookup
    /// grants no pairing, SSH ownership, connection, capture or admission authority.
    pub fn retained_account_key(
        journal: &JournalController,
        local_pairing: &LocalPairingKey,
        client: ClientId,
        pin: ServerPin,
    ) -> Result<Option<AccountBuildKey>, ManagementFailure> {
        if client.server().bytes() != pin.identity
            || local_pairing.parts().server != client.server()
        {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        let originals = journal
            .recover_remote_native_requests(local_pairing, client)
            .map_err(client_failure)?;
        let mut key = None;
        for record in originals
            .iter()
            .filter(|record| record.submitted() && record.assessment().is_some())
        {
            let scope: OriginalScope = decode(record.original_scope())?;
            let request = NativeRequestId::new(record.request_id())
                .map_err(|_| ManagementFailure::UnsafeNativeState)?;
            if scope.server != pin.identity
                || scope.public_key != pin.public_key
                || scope.request != request
                || scope.key.account_uid == 0
            {
                return Err(ManagementFailure::UnsafeNativeState);
            }
            Self::validate_record(record, request)?;
            if key.is_some_and(|original| original != scope.key) {
                return Err(ManagementFailure::UnsafeNativeState);
            }
            key = Some(scope.key);
        }
        Ok(key)
    }

    /// Verifies the exact saved pairing/client and the worker's canonical journal.
    /// Construction cannot enroll, obtain a guest key, Start, or open a connection.
    pub async fn new(
        runtime: Arc<NativePairingRuntime>,
        local_pairing: LocalPairingKey,
        client: ClientId,
        binding: PairedServerBinding,
        queries: ServerQueries,
        key: AccountBuildKey,
        transport: Arc<dyn RemoteNativeTransport>,
    ) -> Result<Self, ManagementFailure> {
        if key.account_uid == 0
            || binding.target().client() != client
            || binding.target().local_pairing().as_ref() != Ok(&local_pairing)
        {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        let journal = Arc::clone(runtime.journal());
        queries
            .validate_recovery_owner(&binding, &journal)
            .map_err(client_failure)?;
        if journal
            .ordinary_use_allowed(&binding)
            .map_err(client_failure)?
        {
            let saved = runtime
                .restore(&local_pairing, client)
                .await
                .map_err(pairing_failure)?;
            if saved.binding().binding().map_err(client_failure)? != binding {
                return Err(ManagementFailure::AuthorizationDenied);
            }
        } else {
            // An already-held original query port can identify canonical accepted
            // recovery after sealing. It cannot authenticate or admit new work.
            if Self::retained_account_key(&journal, &local_pairing, client, binding.target().pin())?
                != Some(key)
            {
                return Err(ManagementFailure::AuthorizationDenied);
            }
        }
        Ok(Self {
            runtime,
            local_pairing,
            client,
            binding,
            journal,
            queries,
            key,
            transport,
        })
    }

    fn check_owner(&self) -> Result<(), ManagementFailure> {
        self.queries
            .validate_owner(&self.binding, &self.journal)
            .map_err(client_failure)
    }

    fn original_scope(&self, request: NativeRequestId) -> OriginalScope {
        let pin = self.binding.target().pin();
        OriginalScope {
            key: self.key,
            server: pin.identity,
            public_key: pin.public_key,
            request,
        }
    }

    fn records(&self) -> Result<Vec<RemoteNativeRequest>, ManagementFailure> {
        self.journal
            .recover_remote_native_requests(&self.local_pairing, self.client)
            .map_err(client_failure)
    }

    fn record(&self, request: NativeRequestId) -> Result<RemoteNativeRequest, ManagementFailure> {
        let record = self
            .records()?
            .into_iter()
            .find(|record| record.request_id() == *request.as_bytes())
            .ok_or(ManagementFailure::StaleAssessment)?;
        let scope: OriginalScope = decode(record.original_scope())?;
        if scope != self.original_scope(request) {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        Self::validate_record(&record, request)?;
        Ok(record)
    }

    fn validate_record(
        record: &RemoteNativeRequest,
        request: NativeRequestId,
    ) -> Result<(), ManagementFailure> {
        let intent: Option<RequestIntent> = record.intent().map(decode).transpose()?;
        if let Some(RequestIntent::Stopped {
            action,
            stage_version,
            ..
        }) = &intent
            && matches!(action, StoppedNativeAction::Stage { .. }) != stage_version.is_some()
        {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        let assessment: Option<RetainedAssessment> = record.assessment().map(decode).transpose()?;
        let digest = match (&intent, &assessment) {
            (Some(RequestIntent::Lifecycle(action)), Some(RetainedAssessment::Live(reference)))
                if reference.original.request == request
                    && reference.original.action == *action =>
            {
                validate_reference(reference.original, reference)?;
                Some(reference.request_digest)
            }
            (
                Some(RequestIntent::Stopped { action, .. }),
                Some(RetainedAssessment::Stopped(reference)),
            ) => {
                validate_stopped_reference(request, *action, reference)?;
                Some(reference.request_digest)
            }
            (
                Some(RequestIntent::Stage {
                    artifact_sha256, ..
                }),
                Some(RetainedAssessment::Stage(reference)),
            ) if reference.original.request == request
                && reference.original.artifact_sha256 == *artifact_sha256 =>
            {
                validate_stage_reference(reference.original, reference)?;
                Some(reference.request_digest)
            }
            (_, None) if !record.submitted() && record.result().is_none() => None,
            _ => return Err(ManagementFailure::UnsafeNativeState),
        };
        if let Some(result) = record.result() {
            let progress = decode(result)?;
            if !record.submitted() || !terminal(&progress) {
                return Err(ManagementFailure::UnsafeNativeState);
            }
            validate_progress(request, digest, &progress)?;
            validate_outcome(intent.as_ref(), &progress)?;
        }
        Ok(())
    }

    fn check_resolution_owner(&self, request: NativeRequestId) -> Result<(), ManagementFailure> {
        self.queries
            .validate_recovery_owner(&self.binding, &self.journal)
            .map_err(client_failure)?;
        let record = self.record(request)?;
        if record.submitted() || record.result().is_some() {
            return Ok(());
        }
        self.check_owner()
    }

    fn check_acknowledgement(&self, progress: &ServiceProgress) -> Result<(), ManagementFailure> {
        self.check_resolution_owner(progress.request)?;
        let record = self.record(progress.request)?;
        if record
            .result()
            .map(decode::<ServiceProgress>)
            .transpose()?
            .as_ref()
            != Some(progress)
        {
            return Err(ManagementFailure::StaleAssessment);
        }
        Ok(())
    }

    async fn retain_intent(
        &self,
        record: &RemoteNativeRequest,
        intent: RequestIntent,
    ) -> Result<(), ManagementFailure> {
        if record.submitted() || record.result().is_some() {
            return Err(ManagementFailure::StaleAssessment);
        }
        self.journal
            .retain_remote_native_intent(
                self.local_pairing.clone(),
                self.client,
                record.request_id(),
                record.original_scope().to_vec(),
                encode(&intent)?,
            )
            .await
            .map_err(client_failure)?;
        Ok(())
    }

    fn check_native_call_owner(
        &self,
        request: &NativeLifecycleAccountRequest,
    ) -> Result<(), ManagementFailure> {
        match request {
            NativeLifecycleAccountRequest::Resolve { request, .. } => {
                self.check_resolution_owner(*request)
            }
            NativeLifecycleAccountRequest::Acknowledge { progress, .. } => {
                self.check_acknowledgement(progress)
            }
            _ => self.check_owner(),
        }
    }

    async fn native_call(
        &self,
        request: NativeLifecycleAccountRequest,
    ) -> Result<NativeLifecycleAccountReply, ManagementFailure> {
        self.check_native_call_owner(&request)?;
        request
            .validate()
            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
        self.transport
            .revalidate(self.key, self.binding.target().pin())
            .await?;
        let reply = self.transport.account_call(request.clone()).await?;
        self.transport
            .revalidate(self.key, self.binding.target().pin())
            .await?;
        self.check_native_call_owner(&request)?;
        reply
            .validate()
            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
        match reply {
            NativeLifecycleAccountReply::Refused(failure) => Err(classify_service(failure)),
            reply => Ok(reply),
        }
    }

    fn check_start_compatibility(&self) -> Result<(), ManagementFailure> {
        self.check_owner()?;
        self.queries.check_compatible().map_err(client_failure)?;
        match self.queries.capabilities() {
            Ok(capabilities)
                if !capabilities.supports(CAPABILITY_ACCOUNT_RUNTIME)
                    || !capabilities.supports(CAPABILITY_REMOTE_NATIVE_LIFECYCLE) =>
            {
                Err(ManagementFailure::Incompatible)
            }
            Ok(_) => Ok(()),
            Err(failure) if failure.class() == ClientFailureClass::Unavailable => Ok(()),
            Err(failure) => Err(client_failure(failure)),
        }
    }

    fn live_epoch(&self) -> Result<ServerEpoch, ManagementFailure> {
        self.check_owner()?;
        self.queries.check_compatible().map_err(client_failure)?;
        let capabilities = self.queries.capabilities().map_err(client_failure)?;
        if !capabilities.supports(CAPABILITY_ACCOUNT_RUNTIME)
            || !capabilities.supports(CAPABILITY_REMOTE_NATIVE_LIFECYCLE)
        {
            return Err(ManagementFailure::Incompatible);
        }
        self.queries.server_epoch().map_err(client_failure)
    }

    async fn runtime_call(
        &self,
        call: AccountRuntimeCall,
        epoch: ServerEpoch,
    ) -> Result<AccountRuntimeReply, ManagementFailure> {
        if self.live_epoch()? != epoch {
            return Err(ManagementFailure::StaleAssessment);
        }
        self.transport
            .revalidate(self.key, self.binding.target().pin())
            .await?;
        let bytes = encode_limited(&call, RUNTIME_MESSAGE_LIMIT)?;
        // This is the existing worker. A same-client standalone handshake would
        // replace its server connection binding and invalidate foreground work.
        let reply = self
            .queries
            .native_account_runtime(&bytes)
            .await
            .map_err(client_failure)?;
        self.transport
            .revalidate(self.key, self.binding.target().pin())
            .await?;
        if self.live_epoch()? != epoch {
            return Err(ManagementFailure::StaleAssessment);
        }
        match decode_limited(&reply, RUNTIME_MESSAGE_LIMIT)? {
            AccountRuntimeReply::Failed(failure) => Err(classify_service(failure)),
            reply => Ok(reply),
        }
    }

    async fn inspect_remote(&self) -> Result<HostServiceObservation, ManagementFailure> {
        let NativeLifecycleAccountReply::Observed(observation) = self
            .native_call(NativeLifecycleAccountRequest::Observe {
                expected_key: self.key,
                expected_pin: self.binding.target().pin(),
            })
            .await?
        else {
            return Err(ManagementFailure::UnsafeNativeState);
        };
        if observation.status.key != self.key
            || observation.status.server != Some(self.binding.target().server())
        {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        Ok(HostServiceObservation {
            status: observation.status,
            generation: observation.registration_generation,
        })
    }

    async fn reserve_remote(&self) -> Result<NativeRequestId, ManagementFailure> {
        let NativeLifecycleAccountReply::Reserved(request) = self
            .native_call(NativeLifecycleAccountRequest::Reserve {
                expected_key: self.key,
                expected_pin: self.binding.target().pin(),
            })
            .await?
        else {
            return Err(ManagementFailure::UnsafeNativeState);
        };
        // A guest reservation has no effects. No Capture or admission can precede
        // this separately durable canonical original-scope commit.
        self.journal
            .retain_remote_native_request(
                RemoteNativeRequest::new(
                    self.local_pairing.clone(),
                    self.client,
                    *request.as_bytes(),
                    encode(&self.original_scope(request))?,
                )
                .map_err(client_failure)?,
            )
            .await
            .map_err(client_failure)?;
        Ok(request)
    }

    async fn capture(
        &self,
        request: NativeRequestId,
        action: ServiceAction,
    ) -> Result<NativeLifecycleRequest, ManagementFailure> {
        let NativeLifecycleAccountReply::Captured(original) = self
            .native_call(NativeLifecycleAccountRequest::Capture {
                expected_key: self.key,
                expected_pin: self.binding.target().pin(),
                request,
                action,
            })
            .await?
        else {
            return Err(ManagementFailure::UnsafeNativeState);
        };
        if original.request != request || original.action != action {
            return Err(ManagementFailure::StaleAssessment);
        }
        Ok(original)
    }

    async fn restore_reference(
        &self,
        original: NativeLifecycleRequest,
        assessment: Option<NativeLifecycleAssessmentRef>,
    ) -> Result<Option<NativeLifecycleAssessmentRef>, ManagementFailure> {
        let NativeLifecycleAccountReply::Restored {
            original: restored,
            assessment: retained,
        } = self
            .native_call(NativeLifecycleAccountRequest::Restore {
                expected_key: self.key,
                expected_pin: self.binding.target().pin(),
                original,
                assessment: assessment.clone(),
            })
            .await?
        else {
            return Err(ManagementFailure::UnsafeNativeState);
        };
        if restored != original
            || assessment
                .as_ref()
                .is_some_and(|expected| retained.as_ref() != Some(expected))
        {
            return Err(ManagementFailure::StaleAssessment);
        }
        if let Some(reference) = &retained {
            validate_reference(original, reference)?;
        }
        Ok(retained)
    }

    async fn retain_reference(
        &self,
        record: &RemoteNativeRequest,
        reference: &NativeLifecycleAssessmentRef,
    ) -> Result<(), ManagementFailure> {
        self.journal
            .retain_remote_native_assessment(
                self.local_pairing.clone(),
                self.client,
                record.request_id(),
                record.original_scope().to_vec(),
                encode(&RetainedAssessment::Live(reference.clone()))?,
            )
            .await
            .map_err(client_failure)?;
        Ok(())
    }

    async fn assess_live(
        &self,
        request: NativeRequestId,
        action: ServiceAction,
    ) -> Result<HostServiceAssessment, ManagementFailure> {
        let record = self.record(request)?;
        self.retain_intent(&record, RequestIntent::Lifecycle(action))
            .await?;
        if let Some(bytes) = record.assessment() {
            let reference = live_reference(bytes)?;
            if reference.original.request != request || reference.original.action != action {
                return Err(ManagementFailure::StaleAssessment);
            }
            self.restore_reference(reference.original, Some(reference.clone()))
                .await?;
            return prompt(&reference);
        }
        let original = self.capture(request, action).await?;
        // Lost Capture or Assess replies restore the original guest reference.
        // Neither path mints another ID, confirmation scope or expiry.
        let restored = self.restore_reference(original, None).await?;
        // An SSH-restored public summary alone cannot prove which saved Client
        // produced it. The same authenticated worker restores its own signed
        // original through idempotent Assess, never a replacement assessment.
        let epoch = self.live_epoch().map_err(|failure| {
            if restored.is_some() && failure == ManagementFailure::Offline {
                ManagementFailure::OutcomePending
            } else {
                failure
            }
        })?;
        let NativeLifecycleAssessment(reference) = self
            .runtime_call(AccountRuntimeCall::AssessNativeLifecycle(original), epoch)
            .await?
        else {
            return Err(ManagementFailure::UnsafeNativeState);
        };
        validate_reference(original, &reference)?;
        if reference.epoch != epoch
            || restored
                .as_ref()
                .is_some_and(|original| original != &reference)
        {
            return Err(ManagementFailure::StaleAssessment);
        }
        self.retain_reference(&record, &reference).await?;
        prompt(&reference)
    }

    async fn restore_prompt(
        &self,
        action: ServiceAction,
        token: AssessmentToken,
    ) -> Result<HostServiceAssessment, ManagementFailure> {
        self.check_owner()?;
        let (request, _) = decode_prompt_token(&token)?;
        let record = self.record(request)?;
        let reference = live_reference(
            record
                .assessment()
                .ok_or(ManagementFailure::StaleAssessment)?,
        )?;
        if reference.original.request != request
            || reference.original.action != action
            || prompt_token(&reference)? != token
        {
            return Err(ManagementFailure::StaleAssessment);
        }
        self.restore_reference(reference.original, Some(reference.clone()))
            .await?;
        prompt(&reference)
    }

    async fn submit_live(
        &self,
        original: HostServiceAssessment,
    ) -> Result<ServiceProgress, ManagementFailure> {
        self.check_owner()?;
        let request = original.prompt.request;
        let record = self.record(request)?;
        // A recovered submitted marker never becomes a new foreground confirmation.
        if record.submitted() || record.result().is_some() {
            return Err(ManagementFailure::StaleAssessment);
        }
        let reference = live_reference(
            record
                .assessment()
                .ok_or(ManagementFailure::StaleAssessment)?,
        )?;
        let expected = prompt(&reference)?;
        if reference.original.request != request
            || original.prompt.action != expected.prompt.action
            || original.prompt.token != expected.prompt.token
            || original.prompt.affected_sessions != expected.prompt.affected_sessions
            || original.runtime_epoch != expected.runtime_epoch
            || record.intent().map(decode).transpose()?
                != Some(RequestIntent::Lifecycle(original.prompt.action))
        {
            return Err(ManagementFailure::StaleAssessment);
        }
        self.restore_reference(reference.original, Some(reference.clone()))
            .await?;
        let epoch = self.live_epoch()?;
        if epoch != reference.epoch {
            return Err(ManagementFailure::StaleAssessment);
        }
        self.journal
            .mark_remote_native_submitted(
                self.local_pairing.clone(),
                self.client,
                record.request_id(),
                record.original_scope().to_vec(),
                record.assessment().map(<[u8]>::to_vec),
            )
            .await
            .map_err(client_failure)?;
        let AccountRuntimeReply::NativeLifecycleAccepted(progress) = self
            .runtime_call(
                AccountRuntimeCall::AdmitNativeLifecycle(reference.clone()),
                epoch,
            )
            .await?
        else {
            return Err(ManagementFailure::UnsafeNativeState);
        };
        validate_progress(request, Some(reference.request_digest), &progress)?;
        if progress.phase != ServicePhase::Accepted
            || progress.state != ServiceRequestState::Pending
        {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        // The server returns only Accepted. The SSH native owner advances only
        // this original request and keeps accepted work independent of the caller.
        self.resolve_remote(request).await
    }

    async fn resolve_remote(
        &self,
        request: NativeRequestId,
    ) -> Result<ServiceProgress, ManagementFailure> {
        self.resolve_remote_with_artifact(request, None).await
    }

    async fn resolve_remote_with_artifact(
        &self,
        request: NativeRequestId,
        artifact: Option<&VerifiedAccountServerArtifact>,
    ) -> Result<ServiceProgress, ManagementFailure> {
        self.check_resolution_owner(request)?;
        let record = self.record(request)?;
        let assessment: Option<RetainedAssessment> = record.assessment().map(decode).transpose()?;
        let intent: Option<RequestIntent> = record.intent().map(decode).transpose()?;
        let stopped = matches!(&intent, Some(RequestIntent::Stopped { .. }));
        let stage = matches!(&intent, Some(RequestIntent::Stage { .. }));
        if let Some(artifact) = artifact {
            let matches = match &intent {
                Some(RequestIntent::Stage {
                    artifact_sha256,
                    version,
                }) => {
                    *artifact_sha256 == artifact.artifact_sha256()
                        && version == &artifact.manifest().version
                }
                Some(RequestIntent::Stopped {
                    action: StoppedNativeAction::Stage { artifact_sha256 },
                    stage_version,
                    ..
                }) => {
                    *artifact_sha256 == artifact.artifact_sha256()
                        && stage_version.as_ref() == Some(&artifact.manifest().version)
                }
                _ => false,
            };
            if !matches || !record.submitted() {
                return Err(ManagementFailure::StaleAssessment);
            }
        }
        let expected_digest = match &assessment {
            Some(RetainedAssessment::Live(reference))
                if !stopped && !stage && reference.original.request == request =>
            {
                Some(reference.request_digest)
            }
            Some(RetainedAssessment::Stopped(reference))
                if stopped && reference.original.request == request =>
            {
                Some(reference.request_digest)
            }
            Some(RetainedAssessment::Stage(reference))
                if stage && reference.original.request == request =>
            {
                Some(reference.request_digest)
            }
            None => None,
            _ => return Err(ManagementFailure::UnsafeNativeState),
        };
        let progress = if let Some(result) = record.result() {
            decode(result)?
        } else {
            let progress = if stage {
                let NativeStageAccountReply::Progress(progress) = self
                    .stage_native_call(
                        NativeStageAccountRequest::Resolve {
                            expected_key: self.key,
                            expected_pin: self.binding.target().pin(),
                            request,
                        },
                        artifact,
                    )
                    .await?
                else {
                    return Err(ManagementFailure::UnsafeNativeState);
                };
                progress
            } else if stopped {
                self.check_resolution_owner(request)?;
                self.transport
                    .revalidate(self.key, self.binding.target().pin())
                    .await?;
                let progress = self
                    .transport
                    .resolve_stopped(self.key, self.binding.target().pin(), request, artifact)
                    .await?;
                self.transport
                    .revalidate(self.key, self.binding.target().pin())
                    .await?;
                self.check_resolution_owner(request)?;
                progress
            } else {
                let NativeLifecycleAccountReply::Progress(progress) = self
                    .native_call(NativeLifecycleAccountRequest::Resolve {
                        expected_key: self.key,
                        expected_pin: self.binding.target().pin(),
                        request,
                    })
                    .await?
                else {
                    return Err(ManagementFailure::UnsafeNativeState);
                };
                progress
            };
            validate_progress(request, expected_digest, &progress)?;
            validate_outcome(intent.as_ref(), &progress)?;
            if terminal(&progress) {
                self.journal
                    .retain_remote_native_result(
                        self.local_pairing.clone(),
                        self.client,
                        record.request_id(),
                        record.original_scope().to_vec(),
                        encode(&progress)?,
                    )
                    .await
                    .map_err(client_failure)?;
            }
            progress
        };
        validate_progress(request, expected_digest, &progress)?;
        validate_outcome(intent.as_ref(), &progress)?;
        if terminal(&progress) {
            let acknowledged = if stage {
                let NativeStageAccountReply::Acknowledged { progress } = self
                    .stage_native_call(
                        NativeStageAccountRequest::Acknowledge {
                            expected_key: self.key,
                            expected_pin: self.binding.target().pin(),
                            progress: progress.clone(),
                        },
                        None,
                    )
                    .await?
                else {
                    return Err(ManagementFailure::UnsafeNativeState);
                };
                progress
            } else if stopped {
                self.check_acknowledgement(&progress)?;
                self.transport
                    .revalidate(self.key, self.binding.target().pin())
                    .await?;
                let acknowledged = self
                    .transport
                    .acknowledge_stopped(self.key, self.binding.target().pin(), progress.clone())
                    .await?;
                self.transport
                    .revalidate(self.key, self.binding.target().pin())
                    .await?;
                self.check_acknowledgement(&progress)?;
                acknowledged
            } else {
                let NativeLifecycleAccountReply::Acknowledged {
                    progress: acknowledged,
                } = self
                    .native_call(NativeLifecycleAccountRequest::Acknowledge {
                        expected_key: self.key,
                        expected_pin: self.binding.target().pin(),
                        progress: progress.clone(),
                    })
                    .await?
                else {
                    return Err(ManagementFailure::UnsafeNativeState);
                };
                acknowledged
            };
            if acknowledged != progress {
                return Err(ManagementFailure::StaleAssessment);
            }
            self.journal
                .retire_remote_native_request(
                    self.local_pairing.clone(),
                    self.client,
                    record.request_id(),
                    record.original_scope().to_vec(),
                    encode(&progress)?,
                )
                .await
                .map_err(client_failure)?;
        }
        Ok(progress)
    }

    fn check_stage_call_owner(
        &self,
        request: &NativeStageAccountRequest,
    ) -> Result<(), ManagementFailure> {
        match request {
            NativeStageAccountRequest::Resolve { request, .. } => {
                self.check_resolution_owner(*request)
            }
            NativeStageAccountRequest::Acknowledge { progress, .. } => {
                self.check_acknowledgement(progress)
            }
            _ => self.check_owner(),
        }
    }

    async fn stage_native_call(
        &self,
        request: NativeStageAccountRequest,
        artifact: Option<&VerifiedAccountServerArtifact>,
    ) -> Result<NativeStageAccountReply, ManagementFailure> {
        self.check_stage_call_owner(&request)?;
        request.validate().map_err(classify_service)?;
        self.transport
            .revalidate(self.key, self.binding.target().pin())
            .await?;
        let reply = self.transport.stage_call(request.clone(), artifact).await?;
        self.transport
            .revalidate(self.key, self.binding.target().pin())
            .await?;
        self.check_stage_call_owner(&request)?;
        reply.validate().map_err(classify_service)?;
        match reply {
            NativeStageAccountReply::Refused(failure) => Err(classify_service(failure)),
            reply => Ok(reply),
        }
    }

    async fn stage_live(
        &self,
        request: NativeRequestId,
        artifact: &VerifiedAccountServerArtifact,
    ) -> Result<ServiceProgress, ManagementFailure> {
        let record = self.record(request)?;
        let intent = RequestIntent::Stage {
            artifact_sha256: artifact.artifact_sha256(),
            version: artifact.manifest().version.clone(),
        };
        if record.submitted() || record.result().is_some() {
            if record.intent().map(decode).transpose()?.as_ref() != Some(&intent) {
                return Err(ManagementFailure::StaleAssessment);
            }
            // Exact original-package re-supply is resolution, not refreshed
            // confirmation. No Assess, Admit, expiry refresh or new ID is sent.
            return self
                .resolve_remote_with_artifact(request, Some(artifact))
                .await;
        }
        self.retain_intent(&record, intent).await?;
        let NativeStageAccountReply::Captured(original) = self
            .stage_native_call(
                NativeStageAccountRequest::Capture {
                    expected_key: self.key,
                    expected_pin: self.binding.target().pin(),
                    request,
                    artifact_sha256: artifact.artifact_sha256(),
                },
                Some(artifact),
            )
            .await?
        else {
            return Err(ManagementFailure::UnsafeNativeState);
        };
        if original.request != request || original.artifact_sha256 != artifact.artifact_sha256() {
            return Err(ManagementFailure::StaleAssessment);
        }
        let NativeStageAccountReply::Restored {
            original: restored,
            assessment: candidate,
            ..
        } = self
            .stage_native_call(
                NativeStageAccountRequest::Restore {
                    expected_key: self.key,
                    expected_pin: self.binding.target().pin(),
                    request,
                },
                None,
            )
            .await?
        else {
            return Err(ManagementFailure::UnsafeNativeState);
        };
        if restored != original {
            return Err(ManagementFailure::StaleAssessment);
        }
        let reference = if let Some(bytes) = record.assessment() {
            let RetainedAssessment::Stage(reference) = decode(bytes)? else {
                return Err(ManagementFailure::StaleAssessment);
            };
            if candidate.as_ref() != Some(&reference) || reference.original != original {
                return Err(ManagementFailure::StaleAssessment);
            }
            reference
        } else {
            let epoch = self.live_epoch().map_err(|failure| {
                if candidate.is_some() && failure == ManagementFailure::Offline {
                    ManagementFailure::OutcomePending
                } else {
                    failure
                }
            })?;
            let AccountRuntimeReply::NativeStageAssessment(reference) = self
                .runtime_call(AccountRuntimeCall::AssessNativeStage(original), epoch)
                .await?
            else {
                return Err(ManagementFailure::UnsafeNativeState);
            };
            validate_stage_reference(original, &reference)?;
            if reference.epoch != epoch
                || candidate
                    .as_ref()
                    .is_some_and(|original| original != &reference)
            {
                return Err(ManagementFailure::StaleAssessment);
            }
            self.journal
                .retain_remote_native_assessment(
                    self.local_pairing.clone(),
                    self.client,
                    record.request_id(),
                    record.original_scope().to_vec(),
                    encode(&RetainedAssessment::Stage(reference.clone()))?,
                )
                .await
                .map_err(client_failure)?;
            reference
        };
        let epoch = self.live_epoch()?;
        if reference.epoch != epoch {
            return Err(ManagementFailure::StaleAssessment);
        }
        let retained = self.record(request)?;
        self.journal
            .mark_remote_native_submitted(
                self.local_pairing.clone(),
                self.client,
                retained.request_id(),
                retained.original_scope().to_vec(),
                retained.assessment().map(<[u8]>::to_vec),
            )
            .await
            .map_err(client_failure)?;
        let AccountRuntimeReply::NativeStageAccepted(progress) = self
            .runtime_call(
                AccountRuntimeCall::AdmitNativeStage(reference.clone()),
                epoch,
            )
            .await?
        else {
            return Err(ManagementFailure::UnsafeNativeState);
        };
        validate_progress(request, Some(reference.request_digest), &progress)?;
        if progress.phase != ServicePhase::Accepted
            || progress.state != ServiceRequestState::Pending
        {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        self.resolve_remote_with_artifact(request, Some(artifact))
            .await
    }

    fn principal(&self) -> Principal {
        let local = |number: u64| {
            let mut bytes = [0; 16];
            bytes[8..].copy_from_slice(&number.to_be_bytes());
            bytes
        };
        let target = self.binding.target();
        Principal {
            server: target.server().bytes(),
            device: target.device().bytes(),
            pairing: local(target.pairing().local()),
            client: local(target.client().local()),
        }
    }

    async fn stopped_send(
        &self,
        channel: &dyn MessageTransport,
        bytes: &[u8],
    ) -> Result<(), ManagementFailure> {
        if bytes.is_empty() || bytes.len() > spaceterm_host::transport::MAX_ENVELOPE_BYTES {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        self.check_owner()?;
        self.transport
            .revalidate(self.key, self.binding.target().pin())
            .await?;
        channel
            .send(bytes)
            .await
            .map_err(|_| ManagementFailure::IoUnavailable)
    }

    async fn stopped_receive(
        &self,
        channel: &dyn MessageTransport,
    ) -> Result<Vec<u8>, ManagementFailure> {
        self.check_owner()?;
        self.transport
            .revalidate(self.key, self.binding.target().pin())
            .await?;
        let bytes = channel
            .recv()
            .await
            .map_err(|_| ManagementFailure::IoUnavailable)?
            .ok_or(ManagementFailure::IoUnavailable)?;
        if bytes.is_empty() || bytes.len() > spaceterm_host::transport::MAX_ENVELOPE_BYTES {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        self.transport
            .revalidate(self.key, self.binding.target().pin())
            .await?;
        self.check_owner()?;
        Ok(bytes)
    }

    async fn stopped_authenticate(
        &self,
        channel: &dyn MessageTransport,
    ) -> Result<AuthenticatedConnection, ManagementFailure> {
        let ordinary = self
            .runtime
            .restore(&self.local_pairing, self.client)
            .await
            .map_err(pairing_failure)?;
        if ordinary.binding().binding().map_err(client_failure)? != self.binding {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        let signer = ordinary.signing_key().await.map_err(pairing_failure)?;
        let (handshake, hello) = ClientHandshake::begin(
            self.binding.target().pin(),
            self.principal(),
            Purpose::Ordinary,
            HandshakeConfig {
                capabilities: vec![
                    CAPABILITY_ACCOUNT_RUNTIME,
                    CAPABILITY_REMOTE_NATIVE_LIFECYCLE,
                ],
                ..HandshakeConfig::default()
            },
            signer,
        )
        .map_err(|_| ManagementFailure::AuthorizationDenied)?;
        self.stopped_send(channel, &hello).await?;
        let reply = self.stopped_receive(channel).await?;
        let (ready, proof) = handshake
            .receive_server(&reply)
            .map_err(|_| ManagementFailure::AuthorizationDenied)?;
        self.stopped_send(channel, &proof).await?;
        let reply = self.stopped_receive(channel).await?;
        let Established::Ordinary(codec) = ready
            .receive_ready(&reply)
            .map_err(|_| ManagementFailure::AuthorizationDenied)?
        else {
            return Err(ManagementFailure::AuthorizationDenied);
        };
        if codec.principal() != self.principal()
            || !codec.capabilities().contains(&CAPABILITY_ACCOUNT_RUNTIME)
            || !codec
                .capabilities()
                .contains(&CAPABILITY_REMOTE_NATIVE_LIFECYCLE)
        {
            return Err(ManagementFailure::Incompatible);
        }
        Ok(codec)
    }

    async fn stopped_frame(
        &self,
        channel: &dyn MessageTransport,
        codec: &mut AuthenticatedConnection,
        request: u64,
        call: &StoppedNativeCall,
    ) -> Result<StoppedNativeReply, ManagementFailure> {
        let message = common::AccountRuntimeMessage {
            request,
            request_generation: 1,
            body: encode_limited(call, RUNTIME_MESSAGE_LIMIT)?,
        };
        let payload = match codec.major() {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::AccountRuntimeRequest(message)),
            }),
            2 => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::AccountRuntimeRequest(message)),
            }),
            _ => return Err(ManagementFailure::Incompatible),
        };
        let bytes = codec
            .encode(&payload)
            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
        self.stopped_send(channel, &bytes).await?;
        let bytes = self.stopped_receive(channel).await?;
        let response = codec
            .decode(&bytes)
            .map_err(|_| ManagementFailure::AuthorizationDenied)?;
        let message = match response {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::AccountRuntimeReply(message)),
            })
            | Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::AccountRuntimeReply(message)),
            }) => message,
            _ => return Err(ManagementFailure::UnsafeNativeState),
        };
        if message.request != request || message.request_generation != 1 {
            return Err(ManagementFailure::StaleAssessment);
        }
        match decode_limited::<StoppedNativeReply>(&message.body, RUNTIME_MESSAGE_LIMIT)? {
            StoppedNativeReply::Failed(failure) => Err(classify_service(failure)),
            reply => Ok(reply),
        }
    }

    async fn retain_stopped_intent(
        &self,
        record: &RemoteNativeRequest,
        action: StoppedNativeAction,
        observed_generation: NonZeroU64,
        stage_version: Option<BuildVersion>,
    ) -> Result<(), ManagementFailure> {
        let generation = match record.intent().map(decode).transpose()? {
            Some(RequestIntent::Stopped {
                action: original,
                generation,
                stage_version: original_version,
            }) if original == action && original_version == stage_version => generation,
            None => observed_generation,
            _ => return Err(ManagementFailure::StaleAssessment),
        };
        self.retain_intent(
            record,
            RequestIntent::Stopped {
                action,
                generation,
                stage_version,
            },
        )
        .await
    }

    async fn stopped_exchange(
        &self,
        request: NativeRequestId,
        action: StoppedNativeAction,
        artifact: Option<VerifiedAccountServerArtifact>,
        admitted: Option<StoppedNativeAssessmentRef>,
        stage_in_same_channel: bool,
    ) -> Result<StoppedNativeReply, ManagementFailure> {
        self.check_owner()?;
        self.transport
            .revalidate(self.key, self.binding.target().pin())
            .await?;
        let record = self.record(request)?;
        let Some(RequestIntent::Stopped {
            action: retained_action,
            generation,
            ..
        }) = record.intent().map(decode).transpose()?
        else {
            return Err(ManagementFailure::StaleAssessment);
        };
        if retained_action != action {
            return Err(ManagementFailure::StaleAssessment);
        }
        let (capture, channel) = self
            .transport
            .stopped_channel(
                RemoteStoppedRequest {
                    expected_key: self.key,
                    expected_pin: self.binding.target().pin(),
                    principal: self.principal(),
                    registration_generation: generation,
                    request,
                    action,
                },
                artifact,
            )
            .await?;
        self.transport
            .revalidate(self.key, self.binding.target().pin())
            .await?;
        if capture.request != request
            || capture.action != action
            || admitted
                .as_ref()
                .is_some_and(|reference| reference.original != capture)
        {
            let _ = channel.close().await;
            return Err(ManagementFailure::StaleAssessment);
        }
        let result = tokio::time::timeout(STOPPED_EXCHANGE_DEADLINE, async {
            let mut codec = self.stopped_authenticate(channel.as_ref()).await?;
            let epoch = ServerEpoch::new(codec.epoch());
            let call = if let Some(reference) = admitted {
                if reference.epoch != epoch {
                    return Err(ManagementFailure::StaleAssessment);
                }
                StoppedNativeCall::Admit(reference)
            } else {
                StoppedNativeCall::Assess(capture)
            };
            match self
                .stopped_frame(channel.as_ref(), &mut codec, 1, &call)
                .await?
            {
                StoppedNativeReply::Assessment(reference) => {
                    validate_stopped_reference(request, action, &reference)?;
                    if reference.original != capture || reference.epoch != epoch {
                        return Err(ManagementFailure::StaleAssessment);
                    }
                    if stage_in_same_channel {
                        if !matches!(action, StoppedNativeAction::Stage { .. }) {
                            return Err(ManagementFailure::UnsafeNativeState);
                        }
                        let record = self.record(request)?;
                        let encoded = encode(&RetainedAssessment::Stopped(reference.clone()))?;
                        self.journal
                            .retain_remote_native_assessment(
                                self.local_pairing.clone(),
                                self.client,
                                record.request_id(),
                                record.original_scope().to_vec(),
                                encoded.clone(),
                            )
                            .await
                            .map_err(client_failure)?;
                        self.journal
                            .mark_remote_native_submitted(
                                self.local_pairing.clone(),
                                self.client,
                                record.request_id(),
                                record.original_scope().to_vec(),
                                Some(encoded),
                            )
                            .await
                            .map_err(client_failure)?;
                        self.stopped_frame(
                            channel.as_ref(),
                            &mut codec,
                            2,
                            &StoppedNativeCall::Admit(reference),
                        )
                        .await
                    } else {
                        Ok(StoppedNativeReply::Assessment(reference))
                    }
                }
                StoppedNativeReply::Failed(failure) => Err(classify_service(failure)),
                reply => Ok(reply),
            }
        })
        .await
        .unwrap_or(Err(ManagementFailure::OutcomePending));
        // Assess-only close releases frozen custody. An admitted guest worker is
        // separately retained by the owner and cannot be cancelled by this close.
        let _ = channel.close().await;
        result
    }

    async fn stopped_reference(
        &self,
        request: NativeRequestId,
        action: StoppedNativeAction,
    ) -> Result<Option<StoppedNativeAssessmentRef>, ManagementFailure> {
        self.check_owner()?;
        self.transport
            .revalidate(self.key, self.binding.target().pin())
            .await?;
        let reference = self
            .transport
            .restore_stopped(self.key, self.binding.target().pin(), request, action)
            .await?;
        self.transport
            .revalidate(self.key, self.binding.target().pin())
            .await?;
        self.check_owner()?;
        if let Some(reference) = &reference {
            validate_stopped_reference(request, action, reference)?;
        }
        Ok(reference)
    }

    async fn assess_stopped(
        &self,
        request: NativeRequestId,
        action: StoppedNativeAction,
        observed_generation: NonZeroU64,
        artifact: Option<VerifiedAccountServerArtifact>,
    ) -> Result<StoppedNativeAssessmentRef, ManagementFailure> {
        let record = self.record(request)?;
        self.retain_stopped_intent(&record, action, observed_generation, None)
            .await?;
        if let Some(bytes) = record.assessment() {
            let RetainedAssessment::Stopped(reference) = decode(bytes)? else {
                return Err(ManagementFailure::StaleAssessment);
            };
            validate_stopped_reference(request, action, &reference)?;
            if self.stopped_reference(request, action).await?.as_ref() != Some(&reference) {
                return Err(ManagementFailure::StaleAssessment);
            }
            return Ok(reference);
        }
        let restored = self.stopped_reference(request, action).await?;
        let StoppedNativeReply::Assessment(reference) = self
            .stopped_exchange(request, action, artifact, None, false)
            .await?
        else {
            return Err(ManagementFailure::UnsafeNativeState);
        };
        if restored
            .as_ref()
            .is_some_and(|original| original != &reference)
        {
            return Err(ManagementFailure::StaleAssessment);
        }
        self.journal
            .retain_remote_native_assessment(
                self.local_pairing.clone(),
                self.client,
                record.request_id(),
                record.original_scope().to_vec(),
                encode(&RetainedAssessment::Stopped(reference.clone()))?,
            )
            .await
            .map_err(client_failure)?;
        Ok(reference)
    }

    async fn submit_stopped(
        &self,
        reference: StoppedNativeAssessmentRef,
    ) -> Result<ServiceProgress, ManagementFailure> {
        let request = reference.original.request;
        let record = self.record(request)?;
        if record.submitted()
            || record.result().is_some()
            || !matches!(record.intent().map(decode).transpose()?, Some(RequestIntent::Stopped { action, .. }) if action == reference.original.action)
            || record.assessment().map(decode).transpose()?
                != Some(RetainedAssessment::Stopped(reference.clone()))
            || self
                .stopped_reference(request, reference.original.action)
                .await?
                .as_ref()
                != Some(&reference)
        {
            return Err(ManagementFailure::StaleAssessment);
        }
        self.journal
            .mark_remote_native_submitted(
                self.local_pairing.clone(),
                self.client,
                record.request_id(),
                record.original_scope().to_vec(),
                record.assessment().map(<[u8]>::to_vec),
            )
            .await
            .map_err(client_failure)?;
        let StoppedNativeReply::Accepted(progress) = self
            .stopped_exchange(
                request,
                reference.original.action,
                None,
                Some(reference.clone()),
                false,
            )
            .await?
        else {
            return Err(ManagementFailure::UnsafeNativeState);
        };
        validate_progress(request, Some(reference.request_digest), &progress)?;
        if progress.phase != ServicePhase::Accepted
            || progress.state != ServiceRequestState::Pending
        {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        self.resolve_remote(request).await
    }
}

impl HostServiceManagement for RemoteHostServices {
    fn binding(&self) -> &PairedServerBinding {
        &self.binding
    }
    fn inspect(&self) -> ManagementFuture<'_, HostServiceObservation> {
        Box::pin(self.inspect_remote())
    }
    fn reserve_request(&self) -> ManagementFuture<'_, NativeRequestId> {
        Box::pin(self.reserve_remote())
    }
    fn assess(
        &self,
        request: NativeRequestId,
        action: ServiceAction,
    ) -> ManagementFuture<'_, HostServiceAssessment> {
        Box::pin(async move {
            // Native absence, not protocol incompatibility, selects stopped maintenance.
            let observation = self.inspect_remote().await?;
            if observation.status.desired == Some(DesiredServerState::Stopped) {
                match action {
                    ServiceAction::Activate(installation) => stopped_prompt(
                        &self
                            .assess_stopped(
                                request,
                                StoppedNativeAction::Activate(installation),
                                observation.generation,
                                None,
                            )
                            .await?,
                    ),
                    ServiceAction::Uninstall => stopped_prompt(
                        &self
                            .assess_stopped(
                                request,
                                StoppedNativeAction::Uninstall,
                                observation.generation,
                                None,
                            )
                            .await?,
                    ),
                    _ => Err(ManagementFailure::StaleAssessment),
                }
            } else {
                self.assess_live(request, action).await
            }
        })
    }
    fn restore_assessment(
        &self,
        action: ServiceAction,
        reference: AssessmentToken,
    ) -> ManagementFuture<'_, HostServiceAssessment> {
        Box::pin(async move {
            let (request, _) = decode_prompt_token(&reference)?;
            let record = self.record(request)?;
            match decode::<RetainedAssessment>(
                record
                    .assessment()
                    .ok_or(ManagementFailure::StaleAssessment)?,
            )? {
                RetainedAssessment::Live(_) => self.restore_prompt(action, reference).await,
                RetainedAssessment::Stage(_) => Err(ManagementFailure::StaleAssessment),
                RetainedAssessment::Stopped(retained) => {
                    let expected = stopped_prompt(&retained)?;
                    if expected.prompt.action != action
                        || expected.prompt.token != reference
                        || self
                            .stopped_reference(request, retained.original.action)
                            .await?
                            .as_ref()
                            != Some(&retained)
                    {
                        return Err(ManagementFailure::StaleAssessment);
                    }
                    Ok(expected)
                }
            }
        })
    }
    fn submit(&self, original: HostServiceAssessment) -> ManagementFuture<'_, ServiceProgress> {
        Box::pin(async move {
            let record = self.record(original.prompt.request)?;
            match decode::<RetainedAssessment>(
                record
                    .assessment()
                    .ok_or(ManagementFailure::StaleAssessment)?,
            )? {
                RetainedAssessment::Live(_) => self.submit_live(original).await,
                RetainedAssessment::Stage(_) => Err(ManagementFailure::StaleAssessment),
                RetainedAssessment::Stopped(reference) => {
                    let expected = stopped_prompt(&reference)?;
                    if original.prompt.request != expected.prompt.request
                        || original.prompt.action != expected.prompt.action
                        || original.prompt.token != expected.prompt.token
                        || original.prompt.affected_sessions != 0
                        || original.runtime_epoch.is_some()
                    {
                        return Err(ManagementFailure::StaleAssessment);
                    }
                    self.submit_stopped(reference).await
                }
            }
        })
    }
    fn start(&self, request: NativeRequestId) -> ManagementFuture<'_, ServiceProgress> {
        Box::pin(async move {
            self.check_start_compatibility()?;
            let observation = self.inspect_remote().await?;
            if observation.status.desired != Some(DesiredServerState::Stopped) {
                return Err(ManagementFailure::StaleAssessment);
            }
            let reference = self
                .assess_stopped(
                    request,
                    StoppedNativeAction::Start,
                    observation.generation,
                    None,
                )
                .await?;
            self.submit_stopped(reference).await
        })
    }
    fn resolve(&self, request: NativeRequestId) -> ManagementFuture<'_, ServiceProgress> {
        Box::pin(self.resolve_remote(request))
    }
    fn retained_requests(&self) -> ManagementFuture<'_, Vec<ServiceProgress>> {
        Box::pin(async move {
            self.queries
                .validate_recovery_owner(&self.binding, &self.journal)
                .map_err(client_failure)?;
            // Native unconfirmed Reserved captures are retained for foreground
            // restoration, not enumerated as already admitted recovery work.
            let originals = self
                .records()?
                .into_iter()
                .filter(|record| record.submitted() || record.result().is_some());
            let mut progress = Vec::new();
            for record in originals {
                let request = NativeRequestId::new(record.request_id())
                    .map_err(|_| ManagementFailure::UnsafeNativeState)?;
                progress.push(self.resolve_remote(request).await?);
            }
            Ok(progress)
        })
    }
    fn stage(
        &self,
        request: NativeRequestId,
        artifact: VerifiedAccountServerArtifact,
    ) -> ManagementFuture<'_, ServiceProgress> {
        Box::pin(async move {
            let record = self.record(request)?;
            match record.intent().map(decode).transpose()? {
                Some(RequestIntent::Stage { .. }) => {
                    return self.stage_live(request, &artifact).await;
                }
                Some(RequestIntent::Stopped {
                    action: StoppedNativeAction::Stage { artifact_sha256 },
                    stage_version,
                    ..
                }) if record.submitted() || record.result().is_some() => {
                    if artifact_sha256 != artifact.artifact_sha256()
                        || stage_version.as_ref() != Some(&artifact.manifest().version)
                    {
                        return Err(ManagementFailure::StaleAssessment);
                    }
                    return self
                        .resolve_remote_with_artifact(request, Some(&artifact))
                        .await;
                }
                Some(RequestIntent::Stopped {
                    action: StoppedNativeAction::Stage { .. },
                    ..
                })
                | None => {}
                _ => return Err(ManagementFailure::StaleAssessment),
            }
            let observation = self.inspect_remote().await?;
            if observation.status.desired == Some(DesiredServerState::Running) {
                return self.stage_live(request, &artifact).await;
            }
            if observation.status.desired != Some(DesiredServerState::Stopped) {
                return Err(ManagementFailure::StaleAssessment);
            }
            let action = StoppedNativeAction::Stage {
                artifact_sha256: artifact.artifact_sha256(),
            };
            let record = self.record(request)?;
            self.retain_stopped_intent(
                &record,
                action,
                observation.generation,
                Some(artifact.manifest().version.clone()),
            )
            .await?;
            let StoppedNativeReply::Accepted(progress) = self
                .stopped_exchange(request, action, Some(artifact), None, true)
                .await?
            else {
                return Err(ManagementFailure::UnsafeNativeState);
            };
            let record = self.record(request)?;
            let RetainedAssessment::Stopped(reference) = decode(
                record
                    .assessment()
                    .ok_or(ManagementFailure::UnsafeNativeState)?,
            )?
            else {
                return Err(ManagementFailure::UnsafeNativeState);
            };
            validate_progress(request, Some(reference.request_digest), &progress)?;
            if progress.phase != ServicePhase::Accepted
                || progress.state != ServiceRequestState::Pending
            {
                return Err(ManagementFailure::UnsafeNativeState);
            }
            self.resolve_remote(request).await
        })
    }
}

fn validate_stopped_reference(
    request: NativeRequestId,
    action: StoppedNativeAction,
    reference: &StoppedNativeAssessmentRef,
) -> Result<(), ManagementFailure> {
    if reference.original.request != request
        || reference.original.action != action
        || reference.affected_sessions != 0
        || reference.original.capture_digest.bytes() == [0; 32]
        || reference.request_digest.bytes() == [0; 32]
        || reference.proof_digest.bytes() == [0; 32]
        || reference.epoch.get() == 0
        || reference.expires_at.boot == [0; 32]
        || reference.expires_at.millis == 0
    {
        return Err(ManagementFailure::StaleAssessment);
    }
    Ok(())
}
fn stopped_prompt(
    reference: &StoppedNativeAssessmentRef,
) -> Result<HostServiceAssessment, ManagementFailure> {
    validate_stopped_reference(
        reference.original.request,
        reference.original.action,
        reference,
    )?;
    let action = match reference.original.action {
        StoppedNativeAction::Activate(installation) => ServiceAction::Activate(installation),
        StoppedNativeAction::Uninstall => ServiceAction::Uninstall,
        _ => return Err(ManagementFailure::StaleAssessment),
    };
    let digest = ring::digest::digest(&ring::digest::SHA256, &encode(reference)?);
    Ok(HostServiceAssessment {
        prompt: NativeActionPrompt {
            request: reference.original.request,
            action,
            affected_sessions: 0,
            token: AssessmentToken::new(format!(
                "remote_native_{}_{}",
                crate::cli::output::hex(reference.original.request.as_bytes()),
                crate::cli::output::hex(digest.as_ref())
            ))?,
        },
        runtime_epoch: None,
    })
}

use AccountRuntimeReply::NativeLifecycleAssessment;

fn validate_stage_reference(
    original: NativeStageRequest,
    reference: &NativeStageAssessmentRef,
) -> Result<(), ManagementFailure> {
    if reference.original != original
        || original.artifact_sha256 == [0; 32]
        || original.capture_digest.bytes() == [0; 32]
        || reference.request_digest.bytes() == [0; 32]
        || reference.proof_digest.bytes() == [0; 32]
        || reference.epoch.get() == 0
        || reference.expires_at.boot == [0; 32]
        || reference.expires_at.millis == 0
    {
        return Err(ManagementFailure::StaleAssessment);
    }
    Ok(())
}

fn validate_reference(
    original: NativeLifecycleRequest,
    reference: &NativeLifecycleAssessmentRef,
) -> Result<(), ManagementFailure> {
    if reference.original != original
        || original.capture_digest.bytes() == [0; 32]
        || reference.request_digest.bytes() == [0; 32]
        || reference.proof_digest.bytes() == [0; 32]
        || reference.epoch.get() == 0
        || reference.expires_at.boot == [0; 32]
        || reference.expires_at.millis == 0
    {
        return Err(ManagementFailure::StaleAssessment);
    }
    Ok(())
}

fn validate_progress(
    request: NativeRequestId,
    digest: Option<RequestDigest>,
    progress: &ServiceProgress,
) -> Result<(), ManagementFailure> {
    if progress.request != request
        || progress.request_digest.bytes() == [0; 32]
        || digest.is_some_and(|digest| progress.request_digest != digest)
        || (matches!(progress.state, ServiceRequestState::Completed(_))
            && progress.phase != ServicePhase::Completed)
        || (matches!(progress.state, ServiceRequestState::NotApplied(_))
            && !matches!(
                progress.phase,
                ServicePhase::Reserved | ServicePhase::Completed
            ))
        || (progress.phase == ServicePhase::Completed
            && matches!(progress.state, ServiceRequestState::Pending))
    {
        return Err(ManagementFailure::StaleAssessment);
    }
    Ok(())
}

fn validate_outcome(
    intent: Option<&RequestIntent>,
    progress: &ServiceProgress,
) -> Result<(), ManagementFailure> {
    let ServiceRequestState::Completed(outcome) = &progress.state else {
        return Ok(());
    };
    let matches = match (intent, outcome) {
        (Some(RequestIntent::Lifecycle(ServiceAction::Stop)), ServiceOutcome::Stopped)
        | (Some(RequestIntent::Lifecycle(ServiceAction::Restart)), ServiceOutcome::Restarted)
        | (Some(RequestIntent::Lifecycle(ServiceAction::Uninstall)), ServiceOutcome::Uninstalled)
        | (
            Some(RequestIntent::Stopped {
                action: StoppedNativeAction::Start,
                ..
            }),
            ServiceOutcome::Started,
        )
        | (
            Some(RequestIntent::Stopped {
                action: StoppedNativeAction::Uninstall,
                ..
            }),
            ServiceOutcome::Uninstalled,
        ) => true,
        (
            Some(RequestIntent::Lifecycle(ServiceAction::Activate(installation))),
            ServiceOutcome::Activated(status),
        )
        | (
            Some(RequestIntent::Stopped {
                action: StoppedNativeAction::Activate(installation),
                ..
            }),
            ServiceOutcome::Activated(status),
        ) => *installation == status.installation,
        (
            Some(RequestIntent::Stopped {
                action: StoppedNativeAction::Stage { artifact_sha256 },
                stage_version,
                ..
            }),
            ServiceOutcome::Staged(status),
        ) => {
            *artifact_sha256 == status.artifact_sha256
                && stage_version.as_ref() == Some(&status.version)
        }
        (
            Some(RequestIntent::Stage {
                artifact_sha256,
                version,
            }),
            ServiceOutcome::Staged(status),
        ) => *artifact_sha256 == status.artifact_sha256 && version == &status.version,
        _ => false,
    };
    if !matches {
        return Err(ManagementFailure::StaleAssessment);
    }
    Ok(())
}

fn terminal(progress: &ServiceProgress) -> bool {
    progress.phase == ServicePhase::Completed
        && matches!(
            progress.state,
            ServiceRequestState::Completed(_) | ServiceRequestState::NotApplied(_)
        )
}

fn prompt(
    reference: &NativeLifecycleAssessmentRef,
) -> Result<HostServiceAssessment, ManagementFailure> {
    Ok(HostServiceAssessment {
        prompt: NativeActionPrompt {
            request: reference.original.request,
            action: reference.original.action,
            affected_sessions: reference.affected_sessions,
            token: prompt_token(reference)?,
        },
        runtime_epoch: Some(reference.epoch),
    })
}

fn prompt_token(
    reference: &NativeLifecycleAssessmentRef,
) -> Result<AssessmentToken, ManagementFailure> {
    // Hash the closed public reference, not a native secret or full proof. The
    // original request ID supports bounded canonical lookup after process restart.
    let digest = ring::digest::digest(&ring::digest::SHA256, &encode(reference)?);
    AssessmentToken::new(format!(
        "remote_native_{}_{}",
        crate::cli::output::hex(reference.original.request.as_bytes()),
        crate::cli::output::hex(digest.as_ref()),
    ))
}

fn decode_prompt_token(
    token: &AssessmentToken,
) -> Result<(NativeRequestId, [u8; 32]), ManagementFailure> {
    let (request, digest) = token
        .as_str()
        .strip_prefix("remote_native_")
        .and_then(|body| body.split_once('_'))
        .ok_or(ManagementFailure::StaleAssessment)?;
    let request = crate::cli::arguments::parse_hex(request)
        .map_err(|_| ManagementFailure::StaleAssessment)?;
    let digest =
        crate::cli::arguments::parse_hex(digest).map_err(|_| ManagementFailure::StaleAssessment)?;
    Ok((
        NativeRequestId::new(request).map_err(|_| ManagementFailure::StaleAssessment)?,
        digest,
    ))
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, ManagementFailure> {
    encode_limited(value, MAX_REMOTE_NATIVE_BYTES)
}
fn encode_limited<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, ManagementFailure> {
    let bytes = serde_json::to_vec(value).map_err(|_| ManagementFailure::UnsafeNativeState)?;
    if bytes.is_empty() || bytes.len() > limit {
        return Err(ManagementFailure::LimitExceeded);
    }
    Ok(bytes)
}
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ManagementFailure> {
    decode_limited(bytes, MAX_REMOTE_NATIVE_BYTES)
}
fn decode_limited<T: DeserializeOwned>(bytes: &[u8], limit: usize) -> Result<T, ManagementFailure> {
    if bytes.is_empty() || bytes.len() > limit {
        return Err(ManagementFailure::LimitExceeded);
    }
    serde_json::from_slice(bytes).map_err(|_| ManagementFailure::UnsafeNativeState)
}
fn client_failure(failure: ClientFailure) -> ManagementFailure {
    match failure.class() {
        ClientFailureClass::Unavailable => ManagementFailure::Offline,
        ClientFailureClass::AuthorizationDenied => ManagementFailure::Revoked,
        ClientFailureClass::StaleAuthority | ClientFailureClass::TargetNotFound => {
            ManagementFailure::StaleAssessment
        }
        ClientFailureClass::Incompatible | ClientFailureClass::MissingCapability => {
            ManagementFailure::Incompatible
        }
        ClientFailureClass::ResourceLimit | ClientFailureClass::CounterExhausted => {
            ManagementFailure::LimitExceeded
        }
        ClientFailureClass::StorageUnavailable => ManagementFailure::StorageUnavailable,
        ClientFailureClass::Cancelled => ManagementFailure::AuthorizationDenied,
        _ => ManagementFailure::UnsafeNativeState,
    }
}
fn pairing_failure(failure: PairingFailure) -> ManagementFailure {
    match failure {
        PairingFailure::PairingRevoked => ManagementFailure::Revoked,
        PairingFailure::ClientNotBound => ManagementFailure::PairingRequired,
        PairingFailure::CredentialUnavailable => ManagementFailure::CredentialUnavailable,
        _ => ManagementFailure::AuthorizationDenied,
    }
}
