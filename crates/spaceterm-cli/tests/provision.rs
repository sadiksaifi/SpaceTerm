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
    let output = spaceterm(&["internal", "check"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"SpaceTerm Core checks passed.\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn hidden_build_metadata_is_available_without_environment_or_display() {
    let output = spaceterm(&["internal", "build-info"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(info["schema"], 1);
    assert_eq!(
        info["stapled"], false,
        "compiled build metadata precedes archive stapling"
    );
    assert_eq!(info["identity"], env!("SPACETERM_CLI_IDENTITY"));
    assert_eq!(info["target"], env!("SPACETERM_CLI_TARGET"));
    assert_eq!(info["version"], env!("SPACETERM_CLI_VERSION"));
    assert_eq!(info["commit"], env!("SPACETERM_CLI_COMMIT"));
    assert_eq!(
        info["floor"],
        serde_json::Value::Null,
        "a build that names a commit has no Update Floor"
    );
    assert_eq!(
        info["protocols"],
        serde_json::to_value(spaceterm_core::update::facts::Protocols::CURRENT).unwrap()
    );
    assert_eq!(info.as_object().unwrap().len(), 9, "{info}");
}

#[test]
fn pairing_json_keeps_the_interaction_required_contract_without_a_qr() {
    let output = spaceterm(&["pair", "--json"]);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stderr.is_empty());
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../golden/pair_interaction_required.json")).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn help_hides_internal_steps_and_lists_completion() {
    let output = spaceterm(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(!help.contains("internal") && !help.contains("diagnostics"));
    assert!(help.contains("\n  completion "));
}

#[test]
fn completions_leave_out_hidden_commands() {
    let output = spaceterm(&["completion", "bash"]);
    assert!(output.status.success());
    let script = String::from_utf8(output.stdout).unwrap();
    assert!(script.contains("end-all-sessions"));
    assert!(!script.contains("ssh-discover") && !script.contains("internal"));
}

#[test]
fn completions_are_generated_without_opening_a_connection() {
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
        &["internal", "build-info"],
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
