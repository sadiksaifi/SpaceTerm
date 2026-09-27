//! The Zed extension registry as a source of Terminal Themes.
//!
//! This Module owns the registry protocol: which requests exist, how large each response may be,
//! and how an extension archive reduces to its theme family documents. Requests go through an
//! injected [`RegistryTransport`], so the network stays behind one Seam and the protocol is
//! testable without it. SpaceTerm contacts the registry only when the user asks it to, and an
//! archive is read in memory: nothing it contains reaches the filesystem.

use std::io::Read as _;
use std::sync::Arc;

use serde::Deserialize;

use crate::appearance::{MAX_EXTENSION_FAMILIES, MAX_FAMILY_BYTES, ZedExtension};

const REGISTRY: &str = "https://api.zed.dev";
/// Zed extension manifests this build understands. Newer schemas may change the archive layout.
const MAX_SCHEMA_VERSION: u32 = 1;
const MAX_LISTING_BYTES: usize = 8 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: usize = 16 * 1024 * 1024;
const MAX_UNPACKED_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 4096;
const MAX_LISTED_EXTENSIONS: usize = 10_000;

/// Fetches one HTTPS resource for the registry protocol.
pub(crate) trait RegistryTransport: Send + Sync {
    /// Returns the body of a successful response, refusing one longer than `limit` bytes.
    ///
    /// Implementations follow redirects only to other HTTPS locations.
    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, TransportError>;
}

/// A content-free transport failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum TransportError {
    #[error("the registry could not be reached")]
    Unreachable,
    #[error("the registry refused the request")]
    Refused,
    #[error("the registry response exceeded its limit")]
    TooLarge,
}

/// A content-free registry failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum RegistryError {
    #[error("the registry could not be reached")]
    Unreachable,
    #[error("the registry refused the request")]
    Refused,
    #[error("the registry response exceeded its limit")]
    TooLarge,
    #[error("the registry response is invalid")]
    InvalidResponse,
    #[error("the extension archive is invalid")]
    InvalidArchive,
    #[error("the extension contains no themes")]
    NoThemes,
}

impl From<TransportError> for RegistryError {
    fn from(error: TransportError) -> Self {
        match error {
            TransportError::Unreachable => Self::Unreachable,
            TransportError::Refused => Self::Refused,
            TransportError::TooLarge => Self::TooLarge,
        }
    }
}

/// One published theme extension, as the registry lists it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RegistryExtension {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) description: Option<String>,
    pub(crate) authors: Vec<String>,
    pub(crate) downloads: u64,
}

/// The Zed extension registry, reached through an injected transport.
#[derive(Clone)]
pub(crate) struct ZedThemeRegistry {
    transport: Arc<dyn RegistryTransport>,
}

impl ZedThemeRegistry {
    pub(crate) fn new(transport: Arc<dyn RegistryTransport>) -> Self {
        Self { transport }
    }

    /// Lists every extension that provides themes, most downloaded first.
    pub(crate) fn list(&self) -> Result<Vec<RegistryExtension>, RegistryError> {
        let url = format!(
            "{REGISTRY}/extensions?provides=themes&max_schema_version={MAX_SCHEMA_VERSION}"
        );
        parse_listing(&self.transport.get(&url, MAX_LISTING_BYTES)?)
    }

    /// Downloads the listed version of one extension and reads its theme families.
    pub(crate) fn download(
        &self,
        extension: &RegistryExtension,
    ) -> Result<ZedExtension, RegistryError> {
        if !is_url_segment(&extension.id) || !is_url_segment(&extension.version) {
            return Err(RegistryError::InvalidResponse);
        }
        let url = format!(
            "{REGISTRY}/extensions/{}/{}/download",
            extension.id, extension.version
        );
        let archive = self.transport.get(&url, MAX_ARCHIVE_BYTES)?;
        Ok(ZedExtension {
            id: extension.id.clone(),
            version: extension.version.clone(),
            families: theme_families(&archive)?,
        })
    }
}

#[derive(Deserialize)]
struct Listing {
    data: Vec<ListedExtension>,
}

#[derive(Deserialize)]
struct ListedExtension {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    authors: Vec<String>,
    #[serde(default)]
    download_count: u64,
    #[serde(default)]
    provides: Vec<String>,
}

fn parse_listing(bytes: &[u8]) -> Result<Vec<RegistryExtension>, RegistryError> {
    let listing: Listing =
        serde_json::from_slice(bytes).map_err(|_| RegistryError::InvalidResponse)?;
    if listing.data.len() > MAX_LISTED_EXTENSIONS {
        return Err(RegistryError::TooLarge);
    }
    let mut extensions = listing
        .data
        .into_iter()
        // A listing entry SpaceTerm cannot address or present is omitted rather than failing the
        // whole registry.
        .filter(|entry| {
            entry.provides.iter().any(|provided| provided == "themes")
                && is_url_segment(&entry.id)
                && is_url_segment(&entry.version)
                && is_display_text(&entry.name, 128)
        })
        .map(|entry| RegistryExtension {
            id: entry.id,
            name: entry.name,
            version: entry.version,
            description: entry
                .description
                .filter(|description| is_display_text(description, 512)),
            authors: entry
                .authors
                .iter()
                .filter_map(|author| author_name(author))
                .take(8)
                .collect(),
            downloads: entry.download_count,
        })
        .collect::<Vec<_>>();
    extensions.sort_by(|left, right| {
        right
            .downloads
            .cmp(&left.downloads)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(extensions)
}

/// The presentable part of a Cargo-style author, without its contact address.
fn author_name(author: &str) -> Option<String> {
    let name = author.split('<').next().unwrap_or_default().trim();
    is_display_text(name, 128).then(|| name.to_owned())
}

fn is_display_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.chars().count() <= max && !value.chars().any(char::is_control)
}

fn is_url_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'))
        && !value.starts_with('.')
}

/// Reads the regular `themes/*.json` entries of a gzip-compressed extension archive.
///
/// Every other entry, including links and nested paths, is skipped unread. The unpacked stream
/// is bounded as a whole, so a small archive cannot expand without limit.
fn theme_families(archive: &[u8]) -> Result<Vec<Vec<u8>>, RegistryError> {
    let unpacked = flate2::read::GzDecoder::new(archive).take(MAX_UNPACKED_BYTES);
    let mut archive = tar::Archive::new(unpacked);
    let mut families = Vec::new();
    let entries = archive
        .entries()
        .map_err(|_| RegistryError::InvalidArchive)?;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_ARCHIVE_ENTRIES {
            return Err(RegistryError::InvalidArchive);
        }
        let mut entry = entry.map_err(|_| RegistryError::InvalidArchive)?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let is_family = {
            let path = entry.path_bytes();
            let path = path.strip_prefix(b"./").unwrap_or(&path);
            path.strip_prefix(b"themes/").is_some_and(|name| {
                name.len() > b".json".len() && name.ends_with(b".json") && !name.contains(&b'/')
            })
        };
        if !is_family {
            continue;
        }
        if families.len() >= MAX_EXTENSION_FAMILIES {
            return Err(RegistryError::InvalidArchive);
        }
        let size = entry.header().size().map_err(|_| RegistryError::InvalidArchive)?;
        if size > MAX_FAMILY_BYTES as u64 {
            return Err(RegistryError::TooLarge);
        }
        let mut bytes = Vec::with_capacity(size as usize);
        (&mut entry)
            .take(MAX_FAMILY_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| RegistryError::InvalidArchive)?;
        if bytes.len() > MAX_FAMILY_BYTES {
            return Err(RegistryError::TooLarge);
        }
        families.push(bytes);
    }
    if families.is_empty() {
        return Err(RegistryError::NoThemes);
    }
    Ok(families)
}

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
