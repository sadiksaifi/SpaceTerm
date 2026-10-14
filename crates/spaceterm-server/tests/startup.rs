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
fn help_and_version_use_the_metadata_version_without_account_effects() {
    let output = server().arg("--distribution-metadata").output().unwrap();
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let version = metadata["version"].as_str().unwrap();
    for argument in ["--help", "--version"] {
        let output = server()
            .arg(argument)
            .env("HOME", "/unavailable/spaceterm-account")
            .env("XDG_DATA_HOME", "/unavailable/spaceterm-data")
            .env("SPACETERM_SERVER_VERSION", "untrusted-runtime-version")
            .output()
            .unwrap();
        assert!(output.status.success(), "{argument}");
        assert!(output.stderr.is_empty());
        assert!(output.stdout.len() <= 4096);
        assert!(String::from_utf8(output.stdout).unwrap().contains(version));
    }
}

#[test]
fn empty_invocation_prints_usage_without_starting_the_account_service() {
    let output = server().output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        format!("{}\n", spaceterm_server::deployment::USAGE).into_bytes()
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
        vec!["--help", "serve"],
        vec!["--version", "serve"],
        vec!["serve", "extra"],
        vec!["--server-key", "credential"],
        vec!["--native-receiver"],
        vec!["--"],
        vec!["setup"],
        vec!["install"],
        vec!["--json", "--non-interactive", "setup"],
        vec!["setup", "--server-key", "credential"],
        vec!["setup", "--executable", "/untrusted/server"],
        vec![
            "owner-enrollment",
            "--request",
            "00000000000000000000000000000001",
        ],
    ] {
        let output = server().args(operands).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            format!("{}\n", spaceterm_server::deployment::USAGE).into_bytes()
        );
    }
}

#[test]
fn startup_failure_diagnostics_include_only_typed_inner_classifications() {
    use spaceterm_core::storage::StoreFailure;
    use spaceterm_distribution::DistributionFailure;
    use spaceterm_host::{
        account_service::ServiceFailure, server_keys::KeyStoreFailure, storage::StorageFailure,
    };
    use spaceterm_server::{
        StartupFailure,
        contracts::{ServerFailure, ServerFailureClass},
    };
    for (failure, expected) in [
        (
            StartupFailure::Account(ServiceFailure::ArtifactRejected),
            "server account unavailable: ArtifactRejected",
        ),
        (
            StartupFailure::Account(ServiceFailure::PairingRequired),
            "server account unavailable: PairingRequired",
        ),
        (
            StartupFailure::Key(KeyStoreFailure::StorageLocked),
            "server key unavailable: StorageLocked",
        ),
        (
            StartupFailure::Storage(StorageFailure::UnknownSchema {
                found: 12345,
                supported: 6789,
            }),
            "server storage unavailable: UnknownSchema",
        ),
        (
            StartupFailure::Store(StoreFailure::InvalidDurableState),
            "server storage unavailable: InvalidDurableState",
        ),
        (
            StartupFailure::Runtime(ServerFailure {
                class: ServerFailureClass::CleanupIncomplete,
                expected: Some(12345),
                actual: Some(6789),
                limit: Some(99999),
            }),
            "server runtime unavailable: CleanupIncomplete",
        ),
        (
            StartupFailure::Distribution(DistributionFailure::ManifestRejected),
            "server distribution metadata unavailable: ManifestRejected",
        ),
    ] {
        assert_eq!(failure.classification(), expected);
    }
}

#[test]
fn fixed_service_serve_operand_reports_only_a_bounded_account_startup_classification() {
    let output = server().arg("serve").output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    // A real installed registration can change the startup stage, but never expose
    // a native error, path, credential or the removed receiver classification.
    let diagnostic = std::str::from_utf8(&output.stderr).unwrap();
    assert!(diagnostic.len() <= 128);
    if matches!(
        diagnostic,
        "server registration missing\n" | "server deliberately stopped\n"
    ) {
        return;
    }
    let (stage, class) = diagnostic.trim_end_matches('\n').split_once(": ").unwrap();
    let allowed: &[&str] = match stage {
        "server account unavailable" => &[
            "RootAccount",
            "UnsupportedHost",
            "Missing",
            "StorageLocked",
            "StorageUnavailable",
            "UnsafeStorage",
            "InvalidRecord",
            "UnsupportedSchema",
            "IdentityMismatch",
            "ArtifactRejected",
            "IncompatibleStorage",
            "UnknownRequest",
            "RequestConflict",
            "RequestRetired",
            "StaleAssessment",
            "ConfirmationRequired",
            "AuthorizationDenied",
            "AuthorizationCancelled",
            "AuthorizationUnavailable",
            "SupervisorUnavailable",
            "LoginSessionUnavailable",
            "Suppressed",
            "LimitExceeded",
            "Pending",
            "PairingRequired",
            "CleanupUnresolved",
            "Indeterminate",
        ],
        "server key unavailable" => &[
            "Missing",
            "StorageLocked",
            "StorageUnavailable",
            "UnsafeStorage",
            "InvalidRecord",
            "IdentityMismatch",
            "AlreadyExists",
            "Conflict",
            "LimitExceeded",
            "Indeterminate",
        ],
        "server storage unavailable" => &[
            "Unavailable",
            "UnsafeStorage",
            "UnknownSchema",
            "InvalidState",
            "Io",
            "Locked",
            "InvalidDurableState",
            "RevisionConflict",
            "Exhausted",
            "InvalidTransaction",
            "IdentityMismatch",
            "RecoveryRequired",
        ],
        "server runtime unavailable" => &[
            "Authentication",
            "ScopeMismatch",
            "StaleAuthority",
            "Capacity",
            "Exhausted",
            "StorageUnavailable",
            "RecoveryRequired",
            "RuntimeUnavailable",
            "NativeUnavailable",
            "CleanupIncomplete",
        ],
        "server distribution metadata unavailable" => &[
            "Verification",
            "InvalidRecord",
            "UnsupportedSchema",
            "IdentityMismatch",
            "LimitExceeded",
            "ArchiveRejected",
            "ManifestRejected",
        ],
        _ => panic!("unexpected startup stage"),
    };
    assert!(
        allowed.contains(&class),
        "startup must print only an enum classification"
    );
    assert_eq!(diagnostic, format!("{stage}: {class}\n"));
}
