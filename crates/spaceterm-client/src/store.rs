//! Saved Connections and exact recovery requests.
use crate::WaitLimit;
use crate::credentials::{CredentialError, CredentialStore, Interaction};
use crate::direct::{DeviceCredential, DirectConnection, DirectConnector, DirectError};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use crate::direct::{Endpoint, MachinePin, PairedDevice};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreKind {
    Keychain,
    SecretService,
    PrivateFile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum UnpairState {
    Paired,
    Pending,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedConnection {
    pub name: String,
    pub route: [u8; 32],
    pub endpoint: String,
    pub paired: PairedDevice,
    pub credential: [u8; 32],
    pub credential_store: StoreKind,
    pub unpair: UnpairState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreError {
    Timeout,
    Interrupted,
    Unavailable,
    Changed,
    NameTaken,
    NotFound,
}

/// Restricted cancellation authority. It contains neither an invitation nor a secret.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingEnrollment {
    pub route: [u8; 32],
    pub name: String,
    pub endpoint: String,
    pub pin: MachinePin,
    pub claim: [u8; 32],
    pub credential: [u8; 32],
    pub credential_store: StoreKind,
    pub public_key: [u8; 32],
    pub offered: bool,
    pub confirmed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingRevocation {
    pub connection: SavedConnection,
    pub confirmed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemovalConfirmation {
    pub token: [u8; 32],
    pub expires: u64,
    pub connection: SavedConnection,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    connections: Vec<SavedConnection>,
    pending: Vec<PendingRevocation>,
    enrollments: Vec<PendingEnrollment>,
    confirmations: Vec<RemovalConfirmation>,
}

/// A held route cleanup lease. Dropping it releases the lease.
pub trait RecoveryGuard {}

/// Computes replacement Saved Connections bytes from the currently retained document.
pub type SavedConnectionChange<'a> = dyn FnMut(Option<&[u8]>) -> Result<Vec<u8>, StoreError> + 'a;

/// Storage for one Saved Connections document. Reads never create state.
pub trait ConnectionStorage: Send + Sync {
    fn load(&self) -> Result<Option<Vec<u8>>, StoreError>;

    /// Holds exclusive ownership from the current read through publication.
    /// Runs the callback once after admission. A callback refusal leaves the stored bytes unchanged.
    fn change(
        &self,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
        change: &mut SavedConnectionChange<'_>,
    ) -> Result<(), StoreError>;

    fn try_recovery(&self, route: &[u8; 32]) -> Result<Option<Box<dyn RecoveryGuard>>, StoreError>;
}

/// One client's Saved Connections. Construction and reads create nothing.
pub struct ConnectionStore {
    storage: Arc<dyn ConnectionStorage>,
}

/// Mutations bounded by one command's deadline and cancellation authority.
pub struct ConnectionTransaction<'a> {
    store: &'a ConnectionStore,
    deadline: Instant,
    cancelled: &'a dyn Fn() -> bool,
}

impl ConnectionStore {
    pub fn new(storage: Arc<dyn ConnectionStorage>) -> Self {
        Self { storage }
    }

    pub fn load(&self) -> Result<Vec<SavedConnection>, StoreError> {
        Ok(self.read()?.connections)
    }
    pub fn pending(&self) -> Result<Vec<SavedConnection>, StoreError> {
        Ok(self
            .read()?
            .pending
            .into_iter()
            .map(|row| row.connection)
            .collect())
    }
    pub fn pending_revocations(&self) -> Result<Vec<PendingRevocation>, StoreError> {
        Ok(self.read()?.pending)
    }
    pub fn pending_enrollments(&self) -> Result<Vec<PendingEnrollment>, StoreError> {
        Ok(self.read()?.enrollments)
    }

    /// The credential item of every saved connection, pending revocation and pending enrollment,
    /// each once, from one read. Deleting this client's data removes these items too.
    pub fn credential_items(&self) -> Result<Vec<(StoreKind, [u8; 32])>, StoreError> {
        let document = self.read()?;
        let mut items = Vec::new();
        let connections = document
            .connections
            .iter()
            .chain(document.pending.iter().map(|row| &row.connection))
            .chain(document.confirmations.iter().map(|row| &row.connection))
            .map(|row| (row.credential_store, row.credential));
        let enrollments = document
            .enrollments
            .iter()
            .map(|row| (row.credential_store, row.credential));
        for item in connections.chain(enrollments) {
            if !items.contains(&item) {
                items.push(item);
            }
        }
        Ok(items)
    }

    fn read(&self) -> Result<Document, StoreError> {
        read(self.storage.load()?.as_deref())
    }

    /// Serializes a live enrollment or cleanup without blocking unrelated connections.
    pub fn recovery_guard(
        &self,
        route: &[u8; 32],
    ) -> Result<Option<Box<dyn RecoveryGuard>>, StoreError> {
        self.storage.try_recovery(route)
    }

    pub fn transaction<'a>(
        &'a self,
        deadline: Instant,
        cancelled: &'a dyn Fn() -> bool,
    ) -> ConnectionTransaction<'a> {
        ConnectionTransaction {
            store: self,
            deadline,
            cancelled,
        }
    }
}

impl ConnectionTransaction<'_> {
    /// The lock covers the current read, the caller's change, and atomic publication.
    pub fn commit_guarded<T>(
        &self,
        change: impl FnOnce(&mut Vec<SavedConnection>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.change(|document| change(&mut document.connections))
    }

    /// Rename one active CLI connection without accessing its immutable credential.
    pub fn rename(&self, old: &str, new: &str) -> Result<SavedConnection, StoreError> {
        if !valid_name(new) {
            return Err(StoreError::NameTaken);
        }
        self.change(|document| {
            let index = document
                .connections
                .iter()
                .position(|row| row.name == old)
                .ok_or(StoreError::NotFound)?;
            if document.connections.iter().any(|row| row.name == new)
                || document.enrollments.iter().any(|row| row.name == new)
            {
                return Err(StoreError::NameTaken);
            }
            let connection = &mut document.connections[index];
            connection.name = new.to_owned();
            let renamed = connection.clone();
            document
                .confirmations
                .retain(|row| row.connection.route != renamed.route);
            Ok(renamed)
        })
    }

    pub fn reserve(&self, enrollment: PendingEnrollment) -> Result<(), StoreError> {
        self.change(|document| {
            if document
                .connections
                .iter()
                .any(|row| row.name == enrollment.name)
            {
                return Err(StoreError::NameTaken);
            }
            if document
                .enrollments
                .iter()
                .any(|row| row.name == enrollment.name)
            {
                return Err(StoreError::Changed);
            }
            document.enrollments.push(enrollment);
            Ok(())
        })
    }

    pub fn mark_offered(&self, route: [u8; 32]) -> Result<(), StoreError> {
        self.change(|document| {
            let row = document
                .enrollments
                .iter_mut()
                .find(|row| row.route == route && !row.confirmed)
                .ok_or(StoreError::Changed)?;
            row.offered = true;
            Ok(())
        })
    }

    pub fn publish(&self, connection: SavedConnection) -> Result<(), StoreError> {
        self.change(|document| {
            let index = document
                .enrollments
                .iter()
                .position(|row| {
                    row.route == connection.route
                        && row.name == connection.name
                        && row.endpoint == connection.endpoint
                        && row.pin == connection.paired.pin
                        && row.credential == connection.credential
                        && row.credential_store == connection.credential_store
                        && !row.confirmed
                })
                .ok_or(StoreError::Changed)?;
            document.enrollments.remove(index);
            document.connections.push(connection);
            Ok(())
        })
    }

    pub fn confirm_enrollment_cleanup(&self, route: [u8; 32]) -> Result<(), StoreError> {
        self.change(|document| {
            let row = document
                .enrollments
                .iter_mut()
                .find(|row| row.route == route)
                .ok_or(StoreError::NotFound)?;
            row.confirmed = true;
            Ok(())
        })
    }
    pub fn retire_enrollment(&self, route: [u8; 32]) -> Result<(), StoreError> {
        self.change(|document| {
            let index = document
                .enrollments
                .iter()
                .position(|row| row.route == route && row.confirmed)
                .ok_or(StoreError::Changed)?;
            document.enrollments.remove(index);
            Ok(())
        })
    }

    /// The preview binds the exact local record and expires after one minute.
    pub fn preview_remove(&self, name: &str, now: u64) -> Result<RemovalConfirmation, StoreError> {
        self.change(|document| {
            let connection = document
                .connections
                .iter()
                .find(|row| row.name == name)
                .ok_or(StoreError::NotFound)?
                .clone();
            document
                .confirmations
                .retain(|row| row.expires > now && row.connection.route != connection.route);
            if document.confirmations.len() >= 128 {
                return Err(StoreError::Unavailable);
            }
            let confirmation = RemovalConfirmation {
                token: crate::random_id().map_err(|_| StoreError::Unavailable)?,
                expires: now.saturating_add(60),
                connection,
            };
            document.confirmations.push(confirmation.clone());
            Ok(confirmation)
        })
    }

    pub fn confirm_remove(
        &self,
        name: &str,
        token: &[u8; 32],
        now: u64,
    ) -> Result<SavedConnection, StoreError> {
        self.change(|document| {
            let confirmation = document
                .confirmations
                .iter()
                .find(|row| &row.token == token && row.expires > now && row.connection.name == name)
                .ok_or(StoreError::Changed)?;
            let index = document
                .connections
                .iter()
                .position(|row| row == &confirmation.connection)
                .ok_or(StoreError::Changed)?;
            Ok(remove(document, index))
        })
    }
    pub fn remove(&self, name: &str) -> Result<SavedConnection, StoreError> {
        self.change(|document| {
            let index = document
                .connections
                .iter()
                .position(|row| row.name == name)
                .ok_or(StoreError::NotFound)?;
            Ok(remove(document, index))
        })
    }
    pub fn confirm_revocation(&self, route: [u8; 32]) -> Result<(), StoreError> {
        self.change(|document| {
            let row = document
                .pending
                .iter_mut()
                .find(|row| row.connection.route == route)
                .ok_or(StoreError::NotFound)?;
            row.confirmed = true;
            Ok(())
        })
    }
    pub fn retire_pending(&self, route: [u8; 32]) -> Result<(), StoreError> {
        self.change(|document| {
            let index = document
                .pending
                .iter()
                .position(|row| row.connection.route == route && row.confirmed)
                .ok_or(StoreError::Changed)?;
            document.pending.remove(index);
            Ok(())
        })
    }

    fn checkpoint(&self) -> Result<(), StoreError> {
        if (self.cancelled)() {
            Err(StoreError::Interrupted)
        } else if Instant::now() >= self.deadline {
            Err(StoreError::Timeout)
        } else {
            Ok(())
        }
    }

    fn change<T>(
        &self,
        change: impl FnOnce(&mut Document) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.checkpoint()?;
        let mut change = Some(change);
        let mut result = None;
        self.store
            .storage
            .change(self.deadline, self.cancelled, &mut |bytes| {
                self.checkpoint()?;
                let mut document = read(bytes)?;
                result = Some(change.take().ok_or(StoreError::Unavailable)?(
                    &mut document,
                )?);
                validate(&document)?;
                self.checkpoint()?;
                serde_json::to_vec(&document).map_err(|_| StoreError::Unavailable)
            })?;
        result.ok_or(StoreError::Unavailable)
    }
}

fn remove(document: &mut Document, index: usize) -> SavedConnection {
    let mut connection = document.connections.remove(index);
    connection.unpair = UnpairState::Pending;
    document
        .confirmations
        .retain(|row| row.connection.route != connection.route);
    document.pending.push(PendingRevocation {
        connection: connection.clone(),
        confirmed: false,
    });
    connection
}
/// Whether `value` is a `connections.json` this build reads.
pub fn readable(value: &serde_json::Value) -> bool {
    Document::deserialize(value).is_ok_and(|document| validate(&document).is_ok())
}
fn read(bytes: Option<&[u8]>) -> Result<Document, StoreError> {
    let document = bytes
        .map(serde_json::from_slice)
        .transpose()
        .map_err(|_| StoreError::Unavailable)?
        .unwrap_or_default();
    validate(&document).map_err(|_| StoreError::Unavailable)?;
    Ok(document)
}
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "local" && name.len() <= 128 && !name.chars().any(char::is_control)
}
fn valid_pin(pin: &MachinePin) -> bool {
    pin.core != [0; 32]
        && pin.public_key != [0; 32]
        && pin.certificate != [0; 32]
        && !pin.build.is_empty()
        && pin.build.len() <= 128
        && pin
            .build
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}
fn validate(document: &Document) -> Result<(), StoreError> {
    if document.connections.len() + document.pending.len() + document.enrollments.len() > 128
        || document.confirmations.len() > 128
    {
        return Err(StoreError::Unavailable);
    }
    let mut names = BTreeSet::new();
    let mut routes = BTreeSet::new();
    let mut credentials = BTreeSet::new();
    for saved in &document.connections {
        if !valid_name(&saved.name) || !names.insert(&saved.name) {
            return Err(StoreError::NameTaken);
        }
        if saved.unpair != UnpairState::Paired {
            return Err(StoreError::Unavailable);
        }
    }
    for saved in document
        .connections
        .iter()
        .chain(document.pending.iter().map(|row| &row.connection))
    {
        if !valid_name(&saved.name)
            || saved.route == [0; 32]
            || saved.credential == [0; 32]
            || !valid_pin(&saved.paired.pin)
            || saved.paired.pairing == [0; 32]
            || saved.paired.client == [0; 32]
            || !routes.insert(saved.route)
            || !credentials.insert(saved.credential)
            || Endpoint::from_url(&saved.endpoint).is_err()
        {
            return Err(StoreError::Unavailable);
        }
    }
    for enrollment in &document.enrollments {
        if !valid_name(&enrollment.name) || !names.insert(&enrollment.name) {
            return Err(StoreError::NameTaken);
        }
        if enrollment.route == [0; 32]
            || enrollment.claim == [0; 32]
            || enrollment.credential == [0; 32]
            || enrollment.public_key == [0; 32]
            || !routes.insert(enrollment.route)
            || !credentials.insert(enrollment.credential)
            || !valid_pin(&enrollment.pin)
            || Endpoint::from_url(&enrollment.endpoint).is_err()
        {
            return Err(StoreError::Unavailable);
        }
    }
    if document
        .pending
        .iter()
        .any(|row| row.connection.unpair != UnpairState::Pending)
        || document
            .confirmations
            .iter()
            .any(|row| row.token == [0; 32] || row.expires == 0)
    {
        return Err(StoreError::Unavailable);
    }
    Ok(())
}

impl SavedConnection {
    /// Loads this Saved Connection's credential from its selected backend. This synchronous
    /// read may ask the person for access. The returned connector uses the loaded credential
    /// for every dial, so a later deadline or cancellation governs the network waits alone.
    pub fn connector(
        &self,
        credentials: &dyn CredentialStore,
        interaction: Interaction,
        deadline: Option<Instant>,
    ) -> Result<Arc<dyn DirectConnector>, DirectError> {
        let secret = credentials
            .read(&self.credential, interaction)
            .map_err(|error| match error {
                CredentialError::InteractionRequired => DirectError::InteractionRequired,
                CredentialError::Unavailable => DirectError::CredentialUnavailable,
            })?
            .ok_or(DirectError::CredentialUnavailable)?;
        Ok(Arc::new(SavedConnector {
            endpoint: self.endpoint.clone(),
            paired: self.paired.clone(),
            credential: DeviceCredential::from_bytes(&secret)?,
            deadline,
        }))
    }
}

struct SavedConnector {
    endpoint: String,
    paired: PairedDevice,
    credential: DeviceCredential,
    deadline: Option<Instant>,
}

impl DirectConnector for SavedConnector {
    fn connect(&self) -> Result<DirectConnection, DirectError> {
        match self.deadline {
            Some(deadline) => self.connect_by(deadline),
            None => DirectConnection::connect(&self.endpoint, &self.paired, &self.credential),
        }
    }

    fn connect_with(&self, wait: &WaitLimit) -> Result<DirectConnection, DirectError> {
        DirectConnection::connect_with(&self.endpoint, &self.paired, &self.credential, wait)
    }
}
