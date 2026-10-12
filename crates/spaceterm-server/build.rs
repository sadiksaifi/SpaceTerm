#[path = "../../build/identity.rs"]
mod identity;

use std::{env, path::PathBuf};

fn main() {
    for variable in ["SPACETERM_PACKAGED", "SPACETERM_RELEASE_TAG"] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    let root =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies the manifest"))
            .join("../..");
    for path in identity::watched_paths(&root).unwrap_or_else(|error| panic!("{error}")) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    for path in [
        "src",
        "build.rs",
        "../../build/identity.rs",
        "../../crates",
        "../../packaging/server",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    let release_tag = env::var("SPACETERM_RELEASE_TAG").ok();
    let identity = identity::resolve_server(
        &root,
        env::var("SPACETERM_PACKAGED").as_deref() == Ok("1"),
        release_tag.as_deref(),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let platform = match (
        env::var("CARGO_CFG_TARGET_OS").as_deref(),
        env::var("CARGO_CFG_TARGET_ARCH").as_deref(),
    ) {
        (Ok("macos"), Ok("aarch64")) => "darwin-arm64",
        (Ok("linux"), Ok("x86_64")) if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") => {
            "linux-x86_64"
        }
        _ => panic!("unsupported server target"),
    };
    for (name, value) in [
        (
            "SPACETERM_SERVER_VERSION",
            identity.version.version.as_str(),
        ),
        (
            "SPACETERM_SERVER_BUNDLE_VERSION",
            identity.version.bundle_version.as_str(),
        ),
        ("SPACETERM_SERVER_COMMIT", identity.commit.as_str()),
        ("SPACETERM_SERVER_BUILD", identity.build.name()),
        ("SPACETERM_SERVER_IDENTIFIER", identity.build.identifier()),
        ("SPACETERM_SERVER_PLATFORM", platform),
    ] {
        println!("cargo:rustc-env={name}={value}");
    }
}
