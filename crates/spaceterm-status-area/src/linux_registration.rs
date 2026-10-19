//! Adds the status item to the account's XDG autostart entries, which desktop sessions start at
//! login. A Hide lasts until the session ends, so the desktop's own registration never undoes it.
use crate::account::home;
use std::{
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RegistrationError {
    #[error("Only an installed SpaceTerm adds its server status to login items.")]
    Unbundled,
    #[error("Your account's autostart entries are unavailable.")]
    Unavailable,
    #[error("The server status could not start.")]
    Refused,
}

/// Registers this executable and starts it unless the person hid it this session or turned its
/// autostart entry off. A second instance exits at once, so starting is always safe.
pub fn register(identity: &str, name: &str) -> Result<(), RegistrationError> {
    let executable = installed_executable()?;
    let directory = config_home()
        .ok_or(RegistrationError::Unavailable)?
        .join("autostart");
    let path = directory.join(format!(
        "io.github.sadiksaifi.{identity}.status-area.desktop"
    ));
    let current = std::fs::read_to_string(&path).ok();
    // The person turned the entry off in their session's startup settings.
    if current.as_deref().is_some_and(disabled) {
        return Ok(());
    }
    let contents = entry(identity, name, &executable)?;
    if current.as_deref() != Some(contents.as_str()) {
        write(&directory, &path, contents.as_bytes())?;
    }
    if hidden_marker(identity).is_some_and(|marker| marker.exists()) {
        return Ok(());
    }
    spawn(&executable)
}

/// Records a Hide in the session's runtime directory, which logout clears.
pub fn hide(identity: &str) {
    if let Some(marker) = hidden_marker(identity) {
        let _ = std::fs::write(marker, b"");
    }
}

fn hidden_marker(identity: &str) -> Option<PathBuf> {
    let runtime = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    runtime
        .is_absolute()
        .then(|| runtime.join(format!("{identity}-status-area-hidden")))
}

/// The executable in an installed layout, `bin/` beside `share/spaceterm`. A Cargo build
/// directory never becomes a login item.
fn installed_executable() -> Result<PathBuf, RegistrationError> {
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|_| RegistrationError::Unbundled)?;
    let prefix = executable
        .parent()
        .filter(|directory| directory.file_name().is_some_and(|name| name == "bin"))
        .and_then(Path::parent)
        .ok_or(RegistrationError::Unbundled)?;
    if !prefix.join("share/spaceterm").is_dir() {
        return Err(RegistrationError::Unbundled);
    }
    Ok(executable)
}

fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home().map(|home| home.join(".config")))
}

fn disabled(entry: &str) -> bool {
    entry.lines().any(|line| {
        matches!(
            line.trim(),
            "Hidden=true" | "X-GNOME-Autostart-enabled=false"
        )
    })
}

fn entry(identity: &str, name: &str, executable: &Path) -> Result<String, RegistrationError> {
    let executable = executable.to_str().ok_or(RegistrationError::Unbundled)?;
    if executable.chars().any(char::is_control) {
        return Err(RegistrationError::Unbundled);
    }
    // The Exec value quotes the path, then the entry format escapes each backslash once more.
    let mut quoted = String::from("\"");
    for character in executable.chars() {
        if matches!(character, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    let exec = quoted.replace('\\', "\\\\");
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={name} Server Status\n\
         Comment=Shows and controls the {name} server while {name} is closed\n\
         Exec={exec}\n\
         Icon=io.github.sadiksaifi.{identity}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n"
    ))
}

/// Replaces the entry in one rename, so the session never reads a partial file.
fn write(directory: &Path, path: &Path, contents: &[u8]) -> Result<(), RegistrationError> {
    std::fs::create_dir_all(directory).map_err(|_| RegistrationError::Unavailable)?;
    let staged = path.with_extension("desktop.partial");
    std::fs::write(&staged, contents)
        .and_then(|()| std::fs::rename(&staged, path))
        .map_err(|_| {
            let _ = std::fs::remove_file(&staged);
            RegistrationError::Unavailable
        })
}

/// Starts the status item in its own session, so closing or crashing the desktop leaves it.
fn spawn(executable: &Path) -> Result<(), RegistrationError> {
    let mut command = Command::new(executable);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe and touches no memory shared with the parent.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    // The status item outlives this short-lived process, which exits without waiting for it.
    command
        .spawn()
        .map(drop)
        .map_err(|_| RegistrationError::Refused)
}
