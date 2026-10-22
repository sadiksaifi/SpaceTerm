use super::{DeviceCredential, DirectError};
use ring::{
    rand::SystemRandom,
    signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey},
};

pub(super) fn generate() -> Result<DeviceCredential, DirectError> {
    let bytes = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| DirectError::StorageUnavailable)?;
    credential(bytes.as_ref())
}
pub(super) fn credential(bytes: &[u8]) -> Result<DeviceCredential, DirectError> {
    if bytes.len() > 256 {
        return Err(DirectError::AuthenticationRefused);
    }
    Ed25519KeyPair::from_pkcs8(bytes).map_err(|_| DirectError::AuthenticationRefused)?;
    Ok(DeviceCredential(zeroize::Zeroizing::new(bytes.to_vec())))
}
pub(super) fn public(bytes: &[u8]) -> Result<[u8; 32], DirectError> {
    Ed25519KeyPair::from_pkcs8(bytes)
        .map_err(|_| DirectError::StorageUnavailable)?
        .public_key()
        .as_ref()
        .try_into()
        .map_err(|_| DirectError::StorageUnavailable)
}
pub(super) fn sign(bytes: &[u8], message: &[u8]) -> Result<Vec<u8>, DirectError> {
    Ok(Ed25519KeyPair::from_pkcs8(bytes)
        .map_err(|_| DirectError::AuthenticationRefused)?
        .sign(message)
        .as_ref()
        .to_vec())
}
pub(super) fn verify(public: &[u8; 32], message: &[u8], proof: &[u8]) -> Result<(), DirectError> {
    UnparsedPublicKey::new(&ED25519, public)
        .verify(message, proof)
        .map_err(|_| DirectError::AuthenticationRefused)
}
pub(super) fn hash(bytes: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut result = [0; 32];
    result.copy_from_slice(digest.as_ref());
    result
}
pub(super) fn random() -> Result<[u8; 32], DirectError> {
    crate::protocol::random().map_err(Into::into)
}
