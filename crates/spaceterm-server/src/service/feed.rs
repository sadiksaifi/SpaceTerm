//! The signed server release feed, `spaceterm-<platform>.json` and its `.sig`. The feed names
//! one archive by size, digest and detached signature. It grants nothing until its own signature
//! verifies with the server artifact trust, and the archive it names is staged only after the
//! ordinary candidate verification.
use super::runtime;
use crate::ServerError;
use base64::Engine;
use serde::Deserialize;
use std::io::{Read, Write};
use std::time::Duration;

const RELEASES: &str = "https://github.com/sadiksaifi/SpaceTerm/releases";
/// The largest feed document, in bytes.
pub const MAX_FEED: usize = 4096;
/// The largest archive a feed or an upload may name, in bytes.
pub const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_REDIRECTS: u32 = 4;

#[cfg(target_os = "macos")]
const TARGET: &str = "aarch64-apple-darwin";
#[cfg(target_os = "linux")]
const TARGET: &str = "x86_64-unknown-linux-gnu";
#[cfg(target_os = "macos")]
const PLATFORM: &str = "darwin-arm64";
#[cfg(target_os = "linux")]
const PLATFORM: &str = "linux-x86_64";

/// A verified feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Feed {
    pub version: String,
    pub published_at: u64,
    pub archive: Archive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Archive {
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub signature: Vec<u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u32,
    identity: String,
    target: String,
    version: String,
    published_at: u64,
    archive: DocumentArchive,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentArchive {
    name: String,
    size: u64,
    sha256: String,
    signature: String,
}

/// Authenticates a feed for this identity and native target.
pub fn verify(identity: &str, feed: &[u8], signature: &[u8]) -> Result<Feed, ServerError> {
    if feed.len() > MAX_FEED || signature.len() != 64 {
        return Err(ServerError::RuntimeUnavailable);
    }
    runtime::authenticate(feed, signature, identity)?;
    let document: Document =
        serde_json::from_slice(feed).map_err(|_| ServerError::RuntimeUnavailable)?;
    let signature = base64::engine::general_purpose::STANDARD
        .decode(&document.archive.signature)
        .map_err(|_| ServerError::RuntimeUnavailable)?;
    if document.schema != 1
        || document.identity != identity
        || document.target != TARGET
        || document.version.is_empty()
        || document.version.len() > 64
        || !document
            .version
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || v == b'.')
        || document.archive.name != archive_name(&document.version)
        || document.archive.size == 0
        || document.archive.size > MAX_ARCHIVE
        || !runtime::digest_valid(&document.archive.sha256)
        || signature.len() != 64
    {
        return Err(ServerError::RuntimeUnavailable);
    }
    Ok(Feed {
        version: document.version,
        published_at: document.published_at,
        archive: Archive {
            name: document.archive.name,
            size: document.archive.size,
            sha256: document.archive.sha256,
            signature,
        },
    })
}

fn archive_name(version: &str) -> String {
    format!("spaceterm-{version}-{PLATFORM}.tar.gz")
}

/// A feed document and its detached signature.
pub type Signed = (Vec<u8>, Vec<u8>);

/// Where the latest feed and its archives come from.
pub trait Source: Send + Sync {
    /// The latest feed and its signature. `None` means this identity publishes no feed.
    fn latest(&self) -> Result<Option<Signed>, ServerError>;
    /// Writes the archive a verified feed names, at most its declared size.
    fn download(&self, feed: &Feed, into: &mut dyn Write) -> Result<(), ServerError>;
}

/// The public GitHub releases. Only the production identity publishes a feed there.
pub struct Releases {
    identity: String,
    agent: ureq::Agent,
}

impl Releases {
    pub fn new(identity: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(MAX_REDIRECTS)
            .timeout_global(Some(TIMEOUT))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .user_agent("SpaceTerm")
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::WebPki)
                    .build(),
            )
            .build()
            .into();
        Self {
            identity: identity.to_owned(),
            agent,
        }
    }

    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, ServerError> {
        self.agent
            .get(url)
            .call()
            .map_err(|_| ServerError::Direct(crate::direct::DirectError::Transport))?
            .body_mut()
            .with_config()
            .limit(limit as u64)
            .read_to_vec()
            .map_err(|_| ServerError::Direct(crate::direct::DirectError::Transport))
    }
}

impl Source for Releases {
    fn latest(&self) -> Result<Option<Signed>, ServerError> {
        if self.identity != "spaceterm" {
            return Ok(None);
        }
        let url = format!("{RELEASES}/latest/download/spaceterm-{PLATFORM}.json");
        let feed = self.get(&url, MAX_FEED)?;
        let signature = self.get(&format!("{url}.sig"), 256)?;
        Ok(Some((feed, signature)))
    }

    fn download(&self, feed: &Feed, into: &mut dyn Write) -> Result<(), ServerError> {
        let url = format!(
            "{RELEASES}/download/v{}/{}",
            feed.version, feed.archive.name
        );
        let transport = || ServerError::Direct(crate::direct::DirectError::Transport);
        let response = self
            .agent
            .get(&url)
            .config()
            .timeout_global(None)
            .timeout_recv_response(Some(TIMEOUT))
            .timeout_recv_body(Some(DOWNLOAD_TIMEOUT))
            .build()
            .call()
            .map_err(|_| transport())?;
        let mut reader = response
            .into_body()
            .into_with_config()
            .limit(feed.archive.size)
            .reader();
        copy_exactly(&mut reader, into, feed.archive.size).map_err(|_| transport())
    }
}

/// Copies exactly `size` bytes, refusing a shorter source.
pub(crate) fn copy_exactly(
    from: &mut dyn Read,
    into: &mut dyn Write,
    size: u64,
) -> std::io::Result<()> {
    let copied = std::io::copy(&mut from.take(size), into)?;
    if copied != size {
        return Err(std::io::ErrorKind::UnexpectedEof.into());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ring::signature::KeyPair;

    /// Signs a feed with the fixture trust of `runtime::test_support`.
    pub(crate) fn signed(
        identity: &str,
        version: &str,
        archive: &[u8],
        signature: &[u8],
    ) -> (Vec<u8>, Vec<u8>) {
        let feed = serde_json::to_vec(&serde_json::json!({
            "schema": 1,
            "identity": identity,
            "target": TARGET,
            "version": version,
            "published_at": 1_700_000_000u64,
            "archive": {
                "name": archive_name(version),
                "size": archive.len(),
                "sha256": ring::digest::digest(&ring::digest::SHA256, archive)
                    .as_ref()
                    .iter()
                    .map(|v| format!("{v:02x}"))
                    .collect::<String>(),
                "signature": base64::engine::general_purpose::STANDARD.encode(signature),
            },
        }))
        .unwrap();
        let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap();
        assert_eq!(key.public_key().as_ref().len(), 32);
        let sig = key.sign(&feed).as_ref().to_vec();
        (feed, sig)
    }

    #[test]
    fn a_feed_verifies_only_for_its_identity_target_and_signature() {
        let root = std::env::temp_dir().join(format!(
            "spaceterm-feed-{:x}",
            u64::from_be_bytes(crate::protocol::random().unwrap()[..8].try_into().unwrap())
        ));
        runtime::test_support::with_artifact(
            &root,
            "spaceterm-development",
            |archive, signature| {
                let archive = std::fs::read(archive).unwrap();
                let signature = std::fs::read(signature).unwrap();
                let (feed, sig) = signed("spaceterm-development", "1.2.3", &archive, &signature);
                let verified = verify("spaceterm-development", &feed, &sig).unwrap();
                assert_eq!(verified.version, "1.2.3");
                assert_eq!(verified.archive.size, archive.len() as u64);
                assert_eq!(verified.archive.signature, signature);
                assert_eq!(
                    verify("spaceterm-preflight", &feed, &sig),
                    Err(ServerError::RuntimeUnavailable)
                );
                let mut tampered = feed.clone();
                tampered[10] ^= 1;
                assert_eq!(
                    verify("spaceterm-development", &tampered, &sig),
                    Err(ServerError::RuntimeUnavailable)
                );
                let (renamed, sig) =
                    signed("spaceterm-development", "1.2.3/../x", &archive, &signature);
                assert_eq!(
                    verify("spaceterm-development", &renamed, &sig),
                    Err(ServerError::RuntimeUnavailable)
                );
            },
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn copying_an_archive_refuses_a_short_source() {
        let mut out = Vec::new();
        assert!(copy_exactly(&mut &b"abc"[..], &mut out, 3).is_ok());
        assert!(copy_exactly(&mut &b"ab"[..], &mut Vec::new(), 3).is_err());
        assert!(copy_exactly(&mut &b"abcd"[..], &mut out, 3).is_ok());
        assert_eq!(out, b"abcabc");
    }
}
