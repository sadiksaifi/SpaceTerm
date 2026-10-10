use super::testing::{OTHER_PNG, PNG, store};
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

    assert_eq!(store.discard(id), Ok(()));

    assert_eq!(store.load(id), Err(BackgroundImageError::Missing));
    assert_eq!(store.discard(id), Ok(()));
    assert_eq!(&*store.load(kept).unwrap(), OTHER_PNG);
}

#[test]
fn nothing_is_read_before_an_image_was_ever_installed() {
    let (store, filesystem) = store();
    let id = BackgroundImageId::of(PNG);

    assert_eq!(store.load(id), Err(BackgroundImageError::Missing));
    assert_eq!(store.discard(id), Ok(()));
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
