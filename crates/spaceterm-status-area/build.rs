#[path = "../../build/identity.rs"]
mod identity;

fn main() {
    let root = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies the manifest directory"),
    )
    .join("../..");
    for input in ["SPACETERM_PACKAGED", "SPACETERM_RELEASE_TAG"] {
        println!("cargo:rerun-if-env-changed={input}");
    }
    for path in identity::watched_paths(&root).expect("Git build identity is required") {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-changed=../../build/identity.rs");
    // The desktop's build script also watches its sources, which can make the checkout dirty
    // without touching Git. Watching the same inputs keeps the two versions identical, as the
    // bundled helper's exact version check requires.
    println!("cargo:rerun-if-changed=../../src");
    let release = std::env::var("SPACETERM_RELEASE_TAG").ok();
    let packaged = std::env::var("SPACETERM_PACKAGED").as_deref() == Ok("1");
    assert!(release.is_none() || packaged, "releases must be packaged");
    let build = identity::resolve(&root, release.as_deref()).expect("valid build identity");
    // The same selection as the desktop and server, so one build controls only its own server.
    let name = match (packaged, release.is_some()) {
        (false, _) => "spaceterm-development",
        (true, false) => "spaceterm-preflight",
        (true, true) => "spaceterm",
    };
    println!("cargo:rustc-env=SPACETERM_STATUS_AREA_IDENTITY={name}");
    println!(
        "cargo:rustc-env=SPACETERM_STATUS_AREA_VERSION={}",
        build.version
    );
}
