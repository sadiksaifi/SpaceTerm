use std::process::Command;

#[test]
fn server_reports_its_compiled_identity_and_refuses_other_namespaces() {
    let executable = env!("CARGO_BIN_EXE_spaceterm-server");
    let output = Command::new(executable)
        .arg("--build-info")
        .env_clear()
        .output()
        .unwrap();
    assert!(output.status.success());
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(info["identity"], env!("SPACETERM_SERVER_IDENTITY"));
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(head.status.success());
    assert_eq!(
        info["commit"],
        String::from_utf8(head.stdout).unwrap().trim()
    );
    for identity in ["spaceterm", "spaceterm-preflight", "spaceterm-development"] {
        if identity != env!("SPACETERM_SERVER_IDENTITY") {
            let refused = Command::new(executable)
                .args(["serve", identity])
                .env_clear()
                .output()
                .unwrap();
            assert_eq!(refused.status.code(), Some(1));
            assert!(refused.stdout.is_empty());
            assert_eq!(
                refused.stderr,
                b"The local server connection did not complete.\n"
            );
        }
    }
}
