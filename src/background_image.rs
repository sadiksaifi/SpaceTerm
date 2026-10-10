//! Keeps the Background Image as SpaceTerm's own private copy in the application data directory.
//!
//! Each copy is named by the SHA-256 digest of its bytes. A Settings Document names one copy by
//! that digest, so choosing another image never rewrites a copy a retained document still names,
//! and a copy whose bytes no longer match its name is never presented. Failures carry only a
//! typed classification, never a path or native error text.

#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::platform::app_directories::AppDirectoryRoot;
use crate::platform::app_paths::AppPaths;
use crate::platform::secure_filesystem::SecureFilesystemError;

/// The largest image SpaceTerm copies or presents.
pub(crate) const MAXIMUM_BYTES: usize = 32 * 1024 * 1024;

const FILE_NAME_PREFIX: &str = "background-image-";
const PREPARE_ATTEMPTS: usize = 8;

/// The digest naming one retained copy.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(crate) struct BackgroundImageId([u8; 32]);

impl BackgroundImageId {
    fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    fn hex(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn file_name(self) -> OsString {
        format!("{FILE_NAME_PREFIX}{}", self.hex()).into()
    }

    fn parse(text: &str) -> Option<Self> {
        if text.len() != 64
            || !text
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return None;
        }
        let mut digest = [0; 32];
        for (index, byte) in digest.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
        }
        Some(Self(digest))
    }
}

impl std::fmt::Debug for BackgroundImageId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "BackgroundImageId({})", self.hex())
    }
}

impl Serialize for BackgroundImageId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for BackgroundImageId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text)
            .ok_or_else(|| serde::de::Error::custom("expected 64 lowercase hexadecimal digits"))
    }
}

/// Why an image could not be copied or presented.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum BackgroundImageError {
    #[error("the image is larger than SpaceTerm presents")]
    TooLarge,
    #[error("the file is not a PNG, JPEG, HEIC, or WebP image")]
    UnsupportedFormat,
    /// No copy with this digest remains, or its bytes no longer match it.
    #[error("the background image copy is missing")]
    Missing,
    #[error("the background image copy is unsafe")]
    Unsafe,
    #[error("the application data directory is unavailable")]
    Unavailable,
}

/// Copies, reads and discards Background Images in the application data directory.
pub(crate) struct BackgroundImageStore {
    paths: Arc<AppPaths>,
}

impl BackgroundImageStore {
    pub(crate) fn new(paths: Arc<AppPaths>) -> Self {
        Self { paths }
    }

    /// Retains a private copy of `bytes` and returns the digest naming it. Copying the same image
    /// again keeps the existing copy.
    pub(crate) fn install(&self, bytes: &[u8]) -> Result<BackgroundImageId, BackgroundImageError> {
        if bytes.len() > MAXIMUM_BYTES {
            return Err(BackgroundImageError::TooLarge);
        }
        if !is_supported_format(bytes) {
            return Err(BackgroundImageError::UnsupportedFormat);
        }
        let id = BackgroundImageId::of(bytes);
        let directory = self
            .paths
            .ensure_secure_root(AppDirectoryRoot::Data)
            .map_err(|_| BackgroundImageError::Unavailable)?;
        let filesystem = self.paths.filesystem();
        let name = id.file_name();
        let existing = filesystem
            .read_private_file(&directory, &name, MAXIMUM_BYTES)
            .map_err(filesystem_error)?;
        if existing
            .as_ref()
            .is_some_and(|snapshot| snapshot.bytes == bytes)
        {
            return Ok(id);
        }
        let expected = existing.map(|snapshot| snapshot.identity);
        for _ in 0..PREPARE_ATTEMPTS {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).map_err(|_| BackgroundImageError::Unavailable)?;
            let prepared = match filesystem.prepare_private_file(&directory, &name, bytes, nonce) {
                Ok(prepared) => prepared,
                Err(SecureFilesystemError::AlreadyExists) => continue,
                Err(error) => return Err(filesystem_error(error)),
            };
            let commit = filesystem
                .commit_private_file(prepared, expected.as_ref())
                .map_err(filesystem_error)?;
            return match commit.published_identity {
                Some(_) => Ok(id),
                None => Err(BackgroundImageError::Unavailable),
            };
        }
        Err(BackgroundImageError::Unavailable)
    }

    /// Reads the copy `id` names, refusing one whose bytes no longer match it.
    pub(crate) fn load(&self, id: BackgroundImageId) -> Result<Arc<[u8]>, BackgroundImageError> {
        let directory = self
            .paths
            .open_secure_root(AppDirectoryRoot::Data)
            .map_err(|_| BackgroundImageError::Unavailable)?
            .ok_or(BackgroundImageError::Missing)?;
        let snapshot = self
            .paths
            .filesystem()
            .read_private_file(&directory, &id.file_name(), MAXIMUM_BYTES)
            .map_err(filesystem_error)?
            .ok_or(BackgroundImageError::Missing)?;
        if snapshot.bytes.len() > MAXIMUM_BYTES || BackgroundImageId::of(&snapshot.bytes) != id {
            return Err(BackgroundImageError::Missing);
        }
        Ok(snapshot.bytes.into())
    }

    /// Removes the copy `id` names. A copy that is already gone counts as discarded.
    pub(crate) fn discard(&self, id: BackgroundImageId) -> Result<(), BackgroundImageError> {
        let Some(directory) = self
            .paths
            .open_secure_root(AppDirectoryRoot::Data)
            .map_err(|_| BackgroundImageError::Unavailable)?
        else {
            return Ok(());
        };
        let filesystem = self.paths.filesystem();
        let name = id.file_name();
        let Some(snapshot) = filesystem
            .read_private_file(&directory, &name, MAXIMUM_BYTES)
            .map_err(filesystem_error)?
        else {
            return Ok(());
        };
        match filesystem.remove_private_file(&directory, &name, &snapshot.identity) {
            Ok(()) | Err(SecureFilesystemError::Missing) => Ok(()),
            Err(error) => Err(filesystem_error(error)),
        }
    }
}

/// Recognizes the still-image formats every supported macOS release presents.
fn is_supported_format(bytes: &[u8]) -> bool {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    const JPEG: &[u8] = b"\xff\xd8\xff";
    const HEIF_BRANDS: [&[u8]; 6] = [b"heic", b"heix", b"hevc", b"heim", b"heis", b"mif1"];
    let webp = bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP";
    let heif = bytes.len() >= 12 && &bytes[4..8] == b"ftyp" && HEIF_BRANDS.contains(&&bytes[8..12]);
    bytes.starts_with(PNG) || bytes.starts_with(JPEG) || webp || heif
}

fn filesystem_error(error: SecureFilesystemError) -> BackgroundImageError {
    match error {
        SecureFilesystemError::Unsafe => BackgroundImageError::Unsafe,
        SecureFilesystemError::TooLarge => BackgroundImageError::TooLarge,
        SecureFilesystemError::Missing => BackgroundImageError::Missing,
        SecureFilesystemError::AlreadyExists
        | SecureFilesystemError::Conflict
        | SecureFilesystemError::Unavailable => BackgroundImageError::Unavailable,
    }
}
