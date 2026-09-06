//! Host facts selected by application composition for shell launch planning.

use std::path::{Path, PathBuf};

pub(crate) fn resource_root() -> PathBuf {
    if let Ok(executable) = std::env::current_exe()
        && let Some(macos) = executable.parent()
        && macos.file_name().is_some_and(|name| name == "MacOS")
        && let Some(contents) = macos.parent()
    {
        let resources = contents.join("Resources");
        if resources.join("shell-integration").is_dir() {
            return resources;
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}

pub(crate) fn user_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_owned())
}

pub(crate) fn local_hostname() -> Option<String> {
    let mut buffer = [0_u8; 256];
    // SAFETY: `buffer` is writable for its complete length and gethostname writes at most that
    // many bytes. A missing terminator is handled by using the full initialized buffer.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return None;
    }
    let length = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    std::str::from_utf8(&buffer[..length])
        .ok()
        .filter(|hostname| !hostname.is_empty() && !hostname.chars().any(char::is_control))
        .map(ToOwned::to_owned)
}
