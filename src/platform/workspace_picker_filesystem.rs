use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::workspace_directory::{
    WorkspaceDirectoryError, validate_workspace_directory as validate_existing_workspace_directory,
};
use crate::domain::ValidatedWorkspaceDirectory;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspacePickerDirectoryEntry {
    name: String,
    path: PathBuf,
}

impl WorkspacePickerDirectoryEntry {
    pub(crate) fn new(name: String, path: PathBuf) -> Self {
        Self { name, path }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum WorkspacePickerFilesystemError {
    #[error("permission denied")]
    PermissionDenied,
    #[error("path is missing")]
    Missing,
    #[error("path is not a directory")]
    NotDirectory,
    #[error("filesystem operation failed")]
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspacePickerExactPathProbe {
    ReadableDirectory,
    Unavailable(WorkspacePickerFilesystemError),
}

pub(crate) trait WorkspacePickerFilesystem: Send + Sync {
    fn list_directories(
        &self,
        directory: &Path,
        hide_dot_prefixed: bool,
    ) -> Result<Vec<WorkspacePickerDirectoryEntry>, WorkspacePickerFilesystemError>;

    fn probe_exact_path(&self, path: &Path) -> WorkspacePickerExactPathProbe;

    fn create_dir_all(&self, path: &Path) -> Result<(), WorkspacePickerFilesystemError>;

    fn validate_workspace_directory(
        &self,
        path: &Path,
    ) -> Result<ValidatedWorkspaceDirectory, WorkspacePickerFilesystemError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NativeWorkspacePickerFilesystem;

impl WorkspacePickerFilesystem for NativeWorkspacePickerFilesystem {
    fn list_directories(
        &self,
        directory: &Path,
        hide_dot_prefixed: bool,
    ) -> Result<Vec<WorkspacePickerDirectoryEntry>, WorkspacePickerFilesystemError> {
        let entries = fs::read_dir(directory).map_err(|error| {
            if error.raw_os_error() == Some(libc::ENOTDIR) {
                WorkspacePickerFilesystemError::NotDirectory
            } else {
                classify_io_error(&error)
            }
        })?;
        let mut directories = Vec::new();

        for entry in entries {
            let entry = entry.map_err(|error| classify_io_error(&error))?;
            let Some(name) = visible_entry_name(entry.file_name(), hide_dot_prefixed) else {
                continue;
            };

            let path = entry.path();
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error)
                    if classify_io_error(&error) == WorkspacePickerFilesystemError::Missing =>
                {
                    continue;
                }
                Err(error) => return Err(classify_io_error(&error)),
            };
            if metadata.is_dir() {
                directories.push(WorkspacePickerDirectoryEntry::new(name, path));
            }
        }

        Ok(directories)
    }

    fn probe_exact_path(&self, path: &Path) -> WorkspacePickerExactPathProbe {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) => {
                return WorkspacePickerExactPathProbe::Unavailable(classify_io_error(&error));
            }
        };
        if !metadata.is_dir() {
            return WorkspacePickerExactPathProbe::Unavailable(
                WorkspacePickerFilesystemError::NotDirectory,
            );
        }

        match fs::read_dir(path) {
            Ok(_) => WorkspacePickerExactPathProbe::ReadableDirectory,
            Err(error) => WorkspacePickerExactPathProbe::Unavailable(classify_io_error(&error)),
        }
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), WorkspacePickerFilesystemError> {
        fs::create_dir_all(path).map_err(|error| {
            if fs::metadata(path).is_ok_and(|metadata| !metadata.is_dir()) {
                WorkspacePickerFilesystemError::NotDirectory
            } else {
                classify_io_error(&error)
            }
        })
    }

    fn validate_workspace_directory(
        &self,
        path: &Path,
    ) -> Result<ValidatedWorkspaceDirectory, WorkspacePickerFilesystemError> {
        validate_existing_workspace_directory(path).map_err(classify_workspace_directory_error)
    }
}

fn visible_entry_name(name: OsString, hide_dot_prefixed: bool) -> Option<String> {
    let name = name.into_string().ok()?;
    if hide_dot_prefixed && name.starts_with('.') {
        None
    } else {
        Some(name)
    }
}

fn classify_io_error(error: &io::Error) -> WorkspacePickerFilesystemError {
    match error.raw_os_error() {
        Some(libc::EPERM | libc::EACCES) => WorkspacePickerFilesystemError::PermissionDenied,
        Some(libc::ENOENT) => WorkspacePickerFilesystemError::Missing,
        _ => WorkspacePickerFilesystemError::Other,
    }
}

fn classify_workspace_directory_error(
    error: WorkspaceDirectoryError,
) -> WorkspacePickerFilesystemError {
    match error {
        WorkspaceDirectoryError::Unavailable(error)
        | WorkspaceDirectoryError::Unreadable(error) => classify_io_error(&error),
        WorkspaceDirectoryError::NotDirectory => WorkspacePickerFilesystemError::NotDirectory,
        WorkspaceDirectoryError::NotAbsolute | WorkspaceDirectoryError::IdentityChanged => {
            WorkspacePickerFilesystemError::Other
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::{MetadataExt, symlink};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::domain::WorkspaceDirectoryIdentity;

    static NEXT_TEMPORARY_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let sequence = NEXT_TEMPORARY_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "spaceterm-workspace-picker-{name}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn error_mapping_should_classify_eperm_as_permission_denied() {
        let error = io::Error::from_raw_os_error(libc::EPERM);

        let result = classify_io_error(&error);

        assert_eq!(result, WorkspacePickerFilesystemError::PermissionDenied);
    }

    #[test]
    fn error_mapping_should_classify_eacces_as_permission_denied() {
        let error = io::Error::from_raw_os_error(libc::EACCES);

        let result = classify_io_error(&error);

        assert_eq!(result, WorkspacePickerFilesystemError::PermissionDenied);
    }

    #[test]
    fn error_mapping_should_classify_enoent_as_missing() {
        let error = io::Error::from_raw_os_error(libc::ENOENT);

        let result = classify_io_error(&error);

        assert_eq!(result, WorkspacePickerFilesystemError::Missing);
    }

    #[test]
    fn error_mapping_should_classify_unrecognized_errors_as_other() {
        let error = io::Error::from_raw_os_error(libc::EIO);

        let result = classify_io_error(&error);

        assert_eq!(result, WorkspacePickerFilesystemError::Other);
    }

    #[test]
    fn exact_path_probe_should_classify_a_missing_path() {
        let root = TestDirectory::new("probe-missing");
        let filesystem = NativeWorkspacePickerFilesystem;

        let result = filesystem.probe_exact_path(&root.path.join("missing"));

        assert_eq!(
            result,
            WorkspacePickerExactPathProbe::Unavailable(WorkspacePickerFilesystemError::Missing)
        );
    }

    #[test]
    fn listing_should_classify_a_regular_file_as_not_directory() {
        let root = TestDirectory::new("list-file");
        let file = root.path.join("file");
        fs::write(&file, b"test").unwrap();
        let filesystem = NativeWorkspacePickerFilesystem;

        let result = filesystem.list_directories(&file, true);

        assert_eq!(result, Err(WorkspacePickerFilesystemError::NotDirectory));
    }

    #[test]
    fn exact_path_probe_should_classify_a_regular_file_as_not_directory() {
        let root = TestDirectory::new("probe-file");
        let file = root.path.join("file");
        fs::write(&file, b"test").unwrap();
        let filesystem = NativeWorkspacePickerFilesystem;

        let result = filesystem.probe_exact_path(&file);

        assert_eq!(
            result,
            WorkspacePickerExactPathProbe::Unavailable(
                WorkspacePickerFilesystemError::NotDirectory
            )
        );
    }

    #[test]
    fn listing_should_hide_dot_prefixed_names_before_metadata_probes() {
        let root = TestDirectory::new("hidden");
        let visible = root.path.join("visible");
        fs::create_dir(&visible).unwrap();
        symlink(root.path.join("missing-target"), root.path.join(".hidden")).unwrap();
        let filesystem = NativeWorkspacePickerFilesystem;

        let result = filesystem.list_directories(&root.path, true).unwrap();

        assert_eq!(
            result,
            vec![WorkspacePickerDirectoryEntry {
                name: String::from("visible"),
                path: visible,
            }]
        );
    }

    #[test]
    fn listing_should_include_dot_prefixed_directories_when_requested() {
        let root = TestDirectory::new("shown-hidden");
        let hidden = root.path.join(".hidden");
        fs::create_dir(&hidden).unwrap();
        let filesystem = NativeWorkspacePickerFilesystem;

        let result = filesystem.list_directories(&root.path, false).unwrap();

        assert_eq!(
            result,
            vec![WorkspacePickerDirectoryEntry {
                name: String::from(".hidden"),
                path: hidden,
            }]
        );
    }

    #[test]
    fn listing_should_omit_broken_visible_symlinks() {
        let root = TestDirectory::new("broken-symlink");
        symlink(root.path.join("missing-target"), root.path.join("broken")).unwrap();
        let filesystem = NativeWorkspacePickerFilesystem;

        let result = filesystem.list_directories(&root.path, true).unwrap();

        assert!(result.is_empty());
    }

    #[test]
    fn listing_should_follow_directory_symlinks_without_changing_their_spelling() {
        let root = TestDirectory::new("symlink");
        let target = root.path.join("target");
        let link = root.path.join("linked");
        fs::create_dir(&target).unwrap();
        symlink(&target, &link).unwrap();
        let filesystem = NativeWorkspacePickerFilesystem;

        let result = filesystem.list_directories(&root.path, true).unwrap();

        assert!(result.contains(&WorkspacePickerDirectoryEntry {
            name: String::from("linked"),
            path: link,
        }));
    }

    #[test]
    fn listing_should_treat_package_bundles_as_directories() {
        let root = TestDirectory::new("package");
        let package = root.path.join("Example.app");
        fs::create_dir(&package).unwrap();
        let filesystem = NativeWorkspacePickerFilesystem;

        let result = filesystem.list_directories(&root.path, true).unwrap();

        assert_eq!(
            result,
            vec![WorkspacePickerDirectoryEntry {
                name: String::from("Example.app"),
                path: package,
            }]
        );
    }

    #[test]
    fn listing_should_omit_non_utf8_names() {
        let invalid_name =
            std::ffi::OsString::from_vec(vec![b'i', b'n', b'v', b'a', b'l', b'i', b'd', 0xff]);

        let result = visible_entry_name(invalid_name, false);

        assert_eq!(result, None);
    }

    #[test]
    fn create_dir_all_should_create_missing_ancestors() {
        let root = TestDirectory::new("recursive-create");
        let nested = root.path.join("one").join("two").join("three");
        let filesystem = NativeWorkspacePickerFilesystem;

        let result = filesystem.create_dir_all(&nested);

        assert!(result.is_ok() && nested.is_dir(), "result was {result:?}");
    }

    #[test]
    fn create_dir_all_should_classify_an_existing_file_as_not_directory() {
        let root = TestDirectory::new("create-file-collision");
        let file = root.path.join("collision");
        fs::write(&file, b"test").unwrap();
        let filesystem = NativeWorkspacePickerFilesystem;

        let result = filesystem.create_dir_all(&file);

        assert_eq!(result, Err(WorkspacePickerFilesystemError::NotDirectory));
    }

    #[test]
    fn final_validation_should_preserve_the_exact_path_and_capture_device_inode_identity() {
        let root = TestDirectory::new("validation");
        let target = root.path.join("target");
        let selected = root.path.join("selected-spelling");
        fs::create_dir(&target).unwrap();
        symlink(&target, &selected).unwrap();
        let metadata = fs::metadata(&target).unwrap();
        let filesystem = NativeWorkspacePickerFilesystem;

        let result = filesystem.validate_workspace_directory(&selected).unwrap();

        assert_eq!(
            (result.path(), result.identity()),
            (
                selected.as_path(),
                WorkspaceDirectoryIdentity::new(metadata.dev(), metadata.ino()),
            )
        );
    }
}
