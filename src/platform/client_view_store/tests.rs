use super::*;
use crate::platform::{
    app_directories::{AppDirectories, AppDirectoryEnvironment},
    unix_secure_filesystem::UnixSecureFilesystem,
};
use spaceterm_client::view::{
    ClientViewState, MAX_VIEW_DOCUMENT_BYTES, PersistedView, PersistedViewParts, RestorationFilter,
    ViewError, ViewPersistence, ViewRevision,
};
use std::{
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture {
    root: PathBuf,
    paths: Arc<AppPaths>,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "native-view-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(root.parent().unwrap()).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let paths = Self::paths(&root);
        Self { root, paths }
    }

    fn paths(root: &std::path::Path) -> Arc<AppPaths> {
        let environment = AppDirectoryEnvironment {
            home: Some(root.as_os_str().to_owned()),
            xdg_state_home: Some(root.join("state").into_os_string()),
            ..Default::default()
        };
        let directories =
            AppDirectories::resolve_xdg("view-test-development", &environment, None).unwrap();
        Arc::new(
            AppPaths::from_directories(directories, 104, Arc::new(UnixSecureFilesystem)).unwrap(),
        )
    }

    fn store(&self) -> NativeViewStore {
        NativeViewStore::new(Arc::clone(&self.paths))
    }
    fn document_path(&self) -> PathBuf {
        self.paths
            .directories()
            .root(AppDirectoryRoot::State)
            .join(DOCUMENT_NAME)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn view(revision: u64) -> PersistedView {
    PersistedView::new(PersistedViewParts {
        revision: ViewRevision::new(revision),
        incarnation_high_water: 0,
        workspaces: Vec::new(),
        active: None,
    })
    .unwrap()
}

#[test]
fn desktop_server_native_view_store_round_trips_and_does_not_create_on_load() {
    let fixture = Fixture::new();
    let mut persistence = ViewPersistence::new(fixture.store());
    assert_eq!(persistence.load(), Ok(None));
    assert!(
        !fixture
            .paths
            .directories()
            .root(AppDirectoryRoot::State)
            .exists()
    );
    persistence.save(&view(1)).unwrap();
    persistence.save(&view(2)).unwrap();
    let mut reopened = ViewPersistence::new(fixture.store());
    assert_eq!(reopened.load(), Ok(Some(view(2))));
    assert_eq!(
        std::fs::metadata(fixture.document_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(fixture.document_path().parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn desktop_server_native_view_store_create_if_absent_and_stale_replacement_preserve_successor() {
    let fixture = Fixture::new();
    let mut first = ViewPersistence::new(fixture.store());
    let mut second = ViewPersistence::new(fixture.store());
    first.load().unwrap();
    second.load().unwrap();
    first.save(&view(1)).unwrap();
    assert_eq!(second.save(&view(2)), Err(ViewError::PublicationConflict));
    assert_eq!(second.blocked(), Some(ViewError::PublicationConflict));
    second.recover().unwrap();
    second.save(&view(3)).unwrap();
    assert_eq!(first.save(&view(4)), Err(ViewError::PublicationConflict));
    assert_eq!(first.recover(), Ok(Some(view(3))));
}

#[test]
fn desktop_server_native_view_store_reports_unsafe_and_oversized_loads_as_failures() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let encoded = view(1).encode().unwrap();
    assert!(matches!(
        store.replace(None, &encoded),
        ViewCommit::Durable(_)
    ));
    assert_eq!(
        store.load(encoded.as_bytes().len() - 1).unwrap_err().class,
        ViewStoreFailureClass::TooLarge
    );
    std::fs::set_permissions(
        fixture.document_path(),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert_eq!(
        store.load(MAX_VIEW_DOCUMENT_BYTES).unwrap_err().class,
        ViewStoreFailureClass::Unsafe
    );
}

#[test]
fn desktop_server_native_view_store_preserves_malformed_and_newer_documents() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    assert!(matches!(
        store.replace(None, &view(1).encode().unwrap()),
        ViewCommit::Durable(_)
    ));
    let mut bytes = view(1).encode().unwrap().into_bytes();
    bytes[8..12].copy_from_slice(&2_u32.to_be_bytes());
    std::fs::write(fixture.document_path(), &bytes).unwrap();
    let mut persistence = ViewPersistence::new(fixture.store());
    assert_eq!(
        persistence.load(),
        Err(ViewError::UnsupportedFormat { version: 2 })
    );
    assert_eq!(
        persistence.save(&view(2)),
        Err(ViewError::UnsupportedFormat { version: 2 })
    );
    assert_eq!(std::fs::read(fixture.document_path()).unwrap(), bytes);
    std::fs::write(fixture.document_path(), b"malformed").unwrap();
    assert_eq!(persistence.recover(), Err(ViewError::InvalidPersistedState));
    assert_eq!(
        persistence.save(&view(2)),
        Err(ViewError::InvalidPersistedState)
    );
    assert_eq!(
        std::fs::read(fixture.document_path()).unwrap(),
        b"malformed"
    );
}

#[test]
fn desktop_server_native_view_store_rejects_displaced_directory() {
    let fixture = Fixture::new();
    let mut persistence = ViewPersistence::new(fixture.store());
    persistence.load().unwrap();
    persistence.save(&view(1)).unwrap();
    let directory = fixture.document_path().parent().unwrap().to_path_buf();
    std::fs::rename(&directory, directory.with_extension("old")).unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    assert_eq!(persistence.save(&view(2)), Err(ViewError::UnsafeStorage));
    assert!(!fixture.document_path().exists());
}

#[test]
fn desktop_server_native_view_store_empty_restore_uses_durable_incarnation_floor() {
    let fixture = Fixture::new();
    let mut persistence = ViewPersistence::new(fixture.store());
    let mut markers = RestorationFilter::new();
    markers
        .raise_incarnation_floor(spaceterm_client::view::AttachmentIncarnation::new(42).unwrap());
    let (restored, _) =
        ClientViewState::restore(persistence.load().unwrap().as_ref(), &markers).unwrap();
    persistence.save(&restored.persisted().unwrap()).unwrap();
    assert_eq!(
        persistence
            .recover()
            .unwrap()
            .unwrap()
            .parts()
            .incarnation_high_water,
        42
    );
}

#[test]
fn desktop_server_native_view_store_uncertain_publication_blocks_until_fresh_recovery() {
    let fixture = Fixture::new();
    let filesystem = Arc::new(crate::platform::testing::RecordingFilesystem::default());
    let paths = Arc::new(
        AppPaths::from_directories(fixture.paths.directories().clone(), 104, filesystem.clone())
            .unwrap(),
    );
    let mut persistence = ViewPersistence::new(NativeViewStore::new(paths));
    persistence.load().unwrap();
    filesystem.files.lock().unwrap().commit_outcome =
        Some(SecureCommitOutcome::CommittedButUnsynced);
    assert_eq!(
        persistence.save(&view(1)),
        Err(ViewError::IndeterminatePublication)
    );
    filesystem.files.lock().unwrap().commit_outcome = None;
    assert_eq!(
        persistence.save(&view(2)),
        Err(ViewError::IndeterminatePublication)
    );
    assert_eq!(persistence.recover(), Ok(Some(view(1))));
    persistence.save(&view(2)).unwrap();
}

#[test]
fn desktop_server_preference_import_export_and_reset_preserve_native_view_document() {
    let fixture = Fixture::new();
    let mut views = ViewPersistence::new(fixture.store());
    views.load().unwrap();
    views.save(&view(1)).unwrap();
    let retained = std::fs::read(fixture.document_path()).unwrap();
    let settings = crate::settings::Settings::load(Arc::new(
        crate::settings::storage::ConfigSettingsStorage::new(fixture.paths.clone()),
    ));
    let exported = settings.export_document().unwrap();
    let mut imported = crate::settings::parse_settings(exported.as_bytes()).unwrap();
    imported.sidebar.default_state = crate::settings::sidebar::SidebarState::Collapsed;
    settings
        .update_committed(settings.snapshot().committed.revision, imported)
        .unwrap()
        .run()
        .unwrap();
    let mut reset = settings.snapshot().committed.as_ref().clone();
    reset.reset_all();
    settings
        .update_committed(settings.snapshot().committed.revision, reset)
        .unwrap()
        .run()
        .unwrap();
    assert_eq!(std::fs::read(fixture.document_path()).unwrap(), retained);
    assert_eq!(views.recover(), Ok(Some(view(1))));
}

#[test]
fn desktop_server_native_view_store_first_save_requires_durable_directory_ancestry() {
    let fixture = Fixture::new();
    let mut persistence = ViewPersistence::new(fixture.store());
    assert_eq!(persistence.load(), Ok(None));
    crate::platform::unix_secure_filesystem::fail_directory_sync_after(0);
    assert_eq!(
        persistence.save(&view(1)),
        Err(ViewError::StorageUnavailable),
        "ancestry failure must refuse before the view file is published"
    );
    assert!(!fixture.document_path().exists());
    let mut recovered = ViewPersistence::new(fixture.store());
    assert_eq!(recovered.load(), Ok(None));
    recovered.save(&view(1)).unwrap();
    assert_eq!(
        ViewPersistence::new(fixture.store()).load(),
        Ok(Some(view(1)))
    );
}

#[test]
fn desktop_server_native_view_store_last_ancestor_sync_failure_cannot_publish() {
    let fixture = Fixture::new();
    let state = fixture.paths.directories().root(AppDirectoryRoot::State);
    let edges = state
        .components()
        .filter(|component| matches!(component, std::path::Component::Normal(_)))
        .count();
    let mut persistence = ViewPersistence::new(fixture.store());
    persistence.load().unwrap();
    crate::platform::unix_secure_filesystem::fail_directory_sync_after(edges * 2 - 1);
    assert_eq!(
        persistence.save(&view(1)),
        Err(ViewError::StorageUnavailable)
    );
    assert!(!fixture.document_path().exists());
    assert_eq!(ViewPersistence::new(fixture.store()).load(), Ok(None));
}

#[test]
fn desktop_server_native_view_store_loaded_directory_also_requires_ancestry_sync() {
    let fixture = Fixture::new();
    let mut first = ViewPersistence::new(fixture.store());
    first.load().unwrap();
    first.save(&view(1)).unwrap();
    let mut reopened = ViewPersistence::new(fixture.store());
    assert_eq!(reopened.load(), Ok(Some(view(1))));
    crate::platform::unix_secure_filesystem::fail_directory_sync_after(0);
    assert_eq!(reopened.save(&view(2)), Err(ViewError::StorageUnavailable));
    assert_eq!(
        ViewPersistence::new(fixture.store()).load(),
        Ok(Some(view(1)))
    );
}

#[test]
fn desktop_server_native_view_store_sync_fault_is_uncertain_until_fresh_reopen() {
    let fixture = Fixture::new();
    let mut persistence = ViewPersistence::new(fixture.store());
    persistence.load().unwrap();
    crate::platform::unix_secure_filesystem::install_before_private_file_publish_hook(|| {
        crate::platform::unix_secure_filesystem::fail_directory_sync_after(0);
    });
    assert_eq!(
        persistence.save(&view(1)),
        Err(ViewError::IndeterminatePublication)
    );
    assert_eq!(
        persistence.save(&view(2)),
        Err(ViewError::IndeterminatePublication)
    );
    drop(persistence);
    let mut fresh = ViewPersistence::new(fixture.store());
    assert_eq!(fresh.load(), Ok(Some(view(1))));
    fresh.save(&view(2)).unwrap();
    assert_eq!(
        ViewPersistence::new(fixture.store()).load(),
        Ok(Some(view(2)))
    );
}

fn crash_boundary(after: bool, test_name: &str) {
    const ROOT: &str = "SPACETERM_VIEW_STORE_CRASH_TEST_ROOT";
    if let Some(root) = std::env::var_os(ROOT) {
        let paths = Fixture::paths(std::path::Path::new(&root));
        let mut persistence = ViewPersistence::new(NativeViewStore::new(paths));
        persistence.load().unwrap();
        let crash = || std::process::exit(91);
        if after {
            crate::platform::unix_secure_filesystem::install_after_private_file_publish_hook(crash);
        } else {
            crate::platform::unix_secure_filesystem::install_before_private_file_publish_hook(
                crash,
            );
        }
        persistence.save(&view(2)).unwrap();
        panic!("publication crash point was not reached");
    }
    for initial_save in [false, true] {
        let fixture = Fixture::new();
        if initial_save {
            let mut persistence = ViewPersistence::new(fixture.store());
            persistence.load().unwrap();
            persistence.save(&view(1)).unwrap();
        }
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name])
            .env(ROOT, &fixture.root)
            .output()
            .unwrap();
        assert_eq!(
            child.status.code(),
            Some(91),
            "native publication must reach the crash boundary"
        );
        let expected = if after {
            Some(view(2))
        } else if initial_save {
            Some(view(1))
        } else {
            None
        };
        assert_eq!(ViewPersistence::new(fixture.store()).load(), Ok(expected));
    }
}

#[test]
fn desktop_server_native_view_store_crash_before_publication_preserves_original() {
    crash_boundary(
        false,
        "platform::client_view_store::tests::desktop_server_native_view_store_crash_before_publication_preserves_original",
    );
}

#[test]
fn desktop_server_native_view_store_crash_after_publication_reopens_published_document() {
    crash_boundary(
        true,
        "platform::client_view_store::tests::desktop_server_native_view_store_crash_after_publication_reopens_published_document",
    );
}
