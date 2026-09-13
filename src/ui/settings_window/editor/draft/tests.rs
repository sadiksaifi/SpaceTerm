use std::{collections::BTreeSet, sync::Arc};

use crate::appearance::{AppearanceDocument, ChromeDensity, ResetTarget, SchemeKind};
use crate::settings::{PreviewPhase, SchemeImport, UserSettings};
use crate::ui::settings_window::test_support::MemoryStorage;

use super::{SaveStatus, SettingsDraft};

fn setup() -> (SettingsDraft, UserSettings, Arc<MemoryStorage>) {
    let storage = MemoryStorage::with_document(&AppearanceDocument::default());
    let (settings, _) = UserSettings::load(storage.clone());
    (SettingsDraft::new(settings.clone()), settings, storage)
}

#[test]
fn an_unchanged_edit_does_not_acquire_a_preview_or_request_a_write() {
    let (mut draft, settings, storage) = setup();

    assert!(!draft.edit(|_| {}));
    assert!(!draft.reset(ResetTarget::AllAppearance));

    assert_eq!(settings.snapshot().phase, PreviewPhase::Idle);
    assert_eq!(draft.status(), SaveStatus::Saved);
    assert!(!draft.has_unwritten_changes());
    assert_eq!(storage.writes(), 0);
}

#[test]
fn an_idle_draft_adopts_another_surfaces_commit() {
    let (mut draft, settings, _) = setup();
    let original = settings.snapshot().committed;
    let mut document = (*original).clone();
    document.preferences.chrome.density = ChromeDensity::Comfortable;
    settings
        .update_committed(original.revision, document)
        .unwrap()
        .run()
        .unwrap();

    draft.synchronize();

    assert_eq!(draft.document(), settings.snapshot().committed.as_ref());
    assert_eq!(draft.status(), SaveStatus::Saved);
}

#[test]
fn synchronization_keeps_a_draft_that_was_blocked_by_another_writer() {
    let (mut draft, settings, storage) = setup();
    let original = settings.snapshot().committed;
    let competing = settings
        .update_committed(original.revision, (*original).clone())
        .unwrap();
    assert!(draft.edit(|document| {
        document.preferences.chrome.density = ChromeDensity::Comfortable;
    }));
    draft.mark_saving();
    competing.run().unwrap();

    draft.synchronize();

    assert_eq!(
        draft.document().preferences.chrome.density,
        ChromeDensity::Comfortable
    );
    let result = draft.prepare_commit().unwrap().run();
    assert!(!draft.settle(true, result));
    assert_eq!(
        storage.document().unwrap().preferences.chrome.density,
        ChromeDensity::Comfortable
    );
    assert_eq!(draft.status(), SaveStatus::Saved);
}

#[test]
fn an_edit_after_publication_reacquires_the_retired_preview() {
    let (mut draft, settings, storage) = setup();
    assert!(draft.edit(|document| {
        document.preferences.chrome.density = ChromeDensity::Comfortable;
    }));
    draft.mark_saving();
    // The storage write has finished, but its scheduling Adapter has not delivered completion.
    draft.prepare_commit().unwrap().run().unwrap();

    assert!(draft.edit(|document| {
        document.preferences.terminal.typography.base_size = 21.0;
    }));
    draft.mark_saving();

    assert_eq!(
        settings
            .snapshot()
            .candidate
            .preferences
            .terminal
            .typography
            .base_size,
        21.0
    );
    let result = draft.prepare_commit().unwrap().run();
    assert!(!draft.settle(true, result));
    let retained = storage.document().unwrap();
    assert_eq!(
        retained.preferences.chrome.density,
        ChromeDensity::Comfortable
    );
    assert_eq!(retained.preferences.terminal.typography.base_size, 21.0);
    assert_eq!(draft.status(), SaveStatus::Saved);
}

#[test]
fn an_obsolete_completion_cannot_report_the_current_edit_saved() {
    let (mut draft, _, _) = setup();
    assert!(draft.edit(|document| {
        document.preferences.chrome.density = ChromeDensity::Comfortable;
    }));
    draft.mark_saving();
    let result = draft.prepare_commit().unwrap().run();

    assert!(!draft.settle(false, result));

    assert_eq!(draft.status(), SaveStatus::Saving);
}

#[test]
fn import_and_removal_share_the_draft_without_selecting_a_scheme() {
    let (mut draft, _, storage) = setup();
    let preferences = draft.document().preferences.clone();
    let source = br##"{"schema_version":1,"schemes":[{"kind":"chrome","id":"custom.sample","name":"Sample","appearance":"light","colors":{"text":"#112233"}}]}"##;
    let receipt = draft
        .import(SchemeImport::SpaceTerm(source), &BTreeSet::new())
        .unwrap();
    draft.mark_saving();
    let id = &receipt.installed[0];

    assert!(
        draft
            .scheme_summaries(SchemeKind::Chrome)
            .unwrap()
            .iter()
            .any(|scheme| &scheme.id == id)
    );
    let exported = draft.export_document().unwrap();
    assert_eq!(
        crate::appearance::parse_settings(exported.as_bytes()).unwrap(),
        *draft.document()
    );
    assert_eq!(draft.document().preferences, preferences);
    assert_eq!(storage.writes(), 0);

    draft.remove_custom_scheme(id).unwrap();
    let result = draft.prepare_commit().unwrap().run();
    assert!(!draft.settle(true, result));
    let retained = storage.document().unwrap();
    assert!(retained.custom_schemes.is_empty());
    assert_eq!(retained.preferences, preferences);
}
