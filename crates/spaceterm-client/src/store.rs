//! Saved Connections and exact recovery requests.
use crate::WaitLimit;
use crate::credentials::{CredentialError, CredentialStore, Interaction};
use crate::direct::{DeviceCredential, DirectConnection, DirectConnector, DirectError};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, mpsc};
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
    /// Binds this Saved Connection to its selected credential store. Each dial reads the
    /// credential with the requested interaction policy. Pending removals cannot dial.
    /// A deadline bounds the credential wait as well as the dial.
    /// A store read already executing can finish after the wait ends. Its result is discarded.
    /// A bounded dial fails with [`DirectError::Transport`] if its store is already reading
    /// for another bounded dial or both bounded read slots are occupied.
    pub fn connector(
        &self,
        credentials: Arc<dyn CredentialStore>,
        interaction: Interaction,
        deadline: Option<Instant>,
    ) -> Arc<dyn DirectConnector> {
        Arc::new(SavedConnector {
            endpoint: self.endpoint.clone(),
            paired: self.paired.clone(),
            key: self.credential,
            unpair: self.unpair,
            credentials,
            interaction,
            deadline,
        })
    }
}

struct SavedConnector {
    endpoint: String,
    paired: PairedDevice,
    key: [u8; 32],
    unpair: UnpairState,
    credentials: Arc<dyn CredentialStore>,
    interaction: Interaction,
    deadline: Option<Instant>,
}

impl SavedConnector {
    fn check_paired(&self) -> Result<(), DirectError> {
        if self.unpair != UnpairState::Paired {
            return Err(DirectError::Revoked);
        }
        Ok(())
    }
}

impl DirectConnector for SavedConnector {
    fn connect(&self) -> Result<DirectConnection, DirectError> {
        if let Some(deadline) = self.deadline {
            return self.connect_by(deadline);
        }
        self.check_paired()?;
        let credential = read_credential(self.credentials.as_ref(), &self.key, self.interaction)?;
        DirectConnection::connect(&self.endpoint, &self.paired, &credential)
    }

    fn connect_with(&self, wait: &WaitLimit) -> Result<DirectConnection, DirectError> {
        self.check_paired()?;
        let credential = CredentialRead::within(self, wait)?;
        DirectConnection::connect_with(&self.endpoint, &self.paired, &credential, wait)
    }
}

fn read_credential(
    store: &dyn CredentialStore,
    key: &[u8; 32],
    interaction: Interaction,
) -> Result<DeviceCredential, DirectError> {
    let secret = store
        .read(key, interaction)
        .map_err(|error| match error {
            CredentialError::InteractionRequired => DirectError::InteractionRequired,
            CredentialError::Unavailable => DirectError::CredentialUnavailable,
        })?
        .ok_or(DirectError::CredentialUnavailable)?;
    DeviceCredential::from_bytes(&secret)
}

const MAX_ACTIVE_CREDENTIAL_READS: usize = 2;
static ACTIVE_CREDENTIAL_READS: Mutex<Vec<Arc<dyn CredentialStore>>> = Mutex::new(Vec::new());

struct CredentialRead(Arc<dyn CredentialStore>);

impl CredentialRead {
    fn within(
        connector: &SavedConnector,
        wait: &WaitLimit,
    ) -> Result<DeviceCredential, DirectError> {
        if wait.ended() {
            return Err(DirectError::Transport);
        }
        let read = Self::reserve(&connector.credentials)?;
        let key = connector.key;
        let interaction = connector.interaction;
        let worker_wait = wait.clone();
        let (reply, received) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("spaceterm-credential-read".into())
            .spawn(move || {
                if !worker_wait.ended() {
                    let result = read_credential(read.0.as_ref(), &key, interaction);
                    drop(read);
                    if !worker_wait.ended() {
                        let _ = reply.send(result);
                    }
                }
            })
            .map_err(|_| DirectError::Transport)?;
        while !wait.ended() {
            let remaining = wait.next_poll().saturating_duration_since(Instant::now());
            match received.recv_timeout(remaining) {
                Ok(result) if !wait.ended() => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                _ => break,
            }
        }
        Err(DirectError::Transport)
    }

    fn reserve(store: &Arc<dyn CredentialStore>) -> Result<Self, DirectError> {
        // A native read can outlive its caller. Hold one slot per store for the whole read
        // so retries cannot accumulate or prevent another store from running.
        let mut active = ACTIVE_CREDENTIAL_READS
            .lock()
            .map_err(|_| DirectError::Transport)?;
        if active.len() >= MAX_ACTIVE_CREDENTIAL_READS
            || active.iter().any(|reader| Arc::ptr_eq(reader, store))
        {
            return Err(DirectError::Transport);
        }
        active.push(Arc::clone(store));
        Ok(Self(Arc::clone(store)))
    }
}

impl Drop for CredentialRead {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE_CREDENTIAL_READS.lock() {
            active.retain(|reader| !Arc::ptr_eq(reader, &self.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use std::net::UdpSocket;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::Duration;
    use zeroize::Zeroizing;

    struct BlockedStore {
        credential: Option<Zeroizing<Vec<u8>>>,
        blocked: AtomicBool,
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        returned: mpsc::Sender<()>,
    }

    impl CredentialStore for BlockedStore {
        fn read(
            &self,
            _: &[u8; 32],
            _: Interaction,
        ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError> {
            if self.blocked.load(Ordering::Acquire) {
                let _ = self.entered.send(());
                let _ = self
                    .release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5));
                let _ = self.returned.send(());
            }
            Ok(self.credential.clone())
        }

        fn insert(&self, _: &[u8; 32], _: &[u8]) -> Result<(), CredentialError> {
            Err(CredentialError::Unavailable)
        }

        fn remove(&self, _: &[u8; 32], _: Interaction) -> Result<(), CredentialError> {
            Err(CredentialError::Unavailable)
        }
    }

    struct ReadControl {
        started: mpsc::Receiver<()>,
        release: mpsc::Sender<()>,
        returned: mpsc::Receiver<()>,
    }

    fn blocked_store(credential: Option<Zeroizing<Vec<u8>>>) -> (Arc<BlockedStore>, ReadControl) {
        let (entered, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (finished, returned) = mpsc::channel();
        (
            Arc::new(BlockedStore {
                credential,
                blocked: AtomicBool::new(false),
                entered,
                release: Mutex::new(released),
                returned: finished,
            }),
            ReadControl {
                started,
                release,
                returned,
            },
        )
    }

    struct EmptyStore;

    impl CredentialStore for EmptyStore {
        fn read(
            &self,
            _: &[u8; 32],
            _: Interaction,
        ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError> {
            Ok(None)
        }

        fn insert(&self, _: &[u8; 32], _: &[u8]) -> Result<(), CredentialError> {
            Err(CredentialError::Unavailable)
        }

        fn remove(&self, _: &[u8; 32], _: Interaction) -> Result<(), CredentialError> {
            Err(CredentialError::Unavailable)
        }
    }

    fn assert_credential_slots(
        saved: &SavedConnection,
        first_store: Arc<BlockedStore>,
        release_first: &mpsc::Sender<()>,
    ) {
        let retry = saved.connector(first_store, Interaction::Quiet, None);
        let wait = WaitLimit::new(Instant::now() + Duration::from_secs(1));
        assert_eq!(
            retry.connect_with(&wait).err(),
            Some(DirectError::Transport)
        );

        let (second_store, second) = blocked_store(None);
        let second_dial = saved.connector(second_store.clone(), Interaction::Quiet, None);
        second_store.blocked.store(true, Ordering::Release);
        let worker = std::thread::spawn(move || {
            let wait = WaitLimit::new(Instant::now() + Duration::from_secs(4));
            second_dial.connect_with(&wait).err()
        });
        second.started.recv_timeout(Duration::from_secs(2)).unwrap();

        let third = saved.connector(Arc::new(EmptyStore), Interaction::Quiet, None);
        let wait = WaitLimit::new(Instant::now() + Duration::from_secs(1));
        assert_eq!(
            third.connect_with(&wait).err(),
            Some(DirectError::Transport),
            "both credential read slots are in use",
        );

        release_first.send(()).unwrap();
        let wait = WaitLimit::new(Instant::now() + Duration::from_secs(1));
        let result = loop {
            let result = third.connect_with(&wait).err();
            if result != Some(DirectError::Transport) || wait.ended() {
                break result;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            result,
            Some(DirectError::CredentialUnavailable),
            "the cancelled read released its slot while the other read remained blocked",
        );
        second.release.send(()).unwrap();
        assert_eq!(
            worker.join().unwrap(),
            Some(DirectError::CredentialUnavailable)
        );
    }

    #[test]
    fn pending_removal_refuses_ordinary_dials_before_loading_the_credential() {
        let (store, read) = blocked_store(None);
        store.blocked.store(true, Ordering::Release);
        read.release.send(()).unwrap();
        let mut saved = saved_connection("quic://127.0.0.1:1".into());
        saved.unpair = UnpairState::Pending;
        let connector = saved.connector(store, Interaction::Requested, None);
        let deadline = Instant::now() + Duration::from_secs(1);

        assert_eq!(connector.connect().err(), Some(DirectError::Revoked));
        assert_eq!(
            connector.connect_by(deadline).err(),
            Some(DirectError::Revoked)
        );
        assert_eq!(
            connector.connect_with(&WaitLimit::new(deadline)).err(),
            Some(DirectError::Revoked)
        );
        drop(connector);
        assert_eq!(
            read.started.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        );
    }

    fn saved_connection(endpoint: String) -> SavedConnection {
        SavedConnection {
            name: "Paired Machine".into(),
            route: [1; 32],
            endpoint,
            paired: PairedDevice {
                pin: MachinePin {
                    build: "spaceterm-test".into(),
                    core: [2; 32],
                    public_key: [3; 32],
                    certificate: [4; 32],
                },
                pairing: [5; 32],
                client: [6; 32],
            },
            credential: [7; 32],
            credential_store: StoreKind::PrivateFile,
            unpair: UnpairState::Paired,
        }
    }

    #[test]
    fn saved_connector_discards_credentials_after_cancel_or_deadline() {
        for cancelling in [true, false] {
            let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
            socket
                .set_read_timeout(Some(Duration::from_millis(150)))
                .unwrap();
            let credential = DeviceCredential::generate().unwrap();
            let (store, read) =
                blocked_store(Some(Zeroizing::new(credential.expose_bytes().to_vec())));
            let saved = saved_connection(format!("quic://{}", socket.local_addr().unwrap()));
            let connector = saved.connector(store.clone(), Interaction::Quiet, None);
            store.blocked.store(true, Ordering::Release);
            let cancel = Arc::new(AtomicBool::new(false));
            let deadline = Instant::now()
                + if cancelling {
                    Duration::from_secs(4)
                } else {
                    Duration::from_millis(500)
                };
            let wait = WaitLimit::new(deadline).with_cancellation(Arc::clone(&cancel));
            let (finished, dial_finished) = mpsc::channel();
            let dialer = Arc::clone(&connector);
            let worker = std::thread::spawn(move || {
                let result = if cancelling {
                    dialer.connect_with(&wait)
                } else {
                    dialer.connect_by(wait.deadline())
                };
                finished.send(result.err()).unwrap();
            });
            let entered_before_expiry = read.started.recv_timeout(Duration::from_secs(2));
            if cancelling {
                cancel.store(true, Ordering::Release);
            }
            let result = dial_finished.recv_timeout(Duration::from_secs(1));
            if cancelling && entered_before_expiry.is_ok() && result.is_ok() {
                assert_credential_slots(&saved, store, &read.release);
            } else {
                let _ = read.release.send(());
            }
            drop(read.release);
            worker.join().unwrap();
            assert!(
                entered_before_expiry.is_ok(),
                "the dial must read its credential"
            );
            assert_eq!(result, Ok(Some(DirectError::Transport)));
            read.returned.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(matches!(
                socket.recv_from(&mut [0; 1500]).unwrap_err().kind(),
                ErrorKind::WouldBlock | ErrorKind::TimedOut
            ));
            assert_eq!(read.started.try_recv(), Err(mpsc::TryRecvError::Empty));
        }
    }
}
