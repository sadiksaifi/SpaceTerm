//! Starts the independent server status item that ships beside this desktop executable.
use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

/// Asks this installation's status helper to add itself to the account's login items. The helper
/// leaves an unchanged registration as it is, so a status item the person hid stays hidden. It
/// never installs, starts, or stops the server. A build without the helper does nothing.
pub(crate) fn register() {
    let Some(helper) = helper() else {
        return;
    };
    // The helper runs launchctl or writes an autostart entry, so the launch never waits for it.
    let _ = std::thread::Builder::new()
        .name("status-area-registration".into())
        .spawn(move || {
            let _ = Command::new(helper)
                .arg("register")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
}

#[cfg(target_os = "macos")]
fn helper() -> Option<PathBuf> {
    let contents = std::env::current_exe().ok()?.parent()?.parent()?.to_owned();
    let name = crate::application_identity::ApplicationIdentity::current().display_name();
    Some(contents.join(format!(
        "Library/LoginItems/{name} Server Status.app/Contents/MacOS/spaceterm-status-area"
    )))
    .filter(|helper| helper.is_file())
}

#[cfg(target_os = "linux")]
fn helper() -> Option<PathBuf> {
    Some(
        std::env::current_exe()
            .ok()?
            .parent()?
            .join("spaceterm-status-area"),
    )
    .filter(|helper| helper.is_file())
}
