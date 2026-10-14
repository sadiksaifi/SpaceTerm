//! Signed Stage admission on an existing paired running connection. Stage never
//! reserves or retires Sessions and never selects the candidate as approved code.

use std::{fmt, sync::atomic::Ordering};

use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use spaceterm_core::{
    authority::{ConnectionGeneration, FieldVersion, ServerEpoch},
    identity::{ClientId, DeviceId, PairingId, ServerId},
    operations::RequestDigest,
    pairing::policy::PairingPrincipal,
};
use spaceterm_distribution::{BuildVersion, VerifiedAccountServerArtifact};
use spaceterm_protocol::authentication::ServerPin;

use super::{
    RegisteredServer, RuntimeLifecycleAuthority, ServiceFailure, ServiceOutcome, ServicePhase,
    ServiceProgress, ServiceRequestState, ServiceSupervisor, ServiceTime,
    install::InstallRecord,
    registration::{REGISTRATION_LIMIT, Registration},
    supervisor::{JOURNAL_LIMIT, Journal, RegistrationBasis, decode, encode},
};
use crate::{
    native_deployment::contracts::NativeRequestId,
    private_filesystem::{Directory, Failure},
    startup::{AccountBuildKey, DesiredServerState},
};

const DOMAIN: &[u8] = b"spaceterm-account-running-stage-origin-v1\0";

/// Guest-issued comparison to one exact original signed-package Stage capture.
/// This value grants neither paired nor native installation authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeStageRequest {
    /// Original guest-issued, durably reserved identifier.
    pub request: NativeRequestId,
    /// Exact authenticated compressed archive identity.
    pub artifact_sha256: [u8; 32],
    /// Binds account/build, pin, registration selection, version, nonce and expiry.
    pub capture_digest: RequestDigest,
}

/// Bounded public comparison to a retained runtime-signed original Stage proof.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeStageAssessmentRef {
    /// Exact original native capture.
    pub original: NativeStageRequest,
    /// Existing installation executor's exact request digest.
    pub request_digest: RequestDigest,
    /// Digest of the retained signed original, not the signature itself.
    pub proof_digest: RequestDigest,
    /// Exact running actor epoch that signed this original.
    pub epoch: ServerEpoch,
    /// Original cross-process monotonic expiry, never extended on retry.
    pub expires_at: ServiceTime,
}

/// In-process evidence of this owner's exact durable native Stage capture.
/// Private fields retain the journal object and allocation baseline. This token
/// cannot be decoded, transferred over the wire, or used for Stage admission.
pub struct NativeStageOwnerToken {
    directory: Directory,
    anchor: std::path::PathBuf,
    key: AccountBuildKey,
    pin: ServerPin,
    capture: NativeStageRequest,
    namespace: [u8; 8],
    high_water: u64,
}

/// Lease-linearized disposition of the original native Stage owner's handoff.
/// This observation grants no admission, replacement or discard authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeStageHandoff {
    /// No admission exists. Wait using the original native expiry.
    Awaiting {
        /// Immutable boot-bound expiry of the captured original.
        expires_at: ServiceTime,
    },
    /// Resolve only this exact original, including a sealed non-admission result.
    ResolveOriginal {
        /// Exact retained signed progress, never replacement authority.
        progress: ServiceProgress,
    },
    /// Irreversible journal retirement proves only that this owner's original
    /// has no outstanding effects. This supplies no outcome or ACK match.
    RetiredOriginal {
        /// Exact capture previously validated by the opaque in-process token.
        original: NativeStageRequest,
    },
    /// Native expiry excludes admission. Unsigned capture metadata stays intact.
    ExpiredNonAdmission {
        /// Exact original capture whose immutable expiry excludes admission.
        original: NativeStageRequest,
        /// Exact terminal signed progress, absent for an unsigned capture.
        progress: Option<ServiceProgress>,
    },
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    artifact_sha256: [u8; 32],
    version: BuildVersion,
    basis: RegistrationBasis,
    issued: ServiceTime,
    nonce: [u8; 32],
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
    fn observed_expiry(&self, now: ServiceTime) -> Result<bool, ServiceFailure> {
        if now.boot == [0; 32] || (now.boot == self.issued.boot && now.millis < self.issued.millis)
        {
            return Err(ServiceFailure::SupervisorUnavailable);
        }
        let expires_at = self.expires_at()?;
        Ok(now.boot != expires_at.boot || now.millis >= expires_at.millis)
    }
    fn validate(&self) -> Result<(), ServiceFailure> {
        self.basis.validate()?;
        if self.basis.desired != DesiredServerState::Running
            || !self.basis.installed
            || self.basis.approved.is_none()
            || self.artifact_sha256 == [0; 32]
            || self.issued.boot == [0; 32]
            || self.nonce == [0; 32]
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        self.expires_at().map(|_| ())
    }
    fn capture(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
    ) -> Result<NativeStageRequest, ServiceFailure> {
        let mut hash = Sha256::new();
        hash.update(b"spaceterm-account-running-stage-capture-v1\0");
        hash.update(encode(&(key, request, self), JOURNAL_LIMIT)?);
        Ok(NativeStageRequest {
            request,
            artifact_sha256: self.artifact_sha256,
            capture_digest: RequestDigest::from_bytes(hash.finalize().into()),
        })
    }
    fn execution(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
    ) -> Result<InstallRecord, ServiceFailure> {
        InstallRecord::stage(
            key,
            request,
            self.artifact_sha256,
            self.version.clone(),
            self.basis.clone(),
        )
    }
    fn matches_artifact(&self, artifact: &VerifiedAccountServerArtifact) -> bool {
        self.artifact_sha256 == artifact.artifact_sha256()
            && self.version == artifact.manifest().version
    }
    fn check_stage_selection(&self) -> Result<(), ServiceFailure> {
        // Stage does not authorize discarding an existing different candidate.
        // Keep this admission policy outside historical digest validation.
        if self.basis.staged.as_ref().is_some_and(|staged| {
            staged.artifact_sha256 != self.artifact_sha256 || staged.version != self.version
        }) {
            return Err(ServiceFailure::RequestConflict);
        }
        Ok(())
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    device: DeviceId,
    pairing: PairingId,
    client: ClientId,
    authorization_version: FieldVersion,
    connection: ConnectionGeneration,
    epoch: ServerEpoch,
}
impl Claims {
    fn from_authority(
        authority: &RuntimeLifecycleAuthority,
        connection: ConnectionGeneration,
    ) -> Self {
        Self {
            device: authority.principal.device(),
            pairing: authority.principal.pairing(),
            client: authority.principal.client(),
            authorization_version: authority.authorization_version,
            connection,
            epoch: authority.epoch,
        }
    }
    fn matches(
        &self,
        principal: PairingPrincipal,
        version: FieldVersion,
        connection: ConnectionGeneration,
        epoch: ServerEpoch,
    ) -> bool {
        self.device == principal.device()
            && self.pairing == principal.pairing()
            && self.client == principal.client()
            && self.authorization_version == version
            && self.connection == connection
            && self.epoch == epoch
    }
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Origin {
    claims: Claims,
    signature: Vec<u8>,
}

/// Retained only in the existing native request journal. The installation record
/// is published in the same journal transaction as its signed Accepted origin.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RunningStageRecord {
    intent: Intent,
    origin: Option<Origin>,
    accepted: bool,
}
impl RunningStageRecord {
    fn transcript(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
        claims: &Claims,
    ) -> Result<Vec<u8>, ServiceFailure> {
        let mut bytes = DOMAIN.to_vec();
        bytes.extend(encode(
            &(
                key,
                self.intent.capture(key, request)?,
                self.intent
                    .execution(key, request)?
                    .progress(request)
                    .request_digest,
                claims,
            ),
            JOURNAL_LIMIT,
        )?);
        Ok(bytes)
    }
    pub(super) fn validate(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
        install: Option<&InstallRecord>,
    ) -> Result<(), ServiceFailure> {
        self.intent.validate()?;
        let Some(origin) = &self.origin else {
            return if !self.accepted && install.is_none() {
                Ok(())
            } else {
                Err(ServiceFailure::InvalidRecord)
            };
        };
        let server = ServerId::new(self.intent.basis.server);
        if origin.claims.device.bytes() == [0; 16]
            || origin.claims.pairing.server() != server
            || origin.claims.client.server() != server
            || origin.claims.authorization_version.get() == 0
            || origin.claims.connection.get() == 0
            || origin.claims.epoch.get() == 0
            || origin.signature.len() != 64
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        UnparsedPublicKey::new(&ED25519, self.intent.basis.public_key)
            .verify(
                &self.transcript(key, request, &origin.claims)?,
                &origin.signature,
            )
            .map_err(|_| ServiceFailure::InvalidRecord)?;
        match (self.accepted, install) {
            (false, None) => Ok(()),
            (_, Some(current))
                if current
                    .matches_stopped_stage(&self.intent.basis, self.intent.artifact_sha256)
                    && current.progress(request).request_digest
                        == self
                            .intent
                            .execution(key, request)?
                            .progress(request)
                            .request_digest =>
            {
                current.validate(key, request)?;
                if !self.accepted
                    && (current.installation.is_some()
                        || self
                            .non_admission_progress(request, Some(current))
                            .is_none())
                {
                    return Err(ServiceFailure::InvalidRecord);
                }
                Ok(())
            }
            _ => Err(ServiceFailure::InvalidRecord),
        }
    }
    fn non_admission_progress(
        &self,
        request: NativeRequestId,
        install: Option<&InstallRecord>,
    ) -> Option<ServiceProgress> {
        if self.accepted || self.origin.is_none() {
            return None;
        }
        let progress = install?.progress(request);
        (progress.phase == ServicePhase::Completed
            && progress.state == ServiceRequestState::NotApplied(ServiceFailure::StaleAssessment))
        .then_some(progress)
    }
    pub(super) fn acknowledgeable(
        &self,
        request: NativeRequestId,
        install: Option<&InstallRecord>,
    ) -> bool {
        self.origin.is_some()
            && (self.non_admission_progress(request, install).is_some()
                || (self.accepted
                    && install.is_some_and(|record| {
                        let progress = record.progress(request);
                        progress.phase == ServicePhase::Completed
                            && matches!(
                                progress.state,
                                ServiceRequestState::Completed(ServiceOutcome::Staged(_))
                                    | ServiceRequestState::NotApplied(_)
                            )
                    })))
    }
    fn reference(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
    ) -> Result<NativeStageAssessmentRef, ServiceFailure> {
        let origin = self.origin.as_ref().ok_or(ServiceFailure::Pending)?;
        Ok(NativeStageAssessmentRef {
            original: self.intent.capture(key, request)?,
            request_digest: self
                .intent
                .execution(key, request)?
                .progress(request)
                .request_digest,
            proof_digest: RequestDigest::from_bytes(Sha256::digest(encode(origin, 4096)?).into()),
            epoch: origin.claims.epoch,
            expires_at: self.intent.expires_at()?,
        })
    }
}

/// Non-deserializable final admission submitted only by the existing paired
/// worker. The actor calls publish in its serialized turn after reauthorization.
/// It has no Session reservation, actor callbacks, or service-manager work.
pub struct RuntimeStageAdmission {
    supervisor: ServiceSupervisor,
    reference: NativeStageAssessmentRef,
    authority: RuntimeLifecycleAuthority,
    connection: ConnectionGeneration,
}
impl fmt::Debug for RuntimeStageAdmission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RuntimeStageAdmission(..)")
    }
}
impl RuntimeStageAdmission {
    /// Publishes native Accepted synchronously under the native lifecycle lease.
    /// The caller must supply the same turn's actual live actor authorization.
    pub fn publish(
        self,
        principal: PairingPrincipal,
        version: FieldVersion,
        connection: ConnectionGeneration,
        epoch: ServerEpoch,
    ) -> Result<ServiceProgress, ServiceFailure> {
        if self.authority.principal != principal
            || self.authority.authorization_version != version
            || self.authority.epoch != epoch
            || self.connection != connection
        {
            return Err(ServiceFailure::PairingRequired);
        }
        let supervisor = &self.supervisor;
        let lease = supervisor.lifecycle_lease()?;
        let request = self.reference.original.request;
        let mut record = supervisor.running_stage_record(self.reference.original)?;
        let origin = record
            .origin
            .as_ref()
            .ok_or(ServiceFailure::PairingRequired)?;
        if !origin.claims.matches(principal, version, connection, epoch)
            || principal.server().bytes() != record.intent.basis.server
            || self.authority.signer.public_key() != record.intent.basis.public_key
        {
            return Err(ServiceFailure::PairingRequired);
        }
        if record.reference(supervisor.key, request)? != self.reference {
            return Err(ServiceFailure::RequestConflict);
        }
        let install = supervisor.original_install(request)?;
        if let Some(progress) = record.non_admission_progress(request, install.as_ref()) {
            return Ok(progress);
        }
        if record.accepted {
            return install
                .map(|record| record.progress(request))
                .ok_or(ServiceFailure::InvalidRecord);
        }
        record.intent.check_time(supervisor.clock.now()?)?;
        supervisor.ensure_no_pending(request)?;
        let current = supervisor.discover()?.ok_or(ServiceFailure::Missing)?;
        if !record.intent.basis.matches(&current) {
            return Err(ServiceFailure::StaleAssessment);
        }
        record.intent.check_stage_selection()?;
        let execution = record.intent.execution(supervisor.key, request)?;
        lease.verify()?;
        // Registration and native storage checks can be slow. The original expiry
        // is checked again at the Accepted publication, never refreshed.
        record.intent.check_time(supervisor.clock.now()?)?;
        record.accepted = true;
        supervisor.mutate_request(request, |journal, index| {
            let slot = &mut journal.reservations[index];
            let original = slot
                .running_stage
                .as_ref()
                .ok_or(ServiceFailure::RequestConflict)?;
            if original.intent != record.intent
                || original.origin != record.origin
                || original.accepted
                || slot.install.is_some()
                || slot.service.is_some()
                || slot.owner.is_some()
                || slot.runtime_intent.is_some()
                || slot.stopped_native.is_some()
            {
                return Err(ServiceFailure::RequestConflict);
            }
            record.intent.check_time(supervisor.clock.now()?)?;
            lease.verify()?;
            slot.running_stage = Some(record.clone());
            slot.install = Some(execution.clone());
            Ok(())
        })?;
        Ok(execution.progress(request))
    }
}

impl ServiceSupervisor {
    /// Retains an exact current durable Stage capture for this in-process owner.
    /// Uncertain storage, a forwarded comparison or an evicted ID cannot mint it.
    pub fn native_stage_owner_token(
        &self,
        expected_key: AccountBuildKey,
        expected_pin: ServerPin,
        capture: NativeStageRequest,
    ) -> Result<NativeStageOwnerToken, ServiceFailure> {
        if self.key != expected_key {
            return Err(ServiceFailure::IdentityMismatch);
        }
        self.available()?;
        let lease = self.lifecycle_lease()?;
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        if registration.server_pin() != expected_pin {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let (directory, _, journal) = self.read_journal()?;
        let slot = &journal.reservations[journal.reservation(capture.request)?];
        let record = slot
            .running_stage
            .as_ref()
            .ok_or(ServiceFailure::RequestConflict)?;
        if record.intent.capture(self.key, capture.request)? != capture {
            return Err(ServiceFailure::RequestConflict);
        }
        if record.intent.basis.server != expected_pin.identity
            || record.intent.basis.public_key != expected_pin.public_key
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        record.validate(self.key, capture.request, slot.install.as_ref())?;
        directory.verify()?;
        lease.verify()?;
        Ok(NativeStageOwnerToken {
            directory,
            anchor: self.anchor.clone(),
            key: self.key,
            pin: expected_pin,
            capture,
            namespace: journal.namespace,
            high_water: journal.high_water,
        })
    }

    /// Observes only the original validated by this in-process Stage owner.
    /// RetiredOriginal drains a worker. It is not terminal progress, an ACK match,
    /// or permission to retire a client journal entry or publish another request.
    pub fn poll_owned_runtime_stage_handoff(
        &self,
        token: &NativeStageOwnerToken,
    ) -> Result<NativeStageHandoff, ServiceFailure> {
        if self.key != token.key || self.anchor != token.anchor {
            return Err(ServiceFailure::IdentityMismatch);
        }
        // Verify the retained directory before any recovery uses its pathname.
        match token.directory.verify() {
            Ok(()) => {}
            Err(Failure::Indeterminate) => {
                self.reconcile_runtime_stage_storage(token.key, token.pin, token.capture.request)?;
                token.directory.verify()?;
            }
            Err(error) => return Err(error.into()),
        }
        match self.poll_runtime_stage_handoff(token.key, token.pin, token.capture) {
            Ok(handoff) => return Ok(handoff),
            Err(ServiceFailure::RequestRetired) => {}
            Err(error) => return Err(error),
        }
        // Missing originals cannot recover a publication or clear uncertainty.
        // Require ordinary known-durable state under the admission lease.
        self.available()?;
        let lease = self.lifecycle_lease()?;
        token.directory.verify()?;
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        if registration.server_pin() != token.pin {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let (_, _, journal) = self.read_journal()?;
        if journal.namespace != token.namespace || journal.high_water < token.high_water {
            return Err(ServiceFailure::RequestConflict);
        }
        // The exact authentic row existed at the retained baseline. Allocation
        // has no holes or reuse. Outstanding Accepted effects cannot be evicted.
        journal.sequence(token.capture.request)?;
        match journal.reservation(token.capture.request) {
            Err(ServiceFailure::RequestRetired) => {}
            Ok(_) => return Err(ServiceFailure::Pending),
            Err(error) => return Err(error),
        }
        token.directory.verify()?;
        lease.verify()?;
        Ok(NativeStageHandoff::RetiredOriginal {
            original: token.capture,
        })
    }

    /// Observes this exact Stage handoff under the same lease as paired admission.
    /// Signed never-admitted expiry can become an original terminal NotApplied
    /// receipt. Accepted work is never reclassified and needs exact resolution.
    /// No actor, Session, archive-copy or service-manager operation runs here.
    pub fn poll_runtime_stage_handoff(
        &self,
        expected_key: AccountBuildKey,
        expected_pin: ServerPin,
        capture: NativeStageRequest,
    ) -> Result<NativeStageHandoff, ServiceFailure> {
        self.reconcile_runtime_stage_storage(expected_key, expected_pin, capture.request)?;
        let lease = self.lifecycle_lease()?;
        // A different helper can resolve and ACK before this owner's next poll.
        // Read the retained exact receipt, not a retirement-gated restore API.
        // An evicted ID or a highwater mark is not proof of settlement.
        let (_, _, journal) = self.read_journal()?;
        let slot = &journal.reservations[journal.reservation(capture.request)?];
        let record = slot
            .running_stage
            .as_ref()
            .ok_or(ServiceFailure::RequestConflict)?;
        if record.intent.capture(self.key, capture.request)? != capture {
            return Err(ServiceFailure::RequestConflict);
        }
        record.validate(self.key, capture.request, slot.install.as_ref())?;
        if slot.acknowledged && !record.acknowledgeable(capture.request, slot.install.as_ref()) {
            return Err(ServiceFailure::InvalidRecord);
        }
        if let Some(install) = &slot.install {
            // Validation excludes any pending install without paired Accepted.
            // A sealed non-admission receipt also resolves only its original ID.
            record.reference(self.key, capture.request)?;
            lease.verify()?;
            return Ok(NativeStageHandoff::ResolveOriginal {
                progress: install.progress(capture.request),
            });
        }
        let expires_at = record.intent.expires_at()?;
        if !record.intent.observed_expiry(self.clock.now()?)? {
            lease.verify()?;
            return Ok(NativeStageHandoff::Awaiting { expires_at });
        }
        lease.verify()?;
        let progress = if record.origin.is_some() {
            // No install record or Accepted origin exists while this admission
            // lease is held. Preserve the original proof and accepted=false.
            let terminal = record
                .intent
                .execution(self.key, capture.request)?
                .expired_stage_before_admission(self.key, capture.request)?;
            self.mutate_request(capture.request, |journal, index| {
                let slot = &mut journal.reservations[index];
                let current = slot
                    .running_stage
                    .as_ref()
                    .ok_or(ServiceFailure::RequestConflict)?;
                if current.intent != record.intent
                    || current.origin != record.origin
                    || current.accepted
                    || slot.install.is_some()
                    || slot.acknowledged
                {
                    return Err(ServiceFailure::RequestConflict);
                }
                if !current.intent.observed_expiry(self.clock.now()?)? {
                    return Err(ServiceFailure::Pending);
                }
                lease.verify()?;
                slot.install = Some(terminal.clone());
                Ok(())
            })?;
            Some(terminal.progress(capture.request))
        } else {
            // Unsigned capture metadata stays unchanged. Its immutable native
            // expiry already excludes all future assessment and admission.
            None
        };
        Ok(NativeStageHandoff::ExpiredNonAdmission {
            original: capture,
            progress,
        })
    }
    /// Authenticates and captures one exact original package before the paired
    /// exchange. Repeating Capture restores the same nonce and original expiry.
    pub fn capture_runtime_stage(
        &self,
        request: NativeRequestId,
        artifact: &VerifiedAccountServerArtifact,
    ) -> Result<NativeStageRequest, ServiceFailure> {
        self.check_stopped_artifact(artifact)?;
        let lease = self.lifecycle_lease()?;
        let (_, _, journal) = self.read_journal()?;
        let slot = &journal.reservations[journal.reservation(request)?];
        if slot.acknowledged {
            return Err(ServiceFailure::RequestRetired);
        }
        if let Some(record) = &slot.running_stage {
            if !record.intent.matches_artifact(artifact) {
                return Err(ServiceFailure::RequestConflict);
            }
            return record.intent.capture(self.key, request);
        }
        if slot.owner.is_some()
            || slot.service.is_some()
            || slot.install.is_some()
            || slot.runtime_intent.is_some()
            || slot.stopped_native.is_some()
        {
            return Err(ServiceFailure::RequestConflict);
        }
        self.ensure_no_pending(request)?;
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        if registration.desired_state() != DesiredServerState::Running
            || !registration.is_installed()
        {
            return Err(ServiceFailure::Suppressed);
        }
        let mut nonce = [0; 32];
        getrandom::fill(&mut nonce).map_err(|_| ServiceFailure::StorageUnavailable)?;
        let record = RunningStageRecord {
            intent: Intent {
                artifact_sha256: artifact.artifact_sha256(),
                version: artifact.manifest().version.clone(),
                basis: RegistrationBasis::capture(&registration),
                issued: self.clock.now()?,
                nonce,
            },
            origin: None,
            accepted: false,
        };
        record.validate(self.key, request, None)?;
        record.intent.check_stage_selection()?;
        lease.verify()?;
        self.mutate_request(request, |journal, index| {
            let slot = &mut journal.reservations[index];
            if slot.owner.is_some()
                || slot.service.is_some()
                || slot.install.is_some()
                || slot.runtime_intent.is_some()
                || slot.stopped_native.is_some()
                || slot.running_stage.is_some()
            {
                return Err(ServiceFailure::RequestConflict);
            }
            slot.running_stage = Some(record.clone());
            Ok(())
        })?;
        record.intent.capture(self.key, request)
    }
    /// Restores original input identity without reassessment or expiry refresh.
    pub fn restore_runtime_stage(
        &self,
        request: NativeRequestId,
    ) -> Result<NativeStageRequest, ServiceFailure> {
        let (_, _, journal) = self.read_journal()?;
        let slot = &journal.reservations[journal.reservation(request)?];
        if slot.acknowledged {
            return Err(ServiceFailure::RequestRetired);
        }
        slot.running_stage
            .as_ref()
            .ok_or(ServiceFailure::RequestConflict)?
            .intent
            .capture(self.key, request)
    }
    fn running_stage_record(
        &self,
        capture: NativeStageRequest,
    ) -> Result<RunningStageRecord, ServiceFailure> {
        let (_, _, journal) = self.read_journal()?;
        let slot = &journal.reservations[journal.reservation(capture.request)?];
        if slot.acknowledged {
            return Err(ServiceFailure::RequestRetired);
        }
        let record = slot
            .running_stage
            .clone()
            .ok_or(ServiceFailure::RequestConflict)?;
        if record.intent.capture(self.key, capture.request)? != capture {
            return Err(ServiceFailure::RequestConflict);
        }
        record.validate(self.key, capture.request, slot.install.as_ref())?;
        Ok(record)
    }
    /// Reads the exact signed original even when its original expiry has elapsed.
    pub fn restore_runtime_stage_reference(
        &self,
        capture: NativeStageRequest,
    ) -> Result<NativeStageAssessmentRef, ServiceFailure> {
        self.running_stage_record(capture)?
            .reference(self.key, capture.request)
    }
    /// Signs only the original guest capture using the current paired worker's
    /// actor-authorized principal, authorization version, connection and epoch.
    pub fn assess_runtime_stage(
        &self,
        capture: NativeStageRequest,
        authority: &RuntimeLifecycleAuthority,
        connection: ConnectionGeneration,
    ) -> Result<NativeStageAssessmentRef, ServiceFailure> {
        let lease = self.lifecycle_lease()?;
        let mut record = self.running_stage_record(capture)?;
        let claims = Claims::from_authority(authority, connection);
        if let Some(origin) = &record.origin {
            if origin.claims != claims {
                return Err(ServiceFailure::PairingRequired);
            }
            return record.reference(self.key, capture.request);
        }
        record.intent.check_stage_selection()?;
        record.intent.check_time(self.clock.now()?)?;
        self.ensure_no_pending(capture.request)?;
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        if !record.intent.basis.matches(&registration) {
            return Err(ServiceFailure::StaleAssessment);
        }
        if authority.principal.server().bytes() != record.intent.basis.server
            || authority.signer.public_key() != record.intent.basis.public_key
            || connection.get() == 0
        {
            return Err(ServiceFailure::PairingRequired);
        }
        let signature = authority
            .signer
            .sign(&record.transcript(self.key, capture.request, &claims)?)
            .map_err(|_| ServiceFailure::StorageUnavailable)?
            .to_vec();
        record.origin = Some(Origin { claims, signature });
        record.validate(self.key, capture.request, None)?;
        lease.verify()?;
        record.intent.check_time(self.clock.now()?)?;
        self.mutate_request(capture.request, |journal, index| {
            let slot = &mut journal.reservations[index];
            let original = slot
                .running_stage
                .as_ref()
                .ok_or(ServiceFailure::RequestConflict)?;
            if original.intent != record.intent
                || original.origin.is_some()
                || original.accepted
                || slot.install.is_some()
            {
                return Err(ServiceFailure::RequestConflict);
            }
            record.intent.check_time(self.clock.now()?)?;
            slot.running_stage = Some(record.clone());
            Ok(())
        })?;
        record.reference(self.key, capture.request)
    }
    /// Builds a native-only final actor message, not an SSH admission capability.
    /// The actor must revalidate authorization and publish in one serialized turn.
    pub fn prepare_runtime_stage_admission(
        &self,
        reference: NativeStageAssessmentRef,
        authority: RuntimeLifecycleAuthority,
        connection: ConnectionGeneration,
    ) -> Result<RuntimeStageAdmission, ServiceFailure> {
        let record = self.running_stage_record(reference.original)?;
        if record.reference(self.key, reference.original.request)? != reference {
            return Err(ServiceFailure::RequestConflict);
        }
        if record
            .origin
            .as_ref()
            .is_none_or(|origin| origin.claims != Claims::from_authority(&authority, connection))
        {
            return Err(ServiceFailure::PairingRequired);
        }
        Ok(RuntimeStageAdmission {
            supervisor: self.clone(),
            reference,
            authority,
            connection,
        })
    }
    /// Resolves only the signed, already Accepted original through the existing
    /// staging executor. Unaccepted work remains Reserved unless native expiry
    /// has sealed its exact signed NotApplied receipt. No service manager,
    /// activation, listening, Start, Stop or Session barrier is involved.
    pub fn resolve_runtime_stage_exact(
        &self,
        request: NativeRequestId,
        artifact: Option<&VerifiedAccountServerArtifact>,
    ) -> Result<ServiceProgress, ServiceFailure> {
        self.reconcile_storage(request)?;
        let lease = self.lifecycle_lease()?;
        let capture = self.restore_runtime_stage(request)?;
        let record = self.running_stage_record(capture)?;
        if let Some(artifact) = artifact {
            self.check_stopped_artifact(artifact)?;
            if !record.intent.matches_artifact(artifact) {
                return Err(ServiceFailure::RequestConflict);
            }
        }
        let install = self.original_install(request)?;
        if let Some(progress) = record.non_admission_progress(request, install.as_ref()) {
            return Ok(progress);
        }
        if !record.accepted {
            if record.origin.is_none() {
                return Err(ServiceFailure::PairingRequired);
            }
            let mut progress = record
                .intent
                .execution(self.key, request)?
                .progress(request);
            progress.phase = ServicePhase::Reserved;
            return Ok(progress);
        }
        self.ensure_no_pending(request)?;
        let original = install.ok_or(ServiceFailure::InvalidRecord)?;
        if original.phase == ServicePhase::Completed {
            return Ok(original.progress(request));
        }
        self.run_stage_install(request, original, artifact, &lease)
    }
    /// Acknowledges only the exact signed terminal Stage result. The original
    /// receipt is retained for bounded lost-reply retries, never fresh authority.
    pub fn acknowledge_runtime_stage_exact(
        &self,
        progress: &ServiceProgress,
    ) -> Result<(), ServiceFailure> {
        self.reconcile_storage(progress.request)?;
        let lease = self.lifecycle_lease()?;
        self.mutate_request(progress.request, |journal, index| {
            let slot = &mut journal.reservations[index];
            let record = slot
                .running_stage
                .as_ref()
                .ok_or(ServiceFailure::PairingRequired)?;
            record.validate(self.key, progress.request, slot.install.as_ref())?;
            let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
            if registration.server_pin().identity != record.intent.basis.server
                || registration.server_pin().public_key != record.intent.basis.public_key
            {
                return Err(ServiceFailure::IdentityMismatch);
            }
            let current = slot
                .install
                .as_ref()
                .ok_or(ServiceFailure::PairingRequired)?;
            if current.progress(progress.request) != *progress {
                return Err(ServiceFailure::RequestConflict);
            }
            if !record.acknowledgeable(progress.request, Some(current)) {
                return Err(ServiceFailure::Pending);
            }
            lease.verify()?;
            slot.acknowledged = true;
            Ok(())
        })
    }
    /// Recovers only the retained publication for this exact native Stage scope.
    /// Neither an absent record nor an uncertain reply can become new authority.
    pub fn reconcile_runtime_stage_storage(
        &self,
        expected_key: AccountBuildKey,
        expected_pin: ServerPin,
        request: NativeRequestId,
    ) -> Result<(), ServiceFailure> {
        if self.key != expected_key {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let directory =
            Directory::open_for_recovery(&self.anchor)?.ok_or(ServiceFailure::Missing)?;
        let registration = directory
            .read_for_recovery("registration", REGISTRATION_LIMIT)?
            .ok_or(ServiceFailure::Missing)?;
        self.stage_recovery_registration(&registration.bytes, expected_pin)?;
        let journal = directory
            .read_for_recovery("requests", JOURNAL_LIMIT)?
            .ok_or(ServiceFailure::UnknownRequest)?;
        self.stage_recovery_journal(&journal.bytes, expected_pin, request)?;
        match Directory::open(&self.anchor) {
            Ok(Some(_)) => {}
            Ok(None) => return Err(ServiceFailure::Missing),
            Err(Failure::Indeterminate) => {
                match directory.reconcile_retained_publication("requests", JOURNAL_LIMIT, |bytes| {
                    self.stage_recovery_journal(bytes, expected_pin, request)
                        .map_err(|_| Failure::Unsafe)
                }) {
                    Ok(()) => {}
                    Err(Failure::Indeterminate) => directory.reconcile_retained_publication(
                        "registration",
                        REGISTRATION_LIMIT,
                        |bytes| {
                            self.stage_recovery_registration(bytes, expected_pin)
                                .map(|_| ())
                                .map_err(|_| Failure::Unsafe)
                        },
                    )?,
                    Err(Failure::Conflict) => directory.verify()?,
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
        let directory = Directory::open(&self.anchor)?.ok_or(ServiceFailure::Missing)?;
        let registration = directory
            .read("registration", REGISTRATION_LIMIT)?
            .ok_or(ServiceFailure::Missing)?;
        let record = self.stage_recovery_registration(&registration.bytes, expected_pin)?;
        let journal = directory
            .read("requests", JOURNAL_LIMIT)?
            .ok_or(ServiceFailure::UnknownRequest)?;
        self.stage_recovery_journal(&journal.bytes, expected_pin, request)?;
        let data = Directory::open_for_recovery(&record.data)?.ok_or(ServiceFailure::Missing)?;
        let registration = RegisteredServer {
            record,
            directory: data,
        };
        super::runtime_inventory::recover_publications(&registration)?;
        registration.directory.verify_established()?;
        self.indeterminate.store(false, Ordering::Release);
        Ok(())
    }
    fn stage_recovery_registration(
        &self,
        bytes: &[u8],
        pin: ServerPin,
    ) -> Result<Registration, ServiceFailure> {
        let record: Registration = decode(bytes, REGISTRATION_LIMIT)?;
        record.validate(self.key)?;
        if record.server != pin.identity || record.public_key != pin.public_key {
            return Err(ServiceFailure::IdentityMismatch);
        }
        Ok(record)
    }
    fn stage_recovery_journal(
        &self,
        bytes: &[u8],
        pin: ServerPin,
        request: NativeRequestId,
    ) -> Result<(), ServiceFailure> {
        let journal: Journal = decode(bytes, JOURNAL_LIMIT)?;
        journal.validate(self.key)?;
        let slot = &journal.reservations[journal.reservation(request)?];
        if slot.owner.is_some()
            || slot.service.is_some()
            || slot.runtime_intent.is_some()
            || slot.stopped_native.is_some()
        {
            return Err(ServiceFailure::RequestConflict);
        }
        let Some(record) = &slot.running_stage else {
            return if slot.install.is_none() {
                Ok(())
            } else {
                Err(ServiceFailure::RequestConflict)
            };
        };
        if record.intent.basis.server != pin.identity
            || record.intent.basis.public_key != pin.public_key
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        record.validate(self.key, request, slot.install.as_ref())
    }
}

/// Closed SSH grammar. Only bounded signed-package input crosses this boundary;
/// no verb can submit a principal, origin proof, or native admission authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeStageAccountRequest {
    /// Captures the original exact package without granting admission.
    Capture {
        /// Expected owning account and compiled build identity.
        expected_key: AccountBuildKey,
        /// Expected retained public server identity and key.
        #[serde(with = "super::remote_wire::pin_codec")]
        expected_pin: ServerPin,
        /// Original guest-issued, durably reserved identifier.
        request: NativeRequestId,
        /// Exact archive identity of the separately transferred signed package.
        artifact_sha256: [u8; 32],
    },
    /// Restores retained original comparisons without refreshing their expiry.
    Restore {
        /// Expected owning account and compiled build identity.
        expected_key: AccountBuildKey,
        /// Expected retained public server identity and key.
        #[serde(with = "super::remote_wire::pin_codec")]
        expected_pin: ServerPin,
        /// Original guest-issued identifier, never a replacement allocation.
        request: NativeRequestId,
    },
    /// Advances only the signed, already Accepted original native Stage effect.
    Resolve {
        /// Expected owning account and compiled build identity.
        expected_key: AccountBuildKey,
        /// Expected retained public server identity and key.
        #[serde(with = "super::remote_wire::pin_codec")]
        expected_pin: ServerPin,
        /// Original identifier whose immutable package selection is retained.
        request: NativeRequestId,
    },
    /// Acknowledges only the exact immutable terminal signed Stage result.
    Acknowledge {
        /// Expected owning account and compiled build identity.
        expected_key: AccountBuildKey,
        /// Expected retained public server identity and key.
        #[serde(with = "super::remote_wire::pin_codec")]
        expected_pin: ServerPin,
        /// Complete original terminal progress, including its execution digest.
        progress: ServiceProgress,
    },
}
impl NativeStageAccountRequest {
    /// Expected native identity comparisons for every closed verb.
    pub fn expected_identity(&self) -> (AccountBuildKey, ServerPin) {
        match self {
            Self::Capture {
                expected_key,
                expected_pin,
                ..
            }
            | Self::Restore {
                expected_key,
                expected_pin,
                ..
            }
            | Self::Resolve {
                expected_key,
                expected_pin,
                ..
            }
            | Self::Acknowledge {
                expected_key,
                expected_pin,
                ..
            } => (*expected_key, *expected_pin),
        }
    }
    /// Validates only bounded public comparisons, never authority.
    pub fn validate(&self) -> Result<(), ServiceFailure> {
        let (key, pin) = self.expected_identity();
        if key.account_uid == 0
            || key.account_uid == u32::MAX
            || pin.identity == [0; 16]
            || pin.public_key == [0; 32]
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        match self {
            Self::Capture {
                artifact_sha256, ..
            } if *artifact_sha256 == [0; 32] => Err(ServiceFailure::InvalidRecord),
            Self::Acknowledge { progress, .. } => validate_stage_progress(progress, true),
            _ => Ok(()),
        }
    }
    /// Encodes one bounded, closed native Stage body.
    pub fn encode(&self) -> Result<Vec<u8>, ServiceFailure> {
        self.validate()?;
        encode(self, super::remote_wire::NATIVE_LIFECYCLE_WIRE_LIMIT)
    }
    /// Decodes bounded comparison data before any native effect.
    pub fn decode(bytes: &[u8]) -> Result<Self, ServiceFailure> {
        if bytes.len() > super::remote_wire::NATIVE_LIFECYCLE_WIRE_LIMIT {
            return Err(ServiceFailure::LimitExceeded);
        }
        let request: Self =
            serde_json::from_slice(bytes).map_err(|_| ServiceFailure::InvalidRecord)?;
        request.validate()?;
        Ok(request)
    }
    /// Exact guest-issued original selected by this request.
    pub fn request(&self) -> NativeRequestId {
        match self {
            Self::Capture { request, .. }
            | Self::Restore { request, .. }
            | Self::Resolve { request, .. } => *request,
            Self::Acknowledge { progress, .. } => progress.request,
        }
    }
}
/// Bounded public results. The signed authority stays in the native journal.
#[expect(
    clippy::large_enum_variant,
    reason = "The bounded wire reply keeps original comparisons inline without changing the shared owned DTO API or adding per-reply allocations."
)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "result",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NativeStageAccountReply {
    /// Original capture, unchanged after lost replies or expiry.
    Captured(NativeStageRequest),
    /// Exact retained original comparisons, without new admission authority.
    Restored {
        /// Original guest-issued capture and exact archive selection.
        original: NativeStageRequest,
        /// Original runtime-signed public reference, if assessment completed.
        assessment: Option<NativeStageAssessmentRef>,
        /// Original native progress, if paired admission completed.
        progress: Option<ServiceProgress>,
    },
    /// Exact original progress, including Reserved or Accepted input-pending work.
    Progress(ServiceProgress),
    /// Exact terminal receipt acknowledged, including on a lost-reply retry.
    Acknowledged {
        /// Complete immutable terminal progress, never a replacement identifier.
        progress: ServiceProgress,
    },
    /// Fixed content-free classification, never native error text.
    Refused(ServiceFailure),
}

impl NativeStageAccountReply {
    /// Validates reply comparisons without importing the retained signed proof.
    pub fn validate(&self) -> Result<(), ServiceFailure> {
        match self {
            Self::Captured(original) => validate_stage_capture(*original),
            Self::Restored {
                original,
                assessment,
                progress,
            } => {
                validate_stage_capture(*original)?;
                if let Some(reference) = assessment
                    && (reference.original != *original
                        || reference.request_digest.bytes() == [0; 32]
                        || reference.proof_digest.bytes() == [0; 32]
                        || reference.epoch.get() == 0
                        || reference.expires_at.boot == [0; 32]
                        || reference.expires_at.millis == 0)
                {
                    return Err(ServiceFailure::InvalidRecord);
                }
                if let Some(progress) = progress {
                    validate_stage_progress(progress, false)?;
                    if progress.request != original.request
                        || assessment.as_ref().is_none_or(|reference| {
                            reference.request_digest != progress.request_digest
                        })
                    {
                        return Err(ServiceFailure::InvalidRecord);
                    }
                }
                Ok(())
            }
            Self::Progress(progress) => validate_stage_progress(progress, false),
            Self::Acknowledged { progress } => validate_stage_progress(progress, true),
            Self::Refused(_) => Ok(()),
        }
    }
    /// Encodes a bounded, content-free native Stage reply.
    pub fn encode(&self) -> Result<Vec<u8>, ServiceFailure> {
        self.validate()?;
        encode(self, super::remote_wire::NATIVE_LIFECYCLE_WIRE_LIMIT)
    }
    /// Decodes and validates one bounded public reply.
    pub fn decode(bytes: &[u8]) -> Result<Self, ServiceFailure> {
        if bytes.len() > super::remote_wire::NATIVE_LIFECYCLE_WIRE_LIMIT {
            return Err(ServiceFailure::LimitExceeded);
        }
        let reply: Self =
            serde_json::from_slice(bytes).map_err(|_| ServiceFailure::InvalidRecord)?;
        reply.validate()?;
        Ok(reply)
    }
}
fn validate_stage_capture(original: NativeStageRequest) -> Result<(), ServiceFailure> {
    if original.artifact_sha256 == [0; 32] || original.capture_digest.bytes() == [0; 32] {
        return Err(ServiceFailure::InvalidRecord);
    }
    Ok(())
}
fn validate_stage_progress(
    progress: &ServiceProgress,
    terminal: bool,
) -> Result<(), ServiceFailure> {
    if progress.request_digest.bytes() == [0; 32]
        || (terminal && progress.phase != ServicePhase::Completed)
    {
        return Err(ServiceFailure::InvalidRecord);
    }
    match (&progress.phase, &progress.state) {
        (
            ServicePhase::Completed,
            ServiceRequestState::Completed(ServiceOutcome::Staged(installation)),
        ) if installation.artifact_sha256 != [0; 32] => Ok(()),
        (ServicePhase::Completed, ServiceRequestState::NotApplied(_)) => Ok(()),
        (
            ServicePhase::Reserved
            | ServicePhase::Accepted
            | ServicePhase::IntentCommitted
            | ServicePhase::Publishing,
            ServiceRequestState::Pending | ServiceRequestState::Indeterminate,
        ) if !terminal => Ok(()),
        _ => Err(ServiceFailure::InvalidRecord),
    }
}
