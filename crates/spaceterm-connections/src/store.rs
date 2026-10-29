//! The CLI's account-private Saved Connections, separate from desktop presentation.
use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use spaceterm_server::direct::{Endpoint, PairedDevice};
use spaceterm_server::{Directory, ServerError};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum UnpairState {
    Paired,
    Pending,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    Unavailable,
    Changed,
    NameTaken,
    NotFound,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    connections: Vec<SavedConnection>,
    pending: Vec<SavedConnection>,
}

/// Only an explicit CLI namespace grants authority to open this store.
pub struct ConnectionStore {
    root: PathBuf,
}

impl ConnectionStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Reading absent state never creates a directory, lock or credential item.
    pub fn load(&self) -> Result<Vec<SavedConnection>, StoreError> {
        Ok(self.read()?.connections)
    }

    pub fn pending(&self) -> Result<Vec<SavedConnection>, StoreError> {
        Ok(self.read()?.pending)
    }

    fn read(&self) -> Result<Document, StoreError> {
        match Directory::open(&self.root, false) {
            Ok(directory) => read(&directory),
            Err(ServerError::NotSetUp) => Ok(Document::default()),
            Err(_) => Err(StoreError::Unavailable),
        }
    }

    /// The lock covers the current read, the caller's change, and atomic publication.
    pub fn commit_guarded<T>(
        &self,
        change: impl FnOnce(&mut Vec<SavedConnection>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.change(|document| change(&mut document.connections))
    }

    /// Local removal is immediate. Restricted revocation remains in the pending queue.
    pub fn remove(&self, name: &str) -> Result<SavedConnection, StoreError> {
        self.change(|document| {
            let index = document
                .connections
                .iter()
                .position(|saved| saved.name == name)
                .ok_or(StoreError::NotFound)?;
            let mut saved = document.connections.remove(index);
            saved.unpair = UnpairState::Pending;
            document.pending.push(saved.clone());
            Ok(saved)
        })
    }

    /// Retire only an exact server-confirmed revocation.
    pub fn retire_pending(&self, route: [u8; 32]) -> Result<(), StoreError> {
        self.change(|document| {
            let index = document
                .pending
                .iter()
                .position(|saved| saved.route == route)
                .ok_or(StoreError::NotFound)?;
            document.pending.remove(index);
            Ok(())
        })
    }

    fn change<T>(
        &self,
        change: impl FnOnce(&mut Document) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let directory = Directory::open(&self.root, true).map_err(|_| StoreError::Unavailable)?;
        let _lock = directory
            .lock("connections.lock", false)
            .map_err(|_| StoreError::Unavailable)?;
        directory
            .verify_path(&self.root)
            .map_err(|_| StoreError::Unavailable)?;
        let mut document = read(&directory)?;
        let result = change(&mut document)?;
        validate(&document)?;
        directory
            .write_json(c"connections.json", &document)
            .map_err(|_| StoreError::Unavailable)?;
        Ok(result)
    }
}

fn read(directory: &Directory) -> Result<Document, StoreError> {
    let document = directory
        .read_json("connections.json")
        .map_err(|_| StoreError::Unavailable)?
        .unwrap_or_default();
    validate(&document)?;
    Ok(document)
}

fn validate(document: &Document) -> Result<(), StoreError> {
    if document.connections.len() + document.pending.len() > 128 {
        return Err(StoreError::Unavailable);
    }
    let mut names = BTreeSet::new();
    let mut routes = BTreeSet::new();
    for saved in &document.connections {
        if saved.name.is_empty()
            || saved.name == "local"
            || saved.name.len() > 128
            || saved.name.chars().any(char::is_control)
            || !names.insert(&saved.name)
        {
            return Err(StoreError::NameTaken);
        }
        if saved.unpair != UnpairState::Paired {
            return Err(StoreError::Unavailable);
        }
    }
    for saved in document.connections.iter().chain(&document.pending) {
        if saved.route == [0; 32]
            || saved.credential == [0; 32]
            || saved.paired.pin.server == [0; 32]
            || saved.paired.pin.public_key == [0; 32]
            || saved.paired.pin.certificate == [0; 32]
            || saved.paired.pairing == [0; 32]
            || saved.paired.client == [0; 32]
            || !routes.insert(saved.route)
            || Endpoint::from_url(&saved.endpoint).is_err()
        {
            return Err(StoreError::Unavailable);
        }
    }
    if document
        .pending
        .iter()
        .any(|saved| saved.unpair != UnpairState::Pending)
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
    fn absent_store_is_passive_and_removal_retains_only_restricted_cleanup() {
        let fixture = Fixture::new();
        let store = fixture.store();
        assert!(store.load().unwrap().is_empty());
        assert!(!store.root.exists());
        store
            .commit_guarded(|rows| {
                rows.push(saved(1));
                Ok(())
            })
            .unwrap();
        let removed = store.remove("server-1").unwrap();
        assert!(store.load().unwrap().is_empty());
        assert_eq!(store.pending().unwrap()[0].paired, removed.paired);
        store.retire_pending(removed.route).unwrap();
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
