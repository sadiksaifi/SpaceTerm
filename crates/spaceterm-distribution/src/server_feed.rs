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
    /// Channel-isolated account artifact name, matching the server packager.
    /// Release names retain their existing canonical spelling.
    pub fn account_archive_name(self, build: BuildChannel, version: &str) -> String {
        let channel = match build {
            BuildChannel::Release => "",
            BuildChannel::Preflight => "-preflight",
            BuildChannel::Development => "-development",
        };
        format!(
            "SpaceTerm-Server{channel}-{version}-{}.tar.gz",
            self.as_str()
        )
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
    authenticate_feed(key, feed, detached_signature)?;
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

/// Account deployment compatibility excludes the removed privileged launcher ABI.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountInstallationCompatibility {
    pub manifest_schema: u16,
    pub registration_schema: u16,
    pub journal_schema: u16,
}

/// Exact-byte signed account-service metadata. BuildVersion permits signed nonrelease builds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountServerFeedV2 {
    pub schema_version: u16,
    pub component: ServerComponent,
    pub build_identity: BuildChannel,
    pub platform: ServerPlatform,
    pub version: crate::BuildVersion,
    #[serde(with = "crate::encoding::commit")]
    pub build_commit: [u8; 20],
    pub published_at: u64,
    pub protocol_majors: Vec<u16>,
    pub storage: StorageCompatibility,
    pub installation: AccountInstallationCompatibility,
    pub archive: ServerArchiveV1,
}

/// Authenticated comparison facts and the exact retained signature transcript.
pub struct VerifiedAccountServerFeed {
    document: AccountServerFeedV2,
    bytes: Box<[u8]>,
    signature: Box<[u8]>,
    key: UpdateKey,
}
impl VerifiedAccountServerFeed {
    pub fn document(&self) -> &AccountServerFeedV2 {
        &self.document
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }
    /// Native production composition must compare against UpdateKey::release().
    pub fn authenticated_by(&self, key: &UpdateKey) -> bool {
        self.key == *key
    }
}

/// Authenticate before parsing, then bind target and build explicitly, including nonrelease builds.
pub fn verify_account_server_feed(
    key: &UpdateKey,
    build: BuildChannel,
    platform: ServerPlatform,
    feed: &[u8],
    detached_signature: &[u8],
    now_unix_seconds: u64,
) -> Result<VerifiedAccountServerFeed, DistributionFailure> {
    authenticate_feed(key, feed, detached_signature)?;
    let document: AccountServerFeedV2 =
        serde_json::from_slice(feed).map_err(|_| DistributionFailure::InvalidRecord)?;
    if document.schema_version != 2
        || document.installation
            != (AccountInstallationCompatibility {
                manifest_schema: crate::manifest::AccountInstallationManifestV2::SCHEMA_VERSION,
                registration_schema: 1,
                journal_schema: 1,
            })
    {
        return Err(DistributionFailure::UnsupportedSchema);
    }
    if document.build_identity != build
        || document.platform != platform
        || (document.archive.name != platform.account_archive_name(build, document.version.as_str())
            // Earlier account feeds used the release-style name for nonrelease
            // builds. Their exact signed build and target remain mandatory.
            && document.archive.name != platform.archive_name(document.version.as_str()))
    {
        return Err(DistributionFailure::IdentityMismatch);
    }
    if document.published_at == 0
        || document.published_at > now_unix_seconds.saturating_add(300)
        || !sorted_nonzero(&document.protocol_majors, 8, false)
        || !document.storage.validate()
        || !(1..=MAX_ARCHIVE_BYTES).contains(&document.archive.size)
        || (build == BuildChannel::Release
            && crate::version::stable_version(document.version.as_str()).is_none())
    {
        return Err(DistributionFailure::InvalidRecord);
    }
    Ok(VerifiedAccountServerFeed {
        document,
        bytes: feed.into(),
        signature: detached_signature.into(),
        key: key.clone(),
    })
}

fn authenticate_feed(
    key: &UpdateKey,
    feed: &[u8],
    signature: &[u8],
) -> Result<(), DistributionFailure> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    if feed.len() > MAX_FEED_BYTES || signature.len() > MAX_FEED_SIGNATURE_BYTES {
        return Err(DistributionFailure::LimitExceeded);
    }
    let text = std::str::from_utf8(signature)
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
    Ok(())
}

/// One authenticated archive together with its validated account-only layout.
/// No caller can substitute a decoded manifest for the archive's signed contents.
pub struct VerifiedAccountServerArtifact {
    feed: VerifiedAccountServerFeed,
    manifest: crate::manifest::AccountInstallationManifestV2,
    manifest_bytes: Box<[u8]>,
    bytes: Box<[u8]>,
    _reservation: ArchiveReservation,
}
impl VerifiedAccountServerArtifact {
    /// Exact archive identity retained by native installation status and request binding.
    pub fn artifact_sha256(&self) -> [u8; 32] {
        self.feed.document.archive.sha256
    }
    pub fn feed(&self) -> &VerifiedAccountServerFeed {
        &self.feed
    }
    pub fn manifest(&self) -> &crate::manifest::AccountInstallationManifestV2 {
        &self.manifest
    }
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl ServerArchiveVerifier {
    /// Reuses the controller's single archive budget for either retained or account artifacts.
    pub fn verify_account_owned(
        &self,
        key: &UpdateKey,
        feed: VerifiedAccountServerFeed,
        bytes: Vec<u8>,
    ) -> Result<VerifiedAccountServerArtifact, DistributionFailure> {
        use sha2::{Digest as _, Sha256};
        use std::io::Read;
        if self
            .occupied
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(DistributionFailure::LimitExceeded);
        }
        let reservation = ArchiveReservation(Arc::clone(&self.occupied));
        if !feed.authenticated_by(key) {
            return Err(DistributionFailure::Verification);
        }
        if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
            return Err(DistributionFailure::LimitExceeded);
        }
        let expected = &feed.document.archive;
        if bytes.len() as u64 != expected.size
            || <[u8; 32]>::from(Sha256::digest(&bytes)) != expected.sha256
            || !key.verifies(&bytes, &expected.signature)
        {
            return Err(DistributionFailure::ArchiveRejected);
        }
        let reader = flate2::read::MultiGzDecoder::new(bytes.as_slice())
            .take(crate::manifest::MAX_EXPANDED_BYTES + 1);
        let mut archive = tar::Archive::new(reader);
        let mut manifest_bytes = None;
        let mut long_name = None;
        // The ordinary iterator allocates GNU/PAX bodies before yielding an entry.
        // Raw iteration lets us reject them at the header, before that allocation.
        for (i, entry) in archive
            .entries()
            .map_err(|_| DistributionFailure::ArchiveRejected)?
            .raw(true)
            .enumerate()
        {
            if i >= crate::manifest::MAX_FILES * 2 + 2 {
                return Err(DistributionFailure::LimitExceeded);
            }
            let mut entry = entry.map_err(|_| DistributionFailure::ArchiveRejected)?;
            let kind = entry.header().entry_type();
            if kind.is_gnu_longname() {
                if long_name.is_some() || !(1..=1043).contains(&entry.size()) {
                    return Err(DistributionFailure::ArchiveRejected);
                }
                let mut name = vec![0; entry.size() as usize];
                entry
                    .read_exact(&mut name)
                    .map_err(|_| DistributionFailure::ArchiveRejected)?;
                if name.pop() != Some(0) {
                    return Err(DistributionFailure::ArchiveRejected);
                }
                long_name = Some(name);
                continue;
            }
            if !kind.is_file() && !kind.is_dir() {
                return Err(DistributionFailure::ArchiveRejected);
            }
            let path = long_name
                .take()
                .map(std::borrow::Cow::Owned)
                .unwrap_or_else(|| entry.path_bytes());
            if path.as_ref() == b"spaceterm-server/installation.json" {
                if manifest_bytes.is_some()
                    || !entry.header().entry_type().is_file()
                    || entry.size() > crate::manifest::MAX_MANIFEST_BYTES as u64
                {
                    return Err(DistributionFailure::ArchiveRejected);
                }
                let mut value = Vec::new();
                entry
                    .read_to_end(&mut value)
                    .map_err(|_| DistributionFailure::ArchiveRejected)?;
                manifest_bytes = Some(value);
            }
        }
        let manifest_bytes = manifest_bytes.ok_or(DistributionFailure::ArchiveRejected)?;
        let manifest =
            crate::manifest::verify_account_installation_manifest(&feed, &manifest_bytes)?;
        crate::manifest::validate_archive(&bytes, &manifest.files, &manifest_bytes)?;
        Ok(VerifiedAccountServerArtifact {
            feed,
            manifest,
            manifest_bytes: manifest_bytes.into_boxed_slice(),
            bytes: bytes.into_boxed_slice(),
            _reservation: reservation,
        })
    }
}
