//! Desktop-compatible facade over the portable signed release reader.
#![cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "only Linux installs client archives")
)]

use super::UpdateError;
pub(crate) use client_feed::ReleasePlatform;
#[cfg(target_os = "linux")]
pub(crate) use client_feed::{MAX_ARCHIVE_BYTES, MAX_FEED_BYTES, MAX_FEED_SIGNATURE_BYTES};
pub(crate) use spaceterm_distribution::UpdateKey;
use spaceterm_distribution::client_feed;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReleaseFeed {
    pub(crate) version: String,
    pub(crate) published_at: u64,
    pub(crate) archive: ReleaseArchive,
    verified: client_feed::ReleaseFeed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReleaseArchive {
    pub(crate) url: String,
    pub(crate) size: u64,
    verified: client_feed::ReleaseArchive,
}

impl ReleaseFeed {
    pub(crate) fn verify(
        key: &UpdateKey,
        platform: ReleasePlatform,
        feed: &[u8],
        signature: &[u8],
        now: u64,
    ) -> Result<Self, UpdateError> {
        let verified = client_feed::ReleaseFeed::verify(key, platform, feed, signature, now)
            .map_err(|_| UpdateError::Verification)?;
        Ok(Self {
            version: verified.version.clone(),
            published_at: verified.published_at,
            archive: ReleaseArchive {
                url: verified.archive.url.clone(),
                size: verified.archive.size,
                verified: verified.archive.clone(),
            },
            verified,
        })
    }

    pub(crate) fn supersedes(&self, current: &str) -> bool {
        self.verified.supersedes(current)
    }
}

impl ReleaseArchive {
    pub(crate) fn verify(&self, key: &UpdateKey, bytes: &[u8]) -> Result<(), UpdateError> {
        self.verified
            .verify(key, bytes)
            .map_err(|_| UpdateError::Verification)
    }
}

#[cfg(test)]
use base64::{Engine as _, engine::general_purpose::STANDARD};
#[cfg(test)]
use sha2::{Digest as _, Sha256};

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use ring::signature::KeyPair as _;

    /// Signs feeds and archives with a fixed test key, never the release key.
    pub(crate) struct ReleaseSigner(ring::signature::Ed25519KeyPair);

    impl ReleaseSigner {
        pub(crate) fn new(seed: u8) -> Self {
            Self(ring::signature::Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
        }

        pub(crate) fn key(&self) -> UpdateKey {
            UpdateKey::from_base64(&STANDARD.encode(self.0.public_key().as_ref())).unwrap()
        }

        pub(crate) fn sign(&self, message: &[u8]) -> String {
            STANDARD.encode(self.0.sign(message).as_ref())
        }

        /// The feed and detached signature a release publishes for `archive`.
        pub(crate) fn feed(
            &self,
            platform: &str,
            version: &str,
            published_at: u64,
            archive: &[u8],
        ) -> (Vec<u8>, Vec<u8>) {
            let digest = Sha256::digest(archive);
            let feed = serde_json::json!({
                "version": version,
                "published_at": published_at,
                "archive": {
                    "name": format!("SpaceTerm-{version}-{platform}.tar.gz"),
                    "size": archive.len(),
                    "sha256": digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
                    "signature": self.sign(archive),
                },
            })
            .to_string()
            .into_bytes();
            let signature = format!("{}\n", self.sign(&feed)).into_bytes();
            (feed, signature)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::ReleaseSigner;
    use super::*;

    const PLATFORM: ReleasePlatform = ReleasePlatform::new("linux-x86_64");
    const NOW: u64 = 1_800_000_000;

    fn verified(feed: &[u8], signature: &[u8]) -> Result<ReleaseFeed, UpdateError> {
        ReleaseFeed::verify(&ReleaseSigner::new(7).key(), PLATFORM, feed, signature, NOW)
    }

    #[test]
    fn release_key_should_come_from_the_release_bundle_template() {
        let plist = include_str!("../../packaging/macos/spaceterm/Info.plist");
        let key = UpdateKey::release().expect("the release template carries an update key");
        let (_, value) = plist.split_once("<key>SUPublicEDKey</key>").unwrap();
        let value = value
            .trim_start()
            .strip_prefix("<string>")
            .unwrap()
            .split_once("</string>")
            .unwrap()
            .0;
        assert!(UpdateKey::from_base64(value).is_some());
        let _ = key;
    }

    #[test]
    fn signed_feed_should_name_its_tagged_archive() {
        let signer = ReleaseSigner::new(7);
        let (feed, signature) = signer.feed("linux-x86_64", "1.2.3", NOW - 60, b"archive");
        let feed = verified(&feed, &signature).unwrap();
        assert_eq!(feed.version, "1.2.3");
        assert_eq!(feed.published_at, NOW - 60);
        assert_eq!(
            feed.archive.url,
            "https://github.com/sadiksaifi/SpaceTerm/releases/download/v1.2.3/SpaceTerm-1.2.3-linux-x86_64.tar.gz"
        );
        assert_eq!(feed.archive.verify(&signer.key(), b"archive"), Ok(()));
        assert!(feed.supersedes("1.2.2"));
        assert!(!feed.supersedes("1.2.3"));
        assert!(!feed.supersedes("dev.abc1234"));
    }

    #[test]
    fn feed_signed_by_another_key_should_be_rejected() {
        let (feed, signature) = ReleaseSigner::new(8).feed("linux-x86_64", "1.2.3", NOW, b"a");
        assert_eq!(verified(&feed, &signature), Err(UpdateError::Verification));
    }

    #[test]
    fn altered_feed_should_be_rejected() {
        let (feed, signature) = ReleaseSigner::new(7).feed("linux-x86_64", "1.2.3", NOW, b"a");
        let altered = String::from_utf8(feed).unwrap().replace("1.2.3", "1.2.4");
        assert_eq!(
            verified(altered.as_bytes(), &signature),
            Err(UpdateError::Verification)
        );
    }

    #[test]
    fn signed_feed_for_another_platform_or_future_should_be_rejected() {
        let signer = ReleaseSigner::new(7);
        for (platform, published) in [("darwin-arm64", NOW), ("linux-x86_64", NOW + 3600)] {
            let (feed, signature) = signer.feed(platform, "1.2.3", published, b"a");
            assert_eq!(verified(&feed, &signature), Err(UpdateError::Verification));
        }
    }

    #[test]
    fn archive_with_a_different_body_should_be_rejected() {
        let signer = ReleaseSigner::new(7);
        let (feed, signature) = signer.feed("linux-x86_64", "1.2.3", NOW, b"archive");
        let feed = verified(&feed, &signature).unwrap();
        assert_eq!(
            feed.archive.verify(&signer.key(), b"archivE"),
            Err(UpdateError::Verification)
        );
        assert_eq!(
            feed.archive
                .verify(&ReleaseSigner::new(8).key(), b"archive"),
            Err(UpdateError::Verification)
        );
    }
}
