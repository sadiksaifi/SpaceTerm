#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::{fs, process::Command};

use spaceterm_host::{
    account_service::{ServiceFailure, test_support::AccountServiceFixture},
    startup::{BuildIdentity, DesiredServerState},
};
use spaceterm_server::{
    StartupFailure,
    account_startup::{AccountStatus, account_status},
};

#[test]
fn status_reports_absent_registration_without_initializing_account_storage() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    assert_eq!(
        account_status(&fixture.reopen()).unwrap(),
        AccountStatus::Missing {
            build_identity: BuildIdentity::Development,
        }
    );
    assert!(!fixture.anchor().exists());
    assert!(!fixture.data_directory().exists());
}

#[test]
fn status_reports_retained_intent_without_reading_identity_or_organization() {
    for desired_state in [DesiredServerState::Running, DesiredServerState::Stopped] {
        let root = tempfile::tempdir().unwrap();
        let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        fixture.initialize(desired_state).unwrap();
        fs::remove_file(fixture.data_directory().join("identity")).unwrap();
        fs::remove_dir(fixture.data_directory().join("organization")).unwrap();
        let before = fs::read(fixture.anchor().join("registration")).unwrap();
        assert_eq!(
            account_status(&fixture.reopen()).unwrap(),
            AccountStatus::Registered {
                build_identity: BuildIdentity::Development,
                desired_state,
            }
        );
        assert_eq!(
            fs::read(fixture.anchor().join("registration")).unwrap(),
            before
        );
        assert!(!fixture.data_directory().join("identity").exists());
        assert!(!fixture.data_directory().join("organization").exists());
    }
}

#[test]
fn status_entry_point_accepts_fixed_bootstrap_flags_and_rejects_extra_operands() {
    use spaceterm_server::deployment::ServerInvocation;
    for arguments in [
        vec!["status"],
        vec!["--json", "status"],
        vec!["--non-interactive", "status"],
        vec!["--json", "--non-interactive", "status"],
        vec!["--non-interactive", "--json", "status"],
    ] {
        assert_eq!(
            ServerInvocation::parse(arguments.into_iter().map(Into::into)),
            Ok(ServerInvocation::Status)
        );
    }
    for arguments in [
        vec!["status", "extra"],
        vec!["--json", "--json", "status"],
        vec!["--json", "--non-interactive", "status", "serve"],
        vec!["--json", "serve"],
        vec!["--non-interactive", "serve"],
        vec!["--non-interactive", "--non-interactive", "status"],
        vec!["--json", "--help"],
        vec!["--non-interactive", "--version"],
        vec!["--json", "--distribution-metadata"],
        vec!["status", "--json"],
        vec!["status", "--request", "00000000000000000000000000000001"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_spaceterm-server"))
            .args(arguments)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn status_does_not_replace_malformed_unknown_or_foreign_registration() {
    for kind in ["malformed", "schema", "account", "build"] {
        let root = tempfile::tempdir().unwrap();
        let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        fixture.initialize(DesiredServerState::Stopped).unwrap();
        let path = fixture.anchor().join("registration");
        let mut registration: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let (bytes, failure) = match kind {
            "malformed" => (
                b"invalid registration".to_vec(),
                ServiceFailure::InvalidRecord,
            ),
            "schema" => {
                registration["schema"] = serde_json::json!(2);
                (
                    serde_json::to_vec(&registration).unwrap(),
                    ServiceFailure::UnsupportedSchema,
                )
            }
            "account" => {
                registration["key"]["account_uid"] = serde_json::json!(0);
                (
                    serde_json::to_vec(&registration).unwrap(),
                    ServiceFailure::IdentityMismatch,
                )
            }
            "build" => {
                registration["key"]["build_identity"] = serde_json::json!("preflight");
                (
                    serde_json::to_vec(&registration).unwrap(),
                    ServiceFailure::IdentityMismatch,
                )
            }
            _ => unreachable!(),
        };
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            account_status(&fixture.reopen()),
            Err(StartupFailure::Account(failure))
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(!fixture.anchor().join("requests").exists());
    }
}
