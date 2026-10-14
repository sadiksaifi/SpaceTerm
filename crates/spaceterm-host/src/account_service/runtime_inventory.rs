//! Signed, account-local runtime custody records. Wire replies never carry native
//! cleanup proofs. Only a validated registration can read this private record.

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
pub const RUNTIME_MESSAGE_LIMIT: usize = 32 * 1024;
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
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: u16,
    server: ServerId,
    epoch: Option<ServerEpoch>,
    instance: [u8; 32],
    generation: u64,
    clean: bool,
    cleanup: Option<Cleanup>,
    #[serde(default)]
    graceful: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<InstanceOwner>,
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
    state: State,
    signer: Arc<dyn SigningKey>,
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
            graceful: false,
            owner: Some(owner),
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
                )?;
            }
            if let Some(marker) = pending {
                super::native_processes::previous_owner_absent(
                    marker.owner.ok_or(ServiceFailure::CleanupUnresolved)?,
                    absence.boot,
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
        Ok(state.clean && marker_matches(registration, &state)?)
    }
    fn absent(registration: &RegisteredServer) -> Result<(State, [u8; 32]), ServiceFailure> {
        let (state, digest) = read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
        if !state.clean || !marker_matches(registration, &state)? {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        registration
            .endpoint()?
            .prove_no_listener()
            .map_err(|_| ServiceFailure::CleanupUnresolved)?;
        Ok((state, digest))
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
            Self::check_absent(registration, assessment)?;
            Self::token(registration, assessment, true)
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
            if epoch.get() == 0 && sessions.is_empty() {
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
        graceful: false,
        owner: None,
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
    let signed: Signed =
        serde_json::from_slice(&record.bytes).map_err(|_| ServiceFailure::InvalidRecord)?;
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
        || (state.epoch.is_some() && state.clean != (state.cleanup.is_some() || state.graceful))
        || (state.cleanup.is_some() && state.graceful)
        || state.owner.is_some_and(|owner| {
            owner.boot == [0; 32]
                || owner.pid == 0
                || owner.pid > i32::MAX as u32
                || owner.birth == 0
        })
    {
        return Err(ServiceFailure::InvalidRecord);
    }
    if let Some(cleanup) = &state.cleanup {
        cleanup.assessment.validate(registration)?;
        if cleanup.request.server() != state.server || cleanup.assessment.epoch != state.epoch {
            return Err(ServiceFailure::InvalidRecord);
        }
    }
    Ok(Some((state, Sha256::digest(&record.bytes).into())))
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
    let signed: Signed =
        serde_json::from_slice(&record.bytes).map_err(|_| ServiceFailure::InvalidRecord)?;
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
    Ok(Some(marker))
}
fn marker_matches(registration: &RegisteredServer, state: &State) -> Result<bool, ServiceFailure> {
    Ok(match read_marker(registration)? {
        Some(marker) => marker.instance == state.instance,
        None => state.epoch.is_none(),
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetiredTokens {
    digest: [u8; 32],
    tokens: Vec<[u8; 32]>,
}
