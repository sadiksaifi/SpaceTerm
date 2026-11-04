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
    for args in [&["--help"][..], &["machine", "--help"], &["--version"]] {
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
fn machine_usage_is_validated_before_input_or_native_credentials() {
    for (args, exit, code, argument) in [
        (
            &["machine", "add", "--json"][..],
            3,
            "interaction_required",
            None,
        ),
        (
            &["--machine", "remote-fixture", "machine", "list", "--json"],
            2,
            "invalid_arguments",
            Some("--machine"),
        ),
        (
            &[
                "machine",
                "add",
                "--trust",
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
fn local_only_commands_reject_a_paired_machine_before_contacting_it() {
    for (args, command) in [
        (&["setup"][..], "setup"),
        (&["start"][..], "start"),
        (&["stop"][..], "stop"),
        (&["stop", "--confirm", "c1-00"][..], "stop"),
        (&["pair"][..], "pair"),
        (&["unpair", "id:9c1d44e0"][..], "unpair"),
        (&["end-all-sessions"][..], "end-all-sessions"),
        (&["uninstall"][..], "uninstall"),
        (&["permissions"][..], "permissions"),
        (
            &["permissions", "request", "microphone"][..],
            "permissions.request",
        ),
        (
            &["permissions", "settings", "accessibility"][..],
            "permissions.settings",
        ),
        (&["internal", "build-info"][..], "internal.build-info"),
        (&["open", "notes.md"][..], "open"),
        (&["show"][..], "show"),
        (&["diagnostics"][..], "diagnostics"),
    ] {
        let mut argv = vec!["--machine", "remote-fixture"];
        argv.extend(args);
        argv.push("--json");
        let output = spaceterm(&argv);
        assert_eq!(output.status.code(), Some(5), "{args:?}");
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["error"]["code"], "local_only");
        assert_eq!(envelope["command"], command);
        assert_eq!(envelope["machine"], serde_json::Value::Null);
    }
}

#[test]
fn remote_admin_verbs_and_work_commands_reach_machine_resolution() {
    for (args, command) in [
        (&["status"][..], "status"),
        (&["restart"][..], "restart"),
        (&["restart", "--confirm", "c1-00"][..], "restart"),
        (&["update"][..], "update"),
        (
            &["update", "--archive", "a.tar", "--signature", "a.sig"][..],
            "update",
        ),
        (&["network"][..], "network"),
        (&["network", "off", "--confirm", "c1-00"][..], "network"),
        (&["list"][..], "list"),
        (
            &["workspace", "create", "api", "--directory", "/srv/api"][..],
            "workspace.create",
        ),
        (&["worktree", "create", "feature"][..], "worktree.create"),
        (&["tab", "create"][..], "tab.create"),
        (&["pane", "split"][..], "pane.split"),
        (&["pane", "read"][..], "pane.read"),
    ] {
        let mut argv = vec!["--machine", "remote-fixture"];
        argv.extend(args);
        argv.push("--json");
        let output = spaceterm(&argv);
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["command"], command);
        assert_eq!(envelope["error"]["code"], "not_found", "{args:?}");
        assert_eq!(envelope["error"]["details"]["level"], "machine", "{args:?}");
        assert_eq!(output.status.code(), Some(4), "{args:?}");
    }
}

#[test]
fn spaceterm_machine_selects_the_route_like_the_flag() {
    let output = Command::new(env!("CARGO_BIN_EXE_spaceterm"))
        .args(["stop", "--json"])
        .env_clear()
        .env("SPACETERM_MACHINE", "remote-fixture")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "local_only");
}

#[test]
fn remote_work_paths_are_rejected_before_route_or_credential_access() {
    let output = spaceterm(&[
        "--machine",
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
fn machine_rename_help_and_parse_failures_describe_only_this_cli() {
    let help = spaceterm(&["machine", "rename", "--help"]);
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("for this CLI only"));
    assert!(help.contains("<OLD> <NEW>"));
    let output = spaceterm(&["machine", "rename", "private-name", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["command"], "machine.rename");
    assert_eq!(envelope["error"]["details"]["argument"], "<NEW>");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-name"));
}

#[test]
fn workspace_pin_rejects_relative_remote_paths_before_route_or_credential_access() {
    for json in [false, true] {
        let mut args = vec![
            "--machine",
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
