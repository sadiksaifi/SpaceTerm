//! Compiled desktop authority and release-specific Core resource admission.
use spaceterm_core::update::{
    resource_manifest::{self, Archive, Build, ManifestError},
    trust::UpdateTrust,
};
use std::path::Path;

pub(crate) const COMPILED: &str =
    include_str!(concat!(env!("OUT_DIR"), "/desktop-build-info.json"));

pub(crate) fn compiled_build() -> Result<Build, ManifestError> {
    let document: serde_json::Value =
        serde_json::from_str(COMPILED).map_err(|_| ManifestError::Malformed)?;
    serde_json::from_value(document["build"].clone()).map_err(|_| ManifestError::Malformed)
}

pub(crate) fn verify_manifest(
    bytes: &[u8],
    signature: &[u8],
) -> Result<Vec<Archive>, ManifestError> {
    resource_manifest::verify(bytes, signature, &trust()?, &compiled_build()?)
}

fn trust() -> Result<UpdateTrust, ManifestError> {
    // Release trust comes only from the compiled document. Nonproduction packages retain their
    // separate public signing key and cannot authorize a Release archive.
    if cfg!(spaceterm_release) {
        let document: serde_json::Value =
            serde_json::from_str(COMPILED).map_err(|_| ManifestError::Malformed)?;
        UpdateTrust::parse(document["update_trust"].to_string().as_bytes())
            .map_err(|_| ManifestError::Malformed)
    } else {
        let document: serde_json::Value =
            serde_json::from_str(include_str!("../packaging/core-nonproduction-trust.json"))
                .map_err(|_| ManifestError::Malformed)?;
        let key = &document["public_key"];
        UpdateTrust::parse(
            serde_json::json!({"current":key,"backup":key})
                .to_string()
                .as_bytes(),
        )
        .map_err(|_| ManifestError::Malformed)
    }
}

pub(crate) fn read_manifest(root: &Path) -> Result<Vec<Archive>, ManifestError> {
    resource_manifest::read(root, &trust()?, &compiled_build()?)
}

pub(crate) fn authorize(
    root: &Path,
    target: &str,
    bytes: &[u8],
    signature: &[u8],
) -> Result<String, ManifestError> {
    let entry = read_manifest(root)?
        .into_iter()
        .find(|entry| entry.target() == target)
        .ok_or(ManifestError::ArchiveMismatch)?;
    verify_archive(&entry, bytes, signature)?;
    Ok(entry.sha256().to_owned())
}

pub(crate) fn verify_archive(
    entry: &Archive,
    bytes: &[u8],
    signature: &[u8],
) -> Result<(), ManifestError> {
    entry.verify(bytes, signature, &trust()?)
}
