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
    // Desktop loaders expand percent field codes and reject an `=` in the program token, so a
    // source path cannot be that token. Like the Development launcher, the entry names a fixed
    // program and passes the path as an argument: the shell receives it as `$0` and replaces
    // itself with it, without evaluating the path.
    let exec = ["/bin/sh", "-c", "exec \"$0\"", executable]
        .map(exec_argument)
        .join(" ");
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

/// One Exec argument: quoted with its reserved characters escaped, each percent sign doubled so
/// it is not a field code, then each backslash escaped again for the entry's string value.
fn exec_argument(argument: &str) -> String {
    let mut quoted = String::from("\"");
    for character in argument.chars() {
        match character {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(character);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted.replace('\\', "\\\\")
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Launches the generated entry through GIO, the loader desktop sessions use for autostart,
    /// and reports the program path the launched process received.
    #[test]
    fn autostart_starts_the_exact_executable_from_a_reserved_source_path() {
        let probe = Command::new("/usr/bin/python3")
            .args(["-c", "from gi.repository import Gio"])
            .status();
        assert!(
            probe.is_ok_and(|status| status.success()),
            "GIO desktop parsing needs python3-gi and gir1.2-glib-2.0; run mise doctor project"
        );
        let scratch =
            std::env::temp_dir().join(format!("status-area-entry-{}", std::process::id()));
        // The same reserved characters the Development launcher supports in a source path.
        let prefix = scratch.join("source space \"quote\" \\ slash %f =equal $dollar `tick`");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        let record = scratch.join("launched");
        let executable = prefix.join("bin/spaceterm-status-area");
        std::fs::write(
            &executable,
            format!("#!/bin/sh\nprintf '%s' \"$0\" > '{}'\n", record.display()),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let entry_path =
            scratch.join("io.github.sadiksaifi.spaceterm-development.status-area.desktop");
        std::fs::write(
            &entry_path,
            entry(
                "spaceterm-development",
                "SpaceTerm Development",
                &executable,
            )
            .unwrap(),
        )
        .unwrap();
        let launch = Command::new("/usr/bin/python3")
            .args([
                "-I",
                "-c",
                "import sys\nfrom gi.repository import Gio\n\
                 info = Gio.DesktopAppInfo.new_from_filename(sys.argv[1])\n\
                 assert info is not None, 'GIO rejected the entry'\n\
                 assert info.launch([], None)",
            ])
            .arg(&entry_path)
            .status()
            .unwrap();
        assert!(launch.success(), "GIO could not launch the entry");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !record.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let launched = std::fs::read_to_string(&record).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&scratch);
        assert_eq!(launched, executable.to_str().unwrap());
    }
}
