//! The CLI's account-private Saved Connections and exact recovery requests.
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use spaceterm_server::direct::{Endpoint, PairedDevice, ServerPin};
use spaceterm_server::{Directory, Lock, LockWait, ServerError};

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
    pub pin: ServerPin,
    pub claim: [u8; 32],
    pub credential: [u8; 32],
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

/// One client's Saved Connections file. Construction and reads create nothing.
pub struct ConnectionStore {
    root: PathBuf,
}

/// Mutations bounded by one command's deadline and cancellation authority.
pub struct ConnectionTransaction<'a> {
    store: &'a ConnectionStore,
    deadline: Instant,
    cancelled: &'a dyn Fn() -> bool,
}

impl ConnectionStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
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

    fn read(&self) -> Result<Document, StoreError> {
        match Directory::open(&self.root, false) {
            Ok(directory) => read(&directory),
            Err(ServerError::NotSetUp) => Ok(Document::default()),
            Err(_) => Err(StoreError::Unavailable),
        }
    }

    /// Serializes a live enrollment or cleanup without blocking unrelated connections.
    pub fn recovery_guard(&self, route: &[u8; 32]) -> Result<Option<Lock>, StoreError> {
        let directory = Directory::open(&self.root, true).map_err(|_| StoreError::Unavailable)?;
        let name: String = route.iter().map(|byte| format!("{byte:02x}")).collect();
        directory
            .try_lock(&format!("connection-{name}.lock"))
            .map_err(|_| StoreError::Unavailable)
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
        let directory =
            Directory::open(&self.store.root, true).map_err(|_| StoreError::Unavailable)?;
        let limit = self.deadline.saturating_duration_since(Instant::now());
        let _lock = match directory
            .wait_lock("connections.lock", limit, Some(self.cancelled))
            .map_err(|_| StoreError::Unavailable)?
        {
            LockWait::Locked(lock) => lock,
            LockWait::Held => return Err(StoreError::Timeout),
            LockWait::Cancelled => return Err(StoreError::Interrupted),
        };
        self.checkpoint()?;
        directory
            .verify_path(&self.store.root)
            .map_err(|_| StoreError::Unavailable)?;
        let mut document = read(&directory)?;
        let result = change(&mut document)?;
        validate(&document)?;
        self.checkpoint()?;
        directory
            .write_json(c"connections.json", &document)
            .map_err(|_| StoreError::Unavailable)?;
        Ok(result)
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
fn read(directory: &Directory) -> Result<Document, StoreError> {
    let document = directory
        .read_json("connections.json")
        .map_err(|_| StoreError::Unavailable)?
        .unwrap_or_default();
    validate(&document).map_err(|_| StoreError::Unavailable)?;
    Ok(document)
}
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "local" && name.len() <= 128 && !name.chars().any(char::is_control)
}
fn valid_pin(pin: &ServerPin) -> bool {
    pin.server != [0; 32]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    fn transaction(store: &ConnectionStore) -> ConnectionTransaction<'_> {
        store.transaction(Instant::now() + std::time::Duration::from_secs(30), &|| {
            false
        })
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "spaceterm-cli-e-{}-{}",
                    std::process::id(),
                    u128::from_be_bytes(crate::random_id().unwrap()[..16].try_into().unwrap())
                ));
            std::fs::create_dir(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(root)
        }
        fn store(&self) -> ConnectionStore {
            ConnectionStore::new(self.0.join("server/clients/cli"))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn saved(id: u8) -> SavedConnection {
        let route = crate::tests::saved(id, id + 5);
        SavedConnection {
            name: format!("server-{id}"),
            route: route.route.id,
            endpoint: route.route.endpoint,
            paired: route.route.paired,
            credential: route.credential,
            unpair: UnpairState::Paired,
        }
    }

    #[test]
    fn rename_changes_only_the_name_and_invalidates_removal_previews() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let mut expected = saved(1);
        transaction(&store)
            .commit_guarded(|rows| {
                rows.push(expected.clone());
                Ok(())
            })
            .unwrap();
        let preview = transaction(&store)
            .preview_remove(&expected.name, 1)
            .unwrap();
        expected.name = "penguin".into();
        assert_eq!(
            transaction(&store).rename("server-1", "penguin").unwrap(),
            expected
        );
        assert_eq!(store.load().unwrap(), vec![expected]);
        transaction(&store).rename("penguin", "server-1").unwrap();
        assert_eq!(
            transaction(&store).confirm_remove("server-1", &preview.token, 2),
            Err(StoreError::Changed)
        );
    }

    #[test]
    fn deadline_and_cancellation_are_checked_again_before_publication() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;
        for expected in [StoreError::Timeout, StoreError::Interrupted] {
            let fixture = Fixture::new();
            let store = fixture.store();
            transaction(&store)
                .commit_guarded(|rows| {
                    rows.push(saved(1));
                    Ok(())
                })
                .unwrap();
            let before = std::fs::read(store.root.join("connections.json")).unwrap();
            let cancelled = AtomicBool::new(false);
            let deadline = Instant::now()
                + if expected == StoreError::Timeout {
                    Duration::from_millis(50)
                } else {
                    Duration::from_secs(5)
                };
            let result = store
                .transaction(deadline, &|| cancelled.load(Ordering::Relaxed))
                .commit_guarded(|rows| {
                    rows[0].name = "must-not-publish".into();
                    if expected == StoreError::Timeout {
                        std::thread::sleep(Duration::from_millis(100));
                    } else {
                        cancelled.store(true, Ordering::Relaxed);
                    }
                    Ok(())
                });
            assert_eq!(result, Err(expected));
            assert_eq!(
                std::fs::read(store.root.join("connections.json")).unwrap(),
                before
            );
        }
    }

    #[test]
    fn failed_renames_preserve_the_document_including_reserved_enrollments() {
        let fixture = Fixture::new();
        let store = fixture.store();
        transaction(&store)
            .commit_guarded(|rows| {
                rows.extend([saved(1), saved(2)]);
                Ok(())
            })
            .unwrap();
        let connection = saved(3);
        transaction(&store)
            .reserve(PendingEnrollment {
                route: connection.route,
                name: connection.name,
                endpoint: connection.endpoint,
                pin: connection.paired.pin,
                claim: [8; 32],
                credential: connection.credential,
                public_key: [9; 32],
                offered: false,
                confirmed: false,
            })
            .unwrap();
        let before = std::fs::read(store.root.join("connections.json")).unwrap();
        for (old, new, error) in [
            ("server-1", "server-2", StoreError::NameTaken),
            ("server-1", "server-1", StoreError::NameTaken),
            ("server-1", "server-3", StoreError::NameTaken),
            ("missing", "available", StoreError::NotFound),
            ("server-1", "local", StoreError::NameTaken),
        ] {
            assert_eq!(transaction(&store).rename(old, new), Err(error));
            assert_eq!(
                std::fs::read(store.root.join("connections.json")).unwrap(),
                before
            );
        }
    }

    #[test]
    fn absent_store_is_passive_and_removal_retains_only_restricted_cleanup() {
        let fixture = Fixture::new();
        let store = fixture.store();
        assert!(store.load().unwrap().is_empty());
        assert!(!store.root.exists());
        transaction(&store)
            .commit_guarded(|rows| {
                rows.push(saved(1));
                Ok(())
            })
            .unwrap();
        let removed = transaction(&store).remove("server-1").unwrap();
        assert!(store.load().unwrap().is_empty());
        assert_eq!(store.pending().unwrap()[0].paired, removed.paired);
        transaction(&store)
            .confirm_revocation(removed.route)
            .unwrap();
        transaction(&store).retire_pending(removed.route).unwrap();
        assert!(store.pending().unwrap().is_empty());
        assert_eq!(
            std::fs::metadata(store.root.join("connections.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn process_writer() {
        let Some(root) = std::env::var_os("SPACETERM_TEST_CONNECTION_ROOT") else {
            return;
        };
        let id = std::env::var("SPACETERM_TEST_CONNECTION_ID")
            .unwrap()
            .parse()
            .unwrap();
        ConnectionStore::new(root.into())
            .transaction(Instant::now() + std::time::Duration::from_secs(30), &|| {
                false
            })
            .commit_guarded(|rows| {
                rows.push(saved(id));
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn concurrent_processes_preserve_both_adds_and_desktop_fixture_bytes() {
        let fixture = Fixture::new();
        let desktop = fixture.0.join("direct-device-connections.json");
        let bytes = include_bytes!("../tests/fixtures/desktop-connections.json");
        let document: crate::records::Document = serde_json::from_slice(bytes).unwrap();
        assert_eq!(document.routes.len(), 1);
        std::fs::write(&desktop, bytes).unwrap();
        let mut children = Vec::new();
        for id in [1, 2] {
            children.push(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "store::tests::process_writer", "--nocapture"])
                    .env("SPACETERM_TEST_CONNECTION_ROOT", fixture.store().root)
                    .env("SPACETERM_TEST_CONNECTION_ID", id.to_string())
                    .spawn()
                    .unwrap(),
            );
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }
        let rows = fixture.store().load().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.name == "server-1"));
        assert!(rows.iter().any(|row| row.name == "server-2"));
        assert_eq!(std::fs::read(&desktop).unwrap(), bytes);
    }
}
