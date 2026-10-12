//! SpaceTerm Status: an optional status-area client that the user launches explicitly.
//!
//! It shows the selected server's status and opens the local desktop installation. It has
//! no terminal, control, startup, enrollment or management path, so quitting or crashing it
//! closes only its own client connection.

#[allow(
    dead_code,
    reason = "the companion derives its names from the desktop identity and needs nothing else"
)]
#[path = "../application_identity.rs"]
mod application_identity;
#[path = "../status_companion/mod.rs"]
mod status_companion;

fn main() -> std::process::ExitCode {
    status_companion::main(std::env::args_os().skip(1))
}
