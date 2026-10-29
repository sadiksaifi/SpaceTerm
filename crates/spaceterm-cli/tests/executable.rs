use std::process::{Command, Output};

fn spaceterm(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spaceterm"))
        .args(args)
        .env_clear()
        .output()
        .unwrap()
}

#[test]
fn help_shows_the_visible_tree_without_hidden_commands() {
    let output = spaceterm(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for visible in [
        "server",
        "connection",
        "list",
        "workspace",
        "tab",
        "pane",
        "worktree",
    ] {
        assert!(help.contains(visible), "{visible}");
    }
    assert!(!help.contains("completion"));
    let output = spaceterm(&["server", "--help"]);
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(!help.contains("provision"));
}

#[test]
fn unimplemented_commands_exit_through_the_contract() {
    for (args, command) in [
        (&["server", "run"][..], "server.run"),
        (&["server", "provision", "build-info"], "server.provision"),
        (&["pane", "run", "true"], "pane.run"),
        (&["pane", "send", "x"], "pane.send"),
        (&["pane", "read"], "pane.read"),
        (&["pane", "release"], "pane.release"),
        (&["completion", "zsh"], "completion"),
        // The server commands Stream F implements are covered by its own tests, and running them
        // here would act on this account's server.
    ] {
        let output = spaceterm(&[args, &["--json"]].concat());
        assert_eq!(output.status.code(), Some(5), "{args:?}");
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["schema"], "spaceterm.cli.v1");
        assert_eq!(envelope["command"], command);
        assert_eq!(envelope["status"], "error");
        assert_eq!(envelope["error"]["code"], "unsupported");
        let output = spaceterm(args);
        assert_eq!(output.status.code(), Some(5), "{args:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            "error: This operation isn\u{2019}t supported. (unsupported)\n".as_bytes()
        );
    }
}

#[test]
fn old_grammar_and_bad_values_are_usage_errors_without_the_value() {
    let identity = "private-identity-must-not-appear";
    for args in [
        &["serve", identity][..],
        &["service-status", identity],
        &["--build-info"],
        &["list", "--timeout", identity],
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
fn version_prints_the_build_version() {
    let output = spaceterm(&["--version"]);
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .starts_with("spaceterm ")
    );
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
    let output = spaceterm(&["--server", "remote-fixture", "server", "start", "--json"]);
    assert_eq!(output.status.code(), Some(5));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "local_only");
    assert_eq!(envelope["server"], serde_json::Value::Null);
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
