//! Exact signed-artifact transfer over the retained SSH byte stream.
//!
//! The receiver authenticates the feed before allocating the archive. No remote
//! pathname, decoded manifest, or successful transfer status grants install authority.

use std::io::{Read, Write};

use spaceterm_distribution::{
    BuildChannel, DistributionFailure, ServerArchiveVerifier, ServerPlatform, UpdateKey,
    VerifiedAccountServerArtifact,
    server_feed::{
        MAX_ARCHIVE_BYTES, MAX_FEED_BYTES, MAX_FEED_SIGNATURE_BYTES, verify_account_server_feed,
    },
};

const MAGIC: &[u8; 8] = b"STBA2\0\0\0";

/// Content-free failures from the bounded signed staging stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ArtifactTransferFailure {
    /// The original stream was interrupted or truncated.
    #[error("bootstrap artifact stream unavailable")]
    Unavailable,
    /// Framing or trailing bytes do not match the fixed staging grammar.
    #[error("invalid bootstrap artifact stream")]
    InvalidFrame,
    /// A declared payload exceeds its independent bound.
    #[error("bootstrap artifact stream limit exceeded")]
    LimitExceeded,
    /// Production signing authority is not configured.
    #[error("bootstrap artifact signing authority unavailable")]
    TrustUnavailable,
    /// Feed, archive, manifest or target authentication failed.
    #[error("bootstrap artifact rejected")]
    Rejected(DistributionFailure),
}

/// Writes only the signature transcript and archive already authenticated by distribution.
/// The SSH process owner supplies the stream, deadline, route checks and exact-child cleanup.
pub fn write_artifact(
    artifact: &VerifiedAccountServerArtifact,
    mut writer: impl Write,
) -> Result<(), ArtifactTransferFailure> {
    let feed = artifact.feed();
    writer
        .write_all(MAGIC)
        .map_err(|_| ArtifactTransferFailure::Unavailable)?;
    for bytes in [feed.bytes(), feed.signature(), artifact.bytes()] {
        writer
            .write_all(&(bytes.len() as u64).to_be_bytes())
            .map_err(|_| ArtifactTransferFailure::Unavailable)?;
        writer
            .write_all(bytes)
            .map_err(|_| ArtifactTransferFailure::Unavailable)?;
    }
    writer
        .flush()
        .map_err(|_| ArtifactTransferFailure::Unavailable)
}

/// Receives a signed server artifact using the compiled production update key.
/// Build, target and the exact approved archive digest come from the retained
/// foreground plan, not from the stream. Another valid signed version is still a replacement.
pub fn receive_artifact(
    reader: &mut impl Read,
    build: BuildChannel,
    platform: ServerPlatform,
    expected_artifact: [u8; 32],
    now_unix_seconds: u64,
) -> Result<VerifiedAccountServerArtifact, ArtifactTransferFailure> {
    let key = UpdateKey::release().ok_or(ArtifactTransferFailure::TrustUnavailable)?;
    receive(
        reader,
        &key,
        build,
        platform,
        expected_artifact,
        now_unix_seconds,
    )
}

fn receive(
    reader: &mut impl Read,
    key: &UpdateKey,
    build: BuildChannel,
    platform: ServerPlatform,
    expected_artifact: [u8; 32],
    now_unix_seconds: u64,
) -> Result<VerifiedAccountServerArtifact, ArtifactTransferFailure> {
    let mut magic = [0; 8];
    reader
        .read_exact(&mut magic)
        .map_err(|_| ArtifactTransferFailure::Unavailable)?;
    if &magic != MAGIC {
        return Err(ArtifactTransferFailure::InvalidFrame);
    }
    let feed = payload(reader, MAX_FEED_BYTES as u64, None)?;
    let signature = payload(reader, MAX_FEED_SIGNATURE_BYTES as u64, None)?;
    let feed =
        verify_account_server_feed(key, build, platform, &feed, &signature, now_unix_seconds)
            .map_err(ArtifactTransferFailure::Rejected)?;
    if expected_artifact == [0; 32] || feed.document().archive.sha256 != expected_artifact {
        return Err(ArtifactTransferFailure::Rejected(
            DistributionFailure::IdentityMismatch,
        ));
    }
    let archive = payload(
        reader,
        MAX_ARCHIVE_BYTES,
        Some(feed.document().archive.size),
    )?;
    let mut trailing = [0; 1];
    if reader
        .read(&mut trailing)
        .map_err(|_| ArtifactTransferFailure::Unavailable)?
        != 0
    {
        return Err(ArtifactTransferFailure::InvalidFrame);
    }
    ServerArchiveVerifier::new()
        .verify_account_owned(key, feed, archive)
        .map_err(ArtifactTransferFailure::Rejected)
}

fn payload(
    reader: &mut impl Read,
    maximum: u64,
    expected: Option<u64>,
) -> Result<Vec<u8>, ArtifactTransferFailure> {
    let mut length = [0; 8];
    reader
        .read_exact(&mut length)
        .map_err(|_| ArtifactTransferFailure::Unavailable)?;
    let length = u64::from_be_bytes(length);
    if length == 0 || length > maximum {
        return Err(ArtifactTransferFailure::LimitExceeded);
    }
    if expected.is_some_and(|expected| length != expected) {
        return Err(ArtifactTransferFailure::InvalidFrame);
    }
    let length = usize::try_from(length).map_err(|_| ArtifactTransferFailure::LimitExceeded)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| ArtifactTransferFailure::LimitExceeded)?;
    bytes.resize(length, 0);
    reader
        .read_exact(&mut bytes)
        .map_err(|_| ArtifactTransferFailure::Unavailable)?;
    Ok(bytes)
}

/// Owned-fixture verification uses the same framing, signature and archive checks.
/// This entry is absent from production composition.
#[cfg(feature = "test-support")]
pub fn receive_artifact_with_key(
    reader: &mut impl Read,
    key: &UpdateKey,
    build: BuildChannel,
    platform: ServerPlatform,
    expected_artifact: [u8; 32],
    now_unix_seconds: u64,
) -> Result<VerifiedAccountServerArtifact, ArtifactTransferFailure> {
    receive(
        reader,
        key,
        build,
        platform,
        expected_artifact,
        now_unix_seconds,
    )
}
