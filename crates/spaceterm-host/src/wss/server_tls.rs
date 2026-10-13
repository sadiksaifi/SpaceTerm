//! TLS certificates and live signatures use the retained server identity.

use std::sync::Arc;

use rcgen::{CertificateParams, PKCS_ED25519, PublicKeyData};
use rustls::{
    SignatureAlgorithm, SignatureScheme,
    sign::{CertifiedKey, SingleCertAndKey},
};
use spaceterm_protocol::authentication::SigningKey as ProtocolSigningKey;

use crate::{server_keys::ServerIdentity, transport::TransportError};

struct IdentitySigner {
    identity: Arc<ServerIdentity>,
    public: [u8; 32],
}
impl std::fmt::Debug for IdentitySigner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("IdentitySigner([redacted])")
    }
}
impl PublicKeyData for IdentitySigner {
    fn der_bytes(&self) -> &[u8] {
        &self.public
    }
    fn algorithm(&self) -> &'static rcgen::SignatureAlgorithm {
        &PKCS_ED25519
    }
}
impl rcgen::SigningKey for IdentitySigner {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rcgen::Error> {
        self.identity
            .sign(message)
            .map(|signature| signature.to_vec())
            .map_err(|_| rcgen::Error::RemoteKeyError)
    }
}
impl rustls::sign::SigningKey for IdentitySigner {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn rustls::sign::Signer>> {
        offered.contains(&SignatureScheme::ED25519).then(|| {
            Box::new(Self {
                identity: self.identity.clone(),
                public: self.public,
            }) as Box<dyn rustls::sign::Signer>
        })
    }
    fn public_key(&self) -> Option<rustls::pki_types::SubjectPublicKeyInfoDer<'_>> {
        Some(self.subject_public_key_info().into())
    }
    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::ED25519
    }
}
impl rustls::sign::Signer for IdentitySigner {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rustls::Error> {
        self.identity
            .sign(message)
            .map(|signature| signature.to_vec())
            .map_err(|_| rustls::Error::General("server identity signing unavailable".into()))
    }
    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::ED25519
    }
}

/// Builds a TLS 1.3 server configuration without exporting private key material.
/// The self-signed certificate carries the exact enrolled Ed25519 public key.
/// Hostname discovery grants no trust: WSS clients must still pin that key and
/// complete the purpose-bound SpaceTerm authentication handshake.
pub fn server_tls_config(
    identity: Arc<ServerIdentity>,
) -> Result<Arc<rustls::ServerConfig>, TransportError> {
    let signer = Arc::new(IdentitySigner {
        public: identity.public_key(),
        identity,
    });
    let certificate = CertificateParams::new(vec!["localhost".into()])
        .and_then(|parameters| parameters.self_signed(signer.as_ref()))
        .map_err(|_| TransportError::TlsIdentityRejected)?;
    let certified = CertifiedKey::new(vec![certificate.der().clone()], signer);
    certified
        .keys_match()
        .map_err(|_| TransportError::TlsIdentityRejected)?;
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|_| TransportError::InvalidConfiguration)?
    .with_no_client_auth()
    .with_cert_resolver(Arc::new(SingleCertAndKey::from(certified)));
    Ok(Arc::new(config))
}
