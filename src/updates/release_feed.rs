//! The signed feed and archive that self-installed Linux desktops update from.
//!
//! A release publishes `spaceterm-desktop-linux-x86_64.json` beside a `.sig` of 64 raw Ed25519
//! bytes. [`feed::read`] verifies that signature under the current or the backup key of
//! `packaging/update-trust.json` before it reads any field, then holds the feed to every rule of
//! its schema. The archive the feed names grants nothing until its size, digest and own signature
//! verify under the same keys. A desktop below the newest release's Update Floor installs the
//! release at that floor first ([`ReleaseFeed::next`]).
#![cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "only the Linux updater installs release archives itself"
    )
)]

use sha2::{Digest as _, Sha256};
use spaceterm_core::update::facts::Version;
use spaceterm_core::update::feed::{self, Feed, FeedTarget, SystemVersion};
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
    /// The oldest glibc the release runs on.
    minimum_system: SystemVersion,
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
    /// The release a desktop running `current` installs next: the newest release, or, while
    /// `current` is below a release's Update Floor, the release at that floor, down to the
    /// first whose floor `current` meets. `get` fetches a URL's body of at most `limit` bytes.
    /// Each feed's detached signature verifies before any field the feed claims is read. Floors
    /// that lead to no release fail verification.
    pub(crate) fn next(
        trust: &UpdateTrust,
        target: FeedTarget,
        current: &str,
        now: u64,
        get: impl Fn(&str, usize) -> Result<Vec<u8>, UpdateError>,
    ) -> Result<Self, UpdateError> {
        let read = |release: Option<&Version>| {
            let document = get(&target.feed_url(release), MAX_FEED_BYTES)?;
            let signature = get(
                &target.feed_signature_url(release),
                MAX_FEED_SIGNATURE_BYTES,
            )?;
            feed::read(target, &document, &signature, trust, now)
                .map_err(|_| UpdateError::Verification)
        };
        // Every build names a release version or a commit, which has no order and no floor.
        let current = Version::parse(current).map_err(|_| UpdateError::Verification)?;
        feed::walk(&current, read)?
            .map(|next| Self::from(next.feed))
            .ok_or(UpdateError::Verification)
    }

    /// Whether the release runs on a host with `system`. A host that does not say runs none.
    pub(crate) fn runs_on(&self, system: Option<SystemVersion>) -> bool {
        system.is_some_and(|system| system >= self.minimum_system)
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

impl From<Feed> for ReleaseFeed {
    fn from(read: Feed) -> Self {
        Self {
            archive: ReleaseArchive {
                url: read.target.archive_url(&read.version),
                size: read.archive.size,
                sha256: read.archive.sha256,
                signature: read.archive.signature,
            },
            version: read.version.to_string(),
            published_at: read.published_at,
            minimum_system: read.minimum_system,
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

    /// The oldest glibc a test release runs on unless a test names another.
    pub(crate) const MINIMUM: SystemVersion = SystemVersion {
        major: 2,
        minor: 35,
    };

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
            self.floored_feed(target, version, "0.0.0", MINIMUM, published_at, archive)
        }

        /// As [`Self::feed`], for a release whose Update Floor is `floor`.
        pub(crate) fn floored_feed(
            &self,
            target: FeedTarget,
            version: &str,
            floor: &str,
            minimum_system: SystemVersion,
            published_at: u64,
            archive: &[u8],
        ) -> (Vec<u8>, Vec<u8>) {
            let version = Version::parse(version).unwrap();
            let feed = Feed {
                target,
                published_at,
                protocols: Protocols::CURRENT,
                minimum_system,
                archive: FeedArchive {
                    name: target.archive(&version),
                    size: archive.len() as u64,
                    sha256: Sha256::digest(archive).into(),
                    signature: self.sign(archive).try_into().unwrap(),
                },
                floor: spaceterm_core::update::ordering::Floor::parse(floor).unwrap(),
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
        // A build that names a commit has no floor, so it reads the newest release only.
        ReleaseFeed::next(
            &signer.trust(),
            TARGET,
            "dev.0123456789ab",
            NOW,
            |url, _| {
                Ok(if url.ends_with(".sig") {
                    signature
                } else {
                    feed
                }
                .to_vec())
            },
        )
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
        for (target, published) in [(FeedTarget::CORE_LINUX, NOW), (TARGET, NOW + 3600)] {
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
