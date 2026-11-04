//! The signed feed and archive that self-installed Linux desktops update from.
//!
//! A release publishes `spaceterm-desktop-linux-x86_64.json` beside a `.sig` of 64 raw Ed25519
//! bytes. [`feed::read`] verifies that signature under the current or the backup key of
//! `packaging/update-trust.json` before it reads any field, then holds the feed to every rule of
//! its schema. The archive the feed names grants nothing until its size, digest and own signature
//! verify under the same keys.
#![cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "only the Linux updater installs release archives itself"
    )
)]

use sha2::{Digest as _, Sha256};
use spaceterm_core::update::feed::{self, FeedTarget};
use spaceterm_core::update::trust::{SIGNATURE_BYTES, UpdateTrust};

use super::{UpdateError, stable_version};

pub(crate) const MAX_FEED_BYTES: usize = feed::MAX_FEED;
pub(crate) const MAX_FEED_SIGNATURE_BYTES: usize = SIGNATURE_BYTES;
pub(crate) const MAX_ARCHIVE_BYTES: u64 = feed::MAX_ARCHIVE;

/// The keys this build trusts. None while `packaging/update-trust.json` holds no keys, which
/// leaves the updater unavailable rather than trusting anything.
pub(crate) fn release_trust() -> Option<UpdateTrust> {
    UpdateTrust::release().ok()
}

/// A verified feed: the newest release and the archive that installs it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReleaseFeed {
    pub(crate) version: String,
    pub(crate) published_at: u64,
    pub(crate) archive: ReleaseArchive,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReleaseArchive {
    pub(crate) url: String,
    pub(crate) size: u64,
    sha256: [u8; 32],
    signature: [u8; SIGNATURE_BYTES],
}

impl ReleaseFeed {
    /// Verifies the feed's detached signature before reading any field the feed claims.
    pub(crate) fn verify(
        trust: &UpdateTrust,
        target: FeedTarget,
        document: &[u8],
        signature: &[u8],
        now: u64,
    ) -> Result<Self, UpdateError> {
        let read = feed::read(target, document, signature, trust, now)
            .map_err(|_| UpdateError::Verification)?;
        Ok(Self {
            archive: ReleaseArchive {
                url: target.archive_url(&read.version),
                size: read.archive.size,
                sha256: read.archive.sha256,
                signature: read.archive.signature,
            },
            version: read.version.to_string(),
            published_at: read.published_at,
        })
    }

    /// Whether this release is newer than `current`. A build without a stable version, such as
    /// one built from an untagged commit, never treats a release as newer.
    pub(crate) fn supersedes(&self, current: &str) -> bool {
        match (stable_version(&self.version), stable_version(current)) {
            (Some(release), Some(current)) => release > current,
            _ => false,
        }
    }
}

impl ReleaseArchive {
    pub(crate) fn verify(&self, trust: &UpdateTrust, bytes: &[u8]) -> Result<(), UpdateError> {
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        if bytes.len() as u64 == self.size
            && digest == self.sha256
            && trust.verify(bytes, &self.signature).is_ok()
        {
            Ok(())
        } else {
            Err(UpdateError::Verification)
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use ring::signature::{Ed25519KeyPair, KeyPair as _};
    use sha2::{Digest as _, Sha256};
    use spaceterm_core::update::facts::{Protocols, Version};
    use spaceterm_core::update::feed::{Feed, FeedArchive, FeedTarget, SystemVersion};
    use spaceterm_core::update::trust::UpdateTrust;

    /// A release's current and backup test keys, never the release keys. It signs with the
    /// current key unless [`ReleaseSigner::with_backup`] selected the backup key.
    pub(crate) struct ReleaseSigner {
        current: Ed25519KeyPair,
        backup: Ed25519KeyPair,
        signs_with_backup: bool,
    }

    fn pair(seed: u8) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap()
    }

    impl ReleaseSigner {
        pub(crate) fn new(seed: u8) -> Self {
            Self {
                current: pair(seed),
                backup: pair(seed.wrapping_add(100)),
                signs_with_backup: false,
            }
        }

        /// The same trust, signing with its backup key.
        pub(crate) fn with_backup(mut self) -> Self {
            self.signs_with_backup = true;
            self
        }

        pub(crate) fn trust(&self) -> UpdateTrust {
            let public = |pair: &Ed25519KeyPair| STANDARD.encode(pair.public_key().as_ref());
            let document = serde_json::json!({
                "current": public(&self.current),
                "backup": public(&self.backup),
            });
            UpdateTrust::parse(document.to_string().as_bytes()).unwrap()
        }

        pub(crate) fn sign(&self, message: &[u8]) -> Vec<u8> {
            let pair = if self.signs_with_backup {
                &self.backup
            } else {
                &self.current
            };
            pair.sign(message).as_ref().to_vec()
        }

        /// The feed and raw signature a release publishes for `target` and `archive`.
        pub(crate) fn feed(
            &self,
            target: FeedTarget,
            version: &str,
            published_at: u64,
            archive: &[u8],
        ) -> (Vec<u8>, Vec<u8>) {
            let version = Version::parse(version).unwrap();
            let feed = Feed {
                target,
                published_at,
                protocols: Protocols::CURRENT,
                minimum_system: SystemVersion {
                    major: 2,
                    minor: 35,
                },
                archive: FeedArchive {
                    name: target.archive(&version),
                    size: archive.len() as u64,
                    sha256: Sha256::digest(archive).into(),
                    signature: self.sign(archive).try_into().unwrap(),
                },
                version,
            }
            .to_bytes();
            let signature = self.sign(&feed);
            (feed, signature)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::ReleaseSigner;
    use super::*;

    const TARGET: FeedTarget = FeedTarget::DESKTOP_LINUX;
    const NOW: u64 = 1_800_000_000;

    fn verified(
        signer: &ReleaseSigner,
        feed: &[u8],
        signature: &[u8],
    ) -> Result<ReleaseFeed, UpdateError> {
        ReleaseFeed::verify(&signer.trust(), TARGET, feed, signature, NOW)
    }

    #[test]
    fn signed_feed_should_name_its_tagged_archive() {
        let signer = ReleaseSigner::new(7);
        let (feed, signature) = signer.feed(TARGET, "1.2.3", NOW - 60, b"archive");
        let feed = verified(&signer, &feed, &signature).unwrap();
        assert_eq!(feed.version, "1.2.3");
        assert_eq!(feed.published_at, NOW - 60);
        assert_eq!(
            feed.archive.url,
            "https://github.com/sadiksaifi/SpaceTerm/releases/download/v1.2.3/spaceterm-desktop-1.2.3-linux-x86_64.tar.gz"
        );
        assert_eq!(feed.archive.verify(&signer.trust(), b"archive"), Ok(()));
        assert!(feed.supersedes("1.2.2"));
        assert!(!feed.supersedes("1.2.3"));
        assert!(!feed.supersedes("dev.abc1234"));
    }

    #[test]
    fn a_release_signed_with_the_backup_key_should_verify() {
        let signer = ReleaseSigner::new(7).with_backup();
        let (feed, signature) = signer.feed(TARGET, "1.2.3", NOW, b"archive");
        let feed = verified(&signer, &feed, &signature).unwrap();
        assert_eq!(feed.archive.verify(&signer.trust(), b"archive"), Ok(()));
    }

    #[test]
    fn feed_signed_by_another_key_should_be_rejected() {
        let (feed, signature) = ReleaseSigner::new(8).feed(TARGET, "1.2.3", NOW, b"a");
        assert_eq!(
            verified(&ReleaseSigner::new(7), &feed, &signature),
            Err(UpdateError::Verification)
        );
    }

    #[test]
    fn altered_feed_should_be_rejected() {
        let signer = ReleaseSigner::new(7);
        let (feed, signature) = signer.feed(TARGET, "1.2.3", NOW, b"a");
        let altered = String::from_utf8(feed).unwrap().replace("1.2.3", "1.2.4");
        assert_eq!(
            verified(&signer, altered.as_bytes(), &signature),
            Err(UpdateError::Verification)
        );
    }

    #[test]
    fn a_core_feed_or_a_future_feed_should_be_rejected() {
        let signer = ReleaseSigner::new(7);
        for (target, published) in [
            (FeedTarget::CORE_LINUX, NOW),
            (TARGET, NOW + 3600),
        ] {
            let (feed, signature) = signer.feed(target, "1.2.3", published, b"a");
            assert_eq!(
                verified(&signer, &feed, &signature),
                Err(UpdateError::Verification)
            );
        }
    }

    #[test]
    fn archive_with_a_different_body_should_be_rejected() {
        let signer = ReleaseSigner::new(7);
        let (feed, signature) = signer.feed(TARGET, "1.2.3", NOW, b"archive");
        let feed = verified(&signer, &feed, &signature).unwrap();
        assert_eq!(
            feed.archive.verify(&signer.trust(), b"archivE"),
            Err(UpdateError::Verification)
        );
        assert_eq!(
            feed.archive
                .verify(&ReleaseSigner::new(8).trust(), b"archive"),
            Err(UpdateError::Verification)
        );
    }
}
