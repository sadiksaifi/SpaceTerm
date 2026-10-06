use super::*;
use crate::appearance::{AppearanceMode, TerminalColorOverrides, ZedExtension};
use crate::platform::secure_filesystem::PrivateFileSnapshot;
use storage::StorageCommit;

#[derive(Default)]
struct MemoryStorage(Mutex<MemoryState>);

#[derive(Default)]
struct MemoryState {
    snapshot: Option<(Vec<u8>, u64)>,
    writes: usize,
    backup: Option<Vec<u8>>,
    successor_after_quarantine: bool,
    failure: Option<StorageError>,
    unsynced: bool,
    successor_after_commit: bool,
}

impl SettingsStorage for MemoryStorage {
    fn quarantine(&self) -> Result<(), StorageError> {
        let mut state = self.0.lock().unwrap();
        if let Some(error) = state.failure {
            return Err(error);
        }
        state.backup = Some(state.snapshot.take().ok_or(StorageError::Unavailable)?.0);
        if state.successor_after_quarantine {
            state.snapshot = Some((b"competing writer".to_vec(), 30));
        }
        Ok(())
    }

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

fn setup() -> (Settings, Arc<MemoryStorage>) {
    let storage = Arc::new(MemoryStorage::default());
    let settings = Settings::load(storage.clone());
    (settings, storage)
}

const ZED_FAMILY: &[u8] = br##"{"name":"Sample Family","themes":[{"name":"Sample","appearance":"light","style":{"terminal.foreground":"#112233"}}]}"##;

fn zed_extension(version: &str, names: &[&str]) -> ZedExtension {
    let themes = names
        .iter()
        .map(|name| serde_json::json!({ "name": name, "appearance": "dark", "style": {} }))
        .collect::<Vec<_>>();
    ZedExtension {
        id: String::from("sample-themes"),
        version: version.to_owned(),
        families: vec![
            serde_json::to_vec(&serde_json::json!({ "name": "Sample", "themes": themes })).unwrap(),
        ],
    }
}

fn installed_names(settings: &Settings) -> Vec<String> {
    let mut names = settings
        .snapshot()
        .candidate
        .terminal_themes
        .iter()
        .map(|theme| theme.name.clone())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn settings_owner_imports_reinstalls_resets_and_exports_without_implicit_selection() {
    let (settings, storage) = setup();
    let initial = settings.snapshot();
    let token = settings.begin_preview(initial.committed.revision).unwrap();
    let before_import = settings.snapshot().catalog_revision;
    let receipt = settings
        .import_preview(&token, before_import, ThemeImport::ZedFamily(ZED_FAMILY))
        .unwrap();
    let imported = settings.snapshot();
    assert_eq!(receipt.installed.len(), 1);
    assert_eq!(receipt.catalog_revision, imported.catalog_revision);
    assert_eq!(imported.candidate.appearance, initial.committed.appearance);
    assert_eq!(imported.candidate.terminal_themes.len(), 1);
    assert_eq!(storage.0.lock().unwrap().writes, 0);
    assert_eq!(
        ThemeCatalog::from_terminal_themes(&settings.snapshot().candidate.terminal_themes)
            .unwrap()
            .summaries()
            .len(),
        3
    );
    assert_eq!(
        settings.import_preview(&token, before_import, ThemeImport::ZedFamily(ZED_FAMILY)),
        Err(SettingsError::Stale)
    );
    let exported = settings.export_document().unwrap();
    let copy = crate::settings::parse_settings(exported.as_bytes()).unwrap();
    assert_eq!(&copy, imported.candidate.as_ref());
    let reinstalled = settings
        .import_preview(
            &token,
            imported.catalog_revision,
            ThemeImport::ZedFamily(ZED_FAMILY),
        )
        .unwrap();
    assert_eq!(reinstalled.installed, receipt.installed);
    assert_eq!(settings.snapshot().candidate.terminal_themes.len(), 1);
    settings
        .reset_preview(&token, ResetTarget::AllAppearance)
        .unwrap();
    assert_eq!(settings.snapshot().candidate.terminal_themes.len(), 1);
    let job = settings.commit_preview(&token).unwrap();
    assert_eq!(
        settings.reset_preview(&token, ResetTarget::TerminalTypography),
        Err(SettingsError::Busy)
    );
    assert_eq!(
        settings.import_preview(
            &token,
            settings.snapshot().catalog_revision,
            ThemeImport::ZedFamily(ZED_FAMILY),
        ),
        Err(SettingsError::Busy)
    );
    job.run().unwrap();
    let restarted = Settings::load(storage);
    assert_eq!(
        restarted.export_document().unwrap(),
        settings.export_document().unwrap()
    );
}

#[test]
fn preview_import_and_reset_use_the_same_serialized_commit_owner() {
    let (settings, storage) = setup();
    let initial = settings.snapshot();
    let token = settings.begin_preview(initial.committed.revision).unwrap();
    let before_import = settings.snapshot().catalog_revision;
    let receipt = settings
        .import_preview(&token, before_import, ThemeImport::ZedFamily(ZED_FAMILY))
        .unwrap();
    assert_eq!(receipt.installed.len(), 1);
    assert_eq!(receipt.catalog_revision, before_import + 1);
    assert!(settings.snapshot().committed.terminal_themes.is_empty());
    settings.commit_preview(&token).unwrap().run().unwrap();
    assert_eq!(settings.snapshot().committed.terminal_themes.len(), 1);
    assert_eq!(
        settings.snapshot().catalog_revision,
        receipt.catalog_revision + 1
    );
    let token = settings
        .begin_preview(settings.snapshot().committed.revision)
        .unwrap();
    let mut candidate = (*settings.snapshot().candidate).clone();
    candidate.appearance.mode = AppearanceMode::Light;
    settings.update_preview(&token, candidate).unwrap();
    settings
        .reset_preview(&token, ResetTarget::AllAppearance)
        .unwrap();
    assert_eq!(
        settings.snapshot().candidate.appearance,
        SettingsDocument::default().appearance
    );
    settings.commit_preview(&token).unwrap().run().unwrap();
    assert_eq!(settings.snapshot().committed.terminal_themes.len(), 1);
    assert_eq!(storage.0.lock().unwrap().writes, 2);
}

#[test]
fn a_zed_family_installs_without_changing_any_selection() {
    let (settings, _) = setup();
    let token = settings.begin_preview(0).unwrap();
    let invalid = settings.import_preview(
        &token,
        settings.snapshot().catalog_revision,
        ThemeImport::ZedFamily(br#"{"themes":"none"}"#),
    );
    assert_eq!(
        invalid,
        Err(SettingsError::Import(ImportError::InvalidZedDocument))
    );
    settings
        .import_preview(
            &token,
            settings.snapshot().catalog_revision,
            ThemeImport::ZedFamily(ZED_FAMILY),
        )
        .unwrap();
    assert_eq!(settings.snapshot().candidate.terminal_themes.len(), 1);
    assert_eq!(
        settings.snapshot().candidate.appearance,
        SettingsDocument::default().appearance
    );
}

#[test]
fn a_large_zed_family_installs_as_one_batch() {
    let themes = (0..200)
        .map(|index| {
            serde_json::json!({
                "name": format!("Theme {index}"),
                "appearance": if index % 2 == 0 { "dark" } else { "light" },
                "style": { "terminal.foreground": "#abcdef" }
            })
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&serde_json::json!({ "themes": themes })).unwrap();
    let (settings, _) = setup();
    let token = settings.begin_preview(0).unwrap();

    let receipt = settings
        .import_preview(
            &token,
            settings.snapshot().catalog_revision,
            ThemeImport::ZedFamily(&bytes),
        )
        .unwrap();

    assert_eq!(receipt.installed.len(), 200);
    assert_eq!(settings.snapshot().candidate.terminal_themes.len(), 200);
}

#[test]
fn updating_an_extension_replaces_its_themes_and_keeps_surviving_selections() {
    let (settings, _) = setup();
    let token = settings.begin_preview(0).unwrap();
    let first = settings
        .import_preview(
            &token,
            settings.snapshot().catalog_revision,
            ThemeImport::ZedExtension(&zed_extension("1.0.0", &["Kept", "Dropped"])),
        )
        .unwrap();
    settings
        .import_preview(
            &token,
            settings.snapshot().catalog_revision,
            ThemeImport::ZedFamily(ZED_FAMILY),
        )
        .unwrap();
    let kept = first
        .installed
        .iter()
        .find(|id| {
            settings
                .snapshot()
                .candidate
                .terminal_themes
                .iter()
                .any(|theme| &theme.id == *id && theme.name == "Kept")
        })
        .unwrap()
        .clone();
    let mut candidate = (*settings.snapshot().candidate).clone();
    candidate.appearance.terminal.themes.dark = kept.clone();
    settings.update_preview(&token, candidate).unwrap();

    let second = settings
        .import_preview(
            &token,
            settings.snapshot().catalog_revision,
            ThemeImport::ZedExtension(&zed_extension("2.0.0", &["Kept", "Added"])),
        )
        .unwrap();

    assert!(second.installed.contains(&kept));
    assert_eq!(installed_names(&settings), ["Added", "Kept", "Sample"]);
    let snapshot = settings.snapshot();
    assert_eq!(snapshot.candidate.appearance.terminal.themes.dark, kept);
    let versions = snapshot
        .candidate
        .terminal_themes
        .iter()
        .filter_map(|theme| theme.metadata.origin.as_ref()?.package_version.clone())
        .collect::<Vec<_>>();
    assert_eq!(versions, ["2.0.0", "2.0.0"]);
}

/// An update that no longer ships the selected theme returns its slot to the built-in theme.
#[test]
fn updating_an_extension_without_the_selected_theme_selects_the_builtin_theme() {
    let (settings, _) = setup();
    let token = settings.begin_preview(0).unwrap();
    let first = settings
        .import_preview(
            &token,
            settings.snapshot().catalog_revision,
            ThemeImport::ZedExtension(&zed_extension("1.0.0", &["Dropped"])),
        )
        .unwrap();
    let mut candidate = (*settings.snapshot().candidate).clone();
    candidate.appearance.terminal.themes.dark = first.installed[0].clone();
    settings.update_preview(&token, candidate).unwrap();

    settings
        .import_preview(
            &token,
            settings.snapshot().catalog_revision,
            ThemeImport::ZedExtension(&zed_extension("2.0.0", &["Added"])),
        )
        .unwrap();

    assert_eq!(installed_names(&settings), ["Added"]);
    assert_eq!(
        settings
            .snapshot()
            .candidate
            .appearance
            .terminal
            .themes
            .dark,
        ThemeId::builtin("builtin.spaceterm.dark")
    );
}

#[test]
fn a_failed_extension_update_keeps_the_installed_version() {
    let (settings, _) = setup();
    let token = settings.begin_preview(0).unwrap();
    settings
        .import_preview(
            &token,
            settings.snapshot().catalog_revision,
            ThemeImport::ZedExtension(&zed_extension("1.0.0", &["Kept"])),
        )
        .unwrap();
    let before = settings.snapshot();
    let broken = ZedExtension {
        families: vec![b"{".to_vec()],
        ..zed_extension("2.0.0", &[])
    };

    assert_eq!(
        settings.import_preview(
            &token,
            before.catalog_revision,
            ThemeImport::ZedExtension(&broken),
        ),
        Err(SettingsError::Import(ImportError::InvalidThemeCount))
    );
    assert_eq!(
        settings.snapshot().catalog_revision,
        before.catalog_revision
    );
    assert_eq!(
        settings.snapshot().candidate.terminal_themes,
        before.candidate.terminal_themes
    );
}

/// Removing the selected theme returns its slot to the built-in theme and changes nothing else.
#[test]
fn preview_deletion_returns_the_selected_slot_to_its_builtin_theme() {
    let (settings, storage) = setup();
    let token = settings.begin_preview(0).unwrap();
    let imported = settings
        .import_preview(
            &token,
            settings.snapshot().catalog_revision,
            ThemeImport::ZedFamily(ZED_FAMILY),
        )
        .unwrap();
    let selected = imported.installed[0].clone();
    let mut candidate = (*settings.snapshot().candidate).clone();
    candidate.appearance.mode = AppearanceMode::Light;
    candidate.appearance.terminal.themes.light = selected.clone();
    candidate
        .appearance
        .terminal
        .overrides
        .insert(selected.clone(), TerminalColorOverrides::default());
    settings.update_preview(&token, candidate).unwrap();

    let catalog_revision = settings.snapshot().catalog_revision;
    let mut expected = settings.snapshot().candidate.appearance.clone();
    expected.terminal.themes.light = ThemeId::builtin("builtin.spaceterm.light");
    let removed_revision = settings
        .remove_themes_preview(&token, catalog_revision, std::slice::from_ref(&selected))
        .unwrap();
    let snapshot = settings.snapshot();
    assert_eq!(removed_revision, snapshot.catalog_revision);
    assert!(snapshot.candidate.terminal_themes.is_empty());
    assert_eq!(snapshot.candidate.appearance, expected);
    assert_eq!(storage.0.lock().unwrap().writes, 0);
}

/// Every selection names an installed theme, so a preview cannot select one that is missing.
#[test]
fn preview_rejects_a_missing_theme_selection() {
    let (settings, _storage) = setup();
    let token = settings.begin_preview(0).unwrap();
    let mut candidate = (*settings.snapshot().candidate).clone();
    candidate.appearance.terminal.themes.dark = ThemeId::new("custom.missing").unwrap();

    assert!(settings.update_preview(&token, candidate).is_err());
}

#[test]
fn deletion_rejects_builtin_and_unknown_ids_without_mutation() {
    let (settings, storage) = setup();
    let token = settings.begin_preview(0).unwrap();
    let before = settings.snapshot();

    assert_eq!(
        settings.remove_themes_preview(
            &token,
            before.catalog_revision,
            &[ThemeId::builtin("builtin.spaceterm.dark")],
        ),
        Err(SettingsError::Catalog(CatalogError::ReservedId))
    );
    assert_eq!(
        settings.snapshot().catalog_revision,
        before.catalog_revision
    );
    assert_eq!(
        settings.remove_themes_preview(
            &token,
            before.catalog_revision,
            &[ThemeId::new("custom.unknown").unwrap()],
        ),
        Err(SettingsError::Catalog(CatalogError::UnknownTheme))
    );
    assert_eq!(
        settings.snapshot().catalog_revision,
        before.catalog_revision
    );
    assert_eq!(
        settings.snapshot().candidate.terminal_themes,
        before.candidate.terminal_themes
    );
    assert_eq!(storage.0.lock().unwrap().writes, 0);
}

#[test]
fn removing_several_themes_is_all_or_nothing() {
    let (settings, storage) = setup();
    let token = settings.begin_preview(0).unwrap();
    let imported = settings
        .import_preview(
            &token,
            settings.snapshot().catalog_revision,
            ThemeImport::ZedFamily(ZED_FAMILY),
        )
        .unwrap();
    let id = imported.installed[0].clone();
    let before = settings.snapshot();

    assert_eq!(
        settings.remove_themes_preview(
            &token,
            before.catalog_revision,
            &[id.clone(), ThemeId::new("custom.unknown").unwrap()],
        ),
        Err(SettingsError::Catalog(CatalogError::UnknownTheme))
    );
    assert_eq!(
        settings.remove_themes_preview(&token, before.catalog_revision, &[]),
        Err(SettingsError::Catalog(CatalogError::UnknownTheme))
    );
    assert_eq!(
        settings.snapshot().candidate.terminal_themes,
        before.candidate.terminal_themes
    );

    settings
        .remove_themes_preview(&token, before.catalog_revision, &[id])
        .unwrap();
    assert!(settings.snapshot().candidate.terminal_themes.is_empty());
    assert_eq!(storage.0.lock().unwrap().writes, 0);
}

#[test]
fn deletion_rejects_stale_and_busy_operations_without_removing_the_theme() {
    let (settings, _) = setup();
    let token = settings.begin_preview(0).unwrap();
    let stale_catalog_revision = settings.snapshot().catalog_revision;
    let imported = settings
        .import_preview(
            &token,
            stale_catalog_revision,
            ThemeImport::ZedFamily(ZED_FAMILY),
        )
        .unwrap();
    let id = imported.installed[0].clone();

    assert_eq!(
        settings.remove_themes_preview(&token, stale_catalog_revision, std::slice::from_ref(&id)),
        Err(SettingsError::Stale)
    );
    let current_catalog_revision = settings.snapshot().catalog_revision;
    let job = settings.commit_preview(&token).unwrap();
    assert_eq!(
        settings.remove_themes_preview(&token, current_catalog_revision, std::slice::from_ref(&id)),
        Err(SettingsError::Busy)
    );
    assert_eq!(settings.snapshot().candidate.terminal_themes.len(), 1);
    drop(job);
}

#[test]
fn preview_deletion_commits_only_the_named_custom_theme() {
    let (settings, storage) = setup();
    let token = settings.begin_preview(0).unwrap();
    let receipt = settings
        .import_preview(
            &token,
            settings.snapshot().catalog_revision,
            ThemeImport::ZedFamily(ZED_FAMILY),
        )
        .unwrap();
    let id = receipt.installed[0].clone();
    settings.commit_preview(&token).unwrap().run().unwrap();
    let installed = settings.snapshot();
    let token = settings
        .begin_preview(installed.committed.revision)
        .unwrap();
    settings
        .remove_themes_preview(
            &token,
            settings.snapshot().catalog_revision,
            std::slice::from_ref(&id),
        )
        .unwrap();
    let deletion = settings.commit_preview(&token).unwrap();
    assert_eq!(settings.snapshot().committed.terminal_themes.len(), 1);
    deletion.run().unwrap();
    assert!(settings.snapshot().committed.terminal_themes.is_empty());
    assert_eq!(
        ThemeCatalog::from_terminal_themes(&settings.snapshot().candidate.terminal_themes)
            .unwrap()
            .summaries()
            .len(),
        2
    );
    assert_eq!(storage.0.lock().unwrap().writes, 2);
}

#[test]
fn invalid_reload_preserves_valid_settings_until_a_later_valid_reload() {
    let (settings, storage) = setup();
    let mut first = SettingsDocument::default();
    first.appearance.terminal.typography.base_size = 20.0;
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
    second.appearance.terminal.typography.base_size = 30.0;
    storage.0.lock().unwrap().snapshot = Some((export_settings(&second).unwrap().into_bytes(), 51));
    settings.reload().unwrap();
    let loaded = settings.snapshot();
    assert!(loaded.committed.revision > committed.revision);
    assert_eq!(loaded.committed.appearance, second.appearance);
    assert_eq!(loaded.status, None);
}

#[test]
fn failed_direct_save_preserves_local_candidate_without_publishing_it() {
    let (settings, storage) = setup();
    let original = settings.snapshot().committed;
    let mut candidate = (*original).clone();
    candidate.appearance.terminal.typography.base_size = 24.0;
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
            .appearance
            .terminal
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
    let restarted = Settings::load(storage);
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
    let settings = Settings::load(storage.clone());
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

#[test]
fn subscribers_receive_coalesced_changes_and_closed_subscribers_are_pruned() {
    let (settings, _) = setup();
    let first = settings.subscribe();
    let second = settings.subscribe();
    let closed = settings.subscribe();
    drop(closed);
    let token = settings.begin_preview(0).unwrap();
    settings.cancel_preview(&token).unwrap();
    assert_eq!(first.try_recv(), Ok(()));
    assert_eq!(second.try_recv(), Ok(()));
    assert!(first.try_recv().is_err());
    assert!(second.try_recv().is_err());
    assert_eq!(settings.0.subscribers.lock().unwrap().len(), 2);
    let late = settings.subscribe();
    settings.begin_preview(0).unwrap();
    assert_eq!(first.try_recv(), Ok(()));
    assert_eq!(second.try_recv(), Ok(()));
    assert_eq!(late.try_recv(), Ok(()));
}

#[test]
fn recovery_keeps_exact_bytes_replaces_backup_and_retires_edits() {
    let (settings, storage) = setup();
    let mut document = (*settings.snapshot().committed).clone();
    document.appearance.mode = AppearanceMode::Light;
    settings
        .update_committed(0, document)
        .unwrap()
        .run()
        .unwrap();
    let before = settings.snapshot();
    let broken = b"{ broken settings\0\xff";
    {
        let mut state = storage.0.lock().unwrap();
        state.snapshot = Some((broken.to_vec(), 20));
        state.backup = Some(b"old backup".to_vec());
    }
    assert_eq!(settings.reload(), Err(SettingsError::Invalid));
    assert!(
        settings
            .update_committed(before.committed.revision, (*before.committed).clone())
            .is_err()
    );
    assert!(settings.snapshot().recoverable_candidate.is_some());
    let first = settings.subscribe();
    let second = settings.subscribe();
    let receipt = settings.recover_by_reset().unwrap();
    let recovered = settings.snapshot();
    assert_eq!(recovered.status, None);
    assert_eq!(recovered.phase, PreviewPhase::Idle);
    assert_eq!(
        recovered.committed.appearance,
        SettingsDocument::default().appearance
    );
    assert!(recovered.recoverable_candidate.is_none());
    assert!(recovered.committed.revision > before.committed.revision);
    assert!(recovered.catalog_revision > before.catalog_revision);
    assert_eq!(receipt.durability, Durability::Synchronized);
    assert_eq!(first.try_recv(), Ok(()));
    assert_eq!(second.try_recv(), Ok(()));
    {
        let state = storage.0.lock().unwrap();
        assert_eq!(state.backup.as_deref(), Some(broken.as_slice()));
        assert_eq!(
            state.snapshot.as_ref().unwrap().0,
            export_settings(&SettingsDocument::default())
                .unwrap()
                .as_bytes()
        );
    }
    assert!(matches!(
        settings.begin_preview(before.committed.revision),
        Err(SettingsError::Stale)
    ));
    settings
        .update_committed(recovered.committed.revision, (*recovered.committed).clone())
        .unwrap()
        .run()
        .unwrap();
}

#[test]
fn recovery_refuses_healthy_unsafe_and_busy_settings_without_quarantining() {
    use super::recovery::RecoveryError;
    let (settings, storage) = setup();
    assert_eq!(
        settings.recover_by_reset(),
        Err(RecoveryError::NotMalformed)
    );
    storage.0.lock().unwrap().failure = Some(StorageError::Unsafe);
    settings.reload().unwrap_err();
    assert!(!settings.snapshot().status.unwrap().is_malformed());
    assert_eq!(
        settings.recover_by_reset(),
        Err(RecoveryError::NotMalformed)
    );
    {
        let mut state = storage.0.lock().unwrap();
        state.failure = None;
        state.snapshot = Some((b"broken".to_vec(), 1));
    }
    settings.reload().unwrap_err();
    let token = settings.begin_preview(0).unwrap();
    assert_eq!(settings.recover_by_reset(), Err(RecoveryError::Busy));
    assert!(storage.0.lock().unwrap().backup.is_none());
    drop(token);
    settings.recover_by_reset().unwrap();
}

#[test]
fn recovery_preserves_backup_and_competing_file_on_conflict() {
    use super::recovery::RecoveryError;
    let storage = Arc::new(MemoryStorage::default());
    {
        let mut state = storage.0.lock().unwrap();
        state.snapshot = Some((b"broken".to_vec(), 1));
        state.successor_after_quarantine = true;
    }
    let settings = Settings::load(storage.clone());
    assert_eq!(
        settings.recover_by_reset(),
        Err(RecoveryError::Storage(StorageError::Conflict))
    );
    let state = storage.0.lock().unwrap();
    assert_eq!(state.backup.as_deref(), Some(b"broken".as_slice()));
    assert_eq!(state.snapshot.as_ref().unwrap().0, b"competing writer");
    assert_eq!(state.writes, 0);
    drop(state);
    assert_eq!(
        settings.snapshot().status,
        Some(SettingsError::Storage(StorageError::Conflict))
    );
    assert_eq!(
        settings.recover_by_reset(),
        Err(RecoveryError::NotMalformed)
    );
}

#[test]
fn recovery_refuses_committing_and_storage_ready_settings() {
    use super::recovery::RecoveryError;
    let (settings, storage) = setup();
    let candidate = (*settings.snapshot().committed).clone();
    let job = settings.update_committed(0, candidate).unwrap();
    assert_eq!(settings.recover_by_reset(), Err(RecoveryError::Busy));
    storage.0.lock().unwrap().failure = Some(StorageError::TooLarge);
    assert_eq!(
        job.run(),
        Err(SettingsError::Storage(StorageError::TooLarge))
    );
    assert!(settings.snapshot().status.unwrap().is_malformed());
    assert_eq!(
        settings.recover_by_reset(),
        Err(RecoveryError::NotMalformed)
    );
    assert!(storage.0.lock().unwrap().backup.is_none());
}

#[test]
fn recovery_stops_on_quarantine_failure_without_writing_defaults() {
    use super::recovery::RecoveryError;
    let storage = Arc::new(MemoryStorage::default());
    storage.0.lock().unwrap().snapshot = Some((b"broken".to_vec(), 1));
    let settings = Settings::load(storage.clone());
    storage.0.lock().unwrap().failure = Some(StorageError::Unsafe);
    assert_eq!(
        settings.recover_by_reset(),
        Err(RecoveryError::Storage(StorageError::Unsafe))
    );
    let state = storage.0.lock().unwrap();
    assert_eq!(state.writes, 0);
    assert!(state.backup.is_none());
    assert_eq!(state.snapshot.as_ref().unwrap().0, b"broken");
    assert!(!settings.snapshot().status.unwrap().is_malformed());
}

#[test]
fn following_the_file_adopts_an_outside_change_once() {
    let (settings, storage) = setup();
    settings
        .update_committed(0, SettingsDocument::default())
        .unwrap()
        .run()
        .unwrap();
    assert_eq!(settings.follow_file(), Ok(false));
    let before = settings.snapshot();

    let mut outside = SettingsDocument::default();
    outside.appearance.terminal.typography.base_size = 21.0;
    storage.0.lock().unwrap().snapshot =
        Some((export_settings(&outside).unwrap().into_bytes(), 40));

    assert_eq!(settings.follow_file(), Ok(true));
    let after = settings.snapshot();
    assert_eq!(after.committed.appearance, outside.appearance);
    assert!(after.committed.revision > before.committed.revision);
    assert!(after.catalog_revision > before.catalog_revision);
    assert_eq!(settings.follow_file(), Ok(false));
    assert_eq!(storage.0.lock().unwrap().writes, 1);
}

#[test]
fn following_a_malformed_file_keeps_the_settings_until_a_valid_one_arrives() {
    let (settings, storage) = setup();
    settings
        .update_committed(0, SettingsDocument::default())
        .unwrap()
        .run()
        .unwrap();
    let committed = settings.snapshot().committed;
    storage.0.lock().unwrap().snapshot = Some((b"{ half typed".to_vec(), 40));

    assert_eq!(settings.follow_file(), Err(SettingsError::Invalid));
    assert_eq!(*settings.snapshot().committed, *committed);
    assert_eq!(settings.snapshot().status, Some(SettingsError::Invalid));
    assert!(
        settings
            .update_committed(committed.revision, (*committed).clone())
            .is_err()
    );

    let mut fixed = SettingsDocument::default();
    fixed.appearance.terminal.typography.base_size = 19.0;
    storage.0.lock().unwrap().snapshot = Some((export_settings(&fixed).unwrap().into_bytes(), 41));
    assert_eq!(settings.follow_file(), Ok(true));
    assert_eq!(settings.snapshot().status, None);
    assert_eq!(settings.snapshot().committed.appearance, fixed.appearance);
}

#[test]
fn following_the_file_waits_for_a_live_preview() {
    let (settings, storage) = setup();
    let token = settings.begin_preview(0).unwrap();
    storage.0.lock().unwrap().snapshot = Some((
        export_settings(&SettingsDocument::default())
            .unwrap()
            .into_bytes(),
        40,
    ));

    assert_eq!(settings.follow_file(), Err(SettingsError::Busy));
    drop(token);
    assert_eq!(settings.follow_file(), Ok(true));
}

#[test]
fn ensuring_the_file_writes_only_a_document_no_file_holds() {
    let (settings, storage) = setup();

    settings
        .ensure_file()
        .unwrap()
        .expect("no file yet")
        .run()
        .unwrap();
    let written = storage
        .0
        .lock()
        .unwrap()
        .snapshot
        .clone()
        .expect("the file");
    assert_eq!(
        crate::settings::parse_settings(&written.0)
            .unwrap()
            .appearance,
        SettingsDocument::default().appearance
    );

    assert!(settings.ensure_file().unwrap().is_none());
    assert_eq!(storage.0.lock().unwrap().writes, 1);
}

#[test]
fn ensuring_the_file_leaves_an_unreadable_file_alone() {
    let storage = Arc::new(MemoryStorage::default());
    storage.0.lock().unwrap().snapshot = Some((b"{ broken".to_vec(), 7));
    let settings = Settings::load(storage.clone());

    assert!(settings.ensure_file().unwrap().is_none());
    assert_eq!(
        storage.0.lock().unwrap().snapshot.as_ref().unwrap().0,
        b"{ broken"
    );
}
