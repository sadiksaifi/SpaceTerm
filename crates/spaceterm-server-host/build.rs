#[path = "../../build/identity.rs"]
mod identity;

const PROTOCOL_MAJOR: u16 = 1;
const CAPABILITIES: &[&str] = &["workspace_policy"];

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
    let target = std::env::var("TARGET").expect("Cargo supplies the target");
    println!("cargo:rustc-env=SPACETERM_SERVER_TARGET={target}");
    println!("cargo:rustc-env=SPACETERM_SERVER_COMMIT={}", build.commit);
    let output = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR").expect("Cargo supplies the output directory"),
    );
    // All strings come from the validated build identity or Cargo's target triple.
    let document = format!(
        "{{\"schema\":1,\"identity\":\"{name}\",\"version\":\"{}\",\"commit\":\"{}\",\"target\":\"{target}\",\"protocol_major\":{PROTOCOL_MAJOR},\"capabilities\":{CAPABILITIES:?}}}\n",
        build.version, build.commit,
    );
    std::fs::write(output.join("build-info.json"), document).expect("write server build metadata");
    std::fs::write(
        output.join("build-info.rs"),
        format!(
            "const PROTOCOL_MAJOR: u16 = {PROTOCOL_MAJOR};\nconst CAPABILITIES: &[&str] = &{CAPABILITIES:?};\n"
        ),
    )
    .expect("write server admission metadata");
}
