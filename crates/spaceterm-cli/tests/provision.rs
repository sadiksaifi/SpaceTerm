use std::process::{Command, Output, Stdio};

fn spaceterm(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spaceterm"))
        .args(args)
        .env_clear()
        .output()
        .unwrap()
}

#[test]
fn source_build_provision_check_uses_compiled_resources() {
    let output = spaceterm(&["server", "provision", "check"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"Server checks passed.\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn hidden_build_metadata_is_available_without_environment_or_display() {
    let output = spaceterm(&["server", "provision", "build-info"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(info["schema"], 1);
    assert_eq!(info["identity"], env!("SPACETERM_CLI_IDENTITY"));
    assert_eq!(info["target"], env!("SPACETERM_CLI_TARGET"));
    assert_eq!(info["version"], env!("SPACETERM_CLI_VERSION"));
    assert_eq!(info["commit"], env!("SPACETERM_CLI_COMMIT"));
    assert_eq!(info["protocol_major"], spaceterm_server::protocol::MAJOR);
    assert_eq!(info.as_object().unwrap().len(), 7, "{info}");
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
