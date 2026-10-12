//! The unchanged schema read by shipped Linux desktop updaters.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::version::stable_version;
use crate::{DistributionFailure, UpdateKey};

const RELEASES: &str = "https://github.com/sadiksaifi/SpaceTerm/releases";
pub const MAX_FEED_BYTES: usize = 4096;
pub const MAX_FEED_SIGNATURE_BYTES: usize = 256;
pub const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
/// Publication clocks may run slightly ahead of the installed host.
const CLOCK_SKEW: u64 = 300;

/// The release asset names of one platform, for example `linux-x86_64`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReleasePlatform(&'static str);

impl ReleasePlatform {
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// The feed of the latest release, which always names that release's own archive.
    pub fn feed_url(self) -> String {
        format!("{RELEASES}/latest/download/latest-{}.json", self.0)
    }

    pub fn feed_signature_url(self) -> String {
        format!("{}.sig", self.feed_url())
    }

    fn archive_name(self, version: &str) -> String {
        format!("SpaceTerm-{version}-{}.tar.gz", self.0)
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
pub struct ReleaseFeed {
    pub version: String,
    pub published_at: u64,
    pub archive: ReleaseArchive,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseArchive {
    pub url: String,
    pub size: u64,
    sha256: [u8; 32],
    signature: [u8; 64],
}

impl ReleaseFeed {
    /// Verifies the detached feed signature before reading any field the feed claims.
    pub fn verify(
        key: &UpdateKey,
        platform: ReleasePlatform,
        feed: &[u8],
        signature: &[u8],
        now: u64,
    ) -> Result<Self, DistributionFailure> {
        if feed.len() > MAX_FEED_BYTES || signature.len() > MAX_FEED_SIGNATURE_BYTES {
            return Err(DistributionFailure::Verification);
        }
        let signature = std::str::from_utf8(signature)
            .ok()
            .and_then(|value| decode_signature(value.trim_ascii()))
            .ok_or(DistributionFailure::Verification)?;
        if !key.verifies(feed, &signature) {
            return Err(DistributionFailure::Verification);
        }
        let document: FeedDocument =
            serde_json::from_slice(feed).map_err(|_| DistributionFailure::Verification)?;
        let archive = document.archive;
        let valid = stable_version(&document.version).is_some()
            && document.published_at > 0
            && document.published_at <= now.saturating_add(CLOCK_SKEW)
            && archive.name == platform.archive_name(&document.version)
            && (1..=MAX_ARCHIVE_BYTES).contains(&archive.size);
        let sha256 = decode_sha256(&archive.sha256);
        let archive_signature = decode_signature(&archive.signature);
        let (true, Some(sha256), Some(signature)) = (valid, sha256, archive_signature) else {
            return Err(DistributionFailure::Verification);
        };
        Ok(Self {
            archive: ReleaseArchive {
                url: format!("{RELEASES}/download/v{}/{}", document.version, archive.name),
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
    pub fn supersedes(&self, current: &str) -> bool {
        match (stable_version(&self.version), stable_version(current)) {
            (Some(release), Some(current)) => release > current,
            _ => false,
        }
    }
}

impl ReleaseArchive {
    pub fn verify(&self, key: &UpdateKey, bytes: &[u8]) -> Result<(), DistributionFailure> {
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        if bytes.len() as u64 == self.size
            && digest == self.sha256
            && key.verifies(bytes, &self.signature)
        {
            Ok(())
        } else {
            Err(DistributionFailure::Verification)
        }
    }
}

fn decode_signature(value: &str) -> Option<[u8; 64]> {
    STANDARD.decode(value).ok()?.try_into().ok()
}

fn decode_sha256(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let mut digest = [0; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(digest)
}
