use std::process::Command;

fn server() -> Command {
    Command::new(env!("CARGO_BIN_EXE_spaceterm-server"))
}

#[test]
fn distribution_metadata_precedes_account_storage_and_credential_effects() {
    let output = server()
        .arg("--distribution-metadata")
        .env("HOME", "/unavailable/spaceterm-account")
        .env("XDG_DATA_HOME", "/unavailable/spaceterm-data")
        .env("SPACETERM_SERVER_BUILD", "release")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(output.stdout.len() <= 4096);
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(metadata["component"], "server");
    assert_eq!(metadata["build_identity"], env!("SPACETERM_SERVER_BUILD"));
    assert_eq!(metadata["protocol_majors"], serde_json::json!([1, 2]));
    assert_eq!(metadata["storage"]["write"], 1);
}

#[test]
fn empty_invocation_prints_usage_without_starting_the_account_service() {
    let output = server().output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"usage: spaceterm-server serve | --distribution-metadata\n"
    );
}

#[test]
fn fixed_service_argv_selects_startup_and_empty_argv_is_rejected() {
    use spaceterm_server::deployment::ServerInvocation;
    assert_eq!(
        ServerInvocation::parse(["serve".into()]),
        Ok(ServerInvocation::Serve)
    );
    assert!(ServerInvocation::parse([]).is_err());
}

#[test]
fn entry_point_rejects_every_unrecognized_or_extra_operand_before_startup() {
    for operands in [
        vec!["--distribution-metadata", "extra"],
        vec!["--distribution-metadata", "--distribution-metadata"],
        vec!["serve", "extra"],
        vec!["--server-key", "credential"],
        vec!["--native-receiver"],
        vec!["--"],
    ] {
        let output = server().args(operands).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            b"usage: spaceterm-server serve | --distribution-metadata\n"
        );
    }
}

#[test]
fn fixed_service_serve_operand_reports_only_a_bounded_account_startup_classification() {
    let output = server().arg("serve").output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    // A real installed registration can change the startup stage, but never expose
    // a native error, path, credential or the removed receiver classification.
    assert!(matches!(
        output.stderr.as_slice(),
        b"server account unavailable\n"
            | b"server registration missing\n"
            | b"server deliberately stopped\n"
            | b"server key unavailable\n"
            | b"server storage unavailable\n"
            | b"server runtime composition unavailable\n"
    ));
}
