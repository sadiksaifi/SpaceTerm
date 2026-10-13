use std::ffi::OsString;

use spaceterm_client_native::cli::arguments::{
    MAX_ARGUMENT_BYTES, MAX_ARGUMENTS, MAX_TOTAL_ARGUMENT_BYTES,
};
use spaceterm_client_native::cli::{CliCommand, CliFailure, ConfirmationMode, parse};

fn invocation(args: &[&str]) -> Result<spaceterm_client_native::cli::CliInvocation, CliFailure> {
    parse(args.iter().map(OsString::from))
}

#[test]
fn no_arguments_do_not_start_a_server() {
    assert_eq!(invocation(&[]).unwrap().command, CliCommand::Help);
}
#[test]
fn help_and_version_require_no_credentials_or_display() {
    for (arg, command) in [
        ("--help", CliCommand::Help),
        ("--version", CliCommand::Version),
    ] {
        assert_eq!(invocation(&[arg]).unwrap().command, command);
    }
}
#[test]
fn build_identity_cannot_be_selected_by_arguments() {
    assert_eq!(
        invocation(&["--build", "release", "status"]),
        Err(CliFailure::Usage)
    );
}
#[test]
fn unknown_and_conflicting_arguments_do_not_echo_values() {
    for args in [
        vec!["status", "secret"],
        vec!["stop", "--assess", "--confirm", "secret"],
        vec!["--json", "--json", "status"],
        vec!["status", "--password", "secret"],
    ] {
        let error = invocation(&args).unwrap_err();
        assert_eq!(error, CliFailure::Usage);
        assert!(!format!("{error:?} {error}").contains("secret"));
    }
}
#[test]
fn argument_count_and_byte_bounds_reject_bound_plus_one() {
    assert_eq!(
        parse(vec![OsString::from("--help"); MAX_ARGUMENTS + 1]),
        Err(CliFailure::LimitExceeded)
    );
    assert_eq!(
        parse([OsString::from("a".repeat(MAX_ARGUMENT_BYTES + 1))]),
        Err(CliFailure::LimitExceeded)
    );
    assert_eq!(
        parse(vec![
            OsString::from("a".repeat(MAX_ARGUMENT_BYTES));
            MAX_TOTAL_ARGUMENT_BYTES / MAX_ARGUMENT_BYTES + 1
        ]),
        Err(CliFailure::LimitExceeded)
    );
    // At the byte ceilings, syntax (not resource admission) rejects these values.
    assert_eq!(
        parse(vec![
            OsString::from("a".repeat(MAX_ARGUMENT_BYTES));
            MAX_TOTAL_ARGUMENT_BYTES / MAX_ARGUMENT_BYTES
        ]),
        Err(CliFailure::Usage)
    );
}
#[test]
fn noninteractive_destructive_commands_require_exact_confirmation() {
    for command in ["stop", "end-all-sessions"] {
        assert_eq!(
            invocation(&["--non-interactive", command]),
            Err(CliFailure::ConfirmationRequired)
        );
        assert!(matches!(
            invocation(&["--non-interactive", command, "--assess"])
                .unwrap()
                .command,
            CliCommand::Stop {
                confirmation: ConfirmationMode::Assess
            } | CliCommand::EndAllSessions {
                confirmation: ConfirmationMode::Assess
            }
        ));
        assert!(invocation(&["--non-interactive", command, "--confirm", "token_1"]).is_ok());
    }
}
#[test]
fn remote_access_requires_explicit_literal_bounded_unique_scope() {
    assert!(
        matches!(invocation(&["remote-access", "enable", "--bind", "0.0.0.0:0", "--bind", "[::]:4000", "--assess"]).unwrap().command, CliCommand::RemoteAccess { endpoints, .. } if endpoints.len() == 2)
    );
    for args in [
        vec!["remote-access", "enable"],
        vec!["remote-access", "enable", "--bind", "example.com:4000"],
        vec![
            "remote-access",
            "enable",
            "--bind",
            "127.0.0.1:4",
            "--bind",
            "127.0.0.1:4",
        ],
        vec!["remote-access", "disable", "--bind", "127.0.0.1:4"],
    ] {
        assert_eq!(invocation(&args), Err(CliFailure::Usage));
    }
    let mut args = vec!["remote-access".to_owned(), "enable".to_owned()];
    for port in 1..=9 {
        args.extend(["--bind".to_owned(), format!("127.0.0.1:{port}")]);
    }
    assert_eq!(
        parse(args.into_iter().map(OsString::from)),
        Err(CliFailure::LimitExceeded)
    );
}
#[test]
fn ids_versions_and_singleton_options_are_strict() {
    for args in [
        vec!["update", "stage", "--version", "01.2.3"],
        vec!["update", "stage", "--version", "1.2.3-rc1"],
        vec!["update", "activate", "--installation", "0", "--assess"],
        vec![
            "unpair",
            "--device",
            "00000000000000000000000000000000",
            "--assess",
        ],
        vec![
            "pair",
            "authorize",
            "11111111111111111111111111111111",
            "--device-fingerprint",
            "bad",
        ],
        vec!["pair", "enroll", "--recover-account", "--recover-account"],
    ] {
        assert_eq!(invocation(&args), Err(CliFailure::Usage));
    }
    assert!(invocation(&["update", "stage", "--version", "1.2.3"]).is_ok());
    assert!(invocation(&["unpair", "--retry-pending"]).is_ok());
}
#[test]
fn operation_identity_keeps_server_pairing_client_stream_and_sequence() {
    let value = "11111111111111111111111111111111:2:3:4:5";
    let CliCommand::Status {
        operation: Some(spaceterm_client_native::cli::arguments::RequestSelector::Protocol(id)),
    } = invocation(&["status", "--operation", value])
        .unwrap()
        .command
    else {
        panic!("operation was not parsed")
    };
    assert_eq!(
        (
            id.stream().pairing().local(),
            id.stream().client().local(),
            id.stream().key().local(),
            id.sequence().get()
        ),
        (2, 3, 4, 5)
    );
    assert_eq!(
        invocation(&[
            "status",
            "--operation",
            "11111111111111111111111111111111:2:3:4:0"
        ]),
        Err(CliFailure::Usage)
    );
}
#[cfg(unix)]
#[test]
fn non_utf8_arguments_fail_without_lossy_echo() {
    use std::os::unix::ffi::OsStringExt;
    assert_eq!(
        parse([OsString::from_vec(vec![0xff])]),
        Err(CliFailure::Usage)
    );
}

#[test]
fn account_install_is_explicit_and_restart_and_uninstall_require_exact_confirmation() {
    assert!(invocation(&["install"]).is_ok());
    assert!(invocation(&["install", "--version", "1.2.3"]).is_ok());
    for command in ["restart", "uninstall"] {
        assert_eq!(
            invocation(&["--non-interactive", command]),
            Err(CliFailure::ConfirmationRequired)
        );
        assert!(invocation(&[command, "--assess"]).is_ok());
        assert!(invocation(&["--non-interactive", command, "--confirm", "original_1"]).is_ok());
        assert_eq!(invocation(&[command, "--force"]), Err(CliFailure::Usage));
    }
    for args in [
        vec!["install", "--executable", "/tmp/server"],
        vec!["install", "--version", "01.2.3"],
        vec!["install", "--version", "18446744073709551616.0.0"],
        vec!["install", "--build", "release"],
        vec!["uninstall", "--delete-data"],
    ] {
        assert_eq!(invocation(&args), Err(CliFailure::Usage));
    }
}

#[test]
fn install_accepts_only_a_complete_explicit_local_artifact_source() {
    assert!(
        invocation(&[
            "install",
            "--feed",
            "/local/candidate.json",
            "--archive",
            "/local/candidate.tar.gz",
        ])
        .is_ok()
    );
    for args in [
        vec!["install", "--feed", "/local/candidate.json"],
        vec!["install", "--archive", "/local/candidate.tar.gz"],
        vec![
            "install",
            "--feed",
            "https://example.com/feed.json",
            "--archive",
            "/local/archive",
        ],
        vec![
            "install",
            "--feed",
            "/local/feed",
            "--archive",
            "file:///local/archive",
        ],
        vec!["install", "--feed", "", "--archive", "/local/archive"],
    ] {
        assert_eq!(invocation(&args), Err(CliFailure::Usage));
    }
}
