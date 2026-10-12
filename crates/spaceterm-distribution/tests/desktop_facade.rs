//! Compile and execute the actual desktop facade without importing desktop dependencies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UpdateError {
    Verification,
}

#[path = "../../../src/updates/release_feed.rs"]
mod facade;

#[test]
fn desktop_facade_preserves_portable_limits_and_typed_errors() {
    #[cfg(target_os = "linux")]
    assert_eq!(
        (
            facade::MAX_FEED_BYTES,
            facade::MAX_FEED_SIGNATURE_BYTES,
            facade::MAX_ARCHIVE_BYTES
        ),
        (4096, 256, 512 * 1024 * 1024)
    );
    assert_eq!(
        facade::ReleaseFeed::verify(
            &facade::testing::ReleaseSigner::new(7).key(),
            facade::ReleasePlatform::new("linux-x86_64"),
            b"malformed",
            b"malformed",
            1_800_000_000
        ),
        Err(UpdateError::Verification)
    );
}
