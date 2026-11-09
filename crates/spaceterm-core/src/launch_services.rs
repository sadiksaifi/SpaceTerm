//! Launch Services registration of the server's own bundle.
//!
//! launchd starts SpaceTerm Core without Launch Services, so nothing registers its bundle on the
//! way. System Settings resolves the bundle through Launch Services to name a privacy entry, and
//! Login Items resolves it to name the server's background item. A retained installation that
//! SpaceTerm deletes is unregistered first, so neither names a bundle that is gone.
use std::path::Path;

type CFTypeRef = *const std::ffi::c_void;

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn LSRegisterURL(url: CFTypeRef, update: u8) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: CFTypeRef,
        buffer: *const u8,
        length: isize,
        directory: u8,
    ) -> CFTypeRef;
    fn CFRelease(value: CFTypeRef);
}

/// Registers the bundle at `bundle` with Launch Services. Returns whether Launch Services
/// accepted it.
pub(crate) fn register(bundle: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let bytes = bundle.as_os_str().as_bytes();
    let Ok(length) = isize::try_from(bytes.len()) else {
        return false;
    };
    // SAFETY: The buffer and length describe the path's bytes for the call. The URL is released
    // after Launch Services, which retains what it keeps, reads it.
    unsafe {
        let url =
            CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), length, 1);
        if url.is_null() {
            return false;
        }
        let status = LSRegisterURL(url, 0);
        CFRelease(url);
        status == 0
    }
}

/// Launch Services' own tool. Unregistering has no public function.
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister";

/// Unregisters the bundle at `bundle` from Launch Services. Returns whether Launch Services
/// accepted it.
pub(crate) fn unregister(bundle: &Path) -> bool {
    std::process::Command::new(LSREGISTER)
        .arg("-u")
        .arg(bundle)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}
