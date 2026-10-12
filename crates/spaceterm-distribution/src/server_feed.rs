//! Separate server feeds. Detached signatures cover the exact JSON, before decoding.
use crate::{DistributionFailure, StableVersion, UpdateKey};
use serde::{Deserialize, Serialize};

pub const MAX_FEED_BYTES: usize = 4096;
pub const MAX_FEED_SIGNATURE_BYTES: usize = 256;
pub const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const RELEASES: &str = "https://github.com/sadiksaifi/SpaceTerm/releases";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildChannel {
    Release,
    Preflight,
    Development,
}
impl BuildChannel {
    pub const fn native_identifier(self) -> &'static str {
        match self {
            Self::Release => "io.github.sadiksaifi.spaceterm.server",
            Self::Preflight => "io.github.sadiksaifi.spaceterm-preflight.server",
            Self::Development => "io.github.sadiksaifi.spaceterm-development.server",
        }
    }
    pub const fn bundle_name(self) -> &'static str {
        match self {
            Self::Release => "SpaceTerm Server.app",
            Self::Preflight => "SpaceTerm Preflight Server.app",
            Self::Development => "SpaceTerm Development Server.app",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ServerPlatform {
    #[serde(rename = "darwin-arm64")]
    DarwinArm64,
    #[serde(rename = "linux-x86_64")]
    LinuxX86_64,
}
impl ServerPlatform {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DarwinArm64 => "darwin-arm64",
            Self::LinuxX86_64 => "linux-x86_64",
        }
    }
    pub fn archive_name(self, version: &str) -> String {
        format!("SpaceTerm-Server-{version}-{}.tar.gz", self.as_str())
    }
    pub fn feed_url(self) -> String {
        format!(
            "{RELEASES}/latest/download/latest-server-{}.json",
            self.as_str()
        )
    }
    pub fn feed_signature_url(self) -> String {
        format!("{}.sig", self.feed_url())
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerComponent {
    Server,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageCompatibility {
    pub read: Vec<u32>,
    pub write: u32,
    pub migrate_from: Vec<u32>,
}
impl StorageCompatibility {
    pub(crate) fn validate(&self) -> bool {
        sorted_nonzero(&self.read, 16, false)
            && sorted_nonzero(&self.migrate_from, 16, true)
            && self.write != 0
            && self.read.contains(&self.write)
    }
}
pub(crate) fn sorted_nonzero<T: Ord + Default>(values: &[T], maximum: usize, empty: bool) -> bool {
    (empty || !values.is_empty())
        && values.len() <= maximum
        && values.iter().all(|v| *v != T::default())
        && values.windows(2).all(|pair| pair[0] < pair[1])
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationCompatibility {
    pub manifest_schema: u16,
    pub registration_schema: u16,
    pub journal_schema: u16,
    pub launcher_abi: u16,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerArchiveV1 {
    pub name: String,
    pub size: u64,
    #[serde(with = "crate::encoding::sha256")]
    pub sha256: [u8; 32],
    #[serde(with = "crate::encoding::signature")]
    pub signature: [u8; 64],
    #[serde(with = "crate::encoding::sha256")]
    pub manifest_sha256: [u8; 32],
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerFeedV1 {
    pub schema_version: u16,
    pub component: ServerComponent,
    pub build_identity: BuildChannel,
    pub platform: ServerPlatform,
    pub version: StableVersion,
    pub published_at: u64,
    pub protocol_majors: Vec<u16>,
    pub storage: StorageCompatibility,
    pub installation: InstallationCompatibility,
    pub archive: ServerArchiveV1,
}

pub struct VerifiedServerFeed {
    pub(crate) document: ServerFeedV1,
}
impl VerifiedServerFeed {
    pub fn document(&self) -> &ServerFeedV1 {
        &self.document
    }
    pub fn archive_url(&self) -> String {
        format!(
            "{RELEASES}/download/v{}/{}",
            self.document.version.as_str(),
            self.document.archive.name
        )
    }
    pub fn supersedes(&self, current: &str) -> bool {
        self.document.version.supersedes(current)
    }
}
pub struct VerifiedServerArchive {
    pub(crate) document: ServerFeedV1,
    pub(crate) bytes: Box<[u8]>,
    reservation: Option<ArchiveReservation>,
}
impl VerifiedServerArchive {
    pub fn feed(&self) -> &ServerFeedV1 {
        &self.document
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Authenticate before parsing. Preflight and Development never consume production feeds.
pub fn verify_server_feed(
    key: &UpdateKey,
    platform: ServerPlatform,
    feed: &[u8],
    detached_signature: &[u8],
    now_unix_seconds: u64,
) -> Result<VerifiedServerFeed, DistributionFailure> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    if feed.len() > MAX_FEED_BYTES || detached_signature.len() > MAX_FEED_SIGNATURE_BYTES {
        return Err(DistributionFailure::LimitExceeded);
    }
    let text = std::str::from_utf8(detached_signature)
        .map_err(|_| DistributionFailure::Verification)?
        .trim_ascii();
    let signature: [u8; 64] = STANDARD
        .decode(text)
        .map_err(|_| DistributionFailure::Verification)?
        .try_into()
        .map_err(|_| DistributionFailure::Verification)?;
    if STANDARD.encode(signature) != text || !key.verifies(feed, &signature) {
        return Err(DistributionFailure::Verification);
    }
    let document: ServerFeedV1 =
        serde_json::from_slice(feed).map_err(|_| DistributionFailure::InvalidRecord)?;
    if document.schema_version != 1
        || document.installation
            != (InstallationCompatibility {
                manifest_schema: 1,
                registration_schema: 2,
                journal_schema: 1,
                launcher_abi: 2,
            })
    {
        return Err(DistributionFailure::UnsupportedSchema);
    }
    if document.build_identity != BuildChannel::Release
        || document.platform != platform
        || document.archive.name != platform.archive_name(document.version.as_str())
    {
        return Err(DistributionFailure::IdentityMismatch);
    }
    if document.published_at == 0
        || document.published_at > now_unix_seconds.saturating_add(300)
        || !sorted_nonzero(&document.protocol_majors, 8, false)
        || !document.storage.validate()
        || !(1..=MAX_ARCHIVE_BYTES).contains(&document.archive.size)
    {
        return Err(DistributionFailure::InvalidRecord);
    }
    Ok(VerifiedServerFeed { document })
}

/// Verifies ordinary Ed25519 over the exact archive bytes, not a prehash format.
/// A controller must retain at most one returned archive at a time, outside runtime budgets.
pub fn verify_server_archive(
    key: &UpdateKey,
    feed: &VerifiedServerFeed,
    archive: &[u8],
) -> Result<VerifiedServerArchive, DistributionFailure> {
    verify_archive_bytes(key, feed, archive)?;
    Ok(VerifiedServerArchive {
        document: feed.document.clone(),
        bytes: archive.into(),
        reservation: None,
    })
}

fn verify_archive_bytes(
    key: &UpdateKey,
    feed: &VerifiedServerFeed,
    archive: &[u8],
) -> Result<(), DistributionFailure> {
    use sha2::{Digest as _, Sha256};
    if archive.len() as u64 > MAX_ARCHIVE_BYTES {
        return Err(DistributionFailure::LimitExceeded);
    }
    let expected = &feed.document.archive;
    let digest: [u8; 32] = Sha256::digest(archive).into();
    if archive.len() as u64 != expected.size
        || digest != expected.sha256
        || !key.verifies(archive, &expected.signature)
    {
        return Err(DistributionFailure::ArchiveRejected);
    }
    Ok(())
}

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct ArchiveReservation(Arc<AtomicBool>);
impl Drop for ArchiveReservation {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// One separately budgeted archive per update controller, through publication/retention.
#[derive(Default)]
pub struct ServerArchiveVerifier {
    occupied: Arc<AtomicBool>,
}
impl ServerArchiveVerifier {
    pub fn new() -> Self {
        Self::default()
    }
    /// Takes ownership of the download buffer so verification adds no second archive allocation.
    pub fn verify_owned(
        &self,
        key: &UpdateKey,
        feed: &VerifiedServerFeed,
        archive: Vec<u8>,
    ) -> Result<VerifiedServerArchive, DistributionFailure> {
        if self
            .occupied
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(DistributionFailure::LimitExceeded);
        }
        let reservation = ArchiveReservation(Arc::clone(&self.occupied));
        verify_archive_bytes(key, feed, &archive)?;
        Ok(VerifiedServerArchive {
            document: feed.document.clone(),
            bytes: archive.into_boxed_slice(),
            reservation: Some(reservation),
        })
    }
    pub fn verify(
        &self,
        key: &UpdateKey,
        feed: &VerifiedServerFeed,
        archive: &[u8],
    ) -> Result<VerifiedServerArchive, DistributionFailure> {
        if self
            .occupied
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(DistributionFailure::LimitExceeded);
        }
        let reservation = ArchiveReservation(Arc::clone(&self.occupied));
        let mut verified = verify_server_archive(key, feed, archive)?;
        verified.reservation = Some(reservation);
        Ok(verified)
    }
}
