#[cfg(target_os = "macos")]
use std::fs::File;
#[cfg(target_os = "macos")]
use std::mem::MaybeUninit;
#[cfg(target_os = "macos")]
use std::os::fd::AsRawFd;

#[cfg(target_os = "macos")]
use super::admission::require_macos_filesystem;
use crate::protected_directory::ProtectedDirectoryError;

// Public sys/attr.h and sys/kauth.h layouts. Decode bytes rather than casting
// pointers: native attrreference offsets are relative to the reference itself.
const BUFFER_BYTES: usize = 4096;
const RETURNED_ATTRS: u32 = 0x8000_0000;
const EXTENDED_SECURITY: u32 = 0x0040_0000;
const VOLUME_CAPABILITIES: u32 = 0x0002_0000;
const ACL_CAPABILITY: u32 = 0x400;
const CAPABILITY_BYTES: usize = 56;
const REFERENCE_OFFSET: usize = 24;
const ACL_FIXED_BYTES: usize = 32;
const FILESEC_HEADER_BYTES: usize = 44;
const ENTRY_BYTES: usize = 24;
const MAX_ENTRIES: u32 = 128;
const FILESEC_MAGIC: u32 = 0x012C_C16D;
const NO_ACL: u32 = u32::MAX;
const ACL_FLAGS: u32 = (1 << 16) | (1 << 17);
const ENTRY_KIND_MASK: u32 = 0xF;
const ENTRY_INHERITANCE_FLAGS: u32 = 0x1F0;
const ALLOW: u32 = 1;
const DENY: u32 = 2;
const READ_ONLY: u32 = 0x0010_0A8A;
const MUTATION: u32 = 0x3574;
const GENERIC_ALL: u32 = 1 << 21;
const GENERIC_EXECUTE: u32 = 1 << 22;
const GENERIC_WRITE: u32 = 1 << 23;
const GENERIC_READ: u32 = 1 << 24;
const KNOWN_RIGHTS: u32 =
    READ_ONLY | MUTATION | GENERIC_ALL | GENERIC_EXECUTE | GENERIC_WRITE | GENERIC_READ;
const MUTATION_RIGHTS: u32 = MUTATION | GENERIC_ALL | GENERIC_WRITE;

/// Private witness obtained before reusing the bounded buffer for ACL inspection.
#[derive(Debug)]
pub(super) struct AclCapability {
    capabilities: [u32; 8],
}

pub(super) fn require_acl_capability(
    record: Result<&[u8], ProtectedDirectoryError>,
) -> Result<AclCapability, ProtectedDirectoryError> {
    let capabilities = decode_capabilities(record)?;
    // vol_capabilities_attr_t: capabilities[4], valid[4]; interfaces index 1.
    if capabilities[1] & ACL_CAPABILITY == 0 || capabilities[5] & ACL_CAPABILITY == 0 {
        return Err(ProtectedDirectoryError::Unsafe);
    }
    Ok(AclCapability { capabilities })
}

fn decode_capabilities(
    record: Result<&[u8], ProtectedDirectoryError>,
) -> Result<[u32; 8], ProtectedDirectoryError> {
    let record = bounded_record(record, CAPABILITY_BYTES)?;
    if record.len() != CAPABILITY_BYTES {
        return Err(ProtectedDirectoryError::Unsafe);
    }
    // ATTR_VOL_INFO selects volume inspection but has no packed data or returned
    // bit. XNU getvolattrlist packs only ATTR_VOL_CAPABILITIES in this request.
    require_returned_attributes(record, RETURNED_ATTRS, VOLUME_CAPABILITIES)?;
    let mut capabilities = [0; 8];
    for (index, value) in capabilities.iter_mut().enumerate() {
        *value = word(record, 24 + 4 * index)?;
    }
    Ok(capabilities)
}

pub(super) fn require_extended_security(
    record: Result<&[u8], ProtectedDirectoryError>,
    capability: AclCapability,
) -> Result<(), ProtectedDirectoryError> {
    // Consume the independent witness. It cannot be obtained from ACL absence.
    let _ = capability.capabilities;
    let record = bounded_record(record, ACL_FIXED_BYTES)?;
    let relative_offset = i32::from_ne_bytes(word(record, REFERENCE_OFFSET)?.to_ne_bytes());
    let relative_offset =
        usize::try_from(relative_offset).map_err(|_| ProtectedDirectoryError::Unsafe)?;
    let start = REFERENCE_OFFSET
        .checked_add(relative_offset)
        .ok_or(ProtectedDirectoryError::Unsafe)?;
    let length = usize::try_from(word(record, REFERENCE_OFFSET + 4)?)
        .map_err(|_| ProtectedDirectoryError::Unsafe)?;
    let end = start
        .checked_add(length)
        .ok_or(ProtectedDirectoryError::Unsafe)?;
    // There is exactly one variable attribute and no interstitial padding in
    // this request. Reject overlaps, gaps and unreported trailing payload.
    if start != ACL_FIXED_BYTES || end != record.len() {
        return Err(ProtectedDirectoryError::Unsafe);
    }
    if length == 0 {
        // XNU attr_pack_common packs a zero-length reference without the
        // EXTENDED_SECURITY returned bit when va_acl is NULL. A successful call
        // is absence only after the separate volume capability proof.
        return require_returned_attributes(record, RETURNED_ATTRS, 0);
    }
    require_returned_attributes(record, RETURNED_ATTRS | EXTENDED_SECURITY, 0)?;
    let payload = record
        .get(start..end)
        .ok_or(ProtectedDirectoryError::Unsafe)?;
    if payload.len() < FILESEC_HEADER_BYTES || word(payload, 0)? != FILESEC_MAGIC {
        return Err(ProtectedDirectoryError::Unsafe);
    }
    let count = word(payload, 36)?;
    if word(payload, 40)? & !ACL_FLAGS != 0 {
        return Err(ProtectedDirectoryError::Unsafe);
    }
    // Bound the count before multiplication or enumeration. NOACL is a header,
    // not a count to multiply or a reason to ignore unknown ACL flags.
    let count = if count == NO_ACL {
        0
    } else if count <= MAX_ENTRIES {
        count
    } else {
        return Err(ProtectedDirectoryError::Unsafe);
    };
    let count = usize::try_from(count).map_err(|_| ProtectedDirectoryError::Unsafe)?;
    let expected_length = count
        .checked_mul(ENTRY_BYTES)
        .and_then(|bytes| bytes.checked_add(FILESEC_HEADER_BYTES))
        .ok_or(ProtectedDirectoryError::Unsafe)?;
    if payload.len() != expected_length {
        return Err(ProtectedDirectoryError::Unsafe);
    }
    for entry in payload[FILESEC_HEADER_BYTES..].as_chunks::<ENTRY_BYTES>().0 {
        // Principal GUIDs are neither resolved nor retained. Even root-qualified
        // and inherit-only mutation ALLOWs are rejected, irrespective of order.
        let flags = word(entry, 16)?;
        let kind = flags & ENTRY_KIND_MASK;
        let rights = word(entry, 20)?;
        if flags & !(ENTRY_KIND_MASK | ENTRY_INHERITANCE_FLAGS) != 0
            || !matches!(kind, ALLOW | DENY)
            || rights & !KNOWN_RIGHTS != 0
            || (kind == ALLOW && rights & MUTATION_RIGHTS != 0)
        {
            return Err(ProtectedDirectoryError::Unsafe);
        }
    }
    Ok(())
}

fn bounded_record(
    record: Result<&[u8], ProtectedDirectoryError>,
    minimum_bytes: usize,
) -> Result<&[u8], ProtectedDirectoryError> {
    let record = record?;
    if record.len() > BUFFER_BYTES {
        return Err(ProtectedDirectoryError::Unsafe);
    }
    let length = usize::try_from(word(record, 0)?).map_err(|_| ProtectedDirectoryError::Unsafe)?;
    if length < minimum_bytes || length > BUFFER_BYTES {
        return Err(ProtectedDirectoryError::Unsafe);
    }
    record.get(..length).ok_or(ProtectedDirectoryError::Unsafe)
}

fn require_returned_attributes(
    record: &[u8],
    common: u32,
    volume: u32,
) -> Result<(), ProtectedDirectoryError> {
    for (index, expected) in [common, volume, 0, 0, 0].into_iter().enumerate() {
        if word(record, 4 + 4 * index)? != expected {
            return Err(ProtectedDirectoryError::Unsafe);
        }
    }
    Ok(())
}

fn word(record: &[u8], offset: usize) -> Result<u32, ProtectedDirectoryError> {
    let end = offset
        .checked_add(4)
        .ok_or(ProtectedDirectoryError::Unsafe)?;
    let bytes = record
        .get(offset..end)
        .ok_or(ProtectedDirectoryError::Unsafe)?;
    let bytes = <[u8; 4]>::try_from(bytes).map_err(|_| ProtectedDirectoryError::Unsafe)?;
    Ok(u32::from_ne_bytes(bytes))
}

#[cfg(target_os = "macos")]
#[derive(Eq, PartialEq)]
pub(super) struct FilesystemVersion {
    filesystem_name: [u8; 16],
    filesystem_type: u32,
    subtype: u32,
    mount_owner: u32,
    mount_flags: u32,
    extended_flags: u32,
}

#[cfg(target_os = "macos")]
impl FilesystemVersion {
    pub(super) fn require_proven_semantics(&self) -> Result<(), ProtectedDirectoryError> {
        // Public sys/mount.h: MNT_EXT_FSKIT is not a native APFS/HFS proof.
        if self.extended_flags & 2 != 0 {
            return Err(ProtectedDirectoryError::Unsafe);
        }
        let end = self
            .filesystem_name
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(ProtectedDirectoryError::Unsafe)?;
        require_macos_filesystem(&self.filesystem_name[..end], self.mount_flags)
    }
}

#[cfg(target_os = "macos")]
pub(super) fn filesystem_version(
    file: &File,
) -> Result<FilesystemVersion, ProtectedDirectoryError> {
    let mut filesystem = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: file owns a live borrowed descriptor and filesystem is writable
    // native storage. No descriptor ownership is transferred by fstatfs.
    if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(ProtectedDirectoryError::Unavailable);
    }
    // SAFETY: fstatfs initialized the output on success.
    let filesystem = unsafe { filesystem.assume_init() };
    Ok(FilesystemVersion {
        filesystem_name: filesystem.f_fstypename.map(|byte| byte as u8),
        filesystem_type: filesystem.f_type,
        subtype: filesystem.f_fssubtype,
        mount_owner: filesystem.f_owner,
        mount_flags: filesystem.f_flags,
        extended_flags: filesystem.f_flags_ext,
    })
}

#[cfg(target_os = "macos")]
pub(super) fn inspect_acl(file: &File) -> Result<(), ProtectedDirectoryError> {
    let mut buffer = [0; BUFFER_BYTES];
    let capability = require_acl_capability(query_attributes(
        file,
        RETURNED_ATTRS,
        libc::ATTR_VOL_INFO | VOLUME_CAPABILITIES,
        &mut buffer,
    ))?;
    let before = capability.capabilities;
    let inspection = require_extended_security(
        query_attributes(file, RETURNED_ATTRS | EXTENDED_SECURITY, 0, &mut buffer),
        capability,
    );
    let after = decode_capabilities(query_attributes(
        file,
        RETURNED_ATTRS,
        libc::ATTR_VOL_INFO | VOLUME_CAPABILITIES,
        &mut buffer,
    ))?;
    if before != after {
        return Err(ProtectedDirectoryError::Changed);
    }
    inspection
}

#[cfg(target_os = "macos")]
fn query_attributes<'buffer>(
    file: &File,
    common: u32,
    volume: u32,
    buffer: &'buffer mut [u8; BUFFER_BYTES],
) -> Result<&'buffer [u8], ProtectedDirectoryError> {
    // XNU fgetattrlist -> getattrlist_internal -> getvolattrlist selects the
    // descriptor's mount and substitutes its root vnode internally. Volume
    // capability requests work on a file or non-root directory descriptor.
    // https://github.com/apple-oss-distributions/xnu/blob/main/bsd/vfs/vfs_attrlist.c
    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: common,
        volattr: volume,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    buffer.fill(0);
    // SAFETY: the File stays borrowed. Both pointers refer to live writable
    // storage of the declared sizes. No native ACL heap object is allocated here.
    let result = unsafe {
        libc::fgetattrlist(
            file.as_raw_fd(),
            (&mut attributes as *mut libc::attrlist).cast(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            libc::FSOPT_REPORT_FULLSIZE | libc::FSOPT_PACK_INVAL_ATTRS,
        )
    };
    if result != 0 {
        // In particular, no failed getter or ENOENT is treated as ACL absence.
        Err(ProtectedDirectoryError::Unavailable)
    } else {
        Ok(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::{AclCapability, require_acl_capability, require_extended_security};
    use crate::protected_directory::ProtectedDirectoryError;

    // Layouts and constants are from SDK sys/attr.h, sys/kauth.h and _guid_t.h.
    const BUFFER_BYTES: usize = 4096;
    const RETURNED_ATTRS: u32 = 0x8000_0000;
    const EXTENDED_SECURITY: u32 = 0x0040_0000;
    const VOLUME_CAPABILITIES: u32 = 0x0002_0000;
    const ACL_CAPABILITY: u32 = 0x400;
    const FILESEC_MAGIC: u32 = 0x012C_C16D;
    const NO_ACL: u32 = u32::MAX;
    const ALLOW: u32 = 1;
    const DENY: u32 = 2;
    const READ_ONLY: u32 = 0x0010_0A8A;
    const MUTATION: u32 = 0x3574;
    const GENERIC_ALL: u32 = 1 << 21;
    const GENERIC_EXECUTE: u32 = 1 << 22;
    const GENERIC_WRITE: u32 = 1 << 23;
    const GENERIC_READ: u32 = 1 << 24;
    const INHERITANCE_FLAGS: u32 = 0x1F0;
    const OWNER_GUID: [u8; 16] = [0x11; 16];
    const OTHER_GUID: [u8; 16] = [0x22; 16];

    type Entry = ([u8; 16], u32, u32);

    struct Record {
        bytes: [u8; BUFFER_BYTES],
        length: usize,
    }

    fn put_word(record: &mut Record, offset: usize, value: u32) {
        record.bytes[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
    }

    fn capabilities_record(valid_interfaces: u32, enabled_interfaces: u32) -> Record {
        let mut record = Record {
            bytes: [0; BUFFER_BYTES],
            length: 56,
        };
        put_word(&mut record, 0, 56);
        put_word(&mut record, 4, RETURNED_ATTRS);
        put_word(&mut record, 8, VOLUME_CAPABILITIES);
        // vol_capabilities_attr_t: capabilities[4], valid[4]; interfaces index 1.
        put_word(&mut record, 28, enabled_interfaces);
        put_word(&mut record, 44, valid_interfaces);
        record
    }

    fn acl_record(count: u32, flags: u32, entries: &[Entry]) -> Record {
        let payload_length = 44 + 24 * entries.len();
        let length = 32 + payload_length;
        assert!(
            length <= BUFFER_BYTES,
            "synthetic record exceeds fixture capacity"
        );
        let mut record = Record {
            bytes: [0; BUFFER_BYTES],
            length,
        };
        put_word(&mut record, 0, u32::try_from(length).unwrap());
        put_word(&mut record, 4, RETURNED_ATTRS | EXTENDED_SECURITY);
        // attrreference_t starts at 24; its offset is relative to that reference.
        put_word(&mut record, 24, 8);
        put_word(&mut record, 28, u32::try_from(payload_length).unwrap());
        put_word(&mut record, 32, FILESEC_MAGIC);
        record.bytes[36..52].copy_from_slice(&OWNER_GUID);
        put_word(&mut record, 68, count);
        put_word(&mut record, 72, flags);
        for (index, (principal, entry_flags, rights)) in entries.iter().enumerate() {
            let offset = 76 + 24 * index;
            record.bytes[offset..offset + 16].copy_from_slice(principal);
            put_word(&mut record, offset + 16, *entry_flags);
            put_word(&mut record, offset + 20, *rights);
        }
        record
    }

    fn absent_acl_record() -> Record {
        let mut record = Record {
            bytes: [0; BUFFER_BYTES],
            length: 32,
        };
        put_word(&mut record, 0, 32);
        put_word(&mut record, 4, RETURNED_ATTRS);
        put_word(&mut record, 24, 8);
        // XNU packs a zero-length reference without the EXTENDED_SECURITY bit
        // for an absent ACL under FSOPT_PACK_INVAL_ATTRS. Capability is separate.
        record
    }

    fn synthetic_capability() -> AclCapability {
        // Isolates pure ACL decoding. This is not native filesystem authority.
        AclCapability {
            capabilities: [0, ACL_CAPABILITY, 0, 0, 0, ACL_CAPABILITY, 0, 0],
        }
    }

    #[test]
    fn valid_enabled_interface_acl_capability_is_admitted() {
        // An unrelated known COPYFILE capability does not weaken the ACL proof.
        for interfaces in [ACL_CAPABILITY, ACL_CAPABILITY | 0x20] {
            let record = capabilities_record(interfaces, interfaces);
            let result = require_acl_capability(Ok(&record.bytes));
            assert!(result.is_ok(), "capability admission returned {result:?}");
        }
    }

    #[test]
    fn missing_unknown_or_disabled_acl_capability_is_rejected() {
        for (valid, enabled) in [(0, 0), (0, ACL_CAPABILITY), (ACL_CAPABILITY, 0), (1, 1)] {
            let record = capabilities_record(valid, enabled);
            assert_eq!(
                require_acl_capability(Ok(&record.bytes)).err(),
                Some(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn format_capability_bits_cannot_substitute_for_interface_acl_capability() {
        let mut record = capabilities_record(0, 0);
        put_word(&mut record, 24, ACL_CAPABILITY);
        put_word(&mut record, 40, ACL_CAPABILITY);
        assert_eq!(
            require_acl_capability(Ok(&record.bytes)).err(),
            Some(ProtectedDirectoryError::Unsafe)
        );
    }

    #[test]
    fn missing_or_unrequested_capability_attributes_are_rejected() {
        for (offset, value) in [
            (4, 0),
            (8, 0),
            (4, RETURNED_ATTRS | EXTENDED_SECURITY),
            (12, 1),
        ] {
            let mut record = capabilities_record(ACL_CAPABILITY, ACL_CAPABILITY);
            put_word(&mut record, offset, value);
            assert_eq!(
                require_acl_capability(Ok(&record.bytes)).err(),
                Some(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn truncated_capability_records_are_rejected() {
        let record = capabilities_record(ACL_CAPABILITY, ACL_CAPABILITY);
        for length in [0, 3, 23, record.length - 1] {
            assert_eq!(
                require_acl_capability(Ok(&record.bytes[..length])).err(),
                Some(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn invalid_reported_capability_lengths_are_rejected() {
        for length in [0, 24, 55, 57, 4097, u32::MAX] {
            let mut record = capabilities_record(ACL_CAPABILITY, ACL_CAPABILITY);
            put_word(&mut record, 0, length);
            assert_eq!(
                require_acl_capability(Ok(&record.bytes)).err(),
                Some(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn failed_capability_inspection_cannot_create_an_acl_capability_witness() {
        assert_eq!(
            require_acl_capability(Err(ProtectedDirectoryError::Unavailable)).err(),
            Some(ProtectedDirectoryError::Unavailable)
        );
    }

    #[test]
    fn failed_extended_security_inspection_is_not_acl_absence() {
        assert_eq!(
            require_extended_security(
                Err(ProtectedDirectoryError::Unavailable),
                synthetic_capability()
            ),
            Err(ProtectedDirectoryError::Unavailable)
        );
    }

    #[test]
    fn confirmed_zero_length_acl_absence_is_admitted() {
        let record = absent_acl_record();
        assert_eq!(
            require_extended_security(Ok(&record.bytes), synthetic_capability()),
            Ok(())
        );
    }

    #[test]
    fn zero_entry_acl_and_no_acl_sentinel_are_admitted() {
        for count in [0, NO_ACL] {
            let record = acl_record(count, 0, &[]);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Ok(())
            );
        }
    }

    #[test]
    fn known_readonly_allow_rights_are_admitted() {
        for rights in [
            0,
            1 << 1,
            1 << 3,
            1 << 7,
            1 << 9,
            1 << 11,
            1 << 20,
            READ_ONLY,
        ] {
            let record = acl_record(1, 0, &[(OTHER_GUID, ALLOW, rights)]);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Ok(()),
                "rights {rights:x}"
            );
        }
    }

    #[test]
    fn known_deny_rights_are_admitted_including_mutation_and_generic_rights() {
        let rights =
            READ_ONLY | MUTATION | GENERIC_ALL | GENERIC_EXECUTE | GENERIC_WRITE | GENERIC_READ;
        let record = acl_record(1, 0, &[(OTHER_GUID, DENY, rights)]);
        assert_eq!(
            require_extended_security(Ok(&record.bytes), synthetic_capability()),
            Ok(())
        );
    }

    #[test]
    fn every_mutation_allow_right_is_rejected() {
        // WRITE_DATA, DELETE, APPEND_DATA, DELETE_CHILD, WRITE_ATTRIBUTES,
        // WRITE_EXTATTRIBUTES, WRITE_SECURITY, CHANGE_OWNER.
        for rights in [
            1 << 2,
            1 << 4,
            1 << 5,
            1 << 6,
            1 << 8,
            1 << 10,
            1 << 12,
            1 << 13,
        ] {
            let record = acl_record(1, 0, &[(OTHER_GUID, ALLOW, rights)]);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Err(ProtectedDirectoryError::Unsafe),
                "rights {rights:x}"
            );
        }
    }

    #[test]
    fn preceding_deny_does_not_rescue_a_mutation_allow() {
        let record = acl_record(
            2,
            0,
            &[(OTHER_GUID, DENY, MUTATION), (OTHER_GUID, ALLOW, MUTATION)],
        );
        assert_eq!(
            require_extended_security(Ok(&record.bytes), synthetic_capability()),
            Err(ProtectedDirectoryError::Unsafe)
        );
    }

    #[test]
    fn owner_qualified_mutation_allow_is_rejected() {
        // The principal matches the synthetic root-owned file's owner GUID.
        let record = acl_record(1, 0, &[(OWNER_GUID, ALLOW, MUTATION)]);
        assert_eq!(
            require_extended_security(Ok(&record.bytes), synthetic_capability()),
            Err(ProtectedDirectoryError::Unsafe)
        );
    }

    #[test]
    fn inherit_only_and_inherited_mutation_allow_entries_are_rejected() {
        for flags in [1 << 8, 1 << 4, INHERITANCE_FLAGS] {
            let record = acl_record(1, 0, &[(OTHER_GUID, ALLOW | flags, MUTATION)]);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn known_inheritance_flags_do_not_reject_readonly_allow_or_deny_entries() {
        for (kind, rights) in [(ALLOW, READ_ONLY), (DENY, MUTATION)] {
            let record = acl_record(1, 0, &[(OTHER_GUID, kind | INHERITANCE_FLAGS, rights)]);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Ok(())
            );
        }
    }

    #[test]
    fn generic_read_and_execute_allows_are_admitted() {
        for rights in [
            GENERIC_READ,
            GENERIC_EXECUTE,
            GENERIC_READ | GENERIC_EXECUTE,
        ] {
            let record = acl_record(1, 0, &[(OTHER_GUID, ALLOW, rights)]);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Ok(())
            );
        }
    }

    #[test]
    fn generic_write_and_all_allows_are_rejected() {
        for rights in [GENERIC_WRITE, GENERIC_ALL, GENERIC_READ | GENERIC_WRITE] {
            let record = acl_record(1, 0, &[(OTHER_GUID, ALLOW, rights)]);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn unknown_rights_are_rejected_in_allow_and_deny_entries() {
        for kind in [ALLOW, DENY] {
            for rights in [1 << 0, 1 << 14, 1 << 19, 1 << 25, 1 << 31] {
                let record = acl_record(1, 0, &[(OTHER_GUID, kind, rights)]);
                assert_eq!(
                    require_extended_security(Ok(&record.bytes), synthetic_capability()),
                    Err(ProtectedDirectoryError::Unsafe)
                );
            }
        }
    }

    #[test]
    fn unknown_and_unimplemented_entry_kinds_are_rejected() {
        for kind in [0, 3, 4, 15] {
            let record = acl_record(1, 0, &[(OTHER_GUID, kind, READ_ONLY)]);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn unknown_and_audit_only_entry_flags_are_rejected() {
        for kind in [ALLOW, DENY] {
            for flags in [1 << 9, 1 << 10, 1 << 31] {
                let record = acl_record(1, 0, &[(OTHER_GUID, kind | flags, READ_ONLY)]);
                assert_eq!(
                    require_extended_security(Ok(&record.bytes), synthetic_capability()),
                    Err(ProtectedDirectoryError::Unsafe)
                );
            }
        }
    }

    #[test]
    fn known_acl_inheritance_flags_are_admitted() {
        for flags in [1 << 16, 1 << 17, (1 << 16) | (1 << 17)] {
            let record = acl_record(1, flags, &[(OTHER_GUID, ALLOW, READ_ONLY)]);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Ok(())
            );
        }
    }

    #[test]
    fn unknown_acl_flags_are_rejected_even_with_no_acl_sentinel() {
        for count in [0, NO_ACL] {
            for flags in [1, 1 << 18, 1 << 31] {
                let record = acl_record(count, flags, &[]);
                assert_eq!(
                    require_extended_security(Ok(&record.bytes), synthetic_capability()),
                    Err(ProtectedDirectoryError::Unsafe)
                );
            }
        }
    }

    #[test]
    fn all_128_readonly_entries_are_admitted() {
        let entries = [(OTHER_GUID, ALLOW, READ_ONLY); 128];
        let record = acl_record(128, 0, &entries);
        assert_eq!(
            require_extended_security(Ok(&record.bytes), synthetic_capability()),
            Ok(())
        );
    }

    #[test]
    fn a_mutation_grant_in_entry_128_is_rejected() {
        let mut entries = [(OTHER_GUID, ALLOW, READ_ONLY); 128];
        entries[127].2 |= 1 << 12;
        let record = acl_record(128, 0, &entries);
        assert_eq!(
            require_extended_security(Ok(&record.bytes), synthetic_capability()),
            Err(ProtectedDirectoryError::Unsafe)
        );
    }

    #[test]
    fn more_than_128_entries_are_rejected_even_with_consistent_lengths() {
        let entries = [(OTHER_GUID, ALLOW, READ_ONLY); 129];
        let record = acl_record(129, 0, &entries);
        assert_eq!(
            require_extended_security(Ok(&record.bytes), synthetic_capability()),
            Err(ProtectedDirectoryError::Unsafe)
        );
    }

    #[test]
    fn truncated_record_header_filesec_header_and_entries_are_rejected() {
        let record = acl_record(1, 0, &[(OTHER_GUID, ALLOW, READ_ONLY)]);
        for length in [0, 3, 23, 31, 32, 75, record.length - 1] {
            assert_eq!(
                require_extended_security(Ok(&record.bytes[..length]), synthetic_capability()),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn invalid_reported_acl_lengths_and_fullsize_truncation_are_rejected() {
        for length in [0, 3, 23, 31, 99, 101, 4097, u32::MAX] {
            let mut record = acl_record(1, 0, &[(OTHER_GUID, ALLOW, READ_ONLY)]);
            put_word(&mut record, 0, length);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn negative_overlapping_or_out_of_bounds_reference_offsets_are_rejected() {
        for offset in [i32::MIN, -24, -1, 0, 1, 7, i32::MAX] {
            let mut record = acl_record(1, 0, &[(OTHER_GUID, ALLOW, READ_ONLY)]);
            record.bytes[24..28].copy_from_slice(&offset.to_ne_bytes());
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn reference_lengths_must_match_the_complete_payload() {
        for length in [0, 43, 67, 69, u32::MAX] {
            let mut record = acl_record(1, 0, &[(OTHER_GUID, ALLOW, READ_ONLY)]);
            put_word(&mut record, 28, length);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn invalid_filesec_magic_is_rejected() {
        for magic in [0, FILESEC_MAGIC ^ 1, u32::MAX] {
            let mut record = acl_record(0, 0, &[]);
            put_word(&mut record, 32, magic);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn inconsistent_counts_and_sentinel_payloads_are_rejected() {
        for count in [0, 2, NO_ACL, u32::MAX - 1] {
            let record = acl_record(count, 0, &[(OTHER_GUID, ALLOW, READ_ONLY)]);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn missing_or_unrequested_acl_attributes_are_rejected() {
        for (offset, value) in [
            (4, EXTENDED_SECURITY),
            (4, RETURNED_ATTRS),
            (4, RETURNED_ATTRS | EXTENDED_SECURITY | 1),
            (8, VOLUME_CAPABILITIES),
            (12, 1),
            (16, 1),
            (20, 1),
        ] {
            let mut record = acl_record(1, 0, &[(OTHER_GUID, ALLOW, READ_ONLY)]);
            put_word(&mut record, offset, value);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn zero_length_acl_absence_rejects_inconsistent_returned_attributes() {
        for attributes in [0, RETURNED_ATTRS | EXTENDED_SECURITY] {
            let mut record = absent_acl_record();
            put_word(&mut record, 4, attributes);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }

    #[test]
    fn acl_absence_does_not_skip_record_or_reference_bounds() {
        for (offset, reported_length, reference_length) in [
            (8_i32, 4097, 0),
            (-1, 32, 0),
            (0, 32, 0),
            (i32::MAX, 32, 0),
            (8, 32, 1),
        ] {
            let mut record = absent_acl_record();
            put_word(&mut record, 0, reported_length);
            record.bytes[24..28].copy_from_slice(&offset.to_ne_bytes());
            put_word(&mut record, 28, reference_length);
            assert_eq!(
                require_extended_security(Ok(&record.bytes), synthetic_capability()),
                Err(ProtectedDirectoryError::Unsafe)
            );
        }
    }
}
