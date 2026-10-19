//! Adds the status item to the account's login items as a LaunchAgent. launchd starts it at
//! login and again after a crash, but not after the person hides it.
use crate::account::home;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RegistrationError {
    #[error("Only an installed SpaceTerm application adds its server status to login items.")]
    Unbundled,
    #[error("Your account's login items are unavailable.")]
    Unavailable,
    #[error(
        "macOS did not allow the server status to start at login. Check Login Items in System Settings."
    )]
    Refused,
}

/// Registers this executable. An unchanged, loaded registration is left as it is, so a status
/// item the person hid stays hidden until the next login.
pub fn register(identity: &str, _name: &str) -> Result<(), RegistrationError> {
    let executable = bundled_executable()?;
    let label = format!("io.github.sadiksaifi.{identity}.status-area");
    let directory = home()
        .ok_or(RegistrationError::Unavailable)?
        .join("Library/LaunchAgents");
    let path = directory.join(format!("{label}.plist"));
    let contents = contents(&label, identity, &executable)?;
    // SAFETY: geteuid has no preconditions.
    let domain = format!("gui/{}", unsafe { libc::geteuid() });
    let target = format!("{domain}/{label}");
    let unchanged = std::fs::read(&path).is_ok_and(|current| current == contents);
    if unchanged && launchctl(&["print", &target]) {
        return Ok(());
    }
    if !unchanged {
        write(&directory, &path, &contents)?;
        // A loaded job keeps its old program until it is unloaded.
        launchctl(&["bootout", &target]);
    }
    if !launchctl(&[
        "bootstrap",
        &domain,
        path.to_str().ok_or(RegistrationError::Unavailable)?,
    ]) {
        return Err(RegistrationError::Refused);
    }
    Ok(())
}

/// The executable inside the status bundle nested in an application bundle.
fn bundled_executable() -> Result<PathBuf, RegistrationError> {
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|_| RegistrationError::Unbundled)?;
    let bundle = executable
        .parent()
        .filter(|path| path.ends_with("Contents/MacOS"))
        .and_then(Path::parent)
        .and_then(Path::parent)
        .filter(|path| path.extension().is_some_and(|extension| extension == "app"))
        .ok_or(RegistrationError::Unbundled)?;
    bundle
        .parent()
        .filter(|path| path.ends_with("Contents/Library/LoginItems"))
        .ok_or(RegistrationError::Unbundled)?;
    Ok(executable)
}

fn contents(label: &str, identity: &str, executable: &Path) -> Result<Vec<u8>, RegistrationError> {
    let executable = escape(executable.to_str().ok_or(RegistrationError::Unbundled)?);
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{executable}</string></array>
<key>AssociatedBundleIdentifiers</key><array><string>io.github.sadiksaifi.{identity}</string></array>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>ProcessType</key><string>Interactive</string>
<key>LimitLoadToSessionType</key><string>Aqua</string>
</dict></plist>
"#
    )
    .into_bytes())
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Replaces the registration in one rename, so launchd never reads a partial file.
fn write(directory: &Path, path: &Path, contents: &[u8]) -> Result<(), RegistrationError> {
    std::fs::create_dir_all(directory).map_err(|_| RegistrationError::Unavailable)?;
    let staged = path.with_extension("plist.partial");
    std::fs::write(&staged, contents)
        .and_then(|()| std::fs::rename(&staged, path))
        .map_err(|_| {
            let _ = std::fs::remove_file(&staged);
            RegistrationError::Unavailable
        })
}

fn launchctl(arguments: &[&str]) -> bool {
    Command::new("/bin/launchctl")
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}
