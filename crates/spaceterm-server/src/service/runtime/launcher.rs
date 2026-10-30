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
    let temporary = parent.join(format!(".launcher-{}", digest(&crate::protocol::random()?)));
    symlink(&target, &temporary).map_err(|_| ServerError::StorageUnavailable)?;
    let result = (|| {
        if std::fs::read_link(&launcher).ok() != before
            || (before.is_none() && std::fs::symlink_metadata(&launcher).is_ok())
        {
            return Err(ServerError::Changed);
        }
        std::fs::rename(&temporary, &launcher).map_err(|_| ServerError::StorageUnavailable)?;
        owned_directory(&parent)?
            .sync_all()
            .map_err(|_| ServerError::StorageUnavailable)
    })();
    let _ = std::fs::remove_file(temporary);
    result.map(|()| true)
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
}
