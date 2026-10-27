//! Signed archive fixtures replace only cryptographic trust and native code-signature observation.
use super::*;
use ring::signature::KeyPair;
use std::{cell::Cell, os::unix::fs::PermissionsExt};

pub(crate) const COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(crate) const VERSION: &str = "dev.aaaaaaaaaaaa";
thread_local! { static FIXTURE_TRUST: Cell<bool> = const { Cell::new(false) }; }

pub(super) fn active() -> bool {
    FIXTURE_TRUST.get()
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
pub(super) fn native_build(identity: &str) -> Vec<u8> {
    serde_json::json!({"schema":1,"identity":identity,"target":target(),"commit":COMMIT,"version":VERSION,"protocol_major":2,"capabilities":["workspace_policy"]}).to_string().into_bytes()
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
    let manifest = serde_json::json!({"schema":1,"identity":identity,"target":target(),"commit":COMMIT,"version":VERSION,"protocol_major":2,"capabilities":["workspace_policy"]}).to_string();
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);
    let mut entries = vec![
        ("server/build.json".to_owned(), manifest.into_bytes(), 0o644),
        (
            "server/bin/spaceterm-server".to_owned(),
            b"signed fixture executable".to_vec(),
            0o755,
        ),
        (
            "server/share/spaceterm/terminfo/x/xterm-spaceterm".to_owned(),
            b"signed terminfo".to_vec(),
            0o644,
        ),
    ];
    for name in serde_json::from_str::<Vec<String>>(include_str!(
        "../../../../../packaging/server-resources.json"
    ))
    .unwrap()
    {
        entries.push((format!("server/{name}"), b"signed resource".to_vec(), 0o644));
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
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            FIXTURE_TRUST.set(self.0);
        }
    }
    let _restore = Restore(FIXTURE_TRUST.replace(true));
    run(&archive, &signature)
}
