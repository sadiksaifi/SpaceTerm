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
/// `approved` is the approved runtime, when one is known.
pub(super) fn inspect(
    home: &Path,
    identity: &str,
    installations: &Path,
    approved: Option<&Path>,
) -> Result<CliLauncher, ServerError> {
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
        Err(_) => Err(ServerError::StorageUnavailable),
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
) -> Result<CliLink, ServerError> {
    let parent = bin(home)?;
    let launcher = parent.join(identity);
    let temporary = temporary(&parent, identity);
    remove_leftover(&temporary, identity, installations)?;
    let before = match std::fs::read_link(&launcher) {
        Ok(target) if managed(&target, identity, installations) => Some(target),
        Ok(_) => return Ok(CliLink::Kept),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) if std::fs::symlink_metadata(&launcher).is_ok() => return Ok(CliLink::Kept),
        Err(_) => return Err(ServerError::StorageUnavailable),
    };
    let target = runtime.join(
        Layout::host(identity)
            .ok_or(ServerError::UnsafeStorage)?
            .executable(),
    );
    if !managed(&target, identity, installations) {
        return Err(ServerError::UnsafeStorage);
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
            Err(_) => return Err(ServerError::StorageUnavailable),
        }
    } else {
        match symlink(&target, &temporary) {
            Ok(()) => {}
            // Another program's entry under the temporary name stays.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Ok(CliLink::Blocked);
            }
            Err(_) => return Err(ServerError::StorageUnavailable),
        }
        before_rename(&temporary);
        if std::fs::rename(&temporary, &launcher).is_err() {
            // The temporary name is this call's own new link.
            let _ = std::fs::remove_file(&temporary);
            return Err(ServerError::StorageUnavailable);
        }
    }
    directory
        .sync_all()
        .map_err(|_| ServerError::StorageUnavailable)?;
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
) -> Result<(), ServerError> {
    if std::fs::read_link(temporary).is_ok_and(|target| managed(&target, identity, installations)) {
        std::fs::remove_file(temporary).map_err(|_| ServerError::StorageUnavailable)?;
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

/// Remove only a link into this identity's retained installations, under the service lock.
#[cfg(any(feature = "client", test))]
pub(super) fn remove(home: &Path, identity: &str, installations: &Path) -> Result<(), ServerError> {
    let parent = home.join(".local/bin");
    if !parent.exists() {
        return Ok(());
    }
    let launcher = parent.join(identity);
    let leftover = temporary(&parent, identity);
    let managed_launcher =
        std::fs::read_link(&launcher).is_ok_and(|target| managed(&target, identity, installations));
    let managed_leftover =
        std::fs::read_link(&leftover).is_ok_and(|target| managed(&target, identity, installations));
    if !managed_launcher && !managed_leftover {
        return Ok(());
    }
    let directory = owned_directory(&parent)?;
    remove_leftover(&leftover, identity, installations)?;
    if managed_launcher {
        std::fs::remove_file(&launcher).map_err(|_| ServerError::StorageUnavailable)?;
    }
    directory
        .sync_all()
        .map_err(|_| ServerError::StorageUnavailable)
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
        let installations = home.join(".local/share/spaceterm-development/server/installations");
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
        let installations = home.join(".local/share/spaceterm-development/server/installations");
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
