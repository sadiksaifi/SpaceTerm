use super::storage::*;
use crate::platform::app_directories::AppDirectoryEnvironment;
use crate::platform::app_paths::{AppPathHostFacts, AppPaths};
use crate::platform::secure_filesystem::{SecureCommitOutcome, SecureFilesystemError};
use crate::platform::testing::RecordingFilesystem;
use std::sync::Arc;

fn storage() -> (ConfigSettingsStorage, Arc<RecordingFilesystem>) {
    let filesystem = Arc::new(RecordingFilesystem::default());
    let paths = AppPaths::resolve(
        &AppDirectoryEnvironment {
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

    assert!(matches!(storage.read(), Err(StorageError::TooLarge)));
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
                &AppDirectoryEnvironment {
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
    fn native_oversized_settings_recovery_preserves_the_original_file() {
        let fixture = Fixture::new();
        let storage = fixture.storage();
        storage.write(b"initial", None).unwrap();
        let bytes = vec![0xff; MAXIMUM_DOCUMENT_BYTES + 100];
        let path = fixture.paths.directories().settings_file();
        fs::write(&path, &bytes).unwrap();
        let settings = crate::settings::UserSettings::load(Arc::new(storage));
        assert_eq!(
            settings.snapshot().status,
            Some(crate::settings::SettingsError::Storage(
                StorageError::TooLarge
            ))
        );
        settings.recover_by_reset().unwrap();
        assert_eq!(
            fs::read(fixture.paths.directories().settings_backup_file()).unwrap(),
            bytes
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            crate::appearance::export_settings(&crate::appearance::SettingsDocument::default())
                .unwrap()
                .as_bytes()
        );
        assert_eq!(settings.snapshot().status, None);
    }

    #[test]
    fn native_recovery_refuses_in_place_and_replacement_repairs_including_oversized_files() {
        for oversized in [false, true] {
            for replace in [false, true] {
                let fixture = Fixture::new();
                let storage = fixture.storage();
                storage.write(b"malformed", None).unwrap();
                let path = fixture.paths.directories().settings_file();
                if oversized {
                    fs::write(&path, vec![0xff; MAXIMUM_DOCUMENT_BYTES + 100]).unwrap();
                }
                let settings = crate::settings::UserSettings::load(Arc::new(storage));
                let repaired = crate::appearance::export_settings(
                    &crate::appearance::SettingsDocument::default(),
                )
                .unwrap();
                if replace {
                    let successor = fixture.root.join("successor");
                    fs::write(&successor, &repaired).unwrap();
                    fs::set_permissions(&successor, fs::Permissions::from_mode(0o600)).unwrap();
                    fs::rename(successor, &path).unwrap();
                } else {
                    fs::write(&path, &repaired).unwrap();
                }
                assert_eq!(
                    settings.recover_by_reset(),
                    Err(crate::settings::recovery::RecoveryError::Storage(
                        StorageError::Conflict
                    ))
                );
                assert_eq!(fs::read(&path).unwrap(), repaired.as_bytes());
                assert!(!fixture.paths.directories().settings_backup_file().exists());
            }
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

#[test]
fn oversized_settings_are_recoverable_and_quarantine_preserves_all_bytes() {
    use crate::settings::UserSettings;
    let (storage, filesystem) = storage();
    storage.write(b"initial", None).unwrap();
    let bytes = vec![0xff; MAXIMUM_DOCUMENT_BYTES + 100];
    {
        let mut files = filesystem.files.lock().unwrap();
        files.values.first_entry().unwrap().get_mut().0 = bytes.clone();
    }
    let settings = UserSettings::load(Arc::new(storage));
    assert!(settings.snapshot().status.unwrap().is_malformed());
    settings.recover_by_reset().unwrap();
    let files = filesystem.files.lock().unwrap();
    let backup = files
        .values
        .iter()
        .find(|(path, _)| path.ends_with("settings.json.bak"))
        .unwrap();
    assert_eq!(backup.1.0, bytes);
    assert_eq!(settings.snapshot().status, None);
}

#[test]
fn quarantine_replaces_backup_without_creating_a_missing_config_directory() {
    let (storage, filesystem) = storage();
    assert_eq!(storage.quarantine(), Err(StorageError::Conflict));
    assert!(filesystem.events.lock().unwrap().is_empty());
    storage.write(b"old", None).unwrap();
    storage.read().unwrap();
    storage.quarantine().unwrap();
    storage.write(b"new\0\xff", None).unwrap();
    storage.read().unwrap();
    storage.quarantine().unwrap();
    assert!(storage.read().unwrap().is_none());
    let files = filesystem.files.lock().unwrap();
    assert_eq!(files.values.len(), 1);
    let (path, (bytes, _)) = files.values.first_key_value().unwrap();
    assert!(path.ends_with("settings.json.bak"));
    assert_eq!(bytes, b"new\0\xff");
}

#[test]
fn recovery_refuses_settings_repaired_after_the_malformed_read() {
    for oversized in [false, true] {
        let (storage, filesystem) = storage();
        storage.write(b"malformed", None).unwrap();
        if oversized {
            filesystem
                .files
                .lock()
                .unwrap()
                .values
                .first_entry()
                .unwrap()
                .get_mut()
                .0 = vec![0xff; MAXIMUM_DOCUMENT_BYTES + 1];
        }
        let settings = crate::settings::UserSettings::load(Arc::new(storage));
        let repaired =
            crate::appearance::export_settings(&crate::appearance::SettingsDocument::default())
                .unwrap()
                .into_bytes();
        {
            let mut files = filesystem.files.lock().unwrap();
            let entry = files.values.first_entry().unwrap().into_mut();
            entry.0 = repaired.clone();
            entry.1 += 1;
        }
        assert_eq!(
            settings.recover_by_reset(),
            Err(crate::settings::recovery::RecoveryError::Storage(
                StorageError::Conflict
            ))
        );
        let files = filesystem.files.lock().unwrap();
        assert_eq!(files.values.len(), 1);
        assert_eq!(files.values.first_key_value().unwrap().1.0, repaired);
    }
}

#[test]
fn recovery_requires_reload_after_a_successor_replaces_its_publication() {
    let (storage, filesystem) = storage();
    storage.write(b"malformed", None).unwrap();
    let settings = crate::settings::UserSettings::load(Arc::new(storage));
    let successor =
        crate::appearance::export_settings(&crate::appearance::SettingsDocument::default())
            .unwrap()
            .into_bytes();
    filesystem.files.lock().unwrap().successor_after_commit = Some(successor.clone());
    settings.recover_by_reset().unwrap();
    assert_eq!(
        settings.snapshot().status,
        Some(crate::settings::SettingsError::Storage(
            StorageError::Conflict
        ))
    );
    assert!(
        settings
            .update_committed(
                settings.snapshot().committed.revision,
                crate::appearance::SettingsDocument::default()
            )
            .is_err()
    );
    settings.reload().unwrap();
    assert!(
        settings
            .begin_preview(settings.snapshot().committed.revision)
            .is_ok()
    );
    let files = filesystem.files.lock().unwrap();
    assert_eq!(
        files
            .values
            .iter()
            .find(|(path, _)| path.ends_with("settings.json"))
            .unwrap()
            .1
            .0,
        successor
    );
    assert_eq!(
        files
            .values
            .iter()
            .find(|(path, _)| path.ends_with("settings.json.bak"))
            .unwrap()
            .1
            .0,
        b"malformed"
    );
}
