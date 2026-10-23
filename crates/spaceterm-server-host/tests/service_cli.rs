use spaceterm_server::ServerError;
use std::{ffi::OsString, os::unix::ffi::OsStringExt, process::Command};

fn assert_protocol_refusal(args: &[OsString]) {
    let output = Command::new(env!("CARGO_BIN_EXE_spaceterm-server"))
        .args(args)
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        format!("{}\n", ServerError::Protocol).as_bytes()
    );
}

#[test]
fn service_controls_refuse_foreign_and_malformed_identities() {
    for identity in [
        "spaceterm",
        "spaceterm-preflight",
        "spaceterm-development",
        "private-identity-must-not-appear-in-errors",
    ] {
        if identity == env!("SPACETERM_SERVER_IDENTITY") {
            continue;
        }
        for command in [
            "service-status",
            "service-start",
            "service-stop",
            "service-restart",
            "service-uninstall",
            "service-enable-linger",
            "service-serve",
            "ssh-connect",
            "ssh-discover",
            "ssh-prepare",
            "ssh-enable-linger",
            "ssh-enable-linger-password",
        ] {
            assert_protocol_refusal(&[command.into(), identity.into()]);
            assert_protocol_refusal(&[command.into(), identity.into(), "--confirm".into()]);
        }
        assert_protocol_refusal(&[
            "service-install".into(),
            identity.into(),
            "/private/installation-must-not-appear-in-errors".into(),
        ]);
    }
    assert_protocol_refusal(&[
        "service-status".into(),
        OsString::from_vec(b"private-non-utf8-identity-\xff".to_vec()),
    ]);
}

#[test]
fn malformed_service_invocations_return_bounded_errors() {
    let identity = env!("SPACETERM_SERVER_IDENTITY");
    for args in [
        vec!["service-status"],
        vec!["service-status", "--confirm", identity],
        vec!["--confirm", "service-status", identity],
        vec!["service-status", identity, "--confirm"],
        vec!["service-status", identity, "--private-unknown-flag"],
        vec!["service-status", identity, "unexpected", "arguments"],
        vec!["service-automatic-start", identity],
        vec!["service-automatic-start", identity, "true"],
        vec!["service-automatic-start", identity, "off", "--confirm"],
        vec!["service-private-unknown-command", identity],
        vec!["service-install", identity],
        vec!["service-install", identity, "--confirm"],
        vec![
            "service-install",
            identity,
            "/private/retained-installation",
            "--confirm",
        ],
        vec!["service-serve", identity, "--confirm"],
        vec!["serve", identity, "--confirm"],
        vec!["status", identity, "--confirm"],
        vec!["setup-state", identity, "--confirm"],
        vec!["ssh-connect", identity, "--confirm"],
        vec!["ssh-connect", identity, "/private/not-an-endpoint"],
        vec!["ssh-discover", identity, "--confirm"],
        vec!["ssh-prepare", identity, "--confirm"],
        vec!["ssh-enable-linger", identity, "--confirm"],
        vec!["ssh-enable-linger-password", identity, "--confirm"],
        vec![
            "ssh-enable-linger-password",
            identity,
            "not-a-password-argument",
        ],
        vec!["ssh-install", identity],
        vec!["ssh-install", identity, "--confirm", "signature", "digest"],
        vec!["--help", identity],
        vec!["--version", identity],
        vec!["--build-info", identity],
        vec!["--check", identity],
    ] {
        let args: Vec<_> = args.into_iter().map(OsString::from).collect();
        assert_protocol_refusal(&args);
    }
    assert_protocol_refusal(&[
        OsString::from_vec(b"service-private-non-utf8-\xff".to_vec()),
        identity.into(),
    ]);
}
