//! The desktop executable's service-control fallback, run where no window system exists.
#![cfg(feature = "desktop")]

use std::process::Command;

fn desktop() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_spaceterm-desktop"));
    // No display, session bus, or home: the fallback must not reach for any of them.
    command.env_clear();
    command
}

#[test]
fn help_lists_the_service_commands_without_starting_the_client() {
    let output = desktop().arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for command in [
        "service-status",
        "service-start",
        "service-stop",
        "service-restart",
    ] {
        assert!(help.contains(command), "help names {command}");
    }
}

#[test]
fn service_commands_refuse_another_build_identity_before_any_account_access() {
    let output = desktop()
        .args(["service-status", "another-identity"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"Invalid arguments. See --help.\n");
}
