//! Owning-server facts and validation. These reads grant no client file or maintenance authority.
use std::path::{Path, PathBuf};

use path_template::WorktreePathTemplateError;
use spaceterm_server::worktree::{Error, ServerFacts, path_template};

pub(crate) fn server_facts() -> Result<ServerFacts, Error> {
    let mut hostname = [0u8; 256];
    // SAFETY: hostname is initialized writable storage of exactly the supplied length.
    if unsafe { libc::gethostname(hostname.as_mut_ptr().cast(), hostname.len()) } != 0 {
        return Err(Error::Unavailable);
    }
    let end = hostname
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(Error::Unavailable)?;
    let machine = std::str::from_utf8(&hostname[..end]).map_err(|_| Error::Unavailable)?;
    let operating_system = match std::env::consts::OS {
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    };
    // This crate is compiled into the owning server. A client or a pairing pin supplies none
    // of these facts, including the release version selected by this crate's build script.
    ServerFacts::new(machine, operating_system, env!("SPACETERM_VERSION"))
}

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
