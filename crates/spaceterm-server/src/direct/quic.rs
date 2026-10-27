//! Paired connections over the QUIC prototype ([`crate::connection::quic`]). The pairing messages
//! run on the L0 stream as length-prefixed JSON, with the same bounds and deadlines as WSS. The
//! prototype carries connections only: pairing, claims and revocation stay on WSS.
use super::*;
use crate::connection::quic::{QuicError, QuicStream, secret_key};
use std::time::{Duration, Instant};

const HANDSHAKE_LIMIT: Duration = Duration::from_secs(5);

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

fn transport(error: QuicError) -> DirectError {
    match error {
        QuicError::Key => DirectError::AuthenticationRefused,
        QuicError::Runtime | QuicError::Transport | QuicError::TimedOut => DirectError::Transport,
    }
}

/// Dials the endpoint's address and port in UDP. iroh's TLS proves the pinned server key, and the
/// pairing exchange then proves the device as over WSS.
pub(super) fn connect(
    endpoint: &str,
    paired: &PairedDevice,
    credential: &DeviceCredential,
) -> Result<DirectConnection, DirectError> {
    let address = wss::validate_endpoint(endpoint)?;
    let deadline = Instant::now() + HANDSHAKE_LIMIT;
    let addresses = address.resolve(deadline)?;
    let secret =
        secret_key(credential.expose_bytes(), &credential.public_key()?).map_err(transport)?;
    let mut stream = QuicStream::connect(secret, &paired.pin.public_key, &addresses, deadline)
        .map_err(transport)?;
    let answer = client::exchange_on(
        &mut stream,
        &paired.pin,
        credential,
        wire::Action::Connect {
            paired: paired.clone(),
        },
        None,
        None,
    )?;
    if !matches!(answer, wire::Answer::Connected) {
        return Err(DirectError::AuthenticationRefused);
    }
    Ok(DirectConnection {
        transport: Transport::Quic(stream),
        paired: paired.clone(),
    })
}
