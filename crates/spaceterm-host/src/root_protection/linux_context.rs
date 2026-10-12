//! Private, proof-carrying Linux inspection. No production authority issuer exists.
//!
//! The lease must be independently issued for trusted namespaces, one boot, live
//! original mounts and every protected inode's complete alias-publication policy.
//! Kernel mount facts alone cannot create this lease.

use std::fs::File;
use std::os::unix::fs::MetadataExt;

use super::admission::require_root_mode;
use super::linux::FilesystemVersion;
use super::linux_evidence::{
    Backend, BackendSelection, BasicMountVersion, BootIdentity, DescriptorMountId, EntryIdentity,
    MAX_MOUNT_WITNESSES, MAX_PUBLICATION_ENTRIES, MountIdKind, Namespace, Queries, WorkBudget,
};
use super::linux_native::KernelQueries;
use crate::protected_directory::{NativeDirectoryProtection, ProtectedDirectoryError};

pub(super) enum HandleKind {
    Directory,
    RegularFile,
}

struct TrustedNamespacePins {
    user: Option<File>,
    mount: Option<File>,
    issued_user_identity: EntryIdentity,
    issued_mount_identity: EntryIdentity,
    #[cfg(test)]
    current_user_identity: EntryIdentity,
    #[cfg(test)]
    current_mount_identity: EntryIdentity,
}

// Deliberately uninhabited in production. Root mode, a persisted marker, namespace
// ownership, absence scans and installer locks cannot construct an issuer token.
enum IndependentIssuer {
    #[cfg(test)]
    Fixture,
}

struct AuthoritySeal {
    _issuer: IndependentIssuer,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "only an absent independent issuer may construct non-idmapped provenance"
    )
)]
enum NonIdmappedProvenance {
    IndependentlyTrustedCreation(AuthoritySeal),
    // Requires successful ordinary mount_setattr(IDMAP) on a private old
    // open_tree clone, plus source immutability or serialized trusted mutation.
    // The retained descriptor must be the ORIGINAL source, never the clone.
    SuccessfulPrivilegedCloneProbe(AuthoritySeal),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum WitnessKind {
    TrustedCreation,
    SuccessfulOldCloneProbe,
}

impl NonIdmappedProvenance {
    fn kind(&self) -> WitnessKind {
        match self {
            Self::IndependentlyTrustedCreation(_seal) => WitnessKind::TrustedCreation,
            Self::SuccessfulPrivilegedCloneProbe(_seal) => WitnessKind::SuccessfulOldCloneProbe,
        }
    }
}

struct OriginalMountWitness {
    original: Option<File>,
    legacy_mount_id: u64,
    proof: NonIdmappedProvenance,
}

#[derive(Clone, Copy, Eq, PartialEq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "publication exposure records require the absent independent authority issuer"
    )
)]
enum WriteExposure {
    OrdinaryAlias,
    ReverseIdmappedAlias,
    ForeignMountNamespace,
    DetachedDirectoryForFreshOpenat,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "publication coverage must be issued independently; production has no constructor"
    )
)]
enum PublicationCoverage {
    // Covers foreign namespaces and detached fresh-open handles as well as the
    // selected view. Participating installer serialization alone is insufficient.
    AllAccountWriteAuthorizingAliasesExcluded(AuthoritySeal),
    AccountWritableExposure(WriteExposure),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum PublicationKind {
    AllAliasesExcluded,
    AccountWritable(WriteExposure),
}

impl PublicationCoverage {
    fn kind(&self) -> PublicationKind {
        match self {
            Self::AllAccountWriteAuthorizingAliasesExcluded(_seal) => {
                PublicationKind::AllAliasesExcluded
            }
            Self::AccountWritableExposure(exposure) => PublicationKind::AccountWritable(*exposure),
        }
    }
}

struct PublishedEntry {
    // Pin the inode as well as recording its identity, preventing inode-number
    // reuse from transferring publication coverage to a different object.
    pin: Option<File>,
    identity: EntryIdentity,
    coverage: PublicationCoverage,
}

// Noncloneable, process-local and nonpersistent. There is no production factory.
// Keeping a File alive pins a mount but does not prove mapping or publication;
// those independent issuer obligations are carried by the private seals.
struct LinuxAuthority {
    _seal: AuthoritySeal,
    namespaces: TrustedNamespacePins,
    issued_boot: BootIdentity,
    witnesses: Vec<OriginalMountWitness>,
    published_entries: Vec<PublishedEntry>,
    #[cfg(test)]
    current_boot: BootIdentity,
    #[cfg(test)]
    probe_outcome: tests::ProbeOutcome,
}

impl LinuxAuthority {
    fn validate_storage(&self) -> Result<(), ProtectedDirectoryError> {
        if self.witnesses.len() > MAX_MOUNT_WITNESSES
            || self.published_entries.len() > MAX_PUBLICATION_ENTRIES
        {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        if self.namespaces.user.is_none()
            || self.namespaces.mount.is_none()
            || self
                .witnesses
                .iter()
                .any(|witness| witness.original.is_none())
            || self
                .published_entries
                .iter()
                .any(|entry| entry.pin.is_none())
        {
            return Err(ProtectedDirectoryError::NativeUnavailable);
        }
        Ok(())
    }

    fn published_entry(
        &self,
        identity: EntryIdentity,
    ) -> Result<&PublishedEntry, ProtectedDirectoryError> {
        let mut matches = self
            .published_entries
            .iter()
            .filter(|entry| entry.identity == identity);
        let entry = matches
            .next()
            .ok_or(ProtectedDirectoryError::NativeUnavailable)?;
        if matches.next().is_some() {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        Ok(entry)
    }

    fn lifecycle<Q: Queries>(
        &self,
        queries: &Q,
        work: &mut InspectionWork<Q::BootLease>,
    ) -> Result<LifecycleVersion, ProtectedDirectoryError> {
        let user = self
            .namespaces
            .user
            .as_ref()
            .ok_or(ProtectedDirectoryError::NativeUnavailable)?;
        let mount = self
            .namespaces
            .mount
            .as_ref()
            .ok_or(ProtectedDirectoryError::NativeUnavailable)?;
        Ok(LifecycleVersion {
            pinned_user: HandleVersion::of(user, &mut work.budget)?.identity,
            pinned_mount: HandleVersion::of(mount, &mut work.budget)?.identity,
            current_user: queries.namespace_identity(Namespace::User, &mut work.budget)?,
            current_mount: queries.namespace_identity(Namespace::Mount, &mut work.budget)?,
            boot: queries.boot_identity(&mut work.boot_lease, &mut work.budget)?,
        })
    }
}

#[derive(Eq, PartialEq)]
struct LifecycleVersion {
    pinned_user: EntryIdentity,
    pinned_mount: EntryIdentity,
    current_user: EntryIdentity,
    current_mount: EntryIdentity,
    boot: BootIdentity,
}

impl LifecycleVersion {
    fn require_current(&self, authority: &LinuxAuthority) -> Result<(), ProtectedDirectoryError> {
        if self.boot != authority.issued_boot
            || self.pinned_user != authority.namespaces.issued_user_identity
            || self.pinned_mount != authority.namespaces.issued_mount_identity
            || self.current_user != self.pinned_user
            || self.current_mount != self.pinned_mount
        {
            Err(ProtectedDirectoryError::NativeUnavailable)
        } else {
            Ok(())
        }
    }
}

#[derive(Eq, PartialEq)]
struct HandleVersion {
    identity: EntryIdentity,
    owner: u32,
    group: u32,
    mode: u32,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl HandleVersion {
    fn of(file: &File, work: &mut WorkBudget) -> Result<Self, ProtectedDirectoryError> {
        let metadata = work.retry(|| file.metadata())?;
        Ok(Self {
            identity: EntryIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            owner: metadata.uid(),
            group: metadata.gid(),
            mode: metadata.mode(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }

    fn require_kind(&self, kind: HandleKind) -> Result<(), ProtectedDirectoryError> {
        let expected = match kind {
            HandleKind::Directory => libc::S_IFDIR,
            HandleKind::RegularFile => libc::S_IFREG,
        };
        if self.mode & libc::S_IFMT != expected {
            Err(ProtectedDirectoryError::Unsafe)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum MappingVersion {
    Native(BasicMountVersion),
    Old {
        selected: DescriptorMountId,
        original: Option<DescriptorMountId>,
        provenance: Option<WitnessKind>,
    },
}

struct MappingEvidence {
    version: MappingVersion,
    admission: Result<(), ProtectedDirectoryError>,
}

#[derive(Default)]
struct SelectedMountIds {
    unique: Option<DescriptorMountId>,
    legacy: Option<DescriptorMountId>,
}

impl SelectedMountIds {
    fn changed_from(&self, before: &Self) -> bool {
        // A later query failure cannot erase a successfully decoded observation.
        // Missing or malformed statx output supplies no identity to compare.
        matches!((before.unique, self.unique), (Some(before), Some(after)) if before != after)
            || matches!((before.legacy, self.legacy), (Some(before), Some(after)) if before != after)
    }
}

fn mapping_evidence<Q: Queries>(
    file: &File,
    round: usize,
    authority: &LinuxAuthority,
    queries: &Q,
    backends: BackendSelection,
    work: &mut WorkBudget,
    selected: &mut SelectedMountIds,
) -> Result<MappingEvidence, ProtectedDirectoryError> {
    // At most three selected backends, once each per sampling round. Only missing
    // capability/masks permit fallback. Malformed or denied queries never become
    // an old-kernel witness and never trigger a privileged probe.
    for backend in backends.into_iter().flatten() {
        let result = match backend {
            Backend::DescriptorQuery => queries
                .statmount_by_fd(file, round, work)
                .and_then(|record| BasicMountVersion::decode(&record, None))
                .map(|version| MappingEvidence {
                    version: MappingVersion::Native(version),
                    admission: version.require_non_idmapped(),
                }),
            Backend::UniqueIdQuery => queries
                .statx(file, MountIdKind::Unique, round, work)
                .and_then(|record| DescriptorMountId::decode(&record, MountIdKind::Unique))
                .inspect(|id| selected.unique = Some(*id))
                .and_then(|id| {
                    queries
                        .statmount_by_id(file, id, round, work)
                        .and_then(|record| BasicMountVersion::decode(&record, Some(id)))
                })
                .map(|version| MappingEvidence {
                    version: MappingVersion::Native(version),
                    admission: version.require_non_idmapped(),
                }),
            Backend::LegacyWitness => {
                old_mapping_evidence(file, round, authority, queries, work, selected)
            }
        };
        match result {
            Err(ProtectedDirectoryError::NativeUnavailable) => continue,
            other => return other,
        }
    }
    Err(ProtectedDirectoryError::NativeUnavailable)
}

fn old_mapping_evidence<Q: Queries>(
    file: &File,
    round: usize,
    authority: &LinuxAuthority,
    queries: &Q,
    work: &mut WorkBudget,
    selected_ids: &mut SelectedMountIds,
) -> Result<MappingEvidence, ProtectedDirectoryError> {
    let selected = queries
        .statx(file, MountIdKind::Legacy, round, work)
        .and_then(|record| DescriptorMountId::decode(&record, MountIdKind::Legacy))?;
    selected_ids.legacy = Some(selected);
    let mut witnesses = authority
        .witnesses
        .iter()
        .filter(|witness| witness.legacy_mount_id == selected.value);
    let Some(witness) = witnesses.next() else {
        return Ok(MappingEvidence {
            version: MappingVersion::Old {
                selected,
                original: None,
                provenance: None,
            },
            admission: Err(ProtectedDirectoryError::NativeUnavailable),
        });
    };
    if witnesses.next().is_some() {
        return Err(ProtectedDirectoryError::Unavailable);
    }
    let original = witness
        .original
        .as_ref()
        .ok_or(ProtectedDirectoryError::NativeUnavailable)?;
    let original = queries
        .statx(original, MountIdKind::Legacy, round, work)
        .and_then(|record| DescriptorMountId::decode(&record, MountIdKind::Legacy))?;
    Ok(MappingEvidence {
        version: MappingVersion::Old {
            selected,
            original: Some(original),
            provenance: Some(witness.proof.kind()),
        },
        admission: if selected == original && original.value == witness.legacy_mount_id {
            Ok(())
        } else {
            Err(ProtectedDirectoryError::NativeUnavailable)
        },
    })
}

#[derive(Eq, PartialEq)]
struct PublicationVersion {
    pinned: EntryIdentity,
    kind: PublicationKind,
}

impl PublicationVersion {
    fn require_coverage(&self, entry: &PublishedEntry) -> Result<(), ProtectedDirectoryError> {
        if self.pinned != entry.identity {
            return Err(ProtectedDirectoryError::NativeUnavailable);
        }
        match self.kind {
            PublicationKind::AllAliasesExcluded => Ok(()),
            PublicationKind::AccountWritable(_) => Err(ProtectedDirectoryError::Unsafe),
        }
    }
}

// The boot descriptor and its prepaid close belong to one inspection. Both
// samples reread it under the same budget, alongside every other native attempt.
struct InspectionWork<B> {
    budget: WorkBudget,
    boot_lease: B,
}

struct Sample {
    lifecycle: Result<LifecycleVersion, ProtectedDirectoryError>,
    filesystem: Result<FilesystemVersion, ProtectedDirectoryError>,
    mapping: Result<MappingEvidence, ProtectedDirectoryError>,
    selected_mount_ids: SelectedMountIds,
    publication: Result<PublicationVersion, ProtectedDirectoryError>,
}

impl Sample {
    fn read<Q: Queries>(
        file: &File,
        round: usize,
        authority: &LinuxAuthority,
        publication: &PublishedEntry,
        queries: &Q,
        backends: Result<BackendSelection, ProtectedDirectoryError>,
        work: &mut InspectionWork<Q::BootLease>,
    ) -> Self {
        let lifecycle = authority.lifecycle(queries, work);
        let filesystem = queries.filesystem_version(file, round, &mut work.budget);
        let mut selected_mount_ids = SelectedMountIds::default();
        let mapping = match &lifecycle {
            Ok(version) if version.require_current(authority).is_ok() => {
                backends.and_then(|backends| {
                    mapping_evidence(
                        file,
                        round,
                        authority,
                        queries,
                        backends,
                        &mut work.budget,
                        &mut selected_mount_ids,
                    )
                })
            }
            _ => Err(ProtectedDirectoryError::NativeUnavailable),
        };
        let publication = publication
            .pin
            .as_ref()
            .ok_or(ProtectedDirectoryError::NativeUnavailable)
            .and_then(|pin| HandleVersion::of(pin, &mut work.budget))
            .map(|version| PublicationVersion {
                pinned: version.identity,
                kind: publication.coverage.kind(),
            });
        Self {
            lifecycle,
            filesystem,
            mapping,
            selected_mount_ids,
            publication,
        }
    }

    fn changed_from(&self, before: &Self) -> bool {
        different(&before.lifecycle, &self.lifecycle)
            || different(&before.filesystem, &self.filesystem)
            || different(&before.publication, &self.publication)
            || self
                .selected_mount_ids
                .changed_from(&before.selected_mount_ids)
            || matches!((&before.mapping, &self.mapping), (Ok(before), Ok(after)) if before.version != after.version)
    }

    fn storage_failed(&self) -> bool {
        [
            self.lifecycle.as_ref().err(),
            self.filesystem.as_ref().err(),
            self.mapping.as_ref().err(),
            self.publication.as_ref().err(),
        ]
        .contains(&Some(&ProtectedDirectoryError::Unavailable))
    }
}

fn different<T: PartialEq>(
    before: &Result<T, ProtectedDirectoryError>,
    after: &Result<T, ProtectedDirectoryError>,
) -> bool {
    matches!((before, after), (Ok(before), Ok(after)) if before != after)
}

// The production instance has no lease. A future issuer must provide the sealed
// authority rather than changing this adapter to infer trust from queried facts.
struct LinuxProtectionContext<'authority, Q = KernelQueries> {
    authority: Option<&'authority LinuxAuthority>,
    queries: Q,
}

impl<Q: Queries> LinuxProtectionContext<'_, Q> {
    fn verify(&self, file: &File, kind: HandleKind) -> Result<(), ProtectedDirectoryError> {
        let mut work = InspectionWork::<Q::BootLease> {
            budget: WorkBudget::new(),
            boot_lease: Q::BootLease::default(),
        };
        let before = HandleVersion::of(file, &mut work.budget)?;
        before.require_kind(kind)?;
        require_root_mode(before.owner, before.mode)?;
        let authority = self
            .authority
            .ok_or(ProtectedDirectoryError::NativeUnavailable)?;
        authority.validate_storage()?;
        let publication = authority.published_entry(before.identity)?;
        let backends = self.queries.select_backends(&mut work.budget);
        let first = Sample::read(
            file,
            0,
            authority,
            publication,
            &self.queries,
            backends,
            &mut work,
        );
        // Complete the second sample even after admission/query failure. Only
        // observed differences take Changed precedence; missing data is no match.
        let second = Sample::read(
            file,
            1,
            authority,
            publication,
            &self.queries,
            backends,
            &mut work,
        );
        let after = HandleVersion::of(file, &mut work.budget);
        if matches!(&after, Ok(after) if before != *after) || second.changed_from(&first) {
            return Err(ProtectedDirectoryError::Changed);
        }
        after?;
        if first.storage_failed() || second.storage_failed() {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        first
            .lifecycle
            .as_ref()
            .map_err(|error| *error)?
            .require_current(authority)?;
        second
            .lifecycle
            .as_ref()
            .map_err(|error| *error)?
            .require_current(authority)?;
        first
            .filesystem
            .as_ref()
            .map_err(|error| *error)?
            .require_proven_semantics()?;
        second
            .filesystem
            .as_ref()
            .map_err(|error| *error)?
            .require_proven_semantics()?;
        // A stable proven write exposure is Unsafe even when the other proof is
        // missing. Neither proof is inferred from the selected ordinary mapping.
        if matches!((&first.mapping, &second.mapping), (Ok(first), Ok(second))
            if first.admission == Err(ProtectedDirectoryError::Unsafe)
                && second.admission == Err(ProtectedDirectoryError::Unsafe))
            || matches!((&first.publication, &second.publication), (Ok(first), Ok(second))
                if first.require_coverage(publication) == Err(ProtectedDirectoryError::Unsafe)
                    && second.require_coverage(publication) == Err(ProtectedDirectoryError::Unsafe))
        {
            return Err(ProtectedDirectoryError::Unsafe);
        }
        first.mapping.as_ref().map_err(|error| *error)?.admission?;
        second.mapping.as_ref().map_err(|error| *error)?.admission?;
        first
            .publication
            .as_ref()
            .map_err(|error| *error)?
            .require_coverage(publication)?;
        second
            .publication
            .as_ref()
            .map_err(|error| *error)?
            .require_coverage(publication)
    }
}

impl<Q: Queries> NativeDirectoryProtection for LinuxProtectionContext<'_, Q> {
    fn verify_directory(&self, directory: &File) -> Result<(), ProtectedDirectoryError> {
        self.verify(directory, HandleKind::Directory)
    }

    fn verify_regular_file(&self, file: &File) -> Result<(), ProtectedDirectoryError> {
        self.verify(file, HandleKind::RegularFile)
    }
}

pub(super) fn verify_without_authority(
    file: &File,
    kind: HandleKind,
) -> Result<(), ProtectedDirectoryError> {
    LinuxProtectionContext {
        authority: None,
        queries: KernelQueries,
    }
    .verify(file, kind)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::os::fd::AsRawFd;

    use super::super::linux_evidence::testing::{
        DescriptorSample, FixtureQueries, LEGACY_MOUNT_ID, MappingQuery, Reply, UNIQUE_MOUNT_ID,
        missing_basic_mount_record,
    };
    use super::super::linux_evidence::{
        BACKENDS, MOUNT_ATTR_IDMAP, STATX_BYTES, STATX_MNT_ID, STATX_MNT_ID_UNIQUE,
    };
    use super::*;

    type InjectedLinuxAuthority = LinuxAuthority;

    pub(super) enum ProbeOutcome {
        NotRequested,
        AuthorizedSuccessfulApplication,
        Denied,
        InvalidRequest,
        UnsupportedFilesystem,
        NoopSuccess,
    }

    fn injected_seal() -> AuthoritySeal {
        AuthoritySeal {
            _issuer: IndependentIssuer::Fixture,
        }
    }

    fn identity(file: &File) -> EntryIdentity {
        let metadata = file
            .metadata()
            .expect("read-only evidence fixture metadata is unavailable");
        EntryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    impl LinuxAuthority {
        fn for_handle(file: &File) -> Self {
            let user = File::open("/proc/thread-self/ns/user")
                .expect("read-only user namespace fixture opening failed");
            let mount = File::open("/proc/thread-self/ns/mnt")
                .expect("read-only mount namespace fixture opening failed");
            let user_identity = identity(&user);
            let mount_identity = identity(&mount);
            let boot = BootIdentity([0x52; 16]);
            Self {
                _seal: injected_seal(),
                namespaces: TrustedNamespacePins {
                    user: Some(user),
                    mount: Some(mount),
                    issued_user_identity: user_identity,
                    current_user_identity: user_identity,
                    issued_mount_identity: mount_identity,
                    current_mount_identity: mount_identity,
                },
                issued_boot: boot,
                current_boot: boot,
                witnesses: vec![OriginalMountWitness {
                    original: Some(file.try_clone().expect("original mount fixture pin failed")),
                    legacy_mount_id: LEGACY_MOUNT_ID,
                    proof: NonIdmappedProvenance::IndependentlyTrustedCreation(injected_seal()),
                }],
                published_entries: vec![PublishedEntry {
                    pin: Some(
                        file.try_clone()
                            .expect("publication inode fixture pin failed"),
                    ),
                    identity: identity(file),
                    coverage: PublicationCoverage::AllAccountWriteAuthorizingAliasesExcluded(
                        injected_seal(),
                    ),
                }],
                probe_outcome: ProbeOutcome::NotRequested,
            }
        }
    }

    impl<'authority> LinuxProtectionContext<'authority, FixtureQueries> {
        fn for_fixture(
            authority: &'authority LinuxAuthority,
            samples: [DescriptorSample; 2],
        ) -> Self {
            Self {
                authority: Some(authority),
                queries: FixtureQueries {
                    samples,
                    original_descriptors: authority
                        .witnesses
                        .iter()
                        .filter_map(|witness| witness.original.as_ref().map(AsRawFd::as_raw_fd))
                        .collect(),
                    original_statx: std::array::from_fn(|_| {
                        DescriptorSample::ordinary(Backend::LegacyWitness)
                            .mapping
                            .statx
                    }),
                    unique_statx: None,
                    descriptor_statmount: None,
                    mapping_query_trace: RefCell::new(Vec::new()),
                    backend_order: BACKENDS,
                    fd_statmount_calls: Cell::new(0),
                    unique_statx_calls: Cell::new(0),
                    id_statmount_calls: Cell::new(0),
                    legacy_statx_calls: Cell::new(0),
                    original_statx_calls: Cell::new(0),
                    user_identity: authority.namespaces.current_user_identity,
                    mount_identity: authority.namespaces.current_mount_identity,
                    boot: authority.current_boot,
                },
            }
        }
    }

    fn directory_handle() -> File {
        let file = File::open("/").expect("read-only root directory fixture opening failed");
        require_root_fixture(&file);
        file
    }

    fn regular_handle() -> File {
        let file = File::open("/usr/bin/true").expect("read-only regular fixture opening failed");
        require_root_fixture(&file);
        file
    }

    fn require_root_fixture(file: &File) {
        let metadata = file
            .metadata()
            .expect("read-only root fixture metadata is unavailable");
        assert_eq!((metadata.uid(), metadata.mode() & 0o022), (0, 0));
    }

    fn ordinary_samples(backend: Backend) -> [DescriptorSample; 2] {
        std::array::from_fn(|_| DescriptorSample::ordinary(backend))
    }

    #[test]
    fn injected_publication_and_mapping_controls_preserve_all_admitted_filesystems() {
        // These positives exercise scope semantics only. They are not evidence of a
        // deployed issuer, actual statmount support, native binds or a privileged probe.
        let directory = directory_handle();
        let file = regular_handle();
        for handle in [&directory, &file] {
            let authority = InjectedLinuxAuthority::for_handle(handle);
            for backend in [
                Backend::LegacyWitness,
                Backend::UniqueIdQuery,
                Backend::DescriptorQuery,
            ] {
                for filesystem_type in [0xEF53, 0x5846_5342, 0x9123_683E, 0x0102_1994] {
                    let mut samples = ordinary_samples(backend);
                    for sample in &mut samples {
                        sample.filesystem.filesystem_type = filesystem_type;
                    }
                    let context = LinuxProtectionContext::for_fixture(&authority, samples);
                    let result = if handle.metadata().unwrap().is_dir() {
                        context.verify_directory(handle)
                    } else {
                        context.verify_regular_file(handle)
                    };
                    assert_eq!(result, Ok(()));
                }
            }
        }
    }

    #[test]
    fn selected_mapping_evidence_never_substitutes_for_publication_authority() {
        let directory = directory_handle();
        let mut authority = InjectedLinuxAuthority::for_handle(&directory);
        authority.published_entries.clear();
        for backend in [
            Backend::LegacyWitness,
            Backend::UniqueIdQuery,
            Backend::DescriptorQuery,
        ] {
            let context =
                LinuxProtectionContext::for_fixture(&authority, ordinary_samples(backend));
            assert_eq!(
                context.verify_directory(&directory),
                Err(ProtectedDirectoryError::NativeUnavailable)
            );
        }
    }

    #[test]
    fn ordinary_selected_view_is_unsafe_when_a_reverse_alias_authorizes_account_writes() {
        let file = regular_handle();
        let mut authority = InjectedLinuxAuthority::for_handle(&file);
        authority.published_entries[0].coverage =
            PublicationCoverage::AccountWritableExposure(WriteExposure::ReverseIdmappedAlias);
        // All query backends report an ordinary selected view. The unsafe fact belongs
        // to publication, not to mapping, metadata timestamps or native watch history.
        for backend in [
            Backend::LegacyWitness,
            Backend::UniqueIdQuery,
            Backend::DescriptorQuery,
        ] {
            let context =
                LinuxProtectionContext::for_fixture(&authority, ordinary_samples(backend));
            assert_eq!(
                context.verify_regular_file(&file),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn foreign_and_detached_fresh_open_aliases_defeat_publication_admission() {
        let directory = directory_handle();
        for exposure in [
            WriteExposure::OrdinaryAlias,
            WriteExposure::ForeignMountNamespace,
            WriteExposure::DetachedDirectoryForFreshOpenat,
        ] {
            let mut authority = InjectedLinuxAuthority::for_handle(&directory);
            authority.published_entries[0].coverage =
                PublicationCoverage::AccountWritableExposure(exposure);
            let context = LinuxProtectionContext::for_fixture(
                &authority,
                ordinary_samples(Backend::DescriptorQuery),
            );
            assert_eq!(
                context.verify_directory(&directory),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn publication_coverage_for_a_directory_does_not_authorize_a_regular_file() {
        let directory = directory_handle();
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        let context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::DescriptorQuery),
        );
        assert_eq!(
            context.verify_regular_file(&file),
            Err(ProtectedDirectoryError::NativeUnavailable)
        );
    }

    #[test]
    fn leaf_publication_coverage_cannot_skip_the_root_or_an_ancestor() {
        use crate::protected_directory::RetainedProtectedDirectory;
        use std::path::Path;

        let leaf = File::open("/usr/bin").expect("read-only leaf fixture opening failed");
        require_root_fixture(&leaf);
        let authority = InjectedLinuxAuthority::for_handle(&leaf);
        let context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::DescriptorQuery),
        );
        let opened = RetainedProtectedDirectory::open(Path::new("/usr/bin"), &context);
        assert_eq!(
            opened.err(),
            Some(ProtectedDirectoryError::NativeUnavailable)
        );
    }

    #[test]
    fn proven_idmapping_is_unsafe_for_both_directory_and_regular_descriptors() {
        let directory = directory_handle();
        let file = regular_handle();
        for backend in [Backend::UniqueIdQuery, Backend::DescriptorQuery] {
            let mut samples = ordinary_samples(backend);
            for sample in &mut samples {
                // Reject even a numerically identity-mapped mount. Mapping arrays or
                // zero mapping counts are not the non-idmapped proof.
                sample
                    .mapping
                    .statmount_record()
                    .long_word(64, MOUNT_ATTR_IDMAP);
            }
            let directory_authority = InjectedLinuxAuthority::for_handle(&directory);
            let context =
                LinuxProtectionContext::for_fixture(&directory_authority, samples.clone());
            assert_eq!(
                context.verify_directory(&directory),
                Err(ProtectedDirectoryError::Unsafe)
            );
            let file_authority = InjectedLinuxAuthority::for_handle(&file);
            let context = LinuxProtectionContext::for_fixture(&file_authority, samples);
            assert_eq!(
                context.verify_regular_file(&file),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn missing_statx_mount_mask_is_not_mapping_evidence() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        for backend in [Backend::LegacyWitness, Backend::UniqueIdQuery] {
            let mut samples = ordinary_samples(backend);
            for sample in &mut samples {
                // glibc 2.35's successful fallback can return basic stat fields only.
                sample.mapping.statx_record().word(0, 0x07ff);
            }
            let context = LinuxProtectionContext::for_fixture(&authority, samples);
            assert_eq!(
                context.verify_directory(&directory),
                Err(ProtectedDirectoryError::NativeUnavailable)
            );
        }
    }

    #[test]
    fn legacy_and_unique_statx_mount_masks_are_not_interchangeable() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        for (backend, wrong_mask) in [
            (Backend::LegacyWitness, STATX_MNT_ID_UNIQUE),
            (Backend::UniqueIdQuery, STATX_MNT_ID),
        ] {
            let mut samples = ordinary_samples(backend);
            for sample in &mut samples {
                sample.mapping.statx_record().word(0, wrong_mask);
            }
            let context = LinuxProtectionContext::for_fixture(&authority, samples);
            assert_eq!(
                context.verify_regular_file(&file),
                Err(ProtectedDirectoryError::NativeUnavailable)
            );
        }
    }

    #[test]
    fn short_or_oversized_statx_output_cannot_issue_a_mount_identity() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        for written in [0, 143, 151, STATX_BYTES - 1, STATX_BYTES + 1] {
            let mut samples = ordinary_samples(Backend::UniqueIdQuery);
            for sample in &mut samples {
                sample.mapping.statx_record().written = written;
            }
            let context = LinuxProtectionContext::for_fixture(&authority, samples);
            assert_eq!(
                context.verify_directory(&directory),
                Err(ProtectedDirectoryError::Unavailable)
            );
        }
    }

    #[test]
    fn statmount_requires_a_returned_basic_mask_not_zero_attribute_bytes() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        for backend in [Backend::UniqueIdQuery, Backend::DescriptorQuery] {
            let mut samples = ordinary_samples(backend);
            for sample in &mut samples {
                sample.mapping.statmount_record().long_word(8, 0);
            }
            let context = LinuxProtectionContext::for_fixture(&authority, samples);
            assert_eq!(
                context.verify_regular_file(&file),
                Err(ProtectedDirectoryError::NativeUnavailable)
            );
        }
    }

    #[test]
    fn short_oversized_and_inconsistent_statmount_outputs_fail_closed() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        for (written, reported) in [
            (0, 512),
            (71, 512),
            (511, 512),
            (512, 0),
            (512, 511),
            (512, 513),
            (513, 513),
            (512, u32::MAX),
        ] {
            let mut samples = ordinary_samples(Backend::DescriptorQuery);
            for sample in &mut samples {
                let record = sample.mapping.statmount_record();
                record.written = written;
                record.word(0, reported);
            }
            let context = LinuxProtectionContext::for_fixture(&authority, samples);
            assert_eq!(
                context.verify_regular_file(&file),
                Err(ProtectedDirectoryError::Unavailable)
            );
        }
    }

    #[test]
    fn id_based_query_must_match_the_unique_id_from_the_held_descriptor() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        for mismatched in [0, LEGACY_MOUNT_ID, UNIQUE_MOUNT_ID + 1] {
            let mut samples = ordinary_samples(Backend::UniqueIdQuery);
            for sample in &mut samples {
                sample.mapping.statmount_record().long_word(40, mismatched);
            }
            let context = LinuxProtectionContext::for_fixture(&authority, samples);
            assert_eq!(
                context.verify_directory(&directory),
                Err(ProtectedDirectoryError::Unavailable)
            );
        }
    }

    #[test]
    fn zero_or_out_of_range_legacy_ids_do_not_match_a_live_witness() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        for mount_id in [0, u64::from(u32::MAX) + 1] {
            let mut samples = ordinary_samples(Backend::LegacyWitness);
            for sample in &mut samples {
                sample.mapping.statx_record().long_word(144, mount_id);
            }
            let context = LinuxProtectionContext::for_fixture(&authority, samples);
            assert_eq!(
                context.verify_directory(&directory),
                Err(ProtectedDirectoryError::Unavailable)
            );
        }
    }

    #[test]
    fn denied_and_failed_queries_are_not_non_idmapped_evidence() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        for reply in [Reply::Denied, Reply::Failed] {
            let mut samples = ordinary_samples(Backend::DescriptorQuery);
            for sample in &mut samples {
                sample.mapping.statmount = reply.clone();
            }
            let context = LinuxProtectionContext::for_fixture(&authority, samples);
            assert_eq!(
                context.verify_regular_file(&file),
                Err(ProtectedDirectoryError::Unavailable)
            );
        }
    }

    #[test]
    fn a_denied_or_failed_descriptor_statx_cannot_select_a_witness_or_query() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        for backend in [Backend::LegacyWitness, Backend::UniqueIdQuery] {
            for reply in [Reply::Denied, Reply::Failed] {
                let mut samples = ordinary_samples(backend);
                for sample in &mut samples {
                    sample.mapping.statx = reply.clone();
                }
                let context = LinuxProtectionContext::for_fixture(&authority, samples);
                assert_eq!(
                    context.verify_directory(&directory),
                    Err(ProtectedDirectoryError::Unavailable)
                );
            }
        }
    }

    #[test]
    fn fd_query_requires_a_valid_nonzero_mount_identity() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        let mut samples = ordinary_samples(Backend::DescriptorQuery);
        for sample in &mut samples {
            sample.mapping.statmount_record().long_word(40, 0);
        }
        let context = LinuxProtectionContext::for_fixture(&authority, samples);
        assert_eq!(
            context.verify_regular_file(&file),
            Err(ProtectedDirectoryError::Unavailable)
        );
    }

    #[test]
    fn unavailable_modern_queries_without_an_old_live_witness_fail_closed() {
        let file = regular_handle();
        let mut authority = InjectedLinuxAuthority::for_handle(&file);
        authority.witnesses.clear();
        let mut samples = ordinary_samples(Backend::DescriptorQuery);
        for sample in &mut samples {
            sample.mapping.statmount = Reply::Unsupported;
            sample.mapping.statx = Reply::Unsupported;
        }
        let context = LinuxProtectionContext::for_fixture(&authority, samples);
        assert_eq!(
            context.verify_regular_file(&file),
            Err(ProtectedDirectoryError::NativeUnavailable)
        );
    }

    #[test]
    fn old_kernel_layout_without_mapping_provenance_fails_closed() {
        let directory = directory_handle();
        let mut authority = InjectedLinuxAuthority::for_handle(&directory);
        authority.witnesses.clear();
        let context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::LegacyWitness),
        );
        assert_eq!(
            context.verify_directory(&directory),
            Err(ProtectedDirectoryError::NativeUnavailable)
        );
    }

    #[test]
    fn ordinary_linux_5_15_tmpfs_without_independent_creation_provenance_fails_closed() {
        let file = regular_handle();
        let mut authority = InjectedLinuxAuthority::for_handle(&file);
        authority.witnesses.clear();
        authority.probe_outcome = ProbeOutcome::UnsupportedFilesystem;
        let mut samples = ordinary_samples(Backend::LegacyWitness);
        for sample in &mut samples {
            sample.filesystem.filesystem_type = 0x0102_1994;
        }
        let context = LinuxProtectionContext::for_fixture(&authority, samples);
        // An old tmpfs probe's expected failure is not a non-idmapped witness.
        assert_eq!(
            context.verify_regular_file(&file),
            Err(ProtectedDirectoryError::NativeUnavailable)
        );
    }

    #[test]
    fn old_kernel_witness_must_match_the_live_original_mount_not_a_reused_number() {
        let file = regular_handle();
        let mut authority = InjectedLinuxAuthority::for_handle(&file);
        authority.witnesses[0].legacy_mount_id += 1;
        let context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::LegacyWitness),
        );
        assert_eq!(
            context.verify_regular_file(&file),
            Err(ProtectedDirectoryError::NativeUnavailable)
        );
    }

    #[test]
    fn failed_or_noop_old_clone_probes_cannot_create_mapping_provenance() {
        let directory = directory_handle();
        for outcome in [
            ProbeOutcome::Denied,
            ProbeOutcome::InvalidRequest,
            ProbeOutcome::UnsupportedFilesystem,
            ProbeOutcome::NoopSuccess,
        ] {
            let mut authority = InjectedLinuxAuthority::for_handle(&directory);
            authority.witnesses.clear();
            authority.probe_outcome = outcome;
            let context = LinuxProtectionContext::for_fixture(
                &authority,
                ordinary_samples(Backend::LegacyWitness),
            );
            assert_eq!(
                context.verify_directory(&directory),
                Err(ProtectedDirectoryError::NativeUnavailable)
            );
        }
    }

    #[test]
    fn only_injected_positive_probe_provenance_is_a_scope_positive_control() {
        let directory = directory_handle();
        let mut authority = InjectedLinuxAuthority::for_handle(&directory);
        authority.probe_outcome = ProbeOutcome::AuthorizedSuccessfulApplication;
        authority.witnesses[0].proof =
            NonIdmappedProvenance::SuccessfulPrivilegedCloneProbe(injected_seal());
        let context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::LegacyWitness),
        );
        // No open_tree, mount_setattr, namespace creation or probe has run. This proves
        // only the admitted shape of the private injection, not production authority.
        assert_eq!(context.verify_directory(&directory), Ok(()));
    }

    #[test]
    fn lost_original_mount_pin_invalidates_old_evidence() {
        let directory = directory_handle();
        let mut authority = InjectedLinuxAuthority::for_handle(&directory);
        authority.witnesses[0].original = None;
        let context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::LegacyWitness),
        );
        assert_eq!(
            context.verify_directory(&directory),
            Err(ProtectedDirectoryError::NativeUnavailable)
        );
    }

    #[test]
    fn reboot_invalidates_the_authority_even_when_all_mount_numbers_match() {
        let file = regular_handle();
        let mut authority = InjectedLinuxAuthority::for_handle(&file);
        authority.current_boot = BootIdentity([0x53; 16]);
        let context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::LegacyWitness),
        );
        assert_eq!(
            context.verify_regular_file(&file),
            Err(ProtectedDirectoryError::NativeUnavailable)
        );
    }

    #[test]
    fn lost_user_or_mount_namespace_pin_invalidates_publication_and_mapping() {
        let directory = directory_handle();
        for lose_user in [true, false] {
            let mut authority = InjectedLinuxAuthority::for_handle(&directory);
            if lose_user {
                authority.namespaces.user = None;
            } else {
                authority.namespaces.mount = None;
            }
            let context = LinuxProtectionContext::for_fixture(
                &authority,
                ordinary_samples(Backend::DescriptorQuery),
            );
            assert_eq!(
                context.verify_directory(&directory),
                Err(ProtectedDirectoryError::NativeUnavailable)
            );
        }
    }

    #[test]
    fn id_lookup_in_another_namespace_cannot_reuse_the_trusted_context() {
        let directory = directory_handle();
        let mut authority = InjectedLinuxAuthority::for_handle(&directory);
        authority.namespaces.current_mount_identity.inode += 1;
        let context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::UniqueIdQuery),
        );
        assert_eq!(
            context.verify_directory(&directory),
            Err(ProtectedDirectoryError::NativeUnavailable)
        );
    }

    #[test]
    fn projected_ownership_in_another_user_namespace_cannot_reuse_host_authority() {
        let file = regular_handle();
        let mut authority = InjectedLinuxAuthority::for_handle(&file);
        authority.namespaces.current_user_identity.inode += 1;
        let context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::DescriptorQuery),
        );
        assert_eq!(
            context.verify_regular_file(&file),
            Err(ProtectedDirectoryError::NativeUnavailable)
        );
    }

    #[test]
    fn mapping_evidence_differences_take_changed_precedence_over_idmap_admission() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        let mut samples = ordinary_samples(Backend::DescriptorQuery);
        samples[1]
            .mapping
            .statmount_record()
            .long_word(64, MOUNT_ATTR_IDMAP);
        let context = LinuxProtectionContext::for_fixture(&authority, samples);
        assert_eq!(
            context.verify_regular_file(&file),
            Err(ProtectedDirectoryError::Changed)
        );
    }

    #[test]
    fn selected_mount_id_differences_are_changed_despite_equal_filesystem_facts() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        let mut samples = ordinary_samples(Backend::UniqueIdQuery);
        samples[1]
            .mapping
            .statx_record()
            .long_word(144, UNIQUE_MOUNT_ID + 1);
        samples[1]
            .mapping
            .statmount_record()
            .long_word(40, UNIQUE_MOUNT_ID + 1);
        let context = LinuxProtectionContext::for_fixture(&authority, samples);
        assert_eq!(
            context.verify_directory(&directory),
            Err(ProtectedDirectoryError::Changed)
        );
    }

    #[test]
    fn unavailable_second_round_mapping_evidence_cannot_preserve_first_round_admission() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        let mut samples = ordinary_samples(Backend::DescriptorQuery);
        samples[1].mapping.statmount = Reply::Denied;
        let context = LinuxProtectionContext::for_fixture(&authority, samples);
        assert_eq!(
            context.verify_regular_file(&file),
            Err(ProtectedDirectoryError::Unavailable)
        );
    }

    #[test]
    fn endlessly_interrupted_mapping_query_exhausts_its_own_bound() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        let mut samples = ordinary_samples(Backend::DescriptorQuery);
        for sample in &mut samples {
            sample.mapping.statmount = Reply::RepeatedInterruptions;
        }
        let context = LinuxProtectionContext::for_fixture(&authority, samples);
        assert_eq!(
            context.verify_regular_file(&file),
            Err(ProtectedDirectoryError::Unavailable)
        );
    }

    #[test]
    fn authority_witness_storage_is_bounded_independently_of_the_observer() {
        let directory = directory_handle();
        let mut authority = InjectedLinuxAuthority::for_handle(&directory);
        for _ in 0..MAX_MOUNT_WITNESSES {
            authority.witnesses.push(OriginalMountWitness {
                original: Some(
                    directory
                        .try_clone()
                        .expect("bounded witness fixture pin failed"),
                ),
                legacy_mount_id: LEGACY_MOUNT_ID,
                proof: NonIdmappedProvenance::IndependentlyTrustedCreation(injected_seal()),
            });
        }
        let context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::LegacyWitness),
        );
        assert_eq!(
            context.verify_directory(&directory),
            Err(ProtectedDirectoryError::Unavailable)
        );
    }

    #[test]
    fn unproven_filesystem_semantics_remain_unsafe_with_injected_mapping_and_publication() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        let mut samples = ordinary_samples(Backend::DescriptorQuery);
        for sample in &mut samples {
            sample.filesystem.filesystem_type = 0xFEED;
        }
        let context = LinuxProtectionContext::for_fixture(&authority, samples);
        assert_eq!(
            context.verify_regular_file(&file),
            Err(ProtectedDirectoryError::Unsafe)
        );
    }

    #[test]
    fn existing_filesystem_version_differences_still_return_changed() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        let mut samples = ordinary_samples(Backend::DescriptorQuery);
        samples[1].filesystem.mount_flags = 1;
        let context = LinuxProtectionContext::for_fixture(&authority, samples);
        assert_eq!(
            context.verify_directory(&directory),
            Err(ProtectedDirectoryError::Changed)
        );
    }

    #[test]
    fn the_original_pin_is_queried_independently_of_its_recorded_mount_number() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        let mut context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::LegacyWitness),
        );
        for reply in &mut context.queries.original_statx {
            if let Reply::Output(record) = reply {
                record.long_word(144, LEGACY_MOUNT_ID + 1);
            }
        }
        assert_eq!(
            context.verify_regular_file(&file),
            Err(ProtectedDirectoryError::NativeUnavailable)
        );
    }

    #[test]
    fn a_failed_original_pin_query_cannot_reuse_the_authority_issued_number() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        let mut context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::LegacyWitness),
        );
        context.queries.original_statx = [Reply::Denied, Reply::Denied];
        assert_eq!(
            context.verify_directory(&directory),
            Err(ProtectedDirectoryError::Unavailable)
        );
    }

    #[test]
    fn differences_in_the_held_original_mount_take_changed_precedence_over_missing_provenance() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        let mut context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::LegacyWitness),
        );
        if let Reply::Output(record) = &mut context.queries.original_statx[1] {
            record.long_word(144, LEGACY_MOUNT_ID + 1);
        }
        assert_eq!(
            context.verify_regular_file(&file),
            Err(ProtectedDirectoryError::Changed)
        );
    }

    #[test]
    fn an_old_selected_mount_change_does_not_hide_behind_a_missing_second_round_witness() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        let mut samples = ordinary_samples(Backend::LegacyWitness);
        samples[1]
            .mapping
            .statx_record()
            .long_word(144, LEGACY_MOUNT_ID + 1);
        let context = LinuxProtectionContext::for_fixture(&authority, samples);
        assert_eq!(
            context.verify_directory(&directory),
            Err(ProtectedDirectoryError::Changed)
        );
    }

    #[test]
    fn publication_coverage_requires_the_still_held_inode_not_its_recorded_identity() {
        let file = regular_handle();
        let mut authority = InjectedLinuxAuthority::for_handle(&file);
        authority.published_entries[0].pin = Some(directory_handle());
        let context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::DescriptorQuery),
        );
        assert_eq!(
            context.verify_regular_file(&file),
            Err(ProtectedDirectoryError::NativeUnavailable)
        );
    }

    #[test]
    fn publication_storage_is_bounded_independently_of_mount_witness_and_observer_budgets() {
        let directory = directory_handle();
        let mut authority = InjectedLinuxAuthority::for_handle(&directory);
        for _ in 0..MAX_PUBLICATION_ENTRIES {
            authority.published_entries.push(PublishedEntry {
                pin: Some(
                    directory
                        .try_clone()
                        .expect("publication fixture pin failed"),
                ),
                identity: identity(&directory),
                coverage: PublicationCoverage::AllAccountWriteAuthorizingAliasesExcluded(
                    injected_seal(),
                ),
            });
        }
        let context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::DescriptorQuery),
        );
        assert_eq!(
            context.verify_directory(&directory),
            Err(ProtectedDirectoryError::Unavailable)
        );
    }

    #[test]
    fn statx_reserved_fields_and_attribute_mask_inconsistency_are_not_mount_evidence() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        for (offset, value) in [
            (30, 1),
            (76, 1),
            (92, 1),
            (108, 1),
            (124, 1),
            (188, 1),
            (248, 1),
            (8, 1),
            (0, STATX_MNT_ID_UNIQUE | 0x8000_0000),
            (0, STATX_MNT_ID | STATX_MNT_ID_UNIQUE),
        ] {
            let mut samples = ordinary_samples(Backend::UniqueIdQuery);
            for sample in &mut samples {
                sample.mapping.statx_record().word(offset, value);
            }
            let context = LinuxProtectionContext::for_fixture(&authority, samples);
            assert_eq!(
                context.verify_directory(&directory),
                Err(ProtectedDirectoryError::Unavailable)
            );
        }
    }

    #[test]
    fn a_basic_statmount_record_cannot_return_unrequested_strings_arrays_or_reserved_fields() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        for offset in [4, 16, 104, 108, 112, 120, 128, 136, 144, 152, 504] {
            let mut samples = ordinary_samples(Backend::DescriptorQuery);
            for sample in &mut samples {
                sample.mapping.statmount_record().word(offset, 1);
            }
            let context = LinuxProtectionContext::for_fixture(&authority, samples);
            assert_eq!(
                context.verify_regular_file(&file),
                Err(ProtectedDirectoryError::Unavailable)
            );
        }
    }

    #[test]
    fn malformed_basic_attributes_and_unrequested_masks_never_fall_back_to_a_live_old_witness() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        for (offset, value) in [
            (8, 3),
            (64, 0x40),
            (64, 0x30),
            (64, 1 << 63),
            (72, 1),
            (80, 1),
            (88, 1),
            (96, 1),
        ] {
            let mut samples = ordinary_samples(Backend::DescriptorQuery);
            for sample in &mut samples {
                sample.mapping.statmount_record().long_word(offset, value);
            }
            let context = LinuxProtectionContext::for_fixture(&authority, samples);
            assert_eq!(
                context.verify_regular_file(&file),
                Err(ProtectedDirectoryError::Unavailable)
            );
        }
    }

    #[test]
    fn denied_id_based_statmount_cannot_be_replaced_by_an_old_mount_witness() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        let mut samples = ordinary_samples(Backend::UniqueIdQuery);
        for sample in &mut samples {
            sample.mapping.statmount = Reply::Denied;
        }
        let context = LinuxProtectionContext::for_fixture(&authority, samples);
        assert_eq!(
            context.verify_directory(&directory),
            Err(ProtectedDirectoryError::Unavailable)
        );
    }

    // Linux 6.8 rejects flags=1 before reading statmount's request, while flags=0
    // supports the ID query. This conservative operation order uses independent
    // ID/witness evidence instead of reclassifying EINVAL or asserting mount trust.
    // https://raw.githubusercontent.com/torvalds/linux/v6.8/fs/namespace.c
    const COMPATIBLE_BACKENDS: [Backend; 3] = [
        Backend::UniqueIdQuery,
        Backend::LegacyWitness,
        Backend::DescriptorQuery,
    ];

    #[test]
    fn compatible_id_query_selection_does_not_attempt_pre_by_fd_invalid_flags() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        let mut context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::UniqueIdQuery),
        );
        context.queries.backend_order = COMPATIBLE_BACKENDS;
        context.queries.descriptor_statmount = Some([Reply::InvalidRequest, Reply::InvalidRequest]);
        // Selection changes only the operations; all evidence checks remain real.
        assert!(context.queries.selected_backends() == COMPATIBLE_BACKENDS);
        assert_eq!(
            (
                context.verify_regular_file(&file),
                context.queries.mapping_query_calls()
            ),
            (Ok(()), [0, 2, 2, 0, 0]),
        );
    }

    #[test]
    fn compatible_live_witness_selection_does_not_attempt_pre_by_fd_invalid_flags() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        let mut context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::LegacyWitness),
        );
        context.queries.backend_order = COMPATIBLE_BACKENDS;
        context.queries.descriptor_statmount = Some([Reply::InvalidRequest, Reply::InvalidRequest]);
        assert!(context.queries.selected_backends() == COMPATIBLE_BACKENDS);
        assert_eq!(
            (
                context.verify_directory(&directory),
                context.queries.mapping_query_calls()
            ),
            (Ok(()), [0, 2, 0, 2, 2]),
        );
    }

    #[test]
    fn selected_fd_query_errors_are_terminal_despite_available_id_evidence() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        for reply in [Reply::Denied, Reply::InvalidRequest] {
            let mut context = LinuxProtectionContext::for_fixture(
                &authority,
                ordinary_samples(Backend::UniqueIdQuery),
            );
            context.queries.descriptor_statmount = Some([reply.clone(), reply]);
            assert_eq!(
                (
                    context.verify_regular_file(&file),
                    context.queries.mapping_query_calls()
                ),
                (Err(ProtectedDirectoryError::Unavailable), [2, 0, 0, 0, 0]),
            );
        }
    }

    #[test]
    fn malformed_missing_basic_fd_reply_is_terminal_despite_valid_independent_id_evidence() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        let mut mount_options = missing_basic_mount_record();
        mount_options.word(4, 1);
        let mut reserved_byte = missing_basic_mount_record();
        reserved_byte.bytes[504] = 1;
        let mut unrequested_mask = missing_basic_mount_record();
        unrequested_mask.long_word(8, 1);
        let mut unknown_mask = missing_basic_mount_record();
        unknown_mask.long_word(8, 1 << 63);
        // Injected malformed replies, not a claim that a kernel emits them. The
        // independent ID backend has valid outputs in both rounds and live authority.
        for record in [mount_options, reserved_byte, unrequested_mask, unknown_mask] {
            let mut context = LinuxProtectionContext::for_fixture(
                &authority,
                ordinary_samples(Backend::UniqueIdQuery),
            );
            context.queries.descriptor_statmount = Some([
                Reply::Output(Box::new(record.clone())),
                Reply::Output(Box::new(record)),
            ]);
            assert_eq!(
                (
                    context.verify_regular_file(&file),
                    context.queries.mapping_query_calls()
                ),
                (Err(ProtectedDirectoryError::Unavailable), [2, 0, 0, 0, 0]),
            );
        }
    }

    #[test]
    fn well_formed_missing_basic_fd_reply_allows_bounded_id_or_live_witness_fallback() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        for (backend, calls) in [
            (Backend::UniqueIdQuery, [2, 2, 2, 0, 0]),
            (Backend::LegacyWitness, [2, 2, 0, 2, 2]),
        ] {
            let mut context =
                LinuxProtectionContext::for_fixture(&authority, ordinary_samples(backend));
            context.queries.descriptor_statmount = Some(std::array::from_fn(|_| {
                Reply::Output(Box::new(missing_basic_mount_record()))
            }));
            assert_eq!(
                (
                    context.verify_directory(&directory),
                    context.queries.mapping_query_calls()
                ),
                (Ok(()), calls),
            );
        }
    }

    #[test]
    fn three_backend_fallback_with_successful_unique_and_legacy_ids_is_admitted() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        let mut context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::LegacyWitness),
        );
        // BACKENDS is the order selected for a "7.0.0" release hint. Fixture
        // selection charges one WorkBudget attempt but supplies no authority.
        // FD and ID statmount both return ENOSYS. Unique statx must succeed too,
        // rather than skipping the ID query as the ordinary legacy fixture does.
        context.queries.descriptor_statmount = Some([Reply::Unsupported, Reply::Unsupported]);
        context.queries.unique_statx = Some(std::array::from_fn(|_| {
            DescriptorSample::ordinary(Backend::UniqueIdQuery)
                .mapping
                .statx
        }));
        let unique_id = DescriptorMountId {
            kind: MountIdKind::Unique,
            value: 0x1_0000_0025,
        };
        assert_eq!(
            (
                context.verify_regular_file(&file),
                context.queries.mapping_query_calls(),
                context.queries.mapping_query_trace(),
            ),
            (
                Ok(()),
                [2, 2, 2, 2, 2],
                vec![
                    (0, MappingQuery::DescriptorStatmount),
                    (0, MappingQuery::UniqueStatx),
                    (0, MappingQuery::IdStatmount(unique_id)),
                    (0, MappingQuery::LegacyStatx),
                    (0, MappingQuery::OriginalStatx),
                    (1, MappingQuery::DescriptorStatmount),
                    (1, MappingQuery::UniqueStatx),
                    (1, MappingQuery::IdStatmount(unique_id)),
                    (1, MappingQuery::LegacyStatx),
                    (1, MappingQuery::OriginalStatx),
                ],
            ),
        );
    }

    #[test]
    fn three_backend_fallback_with_one_retry_retains_final_metadata_capacity() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        let mut context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::LegacyWitness),
        );
        context.queries.descriptor_statmount = Some([Reply::Unsupported, Reply::Unsupported]);
        context.queries.unique_statx =
            Some(std::array::from_fn(
                |round| match DescriptorSample::ordinary(Backend::UniqueIdQuery)
                    .mapping
                    .statx
                {
                    Reply::Output(record) if round == 0 => Reply::InterruptedThenOutput(record),
                    reply => reply,
                },
            ));
        // The uninterrupted path consumes 31 attempts, including the retained
        // boot descriptor's close. One retry leaves the final metadata at 32.
        assert_eq!(
            (
                context.verify_regular_file(&file),
                context.queries.mapping_query_calls(),
            ),
            (Ok(()), [2, 2, 2, 2, 2]),
        );
    }

    #[test]
    fn three_backend_fallback_cannot_skip_final_metadata_when_two_retries_fill_the_budget() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        let mut context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::LegacyWitness),
        );
        context.queries.descriptor_statmount = Some([Reply::Unsupported, Reply::Unsupported]);
        context.queries.unique_statx = Some(std::array::from_fn(|_| {
            match DescriptorSample::ordinary(Backend::UniqueIdQuery)
                .mapping
                .statx
            {
                Reply::Output(record) => Reply::InterruptedThenOutput(record),
                _ => panic!("the unique mount fixture has no statx output"),
            }
        }));
        // Both complete mapping samples fit, but two retries leave no capacity
        // for required final metadata. Admission cannot perform attempt 33.
        assert_eq!(
            (
                context.verify_regular_file(&file),
                context.queries.mapping_query_calls(),
            ),
            (Err(ProtectedDirectoryError::Unavailable), [2, 2, 2, 2, 2]),
        );
    }

    #[test]
    fn well_formed_basic_slave_mount_with_master_is_admitted() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        let mut samples = ordinary_samples(Backend::DescriptorQuery);
        for sample in &mut samples {
            let record = sample.mapping.statmount_record();
            record.long_word(72, 0x0008_0000);
            record.long_word(88, 1);
            record.long_word(96, 0);
        }
        let context = LinuxProtectionContext::for_fixture(&authority, samples);
        assert_eq!(
            (
                context.verify_regular_file(&file),
                context.queries.mapping_query_calls(),
            ),
            (Ok(()), [2, 0, 0, 0, 0]),
        );
    }

    #[test]
    fn basic_slave_reply_with_unrequested_propagate_from_is_unavailable() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        let mut descriptor = DescriptorSample::ordinary(Backend::DescriptorQuery);
        let record = descriptor.mapping.statmount_record();
        record.long_word(72, 0x0008_0000);
        record.long_word(88, 1);
        record.long_word(96, 1);
        let reply = descriptor.mapping.statmount;
        let mut context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::UniqueIdQuery),
        );
        context.queries.descriptor_statmount = Some([reply.clone(), reply]);
        // Injected malformed output, not a claim that Linux emits it. A valid
        // independent ID query and live witness cannot replace this failure.
        assert_eq!(
            (
                context.verify_regular_file(&file),
                context.queries.mapping_query_calls(),
            ),
            (Err(ProtectedDirectoryError::Unavailable), [2, 0, 0, 0, 0]),
        );
    }

    #[test]
    fn missing_basic_slave_reply_with_unrequested_propagate_from_never_falls_back() {
        let file = regular_handle();
        let authority = InjectedLinuxAuthority::for_handle(&file);
        let mut descriptor = DescriptorSample::ordinary(Backend::DescriptorQuery);
        let record = descriptor.mapping.statmount_record();
        record.long_word(8, 0);
        record.long_word(72, 0x0008_0000);
        record.long_word(88, 1);
        record.long_word(96, 1);
        let reply = descriptor.mapping.statmount;
        let mut context = LinuxProtectionContext::for_fixture(
            &authority,
            ordinary_samples(Backend::UniqueIdQuery),
        );
        context.queries.descriptor_statmount = Some([reply.clone(), reply]);
        assert_eq!(
            (
                context.verify_regular_file(&file),
                context.queries.mapping_query_calls(),
            ),
            (Err(ProtectedDirectoryError::Unavailable), [2, 0, 0, 0, 0]),
        );
    }

    #[test]
    fn independently_decoded_selected_id_change_precedes_a_later_denied_statmount_query() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        let mut samples = ordinary_samples(Backend::UniqueIdQuery);
        // Injected validator contract only. This does not claim a held mount's
        // unique ID changes natively. Both IDs independently pass statx decoding.
        samples[1]
            .mapping
            .statx_record()
            .long_word(144, UNIQUE_MOUNT_ID + 1);
        samples[1].mapping.statmount = Reply::Denied;
        let context = LinuxProtectionContext::for_fixture(&authority, samples);
        assert_eq!(
            (
                context.verify_directory(&directory),
                context.queries.mapping_query_calls()
            ),
            (Err(ProtectedDirectoryError::Changed), [2, 2, 2, 0, 0]),
        );
    }

    #[test]
    fn matching_selected_ids_with_a_later_denied_statmount_query_are_unavailable() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        let mut samples = ordinary_samples(Backend::UniqueIdQuery);
        samples[1].mapping.statmount = Reply::Denied;
        let context = LinuxProtectionContext::for_fixture(&authority, samples);
        assert_eq!(
            (
                context.verify_directory(&directory),
                context.queries.mapping_query_calls()
            ),
            (Err(ProtectedDirectoryError::Unavailable), [2, 2, 2, 0, 0]),
        );
    }

    #[test]
    fn invalid_or_malformed_selected_statx_with_a_later_denial_is_unavailable() {
        let directory = directory_handle();
        let authority = InjectedLinuxAuthority::for_handle(&directory);
        for (mount_id, written) in [(0, STATX_BYTES), (UNIQUE_MOUNT_ID + 1, STATX_BYTES - 1)] {
            let mut samples = ordinary_samples(Backend::UniqueIdQuery);
            let record = samples[1].mapping.statx_record();
            record.long_word(144, mount_id);
            record.written = written;
            samples[1].mapping.statmount = Reply::Denied;
            let context = LinuxProtectionContext::for_fixture(&authority, samples);
            assert_eq!(
                (
                    context.verify_directory(&directory),
                    context.queries.mapping_query_calls()
                ),
                (Err(ProtectedDirectoryError::Unavailable), [2, 2, 1, 0, 0]),
            );
        }
    }
}
