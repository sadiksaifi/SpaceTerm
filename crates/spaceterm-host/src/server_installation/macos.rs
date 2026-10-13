use crate::account_service::ServiceFailure;
use spaceterm_distribution::BuildChannel;
use std::{ffi::c_void, os::unix::ffi::OsStrExt, path::Path};

type Reference = *const c_void;
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: Reference,
        bytes: *const u8,
        length: isize,
        directory: u8,
    ) -> Reference;
    fn CFStringCreateWithBytes(
        allocator: Reference,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external: u8,
    ) -> Reference;
    fn CFRelease(value: Reference);
}
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecStaticCodeCreateWithPath(path: Reference, flags: u32, code: *mut Reference) -> i32;
    fn SecRequirementCreateWithString(
        string: Reference,
        flags: u32,
        requirement: *mut Reference,
    ) -> i32;
    fn SecStaticCodeCheckValidity(code: Reference, flags: u32, requirement: Reference) -> i32;
}
struct Object(Reference);
impl Object {
    fn new(value: Reference) -> Result<Self, ServiceFailure> {
        if value.is_null() {
            Err(ServiceFailure::ArtifactRejected)
        } else {
            Ok(Self(value))
        }
    }
}
impl Drop for Object {
    fn drop(&mut self) {
        // SAFETY: each Object owns one successful CoreFoundation/Security Create reference.
        unsafe { CFRelease(self.0) };
    }
}

pub(super) fn verify(path: &Path, build: BuildChannel) -> Result<(), ServiceFailure> {
    let bytes = path.as_os_str().as_bytes();
    let length = isize::try_from(bytes.len()).map_err(|_| ServiceFailure::ArtifactRejected)?;
    // SAFETY: the byte slice remains alive during creation; the resulting CFURL owns its data.
    let url = Object::new(unsafe {
        CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), length, 1)
    })?;
    let mut code = std::ptr::null();
    // SAFETY: live URL and writable output pointer. No code in the candidate executes.
    if unsafe { SecStaticCodeCreateWithPath(url.0, 0, &mut code) } != 0 {
        return Err(ServiceFailure::ArtifactRejected);
    }
    let code = Object::new(code)?;
    let mut expression = format!("identifier \"{}\"", build.native_identifier());
    // Read the same certificate pin the existing release packager uses. No second identity.
    let config = include_str!("../../../../packaging/macos/spaceterm/Packager.toml");
    let fingerprint = config
        .lines()
        .find_map(|line| {
            line.strip_prefix("signing-identity = \"")?
                .strip_suffix('"')
        })
        .filter(|value| value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or(ServiceFailure::ArtifactRejected)?;
    expression.push_str(&format!(" and certificate leaf = H\"{fingerprint}\""));

    // SAFETY: UTF-8 bytes remain alive for creation; the CFString owns a copy.
    let string = Object::new(unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            expression.as_ptr(),
            expression.len() as isize,
            0x08000100,
            0,
        )
    })?;
    let mut requirement = std::ptr::null();
    // SAFETY: live string and writable output pointer.
    if unsafe { SecRequirementCreateWithString(string.0, 0, &mut requirement) } != 0 {
        return Err(ServiceFailure::ArtifactRejected);
    }
    let requirement = Object::new(requirement)?;
    // kSecCSCheckAllArchitectures | kSecCSStrictValidate. Validates the entire sealed bundle.
    // SAFETY: both references remain owned throughout the side-effect-free signature check.
    if unsafe { SecStaticCodeCheckValidity(code.0, (1 << 0) | (1 << 4), requirement.0) } != 0 {
        return Err(ServiceFailure::ArtifactRejected);
    }
    Ok(())
}

// Account-private modes do not constrain extended macOS ACL entries.
pub(super) fn verify_acl(file: &std::fs::File) -> Result<(), ServiceFailure> {
    use std::os::fd::AsRawFd;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut c_void;
        fn acl_free(acl: *mut c_void) -> libc::c_int;
    }
    // SAFETY: the retained descriptor is live, and ACL_TYPE_EXTENDED is 0x100.
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), 0x100) };
    if acl.is_null() {
        return if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            Ok(())
        } else {
            Err(ServiceFailure::StorageUnavailable)
        };
    }
    // SAFETY: the ACL was allocated by acl_get_fd_np and is not used after release.
    if unsafe { acl_free(acl) } != 0 {
        return Err(ServiceFailure::StorageUnavailable);
    }
    Err(ServiceFailure::UnsafeStorage)
}
