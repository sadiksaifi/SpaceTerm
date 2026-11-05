mod build_resources;
#[path = "../../build/identity.rs"]
mod identity;

fn main() {
    let root = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies the manifest directory"),
    )
    .join("../..");
    for input in [
        "SPACETERM_PACKAGED",
        "SPACETERM_RELEASE_TAG",
        "SPACETERM_UPDATE_FLOOR",
        "SPACETERM_CLI_SOURCE_STATE",
    ] {
        println!("cargo:rerun-if-env-changed={input}");
    }
    for path in identity::watched_paths(&root).expect("Git build identity is required") {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-changed=../../build/identity.rs");
    println!("cargo:rerun-if-changed=../spaceterm-core/src/protocol/version.rs");
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
    if !packaged {
        build_resources::stage(&root, &output);
    }
    let source = std::env::var("SPACETERM_CLI_SOURCE_STATE").ok();
    assert!(
        source.as_deref().is_none_or(|value| value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))),
        "valid source digest"
    );
    let source = source.map_or_else(|| "null".to_owned(), |value| format!("\"{value}\""));
    let protocols = protocols(&root);
    let floor = floor(release.is_some().then_some(build.version.as_str()));
    // All strings come from the validated build identity, the validated floor or Cargo's target
    // triple.
    let document = format!(
        "{{\"schema\":1,\"source_sha256\":{source},\"identity\":\"{name}\",\"version\":\"{}\",\"floor\":\"{floor}\",\"commit\":\"{}\",\"target\":\"{target}\",\"protocols\":{protocols}}}\n",
        build.version, build.commit,
    );
    std::fs::write(output.join("build-info.json"), document).expect("write server build metadata");
}

/// The build document's `floor`: the Update Floor the release task computes and passes as
/// `SPACETERM_UPDATE_FLOOR`, which a release requires and which is never newer than its
/// `release` version. Development and Preflight builds have no order, so they name `0.0.0` and
/// take no floor.
fn floor(release: Option<&str>) -> String {
    let floor = std::env::var("SPACETERM_UPDATE_FLOOR").ok();
    let Some(release) = release else {
        assert!(floor.is_none(), "only a release takes an Update Floor");
        return "0.0.0".to_owned();
    };
    let floor = floor.expect("a release requires SPACETERM_UPDATE_FLOOR");
    let parts = |text: &str| -> Option<[u64; 3]> {
        let mut out = [0; 3];
        let mut parts = text.split('.');
        for slot in &mut out {
            let part = parts.next()?;
            let canonical = part == "0" || !part.starts_with('0');
            if part.is_empty() || !canonical || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            *slot = part.parse().ok()?;
        }
        parts.next().is_none().then_some(out)
    };
    let order = parts(&floor).expect("the Update Floor is MAJOR.MINOR.PATCH");
    assert!(
        parts(release).is_some_and(|release| order <= release),
        "the Update Floor is never newer than the release"
    );
    floor
}

/// The build document's `protocols` object, from the three constants in the Core's
/// `protocol/version.rs`. Each is declared on one line as
/// `pub const WORK: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };`.
fn protocols(root: &std::path::Path) -> String {
    let source =
        std::fs::read_to_string(root.join("crates/spaceterm-core/src/protocol/version.rs"))
            .expect("read the protocol versions");
    let version = |name: &str| {
        let prefix = format!("pub const {name}: ProtocolVersion = ProtocolVersion {{ major: ");
        let mut found = source.lines().filter_map(|line| line.strip_prefix(&prefix));
        let rest = found.next().expect("the protocol version is declared");
        assert!(
            found.next().is_none(),
            "the protocol version is declared once"
        );
        let (major, rest) = rest.split_once(", minor: ").expect("a major and a minor");
        let minor = rest.strip_suffix(" };").expect("one declaration per line");
        let major: u16 = major.parse().expect("a u16 major");
        let minor: u16 = minor.parse().expect("a u16 minor");
        assert!(major > 0, "majors start at 1");
        format!("{{\"major\":{major},\"minor\":{minor}}}")
    };
    format!(
        "{{\"work\":{},\"service\":{},\"desktop_channel\":{}}}",
        version("WORK"),
        version("SERVICE"),
        version("DESKTOP_CHANNEL")
    )
}
