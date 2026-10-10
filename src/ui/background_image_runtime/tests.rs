use super::*;
use crate::background_image::BackgroundImageError;
use crate::background_image::testing::{OTHER_PNG, PNG, store};
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use gpui::TestAppContext;

fn commit(settings: &Settings, image: Option<BackgroundImageId>) {
    let committed = settings.snapshot().committed;
    let mut document = (*committed).clone();
    document.appearance.window.background_image = image;
    settings
        .update_committed(committed.revision, document)
        .unwrap()
        .run()
        .unwrap();
}

fn presented_bytes(cx: &mut TestAppContext) -> Option<Vec<u8>> {
    cx.update(|cx| presented(cx).map(|image| image.bytes.to_vec()))
}

#[gpui::test]
fn the_named_image_is_presented_and_a_copy_settings_stop_naming_is_discarded(
    cx: &mut TestAppContext,
) {
    let settings = Settings::load(Arc::new(MemoryStorage::default()));
    let store = Arc::new(store().0);
    let first = store.install(PNG).unwrap();
    let second = store.install(OTHER_PNG).unwrap();
    commit(&settings, Some(first));
    cx.update(|cx| install(settings.clone(), Arc::clone(&store), cx));
    cx.run_until_parked();
    assert_eq!(presented_bytes(cx).as_deref(), Some(PNG));

    commit(&settings, Some(second));
    cx.run_until_parked();
    assert_eq!(presented_bytes(cx).as_deref(), Some(OTHER_PNG));
    assert_eq!(store.load(first), Err(BackgroundImageError::Missing));

    commit(&settings, None);
    cx.run_until_parked();
    assert_eq!(presented_bytes(cx), None);
    assert_eq!(store.load(second), Err(BackgroundImageError::Missing));
}

#[gpui::test]
fn a_previewed_image_is_presented_before_it_is_saved(cx: &mut TestAppContext) {
    let settings = Settings::load(Arc::new(MemoryStorage::default()));
    let store = Arc::new(store().0);
    let id = store.install(PNG).unwrap();
    cx.update(|cx| install(settings.clone(), Arc::clone(&store), cx));
    let token = settings.begin_preview(0).unwrap();
    let mut document = SettingsDocument::default();
    document.appearance.window.background_image = Some(id);
    settings.update_preview(&token, document).unwrap();
    cx.run_until_parked();
    assert_eq!(presented_bytes(cx).as_deref(), Some(PNG));

    drop(token);
    cx.run_until_parked();
    assert_eq!(presented_bytes(cx), None);
    assert_eq!(
        store.load(id),
        Err(BackgroundImageError::Missing),
        "a copy only a cancelled preview named is discarded"
    );
}

#[gpui::test]
fn a_missing_copy_presents_nothing(cx: &mut TestAppContext) {
    let settings = Settings::load(Arc::new(MemoryStorage::default()));
    let store = Arc::new(store().0);
    let id = store.install(PNG).unwrap();
    store.discard(store.retire(id)).unwrap();
    commit(&settings, Some(id));
    cx.update(|cx| install(settings.clone(), Arc::clone(&store), cx));
    cx.run_until_parked();

    assert_eq!(presented_bytes(cx), None);
}

/// A choice may copy the very image the runtime is about to discard; nothing is discarded until
/// the choice ends, and then only what no Settings name.
#[gpui::test]
fn a_copy_is_kept_while_a_choice_is_in_progress(cx: &mut TestAppContext) {
    let settings = Settings::load(Arc::new(MemoryStorage::default()));
    let store = Arc::new(store().0);
    let first = store.install(PNG).unwrap();
    commit(&settings, Some(first));
    cx.update(|cx| install(settings.clone(), Arc::clone(&store), cx));
    cx.run_until_parked();

    cx.update(begin_choice);
    commit(&settings, None);
    cx.run_until_parked();
    assert!(store.load(first).is_ok(), "the choice may name it again");

    let kept = store.install(OTHER_PNG).unwrap();
    cx.update(|cx| end_choice(Some(kept), cx));
    cx.run_until_parked();
    assert_eq!(store.load(first), Err(BackgroundImageError::Missing));
    assert_eq!(
        store.load(kept),
        Err(BackgroundImageError::Missing),
        "a copy the Settings never named is discarded once the choice ends"
    );
}

/// Choosing the image the Settings already name restores a copy that went missing.
#[gpui::test]
fn choosing_the_named_image_again_restores_a_missing_copy(cx: &mut TestAppContext) {
    let settings = Settings::load(Arc::new(MemoryStorage::default()));
    let store = Arc::new(store().0);
    let id = store.install(PNG).unwrap();
    store.discard(store.retire(id)).unwrap();
    commit(&settings, Some(id));
    cx.update(|cx| install(settings.clone(), Arc::clone(&store), cx));
    cx.run_until_parked();
    assert_eq!(presented_bytes(cx), None);

    cx.update(begin_choice);
    assert_eq!(store.install(PNG), Ok(id));
    cx.update(|cx| end_choice(Some(id), cx));
    cx.run_until_parked();
    assert_eq!(presented_bytes(cx).as_deref(), Some(PNG));
}

/// An editor's draft may name a copy before the Settings do, while another write finishes.
#[gpui::test]
fn a_lease_keeps_a_copy_the_settings_do_not_name(cx: &mut TestAppContext) {
    let settings = Settings::load(Arc::new(MemoryStorage::default()));
    let store = Arc::new(store().0);
    let first = store.install(PNG).unwrap();
    commit(&settings, Some(first));
    cx.update(|cx| install(settings.clone(), Arc::clone(&store), cx));
    cx.run_until_parked();

    let drafted = store.install(OTHER_PNG).unwrap();
    let lease = cx.update(|cx| lease(drafted, cx)).unwrap();
    commit(&settings, None);
    cx.run_until_parked();
    assert_eq!(store.load(first), Err(BackgroundImageError::Missing));
    assert!(store.load(drafted).is_ok(), "the lease keeps it");

    drop(lease);
    commit(&settings, Some(first));
    cx.run_until_parked();
    assert_eq!(
        store.load(drafted),
        Err(BackgroundImageError::Missing),
        "the next change discards a copy nothing names any longer"
    );
}

/// Naming a copy again, as an import or an edit to the settings file can, keeps it even while
/// its discard is still waiting to run.
#[gpui::test]
fn a_copy_named_again_before_its_discard_runs_is_kept(cx: &mut TestAppContext) {
    let settings = Settings::load(Arc::new(MemoryStorage::default()));
    let store = Arc::new(store().0);
    let id = store.install(PNG).unwrap();
    commit(&settings, Some(id));
    cx.update(|cx| install(settings.clone(), Arc::clone(&store), cx));
    cx.run_until_parked();

    commit(&settings, None);
    cx.update(|cx| sync(false, cx));
    commit(&settings, Some(id));
    cx.update(|cx| sync(false, cx));
    cx.run_until_parked();
    assert!(store.load(id).is_ok());
    assert_eq!(presented_bytes(cx).as_deref(), Some(PNG));
}

/// A discard that lapses because another image was copied meanwhile is decided again, so the copy
/// nothing names is not left behind.
#[gpui::test]
fn a_lapsed_discard_is_tried_again(cx: &mut TestAppContext) {
    let settings = Settings::load(Arc::new(MemoryStorage::default()));
    let store = Arc::new(store().0);
    let id = store.install(PNG).unwrap();
    commit(&settings, Some(id));
    cx.update(|cx| install(settings.clone(), Arc::clone(&store), cx));
    cx.run_until_parked();

    commit(&settings, None);
    cx.update(|cx| sync(false, cx));
    let other = store.install(OTHER_PNG).unwrap();
    cx.run_until_parked();
    assert_eq!(store.load(id), Err(BackgroundImageError::Missing));
    assert!(store.load(other).is_ok(), "nothing owned this copy yet");
}
