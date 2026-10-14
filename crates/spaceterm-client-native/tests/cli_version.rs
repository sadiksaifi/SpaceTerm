#![cfg(feature = "cli")]

use std::process::Command;

use spaceterm_distribution::BuildVersion;

#[test]
fn headless_version_reports_a_build_version_in_text_and_json() {
    let executable = env!("CARGO_BIN_EXE_spaceterm-cli");
    let text = Command::new(executable).arg("--version").output().unwrap();
    assert!(text.status.success());
    assert!(text.stderr.is_empty());
    let text = String::from_utf8(text.stdout).unwrap();
    let version = text
        .strip_prefix("spaceterm-cli ")
        .and_then(|text| text.strip_suffix('\n'))
        .expect("version includes the executable name and a version");
    assert!(BuildVersion::parse(version).is_ok());

    let json = Command::new(executable)
        .args(["--json", "--version"])
        .output()
        .unwrap();
    assert!(json.status.success());
    assert!(json.stderr.is_empty());
    let record: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(record["schema"], 1);
    assert_eq!(record["version"], text.trim_end());
}
