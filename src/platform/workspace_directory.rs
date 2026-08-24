use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::domain::{ValidatedWorkspaceDirectory, WorkspaceDirectoryIdentity};

#[derive(Debug, Error)]
pub(crate) enum WorkspaceDirectoryError {
    #[error("the path is not absolute")]
    NotAbsolute,
    #[error("the path is unavailable: {0}")]
    Unavailable(#[source] io::Error),
    #[error("the path is not a directory")]
    NotDirectory,
    #[error("the directory is not readable: {0}")]
    Unreadable(#[source] io::Error),
    #[error("the path no longer identifies the selected directory")]
    IdentityChanged,
}

pub(crate) fn validate_workspace_directory(
    path: &Path,
) -> Result<ValidatedWorkspaceDirectory, WorkspaceDirectoryError> {
    if !path.is_absolute() {
        return Err(WorkspaceDirectoryError::NotAbsolute);
    }
    let metadata = fs::metadata(path).map_err(WorkspaceDirectoryError::Unavailable)?;
    if !metadata.is_dir() {
        return Err(WorkspaceDirectoryError::NotDirectory);
    }
    fs::read_dir(path).map_err(WorkspaceDirectoryError::Unreadable)?;
    Ok(ValidatedWorkspaceDirectory::new(
        PathBuf::from(path),
        WorkspaceDirectoryIdentity::new(metadata.dev(), metadata.ino()),
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temporary_directory(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("spaceterm-workspace-{name}-{nonce}"))
    }

    #[test]
    fn validation_should_preserve_selected_symlink_path_and_resolve_target_identity() {
        let root = temporary_directory("symlink");
        let target = root.join("target");
        let selected = root.join("selected");
        fs::create_dir_all(&target).unwrap();
        symlink(&target, &selected).unwrap();

        let selected_directory = validate_workspace_directory(&selected).unwrap();
        let target_directory = validate_workspace_directory(&target).unwrap();

        assert_eq!(
            (selected_directory.path(), selected_directory.identity()),
            (selected.as_path(), target_directory.identity())
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validation_should_reject_a_regular_file() {
        let root = temporary_directory("file");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("not-a-directory");
        fs::write(&file, b"test").unwrap();

        let result = validate_workspace_directory(&file);

        assert!(matches!(result, Err(WorkspaceDirectoryError::NotDirectory)));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validation_should_reject_relative_and_missing_paths() {
        assert!(matches!(
            validate_workspace_directory(Path::new("relative")),
            Err(WorkspaceDirectoryError::NotAbsolute)
        ));
        let missing = temporary_directory("missing");
        assert!(matches!(
            validate_workspace_directory(&missing),
            Err(WorkspaceDirectoryError::Unavailable(_))
        ));
    }
}
