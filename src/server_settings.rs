//! Server-owned validation. A draft never creates folders or grants client file authority.
use std::path::{Path, PathBuf};

use path_template::WorktreePathTemplateError;
use spaceterm_server::worktree::{Error, path_template};

pub(crate) fn validate_location_template(template: &str) -> Result<(), Error> {
    path_template::validate(template).map_err(Error::Template)?;
    let home = crate::platform::app_directories::AppDirectoryEnvironment::capture()
        .home
        .map(PathBuf::from);
    let prefix = template.split('{').next().ok_or(Error::Unavailable)?;
    // A placeholder can be part of a directory name. Only complete literal components
    // identify a folder whose permissions can be observed before repository/branch selection.
    let (folder, _) = prefix.rsplit_once('/').ok_or(Error::Unavailable)?;
    let mut directory = if folder == "~" || folder.starts_with("~/") {
        let home = home
            .filter(|home| home.is_absolute())
            .ok_or(Error::Unavailable)?;
        home.join(folder.strip_prefix("~/").unwrap_or(""))
    } else if folder.is_empty() {
        PathBuf::from("/")
    } else {
        PathBuf::from(folder)
    };
    loop {
        match std::fs::metadata(&directory) {
            Ok(metadata) if metadata.is_dir() && writable_directory(&directory) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A dangling link cannot be created as a folder by a later Worktree operation.
                if std::fs::symlink_metadata(&directory).is_ok() || !directory.pop() {
                    break;
                }
            }
            _ => break,
        }
    }
    Err(Error::Template(WorktreePathTemplateError::NotWritable))
}

fn writable_directory(directory: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let Ok(directory) = CString::new(directory.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: the absolute path is NUL-terminated and remains alive through the call.
    // AT_EACCESS observes the server process's effective account, including ACLs.
    unsafe {
        libc::faccessat(
            libc::AT_FDCWD,
            directory.as_ptr(),
            libc::W_OK | libc::X_OK,
            libc::AT_EACCESS,
        ) == 0
    }
}
