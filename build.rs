#[path = "build/identity.rs"]
mod identity;
#[path = "build/release_notes.rs"]
mod release_notes;

use std::{env, path::PathBuf, process::Command};

fn desktop_provenance(
    root: &std::path::Path,
    identity: &identity::Identity,
    release: bool,
    packaged: bool,
) {
    for key in [
        "SPACETERM_CLI_SOURCE_STATE",
        "SPACETERM_RELEASE_FLOOR",
        "SPACETERM_RELEASE_APPLE_TEAM_ID",
        "SPACETERM_RELEASE_UPDATE_TRUST_JSON",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    println!("cargo:rerun-if-changed=packaging/update-trust.json");
    println!("cargo:rerun-if-changed=crates/spaceterm-core/src/protocol/version.rs");
    let source = env::var("SPACETERM_CLI_SOURCE_STATE").ok();
    assert!(
        source.as_ref().is_none_or(|s| s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))),
        "valid source snapshot digest"
    );
    let core_floor = env::var("SPACETERM_RELEASE_FLOOR").ok();
    let desktop_floor = env::var("SPACETERM_DESKTOP_FLOOR").ok();
    assert!(
        !(release && env::var_os("CARGO_FEATURE_DESKTOP").is_some())
            || (source.is_some() && core_floor.is_some() && desktop_floor.is_some()),
        "release desktops require source and both Update Floors"
    );
    let text = std::fs::read_to_string(root.join("crates/spaceterm-core/src/protocol/version.rs"))
        .expect("protocol source");
    let mut protocols = serde_json::Map::new();
    for (key, constant) in [
        ("work", "WORK"),
        ("service", "SERVICE"),
        ("desktop_channel", "DESKTOP_CHANNEL"),
    ] {
        let prefix = format!("pub const {constant}: ProtocolVersion = ProtocolVersion {{ major: ");
        let rest = text
            .lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .expect("protocol declaration");
        let (major, minor) = rest
            .strip_suffix(" };")
            .expect("protocol declaration")
            .split_once(", minor: ")
            .expect("protocol version");
        protocols.insert(key.into(), serde_json::json!({"major":major.parse::<u16>().expect("protocol major"), "minor":minor.parse::<u16>().expect("protocol minor")}));
    }
    let trust_bytes = env::var("SPACETERM_RELEASE_UPDATE_TRUST_JSON")
        .map(String::into_bytes)
        .unwrap_or_else(|_| {
            std::fs::read(root.join("packaging/update-trust.json")).expect("public update trust")
        });
    let trust: serde_json::Value =
        serde_json::from_slice(&trust_bytes).expect("public update trust JSON");
    let document = serde_json::json!({
        "schema":1, "target":env::var("TARGET").expect("Cargo target"), "update_trust":trust,
        "build": {
            "identity":if release {"spaceterm"} else if packaged {"spaceterm-preflight"} else {"spaceterm-development"},
            "commit":identity.commit, "version":identity.version, "source_sha256":source,
            "protocols":protocols, "core_floor":core_floor, "desktop_floor":desktop_floor,
            "apple_team_id":env::var("SPACETERM_RELEASE_APPLE_TEAM_ID").ok(),
        }
    });
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output"));
    std::fs::write(output.join("desktop-build-info.json"), document.to_string())
        .expect("write desktop provenance");
}

fn main() {
    println!("cargo:rerun-if-env-changed=SPACETERM_PACKAGED");
    println!("cargo:rerun-if-env-changed=SPACETERM_CLI_SOURCE_STATE");
    println!("cargo:rerun-if-env-changed=SPACETERM_RELEASE_TAG");
    println!("cargo:rerun-if-env-changed=SPACETERM_SPARKLE_DIR");
    println!("cargo:rustc-check-cfg=cfg(spaceterm_sparkle)");
    println!("cargo:rustc-check-cfg=cfg(spaceterm_packaged)");
    println!("cargo:rustc-check-cfg=cfg(spaceterm_release)");
    let root =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies the manifest"));
    // Watch source changes as well as refs so development identity cannot retain a release label.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build/identity.rs");
    println!("cargo:rerun-if-changed=build/release_notes.rs");
    for path in identity::watched_paths(&root).unwrap_or_else(|error| panic!("{error}")) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let release_tag = env::var("SPACETERM_RELEASE_TAG").ok();
    let identity =
        identity::resolve(&root, release_tag.as_deref()).unwrap_or_else(|error| panic!("{error}"));
    println!("cargo:rustc-env=SPACETERM_VERSION={}", identity.version);
    println!("cargo:rustc-env=SPACETERM_COMMIT={}", identity.commit);
    println!(
        "cargo:rustc-env=SPACETERM_TARGET={}",
        env::var("TARGET").expect("Cargo supplies TARGET")
    );
    println!(
        "cargo:rustc-env=SPACETERM_BUNDLE_VERSION={}",
        identity.bundle_version
    );
    release_notes::emit(&root, release_tag.as_deref()).unwrap_or_else(|error| panic!("{error}"));
    // The desktops' Update Floor: the Stepping Stone a person installs first when this build
    // can't read their data. Only a release has one.
    println!("cargo:rerun-if-env-changed=SPACETERM_DESKTOP_FLOOR");
    let floor = env::var("SPACETERM_DESKTOP_FLOOR").ok();
    assert!(
        floor.is_none() || release_tag.is_some(),
        "only a release takes an Update Floor"
    );
    println!(
        "cargo:rustc-env=SPACETERM_DESKTOP_FLOOR={}",
        floor.unwrap_or_default()
    );
    // The application identity follows these inputs.
    let packaged = env::var("SPACETERM_PACKAGED").as_deref() == Ok("1");
    // The headless server depends on this library but does not carry a client candidate.
    desktop_provenance(&root, &identity, release_tag.is_some(), packaged);
    if packaged {
        assert!(
            env::var_os("CARGO_FEATURE_DEVELOPER_TOOLS").is_none(),
            "a packaged build must exclude developer tools"
        );
        println!("cargo:rustc-cfg=spaceterm_packaged");
    }
    if release_tag.is_some() {
        assert!(packaged, "only a packaged build may carry a release tag");
        println!("cargo:rustc-cfg=spaceterm_release");
    }

    if env::var_os("CARGO_FEATURE_DESKTOP").is_none()
        || env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos")
    {
        return;
    }
    assert_eq!(
        env::var("CARGO_CFG_TARGET_ARCH").as_deref(),
        Ok("aarch64"),
        "SpaceTerm supports Apple Silicon Macs only"
    );
    let Some(frameworks) = env::var_os("SPACETERM_SPARKLE_DIR") else {
        assert!(
            release_tag.is_none(),
            "a release build must link the signed updater"
        );
        return;
    };
    let frameworks = PathBuf::from(frameworks)
        .canonicalize()
        .expect("Sparkle distribution is missing");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    let object = output.join("spaceterm-updater.o");
    println!("cargo:rerun-if-changed=src/platform/macos_updater.m");
    println!("cargo:rerun-if-changed=src/platform/macos_updater.h");
    let status = Command::new("xcrun")
        .args([
            "clang",
            "-fobjc-arc",
            "-fblocks",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-Wno-unused-parameter",
            "-mmacosx-version-min=26.0",
            "-arch",
            "arm64",
            "-c",
            "src/platform/macos_updater.m",
            "-F",
        ])
        .arg(&frameworks)
        .arg("-o")
        .arg(&object)
        .status()
        .expect("Xcode is required to build the updater");
    assert!(status.success(), "macOS updater compilation failed");
    let status = Command::new("xcrun")
        .args(["ar", "crs"])
        .arg(output.join("libspaceterm_updater.a"))
        .arg(object)
        .status()
        .expect("Xcode ar is required");
    assert!(status.success(), "macOS updater archive failed");
    println!("cargo:rustc-cfg=spaceterm_sparkle");
    println!("cargo:rustc-link-search=native={}", output.display());
    println!("cargo:rustc-link-search=framework={}", frameworks.display());
    println!("cargo:rustc-link-lib=static=spaceterm_updater");
    println!("cargo:rustc-link-lib=framework=Sparkle");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
}
