//! Native account incarnation comparison, distinct from publication authority.

use super::contracts::{AccountBuildKey, NativeFailure, build_code, nonzero, validate_key};
use sha2::{Digest, Sha256};
use std::fmt;

/// Canonical account-incarnation facts. A checked value does not prove its OS provenance.
#[derive(Clone, Eq, PartialEq)]
pub struct NativeAccountId {
    platform: u8,
    bytes: Vec<u8>,
}
impl NativeAccountId {
    /// Checks the binary Directory Services GeneratedUID, not a username or home path.
    pub fn macos(generated_uid: [u8; 16]) -> Result<Self, NativeFailure> {
        nonzero(&generated_uid)?;
        Ok(Self {
            platform: 1,
            bytes: generated_uid.to_vec(),
        })
    }
    /// Checks canonical Linux provider identities. Only an OS-anchored provider may attest them.
    pub fn linux(issuer: &[u8], incarnation: &[u8]) -> Result<Self, NativeFailure> {
        if issuer.is_empty()
            || issuer.len() > 256
            || incarnation.is_empty()
            || incarnation.len() > 256
        {
            return Err(NativeFailure::InvalidRecord);
        }
        nonzero(issuer)?;
        nonzero(incarnation)?;
        let mut bytes = Vec::with_capacity(4 + issuer.len() + incarnation.len());
        bytes.extend_from_slice(&(issuer.len() as u16).to_le_bytes());
        bytes.extend_from_slice(issuer);
        bytes.extend_from_slice(&(incarnation.len() as u16).to_le_bytes());
        bytes.extend_from_slice(incarnation);
        Ok(Self { platform: 2, bytes })
    }
    pub(crate) fn platform(&self) -> u8 {
        self.platform
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
impl fmt::Debug for NativeAccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeAccountId(..)")
    }
}

/// UID, primary GID, build namespace, and native account incarnation comparison facts.
#[derive(Clone, Eq, PartialEq)]
pub struct AccountBinding {
    key: AccountBuildKey,
    primary_gid: u32,
    native: NativeAccountId,
}
impl AccountBinding {
    /// Checks bounded facts. This constructor does not attest a live native account.
    pub fn new(
        key: AccountBuildKey,
        primary_gid: u32,
        native: NativeAccountId,
    ) -> Result<Self, NativeFailure> {
        validate_key(key)?;
        if primary_gid == u32::MAX {
            return Err(NativeFailure::InvalidRecord);
        }
        Ok(Self {
            key,
            primary_gid,
            native,
        })
    }
    /// Returns the exact registered account/build key.
    pub fn key(&self) -> AccountBuildKey {
        self.key
    }
    /// Returns the approved primary group, never an environment override.
    pub fn primary_gid(&self) -> u32 {
        self.primary_gid
    }
    /// Returns the canonical domain-separated binding digest.
    pub fn sha256(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"SpaceTerm native account binding v1\0");
        h.update([self.native.platform(), build_code(self.key.build_identity)]);
        h.update(self.key.account_uid.to_le_bytes());
        h.update(self.primary_gid.to_le_bytes());
        h.update((self.native.bytes().len() as u16).to_le_bytes());
        h.update(self.native.bytes());
        h.finalize().into()
    }
    /// Rejects UID reuse, primary-group change, or another build namespace.
    pub fn compare(&self, current: &Self) -> Result<(), NativeFailure> {
        if self == current {
            Ok(())
        } else {
            Err(NativeFailure::IdentityMismatch)
        }
    }
}
impl fmt::Debug for AccountBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccountBinding(..)")
    }
}

/// Read-only native account-lifecycle observation supplied by the trusted composition root.
pub trait NativeAccountProvider: Send + Sync {
    /// Proves account incarnation and primary GID through an OS-anchored provider.
    fn observe(&self, key: AccountBuildKey) -> Result<AccountBinding, NativeFailure>;
}

/// Production observation remains unavailable until a native incarnation provider is bound.
#[derive(Default)]
pub struct SystemAccountProvider;
impl NativeAccountProvider for SystemAccountProvider {
    fn observe(&self, key: AccountBuildKey) -> Result<AccountBinding, NativeFailure> {
        validate_key(key)?;
        // passwd facts do not establish Linux reincarnation; UID queries do not establish
        // macOS GeneratedUID uniqueness. Neither can substitute for the native provider.
        Err(NativeFailure::AccountIdentityUnavailable)
    }
}
