//! The HTTPS transport the theme registry protocol and the release updater run over.
//!
//! Requests are HTTPS-only, including every redirect, and certificates verify against the
//! Operating-System trust store. Failures are classified without their native detail.

use std::time::Duration;

use ureq::tls::{RootCerts, TlsConfig};

use crate::theme_registry::{RegistryTransport, TransportError};

const TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_REDIRECTS: u32 = 4;

pub(crate) struct HttpsTransport {
    agent: ureq::Agent,
}

impl HttpsTransport {
    pub(crate) fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(MAX_REDIRECTS)
            .timeout_global(Some(TIMEOUT))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .user_agent("SpaceTerm")
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build()
            .into();
        Self { agent }
    }
}

impl RegistryTransport for HttpsTransport {
    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, TransportError> {
        let mut response = self.agent.get(url).call().map_err(classify)?;
        response
            .body_mut()
            .with_config()
            .limit(limit as u64)
            .read_to_vec()
            .map_err(classify)
    }
}

impl crate::updates::UpdateTransport for HttpsTransport {
    fn get(&self, url: &str, limit: usize) -> std::io::Result<Vec<u8>> {
        let mut response = self.agent.get(url).call().map_err(std::io::Error::other)?;
        response
            .body_mut()
            .with_config()
            .limit(limit as u64)
            .read_to_vec()
            .map_err(std::io::Error::other)
    }

    fn open(&self, url: &str, limit: u64) -> std::io::Result<Box<dyn std::io::Read + Send>> {
        // A release archive can take longer than the metadata timeout on a slow connection.
        let response = self
            .agent
            .get(url)
            .config()
            .timeout_global(None)
            .timeout_recv_response(Some(TIMEOUT))
            .timeout_recv_body(Some(DOWNLOAD_TIMEOUT))
            .build()
            .call()
            .map_err(std::io::Error::other)?;
        Ok(Box::new(
            response.into_body().into_with_config().limit(limit).reader(),
        ))
    }
}

fn classify(error: ureq::Error) -> TransportError {
    match error {
        ureq::Error::StatusCode(_) | ureq::Error::RequireHttpsOnly(_) => TransportError::Refused,
        ureq::Error::BodyExceedsLimit(_) => TransportError::TooLarge,
        _ => TransportError::Unreachable,
    }
}
