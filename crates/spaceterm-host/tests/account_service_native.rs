#![cfg(any(target_os = "macos", target_os = "linux"))]

use spaceterm_host::account_service::{NativeServiceProcessTable, TrustedAccountFacts};

#[test]
fn native_observation_retains_child_pid_account_executable_and_start_identity() {
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let result = NativeServiceProcessTable::current()
        .unwrap()
        .observe(child.id());
    let _ = child.kill();
    child.wait().unwrap();
    let process = result
        .unwrap()
        .expect("the child must be observed before it exits");
    assert_eq!(process.pid(), child.id());
    assert_eq!(process.uid(), TrustedAccountFacts::current().unwrap().uid());
    assert_ne!(process.start_identity(), 0);
    assert_eq!(
        process.executable(),
        std::path::Path::new("/bin/sleep").canonicalize().unwrap()
    );
    assert!(!format!("{process:?}").contains("sleep"));
    assert_eq!(
        NativeServiceProcessTable::current()
            .unwrap()
            .observe(child.id())
            .unwrap(),
        None
    );
}

#[test]
fn trusted_account_facts_match_native_password_record() {
    let facts = TrustedAccountFacts::current().unwrap();
    assert!(facts.home().is_absolute());
    assert!(facts.login_shell().is_absolute());
    assert_ne!(facts.uid(), 0);
    assert_eq!(format!("{facts:?}"), "TrustedAccountFacts(..)");
}

#[test]
fn trusted_account_ignores_caller_home_and_shell() {
    if std::env::var_os("SPACETERM_ACCOUNT_TEST_CHILD").is_some() {
        let facts = TrustedAccountFacts::current().unwrap();
        assert_ne!(facts.home(), std::path::Path::new("/untrusted-caller-home"));
        assert_ne!(
            facts.login_shell(),
            std::path::Path::new("/untrusted-caller-shell")
        );
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "trusted_account_ignores_caller_home_and_shell"])
        .env("SPACETERM_ACCOUNT_TEST_CHILD", "1")
        .env("HOME", "/untrusted-caller-home")
        .env("SHELL", "/untrusted-caller-shell")
        .status()
        .unwrap();
    assert!(status.success());
}

#[cfg(target_os = "linux")]
#[test]
fn native_observation_preserves_a_deleted_child_executable_identity() {
    // Keep executable creation out of concurrent tests' fork windows. A child
    // temporarily inheriting the writable copy can otherwise make exec return ETXTBSY.
    if std::env::var_os("SPACETERM_DELETED_EXECUTABLE_TEST_CHILD").is_none() {
        let status = spaceterm_host::process::spawn(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "native_observation_preserves_a_deleted_child_executable_identity",
                ])
                .env("SPACETERM_DELETED_EXECUTABLE_TEST_CHILD", "1"),
        )
        .unwrap()
        .wait()
        .unwrap();
        assert!(status.success());
        return;
    }
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().unwrap();
    let executable = root.path().join("owned-sleep");
    std::fs::copy("/bin/sleep", &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = std::process::Command::new(&executable)
        .arg("30")
        .spawn()
        .unwrap();
    std::fs::remove_file(&executable).unwrap();
    let observed = NativeServiceProcessTable::current()
        .unwrap()
        .observe(child.id());
    let _ = child.kill();
    child.wait().unwrap();
    let observed = observed.unwrap().unwrap();
    assert_eq!(observed.pid(), child.id());
    assert!(
        observed
            .executable()
            .as_os_str()
            .as_encoded_bytes()
            .ends_with(b" (deleted)")
    );
}

#[cfg(feature = "test-support")]
#[test]
fn native_current_factory_honors_durable_stop_without_loading_keys() {
    use spaceterm_host::{
        account_service::{
            AccountStartupAdmission, ServiceSupervisor, test_support::AccountServiceFixture,
        },
        startup::DesiredServerState,
    };
    let root = tempfile::tempdir().unwrap();
    let build = ServiceSupervisor::current(None)
        .unwrap()
        .key()
        .build_identity;
    let fixture = AccountServiceFixture::new(root.path(), build).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let pin = registration.server_pin();
    std::fs::remove_file(registration.data_directory().join("identity")).unwrap();
    std::fs::remove_dir(registration.organization_directory()).unwrap();
    for _ in 0..2 {
        let native = fixture.native_current().unwrap();
        assert!(matches!(
            native.admit_startup(),
            Ok(AccountStartupAdmission::Stopped)
        ));
        assert_eq!(native.discover().unwrap().unwrap().server_pin(), pin);
    }
    assert!(!registration.data_directory().join("identity").exists());
    assert!(!registration.organization_directory().exists());
}
