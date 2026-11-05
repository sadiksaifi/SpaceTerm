//! Fixtures of every data root at each generation it still migrates, and the checks that carry
//! each one to the current generation and read every file strictly. Each crate that owns a root
//! keeps its fixture tests under a `migration` module, so `cargo test migration` runs them all.
//!
//! A fixture lives at `fixtures/migrations/<root>/gen-<N>/`, laid out as the root's directories.
use super::{Area, NoCredentials, Outcome, Registry, Store};
use std::path::{Path, PathBuf};

/// `fixtures/migrations/<root>` in this repository.
pub fn directory(root: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/migrations")
        .join(root)
}

/// Copies `from` to `to` with the account-private modes the stores require.
pub fn copy_private(from: &Path, to: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(to).unwrap();
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o700)).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_private(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}

/// A fresh account-private directory for one test.
pub fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "st-fixture-{name}-{}-{:x}",
        std::process::id(),
        u64::from_be_bytes(crate::protocol::random().unwrap()[..8].try_into().unwrap())
    ));
    let _ = std::fs::remove_dir_all(&path);
    path
}

/// Carries the fixture of every generation from `oldest` to `current` in `fixtures` to the
/// current generation with `store`, which `open` builds over a private copy, and reads every
/// file of the result with its strict reader. Every kind of file is present except `omitted`.
/// Panics name the generation and file that failed.
pub fn check(
    registry: &Registry,
    fixtures: &Path,
    omitted: &[&str],
    open: impl Fn(&Path) -> Box<dyn Store>,
) {
    assert!(
        registry.consistent(),
        "the registry's steps span its generations"
    );
    for generation in registry.generations.oldest..=registry.generations.current {
        let fixture = fixtures.join(format!("gen-{generation}"));
        assert!(fixture.is_dir(), "no fixture for generation {generation}");
        let copy = scratch("check");
        copy_private(&fixture, &copy);
        let store = open(&copy);
        let outcome = super::run(registry, store.as_ref(), &mut NoCredentials, false);
        assert!(
            matches!(outcome, Ok(Outcome::Current | Outcome::Migrated { .. })),
            "generation {generation} migrates: {outcome:?}"
        );
        let mut read = 0;
        for path in store.list(Area::Root).unwrap() {
            if registry.rule(&path).is_none() {
                continue;
            }
            let bytes = store.read(Area::Root, &path).unwrap().unwrap();
            assert!(
                registry.readable(&path, &bytes),
                "generation {generation}: {path} reads strictly"
            );
            read += 1;
        }
        let kinds: std::collections::BTreeSet<_> = registry
            .rules()
            .map(|rule| rule.name)
            .filter(|name| !omitted.contains(name))
            .collect();
        let present: std::collections::BTreeSet<_> = store
            .list(Area::Root)
            .unwrap()
            .iter()
            .filter_map(|path| registry.rule(path).map(|rule| rule.name))
            .collect();
        assert_eq!(
            present, kinds,
            "generation {generation} holds every kind of file"
        );
        assert!(read > 0);
        std::fs::remove_dir_all(&copy).unwrap();
    }
}

/// Writes `captured`, a root that this build's own writers produced, as the fixture of the
/// current generation, after every file it holds reads strictly.
pub fn capture(registry: &Registry, captured: &Path, store: &dyn Store, fixtures: &Path) {
    let current = registry.generations.current;
    assert_eq!(
        super::run(registry, store, &mut NoCredentials, false),
        Ok(Outcome::Current)
    );
    for path in store.list(Area::Root).unwrap() {
        if registry.rule(&path).is_some() {
            let bytes = store.read(Area::Root, &path).unwrap().unwrap();
            assert!(registry.readable(&path, &bytes), "{path} reads strictly");
        }
    }
    let target = fixtures.join(format!("gen-{current}"));
    let _ = std::fs::remove_dir_all(&target);
    copy_members(registry, captured, &target);
}

/// Copies the registry's files and the marker, leaving locks and other files behind.
fn copy_members(registry: &Registry, from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            copy_members(registry, &path, &to.join(entry.file_name()));
            continue;
        }
        let name = entry.file_name().into_string().unwrap();
        if name == super::MARKER || registry.rule(&name).is_some() {
            std::fs::create_dir_all(to).unwrap();
            let value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let mut text = serde_json::to_string(&value).unwrap();
            text.push('\n');
            std::fs::write(to.join(&name), text).unwrap();
        }
    }
}

/// Writes every client record this library keeps into the client directory `directory`, with
/// the library's own writers. A paired route's `route.json` is left out.
#[cfg(feature = "client")]
pub fn write_client_records(directory: &Path) {
    use crate::storage::Directory;
    let (core, client) = ([7; 32], [8; 32]);
    let open = || Directory::open(directory, true).unwrap();
    let shared = crate::hierarchy::State::first("/".as_ref()).unwrap();
    let workspace = shared.inventory.workspaces[0].id;
    {
        let mut views = crate::client_view::ClientViewStore::open(open(), core, client).unwrap();
        views
            .save(crate::client_view::ClientView {
                workspaces: vec![crate::client_view::WorkspaceView {
                    workspace,
                    tabs: Vec::new(),
                }],
            })
            .unwrap();
        views
            .save_inventory(crate::client_view::RetainedInventory {
                revision: 1,
                workspaces: Vec::new(),
            })
            .unwrap();
    }
    crate::attachments::Attachments::open(open(), core, client)
        .unwrap()
        .initialize(&shared.inventory, Some(workspace))
        .unwrap();
    crate::hierarchy::ClientJournal::open(open(), core, client)
        .unwrap()
        .submit(
            1,
            crate::hierarchy::Mutation::RenameWorkspace {
                workspace,
                version: 0,
                name: "Fixture".into(),
            },
        )
        .unwrap();
    crate::service::lane::admin::PendingOperations::new(open())
        .add(&crate::service::operations::OperationId([1; 16]))
        .unwrap();
    let directory = open();
    directory
        .write_json(
            c"client.json",
            &serde_json::json!({ "format": "account-local-1", "client": client }),
        )
        .unwrap();
    directory
        .write_json(
            c"workspace-order.json",
            &serde_json::json!({
                "version": 1,
                "client": client,
                "order": crate::workspace_order::WorkspaceOrder::default(),
            }),
        )
        .unwrap();
}
