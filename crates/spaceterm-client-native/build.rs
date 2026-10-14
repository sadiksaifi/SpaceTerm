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
    for path in ["src", "build.rs", "../../build/identity.rs"] {
        println!("cargo:rerun-if-changed={path}");
    }
    let release_tag = env::var("SPACETERM_RELEASE_TAG").ok();
    let identity = identity::resolve_server(
        &root,
        env::var("SPACETERM_PACKAGED").as_deref() == Ok("1"),
        release_tag.as_deref(),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    println!(
        "cargo:rustc-env=SPACETERM_CLI_VERSION={}",
        identity.version.version
    );
}
