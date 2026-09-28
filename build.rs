use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=SPACETERM_RELEASE_TAG");
    println!("cargo:rerun-if-env-changed=SPACETERM_SPARKLE_DIR");
    println!("cargo:rustc-check-cfg=cfg(spaceterm_sparkle)");
    // Watch source changes as well as refs so development identity cannot retain a release label.
    for path in [
        "src",
        "scripts/release-version.py",
        ".git/HEAD",
        ".git/index",
        ".git/refs",
        ".git/packed-refs",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    let mut version = Command::new("python3");
    version.args(["scripts/release-version.py", "--cargo"]);
    if let Ok(tag) = env::var("SPACETERM_RELEASE_TAG") {
        version.args(["--tag", &tag, "--require-clean"]);
    }
    let result = version
        .output()
        .expect("Python is required to resolve Git build identity");
    assert!(
        result.status.success(),
        "Git build identity could not be resolved"
    );
    print!(
        "{}",
        String::from_utf8(result.stdout).expect("build identity is UTF-8")
    );

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    assert_eq!(
        env::var("CARGO_CFG_TARGET_ARCH").as_deref(),
        Ok("aarch64"),
        "SpaceTerm supports Apple Silicon Macs only"
    );
    let Some(frameworks) = env::var_os("SPACETERM_SPARKLE_DIR") else {
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
