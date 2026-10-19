//! The account's status-area control for its local SpaceTerm Server. It runs apart from the
//! desktop and the server, so its controls work while the desktop is closed or after it crashes,
//! and the server stays free of any graphical dependency.
mod account;
mod menu;
mod worker;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod linux_registration;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_registration;

#[cfg(target_os = "linux")]
use linux_registration as registration;
#[cfg(target_os = "macos")]
use macos_registration as registration;

use spaceterm_server::service::{
    Service,
    control::{Controller, cli, display_name},
};

/// This build's compiled identity. Status and controls never reach another build's server.
const IDENTITY: &str = env!("SPACETERM_STATUS_AREA_IDENTITY");
const VERSION: &str = env!("SPACETERM_STATUS_AREA_VERSION");

const USAGE: &str = "Usage: spaceterm-status-area [register | --version | service-COMMAND IDENTITY]

With no argument, shows this build's local server in the status area until you choose Hide \
Status Icon or log out. register adds the status item to this account's login items and starts \
it once. Neither starts, stops, or installs the server.

Without a status area or display, the service commands below control the same server.";

/// Why the status item could not run. Messages are bounded and name no path or native error.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StatusError {
    #[error("The server status could not start.")]
    Unavailable,
    #[error("The desktop session's status area is unavailable.")]
    SessionUnavailable,
}

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    // Metadata needs no account, so packaging and help work wherever the executable runs.
    match arguments.as_slice() {
        [argument] if argument == "--version" => {
            println!("{} Server Status {VERSION}", display_name(IDENTITY));
            return;
        }
        [argument] if argument == "--help" => {
            println!("{USAGE}\n\n{}", cli::help(IDENTITY));
            return;
        }
        // The fallback for a session without a status area: the same commands, output, and
        // confirmation as the headless server, at this installation's stable path.
        [command, ..] if command.as_encoded_bytes().starts_with(b"service-") => {
            match cli::run(IDENTITY, &arguments, &mut std::io::stdout().lock()) {
                Ok(code) => std::process::exit(code.into()),
                Err(error) => fail(&error),
            }
        }
        [argument] if argument == "register" => {
            if let Err(error) = registration::register(IDENTITY, display_name(IDENTITY)) {
                fail(&error);
            }
            return;
        }
        [] => {}
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
    let controller = match Service::for_identity(IDENTITY) {
        Ok(service) => Controller::new(service),
        Err(error) => fail(&error),
    };
    if let Err(error) = run(controller) {
        fail(&error);
    }
}

#[cfg(target_os = "macos")]
fn run(controller: Controller) -> Result<(), StatusError> {
    macos::run(controller)
}

#[cfg(target_os = "linux")]
fn run(controller: Controller) -> Result<(), StatusError> {
    linux::run(controller)
}

/// Prints only the bounded classification. Native errors and paths never reach output.
fn fail(error: &dyn std::fmt::Display) -> ! {
    eprintln!("{error}");
    std::process::exit(1);
}
