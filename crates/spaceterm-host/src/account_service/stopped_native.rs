//! Request-bound native maintenance while deliberate Stop remains in force.
//! The stopped lease is not owner enrollment authority. Server composition must
//! authenticate the initiating saved Client against the frozen established store.

use std::{fmt, sync::Arc};

use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use spaceterm_core::{
    authority::{FieldVersion, ServerEpoch},
    identity::{ClientId, DeviceId, PairingId},
    operations::RequestDigest,
    pairing::policy::PairingPrincipal,
};
use spaceterm_distribution::VerifiedAccountServerArtifact;
use spaceterm_protocol::authentication::SigningKey;

use super::{
    RegisteredServer, ServiceAction, ServiceFailure, ServicePhase, ServiceProgress,
    ServiceRequestState, ServiceSupervisor, ServiceTime, SupervisorState,
    install::InstallRecord,
    supervisor::{
        AssessmentRecord, JOURNAL_LIMIT, RegistrationBasis, RequestBinding, RequestRecord, encode,
    },
};
use crate::{
    native_deployment::contracts::NativeRequestId,
    private_filesystem::{Directory, Lease},
    startup::{AccountBuildKey, DesiredServerState, InstallationId},
};

mod producer;
mod wire;
pub use wire::{
    STOPPED_NATIVE_WIRE_LIMIT, StoppedNativeAccountReply, StoppedNativeAccountRequest,
    StoppedNativeArtifactTransfer, StoppedNativeChannelRequest,
};

const DOMAIN: &[u8] = b"spaceterm-account-stopped-native-origin-v1\0";

/// Exact stopped operation. Staging selects an authenticated archive, not code
/// approval; activation names the already retained exact installation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum StoppedNativeAction {
    /// Explicitly restores Running intent and starts approved code.
    Start,
    /// Retains this exact signed archive without changing Stopped intent.
    Stage {
        /// Archive identity from VerifiedAccountServerArtifact.
        artifact_sha256: [u8; 32],
    },
    /// Approves the selected staged installation without starting it.
    Activate(InstallationId),
    /// Removes this service's verified code and configuration, retaining data.
    Uninstall,
}

/// Original guest-issued comparison, captured before the paired handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoppedNativeRequest {
    /// Original durably reserved native identifier.
    pub request: NativeRequestId,
    /// Exact action and artifact or installation selection.
    pub action: StoppedNativeAction,
    /// Binds account/build, registration, action, issue time and original expiry.
    pub capture_digest: RequestDigest,
}

/// Public comparison to the retained signed original. It grants no admission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoppedNativeAssessmentRef {
    /// Exact original capture.
    pub original: StoppedNativeRequest,
    /// Existing supervisor's exact execution digest.
    pub request_digest: RequestDigest,
    /// Digest of the retained paired-origin signature and claims.
    pub proof_digest: RequestDigest,
    /// Frozen canonical store epoch. No runtime epoch is allocated.
    pub epoch: ServerEpoch,
    /// Always zero after confirmed stopped/native descendant absence.
    pub affected_sessions: u32,
    /// Original cross-process monotonic expiry, never extended by reconnection.
    pub expires_at: ServiceTime,
}

/// Closed comparison grammar carried only inside ordinary paired envelopes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum StoppedNativeCall {
    /// Assesses or restores this exact original without accepting host effects.
    Assess(StoppedNativeRequest),
    /// Explicitly admits the exact retained assessment after confirmation.
    Admit(StoppedNativeAssessmentRef),
}

/// Bounded public replies contain no paths, keys, raw errors or native authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum StoppedNativeReply {
    /// Exact retained paired-origin assessment.
    Assessment(StoppedNativeAssessmentRef),
    /// Original durable acceptance. The native owner completes subsequent effects.
    Accepted(ServiceProgress),
    /// Fixed content-free classification.
    Failed(ServiceFailure),
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    action: StoppedNativeAction,
    basis: RegistrationBasis,
    issued: ServiceTime,
    token: [u8; 32],
}
impl Intent {
    fn expires_at(&self) -> Result<ServiceTime, ServiceFailure> {
        Ok(ServiceTime {
            boot: self.issued.boot,
            millis: self
                .issued
                .millis
                .checked_add(30_000)
                .ok_or(ServiceFailure::LimitExceeded)?,
        })
    }
    fn check_time(&self, now: ServiceTime) -> Result<(), ServiceFailure> {
        if now.boot != self.issued.boot
            || now.millis < self.issued.millis
            || now.millis >= self.expires_at()?.millis
        {
            return Err(ServiceFailure::StaleAssessment);
        }
        Ok(())
    }
    fn validate(&self) -> Result<(), ServiceFailure> {
        self.basis.validate()?;
        if self.basis.desired != DesiredServerState::Stopped
            || !self.basis.installed
            || self.issued.boot == [0; 32]
            || self.token == [0; 32]
            || matches!(self.action, StoppedNativeAction::Stage { artifact_sha256 } if artifact_sha256 == [0; 32])
            || matches!(self.action, StoppedNativeAction::Activate(id) if self.basis.staged.as_ref().is_none_or(|status| status.installation != id))
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        self.expires_at().map(|_| ())
    }
    fn capture(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
    ) -> Result<StoppedNativeRequest, ServiceFailure> {
        let mut hash = Sha256::new();
        hash.update(b"spaceterm-account-stopped-native-capture-v1\0");
        hash.update(encode(&(key, request, self), JOURNAL_LIMIT)?);
        Ok(StoppedNativeRequest {
            request,
            action: self.action,
            capture_digest: RequestDigest::from_bytes(hash.finalize().into()),
        })
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    device: DeviceId,
    pairing: PairingId,
    client: ClientId,
    authorization_version: FieldVersion,
    epoch: ServerEpoch,
}
impl Claims {
    fn from_authorization(
        principal: PairingPrincipal,
        version: FieldVersion,
        epoch: ServerEpoch,
    ) -> Self {
        Self {
            device: principal.device(),
            pairing: principal.pairing(),
            client: principal.client(),
            authorization_version: version,
            epoch,
        }
    }
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Origin {
    claims: Claims,
    signature: Vec<u8>,
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
enum Execution {
    Service(RequestRecord),
    Stage(InstallRecord),
}
impl Execution {
    fn progress(&self, request: NativeRequestId) -> ServiceProgress {
        match self {
            Self::Service(record) => record.progress(request),
            Self::Stage(record) => record.progress(request),
        }
    }
}

/// Retained in the existing native request journal. Wire decoding cannot obtain
/// an admission lease, a core PairingPrincipal, or the original server signer.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoppedNativeRecord {
    intent: Intent,
    execution: Option<Execution>,
    origin: Option<Origin>,
    accepted: bool,
}
impl StoppedNativeRecord {
    fn check_staged_candidate(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
    ) -> Result<(), ServiceFailure> {
        let StoppedNativeAction::Stage { artifact_sha256 } = self.intent.action else {
            return Ok(());
        };
        let Some(staged) = &self.intent.basis.staged else {
            return Ok(());
        };
        if staged.artifact_sha256 != artifact_sha256 {
            return Err(ServiceFailure::RequestConflict);
        }
        if let Some(Execution::Stage(execution)) = &self.execution {
            let candidate = InstallRecord::stage(
                key,
                request,
                artifact_sha256,
                staged.version.clone(),
                self.intent.basis.clone(),
            )?;
            if candidate.progress(request).request_digest
                != execution.progress(request).request_digest
            {
                return Err(ServiceFailure::RequestConflict);
            }
        }
        Ok(())
    }
    fn transcript(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
        origin: &Origin,
    ) -> Result<Vec<u8>, ServiceFailure> {
        let execution = self
            .execution
            .as_ref()
            .ok_or(ServiceFailure::InvalidRecord)?;
        let mut bytes = DOMAIN.to_vec();
        bytes.extend(encode(
            &(
                key,
                self.intent.capture(key, request)?,
                execution.progress(request).request_digest,
                &origin.claims,
            ),
            JOURNAL_LIMIT,
        )?);
        Ok(bytes)
    }
    pub(super) fn validate(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
        service: Option<&RequestRecord>,
        install: Option<&InstallRecord>,
    ) -> Result<(), ServiceFailure> {
        self.intent.validate()?;
        let (Some(execution), Some(origin)) = (&self.execution, &self.origin) else {
            return if self.execution.is_none()
                && self.origin.is_none()
                && !self.accepted
                && service.is_none()
                && install.is_none()
            {
                Ok(())
            } else {
                Err(ServiceFailure::InvalidRecord)
            };
        };
        let server = spaceterm_core::identity::ServerId::new(self.intent.basis.server);
        if origin.claims.device.bytes() == [0; 16]
            || origin.claims.pairing.server() != server
            || origin.claims.client.server() != server
            || origin.claims.authorization_version.get() == 0
            || origin.claims.epoch.get() == 0
            || origin.signature.len() != 64
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        UnparsedPublicKey::new(&ED25519, self.intent.basis.public_key)
            .verify(&self.transcript(key, request, origin)?, &origin.signature)
            .map_err(|_| ServiceFailure::InvalidRecord)?;
        match (&self.intent.action, execution) {
            (StoppedNativeAction::Start, Execution::Service(record))
                if record.binding == RequestBinding::Start(self.intent.basis.clone()) => {}
            (
                action @ (StoppedNativeAction::Activate(_) | StoppedNativeAction::Uninstall),
                Execution::Service(record),
            ) => {
                let RequestBinding::Confirmed(assessment) = &record.binding else {
                    return Err(ServiceFailure::InvalidRecord);
                };
                let expected = match action {
                    StoppedNativeAction::Activate(id) => ServiceAction::Activate(*id),
                    StoppedNativeAction::Uninstall => ServiceAction::Uninstall,
                    _ => return Err(ServiceFailure::InvalidRecord),
                };
                if assessment.action != expected
                    || assessment.basis != self.intent.basis
                    || assessment.issued != self.intent.issued
                    || assessment.token != self.intent.token
                    || assessment.runtime.epoch.is_some()
                    || !assessment.runtime.sessions.is_empty()
                {
                    return Err(ServiceFailure::InvalidRecord);
                }
            }
            (StoppedNativeAction::Stage { artifact_sha256 }, Execution::Stage(record))
                if record.matches_stopped_stage(&self.intent.basis, *artifact_sha256) => {}
            _ => return Err(ServiceFailure::InvalidRecord),
        }
        let progress = execution.progress(request);
        if progress.phase != ServicePhase::Accepted
            || progress.state != ServiceRequestState::Pending
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        match execution {
            Execution::Service(record) => {
                if record.runtime_origin.is_some() {
                    return Err(ServiceFailure::InvalidRecord);
                }
                record.validate(key, request)?;
            }
            Execution::Stage(record) => {
                if record.installation.is_some() {
                    return Err(ServiceFailure::InvalidRecord);
                }
                record.validate(key, request)?;
            }
        }
        if !self.accepted {
            if service.is_some() || install.is_some() {
                return Err(ServiceFailure::InvalidRecord);
            }
        } else {
            match (execution, service, install) {
                (Execution::Service(original), Some(current), None)
                    if original.binding == current.binding
                        && original.digest == current.digest
                        && current.runtime_origin.is_none()
                        && current.phase != ServicePhase::Reserved => {}
                (Execution::Stage(original), None, Some(current))
                    if original.progress(request).request_digest
                        == current.progress(request).request_digest => {}
                _ => return Err(ServiceFailure::InvalidRecord),
            }
        }
        Ok(())
    }
    pub(super) fn acknowledgeable(
        &self,
        request: NativeRequestId,
        service: Option<&RequestRecord>,
        install: Option<&InstallRecord>,
    ) -> bool {
        if !self.accepted {
            return false;
        }
        match (&self.execution, service, install) {
            (Some(Execution::Service(original)), Some(current), None) => {
                current.phase == ServicePhase::Completed
                    && current.digest == original.digest
                    && current.binding == original.binding
            }
            (Some(Execution::Stage(original)), None, Some(current)) => {
                current.phase == ServicePhase::Completed
                    && current.progress(request).request_digest
                        == original.progress(request).request_digest
            }
            _ => false,
        }
    }
    fn reference(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
    ) -> Result<StoppedNativeAssessmentRef, ServiceFailure> {
        let origin = self.origin.as_ref().ok_or(ServiceFailure::Pending)?;
        let execution = self.execution.as_ref().ok_or(ServiceFailure::Pending)?;
        Ok(StoppedNativeAssessmentRef {
            original: self.intent.capture(key, request)?,
            request_digest: execution.progress(request).request_digest,
            proof_digest: RequestDigest::from_bytes(Sha256::digest(encode(origin, 4096)?).into()),
            epoch: origin.claims.epoch,
            affected_sessions: 0,
            expires_at: self.intent.expires_at()?,
        })
    }
}

/// Immutable stopped custody excludes startup and every coordinated native
/// mutation while the established database and paired handshake are in use.
/// It must be dropped, with the frozen store, before executing an admitted Start.
pub struct StoppedNativeAdmission {
    supervisor: ServiceSupervisor,
    registration: RegisteredServer,
    capture: StoppedNativeRequest,
    lifecycle: Lease,
    instance: Lease,
}
impl fmt::Debug for StoppedNativeAdmission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoppedNativeAdmission(..)")
    }
}
impl StoppedNativeAdmission {
    /// Established registration whose paths stay native and never enter replies.
    pub fn registration(&self) -> &RegisteredServer {
        &self.registration
    }
    /// Exact original capture retained before authentication.
    pub fn original(&self) -> StoppedNativeRequest {
        self.capture
    }
    /// Rechecks immutable registration, original expiry, signed descendant
    /// custody, actual manager scope, and registered listener absence.
    pub async fn revalidate(&self) -> Result<(), ServiceFailure> {
        self.lifecycle.verify()?;
        self.instance.verify()?;
        let record = self.supervisor.stopped_record(self.capture)?;
        record.intent.check_time(self.supervisor.clock.now()?)?;
        if record.accepted {
            return Err(ServiceFailure::Pending);
        }
        let current = self.supervisor.discover()?.ok_or(ServiceFailure::Missing)?;
        if !record.intent.basis.matches(&current)
            || current.record.data != self.registration.record.data
            || current.record.organization != self.registration.record.organization
            || current.record.runtime != self.registration.record.runtime
            || current.key() != self.registration.key()
        {
            return Err(ServiceFailure::StaleAssessment);
        }
        self.supervisor.check_startup_barrier(&current.record)?;
        self.supervisor.processes()?.verify_absent(&current).await?;
        if !matches!(
            self.supervisor.manager()?.status(Some(&current)).await?,
            SupervisorState::Inactive | SupervisorState::Disabled | SupervisorState::NotInstalled
        ) {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        self.lifecycle.verify()?;
        self.instance.verify()?;
        let current = self.supervisor.discover()?.ok_or(ServiceFailure::Missing)?;
        if !record.intent.basis.matches(&current) {
            return Err(ServiceFailure::StaleAssessment);
        }
        record.intent.check_time(self.supervisor.clock.now()?)?;
        Ok(())
    }
    /// The server supplies only a principal derived from the actual ordinary
    /// handshake and current canonical frozen PairingAuthorization. This native
    /// API cannot be reached by deserializing comparison DTOs.
    pub async fn assess(
        &self,
        principal: PairingPrincipal,
        authorization_version: FieldVersion,
        epoch: ServerEpoch,
        signer: Arc<dyn SigningKey>,
        artifact: Option<&VerifiedAccountServerArtifact>,
    ) -> Result<StoppedNativeAssessmentRef, ServiceFailure> {
        self.revalidate().await?;
        let mut record = self.supervisor.stopped_record(self.capture)?;
        let claims = Claims::from_authorization(principal, authorization_version, epoch);
        if principal.server().bytes() != self.registration.server_pin().identity
            || signer.public_key() != self.registration.server_pin().public_key
            || authorization_version.get() == 0
            || epoch.get() == 0
        {
            return Err(ServiceFailure::PairingRequired);
        }
        record.check_staged_candidate(self.supervisor.key, self.capture.request)?;
        if let Some(origin) = &record.origin {
            if origin.claims != claims {
                return Err(ServiceFailure::PairingRequired);
            }
            return record.reference(self.supervisor.key, self.capture.request);
        }
        let execution = match record.intent.action {
            StoppedNativeAction::Start => {
                self.supervisor
                    .manager()?
                    .verify_installation(&self.registration)?;
                let binding = RequestBinding::Start(record.intent.basis.clone());
                Execution::Service(RequestRecord {
                    digest: binding.digest(self.supervisor.key, self.capture.request)?,
                    binding,
                    phase: ServicePhase::Accepted,
                    state: ServiceRequestState::Pending,
                    reserving: false,
                    aborting: false,
                    runtime_origin: None,
                })
            }
            action @ (StoppedNativeAction::Activate(_) | StoppedNativeAction::Uninstall) => {
                let runtime = self
                    .supervisor
                    .processes()?
                    .assess(&self.registration)
                    .await?;
                if runtime.epoch.is_some() || !runtime.sessions.is_empty() {
                    return Err(ServiceFailure::CleanupUnresolved);
                }
                let action = match action {
                    StoppedNativeAction::Activate(id) => ServiceAction::Activate(id),
                    StoppedNativeAction::Uninstall => ServiceAction::Uninstall,
                    _ => return Err(ServiceFailure::InvalidRecord),
                };
                let binding = RequestBinding::Confirmed(AssessmentRecord {
                    action,
                    basis: record.intent.basis.clone(),
                    runtime,
                    issued: record.intent.issued,
                    token: record.intent.token,
                });
                self.supervisor
                    .verify_execution_selection(&binding, &self.registration)?;
                Execution::Service(RequestRecord {
                    digest: binding.digest(self.supervisor.key, self.capture.request)?,
                    binding,
                    phase: ServicePhase::Accepted,
                    state: ServiceRequestState::Pending,
                    reserving: false,
                    aborting: false,
                    runtime_origin: None,
                })
            }
            StoppedNativeAction::Stage { artifact_sha256 } => {
                let artifact = artifact.ok_or(ServiceFailure::ArtifactRejected)?;
                self.supervisor.check_stopped_artifact(artifact)?;
                if artifact.artifact_sha256() != artifact_sha256 {
                    return Err(ServiceFailure::RequestConflict);
                }
                Execution::Stage(InstallRecord::stopped_stage(
                    self.supervisor.key,
                    self.capture.request,
                    artifact,
                    record.intent.basis.clone(),
                )?)
            }
        };
        record.execution = Some(execution);
        record.check_staged_candidate(self.supervisor.key, self.capture.request)?;
        let mut origin = Origin {
            claims,
            signature: Vec::new(),
        };
        origin.signature = signer
            .sign(&record.transcript(self.supervisor.key, self.capture.request, &origin)?)
            .map_err(|_| ServiceFailure::StorageUnavailable)?
            .to_vec();
        record.origin = Some(origin);
        self.revalidate().await?;
        self.supervisor
            .mutate_request(self.capture.request, |journal, index| {
                let slot = &mut journal.reservations[index];
                let current = slot
                    .stopped_native
                    .as_ref()
                    .ok_or(ServiceFailure::RequestConflict)?;
                if current.intent != record.intent || current.origin.is_some() || current.accepted {
                    return Err(ServiceFailure::RequestConflict);
                }
                slot.stopped_native = Some(record.clone());
                Ok(())
            })?;
        record.reference(self.supervisor.key, self.capture.request)
    }
    /// Durably admits only the exact paired original. No manager or artifact
    /// publication effects run while the frozen database/instance lease is held.
    pub async fn accept(
        &self,
        reference: &StoppedNativeAssessmentRef,
        principal: PairingPrincipal,
        authorization_version: FieldVersion,
        epoch: ServerEpoch,
    ) -> Result<ServiceProgress, ServiceFailure> {
        self.revalidate().await?;
        let mut record = self.supervisor.stopped_record(self.capture)?;
        if record.reference(self.supervisor.key, self.capture.request)? != *reference
            || record.origin.as_ref().is_none_or(|origin| {
                origin.claims != Claims::from_authorization(principal, authorization_version, epoch)
            })
        {
            return Err(ServiceFailure::PairingRequired);
        }
        record.check_staged_candidate(self.supervisor.key, self.capture.request)?;
        let execution = record
            .execution
            .as_ref()
            .ok_or(ServiceFailure::Pending)?
            .clone();
        record.accepted = true;
        self.lifecycle.verify()?;
        self.instance.verify()?;
        self.supervisor
            .mutate_request(self.capture.request, |journal, index| {
                record.intent.check_time(self.supervisor.clock.now()?)?;
                let slot = &mut journal.reservations[index];
                let current = slot
                    .stopped_native
                    .as_ref()
                    .ok_or(ServiceFailure::RequestConflict)?;
                if slot.acknowledged
                    || current.accepted
                    || current.intent != record.intent
                    || current.execution != record.execution
                    || current.origin != record.origin
                    || slot.service.is_some()
                    || slot.install.is_some()
                    || slot.owner.is_some()
                {
                    return Err(ServiceFailure::RequestConflict);
                }
                slot.stopped_native = Some(record.clone());
                match &execution {
                    Execution::Service(original) => slot.service = Some(original.clone()),
                    Execution::Stage(original) => slot.install = Some(original.clone()),
                }
                Ok(())
            })?;
        Ok(execution.progress(self.capture.request))
    }
}

impl ServiceSupervisor {
    /// Captures a previously reserved original under exact retained Stopped
    /// intent. Repetition restores its original issue time, never a fresh window.
    pub fn capture_stopped_native(
        &self,
        request: NativeRequestId,
        action: StoppedNativeAction,
    ) -> Result<StoppedNativeRequest, ServiceFailure> {
        self.capture_stopped_native_inner(request, action, None)
    }
    /// Checks the initiating Client's exact observed registration under the same
    /// native lease that captures the original approved/staged selection. A
    /// separate inspection followed by generic Start cannot supply this guarantee.
    pub fn capture_stopped_native_expected(
        &self,
        request: NativeRequestId,
        action: StoppedNativeAction,
        key: AccountBuildKey,
        pin: spaceterm_protocol::authentication::ServerPin,
        generation: u64,
    ) -> Result<StoppedNativeRequest, ServiceFailure> {
        self.capture_stopped_native_inner(request, action, Some((key, pin, generation)))
    }
    fn capture_stopped_native_inner(
        &self,
        request: NativeRequestId,
        action: StoppedNativeAction,
        expected: Option<(
            AccountBuildKey,
            spaceterm_protocol::authentication::ServerPin,
            u64,
        )>,
    ) -> Result<StoppedNativeRequest, ServiceFailure> {
        let lifecycle = self.lifecycle_lease()?;
        let (_, _, journal) = self.read_journal()?;
        let slot = &journal.reservations[journal.reservation(request)?];
        if slot.acknowledged {
            return Err(ServiceFailure::RequestRetired);
        }
        if let Some(record) = &slot.stopped_native {
            if expected.is_some_and(|(key, pin, generation)| {
                key != self.key
                    || generation != record.intent.basis.generation
                    || pin.identity != record.intent.basis.server
                    || pin.public_key != record.intent.basis.public_key
            }) {
                return Err(ServiceFailure::StaleAssessment);
            }
            if record.intent.action != action {
                return Err(ServiceFailure::RequestConflict);
            }
            return record.intent.capture(self.key, request);
        }
        if slot.owner.is_some()
            || slot.service.is_some()
            || slot.install.is_some()
            || slot.runtime_intent.is_some()
            || slot.running_stage.is_some()
        {
            return Err(ServiceFailure::RequestConflict);
        }
        self.ensure_no_pending(request)?;
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        if expected.is_some_and(|(key, pin, generation)| {
            registration.key() != key
                || registration.server_pin() != pin
                || registration.generation() != generation
        }) {
            return Err(ServiceFailure::StaleAssessment);
        }
        if registration.desired_state() != DesiredServerState::Stopped
            || !registration.is_installed()
        {
            return Err(ServiceFailure::Suppressed);
        }
        match action {
            StoppedNativeAction::Start => self.manager()?.verify_installation(&registration)?,
            StoppedNativeAction::Activate(id) => {
                self.validate_lifecycle_action(&registration, ServiceAction::Activate(id))?
            }
            StoppedNativeAction::Stage { artifact_sha256 } => {
                if artifact_sha256 == [0; 32] {
                    return Err(ServiceFailure::ArtifactRejected);
                }
                if registration
                    .staged_installation()
                    .is_some_and(|staged| staged.artifact_sha256 != artifact_sha256)
                {
                    return Err(ServiceFailure::RequestConflict);
                }
            }
            _ => {}
        }
        let mut token = [0; 32];
        getrandom::fill(&mut token).map_err(|_| ServiceFailure::StorageUnavailable)?;
        let record = StoppedNativeRecord {
            intent: Intent {
                action,
                basis: RegistrationBasis::capture(&registration),
                issued: self.clock.now()?,
                token,
            },
            execution: None,
            origin: None,
            accepted: false,
        };
        record.intent.validate()?;
        lifecycle.verify()?;
        self.mutate_request(request, |journal, index| {
            let slot = &mut journal.reservations[index];
            if slot.owner.is_some()
                || slot.service.is_some()
                || slot.install.is_some()
                || slot.runtime_intent.is_some()
                || slot.running_stage.is_some()
                || slot.stopped_native.is_some()
            {
                return Err(ServiceFailure::RequestConflict);
            }
            slot.stopped_native = Some(record.clone());
            Ok(())
        })?;
        record.intent.capture(self.key, request)
    }
    /// Read-only restoration never refreshes expiry or creates native authority.
    pub fn restore_stopped_native(
        &self,
        request: NativeRequestId,
    ) -> Result<StoppedNativeRequest, ServiceFailure> {
        let (_, _, journal) = self.read_journal()?;
        let slot = &journal.reservations[journal.reservation(request)?];
        if slot.acknowledged {
            return Err(ServiceFailure::RequestRetired);
        }
        slot.stopped_native
            .as_ref()
            .ok_or(ServiceFailure::RequestConflict)?
            .intent
            .capture(self.key, request)
    }
    /// Restores the signed original even after expiry. This read neither
    /// reassesses the operation nor supplies new admission authority.
    pub fn restore_stopped_native_reference(
        &self,
        capture: StoppedNativeRequest,
    ) -> Result<StoppedNativeAssessmentRef, ServiceFailure> {
        self.stopped_record(capture)?
            .reference(self.key, capture.request)
    }
    /// Resolves only an already paired-admitted original. Unaccepted comparisons
    /// remain Reserved and cannot be promoted by account reachability or retry.
    pub async fn resolve_stopped_native_exact(
        &self,
        capture: StoppedNativeRequest,
    ) -> Result<ServiceProgress, ServiceFailure> {
        self.resolve_stopped_native_with_artifact_exact(capture, None)
            .await
    }
    /// Resupplies only the exact signed package for an already accepted original.
    /// Unsigned or assessed-only requests never gain admission through resolution.
    pub async fn resolve_stopped_native_with_artifact_exact(
        &self,
        capture: StoppedNativeRequest,
        artifact: Option<&VerifiedAccountServerArtifact>,
    ) -> Result<ServiceProgress, ServiceFailure> {
        self.reconcile_storage(capture.request)?;
        let record = self.stopped_record(capture)?;
        if !record.accepted {
            let mut progress = record
                .execution
                .as_ref()
                .ok_or(ServiceFailure::Pending)?
                .progress(capture.request);
            progress.phase = ServicePhase::Reserved;
            return Ok(progress);
        }
        self.execute_stopped_native(capture, artifact).await
    }
    /// Retains the exact terminal signed receipt for bounded lost-reply ACK
    /// retries. Acknowledgment never admits new effects or another request ID.
    pub fn acknowledge_stopped_native_exact(
        &self,
        progress: &ServiceProgress,
    ) -> Result<(), ServiceFailure> {
        self.reconcile_storage(progress.request)?;
        let lifecycle = self.lifecycle_lease()?;
        self.mutate_request(progress.request, |journal, index| {
            let slot = &mut journal.reservations[index];
            let record = slot
                .stopped_native
                .as_ref()
                .ok_or(ServiceFailure::PairingRequired)?;
            record.validate(
                self.key,
                progress.request,
                slot.service.as_ref(),
                slot.install.as_ref(),
            )?;
            let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
            if registration.server_pin().identity != record.intent.basis.server
                || registration.server_pin().public_key != record.intent.basis.public_key
                || registration.key() != self.key
            {
                return Err(ServiceFailure::IdentityMismatch);
            }
            let current = match (&slot.service, &slot.install) {
                (Some(current), None) => current.progress(progress.request),
                (None, Some(current)) => current.progress(progress.request),
                _ => return Err(ServiceFailure::PairingRequired),
            };
            if current != *progress {
                return Err(ServiceFailure::RequestConflict);
            }
            if !record.acknowledgeable(
                progress.request,
                slot.service.as_ref(),
                slot.install.as_ref(),
            ) {
                return Err(ServiceFailure::Pending);
            }
            lifecycle.verify()?;
            slot.acknowledged = true;
            Ok(())
        })
    }
    fn stopped_record(
        &self,
        capture: StoppedNativeRequest,
    ) -> Result<StoppedNativeRecord, ServiceFailure> {
        let (_, _, journal) = self.read_journal()?;
        let slot = &journal.reservations[journal.reservation(capture.request)?];
        if slot.acknowledged {
            return Err(ServiceFailure::RequestRetired);
        }
        let record = slot
            .stopped_native
            .clone()
            .ok_or(ServiceFailure::RequestConflict)?;
        if record.intent.capture(self.key, capture.request)? != capture {
            return Err(ServiceFailure::RequestConflict);
        }
        Ok(record)
    }
    /// Confirms native instance, signed descendant and listener absence before
    /// composition may open existing keys or SQLite. No storage is initialized.
    pub async fn admit_stopped_native(
        &self,
        capture: StoppedNativeRequest,
    ) -> Result<StoppedNativeAdmission, ServiceFailure> {
        let lifecycle = self.lifecycle_lease()?;
        let record = self.stopped_record(capture)?;
        if record.accepted {
            return Err(ServiceFailure::Pending);
        }
        record.intent.check_time(self.clock.now()?)?;
        self.ensure_no_pending(capture.request)?;
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        if !record.intent.basis.matches(&registration) {
            return Err(ServiceFailure::StaleAssessment);
        }
        self.check_startup_barrier(&registration.record)?;
        let directory = Directory::open(&self.anchor)?.ok_or(ServiceFailure::Missing)?;
        let instance = directory.lease("instance-lock")?;
        let admission = StoppedNativeAdmission {
            supervisor: self.clone(),
            registration,
            capture,
            lifecycle,
            instance,
        };
        admission.revalidate().await?;
        Ok(admission)
    }
    /// Resumes effects only after paired admission has become durable and the
    /// caller has released its frozen store and stopped instance lease.
    pub async fn execute_stopped_native(
        &self,
        capture: StoppedNativeRequest,
        artifact: Option<&VerifiedAccountServerArtifact>,
    ) -> Result<ServiceProgress, ServiceFailure> {
        let record = self.stopped_record(capture)?;
        if !record.accepted {
            return Err(ServiceFailure::PairingRequired);
        }
        match record.execution.ok_or(ServiceFailure::InvalidRecord)? {
            Execution::Service(_) => self.resolve(capture.request).await,
            Execution::Stage(_) => {
                if let Some(artifact) = artifact {
                    self.check_stopped_artifact(artifact)?;
                    if !matches!(capture.action, StoppedNativeAction::Stage { artifact_sha256 } if artifact_sha256 == artifact.artifact_sha256())
                    {
                        return Err(ServiceFailure::RequestConflict);
                    }
                }
                let lifecycle = self.lifecycle_lease()?;
                self.ensure_no_pending(capture.request)?;
                let original = self
                    .original_install(capture.request)?
                    .ok_or(ServiceFailure::InvalidRecord)?;
                if original.phase == ServicePhase::Completed {
                    return Ok(original.progress(capture.request));
                }
                self.run_stage_install(capture.request, original, artifact, &lifecycle)
            }
        }
    }
    pub(super) fn check_stopped_artifact(
        &self,
        artifact: &VerifiedAccountServerArtifact,
    ) -> Result<(), ServiceFailure> {
        use spaceterm_distribution::{BuildChannel, ServerPlatform, UpdateKey};
        let build = match self.key.build_identity {
            crate::startup::BuildIdentity::Release => BuildChannel::Release,
            crate::startup::BuildIdentity::Preflight => BuildChannel::Preflight,
            crate::startup::BuildIdentity::Development => BuildChannel::Development,
        };
        let platform = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            ServerPlatform::DarwinArm64
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            ServerPlatform::LinuxX86_64
        } else {
            return Err(ServiceFailure::UnsupportedHost);
        };
        #[cfg(feature = "test-support")]
        let trust = self
            .test_installation_trust
            .clone()
            .or_else(UpdateKey::release);
        #[cfg(not(feature = "test-support"))]
        let trust = UpdateKey::release();
        let trust = trust.ok_or(ServiceFailure::ArtifactRejected)?;
        if !artifact.feed().authenticated_by(&trust)
            || artifact.manifest().build_identity != build
            || artifact.manifest().platform != platform
        {
            return Err(ServiceFailure::ArtifactRejected);
        }
        Ok(())
    }
}
