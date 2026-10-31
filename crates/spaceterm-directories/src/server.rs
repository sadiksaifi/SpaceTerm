//! Account-wide server discovery is independent of launch environments and installation paths.
use std::path::{Path, PathBuf};

use crate::DirectoryError;

#[derive(Clone)]
pub struct ServerDirectories {
    pub data: PathBuf,
    pub runtime: PathBuf,
}

impl ServerDirectories {
    /// Use account-home XDG defaults for durable server state. Environment overrides remain
    /// client-specific: a service and desktop must resolve the same identity and endpoint.
    pub fn for_account(home: &Path, uid: u32, identity: &str) -> Result<Self, DirectoryError> {
        if !matches!(
            identity,
            "spaceterm" | "spaceterm-preflight" | "spaceterm-development"
        ) || !home.is_absolute()
        {
            return Err(DirectoryError::InvalidAppName);
        }
        #[cfg(target_os = "macos")]
        let temporary = Path::new("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let temporary = Path::new("/tmp");
        Ok(Self {
            data: home.join(".local/share").join(identity).join("server"),
            runtime: temporary.join(format!("{identity}-server-{uid}")),
        })
    }
}

/// The account-private directory holding the desktop's channel endpoint. It sits in the
/// operating system's own per-account directory, so no launch environment and no other account
/// can choose or occupy it, and a shell the server starts finds the desktop the person launched:
/// the user cache directory `confstr(_CS_DARWIN_USER_CACHE_DIR)` on macOS, and the user runtime
/// directory `/run/user/<uid>` on Linux. The account has none without a login session there.
pub fn desktop_channel_directory(uid: u32, identity: &str) -> Result<PathBuf, DirectoryError> {
    ServerDirectories::for_account(Path::new("/"), uid, identity)?;
    Ok(private_account_root(&account_root(uid)?, uid)?.join(format!("{identity}-desktop")))
}

#[cfg(target_os = "macos")]
fn account_root(_: u32) -> Result<PathBuf, DirectoryError> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: a null buffer of length zero asks only for the required length.
    let length = unsafe { libc::confstr(libc::_CS_DARWIN_USER_CACHE_DIR, std::ptr::null_mut(), 0) };
    if length == 0 {
        return Err(DirectoryError::NoAccountDirectory);
    }
    let mut buffer = vec![0u8; length];
    // SAFETY: the buffer is writable for `length` bytes, which includes the terminator.
    let written = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_CACHE_DIR,
            buffer.as_mut_ptr().cast(),
            length,
        )
    };
    if written == 0 || written > length {
        return Err(DirectoryError::NoAccountDirectory);
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    let path = Path::new(std::ffi::OsStr::from_bytes(&buffer[..end]));
    if !path.is_absolute() {
        return Err(DirectoryError::NoAccountDirectory);
    }
    // The system names it beneath `/var`, a link to `/private/var`; descriptor-relative opening
    // follows no link.
    std::fs::canonicalize(path).map_err(|_| DirectoryError::NoAccountDirectory)
}

#[cfg(not(target_os = "macos"))]
fn account_root(uid: u32) -> Result<PathBuf, DirectoryError> {
    Ok(Path::new("/run/user").join(uid.to_string()))
}

/// `root`, when it is a real directory that `uid` owns and no other account can enter.
fn private_account_root(root: &Path, uid: u32) -> Result<PathBuf, DirectoryError> {
    use std::os::unix::fs::MetadataExt;
    let metadata =
        std::fs::symlink_metadata(root).map_err(|_| DirectoryError::NoAccountDirectory)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(DirectoryError::NoAccountDirectory);
    }
    Ok(root.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolates_accounts_and_build_identities_without_launch_environment() {
        let development =
            ServerDirectories::for_account(Path::new("/home/owner"), 501, "spaceterm-development")
                .unwrap();
        let release =
            ServerDirectories::for_account(Path::new("/home/owner"), 501, "spaceterm").unwrap();
        let other =
            ServerDirectories::for_account(Path::new("/home/other"), 502, "spaceterm-development")
                .unwrap();
        assert_eq!(
            development.data,
            Path::new("/home/owner/.local/share/spaceterm-development/server")
        );
        assert_eq!(
            release.data,
            Path::new("/home/owner/.local/share/spaceterm/server")
        );
        assert_eq!(
            other.data,
            Path::new("/home/other/.local/share/spaceterm-development/server")
        );
        assert_ne!(development.data, other.data);
        assert_ne!(development.data, release.data);
        assert_ne!(development.runtime, release.runtime);
        assert_ne!(development.runtime, other.runtime);
    }

    #[test]
    fn desktop_channel_directory_is_per_identity_in_the_accounts_own_directory() {
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let development = desktop_channel_directory(uid, "spaceterm-development").unwrap();
        let release = desktop_channel_directory(uid, "spaceterm").unwrap();
        assert_eq!(development.parent(), release.parent());
        assert_ne!(development, release);
        assert!(development.ends_with("spaceterm-development-desktop"));
        assert!(!development.starts_with("/tmp") && !development.starts_with("/private/tmp"));
        assert!(desktop_channel_directory(uid, "other").is_err());
        // The socket `d` beneath it fits the host's 103-byte `sun_path`.
        assert!(development.join("d").as_os_str().len() <= 103);
    }

    #[test]
    fn only_a_real_private_directory_of_the_account_is_its_root() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("spaceterm-account-root-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let private = root.join("private");
        let shared = root.join("shared");
        std::fs::create_dir(&private).unwrap();
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&private, root.join("link")).unwrap();
        assert_eq!(private_account_root(&private, uid).unwrap(), private);
        for refused in [shared.clone(), root.join("link"), root.join("missing")] {
            assert!(private_account_root(&refused, uid).is_err(), "{refused:?}");
        }
        assert!(private_account_root(&private, uid.wrapping_add(1)).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
