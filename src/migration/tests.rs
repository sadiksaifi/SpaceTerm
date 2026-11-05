use super::*;
use spaceterm_core::migration::fixtures;

fn native() -> Arc<dyn crate::platform::secure_filesystem::SecureFilesystem> {
    Arc::new(crate::platform::unix_secure_filesystem::UnixSecureFilesystem)
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
    write(
        "state/update-history.json",
        serde_json::to_value(crate::updates::policy::UpdateHistory::default()).unwrap(),
    );
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
    assert_eq!(
        check_state(&store, identity),
        StateCheck::ResetRequired {
            reason: migration::ResetReason::TooOld,
            kinds: vec![DataKind::ClientRecords],
        }
    );
    assert_eq!(
        store.read(Area::Root, "core/desktop/view.json").unwrap(),
        None
    );
    assert!(
        store
            .read(Area::Root, "core/desktop/client.json")
            .unwrap()
            .is_some()
    );
    assert!(reset_state(&store, identity));
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
    assert_eq!(
        check_state(&store, identity),
        StateCheck::Ready { reset: Vec::new() }
    );
    std::fs::remove_dir_all(&root).unwrap();
}
