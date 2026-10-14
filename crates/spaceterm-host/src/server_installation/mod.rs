//! Signed account-private server code, independent of desktop installation.
//!
//! Selection uses atomic publication with durable original-transition recovery. This module never starts or
//! stops a process and never changes deliberate lifecycle intent. The account
//! service must quiesce the old runtime before selecting another installation.

mod admission;
mod archive;
mod journal;
#[cfg(target_os = "macos")]
mod macos;
mod records;

use crate::{
    account_service::{RegisteredServer, ServiceFailure},
    approved_installation::{AccountInstallationManifestV2, verify_account_installation_manifest},
    private_filesystem::Directory,
    startup::{AccountBuildKey, BuildIdentity, InstallationId},
};
pub use admission::{InstallationAdmission, InstallationOperation, InstallationScope};
use archive::Tree;
use journal::Journal;
use records::Record;
use serde::{Deserialize, Serialize};
use spaceterm_distribution::{
    BuildChannel, ServerPlatform, UpdateKey, VerifiedAccountServerArtifact,
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
    #[serde(default)]
    incomplete: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

/// Original interrupted code publication or removal, not new mutation authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterruptedInstallationStatus {
    /// Exact candidate named by the retained selection journal.
    pub installation: InstallationId,
    /// Original authenticated archive digest.
    pub artifact_sha256: [u8; 32],
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
    pub fn open(scope: &InstallationScope<'_>) -> Result<Self, ServiceFailure> {
        let trust = UpdateKey::release().ok_or(ServiceFailure::ArtifactRejected)?;
        Self::open_with_trust(scope, trust, false)
    }
    fn open_with_trust(
        scope: &InstallationScope<'_>,
        trust: UpdateKey,
        _test_signature: bool,
    ) -> Result<Self, ServiceFailure> {
        scope.verify(scope.registered.key(), InstallationOperation::Open)?;
        Self::open_storage(scope.registered, trust, _test_signature, true)
    }
    /// Opens established selection, reconciling only an interrupted original publication.
    /// This never creates code storage, replaces missing selection, or activates staged code.
    /// Startup calls this only after the account provider's instance and desired-state admission.
    pub fn open_existing(registered: &RegisteredServer) -> Result<Self, ServiceFailure> {
        let trust = UpdateKey::release().ok_or(ServiceFailure::ArtifactRejected)?;
        Self::open_storage(registered, trust, false, false)
    }
    fn open_storage(
        registered: &RegisteredServer,
        trust: UpdateKey,
        _test_signature: bool,
        initialize: bool,
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
        let directory = if initialize {
            Directory::create(&path)?
        } else {
            Directory::open(&path)?.ok_or(ServiceFailure::Missing)?
        };
        let tree = Tree::open(&path, &directory)?;
        if tree.record("lock", 1)?.is_none() {
            if !initialize {
                return Err(ServiceFailure::Missing);
            }
            match tree.publish_record("lock", b"", None) {
                Ok(())
                | Err(ServiceFailure::RequestConflict | ServiceFailure::CleanupUnresolved) => (),
                Err(error) => return Err(error),
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
        store.tree.reconcile_selection()?;
        if store.tree.record("selection", STATE_LIMIT)?.is_none() {
            if !initialize {
                return Err(ServiceFailure::Missing);
            }
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
    fn mutation<T>(
        &self,
        scope: &InstallationScope<'_>,
        operation: InstallationOperation,
        effect: impl FnOnce() -> Result<T, ServiceFailure>,
    ) -> Result<T, ServiceFailure> {
        scope.verify(self.key, operation)?;
        if self.uncertain.load(Ordering::Acquire) {
            return Err(ServiceFailure::Indeterminate);
        }
        if scope.registered.data_directory().join("installations") != self.path {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let result = effect();
        if result.as_ref().err() == Some(&ServiceFailure::Indeterminate) {
            self.uncertain.store(true, Ordering::Release);
        }
        result
    }
    /// Stages only the artifact bound to the account service's original admission.
    pub fn stage(
        &self,
        scope: &InstallationScope<'_>,
        artifact: &VerifiedServerArtifact,
    ) -> Result<InstallationId, ServiceFailure> {
        self.mutation(
            scope,
            InstallationOperation::Stage {
                artifact_sha256: artifact.artifact_sha256(),
            },
            || self.stage_inner(artifact),
        )
    }
    /// Selects staged code only after the lifecycle provider confirms quiescence.
    pub fn activate(
        &self,
        scope: &InstallationScope<'_>,
        id: InstallationId,
        generation: u64,
        storage_schema: u32,
    ) -> Result<(), ServiceFailure> {
        self.mutation(
            scope,
            InstallationOperation::Activate {
                installation: id,
                generation,
                storage_schema,
            },
            || self.activate_inner(id, generation, storage_schema),
        )
    }
    /// Selects retained rollback code under the original lifecycle admission.
    pub fn rollback(
        &self,
        scope: &InstallationScope<'_>,
        generation: u64,
        storage_schema: u32,
    ) -> Result<(), ServiceFailure> {
        self.mutation(
            scope,
            InstallationOperation::Rollback {
                generation,
                storage_schema,
            },
            || self.rollback_inner(generation, storage_schema),
        )
    }
    /// Abandons only the account service's admitted staged candidate.
    pub fn discard_staged(
        &self,
        scope: &InstallationScope<'_>,
        id: InstallationId,
        generation: u64,
    ) -> Result<(), ServiceFailure> {
        self.mutation(
            scope,
            InstallationOperation::Discard {
                installation: id,
                generation,
            },
            || self.discard_staged_inner(id, generation),
        )
    }
    /// Abandons only the original interrupted preparation without repeating extraction.
    pub fn discard_preparing(
        &self,
        scope: &InstallationScope<'_>,
        id: InstallationId,
        generation: u64,
    ) -> Result<(), ServiceFailure> {
        self.mutation(
            scope,
            InstallationOperation::Discard {
                installation: id,
                generation,
            },
            || self.discard_preparing_inner(id, generation),
        )
    }
    /// Finishes already-journaled cleanup under account-service admission.
    pub fn cleanup(&self, scope: &InstallationScope<'_>) -> Result<(), ServiceFailure> {
        self.mutation(scope, InstallationOperation::Cleanup, || {
            self.cleanup_inner()
        })
    }
    /// Removes verified code under confirmed account lifecycle admission, retaining all server data.
    pub fn uninstall(&self, scope: &InstallationScope<'_>) -> Result<(), ServiceFailure> {
        self.mutation(scope, InstallationOperation::Uninstall, || {
            loop {
                scope.verify(self.key, InstallationOperation::Uninstall)?;
                self.cleanup_inner()?;
                let lock = self.tree.lock()?;
                let (mut selection, expected) = self.load()?;
                if let Some(preparing) = selection.preparing {
                    let generation = selection.generation;
                    drop(lock);
                    self.discard_preparing_inner(preparing.id(), generation)?;
                    continue;
                }
                let version = selection
                    .staged
                    .take()
                    .or_else(|| selection.previous.take())
                    .or_else(|| selection.approved.take());
                let Some(version) = version else {
                    return Ok(());
                };
                // Reuse the durable original cleanup journal. Interrupted removal
                // resumes this exact inode, never a directory scan or a new candidate.
                self.validate_version(version)?;
                selection.cleanup = Some(version);
                self.advance(&mut selection)?;
                self.publish(&selection, Some(&expected))?;
            }
        })
    }
    /// Returns only the original preparing candidate, never a directory-scan result.
    pub fn preparing_status(
        &self,
    ) -> Result<Option<InterruptedInstallationStatus>, ServiceFailure> {
        let _lock = self.tree.lock()?;
        let (selection, _) = self.load()?;
        Ok(selection.preparing.map(|preparing| {
            let (installation, artifact_sha256) = match preparing {
                Preparation::Created(version) => (version.id, version.digest),
                Preparation::Reserved(candidate) => (candidate.id, candidate.digest),
            };
            InterruptedInstallationStatus {
                installation,
                artifact_sha256,
            }
        }))
    }
    /// Returns only the already journaled cleanup candidate.
    pub fn cleanup_status(&self) -> Result<Option<InterruptedInstallationStatus>, ServiceFailure> {
        let _lock = self.tree.lock()?;
        let (selection, _) = self.load()?;
        Ok(selection
            .cleanup
            .map(|version| InterruptedInstallationStatus {
                installation: version.id,
                artifact_sha256: version.digest,
            }))
    }
    /// Returns authenticated staged facts without activating or executing the candidate.
    pub fn staged_status(&self) -> Result<Option<InstallationStatus>, ServiceFailure> {
        let _lock = self.tree.lock()?;
        let (selection, _) = self.load()?;
        let Some(version) = selection.staged else {
            return Ok(None);
        };
        let manifest = self.validate_version(version)?;
        Ok(Some(InstallationStatus {
            installation: version.id,
            version: manifest.version,
            artifact_sha256: version.digest,
        }))
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
    fn stage_inner(
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
        let candidate = Candidate {
            id,
            digest: feed.document().archive.sha256,
            cleanup_nonce,
        };
        let (mut journal, tree) = Journal::create(&self.tree, candidate)?;
        let version = journal.version();
        selection.preparing = Some(Preparation::Created(version));
        self.advance(&mut selection)?;
        self.publish(&selection, Some(&reserved))?;
        #[cfg(feature = "test-support")]
        test_support::created_version();
        self.tree.publish_record(&metadata_name(id), &bytes, None)?;
        #[cfg(feature = "test-support")]
        test_support::published_metadata();
        let (_, prepared) = self.load()?;
        // A crash during extraction retains this original candidate, never executable approval.
        tree.extract(artifact, &self.tree, &mut journal)?;
        self.check_native_signature(id, artifact.manifest())?;
        self.tree.sync()?;
        selection.preparing = None;
        selection.staged = Some(Version {
            incomplete: false,
            ..version
        });
        self.advance(&mut selection)?;
        self.publish(&selection, Some(&prepared))?;
        Ok(id)
    }
    /// Atomically selects the exact staged code and retains the previous approved version.
    /// The lifecycle owner must first obtain quiescence. Schema changes require its separate migration path.
    /// This operation supports only code that reads and writes the existing storage schema.
    fn activate_inner(
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
    fn rollback_inner(&self, generation: u64, storage_schema: u32) -> Result<(), ServiceFailure> {
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
    fn discard_staged_inner(
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
    fn discard_preparing_inner(
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
        let preparation = selection.preparing.ok_or(ServiceFailure::RequestConflict)?;
        let version = match preparation {
            Preparation::Created(version) => version,
            Preparation::Reserved(candidate) => {
                let Some(journal) = Journal::read(&self.tree, candidate)? else {
                    // No version was exposed before its creation identity became durable.
                    // Unrecorded private temporaries grant no removal authority.
                    if self.tree.entry_identity(&version_name(id))?.is_some() {
                        return Err(ServiceFailure::CleanupUnresolved);
                    }
                    selection.preparing = None;
                    self.advance(&mut selection)?;
                    return self.publish(&selection, Some(&expected));
                };
                journal.version()
            }
        };
        if let Some(journal) = Journal::read(&self.tree, candidate(version))? {
            if journal.version().device != version.device
                || journal.version().inode != version.inode
            {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            journal.tree(&self.tree)?;
        } else {
            self.version_tree(version)?;
        }
        selection.preparing = None;
        selection.cleanup = Some(version);
        self.advance(&mut selection)?;
        self.publish(&selection, Some(&expected))
    }
    /// Removes only the journaled superseded tree and its signed metadata.
    /// Unlisted entries, replacement objects, links or unsafe ownership stop cleanup.
    fn cleanup_inner(&self) -> Result<(), ServiceFailure> {
        let _lock = self.tree.lock()?;
        let (mut selection, expected) = self.load()?;
        let Some(version) = selection.cleanup else {
            return Ok(());
        };
        let journal = Journal::read(&self.tree, candidate(version))?;
        let root_absent = match &journal {
            Some(journal) => journal.tree(&self.tree)?.is_none(),
            None => {
                self.version_tree(version)?.is_none()
                    && self.tree.entry_identity(&cleanup_name(version))?.is_none()
            }
        };
        if version.incomplete && journal.is_none() && !root_absent {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        let metadata = match self.metadata(version) {
            Ok(value) => Some(value),
            Err(ServiceFailure::Missing) if version.incomplete => None,
            Err(ServiceFailure::Missing) if root_absent => None,
            Err(error) => return Err(error),
        };
        if let Some(mut journal) = journal {
            if journal.version().device != version.device
                || journal.version().inode != version.inode
            {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            journal.cleanup(
                &self.tree,
                metadata.as_ref().map(|(manifest, _)| manifest),
                version.incomplete,
            )?;
            if let Some((_, record)) = &metadata {
                self.tree
                    .remove_record(&metadata_name(version.id), record)?;
                #[cfg(feature = "test-support")]
                test_support::cleanup_boundary(7);
            }
            journal.remove(&self.tree)?;
            #[cfg(feature = "test-support")]
            test_support::cleanup_boundary(8);
        } else if let Some((manifest, record)) = metadata {
            self.remove_version(version, &manifest, &record)?;
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
        self.retain_version(version)
    }
    fn retain_version(
        &self,
        version: Version,
    ) -> Result<InstalledServerExecutable, ServiceFailure> {
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
            protocol_majors: manifest.protocol_majors,
        })
    }
    /// Verifies that approved code implements the retained organization schema.
    pub fn retain_compatible_approved(
        &self,
        schema: u32,
    ) -> Result<InstalledServerExecutable, ServiceFailure> {
        let _lock = self.tree.lock()?;
        let (selection, _) = self.load()?;
        let version = selection.approved.ok_or(ServiceFailure::Missing)?;
        self.compatible(version, schema)?;
        self.retain_version(version)
    }
    /// Authenticates the exact staged candidate without approving or executing it.
    pub fn retain_compatible_staged(
        &self,
        id: InstallationId,
        schema: u32,
    ) -> Result<InstalledServerExecutable, ServiceFailure> {
        let _lock = self.tree.lock()?;
        let (selection, _) = self.load()?;
        let version = selection
            .staged
            .filter(|version| version.id == id)
            .ok_or(ServiceFailure::RequestConflict)?;
        self.compatible(version, schema)?;
        self.retain_version(version)
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
    ) -> Result<(AccountInstallationManifestV2, Record), ServiceFailure> {
        let record = self
            .tree
            .record(&metadata_name(version.id), METADATA_LIMIT)?
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
        metadata: &Record,
    ) -> Result<(), ServiceFailure> {
        if let Some(tree) = self.version_tree(version)? {
            let mut journal = Journal::retain_legacy(&self.tree, version, &tree, manifest)?;
            journal.cleanup(&self.tree, Some(manifest), version.incomplete)?;
            self.tree
                .remove_record(&metadata_name(version.id), metadata)?;
            return journal.remove(&self.tree);
        }
        // The old root capture has a durable, deterministic name and recorded
        // identity. Unknown old child capture names never grant removal authority.
        let captured = cleanup_name(version);
        if let Some(tree) = self.tree.child(&captured)? {
            if tree.identity()? != (version.device, version.inode) {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            tree.require_empty()?;
            self.tree
                .unlink_expected(&captured, true, (version.device, version.inode))?;
            self.tree.sync()?;
        }
        self.tree
            .remove_record(&metadata_name(version.id), metadata)?;
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
    fn load(&self) -> Result<(Selection, Record), ServiceFailure> {
        if self.uncertain.load(Ordering::Acquire) {
            return Err(ServiceFailure::Indeterminate);
        }
        self.directory.verify()?;
        if let Err(error) = self.tree.reconcile_selection() {
            if error == ServiceFailure::Indeterminate {
                self.uncertain.store(true, Ordering::Release);
            }
            return Err(error);
        }
        let record = self
            .tree
            .record("selection", STATE_LIMIT)?
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
        expected: Option<&Record>,
    ) -> Result<(), ServiceFailure> {
        let bytes = serde_json::to_vec(selection).map_err(|_| ServiceFailure::InvalidRecord)?;
        if bytes.len() > STATE_LIMIT {
            return Err(ServiceFailure::LimitExceeded);
        }
        match self.tree.publish_selection(&bytes, expected) {
            Err(ServiceFailure::Indeterminate) => {
                self.uncertain.store(true, Ordering::Release);
                Err(ServiceFailure::Indeterminate)
            }
            result => result,
        }
    }
}

/// Verified local executable with a retained inode. Debug never emits its native path.
pub struct InstalledServerExecutable {
    _file: std::fs::File,
    path: PathBuf,
    status: InstallationStatus,
    protocol_majors: Vec<u16>,
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
    /// Returns compatibility from this exact selected artifact's authenticated manifest.
    pub fn protocol_majors(&self) -> &[u16] {
        &self.protocol_majors
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
fn candidate(version: Version) -> Candidate {
    Candidate {
        id: version.id,
        digest: version.digest,
        cleanup_nonce: version.cleanup_nonce,
    }
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
        static AFTER_SELECTION_PUBLICATION: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
        static FAIL_PUBLICATION_SYNC: RefCell<bool> = const { RefCell::new(false) };
    }
    thread_local! {
        static CLEANUP_PROBE: RefCell<Option<CleanupProbe>> = const { RefCell::new(None) };
        static FAIL_TREE_SYNC: RefCell<bool> = const { RefCell::new(false) };
        static FAIL_FILE_SYNC: RefCell<Option<(u64, u64)>> = const { RefCell::new(None) };
    }
    struct CleanupProbe {
        boundary: u8,
        operation: Box<dyn FnOnce()>,
    }
    pub fn at_cleanup_boundary(boundary: u8, operation: impl FnOnce() + 'static) {
        CLEANUP_PROBE.with(|slot| {
            *slot.borrow_mut() = Some(CleanupProbe {
                boundary,
                operation: Box::new(operation),
            })
        });
    }
    pub(super) fn cleanup_boundary(boundary: u8) {
        let operation = CLEANUP_PROBE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|probe| probe.boundary == boundary)
            {
                slot.take()
            } else {
                None
            }
        });
        if let Some(probe) = operation {
            (probe.operation)();
        }
    }
    pub fn fail_next_tree_sync() {
        FAIL_TREE_SYNC.with(|slot| *slot.borrow_mut() = true);
    }
    pub(super) fn tree_sync() -> Result<(), ServiceFailure> {
        if FAIL_TREE_SYNC.with(|slot| slot.replace(false)) {
            Err(ServiceFailure::Indeterminate)
        } else {
            Ok(())
        }
    }
    /// Reports uncertainty at the next native regular-file sync of this exact inode.
    pub fn fail_next_file_sync_for(device: u64, inode: u64) {
        FAIL_FILE_SYNC.with(|slot| *slot.borrow_mut() = Some((device, inode)));
    }
    pub(super) fn file_sync(file: &std::fs::File) -> Result<(), ServiceFailure> {
        FAIL_FILE_SYNC.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_some() && *slot == Some(records::Identity::of(file)?.tuple()) {
                slot.take();
                Err(ServiceFailure::Indeterminate)
            } else {
                Ok(())
            }
        })
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
        AFTER_SELECTION_PUBLICATION.with(|slot| *slot.borrow_mut() = Some(Box::new(operation)));
    }
    /// Reports uncertainty after atomic selection publication, before parent synchronization.
    pub fn fail_next_publication_durability() {
        FAIL_PUBLICATION_SYNC.with(|slot| *slot.borrow_mut() = true);
    }
    pub(super) fn published_selection() {
        let operation = AFTER_SELECTION_PUBLICATION.with(|slot| slot.borrow_mut().take());
        if let Some(operation) = operation {
            operation();
        }
    }
    pub(super) fn publication_sync() -> Result<(), ServiceFailure> {
        if FAIL_PUBLICATION_SYNC.with(|slot| slot.replace(false)) {
            Err(ServiceFailure::Indeterminate)
        } else {
            Ok(())
        }
    }
    /// Uses an already-created test registration and an injected Ed25519 test public key.
    /// Archive signatures, layout, architecture and native filesystem checks remain real.
    pub fn store(
        registered: &RegisteredServer,
        key: UpdateKey,
    ) -> Result<TestInstallationStore<'_>, ServiceFailure> {
        let scope = InstallationScope::new(registered, &TEST_ADMISSION)?;
        let inner = ServerInstallationStore::open_with_trust(&scope, key, true)?;
        Ok(TestInstallationStore { inner, scope })
    }
    /// Opens the real store with an independently supplied lifecycle gate.
    pub fn store_with_scope(
        scope: &InstallationScope<'_>,
        key: UpdateKey,
    ) -> Result<ServerInstallationStore, ServiceFailure> {
        ServerInstallationStore::open_with_trust(scope, key, true)
    }
    /// Opens established code storage without test lifecycle permission or publication.
    pub fn existing_store(
        registered: &RegisteredServer,
        key: UpdateKey,
    ) -> Result<ServerInstallationStore, ServiceFailure> {
        ServerInstallationStore::open_storage(registered, key, true, false)
    }
    struct TestAdmission;
    static TEST_ADMISSION: TestAdmission = TestAdmission;
    impl InstallationAdmission for TestAdmission {
        fn verify(
            &self,
            _: &RegisteredServer,
            _: InstallationOperation,
        ) -> Result<(), ServiceFailure> {
            Ok(())
        }
    }
    /// Test-only lifecycle composition. No permissive admission is linked in production.
    pub struct TestInstallationStore<'a> {
        inner: ServerInstallationStore,
        scope: InstallationScope<'a>,
    }
    impl std::ops::Deref for TestInstallationStore<'_> {
        type Target = ServerInstallationStore;
        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl TestInstallationStore<'_> {
        pub fn stage(
            &self,
            artifact: &VerifiedServerArtifact,
        ) -> Result<InstallationId, ServiceFailure> {
            self.inner.stage(&self.scope, artifact)
        }
        pub fn activate(
            &self,
            id: InstallationId,
            generation: u64,
            schema: u32,
        ) -> Result<(), ServiceFailure> {
            self.inner.activate(&self.scope, id, generation, schema)
        }
        pub fn rollback(&self, generation: u64, schema: u32) -> Result<(), ServiceFailure> {
            self.inner.rollback(&self.scope, generation, schema)
        }
        pub fn discard_staged(
            &self,
            id: InstallationId,
            generation: u64,
        ) -> Result<(), ServiceFailure> {
            self.inner.discard_staged(&self.scope, id, generation)
        }
        pub fn discard_preparing(
            &self,
            id: InstallationId,
            generation: u64,
        ) -> Result<(), ServiceFailure> {
            self.inner.discard_preparing(&self.scope, id, generation)
        }
        pub fn cleanup(&self) -> Result<(), ServiceFailure> {
            self.inner.cleanup(&self.scope)
        }
    }
}
