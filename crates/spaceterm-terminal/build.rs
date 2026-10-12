#[path = "../../build/identity.rs"]
mod identity;

use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=SPACETERM_PACKAGED");
    println!("cargo:rerun-if-env-changed=SPACETERM_RELEASE_TAG");
    let root =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies the manifest"))
            .join("../..");
    println!("cargo:rerun-if-changed={}", root.join("build.rs").display());
    println!("cargo:rerun-if-changed={}", root.join("src").display());
    println!("cargo:rerun-if-changed={}", root.join("crates").display());
    println!(
        "cargo:rerun-if-changed={}",
        root.join("build/identity.rs").display()
    );
    for path in identity::watched_paths(&root).unwrap_or_else(|error| panic!("{error}")) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let release_tag = env::var("SPACETERM_RELEASE_TAG").ok();
    let identity =
        identity::resolve(&root, release_tag.as_deref()).unwrap_or_else(|error| panic!("{error}"));
    if release_tag.is_some() {
        assert!(
            env::var("SPACETERM_PACKAGED").as_deref() == Ok("1"),
            "only a packaged build may carry a release tag"
        );
    }
    println!("cargo:rustc-env=SPACETERM_VERSION={}", identity.version);
}
