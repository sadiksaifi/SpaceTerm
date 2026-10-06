//! The signed feed and archive that self-installed releases update from.
//!
//! A release publishes one feed per platform, signed with the same Ed25519 key that signs the
//! archive it names. Neither the feed nor the archive grants authority until both signatures
//! verify against the key the running build carries.
#![cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "only the Linux updater installs release archives itself"
    )
)]

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::{UpdateError, stable_version};

const RELEASES: &str = "https://github.com/sadiksaifi/SpaceTerm/releases";
pub(crate) const MAX_FEED_BYTES: usize = 4096;
pub(crate) const MAX_FEED_SIGNATURE_BYTES: usize = 256;
pub(crate) const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
/// Publication clocks may run slightly ahead of the installed host.
const CLOCK_SKEW: u64 = 300;

/// The release asset names of one platform, for example `linux-x86_64`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReleasePlatform(&'static str);

impl ReleasePlatform {
    pub(crate) const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// The feed of the latest release, which always names that release's own archive.
    pub(crate) fn feed_url(self) -> String {
        format!("{RELEASES}/latest/download/latest-{}.json", self.0)
    }

    pub(crate) fn feed_signature_url(self) -> String {
        format!("{}.sig", self.feed_url())
    }

    fn archive_name(self, version: &str) -> String {
        format!("SpaceTerm-{version}-{}.tar.gz", self.0)
    }
}

/// The Ed25519 public key installed releases trust.
#[derive(Clone)]
pub(crate) struct UpdateKey([u8; 32]);

impl UpdateKey {
    /// The release identity's bundle template is the one source of the update trust root.
    pub(crate) fn release() -> Option<Self> {
        Self::from_property_list(include_str!(
            "../../packaging/macos/spaceterm/Info.plist"
        ))
    }

    fn from_property_list(plist: &str) -> Option<Self> {
        let (_, after) = plist.split_once("<key>SUPublicEDKey</key>")?;
        let value = after.trim_start().strip_prefix("<string>")?;
        let (value, _) = value.split_once("</string>")?;
        Self::from_base64(value.trim())
    }

    pub(crate) fn from_base64(value: &str) -> Option<Self> {
        STANDARD.decode(value).ok()?.try_into().ok().map(Self)
    }

    fn verifies(&self, message: &[u8], signature: &[u8; 64]) -> bool {
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &self.0)
            .verify(message, signature)
            .is_ok()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeedDocument {
    version: String,
    published_at: u64,
    archive: ArchiveDocument,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveDocument {
    name: String,
    size: u64,
    sha256: String,
    signature: String,
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
    signature: [u8; 64],
}

impl ReleaseFeed {
    /// Verifies the detached feed signature before reading any field the feed claims.
    pub(crate) fn verify(
        key: &UpdateKey,
        platform: ReleasePlatform,
        feed: &[u8],
        signature: &[u8],
        now: u64,
    ) -> Result<Self, UpdateError> {
        if feed.len() > MAX_FEED_BYTES || signature.len() > MAX_FEED_SIGNATURE_BYTES {
            return Err(UpdateError::Verification);
        }
        let signature = std::str::from_utf8(signature)
            .ok()
            .and_then(|value| decode_signature(value.trim_ascii()))
            .ok_or(UpdateError::Verification)?;
        if !key.verifies(feed, &signature) {
            return Err(UpdateError::Verification);
        }
        let document: FeedDocument =
            serde_json::from_slice(feed).map_err(|_| UpdateError::Verification)?;
        let archive = document.archive;
        let valid = stable_version(&document.version).is_some()
            && document.published_at > 0
            && document.published_at <= now.saturating_add(CLOCK_SKEW)
            && archive.name == platform.archive_name(&document.version)
            && (1..=MAX_ARCHIVE_BYTES).contains(&archive.size);
        let sha256 = decode_sha256(&archive.sha256);
        let archive_signature = decode_signature(&archive.signature);
        let (true, Some(sha256), Some(signature)) = (valid, sha256, archive_signature) else {
            return Err(UpdateError::Verification);
        };
        Ok(Self {
            archive: ReleaseArchive {
                url: format!(
                    "{RELEASES}/download/v{}/{}",
                    document.version, archive.name
                ),
                size: archive.size,
                sha256,
                signature,
            },
            version: document.version,
            published_at: document.published_at,
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
    pub(crate) fn verify(&self, key: &UpdateKey, bytes: &[u8]) -> Result<(), UpdateError> {
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        if bytes.len() as u64 == self.size
            && digest == self.sha256
            && key.verifies(bytes, &self.signature)
        {
            Ok(())
        } else {
            Err(UpdateError::Verification)
        }
    }
}

fn decode_signature(value: &str) -> Option<[u8; 64]> {
    STANDARD.decode(value).ok()?.try_into().ok()
}

fn decode_sha256(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut digest = [0; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(digest)
}

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
            UpdateKey(self.0.public_key().as_ref().try_into().unwrap())
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
        assert!(plist.contains(&STANDARD.encode(key.0)));
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
            assert_eq!(
                verified(&feed, &signature),
                Err(UpdateError::Verification)
            );
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
            feed.archive.verify(&ReleaseSigner::new(8).key(), b"archive"),
            Err(UpdateError::Verification)
        );
    }
}
