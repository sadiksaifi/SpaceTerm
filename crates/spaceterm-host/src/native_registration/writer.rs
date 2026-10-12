//! Serialized bounded head publication and irreversible uncertainty within one publisher.
//!
//! The storage port must establish protected cross-process serialization and native custody.
//! This engine coordinates calls and durability; it does not itself establish filesystem authority.

use crate::{
    installation::validate_successor,
    native_deployment::contracts::*,
    native_registration::{RegistrationHeadV2, encode_registration_head},
};
use std::sync::Mutex;

/// Trusted native storage edge. All inputs are fixed account/build facts or bounded head bytes.
/// The adapter must never infer publication authority from root ownership or these inputs.
pub trait RegistrationPublicationStorage: Send + Sync {
    /// Checks current durable bytes under the native protected publisher/account transaction locks.
    fn read_current(&self, key: AccountBuildKey) -> Result<[u8; 256], NativeFailure>;
    /// Creates a fresh protected inode and writes the exact next head while retaining its handle.
    fn write_fresh(&self, key: AccountBuildKey, bytes: &[u8; 256]) -> Result<(), NativeFailure>;
    /// Synchronizes the retained fresh file, not a reopened pathname.
    fn sync_fresh(&self) -> Result<(), NativeFailure>;
    /// Atomically replaces only the fixed head under native serialization.
    fn replace_head(&self, key: AccountBuildKey) -> Result<(), NativeFailure>;
    /// Synchronizes the retained protected parent after the replacement.
    fn sync_parent(&self) -> Result<(), NativeFailure>;
}

/// Content-free publisher state, latched closed after any uncertain effect or synchronization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationState {
    /// No prior uncertain write in this publisher's scope.
    Open,
    /// Validated native recovery is required. A later sync cannot clear this state.
    RecoveryRequired,
}

/// One serialized head writer. Production requires a separate native cross-process lock scope.
pub struct RegistrationHeadWriter<Storage> {
    storage: Storage,
    state: Mutex<PublicationState>,
}
impl<Storage: RegistrationPublicationStorage> RegistrationHeadWriter<Storage> {
    /// Injects a trusted storage edge; does not issue native authorization or custody.
    pub fn new(storage: Storage) -> Self {
        Self {
            storage,
            state: Mutex::new(PublicationState::Open),
        }
    }
    /// Consumes this writer and returns its retained native storage for explicit recovery.
    /// This does not clear disk uncertainty or change the original publication result.
    pub fn into_storage(self) -> Storage {
        self.storage
    }
    /// Returns the uncertainty latch. Poisoning is uncertainty, not a successful recovery.
    pub fn state(&self) -> PublicationState {
        self.state
            .lock()
            .map_or(PublicationState::RecoveryRequired, |state| *state)
    }
    /// Publishes one validated successor. The first uncertain result permanently closes this writer.
    /// Recreating a writer does not recover disk state: production storage must also enforce the journal latch.
    pub fn publish_proposal(
        &self,
        current: &RegistrationHeadV2,
        next: &RegistrationHeadV2,
    ) -> PublicationDisposition<()> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return PublicationDisposition::Indeterminate,
        };
        if *state != PublicationState::Open {
            return PublicationDisposition::NotApplied(NativeFailure::Indeterminate);
        }
        let prepare = || {
            validate_successor(current, next)?;
            let expected = encode_registration_head(current)?;
            let observed = self.storage.read_current(current.key)?;
            if observed != expected {
                return Err(NativeFailure::GenerationMismatch);
            }
            encode_registration_head(next)
        };
        let bytes = match prepare() {
            Ok(b) => b,
            Err(failure) => return PublicationDisposition::NotApplied(failure),
        };
        // An error after entering a mutation callback is not proof that no bytes changed.
        if self.storage.write_fresh(next.key, &bytes).is_err() {
            *state = PublicationState::RecoveryRequired;
            return PublicationDisposition::Indeterminate;
        }
        if self.storage.sync_fresh().is_err()
            || self.storage.replace_head(next.key).is_err()
            || self.storage.sync_parent().is_err()
        {
            *state = PublicationState::RecoveryRequired;
            return PublicationDisposition::Indeterminate;
        }
        PublicationDisposition::Durable(())
    }
}

/// Fixed production storage edge. No native publication issuer is present in this checkout.
#[derive(Default)]
pub struct SystemRegistrationStorage;
impl RegistrationPublicationStorage for SystemRegistrationStorage {
    fn read_current(&self, key: AccountBuildKey) -> Result<[u8; 256], NativeFailure> {
        validate_key(key)?;
        Err(NativeFailure::CustodyUnavailable)
    }
    fn write_fresh(&self, _key: AccountBuildKey, _bytes: &[u8; 256]) -> Result<(), NativeFailure> {
        Err(NativeFailure::CustodyUnavailable)
    }
    fn sync_fresh(&self) -> Result<(), NativeFailure> {
        Err(NativeFailure::CustodyUnavailable)
    }
    fn replace_head(&self, _key: AccountBuildKey) -> Result<(), NativeFailure> {
        Err(NativeFailure::CustodyUnavailable)
    }
    fn sync_parent(&self) -> Result<(), NativeFailure> {
        Err(NativeFailure::CustodyUnavailable)
    }
}
