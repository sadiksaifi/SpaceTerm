//! The CLI's private directory adapter for Saved Connections.
use std::path::PathBuf;
use std::time::Instant;

use crate::{CoreError, Directory, Lock, LockWait};

use spaceterm_client::{ConnectionStorage, RecoveryGuard, SavedConnectionChange, StoreError};

pub struct PrivateConnections(PathBuf);

impl RecoveryGuard for Lock {}

impl PrivateConnections {
    pub fn new(root: PathBuf) -> Self {
        Self(root)
    }
}

impl ConnectionStorage for PrivateConnections {
    fn load(&self) -> Result<Option<Vec<u8>>, StoreError> {
        match Directory::open(&self.0, false) {
            Ok(directory) => directory
                .read_bytes("connections.json")
                .map_err(|_| StoreError::Unavailable),
            Err(CoreError::NotSetUp) => Ok(None),
            Err(_) => Err(StoreError::Unavailable),
        }
    }

    fn change(
        &self,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
        change: &mut SavedConnectionChange<'_>,
    ) -> Result<(), StoreError> {
        let directory = Directory::open(&self.0, true).map_err(|_| StoreError::Unavailable)?;
        let limit = deadline.saturating_duration_since(Instant::now());
        let _lock = match directory
            .wait_lock("connections.lock", limit, Some(cancelled))
            .map_err(|_| StoreError::Unavailable)?
        {
            LockWait::Locked(lock) => lock,
            LockWait::Held => return Err(StoreError::Timeout),
            LockWait::Cancelled => return Err(StoreError::Interrupted),
        };
        if cancelled() {
            return Err(StoreError::Interrupted);
        }
        if Instant::now() >= deadline {
            return Err(StoreError::Timeout);
        }
        directory
            .verify_path(&self.0)
            .map_err(|_| StoreError::Unavailable)?;
        let bytes = directory
            .read_bytes("connections.json")
            .map_err(|_| StoreError::Unavailable)?;
        let replacement = change(bytes.as_deref())?;
        directory
            .write_bytes(c"connections.json", &replacement)
            .map_err(|_| StoreError::Unavailable)
    }

    fn try_recovery(&self, route: &[u8; 32]) -> Result<Option<Box<dyn RecoveryGuard>>, StoreError> {
        let directory = Directory::open(&self.0, true).map_err(|_| StoreError::Unavailable)?;
        let name: String = route.iter().map(|byte| format!("{byte:02x}")).collect();
        directory
            .try_lock(&format!("connection-{name}.lock"))
            .map(|lock| lock.map(|lock| Box::new(lock) as Box<dyn RecoveryGuard>))
            .map_err(|_| StoreError::Unavailable)
    }
}

pub fn connection_store(root: PathBuf) -> spaceterm_client::ConnectionStore {
    spaceterm_client::ConnectionStore::new(std::sync::Arc::new(PrivateConnections::new(root)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_client::{
        ConnectionStore, ConnectionTransaction, PendingEnrollment, SavedConnection, StoreKind,
        UnpairState,
    };
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

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
                    u128::from_be_bytes(
                        crate::protocol::random().unwrap()[..16].try_into().unwrap()
                    )
                ));
            std::fs::create_dir(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(root)
        }
        fn root(&self) -> PathBuf {
            self.0.join("core/clients/cli")
        }
        fn store(&self) -> ConnectionStore {
            super::connection_store(self.root())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn saved(id: u8) -> SavedConnection {
        let route = spaceterm_client::tests::saved(id, id + 5);
        SavedConnection {
            name: format!("server-{id}"),
            route: route.route.id,
            endpoint: route.route.endpoint,
            paired: route.route.paired,
            credential: route.credential,
            credential_store: StoreKind::Keychain,
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
            let before = std::fs::read(fixture.root().join("connections.json")).unwrap();
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
                std::fs::read(fixture.root().join("connections.json")).unwrap(),
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
                credential_store: connection.credential_store,
                public_key: [9; 32],
                offered: false,
                confirmed: false,
            })
            .unwrap();
        let before = std::fs::read(fixture.root().join("connections.json")).unwrap();
        for (old, new, error) in [
            ("server-1", "server-2", StoreError::NameTaken),
            ("server-1", "server-1", StoreError::NameTaken),
            ("server-1", "server-3", StoreError::NameTaken),
            ("missing", "available", StoreError::NotFound),
            ("server-1", "local", StoreError::NameTaken),
        ] {
            assert_eq!(transaction(&store).rename(old, new), Err(error));
            assert_eq!(
                std::fs::read(fixture.root().join("connections.json")).unwrap(),
                before
            );
        }
    }

    #[test]
    fn absent_store_is_passive_and_removal_retains_only_restricted_cleanup() {
        let fixture = Fixture::new();
        let store = fixture.store();
        assert!(store.load().unwrap().is_empty());
        assert!(!fixture.root().exists());
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
            std::fs::metadata(fixture.root().join("connections.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    /// An interrupted `machine add` leaves only its pending enrollment, and that names a
    /// credential item too.
    #[test]
    fn credential_items_include_saved_pending_and_enrolling_connections() {
        let fixture = Fixture::new();
        let store = fixture.store();
        assert!(store.credential_items().unwrap().is_empty());
        transaction(&store)
            .commit_guarded(|rows| {
                rows.extend([saved(1), saved(2)]);
                Ok(())
            })
            .unwrap();
        transaction(&store).remove("server-2").unwrap();
        let enrolling = saved(3);
        transaction(&store)
            .reserve(PendingEnrollment {
                route: enrolling.route,
                name: enrolling.name,
                endpoint: enrolling.endpoint,
                pin: enrolling.paired.pin,
                claim: [8; 32],
                credential: enrolling.credential,
                credential_store: StoreKind::PrivateFile,
                public_key: [9; 32],
                offered: false,
                confirmed: false,
            })
            .unwrap();

        assert_eq!(
            store.credential_items().unwrap(),
            vec![
                (StoreKind::Keychain, saved(1).credential),
                (StoreKind::Keychain, saved(2).credential),
                (StoreKind::PrivateFile, enrolling.credential),
            ]
        );
    }
}
