//! The account launcher follows an approved runtime. Other local launchers remain untouched.
use super::*;
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
/// `approved` is the approved runtime, when one is known.
pub(super) fn inspect(
    home: &Path,
    identity: &str,
    installations: &Path,
    approved: Option<&Path>,
) -> Result<CliLauncher, ServerError> {
    let launcher = home.join(".local/bin").join(identity);
    match std::fs::read_link(&launcher) {
        Ok(target) if managed(&target, installations) => Ok(if !target.is_file() {
            CliLauncher::Stale
        } else if approved.is_some_and(|runtime| target != runtime.join("bin/spaceterm")) {
            CliLauncher::Outdated
        } else {
            CliLauncher::Linked
        }),
        Ok(_) => Ok(CliLauncher::Foreign),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(CliLauncher::Missing),
        Err(_) if std::fs::symlink_metadata(&launcher).is_ok() => Ok(CliLauncher::NotLink),
        Err(_) => Err(ServerError::StorageUnavailable),
    }
}

/// Called only with the service lock held. Returns false for a launcher owned by another tool.
///
/// The service lock orders only SpaceTerm's own writers. A missing launcher is created
/// exclusively, so an entry another program creates first stays. A managed launcher is
/// replaced by renaming a new link over it; another program that writes this exact path
/// between the check and the rename loses its entry, because POSIX has no atomic
/// compare-and-replace.
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
    if before.is_none() {
        before_creation(&launcher);
        match symlink(&target, &launcher) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(_) => return Err(ServerError::StorageUnavailable),
        }
    } else {
        let temporary = parent.join(format!(".launcher-{}", digest(&crate::protocol::random()?)));
        symlink(&target, &temporary).map_err(|_| ServerError::StorageUnavailable)?;
        if std::fs::rename(&temporary, &launcher).is_err() {
            // The temporary name is this call's own new link.
            let _ = std::fs::remove_file(&temporary);
            return Err(ServerError::StorageUnavailable);
        }
    }
    directory
        .sync_all()
        .map_err(|_| ServerError::StorageUnavailable)?;
    Ok(true)
}

#[cfg(test)]
thread_local! {
    /// Runs on the launcher path just before exclusive creation, as another program could.
    static BEFORE_CREATION: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
}

fn before_creation(_launcher: &Path) {
    #[cfg(test)]
    if let Some(intrude) = BEFORE_CREATION.take() {
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
        assert!(
            !select(
                &home,
                "spaceterm-development",
                &installations,
                &installations.join("a".repeat(64))
            )
            .unwrap()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"local tool");
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }
}
