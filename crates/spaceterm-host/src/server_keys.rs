//! File-backed server identities and pairing secrets in admitted private storage.
//!
//! Identity creation requires an internal initial-install capability. Established
//! startup never creates an identity in response to a missing or denied read.

use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use ring::signature::{Ed25519KeyPair, KeyPair as _};
use sha2::{Digest as _, Sha256};
use spaceterm_core::pairing::EnrollmentTicketId;
use spaceterm_protocol::{
    ProtocolError,
    authentication::{ServerPin, SigningKey},
};
use zeroize::{Zeroize as _, Zeroizing};

use crate::{
    account_service::RegisteredServer,
    private_filesystem::{Directory, Failure, Snapshot},
    startup::{AccountBuildKey, BuildIdentity},
};

const IDENTITY_NAME: &str = "identity";
const IDENTITY_BYTES: usize = 60;
const MAX_SECRET_BYTES: usize = 4096;
const SECRET_HEADER_BYTES: usize = 40;

/// Bounded key-store failure classifications. No secret or native error text is retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyStoreFailure {
    /// An established required file is absent.
    Missing,
    /// Native storage is locked.
    StorageLocked,
    /// Native storage is unavailable, not absent.
    StorageUnavailable,
    /// Permissions, ownership, links or descriptor identity are unsafe.
    UnsafeStorage,
    /// A bounded record is malformed or unsupported.
    InvalidRecord,
    /// The exact account, build, server or public pin differs.
    IdentityMismatch,
    /// Exclusive initial publication found an existing identity or secret.
    AlreadyExists,
    /// The exact inspected entry was replaced or modified.
    Conflict,
    /// Secret or record bounds were exceeded.
    LimitExceeded,
    /// Publication or removal may have happened without durable synchronization.
    Indeterminate,
}

impl From<Failure> for KeyStoreFailure {
    fn from(error: Failure) -> Self {
        match error {
            Failure::Missing => Self::Missing,
            Failure::AlreadyExists => Self::AlreadyExists,
            Failure::Conflict => Self::Conflict,
            Failure::LimitExceeded => Self::LimitExceeded,
            Failure::Unsafe => Self::UnsafeStorage,
            Failure::Unavailable => Self::StorageUnavailable,
            Failure::Indeterminate => Self::Indeterminate,
        }
    }
}

/// Zeroizing signing identity. Only its public pin and signing capability are exposed.
pub struct ServerIdentity {
    pin: ServerPin,
    seed: Zeroizing<[u8; 32]>,
}
impl ServerIdentity {
    /// Returns the public enrolled server identity and Ed25519 pin.
    pub fn pin(&self) -> ServerPin {
        self.pin
    }
}
impl SigningKey for ServerIdentity {
    fn public_key(&self) -> [u8; 32] {
        self.pin.public_key
    }
    fn sign(&self, transcript: &[u8]) -> Result<[u8; 64], ProtocolError> {
        let signer = Ed25519KeyPair::from_seed_unchecked(self.seed.as_ref())
            .map_err(|_| ProtocolError::SigningUnavailable)?;
        signer
            .sign(transcript)
            .as_ref()
            .try_into()
            .map_err(|_| ProtocolError::SigningUnavailable)
    }
}
impl fmt::Debug for ServerIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServerIdentity([redacted])")
    }
}

/// A bounded zeroizing bearer body. It has no serialization or seed accessor.
pub struct PairingSecret(Zeroizing<Vec<u8>>);
impl PairingSecret {
    /// Takes zeroizing ownership of a nonempty secret of at most 4096 bytes.
    pub fn new(bytes: Zeroizing<Vec<u8>>) -> Result<Self, KeyStoreFailure> {
        if bytes.is_empty() {
            return Err(KeyStoreFailure::InvalidRecord);
        }
        if bytes.len() > MAX_SECRET_BYTES {
            return Err(KeyStoreFailure::LimitExceeded);
        }
        Ok(Self(bytes))
    }
    /// Borrows secret bytes only for the duration of the supplied operation.
    pub fn with_bytes<T>(&self, use_bytes: impl FnOnce(&[u8]) -> T) -> T {
        use_bytes(&self.0)
    }
}
impl fmt::Debug for PairingSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingSecret([redacted])")
    }
}

/// Exact opaque file version used for identity-checked secret removal.
pub struct SecretVersion {
    snapshot: Snapshot,
    ticket: EnrollmentTicketId,
    digest: [u8; 32],
}
impl fmt::Debug for SecretVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretVersion(..)")
    }
}

struct Initialization {
    directory: Directory,
    key: AccountBuildKey,
    indeterminate: AtomicBool,
}

/// Noncloneable capability issued only within serialized initial installation.
pub struct IdentityCreatePermit(Arc<Initialization>);
impl fmt::Debug for IdentityCreatePermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IdentityCreatePermit(..)")
    }
}

/// Retained private file authority for one registered account/build/server.
pub struct ServerKeyStore {
    directory: Directory,
    key: AccountBuildKey,
    expected: Option<ServerPin>,
    initialization: Option<Arc<Initialization>>,
    indeterminate: AtomicBool,
}
impl fmt::Debug for ServerKeyStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServerKeyStore(..)")
    }
}

impl ServerKeyStore {
    /// Opens existing storage. It never creates directories, identities or secrets.
    pub fn open(registration: &RegisteredServer) -> Result<Self, KeyStoreFailure> {
        registration.directory.verify_established()?;
        Ok(Self {
            directory: registration.directory.clone(),
            key: registration.key(),
            expected: Some(registration.server_pin()),
            initialization: None,
            indeterminate: AtomicBool::new(false),
        })
    }

    // The initial-install owner must hold its mutation lock before calling this.
    #[cfg(feature = "test-support")]
    pub(crate) fn initial(
        directory: Directory,
        key: AccountBuildKey,
    ) -> (Self, IdentityCreatePermit) {
        let initialization = Arc::new(Initialization {
            directory: directory.clone(),
            key,
            indeterminate: AtomicBool::new(false),
        });
        let store = Self {
            directory,
            key,
            expected: None,
            initialization: Some(Arc::clone(&initialization)),
            indeterminate: AtomicBool::new(false),
        };
        (store, IdentityCreatePermit(initialization))
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn load_initial_identity(&self) -> Result<Arc<ServerIdentity>, KeyStoreFailure> {
        self.available()?;
        if self.initialization.is_none() {
            return Err(KeyStoreFailure::IdentityMismatch);
        }
        let snapshot = self
            .directory
            .read(IDENTITY_NAME, IDENTITY_BYTES)?
            .ok_or(KeyStoreFailure::Missing)?;
        decode_identity(&snapshot.bytes, self.key)
    }

    /// Exclusively publishes server ID and seed in one synchronized bounded record.
    pub fn create_identity(
        &self,
        permit: IdentityCreatePermit,
    ) -> Result<Arc<ServerIdentity>, KeyStoreFailure> {
        self.available()?;
        if self
            .initialization
            .as_ref()
            .is_none_or(|scope| !Arc::ptr_eq(scope, &permit.0))
            || permit.0.key != self.key
            || permit.0.indeterminate.load(Ordering::Acquire)
        {
            return Err(KeyStoreFailure::IdentityMismatch);
        }
        permit.0.directory.verify()?;
        let mut server = [0; 16];
        let mut seed = Zeroizing::new([0; 32]);
        getrandom::fill(&mut server).map_err(|_| KeyStoreFailure::StorageUnavailable)?;
        getrandom::fill(seed.as_mut()).map_err(|_| KeyStoreFailure::StorageUnavailable)?;
        if server == [0; 16] {
            return Err(KeyStoreFailure::InvalidRecord);
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(IDENTITY_BYTES));
        bytes.extend_from_slice(b"STSK");
        bytes.extend_from_slice(&[1, build_tag(self.key.build_identity), 0, 0]);
        bytes.extend_from_slice(&self.key.account_uid.to_be_bytes());
        bytes.extend_from_slice(&server);
        bytes.extend_from_slice(seed.as_ref());
        match self.directory.publish(IDENTITY_NAME, &bytes, None) {
            Ok(()) => decode_identity(&bytes, self.key),
            Err(Failure::Conflict | Failure::AlreadyExists) => Err(KeyStoreFailure::AlreadyExists),
            Err(error) => {
                if error == Failure::Indeterminate {
                    permit.0.indeterminate.store(true, Ordering::Release);
                    self.indeterminate.store(true, Ordering::Release);
                }
                Err(error.into())
            }
        }
    }

    /// Loads an existing identity and compares both registered and caller pins.
    pub fn load_identity(
        &self,
        expected: ServerPin,
    ) -> Result<Arc<ServerIdentity>, KeyStoreFailure> {
        self.available()?;
        if self.expected.is_some_and(|pin| pin != expected) {
            return Err(KeyStoreFailure::IdentityMismatch);
        }
        let snapshot = self
            .directory
            .read(IDENTITY_NAME, IDENTITY_BYTES)?
            .ok_or(KeyStoreFailure::Missing)?;
        let identity = decode_identity(&snapshot.bytes, self.key)?;
        if identity.pin() != expected {
            return Err(KeyStoreFailure::IdentityMismatch);
        }
        Ok(identity)
    }

    /// Exclusively creates one server-bound bearer record; it never overwrites a ticket.
    pub fn create_pairing_secret(
        &self,
        ticket: EnrollmentTicketId,
        secret: PairingSecret,
    ) -> Result<SecretVersion, KeyStoreFailure> {
        self.available()?;
        self.validate_ticket(&ticket)?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(SECRET_HEADER_BYTES + secret.0.len()));
        bytes.extend_from_slice(b"STPS");
        bytes.extend_from_slice(&[1, 0, 0, 0]);
        bytes.extend_from_slice(&ticket.parts().server.bytes());
        bytes.extend_from_slice(&ticket.parts().nonce);
        bytes.extend_from_slice(&secret.0);
        let name = secret_name(&ticket);
        let snapshot = self.publication(&name, &bytes)?;
        Ok(SecretVersion {
            snapshot,
            ticket,
            digest: Sha256::digest(bytes.as_slice()).into(),
        })
    }

    /// Loads an optional ticket only after existing-storage validation.
    pub fn load_pairing_secret(
        &self,
        ticket: EnrollmentTicketId,
    ) -> Result<Option<(PairingSecret, SecretVersion)>, KeyStoreFailure> {
        self.available()?;
        self.validate_ticket(&ticket)?;
        let Some(snapshot) = self.directory.read(
            &secret_name(&ticket),
            SECRET_HEADER_BYTES + MAX_SECRET_BYTES,
        )?
        else {
            return Ok(None);
        };
        let bytes = &snapshot.bytes;
        if bytes.len() <= SECRET_HEADER_BYTES || &bytes[..8] != b"STPS\x01\0\0\0" {
            return Err(KeyStoreFailure::InvalidRecord);
        }
        if bytes[8..24] != ticket.parts().server.bytes() || bytes[24..40] != ticket.parts().nonce {
            return Err(KeyStoreFailure::IdentityMismatch);
        }
        let secret = PairingSecret::new(Zeroizing::new(bytes[SECRET_HEADER_BYTES..].to_vec()))?;
        Ok(Some((secret, version(snapshot, ticket))))
    }

    /// Removes only the inspected entry, rejecting replacement and in-place edits.
    pub fn remove_pairing_secret(
        &self,
        ticket: EnrollmentTicketId,
        expected: SecretVersion,
    ) -> Result<(), KeyStoreFailure> {
        self.available()?;
        self.validate_ticket(&ticket)?;
        if ticket != expected.ticket {
            return Err(KeyStoreFailure::Conflict);
        }
        let name = secret_name(&ticket);
        let snapshot = self
            .directory
            .read(&name, SECRET_HEADER_BYTES + MAX_SECRET_BYTES)?
            .ok_or(KeyStoreFailure::Conflict)?;
        let digest: [u8; 32] = Sha256::digest(snapshot.bytes.as_slice()).into();
        if digest != expected.digest || snapshot.identity != expected.snapshot.identity {
            return Err(KeyStoreFailure::Conflict);
        }
        let result = self.directory.remove(&name, &expected.snapshot);
        if matches!(result, Err(Failure::Indeterminate)) {
            self.indeterminate.store(true, Ordering::Release);
        }
        result.map_err(Into::into)
    }

    fn available(&self) -> Result<(), KeyStoreFailure> {
        if self.indeterminate.load(Ordering::Acquire) {
            return Err(KeyStoreFailure::Indeterminate);
        }
        self.directory.verify().map_err(Into::into)
    }
    fn validate_ticket(&self, ticket: &EnrollmentTicketId) -> Result<(), KeyStoreFailure> {
        if self
            .expected
            .is_none_or(|pin| ticket.parts().server.bytes() != pin.identity)
        {
            return Err(KeyStoreFailure::IdentityMismatch);
        }
        Ok(())
    }
    fn publication(&self, name: &str, bytes: &[u8]) -> Result<Snapshot, KeyStoreFailure> {
        let result = self.directory.publish_retained(name, bytes, None);
        match result {
            Err(Failure::Conflict | Failure::AlreadyExists) => Err(KeyStoreFailure::AlreadyExists),
            Err(Failure::Indeterminate) => {
                self.indeterminate.store(true, Ordering::Release);
                Err(KeyStoreFailure::Indeterminate)
            }
            other => other.map_err(Into::into),
        }
    }
}

/// Native boundary hooks for deterministic race and durability regressions.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    /// Runs after the exact filesystem commit, before the caller receives its result.
    pub fn after_next_publication(operation: impl FnOnce() + 'static) {
        crate::private_filesystem::test_support::after_next_publication(operation);
    }

    /// Supplies CommittedButUnsynced at the native publication outcome boundary.
    /// The test still uses real private files; this does not simulate a power loss.
    pub fn fail_next_publication_durability() {
        crate::private_filesystem::test_support::fail_next_publication_durability();
    }

    /// Injects EIO at the next removal's containing-directory synchronization.
    pub fn fail_next_removal_sync() {
        crate::private_filesystem::test_support::fail_next_removal_sync();
    }
}

fn version(mut snapshot: Snapshot, ticket: EnrollmentTicketId) -> SecretVersion {
    let digest = Sha256::digest(snapshot.bytes.as_slice()).into();
    // A removal token retains only file identity and a hash, not the bearer body.
    snapshot.bytes.zeroize();
    snapshot.bytes.clear();
    SecretVersion {
        snapshot,
        ticket,
        digest,
    }
}
fn decode_identity(
    bytes: &[u8],
    key: AccountBuildKey,
) -> Result<Arc<ServerIdentity>, KeyStoreFailure> {
    if bytes.len() != IDENTITY_BYTES
        || &bytes[..4] != b"STSK"
        || bytes[4] != 1
        || bytes[6..8] != [0, 0]
    {
        return Err(KeyStoreFailure::InvalidRecord);
    }
    if bytes[5] != build_tag(key.build_identity) || bytes[8..12] != key.account_uid.to_be_bytes() {
        return Err(KeyStoreFailure::IdentityMismatch);
    }
    let server: [u8; 16] = bytes[12..28]
        .try_into()
        .map_err(|_| KeyStoreFailure::InvalidRecord)?;
    if server == [0; 16] {
        return Err(KeyStoreFailure::InvalidRecord);
    }
    let seed: Zeroizing<[u8; 32]> = Zeroizing::new(
        bytes[28..60]
            .try_into()
            .map_err(|_| KeyStoreFailure::InvalidRecord)?,
    );
    let signer = Ed25519KeyPair::from_seed_unchecked(seed.as_ref())
        .map_err(|_| KeyStoreFailure::InvalidRecord)?;
    let public_key = signer
        .public_key()
        .as_ref()
        .try_into()
        .map_err(|_| KeyStoreFailure::InvalidRecord)?;
    Ok(Arc::new(ServerIdentity {
        pin: ServerPin {
            identity: server,
            public_key,
        },
        seed,
    }))
}
fn build_tag(build: BuildIdentity) -> u8 {
    match build {
        BuildIdentity::Release => 1,
        BuildIdentity::Preflight => 2,
        BuildIdentity::Development => 3,
    }
}
fn secret_name(ticket: &EnrollmentTicketId) -> String {
    let mut name = String::from("pairing-");
    use std::fmt::Write as _;
    for byte in ticket.parts().nonce {
        let _ = write!(name, "{byte:02x}");
    }
    name
}
