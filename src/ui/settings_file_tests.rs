use std::{rc::Rc, sync::Arc};

use gpui::TestAppContext;

use super::*;
use crate::appearance::SettingsDocument;
use crate::platform::settings_file::testing::RecordingSettingsFile;
use crate::ui::settings_window::test_support::MemoryStorage;

fn document_with_size(size: f32) -> SettingsDocument {
    let mut document = SettingsDocument::default();
    document.preferences.terminal.typography.base_size = size;
    document
}

fn install(
    storage: &Arc<MemoryStorage>,
    file: &Rc<RecordingSettingsFile>,
    cx: &mut TestAppContext,
) -> UserSettings {
    let settings = UserSettings::load(storage.clone());
    let access: Rc<dyn SettingsFileAccess> = file.clone();
    cx.update(|cx| SettingsFile::install(settings.clone(), access, cx));
    settings
}

fn base_size(settings: &UserSettings) -> f32 {
    settings
        .snapshot()
        .committed
        .preferences
        .terminal
        .typography
        .base_size
}

#[gpui::test]
fn a_save_elsewhere_reaches_the_settings_once_the_file_settles(cx: &mut TestAppContext) {
    let storage = MemoryStorage::with_document(&document_with_size(13.0));
    let file = RecordingSettingsFile::watchable();
    let settings = install(&storage, &file, cx);

    storage.save_elsewhere(&document_with_size(17.0));
    file.announce_change();
    cx.run_until_parked();
    assert_eq!(base_size(&settings), 13.0);

    cx.executor().advance_clock(SETTLE_DELAY);
    cx.run_until_parked();
    assert_eq!(base_size(&settings), 17.0);
    assert_eq!(storage.writes(), 0);
}

#[gpui::test]
fn a_save_elsewhere_waits_for_a_live_preview_to_end(cx: &mut TestAppContext) {
    let storage = MemoryStorage::with_document(&document_with_size(13.0));
    let file = RecordingSettingsFile::watchable();
    let settings = install(&storage, &file, cx);
    let revision = settings.snapshot().committed.revision;
    let preview = settings.begin_preview(revision).unwrap();

    storage.save_elsewhere(&document_with_size(17.0));
    file.announce_change();
    cx.executor().advance_clock(SETTLE_DELAY);
    cx.run_until_parked();
    assert_eq!(base_size(&settings), 13.0);

    drop(preview);
    cx.run_until_parked();
    assert_eq!(base_size(&settings), 17.0);
}

#[gpui::test]
fn opening_the_file_starts_a_watch_that_could_not_start_at_launch(cx: &mut TestAppContext) {
    let storage = Arc::new(MemoryStorage::default());
    let file = Rc::new(RecordingSettingsFile::default());
    install(&storage, &file, cx);
    assert!(!file.is_watched());

    file.watchable.set(true);
    assert!(cx.update(SettingsFile::open));

    assert_eq!(file.opened.get(), 1);
    assert!(file.is_watched());
    assert_eq!(
        cx.update(|cx| SettingsFile::location(cx)).as_deref(),
        Some("~/.config/spaceterm/settings.json")
    );
}
