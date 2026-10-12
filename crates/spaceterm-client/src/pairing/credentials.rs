//! Native key references and revocable, process-local signing leases.

use super::journal::PairingJournalSnapshot;
use serde::{Deserialize, Serialize};
use spaceterm_core::{
    identity::{PairingId, ServerId},
    operations::OperationId,
    pairing::{
        CredentialReference, LocalPairingGeneration, LocalPairingKey, MAX_PENDING_REVOCATIONS,
        PairingFailure,
    },
};
use spaceterm_protocol::authentication::SigningKey;
use std::{
    collections::HashSet,
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};

pub type PairingFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type CredentialFailure = PairingFailure;

/// Local build namespace. Server messages must never supply this value.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct CredentialNamespace(String);
impl CredentialNamespace {
    pub fn new(value: String) -> Result<Self, CredentialFailure> {
        if value.is_empty()
            || value.len() > 96
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        {
            return Err(PairingFailure::InvalidRequest);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for CredentialNamespace {
    type Error = spaceterm_core::pairing::PairingContractError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value).map_err(|_| spaceterm_core::pairing::PairingContractError::Invalid)
    }
}
impl From<CredentialNamespace> for String {
    fn from(value: CredentialNamespace) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CredentialPurpose {
    DeviceIdentity,
    PairingAuthentication { server: ServerId },
}

/// Public, retained metadata around the single canonical opaque slot identity.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "CredentialScopeParts", into = "CredentialScopeParts")]
pub struct CredentialScope(CredentialScopeParts);
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialScopeParts {
    pub namespace: CredentialNamespace,
    pub purpose: CredentialPurpose,
    pub reference: CredentialReference,
}
impl CredentialScope {
    pub fn new(parts: CredentialScopeParts) -> Result<Self, CredentialFailure> {
        if parts.reference.opaque() == [0; 16]
            || matches!(parts.purpose, CredentialPurpose::PairingAuthentication { server } if server.bytes() == [0; 16])
        {
            return Err(PairingFailure::InvalidRequest);
        }
        Ok(Self(parts))
    }
    pub fn parts(&self) -> &CredentialScopeParts {
        &self.0
    }
    pub fn reference(&self) -> CredentialReference {
        self.0.reference
    }
}
impl TryFrom<CredentialScopeParts> for CredentialScope {
    type Error = spaceterm_core::pairing::PairingContractError;
    fn try_from(value: CredentialScopeParts) -> Result<Self, Self::Error> {
        Self::new(value).map_err(|_| spaceterm_core::pairing::PairingContractError::Invalid)
    }
}
impl From<CredentialScope> for CredentialScopeParts {
    fn from(value: CredentialScope) -> Self {
        value.0
    }
}
impl fmt::Debug for CredentialScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CredentialScope([redacted])")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreparedCredential {
    pub scope: CredentialScope,
    pub public_key: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialDeletion {
    Deleted,
    AlreadyMissing,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialUsePurpose {
    Enrollment,
    EnrollmentAcknowledgement { operation: OperationId },
    Ordinary,
    Restricted { operation: OperationId },
}

/// Native UI is allowed only for a deliberate foreground credential action.
/// Ordinary reconnect and purpose-restricted retries remain noninteractive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeCredentialAccess {
    Noninteractive,
    Foreground,
}

/// Cloning a lease does not extend authority. Native signers check it on every sign.
#[derive(Clone)]
pub struct CredentialUse {
    scope: CredentialScope,
    purpose: CredentialUsePurpose,
    valid: Arc<AtomicBool>,
    access: NativeCredentialAccess,
}
impl CredentialUse {
    pub(crate) fn enrollment(scope: CredentialScope, valid: Arc<AtomicBool>) -> Self {
        Self {
            scope,
            purpose: CredentialUsePurpose::Enrollment,
            valid,
            access: NativeCredentialAccess::Foreground,
        }
    }
    pub fn scope(&self) -> &CredentialScope {
        &self.scope
    }
    pub fn native_access(&self) -> NativeCredentialAccess {
        self.access
    }
    pub fn purpose(&self) -> CredentialUsePurpose {
        self.purpose
    }
    pub fn validate(&self, scope: &CredentialScope) -> Result<(), CredentialFailure> {
        if &self.scope != scope || !self.valid.load(Ordering::Acquire) {
            return Err(PairingFailure::AuthorizationDenied);
        }
        if let CredentialUsePurpose::Restricted { operation } = self.purpose
            && self.scope.parts().purpose
                != (CredentialPurpose::PairingAuthentication {
                    server: operation.stream().server(),
                })
        {
            return Err(PairingFailure::IdentityMismatch);
        }
        Ok(())
    }
}
impl fmt::Debug for CredentialUse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CredentialUse([redacted])")
    }
}

#[derive(Clone)]
pub(crate) struct CredentialGate {
    scope: CredentialScope,
    valid: Arc<AtomicBool>,
}
impl CredentialGate {
    pub(crate) fn new(scope: CredentialScope) -> Self {
        Self {
            scope,
            valid: Arc::new(AtomicBool::new(true)),
        }
    }
    pub(crate) fn lease(&self, purpose: CredentialUsePurpose) -> CredentialUse {
        CredentialUse {
            scope: self.scope.clone(),
            purpose,
            valid: self.valid.clone(),
            access: NativeCredentialAccess::Noninteractive,
        }
    }
    pub(crate) fn seal(&self) {
        self.valid.store(false, Ordering::Release);
    }
}

/// One exact local incarnation's ordinary authority, never a reconnect retry loop.
pub struct OrdinaryCredentialAuthority {
    key: LocalPairingKey,
    gate: CredentialGate,
}
impl OrdinaryCredentialAuthority {
    pub(crate) fn new(key: LocalPairingKey, gate: CredentialGate) -> Self {
        Self { key, gate }
    }
    pub fn key(&self) -> &LocalPairingKey {
        &self.key
    }
    pub fn lease(&self) -> CredentialUse {
        self.gate.lease(CredentialUsePurpose::Ordinary)
    }
    /// Deliberate additional-Client authorization, never ordinary reconnect.
    pub fn foreground_lease(&self) -> CredentialUse {
        let mut lease = self.lease();
        lease.access = NativeCredentialAccess::Foreground;
        lease
    }
    pub(crate) fn device_lease(
        &self,
        credential: &PreparedCredential,
    ) -> Result<CredentialUse, CredentialFailure> {
        if credential.scope.parts().purpose != CredentialPurpose::DeviceIdentity
            || credential.scope.parts().namespace != self.gate.scope.parts().namespace
        {
            return Err(PairingFailure::IdentityMismatch);
        }
        let lease = CredentialUse {
            scope: credential.scope.clone(),
            purpose: CredentialUsePurpose::Ordinary,
            valid: self.gate.valid.clone(),
            access: NativeCredentialAccess::Foreground,
        };
        lease.validate(&credential.scope)?;
        Ok(lease)
    }
    pub fn seal(&self) {
        self.gate.seal();
    }
}

type PairingLifetime = (ServerId, PairingId);
fn pairing_lifetime(key: &LocalPairingKey) -> PairingLifetime {
    (key.parts().server, key.parts().pairing)
}

#[derive(Default)]
struct PairingAuthorityState {
    revision: Option<u64>,
    bindings: Vec<(LocalPairingKey, CredentialScope)>,
    gates: Vec<(LocalPairingKey, CredentialGate)>,
    // Only local removals not yet observed as durable suppression need a latch.
    sealed: HashSet<PairingLifetime>,
    closed: bool,
}
impl PairingAuthorityState {
    fn close(&mut self) {
        self.closed = true;
        for (_, gate) in &self.gates {
            gate.seal();
        }
    }
}

/// Owned once by PairingClientJournal. Issuance, recovered membership and local removal
/// share one lock. The latest validated revision cannot be replaced by a stale
/// read, so retired gates can be reclaimed without granting stale snapshots.
/// Failed local removals stay latched until durable suppression is observed.
#[derive(Default)]
pub struct PairingAuthorityFence {
    state: Mutex<PairingAuthorityState>,
}
impl PairingAuthorityFence {
    fn state(&self) -> Result<MutexGuard<'_, PairingAuthorityState>, PairingFailure> {
        match self.state.lock() {
            Ok(state) if !state.closed => Ok(state),
            Ok(_) => Err(PairingFailure::RecoveryRequired),
            Err(poisoned) => {
                poisoned.into_inner().close();
                Err(PairingFailure::RecoveryRequired)
            }
        }
    }
    pub(crate) fn issue(
        &self,
        key: &LocalPairingKey,
        scope: &CredentialScope,
    ) -> Result<OrdinaryCredentialAuthority, PairingFailure> {
        let mut state = self.state()?;
        let lifetime = pairing_lifetime(key);
        if state.sealed.contains(&lifetime)
            || !state
                .bindings
                .iter()
                .any(|(current, credential)| current == key && credential == scope)
        {
            return Err(PairingFailure::AuthorizationDenied);
        }
        let gate = if let Some((current, gate)) = state
            .gates
            .iter()
            .find(|(old, _)| pairing_lifetime(old) == lifetime)
        {
            if current != key || gate.scope != *scope {
                return Err(PairingFailure::CredentialIntegrity);
            }
            gate.clone()
        } else {
            if state.gates.len() >= MAX_PENDING_REVOCATIONS {
                return Err(PairingFailure::LimitExceeded);
            }
            let gate = CredentialGate::new(scope.clone());
            state.gates.push((key.clone(), gate.clone()));
            gate
        };
        gate.lease(CredentialUsePurpose::Ordinary).validate(scope)?;
        Ok(OrdinaryCredentialAuthority::new(key.clone(), gate))
    }
    pub(crate) fn observe(&self, recovered: &PairingJournalSnapshot) -> Result<(), PairingFailure> {
        let suppressed: HashSet<_> = recovered.suppressed.iter().map(pairing_lifetime).collect();
        let mut state = self.state()?;
        // Suppression applies even before the first gate exists. Equal/older
        // reads may narrow membership, but can never restore an absent binding.
        state
            .bindings
            .retain(|(key, _)| !suppressed.contains(&pairing_lifetime(key)));
        state.gates.retain(|(key, gate)| {
            if suppressed.contains(&pairing_lifetime(key)) {
                gate.seal();
                false
            } else {
                true
            }
        });
        state
            .sealed
            .retain(|lifetime| !suppressed.contains(lifetime));
        if state
            .revision
            .is_some_and(|revision| revision >= recovered.revision)
        {
            return Ok(());
        }

        let bindings: Vec<_> = recovered
            .saved
            .iter()
            .filter(|saved| !suppressed.contains(&pairing_lifetime(saved.key())))
            .map(|saved| (saved.key().clone(), saved.credential().scope.clone()))
            .collect();
        state.gates.retain(|(key, gate)| {
            if bindings
                .iter()
                .any(|(current, scope)| current == key && *scope == gate.scope)
            {
                true
            } else {
                // Cached leases keep the sealed Arc after the registry entry
                // is reclaimed. No historical gate consumes live capacity.
                gate.seal();
                false
            }
        });
        state.bindings = bindings;
        state.revision = Some(recovered.revision);
        Ok(())
    }
    pub(crate) fn seal(&self, key: &LocalPairingKey) {
        let Ok(mut state) = self.state() else {
            return;
        };
        let lifetime = pairing_lifetime(key);
        if !state.sealed.contains(&lifetime) && state.sealed.len() >= MAX_PENDING_REVOCATIONS {
            state.close();
            return;
        }
        state.sealed.insert(lifetime);
        state.gates.retain(|(key, gate)| {
            if pairing_lifetime(key) == lifetime {
                gate.seal();
                false
            } else {
                true
            }
        });
    }
}

/// A durable journal reservation, consumed before generating any key material.
/// It cannot be constructed from a snapshot, native locator or server message.
pub struct CredentialCreationReservation {
    scope: CredentialScope,
    generation: LocalPairingGeneration,
}
impl CredentialCreationReservation {
    pub(crate) fn new(scope: CredentialScope, generation: LocalPairingGeneration) -> Self {
        Self { scope, generation }
    }
    pub fn scope(&self) -> &CredentialScope {
        &self.scope
    }
    pub fn generation(&self) -> LocalPairingGeneration {
        self.generation
    }
}

/// Implementations expose signing capabilities, never private-key export.
/// Creation is explicit and exclusive; missing reads never regenerate a key.
pub trait NativeDeviceCredentials: Send + Sync {
    fn create(
        &self,
        reservation: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, CredentialFailure>>;
    /// Read-only recovery of an incomplete durable preparation. It never
    /// regenerates a missing key or returns private bytes.
    fn recover_preparation<'a>(
        &'a self,
        scope: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, CredentialFailure>>;
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        authorization: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, CredentialFailure>>;
    fn delete_pairing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, CredentialFailure>>;
}
