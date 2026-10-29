//! Saved Connections and their device credentials, shared by the desktop and the CLI.
//!
//! Each client keeps its own records in its own namespace. A credential lives only in the
//! native store, never in a file.
use std::path::PathBuf;
use std::sync::Arc;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Interaction {
    /// Never shows a prompt.
    Quiet,
    /// May show the store's own prompt.
    Requested,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialError {
    InteractionRequired,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreKind {
    Keychain,
    SecretService,
}

/// Native storage for one immutable secret per pairing, keyed by its credential key.
pub trait CredentialStore: Send + Sync {
    fn kind(&self) -> StoreKind;

    fn read(
        &self,
        key: &[u8; 32],
        interaction: Interaction,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError>;

    fn insert(&self, key: &[u8; 32], secret: &[u8]) -> Result<(), CredentialError>;

    fn remove(&self, key: &[u8; 32], interaction: Interaction) -> Result<(), CredentialError>;
}

/// This machine's credential store for `identity`: the Keychain on macOS, Secret Service on
/// Linux. Without one, `Unavailable`. Server-role and local commands never call it.
pub fn native(_identity: &str) -> Result<Arc<dyn CredentialStore>, CredentialError> {
    Err(CredentialError::Unavailable)
}

/// Whether Unpair This Computer still owes the server a revocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnpairState {
    Paired,
    Pending,
}

/// One Saved Connection.
#[derive(Clone, Debug)]
pub struct SavedConnection {
    /// Unique among this client's records. `local` is reserved.
    pub name: String,
    pub route: [u8; 32],
    /// `quic://HOST:PORT`.
    pub endpoint: String,
    pub paired: spaceterm_server::direct::PairedDevice,
    pub credential: [u8; 32],
    pub unpair: UnpairState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreError {
    Unavailable,
    /// Another writer changed the records since they were loaded.
    Changed,
    /// The name is reserved or taken.
    NameTaken,
    NotFound,
}

/// One client's Saved Connections file, changed only under its lock.
pub struct ConnectionStore {
    #[allow(
        dead_code,
        reason = "the records file is read and written by the connection commands"
    )]
    root: PathBuf,
}

impl ConnectionStore {
    /// The store whose `connections.json` and `connections.lock` live in `root`.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn load(&self) -> Result<Vec<SavedConnection>, StoreError> {
        Err(StoreError::Unavailable)
    }

    /// Applies `change` to the current records under the lock, then writes them.
    pub fn commit_guarded<T>(
        &self,
        _change: impl FnOnce(&mut Vec<SavedConnection>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        Err(StoreError::Unavailable)
    }

    /// Removes the record named `name` and returns it.
    pub fn remove(&self, _name: &str) -> Result<SavedConnection, StoreError> {
        Err(StoreError::Unavailable)
    }
}
