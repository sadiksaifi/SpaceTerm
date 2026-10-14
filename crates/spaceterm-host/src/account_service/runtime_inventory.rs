//! Signed, account-local runtime custody records. Wire replies never carry native
//! cleanup proofs. Only a validated registration can read this private record.

pub(super) mod lineage;
mod stopped_owner;
use stopped_owner::StoppedEpoch;
pub use stopped_owner::StoppedRuntimeCustody;

use std::sync::Arc;

use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use spaceterm_core::{
    authority::{ServerEpoch, SessionKey},
    identity::ServerId,
    native_management::{NativeManagementRequestRef, NativeRetirementAction},
    retirement::{CleanupResult, ConfirmedCleanupClass, SessionCleanupObservation},
};
use spaceterm_protocol::authentication::SigningKey;

use super::{RegisteredServer, RuntimeAssessment, ServiceFailure, ServiceFuture, ServiceProcesses};

/// Maximum native lifecycle comparison or custody record size.
pub const RUNTIME_MESSAGE_LIMIT: usize = spaceterm_protocol::limits::MAX_ACCOUNT_RUNTIME_BYTES;
const RECORD_LIMIT: usize = 256 * 1024;
const STARTING: &str = "runtime-starting";
const RECORD: &str = "runtime-custody";
const DOMAIN: &[u8] = b"spaceterm-account-runtime-custody-v1\0";

/// Comparison data on an already paired, authenticated local connection. These
/// fields cannot authorize native retirement without the retained host journal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum AccountRuntimeCall {
    /// Reads the live actor scope without changing admissions.
    Assess,
    /// Assesses the guest-captured original request using the actual paired principal.
    AssessNativeLifecycle(super::NativeLifecycleRequest),
    /// Admits only this retained exact assessment; never advances native effects.
    AdmitNativeLifecycle(super::NativeLifecycleAssessmentRef),
    /// Assesses the exact original signed-package Stage without reserving Sessions.
    AssessNativeStage(super::NativeStageRequest),
    /// Admits Stage with current paired authority in one serialized actor turn.
    AdmitNativeStage(super::NativeStageAssessmentRef),
    /// Reserves admissions against the original actor-issued scope.
    Reserve(RuntimeAssessment),
    /// Releases this original reservation after durable non-admission.
    Abort(RuntimeAssessment),
    /// Starts only the host-journal-authorized retirement.
    Quiesce {
        /// Original provider-issued request and digest.
        request: NativeManagementRequestRef,
        /// Fixed action independently checked in the host journal.
        action: NativeRetirementAction,
        /// Exact original all-client scope.
        assessment: RuntimeAssessment,
    },
}
/// Bounded comparison replies, never native cleanup proofs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum AccountRuntimeReply {
    /// Original actor-issued assessment.
    Assessment(RuntimeAssessment),
    /// Bounded summary and reference; the authority remains in the native journal.
    NativeLifecycleAssessment(super::NativeLifecycleAssessmentRef),
    /// Exact durable original acceptance, without native service effects.
    NativeLifecycleAccepted(super::ServiceProgress),
    /// Exact original signed-package Stage comparison; no code is approved.
    NativeStageAssessment(super::NativeStageAssessmentRef),
    /// Durable original Stage acceptance, with no archive or manager effects.
    NativeStageAccepted(super::ServiceProgress),
    /// This original scope owns the admission barrier.
    Reserved,
    /// The original token can no longer reserve admissions.
    Aborted,
    /// The server owns completion even after this connection disappears.
    QuiescenceAccepted,
    /// Typed content-free failure.
    Failed(ServiceFailure),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cleanup {
    request: NativeManagementRequestRef,
    action: NativeRetirementAction,
    assessment: RuntimeAssessment,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeNonAdmission {
    request: NativeManagementRequestRef,
    assessment: RuntimeAssessment,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct State {
    schema: u16,
    pub(super) server: ServerId,
    pub(super) epoch: Option<ServerEpoch>,
    instance: [u8; 32],
    generation: u64,
    clean: bool,
    cleanup: Option<Cleanup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    native_non_admission: Option<NativeNonAdmission>,
    #[serde(default)]
    graceful: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<InstanceOwner>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stopped_owner: Option<StoppedEpoch>,
}

/// Signed native incarnation. The boot prevents PID or process-birth reuse
/// across reboot from inheriting a previous instance's runtime authority.
#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InstanceOwner {
    pub(super) boot: [u8; 32],
    pub(super) pid: u32,
    pub(super) birth: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Signed {
    body: Vec<u8>,
    signature: Vec<u8>,
}

/// The actual runtime's privately held publication scope. It cannot be decoded
/// from a client request or reconstructed from an empty runtime registry.
pub struct RuntimeCustody {
    pub(super) state: State,
    pub(super) signer: Arc<dyn SigningKey>,
}
impl RuntimeCustody {
    /// Publishes a fresh unclean instance before opening any runtime listeners.
    /// A previous clean record cannot prove absence of this new instance.
    pub fn begin(
        registration: &RegisteredServer,
        epoch: ServerEpoch,
        signer: Arc<dyn SigningKey>,
    ) -> Result<Self, ServiceFailure> {
        if epoch.get() == 0 || signer.public_key() != registration.server_pin().public_key {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let previous = read(registration)?;
        if previous
            .as_ref()
            .is_some_and(|(state, _)| state.epoch.is_some_and(|old| old.get() >= epoch.get()))
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let old_instance = previous.as_ref().map(|(state, _)| state.instance);
        let owner = super::native_processes::instance_owner()?;
        let instance = match read_marker(registration)? {
            Some(marker) if Some(marker.instance) != old_instance => {
                if marker.owner != Some(owner)
                    || marker.epoch.is_some_and(|prepared| prepared != epoch)
                {
                    return Err(ServiceFailure::IdentityMismatch);
                }
                marker.instance
            }
            _ => Self::prepare_start_for_epoch(registration, signer.as_ref(), epoch)?,
        };
        let state = State {
            schema: 1,
            server: ServerId::new(registration.server_pin().identity),
            epoch: Some(epoch),
            instance,
            generation: previous.map_or(Ok(1), |(state, _)| {
                state
                    .generation
                    .checked_add(1)
                    .ok_or(ServiceFailure::LimitExceeded)
            })?,
            clean: false,
            cleanup: None,
            native_non_admission: None,
            graceful: false,
            owner: Some(owner),
            stopped_owner: None,
        };
        publish(registration, &state, signer.as_ref())?;
        Ok(Self { state, signer })
    }

    /// Fences native absence before recovery can launch any external work. The
    /// prior signed custody remains available for previous-instance cleanup.
    pub fn prepare_start(
        registration: &RegisteredServer,
        signer: &dyn SigningKey,
    ) -> Result<[u8; 32], ServiceFailure> {
        Self::prepare_start_inner(registration, signer, None)
    }

    /// Retains the prospective epoch before recovery can commit it. If the
    /// process dies between epoch commit and begin, the next owner reconciles
    /// this exact attempt rather than replacing the original organization.
    pub fn prepare_start_for_epoch(
        registration: &RegisteredServer,
        signer: &dyn SigningKey,
        epoch: ServerEpoch,
    ) -> Result<[u8; 32], ServiceFailure> {
        if epoch.get() == 0 {
            return Err(ServiceFailure::InvalidRecord);
        }
        Self::prepare_start_inner(registration, signer, Some(epoch))
    }

    fn prepare_start_inner(
        registration: &RegisteredServer,
        signer: &dyn SigningKey,
        epoch: Option<ServerEpoch>,
    ) -> Result<[u8; 32], ServiceFailure> {
        if signer.public_key() != registration.server_pin().public_key {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let mut instance = [0; 32];
        getrandom::fill(&mut instance).map_err(|_| ServiceFailure::StorageUnavailable)?;
        let body = serde_json::to_vec(&Starting {
            schema: 1,
            server: ServerId::new(registration.server_pin().identity),
            instance,
            epoch,
            owner: Some(super::native_processes::instance_owner()?),
        })
        .map_err(|_| ServiceFailure::InvalidRecord)?;
        let bytes = serde_json::to_vec(&Signed {
            signature: signer
                .sign(&signed_bytes(&body))
                .map_err(|_| ServiceFailure::StorageUnavailable)?
                .to_vec(),
            body,
        })
        .map_err(|_| ServiceFailure::InvalidRecord)?;
        let previous = registration.directory.read(STARTING, RECORD_LIMIT)?;
        registration
            .directory
            .publish(STARTING, &bytes, previous.as_ref())?;
        Ok(instance)
    }
    /// Publishes ordinary instance cleanup after the actor, runtime workers,
    /// listeners and started writes have joined. This changes no desired intent
    /// and never manufactures a native request or its replay authority.
    pub fn completed_instance(
        &self,
        registration: &RegisteredServer,
    ) -> Result<(), ServiceFailure> {
        let (current, _) = read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
        if current.instance != self.state.instance || current.epoch != self.state.epoch {
            return Err(ServiceFailure::IdentityMismatch);
        }
        if current.clean {
            return Ok(());
        }
        let mut completed = current;
        completed.clean = true;
        completed.graceful = true;
        publish(registration, &completed, self.signer.as_ref())
    }

    /// Retains completion of only an originally proved-unaccepted actor barrier.
    /// This is not a retirement result and does not claim native process absence.
    pub fn reservation_aborted(
        &self,
        registration: &RegisteredServer,
        request: NativeManagementRequestRef,
        assessment: &RuntimeAssessment,
    ) -> Result<(), ServiceFailure> {
        assessment.validate(registration)?;
        if assessment.epoch != self.state.epoch || request.server() != self.state.server {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let (mut current, _) = read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
        if current.instance != self.state.instance
            || current.epoch != self.state.epoch
            || !marker_matches(registration, &current)?
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let original = NativeNonAdmission {
            request,
            assessment: assessment.clone(),
        };
        if current.native_non_admission.as_ref() == Some(&original) {
            return Ok(());
        }
        current.native_non_admission = Some(original);
        publish(registration, &current, self.signer.as_ref())
    }

    /// Retains immutable parent facts for same-boot crash reconciliation. A missed
    /// checkpoint never grants absence; unknown ancestry still refuses recovery.
    pub fn checkpoint_lineage(
        &self,
        registration: &RegisteredServer,
    ) -> Result<(), ServiceFailure> {
        lineage::checkpoint(registration, self)
    }

    /// Records exact joined cleanup only after the actor confirms all descendants,
    /// started writes and listeners. The original host request is retained.
    pub fn completed(
        &self,
        registration: &RegisteredServer,
        request: NativeManagementRequestRef,
        action: NativeRetirementAction,
        assessment: &RuntimeAssessment,
        observations: &[SessionCleanupObservation],
    ) -> Result<(), ServiceFailure> {
        assessment.validate(registration)?;
        super::processes::cleanup_matches(&assessment.sessions, observations)?;
        if observations
            .iter()
            .any(|observation| observation.first_reason() != action.reason())
        {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        if assessment.epoch != self.state.epoch || request.server() != self.state.server {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let (current, _) = read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
        let cleanup = Cleanup {
            request,
            action,
            assessment: assessment.clone(),
        };
        if current.instance != self.state.instance {
            return Err(ServiceFailure::IdentityMismatch);
        }
        if current.clean {
            return if current.cleanup.as_ref() == Some(&cleanup) {
                Ok(())
            } else {
                Err(ServiceFailure::RequestConflict)
            };
        }
        let mut completed = self.state.clone();
        completed.clean = true;
        completed.cleanup = Some(cleanup);
        publish(registration, &completed, self.signer.as_ref())
    }
}

/// Opaque native process-scope evidence. Only the account process provider can
/// issue it after checking the manager scope and listener under startup admission.
/// It is not serializable and is never accepted from the protocol.
pub struct NativeInstanceAbsence {
    pub(super) key: crate::startup::AccountBuildKey,
    pub(super) server: ServerId,
    pub(super) epoch: ServerEpoch,
    pub(super) boot: [u8; 32],
    pub(super) observer: Arc<dyn super::native_processes::InstanceProcessObserver>,
}

/// Native absence/replay authority shared by callers after reconstruction. It
/// never treats a missing record, unavailable listener or dead PID as cleanup.
pub struct RetainedRuntimeProcesses;
impl RetainedRuntimeProcesses {
    fn reconcile(
        registration: &RegisteredServer,
        epoch: ServerEpoch,
        expected: &[spaceterm_core::retirement::RetiredSession],
        absence: Option<&NativeInstanceAbsence>,
    ) -> Result<Vec<SessionCleanupObservation>, ServiceFailure> {
        let sessions: Vec<_> = expected.iter().map(|value| value.session()).collect();
        super::processes::validate_previous_scope(
            ServerId::new(registration.server_pin().identity),
            epoch,
            &sessions,
        )?;
        let (state, _) = read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
        let old_epoch = state.epoch.unwrap_or(ServerEpoch::new(0));
        let pending = read_marker(registration)?.filter(|marker| marker.instance != state.instance);
        if old_epoch != epoch
            && !pending.as_ref().is_some_and(|marker| {
                marker.epoch == Some(epoch) && old_epoch.checked_successor() == Some(epoch)
            })
        {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        if !state.clean || pending.is_some() {
            let absence = absence.ok_or(ServiceFailure::CleanupUnresolved)?;
            if absence.key != registration.key()
                || absence.server != state.server
                || absence.epoch != epoch
            {
                return Err(ServiceFailure::IdentityMismatch);
            }
            if !state.clean {
                super::native_processes::previous_owner_absent(
                    state.owner.ok_or(ServiceFailure::CleanupUnresolved)?,
                    absence.boot,
                    &lineage::retained(registration, &state)?,
                    absence.observer.as_ref(),
                )?;
            }
            if let Some(marker) = pending {
                super::native_processes::previous_owner_absent(
                    marker.owner.ok_or(ServiceFailure::CleanupUnresolved)?,
                    absence.boot,
                    &[],
                    absence.observer.as_ref(),
                )?;
            }
        }
        registration
            .endpoint()?
            .prove_no_listener()
            .map_err(|_| ServiceFailure::CleanupUnresolved)?;
        // Whole-instance cleanup and exact native scope absence prove old-epoch
        // ownership. The validated store supplies each original first reason;
        // the historical native request remains unchanged for exact replay.
        expected
            .iter()
            .map(|expected| {
                SessionCleanupObservation::new(
                    expected.session(),
                    expected.first_reason(),
                    CleanupResult::Confirmed(
                        ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership,
                    ),
                )
                .map_err(|_| ServiceFailure::InvalidRecord)
            })
            .collect()
    }

    /// Resolves only the exact original native request from signed local custody.
    pub fn cleanup(
        registration: &RegisteredServer,
        request: NativeManagementRequestRef,
        action: NativeRetirementAction,
        assessment: &RuntimeAssessment,
    ) -> Result<Vec<SessionCleanupObservation>, ServiceFailure> {
        assessment.validate(registration)?;
        let (state, _) = read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
        if !state.clean
            || state.cleanup.as_ref()
                != Some(&Cleanup {
                    request,
                    action,
                    assessment: assessment.clone(),
                })
        {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        observations(&assessment.sessions, action)
    }
    /// Reads custody state without treating missing metadata as evidence.
    pub fn is_clean(registration: &RegisteredServer) -> Result<bool, ServiceFailure> {
        let Some((state, _)) = read(registration)? else {
            return Ok(false);
        };
        Ok(state.clean
            && !state
                .stopped_owner
                .as_ref()
                .is_some_and(|owner| owner.pending)
            && marker_matches(registration, &state)?)
    }
    fn absent(registration: &RegisteredServer) -> Result<(State, [u8; 32]), ServiceFailure> {
        let (state, digest) = read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
        if !state.clean
            || state
                .stopped_owner
                .as_ref()
                .is_some_and(|owner| owner.pending)
            || !marker_matches(registration, &state)?
        {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        registration
            .endpoint()?
            .prove_no_listener()
            .map_err(|_| ServiceFailure::CleanupUnresolved)?;
        Ok((state, digest))
    }
    /// Adds physical instance absence to signed clean custody. Manager scope
    /// alone cannot discharge a retained owner that escaped or outlived it.
    pub(super) fn verify_native_instance_absent(
        registration: &RegisteredServer,
        boot: [u8; 32],
        observer: &dyn super::native_processes::InstanceProcessObserver,
    ) -> Result<(), ServiceFailure> {
        if boot == [0; 32] {
            return Err(ServiceFailure::SupervisorUnavailable);
        }
        let (state, _) = Self::absent(registration)?;
        let marker = read_marker(registration)?;
        let retained = lineage::retained(registration, &state)?;
        if let Some(owner) = state.owner {
            super::native_processes::previous_owner_absent(owner, boot, &retained, observer)?;
        } else if state.epoch.is_some()
            && state.stopped_owner.is_none()
            && marker.as_ref().is_none_or(|marker| marker.owner.is_none())
        {
            // A prior runtime with no retained birth cannot be reinterpreted as
            // never started. Completed runtime-free stopped epochs are distinct.
            return Err(ServiceFailure::CleanupUnresolved);
        }
        if let Some(owner) = marker.and_then(|marker| marker.owner)
            && Some(owner) != state.owner
        {
            super::native_processes::previous_owner_absent(owner, boot, &retained, observer)?;
        }
        Ok(())
    }
    fn token(
        registration: &RegisteredServer,
        assessment: &RuntimeAssessment,
        abort: bool,
    ) -> Result<(), ServiceFailure> {
        let (_, digest) = Self::absent(registration)?;
        let _lease = registration.directory.lease("runtime-token-lock")?;
        let previous = registration
            .directory
            .read("runtime-retired-tokens", RECORD_LIMIT)?;
        let mut retired = match &previous {
            Some(record) => serde_json::from_slice::<RetiredTokens>(&record.bytes)
                .map_err(|_| ServiceFailure::InvalidRecord)?,
            None => RetiredTokens {
                digest,
                tokens: Vec::new(),
            },
        };
        if retired.tokens.len() > 64 {
            return Err(ServiceFailure::InvalidRecord);
        }
        if retired.digest != digest {
            retired = RetiredTokens {
                digest,
                tokens: Vec::new(),
            };
        }
        if retired.tokens.contains(&assessment.token) {
            return if abort {
                Ok(())
            } else {
                Err(ServiceFailure::StaleAssessment)
            };
        }
        if abort {
            if retired.tokens.len() >= 64 {
                return Err(ServiceFailure::LimitExceeded);
            }
            retired.tokens.push(assessment.token);
            let bytes = serde_json::to_vec(&retired).map_err(|_| ServiceFailure::InvalidRecord)?;
            registration
                .directory
                .publish("runtime-retired-tokens", &bytes, previous.as_ref())?;
        }
        Ok(())
    }
    fn check_absent(
        registration: &RegisteredServer,
        assessment: &RuntimeAssessment,
    ) -> Result<(), ServiceFailure> {
        assessment.validate(registration)?;
        let (state, digest) = Self::absent(registration)?;
        if assessment.epoch.is_some()
            || !assessment.sessions.is_empty()
            || assessment.generation != state.generation
            || assessment.token[..16] != digest[..16]
        {
            return Err(ServiceFailure::StaleAssessment);
        }
        Ok(())
    }
}
impl ServiceProcesses for RetainedRuntimeProcesses {
    fn reconcile_previous_instance<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        epoch: ServerEpoch,
        expected: &'a [spaceterm_core::retirement::RetiredSession],
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move { Self::reconcile(registration, epoch, expected, None) })
    }
    fn reconcile_previous_instance_with_absence<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        epoch: ServerEpoch,
        expected: &'a [spaceterm_core::retirement::RetiredSession],
        absence: &'a NativeInstanceAbsence,
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move { Self::reconcile(registration, epoch, expected, Some(absence)) })
    }
    fn assess<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, RuntimeAssessment> {
        Box::pin(async move {
            let (state, digest) = Self::absent(registration)?;
            let mut token = [0; 32];
            getrandom::fill(&mut token).map_err(|_| ServiceFailure::StorageUnavailable)?;
            token[..16].copy_from_slice(&digest[..16]);
            Ok(RuntimeAssessment {
                server: state.server,
                epoch: None,
                generation: state.generation,
                sessions: Vec::new(),
                token,
            })
        })
    }
    fn revalidate<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            Self::check_absent(registration, assessment)?;
            Self::token(registration, assessment, false)
        })
    }
    fn abort_reservation<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            if assessment.epoch.is_some() {
                return Err(ServiceFailure::Pending);
            }
            Self::check_absent(registration, assessment)?;
            Self::token(registration, assessment, true)
        })
    }
    fn abort_native_origin<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        request: NativeManagementRequestRef,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            assessment.validate(registration)?;
            let (state, _) = read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
            if state.epoch != assessment.epoch
                || request.server() != state.server
                || !marker_matches(registration, &state)?
            {
                return Err(ServiceFailure::IdentityMismatch);
            }
            let original = NativeNonAdmission {
                request,
                assessment: assessment.clone(),
            };
            match &state.native_non_admission {
                Some(retained) if *retained == original => Ok(()),
                Some(_) => Err(ServiceFailure::RequestConflict),
                None => Err(ServiceFailure::Pending),
            }
        })
    }
    fn abort_reservation_with_absence<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
        absence: &'a NativeInstanceAbsence,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            assessment.validate(registration)?;
            if assessment.epoch != Some(absence.epoch)
                || absence.key != registration.key()
                || absence.server != assessment.server
                || absence.boot == [0; 32]
            {
                return Err(ServiceFailure::IdentityMismatch);
            }
            let (state, _) = read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
            if state.epoch != assessment.epoch || !marker_matches(registration, &state)? {
                return Err(ServiceFailure::IdentityMismatch);
            }
            super::native_processes::previous_owner_absent(
                state.owner.ok_or(ServiceFailure::CleanupUnresolved)?,
                absence.boot,
                &lineage::retained(registration, &state)?,
                absence.observer.as_ref(),
            )?;
            registration
                .endpoint()?
                .prove_no_listener()
                .map_err(|_| ServiceFailure::CleanupUnresolved)?;
            Ok(())
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
                Self::check_absent(registration, assessment)?;
                Ok(Vec::new())
            } else {
                Self::cleanup(registration, request, action, assessment)
            }
        })
    }
    fn verify_absent<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move { Self::absent(registration).map(|_| ()) })
    }
    fn previous_instance_cleanup<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        epoch: ServerEpoch,
        sessions: &'a [SessionKey],
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move {
            super::processes::validate_previous_scope(
                ServerId::new(registration.server_pin().identity),
                epoch,
                sessions,
            )?;
            let (state, _) = read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
            if !state.clean || state.epoch.unwrap_or(ServerEpoch::new(0)) != epoch {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            if sessions.is_empty()
                && (epoch.get() == 0
                    || state
                        .stopped_owner
                        .as_ref()
                        .is_some_and(|owner| !owner.pending))
            {
                return Ok(Vec::new());
            }
            // Completed native retirement descriptors no longer appear in the
            // store. Their historical assessment is replay authority, not the
            // pending recovery scope. NativeServiceProcesses separately verifies
            // the current manager scope and endpoint before admitting startup.
            if sessions.is_empty() {
                return Ok(Vec::new());
            }
            let cleanup = state.cleanup.ok_or(ServiceFailure::CleanupUnresolved)?;
            if sessions
                .iter()
                .any(|session| cleanup.assessment.sessions.binary_search(session).is_err())
            {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            observations(sessions, cleanup.action)
        })
    }
}

pub(super) fn initialize(
    registration: &RegisteredServer,
    signer: &dyn SigningKey,
) -> Result<(), ServiceFailure> {
    let mut instance = [0; 32];
    getrandom::fill(&mut instance).map_err(|_| ServiceFailure::StorageUnavailable)?;
    let state = State {
        schema: 1,
        server: ServerId::new(registration.server_pin().identity),
        epoch: None,
        instance,
        generation: 1,
        clean: true,
        cleanup: None,
        native_non_admission: None,
        graceful: false,
        owner: None,
        stopped_owner: None,
    };
    // Only exclusive initial registration calls this. Never repair missing custody
    // metadata on an established store by publishing an empty clean instance.
    if let Some((previous, _)) = read(registration)? {
        return if previous.epoch.is_none() && previous.clean && previous.cleanup.is_none() {
            Ok(())
        } else {
            Err(ServiceFailure::RequestConflict)
        };
    }
    publish(registration, &state, signer)
}
fn observations(
    sessions: &[SessionKey],
    action: NativeRetirementAction,
) -> Result<Vec<SessionCleanupObservation>, ServiceFailure> {
    sessions
        .iter()
        .map(|session| {
            SessionCleanupObservation::new(
                *session,
                action.reason(),
                CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
            )
            .map_err(|_| ServiceFailure::InvalidRecord)
        })
        .collect()
}
fn signed_bytes(body: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(DOMAIN.len() + body.len());
    bytes.extend_from_slice(DOMAIN);
    bytes.extend_from_slice(body);
    bytes
}
fn publish(
    registration: &RegisteredServer,
    state: &State,
    signer: &dyn SigningKey,
) -> Result<(), ServiceFailure> {
    if signer.public_key() != registration.server_pin().public_key {
        return Err(ServiceFailure::IdentityMismatch);
    }
    let body = serde_json::to_vec(state).map_err(|_| ServiceFailure::InvalidRecord)?;
    let signature = signer
        .sign(&signed_bytes(&body))
        .map_err(|_| ServiceFailure::StorageUnavailable)?
        .to_vec();
    let bytes = serde_json::to_vec(&Signed { body, signature })
        .map_err(|_| ServiceFailure::InvalidRecord)?;
    if bytes.len() > RECORD_LIMIT {
        return Err(ServiceFailure::LimitExceeded);
    }
    let previous = registration.directory.read(RECORD, RECORD_LIMIT)?;
    registration
        .directory
        .publish(RECORD, &bytes, previous.as_ref())
        .map_err(Into::into)
}
fn read(registration: &RegisteredServer) -> Result<Option<(State, [u8; 32])>, ServiceFailure> {
    let Some(record) = registration.directory.read(RECORD, RECORD_LIMIT)? else {
        return Ok(None);
    };
    let state = decode_state(registration, &record.bytes)?;
    Ok(Some((state, Sha256::digest(&record.bytes).into())))
}

fn decode_state(registration: &RegisteredServer, bytes: &[u8]) -> Result<State, ServiceFailure> {
    let signed: Signed =
        serde_json::from_slice(bytes).map_err(|_| ServiceFailure::InvalidRecord)?;
    if signed.signature.len() != 64 || signed.body.len() > RECORD_LIMIT {
        return Err(ServiceFailure::InvalidRecord);
    }
    UnparsedPublicKey::new(&ED25519, registration.server_pin().public_key)
        .verify(&signed_bytes(&signed.body), &signed.signature)
        .map_err(|_| ServiceFailure::IdentityMismatch)?;
    let state: State =
        serde_json::from_slice(&signed.body).map_err(|_| ServiceFailure::InvalidRecord)?;
    if state.schema != 1
        || state.server.bytes() != registration.server_pin().identity
        || state.instance == [0; 32]
        || state.generation == 0
        || state.epoch.is_some_and(|epoch| epoch.get() == 0)
        || (state.epoch.is_none() && (!state.clean || state.cleanup.is_some() || state.graceful))
        || (state.epoch.is_some()
            && state.stopped_owner.is_none()
            && state.clean != (state.cleanup.is_some() || state.graceful))
        || (state.cleanup.is_some() && state.graceful)
        || state.owner.is_some_and(|owner| {
            owner.boot == [0; 32]
                || owner.pid == 0
                || owner.pid > i32::MAX as u32
                || owner.birth == 0
        })
        || state
            .stopped_owner
            .as_ref()
            .is_some_and(|owner| !owner.validate())
    {
        return Err(ServiceFailure::InvalidRecord);
    }
    if let Some(original) = &state.native_non_admission {
        original.assessment.validate(registration)?;
        if original.request.server() != state.server || original.assessment.epoch != state.epoch {
            return Err(ServiceFailure::InvalidRecord);
        }
    }
    if let Some(cleanup) = &state.cleanup {
        cleanup.assessment.validate(registration)?;
        if cleanup.request.server() != state.server || cleanup.assessment.epoch != state.epoch {
            return Err(ServiceFailure::InvalidRecord);
        }
    }
    Ok(state)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Starting {
    schema: u16,
    server: ServerId,
    instance: [u8; 32],
    #[serde(default)]
    epoch: Option<ServerEpoch>,
    #[serde(default)]
    owner: Option<InstanceOwner>,
}
fn read_marker(registration: &RegisteredServer) -> Result<Option<Starting>, ServiceFailure> {
    let Some(record) = registration.directory.read(STARTING, RECORD_LIMIT)? else {
        return Ok(None);
    };
    decode_marker(registration, &record.bytes).map(Some)
}

fn decode_marker(
    registration: &RegisteredServer,
    bytes: &[u8],
) -> Result<Starting, ServiceFailure> {
    let signed: Signed =
        serde_json::from_slice(bytes).map_err(|_| ServiceFailure::InvalidRecord)?;
    if signed.signature.len() != 64 {
        return Err(ServiceFailure::InvalidRecord);
    }
    UnparsedPublicKey::new(&ED25519, registration.server_pin().public_key)
        .verify(&signed_bytes(&signed.body), &signed.signature)
        .map_err(|_| ServiceFailure::IdentityMismatch)?;
    let marker: Starting =
        serde_json::from_slice(&signed.body).map_err(|_| ServiceFailure::InvalidRecord)?;
    if marker.schema != 1
        || marker.server.bytes() != registration.server_pin().identity
        || marker.instance == [0; 32]
        || marker.epoch.is_some_and(|epoch| epoch.get() == 0)
        || marker.owner.is_some_and(|owner| {
            owner.boot == [0; 32]
                || owner.pid == 0
                || owner.pid > i32::MAX as u32
                || owner.birth == 0
        })
    {
        return Err(ServiceFailure::InvalidRecord);
    }
    Ok(marker)
}

/// Called only under native instance admission. Replays an exact original signed
/// publication; it grants no process absence and never resets unclean custody.
pub(super) fn recover_publications(registration: &RegisteredServer) -> Result<(), ServiceFailure> {
    use crate::private_filesystem::Failure;
    match registration.directory.verify() {
        Ok(()) => return Ok(()),
        Err(Failure::Indeterminate) => {}
        Err(error) => return Err(error.into()),
    }
    for name in [RECORD, STARTING] {
        let result =
            registration
                .directory
                .reconcile_retained_publication(name, RECORD_LIMIT, |bytes| {
                    let valid = if name == RECORD {
                        decode_state(registration, bytes).map(|_| ())
                    } else {
                        decode_marker(registration, bytes).map(|_| ())
                    };
                    valid.map_err(|_| Failure::Unsafe)
                });
        match result {
            Ok(()) | Err(Failure::Conflict) => {
                return registration.directory.verify().map_err(Into::into);
            }
            Err(Failure::Indeterminate) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(ServiceFailure::Indeterminate)
}

fn marker_matches(registration: &RegisteredServer, state: &State) -> Result<bool, ServiceFailure> {
    Ok(match read_marker(registration)? {
        Some(marker) => marker.instance == state.instance,
        None => state.epoch.is_none() || state.stopped_owner.is_some(),
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetiredTokens {
    digest: [u8; 32],
    tokens: Vec<[u8; 32]>,
}
