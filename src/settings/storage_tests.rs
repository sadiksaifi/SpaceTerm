use super::storage::*;
use crate::platform::app_paths::{AppPathEnvironment, AppPathHostFacts, AppPaths};
use crate::platform::secure_filesystem::{SecureCommitOutcome, SecureFilesystemError};
use crate::platform::testing::RecordingFilesystem;
use std::sync::Arc;

fn storage() -> (ConfigSettingsStorage, Arc<RecordingFilesystem>) {
    let filesystem = Arc::new(RecordingFilesystem::default());
    let paths = AppPaths::resolve(
        &AppPathEnvironment {
            home: Some("/home/test".into()),
            ..Default::default()
        },
        &AppPathHostFacts::new("/runtime".into(), 200).unwrap(),
        filesystem.clone(),
    )
    .unwrap();
    (ConfigSettingsStorage::new(Arc::new(paths)), filesystem)
}

#[test]
fn missing_config_is_read_without_creating_directories() {
    let (storage, filesystem) = storage();
    assert!(storage.read().unwrap().is_none());
    assert!(filesystem.events.lock().unwrap().is_empty());
}

#[test]
fn unsafe_and_unavailable_roots_return_typed_storage_failures() {
    for (filesystem_error, storage_error) in [
        (SecureFilesystemError::Unsafe, StorageError::Unsafe),
        (
            SecureFilesystemError::Unavailable,
            StorageError::Unavailable,
        ),
    ] {
        let (storage, filesystem) = storage();
        *filesystem.root_failure.lock().unwrap() = Some(filesystem_error);

        assert!(matches!(storage.read(), Err(error) if error == storage_error));
        assert!(matches!(
            storage.write(b"candidate", None),
            Err(error) if error == storage_error
        ));
        assert_eq!(filesystem.prepared_file_count(), 0);
    }
}

#[test]
fn exact_committed_bytes_grant_refreshed_identity_and_stale_writes_conflict() {
    let (storage, _) = storage();
    let first = storage.write(b"first", None).unwrap();
    assert!(first.identity.is_some());
    let second = storage.write(b"second", first.identity.as_ref()).unwrap();
    assert!(second.identity.is_some());
    assert!(matches!(
        storage.write(b"stale", first.identity.as_ref()),
        Err(StorageError::Conflict)
    ));
    assert_eq!(storage.read().unwrap().unwrap().bytes, b"second");
}

#[test]
fn successful_commit_never_captures_a_successors_identity() {
    let (storage, filesystem) = storage();
    filesystem.files.lock().unwrap().successor_after_commit = Some(b"other writer".to_vec());
    let result = storage.write(b"candidate", None).unwrap();
    assert_eq!(result.durability, Durability::Synchronized);
    assert!(result.identity.is_none());
    assert_eq!(storage.read().unwrap().unwrap().bytes, b"other writer");
}

#[test]
fn successful_commit_never_captures_a_byte_identical_successors_identity() {
    let (storage, filesystem) = storage();
    filesystem.files.lock().unwrap().successor_after_commit = Some(b"candidate".to_vec());

    let result = storage.write(b"candidate", None).unwrap();

    assert_eq!(result.durability, Durability::Synchronized);
    assert!(result.identity.is_none());
    assert!(matches!(
        storage.write(b"replacement", result.identity.as_ref()),
        Err(StorageError::Conflict)
    ));
    assert_eq!(storage.read().unwrap().unwrap().bytes, b"candidate");
}

#[test]
fn uncertain_sync_does_not_turn_a_committed_write_into_failure() {
    let (storage, filesystem) = storage();
    filesystem.files.lock().unwrap().commit_outcome =
        Some(SecureCommitOutcome::CommittedButUnsynced);
    let result = storage.write(b"candidate", None).unwrap();
    assert_eq!(result.durability, Durability::Uncertain);
    assert!(result.identity.is_some());
}

#[test]
fn temporary_collisions_are_bounded_and_oversized_writes_do_not_create_config() {
    let (storage, filesystem) = storage();
    assert!(matches!(
        storage.write(&vec![0; MAXIMUM_DOCUMENT_BYTES + 1], None),
        Err(StorageError::TooLarge)
    ));
    assert!(filesystem.events.lock().unwrap().is_empty());
    filesystem.files.lock().unwrap().prepare_failures = 100;
    assert!(matches!(
        storage.write(b"candidate", None),
        Err(StorageError::Unavailable)
    ));
    assert_eq!(filesystem.files.lock().unwrap().prepare_count, 16);
    assert!(storage.read().unwrap().is_none());
}

#[test]
fn noncollision_prepare_failure_is_not_retried() {
    let (storage, filesystem) = storage();
    filesystem.files.lock().unwrap().prepare_error = Some(SecureFilesystemError::Unsafe);

    assert!(matches!(
        storage.write(b"candidate", None),
        Err(StorageError::Unsafe)
    ));
    assert_eq!(filesystem.files.lock().unwrap().prepare_count, 1);
    assert_eq!(filesystem.prepared_file_count(), 0);
    assert!(storage.read().unwrap().is_none());
}

#[test]
fn commit_failure_cleans_up_the_prepared_file_without_publication() {
    let (storage, filesystem) = storage();
    filesystem.files.lock().unwrap().commit_error = Some(SecureFilesystemError::Unavailable);

    assert!(matches!(
        storage.write(b"candidate", None),
        Err(StorageError::Unavailable)
    ));
    assert_eq!(filesystem.prepared_file_count(), 0);
    assert!(storage.read().unwrap().is_none());
}

#[test]
fn oversized_committed_document_is_rejected_on_read() {
    let (storage, filesystem) = storage();
    storage.write(b"candidate", None).unwrap();
    let mut files = filesystem.files.lock().unwrap();
    let (bytes, _) = files.values.first_entry().unwrap().into_mut();
    *bytes = vec![0; MAXIMUM_DOCUMENT_BYTES + 1];
    drop(files);

    assert!(matches!(storage.read(), Err(StorageError::Unsafe)));
}

#[cfg(all(target_os = "macos", feature = "macos-native-tests"))]
mod native {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        path::PathBuf,
    };

    struct Fixture {
        root: PathBuf,
        paths: Arc<AppPaths>,
    }
    impl Fixture {
        fn new() -> Self {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).unwrap();
            let root = fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "spaceterm-settings-{:032x}",
                    u128::from_ne_bytes(nonce)
                ));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let paths = AppPaths::resolve(
                &AppPathEnvironment {
                    home: Some(root.clone().into_os_string()),
                    xdg_config_home: Some(root.join("config").into_os_string()),
                    ..Default::default()
                },
                &AppPathHostFacts::new(root.clone(), 103).unwrap(),
                Arc::new(crate::platform::macos_secure_filesystem::MacosSecureFilesystem),
            )
            .unwrap();
            Self {
                root,
                paths: Arc::new(paths),
            }
        }
        fn storage(&self) -> ConfigSettingsStorage {
            ConfigSettingsStorage::new(Arc::clone(&self.paths))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn native_settings_create_private_files_and_reject_symlink_successors() {
        let fixture = Fixture::new();
        let storage = fixture.storage();
        assert!(storage.read().unwrap().is_none());
        assert!(!fixture.paths.config().exists());
        let first = storage.write(b"first", None).unwrap();
        let path = fixture.paths.config().join("settings.json");
        assert_eq!(
            fs::metadata(fixture.paths.config())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let outside = fixture.root.join("outside");
        fs::write(&outside, b"preserve").unwrap();
        fs::remove_file(&path).unwrap();
        symlink(&outside, &path).unwrap();
        assert!(
            storage
                .write(b"replacement", first.identity.as_ref())
                .is_err()
        );
        assert!(storage.read().is_err());
        assert_eq!(fs::read(outside).unwrap(), b"preserve");
    }

    #[test]
    fn native_settings_independent_owners_cannot_overwrite_a_competing_commit() {
        let fixture = Fixture::new();
        let first = fixture.storage();
        let second = fixture.storage();
        let identity = first.write(b"initial", None).unwrap().identity;
        second.write(b"external", identity.as_ref()).unwrap();
        assert!(matches!(
            first.write(b"stale", identity.as_ref()),
            Err(StorageError::Conflict)
        ));
        assert_eq!(first.read().unwrap().unwrap().bytes, b"external");
    }
}
