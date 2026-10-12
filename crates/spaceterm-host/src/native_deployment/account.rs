//! Native account incarnation comparison, distinct from publication authority.

use super::contracts::{AccountBuildKey, NativeFailure, build_code, nonzero, validate_key};
use sha2::{Digest, Sha256};
use std::fmt;

/// Canonical account-incarnation facts. A checked value does not prove its OS provenance.
#[derive(Clone, Eq, PartialEq)]
pub struct NativeAccountId {
    platform: u8,
    bytes: Vec<u8>,
}
impl NativeAccountId {
    /// Checks the binary Directory Services GeneratedUID, not a username or home path.
    pub fn macos(generated_uid: [u8; 16]) -> Result<Self, NativeFailure> {
        nonzero(&generated_uid)?;
        Ok(Self {
            platform: 1,
            bytes: generated_uid.to_vec(),
        })
    }
    /// Checks canonical Linux provider identities. Only an OS-anchored provider may attest them.
    pub fn linux(issuer: &[u8], incarnation: &[u8]) -> Result<Self, NativeFailure> {
        if issuer.is_empty()
            || issuer.len() > 256
            || incarnation.is_empty()
            || incarnation.len() > 256
        {
            return Err(NativeFailure::InvalidRecord);
        }
        nonzero(issuer)?;
        nonzero(incarnation)?;
        let mut bytes = Vec::with_capacity(4 + issuer.len() + incarnation.len());
        bytes.extend_from_slice(&(issuer.len() as u16).to_le_bytes());
        bytes.extend_from_slice(issuer);
        bytes.extend_from_slice(&(incarnation.len() as u16).to_le_bytes());
        bytes.extend_from_slice(incarnation);
        Ok(Self { platform: 2, bytes })
    }
    pub(crate) fn platform(&self) -> u8 {
        self.platform
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
impl fmt::Debug for NativeAccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeAccountId(..)")
    }
}

/// UID, primary GID, build namespace, and native account incarnation comparison facts.
#[derive(Clone, Eq, PartialEq)]
pub struct AccountBinding {
    key: AccountBuildKey,
    primary_gid: u32,
    native: NativeAccountId,
}
impl AccountBinding {
    /// Checks bounded facts. This constructor does not attest a live native account.
    pub fn new(
        key: AccountBuildKey,
        primary_gid: u32,
        native: NativeAccountId,
    ) -> Result<Self, NativeFailure> {
        validate_key(key)?;
        if primary_gid == u32::MAX {
            return Err(NativeFailure::InvalidRecord);
        }
        Ok(Self {
            key,
            primary_gid,
            native,
        })
    }
    /// Returns the exact registered account/build key.
    pub fn key(&self) -> AccountBuildKey {
        self.key
    }
    /// Returns the approved primary group, never an environment override.
    pub fn primary_gid(&self) -> u32 {
        self.primary_gid
    }
    /// Returns the canonical domain-separated binding digest.
    pub fn sha256(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"SpaceTerm native account binding v1\0");
        h.update([self.native.platform(), build_code(self.key.build_identity)]);
        h.update(self.key.account_uid.to_le_bytes());
        h.update(self.primary_gid.to_le_bytes());
        h.update((self.native.bytes().len() as u16).to_le_bytes());
        h.update(self.native.bytes());
        h.finalize().into()
    }
    /// Rejects UID reuse, primary-group change, or another build namespace.
    pub fn compare(&self, current: &Self) -> Result<(), NativeFailure> {
        if self == current {
            Ok(())
        } else {
            Err(NativeFailure::IdentityMismatch)
        }
    }
}
impl fmt::Debug for AccountBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccountBinding(..)")
    }
}

/// Read-only native account-lifecycle observation supplied by the trusted composition root.
pub trait NativeAccountProvider: Send + Sync {
    /// Proves account incarnation and primary GID through an OS-anchored provider.
    fn observe(&self, key: AccountBuildKey) -> Result<AccountBinding, NativeFailure>;
}

/// Native comparison observer. Observation never grants custody or credential authority.
#[derive(Default)]
pub struct SystemAccountProvider;
impl NativeAccountProvider for SystemAccountProvider {
    fn observe(&self, key: AccountBuildKey) -> Result<AccountBinding, NativeFailure> {
        ObservedAccountProvider(SystemPeer).observe(key)
    }
}

trait AccountPeer: Send + Sync {
    fn observe(&self, uid: u32) -> Result<AccountObservation, NativeFailure>;
}
struct ObservedAccountProvider<P>(P);
impl<P: AccountPeer> NativeAccountProvider for ObservedAccountProvider<P> {
    fn observe(&self, key: AccountBuildKey) -> Result<AccountBinding, NativeFailure> {
        validate_key(key)?;
        let (uid, gid, native) = match self.0.observe(key.account_uid)? {
            AccountObservation::Macos {
                uid,
                gid,
                generated_uid,
                uid_matches,
                identity_matches,
            } => {
                if uid_matches != 1 || identity_matches != 1 {
                    return Err(NativeFailure::AccountIdentityUnavailable);
                }
                (
                    uid,
                    gid,
                    NativeAccountId::macos(generated_uid)
                        .map_err(|_| NativeFailure::AccountIdentityUnavailable)?,
                )
            }
            AccountObservation::Linux {
                uid,
                gid,
                issuer,
                home,
                registrations,
            } => {
                if registrations > 1 {
                    return Err(NativeFailure::AccountIdentityUnavailable);
                }
                (
                    uid,
                    gid,
                    NativeAccountId::linux(&issuer, &home.canonical()?)?,
                )
            }
        };
        if uid != key.account_uid || gid == u32::MAX {
            return Err(NativeFailure::AccountIdentityUnavailable);
        }
        AccountBinding::new(key, gid, native)
    }
}

#[derive(Clone)]
enum AccountObservation {
    #[cfg_attr(
        not(any(target_os = "macos", test)),
        expect(dead_code, reason = "macOS native observation")
    )]
    Macos {
        uid: u32,
        gid: u32,
        generated_uid: [u8; 16],
        uid_matches: usize,
        identity_matches: usize,
    },
    #[cfg_attr(
        not(any(target_os = "linux", test)),
        expect(dead_code, reason = "Linux native observation")
    )]
    Linux {
        uid: u32,
        gid: u32,
        issuer: [u8; 32],
        home: HomeObject,
        registrations: usize,
    },
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct HomeObject {
    inode: u64,
    birth_seconds: i64,
    birth_nanoseconds: u32,
}
impl HomeObject {
    fn canonical(self) -> Result<[u8; 21], NativeFailure> {
        if self.inode == 0 || self.birth_nanoseconds >= 1_000_000_000 {
            return Err(NativeFailure::AccountIdentityUnavailable);
        }
        let mut bytes = [0; 21];
        bytes[0] = 1; // HomeObject. Providers 2 and 3 remain reserved.
        bytes[1..9].copy_from_slice(&self.inode.to_le_bytes());
        bytes[9..17].copy_from_slice(&self.birth_seconds.to_le_bytes());
        bytes[17..21].copy_from_slice(&self.birth_nanoseconds.to_le_bytes());
        Ok(bytes)
    }
}
#[cfg(any(target_os = "linux", test))]
fn require_home_filesystem(magic: u64) -> Result<(), NativeFailure> {
    match magic {
        0xef53 | 0x58465342 | 0x9123683e => Ok(()), // ext4, XFS, btrfs
        _ => Err(NativeFailure::AccountIdentityUnavailable),
    }
}
struct SystemPeer;
impl AccountPeer for SystemPeer {
    fn observe(&self, uid: u32) -> Result<AccountObservation, NativeFailure> {
        #[cfg(target_os = "macos")]
        {
            macos_observer::observe(uid)
        }
        #[cfg(target_os = "linux")]
        {
            linux_observer::observe(uid)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = uid;
            Err(NativeFailure::UnsupportedHost)
        }
    }
}

#[cfg(test)]
mod observer_tests {
    use super::*;
    use crate::startup::BuildIdentity;

    struct Peer(AccountObservation);
    impl AccountPeer for Peer {
        fn observe(&self, _: u32) -> Result<AccountObservation, NativeFailure> {
            Ok(self.0.clone())
        }
    }
    fn key() -> AccountBuildKey {
        AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        }
    }
    fn mac() -> AccountObservation {
        AccountObservation::Macos {
            uid: 501,
            gid: 20,
            generated_uid: [7; 16],
            uid_matches: 1,
            identity_matches: 1,
        }
    }
    fn home(inode: u64) -> AccountObservation {
        AccountObservation::Linux {
            uid: 501,
            gid: 20,
            issuer: [9; 32],
            home: HomeObject {
                inode,
                birth_seconds: 123,
                birth_nanoseconds: 456,
            },
            registrations: 1,
        }
    }
    #[test]
    fn generated_uid_observation_issues_through_the_host_account_interface() {
        let provider = ObservedAccountProvider(Peer(mac()));
        let observed = NativeAccountProvider::observe(&provider, key()).unwrap();
        assert_eq!(
            observed,
            AccountBinding::new(key(), 20, NativeAccountId::macos([7; 16]).unwrap()).unwrap()
        );
    }
    #[test]
    fn duplicate_generated_uid_never_observes_an_account() {
        let mut record = mac();
        if let AccountObservation::Macos {
            identity_matches, ..
        } = &mut record
        {
            *identity_matches = 2;
        }
        assert_eq!(
            ObservedAccountProvider(Peer(record)).observe(key()),
            Err(NativeFailure::AccountIdentityUnavailable)
        );
    }
    #[test]
    fn native_account_observation_rejects_uid_and_gid_substitution() {
        for (uid, gid) in [(502, 20), (501, u32::MAX)] {
            let mut record = mac();
            if let AccountObservation::Macos {
                uid: current_uid,
                gid: current_gid,
                ..
            } = &mut record
            {
                *current_uid = uid;
                *current_gid = gid;
            }
            assert!(
                ObservedAccountProvider(Peer(record))
                    .observe(key())
                    .is_err()
            );
        }
    }
    #[test]
    fn home_object_golden_vector_excludes_device_mount_path_and_names() {
        let object = HomeObject {
            inode: 0x0102030405060708,
            birth_seconds: 0x1122334455667788,
            birth_nanoseconds: 0x12345678,
        };
        assert_eq!(
            object.canonical().unwrap(),
            [
                1, 8, 7, 6, 5, 4, 3, 2, 1, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x78,
                0x56, 0x34, 0x12
            ]
        );
    }
    #[test]
    fn home_object_observation_issues_and_recreation_rejects_the_old_binding() {
        let original = ObservedAccountProvider(Peer(home(77)))
            .observe(key())
            .unwrap();
        assert_eq!(
            original.compare(
                &ObservedAccountProvider(Peer(home(77)))
                    .observe(key())
                    .unwrap()
            ),
            Ok(())
        );
        assert_eq!(
            original.compare(
                &ObservedAccountProvider(Peer(home(78)))
                    .observe(key())
                    .unwrap()
            ),
            Err(NativeFailure::IdentityMismatch)
        );
    }
    #[test]
    fn two_registrations_for_one_home_object_refuse_observation() {
        let mut record = home(77);
        if let AccountObservation::Linux { registrations, .. } = &mut record {
            *registrations = 2;
        }
        assert_eq!(
            ObservedAccountProvider(Peer(record)).observe(key()),
            Err(NativeFailure::AccountIdentityUnavailable)
        );
    }
    #[test]
    fn unsupported_lower_home_filesystems_never_fall_back_to_uid() {
        for magic in [
            0x6969, 0xff534d42, 0x0187, 0x01021994, 0x794c7630, 0x65735546,
        ] {
            assert_eq!(
                require_home_filesystem(magic),
                Err(NativeFailure::AccountIdentityUnavailable)
            );
        }
        for magic in [0xef53, 0x58465342, 0x9123683e] {
            assert_eq!(require_home_filesystem(magic), Ok(()));
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn system_generated_uid_observes_the_real_local_account() {
        // SAFETY: getuid has no arguments and does not mutate account state.
        let uid = unsafe { libc::getuid() };
        assert_ne!(
            uid, 0,
            "the account observer gate requires an unprivileged test account"
        );
        assert!(
            SystemAccountProvider
                .observe(AccountBuildKey {
                    account_uid: uid,
                    build_identity: BuildIdentity::Development
                })
                .is_ok()
        );
    }
}

#[cfg(target_os = "macos")]
mod macos_observer {
    use super::*;
    use std::{
        ffi::{c_char, c_void},
        ptr,
    };
    type Ref = *const c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: Ref);
        fn CFGetTypeID(value: Ref) -> usize;
        fn CFArrayGetTypeID() -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFArrayGetCount(array: Ref) -> isize;
        fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
        fn CFArrayCreate(allocator: Ref, values: *const Ref, count: isize, callbacks: Ref) -> Ref;
        fn CFStringCreateWithBytes(
            allocator: Ref,
            bytes: *const u8,
            length: isize,
            encoding: u32,
            external: u8,
        ) -> Ref;
        fn CFStringGetCString(string: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    }
    #[link(name = "OpenDirectory", kind = "framework")]
    unsafe extern "C" {
        static kODRecordTypeUsers: Ref;
        static kODAttributeTypeUniqueID: Ref;
        static kODAttributeTypePrimaryGroupID: Ref;
        static kODAttributeTypeGUID: Ref;
        fn ODNodeCreateWithNodeType(
            allocator: Ref,
            session: Ref,
            kind: u32,
            error: *mut Ref,
        ) -> Ref;
        fn ODQueryCreateWithNode(
            allocator: Ref,
            node: Ref,
            records: Ref,
            attribute: Ref,
            matching: u32,
            value: Ref,
            attributes: Ref,
            maximum: isize,
            error: *mut Ref,
        ) -> Ref;
        fn ODQueryCopyResults(query: Ref, partial: bool, error: *mut Ref) -> Ref;
        fn ODRecordCopyValues(record: Ref, attribute: Ref, error: *mut Ref) -> Ref;
    }
    struct Owned(Ref);
    impl Owned {
        fn checked(value: Ref, error: Ref) -> Result<Self, NativeFailure> {
            // SAFETY: OD's error and create/copy results use CF ownership conventions.
            unsafe {
                if !error.is_null() {
                    CFRelease(error);
                    if !value.is_null() {
                        CFRelease(value);
                    }
                    return Err(NativeFailure::AccountIdentityUnavailable);
                }
            }
            if value.is_null() {
                Err(NativeFailure::AccountIdentityUnavailable)
            } else {
                Ok(Self(value))
            }
        }
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: Owned retains exactly one create/copy reference.
            unsafe { CFRelease(self.0) }
        }
    }
    fn string(text: &[u8]) -> Result<Owned, NativeFailure> {
        // SAFETY: bounded borrowed bytes remain live for this copying call.
        Owned::checked(
            unsafe {
                CFStringCreateWithBytes(
                    ptr::null(),
                    text.as_ptr(),
                    text.len() as isize,
                    0x08000100,
                    0,
                )
            },
            ptr::null(),
        )
    }
    fn count(array: &Owned) -> Result<usize, NativeFailure> {
        // SAFETY: the create/copy reference is live; validate the native container type.
        unsafe {
            if CFGetTypeID(array.0) != CFArrayGetTypeID() {
                return Err(NativeFailure::AccountIdentityUnavailable);
            }
            let count = CFArrayGetCount(array.0);
            if !(0..=2).contains(&count) {
                return Err(NativeFailure::LimitExceeded);
            }
            Ok(count as usize)
        }
    }
    fn query(node: &Owned, attribute: Ref, value: &Owned) -> Result<Owned, NativeFailure> {
        let mut error = ptr::null();
        // SAFETY: static attribute constants and the node/value references remain live.
        let attributes = unsafe {
            [
                kODAttributeTypeUniqueID,
                kODAttributeTypePrimaryGroupID,
                kODAttributeTypeGUID,
            ]
        };
        let attributes = Owned::checked(
            unsafe { CFArrayCreate(ptr::null(), attributes.as_ptr(), 3, ptr::null()) },
            ptr::null(),
        )?;
        // Two results detect ambiguity without an unbounded directory enumeration.
        let query = Owned::checked(
            unsafe {
                ODQueryCreateWithNode(
                    ptr::null(),
                    node.0,
                    kODRecordTypeUsers,
                    attribute,
                    0x2001,
                    value.0,
                    attributes.0,
                    2,
                    &mut error,
                )
            },
            error,
        )?;
        error = ptr::null();
        Owned::checked(
            unsafe { ODQueryCopyResults(query.0, false, &mut error) },
            error,
        )
    }
    fn attribute(record: Ref, attribute: Ref) -> Result<Vec<u8>, NativeFailure> {
        let mut error = ptr::null();
        // SAFETY: record comes from a live OD result array; attribute is a static constant.
        let values = Owned::checked(
            unsafe { ODRecordCopyValues(record, attribute, &mut error) },
            error,
        )?;
        if count(&values)? != 1 {
            return Err(NativeFailure::AccountIdentityUnavailable);
        }
        let mut bytes = [0i8; 128];
        unsafe {
            let value = CFArrayGetValueAtIndex(values.0, 0);
            if value.is_null()
                || CFGetTypeID(value) != CFStringGetTypeID()
                || CFStringGetCString(value, bytes.as_mut_ptr(), 128, 0x08000100) == 0
            {
                return Err(NativeFailure::AccountIdentityUnavailable);
            }
        }
        let length = bytes
            .iter()
            .position(|value| *value == 0)
            .ok_or(NativeFailure::AccountIdentityUnavailable)?;
        Ok(bytes[..length].iter().map(|byte| *byte as u8).collect())
    }
    fn number(bytes: &[u8]) -> Result<u32, NativeFailure> {
        let text =
            std::str::from_utf8(bytes).map_err(|_| NativeFailure::AccountIdentityUnavailable)?;
        let number = text
            .parse::<u32>()
            .map_err(|_| NativeFailure::AccountIdentityUnavailable)?;
        if number == u32::MAX || number.to_string() != text {
            return Err(NativeFailure::AccountIdentityUnavailable);
        }
        Ok(number)
    }
    fn uuid(bytes: &[u8]) -> Result<[u8; 16], NativeFailure> {
        if bytes.len() != 36 || [8, 13, 18, 23].iter().any(|index| bytes[*index] != b'-') {
            return Err(NativeFailure::AccountIdentityUnavailable);
        }
        let mut result = [0; 16];
        let mut n = 0;
        for (i, byte) in bytes.iter().enumerate() {
            if [8, 13, 18, 23].contains(&i) {
                continue;
            }
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(NativeFailure::AccountIdentityUnavailable),
            };
            result[n / 2] = (result[n / 2] << 4) | digit;
            n += 1;
        }
        nonzero(&result).map_err(|_| NativeFailure::AccountIdentityUnavailable)?;
        Ok(result)
    }
    fn sample(node: &Owned, uid: u32) -> Result<AccountObservation, NativeFailure> {
        let text = string(uid.to_string().as_bytes())?;
        let records = query(node, unsafe { kODAttributeTypeUniqueID }, &text)?;
        if count(&records)? != 1 {
            return Err(NativeFailure::AccountIdentityUnavailable);
        }
        // SAFETY: exactly one result is retained by records for all attribute queries.
        let record = unsafe { CFArrayGetValueAtIndex(records.0, 0) };
        let observed_uid = number(&attribute(record, unsafe { kODAttributeTypeUniqueID })?)?;
        let gid = number(&attribute(record, unsafe {
            kODAttributeTypePrimaryGroupID
        })?)?;
        let guid = attribute(record, unsafe { kODAttributeTypeGUID })?;
        let generated_uid = uuid(&guid)?;
        let matches = query(node, unsafe { kODAttributeTypeGUID }, &string(&guid)?)?;
        let identity_matches = count(&matches)?;
        // Validate libc's native UID/GID lookup against the directory record.
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut buffer = [0i8; 16 * 1024];
        let mut result = ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                entry.as_mut_ptr(),
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut result,
            )
        };
        if status != 0 || result.is_null() {
            return Err(NativeFailure::AccountIdentityUnavailable);
        }
        let entry = unsafe { entry.assume_init() };
        if entry.pw_uid != observed_uid || entry.pw_gid != gid {
            return Err(NativeFailure::AccountIdentityUnavailable);
        }
        Ok(AccountObservation::Macos {
            uid: observed_uid,
            gid,
            generated_uid,
            uid_matches: 1,
            identity_matches,
        })
    }
    pub(super) fn observe(uid: u32) -> Result<AccountObservation, NativeFailure> {
        let mut error = ptr::null();
        // Authentication search node covers the configured native account providers.
        let node = Owned::checked(
            unsafe { ODNodeCreateWithNodeType(ptr::null(), ptr::null(), 0x2201, &mut error) },
            error,
        )?;
        let before = sample(&node, uid)?;
        let after = sample(&node, uid)?;
        match (&before, &after) {
            (
                AccountObservation::Macos {
                    uid: a,
                    gid: b,
                    generated_uid: c,
                    uid_matches: d,
                    identity_matches: e,
                },
                AccountObservation::Macos {
                    uid: f,
                    gid: g,
                    generated_uid: h,
                    uid_matches: i,
                    identity_matches: j,
                },
            ) if (a, b, c, d, e) == (f, g, h, i, j) => Ok(after),
            _ => Err(NativeFailure::IdentityMismatch),
        }
    }
}

#[cfg(target_os = "linux")]
mod linux_observer {
    use super::*;
    use std::{
        ffi::{CStr, CString},
        fs::File,
        io::Read,
        mem::MaybeUninit,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::MetadataExt,
        },
        ptr,
    };

    const REGISTRY: &str = "/var/lib/spaceterm/server-registrations";
    const MAX_REGISTRATIONS: usize = 4096;
    struct Chain {
        handles: Vec<File>,
        names: Vec<CString>,
    }
    fn unavailable() -> NativeFailure {
        NativeFailure::StorageUnavailable
    }
    fn open_at(parent: i32, name: &CStr, flags: i32) -> Result<File, NativeFailure> {
        // SAFETY: the borrowed name is NUL terminated; successful openat transfers its fd.
        let fd = unsafe {
            libc::openat(
                parent,
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::ELOOP | libc::ENOTDIR) => NativeFailure::UnsafeNativeState,
                _ => unavailable(),
            });
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    fn protected(file: &File) -> Result<(), NativeFailure> {
        let metadata = file.metadata().map_err(|_| unavailable())?;
        // On the admitted POSIX ACL filesystems the group mode is the ACL mask.
        // No masked named user or group may write when that bit is absent.
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let mut fs = MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::fstatfs(file.as_raw_fd(), fs.as_mut_ptr()) } != 0 {
            return Err(unavailable());
        }
        require_home_filesystem(u64::from(unsafe { fs.assume_init() }.f_type as u32))
    }
    impl Chain {
        fn open(path: &str) -> Result<Self, NativeFailure> {
            Self::bytes(path.as_bytes())
        }
        fn bytes(path: &[u8]) -> Result<Self, NativeFailure> {
            if path.first() != Some(&b'/') || path.len() > 4096 || path.contains(&0) {
                return Err(NativeFailure::UnsafeNativeState);
            }
            let mut handles = vec![open_at(
                libc::AT_FDCWD,
                c"/",
                libc::O_RDONLY | libc::O_DIRECTORY,
            )?];
            protected(&handles[0])?;
            let mut names = Vec::new();
            for part in path[1..].split(|b| *b == b'/') {
                if part.is_empty() || part == b"." || part == b".." || handles.len() >= 255 {
                    return Err(NativeFailure::UnsafeNativeState);
                }
                let name = CString::new(part).map_err(|_| NativeFailure::UnsafeNativeState)?;
                let child = open_at(
                    handles.last().ok_or_else(unavailable)?.as_raw_fd(),
                    &name,
                    libc::O_RDONLY | libc::O_DIRECTORY,
                )?;
                protected(&child)?;
                handles.push(child);
                names.push(name);
            }
            let chain = Self { handles, names };
            chain.verify()?;
            Ok(chain)
        }
        fn last(&self) -> Result<&File, NativeFailure> {
            self.handles.last().ok_or_else(unavailable)
        }
        fn verify(&self) -> Result<(), NativeFailure> {
            for handle in &self.handles {
                protected(handle)?;
            }
            for (index, name) in self.names.iter().enumerate() {
                let mut stat = MaybeUninit::<libc::stat>::uninit();
                if unsafe {
                    libc::fstatat(
                        self.handles[index].as_raw_fd(),
                        name.as_ptr(),
                        stat.as_mut_ptr(),
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                } != 0
                {
                    return Err(unavailable());
                }
                let stat = unsafe { stat.assume_init() };
                let retained = self.handles[index + 1]
                    .metadata()
                    .map_err(|_| unavailable())?;
                if stat.st_dev != retained.dev() || stat.st_ino != retained.ino() {
                    return Err(NativeFailure::UnsafeNativeState);
                }
            }
            Ok(())
        }
        fn read(&self, name: &CStr, limit: usize) -> Result<Vec<u8>, NativeFailure> {
            self.verify()?;
            let mut file = open_at(
                self.last()?.as_raw_fd(),
                name,
                libc::O_RDONLY | libc::O_NONBLOCK,
            )?;
            let before = file.metadata().map_err(|_| unavailable())?;
            if !before.is_file() || before.nlink() != 1 {
                return Err(NativeFailure::UnsafeNativeState);
            }
            protected(&file)?;
            let mut bytes = Vec::new();
            (&mut file)
                .take((limit + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| unavailable())?;
            if bytes.len() > limit {
                return Err(NativeFailure::LimitExceeded);
            }
            protected(&file)?;
            let after = file.metadata().map_err(|_| unavailable())?;
            let mut linked = MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    self.last()?.as_raw_fd(),
                    name.as_ptr(),
                    linked.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(unavailable());
            }
            let linked = unsafe { linked.assume_init() };
            if before.dev() != after.dev()
                || before.ino() != after.ino()
                || before.ctime() != after.ctime()
                || before.ctime_nsec() != after.ctime_nsec()
                || before.len() != after.len()
                || linked.st_dev != after.dev()
                || linked.st_ino != after.ino()
            {
                return Err(NativeFailure::UnsafeNativeState);
            }
            self.verify()?;
            Ok(bytes)
        }
    }
    fn issuer() -> Result<[u8; 32], NativeFailure> {
        let machine = Chain::open("/etc")?.read(c"machine-id", 33)?;
        let machine = machine.strip_suffix(b"\n").unwrap_or(&machine);
        if machine.len() != 32
            || !machine.iter().all(u8::is_ascii_hexdigit)
            || machine.iter().all(|b| *b == b'0')
        {
            return Err(NativeFailure::AccountIdentityUnavailable);
        }
        // This is a public installation-key identifier, not a credential or a
        // lifecycle marker. A2 provisions it once and preserves it through upgrades.
        let key_id = Chain::open(REGISTRY)?.read(c"installation-key-id", 32)?;
        if key_id.len() != 32 {
            return Err(NativeFailure::AccountIdentityUnavailable);
        }
        nonzero(&key_id).map_err(|_| NativeFailure::AccountIdentityUnavailable)?;
        let mut hash = Sha256::new();
        hash.update(b"SpaceTerm Linux HomeObject issuer v1\0");
        hash.update(machine);
        hash.update(key_id);
        Ok(hash.finalize().into())
    }
    fn home(uid: u32) -> Result<(u32, Vec<u8>), NativeFailure> {
        let mut passwd = MaybeUninit::<libc::passwd>::uninit();
        let mut buffer = [0i8; 16 * 1024];
        let mut result = ptr::null_mut();
        // SAFETY: passwd and buffer are writable for the full reentrant lookup.
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                passwd.as_mut_ptr(),
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut result,
            )
        };
        if status != 0 || result.is_null() {
            return Err(NativeFailure::AccountIdentityUnavailable);
        }
        let passwd = unsafe { passwd.assume_init() };
        if passwd.pw_uid != uid || passwd.pw_gid == u32::MAX || passwd.pw_dir.is_null() {
            return Err(NativeFailure::AccountIdentityUnavailable);
        }
        // libc's returned strings reside in the bounded lookup buffer.
        let path = unsafe { CStr::from_ptr(passwd.pw_dir) }.to_bytes().to_vec();
        if path.len() < 2 || path.len() > 4096 || path[0] != b'/' {
            return Err(NativeFailure::AccountIdentityUnavailable);
        }
        Ok((passwd.pw_gid, path))
    }
    fn lower_home(uid: u32, path: &[u8]) -> Result<HomeObject, NativeFailure> {
        let split = path
            .iter()
            .rposition(|b| *b == b'/')
            .ok_or(NativeFailure::AccountIdentityUnavailable)?;
        let parent = if split == 0 {
            b"/".as_slice()
        } else {
            &path[..split]
        };
        let name = &path[split + 1..];
        if name.is_empty() || name == b"." || name == b".." {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let name = CString::new(name).map_err(|_| NativeFailure::UnsafeNativeState)?;
        let chain = if parent == b"/" {
            let root = open_at(libc::AT_FDCWD, c"/", libc::O_RDONLY | libc::O_DIRECTORY)?;
            protected(&root)?;
            Chain {
                handles: vec![root],
                names: vec![],
            }
        } else {
            Chain::bytes(parent)?
        };
        // AT_RECURSIVE is deliberately absent. The detached parent clone contains
        // the underlying home dentry, not any encrypted/FUSE/login-time child mount.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_open_tree,
                chain.last()?.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_EMPTY_PATH as u32 | 1 | libc::O_CLOEXEC as u32,
            )
        };
        if fd < 0 {
            return Err(unavailable());
        }
        let clone = unsafe { File::from_raw_fd(fd as i32) };
        let lower = open_at(clone.as_raw_fd(), &name, libc::O_RDONLY | libc::O_DIRECTORY)?;
        let mut fs = MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::fstatfs(lower.as_raw_fd(), fs.as_mut_ptr()) } != 0 {
            return Err(unavailable());
        }
        require_home_filesystem(u64::from(unsafe { fs.assume_init() }.f_type as u32))?;
        let mut stat = MaybeUninit::<libc::statx>::uninit();
        if unsafe {
            libc::statx(
                lower.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
                libc::STATX_INO | libc::STATX_BTIME | libc::STATX_UID | libc::STATX_TYPE,
                stat.as_mut_ptr(),
            )
        } != 0
        {
            return Err(unavailable());
        }
        let stat = unsafe { stat.assume_init() };
        let required = libc::STATX_INO | libc::STATX_BTIME | libc::STATX_UID | libc::STATX_TYPE;
        if stat.stx_mask & required != required
            || stat.stx_uid != uid
            || u32::from(stat.stx_mode) & libc::S_IFMT != libc::S_IFDIR
        {
            return Err(NativeFailure::AccountIdentityUnavailable);
        }
        let object = HomeObject {
            inode: stat.stx_ino,
            birth_seconds: stat.stx_btime.tv_sec,
            birth_nanoseconds: stat.stx_btime.tv_nsec,
        };
        object.canonical()?;
        chain.verify()?;
        Ok(object)
    }
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    fn registrations(issuer: &[u8; 32], object: HomeObject) -> Result<usize, NativeFailure> {
        use crate::{
            native_deployment::profile::RegistrationProfile,
            native_registration::decode_registration_head, startup::BuildIdentity,
        };
        let chain = Chain::open(REGISTRY)?;
        let fd = unsafe { libc::fcntl(chain.last()?.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 {
            return Err(unavailable());
        }
        let directory = unsafe { libc::fdopendir(fd) };
        if directory.is_null() {
            unsafe {
                libc::close(fd);
            }
            return Err(unavailable());
        }
        let directory = Directory(directory);
        let identity = NativeAccountId::linux(issuer, &object.canonical()?)?;
        let mut count = 0;
        let mut entries = 0;
        loop {
            unsafe {
                *libc::__errno_location() = 0;
            }
            let entry = unsafe { libc::readdir(directory.0) };
            if entry.is_null() {
                if unsafe { *libc::__errno_location() } != 0 {
                    return Err(unavailable());
                }
                break;
            }
            entries += 1;
            if entries > MAX_REGISTRATIONS {
                return Err(NativeFailure::LimitExceeded);
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            let Some(stem) = name.to_bytes().strip_suffix(b".registration") else {
                continue;
            };
            let stem = std::str::from_utf8(stem).map_err(|_| NativeFailure::InvalidRecord)?;
            let (build, uid) = stem.split_once('-').ok_or(NativeFailure::InvalidRecord)?;
            let build_identity = match build {
                "release" => BuildIdentity::Release,
                "preflight" => BuildIdentity::Preflight,
                "development" => BuildIdentity::Development,
                _ => return Err(NativeFailure::InvalidRecord),
            };
            let account_uid = uid
                .parse::<u32>()
                .map_err(|_| NativeFailure::InvalidRecord)?;
            if account_uid.to_string() != uid {
                return Err(NativeFailure::InvalidRecord);
            }
            let key = AccountBuildKey {
                account_uid,
                build_identity,
            };
            let head = decode_registration_head(&chain.read(name, 256)?, key)?;
            // Immutable profile names are full digest hex, never a generation scan.
            let profiles = Chain::open(&format!("{REGISTRY}/profiles"))?;
            let profile_name = CString::new(format!(
                "{}.profile",
                super::super::bindings::hex(&head.profile_sha256)
            ))
            .map_err(|_| NativeFailure::InvalidRecord)?;
            let bytes = profiles.read(&profile_name, 64 * 1024)?;
            if <[u8; 32]>::from(Sha256::digest(&bytes)) != head.profile_sha256 {
                return Err(NativeFailure::IdentityMismatch);
            }
            let profile = RegistrationProfile::decode(&bytes, key)?;
            if profile.account().native == identity {
                count += 1;
            }
            if count > 1 {
                return Err(NativeFailure::AccountIdentityUnavailable);
            }
        }
        chain.verify()?;
        Ok(count)
    }
    pub(super) fn observe(uid: u32) -> Result<AccountObservation, NativeFailure> {
        // The account-only server cannot assert its own native incarnation.
        if unsafe { libc::geteuid() } != 0 {
            return Err(NativeFailure::AccountIdentityUnavailable);
        }
        let (gid, path) = home(uid)?;
        let issuer = issuer()?;
        let object = lower_home(uid, &path)?;
        let registrations = registrations(&issuer, object)?;
        let (after_gid, after_path) = home(uid)?;
        let after_object = lower_home(uid, &after_path)?;
        if issuer != self::issuer()? || gid != after_gid || object != after_object {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(AccountObservation::Linux {
            uid,
            gid,
            issuer,
            home: object,
            registrations,
        })
    }
}
