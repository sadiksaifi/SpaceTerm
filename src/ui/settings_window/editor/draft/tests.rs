use std::{collections::BTreeSet, sync::Arc};

use crate::appearance::{AppearanceDocument, ChromeDensity, ResetTarget, SchemeKind};
use crate::settings::storage::StorageError;
use crate::settings::{PreviewPhase, SchemeImport, SettingsError, UserSettings};
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
    // The storage write has finished, but its scheduling Adapter has not delivered completion.
    draft.prepare_commit().unwrap().run().unwrap();

    assert!(draft.edit(|document| {
        document.preferences.terminal.typography.base_size = 21.0;
    }));

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
    let result = draft.prepare_commit().unwrap().run();

    assert!(draft.settle(false, result));

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
    assert_eq!(draft.status(), SaveStatus::Saving);
    assert!(draft.has_unwritten_changes());
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

    let result = draft.prepare_commit().unwrap().run();
    assert!(!draft.settle(true, result));
    assert_eq!(draft.status(), SaveStatus::Saved);

    draft.remove_custom_scheme(id).unwrap();
    assert_eq!(draft.status(), SaveStatus::Saving);
    assert!(draft.has_unwritten_changes());
    let result = draft.prepare_commit().unwrap().run();
    assert!(!draft.settle(true, result));
    let retained = storage.document().unwrap();
    assert!(retained.custom_schemes.is_empty());
    assert_eq!(retained.preferences, preferences);
}

#[test]
fn a_successful_edit_owns_its_unwritten_state_before_scheduling() {
    let (mut draft, settings, storage) = setup();

    assert!(draft.edit(|document| {
        document.preferences.chrome.density = ChromeDensity::Comfortable;
    }));

    assert_eq!(draft.status(), SaveStatus::Saving);
    assert!(draft.has_unwritten_changes());
    assert_eq!(settings.snapshot().phase, PreviewPhase::Previewing);
    assert_eq!(storage.writes(), 0);
    assert!(!draft.edit(|_| {}));
    assert_eq!(draft.status(), SaveStatus::Saving);
}

#[test]
fn unchanged_or_rejected_edits_preserve_a_save_failure_until_retry_finishes() {
    let (mut draft, _, storage) = setup();
    assert!(draft.edit(|document| {
        document.preferences.chrome.density = ChromeDensity::Comfortable;
    }));
    storage.fail_writes(Some(StorageError::Unavailable));
    let result = draft.prepare_commit().unwrap().run();
    assert!(!draft.settle(true, result));
    let failed = SaveStatus::Failed(SettingsError::Storage(StorageError::Unavailable));
    assert_eq!(draft.status(), failed);

    assert!(!draft.edit(|_| {}));
    assert!(
        draft
            .import(SchemeImport::SpaceTerm(b"invalid"), &BTreeSet::new())
            .is_err()
    );
    assert_eq!(draft.status(), failed);
    assert!(draft.has_unwritten_changes());

    storage.fail_writes(None);
    let retry = draft.prepare_commit().unwrap();
    assert_eq!(draft.status(), failed);
    assert!(!draft.settle(true, retry.run()));
    assert_eq!(draft.status(), SaveStatus::Saved);
}

#[test]
fn busy_commit_preparation_requeues_a_failed_draft() {
    let (mut draft, _, storage) = setup();
    assert!(draft.edit(|document| {
        document.preferences.chrome.density = ChromeDensity::Comfortable;
    }));
    storage.fail_writes(Some(StorageError::Unavailable));
    let result = draft.prepare_commit().unwrap().run();
    assert!(!draft.settle(true, result));
    storage.fail_writes(None);
    let retry = draft.prepare_commit().unwrap();

    assert!(matches!(draft.prepare_commit(), Err(SettingsError::Busy)));

    assert_eq!(draft.status(), SaveStatus::Saving);
    assert!(draft.has_unwritten_changes());
    assert!(!draft.settle(true, retry.run()));
    assert_eq!(draft.status(), SaveStatus::Saved);
}

#[test]
fn resynchronization_after_failure_owns_its_unwritten_state() {
    let (mut draft, _, storage) = setup();
    assert!(draft.edit(|document| {
        document.preferences.chrome.density = ChromeDensity::Comfortable;
    }));
    let in_flight = draft.prepare_commit().unwrap();
    assert!(draft.edit(|document| {
        document.preferences.terminal.typography.base_size = 21.0;
    }));
    storage.fail_writes(Some(StorageError::Unavailable));

    assert!(draft.settle(false, in_flight.run()));

    assert_eq!(draft.status(), SaveStatus::Saving);
    assert!(draft.has_unwritten_changes());
    storage.fail_writes(None);
    let result = draft.prepare_commit().unwrap().run();
    assert!(!draft.settle(true, result));
    assert_eq!(
        storage
            .document()
            .unwrap()
            .preferences
            .terminal
            .typography
            .base_size,
        21.0
    );
}

#[test]
fn a_completed_write_preserves_the_edit_made_while_storage_was_blocked() {
    let (mut draft, settings, storage) = setup();
    draft.edit(|document| document.preferences.chrome.density = ChromeDensity::Comfortable);
    let blocked = storage.block_next_write();
    let first = draft.prepare_commit().unwrap();
    let worker = std::thread::spawn(move || first.run());
    blocked.wait_until_started();
    draft.edit(|document| document.preferences.terminal.typography.base_size = 21.0);
    blocked.release();
    let result = worker.join().unwrap();

    assert!(draft.settle(false, result));
    assert_eq!(
        draft.document().preferences.terminal.typography.base_size,
        21.0
    );
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
}

#[test]
fn opening_during_a_foreign_preview_never_adopts_its_canceled_values() {
    let (_, settings, storage) = setup();
    let committed = settings.snapshot().committed;
    let foreign = settings.begin_preview(committed.revision).unwrap();
    let mut candidate = (*committed).clone();
    candidate.preferences.chrome.density = ChromeDensity::Comfortable;
    settings.update_preview(&foreign, candidate).unwrap();
    let mut draft = SettingsDraft::new(settings.clone());
    drop(foreign);
    draft.synchronize();
    draft.edit(|document| document.preferences.terminal.typography.base_size = 21.0);
    let result = draft.prepare_commit().unwrap().run();
    draft.settle(true, result);
    assert_eq!(
        storage.document().unwrap().preferences.chrome.density,
        ChromeDensity::Compact
    );
}

#[test]
fn exporting_while_another_writer_is_busy_uses_the_authoritative_draft() {
    let (mut draft, settings, _) = setup();
    let committed = settings.snapshot().committed;
    let competing = settings
        .update_committed(committed.revision, (*committed).clone())
        .unwrap();
    draft.edit(|document| document.preferences.terminal.typography.base_size = 21.0);
    let exported = draft.export_document().unwrap();
    assert_eq!(
        crate::appearance::parse_settings(exported.as_bytes())
            .unwrap()
            .preferences
            .terminal
            .typography
            .base_size,
        21.0
    );
    drop(competing);
}
