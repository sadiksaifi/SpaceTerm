use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::{LocalFilesystemAuthority, LocalFilesystemError, validate_absolute_path};
use crate::domain::ValidatedWorkspaceDirectory;

#[derive(Clone, Eq, PartialEq)]
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

impl WorkspacePickerFilesystem for LocalFilesystemAuthority {
    fn list_directories(
        &self,
        directory: &Path,
        hide_dot_prefixed: bool,
    ) -> Result<Vec<WorkspacePickerDirectoryEntry>, WorkspacePickerFilesystemError> {
        validate_absolute_path(directory).map_err(classify_workspace_directory_error)?;
        let entries = fs::read_dir(directory).map_err(classify_io_error)?;
        let mut directories = Vec::new();

        for entry in entries {
            let entry = entry.map_err(classify_io_error)?;
            let Some(name) = visible_entry_name(entry.file_name(), hide_dot_prefixed) else {
                continue;
            };

            let path = entry.path();
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    continue;
                }
                Err(error) => return Err(classify_io_error(error)),
            };
            if metadata.is_dir() {
                directories.push(WorkspacePickerDirectoryEntry::new(name, path));
            }
        }

        Ok(directories)
    }

    fn probe_exact_path(&self, path: &Path) -> WorkspacePickerExactPathProbe {
        match self.validate_workspace_directory(path) {
            Ok(_) => WorkspacePickerExactPathProbe::ReadableDirectory,
            Err(error) => WorkspacePickerExactPathProbe::Unavailable(
                classify_workspace_directory_error(error),
            ),
        }
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), WorkspacePickerFilesystemError> {
        validate_absolute_path(path).map_err(classify_workspace_directory_error)?;
        fs::create_dir_all(path).map_err(|error| {
            if fs::metadata(path).is_ok_and(|metadata| !metadata.is_dir()) {
                WorkspacePickerFilesystemError::NotDirectory
            } else {
                classify_io_error(error)
            }
        })
    }

    fn validate_workspace_directory(
        &self,
        path: &Path,
    ) -> Result<ValidatedWorkspaceDirectory, WorkspacePickerFilesystemError> {
        LocalFilesystemAuthority::validate_workspace_directory(self, path)
            .map_err(classify_workspace_directory_error)
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

impl std::fmt::Debug for WorkspacePickerDirectoryEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorkspacePickerDirectoryEntry(<redacted>)")
    }
}

fn classify_io_error(error: io::Error) -> WorkspacePickerFilesystemError {
    classify_workspace_directory_error(super::classify_io_error(error))
}

fn classify_workspace_directory_error(
    error: LocalFilesystemError,
) -> WorkspacePickerFilesystemError {
    match error {
        LocalFilesystemError::PermissionDenied => WorkspacePickerFilesystemError::PermissionDenied,
        LocalFilesystemError::Missing => WorkspacePickerFilesystemError::Missing,
        LocalFilesystemError::NotDirectory => WorkspacePickerFilesystemError::NotDirectory,
        _ => WorkspacePickerFilesystemError::Other,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

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
    fn error_mapping_should_classify_permission_denied_as_permission_denied() {
        let error = io::Error::from(io::ErrorKind::PermissionDenied);

        let result = classify_io_error(error);

        assert_eq!(result, WorkspacePickerFilesystemError::PermissionDenied);
    }

    #[test]
    fn error_mapping_should_classify_not_found_as_missing() {
        let error = io::Error::from(io::ErrorKind::NotFound);

        let result = classify_io_error(error);

        assert_eq!(result, WorkspacePickerFilesystemError::Missing);
    }

    #[test]
    fn error_mapping_should_classify_unrecognized_errors_as_other() {
        let error = io::Error::from(io::ErrorKind::Other);

        let result = classify_io_error(error);

        assert_eq!(result, WorkspacePickerFilesystemError::Other);
    }

    #[test]
    fn exact_path_probe_should_classify_a_missing_path() {
        let root = TestDirectory::new("probe-missing");
        let filesystem = LocalFilesystemAuthority::testing();

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
        let filesystem = LocalFilesystemAuthority::testing();

        let result = filesystem.list_directories(&file, true);

        assert_eq!(result, Err(WorkspacePickerFilesystemError::NotDirectory));
    }

    #[test]
    fn exact_path_probe_should_classify_a_regular_file_as_not_directory() {
        let root = TestDirectory::new("probe-file");
        let file = root.path.join("file");
        fs::write(&file, b"test").unwrap();
        let filesystem = LocalFilesystemAuthority::testing();

        let result = filesystem.probe_exact_path(&file);

        assert_eq!(
            result,
            WorkspacePickerExactPathProbe::Unavailable(
                WorkspacePickerFilesystemError::NotDirectory
            )
        );
    }

    #[test]
    fn listing_should_include_dot_prefixed_directories_when_requested() {
        let root = TestDirectory::new("shown-hidden");
        let hidden = root.path.join(".hidden");
        fs::create_dir(&hidden).unwrap();
        let filesystem = LocalFilesystemAuthority::testing();

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
    fn listing_should_treat_package_bundles_as_directories() {
        let root = TestDirectory::new("package");
        let package = root.path.join("Example.app");
        fs::create_dir(&package).unwrap();
        let filesystem = LocalFilesystemAuthority::testing();

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
    fn create_dir_all_should_create_missing_ancestors() {
        let root = TestDirectory::new("recursive-create");
        let nested = root.path.join("one").join("two").join("three");
        let filesystem = LocalFilesystemAuthority::testing();

        let result = filesystem.create_dir_all(&nested);

        assert!(result.is_ok() && nested.is_dir(), "result was {result:?}");
    }

    #[test]
    fn create_dir_all_should_classify_an_existing_file_as_not_directory() {
        let root = TestDirectory::new("create-file-collision");
        let file = root.path.join("collision");
        fs::write(&file, b"test").unwrap();
        let filesystem = LocalFilesystemAuthority::testing();

        let result = filesystem.create_dir_all(&file);

        assert_eq!(result, Err(WorkspacePickerFilesystemError::NotDirectory));
    }

    mod macos_adapter_tests {
        include!("../macos_adapter_tests/picker.rs");
    }
}
