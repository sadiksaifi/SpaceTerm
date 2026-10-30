//! The account launcher follows an approved runtime. Other local launchers remain untouched.
use super::*;
use std::ffi::CString;
use std::os::unix::fs::{DirBuilderExt, symlink};

fn managed(target: &Path, installations: &Path) -> bool {
    let Ok(relative) = target.strip_prefix(installations) else {
        return false;
    };
    let parts: Vec<_> = relative.components().collect();
    matches!(parts.as_slice(), [std::path::Component::Normal(digest), std::path::Component::Normal(bin), std::path::Component::Normal(executable)]
        if digest.to_str().is_some_and(digest_valid) && *bin == "bin" && *executable == "spaceterm")
}

fn bin(home: &Path) -> Result<PathBuf, ServerError> {
    owned_directory(home)?;
    let mut path = home.to_path_buf();
    for name in [".local", "bin"] {
        path.push(name);
        match std::fs::DirBuilder::new().mode(0o755).create(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(ServerError::StorageUnavailable),
        }
        owned_directory(&path)?;
    }
    Ok(path)
}

/// A passive read for presentation. Replacement decides again under the service lock.
pub(super) fn inspect(
    home: &Path,
    identity: &str,
    installations: &Path,
) -> Result<CliLauncher, ServerError> {
    let launcher = home.join(".local/bin").join(identity);
    match std::fs::read_link(&launcher) {
        Ok(target) if managed(&target, installations) => Ok(if target.is_file() {
            CliLauncher::Linked
        } else {
            CliLauncher::Stale
        }),
        Ok(_) => Ok(CliLauncher::Foreign),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(CliLauncher::Missing),
        Err(_) if std::fs::symlink_metadata(&launcher).is_ok() => Ok(CliLauncher::NotLink),
        Err(_) => Err(ServerError::StorageUnavailable),
    }
}

/// Called only with the service lock held. Returns false for a launcher owned by another tool.
///
/// The service lock orders SpaceTerm's own writers only, so publication never replaces an entry
/// another program creates meanwhile: a missing launcher is created exclusively, and a managed
/// one is exchanged atomically and put back if the exchange displaced anything else.
pub(super) fn select(
    home: &Path,
    identity: &str,
    installations: &Path,
    runtime: &Path,
) -> Result<bool, ServerError> {
    let parent = bin(home)?;
    let launcher = parent.join(identity);
    let before = match std::fs::read_link(&launcher) {
        Ok(target) if managed(&target, installations) => Some(target),
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) if std::fs::symlink_metadata(&launcher).is_ok() => return Ok(false),
        Err(_) => return Err(ServerError::StorageUnavailable),
    };
    let target = runtime.join("bin/spaceterm");
    if !managed(&target, installations) {
        return Err(ServerError::UnsafeStorage);
    }
    if before.as_ref() == Some(&target) {
        return Ok(true);
    }
    let directory = owned_directory(&parent)?;
    let published = match before {
        None => {
            before_publication(&launcher);
            match symlink(&target, &launcher) {
                Ok(()) => true,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
                Err(_) => return Err(ServerError::StorageUnavailable),
            }
        }
        Some(previous) => {
            let name = format!(".launcher-{}", digest(&crate::protocol::random()?));
            let temporary = parent.join(&name);
            symlink(&target, &temporary).map_err(|_| ServerError::StorageUnavailable)?;
            let result = replace(
                &directory, &name, identity, &temporary, &launcher, &previous,
            );
            // Only SpaceTerm's own links are ever removed: a failed exchange back could leave
            // another program's entry under the temporary name.
            if std::fs::read_link(&temporary)
                .is_ok_and(|linked| linked == target || linked == previous)
            {
                let _ = std::fs::remove_file(&temporary);
            }
            result?
        }
    };
    if published {
        directory
            .sync_all()
            .map_err(|_| ServerError::StorageUnavailable)?;
    }
    Ok(published)
}

/// Exchanges the new link at `name` with the launcher. Afterwards `name` holds what the
/// launcher held, which must be the managed link `previous`; anything else goes back.
fn replace(
    directory: &File,
    name: &str,
    identity: &str,
    temporary: &Path,
    launcher: &Path,
    previous: &Path,
) -> Result<bool, ServerError> {
    let name = CString::new(name).map_err(|_| ServerError::UnsafeStorage)?;
    let launcher_name = CString::new(identity).map_err(|_| ServerError::UnsafeStorage)?;
    before_publication(launcher);
    match rename::exchange_at(directory, &name, &launcher_name) {
        Ok(()) => {}
        // The managed link went away meanwhile, so publish only into an empty path.
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
            return match rename::rename_noreplace_at(directory, &name, &launcher_name) {
                Ok(()) => Ok(true),
                Err(error) if error.raw_os_error() == Some(libc::EEXIST) => Ok(false),
                Err(_) => Err(ServerError::StorageUnavailable),
            };
        }
        Err(_) => return Err(ServerError::StorageUnavailable),
    }
    if std::fs::read_link(temporary).is_ok_and(|displaced| displaced == previous) {
        return Ok(true);
    }
    rename::exchange_at(directory, &name, &launcher_name)
        .map_err(|_| ServerError::StorageUnavailable)?;
    Ok(false)
}

/// Same-directory renames that never replace an entry they didn't expect.
mod rename {
    use std::ffi::CStr;
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    #[cfg(target_os = "macos")]
    const EXCHANGE: libc::c_uint = libc::RENAME_SWAP;
    #[cfg(target_os = "macos")]
    const NO_REPLACE: libc::c_uint = libc::RENAME_EXCL;
    #[cfg(target_os = "linux")]
    const EXCHANGE: libc::c_uint = libc::RENAME_EXCHANGE;
    #[cfg(target_os = "linux")]
    const NO_REPLACE: libc::c_uint = libc::RENAME_NOREPLACE;

    pub(super) fn exchange_at(directory: &File, first: &CStr, second: &CStr) -> io::Result<()> {
        rename_at(directory, first, second, EXCHANGE)
    }

    pub(super) fn rename_noreplace_at(directory: &File, from: &CStr, to: &CStr) -> io::Result<()> {
        rename_at(directory, from, to, NO_REPLACE)
    }

    fn rename_at(directory: &File, from: &CStr, to: &CStr, flags: libc::c_uint) -> io::Result<()> {
        let directory = directory.as_raw_fd();
        // SAFETY: the descriptor and both NUL-terminated names remain valid for this call.
        #[cfg(target_os = "macos")]
        let result =
            unsafe { libc::renameatx_np(directory, from.as_ptr(), directory, to.as_ptr(), flags) };
        // SAFETY: the descriptor and both NUL-terminated names remain valid for this call.
        #[cfg(target_os = "linux")]
        let result =
            unsafe { libc::renameat2(directory, from.as_ptr(), directory, to.as_ptr(), flags) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Runs on the launcher path immediately before publication, as another program could.
    static BEFORE_PUBLICATION: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
}

fn before_publication(_launcher: &Path) {
    #[cfg(test)]
    if let Some(intrude) = BEFORE_PUBLICATION.take() {
        intrude(_launcher);
    }
}

/// Remove only a link into this identity's retained installations, under the service lock.
#[cfg(any(feature = "client", test))]
pub(super) fn remove(home: &Path, identity: &str, installations: &Path) -> Result<(), ServerError> {
    let parent = home.join(".local/bin");
    if !parent.exists() {
        return Ok(());
    }
    let launcher = parent.join(identity);
    if !std::fs::read_link(&launcher).is_ok_and(|target| managed(&target, installations)) {
        return Ok(());
    }
    let directory = owned_directory(&parent)?;
    std::fs::remove_file(&launcher).map_err(|_| ServerError::StorageUnavailable)?;
    directory
        .sync_all()
        .map_err(|_| ServerError::StorageUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_selects_only_its_managed_link_and_uninstall_preserves_other_launchers() {
        let home = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("st-launcher-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let installations = home.join(".local/share/spaceterm-development/server/installations");
        let first = installations.join("a".repeat(64));
        let second = installations.join("b".repeat(64));
        let path = home.join(".local/bin/spaceterm-development");
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(home.clone());
        assert!(select(&home, "spaceterm-development", &installations, &first).unwrap());
        assert_eq!(
            std::fs::read_link(&path).unwrap(),
            first.join("bin/spaceterm")
        );
        assert!(select(&home, "spaceterm-development", &installations, &second).unwrap());
        assert_eq!(
            std::fs::read_link(&path).unwrap(),
            second.join("bin/spaceterm")
        );
        remove(&home, "spaceterm-development", &installations).unwrap();
        assert!(!path.is_symlink());
        symlink("/another/tool", &path).unwrap();
        assert!(!select(&home, "spaceterm-development", &installations, &first).unwrap());
        remove(&home, "spaceterm-development", &installations).unwrap();
        assert_eq!(
            std::fs::read_link(&path).unwrap(),
            Path::new("/another/tool")
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"local tool").unwrap();
        assert!(!select(&home, "spaceterm-development", &installations, &first).unwrap());
        remove(&home, "spaceterm-development", &installations).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"local tool");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            path.parent().unwrap(),
            std::fs::Permissions::from_mode(0o775),
        )
        .unwrap();
        remove(&home, "spaceterm-development", &installations).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"local tool");
        std::fs::remove_file(&path).unwrap();
        remove(&home, "spaceterm-development", &installations).unwrap();
    }

    #[test]
    fn an_entry_another_program_creates_just_before_publication_is_kept() {
        let home = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("st-launcher-race-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(home.clone());
        let installations = home.join(".local/share/spaceterm-development/server/installations");
        let first = installations.join("a".repeat(64));
        let second = installations.join("b".repeat(64));
        let path = home.join(".local/bin/spaceterm-development");
        let bin = path.parent().unwrap().to_owned();
        let leftovers = || {
            std::fs::read_dir(&bin)
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".launcher-")
                })
                .count()
        };

        // A missing launcher: the other program's file wins and stays.
        BEFORE_PUBLICATION.set(Some(|launcher| {
            std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
            std::fs::write(launcher, b"local tool").unwrap();
        }));
        assert!(!select(&home, "spaceterm-development", &installations, &first).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"local tool");

        // A managed launcher replaced by another program's link just before the exchange.
        std::fs::remove_file(&path).unwrap();
        assert!(select(&home, "spaceterm-development", &installations, &first).unwrap());
        BEFORE_PUBLICATION.set(Some(|launcher| {
            std::fs::remove_file(launcher).unwrap();
            symlink("/another/tool", launcher).unwrap();
        }));
        assert!(!select(&home, "spaceterm-development", &installations, &second).unwrap());
        assert_eq!(
            std::fs::read_link(&path).unwrap(),
            Path::new("/another/tool")
        );
        assert_eq!(leftovers(), 0);

        // A managed launcher replaced by another program's file just before the exchange.
        std::fs::remove_file(&path).unwrap();
        assert!(select(&home, "spaceterm-development", &installations, &first).unwrap());
        BEFORE_PUBLICATION.set(Some(|launcher| {
            std::fs::remove_file(launcher).unwrap();
            std::fs::write(launcher, b"local tool").unwrap();
        }));
        assert!(!select(&home, "spaceterm-development", &installations, &second).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"local tool");
        assert_eq!(leftovers(), 0);

        // A managed launcher removed just before the exchange is published into the empty path.
        std::fs::remove_file(&path).unwrap();
        assert!(select(&home, "spaceterm-development", &installations, &first).unwrap());
        BEFORE_PUBLICATION.set(Some(|launcher| std::fs::remove_file(launcher).unwrap()));
        assert!(select(&home, "spaceterm-development", &installations, &second).unwrap());
        assert_eq!(
            std::fs::read_link(&path).unwrap(),
            second.join("bin/spaceterm")
        );
        assert_eq!(leftovers(), 0);
    }
}
