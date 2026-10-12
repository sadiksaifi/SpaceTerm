//! Raw read-only Linux query adapters. None of these operations issues authority.

use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};

use super::linux::{self, FilesystemVersion};
use super::linux_evidence::{
    BACKENDS, Backend, BackendSelection, BootIdentity, DescriptorMountId, EntryIdentity,
    MountIdKind, Namespace, Queries, Record, STATMOUNT_BYTES, STATMOUNT_MNT_BASIC, STATX_BYTES,
    WorkBudget,
};
use crate::protected_directory::ProtectedDirectoryError;

pub(super) struct KernelQueries;

#[repr(C, align(8))]
struct AlignedBuffer<const N: usize>([u8; N]);

// Linux 6.8's version-zero request works without new libc wrappers. Later
// namespace-ID fields are neither requested nor populated by this adapter.
#[repr(C)]
struct MountIdRequest {
    size: u32,
    spare: u32,
    mount_id: u64,
    parameter: u64,
}

const _: [(); 24] = [(); std::mem::size_of::<MountIdRequest>()];

// Linux v7.0 include/uapi/linux/mount.h defines the version-one request's second
// word as a union. STATMOUNT_BY_FD selects mnt_fd and requires both IDs to be zero.
// https://raw.githubusercontent.com/torvalds/linux/v7.0/include/uapi/linux/mount.h
#[repr(C, align(8))]
struct MountFdRequest {
    size: u32,
    mount_fd: u32,
    mount_id: u64,
    parameter: u64,
    mount_namespace_id: u64,
}

const _: [(); 32] = [(); std::mem::size_of::<MountFdRequest>()];
const STATMOUNT_BY_FD: u32 = 0x0000_0001;

// These are SpaceTerm's native Linux executable architectures. Other Linux ABIs
// can still use descriptor statx and independently issued old-mount witnesses.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const SYS_STATMOUNT: libc::c_long = 457;

impl Queries for KernelQueries {
    type BootLease = Option<File>;

    fn select_backends(
        &self,
        work: &mut WorkBudget,
    ) -> Result<BackendSelection, ProtectedDirectoryError> {
        select_mapping_backends(work, || {
            let mut status = MaybeUninit::<libc::utsname>::zeroed();
            // SAFETY: status is writable storage for the fixed Linux uname ABI.
            if unsafe { libc::uname(status.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: uname initialized status on success.
            let status = unsafe { status.assume_init() };
            Ok(status.release.map(|byte| byte as u8))
        })
    }

    fn filesystem_version(
        &self,
        file: &File,
        _round: usize,
        work: &mut WorkBudget,
    ) -> Result<FilesystemVersion, ProtectedDirectoryError> {
        linux::filesystem_version(file, work)
    }

    fn namespace_identity(
        &self,
        namespace: Namespace,
        work: &mut WorkBudget,
    ) -> Result<EntryIdentity, ProtectedDirectoryError> {
        let path = match namespace {
            Namespace::User => c"/proc/thread-self/ns/user",
            Namespace::Mount => c"/proc/thread-self/ns/mnt",
        };
        let mut status = MaybeUninit::<libc::stat>::uninit();
        work.retry(|| {
            // SAFETY: the fixed procfs name is terminated and status is writable.
            // Following this namespace link selects the calling thread's context.
            if unsafe { libc::stat(path.as_ptr(), status.as_mut_ptr()) } != 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        })?;
        // SAFETY: stat initialized status on success.
        let status = unsafe { status.assume_init() };
        Ok(EntryIdentity {
            device: status.st_dev,
            inode: status.st_ino,
        })
    }

    fn boot_identity(
        &self,
        lease: &mut Self::BootLease,
        work: &mut WorkBudget,
    ) -> Result<BootIdentity, ProtectedDirectoryError> {
        // This read is an invalidation check under an existing sealed authority,
        // never a marker, issuer, host-namespace authentication or boot trust root.
        query_boot_identity(
            lease,
            work,
            || {
                // SAFETY: the fixed read-only procfs path is terminated. No creation,
                // mount, namespace or account operation is performed.
                let descriptor = unsafe {
                    libc::open(
                        c"/proc/sys/kernel/random/boot_id".as_ptr(),
                        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                    )
                };
                if descriptor < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    // SAFETY: open returned a new descriptor with its close prepaid.
                    Ok(unsafe { File::from_raw_fd(descriptor) })
                }
            },
            |file, buffer| {
                // SAFETY: file is held and buffer contains 38 writable bytes. Each
                // sample starts at offset zero without a separate seek operation.
                let count = unsafe {
                    libc::pread(
                        file.as_raw_fd(),
                        buffer.as_mut_ptr().cast(),
                        buffer.len(),
                        0,
                    )
                };
                if count < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    usize::try_from(count).map_err(|_| io::Error::from_raw_os_error(libc::EIO))
                }
            },
        )
    }

    fn statx(
        &self,
        file: &File,
        kind: MountIdKind,
        _round: usize,
        work: &mut WorkBudget,
    ) -> Result<Record<STATX_BYTES>, ProtectedDirectoryError> {
        let mut buffer = AlignedBuffer([0; STATX_BYTES]);
        work.retry(|| {
            buffer.0.fill(0);
            // SAFETY: File retains the exact descriptor. AT_EMPTY_PATH selects
            // its f_path, not a reconstructed pathname. The aligned output is the
            // fixed 256-byte Linux statx ABI, including on glibc 2.35.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_statx,
                    file.as_raw_fd(),
                    c"".as_ptr(),
                    libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW | libc::AT_NO_AUTOMOUNT,
                    kind.mask(),
                    buffer.0.as_mut_ptr(),
                )
            };
            syscall_result(result)
        })?;
        Ok(Record {
            bytes: buffer.0,
            written: STATX_BYTES,
        })
    }

    fn statmount_by_id(
        &self,
        _file: &File,
        id: DescriptorMountId,
        _round: usize,
        work: &mut WorkBudget,
    ) -> Result<Record<STATMOUNT_BYTES>, ProtectedDirectoryError> {
        if id.kind != MountIdKind::Unique {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        let request = MountIdRequest {
            size: 24,
            spare: 0,
            mount_id: id.value,
            parameter: STATMOUNT_MNT_BASIC,
        };
        query_statmount(&request, work)
    }

    fn statmount_by_fd(
        &self,
        file: &File,
        _round: usize,
        work: &mut WorkBudget,
    ) -> Result<Record<STATMOUNT_BYTES>, ProtectedDirectoryError> {
        query_statmount_by_fd(file, work, statmount_by_fd_once)
    }
}

fn select_mapping_backends(
    work: &mut WorkBudget,
    mut query_release: impl FnMut() -> io::Result<[u8; 65]>,
) -> Result<BackendSelection, ProtectedDirectoryError> {
    // One bounded uname query per inspection selects operations, never authority.
    // Linux 6.8 rejects nonzero statmount flags before reading the request; the
    // v7.0 interface accepts BY_FD. Conservatively skip optional backports below 7.
    // https://raw.githubusercontent.com/torvalds/linux/v6.8/fs/namespace.c
    // https://raw.githubusercontent.com/torvalds/linux/v7.0/fs/namespace.c
    let release = work.retry(&mut query_release)?;
    let length = release
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(ProtectedDirectoryError::Unavailable)?;
    let mut components = release[..length].split(|byte| *byte == b'.');
    let mut version = [0_u32; 2];
    for number in &mut version {
        let component = components
            .next()
            .filter(|component| !component.is_empty())
            .ok_or(ProtectedDirectoryError::Unavailable)?;
        for byte in component {
            if !byte.is_ascii_digit() {
                return Err(ProtectedDirectoryError::Unavailable);
            }
            *number = number
                .checked_mul(10)
                .and_then(|number| number.checked_add(u32::from(*byte - b'0')))
                .ok_or(ProtectedDirectoryError::Unavailable)?;
        }
    }
    if version >= [7, 0] {
        Ok(BACKENDS.map(Some))
    } else {
        Ok([
            Some(Backend::UniqueIdQuery),
            Some(Backend::LegacyWitness),
            None,
        ])
    }
}

fn query_statmount_by_fd(
    file: &File,
    work: &mut WorkBudget,
    mut invoke: impl FnMut(&MountFdRequest, &mut AlignedBuffer<STATMOUNT_BYTES>, u32) -> io::Result<()>,
) -> Result<Record<STATMOUNT_BYTES>, ProtectedDirectoryError> {
    let request = MountFdRequest {
        size: 32,
        // A live File's nonnegative descriptor fits the union's unsigned word.
        mount_fd: file.as_raw_fd() as u32,
        mount_id: 0,
        parameter: STATMOUNT_MNT_BASIC,
        mount_namespace_id: 0,
    };
    let mut buffer = AlignedBuffer([0; STATMOUNT_BYTES]);
    work.retry(|| {
        buffer.0.fill(0);
        invoke(&request, &mut buffer, STATMOUNT_BY_FD)
    })?;
    Ok(Record {
        bytes: buffer.0,
        written: STATMOUNT_BYTES,
    })
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn statmount_by_fd_once(
    request: &MountFdRequest,
    buffer: &mut AlignedBuffer<STATMOUNT_BYTES>,
    flags: u32,
) -> io::Result<()> {
    // Linux v7.0 fs/namespace.c selects mnt_file->f_path.mnt with BY_FD, including
    // a detached mount. This supplies mount facts, not namespace or alias authority.
    // https://raw.githubusercontent.com/torvalds/linux/v7.0/fs/namespace.c
    // SAFETY: the caller retains File throughout the query. The initialized
    // version-one request and fixed 512-byte output are aligned. BY_FD matches
    // the descriptor union word. MNT_BASIC requests no variable-sized output.
    let result = unsafe {
        libc::syscall(
            SYS_STATMOUNT,
            request as *const MountFdRequest,
            buffer.0.as_mut_ptr(),
            buffer.0.len(),
            flags,
        )
    };
    syscall_result(result)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn statmount_by_fd_once(
    _request: &MountFdRequest,
    _buffer: &mut AlignedBuffer<STATMOUNT_BYTES>,
    _flags: u32,
) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOSYS))
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn query_statmount(
    request: &MountIdRequest,
    work: &mut WorkBudget,
) -> Result<Record<STATMOUNT_BYTES>, ProtectedDirectoryError> {
    let mut buffer = AlignedBuffer([0; STATMOUNT_BYTES]);
    work.retry(|| {
        buffer.0.fill(0);
        // SAFETY: request is the initialized 24-byte version-zero ABI. Output
        // storage is aligned and fixed at 512 bytes. Flags are zero. No variable
        // data, enlarged-buffer retry or new libc symbol is required.
        let result = unsafe {
            libc::syscall(
                SYS_STATMOUNT,
                request as *const MountIdRequest,
                buffer.0.as_mut_ptr(),
                buffer.0.len(),
                0_u32,
            )
        };
        syscall_result(result)
    })?;
    Ok(Record {
        bytes: buffer.0,
        written: STATMOUNT_BYTES,
    })
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn query_statmount(
    _request: &MountIdRequest,
    work: &mut WorkBudget,
) -> Result<Record<STATMOUNT_BYTES>, ProtectedDirectoryError> {
    work.retry(|| Err(io::Error::from_raw_os_error(libc::ENOSYS)))
}

fn syscall_result(result: libc::c_long) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else if result == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(libc::EIO))
    }
}

fn query_boot_identity<D>(
    lease: &mut Option<D>,
    work: &mut WorkBudget,
    mut open: impl FnMut() -> io::Result<D>,
    mut read: impl FnMut(&D, &mut [u8; 38]) -> io::Result<usize>,
) -> Result<BootIdentity, ProtectedDirectoryError> {
    if lease.is_none() {
        // The descriptor stays held through both samples and final metadata. Its
        // single close is prepaid before opening, including every failure path.
        work.reserve_cleanup()?;
        *lease = Some(work.retry(&mut open)?);
    }
    let descriptor = lease.as_ref().ok_or(ProtectedDirectoryError::Unavailable)?;
    // The one extra byte rejects oversize output without allocating or looping.
    let mut buffer = [0; 38];
    let count = work.retry(|| {
        buffer.fill(0);
        read(descriptor, &mut buffer)
    })?;
    decode_boot_identity(&buffer, count)
}

fn decode_boot_identity(
    buffer: &[u8; 38],
    count: usize,
) -> Result<BootIdentity, ProtectedDirectoryError> {
    if count != 37 || buffer[36] != b'\n' {
        return Err(ProtectedDirectoryError::Unavailable);
    }
    let mut bytes = [0; 16];
    let mut digit = 0;
    for (offset, byte) in buffer[..36].iter().copied().enumerate() {
        if [8, 13, 18, 23].contains(&offset) {
            if byte != b'-' {
                return Err(ProtectedDirectoryError::Unavailable);
            }
            continue;
        }
        let value = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return Err(ProtectedDirectoryError::Unavailable),
        };
        bytes[digit / 2] = (bytes[digit / 2] << 4) | value;
        digit += 1;
    }
    if bytes == [0; 16] {
        return Err(ProtectedDirectoryError::Unavailable);
    }
    Ok(BootIdentity(bytes))
}

#[cfg(test)]
mod boot_lease_tests {
    use std::cell::Cell;
    use std::io;

    use super::super::linux_evidence::WorkBudget;
    use super::query_boot_identity;
    use crate::protected_directory::ProtectedDirectoryError;

    const FIRST_BOOT: &[u8; 37] = b"01234567-89ab-cdef-0123-456789abcdef\n";
    const SECOND_BOOT: &[u8; 37] = b"11234567-89ab-cdef-0123-456789abcdef\n";

    struct Descriptor<'a> {
        attempts: &'a Cell<usize>,
        closes: &'a Cell<usize>,
    }

    impl Drop for Descriptor<'_> {
        fn drop(&mut self) {
            self.attempts.set(self.attempts.get() + 1);
            self.closes.set(self.closes.get() + 1);
        }
    }

    #[test]
    fn boot_samples_share_one_descriptor_but_reread_identity_and_close_once() {
        let mut work = WorkBudget::new();
        let mut lease = None;
        let attempts = Cell::new(0);
        let opens = Cell::new(0);
        let reads = Cell::new(0);
        let closes = Cell::new(0);
        let mut samples = Vec::new();
        for _ in 0..2 {
            samples.push(
                query_boot_identity(
                    &mut lease,
                    &mut work,
                    || {
                        attempts.set(attempts.get() + 1);
                        opens.set(opens.get() + 1);
                        Ok(Descriptor {
                            attempts: &attempts,
                            closes: &closes,
                        })
                    },
                    |descriptor, buffer| {
                        assert_eq!(descriptor.closes.get(), 0);
                        assert_eq!(*buffer, [0; 38]);
                        attempts.set(attempts.get() + 1);
                        reads.set(reads.get() + 1);
                        let record = if reads.get() == 1 {
                            FIRST_BOOT
                        } else {
                            SECOND_BOOT
                        };
                        buffer[..37].copy_from_slice(record);
                        Ok(37)
                    },
                )
                .map(|boot| boot.0),
            );
        }
        drop(lease);
        for _ in 0..28 {
            work.retry(|| {
                attempts.set(attempts.get() + 1);
                Ok(())
            })
            .unwrap();
        }
        let mut invoked = false;
        let exhausted = work.retry(|| {
            invoked = true;
            Ok(())
        });
        assert_eq!(
            (
                samples,
                opens.get(),
                reads.get(),
                closes.get(),
                attempts.get(),
                exhausted,
                invoked,
            ),
            (
                vec![
                    Ok([
                        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67,
                        0x89, 0xab, 0xcd, 0xef,
                    ]),
                    Ok([
                        0x11, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67,
                        0x89, 0xab, 0xcd, 0xef,
                    ]),
                ],
                1,
                2,
                1,
                32,
                Err(ProtectedDirectoryError::Unavailable),
                false,
            ),
        );
    }

    #[test]
    fn boot_close_reservation_bounds_opening_and_later_reads_without_attempt_33() {
        for (preceding, expected_first, expected_calls) in [
            (29, Ok(0x01), (1, 1, 1, 32)),
            (31, Err(ProtectedDirectoryError::Unavailable), (0, 0, 0, 31)),
        ] {
            let mut work = WorkBudget::new();
            let mut lease = None;
            let attempts = Cell::new(0);
            let opens = Cell::new(0);
            let reads = Cell::new(0);
            let closes = Cell::new(0);
            for _ in 0..preceding {
                work.retry(|| {
                    attempts.set(attempts.get() + 1);
                    Ok(())
                })
                .unwrap();
            }
            let mut samples = Vec::new();
            for _ in 0..2 {
                samples.push(
                    query_boot_identity(
                        &mut lease,
                        &mut work,
                        || {
                            attempts.set(attempts.get() + 1);
                            opens.set(opens.get() + 1);
                            Ok(Descriptor {
                                attempts: &attempts,
                                closes: &closes,
                            })
                        },
                        |_descriptor, buffer| {
                            attempts.set(attempts.get() + 1);
                            reads.set(reads.get() + 1);
                            buffer[..37].copy_from_slice(FIRST_BOOT);
                            Ok(37)
                        },
                    )
                    .map(|boot| boot.0[0]),
                );
            }
            drop(lease);
            assert_eq!(
                (
                    samples,
                    (opens.get(), reads.get(), closes.get(), attempts.get()),
                ),
                (
                    vec![expected_first, Err(ProtectedDirectoryError::Unavailable)],
                    expected_calls,
                ),
            );
        }
    }

    #[test]
    fn a_denied_boot_open_is_not_cached_as_an_opened_lease_or_missing_capability() {
        let mut work = WorkBudget::new();
        let mut lease = None;
        let attempts = Cell::new(0);
        let opens = Cell::new(0);
        let reads = Cell::new(0);
        let closes = Cell::new(0);
        let mut samples = Vec::new();
        for _ in 0..2 {
            samples.push(
                query_boot_identity(
                    &mut lease,
                    &mut work,
                    || {
                        attempts.set(attempts.get() + 1);
                        opens.set(opens.get() + 1);
                        if opens.get() == 1 {
                            Err(io::Error::from_raw_os_error(libc::EACCES))
                        } else {
                            Ok(Descriptor {
                                attempts: &attempts,
                                closes: &closes,
                            })
                        }
                    },
                    |_descriptor, buffer| {
                        attempts.set(attempts.get() + 1);
                        reads.set(reads.get() + 1);
                        buffer[..37].copy_from_slice(FIRST_BOOT);
                        Ok(37)
                    },
                )
                .map(|boot| boot.0[0]),
            );
        }
        drop(lease);
        assert_eq!(
            (
                samples,
                opens.get(),
                reads.get(),
                closes.get(),
                attempts.get(),
            ),
            (
                vec![Err(ProtectedDirectoryError::Unavailable), Ok(0x01)],
                2,
                1,
                1,
                4,
            ),
        );
    }

    #[test]
    fn denied_or_malformed_boot_samples_are_not_cached_as_identity_or_capability() {
        for denied in [true, false] {
            let mut work = WorkBudget::new();
            let mut lease = None;
            let opens = Cell::new(0);
            let reads = Cell::new(0);
            let attempts = Cell::new(0);
            let closes = Cell::new(0);
            let mut samples = Vec::new();
            for _ in 0..2 {
                samples.push(
                    query_boot_identity(
                        &mut lease,
                        &mut work,
                        || {
                            attempts.set(attempts.get() + 1);
                            opens.set(opens.get() + 1);
                            Ok(Descriptor {
                                attempts: &attempts,
                                closes: &closes,
                            })
                        },
                        |_descriptor, buffer| {
                            assert_eq!(*buffer, [0; 38]);
                            attempts.set(attempts.get() + 1);
                            reads.set(reads.get() + 1);
                            if reads.get() == 1 {
                                buffer.fill(0xff);
                                if denied {
                                    return Err(io::Error::from_raw_os_error(libc::EACCES));
                                }
                            } else {
                                buffer[..37].copy_from_slice(FIRST_BOOT);
                            }
                            Ok(37)
                        },
                    )
                    .map(|boot| boot.0[0]),
                );
            }
            drop(lease);
            assert_eq!(
                (
                    samples,
                    opens.get(),
                    reads.get(),
                    closes.get(),
                    attempts.get(),
                ),
                (
                    vec![Err(ProtectedDirectoryError::Unavailable), Ok(0x01)],
                    1,
                    2,
                    1,
                    4,
                ),
            );
        }
    }

    #[test]
    fn boot_samples_share_four_interruptions_and_clear_partial_output_before_retrying() {
        let mut work = WorkBudget::new();
        let mut lease = None;
        let attempts = Cell::new(0);
        let opens = Cell::new(0);
        let reads = Cell::new(0);
        let closes = Cell::new(0);
        let mut samples = Vec::new();
        for _ in 0..2 {
            samples.push(
                query_boot_identity(
                    &mut lease,
                    &mut work,
                    || {
                        attempts.set(attempts.get() + 1);
                        opens.set(opens.get() + 1);
                        Ok(Descriptor {
                            attempts: &attempts,
                            closes: &closes,
                        })
                    },
                    |_descriptor, buffer| {
                        assert_eq!(*buffer, [0; 38]);
                        attempts.set(attempts.get() + 1);
                        reads.set(reads.get() + 1);
                        if reads.get() == 5 {
                            buffer[..37].copy_from_slice(FIRST_BOOT);
                            Ok(37)
                        } else {
                            buffer.fill(0xff);
                            Err(io::Error::from_raw_os_error(libc::EINTR))
                        }
                    },
                )
                .map(|boot| boot.0[0]),
            );
        }
        drop(lease);
        assert_eq!(
            (
                samples,
                opens.get(),
                reads.get(),
                closes.get(),
                attempts.get(),
            ),
            (
                vec![Ok(0x01), Err(ProtectedDirectoryError::Unavailable)],
                1,
                6,
                1,
                8,
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io;
    use std::mem::{align_of, offset_of, size_of};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    use super::super::linux_evidence::{
        Backend, BasicMountVersion, DescriptorMountId, MountIdKind, Queries, WorkBudget,
    };
    use super::{
        KernelQueries, MountFdRequest, decode_boot_identity, query_statmount_by_fd,
        select_mapping_backends,
    };
    use crate::protected_directory::ProtectedDirectoryError;

    // Scripted syscall-boundary tests cover request and retry mechanics. They do
    // not observe kernel-7 support or provide namespace/publication authority.
    fn basic_mount_bytes() -> [u8; 512] {
        let mut bytes = [0; 512];
        bytes[..4].copy_from_slice(&512_u32.to_ne_bytes());
        bytes[8..16].copy_from_slice(&2_u64.to_ne_bytes());
        bytes[40..48].copy_from_slice(&0x1_0000_0025_u64.to_ne_bytes());
        bytes[56..60].copy_from_slice(&37_u32.to_ne_bytes());
        bytes
    }

    fn release_record(release: &[u8]) -> [u8; 65] {
        let mut record = [0; 65];
        record[..release.len()].copy_from_slice(release);
        record
    }

    #[test]
    fn backend_selection_skips_by_fd_on_pre_seven_kernel_release_hints() {
        for release in [b"5.15.0-249-generic".as_slice(), b"6.8.0", b"6.19.0-custom"] {
            let mut work = WorkBudget::new();
            let mut calls = 0;
            let selected = select_mapping_backends(&mut work, || {
                calls += 1;
                Ok(release_record(release))
            });
            assert_eq!(
                (selected, calls),
                (
                    Ok([
                        Some(Backend::UniqueIdQuery),
                        Some(Backend::LegacyWitness),
                        None
                    ]),
                    1
                ),
            );
        }
    }

    #[test]
    fn backend_selection_prefers_by_fd_from_linux_seven_release_hints() {
        for release in [b"7.0.0-rc1".as_slice(), b"7.1.0", b"8.0.0-custom"] {
            let mut work = WorkBudget::new();
            let mut calls = 0;
            let selected = select_mapping_backends(&mut work, || {
                calls += 1;
                Ok(release_record(release))
            });
            assert_eq!(
                (selected, calls),
                (
                    Ok([
                        Some(Backend::DescriptorQuery),
                        Some(Backend::UniqueIdQuery),
                        Some(Backend::LegacyWitness)
                    ]),
                    1
                ),
            );
        }
    }

    #[test]
    fn malformed_release_hints_cannot_select_native_queries() {
        for release in [
            release_record(b""),
            release_record(b"7"),
            release_record(b"7."),
            release_record(b"+7.0.0"),
            release_record(b"7.x.0"),
            release_record(b"4294967296.0.0"),
            release_record(b"7.4294967296.0"),
            [b'7'; 65],
        ] {
            let mut work = WorkBudget::new();
            assert_eq!(
                select_mapping_backends(&mut work, || Ok(release)),
                Err(ProtectedDirectoryError::Unavailable),
            );
        }
    }

    #[test]
    fn denied_or_failed_release_queries_cannot_select_native_queries() {
        for errno in [libc::EINVAL, libc::EACCES, libc::EPERM, libc::EIO] {
            let mut work = WorkBudget::new();
            let mut calls = 0;
            let selected = select_mapping_backends(&mut work, || {
                calls += 1;
                Err(io::Error::from_raw_os_error(errno))
            });
            assert_eq!(
                (selected, calls),
                (Err(ProtectedDirectoryError::Unavailable), 1)
            );
        }
    }

    #[test]
    fn backend_selection_cannot_invoke_a_thirty_third_native_attempt() {
        let mut work = WorkBudget::new();
        for _ in 0..31 {
            work.retry(|| Ok(())).unwrap();
        }
        let mut calls = 0;
        let selected = select_mapping_backends(&mut work, || {
            calls += 1;
            Err(io::Error::from_raw_os_error(libc::EINTR))
        });
        assert_eq!(
            (selected, calls),
            (Err(ProtectedDirectoryError::Unavailable), 1)
        );
    }

    #[test]
    fn native_backend_selection_charges_its_release_query_to_the_call_bound() {
        let mut work = WorkBudget::new();
        for _ in 0..31 {
            work.retry(|| Ok(())).unwrap();
        }
        // A read-only uname observation, not proof of statmount or protection.
        let selected = KernelQueries.select_backends(&mut work);
        assert!(
            selected.is_ok(),
            "read-only kernel release observation failed: {selected:?}"
        );
        let mut invoked = false;
        let exhausted = work.retry(|| {
            invoked = true;
            Ok(())
        });
        assert_eq!(
            (exhausted, invoked),
            (Err(ProtectedDirectoryError::Unavailable), false)
        );
    }

    #[test]
    fn backend_selection_and_mapping_queries_share_the_four_interruption_bound() {
        let mut work = WorkBudget::new();
        let mut calls = 0;
        let selected = select_mapping_backends(&mut work, || {
            calls += 1;
            if calls <= 4 {
                Err(io::Error::from_raw_os_error(libc::EINTR))
            } else {
                Ok(release_record(b"7.0.0"))
            }
        });
        assert!(selected.is_ok());
        let file = File::open("/").expect("read-only fd query fixture opening failed");
        let mut query_calls = 0;
        let query = query_statmount_by_fd(&file, &mut work, |_request, _buffer, _flags| {
            query_calls += 1;
            Err(io::Error::from_raw_os_error(libc::EINTR))
        });
        assert_eq!(
            (query.err(), calls, query_calls),
            (Some(ProtectedDirectoryError::Unavailable), 5, 1),
        );
    }

    #[test]
    fn fd_statmount_request_matches_the_version_one_union_layout() {
        assert_eq!(
            (
                size_of::<MountFdRequest>(),
                align_of::<MountFdRequest>(),
                offset_of!(MountFdRequest, size),
                offset_of!(MountFdRequest, mount_fd),
                offset_of!(MountFdRequest, mount_id),
                offset_of!(MountFdRequest, parameter),
                offset_of!(MountFdRequest, mount_namespace_id),
            ),
            (32, 8, 0, 4, 8, 16, 24),
        );
    }

    #[test]
    fn both_fd_query_rounds_select_the_exact_held_descriptor_with_basic_only_flags() {
        let file = File::open("/usr/bin/true").expect("read-only fd query fixture opening failed");
        let descriptor = u32::try_from(file.as_raw_fd()).unwrap();
        let mut work = WorkBudget::new();
        let mut calls = 0;
        for _round in 0..2 {
            let record = query_statmount_by_fd(&file, &mut work, |request, buffer, flags| {
                assert_eq!(
                    (
                        request.size,
                        request.mount_fd,
                        request.mount_id,
                        request.parameter,
                        request.mount_namespace_id,
                        flags,
                    ),
                    (32, descriptor, 0, 2, 0, 1),
                );
                assert_eq!((buffer.0.len(), buffer.0.as_ptr() as usize % 8), (512, 0));
                assert_eq!(buffer.0, [0; 512]);
                buffer.0 = basic_mount_bytes();
                calls += 1;
                Ok(())
            })
            .expect("scripted fd query failed");
            assert_eq!(
                BasicMountVersion::decode(&record, None),
                Ok(BasicMountVersion {
                    unique_id: 0x1_0000_0025,
                    parent_unique_id: 0,
                    legacy_id: 37,
                    parent_legacy_id: 0,
                    attributes: 0,
                    propagation: 0,
                    peer_group: 0,
                    master: 0,
                    propagate_from: 0,
                }),
            );
        }
        assert_eq!(calls, 2);
    }

    #[test]
    fn interrupted_fd_queries_clear_partial_output_before_retrying() {
        let file = File::open("/").expect("read-only fd query fixture opening failed");
        let mut work = WorkBudget::new();
        let mut calls = 0;
        let record = query_statmount_by_fd(&file, &mut work, |_request, buffer, _flags| {
            assert_eq!(buffer.0, [0; 512]);
            calls += 1;
            if calls == 1 {
                buffer.0.fill(0xff);
                Err(io::Error::from_raw_os_error(libc::EINTR))
            } else {
                buffer.0 = basic_mount_bytes();
                Ok(())
            }
        })
        .expect("scripted interrupted fd query failed");
        assert_eq!(record.bytes, basic_mount_bytes());
        assert_eq!(record.written, 512);
        assert_eq!(calls, 2);
    }

    #[test]
    fn failed_fd_queries_never_return_written_evidence_or_grow_the_buffer() {
        let file = File::open("/").expect("read-only fd query fixture opening failed");
        for (errno, expected) in [
            (libc::ENOSYS, ProtectedDirectoryError::NativeUnavailable),
            (libc::EOPNOTSUPP, ProtectedDirectoryError::NativeUnavailable),
            (libc::EINVAL, ProtectedDirectoryError::Unavailable),
            (libc::EBADF, ProtectedDirectoryError::Unavailable),
            (libc::EFAULT, ProtectedDirectoryError::Unavailable),
            (libc::EPERM, ProtectedDirectoryError::Unavailable),
            (libc::EACCES, ProtectedDirectoryError::Unavailable),
            (libc::EIO, ProtectedDirectoryError::Unavailable),
            (libc::EOVERFLOW, ProtectedDirectoryError::Unavailable),
        ] {
            let mut work = WorkBudget::new();
            let mut calls = 0;
            let result = query_statmount_by_fd(&file, &mut work, |_request, buffer, _flags| {
                assert_eq!(buffer.0.len(), 512);
                buffer.0 = basic_mount_bytes();
                calls += 1;
                Err(io::Error::from_raw_os_error(errno))
            });
            assert_eq!(result.err(), Some(expected), "errno {errno}");
            assert_eq!(calls, 1, "errno {errno}");
        }
    }

    #[test]
    fn fd_query_rounds_share_the_existing_four_interruption_budget() {
        let file = File::open("/").expect("read-only fd query fixture opening failed");
        let mut work = WorkBudget::new();
        let mut calls = 0;
        let first = query_statmount_by_fd(&file, &mut work, |_request, buffer, _flags| {
            calls += 1;
            if calls <= 4 {
                Err(io::Error::from_raw_os_error(libc::EINTR))
            } else {
                buffer.0 = basic_mount_bytes();
                Ok(())
            }
        });
        assert!(first.is_ok());
        let second = query_statmount_by_fd(&file, &mut work, |_request, _buffer, _flags| {
            calls += 1;
            Err(io::Error::from_raw_os_error(libc::EINTR))
        });
        assert_eq!(second.err(), Some(ProtectedDirectoryError::Unavailable));
        assert_eq!(calls, 6);
    }

    #[test]
    fn fd_query_retries_cannot_invoke_a_thirty_third_native_attempt() {
        let file = File::open("/").expect("read-only fd query fixture opening failed");
        let mut work = WorkBudget::new();
        for _ in 0..31 {
            work.retry(|| Ok(())).unwrap();
        }
        let mut calls = 0;
        let result = query_statmount_by_fd(&file, &mut work, |_request, _buffer, _flags| {
            calls += 1;
            Err(io::Error::from_raw_os_error(libc::EINTR))
        });
        assert_eq!(result.err(), Some(ProtectedDirectoryError::Unavailable));
        assert_eq!(calls, 1);
    }

    // Read-only syscall observations, not an issuer, alias fixture or approval of
    // these system files. Linux 5.15 already supports the requested legacy ID.
    fn observe_descriptor_statx(path: &str) {
        let file = File::open(path).expect("read-only statx observation handle opening failed");
        let mut work = WorkBudget::new();
        let record = KernelQueries
            .statx(&file, MountIdKind::Legacy, 0, &mut work)
            .expect("read-only descriptor statx observation failed");
        assert!(DescriptorMountId::decode(&record, MountIdKind::Legacy).is_ok());
        let mask = u32::from_ne_bytes(record.bytes[..4].try_into().unwrap());
        assert_ne!(
            mask & 0x100,
            0,
            "the native statx observation omitted inode identity"
        );
        let inode = u64::from_ne_bytes(record.bytes[32..40].try_into().unwrap());
        assert_eq!(
            inode,
            file.metadata()
                .expect("statx observation metadata failed")
                .ino()
        );
    }

    #[test]
    fn raw_statx_observes_the_held_directory_descriptor() {
        observe_descriptor_statx("/");
    }

    #[test]
    fn raw_statx_observes_the_held_regular_file_descriptor() {
        observe_descriptor_statx("/usr/bin/true");
    }

    #[test]
    fn boot_invalidation_requires_one_complete_bounded_uuid_record() {
        let mut record = [0; 38];
        record[..37].copy_from_slice(b"01234567-89ab-cdef-0123-456789abcdef\n");
        assert!(decode_boot_identity(&record, 37).is_ok());
        for length in [0, 36, 38, usize::MAX] {
            assert_eq!(
                decode_boot_identity(&record, length).err(),
                Some(ProtectedDirectoryError::Unavailable)
            );
        }
        for offset in [8, 13, 18, 23, 36] {
            let mut malformed = record;
            malformed[offset] = b'0';
            assert_eq!(
                decode_boot_identity(&malformed, 37).err(),
                Some(ProtectedDirectoryError::Unavailable)
            );
        }
        record[..37].copy_from_slice(b"00000000-0000-0000-0000-000000000000\n");
        assert_eq!(
            decode_boot_identity(&record, 37).err(),
            Some(ProtectedDirectoryError::Unavailable)
        );
    }
}
