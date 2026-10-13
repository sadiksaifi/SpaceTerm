//! Signed account-private server code, independent of desktop installation.
//!
//! Selection is one atomic private-file publication. This module never starts or
//! stops a process and never changes deliberate lifecycle intent. The account
//! service must quiesce the old runtime before selecting another installation.

mod archive;
#[cfg(target_os = "macos")]
mod macos;

use crate::{
    account_service::{RegisteredServer, ServiceFailure},
    private_filesystem::{Directory, Failure, Snapshot},
    startup::{AccountBuildKey, BuildIdentity, InstallationId},
};
use archive::Tree;
use serde::{Deserialize, Serialize};
use spaceterm_distribution::{
    AccountInstallationManifestV2, BuildChannel, ServerPlatform, UpdateKey,
    VerifiedAccountServerArtifact, verify_account_installation_manifest,
    verify_account_server_feed,
};
use std::{
    fmt,
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// The account artifact's single authenticated representation, including its residency reservation.
pub use spaceterm_distribution::VerifiedAccountServerArtifact as VerifiedServerArtifact;

/// Bounded artifact facts for the account service's status and retained outcomes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallationStatus {
    /// The selected account installation identity.
    pub installation: InstallationId,
    /// The signed server build version.
    pub version: spaceterm_distribution::BuildVersion,
    /// The exact signed compressed archive digest.
    pub artifact_sha256: [u8; 32],
}

/// Binds an already authenticated account artifact to production's compiled build,
/// native target and existing Ed25519 trust root. It never executes candidate code.
/// The account schema requires VerifiedAccountServerArtifact rather than a legacy
/// privileged-helper VerifiedServerArchive.
pub fn verify_server_artifact(
    archive: VerifiedServerArtifact,
) -> Result<VerifiedServerArtifact, ServiceFailure> {
    let trust = UpdateKey::release().ok_or(ServiceFailure::ArtifactRejected)?;
    let build = match option_env!("SPACETERM_HOST_BUILD") {
        Some("release") => BuildChannel::Release,
        Some("preflight") => BuildChannel::Preflight,
        Some("development") => BuildChannel::Development,
        _ => return Err(ServiceFailure::UnsupportedHost),
    };
    if !archive.feed().authenticated_by(&trust)
        || archive.manifest().build_identity != build
        || archive.manifest().platform != platform()?
    {
        return Err(ServiceFailure::ArtifactRejected);
    }
    Ok(archive)
}

const STATE_LIMIT: usize = 16 * 1024;
const METADATA_LIMIT: usize = 2 * spaceterm_distribution::manifest::MAX_MANIFEST_BYTES + 32 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Version {
    id: InstallationId,
    digest: [u8; 32],
    device: u64,
    inode: u64,
    #[serde(default)]
    cleanup_nonce: [u8; 16],
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    id: InstallationId,
    digest: [u8; 32],
    #[serde(default)]
    cleanup_nonce: [u8; 16],
}

// Created keeps reading the original Version representation. Reserved has no
// native identity yet and grants no authority to adopt or remove a tree.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(untagged)]
enum Preparation {
    Created(Version),
    Reserved(Candidate),
}
impl Preparation {
    fn id(self) -> InstallationId {
        match self {
            Self::Created(version) => version.id,
            Self::Reserved(candidate) => candidate.id,
        }
    }
    fn created(self) -> Option<Version> {
        match self {
            Self::Created(version) => Some(version),
            Self::Reserved(_) => None,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    schema: u16,
    key: AccountBuildKey,
    generation: u64,
    high_water: u64,
    approved: Option<Version>,
    previous: Option<Version>,
    staged: Option<Version>,
    preparing: Option<Preparation>,
    cleanup: Option<Version>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    feed: String,
    signature: String,
    manifest: String,
}

/// Bounded selection facts, not authority to start a process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstallationSnapshot {
    /// Versioned compare-and-publish generation.
    pub generation: u64,
    /// Only this installation may be selected for subsequent startup.
    pub approved: Option<InstallationId>,
    /// The retained rollback version, never selected implicitly.
    pub previous: Option<InstallationId>,
    /// Verified code waiting for explicit activation.
    pub staged: Option<InstallationId>,
    /// An interrupted preparation that requires explicit discard, never startup.
    pub preparing: Option<InstallationId>,
    /// A superseded version whose exact native cleanup has not completed.
    pub cleanup_pending: Option<InstallationId>,
}

/// Native filesystem owner bound to an admitted account registration and update trust root.
pub struct ServerInstallationStore {
    directory: Directory,
    tree: Tree,
    path: PathBuf,
    key: AccountBuildKey,
    trust: UpdateKey,
    uncertain: AtomicBool,
    #[cfg(feature = "test-support")]
    skip_native_signature: bool,
}

impl ServerInstallationStore {
    /// Opens or creates code storage in the registration's private data directory.
    /// Production always uses the existing release Ed25519 public key for every build.
    pub fn open(registered: &RegisteredServer) -> Result<Self, ServiceFailure> {
        let trust = UpdateKey::release().ok_or(ServiceFailure::ArtifactRejected)?;
        Self::open_with_trust(registered, trust, false)
    }
    fn open_with_trust(
        registered: &RegisteredServer,
        trust: UpdateKey,
        _test_signature: bool,
    ) -> Result<Self, ServiceFailure> {
        let key = registered.key();
        // SAFETY: geteuid is a side-effect-free credential query.
        let uid = unsafe { libc::geteuid() };
        if key.account_uid == 0 || uid == 0 {
            return Err(ServiceFailure::RootAccount);
        }
        if key.account_uid != uid {
            return Err(ServiceFailure::IdentityMismatch);
        }
        platform()?;
        registered.directory.verify()?;
        Directory::open(registered.data_directory())?
            .ok_or(ServiceFailure::Missing)?
            .verify_established()?;
        let path = registered.data_directory().join("installations");
        let directory = Directory::create(&path)?;
        let tree = Tree::open(&path, &directory)?;
        if directory.read("lock", 1)?.is_none() {
            match directory.publish("lock", b"", None) {
                Ok(()) | Err(Failure::Conflict) => (),
                Err(error) => return Err(error.into()),
            }
        }
        let store = Self {
            directory,
            tree,
            path,
            key,
            trust,
            uncertain: AtomicBool::new(false),
            #[cfg(feature = "test-support")]
            skip_native_signature: _test_signature,
        };
        let _lock = store.tree.lock()?;
        if store.directory.read("selection", STATE_LIMIT)?.is_none() {
            let selection = Selection {
                schema: 1,
                key,
                generation: 1,
                high_water: 0,
                approved: None,
                previous: None,
                staged: None,
                preparing: None,
                cleanup: None,
            };
            store.publish(&selection, None)?;
        }
        store.load()?;
        Ok(store)
    }
    /// Reads retained selection without inferring approval from directory contents.
    pub fn snapshot(&self) -> Result<InstallationSnapshot, ServiceFailure> {
        let _lock = self.tree.lock()?;
        let (selection, _) = self.load()?;
        Ok(InstallationSnapshot {
            generation: selection.generation,
            approved: selection.approved.map(|v| v.id),
            previous: selection.previous.map(|v| v.id),
            staged: selection.staged.map(|v| v.id),
            cleanup_pending: selection.cleanup.map(|v| v.id),
            preparing: selection.preparing.map(Preparation::id),
        })
    }
    /// Stages exact authenticated bytes without activating code or altering service intent.
    /// A second candidate requires explicit discard or activation of the first.
    pub fn stage(
        &self,
        artifact: &VerifiedAccountServerArtifact,
    ) -> Result<InstallationId, ServiceFailure> {
        let _lock = self.tree.lock()?;
        let (mut selection, expected) = self.load()?;
        let feed = artifact.feed();
        if !feed.authenticated_by(&self.trust)
            || feed.document().build_identity != channel(self.key.build_identity)
            || feed.document().platform != platform()?
        {
            return Err(ServiceFailure::ArtifactRejected);
        }
        if selection.cleanup.is_some() {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        if selection.preparing.is_some() {
            return Err(ServiceFailure::Pending);
        }
        if let Some(staged) = selection.staged {
            return if staged.digest == feed.document().archive.sha256 {
                self.validate_version(staged)?;
                Ok(staged.id)
            } else {
                Err(ServiceFailure::RequestConflict)
            };
        }
        // Reservation, retained native identity and completed staging each publish once.
        selection
            .generation
            .checked_add(3)
            .ok_or(ServiceFailure::LimitExceeded)?;
        let number = selection
            .high_water
            .checked_add(1)
            .ok_or(ServiceFailure::LimitExceeded)?;
        let id = InstallationId(NonZeroU64::new(number).ok_or(ServiceFailure::LimitExceeded)?);
        let metadata = Metadata {
            feed: std::str::from_utf8(feed.bytes())
                .map_err(|_| ServiceFailure::ArtifactRejected)?
                .into(),
            signature: std::str::from_utf8(feed.signature())
                .map_err(|_| ServiceFailure::ArtifactRejected)?
                .into(),
            manifest: std::str::from_utf8(artifact.manifest_bytes())
                .map_err(|_| ServiceFailure::ArtifactRejected)?
                .into(),
        };
        let bytes = serde_json::to_vec(&metadata).map_err(|_| ServiceFailure::InvalidRecord)?;
        if bytes.len() > METADATA_LIMIT {
            return Err(ServiceFailure::LimitExceeded);
        }
        // Retain the candidate and consume its identity before either mkdir or
        // metadata publication. A crash cannot make the same ID fresh again.
        let mut cleanup_nonce = [0; 16];
        getrandom::fill(&mut cleanup_nonce).map_err(|_| ServiceFailure::StorageUnavailable)?;
        selection.high_water = number;
        selection.preparing = Some(Preparation::Reserved(Candidate {
            id,
            digest: feed.document().archive.sha256,
            cleanup_nonce,
        }));
        self.advance(&mut selection)?;
        self.publish(&selection, Some(&expected))?;
        let (_, reserved) = self.load()?;
        let tree = self.tree.create_child(&version_name(id))?;
        #[cfg(feature = "test-support")]
        test_support::created_version();
        let (device, inode) = tree.identity()?;
        let version = Version {
            id,
            digest: feed.document().archive.sha256,
            device,
            inode,
            cleanup_nonce,
        };
        self.directory.publish(&metadata_name(id), &bytes, None)?;
        #[cfg(feature = "test-support")]
        test_support::published_metadata();
        selection.preparing = Some(Preparation::Created(version));
        self.advance(&mut selection)?;
        self.publish(&selection, Some(&reserved))?;
        let (_, prepared) = self.load()?;
        // A crash during extraction retains this original candidate, never executable approval.
        tree.extract(artifact)?;
        self.check_native_signature(id, artifact.manifest())?;
        self.tree.sync()?;
        selection.preparing = None;
        selection.staged = Some(version);
        self.advance(&mut selection)?;
        self.publish(&selection, Some(&prepared))?;
        Ok(id)
    }
    /// Atomically selects the exact staged code and retains the previous approved version.
    /// The lifecycle owner must first obtain quiescence. Schema changes require its separate migration path.
    /// This operation supports only code that reads and writes the existing storage schema.
    pub fn activate(
        &self,
        id: InstallationId,
        generation: u64,
        storage_schema: u32,
    ) -> Result<(), ServiceFailure> {
        let _lock = self.tree.lock()?;
        let (mut selection, expected) = self.load()?;
        if selection.generation != generation {
            return Err(ServiceFailure::RequestConflict);
        }
        if selection.preparing.is_some() {
            return Err(ServiceFailure::Pending);
        }
        if selection.cleanup.is_some() {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        let staged = selection
            .staged
            .filter(|v| v.id == id)
            .ok_or(ServiceFailure::RequestConflict)?;
        self.compatible(staged, storage_schema)?;
        selection.cleanup = selection.previous;
        selection.previous = selection.approved;
        selection.approved = Some(staged);
        selection.staged = None;
        self.advance(&mut selection)?;
        self.publish(&selection, Some(&expected))
    }
    /// Explicitly selects the retained previous version after lifecycle quiescence and schema validation.
    pub fn rollback(&self, generation: u64, storage_schema: u32) -> Result<(), ServiceFailure> {
        let _lock = self.tree.lock()?;
        let (mut selection, expected) = self.load()?;
        if selection.generation != generation {
            return Err(ServiceFailure::RequestConflict);
        }
        if selection.preparing.is_some() {
            return Err(ServiceFailure::Pending);
        }
        if selection.cleanup.is_some() {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        let previous = selection.previous.ok_or(ServiceFailure::Missing)?;
        self.compatible(previous, storage_schema)?;
        selection.previous = selection.approved;
        selection.approved = Some(previous);
        self.advance(&mut selection)?;
        self.publish(&selection, Some(&expected))
    }
    /// Moves only the selected staged candidate into exact cleanup, leaving approval unchanged.
    pub fn discard_staged(
        &self,
        id: InstallationId,
        generation: u64,
    ) -> Result<(), ServiceFailure> {
        let _lock = self.tree.lock()?;
        let (mut selection, expected) = self.load()?;
        if selection.generation != generation || selection.staged.is_none_or(|v| v.id != id) {
            return Err(ServiceFailure::RequestConflict);
        }
        if selection.cleanup.is_some() {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        selection.cleanup = selection.staged.take();
        self.advance(&mut selection)?;
        self.publish(&selection, Some(&expected))
    }
    /// Explicitly abandons the retained interrupted preparation without retrying extraction.
    pub fn discard_preparing(
        &self,
        id: InstallationId,
        generation: u64,
    ) -> Result<(), ServiceFailure> {
        let _lock = self.tree.lock()?;
        let (mut selection, expected) = self.load()?;
        if selection.generation != generation || selection.preparing.is_none_or(|v| v.id() != id) {
            return Err(ServiceFailure::RequestConflict);
        }
        if selection.cleanup.is_some() {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        // The reservation does not authenticate a post-crash directory inode.
        // Original-phase creation reconciliation belongs to private_filesystem.
        let version = selection
            .preparing
            .and_then(Preparation::created)
            .ok_or(ServiceFailure::CleanupUnresolved)?;
        selection.preparing = None;
        selection.cleanup = Some(version);
        self.advance(&mut selection)?;
        self.publish(&selection, Some(&expected))
    }
    /// Removes only the journaled superseded tree and its signed metadata.
    /// Unlisted entries, replacement objects, links or unsafe ownership stop cleanup.
    pub fn cleanup(&self) -> Result<(), ServiceFailure> {
        let _lock = self.tree.lock()?;
        let (mut selection, expected) = self.load()?;
        let Some(version) = selection.cleanup else {
            return Ok(());
        };
        // A captured root from an interrupted removal is still the original
        // cleanup phase, not evidence that the version has been removed.
        if self.tree.child(&cleanup_name(version))?.is_some() {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        match self.metadata(version) {
            Ok((manifest, record)) => self.remove_version(version, &manifest, &record)?,
            // Absence is completion evidence only when both original entries are gone.
            Err(ServiceFailure::Missing)
                if self.tree.child(&version_name(version.id))?.is_none() => {}
            Err(error) => return Err(error),
        }
        self.tree.sync()?;
        selection.cleanup = None;
        self.advance(&mut selection)?;
        self.publish(&selection, Some(&expected))
    }
    /// Verifies the selected executable and resources again before native startup uses its local path.
    pub fn retain_approved(&self) -> Result<InstalledServerExecutable, ServiceFailure> {
        let _lock = self.tree.lock()?;
        let (selection, _) = self.load()?;
        let version = selection.approved.ok_or(ServiceFailure::Missing)?;
        let manifest = self.validate_version(version)?;
        let tree = self.version_tree(version)?.ok_or(ServiceFailure::Missing)?;
        let expected = manifest
            .files
            .iter()
            .find(|f| f.name == manifest.entry_points.server)
            .ok_or(ServiceFailure::ArtifactRejected)?;
        let file = tree.read_file(&expected.name, archive::installed_mode(expected.mode))?;
        self.directory.verify()?;
        Ok(InstalledServerExecutable {
            _file: file,
            path: self
                .path
                .join(version_name(version.id))
                .join(expected.name.as_str()),
            status: InstallationStatus {
                installation: version.id,
                version: manifest.version,
                artifact_sha256: version.digest,
            },
        })
    }
    fn compatible(&self, version: Version, schema: u32) -> Result<(), ServiceFailure> {
        let manifest = self.validate_version(version)?;
        if schema == 0
            || manifest.storage.write != schema
            || !manifest.storage.read.contains(&schema)
        {
            return Err(ServiceFailure::IncompatibleStorage);
        }
        Ok(())
    }
    fn validate_version(
        &self,
        version: Version,
    ) -> Result<AccountInstallationManifestV2, ServiceFailure> {
        let (manifest, _) = self.metadata(version)?;
        let tree = self.version_tree(version)?.ok_or(ServiceFailure::Missing)?;
        tree.validate(&manifest)?;
        self.check_native_signature(version.id, &manifest)?;
        self.version_tree(version)?.ok_or(ServiceFailure::Missing)?;
        self.directory.verify()?;
        Ok(manifest)
    }
    fn metadata(
        &self,
        version: Version,
    ) -> Result<(AccountInstallationManifestV2, Snapshot), ServiceFailure> {
        let record = self
            .directory
            .read(&metadata_name(version.id), METADATA_LIMIT)?
            .ok_or(ServiceFailure::Missing)?;
        let metadata: Metadata =
            serde_json::from_slice(&record.bytes).map_err(|_| ServiceFailure::InvalidRecord)?;
        let feed = verify_account_server_feed(
            &self.trust,
            channel(self.key.build_identity),
            platform()?,
            metadata.feed.as_bytes(),
            metadata.signature.as_bytes(),
            u64::MAX,
        )
        .map_err(|_| ServiceFailure::ArtifactRejected)?;
        if feed.document().archive.sha256 != version.digest {
            return Err(ServiceFailure::ArtifactRejected);
        }
        let manifest = verify_account_installation_manifest(&feed, metadata.manifest.as_bytes())
            .map_err(|_| ServiceFailure::ArtifactRejected)?;
        // Removal authority stays bound to the bytes and inode authenticated here.
        Ok((manifest, record))
    }
    fn version_tree(&self, version: Version) -> Result<Option<Tree>, ServiceFailure> {
        let tree = self.tree.child(&version_name(version.id))?;
        if let Some(tree) = &tree
            && tree.identity()? != (version.device, version.inode)
        {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        Ok(tree)
    }
    fn remove_version(
        &self,
        version: Version,
        manifest: &AccountInstallationManifestV2,
        metadata: &Snapshot,
    ) -> Result<(), ServiceFailure> {
        if let Some(tree) = self.version_tree(version)? {
            tree.remove_contents(manifest)?;
            #[cfg(feature = "test-support")]
            test_support::removing_version();
            self.tree
                .remove_child(&version_name(version.id), &cleanup_name(version), &tree)?;
            #[cfg(feature = "test-support")]
            test_support::removed_version();
        }
        self.directory
            .remove(&metadata_name(version.id), metadata)?;
        Ok(())
    }
    fn check_native_signature(
        &self,
        _id: InstallationId,
        _manifest: &AccountInstallationManifestV2,
    ) -> Result<(), ServiceFailure> {
        #[cfg(feature = "test-support")]
        if self.skip_native_signature {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        macos::verify(
            &self
                .path
                .join(version_name(_id))
                .join(_manifest.build_identity.bundle_name()),
            _manifest.build_identity,
        )?;
        Ok(())
    }
    fn load(&self) -> Result<(Selection, Snapshot), ServiceFailure> {
        if self.uncertain.load(Ordering::Acquire) {
            return Err(ServiceFailure::Indeterminate);
        }
        self.directory.verify()?;
        let record = self
            .directory
            .read("selection", STATE_LIMIT)?
            .ok_or(ServiceFailure::Missing)?;
        let selection: Selection =
            serde_json::from_slice(&record.bytes).map_err(|_| ServiceFailure::InvalidRecord)?;
        if selection.schema != 1 {
            return Err(ServiceFailure::UnsupportedSchema);
        }
        if selection.key != self.key {
            return Err(ServiceFailure::IdentityMismatch);
        }
        if selection.generation == 0 {
            return Err(ServiceFailure::InvalidRecord);
        }
        let mut ids = std::collections::BTreeSet::new();
        for version in [
            selection.approved,
            selection.previous,
            selection.staged,
            selection.preparing.and_then(Preparation::created),
            selection.cleanup,
        ]
        .into_iter()
        .flatten()
        {
            if version.id.0.get() > selection.high_water
                || version.inode == 0
                || !ids.insert(version.id.0.get())
            {
                return Err(ServiceFailure::InvalidRecord);
            }
        }
        if let Some(Preparation::Reserved(candidate)) = selection.preparing
            && (candidate.id.0.get() > selection.high_water || !ids.insert(candidate.id.0.get()))
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        if selection.preparing.is_some()
            && (selection.staged.is_some() || selection.cleanup.is_some())
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        if selection.previous.is_some() && selection.approved.is_none() {
            return Err(ServiceFailure::InvalidRecord);
        }
        Ok((selection, record))
    }
    fn advance(&self, selection: &mut Selection) -> Result<(), ServiceFailure> {
        selection.generation = selection
            .generation
            .checked_add(1)
            .ok_or(ServiceFailure::LimitExceeded)?;
        Ok(())
    }
    fn publish(
        &self,
        selection: &Selection,
        expected: Option<&Snapshot>,
    ) -> Result<(), ServiceFailure> {
        let bytes = serde_json::to_vec(selection).map_err(|_| ServiceFailure::InvalidRecord)?;
        if bytes.len() > STATE_LIMIT {
            return Err(ServiceFailure::LimitExceeded);
        }
        match self.directory.publish("selection", &bytes, expected) {
            Err(Failure::Indeterminate) => {
                self.uncertain.store(true, Ordering::Release);
                Err(ServiceFailure::Indeterminate)
            }
            result => result.map_err(Into::into),
        }
    }
}

/// Verified local executable with a retained inode. Debug never emits its native path.
pub struct InstalledServerExecutable {
    _file: std::fs::File,
    path: PathBuf,
    status: InstallationStatus,
}
impl InstalledServerExecutable {
    /// Returns the exact selected installation identity.
    pub fn installation(&self) -> InstallationId {
        self.status.installation
    }
    /// Returns the original signed artifact facts.
    pub fn status(&self) -> InstallationStatus {
        self.status.clone()
    }
    /// Returns a locally admitted path for account service registration, not remote file authority.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
impl fmt::Debug for InstalledServerExecutable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("InstalledServerExecutable(..)")
    }
}
impl fmt::Debug for ServerInstallationStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ServerInstallationStore(..)")
    }
}
fn channel(build: BuildIdentity) -> BuildChannel {
    match build {
        BuildIdentity::Release => BuildChannel::Release,
        BuildIdentity::Preflight => BuildChannel::Preflight,
        BuildIdentity::Development => BuildChannel::Development,
    }
}
fn platform() -> Result<ServerPlatform, ServiceFailure> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(ServerPlatform::DarwinArm64)
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(ServerPlatform::LinuxX86_64)
    } else {
        Err(ServiceFailure::UnsupportedHost)
    }
}
fn version_name(id: InstallationId) -> String {
    format!("version-{}", id.0.get())
}
fn metadata_name(id: InstallationId) -> String {
    format!("metadata-{}", id.0.get())
}
fn cleanup_name(version: Version) -> String {
    let nonce = u128::from_be_bytes(version.cleanup_nonce);
    format!(".cleanup-version-{}-{nonce:032x}", version.id.0.get())
}

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    //! Isolated account composition. The test key and code-signature bypass do not exist in production.
    use super::*;
    use std::cell::RefCell;
    thread_local! {
        static BEFORE_FILE_REMOVAL: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
        static BEFORE_VERSION_REMOVAL: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
        static AFTER_VERSION_REMOVAL: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
        static AFTER_VERSION_CREATION: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
        static AFTER_METADATA_PUBLICATION: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
        static PARTIAL_COPY: RefCell<Option<CopyInterruption>> = const { RefCell::new(None) };
    }
    struct CopyInterruption {
        bytes: u64,
        operation: Box<dyn FnOnce()>,
    }
    /// Writes a real prefix of the next payload, then interrupts that copy.
    pub fn after_next_partial_copy(bytes: u64, operation: impl FnOnce() + 'static) {
        PARTIAL_COPY.with(|slot| {
            *slot.borrow_mut() = Some(CopyInterruption {
                bytes,
                operation: Box::new(operation),
            })
        });
    }
    pub(super) fn copying(
        reader: &mut impl std::io::Read,
        file: &mut std::fs::File,
    ) -> Result<(), ServiceFailure> {
        use std::io::Read as _;
        let interruption = PARTIAL_COPY.with(|slot| slot.borrow_mut().take());
        if let Some(interruption) = interruption {
            std::io::copy(&mut reader.take(interruption.bytes), file)
                .map_err(|_| ServiceFailure::StorageUnavailable)?;
            (interruption.operation)();
            return Err(ServiceFailure::StorageUnavailable);
        }
        Ok(())
    }
    /// Runs after the native version mkdir, before its next durable checkpoint.
    pub fn after_next_version_creation(operation: impl FnOnce() + 'static) {
        AFTER_VERSION_CREATION.with(|slot| *slot.borrow_mut() = Some(Box::new(operation)));
    }
    /// Runs after signed metadata has completed private-file publication.
    pub fn after_next_metadata_publication(operation: impl FnOnce() + 'static) {
        AFTER_METADATA_PUBLICATION.with(|slot| *slot.borrow_mut() = Some(Box::new(operation)));
    }
    pub(super) fn created_version() {
        AFTER_VERSION_CREATION.with(|slot| {
            let operation = slot.borrow_mut().take();
            if let Some(operation) = operation {
                operation();
            }
        });
    }
    pub(super) fn published_metadata() {
        AFTER_METADATA_PUBLICATION.with(|slot| {
            let operation = slot.borrow_mut().take();
            if let Some(operation) = operation {
                operation();
            }
        });
    }
    /// Runs at the cleanup boundary after retaining the file being removed.
    pub fn before_next_file_removal(operation: impl FnOnce() + 'static) {
        BEFORE_FILE_REMOVAL.with(|slot| *slot.borrow_mut() = Some(Box::new(operation)));
    }
    /// Runs at the cleanup boundary after retaining the version directory being removed.
    pub fn before_next_version_removal(operation: impl FnOnce() + 'static) {
        BEFORE_VERSION_REMOVAL.with(|slot| *slot.borrow_mut() = Some(Box::new(operation)));
    }
    pub(super) fn removing_file() {
        BEFORE_FILE_REMOVAL.with(|slot| {
            let operation = slot.borrow_mut().take();
            if let Some(operation) = operation {
                operation();
            }
        });
    }
    pub(super) fn removing_version() {
        BEFORE_VERSION_REMOVAL.with(|slot| {
            let operation = slot.borrow_mut().take();
            if let Some(operation) = operation {
                operation();
            }
        });
    }
    /// Runs after the version directory is removed and synchronized, before metadata removal.
    pub fn after_next_version_removal(operation: impl FnOnce() + 'static) {
        AFTER_VERSION_REMOVAL.with(|slot| *slot.borrow_mut() = Some(Box::new(operation)));
    }
    pub(super) fn removed_version() {
        AFTER_VERSION_REMOVAL.with(|slot| {
            let operation = slot.borrow_mut().take();
            if let Some(operation) = operation {
                operation();
            }
        });
    }
    /// Holds only the real installer mutation lock at a controlled native fork boundary.
    pub fn with_installation_lock<T>(
        store: &ServerInstallationStore,
        operation: impl FnOnce() -> T,
    ) -> Result<T, ServiceFailure> {
        let _lock = store.tree.lock()?;
        Ok(operation())
    }
    /// Runs a controlled process-interruption hook after the next private publication.
    pub fn after_next_publication(operation: impl FnOnce() + 'static) {
        crate::private_filesystem::test_support::after_next_publication(operation);
    }
    /// Uses an already-created test registration and an injected Ed25519 test public key.
    /// Archive signatures, layout, architecture and native filesystem checks remain real.
    pub fn store(
        registered: &RegisteredServer,
        key: UpdateKey,
    ) -> Result<ServerInstallationStore, ServiceFailure> {
        ServerInstallationStore::open_with_trust(registered, key, true)
    }
}
