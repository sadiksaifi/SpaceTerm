//! Fresh publication and retained execution. Native peers supply policy, not comparison facts.

use std::{
    fs::File,
    path::Path,
    sync::{Arc, Mutex},
};

use super::contracts::*;
use crate::{
    approved_installation::{ApprovedInstallationDescriptor, RetainedApprovedInstallation},
    installation::RetainedRegistration,
    native_registration::writer::PublicationState,
    protected_directory::NativeDirectoryProtection,
};
use sha2::{Digest, Sha256};
use spaceterm_distribution::{BuildChannel, InstallationManifestV1, VerifiedServerArchive};

mod archive;
mod filesystem;

pub(crate) struct RetainedPublication {
    tree: filesystem::DirectoryTree,
    files: Vec<filesystem::BoundFile>,
    manifest: InstallationManifestV1,
    closure_sha256: [u8; 32],
}
impl RetainedPublication {
    pub(crate) fn verify(&self) -> Result<(), NativeFailure> {
        if authenticate_closure(&self.tree, &self.files, &self.manifest)? != self.closure_sha256 {
            return Err(NativeFailure::ArtifactRejected);
        }
        Ok(())
    }
    pub(crate) fn server(&self) -> Result<&File, NativeFailure> {
        self.verify()?;
        self.file(&self.manifest.entry_points.server)
    }
    pub(crate) fn credential_tool(&self) -> Result<&File, NativeFailure> {
        self.verify()?;
        self.file(
            self.manifest
                .entry_points
                .credential_tool
                .as_deref()
                .ok_or(NativeFailure::UnsupportedHost)?,
        )
    }
    fn file(&self, name: &str) -> Result<&File, NativeFailure> {
        self.files
            .iter()
            .find(|f| f.relative == name)
            .map(|f| &f.file)
            .ok_or(NativeFailure::ExecutionBindingUnproven)
    }
}
fn authenticate_closure(
    tree: &filesystem::DirectoryTree,
    files: &[filesystem::BoundFile],
    manifest: &InstallationManifestV1,
) -> Result<[u8; 32], NativeFailure> {
    // Linux can preserve identical ctime samples across a rename-and-restore interval.
    // Observe retained inodes across all native policy calls; samples are not mutation generations.
    // Observation still does not replace native publication policy or writable-alias exclusion.
    let observations = tree.observe(files)?;
    tree.verify(files)?;
    let digest = closure_digest(tree, files, manifest)?;
    tree.verify(files)?;
    for observation in observations {
        observation.finish().map_err(filesystem::protection)?;
    }
    Ok(digest)
}
fn closure_digest(
    tree: &filesystem::DirectoryTree,
    files: &[filesystem::BoundFile],
    manifest: &InstallationManifestV1,
) -> Result<[u8; 32], NativeFailure> {
    let views: Vec<_> = manifest
        .files
        .iter()
        .map(|expected| {
            files
                .iter()
                .find(|file| file.relative == expected.name)
                .map(filesystem::BoundFile::view)
                .ok_or(NativeFailure::ArtifactRejected)
        })
        .collect::<Result<_, _>>()?;
    let native = tree.protection.authenticate_execution(manifest, &views)?;
    nonzero(&native)?;
    let mut hash = Sha256::new();
    hash.update(b"SpaceTerm retained execution closure v1\0");
    hash.update(native);
    for file in &manifest.files {
        hash.update((file.name.len() as u64).to_le_bytes());
        hash.update(file.name.as_bytes());
        hash.update(file.mode.to_le_bytes());
        hash.update(file.size.to_le_bytes());
        hash.update(file.sha256);
    }
    Ok(hash.finalize().into())
}

/// Allocation and account facts read under the installation owner's native transaction.
/// These facts alone cannot construct installation authority.
#[derive(Clone)]
pub struct PublicationIdentity {
    /// Registered account and build.
    pub key: AccountBuildKey,
    /// Nonreused allocation reserved before destination creation.
    pub installation: InstallationId,
    /// Retained server identity.
    pub server_id: [u8; 16],
    /// Native account incarnation binding.
    pub account_binding_sha256: [u8; 32],
    /// Exact protected profile binding.
    pub registration_profile_sha256: [u8; 32],
    /// Native cryptographic nonce reserved with the allocation.
    pub publication_nonce: [u8; 32],
}
impl std::fmt::Debug for PublicationIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PublicationIdentity(..)")
    }
}

/// An authenticated destination file borrowed by the native code-policy inspector.
/// Names are manifest-relative values, never caller-selected privileged paths.
pub struct PublicationFile<'a> {
    pub(crate) name: &'a str,
    pub(crate) file: &'a File,
}
impl PublicationFile<'_> {
    /// Returns the authenticated manifest-relative name for dependency resolution.
    pub fn name(&self) -> &str {
        self.name
    }
    /// Returns the exact read-only destination descriptor, after staging writers closed.
    pub fn file(&self) -> &File {
        self.file
    }
}

/// Creation evidence issued only by the descriptor-relative fresh publisher.
/// A decoded descriptor, retained source, or permission change cannot construct this value.
pub struct FreshPublicationInode<'a> {
    parent: &'a File,
    destination: &'a File,
}
impl<'a> FreshPublicationInode<'a> {
    pub(crate) fn new(parent: &'a File, destination: &'a File) -> Self {
        Self {
            parent,
            destination,
        }
    }
    /// Returns the retained protected parent of the exclusive native creation.
    pub fn parent(&self) -> &File {
        self.parent
    }
    /// Returns the fresh destination. For files, all publisher writers are already closed.
    pub fn destination(&self) -> &File {
        self.destination
    }
}

/// Trusted native boundary retained through resource use, independently of transaction locks.
/// No default implementation converts DAC samples, hashes, or mount queries into policy.
pub trait NativePublicationProtection: NativeDirectoryProtection + Send + Sync {
    /// Binds genuine fresh creation to the native issuer's retained publication history.
    /// The issuer also verifies its trusted parent/storage policy; freshness alone excludes no aliases.
    fn record_fresh(&self, inode: &FreshPublicationInode<'_>) -> Result<(), NativeFailure>;
    /// Revalidates trusted publication policy and descriptor-bound mount evidence for every handle.
    /// Linux must exclude writable aliases, retain original mounts/namespaces/storage/boot,
    /// and cover every inode. DAC ownership alone is insufficient.
    fn verify_custody(&self, handles: &[&File]) -> Result<(), NativeFailure>;
    /// Authenticates the exact destination execution closure, including native code policy,
    /// interpreter, libraries, providers, configuration and resources. Returns its native binding.
    /// Path-only inspection of an account-writable copy is forbidden.
    fn authenticate_execution(
        &self,
        manifest: &InstallationManifestV1,
        files: &[PublicationFile<'_>],
    ) -> Result<[u8; 32], NativeFailure>;
}

/// Operation-scoped native serialization. Its destructor releases the native lock descriptors.
/// Custody outlives this guard, so Stage remains possible while approved code is running.
pub trait NativePublicationTransaction: Send + Sync {
    /// Revalidates the installation owner's retained transaction and journal state.
    fn revalidate(&self) -> Result<(), NativeFailure>;
}

/// Retained publication parent, custody policy and operation-only serialization.
/// Its constructor traverses and verifies all ancestors; it never adopts a generation.
pub struct PublicationScope {
    pub(crate) identity: PublicationIdentity,
    tree: filesystem::DirectoryTree,
    transaction: Box<dyn NativePublicationTransaction>,
}
impl PublicationScope {
    /// Called only by trusted native composition after authorization and allocation.
    /// The protection peer is an authority-bearing adapter, not an assertion by a caller.
    pub fn open(
        parent: &Path,
        identity: PublicationIdentity,
        protection: Arc<dyn NativePublicationProtection>,
        transaction: Box<dyn NativePublicationTransaction>,
    ) -> Result<Self, NativeFailure> {
        transaction.revalidate()?;
        validate_key(identity.key)?;
        for value in [
            &identity.server_id[..],
            &identity.account_binding_sha256,
            &identity.registration_profile_sha256,
            &identity.publication_nonce,
        ] {
            nonzero(value)?;
        }
        let tree = filesystem::DirectoryTree::open(parent, protection)?;
        transaction.revalidate()?;
        Ok(Self {
            identity,
            tree,
            transaction,
        })
    }
}

/// Native head-selected retention input. A descriptor digest is not custody authority.
pub struct ApprovedPublication {
    pub(crate) scope: PublicationScope,
    pub(crate) descriptor_sha256: [u8; 32],
}
impl ApprovedPublication {
    /// Binds a native protected selection to an already retained publication scope.
    pub fn new(
        scope: PublicationScope,
        descriptor_sha256: [u8; 32],
    ) -> Result<Self, NativeFailure> {
        nonzero(&descriptor_sha256)?;
        Ok(Self {
            scope,
            descriptor_sha256,
        })
    }
}

/// Native transaction edge. Production uses common authorization and registration handles.
/// Associated types allow injected native peers without a production test switch or forged grant.
pub trait InstallationPublicationNative: Send + Sync {
    /// Independently authenticated native request.
    type Request;
    /// Independently retained registration selection.
    type Registration;
    /// Revalidates authorization and the original journal's reserved allocation/nonce.
    /// This boundary acquires serialization but admits no allocation or installation mutation.
    /// The returned scope retains operation serialization separately from long-lived custody.
    fn begin_publication(
        &self,
        request: &Self::Request,
        candidate: &VerifiedServerArchive,
    ) -> Result<PublicationScope, NativeFailure>;
    /// Reacquires native boot-bound custody and reads only the exact approved selection.
    fn open_approved(
        &self,
        registration: &Self::Registration,
    ) -> Result<ApprovedPublication, NativeFailure>;
    /// Durably marks the existing native journal uncertain, preserving the original allocation.
    /// Partial generations stay protected for operation-aware recovery, never pathname cleanup.
    fn latch_uncertainty(&self) -> Result<(), NativeFailure>;
}

/// An immutable staged candidate. Publication does not select it for startup.
pub struct StagedInstallation {
    descriptor: ApprovedInstallationDescriptor,
    descriptor_sha256: [u8; 32],
    custody: crate::approved_installation::InstallationCustody,
}
impl StagedInstallation {
    /// Returns authenticated destination facts, not activation authority.
    pub fn descriptor(&self) -> &ApprovedInstallationDescriptor {
        &self.descriptor
    }
    /// Returns the exact immutable descriptor binding for a later journaled Stage.
    pub fn descriptor_sha256(&self) -> &[u8; 32] {
        &self.descriptor_sha256
    }
    /// Revalidates all retained destination files, ancestors, native policy and execution bindings.
    pub fn revalidate(&self) -> Result<(), NativeFailure> {
        self.custody.revalidate()
    }
}
impl std::fmt::Debug for StagedInstallation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StagedInstallation(..)")
    }
}

/// Fresh destination publication and head-selected execution retention.
pub trait InstallationPublisher: Send + Sync {
    /// Publishes fresh destination inodes without selecting them for startup.
    fn publish_fresh(
        &self,
        request: &AuthorizedNativeRequest,
        candidate: &VerifiedServerArchive,
    ) -> PublicationDisposition<StagedInstallation>;
    /// Retains the exact approved executable and execution closure through use.
    fn retain_approved(
        &self,
        registration: &RetainedRegistration,
    ) -> Result<RetainedApprovedInstallation, NativeFailure>;
}

/// Fresh-copy engine. Native peers own authorization, allocation, custody and the shared journal.
pub struct FreshInstallationPublisher<Native> {
    native: Native,
    state: Mutex<PublicationState>,
}
impl<Native: InstallationPublicationNative> FreshInstallationPublisher<Native> {
    /// Injects the trusted native edge. No path or protection assertion comes from a client.
    pub fn new(native: Native) -> Self {
        Self {
            native,
            state: Mutex::new(PublicationState::Open),
        }
    }
    /// Returns the irreversible process-local uncertainty latch; native recovery remains separate.
    pub fn state(&self) -> PublicationState {
        self.state
            .lock()
            .map_or(PublicationState::RecoveryRequired, |s| *s)
    }
    /// Publishes using the peer's authenticated request type. Tests inject only at this native edge.
    pub fn publish_fresh(
        &self,
        request: &Native::Request,
        candidate: &VerifiedServerArchive,
    ) -> PublicationDisposition<StagedInstallation> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return PublicationDisposition::Indeterminate,
        };
        if *state != PublicationState::Open {
            return PublicationDisposition::NotApplied(NativeFailure::Indeterminate);
        }
        let prepare = || {
            let (manifest, bytes) = archive::authenticate(candidate)?;
            let scope = self.native.begin_publication(request, candidate)?;
            if build_channel(scope.identity.key.build_identity)
                != manifest.document().build_identity
            {
                return Err(NativeFailure::IdentityMismatch);
            }
            if manifest.document().files.iter().any(|f| {
                f.name == "descriptor.json"
                    || f.name.starts_with("descriptor.json/")
                    || f.name.starts_with("installation.json/")
            }) {
                return Err(NativeFailure::ArtifactRejected);
            }
            scope
                .tree
                .require_absent(scope.identity.installation.0.get())?;
            scope.transaction.revalidate()?;
            Ok((scope, manifest, bytes))
        };
        let (scope, manifest, bytes) = match prepare() {
            Ok(prepared) => prepared,
            Err(error) => return PublicationDisposition::NotApplied(error),
        };
        // The first mkdir may have changed storage even if its callback fails. From this
        // point every failure latches the existing native journal and preserves the partial tree.
        match publish(scope, candidate, manifest.document(), &bytes, || {
            self.native.latch_uncertainty()
        }) {
            Ok(staged) => PublicationDisposition::Durable(staged),
            Err(_) => {
                *state = PublicationState::RecoveryRequired;
                PublicationDisposition::Indeterminate
            }
        }
    }
    /// Retains only the peer's protected approved selection, never its staged candidate.
    pub fn retain_approved(
        &self,
        registration: &Native::Registration,
    ) -> Result<RetainedApprovedInstallation, NativeFailure> {
        let state = self
            .state
            .lock()
            .map_err(|_| NativeFailure::Indeterminate)?;
        if *state != PublicationState::Open {
            return Err(NativeFailure::Indeterminate);
        }
        retain(self.native.open_approved(registration)?)
    }
}
impl<
    N: InstallationPublicationNative<
            Request = AuthorizedNativeRequest,
            Registration = RetainedRegistration,
        >,
> InstallationPublisher for FreshInstallationPublisher<N>
{
    fn publish_fresh(
        &self,
        request: &AuthorizedNativeRequest,
        candidate: &VerifiedServerArchive,
    ) -> PublicationDisposition<StagedInstallation> {
        FreshInstallationPublisher::publish_fresh(self, request, candidate)
    }
    fn retain_approved(
        &self,
        registration: &RetainedRegistration,
    ) -> Result<RetainedApprovedInstallation, NativeFailure> {
        registration.revalidate()?;
        let retained = FreshInstallationPublisher::retain_approved(self, registration)?;
        let descriptor = retained.descriptor();
        let head = registration.head();
        let digest: [u8; 32] = Sha256::digest(descriptor.encode()?).into();
        if descriptor.key != head.key
            || descriptor.installation != head.approved_installation
            || descriptor.server_id != head.server_id
            || descriptor.registration_profile_sha256 != head.profile_sha256
            || descriptor.manifest_sha256 != head.approved_manifest_sha256
            || digest != head.approved_descriptor_sha256
        {
            return Err(NativeFailure::GenerationMismatch);
        }
        registration.revalidate()?;
        Ok(retained)
    }
}

impl<
    N: InstallationPublicationNative<
            Request = AuthorizedNativeRequest,
            Registration = RetainedRegistration,
        >,
> crate::installation::ApprovedInstallationRetention for FreshInstallationPublisher<N>
{
    fn retain_approved(
        &self,
        registration: &RetainedRegistration,
    ) -> Result<RetainedApprovedInstallation, NativeFailure> {
        InstallationPublisher::retain_approved(self, registration)
    }
}

/// Production remains closed until native authorization, custody and privileged evidence pass.
#[derive(Default)]
pub struct UnavailableInstallationPublisher;
impl InstallationPublisher for UnavailableInstallationPublisher {
    fn publish_fresh(
        &self,
        _: &AuthorizedNativeRequest,
        _: &VerifiedServerArchive,
    ) -> PublicationDisposition<StagedInstallation> {
        PublicationDisposition::NotApplied(NativeFailure::CustodyUnavailable)
    }
    fn retain_approved(
        &self,
        _: &RetainedRegistration,
    ) -> Result<RetainedApprovedInstallation, NativeFailure> {
        Err(NativeFailure::CustodyUnavailable)
    }
}

fn build_channel(build: BuildIdentity) -> BuildChannel {
    match build {
        BuildIdentity::Release => BuildChannel::Release,
        BuildIdentity::Preflight => BuildChannel::Preflight,
        BuildIdentity::Development => BuildChannel::Development,
    }
}
fn prepare_fresh(
    scope: &mut PublicationScope,
    candidate: &VerifiedServerArchive,
    manifest: &InstallationManifestV1,
    manifest_bytes: &[u8],
) -> Result<
    (
        ApprovedInstallationDescriptor,
        [u8; 32],
        Vec<filesystem::BoundFile>,
    ),
    NativeFailure,
> {
    scope.transaction.revalidate()?;
    scope
        .tree
        .create_generation(scope.identity.installation.0.get())?;
    let mut files = archive::copy_files(candidate, manifest, &mut scope.tree)?;
    let manifest_digest: [u8; 32] = Sha256::digest(manifest_bytes).into();
    files.push(scope.tree.copy_file(
        "installation.json",
        manifest_bytes.len() as u64,
        0o400,
        manifest_digest,
        &mut &manifest_bytes[..],
    )?);
    scope.tree.verify(&files)?;
    let closure_sha256 = authenticate_closure(&scope.tree, &files, manifest)?;
    // Recheck after native code/dependency policy calls, before deriving any descriptor.
    scope.tree.verify(&files)?;
    let server = manifest
        .files
        .iter()
        .find(|f| f.name == manifest.entry_points.server)
        .ok_or(NativeFailure::ArtifactRejected)?;
    let identity = &scope.identity;
    let descriptor = ApprovedInstallationDescriptor {
        schema_version: 1,
        key: identity.key,
        installation: identity.installation,
        server_id: identity.server_id,
        account_binding_sha256: identity.account_binding_sha256,
        registration_profile_sha256: identity.registration_profile_sha256,
        publication_nonce: identity.publication_nonce,
        archive_sha256: candidate.feed().archive.sha256,
        manifest_sha256: manifest_digest,
        server_artifact_sha256: server.sha256,
        execution_closure_sha256: closure_sha256,
        launcher_abi: manifest.launcher_abi,
    };
    let bytes = descriptor.encode()?;
    let descriptor_sha256 = Sha256::digest(&bytes).into();
    files.push(scope.tree.copy_file(
        "descriptor.json",
        bytes.len() as u64,
        0o400,
        descriptor_sha256,
        &mut bytes.as_slice(),
    )?);
    scope.tree.seal()?;
    scope.tree.verify(&files)?;
    if authenticate_closure(&scope.tree, &files, manifest)? != closure_sha256 {
        return Err(NativeFailure::ArtifactRejected);
    }
    scope.tree.verify(&files)?;
    scope.transaction.revalidate()?;
    Ok((descriptor, descriptor_sha256, files))
}
fn publish(
    mut scope: PublicationScope,
    candidate: &VerifiedServerArchive,
    manifest: &InstallationManifestV1,
    manifest_bytes: &[u8],
    latch: impl FnOnce() -> Result<(), NativeFailure>,
) -> Result<StagedInstallation, NativeFailure> {
    let (descriptor, descriptor_sha256, files) =
        match prepare_fresh(&mut scope, candidate, manifest, manifest_bytes) {
            Ok(prepared) => prepared,
            Err(error) => {
                // Persist uncertainty while still holding the original native serialization.
                // Failure to persist never upgrades the outcome or permits later admission.
                let _ = latch();
                return Err(error);
            }
        };
    let retained = RetainedPublication {
        tree: scope.tree,
        files,
        manifest: manifest.clone(),
        closure_sha256: descriptor.execution_closure_sha256,
    };
    Ok(StagedInstallation {
        descriptor,
        descriptor_sha256,
        custody: crate::approved_installation::InstallationCustody::from_publication(retained),
    })
}
fn retain(selection: ApprovedPublication) -> Result<RetainedApprovedInstallation, NativeFailure> {
    let mut scope = selection.scope;
    scope
        .tree
        .retain_generation(scope.identity.installation.0.get())?;
    let (record, bytes) = scope.tree.read_record("descriptor.json", 16 * 1024)?;
    let descriptor =
        ApprovedInstallationDescriptor::decode_authenticated(&bytes, &selection.descriptor_sha256)?;
    let identity = &scope.identity;
    if descriptor.key != identity.key
        || descriptor.installation != identity.installation
        || descriptor.server_id != identity.server_id
        || descriptor.account_binding_sha256 != identity.account_binding_sha256
        || descriptor.registration_profile_sha256 != identity.registration_profile_sha256
        || descriptor.publication_nonce != identity.publication_nonce
    {
        return Err(NativeFailure::IdentityMismatch);
    }
    let (manifest_record, bytes) = scope.tree.read_record(
        "installation.json",
        spaceterm_distribution::manifest::MAX_MANIFEST_BYTES,
    )?;
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    if digest != descriptor.manifest_sha256 {
        return Err(NativeFailure::ArtifactRejected);
    }
    let manifest: InstallationManifestV1 =
        serde_json::from_slice(&bytes).map_err(|_| NativeFailure::InvalidRecord)?;
    manifest.validate().map_err(archive::failure)?;
    if manifest.build_identity != build_channel(descriptor.key.build_identity)
        || manifest.launcher_abi != descriptor.launcher_abi
    {
        return Err(NativeFailure::IdentityMismatch);
    }
    let server = manifest
        .files
        .iter()
        .find(|file| file.name == manifest.entry_points.server)
        .ok_or(NativeFailure::ArtifactRejected)?;
    if server.sha256 != descriptor.server_artifact_sha256 {
        return Err(NativeFailure::ArtifactRejected);
    }
    let mut files = Vec::with_capacity(manifest.files.len() + 2);
    for expected in &manifest.files {
        files.push(scope.tree.retain_file(
            &expected.name,
            expected.size,
            expected.mode,
            expected.sha256,
        )?);
    }
    files.extend([manifest_record, record]);
    let retained = RetainedPublication {
        tree: scope.tree,
        files,
        manifest,
        closure_sha256: descriptor.execution_closure_sha256,
    };
    retained.verify()?;
    scope.transaction.revalidate()?;
    RetainedApprovedInstallation::from_publication(
        descriptor,
        crate::approved_installation::InstallationCustody::from_publication(retained),
    )
}
