//! Paired connections over QUIC ([`crate::connection::quic`]): the pinned TLS configurations and
//! the pairing messages, which run on the L0 stream as length-prefixed JSON, each under a
//! deadline.
//!
//! The server presents its pinned certificate. The client presents a self-signed certificate of
//! its device key, made for each connection; the server accepts any such certificate during the
//! handshake and later requires its key to be the enrolled key of the device that authenticates.
use super::*;
use crate::connection::quic::{ALPN, QuicError, QuicStream};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

pub(super) const HANDSHAKE_LIMIT: Duration = Duration::from_secs(5);

impl client::Messages for QuicStream {
    fn send(&mut self, value: &impl Serialize) -> Result<(), DirectError> {
        let bytes = zeroize::Zeroizing::new(
            serde_json::to_vec(value).map_err(|_| DirectError::InvalidRequest)?,
        );
        if bytes.len() > wire::MAX_AUTH_FRAME {
            return Err(DirectError::Capacity);
        }
        let frame = zeroize::Zeroizing::new(crate::protocol::prefixed(&bytes));
        self.send_bytes(&frame, Instant::now() + HANDSHAKE_LIMIT)
            .map_err(transport)
    }
    fn receive<T: serde::de::DeserializeOwned>(&mut self) -> Result<T, DirectError> {
        let bytes = zeroize::Zeroizing::new(
            self.receive_prefixed(wire::MAX_AUTH_FRAME, Instant::now() + HANDSHAKE_LIMIT)
                .map_err(transport)?,
        );
        serde_json::from_slice(&bytes).map_err(|_| DirectError::InvalidRequest)
    }
}

pub(super) fn transport(error: QuicError) -> DirectError {
    match error {
        QuicError::Configuration => DirectError::StorageUnavailable,
        QuicError::Transport | QuicError::TimedOut | QuicError::Closed => DirectError::Transport,
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A self-signed certificate of an Ed25519 PKCS#8 key, as the server's own was made.
fn certificate(key: &[u8]) -> Result<CertificateDer<'static>, DirectError> {
    let pair = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(&key.into(), &rcgen::PKCS_ED25519)
        .map_err(|_| DirectError::StorageUnavailable)?;
    rcgen::CertificateParams::new(vec![crate::connection::quic::SERVER_NAME.into()])
        .and_then(|params| params.self_signed(&pair))
        .map(|certificate| certificate.der().clone())
        .map_err(|_| DirectError::StorageUnavailable)
}

#[cfg(test)]
pub(super) fn tests_certificate(key: &[u8]) -> Vec<u8> {
    certificate(key).unwrap().to_vec()
}

/// The Ed25519 public key of a certificate's subject, from the parsed certificate's
/// SubjectPublicKeyInfo, which must be exactly an Ed25519 key. A key elsewhere in the
/// certificate, such as in an extension, never counts.
pub(super) fn certificate_key(certificate: &[u8]) -> Option<[u8; 32]> {
    const ED25519_SPKI: [u8; 12] = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let certificate = CertificateDer::from(certificate);
    let parsed = webpki::EndEntityCert::try_from(&certificate).ok()?;
    let spki = parsed.subject_public_key_info();
    let key = spki.as_ref().strip_prefix(ED25519_SPKI.as_slice())?;
    key.try_into().ok()
}

/// The client's configuration: the pinned server certificate, and this device's certificate.
fn client_config(
    pin: &ServerPin,
    credential: &DeviceCredential,
    mismatch: Arc<AtomicBool>,
) -> Result<noq_proto::ClientConfig, DirectError> {
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(credential.expose_bytes().to_vec()));
    let certificate = certificate(credential.expose_bytes())?;
    presenting(pin, certificate, key, mismatch)
}

/// A client configuration that verifies the pinned server and presents `certificate`.
pub(super) fn presenting(
    pin: &ServerPin,
    certificate: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
    mismatch: Arc<AtomicBool>,
) -> Result<noq_proto::ClientConfig, DirectError> {
    let mut tls = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| DirectError::Transport)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned::new(pin.certificate, mismatch)))
        .with_client_auth_cert(vec![certificate], key)
        .map_err(|_| DirectError::StorageUnavailable)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    crate::connection::quic::client_config(tls).map_err(transport)
}

/// Accepts any client certificate whose handshake signature verifies. The pairing exchange that
/// follows decides which device it belongs to.
#[derive(Debug)]
struct AnyDevice;

impl rustls::server::danger::ClientCertVerifier for AnyDevice {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        if !intermediates.is_empty() {
            return Err(rustls::Error::General(
                "Unexpected certificate chain".into(),
            ));
        }
        Ok(rustls::server::danger::ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        // rustls does not hold the client to `supported_verify_schemes`, so only an Ed25519
        // signature, over the certificate's own Ed25519 key, proves a device.
        if dss.scheme != rustls::SignatureScheme::ED25519 {
            return Err(rustls::Error::PeerMisbehaved(
                rustls::PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme,
            ));
        }
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![rustls::SignatureScheme::ED25519]
    }
}

/// The server's configuration: its pinned certificate, and a certificate from every client.
pub(super) fn server_config(
    certificate: &[u8],
    key: &[u8],
) -> Result<Arc<noq_proto::ServerConfig>, DirectError> {
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| DirectError::Transport)?
        .with_client_cert_verifier(Arc::new(AnyDevice))
        .with_single_cert(
            vec![CertificateDer::from(certificate.to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec())),
        )
        .map_err(|_| DirectError::StorageUnavailable)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    // 0-RTT data could be replayed; nothing here needs it.
    tls.max_early_data_size = 0;
    crate::connection::quic::server_config(tls).map_err(transport)
}

/// Accepts only the server certificate whose hash is pinned, and records a mismatch.
#[derive(Debug)]
pub(super) struct Pinned {
    certificate: [u8; 32],
    mismatch: Arc<AtomicBool>,
}
impl Pinned {
    pub(super) fn new(certificate: [u8; 32], mismatch: Arc<AtomicBool>) -> Self {
        Self {
            certificate,
            mismatch,
        }
    }
}
impl rustls::client::danger::ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if !intermediates.is_empty() || crypto::hash(cert.as_ref()) != self.certificate {
            self.mismatch.store(true, Ordering::Release);
            return Err(rustls::Error::General("Pinned certificate mismatch".into()));
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Dials the endpoint's address and port in UDP, verifies the pinned certificate, and opens the
/// L0 stream on which the pairing exchange runs.
pub(super) fn dial(
    endpoint: &str,
    pin: &ServerPin,
    credential: &DeviceCredential,
) -> Result<QuicStream, DirectError> {
    let address = Endpoint::from_url(endpoint)?;
    let deadline = Instant::now() + HANDSHAKE_LIMIT;
    let addresses = address.resolve(deadline)?;
    let mismatch = Arc::new(AtomicBool::new(false));
    let config = client_config(pin, credential, Arc::clone(&mismatch))?;
    QuicStream::connect(config, &addresses, deadline).map_err(|error| {
        if mismatch.load(Ordering::Acquire) {
            DirectError::PinMismatch
        } else {
            transport(error)
        }
    })
}
