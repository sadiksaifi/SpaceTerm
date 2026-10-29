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
fn every_command_exits_through_the_contract() {
    for (args, command) in [
        (&["server", "setup"][..], "server.setup"),
        (&["server", "pair"], "server.pair"),
        (&["server", "status"], "server.status"),
        (&["server", "start"], "server.start"),
        (&["server", "stop"], "server.stop"),
        (&["server", "restart"], "server.restart"),
        (&["server", "update"], "server.update"),
        (&["server", "unpair", "id:9c1d44e0"], "server.unpair"),
        (&["server", "run"], "server.run"),
        (&["server", "provision", "build-info"], "server.provision"),
        (&["connection", "add"], "connection.add"),
        (&["connection", "list"], "connection.list"),
        (&["connection", "remove", "penguin"], "connection.remove"),
        (&["list"], "list"),
        (&["workspace", "create", "api"], "workspace.create"),
        (&["workspace", "rename", "api-v2"], "workspace.rename"),
        (&["workspace", "kill"], "workspace.kill"),
        (&["tab", "create"], "tab.create"),
        (&["tab", "close"], "tab.close"),
        (&["pane", "split"], "pane.split"),
        (&["pane", "close"], "pane.close"),
        (&["pane", "run", "true"], "pane.run"),
        (&["pane", "send", "x"], "pane.send"),
        (&["pane", "read"], "pane.read"),
        (&["pane", "release"], "pane.release"),
        (&["worktree", "create", "feature-y"], "worktree.create"),
        (&["worktree", "remove"], "worktree.remove"),
        (&["completion", "zsh"], "completion"),
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
