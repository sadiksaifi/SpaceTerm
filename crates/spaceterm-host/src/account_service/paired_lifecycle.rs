//! Paired admission of a guest-issued native lifecycle request. Wire values are
//! comparisons only. The live runtime signs and retains the original authority.

use std::sync::{Arc, atomic::Ordering};

use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use spaceterm_core::{
    authority::{FieldVersion, ServerEpoch},
    identity::{ClientId, DeviceId, PairingId, ServerId},
    operations::RequestDigest,
    pairing::policy::PairingPrincipal,
};
use spaceterm_protocol::authentication::{ServerPin, SigningKey};

use super::{
    RegisteredServer, RuntimeAssessment, ServiceAction, ServiceFailure, ServiceFuture,
    ServiceOutcome, ServicePhase, ServiceProcesses, ServiceProgress, ServiceRequestState,
    ServiceSupervisor, ServiceTime,
    registration::{REGISTRATION_LIMIT, Registration},
    runtime_inventory::RuntimeCustody,
    supervisor::{
        AssessmentRecord, JOURNAL_LIMIT, Journal, RegistrationBasis, RequestBinding, RequestRecord,
        decode, encode,
    },
};
use crate::{
    native_deployment::contracts::NativeRequestId,
    private_filesystem::{Directory, Failure},
    startup::AccountBuildKey,
};

const DOMAIN: &[u8] = b"spaceterm-account-runtime-native-admission-v1\0";

/// Guest-issued original request comparison, captured before the paired exchange.
/// This value contains no native authority, paths, credentials or runtime scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLifecycleRequest {
    /// The original guest-issued, durably reserved identifier.
    pub request: NativeRequestId,
    /// Exact action, including the selected installation for activation.
    pub action: ServiceAction,
    /// Digest of the original account/build, registration, action and expiry.
    pub capture_digest: RequestDigest,
}

/// Bounded public summary of an assessment retained in the native journal.
/// Decoding or confirming this reference does not issue admission authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLifecycleAssessmentRef {
    /// Original guest request.
    pub original: NativeLifecycleRequest,
    /// Canonical assessed action and runtime scope digest.
    pub request_digest: RequestDigest,
    /// Digest of the retained signed runtime-origin proof.
    pub proof_digest: RequestDigest,
    /// Exact runtime epoch that produced the assessment.
    pub epoch: ServerEpoch,
    /// All-client affected Session count.
    pub affected_sessions: u32,
    /// Original cross-process monotonic expiry, never refreshed on restore.
    pub expires_at: ServiceTime,
}
impl NativeLifecycleAssessmentRef {
    /// Records explicit foreground confirmation of this exact public summary.
    /// The runtime still rechecks pairing, proof, scope and the original expiry.
    pub fn confirm(self) -> ConfirmedNativeLifecycleAction {
        ConfirmedNativeLifecycleAction(self)
    }
}

/// In-process evidence that this owner observed an exact durable native capture.
/// Private fields retain the native journal object and allocation baseline. This
/// token cannot be decoded, transferred over the wire, or used for admission.
pub struct NativeLifecycleOwnerToken {
    directory: Directory,
    anchor: std::path::PathBuf,
    key: AccountBuildKey,
    pin: ServerPin,
    capture: NativeLifecycleRequest,
    namespace: [u8; 8],
    high_water: u64,
}

/// Lease-linearized disposition of the original native owner's handoff.
/// This observation issues no admission or replacement action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeLifecycleHandoff {
    /// No admission or reservation exists. Wait using the original native expiry.
    Awaiting {
        /// Immutable boot-bound expiry of the captured original.
        expires_at: ServiceTime,
    },
    /// Resolve only this signed original, including interrupted non-admission cleanup.
    ResolveOriginal {
        /// Exact retained signed progress, never replacement authority.
        progress: ServiceProgress,
    },
    /// Existing irreversible journal retirement proves only that this owner's
    /// original has no outstanding effects. It supplies no outcome or ACK match.
    RetiredOriginal {
        /// Exact capture previously validated by the opaque in-process owner token.
        original: NativeLifecycleRequest,
    },
    /// Native expiry excludes admission. Unsigned capture metadata remains intact.
    ExpiredNonAdmission {
        /// Exact original capture whose native expiry excludes admission.
        original: NativeLifecycleRequest,
        /// Exact terminal signed progress, absent for an unsigned capture.
        progress: Option<ServiceProgress>,
    },
}

/// Native caller's non-deserializable explicit confirmation. This is not a
/// credential and cannot replace the final authenticated paired admission.
pub struct ConfirmedNativeLifecycleAction(NativeLifecycleAssessmentRef);
impl ConfirmedNativeLifecycleAction {
    /// Returns comparison data for the final paired request.
    pub fn reference(&self) -> &NativeLifecycleAssessmentRef {
        &self.0
    }
    /// Moves comparison data into the final paired request.
    pub fn into_reference(self) -> NativeLifecycleAssessmentRef {
        self.0
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeIntent {
    action: ServiceAction,
    basis: RegistrationBasis,
    issued: ServiceTime,
    token: [u8; 32],
}
impl RuntimeIntent {
    pub(super) fn validate(&self) -> Result<(), ServiceFailure> {
        self.basis.validate()?;
        if self.token == [0; 32] || self.issued.boot == [0; 32] {
            return Err(ServiceFailure::InvalidRecord);
        }
        self.expires_at()?;
        Ok(())
    }
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
    fn reference(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
    ) -> Result<NativeLifecycleRequest, ServiceFailure> {
        let mut digest = Sha256::new();
        digest.update(b"spaceterm-account-native-capture-v1\0");
        digest.update(encode(&key, 1024)?);
        digest.update(request.as_bytes());
        digest.update(encode(self, super::supervisor::JOURNAL_LIMIT)?);
        Ok(NativeLifecycleRequest {
            request,
            action: self.action,
            capture_digest: RequestDigest::from_bytes(digest.finalize().into()),
        })
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginClaims {
    capture: NativeLifecycleRequest,
    server: ServerId,
    epoch: ServerEpoch,
    device: DeviceId,
    pairing: PairingId,
    client: ClientId,
    authorization_version: FieldVersion,
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeOriginProof {
    claims: OriginClaims,
    signature: Vec<u8>,
}
impl RuntimeOriginProof {
    fn transcript(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
        digest: RequestDigest,
    ) -> Result<Vec<u8>, ServiceFailure> {
        let mut bytes = DOMAIN.to_vec();
        bytes.extend(encode(
            &(key, request, digest, &self.claims),
            super::supervisor::JOURNAL_LIMIT,
        )?);
        Ok(bytes)
    }
    pub(super) fn validate(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
        record: &RequestRecord,
        intent: &RuntimeIntent,
    ) -> Result<(), ServiceFailure> {
        let RequestBinding::Confirmed(assessment) = &record.binding else {
            return Err(ServiceFailure::InvalidRecord);
        };
        let claims = &self.claims;
        if claims.capture != intent.reference(key, request)?
            || claims.capture.action != assessment.action
            || assessment.basis != intent.basis
            || assessment.issued != intent.issued
            || assessment.token != intent.token
            || claims.server != assessment.runtime.server
            || Some(claims.epoch) != assessment.runtime.epoch
            || claims.pairing.server() != claims.server
            || claims.client.server() != claims.server
            || claims.device.bytes() == [0; 16]
            || claims.authorization_version.get() == 0
            || self.signature.len() != 64
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        UnparsedPublicKey::new(&ED25519, assessment.basis.public_key)
            .verify(
                &self.transcript(key, request, record.digest)?,
                &self.signature,
            )
            .map_err(|_| ServiceFailure::InvalidRecord)
    }
    fn matches(&self, authority: &RuntimeLifecycleAuthority) -> bool {
        self.claims.server == authority.principal.server()
            && self.claims.epoch == authority.epoch
            && self.claims.device == authority.principal.device()
            && self.claims.pairing == authority.principal.pairing()
            && self.claims.client == authority.principal.client()
            && self.claims.authorization_version == authority.authorization_version
    }
    fn digest(&self) -> Result<RequestDigest, ServiceFailure> {
        Ok(RequestDigest::from_bytes(
            Sha256::digest(encode(self, 4096)?).into(),
        ))
    }
}

/// Live runtime's native-only authority. It cannot be decoded from a wire call,
/// obtained from a UID, or reconstructed from a public assessment reference.
pub struct RuntimeLifecycleAuthority {
    pub(super) principal: PairingPrincipal,
    pub(super) authorization_version: FieldVersion,
    pub(super) epoch: ServerEpoch,
    pub(super) signer: Arc<dyn SigningKey>,
}
impl RuntimeCustody {
    /// Binds the current actor-authorized pairing to this signed runtime epoch.
    /// Callers must revalidate the actual authenticated connection at admission.
    pub fn native_lifecycle_authority(
        &self,
        principal: PairingPrincipal,
        authorization_version: FieldVersion,
    ) -> Result<RuntimeLifecycleAuthority, ServiceFailure> {
        if principal.server() != self.state.server || authorization_version.get() == 0 {
            return Err(ServiceFailure::PairingRequired);
        }
        Ok(RuntimeLifecycleAuthority {
            principal,
            authorization_version,
            epoch: self.state.epoch.ok_or(ServiceFailure::IdentityMismatch)?,
            signer: Arc::clone(&self.signer),
        })
    }
}

impl ServiceSupervisor {
    /// Recovers only existing native publications for this original account,
    /// pin and identifier. It issues no assessment, admission, runtime abort or
    /// native effect. Ordinary discovery stays fenced until recovery completes.
    pub fn reconcile_runtime_lifecycle_storage(
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
        self.runtime_recovery_registration(&registration.bytes, expected_pin)?;
        let journal = directory
            .read_for_recovery("requests", JOURNAL_LIMIT)?
            .ok_or(ServiceFailure::UnknownRequest)?;
        self.runtime_recovery_journal(&journal.bytes, expected_pin, request)?;
        match Directory::open(&self.anchor) {
            Ok(Some(_)) => {}
            Ok(None) => return Err(ServiceFailure::Missing),
            Err(Failure::Indeterminate) => {
                let recovered =
                    directory.reconcile_retained_publication("requests", JOURNAL_LIMIT, |bytes| {
                        self.runtime_recovery_journal(bytes, expected_pin, request)
                            .map_err(|_| Failure::Unsafe)
                    });
                match recovered {
                    Ok(()) => {}
                    Err(Failure::Indeterminate) => {
                        directory.reconcile_retained_publication(
                            "registration",
                            REGISTRATION_LIMIT,
                            |bytes| {
                                self.runtime_recovery_registration(bytes, expected_pin)
                                    .map(|_| ())
                                    .map_err(|_| Failure::Unsafe)
                            },
                        )?;
                    }
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
        let record = self.runtime_recovery_registration(&registration.bytes, expected_pin)?;
        let journal = directory
            .read("requests", JOURNAL_LIMIT)?
            .ok_or(ServiceFailure::UnknownRequest)?;
        self.runtime_recovery_journal(&journal.bytes, expected_pin, request)?;
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
    fn runtime_recovery_registration(
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
    fn runtime_recovery_journal(
        &self,
        bytes: &[u8],
        pin: ServerPin,
        request: NativeRequestId,
    ) -> Result<(), ServiceFailure> {
        let journal: Journal = decode(bytes, JOURNAL_LIMIT)?;
        journal.validate(self.key)?;
        let reservation = &journal.reservations[journal.reservation(request)?];
        if reservation.owner.is_some()
            || reservation.install.is_some()
            || reservation.stopped_native.is_some()
            || reservation.running_stage.is_some()
            || (reservation.service.is_some() && reservation.runtime_intent.is_none())
        {
            return Err(ServiceFailure::RequestConflict);
        }
        if let Some(intent) = &reservation.runtime_intent
            && (intent.basis.server != pin.identity || intent.basis.public_key != pin.public_key)
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        Ok(())
    }
    /// Resolves only this journal-validated signed paired original. Plain
    /// Reserved is read-only; interrupted reservation permits only exact original
    /// non-admission cleanup. Admission and replacement requests are never issued.
    pub fn resolve_runtime_lifecycle(
        &self,
        expected_key: AccountBuildKey,
        expected_pin: ServerPin,
        request: NativeRequestId,
    ) -> ServiceFuture<'_, ServiceProgress> {
        Box::pin(async move {
            self.reconcile_runtime_lifecycle_storage(expected_key, expected_pin, request)?;
            let capture = self.restore_runtime_request(request)?;
            self.restore_runtime_lifecycle_reference(capture)?;
            self.resolve(request).await
        })
    }
    /// Retains an exact current durable capture for this in-process owner. An
    /// uncertain snapshot, forwarded comparison or retired ID cannot mint a token.
    pub fn native_lifecycle_owner_token(
        &self,
        expected_key: AccountBuildKey,
        expected_pin: ServerPin,
        capture: NativeLifecycleRequest,
    ) -> Result<NativeLifecycleOwnerToken, ServiceFailure> {
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
        let reservation = &journal.reservations[journal.reservation(capture.request)?];
        let intent = reservation
            .runtime_intent
            .as_ref()
            .ok_or(ServiceFailure::RequestConflict)?;
        if intent.basis.server != expected_pin.identity
            || intent.basis.public_key != expected_pin.public_key
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        if intent.reference(self.key, capture.request)? != capture {
            return Err(ServiceFailure::RequestConflict);
        }
        if let Some(record) = &reservation.service {
            self.runtime_reference(capture, record, intent)?;
        }
        directory.verify()?;
        lease.verify()?;
        Ok(NativeLifecycleOwnerToken {
            directory,
            anchor: self.anchor.clone(),
            key: self.key,
            pin: expected_pin,
            capture,
            namespace: journal.namespace,
            high_water: journal.high_water,
        })
    }
    /// Observes only the original previously validated by this in-process owner.
    /// RetiredOriginal is a worker-drain classification, not terminal progress,
    /// an acknowledgment match, or permission to retire a client journal entry.
    pub fn poll_owned_runtime_lifecycle_handoff(
        &self,
        token: &NativeLifecycleOwnerToken,
    ) -> Result<NativeLifecycleHandoff, ServiceFailure> {
        if self.key != token.key || self.anchor != token.anchor {
            return Err(ServiceFailure::IdentityMismatch);
        }
        // Retained parent/anchor descriptors reject a replaced object before any
        // original recovery or expiry publication can use the current pathname.
        match token.directory.verify() {
            Ok(()) => {}
            Err(Failure::Indeterminate) => {
                self.reconcile_runtime_lifecycle_storage(
                    token.key,
                    token.pin,
                    token.capture.request,
                )?;
                token.directory.verify()?;
            }
            Err(error) => return Err(error.into()),
        }
        match self.poll_runtime_lifecycle_handoff(token.key, token.pin, token.capture) {
            Ok(handoff) => return Ok(handoff),
            Err(ServiceFailure::RequestRetired) => {}
            Err(error) => return Err(error),
        }
        // Missing originals cannot recover a publication or clear an uncertainty
        // latch. Only ordinary current known-durable state proves safe retirement.
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
        // The authentic row existed at the retained baseline. Allocation has no
        // holes or reuse. Only unreserved non-admission and acknowledged terminal
        // receipts can disappear; Accepted, reserving and unconsumed outcomes cannot.
        journal.sequence(token.capture.request)?;
        match journal.reservation(token.capture.request) {
            Err(ServiceFailure::RequestRetired) => {}
            Ok(_) => return Err(ServiceFailure::Pending),
            Err(error) => return Err(error),
        }
        token.directory.verify()?;
        lease.verify()?;
        Ok(NativeLifecycleHandoff::RetiredOriginal {
            original: token.capture,
        })
    }
    /// Observes the original handoff under the same lease as paired admission.
    /// Pending or uncertain storage is not non-admission proof and must be retried.
    /// Interrupted actor reservation is resolved through the existing exact cleanup,
    /// never by expiry alone. No process, manager or actor operation runs here.
    pub fn poll_runtime_lifecycle_handoff(
        &self,
        expected_key: AccountBuildKey,
        expected_pin: ServerPin,
        capture: NativeLifecycleRequest,
    ) -> Result<NativeLifecycleHandoff, ServiceFailure> {
        self.reconcile_runtime_lifecycle_storage(expected_key, expected_pin, capture.request)?;
        let lease = self.lifecycle_lease()?;
        // This owner observation may read the existing acknowledged receipt.
        // Ordinary restore/resolve/admission keep their retirement refusal.
        let (_, _, journal) = self.read_journal()?;
        let reservation = &journal.reservations[journal.reservation(capture.request)?];
        let intent = reservation
            .runtime_intent
            .clone()
            .ok_or(ServiceFailure::RequestConflict)?;
        if intent.reference(self.key, capture.request)? != capture {
            return Err(ServiceFailure::RequestConflict);
        }
        let mut original = reservation.service.clone();
        if let Some(record) = &original {
            self.runtime_reference(capture, record, &intent)?;
            if record.phase != ServicePhase::Reserved || record.reserving {
                lease.verify()?;
                return Ok(NativeLifecycleHandoff::ResolveOriginal {
                    progress: record.progress(capture.request),
                });
            }
        }
        let now = self.clock.now()?;
        if now.boot == [0; 32]
            || (now.boot == intent.issued.boot && now.millis < intent.issued.millis)
        {
            return Err(ServiceFailure::SupervisorUnavailable);
        }
        let expires_at = intent.expires_at()?;
        lease.verify()?;
        if now.boot == expires_at.boot && now.millis < expires_at.millis {
            return Ok(NativeLifecycleHandoff::Awaiting { expires_at });
        }
        // No reserving marker exists under the admission lease. Signed comparison
        // data is preserved while this exact original becomes terminal NotApplied.
        // Unsigned captures remain unchanged; their immutable boot-bound expiry
        // already excludes future assessment and admission.
        let progress = if let Some(record) = &mut original {
            record.phase = ServicePhase::Completed;
            record.state = ServiceRequestState::NotApplied(ServiceFailure::StaleAssessment);
            self.write_request(capture.request, record)?;
            Some(record.progress(capture.request))
        } else {
            None
        };
        Ok(NativeLifecycleHandoff::ExpiredNonAdmission {
            original: capture,
            progress,
        })
    }
    /// Captures the original registration, exact action and expiry before any
    /// remote assessment. Repeats restore the same capture without extending it.
    pub fn capture_runtime_request(
        &self,
        request: NativeRequestId,
        action: ServiceAction,
    ) -> Result<NativeLifecycleRequest, ServiceFailure> {
        let lease = self.lifecycle_lease()?;
        let (_, _, journal) = self.read_journal()?;
        let reservation = &journal.reservations[journal.reservation(request)?];
        if reservation.acknowledged {
            return Err(ServiceFailure::RequestRetired);
        }
        if let Some(intent) = &reservation.runtime_intent {
            if intent.action != action {
                return Err(ServiceFailure::RequestConflict);
            }
            return intent.reference(self.key, request);
        }
        if reservation.service.is_some()
            || reservation.owner.is_some()
            || reservation.install.is_some()
            || reservation.stopped_native.is_some()
            || reservation.running_stage.is_some()
        {
            return Err(ServiceFailure::RequestConflict);
        }
        self.ensure_no_pending(request)?;
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        self.validate_lifecycle_action(&registration, action)?;
        let mut token = [0; 32];
        getrandom::fill(&mut token).map_err(|_| ServiceFailure::StorageUnavailable)?;
        let intent = RuntimeIntent {
            action,
            basis: RegistrationBasis::capture(&registration),
            issued: self.clock.now()?,
            token,
        };
        intent.validate()?;
        lease.verify()?;
        self.mutate_request(request, |journal, index| {
            let reservation = &mut journal.reservations[index];
            if reservation.service.is_some()
                || reservation.owner.is_some()
                || reservation.install.is_some()
                || reservation.runtime_intent.is_some()
                || reservation.stopped_native.is_some()
                || reservation.running_stage.is_some()
            {
                return Err(ServiceFailure::RequestConflict);
            }
            reservation.runtime_intent = Some(intent.clone());
            Ok(())
        })?;
        intent.reference(self.key, request)
    }
    /// Restores only the original guest-issued capture, even after a lost reply.
    pub fn restore_runtime_request(
        &self,
        request: NativeRequestId,
    ) -> Result<NativeLifecycleRequest, ServiceFailure> {
        let (_, _, journal) = self.read_journal()?;
        let reservation = &journal.reservations[journal.reservation(request)?];
        if reservation.acknowledged {
            return Err(ServiceFailure::RequestRetired);
        }
        reservation
            .runtime_intent
            .as_ref()
            .ok_or(ServiceFailure::RequestConflict)?
            .reference(self.key, request)
    }
    /// Restores the exact signed original comparison without reassessment or
    /// admission authority. Expired references retain their original expiry.
    pub fn restore_runtime_lifecycle_reference(
        &self,
        capture: NativeLifecycleRequest,
    ) -> Result<NativeLifecycleAssessmentRef, ServiceFailure> {
        let intent = self.runtime_intent(capture)?;
        let record = self
            .original(capture.request)?
            .ok_or(ServiceFailure::RequestConflict)?;
        self.runtime_reference(capture, &record, &intent)
    }
    /// Acknowledges only the exact terminal result of a signed paired lifecycle
    /// action. The bounded journal retains the original signed
    /// receipt for exact lost-reply retries; evicted receipts return RequestRetired.
    pub fn acknowledge_runtime_lifecycle_exact(
        &self,
        progress: &ServiceProgress,
    ) -> Result<(), ServiceFailure> {
        self.reconcile_storage(progress.request)?;
        let lease = self.lifecycle_lease()?;
        self.mutate_request(progress.request, |journal, index| {
            let reservation = &mut journal.reservations[index];
            let record = reservation
                .service
                .as_ref()
                .ok_or(ServiceFailure::PairingRequired)?;
            let intent = reservation
                .runtime_intent
                .as_ref()
                .ok_or(ServiceFailure::PairingRequired)?;
            let origin = record
                .runtime_origin
                .as_ref()
                .ok_or(ServiceFailure::PairingRequired)?;
            origin.validate(self.key, progress.request, record, intent)?;
            if record.progress(progress.request) != *progress {
                return Err(ServiceFailure::RequestConflict);
            }
            let RequestBinding::Confirmed(assessment) = &record.binding else {
                return Err(ServiceFailure::PairingRequired);
            };
            if !matches!(
                assessment.action,
                ServiceAction::Stop
                    | ServiceAction::Restart
                    | ServiceAction::Activate(_)
                    | ServiceAction::Uninstall
            ) {
                return Err(ServiceFailure::RequestConflict);
            }
            if record.phase != ServicePhase::Completed
                || !matches!(
                    record.state,
                    ServiceRequestState::Completed(
                        ServiceOutcome::Stopped
                            | ServiceOutcome::Restarted
                            | ServiceOutcome::Activated(_)
                            | ServiceOutcome::Uninstalled
                    ) | ServiceRequestState::NotApplied(_)
                )
            {
                return Err(ServiceFailure::Pending);
            }
            lease.verify()?;
            reservation.acknowledged = true;
            Ok(())
        })
    }
    fn runtime_intent(
        &self,
        capture: NativeLifecycleRequest,
    ) -> Result<RuntimeIntent, ServiceFailure> {
        let (_, _, journal) = self.read_journal()?;
        let reservation = &journal.reservations[journal.reservation(capture.request)?];
        if reservation.acknowledged {
            return Err(ServiceFailure::RequestRetired);
        }
        let intent = reservation
            .runtime_intent
            .clone()
            .ok_or(ServiceFailure::RequestConflict)?;
        if intent.reference(self.key, capture.request)? != capture {
            return Err(ServiceFailure::RequestConflict);
        }
        Ok(intent)
    }
    fn runtime_reference(
        &self,
        capture: NativeLifecycleRequest,
        record: &RequestRecord,
        intent: &RuntimeIntent,
    ) -> Result<NativeLifecycleAssessmentRef, ServiceFailure> {
        let RequestBinding::Confirmed(assessment) = &record.binding else {
            return Err(ServiceFailure::RequestConflict);
        };
        let origin = record
            .runtime_origin
            .as_ref()
            .ok_or(ServiceFailure::RequestConflict)?;
        origin.validate(self.key, capture.request, record, intent)?;
        Ok(NativeLifecycleAssessmentRef {
            original: capture,
            request_digest: record.digest,
            proof_digest: origin.digest()?,
            epoch: assessment
                .runtime
                .epoch
                .ok_or(ServiceFailure::IdentityMismatch)?,
            affected_sessions: assessment.runtime.sessions.len() as u32,
            expires_at: intent.expires_at()?,
        })
    }
    /// Assesses using the live runtime and retains its signed exact origin.
    /// Client input never supplies a RuntimeAssessment or a pairing principal.
    pub fn assess_runtime_lifecycle<'a>(
        &'a self,
        capture: NativeLifecycleRequest,
        authority: &'a RuntimeLifecycleAuthority,
        runtime: &'a dyn ServiceProcesses,
    ) -> ServiceFuture<'a, NativeLifecycleAssessmentRef> {
        Box::pin(async move {
            let lease = self.lifecycle_lease()?;
            let intent = self.runtime_intent(capture)?;
            intent.check_time(self.clock.now()?)?;
            if let Some(record) = self.original(capture.request)? {
                if !record
                    .runtime_origin
                    .as_ref()
                    .is_some_and(|origin| origin.matches(authority))
                {
                    return Err(ServiceFailure::PairingRequired);
                }
                return self.runtime_reference(capture, &record, &intent);
            }
            self.ensure_no_pending(capture.request)?;
            let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
            if !intent.basis.matches(&registration)
                || authority.signer.public_key() != registration.server_pin().public_key
            {
                return Err(ServiceFailure::StaleAssessment);
            }
            let scope: RuntimeAssessment = runtime.assess(&registration).await?;
            scope.validate(&registration)?;
            if scope.epoch != Some(authority.epoch) || scope.server != authority.principal.server()
            {
                return Err(ServiceFailure::IdentityMismatch);
            }
            let binding = RequestBinding::Confirmed(AssessmentRecord {
                action: intent.action,
                basis: intent.basis.clone(),
                runtime: scope,
                issued: intent.issued,
                token: intent.token,
            });
            let mut record = RequestRecord {
                digest: binding.digest(self.key, capture.request)?,
                binding,
                phase: ServicePhase::Reserved,
                state: ServiceRequestState::Pending,
                reserving: false,
                aborting: false,
                runtime_origin: None,
            };
            let mut origin = RuntimeOriginProof {
                claims: OriginClaims {
                    capture,
                    server: authority.principal.server(),
                    epoch: authority.epoch,
                    device: authority.principal.device(),
                    pairing: authority.principal.pairing(),
                    client: authority.principal.client(),
                    authorization_version: authority.authorization_version,
                },
                signature: Vec::new(),
            };
            origin.signature = authority
                .signer
                .sign(&origin.transcript(self.key, capture.request, record.digest)?)
                .map_err(|_| ServiceFailure::StorageUnavailable)?
                .to_vec();
            record.runtime_origin = Some(origin);
            intent.check_time(self.clock.now()?)?;
            if !intent
                .basis
                .matches(&self.discover()?.ok_or(ServiceFailure::Missing)?)
            {
                return Err(ServiceFailure::StaleAssessment);
            }
            lease.verify()?;
            self.write_request(capture.request, &record)?;
            self.runtime_reference(capture, &record, &intent)
        })
    }
    /// Durably admits the exact retained paired-origin request and returns at
    /// Accepted. Only the native owner resolves its subsequent service effects.
    pub fn admit_runtime_lifecycle<'a>(
        &'a self,
        reference: NativeLifecycleAssessmentRef,
        authority: &'a RuntimeLifecycleAuthority,
        runtime: &'a dyn ServiceProcesses,
    ) -> ServiceFuture<'a, ServiceProgress> {
        Box::pin(async move {
            let request = reference.original.request;
            let lease = self.lifecycle_lease()?;
            let intent = self.runtime_intent(reference.original)?;
            let mut record = self.original(request)?.ok_or(ServiceFailure::Missing)?;
            if !record
                .runtime_origin
                .as_ref()
                .is_some_and(|origin| origin.matches(authority))
            {
                return Err(ServiceFailure::PairingRequired);
            }
            if self.runtime_reference(reference.original, &record, &intent)? != reference {
                return Err(ServiceFailure::RequestConflict);
            }
            if record.phase != ServicePhase::Reserved {
                return Ok(record.progress(request));
            }
            intent.check_time(self.clock.now()?)?;
            self.ensure_no_pending(request)?;
            if record.reserving {
                return Err(ServiceFailure::Pending);
            }
            self.admit_record(request, &mut record, &lease, Some(runtime))
                .await?;
            Ok(record.progress(request))
        })
    }
}
