//! The host's system version, which a feed's `minimum_system` names: the macOS version on
//! `macos-arm64`, the glibc version on `linux-x86_64`.
use super::facts::{Platform, decimal};
use super::feed::SystemVersion;

/// This build's platform.
pub const PLATFORM: Platform = if cfg!(target_os = "macos") {
    Platform::MacosArm64
} else {
    Platform::LinuxX86_64
};

/// The leading `MAJOR.MINOR` of a version text such as `26.0.1` or `2.35`. A missing minor
/// reads as 0.
pub fn parse(text: &str) -> Option<SystemVersion> {
    let mut parts = text.trim().split('.');
    let major = decimal(parts.next()?)?;
    let minor = match parts.next() {
        Some(minor) => decimal(minor)?,
        None => 0,
    };
    Some(SystemVersion { major, minor })
}

/// The running host's system version. None when the system does not say.
pub fn host() -> Option<SystemVersion> {
    parse(&native()?)
}

#[cfg(target_os = "macos")]
fn native() -> Option<String> {
    let mut buffer = [0u8; 64];
    let mut length = buffer.len();
    // SAFETY: the name is NUL-terminated, and the kernel writes at most `length` bytes into
    // `buffer` and stores the written length back.
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return None;
    }
    let text = buffer.get(..length)?;
    let text = text.strip_suffix(&[0]).unwrap_or(text);
    String::from_utf8(text.to_vec()).ok()
}

#[cfg(target_os = "linux")]
fn native() -> Option<String> {
    // SAFETY: glibc returns a pointer to a static NUL-terminated string.
    let version = unsafe { std::ffi::CStr::from_ptr(libc::gnu_get_libc_version()) };
    version.to_str().ok().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_versions_read_their_major_and_minor() {
        let version = |major, minor| Some(SystemVersion { major, minor });
        assert_eq!(parse("26.0.1"), version(26, 0));
        assert_eq!(parse("2.35"), version(2, 35));
        assert_eq!(parse("27"), version(27, 0));
        assert_eq!(parse("26.1\n"), version(26, 1));
        assert_eq!(parse(""), None);
        assert_eq!(parse("2.x"), None);
        assert_eq!(parse("02.35"), None);
        assert!(
            SystemVersion {
                major: 2,
                minor: 35
            } < SystemVersion {
                major: 2,
                minor: 39
            }
        );
        assert!(
            SystemVersion {
                major: 26,
                minor: 9
            } < SystemVersion {
                major: 27,
                minor: 0
            }
        );
    }

    #[test]
    fn the_host_reports_its_system_version() {
        let host = host().expect("the host names its system version");
        if cfg!(target_os = "macos") {
            assert!(host.major >= 26, "{host:?}");
        } else {
            assert!(host.major == 2, "{host:?}");
        }
    }
}
