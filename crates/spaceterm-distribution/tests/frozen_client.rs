//! The exact shipped reader blob 1c3209928b99624907209cccdc1120babac27727.
//! Every tag v0.5.0 through v0.6.7 carries these same reader bytes.
use spaceterm_distribution::client_feed;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UpdateError {
    Verification,
}
#[path = "fixtures/shipped-v0.5.0-through-v0.6.7/stable_version.rs"]
mod frozen_version;
use frozen_version::stable_version;
#[path = "fixtures/shipped-v0.5.0-through-v0.6.7/src/updates/release_feed.rs"]
mod frozen;

#[test]
fn frozen_shipped_reader_and_current_reader_accept_the_same_exact_client_feed() {
    let signer = frozen::testing::ReleaseSigner::new(7);
    let platform = frozen::ReleasePlatform::new("linux-x86_64");
    let current_platform = client_feed::ReleasePlatform::new("linux-x86_64");
    let current_signer = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ring::signature::KeyPair as _;
    let key = spaceterm_distribution::UpdateKey::from_base64(
        &STANDARD.encode(current_signer.public_key().as_ref()),
    )
    .unwrap();
    let (feed, signature) = signer.feed("linux-x86_64", "1.2.3", 1_800_000_000, b"archive");
    let retained =
        frozen::ReleaseFeed::verify(&signer.key(), platform, &feed, &signature, 1_800_000_000)
            .unwrap();
    let extracted =
        client_feed::ReleaseFeed::verify(&key, current_platform, &feed, &signature, 1_800_000_000)
            .unwrap();
    assert_eq!(retained.version, extracted.version);
    assert_eq!(retained.archive.url, extracted.archive.url);
    assert_eq!(retained.archive.size, extracted.archive.size);
    assert!(retained.archive.verify(&signer.key(), b"archive").is_ok());
    assert!(extracted.archive.verify(&key, b"archive").is_ok());
    assert_eq!(
        (
            frozen::MAX_FEED_BYTES,
            frozen::MAX_FEED_SIGNATURE_BYTES,
            frozen::MAX_ARCHIVE_BYTES
        ),
        (
            client_feed::MAX_FEED_BYTES,
            client_feed::MAX_FEED_SIGNATURE_BYTES,
            client_feed::MAX_ARCHIVE_BYTES
        )
    );
    assert_eq!(platform.feed_url(), current_platform.feed_url());
    assert_eq!(
        platform.feed_signature_url(),
        current_platform.feed_signature_url()
    );
    for len in [4096, 4097] {
        let mut padded = feed.clone();
        padded.resize(len, b' ');
        let signature = signer.sign(&padded);
        assert_eq!(
            frozen::ReleaseFeed::verify(
                &signer.key(),
                platform,
                &padded,
                signature.as_bytes(),
                1_800_000_000
            )
            .is_ok(),
            client_feed::ReleaseFeed::verify(
                &key,
                current_platform,
                &padded,
                signature.as_bytes(),
                1_800_000_000
            )
            .is_ok()
        );
    }
}

#[test]
fn frozen_shipped_reader_rejects_server_feed_and_server_archive_names() {
    let signer = frozen::testing::ReleaseSigner::new(7);
    let (feed, _) = signer.feed("linux-x86_64", "1.2.3", 1_800_000_000, b"archive");
    let feed = String::from_utf8(feed)
        .unwrap()
        .replace("SpaceTerm-1.2.3", "SpaceTerm-Server-1.2.3");
    assert!(
        frozen::ReleaseFeed::verify(
            &signer.key(),
            frozen::ReleasePlatform::new("linux-x86_64"),
            feed.as_bytes(),
            signer.sign(feed.as_bytes()).as_bytes(),
            1_800_000_000
        )
        .is_err()
    );
}
