use super::*;
use crate::appearance::{
    Appearance, AppearanceGeneration, AvailableFonts, ChromeColorOverrides, SchemeSelection,
    SystemAppearance,
};
use crate::platform::secure_filesystem::PrivateFileSnapshot;
use storage::StorageCommit;

#[derive(Default)]
struct MemoryStorage(Mutex<MemoryState>);

#[derive(Default)]
struct MemoryState {
    snapshot: Option<(Vec<u8>, u64)>,
    writes: usize,
    failure: Option<StorageError>,
    unsynced: bool,
    successor_after_commit: bool,
}

impl SettingsStorage for MemoryStorage {
    fn read(&self) -> Result<Option<PrivateFileSnapshot>, StorageError> {
        let state = self.0.lock().unwrap();
        if let Some(error) = state.failure {
            return Err(error);
        }
        Ok(state
            .snapshot
            .as_ref()
            .map(|(bytes, identity)| PrivateFileSnapshot {
                bytes: bytes.clone(),
                identity: SecureEntryIdentity::from_opaque(*identity),
            }))
    }

    fn write(
        &self,
        bytes: &[u8],
        expected: Option<&SecureEntryIdentity>,
    ) -> Result<StorageCommit, StorageError> {
        let mut state = self.0.lock().unwrap();
        if let Some(error) = state.failure {
            return Err(error);
        }
        let expected = expected
            .and_then(|identity| identity.opaque_ref::<u64>())
            .copied();
        if expected != state.snapshot.as_ref().map(|(_, identity)| *identity) {
            return Err(StorageError::Conflict);
        }
        state.writes += 1;
        let identity = expected.unwrap_or_default() + 1;
        state.snapshot = Some((bytes.to_vec(), identity));
        if state.successor_after_commit {
            state.snapshot = Some((b"external successor".to_vec(), identity + 1));
        }
        Ok(StorageCommit {
            durability: if state.unsynced {
                Durability::Uncertain
            } else {
                Durability::Synchronized
            },
            identity: (!state.successor_after_commit)
                .then(|| SecureEntryIdentity::from_opaque(identity)),
        })
    }
}

fn setup() -> (UserSettings, Arc<MemoryStorage>) {
    let storage = Arc::new(MemoryStorage::default());
    let (settings, _) = UserSettings::load(storage.clone());
    (settings, storage)
}

const IMPORTED_SCHEME: &[u8] = br##"{"schema_version":1,"schemes":[{"kind":"chrome","id":"custom.sample","name":"Sample","appearance":"light","colors":{"text":"#112233"}}]}"##;

#[test]
fn settings_owner_imports_replaces_resets_and_exports_without_implicit_selection() {
    let (settings, storage) = setup();
    let initial = settings.snapshot();
    let token = settings.begin_preview(initial.committed.revision).unwrap();
    let before_import = settings.snapshot().catalog_revision;
    let receipt = settings
        .import_preview(
            &token,
            before_import,
            SchemeImport::SpaceTerm(IMPORTED_SCHEME),
            &BTreeSet::new(),
        )
        .unwrap();
    let imported = settings.snapshot();
    assert_eq!(
        receipt.installed,
        vec![SchemeId::new("custom.sample").unwrap()]
    );
    assert_eq!(receipt.catalog_revision, imported.catalog_revision);
    assert_eq!(
        imported.candidate.preferences,
        initial.committed.preferences
    );
    assert_eq!(imported.candidate.custom_schemes.len(), 1);
    assert_eq!(storage.0.lock().unwrap().writes, 0);
    assert_eq!(settings.list_schemes().unwrap().len(), 5);
    assert_eq!(
        settings.import_preview(
            &token,
            before_import,
            SchemeImport::SpaceTerm(IMPORTED_SCHEME),
            &BTreeSet::new()
        ),
        Err(SettingsError::Stale)
    );
    let id = SchemeId::new("custom.sample").unwrap();
    let exported = settings
        .export_schemes(&[(SchemeKind::Chrome, id.clone())])
        .unwrap();
    assert!(!exported.contains("typography"));
    assert_eq!(
        settings.import_preview(
            &token,
            imported.catalog_revision,
            SchemeImport::SpaceTerm(IMPORTED_SCHEME),
            &BTreeSet::new()
        ),
        Err(SettingsError::Catalog(CatalogError::DuplicateId))
    );
    assert_eq!(
        settings.snapshot().catalog_revision,
        imported.catalog_revision
    );
    settings
        .import_preview(
            &token,
            imported.catalog_revision,
            SchemeImport::SpaceTerm(exported.as_bytes()),
            &BTreeSet::from([id]),
        )
        .unwrap();
    settings
        .reset_preview(&token, ResetTarget::AllAppearance)
        .unwrap();
    assert_eq!(settings.snapshot().candidate.custom_schemes.len(), 1);
    let job = settings.commit_preview(&token).unwrap();
    assert_eq!(
        settings.reset_preview(&token, ResetTarget::ChromeTypography),
        Err(SettingsError::Busy)
    );
    assert_eq!(
        settings.import_preview(
            &token,
            settings.snapshot().catalog_revision,
            SchemeImport::SpaceTerm(IMPORTED_SCHEME),
            &BTreeSet::new()
        ),
        Err(SettingsError::Busy)
    );
    job.run().unwrap();
    let (restarted, _) = UserSettings::load(storage);
    assert_eq!(
        restarted.export_document().unwrap(),
        settings.export_document().unwrap()
    );
}

#[test]
fn direct_import_and_reset_use_the_same_serialized_commit_owner() {
    let (settings, storage) = setup();
    let initial = settings.snapshot();
    let (receipt, job) = settings
        .import_committed(
            initial.committed.revision,
            initial.catalog_revision,
            SchemeImport::SpaceTerm(IMPORTED_SCHEME),
            &BTreeSet::new(),
        )
        .unwrap();
    assert_eq!(
        receipt.installed,
        vec![SchemeId::new("custom.sample").unwrap()]
    );
    assert_eq!(receipt.catalog_revision, initial.catalog_revision + 1);
    assert!(settings.snapshot().candidate.custom_schemes.is_empty());
    job.run().unwrap();
    assert_eq!(
        settings.snapshot().catalog_revision,
        receipt.catalog_revision
    );
    let revision = settings.snapshot().committed.revision;
    settings
        .reset_committed(revision, ResetTarget::AllAppearance)
        .unwrap()
        .run()
        .unwrap();
    assert_eq!(settings.snapshot().committed.custom_schemes.len(), 1);
    assert_eq!(storage.0.lock().unwrap().writes, 2);
}

#[test]
fn zed_candidates_require_explicit_selection_and_install_through_settings() {
    let bytes = br##"{"themes":[{"name":"Sample","appearance":"dark","style":{"terminal.foreground":"#abcdef"}}]}"##;
    let candidates = UserSettings::list_import_candidates(bytes).unwrap();
    assert_eq!(candidates.len(), 1);
    let (settings, _) = setup();
    let token = settings.begin_preview(0).unwrap();
    settings
        .import_preview(
            &token,
            settings.snapshot().catalog_revision,
            SchemeImport::Zed {
                bytes,
                candidate_index: candidates[0].index,
                kinds: &[ZedImportKind::Terminal],
            },
            &BTreeSet::new(),
        )
        .unwrap();
    assert_eq!(settings.snapshot().candidate.custom_schemes.len(), 1);
    assert_eq!(
        settings.snapshot().candidate.preferences,
        AppearanceDocument::default().preferences
    );
}

#[test]
fn preview_deletion_preserves_selected_request_and_resolves_to_builtin_fallback() {
    let (settings, storage) = setup();
    let token = settings.begin_preview(0).unwrap();
    let imported = settings
        .import_preview(
            &token,
            settings.snapshot().catalog_revision,
            SchemeImport::SpaceTerm(IMPORTED_SCHEME),
            &BTreeSet::new(),
        )
        .unwrap();
    let selected = imported.installed[0].clone();
    let mut candidate = (*settings.snapshot().candidate).clone();
    candidate.preferences.chrome.scheme = SchemeSelection::Fixed {
        id: selected.clone(),
        appearance: Appearance::Light,
    };
    candidate
        .preferences
        .chrome
        .overrides
        .insert(selected.clone(), ChromeColorOverrides::default());
    settings.update_preview(&token, candidate).unwrap();

    let catalog_revision = settings.snapshot().catalog_revision;
    let preferences_before_deletion = settings.snapshot().candidate.preferences.clone();
    let removed_revision = settings
        .remove_custom_scheme_preview(&token, catalog_revision, &selected)
        .unwrap();
    let snapshot = settings.snapshot();
    assert_eq!(removed_revision, snapshot.catalog_revision);
    assert!(snapshot.candidate.custom_schemes.is_empty());
    assert_eq!(snapshot.candidate.preferences, preferences_before_deletion);
    let resolved = SchemeCatalog::default()
        .resolve(
            AppearanceGeneration::INITIAL,
            &snapshot.candidate.preferences,
            SystemAppearance::unavailable(),
            &AvailableFonts::default(),
        )
        .unwrap();
    assert_eq!(resolved.chrome.requested_scheme, selected);
    assert_eq!(
        resolved.chrome.effective_scheme,
        SchemeId::builtin("builtin.spaceterm.chrome.light")
    );
    assert_eq!(storage.0.lock().unwrap().writes, 0);
}

#[test]
fn deletion_rejects_builtin_and_unknown_ids_without_mutation() {
    let (settings, storage) = setup();
    let token = settings.begin_preview(0).unwrap();
    let before = settings.snapshot();

    assert_eq!(
        settings.remove_custom_scheme_preview(
            &token,
            before.catalog_revision,
            &SchemeId::builtin("builtin.vague-pro.chrome.dark"),
        ),
        Err(SettingsError::Catalog(CatalogError::ReservedId))
    );
    assert_eq!(
        settings.snapshot().catalog_revision,
        before.catalog_revision
    );
    assert_eq!(
        settings.remove_custom_scheme_preview(
            &token,
            before.catalog_revision,
            &SchemeId::new("custom.unknown").unwrap(),
        ),
        Err(SettingsError::Catalog(CatalogError::UnknownReplacement))
    );
    assert_eq!(
        settings.snapshot().catalog_revision,
        before.catalog_revision
    );
    assert_eq!(
        settings.snapshot().candidate.custom_schemes,
        before.candidate.custom_schemes
    );
    assert_eq!(storage.0.lock().unwrap().writes, 0);
}

#[test]
fn deletion_rejects_stale_and_busy_operations_without_removing_the_scheme() {
    let (settings, _) = setup();
    let token = settings.begin_preview(0).unwrap();
    let stale_catalog_revision = settings.snapshot().catalog_revision;
    let imported = settings
        .import_preview(
            &token,
            stale_catalog_revision,
            SchemeImport::SpaceTerm(IMPORTED_SCHEME),
            &BTreeSet::new(),
        )
        .unwrap();
    let id = imported.installed[0].clone();

    assert_eq!(
        settings.remove_custom_scheme_preview(&token, stale_catalog_revision, &id),
        Err(SettingsError::Stale)
    );
    let current_catalog_revision = settings.snapshot().catalog_revision;
    let job = settings.commit_preview(&token).unwrap();
    assert_eq!(
        settings.remove_custom_scheme_preview(&token, current_catalog_revision, &id),
        Err(SettingsError::Busy)
    );
    assert!(matches!(
        settings.remove_custom_scheme_committed(0, current_catalog_revision, &id),
        Err(SettingsError::Busy)
    ));
    assert_eq!(settings.snapshot().candidate.custom_schemes.len(), 1);
    drop(job);
}

#[test]
fn direct_deletion_commits_only_the_named_custom_scheme() {
    let (settings, storage) = setup();
    let initial = settings.snapshot();
    let (receipt, job) = settings
        .import_committed(
            initial.committed.revision,
            initial.catalog_revision,
            SchemeImport::SpaceTerm(IMPORTED_SCHEME),
            &BTreeSet::new(),
        )
        .unwrap();
    let id = receipt.installed[0].clone();
    job.run().unwrap();
    let installed = settings.snapshot();

    let deletion = settings
        .remove_custom_scheme_committed(
            installed.committed.revision,
            installed.catalog_revision,
            &id,
        )
        .unwrap();
    assert_eq!(settings.snapshot().committed.custom_schemes.len(), 1);
    deletion.run().unwrap();
    assert!(settings.snapshot().committed.custom_schemes.is_empty());
    assert_eq!(settings.list_schemes().unwrap().len(), 4);
    assert_eq!(storage.0.lock().unwrap().writes, 2);
}

#[test]
fn invalid_reload_preserves_valid_settings_until_a_later_valid_reload() {
    let (settings, storage) = setup();
    let mut first = AppearanceDocument::default();
    first.preferences.chrome.typography.base_size = 20.0;
    settings.update_committed(0, first).unwrap().run().unwrap();
    let committed = settings.snapshot().committed;
    storage.0.lock().unwrap().snapshot = Some((b"invalid document".to_vec(), 50));
    assert!(settings.reload().is_err());
    assert_eq!(*settings.snapshot().committed, *committed);
    assert_eq!(
        storage.0.lock().unwrap().snapshot.as_ref().unwrap().0,
        b"invalid document"
    );
    let mut second = (*committed).clone();
    second.preferences.terminal.typography.base_size = 30.0;
    storage.0.lock().unwrap().snapshot = Some((export_settings(&second).unwrap().into_bytes(), 51));
    settings.reload().unwrap();
    let loaded = settings.snapshot();
    assert!(loaded.committed.revision > committed.revision);
    assert_eq!(loaded.committed.preferences, second.preferences);
    assert_eq!(loaded.status, None);
}

#[test]
fn failed_direct_save_preserves_local_candidate_without_publishing_it() {
    let (settings, storage) = setup();
    let original = settings.snapshot().committed;
    let mut candidate = (*original).clone();
    candidate.preferences.chrome.typography.base_size = 24.0;
    let job = settings
        .update_committed(original.revision, candidate.clone())
        .unwrap();
    storage.0.lock().unwrap().snapshot =
        Some((export_settings(&original).unwrap().into_bytes(), 55));
    assert_eq!(
        job.run(),
        Err(SettingsError::Storage(StorageError::Conflict))
    );
    let failed = settings.snapshot();
    assert_eq!(failed.phase, PreviewPhase::Idle);
    assert_eq!(*failed.candidate, *original);
    assert_eq!(failed.recoverable_candidate.as_deref(), Some(&candidate));
    settings.reload().unwrap();
    assert_eq!(
        settings.snapshot().recoverable_candidate.as_deref(),
        Some(&candidate)
    );
    candidate.revision = settings.snapshot().committed.revision;
    settings
        .update_committed(candidate.revision, candidate)
        .unwrap()
        .run()
        .unwrap();
    assert!(settings.snapshot().recoverable_candidate.is_none());
    assert_eq!(
        settings
            .snapshot()
            .committed
            .preferences
            .chrome
            .typography
            .base_size,
        24.0
    );
}

#[test]
fn preview_cancel_and_owner_destruction_never_write() {
    let (settings, storage) = setup();
    let committed = settings.snapshot().committed;
    let token = settings.begin_preview(committed.revision).unwrap();
    settings
        .update_preview(&token, (*committed).clone())
        .unwrap();
    assert_eq!(settings.snapshot().phase, PreviewPhase::Previewing);
    settings.cancel_preview(&token).unwrap();
    assert_eq!(settings.snapshot().phase, PreviewPhase::Idle);
    let token = settings.begin_preview(committed.revision).unwrap();
    drop(token);
    assert_eq!(settings.snapshot().phase, PreviewPhase::Idle);
    assert_eq!(storage.0.lock().unwrap().writes, 0);
}

#[test]
fn invalid_preview_candidate_preserves_the_previous_valid_preview() {
    let (settings, _) = setup();
    let previous = settings.snapshot().candidate;
    let token = settings.begin_preview(previous.revision).unwrap();
    let mut invalid = (*previous).clone();
    invalid.revision += 1;
    assert_eq!(
        settings.update_preview(&token, invalid),
        Err(SettingsError::Stale)
    );
    assert_eq!(
        export_settings(&settings.snapshot().candidate).unwrap(),
        export_settings(&previous).unwrap()
    );
}

#[test]
fn commit_captures_one_candidate_and_rejects_conflicting_edits() {
    let (settings, storage) = setup();
    let original = settings.snapshot().committed;
    let token = settings.begin_preview(original.revision).unwrap();
    let job = settings.commit_preview(&token).unwrap();
    assert_eq!(settings.snapshot().phase, PreviewPhase::Committing);
    assert_eq!(settings.cancel_preview(&token), Err(SettingsError::Busy));
    assert_eq!(
        settings.update_preview(&token, (*original).clone()),
        Err(SettingsError::Busy)
    );
    assert_eq!(settings.reload(), Err(SettingsError::Busy));
    drop(token);
    let outcome = job.run().unwrap();
    assert_eq!(outcome.revision, original.revision + 1);
    assert_eq!(settings.snapshot().phase, PreviewPhase::Idle);
    let (restarted, _) = UserSettings::load(storage);
    assert_eq!(
        export_settings(&settings.snapshot().committed).unwrap(),
        export_settings(&restarted.snapshot().committed).unwrap()
    );
}

#[test]
fn failed_commit_retains_editable_preview_when_owner_survives() {
    let (settings, storage) = setup();
    let original = settings.snapshot().committed;
    let token = settings.begin_preview(original.revision).unwrap();
    let job = settings.commit_preview(&token).unwrap();
    storage.0.lock().unwrap().failure = Some(StorageError::Unavailable);
    assert_eq!(
        job.run(),
        Err(SettingsError::Storage(StorageError::Unavailable))
    );
    assert_eq!(settings.snapshot().phase, PreviewPhase::Previewing);
    settings
        .update_preview(&token, (*original).clone())
        .unwrap();
    storage.0.lock().unwrap().failure = None;
    settings.commit_preview(&token).unwrap().run().unwrap();
    assert_eq!(
        settings.snapshot().committed.revision,
        original.revision + 1
    );
}

#[test]
fn failed_commit_after_owner_destruction_restores_committed_appearance() {
    let (settings, storage) = setup();
    let original = settings.snapshot().committed;
    let token = settings.begin_preview(original.revision).unwrap();
    let job = settings.commit_preview(&token).unwrap();
    drop(token);
    storage.0.lock().unwrap().failure = Some(StorageError::Unavailable);
    assert!(job.run().is_err());
    assert_eq!(settings.snapshot().phase, PreviewPhase::Idle);
    assert_eq!(settings.snapshot().candidate.revision, original.revision);
}

#[test]
fn abandoned_commit_job_releases_busy_state_without_writing() {
    let (settings, storage) = setup();
    let token = settings
        .begin_preview(settings.snapshot().committed.revision)
        .unwrap();
    drop(settings.commit_preview(&token).unwrap());
    assert_eq!(settings.snapshot().phase, PreviewPhase::Previewing);
    assert_eq!(storage.0.lock().unwrap().writes, 0);
}

#[test]
fn competing_writer_is_preserved_and_requires_explicit_reload() {
    let (settings, storage) = setup();
    let original = settings.snapshot().committed;
    let job = settings
        .update_committed(original.revision, (*original).clone())
        .unwrap();
    let external = export_settings(&original).unwrap().into_bytes();
    storage.0.lock().unwrap().snapshot = Some((external.clone(), 55));
    assert_eq!(
        job.run(),
        Err(SettingsError::Storage(StorageError::Conflict))
    );
    assert_eq!(
        storage.0.lock().unwrap().snapshot.as_ref().unwrap().0,
        external
    );
    assert!(
        settings
            .update_committed(original.revision, (*original).clone())
            .is_err()
    );
    settings.reload().unwrap();
    assert!(matches!(
        settings.update_committed(original.revision, (*original).clone()),
        Err(SettingsError::Stale)
    ));
    let reloaded = settings.snapshot().committed;
    settings
        .update_committed(reloaded.revision, (*reloaded).clone())
        .unwrap()
        .run()
        .unwrap();
}

#[test]
fn successor_installed_after_commit_does_not_authorize_next_overwrite() {
    let (settings, storage) = setup();
    storage.0.lock().unwrap().successor_after_commit = true;
    let original = settings.snapshot().committed;
    let outcome = settings
        .update_committed(original.revision, (*original).clone())
        .unwrap()
        .run()
        .unwrap();
    assert!(outcome.reload_required);
    let current = settings.snapshot().committed;
    assert_eq!(current.revision, original.revision + 1);
    assert!(
        settings
            .update_committed(current.revision, (*current).clone())
            .is_err()
    );
    assert_eq!(storage.0.lock().unwrap().writes, 1);
    assert_eq!(
        storage.0.lock().unwrap().snapshot.as_ref().unwrap().0,
        b"external successor"
    );
}

#[test]
fn uncertain_durability_is_committed_and_identity_is_reconciled() {
    let (settings, storage) = setup();
    storage.0.lock().unwrap().unsynced = true;
    let original = settings.snapshot().committed;
    let outcome = settings
        .update_committed(original.revision, (*original).clone())
        .unwrap()
        .run()
        .unwrap();
    assert_eq!(outcome.durability, Durability::Uncertain);
    assert!(!outcome.reload_required);
    assert_eq!(
        settings.snapshot().committed.revision,
        original.revision + 1
    );
    let current = settings.snapshot().committed;
    settings
        .update_committed(current.revision, (*current).clone())
        .unwrap()
        .run()
        .unwrap();
    assert_eq!(storage.0.lock().unwrap().writes, 2);
}

#[test]
fn invalid_startup_document_is_retained_and_cannot_be_overwritten() {
    let storage = Arc::new(MemoryStorage::default());
    storage.0.lock().unwrap().snapshot = Some((b"invalid document".to_vec(), 1));
    let (settings, _) = UserSettings::load(storage.clone());
    assert_eq!(settings.snapshot().status, Some(SettingsError::Invalid));
    let current = settings.snapshot().committed;
    assert!(
        settings
            .update_committed(current.revision, (*current).clone())
            .is_err()
    );
    assert_eq!(
        storage.0.lock().unwrap().snapshot.as_ref().unwrap().0,
        b"invalid document"
    );
}

#[test]
fn preview_tokens_cannot_cross_settings_owners_or_revisions() {
    let (first, _) = setup();
    let (second, _) = setup();
    let original = first.snapshot().committed;
    let token = first.begin_preview(original.revision).unwrap();
    assert_eq!(second.cancel_preview(&token), Err(SettingsError::Stale));
    first.commit_preview(&token).unwrap().run().unwrap();
    assert_eq!(first.cancel_preview(&token), Err(SettingsError::Stale));
}
