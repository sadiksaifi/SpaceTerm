//! Release-specific authorization for the Core archives shipped by one desktop build.
use super::{facts::Protocols, trust::UpdateTrust};
use base64::Engine;
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};

pub const MAX_MANIFEST: usize = 16 * 1024;
pub const TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"];

/// Public build inputs compiled before any signed Core archive exists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub identity: String,
    pub commit: String,
    pub version: String,
    pub source_sha256: Option<String>,
    pub protocols: Protocols,
    pub core_floor: Option<String>,
    pub desktop_floor: Option<String>,
    pub apple_team_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u32,
    build: Build,
    archives: Vec<Entry>,
}

/// One archive authorized by an authenticated manifest. Constructed only by verification.
#[derive(Clone, Debug)]
pub struct Archive(Entry);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    target: String,
    sha256: String,
    signature: String,
}

/// Bounded failures without manifest contents, file names or native errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("the Core resource manifest is missing")]
    Missing,
    #[error("the Core resource manifest is unavailable")]
    Unavailable,
    #[error("the Core resource manifest signature does not verify")]
    Signature,
    #[error("the Core resource manifest is malformed")]
    Malformed,
    #[error("the Core resource manifest belongs to another desktop build")]
    BuildMismatch,
    #[error("the Core archive is not authorized by the resource manifest")]
    ArchiveMismatch,
}

/// Authenticate the exact bytes before interpreting any declaration. Every release lists both
/// native targets exactly once; no external digest file grants archive authority.
pub fn verify(
    bytes: &[u8],
    signature: &[u8],
    trust: &UpdateTrust,
    expected: &Build,
) -> Result<Vec<Archive>, ManifestError> {
    if bytes.len() > MAX_MANIFEST {
        return Err(ManifestError::Malformed);
    }
    trust
        .verify(bytes, signature)
        .map_err(|_| ManifestError::Signature)?;
    let document: Document = serde_json::from_slice(bytes).map_err(|_| ManifestError::Malformed)?;
    if document.schema != 1 || document.archives.len() != TARGETS.len() {
        return Err(ManifestError::Malformed);
    }
    if document.build != *expected {
        return Err(ManifestError::BuildMismatch);
    }
    for target in TARGETS {
        if document
            .archives
            .iter()
            .filter(|archive| archive.target == target)
            .count()
            != 1
        {
            return Err(ManifestError::Malformed);
        }
    }
    for archive in &document.archives {
        if archive.sha256.len() != 64
            || !archive
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || base64::engine::general_purpose::STANDARD
                .decode(&archive.signature)
                .map_err(|_| ManifestError::Malformed)?
                .len()
                != 64
        {
            return Err(ManifestError::Malformed);
        }
    }
    Ok(document.archives.into_iter().map(Archive).collect())
}

/// Read a bounded manifest pair from a packaged catalog and authenticate it before use.
pub fn read(
    root: &std::path::Path,
    trust: &UpdateTrust,
    expected: &Build,
) -> Result<Vec<Archive>, ManifestError> {
    fn file(path: &std::path::Path, maximum: usize) -> Result<Vec<u8>, ManifestError> {
        use std::{io::Read, os::unix::fs::OpenOptionsExt};
        let source = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    ManifestError::Missing
                } else {
                    ManifestError::Unavailable
                }
            })?;
        let metadata = source.metadata().map_err(|_| ManifestError::Unavailable)?;
        if !metadata.is_file() || metadata.len() > maximum as u64 {
            return Err(ManifestError::Malformed);
        }
        let mut bytes = Vec::new();
        source
            .take(maximum as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ManifestError::Unavailable)?;
        if bytes.len() > maximum {
            return Err(ManifestError::Malformed);
        }
        Ok(bytes)
    }
    verify(
        &file(&root.join("manifest.json"), MAX_MANIFEST)?,
        &file(&root.join("manifest.sig"), 64)?,
        trust,
        expected,
    )
}

impl Archive {
    pub fn target(&self) -> &str {
        &self.0.target
    }
    pub fn sha256(&self) -> &str {
        &self.0.sha256
    }

    fn signature_bytes(&self) -> Result<Vec<u8>, ManifestError> {
        base64::engine::general_purpose::STANDARD
            .decode(&self.0.signature)
            .map_err(|_| ManifestError::Malformed)
    }

    /// Authorize these captured archive bytes and the exact signature named by the manifest.
    /// Native admission and immutable installation checks still follow this operation.
    pub fn verify(
        &self,
        bytes: &[u8],
        signature: &[u8],
        trust: &UpdateTrust,
    ) -> Result<(), ManifestError> {
        let actual: String = digest(&SHA256, bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if actual != self.0.sha256 || signature != self.signature_bytes()? {
            return Err(ManifestError::ArchiveMismatch);
        }
        trust
            .verify(bytes, signature)
            .map_err(|_| ManifestError::Signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::trust::testing::{TestKey, trust};

    fn build() -> Build {
        Build {
            identity: "spaceterm".into(),
            commit: "a".repeat(40),
            version: "0.10.0".into(),
            source_sha256: Some("b".repeat(64)),
            protocols: Protocols::CURRENT,
            core_floor: Some("0.10.0".into()),
            desktop_floor: Some("0.10.0".into()),
            apple_team_id: Some("ABCDE12345".into()),
        }
    }
    fn document(key: &TestKey, bytes: &[u8]) -> Vec<u8> {
        let sha256: String = digest(&SHA256, bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        serde_json::to_vec(&serde_json::json!({"schema":1, "build":build(), "archives":
            TARGETS.map(|target| serde_json::json!({"target":target,"sha256":sha256,
                "signature":base64::engine::general_purpose::STANDARD.encode(key.sign(bytes))}))}))
        .unwrap()
    }
    #[test]
    fn a_missing_manifest_never_authorizes_an_archive() {
        let root =
            std::env::temp_dir().join(format!("spaceterm-absent-manifest-{}", std::process::id()));
        let key = TestKey::generate();
        assert_eq!(
            read(&root, &trust(&key, &TestKey::generate()), &build()).unwrap_err(),
            ManifestError::Missing
        );
    }
    #[test]
    fn reads_only_a_complete_bounded_regular_manifest_pair() {
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let root = Fixture(std::env::temp_dir().join(
            format!("spaceterm-manifest-files-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()),
        ));
        std::fs::create_dir(&root.0).unwrap();
        let key = TestKey::generate();
        let trust = trust(&key, &TestKey::generate());
        let manifest = document(&key, b"archive");
        let path = root.0.join("manifest.json");
        let signature = root.0.join("manifest.sig");
        std::fs::write(&path, &manifest).unwrap();
        assert_eq!(
            read(&root.0, &trust, &build()).unwrap_err(),
            ManifestError::Missing
        );
        std::fs::write(&signature, key.sign(&manifest)).unwrap();
        assert_eq!(read(&root.0, &trust, &build()).unwrap().len(), 2);
        std::fs::write(&path, vec![b' '; MAX_MANIFEST + 1]).unwrap();
        assert_eq!(
            read(&root.0, &trust, &build()).unwrap_err(),
            ManifestError::Malformed
        );
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&signature, &path).unwrap();
        assert_eq!(
            read(&root.0, &trust, &build()).unwrap_err(),
            ManifestError::Unavailable
        );
    }
    #[test]
    fn authenticates_only_the_archives_listed_for_this_exact_build() {
        let key = TestKey::generate();
        let trust = trust(&key, &TestKey::generate());
        let bytes = b"finalized Core archive";
        let manifest = document(&key, bytes);
        let entries = verify(&manifest, &key.sign(&manifest), &trust, &build()).unwrap();
        assert_eq!(entries[0].verify(bytes, &key.sign(bytes), &trust), Ok(()));
        for replacement in [
            b"swapped target archive".as_slice(),
            b"another valid same-key archive",
        ] {
            assert_eq!(
                entries[0].verify(replacement, &key.sign(replacement), &trust),
                Err(ManifestError::ArchiveMismatch)
            );
        }
        assert_eq!(
            entries[0].verify(bytes, &[0; 64], &trust),
            Err(ManifestError::ArchiveMismatch)
        );
    }
    #[test]
    fn rejects_tampered_unsigned_and_wrong_key_manifests() {
        let key = TestKey::generate();
        let trust = trust(&key, &TestKey::generate());
        let mut manifest = document(&key, b"archive");
        let signature = key.sign(&manifest);
        manifest.push(b' ');
        assert_eq!(
            verify(&manifest, &signature, &trust, &build()).unwrap_err(),
            ManifestError::Signature
        );
        for signature in [vec![], TestKey::generate().sign(&manifest)] {
            assert_eq!(
                verify(&manifest, &signature, &trust, &build()).unwrap_err(),
                ManifestError::Signature
            );
        }
    }
    #[test]
    fn rejects_signed_manifests_for_different_source_version_identity_or_policy() {
        let key = TestKey::generate();
        let trust = trust(&key, &TestKey::generate());
        let manifest = document(&key, b"archive");
        for field in [
            "commit",
            "source_sha256",
            "version",
            "identity",
            "core_floor",
            "desktop_floor",
            "apple_team_id",
        ] {
            let mut wrong = serde_json::to_value(build()).unwrap();
            wrong[field] = serde_json::Value::String("different".into());
            let expected = serde_json::from_value(wrong).unwrap();
            assert_eq!(
                verify(&manifest, &key.sign(&manifest), &trust, &expected).unwrap_err(),
                ManifestError::BuildMismatch,
                "{field}"
            );
        }
    }
    #[test]
    fn rejects_duplicate_or_missing_targets_even_when_signed() {
        let key = TestKey::generate();
        let trust = trust(&key, &TestKey::generate());
        let mut value: serde_json::Value =
            serde_json::from_slice(&document(&key, b"archive")).unwrap();
        value["archives"][1] = value["archives"][0].clone();
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(
            verify(&bytes, &key.sign(&bytes), &trust, &build()).unwrap_err(),
            ManifestError::Malformed
        );
    }
}
