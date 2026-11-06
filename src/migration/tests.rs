use super::*;
use spaceterm_core::migration::fixtures;

/// Every file under `root` with its bytes.
fn files(root: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    let mut found = std::collections::BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
    }
    found
}

fn native() -> Arc<dyn crate::platform::secure_filesystem::SecureFilesystem> {
    Arc::new(crate::platform::unix_secure_filesystem::UnixSecureFilesystem)
}

// Shapes from v0.6.8: src/settings/document.rs, src/appearance/preferences.rs,
// src/updates/policy.rs, src/ssh/managed_hosts.rs and src/background_image.rs.
// Values are fabricated; the image name is its bytes' SHA-256 digest.
const LEGACY_SETTINGS: &[u8] =
    include_bytes!("../../fixtures/pre-greenfield/v0.6.8/config/settings.json");
const LEGACY_SSH: &[u8] = include_bytes!("../../fixtures/pre-greenfield/v0.6.8/config/ssh_config");

#[test]
fn reset_archive_publication_preserves_another_resets_snapshot() {
    use migration::{Area, Store, StoreError};
    let root = fixtures::scratch("reset-archive-publication");
    let stores: [Box<dyn Store>; 2] = [
        Box::new(config_store(native(), root.join("config"))),
        Box::new(desktop_state(&root)),
    ];
    for store in stores {
        let path = "gen-0/settings.json";
        store
            .write(Area::Reset, path, b"other reset's snapshot")
            .unwrap();
        assert_eq!(
            store.write(Area::Reset, path, b"different snapshot"),
            Err(StoreError::Conflict)
        );
        assert_eq!(
            store.read(Area::Reset, path).unwrap(),
            Some(b"other reset's snapshot".to_vec())
        );
        store
            .write(Area::Reset, path, b"other reset's snapshot")
            .unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn reset_preserves_a_save_published_during_archival() {
    use migration::{Area, Store, StoreError};
    use std::cell::Cell;

    // A second writer publishes through another real private-filesystem handle as soon as
    // the archive commits, before the runner removes the source.
    struct SavingWriter<'a> {
        store: &'a dyn Store,
        name: &'a str,
        successor: &'a [u8],
        saved: Cell<bool>,
    }
    impl Store for SavingWriter<'_> {
        fn list(&self, area: Area) -> Result<Vec<String>, StoreError> {
            self.store.list(area)
        }
        fn read(&self, area: Area, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
            self.store.read(area, path)
        }
        fn write(&self, area: Area, path: &str, bytes: &[u8]) -> Result<(), StoreError> {
            self.store.write(area, path, bytes)?;
            if area == Area::Reset
                && path == format!("gen-0/{}", self.name)
                && !self.saved.replace(true)
            {
                self.store.write(Area::Root, self.name, self.successor)?;
            }
            Ok(())
        }
        fn archive_and_remove(
            &self,
            path: &str,
            save: &mut dyn FnMut(&[u8]) -> Result<(), StoreError>,
        ) -> Result<(), StoreError> {
            self.store.archive_and_remove(path, save)
        }
        fn remove(&self, area: Area, path: &str) -> Result<(), StoreError> {
            self.store.remove(area, path)
        }
    }

    use crate::platform::secure_filesystem::{PrivateFileSnapshot, SecureEntryIdentity};
    use crate::settings::storage::{
        ConfigSettingsStorage, SettingsStorage, StorageCommit, StorageError,
    };
    use crate::settings::{Settings, SettingsError, recovery::RecoveryError};
    use std::sync::atomic::{AtomicBool, Ordering};

    // Interleave the second native writer inside the first reset's archival operation.
    // Every ordinary read, write and admission uses the real Settings storage adapter.
    struct RacedStorage {
        settings: ConfigSettingsStorage,
        store: secure_store::SecureStore,
        name: &'static str,
        successor: Vec<u8>,
        injected: AtomicBool,
    }
    impl SettingsStorage for RacedStorage {
        fn prepare(&self) -> Result<(), SettingsError> {
            self.settings.prepare()
        }
        fn read(&self) -> Result<Option<PrivateFileSnapshot>, StorageError> {
            self.settings.read()
        }
        fn write(
            &self,
            bytes: &[u8],
            expected: Option<&SecureEntryIdentity>,
        ) -> Result<StorageCommit, StorageError> {
            self.settings.write(bytes, expected)
        }
        fn quarantine(&self) -> Result<(), StorageError> {
            self.settings.quarantine()
        }
        fn reset_generation(&self) -> Result<bool, StorageError> {
            if self.injected.swap(true, Ordering::Relaxed) {
                return self.settings.reset_generation();
            }
            let competing = SavingWriter {
                store: &self.store,
                name: self.name,
                successor: &self.successor,
                saved: Cell::new(false),
            };
            let result =
                migration::reset_confirmed(&CONFIG, &competing, &mut migration::NoCredentials);
            assert!(competing.saved.get());
            result
                .map(|kinds| !kinds.is_empty())
                .map_err(|error| match error {
                    Refusal::Storage(StoreError::Conflict) => StorageError::Conflict,
                    Refusal::Storage(StoreError::Unsafe) => StorageError::Unsafe,
                    _ => StorageError::Unavailable,
                })
        }
    }

    for (name, original) in [(SETTINGS, LEGACY_SETTINGS), ("ssh_config", LEGACY_SSH)] {
        for follow in [false, true] {
            let root = fixtures::scratch("reset-concurrent-save");
            let directories = AppDirectories::resolve_xdg(
                "spaceterm",
                &spaceterm_directories::AppDirectoryEnvironment {
                    home: Some(root.clone().into()),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
            let store = config_store(native(), directories.config.clone());
            let successor = if name == SETTINGS {
                let mut value: serde_json::Value = serde_json::from_slice(original).unwrap();
                value["revision"] = 12345.into();
                serde_json::to_vec(&value).unwrap()
            } else {
                String::from_utf8(original.to_vec())
                    .unwrap()
                    .replace("tester", "successor")
                    .into_bytes()
            };
            store.write(Area::Root, SETTINGS, LEGACY_SETTINGS).unwrap();
            store.write(Area::Root, "ssh_config", LEGACY_SSH).unwrap();
            let paths = Arc::new(
                crate::platform::app_paths::AppPaths::from_directories(directories, 103, native())
                    .unwrap(),
            );
            let storage = Arc::new(RacedStorage {
                settings: ConfigSettingsStorage::new(paths.clone()),
                store,
                name,
                successor: successor.clone(),
                injected: AtomicBool::new(false),
            });
            let settings = Settings::load(storage.clone());
            assert_eq!(settings.snapshot().status, Some(SettingsError::Invalid));
            assert_eq!(
                settings.recover_by_reset(),
                Err(RecoveryError::Storage(StorageError::Conflict))
            );
            assert_eq!(
                settings.snapshot().status,
                Some(SettingsError::Storage(StorageError::Conflict))
            );
            assert_eq!(
                storage.store.read(Area::Root, name).unwrap(),
                Some(successor.clone())
            );
            assert_eq!(
                storage
                    .store
                    .read(Area::Reset, &format!("gen-0/{name}"))
                    .unwrap(),
                Some(original.to_vec())
            );
            assert_eq!(
                storage.store.read(Area::Root, "generation.json").unwrap(),
                None
            );
            if name == "ssh_config" {
                // The host conflict happened after Settings had already been removed.
                assert_eq!(storage.store.read(Area::Root, SETTINGS).unwrap(), None);
            }
            let before = files(&root);
            let refreshed = if follow {
                settings.follow_file().map(|_| ())
            } else {
                settings.reload()
            };
            assert_eq!(refreshed, Err(SettingsError::Invalid));
            assert_eq!(settings.snapshot().status, Some(SettingsError::Invalid));
            assert_eq!(files(&root), before);
            // The same Settings owner can confirm the remaining reset and resume editing.
            settings.recover_by_reset().unwrap();
            assert_eq!(settings.snapshot().status, None);
            assert_eq!(storage.store.read(Area::Root, "ssh_config").unwrap(), None);
            assert_eq!(
                storage
                    .store
                    .read(Area::Reset, &format!("gen-0-1/{name}"))
                    .unwrap(),
                Some(successor)
            );
            assert_eq!(
                storage
                    .store
                    .read(Area::Reset, &format!("gen-0/{name}"))
                    .unwrap(),
                Some(original.to_vec())
            );
            let snapshot = settings.snapshot();
            let mut candidate = (*snapshot.committed).clone();
            candidate.appearance.window.opacity = 0.8;
            settings
                .update_committed(snapshot.committed.revision, candidate)
                .unwrap()
                .run()
                .unwrap();
            let restarted = Settings::load(Arc::new(ConfigSettingsStorage::new(paths)));
            assert_eq!(restarted.snapshot().status, None);
            assert_eq!(
                restarted.snapshot().committed.appearance.window.opacity,
                0.8
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn pre_greenfield_managed_hosts_alone_offer_settings_recovery() {
    use migration::{Area, Store};
    let root = fixtures::scratch("pre-greenfield-hosts");
    let store = config_store(native(), root.clone());
    store.write(Area::Root, "ssh_config", LEGACY_SSH).unwrap();
    let before = files(&root);
    assert!(matches!(
        migration::run(&CONFIG, &store, &mut migration::NoCredentials, false),
        Err(Refusal::Reset {
            reason: migration::ResetReason::Invalid,
            ..
        })
    ));
    assert_eq!(files(&root), before);
    migration::reset_confirmed(&CONFIG, &store, &mut migration::NoCredentials).unwrap();
    assert_eq!(store.read(Area::Root, "ssh_config").unwrap(), None);
    assert_eq!(
        store.read(Area::Reset, "gen-0/ssh_config").unwrap(),
        Some(LEGACY_SSH.to_vec())
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn future_marked_config_is_never_offered_for_reset() {
    use migration::{Area, Store};
    let root = fixtures::scratch("future-config");
    let store = config_store(native(), root.clone());
    store.write(Area::Root, SETTINGS, LEGACY_SETTINGS).unwrap();
    store
        .write(Area::Root, "generation.json", br#"{"generation":7}"#)
        .unwrap();
    let before = files(&root);
    assert_eq!(
        migration::run(&CONFIG, &store, &mut migration::NoCredentials, false),
        Err(Refusal::Newer)
    );
    assert_eq!(
        migration::reset_confirmed(&CONFIG, &store, &mut migration::NoCredentials),
        Err(Refusal::Newer)
    );
    assert_eq!(files(&root), before);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn unmarked_current_shaped_settings_still_require_reset() {
    use migration::{Area, Store};
    let root = fixtures::scratch("unmarked-config");
    let store = config_store(native(), root.clone());
    let bytes = crate::settings::export_settings(&Default::default()).unwrap();
    store.write(Area::Root, SETTINGS, bytes.as_bytes()).unwrap();
    let before = files(&root);
    assert!(matches!(
        migration::run(&CONFIG, &store, &mut migration::NoCredentials, false),
        Err(Refusal::Reset {
            reason: migration::ResetReason::Invalid,
            ..
        })
    ));
    assert_eq!(files(&root), before);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn pre_greenfield_images_wait_for_reset_and_history_resets_with_them() {
    use migration::{Area, Store};
    let root = fixtures::scratch("pre-greenfield-state");
    fixtures::copy_private(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/pre-greenfield/v0.6.8"),
        &root,
    );
    let store = desktop_state(&root);
    let before = files(&root);
    let check = check_state(&store, "spaceterm-development");
    let StateCheck::ResetRequired { reason, kinds } = &check else {
        panic!("expected reset offer, got {check:?}")
    };
    assert_eq!(*reason, migration::ResetReason::Invalid);
    assert!(migration::nouns(kinds).contains("background images"));
    assert!(crate::ui::data_reset::launch_prompt(&check, None).is_some());
    assert_eq!(files(&root), before);
    assert_eq!(check_state(&store, "spaceterm-development"), check);
    assert!(reset_state(&store, "spaceterm-development"));
    for (path, bytes) in &before {
        if path.starts_with(root.join("data")) || path.starts_with(root.join("state")) {
            assert!(!path.exists());
            assert_eq!(
                store
                    .read(
                        Area::Reset,
                        &format!(
                            "gen-0/{}",
                            path.strip_prefix(&root).unwrap().to_str().unwrap()
                        )
                    )
                    .unwrap(),
                Some(bytes.clone())
            );
        }
    }
    assert_eq!(
        check_state(&store, "spaceterm-development"),
        StateCheck::Ready {
            reset: vec![DataKind::UpdateHistory]
        }
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn pre_greenfield_history_alone_resets_automatically_with_a_notice() {
    use migration::{Area, Store};
    let root = fixtures::scratch("pre-greenfield-history");
    let store = desktop_state(&root);
    let bytes = include_bytes!("../../fixtures/pre-greenfield/v0.6.8/state/update-history.json");
    store
        .write(Area::Root, "state/update-history.json", bytes)
        .unwrap();
    assert_eq!(
        check_state(&store, "spaceterm-development"),
        StateCheck::Ready {
            reset: vec![DataKind::UpdateHistory]
        }
    );
    assert_eq!(
        store.read(Area::Root, "state/update-history.json").unwrap(),
        None
    );
    assert_eq!(
        store
            .read(Area::Reset, "gen-0/state/update-history.json")
            .unwrap(),
        Some(bytes.to_vec())
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn image_reset_preserves_large_copies_and_ignores_unowned_files() {
    use migration::{Area, Store};
    let root = fixtures::scratch("pre-greenfield-large-image");
    let store = desktop_state(&root);
    let name = format!("data/background-image-{}", "ab".repeat(32));
    let bytes = vec![0xa5; 13 * 1024 * 1024];
    store.write(Area::Root, &name, &bytes).unwrap();
    for path in [
        "data/background-image-other",
        "data/settings.json.bak",
        "data/unowned",
        "core/other-client/client.json",
    ] {
        store.write(Area::Root, path, b"preserve").unwrap();
    }
    assert!(matches!(
        check_state(&store, "spaceterm-development"),
        StateCheck::ResetRequired { .. }
    ));
    assert!(reset_state(&store, "spaceterm-development"));
    assert_eq!(
        store.read(Area::Reset, &format!("gen-0/{name}")).unwrap(),
        Some(bytes)
    );
    for path in [
        "data/background-image-other",
        "data/settings.json.bak",
        "data/unowned",
        "core/other-client/client.json",
    ] {
        assert_eq!(
            store.read(Area::Root, path).unwrap(),
            Some(b"preserve".to_vec())
        );
    }
    // New copies in a marked greenfield root remain available on subsequent launches.
    store.write(Area::Root, &name, b"current copy").unwrap();
    assert_eq!(
        check_state(&store, "spaceterm-development"),
        StateCheck::Ready { reset: vec![] }
    );
    assert_eq!(
        store.read(Area::Root, &name).unwrap(),
        Some(b"current copy".to_vec())
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn pre_greenfield_settings_offer_reset_without_changing_files_and_become_writable() {
    use crate::settings::{Settings, SettingsError, storage::ConfigSettingsStorage};
    use migration::{Area, Store};
    for marked in [false, true] {
        let root = fixtures::scratch("pre-greenfield-settings");
        let directories = AppDirectories::resolve_xdg(
            "spaceterm",
            &spaceterm_directories::AppDirectoryEnvironment {
                home: Some(root.clone().into()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let store = config_store(native(), directories.config.clone());
        store.write(Area::Root, SETTINGS, LEGACY_SETTINGS).unwrap();
        store.write(Area::Root, "ssh_config", LEGACY_SSH).unwrap();
        store
            .write(Area::Root, "settings.json.bak", b"earlier backup")
            .unwrap();
        if marked {
            store
                .write(Area::Root, "generation.json", br#"{"generation":1}"#)
                .unwrap();
        }
        let paths = Arc::new(
            crate::platform::app_paths::AppPaths::from_directories(directories, 103, native())
                .unwrap(),
        );
        let load = || Settings::load(Arc::new(ConfigSettingsStorage::new(paths.clone())));
        let before = files(&root);
        let settings = load();
        assert_eq!(settings.snapshot().status, Some(SettingsError::Invalid));
        assert!(settings.snapshot().status.unwrap().is_malformed());
        // Declining and launching again repeats the recovery offer without effects.
        assert_eq!(load().snapshot().status, Some(SettingsError::Invalid));
        assert_eq!(files(&root), before);
        settings.recover_by_reset().unwrap();
        assert_eq!(settings.snapshot().status, None);
        let generation = if marked { 1 } else { 0 };
        assert_eq!(
            store
                .read(Area::Reset, &format!("gen-{generation}/settings.json"))
                .unwrap(),
            Some(LEGACY_SETTINGS.to_vec())
        );
        assert_eq!(
            store
                .read(Area::Reset, &format!("gen-{generation}/ssh_config"))
                .unwrap(),
            Some(LEGACY_SSH.to_vec())
        );
        assert_eq!(
            store.read(Area::Root, "settings.json.bak").unwrap(),
            Some(b"earlier backup".to_vec())
        );
        let snapshot = settings.snapshot();
        let mut candidate = (*snapshot.committed).clone();
        candidate.appearance.window.opacity = 0.8;
        settings
            .update_committed(snapshot.committed.revision, candidate)
            .unwrap()
            .run()
            .unwrap();
        let restarted = load();
        assert_eq!(restarted.snapshot().status, None);
        assert_eq!(
            restarted.snapshot().committed.appearance.window.opacity,
            0.8
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}

fn desktop_state(root: &std::path::Path) -> DirectoryStore {
    state_store(root.join("data"), root.join("state"), root.join("core"))
}

#[test]
fn every_desktop_config_migration_fixture_migrates_and_reads_strictly() {
    fixtures::check(
        &CONFIG,
        &fixtures::directory("desktop-config"),
        &[],
        |root| Box::new(config_store(native(), root.to_path_buf())),
    );
}

#[test]
fn every_desktop_state_migration_fixture_migrates_and_reads_strictly() {
    fixtures::check(&STATE, &fixtures::directory("desktop-state"), &[], |root| {
        Box::new(desktop_state(root))
    });
}

/// `mise run migration:fixture desktop-config` writes this build's generation's fixture.
#[test]
#[ignore = "writes the fixture of the current generation"]
fn capture_desktop_config_migration_fixture() {
    let root = fixtures::scratch("desktop-config");
    let store = config_store(native(), root.clone());
    migration::run(&CONFIG, &store, &mut migration::NoCredentials, false).unwrap();
    let settings = crate::settings::export_settings(&Default::default()).unwrap();
    migration::Store::write(&store, migration::Area::Root, SETTINGS, settings.as_bytes()).unwrap();
    fixtures::capture(
        &CONFIG,
        &root,
        &store,
        &fixtures::directory("desktop-config"),
    );
    std::fs::remove_dir_all(&root).unwrap();
}

/// `mise run migration:fixture desktop-state` writes this build's generation's fixture.
#[test]
#[ignore = "writes the fixture of the current generation"]
fn capture_desktop_state_migration_fixture() {
    let root = fixtures::scratch("desktop-state");
    let store = desktop_state(&root);
    migration::run(&STATE, &store, &mut migration::NoCredentials, false).unwrap();
    let write = |path: &str, value: serde_json::Value| {
        use migration::{Area, Store};
        store
            .write(Area::Root, path, &serde_json::to_vec(&value).unwrap())
            .unwrap();
    };
    write(
        "data/direct-device-connections.json",
        serde_json::json!({ "schema": 1, "routes": [], "pending": [] }),
    );
    write(
        "data/core-context.json",
        serde_json::json!({ "schema": 1, "selected": "Local", "facts": [] }),
    );
    // The update history goes through the updaters' own writer.
    let directories = spaceterm_directories::AppDirectories::resolve_xdg(
        "state",
        &spaceterm_directories::AppDirectoryEnvironment {
            home: Some(root.clone().into()),
            xdg_state_home: Some(root.clone().into()),
            xdg_data_home: Some(root.join("unused").into()),
            xdg_config_home: Some(root.join("unused").into()),
            xdg_cache_home: Some(root.join("unused").into()),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let paths =
        crate::platform::app_paths::AppPaths::from_directories(directories, 103, native()).unwrap();
    crate::platform::update_history_file::UpdateHistoryFile::new(Some(Arc::new(paths)))
        .save(&crate::updates::policy::UpdateHistory::default());
    assert!(root.join("state/update-history.json").is_file());
    let _ = std::fs::remove_dir_all(root.join("unused"));
    fixtures::write_client_records(&root.join("core/desktop"));
    write(
        &format!("core/desktop-direct-{}/route.json", "09".repeat(32)),
        serde_json::json!({
            "version": 1,
            "paired": spaceterm_core::direct::PairedDevice {
                pin: spaceterm_core::direct::MachinePin {
                    build: "spaceterm-development".into(),
                    core: [1; 32],
                    public_key: [2; 32],
                    certificate: [3; 32],
                },
                pairing: [4; 32],
                client: [5; 32],
            },
        }),
    );
    fixtures::capture(&STATE, &root, &store, &fixtures::directory("desktop-state"));
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn desktop_state_migration_refuses_newer_data_and_asks_before_resetting_client_records() {
    use migration::{Area, Store};
    let identity = "spaceterm-development";
    let root = fixtures::scratch("desktop-state-check");
    let store = desktop_state(&root);
    store
        .write(Area::Root, "generation.json", br#"{"generation":99}"#)
        .unwrap();
    store
        .write(Area::Root, "core/desktop/client.json", br#"{"newer":true}"#)
        .unwrap();
    assert_eq!(check_state(&store, identity), StateCheck::Newer);
    assert_eq!(
        store.read(Area::Root, "core/desktop/client.json").unwrap(),
        Some(br#"{"newer":true}"#.to_vec())
    );

    store
        .write(Area::Root, "generation.json", br#"{"generation":0}"#)
        .unwrap();
    store
        .write(Area::Root, "core/desktop/view.json", br#"{"old":true}"#)
        .unwrap();
    let before = files(&root);
    assert_eq!(
        check_state(&store, identity),
        StateCheck::ResetRequired {
            reason: migration::ResetReason::TooOld,
            kinds: vec![DataKind::ClientRecords],
        }
    );
    // Quit leaves the root as it was: no file moves, changes or appears.
    assert_eq!(files(&root), before);
    // Nothing moves before the person decides, the rebuildable view included.
    assert_eq!(
        store.read(Area::Root, "core/desktop/view.json").unwrap(),
        Some(br#"{"old":true}"#.to_vec())
    );
    assert_eq!(migration::pending_notice(&store), Ok(Vec::new()));
    assert!(
        store
            .read(Area::Root, "core/desktop/client.json")
            .unwrap()
            .is_some()
    );
    assert_eq!(
        migration::reset_confirmed(&STATE, &store, &mut migration::NoCredentials),
        Ok(vec![DataKind::View, DataKind::ClientRecords])
    );
    assert_eq!(
        store.read(Area::Root, "core/desktop/client.json").unwrap(),
        None
    );
    assert_eq!(
        check_state(&store, identity),
        StateCheck::Ready {
            reset: vec![DataKind::View]
        }
    );
    // The notice stays until the person acknowledges it, across launches that never showed it.
    assert_eq!(
        check_state(&store, identity),
        StateCheck::Ready {
            reset: vec![DataKind::View]
        }
    );
    crate::ui::data_reset::acknowledge(&store);
    assert_eq!(
        check_state(&store, identity),
        StateCheck::Ready { reset: Vec::new() }
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn desktop_state_migration_opens_nothing_on_unavailable_storage() {
    use std::os::unix::fs::PermissionsExt;
    let root = fixtures::scratch("desktop-state-unsafe");
    let store = desktop_state(&root);
    assert!(matches!(
        check_state(&store, "spaceterm-development"),
        StateCheck::Ready { .. }
    ));
    std::fs::set_permissions(root.join("data"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        check_state(&store, "spaceterm-development"),
        StateCheck::Unavailable
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn the_update_history_the_updaters_write_is_a_desktop_state_migration_file() {
    use migration::{Area, Store};
    let root = fixtures::scratch("desktop-state-history");
    let directories = spaceterm_directories::AppDirectories::resolve_xdg(
        "state",
        &spaceterm_directories::AppDirectoryEnvironment {
            home: Some(root.clone().into()),
            xdg_state_home: Some(root.clone().into()),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let paths =
        crate::platform::app_paths::AppPaths::from_directories(directories, 103, native()).unwrap();
    crate::platform::update_history_file::UpdateHistoryFile::new(Some(Arc::new(paths)))
        .save(&crate::updates::policy::UpdateHistory::default());
    let store = desktop_state(&root);
    let path = "state/update-history.json";
    assert!(
        store
            .list(Area::Root)
            .unwrap()
            .iter()
            .any(|listed| listed == path)
    );
    assert!(STATE.readable(path, &store.read(Area::Root, path).unwrap().unwrap()));
    std::fs::remove_dir_all(&root).unwrap();
}
