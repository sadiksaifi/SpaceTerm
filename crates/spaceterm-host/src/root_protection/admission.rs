use crate::protected_directory::ProtectedDirectoryError;

pub(super) fn require_root_mode(owner: u32, mode: u32) -> Result<(), ProtectedDirectoryError> {
    if owner != 0 || mode & 0o022 != 0 {
        Err(ProtectedDirectoryError::Unsafe)
    } else {
        Ok(())
    }
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn require_linux_filesystem(
    filesystem_type: u64,
) -> Result<(), ProtectedDirectoryError> {
    // Linux v5.15 fs/{ext4,xfs,btrfs}/acl.c and mm/shmem.c use POSIX ACLs.
    // fs/posix_acl.c maps ACL_MASK to group mode and masks every nonowner
    // named-user/group grant. The unmasked owner entry requires separate host
    // ownership and alias-publication proofs; projected UID zero is insufficient.
    // https://github.com/torvalds/linux/blob/v5.15/fs/posix_acl.c
    match filesystem_type {
        0xEF53 | 0x5846_5342 | 0x9123_683E | 0x0102_1994 => Ok(()),
        _ => Err(ProtectedDirectoryError::Unsafe),
    }
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn require_macos_filesystem(
    filesystem_name: &[u8],
    mount_flags: u32,
) -> Result<(), ProtectedDirectoryError> {
    // Public sys/mount.h: MNT_LOCAL and MNT_IGNORE_OWNERSHIP.
    const LOCAL: u32 = 0x1000;
    const IGNORE_OWNERSHIP: u32 = 0x0020_0000;
    if !matches!(filesystem_name, b"apfs" | b"hfs")
        || mount_flags & LOCAL == 0
        || mount_flags & IGNORE_OWNERSHIP != 0
    {
        Err(ProtectedDirectoryError::Unsafe)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{require_linux_filesystem, require_macos_filesystem, require_root_mode};
    use crate::protected_directory::ProtectedDirectoryError;

    #[test]
    fn root_owned_nonwritable_modes_are_admitted() {
        for mode in [0o700, 0o755, 0o600, 0o644, 0o040755, 0o100755] {
            assert_eq!(require_root_mode(0, mode), Ok(()), "mode {mode:o}");
        }
    }

    #[test]
    fn every_nonzero_owner_is_rejected() {
        for owner in [1, 501, u32::MAX] {
            assert_eq!(
                require_root_mode(owner, 0o755),
                Err(ProtectedDirectoryError::Unsafe),
                "owner {owner}"
            );
        }
    }

    #[test]
    fn group_or_other_write_is_rejected_even_with_a_sticky_bit() {
        for mode in [0o720, 0o702, 0o777, 0o1777, 0o664, 0o606] {
            assert_eq!(
                require_root_mode(0, mode),
                Err(ProtectedDirectoryError::Unsafe),
                "mode {mode:o}"
            );
        }
    }

    #[test]
    fn linux_proven_posix_filesystems_are_admitted() {
        // EXT4_SUPER_MAGIC, XFS_SUPER_MAGIC, BTRFS_SUPER_MAGIC, TMPFS_MAGIC.
        for filesystem_type in [0xEF53, 0x5846_5342, 0x9123_683E, 0x0102_1994] {
            assert_eq!(require_linux_filesystem(filesystem_type), Ok(()));
        }
    }

    #[test]
    fn linux_unproven_filesystem_semantics_are_rejected() {
        for filesystem_type in [0, 0xFEED, u64::MAX] {
            assert_eq!(
                require_linux_filesystem(filesystem_type),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn macos_local_apfs_and_hfs_with_enforced_ownership_are_admitted() {
        // sys/mount.h: MNT_LOCAL = 0x1000; MNT_RDONLY = 1.
        for name in [b"apfs".as_slice(), b"hfs".as_slice()] {
            for flags in [0x1000, 0x1001] {
                assert_eq!(require_macos_filesystem(name, flags), Ok(()));
            }
        }
    }

    #[test]
    fn macos_unproven_filesystem_semantics_are_rejected() {
        for name in [b"nfs".as_slice(), b"smbfs", b"msdos", b"", b"APFS"] {
            assert_eq!(
                require_macos_filesystem(name, 0x1000),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn macos_nonlocal_mounts_are_rejected_even_when_readonly() {
        for name in [b"apfs".as_slice(), b"hfs".as_slice()] {
            for flags in [0, 1] {
                assert_eq!(
                    require_macos_filesystem(name, flags),
                    Err(ProtectedDirectoryError::Unsafe)
                );
            }
        }
    }

    #[test]
    fn macos_ownership_disabled_mounts_are_rejected_even_when_readonly() {
        // sys/mount.h: MNT_IGNORE_OWNERSHIP = 0x00200000.
        for name in [b"apfs".as_slice(), b"hfs".as_slice()] {
            for flags in [0x0020_1000, 0x0020_1001] {
                assert_eq!(
                    require_macos_filesystem(name, flags),
                    Err(ProtectedDirectoryError::Unsafe)
                );
            }
        }
    }
}
