//! Build the real trust readers with a hostile inherited environment in a disposable checkout.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn inherited_environment_cannot_replace_tracked_core_or_desktop_trust() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture =
        Fixture(std::env::temp_dir().join(format!("spaceterm-trust-build-{}", std::process::id())));
    fs::create_dir(&fixture.0).unwrap();
    for directory in [
        "build",
        "src",
        "packaging",
        "crates/spaceterm-core/src/protocol",
    ] {
        fs::create_dir_all(fixture.0.join(directory)).unwrap();
    }
    for path in [
        "build.rs",
        "build/identity.rs",
        "build/release_notes.rs",
        "packaging/update-trust.json",
        "crates/spaceterm-core/src/protocol/version.rs",
    ] {
        fs::copy(root.join(path), fixture.0.join(path)).unwrap();
    }
    fs::write(fixture.0.join("Cargo.toml"), concat!(
        "[package]\nname=\"spaceterm-trust-fixture\"\nversion=\"0.0.0\"\nedition=\"2024\"\n",
        "[workspace]\n[build-dependencies]\nserde_json=\"1.0\"\n",
        "[dependencies]\nbase64=\"=0.22.1\"\nring=\"=0.17.14\"\n",
        "serde={version=\"1.0\",features=[\"derive\"]}\nserde_json=\"1.0\"\nthiserror=\"2.0.17\"\n",
    )).unwrap();
    let trust_module = root.join("crates/spaceterm-core/src/update/trust.rs");
    fs::write(fixture.0.join("src/main.rs"), format!(r#"
#[path = {trust_module:?}]
mod trust;
fn main() {{
    let tracked = include_bytes!("../packaging/update-trust.json");
    let expected: serde_json::Value = serde_json::from_slice(tracked).unwrap();
    let desktop: serde_json::Value = serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/desktop-build-info.json"))).unwrap();
    println!("{{}}", serde_json::json!({{
        "desktop": desktop["update_trust"] == expected,
        "core": trust::UpdateTrust::release() == trust::UpdateTrust::parse(tracked),
    }}));
}}
"#)).unwrap();
    for arguments in [
        vec!["init", "--quiet"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgSign=false",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(arguments)
                .current_dir(&fixture.0)
                .status()
                .unwrap()
                .success()
        );
    }
    let mut substituted: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("packaging/update-trust.json")).unwrap())
            .unwrap();
    substituted["backup"] =
        serde_json::Value::String("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=".into());
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["run", "--quiet", "--offline", "--manifest-path"])
        .arg(fixture.0.join("Cargo.toml"))
        .env(
            "CARGO_TARGET_DIR",
            std::env::var_os("CARGO_TARGET_DIR")
                .map_or_else(|| root.join("target"), PathBuf::from)
                .join("trust-fixture"),
        )
        .env(
            "SPACETERM_RELEASE_UPDATE_TRUST_JSON",
            substituted.to_string(),
        )
        .env_remove("SPACETERM_RELEASE_TAG")
        .env_remove("SPACETERM_RELEASE_NOTES")
        .env_remove("SPACETERM_PACKAGED")
        .env_remove("SPACETERM_RELEASE_FLOOR")
        .env_remove("SPACETERM_DESKTOP_FLOOR")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fixture build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({"desktop":true,"core":true})
    );
}
