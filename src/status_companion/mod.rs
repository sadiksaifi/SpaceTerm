//! The optional status-area companion. It is its own Client with its own identity, reads
//! status through the status-only facade, and offers three commands: open the local
//! desktop installation, retry the connection, and quit itself.

mod controller;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod model;
mod native;
#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::io::Write as _;
use std::process::ExitCode;
use std::sync::Arc;

use spaceterm_client::management_cli::{StatusBackend, StatusManagement, StatusOnlyManagement};

use crate::application_identity::ApplicationIdentity;
use controller::CompanionFailure;

/// The companion's names, derived from the desktop identity it ships with. A companion
/// never shares the desktop's application identifier, bundle identifier or bus name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CompanionIdentity {
    desktop: ApplicationIdentity,
}

impl CompanionIdentity {
    pub(crate) const fn current() -> Self {
        Self::for_desktop(ApplicationIdentity::current())
    }

    pub(crate) const fn for_desktop(desktop: ApplicationIdentity) -> Self {
        Self { desktop }
    }

    pub(crate) fn display_name(self) -> String {
        format!("{} Status", self.desktop.display_name())
    }

    pub(crate) fn application_id(self) -> String {
        format!("{}.status", self.desktop.application_id())
    }

    pub(crate) const fn desktop_name(self) -> &'static str {
        self.desktop.display_name()
    }

    #[cfg_attr(
        not(any(test, target_os = "macos")),
        allow(
            dead_code,
            reason = "only macOS looks the desktop up by its bundle identifier"
        )
    )]
    pub(crate) const fn desktop_id(self) -> &'static str {
        self.desktop.application_id()
    }

    pub(crate) fn version_label(self) -> String {
        format!("{} {}", self.display_name(), env!("SPACETERM_VERSION"))
    }
}

/// The only status client the companion composes: the status-only facade over a backend
/// bound to the companion's own Client. Production composes
/// [`controller::StatusSource::Unbound`] until pairing supplies that binding (C23).
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "production has no companion Client binding until pairing supplies it"
    )
)]
pub(crate) fn status_client(backend: impl StatusBackend + 'static) -> Arc<dyn StatusManagement> {
    Arc::new(StatusOnlyManagement::new(backend))
}

enum Invocation {
    Run,
    Help,
    Version,
    Usage,
}

/// Launch arguments are few and bounded. macOS may add a process serial number argument.
fn invocation(arguments: impl Iterator<Item = OsString>) -> Invocation {
    let arguments: Vec<OsString> = arguments
        .filter(|argument| {
            !(cfg!(target_os = "macos")
                && argument
                    .to_str()
                    .is_some_and(|text| text.starts_with("-psn_")))
        })
        .take(2)
        .collect();
    match arguments.as_slice() {
        [] => Invocation::Run,
        [only] if only == "--version" => Invocation::Version,
        [only] if only == "--help" || only == "-h" => Invocation::Help,
        _ => Invocation::Usage,
    }
}

fn usage(identity: CompanionIdentity) -> String {
    format!(
        "Usage: spaceterm-status [--help | --version]\n\n\
         {} shows the selected server's status in the status area and opens {}.\n\
         It reads status only. It never starts, stops or changes a server.\n",
        identity.display_name(),
        identity.desktop_name(),
    )
}

pub(crate) fn main(arguments: impl Iterator<Item = OsString>) -> ExitCode {
    let identity = CompanionIdentity::current();
    let (code, output, error) = match invocation(arguments) {
        Invocation::Version => (0, Some(format!("{}\n", identity.version_label())), None),
        Invocation::Help => (0, Some(usage(identity)), None),
        Invocation::Usage => (2, None, Some(usage(identity))),
        Invocation::Run => {
            let (code, message) = report(identity, run(identity));
            (code, None, message.map(|message| format!("{message}\n")))
        }
    };
    if let Some(output) = output
        && std::io::stdout().write_all(output.as_bytes()).is_err()
    {
        return ExitCode::from(74);
    }
    if let Some(error) = error {
        let _ = std::io::stderr().write_all(error.as_bytes());
    }
    ExitCode::from(code)
}

/// The exit status and the one content-free line a launch reports.
pub(crate) fn report(
    identity: CompanionIdentity,
    result: Result<(), CompanionFailure>,
) -> (u8, Option<String>) {
    let name = identity.display_name();
    match result {
        Ok(()) => (0, None),
        // No package installs spaceterm-cli yet, so the next step names the desktop.
        Err(CompanionFailure::StatusAreaUnavailable) => (
            69,
            Some(format!(
                "{name}: this session has no status area. Open {} to see server status.",
                identity.desktop_name()
            )),
        ),
        Err(CompanionFailure::AlreadyRunning) => (0, Some(format!("{name} is already running."))),
        Err(
            CompanionFailure::DesktopUnavailable
            | CompanionFailure::MainThreadRequired
            | CompanionFailure::RuntimeUnavailable,
        ) => (70, Some(format!("{name} could not start."))),
    }
}

#[cfg(target_os = "macos")]
fn run(identity: CompanionIdentity) -> Result<(), CompanionFailure> {
    macos::run(identity)
}

#[cfg(target_os = "linux")]
fn run(identity: CompanionIdentity) -> Result<(), CompanionFailure> {
    linux::run(identity)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn run(_identity: CompanionIdentity) -> Result<(), CompanionFailure> {
    Err(CompanionFailure::StatusAreaUnavailable)
}
