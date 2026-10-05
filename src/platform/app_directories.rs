#[cfg(test)]
pub(crate) use spaceterm_directories::APP_DIR_NAME;
#[cfg(target_os = "linux")]
pub(crate) use spaceterm_directories::DesktopResourceDirectories;
pub(crate) use spaceterm_directories::{
    AppDirectories, AppDirectoryEnvironment, AppDirectoryFile, AppDirectoryRoot, DirectoryError,
};
