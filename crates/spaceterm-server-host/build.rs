#[path = "../../build/identity.rs"]
mod identity;

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for input in ["SPACETERM_PACKAGED", "SPACETERM_RELEASE_TAG"] {
        println!("cargo:rerun-if-env-changed={input}");
    }
    for path in identity::watched_paths(&root).expect("Git build identity is required") {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-changed=../../build/identity.rs");
    println!("cargo:rerun-if-changed=src");
    let release = std::env::var("SPACETERM_RELEASE_TAG").ok();
    let packaged = std::env::var("SPACETERM_PACKAGED").as_deref() == Ok("1");
    assert!(release.is_none() || packaged, "releases must be packaged");
    let build = identity::resolve(&root, release.as_deref()).expect("valid build identity");
    let name = match (packaged, release.is_some()) {
        (false, _) => "spaceterm-development",
        (true, false) => "spaceterm-preflight",
        (true, true) => "spaceterm",
    };
    println!("cargo:rustc-env=SPACETERM_SERVER_IDENTITY={name}");
    println!("cargo:rustc-env=SPACETERM_SERVER_VERSION={}", build.version);
    println!(
        "cargo:rustc-env=SPACETERM_SERVER_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
    let commit = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("Git is required");
    assert!(commit.status.success());
    println!(
        "cargo:rustc-env=SPACETERM_SERVER_COMMIT={}",
        String::from_utf8(commit.stdout).unwrap().trim()
    );
}
