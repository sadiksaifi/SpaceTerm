use std::{
    ffi::{CStr, OsString},
    os::unix::{ffi::OsStringExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

use super::{ServiceFailure, registration::validate_path};

/// Facts resolved from the current account's native password database, not its environment.
/// Debug deliberately omits account paths.
pub struct TrustedAccountFacts {
    uid: u32,
    home: PathBuf,
    login_shell: PathBuf,
}
impl TrustedAccountFacts {
    /// Resolves the actual nonroot account with matching real and effective IDs.
    /// Missing account storage is unavailable, never an instruction to create a home.
    pub fn current() -> Result<Self, ServiceFailure> {
        // SAFETY: credential queries have no preconditions or side effects.
        let (uid, effective) = unsafe { (libc::getuid(), libc::geteuid()) };
        if uid == 0 || effective == 0 {
            return Err(ServiceFailure::RootAccount);
        }
        if uid != effective || uid == u32::MAX {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut result = std::ptr::null_mut();
        let mut account = std::mem::MaybeUninit::<libc::passwd>::uninit();
        // SAFETY: getpwuid_r bounds writes by buffer.len(). All strings are copied
        // while that buffer is alive, after checking successful initialization.
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                account.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status != 0 || result.is_null() {
            return Err(ServiceFailure::StorageUnavailable);
        }
        // SAFETY: successful getpwuid_r initialized account.
        let account = unsafe { account.assume_init() };
        if account.pw_uid != uid || account.pw_dir.is_null() || account.pw_shell.is_null() {
            return Err(ServiceFailure::IdentityMismatch);
        }
        // SAFETY: both strings belong to the successful password database result.
        let home = PathBuf::from(OsString::from_vec(
            unsafe { CStr::from_ptr(account.pw_dir) }
                .to_bytes()
                .to_vec(),
        ));
        let shell = unsafe { CStr::from_ptr(account.pw_shell) }.to_bytes();
        // An empty native shell means the POSIX default, not caller SHELL.
        let login_shell = PathBuf::from(OsString::from_vec(if shell.is_empty() {
            b"/bin/sh".to_vec()
        } else {
            shell.to_vec()
        }));
        validate_path(&home)?;
        validate_path(&login_shell)?;
        let facts =
            std::fs::symlink_metadata(&home).map_err(|_| ServiceFailure::StorageUnavailable)?;
        if !facts.is_dir() || facts.uid() != uid {
            return Err(ServiceFailure::UnsafeStorage);
        }
        Ok(Self {
            uid,
            home,
            login_shell,
        })
    }
    /// Returns the independently resolved account UID.
    pub fn uid(&self) -> u32 {
        self.uid
    }
    /// Returns the native account home, never HOME.
    pub fn home(&self) -> &Path {
        &self.home
    }
    /// Returns the native login shell, never SHELL.
    pub fn login_shell(&self) -> &Path {
        &self.login_shell
    }
}
impl std::fmt::Debug for TrustedAccountFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TrustedAccountFacts(..)")
    }
}
