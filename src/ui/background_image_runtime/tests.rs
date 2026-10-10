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
    // Cancelling a preview never touches the committed Settings, so the copy stays.
    assert!(store.load(id).is_ok());
}

#[gpui::test]
fn a_missing_copy_presents_nothing(cx: &mut TestAppContext) {
    let settings = Settings::load(Arc::new(MemoryStorage::default()));
    let store = Arc::new(store().0);
    let id = store.install(PNG).unwrap();
    store.discard(id).unwrap();
    commit(&settings, Some(id));
    cx.update(|cx| install(settings.clone(), Arc::clone(&store), cx));
    cx.run_until_parked();

    assert_eq!(presented_bytes(cx), None);
}
