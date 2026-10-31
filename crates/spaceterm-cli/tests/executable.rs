use std::process::{Command, Output};

fn spaceterm(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spaceterm"))
        .args(args)
        .env_clear()
        .output()
        .unwrap()
}

#[test]
fn bad_values_are_usage_errors_without_the_value() {
    let identity = "private-identity-must-not-appear";
    for args in [
        &["list", "--timeout", identity][..],
        &["--private-flag-must-not-appear", "list"],
    ] {
        let output = spaceterm(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.starts_with("error: "), "{stderr}");
        assert!(!stderr.contains("must-not-appear"), "{stderr}");
    }
    let output = spaceterm(&["list", "--json", "--timeout", identity]);
    assert_eq!(output.status.code(), Some(2));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["command"], "list");
    assert_eq!(envelope["error"]["code"], "invalid_arguments");
    assert_eq!(envelope["error"]["details"]["argument"], "--timeout");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("must-not-appear"));
}

#[test]
fn help_and_version_exit_successfully_on_stdout() {
    for args in [&["--help"][..], &["server", "--help"], &["--version"]] {
        let output = spaceterm(args);
        assert!(output.status.success(), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
        assert!(!output.stdout.is_empty(), "{args:?}");
        if args == ["--version"] {
            assert!(
                String::from_utf8(output.stdout)
                    .unwrap()
                    .starts_with("spaceterm ")
            );
        }
    }
}

#[test]
fn connection_usage_is_validated_before_input_or_native_credentials() {
    for (args, exit, code, argument) in [
        (
            &["connection", "add", "--json"][..],
            3,
            "interaction_required",
            None,
        ),
        (
            &["--server", "remote-fixture", "connection", "list", "--json"],
            2,
            "invalid_arguments",
            Some("--server"),
        ),
        (
            &[
                "connection",
                "add",
                "--join",
                "--address",
                "private-invalid/address",
                "--json",
            ],
            2,
            "invalid_arguments",
            Some("--address"),
        ),
    ] {
        let output = spaceterm(args);
        assert_eq!(output.status.code(), Some(exit));
        assert!(output.stderr.is_empty());
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["error"]["code"], code);
        if let Some(argument) = argument {
            assert_eq!(envelope["error"]["details"]["argument"], argument);
        }
        assert!(!String::from_utf8_lossy(&output.stdout).contains("private-invalid"));
    }
}

#[test]
fn machine_only_commands_reject_remote_routes_before_contacting_a_server() {
    for (args, command) in [
        (&["server", "start"][..], "server.start"),
        (&["server", "permissions"][..], "server.permissions"),
        (
            &["server", "permissions", "request", "microphone"][..],
            "server.permissions.request",
        ),
        (
            &["server", "permissions", "settings", "device-control"][..],
            "server.permissions.settings",
        ),
    ] {
        let mut argv = vec!["--server", "remote-fixture"];
        argv.extend(args);
        argv.push("--json");
        let output = spaceterm(&argv);
        assert_eq!(output.status.code(), Some(5), "{args:?}");
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["error"]["code"], "local_only");
        assert_eq!(envelope["command"], command);
        assert_eq!(envelope["server"], serde_json::Value::Null);
    }
}

#[test]
fn remote_work_paths_are_rejected_before_route_or_credential_access() {
    let output = spaceterm(&[
        "--server",
        "remote-fixture",
        "workspace",
        "create",
        "api",
        "--directory",
        "private-relative-directory",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "invalid_arguments");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-relative-directory"));
}

#[test]
fn connection_rename_help_and_parse_failures_describe_only_cli_connections() {
    let help = spaceterm(&["connection", "rename", "--help"]);
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("CLI's saved connection only"));
    assert!(help.contains("<OLD> <NEW>"));
    let output = spaceterm(&["connection", "rename", "private-name", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["command"], "connection.rename");
    assert_eq!(envelope["error"]["details"]["argument"], "<NEW>");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-name"));
}

#[test]
fn workspace_pin_rejects_relative_remote_paths_before_route_or_credential_access() {
    for json in [false, true] {
        let mut args = vec![
            "--server",
            "remote-fixture",
            "workspace",
            "pin",
            "private-relative-directory",
        ];
        if json {
            args.push("--json");
        }
        let output = spaceterm(&args);
        assert_eq!(output.status.code(), Some(2));
        if json {
            let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(envelope["command"], "workspace.pin");
            assert_eq!(envelope["error"]["code"], "invalid_arguments");
            assert_eq!(envelope["error"]["details"]["argument"], "<DIR>");
            assert!(output.stderr.is_empty());
        } else {
            assert!(output.stdout.is_empty());
            assert_eq!(
                String::from_utf8_lossy(&output.stderr),
                include_str!("../golden/work_workspace_pin_invalid.stderr")
            );
        }
        assert!(!String::from_utf8_lossy(&output.stdout).contains("private-relative-directory"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-relative-directory"));
    }
}
