use spaceterm_server::{StartupFailure, production_startup};

#[test]
fn production_startup_fails_closed_without_native_credentials() {
    assert_eq!(
        production_startup(),
        Err(StartupFailure::NativeCredentialDeliveryUnavailable)
    );
}

#[test]
fn shipped_entry_point_exits_unsuccessfully_with_bounded_classification() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_spaceterm-server"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"native credential delivery unavailable\n");
}
