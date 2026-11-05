//! Signed archive fixtures replace only cryptographic trust and native code-signature observation.
use super::*;
use crate::update::facts::Protocols;
use crate::update::ordering::Floor;
use ring::signature::KeyPair;
use std::{
    cell::{Cell, RefCell},
    os::unix::fs::PermissionsExt,
};

pub(crate) const COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(crate) const VERSION: &str = "dev.aaaaaaaaaaaa";
thread_local! { static FIXTURE_TRUST: Cell<bool> = const { Cell::new(false) }; }
thread_local! { static FIXTURE_BUILD: RefCell<Option<(String, Option<Floor>, Protocols)>> = const { RefCell::new(None) }; }

/// The Update Floor fixtures of `version` declare unless a test names one: one every version
/// meets for a Release version, and none for a development one.
pub(crate) fn no_floor(version: &str) -> Option<Floor> {
    crate::update::facts::Version::parse(version)
        .ok()?
        .release()
        .map(|_| Floor::parse("0.0.0").unwrap())
}

/// Builds fixtures inside `run` that declare work protocol `major`, as a later release would.
pub(crate) fn with_major<T>(major: u16, run: impl FnOnce() -> T) -> T {
    let mut protocols = Protocols::CURRENT;
    protocols.work.major = major;
    with_build(VERSION, protocols, run)
}

/// Builds fixtures inside `run` that declare `version` and `protocols`. A Release `version`
/// keeps [`COMMIT`]; a development one must name it.
pub(crate) fn with_build<T>(version: &str, protocols: Protocols, run: impl FnOnce() -> T) -> T {
    with_floor(version, no_floor(version), protocols, run)
}

/// Builds fixtures inside `run` that declare `version` with the Update Floor `floor`.
pub(crate) fn with_floor<T>(
    version: &str,
    floor: Option<Floor>,
    protocols: Protocols,
    run: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<(String, Option<Floor>, Protocols)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FIXTURE_BUILD.set(self.0.take());
        }
    }
    let _restore = Restore(FIXTURE_BUILD.replace(Some((version.to_owned(), floor, protocols))));
    run()
}

/// The build document fixtures declare now.
fn manifest(identity: &str) -> serde_json::Value {
    let (version, floor, protocols) = FIXTURE_BUILD.with_borrow(|build| {
        build
            .clone()
            .unwrap_or((VERSION.to_owned(), None, Protocols::CURRENT))
    });
    serde_json::json!({"schema":1,"source_sha256":null,"identity":identity,"target":target(),"commit":COMMIT,"version":version,"floor":floor,"protocols":protocols})
}

pub(super) fn active() -> bool {
    FIXTURE_TRUST.get()
}

/// Runs `run` on this thread with fixture signing trust, as [`with_artifact`] does for its own
/// thread. A fixture server serving connections on other threads needs it there too.
pub(crate) fn with_fixture_trust<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            FIXTURE_TRUST.set(self.0);
        }
    }
    let _restore = Restore(FIXTURE_TRUST.replace(true));
    run()
}
pub(super) fn key() -> Vec<u8> {
    pair().public_key().as_ref().to_vec()
}
pub(super) fn production_key() -> Vec<u8> {
    production_pair().public_key().as_ref().to_vec()
}
pub(crate) fn sign_with_production_trust(archive: &Path, signature: &Path) {
    std::fs::write(
        signature,
        production_pair()
            .sign(&std::fs::read(archive).unwrap())
            .as_ref(),
    )
    .unwrap();
}
fn production_pair() -> ring::signature::Ed25519KeyPair {
    ring::signature::Ed25519KeyPair::from_seed_unchecked(&[43; 32]).unwrap()
}
/// What a fixture installation's executable reports: the build document its bytes hold, as an
/// executable reports the build it was compiled from, or else the installation's signed one.
pub(super) fn native_build(installation: &Path, identity: &str) -> Vec<u8> {
    let compiled = Layout::host(identity).and_then(|layout| {
        let path = installation.join(layout.executable());
        let mut first = [0];
        std::fs::File::open(&path)
            .ok()?
            .read_exact(&mut first)
            .ok()?;
        (first == *b"{")
            .then(|| std::fs::read(&path).ok())
            .flatten()
    });
    compiled
        .or_else(|| std::fs::read(installation.join("build.json")).ok())
        .unwrap_or_default()
}
pub(super) fn sign(archive: &Path, signature: &Path) {
    std::fs::write(
        signature,
        pair().sign(&std::fs::read(archive).unwrap()).as_ref(),
    )
    .unwrap();
}
fn pair() -> ring::signature::Ed25519KeyPair {
    ring::signature::Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap()
}
pub(crate) fn target() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "aarch64-apple-darwin"
    }
    #[cfg(target_os = "linux")]
    {
        "x86_64-unknown-linux-gnu"
    }
}
pub(crate) fn with_artifact<T>(
    directory: &Path,
    identity: &str,
    run: impl FnOnce(&Path, &Path) -> T,
) -> T {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let archive = directory.join("archive.tar.gz");
    let signature = directory.join("artifact.sig");
    let manifest = manifest(identity).to_string();
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);
    let layout = super::Layout::host(identity).unwrap();
    let packaged = |path: PathBuf| format!("core/{}", path.display());
    let mut entries = vec![
        ("core/build.json".to_owned(), manifest.into_bytes(), 0o644),
        (
            packaged(layout.executable()),
            b"signed fixture executable".to_vec(),
            0o755,
        ),
        (
            packaged(layout.resources().join("terminfo/x/xterm-spaceterm")),
            b"signed terminfo".to_vec(),
            0o644,
        ),
    ];
    for name in serde_json::from_str::<Vec<PathBuf>>(include_str!(
        "../../../../../packaging/core-resources.json"
    ))
    .unwrap()
    {
        entries.push((
            packaged(layout.resource(&name).unwrap()),
            b"signed resource".to_vec(),
            0o644,
        ));
    }
    for name in layout.bundle_files() {
        entries.push((packaged(name), b"signed bundle file".to_vec(), 0o644));
    }
    for (name, bytes, mode) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes.as_slice())
            .unwrap();
    }
    let bytes = tar.into_inner().unwrap().finish().unwrap();
    std::fs::write(&archive, &bytes).unwrap();
    std::fs::write(&signature, pair().sign(&bytes).as_ref()).unwrap();
    with_fixture_trust(|| run(&archive, &signature))
}
