//! A registry transport that answers from memory, for tests of registry consumers.

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::{RegistryTransport, TransportError};

/// Answers each URL with a prepared result and records every request in order.
#[derive(Default)]
pub(crate) struct MemoryTransport {
    routes: BTreeMap<String, Result<Vec<u8>, TransportError>>,
    requests: Mutex<Vec<String>>,
}

impl MemoryTransport {
    pub(crate) fn route(mut self, url: &str, response: Result<Vec<u8>, TransportError>) -> Self {
        self.routes.insert(url.to_owned(), response);
        self
    }

    pub(crate) fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl RegistryTransport for MemoryTransport {
    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, TransportError> {
        self.requests.lock().unwrap().push(url.to_owned());
        let response = self
            .routes
            .get(url)
            .cloned()
            .unwrap_or(Err(TransportError::Refused))?;
        if response.len() > limit {
            return Err(TransportError::TooLarge);
        }
        Ok(response)
    }
}

/// A gzip-compressed extension archive holding the given `themes/` family documents.
pub(crate) fn extension_archive(families: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for (name, bytes) in families {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(bytes.len() as u64);
        builder
            .append_data(&mut header, format!("./themes/{name}"), *bytes)
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}
