//! Bounded archive reads after detached distribution authentication.

use crate::native_deployment::contracts::NativeFailure;
use spaceterm_distribution::{
    DistributionFailure, InstallationManifestV1, VerifiedInstallationManifest,
    VerifiedServerArchive,
    manifest::{MAX_EXPANDED_BYTES, MAX_FILES, MAX_MANIFEST_BYTES},
    verify_installation_manifest,
};
use std::io::{Cursor, Read};

pub(crate) fn failure(error: DistributionFailure) -> NativeFailure {
    match error {
        DistributionFailure::LimitExceeded => NativeFailure::LimitExceeded,
        DistributionFailure::IdentityMismatch => NativeFailure::IdentityMismatch,
        DistributionFailure::UnsupportedSchema => NativeFailure::UnsupportedSchema,
        _ => NativeFailure::ArtifactRejected,
    }
}
fn archive(
    candidate: &VerifiedServerArchive,
) -> tar::Archive<std::io::Take<flate2::read::MultiGzDecoder<Cursor<&[u8]>>>> {
    tar::Archive::new(
        flate2::read::MultiGzDecoder::new(Cursor::new(candidate.bytes()))
            .take(MAX_EXPANDED_BYTES + 1),
    )
}
pub(crate) fn authenticate(
    candidate: &VerifiedServerArchive,
) -> Result<(VerifiedInstallationManifest, Vec<u8>), NativeFailure> {
    let mut tar = archive(candidate);
    let mut manifest = None;
    let mut long_name = None;
    // Ordinary iteration buffers extension payloads before yielding an entry. Discover the
    // manifest from raw records so untrusted metadata is bounded before any such allocation.
    let entries = tar
        .entries()
        .map_err(|_| NativeFailure::ArtifactRejected)?
        .raw(true);
    for (count, entry) in entries.enumerate() {
        if count >= MAX_FILES * 2 + 2 {
            return Err(NativeFailure::LimitExceeded);
        }
        let mut entry = entry.map_err(|_| NativeFailure::ArtifactRejected)?;
        let kind = entry.header().entry_type();
        if kind.is_gnu_longname() {
            // Copy iteration recognizes extensions only in GNU or UStar headers.
            // Reject other layouts before reading their payload or creating a generation.
            if entry.header().as_gnu().is_none() && entry.header().as_ustar().is_none() {
                return Err(NativeFailure::ArtifactRejected);
            }
            // Match distribution's root prefix plus 1024-byte relative name and terminator.
            if long_name.is_some() || entry.size() == 0 || entry.size() > 1043 {
                return Err(NativeFailure::ArtifactRejected);
            }
            let mut bytes = vec![0; entry.size() as usize];
            entry
                .read_exact(&mut bytes)
                .map_err(|_| NativeFailure::ArtifactRejected)?;
            if bytes.pop() != Some(0) || std::str::from_utf8(&bytes).is_err() {
                return Err(NativeFailure::ArtifactRejected);
            }
            long_name = Some(bytes);
            continue;
        }
        // In particular, never read GNU link or PAX payloads, even if their headers
        // advertise a small size. Distribution admits no overrides of these kinds.
        if !kind.is_file() && !kind.is_dir() {
            return Err(NativeFailure::ArtifactRejected);
        }
        let path = long_name
            .take()
            .map(std::borrow::Cow::Owned)
            .unwrap_or_else(|| entry.path_bytes());
        if path.as_ref() == b"spaceterm-server/installation.json" {
            if manifest.is_some() || !kind.is_file() {
                return Err(NativeFailure::ArtifactRejected);
            }
            if entry.size() > MAX_MANIFEST_BYTES as u64 {
                return Err(NativeFailure::LimitExceeded);
            }
            let mut bytes = Vec::new();
            entry
                .take(MAX_MANIFEST_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| NativeFailure::ArtifactRejected)?;
            if bytes.len() > MAX_MANIFEST_BYTES {
                return Err(NativeFailure::LimitExceeded);
            }
            manifest = Some(bytes);
        }
    }
    let bytes = manifest.ok_or(NativeFailure::ArtifactRejected)?;
    // Distribution owns complete link/type/mode/path/tail validation over the immutable archive.
    let verified = verify_installation_manifest(candidate, &bytes).map_err(failure)?;
    Ok((verified, bytes))
}

pub(crate) fn copy_files(
    candidate: &VerifiedServerArchive,
    manifest: &InstallationManifestV1,
    tree: &mut super::filesystem::DirectoryTree,
) -> Result<Vec<super::filesystem::BoundFile>, NativeFailure> {
    let mut tar = archive(candidate);
    let mut files = Vec::with_capacity(manifest.files.len() + 2);
    for entry in tar.entries().map_err(|_| NativeFailure::ArtifactRejected)? {
        let mut entry = entry.map_err(|_| NativeFailure::ArtifactRejected)?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path_bytes();
        let name = std::str::from_utf8(&path).map_err(|_| NativeFailure::ArtifactRejected)?;
        let name = name
            .strip_prefix("spaceterm-server/")
            .ok_or(NativeFailure::ArtifactRejected)?;
        if name == "installation.json" {
            continue;
        }
        let index = manifest
            .files
            .binary_search_by(|f| f.name.as_str().cmp(name))
            .map_err(|_| NativeFailure::ArtifactRejected)?;
        let expected = &manifest.files[index];
        files.push(tree.copy_file(
            &expected.name,
            expected.size,
            expected.mode,
            expected.sha256,
            &mut entry,
        )?);
    }
    files.sort_by(|a, b| a.relative.cmp(&b.relative));
    if files.len() != manifest.files.len()
        || files
            .iter()
            .zip(&manifest.files)
            .any(|(a, b)| a.relative != b.name)
    {
        return Err(NativeFailure::ArtifactRejected);
    }
    Ok(files)
}
