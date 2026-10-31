#[path = "../../build/identity.rs"]
mod identity;
#[path = "../spaceterm-server/src/protocol/major.rs"]
mod major;

fn main() {
    let root = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies the manifest directory"),
    )
    .join("../..");
    for input in [
        "SPACETERM_PACKAGED",
        "SPACETERM_RELEASE_TAG",
        "SPACETERM_CLI_SOURCE_STATE",
    ] {
        println!("cargo:rerun-if-env-changed={input}");
    }
    for path in identity::watched_paths(&root).expect("Git build identity is required") {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-changed=../../build/identity.rs");
    println!("cargo:rerun-if-changed=../spaceterm-server/src/protocol/major.rs");
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
    println!("cargo:rustc-env=SPACETERM_CLI_IDENTITY={name}");
    println!("cargo:rustc-env=SPACETERM_CLI_VERSION={}", build.version);
    let target = std::env::var("TARGET").expect("Cargo supplies the target");
    println!("cargo:rustc-env=SPACETERM_CLI_TARGET={target}");
    println!("cargo:rustc-env=SPACETERM_CLI_COMMIT={}", build.commit);
    let output = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR").expect("Cargo supplies the output directory"),
    );
    let source = std::env::var("SPACETERM_CLI_SOURCE_STATE").ok();
    assert!(
        source.as_deref().is_none_or(|value| value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))),
        "valid source digest"
    );
    let source = source.map_or_else(|| "null".to_owned(), |value| format!("\"{value}\""));
    // All strings come from the validated build identity or Cargo's target triple.
    let document = format!(
        "{{\"schema\":1,\"source_sha256\":{source},\"identity\":\"{name}\",\"version\":\"{}\",\"commit\":\"{}\",\"target\":\"{target}\",\"protocol_major\":{}}}\n",
        build.version,
        build.commit,
        major::MAJOR,
    );
    std::fs::write(output.join("build-info.json"), document).expect("write server build metadata");
}
