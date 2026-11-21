//! The account launcher follows an approved runtime. Other local launchers remain untouched.
use super::*;
use std::os::unix::fs::{DirBuilderExt, symlink};

/// Whether `target` is the executable of one of `identity`'s retained installations.
fn managed(target: &Path, identity: &str, installations: &Path) -> bool {
    let Some(executable) = Layout::host(identity).map(Layout::executable) else {
        return false;
    };
    let Ok(relative) = target.strip_prefix(installations) else {
        return false;
    };
    let mut parts = relative.components();
    matches!(parts.next(), Some(std::path::Component::Normal(digest)) if digest.to_str().is_some_and(digest_valid))
        && parts.as_path() == executable
}

fn bin(home: &Path) -> Result<PathBuf, CoreError> {
    owned_directory(home)?;
    let mut path = home.to_path_buf();
    for name in [".local", "bin"] {
        path.push(name);
        match std::fs::DirBuilder::new().mode(0o755).create(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(CoreError::StorageUnavailable),
        }
        owned_directory(&path)?;
    }
    Ok(path)
}

/// A passive read for presentation. Replacement decides again under the service lock.
/// `approved` is the approved runtime, when one is known.
pub(super) fn inspect(
    home: &Path,
    identity: &str,
    installations: &Path,
    approved: Option<&Path>,
) -> Result<CliLauncher, CoreError> {
    let launcher = home.join(".local/bin").join(identity);
    // A replacement needs the temporary name, which another program's entry may hold.
    let blocked = || {
        let temporary = temporary(&home.join(".local/bin"), identity);
        std::fs::symlink_metadata(&temporary).is_ok()
            && !std::fs::read_link(&temporary)
                .is_ok_and(|target| managed(&target, identity, installations))
    };
    match std::fs::read_link(&launcher) {
        Ok(target) if managed(&target, identity, installations) => Ok(
            if target.is_file()
                && approved.is_none_or(|runtime| {
                    Layout::host(identity)
                        .is_some_and(|layout| target == runtime.join(layout.executable()))
                })
            {
                CliLauncher::Linked
            } else if blocked() {
                CliLauncher::Blocked
            } else if !target.is_file() {
                CliLauncher::Stale
            } else {
                CliLauncher::Outdated
            },
        ),
        Ok(_) => Ok(CliLauncher::Foreign),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(CliLauncher::Missing),
        Err(_) if std::fs::symlink_metadata(&launcher).is_ok() => Ok(CliLauncher::NotLink),
        Err(_) => Err(CoreError::StorageUnavailable),
    }
}

/// Called only with the service lock held. Another tool's launcher is kept, and another tool's
/// entry at the temporary name blocks a replacement; neither is touched.
///
/// The service lock orders only SpaceTerm's own writers. A missing launcher is created
/// exclusively, so an entry another program creates first stays. A managed launcher is
/// replaced by renaming a new link over it from [`temporary`]; another program that writes this
/// exact path between the check and the rename loses its entry, because POSIX has no atomic
/// compare-and-replace.
pub(super) fn select(
    home: &Path,
    identity: &str,
    installations: &Path,
    runtime: &Path,
) -> Result<CliLink, CoreError> {
    let parent = bin(home)?;
    let launcher = parent.join(identity);
    let temporary = temporary(&parent, identity);
    remove_leftover(&temporary, identity, installations)?;
    let before = match std::fs::read_link(&launcher) {
        Ok(target) if managed(&target, identity, installations) => Some(target),
        Ok(_) => return Ok(CliLink::Kept),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) if std::fs::symlink_metadata(&launcher).is_ok() => return Ok(CliLink::Kept),
        Err(_) => return Err(CoreError::StorageUnavailable),
    };
    let target = runtime.join(
        Layout::host(identity)
            .ok_or(CoreError::UnsafeStorage)?
            .executable(),
    );
    if !managed(&target, identity, installations) {
        return Err(CoreError::UnsafeStorage);
    }
    if before.as_ref() == Some(&target) {
        return Ok(CliLink::Linked);
    }
    let directory = owned_directory(&parent)?;
    if before.is_none() {
        before_creation(&launcher);
        match symlink(&target, &launcher) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Ok(CliLink::Kept);
            }
            Err(_) => return Err(CoreError::StorageUnavailable),
        }
    } else {
        match symlink(&target, &temporary) {
            Ok(()) => {}
            // Another program's entry under the temporary name stays.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Ok(CliLink::Blocked);
            }
            Err(_) => return Err(CoreError::StorageUnavailable),
        }
        before_rename(&temporary);
        if std::fs::rename(&temporary, &launcher).is_err() {
            // The temporary name is this call's own new link.
            let _ = std::fs::remove_file(&temporary);
            return Err(CoreError::StorageUnavailable);
        }
    }
    directory
        .sync_all()
        .map_err(|_| CoreError::StorageUnavailable)?;
    Ok(CliLink::Linked)
}

/// The one name a replacement's new link takes before the rename, so an interrupted
/// replacement leaves at most one leftover per identity.
pub(super) fn temporary(parent: &Path, identity: &str) -> PathBuf {
    parent.join(format!(".launcher-{identity}"))
}

/// Removes a replacement's leftover new link. Only a link into this identity's installations is
/// SpaceTerm's own; anything else under that name stays.
fn remove_leftover(
    temporary: &Path,
    identity: &str,
    installations: &Path,
) -> Result<(), CoreError> {
    if std::fs::read_link(temporary).is_ok_and(|target| managed(&target, identity, installations)) {
        std::fs::remove_file(temporary).map_err(|_| CoreError::StorageUnavailable)?;
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// Runs on the launcher path just before exclusive creation, as another program could.
    static BEFORE_CREATION: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
    /// Runs on the temporary link just before the rename. A panic here interrupts the
    /// replacement as a crash would.
    static BEFORE_RENAME: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
    /// Runs on the launcher path after removal found SpaceTerm's link there, as another
    /// program could.
    static BEFORE_REMOVAL: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
}

fn before_creation(_launcher: &Path) {
    #[cfg(test)]
    if let Some(intrude) = BEFORE_CREATION.take() {
        intrude(_launcher);
    }
}

fn before_rename(_temporary: &Path) {
    #[cfg(test)]
    if let Some(interrupt) = BEFORE_RENAME.take() {
        interrupt(_temporary);
    }
}

fn before_removal(_launcher: &Path) {
    #[cfg(test)]
    if let Some(intrude) = BEFORE_REMOVAL.take() {
        intrude(_launcher);
    }
}

/// The one name a removal moves the launcher to before it decides, so it deletes only the entry
/// it checked.
fn removing(identity: &str) -> String {
    format!(".launcher-{identity}-removing")
}

/// Renames `from` to `to` inside `directory`, unless an entry already holds `to`.
fn rename_exclusive(directory: &File, from: &str, to: &str) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let invalid = |_| std::io::Error::from(std::io::ErrorKind::InvalidInput);
    let from = std::ffi::CString::new(from).map_err(invalid)?;
    let to = std::ffi::CString::new(to).map_err(invalid)?;
    let fd = directory.as_raw_fd();
    #[cfg(target_os = "linux")]
    // SAFETY: renameat2 acts on two names beneath the retained directory, refusing replacement.
    let result =
        unsafe { libc::renameat2(fd, from.as_ptr(), fd, to.as_ptr(), libc::RENAME_NOREPLACE) };
    #[cfg(target_os = "macos")]
    // SAFETY: renameatx_np acts on two names beneath the retained directory, refusing replacement.
    let result =
        unsafe { libc::renameatx_np(fd, from.as_ptr(), fd, to.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Remove only a link into this identity's retained installations, under the service lock.
///
/// The service lock orders only SpaceTerm's own writers, so the launcher first moves to
/// [`removing`] without replacing anything there, and only a managed link found under that name
/// is deleted. Another program's entry that took the launcher path after the check moves back.
/// If yet another entry took the path meanwhile, the moved entry stays under the removal name
/// and the removal fails. The SpaceTerm-named temporary and removal names carry the window
/// [`select`] accepts for the launcher path.
pub(in crate::service) fn remove(
    home: &Path,
    identity: &str,
    installations: &Path,
) -> Result<(), CoreError> {
    let parent = home.join(".local/bin");
    if !parent.exists() {
        return Ok(());
    }
    let launcher = parent.join(identity);
    let leftover = temporary(&parent, identity);
    let removal = removing(identity);
    let moved = parent.join(&removal);
    let ours = |path: &Path| {
        std::fs::read_link(path).is_ok_and(|target| managed(&target, identity, installations))
    };
    if !ours(&launcher) && !ours(&leftover) && !ours(&moved) {
        return Ok(());
    }
    let directory = owned_directory(&parent)?;
    remove_leftover(&leftover, identity, installations)?;
    // An interrupted removal's own link.
    remove_leftover(&moved, identity, installations)?;
    if ours(&launcher) {
        before_removal(&launcher);
        match rename_exclusive(&directory, identity, &removal) {
            Ok(()) if ours(&moved) => {
                std::fs::remove_file(&moved).map_err(|_| CoreError::StorageUnavailable)?;
            }
            Ok(()) => rename_exclusive(&directory, &removal, identity)
                .map_err(|_| CoreError::StorageUnavailable)?,
            // Gone meanwhile.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(CoreError::StorageUnavailable),
        }
    }
    directory
        .sync_all()
        .map_err(|_| CoreError::StorageUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable() -> PathBuf {
        Layout::host("spaceterm-development").unwrap().executable()
    }

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
        let installations = home.join(".local/share/spaceterm-development/core/installations");
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
        assert_eq!(
            select(&home, "spaceterm-development", &installations, &first),
            Ok(CliLink::Linked)
        );
        assert_eq!(std::fs::read_link(&path).unwrap(), first.join(executable()));
        assert_eq!(
            select(&home, "spaceterm-development", &installations, &second),
            Ok(CliLink::Linked)
        );
        assert_eq!(
            std::fs::read_link(&path).unwrap(),
            second.join(executable())
        );
        remove(&home, "spaceterm-development", &installations).unwrap();
        assert!(!path.is_symlink());
        symlink("/another/tool", &path).unwrap();
        assert_eq!(
            select(&home, "spaceterm-development", &installations, &first),
            Ok(CliLink::Kept)
        );
        remove(&home, "spaceterm-development", &installations).unwrap();
        assert_eq!(
            std::fs::read_link(&path).unwrap(),
            Path::new("/another/tool")
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"local tool").unwrap();
        assert_eq!(
            select(&home, "spaceterm-development", &installations, &first),
            Ok(CliLink::Kept)
        );
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
    fn an_entry_another_program_creates_at_a_missing_launcher_is_kept() {
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
        let installations = home.join(".local/share/spaceterm-development/core/installations");
        let path = home.join(".local/bin/spaceterm-development");
        BEFORE_CREATION.set(Some(|launcher| {
            std::fs::write(launcher, b"local tool").unwrap();
        }));
        assert_eq!(
            select(
                &home,
                "spaceterm-development",
                &installations,
                &installations.join("a".repeat(64))
            ),
            Ok(CliLink::Kept)
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"local tool");
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn removal_keeps_an_entry_another_program_writes_after_the_check() {
        let home = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("st-launcher-removal-{}", std::process::id()));
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
        let installations = home.join(".local/share/spaceterm-development/core/installations");
        let runtime = installations.join("a".repeat(64));
        let path = home.join(".local/bin/spaceterm-development");
        assert_eq!(
            select(&home, "spaceterm-development", &installations, &runtime),
            Ok(CliLink::Linked)
        );
        // Another program replaces SpaceTerm's link after removal found it managed.
        BEFORE_REMOVAL.set(Some(|launcher| {
            std::fs::remove_file(launcher).unwrap();
            std::fs::write(launcher, b"local tool").unwrap();
        }));
        remove(&home, "spaceterm-development", &installations).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"local tool");
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );

        // A removal interrupted after its move leaves SpaceTerm's link under the removal name,
        // which the next removal deletes.
        std::fs::remove_file(&path).unwrap();
        let interrupted = path.with_file_name(".launcher-spaceterm-development-removing");
        symlink(runtime.join(executable()), &interrupted).unwrap();
        remove(&home, "spaceterm-development", &installations).unwrap();
        assert!(!interrupted.is_symlink());
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            0
        );
    }

    #[test]
    fn an_interrupted_replacement_leaves_one_leftover_that_the_next_run_and_uninstall_remove() {
        let home = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("st-launcher-leftover-{}", std::process::id()));
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
        let identity = "spaceterm-development";
        let installations = home.join(".local/share/spaceterm-development/core/installations");
        let first = installations.join("a".repeat(64));
        let second = installations.join("b".repeat(64));
        let path = home.join(".local/bin").join(identity);
        let leftover = home.join(".local/bin/.launcher-spaceterm-development");
        let entries = || std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(
            select(&home, identity, &installations, &first),
            Ok(CliLink::Linked)
        );

        // Interrupt two replacements after their new link exists, as a crash would.
        for _ in 0..2 {
            let (home, installations, runtime) =
                (home.clone(), installations.clone(), second.clone());
            let interrupted = std::thread::spawn(move || {
                BEFORE_RENAME.set(Some(|_| panic!("interrupted")));
                select(&home, identity, &installations, &runtime)
            })
            .join();
            assert!(interrupted.is_err());
            assert_eq!(std::fs::read_link(&path).unwrap(), first.join(executable()));
            assert_eq!(
                std::fs::read_link(&leftover).unwrap(),
                second.join(executable())
            );
            assert_eq!(entries(), 2);
        }

        // The next replacement removes the leftover first.
        assert_eq!(
            select(&home, identity, &installations, &second),
            Ok(CliLink::Linked)
        );
        assert_eq!(
            std::fs::read_link(&path).unwrap(),
            second.join(executable())
        );
        assert_eq!(entries(), 1);

        // Uninstall removes a leftover with the launcher.
        symlink(first.join(executable()), &leftover).unwrap();
        remove(&home, identity, &installations).unwrap();
        assert_eq!(entries(), 0);

        // Another program's entry under that name is never removed. It blocks the replacement,
        // and the launcher reads as blocked so a client can show that entry.
        std::fs::write(&leftover, b"local tool").unwrap();
        assert_eq!(
            select(&home, identity, &installations, &first),
            Ok(CliLink::Linked)
        );
        assert_eq!(
            select(&home, identity, &installations, &second),
            Ok(CliLink::Blocked)
        );
        assert_eq!(std::fs::read(&leftover).unwrap(), b"local tool");
        assert_eq!(
            inspect(&home, identity, &installations, Some(&second)),
            Ok(CliLauncher::Blocked)
        );
        remove(&home, identity, &installations).unwrap();
        assert_eq!(std::fs::read(&leftover).unwrap(), b"local tool");
        assert!(!path.exists());
    }
}
