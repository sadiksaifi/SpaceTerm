use super::testing::{OTHER_PNG, PNG, store, store_inspecting};
use super::*;

#[test]
fn an_installed_image_loads_back_under_its_digest() {
    let (store, _) = store();

    let id = store.install(PNG).unwrap();

    assert_eq!(&*store.load(id).unwrap(), PNG);
    assert_eq!(store.install(PNG).unwrap(), id);
    assert_ne!(store.install(OTHER_PNG).unwrap(), id);
    assert_eq!(&*store.load(id).unwrap(), PNG);
}

#[test]
fn each_supported_format_is_copied() {
    let (store, _) = store();
    for bytes in [
        PNG,
        b"\xff\xd8\xff\xe0\0\x10JFIF".as_slice(),
        b"RIFF\0\0\0\0WEBPVP8 ".as_slice(),
        b"\0\0\0\x18ftypheic\0\0\0\0".as_slice(),
    ] {
        assert!(store.install(bytes).is_ok());
    }
}

#[test]
fn a_file_that_is_not_a_supported_image_is_refused_without_a_copy() {
    let (store, filesystem) = store();

    assert_eq!(
        store.install(b"GIF89a\x01\0\x01\0"),
        Err(BackgroundImageError::UnsupportedFormat)
    );
    let mut oversized = PNG.to_vec();
    oversized.resize(MAXIMUM_BYTES + 1, 0);
    assert_eq!(
        store.install(&oversized),
        Err(BackgroundImageError::TooLarge)
    );
    assert_eq!(filesystem.files.lock().unwrap().prepare_count, 0);
}

#[test]
fn a_copy_whose_bytes_changed_is_not_presented() {
    let (store, filesystem) = store();
    let id = store.install(PNG).unwrap();
    for (bytes, _) in filesystem.files.lock().unwrap().values.values_mut() {
        bytes.push(0);
    }

    assert_eq!(store.load(id), Err(BackgroundImageError::Missing));
}

#[test]
fn a_discarded_copy_is_gone_and_discarding_it_again_succeeds() {
    let (store, _) = store();
    let kept = store.install(OTHER_PNG).unwrap();
    let id = store.install(PNG).unwrap();

    assert_eq!(store.discard(store.retire(id)), Ok(Discarded::Removed));

    assert_eq!(store.load(id), Err(BackgroundImageError::Missing));
    assert_eq!(store.discard(store.retire(id)), Ok(Discarded::Removed));
    assert_eq!(&*store.load(kept).unwrap(), OTHER_PNG);
}

#[test]
fn nothing_is_read_before_an_image_was_ever_installed() {
    let (store, filesystem) = store();
    let id = BackgroundImageId::of(PNG);

    assert_eq!(store.load(id), Err(BackgroundImageError::Missing));
    assert_eq!(store.discard(store.retire(id)), Ok(Discarded::Removed));
    assert!(filesystem.events.lock().unwrap().is_empty());
}

#[test]
fn the_digest_round_trips_as_lowercase_hexadecimal_only() {
    let id = BackgroundImageId::of(PNG);
    let text = serde_json::to_string(&id).unwrap();

    assert_eq!(text.len(), 66);
    assert_eq!(
        serde_json::from_str::<BackgroundImageId>(&text).unwrap(),
        id
    );
    for invalid in [
        text.to_uppercase(),
        "\"abc\"".to_owned(),
        format!("\"{}g\"", "0".repeat(63)),
    ] {
        assert!(serde_json::from_str::<BackgroundImageId>(&invalid).is_err());
    }
}

#[test]
fn an_image_the_system_cannot_open_is_refused_without_a_copy() {
    let (store, filesystem) = store_inspecting(None);

    assert_eq!(
        store.install(PNG),
        Err(BackgroundImageError::UnsupportedFormat)
    );
    assert_eq!(filesystem.files.lock().unwrap().prepare_count, 0);
}

#[test]
fn an_image_with_more_pixels_than_a_large_photo_is_refused() {
    let (store, filesystem) = store_inspecting(Some((16_384, 16_384)));
    assert_eq!(store.install(PNG), Err(BackgroundImageError::TooManyPixels));
    assert_eq!(filesystem.files.lock().unwrap().prepare_count, 0);

    let (store, _) = store_inspecting(Some((8_064, 6_048)));
    assert!(store.install(PNG).is_ok());
}

/// Choosing an image again while its earlier copy waits to be discarded keeps the copy.
#[test]
fn a_copy_chosen_again_after_its_retirement_is_kept() {
    let (store, _) = store();
    let id = store.install(PNG).unwrap();
    let retirement = store.retire(id);

    assert_eq!(store.install(PNG), Ok(id));
    assert_eq!(store.discard(retirement), Ok(Discarded::Lapsed));
    assert_eq!(&*store.load(id).unwrap(), PNG);

    assert_eq!(store.discard(store.retire(id)), Ok(Discarded::Removed));
    assert_eq!(store.load(id), Err(BackgroundImageError::Missing));
}

/// Settings that name a copy again, as an import or an edit to the settings file can, keep it.
#[test]
fn a_copy_named_again_after_its_retirement_is_kept() {
    let (store, _) = store();
    let id = store.install(PNG).unwrap();
    let retirement = store.retire(id);

    store.renew();

    assert_eq!(store.discard(retirement), Ok(Discarded::Lapsed));
    assert_eq!(&*store.load(id).unwrap(), PNG);
}
