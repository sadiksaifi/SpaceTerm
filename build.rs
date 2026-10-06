#[path = "build/identity.rs"]
mod identity;

use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=SPACETERM_PACKAGED");
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
    for path in identity::watched_paths(&root).unwrap_or_else(|error| panic!("{error}")) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let release_tag = env::var("SPACETERM_RELEASE_TAG").ok();
    let identity =
        identity::resolve(&root, release_tag.as_deref()).unwrap_or_else(|error| panic!("{error}"));
    println!("cargo:rustc-env=SPACETERM_VERSION={}", identity.version);
    println!(
        "cargo:rustc-env=SPACETERM_BUNDLE_VERSION={}",
        identity.bundle_version
    );
    // The application identity follows these inputs.
    let packaged = env::var("SPACETERM_PACKAGED").as_deref() == Ok("1");
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

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
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
