//! Bounded Linux mount evidence decoding and the private native-query seam.
//!
//! A mount query establishes selected-mount facts, not publication authority.

use std::fs::File;
use std::io;

use super::linux::FilesystemVersion;
use crate::protected_directory::ProtectedDirectoryError;

pub(super) const STATX_BYTES: usize = 256;
pub(super) const STATMOUNT_BYTES: usize = 512;
pub(super) const MAX_MOUNT_WITNESSES: usize = 256;
pub(super) const MAX_PUBLICATION_ENTRIES: usize = 256;
pub(super) const MAX_EVIDENCE_SYSCALLS_PER_HANDLE: usize = 32;
pub(super) const MAX_BACKEND_ATTEMPTS: usize = 3;
pub(super) const MAX_QUERY_INTERRUPTS: usize = 4;

// Linux include/uapi/linux/{stat,mount}.h. Request one statx mount-ID domain at
// a time. Basic statmount deliberately requests no strings or mapping arrays.
pub(super) const STATX_MNT_ID: u32 = 0x1000;
pub(super) const STATX_MNT_ID_UNIQUE: u32 = 0x4000;
pub(super) const STATMOUNT_MNT_BASIC: u64 = 2;
pub(super) const MOUNT_ATTR_IDMAP: u64 = 0x0010_0000;
const KNOWN_STATX_MASK: u32 = 0x0003_ffff;
const KNOWN_MOUNT_ATTRIBUTES: u64 = 0x0030_00bf;
const PROPAGATION_FLAGS: u64 = 0x001e_0000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EntryIdentity {
    pub(super) device: u64,
    pub(super) inode: u64,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct BootIdentity(pub(super) [u8; 16]);

#[derive(Clone, Copy)]
pub(super) enum Namespace {
    User,
    Mount,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MountIdKind {
    Legacy,
    Unique,
}

impl MountIdKind {
    pub(super) const fn mask(self) -> u32 {
        match self {
            Self::Legacy => STATX_MNT_ID,
            Self::Unique => STATX_MNT_ID_UNIQUE,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Backend {
    DescriptorQuery,
    UniqueIdQuery,
    LegacyWitness,
}

pub(super) const BACKENDS: [Backend; MAX_BACKEND_ATTEMPTS] = [
    Backend::DescriptorQuery,
    Backend::UniqueIdQuery,
    Backend::LegacyWitness,
];

pub(super) type BackendSelection = [Option<Backend>; MAX_BACKEND_ATTEMPTS];

#[cfg(test)]
mod budget_tests {
    use std::cell::Cell;
    use std::io;

    use super::WorkBudget;
    use crate::protected_directory::ProtectedDirectoryError;

    #[test]
    fn the_inspector_never_invokes_a_thirty_third_native_attempt() {
        let mut work = WorkBudget::new();
        let calls = Cell::new(0);
        for _ in 0..32 {
            assert_eq!(
                work.retry(|| {
                    calls.set(calls.get() + 1);
                    Ok(())
                }),
                Ok(())
            );
        }
        assert_eq!(
            work.retry(|| {
                calls.set(calls.get() + 1);
                Ok(())
            }),
            Err(ProtectedDirectoryError::Unavailable)
        );
        assert_eq!(calls.get(), 32);
    }

    #[test]
    fn transient_interruptions_share_one_four_interruption_inspection_budget() {
        let mut work = WorkBudget::new();
        let calls = Cell::new(0);
        for _ in 0..4 {
            let interrupted = Cell::new(false);
            assert_eq!(
                work.retry(|| {
                    calls.set(calls.get() + 1);
                    if !interrupted.replace(true) {
                        Err(io::Error::from_raw_os_error(libc::EINTR))
                    } else {
                        Ok(())
                    }
                }),
                Ok(())
            );
        }
        assert_eq!(
            work.retry(|| {
                calls.set(calls.get() + 1);
                Err::<(), _>(io::Error::from_raw_os_error(libc::EINTR))
            }),
            Err(ProtectedDirectoryError::Unavailable)
        );
        assert_eq!(calls.get(), 9);
    }

    #[test]
    fn reserving_a_temporary_descriptor_close_consumes_the_native_call_bound() {
        let mut work = WorkBudget::new();
        for _ in 0..31 {
            assert_eq!(work.retry(|| Ok(())), Ok(()));
        }
        assert_eq!(work.reserve_cleanup(), Ok(()));
        let invoked = Cell::new(false);
        assert_eq!(
            work.retry(|| {
                invoked.set(true);
                Ok(())
            }),
            Err(ProtectedDirectoryError::Unavailable)
        );
        assert!(!invoked.get());
    }
}

#[derive(Clone)]
pub(super) struct Record<const N: usize> {
    pub(super) bytes: [u8; N],
    pub(super) written: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DescriptorMountId {
    pub(super) kind: MountIdKind,
    pub(super) value: u64,
}

impl DescriptorMountId {
    pub(super) fn decode(
        record: &Record<STATX_BYTES>,
        kind: MountIdKind,
    ) -> Result<Self, ProtectedDirectoryError> {
        if record.written != STATX_BYTES {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        let mask = word(&record.bytes, 0)?;
        if mask & !KNOWN_STATX_MASK != 0
            || mask & (STATX_MNT_ID | STATX_MNT_ID_UNIQUE) == (STATX_MNT_ID | STATX_MNT_ID_UNIQUE)
            || record.bytes[30..32] != [0, 0]
            || long_word(&record.bytes, 8)? & !long_word(&record.bytes, 56)? != 0
            || record.bytes[188..].iter().any(|byte| *byte != 0)
        {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        for offset in [64, 80, 96, 112] {
            if word(&record.bytes, offset + 8)? >= 1_000_000_000
                || word(&record.bytes, offset + 12)? != 0
            {
                return Err(ProtectedDirectoryError::Unavailable);
            }
        }
        if mask & kind.mask() == 0 {
            // Includes successful glibc fallbacks and kernels ignoring newer bits.
            return Err(ProtectedDirectoryError::NativeUnavailable);
        }
        let value = long_word(&record.bytes, 144)?;
        let valid = match kind {
            MountIdKind::Legacy => value != 0 && value <= u64::from(u32::MAX),
            // Unique mount IDs occupy a different domain from reusable old IDs.
            MountIdKind::Unique => value > u64::from(u32::MAX),
        };
        if !valid {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        Ok(Self { kind, value })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BasicMountVersion {
    pub(super) unique_id: u64,
    pub(super) parent_unique_id: u64,
    pub(super) legacy_id: u32,
    pub(super) parent_legacy_id: u32,
    pub(super) attributes: u64,
    pub(super) propagation: u64,
    pub(super) peer_group: u64,
    pub(super) master: u64,
    pub(super) propagate_from: u64,
}

impl BasicMountVersion {
    pub(super) fn decode(
        record: &Record<STATMOUNT_BYTES>,
        requested_id: Option<DescriptorMountId>,
    ) -> Result<Self, ProtectedDirectoryError> {
        if record.written != STATMOUNT_BYTES || word(&record.bytes, 0)? != STATMOUNT_BYTES as u32 {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        let mask = long_word(&record.bytes, 8)?;
        // Validate the unconditional envelope before allowing missing-capability
        // fallback. Offset 4 is mnt_opts; offset 96 is PROPAGATE_FROM, not BASIC.
        if mask & !STATMOUNT_MNT_BASIC != 0
            || word(&record.bytes, 4)? != 0
            || record.bytes[16..40].iter().any(|byte| *byte != 0)
            || record.bytes[96..].iter().any(|byte| *byte != 0)
        {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        if mask & STATMOUNT_MNT_BASIC == 0 {
            return Err(ProtectedDirectoryError::NativeUnavailable);
        }
        let version = Self {
            unique_id: long_word(&record.bytes, 40)?,
            parent_unique_id: long_word(&record.bytes, 48)?,
            legacy_id: word(&record.bytes, 56)?,
            parent_legacy_id: word(&record.bytes, 60)?,
            attributes: long_word(&record.bytes, 64)?,
            propagation: long_word(&record.bytes, 72)?,
            peer_group: long_word(&record.bytes, 80)?,
            master: long_word(&record.bytes, 88)?,
            propagate_from: long_word(&record.bytes, 96)?,
        };
        if version.unique_id <= u64::from(u32::MAX)
            || version.legacy_id == 0
            || (version.parent_unique_id == 0) != (version.parent_legacy_id == 0)
            || (version.parent_unique_id != 0 && version.parent_unique_id <= u64::from(u32::MAX))
            || version.attributes & !KNOWN_MOUNT_ATTRIBUTES != 0
            || version.attributes & 0x30 == 0x30
            || version.propagation & !PROPAGATION_FLAGS != 0
            || (version.propagation & 0x0006_0000 != 0 && version.propagation.count_ones() != 1)
            || (version.peer_group != 0 && version.propagation & 0x0010_0000 == 0)
            || ((version.master != 0 || version.propagate_from != 0)
                && version.propagation & 0x0008_0000 == 0)
            || requested_id
                .is_some_and(|id| id.kind != MountIdKind::Unique || id.value != version.unique_id)
        {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        Ok(version)
    }

    pub(super) fn require_non_idmapped(self) -> Result<(), ProtectedDirectoryError> {
        if self.attributes & MOUNT_ATTR_IDMAP != 0 {
            Err(ProtectedDirectoryError::Unsafe)
        } else {
            Ok(())
        }
    }
}

// This budget is independent of the enclosing observer's 4096-call allowance.
// Namespace, filesystem, metadata, boot and mount queries all consume it. A
// temporary descriptor's one close is reserved before its first open attempt.
pub(super) struct WorkBudget {
    calls: usize,
    interrupts: usize,
}

impl WorkBudget {
    pub(super) fn new() -> Self {
        Self {
            calls: 0,
            interrupts: 0,
        }
    }

    pub(super) fn reserve_cleanup(&mut self) -> Result<(), ProtectedDirectoryError> {
        self.step()
    }

    fn step(&mut self) -> Result<(), ProtectedDirectoryError> {
        if self.calls >= MAX_EVIDENCE_SYSCALLS_PER_HANDLE {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        self.calls += 1;
        Ok(())
    }

    pub(super) fn retry<T>(
        &mut self,
        mut operation: impl FnMut() -> io::Result<T>,
    ) -> Result<T, ProtectedDirectoryError> {
        loop {
            self.step()?;
            match operation() {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    if self.interrupts >= MAX_QUERY_INTERRUPTS {
                        return Err(ProtectedDirectoryError::Unavailable);
                    }
                    self.interrupts += 1;
                }
                Err(error) => {
                    return Err(match error.raw_os_error() {
                        Some(code) if [libc::ENOSYS, libc::EOPNOTSUPP].contains(&code) => {
                            ProtectedDirectoryError::NativeUnavailable
                        }
                        _ => ProtectedDirectoryError::Unavailable,
                    });
                }
                Ok(value) => return Ok(value),
            }
        }
    }
}

/// Native outputs, capability failures and operation-selection hints are injectable.
/// Admission, record validation, original-pin matching and publication checks remain real code.
pub(super) trait Queries {
    // Retain only an opened descriptor between samples, never a cached identity
    // or failed capability. Its close must be reserved in this inspection's budget.
    type BootLease: Default;

    // Operation selection is only a capability hint. Every chosen backend must
    // still independently establish its evidence under the existing authority.
    fn select_backends(
        &self,
        work: &mut WorkBudget,
    ) -> Result<BackendSelection, ProtectedDirectoryError>;

    fn filesystem_version(
        &self,
        file: &File,
        round: usize,
        work: &mut WorkBudget,
    ) -> Result<FilesystemVersion, ProtectedDirectoryError>;

    fn namespace_identity(
        &self,
        namespace: Namespace,
        work: &mut WorkBudget,
    ) -> Result<EntryIdentity, ProtectedDirectoryError>;

    fn boot_identity(
        &self,
        lease: &mut Self::BootLease,
        work: &mut WorkBudget,
    ) -> Result<BootIdentity, ProtectedDirectoryError>;

    fn statx(
        &self,
        file: &File,
        kind: MountIdKind,
        round: usize,
        work: &mut WorkBudget,
    ) -> Result<Record<STATX_BYTES>, ProtectedDirectoryError>;

    fn statmount_by_id(
        &self,
        file: &File,
        id: DescriptorMountId,
        round: usize,
        work: &mut WorkBudget,
    ) -> Result<Record<STATMOUNT_BYTES>, ProtectedDirectoryError>;

    fn statmount_by_fd(
        &self,
        file: &File,
        round: usize,
        work: &mut WorkBudget,
    ) -> Result<Record<STATMOUNT_BYTES>, ProtectedDirectoryError>;
}

fn word(record: &[u8], offset: usize) -> Result<u32, ProtectedDirectoryError> {
    let bytes = record
        .get(offset..offset + 4)
        .ok_or(ProtectedDirectoryError::Unavailable)?;
    let bytes = <[u8; 4]>::try_from(bytes).map_err(|_| ProtectedDirectoryError::Unavailable)?;
    Ok(u32::from_ne_bytes(bytes))
}

fn long_word(record: &[u8], offset: usize) -> Result<u64, ProtectedDirectoryError> {
    let bytes = record
        .get(offset..offset + 8)
        .ok_or(ProtectedDirectoryError::Unavailable)?;
    let bytes = <[u8; 8]>::try_from(bytes).map_err(|_| ProtectedDirectoryError::Unavailable)?;
    Ok(u64::from_ne_bytes(bytes))
}

#[cfg(test)]
pub(super) mod testing {
    use std::cell::{Cell, RefCell};
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    use super::*;

    pub(in crate::root_protection) const LEGACY_MOUNT_ID: u64 = 37;
    pub(in crate::root_protection) const UNIQUE_MOUNT_ID: u64 = 0x1_0000_0025;

    #[derive(Clone, Copy)]
    pub(in crate::root_protection) struct FilesystemFacts {
        pub(in crate::root_protection) filesystem_type: u64,
        pub(in crate::root_protection) filesystem_id: libc::c_ulong,
        pub(in crate::root_protection) mount_flags: libc::c_ulong,
    }

    impl FilesystemFacts {
        const fn ext4() -> Self {
            Self {
                filesystem_type: 0xEF53,
                filesystem_id: 17,
                mount_flags: 0,
            }
        }
    }

    impl<const N: usize> Record<N> {
        pub(in crate::root_protection) fn word(&mut self, offset: usize, value: u32) {
            self.bytes[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
        }

        pub(in crate::root_protection) fn long_word(&mut self, offset: usize, value: u64) {
            self.bytes[offset..offset + 8].copy_from_slice(&value.to_ne_bytes());
        }
    }

    pub(in crate::root_protection) fn missing_basic_mount_record() -> Record<STATMOUNT_BYTES> {
        let mut record = Record {
            bytes: [0; STATMOUNT_BYTES],
            written: STATMOUNT_BYTES,
        };
        record.word(0, STATMOUNT_BYTES as u32);
        record
    }

    #[derive(Clone)]
    pub(in crate::root_protection) enum Reply<const N: usize> {
        Output(Box<Record<N>>),
        InterruptedThenOutput(Box<Record<N>>),
        Unsupported,
        Denied,
        InvalidRequest,
        Failed,
        RepeatedInterruptions,
    }

    impl<const N: usize> Reply<N> {
        fn query(&self, work: &mut WorkBudget) -> Result<Record<N>, ProtectedDirectoryError> {
            let mut interrupted = false;
            work.retry(|| match self {
                Self::Output(record) => Ok(record.as_ref().clone()),
                Self::InterruptedThenOutput(record) => {
                    if std::mem::replace(&mut interrupted, true) {
                        Ok(record.as_ref().clone())
                    } else {
                        Err(io::Error::from_raw_os_error(libc::EINTR))
                    }
                }
                Self::Unsupported => Err(io::Error::from_raw_os_error(libc::ENOSYS)),
                Self::Denied => Err(io::Error::from_raw_os_error(libc::EPERM)),
                Self::InvalidRequest => Err(io::Error::from_raw_os_error(libc::EINVAL)),
                Self::Failed => Err(io::Error::from_raw_os_error(libc::EIO)),
                Self::RepeatedInterruptions => Err(io::Error::from_raw_os_error(libc::EINTR)),
            })
        }
    }

    #[derive(Clone)]
    pub(in crate::root_protection) struct MappingObservation {
        pub(in crate::root_protection) backend: Backend,
        pub(in crate::root_protection) statx: Reply<STATX_BYTES>,
        pub(in crate::root_protection) statmount: Reply<STATMOUNT_BYTES>,
    }

    impl MappingObservation {
        fn ordinary(backend: Backend) -> Self {
            let mut statx = Record {
                bytes: [0; STATX_BYTES],
                written: STATX_BYTES,
            };
            let (mask, mount_id) = match backend {
                Backend::LegacyWitness => (STATX_MNT_ID, LEGACY_MOUNT_ID),
                Backend::UniqueIdQuery | Backend::DescriptorQuery => {
                    (STATX_MNT_ID_UNIQUE, UNIQUE_MOUNT_ID)
                }
            };
            statx.word(0, mask);
            statx.long_word(144, mount_id);
            let mut statmount = Record {
                bytes: [0; STATMOUNT_BYTES],
                written: STATMOUNT_BYTES,
            };
            statmount.word(0, STATMOUNT_BYTES as u32);
            statmount.long_word(8, STATMOUNT_MNT_BASIC);
            statmount.long_word(40, UNIQUE_MOUNT_ID);
            statmount.word(56, LEGACY_MOUNT_ID as u32);
            Self {
                backend,
                statx: Reply::Output(Box::new(statx)),
                statmount: match backend {
                    Backend::LegacyWitness => Reply::Unsupported,
                    Backend::UniqueIdQuery | Backend::DescriptorQuery => {
                        Reply::Output(Box::new(statmount))
                    }
                },
            }
        }

        pub(in crate::root_protection) fn statx_record(&mut self) -> &mut Record<STATX_BYTES> {
            match &mut self.statx {
                Reply::Output(record) => record,
                _ => panic!("the fixture has no statx output"),
            }
        }

        pub(in crate::root_protection) fn statmount_record(
            &mut self,
        ) -> &mut Record<STATMOUNT_BYTES> {
            match &mut self.statmount {
                Reply::Output(record) => record,
                _ => panic!("the fixture has no statmount output"),
            }
        }
    }

    #[derive(Clone)]
    pub(in crate::root_protection) struct DescriptorSample {
        pub(in crate::root_protection) filesystem: FilesystemFacts,
        pub(in crate::root_protection) mapping: MappingObservation,
    }

    impl DescriptorSample {
        pub(in crate::root_protection) fn ordinary(backend: Backend) -> Self {
            Self {
                filesystem: FilesystemFacts::ext4(),
                mapping: MappingObservation::ordinary(backend),
            }
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(in crate::root_protection) enum MappingQuery {
        DescriptorStatmount,
        UniqueStatx,
        IdStatmount(DescriptorMountId),
        LegacyStatx,
        OriginalStatx,
    }

    // These are scripted OS replies, never an issuer or a replacement for handle
    // metadata. Original-pin output is independent of the witness's recorded ID.
    pub(in crate::root_protection) struct FixtureQueries {
        pub(in crate::root_protection) samples: [DescriptorSample; 2],
        pub(in crate::root_protection) original_descriptors: Vec<libc::c_int>,
        pub(in crate::root_protection) original_statx: [Reply<STATX_BYTES>; 2],
        pub(in crate::root_protection) unique_statx: Option<[Reply<STATX_BYTES>; 2]>,
        pub(in crate::root_protection) descriptor_statmount: Option<[Reply<STATMOUNT_BYTES>; 2]>,
        pub(in crate::root_protection) mapping_query_trace: RefCell<Vec<(usize, MappingQuery)>>,
        pub(in crate::root_protection) backend_order: [Backend; MAX_BACKEND_ATTEMPTS],
        pub(in crate::root_protection) fd_statmount_calls: Cell<usize>,
        pub(in crate::root_protection) unique_statx_calls: Cell<usize>,
        pub(in crate::root_protection) id_statmount_calls: Cell<usize>,
        pub(in crate::root_protection) legacy_statx_calls: Cell<usize>,
        pub(in crate::root_protection) original_statx_calls: Cell<usize>,
        pub(in crate::root_protection) user_identity: EntryIdentity,
        pub(in crate::root_protection) mount_identity: EntryIdentity,
        pub(in crate::root_protection) boot: BootIdentity,
    }

    impl FixtureQueries {
        pub(in crate::root_protection) fn selected_backends(
            &self,
        ) -> [Backend; MAX_BACKEND_ATTEMPTS] {
            self.backend_order
        }

        // Boundary invocations, ordered as FD, unique statx, ID statmount, legacy
        // statx and original-pin statx. WorkBudget still bounds native attempts.
        pub(in crate::root_protection) fn mapping_query_calls(&self) -> [usize; 5] {
            [
                self.fd_statmount_calls.get(),
                self.unique_statx_calls.get(),
                self.id_statmount_calls.get(),
                self.legacy_statx_calls.get(),
                self.original_statx_calls.get(),
            ]
        }

        pub(in crate::root_protection) fn mapping_query_trace(&self) -> Vec<(usize, MappingQuery)> {
            self.mapping_query_trace.borrow().clone()
        }
    }

    impl Queries for FixtureQueries {
        type BootLease = bool;

        fn select_backends(
            &self,
            work: &mut WorkBudget,
        ) -> Result<BackendSelection, ProtectedDirectoryError> {
            work.retry(|| Ok(self.selected_backends().map(Some)))
        }

        fn filesystem_version(
            &self,
            _file: &File,
            round: usize,
            work: &mut WorkBudget,
        ) -> Result<FilesystemVersion, ProtectedDirectoryError> {
            let facts = self.samples[round].filesystem;
            work.retry(|| Ok(()))?;
            work.retry(|| {
                Ok(FilesystemVersion {
                    filesystem_type: facts.filesystem_type,
                    filesystem_id: facts.filesystem_id,
                    mount_flags: facts.mount_flags,
                })
            })
        }

        fn namespace_identity(
            &self,
            namespace: Namespace,
            work: &mut WorkBudget,
        ) -> Result<EntryIdentity, ProtectedDirectoryError> {
            work.retry(|| {
                Ok(match namespace {
                    Namespace::User => self.user_identity,
                    Namespace::Mount => self.mount_identity,
                })
            })
        }

        fn boot_identity(
            &self,
            lease: &mut Self::BootLease,
            work: &mut WorkBudget,
        ) -> Result<BootIdentity, ProtectedDirectoryError> {
            if !*lease {
                work.reserve_cleanup()?;
                work.retry(|| Ok(()))?;
                *lease = true;
            }
            work.retry(|| Ok(self.boot))
        }

        fn statx(
            &self,
            file: &File,
            kind: MountIdKind,
            round: usize,
            work: &mut WorkBudget,
        ) -> Result<Record<STATX_BYTES>, ProtectedDirectoryError> {
            if self.original_descriptors.contains(&file.as_raw_fd()) {
                self.mapping_query_trace
                    .borrow_mut()
                    .push((round, MappingQuery::OriginalStatx));
                self.original_statx_calls
                    .set(self.original_statx_calls.get() + 1);
                return self.original_statx[round].query(work);
            }
            let query = match kind {
                MountIdKind::Unique => MappingQuery::UniqueStatx,
                MountIdKind::Legacy => MappingQuery::LegacyStatx,
            };
            self.mapping_query_trace.borrow_mut().push((round, query));
            let calls = match kind {
                MountIdKind::Unique => &self.unique_statx_calls,
                MountIdKind::Legacy => &self.legacy_statx_calls,
            };
            calls.set(calls.get() + 1);
            if let (MountIdKind::Unique, Some(replies)) = (kind, &self.unique_statx) {
                return replies[round].query(work);
            }
            let mapping = &self.samples[round].mapping;
            match (mapping.backend, kind) {
                (Backend::LegacyWitness, MountIdKind::Legacy)
                | (Backend::UniqueIdQuery, MountIdKind::Unique) => mapping.statx.query(work),
                _ => Reply::Unsupported.query(work),
            }
        }

        fn statmount_by_id(
            &self,
            _file: &File,
            id: DescriptorMountId,
            round: usize,
            work: &mut WorkBudget,
        ) -> Result<Record<STATMOUNT_BYTES>, ProtectedDirectoryError> {
            self.mapping_query_trace
                .borrow_mut()
                .push((round, MappingQuery::IdStatmount(id)));
            self.id_statmount_calls
                .set(self.id_statmount_calls.get() + 1);
            let mapping = &self.samples[round].mapping;
            if mapping.backend == Backend::UniqueIdQuery {
                mapping.statmount.query(work)
            } else {
                Reply::Unsupported.query(work)
            }
        }

        fn statmount_by_fd(
            &self,
            _file: &File,
            round: usize,
            work: &mut WorkBudget,
        ) -> Result<Record<STATMOUNT_BYTES>, ProtectedDirectoryError> {
            self.mapping_query_trace
                .borrow_mut()
                .push((round, MappingQuery::DescriptorStatmount));
            self.fd_statmount_calls
                .set(self.fd_statmount_calls.get() + 1);
            if let Some(replies) = &self.descriptor_statmount {
                return replies[round].query(work);
            }
            let mapping = &self.samples[round].mapping;
            if mapping.backend == Backend::DescriptorQuery {
                mapping.statmount.query(work)
            } else {
                Reply::Unsupported.query(work)
            }
        }
    }
}

#[cfg(test)]
mod decode_tests {
    use super::testing::{DescriptorSample, missing_basic_mount_record};
    use super::{Backend, BasicMountVersion, ProtectedDirectoryError};

    #[test]
    fn a_basic_slave_mount_can_report_a_master_without_propagate_from() {
        let mut sample = DescriptorSample::ordinary(Backend::DescriptorQuery);
        let record = sample.mapping.statmount_record();
        record.long_word(72, 0x0008_0000);
        record.long_word(88, 1);
        record.long_word(96, 0);
        assert_eq!(
            BasicMountVersion::decode(record, None),
            Ok(BasicMountVersion {
                unique_id: 0x1_0000_0025,
                parent_unique_id: 0,
                legacy_id: 37,
                parent_legacy_id: 0,
                attributes: 0,
                propagation: 0x0008_0000,
                peer_group: 0,
                master: 1,
                propagate_from: 0,
            }),
        );
    }

    #[test]
    fn a_basic_slave_reply_cannot_report_unrequested_propagate_from() {
        let mut sample = DescriptorSample::ordinary(Backend::DescriptorQuery);
        let record = sample.mapping.statmount_record();
        record.long_word(72, 0x0008_0000);
        record.long_word(88, 1);
        // propagate_from belongs to STATMOUNT_PROPAGATE_FROM (0x4), not BASIC (0x2).
        record.long_word(96, 1);
        assert_eq!(
            BasicMountVersion::decode(record, None),
            Err(ProtectedDirectoryError::Unavailable),
        );
    }

    #[test]
    fn missing_basic_mask_does_not_hide_unrequested_propagate_from_on_a_slave_mount() {
        let mut sample = DescriptorSample::ordinary(Backend::DescriptorQuery);
        let record = sample.mapping.statmount_record();
        record.long_word(8, 0);
        record.long_word(72, 0x0008_0000);
        record.long_word(88, 1);
        record.long_word(96, 1);
        assert_eq!(
            BasicMountVersion::decode(record, None),
            Err(ProtectedDirectoryError::Unavailable),
        );
    }

    #[test]
    fn missing_basic_mask_does_not_hide_an_unrequested_mount_options_offset() {
        let mut record = missing_basic_mount_record();
        // Offset 4 is mnt_opts, an unrequested string offset, not a reserved word.
        record.word(4, 1);
        assert_eq!(
            BasicMountVersion::decode(&record, None),
            Err(ProtectedDirectoryError::Unavailable),
        );
    }

    #[test]
    fn missing_basic_mask_does_not_hide_a_nonzero_reserved_byte() {
        let mut record = missing_basic_mount_record();
        // Linux v7.0 statmount.__spare2 includes the final eight bytes.
        record.bytes[504] = 1;
        assert_eq!(
            BasicMountVersion::decode(&record, None),
            Err(ProtectedDirectoryError::Unavailable),
        );
    }

    #[test]
    fn missing_basic_mask_does_not_hide_unrequested_or_unknown_mask_bits() {
        for mask in [1, 1 << 63] {
            let mut record = missing_basic_mount_record();
            record.long_word(8, mask);
            assert_eq!(
                BasicMountVersion::decode(&record, None),
                Err(ProtectedDirectoryError::Unavailable),
                "mask {mask:#x}",
            );
        }
    }

    #[test]
    fn a_well_formed_missing_basic_mask_reports_missing_capability() {
        let record = missing_basic_mount_record();
        assert_eq!(
            BasicMountVersion::decode(&record, None),
            Err(ProtectedDirectoryError::NativeUnavailable),
        );
    }
}
