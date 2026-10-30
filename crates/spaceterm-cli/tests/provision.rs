use std::process::{Command, Output, Stdio};

fn spaceterm(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spaceterm"))
        .args(args)
        .env_clear()
        .output()
        .unwrap()
}

#[test]
fn hidden_build_metadata_is_available_without_environment_or_display() {
    let output = spaceterm(&["server", "provision", "build-info"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(info["schema"], 1);
    assert_eq!(info["identity"], env!("SPACETERM_SERVER_IDENTITY"));
    assert_eq!(info["target"], env!("SPACETERM_SERVER_TARGET"));
    assert_eq!(info["version"], env!("SPACETERM_SERVER_VERSION"));
    assert_eq!(info["commit"], env!("SPACETERM_SERVER_COMMIT"));
    assert_eq!(info["protocol_major"], spaceterm_server::protocol::MAJOR);
    assert!(
        info["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "workspace_policy")
    );
}

#[test]
fn removed_arguments_fail_before_server_access_without_echoing_the_input() {
    let identity = "private-identity-must-not-appear";
    for args in [
        vec!["serve", identity],
        vec!["development-serve", identity],
        vec!["service-serve", identity],
        vec!["status", identity],
        vec!["setup-state", identity],
        vec!["service-status", identity],
        vec!["service-install", identity, "/private/path-must-not-appear"],
        vec!["ssh-discover", identity],
        vec!["setup-start", identity],
        vec!["setup-direct", identity, "127.0.0.1:9231"],
        vec!["ssh-enable-linger-password", identity],
        vec!["--build-info"],
        vec!["--check"],
        vec!["server", "provision", "return"],
        vec!["server", "provision", "recover"],
        vec!["server", "provision", "network"],
        vec!["server", "provision", "automatic-start"],
        vec!["server", "provision", "linger"],
        vec!["server", "pair", "--qr"],
        vec!["server", "run", "--identity", identity],
        vec!["server", "run", "--service", "--owned"],
    ] {
        let output = spaceterm(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("must-not-appear"));
    }
}

#[test]
fn pairing_json_keeps_the_interaction_required_contract_without_a_qr() {
    let output = spaceterm(&["server", "pair", "--json"]);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stderr.is_empty());
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../golden/server_pair_interaction_required.json"
    ))
    .unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn server_help_hides_internal_steps_and_gives_the_exact_uninstall_line() {
    let output = spaceterm(&["server", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(!help.contains("provision"));
    assert!(!help.contains("service-run"));
    assert!(help.contains("curl -fsSL https://github.com/sadiksaifi/SpaceTerm/releases/latest/download/install.sh | sh -s -- --headless --uninstall"));
}

#[test]
fn completions_are_generated_without_opening_a_server() {
    let output = spaceterm(&["completion", "zsh"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("#compdef spaceterm")
    );
}

#[test]
fn a_broken_stdout_pipe_exits_141_without_a_message() {
    use std::os::{fd::OwnedFd, unix::net::UnixStream};
    for args in [
        &["--help"][..],
        &["server", "provision", "build-info"],
        &["list", "--json"],
        &["completion", "zsh"],
    ] {
        let (writer, reader) = UnixStream::pair().unwrap();
        drop(reader);
        let output = Command::new(env!("CARGO_BIN_EXE_spaceterm"))
            .args(args)
            .env_clear()
            .stdout(Stdio::from(OwnedFd::from(writer)))
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(141), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
    }
}
