//! The release signing keys, `packaging/update-trust.json`: two raw 32-byte Ed25519 public keys in
//! standard base64. A signature is valid when it verifies under `current` or under `backup`.
//!
//! Signatures are pure Ed25519 as in RFC 8032, with no prehash and no context, and every
//! signature is exactly 64 raw bytes.
use base64::Engine;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::Deserialize;

/// Bytes in an Ed25519 signature.
pub const SIGNATURE_BYTES: usize = 64;
/// Bytes in an Ed25519 public key.
const KEY_BYTES: usize = 32;

/// The keys a Release build trusts for its feeds and archives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateTrust {
    current: [u8; KEY_BYTES],
    backup: [u8; KEY_BYTES],
}

/// A trust document that is not two base64 Ed25519 public keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the update trust is malformed")]
pub struct InvalidTrust;

/// A signature that verifies under neither key, or that is not 64 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the signature does not verify")]
pub struct BadSignature;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    current: String,
    backup: String,
}

impl UpdateTrust {
    /// The keys this build embeds from `packaging/update-trust.json`.
    pub fn release() -> Result<Self, InvalidTrust> {
        Self::parse(match option_env!("SPACETERM_RELEASE_UPDATE_TRUST_JSON") {
            Some(document) => document.as_bytes(),
            None => include_bytes!("../../../../packaging/update-trust.json"),
        })
    }

    /// Reads a trust document.
    pub fn parse(document: &[u8]) -> Result<Self, InvalidTrust> {
        let document: Document = serde_json::from_slice(document).map_err(|_| InvalidTrust)?;
        Ok(Self {
            current: key(&document.current)?,
            backup: key(&document.backup)?,
        })
    }

    /// Checks `signature` over the exact bytes of `message` under `current`, then `backup`.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), BadSignature> {
        if signature.len() != SIGNATURE_BYTES {
            return Err(BadSignature);
        }
        [&self.current, &self.backup]
            .into_iter()
            .any(|key| {
                UnparsedPublicKey::new(&ED25519, key)
                    .verify(message, signature)
                    .is_ok()
            })
            .then_some(())
            .ok_or(BadSignature)
    }
}

fn key(text: &str) -> Result<[u8; KEY_BYTES], InvalidTrust> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(InvalidTrust)
}

#[cfg(test)]
pub(crate) mod testing {
    use base64::Engine;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    /// A generated signing key for tests only.
    pub(crate) struct TestKey(Ed25519KeyPair);

    impl TestKey {
        pub(crate) fn generate() -> Self {
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
            Self(Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap())
        }

        pub(crate) fn public(&self) -> String {
            base64::engine::general_purpose::STANDARD.encode(self.0.public_key().as_ref())
        }

        pub(crate) fn sign(&self, message: &[u8]) -> Vec<u8> {
            self.0.sign(message).as_ref().to_vec()
        }
    }

    /// A trust document of these two keys.
    pub(crate) fn trust(current: &TestKey, backup: &TestKey) -> super::UpdateTrust {
        let document =
            serde_json::json!({ "current": current.public(), "backup": backup.public() });
        super::UpdateTrust::parse(document.to_string().as_bytes()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{TestKey, trust};
    use super::*;

    #[test]
    fn signatures_verify_under_the_current_or_the_backup_key() {
        let (current, backup, other) = (
            TestKey::generate(),
            TestKey::generate(),
            TestKey::generate(),
        );
        let trust = trust(&current, &backup);
        let message = b"spaceterm-core-1.4.0-linux-x86_64.tar.gz";
        assert_eq!(trust.verify(message, &current.sign(message)), Ok(()));
        assert_eq!(trust.verify(message, &backup.sign(message)), Ok(()));
        assert_eq!(
            trust.verify(message, &other.sign(message)),
            Err(BadSignature)
        );
        assert_eq!(
            trust.verify(b"another message", &current.sign(message)),
            Err(BadSignature)
        );
    }

    #[test]
    fn each_key_binds_its_signature_to_the_exact_bytes() {
        let (current, backup) = (TestKey::generate(), TestKey::generate());
        let trust = trust(&current, &backup);
        let message = b"feed";
        for key in [&current, &backup] {
            let signature = key.sign(message);
            assert_eq!(trust.verify(message, &signature), Ok(()));
            assert_eq!(trust.verify(b"fees", &signature), Err(BadSignature));
            assert_eq!(trust.verify(b"feed\n", &signature), Err(BadSignature));
        }
    }

    #[test]
    fn signatures_are_exactly_64_raw_bytes() {
        let (current, backup) = (TestKey::generate(), TestKey::generate());
        let trust = trust(&current, &backup);
        let message = b"feed";
        for key in [&current, &backup] {
            let signature = key.sign(message);
            let truncated = &signature[..63];
            let newline = [signature.as_slice(), b"\n"].concat();
            let extended = [signature.as_slice(), &[0]].concat();
            let encoded = base64::engine::general_purpose::STANDARD.encode(&signature);
            for refused in [truncated, &newline, &extended, encoded.as_bytes()] {
                assert_eq!(trust.verify(message, refused), Err(BadSignature));
            }
        }
    }

    #[test]
    fn a_trust_document_holds_exactly_two_32_byte_keys() {
        let key = TestKey::generate().public();
        let short = base64::engine::general_purpose::STANDARD.encode([7; 31]);
        for document in [
            serde_json::json!({ "current": key, "backup": short }),
            serde_json::json!({ "current": key }),
            serde_json::json!({ "current": key, "backup": key, "next": key }),
            serde_json::json!({ "current": "not base64", "backup": key }),
        ] {
            assert_eq!(
                UpdateTrust::parse(document.to_string().as_bytes()),
                Err(InvalidTrust)
            );
        }
    }
}
