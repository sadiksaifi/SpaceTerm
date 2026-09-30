//! The Directory Picker's source for this computer's directories, read through the Local
//! Filesystem Authority.
use std::path::PathBuf;

use gpui::BackgroundExecutor;

use super::{
    DirectoryListing, DirectoryRow, DirectorySource, DirectorySourceError, ExactPathState,
    MAXIMUM_DIRECTORY_ROWS, PickerPath, SourceFuture,
};
use crate::domain::PinnedDirectory;
use crate::platform::local_filesystem::{LocalFilesystemAuthority, LocalFilesystemError};

/// Reads local directories on the background executor, with `~` naming the account's home.
pub(crate) struct LocalDirectorySource {
    filesystem: LocalFilesystemAuthority,
    home: PathBuf,
    executor: BackgroundExecutor,
}

impl LocalDirectorySource {
    pub(crate) fn new(
        filesystem: LocalFilesystemAuthority,
        home: PathBuf,
        executor: BackgroundExecutor,
    ) -> Self {
        Self {
            filesystem,
            home,
            executor,
        }
    }

    /// Resolves a picker spelling to the local path it names, without a trailing separator.
    fn local_path(&self, directory: &PickerPath) -> PathBuf {
        let spelled = directory.as_str().trim_end_matches('/');
        match spelled.strip_prefix('~') {
            Some("") => self.home.clone(),
            Some(relative) => self.home.join(relative.trim_start_matches('/')),
            None if spelled.is_empty() => PathBuf::from("/"),
            None => PathBuf::from(spelled),
        }
    }

    fn run<T: Send + 'static>(
        &self,
        directory: &PickerPath,
        operation: impl FnOnce(LocalFilesystemAuthority, PathBuf) -> Result<T, LocalFilesystemError>
        + Send
        + 'static,
    ) -> SourceFuture<T> {
        let filesystem = self.filesystem.clone();
        let path = self.local_path(directory);
        let task = self
            .executor
            .spawn(async move { operation(filesystem, path).map_err(source_error) });
        Box::pin(task)
    }
}

impl DirectorySource for LocalDirectorySource {
    fn machine_name(&self) -> Option<&str> {
        None
    }

    fn discover_home(&self) -> SourceFuture<PickerPath> {
        let home = self
            .home
            .to_str()
            .ok_or(DirectorySourceError::Other)
            .and_then(|home| {
                PickerPath::new(home.to_owned()).map_err(|_| DirectorySourceError::Other)
            });
        Box::pin(async move { home })
    }

    fn list_directories(&self, directory: PickerPath) -> SourceFuture<DirectoryListing> {
        self.run(&directory, |filesystem, path| {
            let listing = filesystem.list_child_directories(&path, MAXIMUM_DIRECTORY_ROWS)?;
            let rows = listing
                .names
                .into_iter()
                .filter_map(|name| DirectoryRow::new(name).ok())
                .collect();
            Ok(DirectoryListing::bounded(rows, listing.truncated))
        })
    }

    fn probe_exact_path(&self, directory: PickerPath) -> SourceFuture<ExactPathState> {
        self.run(&directory, |filesystem, path| {
            match filesystem.probe_directory(&path) {
                Ok(()) => Ok(ExactPathState::ReadableDirectory),
                Err(LocalFilesystemError::Missing) => Ok(ExactPathState::Missing),
                Err(error) => Err(error),
            }
        })
    }

    fn create_directory_recursively(&self, directory: PickerPath) -> SourceFuture<()> {
        self.run(&directory, |filesystem, path| {
            filesystem.create_directory_all(&path)
        })
    }

    fn pin(&self, directory: PickerPath) -> SourceFuture<PinnedDirectory> {
        self.run(&directory, |filesystem, path| {
            filesystem
                .validate_directory(&path)
                .map(PinnedDirectory::Local)
        })
    }
}

const fn source_error(error: LocalFilesystemError) -> DirectorySourceError {
    match error {
        LocalFilesystemError::Missing => DirectorySourceError::Missing,
        LocalFilesystemError::NotDirectory => DirectorySourceError::NotDirectory,
        LocalFilesystemError::PermissionDenied => DirectorySourceError::PermissionDenied,
        LocalFilesystemError::Unreadable
        | LocalFilesystemError::NotAbsolute
        | LocalFilesystemError::Malformed
        | LocalFilesystemError::NotFile
        | LocalFilesystemError::IdentityChanged
        | LocalFilesystemError::Capacity
        | LocalFilesystemError::Other => DirectorySourceError::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "spaceterm-local-directory-source-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(path.join("Projects/SpaceTerm")).unwrap();
            std::fs::write(path.join("notes.txt"), b"").unwrap();
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn source(home: &Fixture, cx: &gpui::TestAppContext) -> LocalDirectorySource {
        LocalDirectorySource::new(
            LocalFilesystemAuthority::testing(),
            home.0.clone(),
            cx.executor(),
        )
    }

    #[test]
    fn only_a_denied_permission_should_read_as_permission_denied() {
        assert_eq!(
            source_error(LocalFilesystemError::PermissionDenied),
            DirectorySourceError::PermissionDenied
        );
        assert_eq!(
            source_error(LocalFilesystemError::Unreadable),
            DirectorySourceError::Other
        );
    }

    fn path(value: &str) -> PickerPath {
        PickerPath::new(value.to_owned()).unwrap()
    }

    #[gpui::test]
    async fn home_relative_paths_list_and_probe_local_directories(cx: &mut gpui::TestAppContext) {
        let home = Fixture::new("list");
        let source = source(&home, cx);

        assert_eq!(
            source.discover_home().await.unwrap().as_str(),
            home.0.to_str().unwrap()
        );
        let listing = source.list_directories(path("~/")).await.unwrap();
        let names = listing
            .rows()
            .iter()
            .map(DirectoryRow::name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["Projects"]);
        assert_eq!(
            source.probe_exact_path(path("~/Projects/")).await,
            Ok(ExactPathState::ReadableDirectory)
        );
        assert_eq!(
            source.probe_exact_path(path("~/Missing/")).await,
            Ok(ExactPathState::Missing)
        );
        assert_eq!(
            source.probe_exact_path(path("~/notes.txt")).await,
            Err(DirectorySourceError::NotDirectory)
        );
    }

    #[gpui::test]
    async fn creating_and_pinning_yield_a_validated_local_directory(cx: &mut gpui::TestAppContext) {
        let home = Fixture::new("pin");
        let source = source(&home, cx);

        source
            .create_directory_recursively(path("~/New/Nested/"))
            .await
            .unwrap();
        let Ok(PinnedDirectory::Local(directory)) = source.pin(path("~/New/Nested/")).await else {
            panic!("the created directory was not pinned as a local directory");
        };
        assert_eq!(
            directory.path().to_str(),
            home.0.join("New/Nested").to_str()
        );
        assert_eq!(
            source.pin(path("~/notes.txt")).await.unwrap_err(),
            DirectorySourceError::NotDirectory
        );
    }
}
