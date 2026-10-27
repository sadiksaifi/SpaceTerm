//! The paired carrier: TLS and the WebSocket directly on the nonblocking TCP socket, one binary
//! message per frame. There is no relay thread and no intermediate socket.
//!
//! [`TlsSocket`] drives rustls by hand so that every call returns `WouldBlock` instead of waiting:
//! reads drain decrypted bytes before reading TCP, and writes queue ciphertext that `flush` sends.
//! The TCP socket disables Nagle's algorithm and keeps at most [`NOTSENT_LOWAT`] bytes unsent in
//! the kernel, so priority stays in the outbox.
use super::carrier::{Carrier, CarrierError, Flush, Offer, Priority, Readiness};
use crate::protocol::{RawFrame, limits};
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use tungstenite::{Message, WebSocket, protocol::WebSocketConfig};

/// Unsent bytes the kernel holds before the socket stops reporting writable.
pub const NOTSENT_LOWAT: u32 = 16 * 1024;
/// Ciphertext and plaintext rustls buffers for sending, which two accepted frames fit.
const TLS_BUFFER: usize = 2 * limits::MAX_FRAME_BYTES + 64 * 1024;

/// Sets `TCP_NODELAY` and `TCP_NOTSENT_LOWAT` on a carrier's socket.
pub fn tune(tcp: &TcpStream) -> std::io::Result<()> {
    tcp.set_nodelay(true)?;
    #[cfg(target_os = "macos")]
    const TCP_NOTSENT_LOWAT: libc::c_int = 0x201;
    #[cfg(not(target_os = "macos"))]
    const TCP_NOTSENT_LOWAT: libc::c_int = libc::TCP_NOTSENT_LOWAT;
    let value = NOTSENT_LOWAT as libc::c_int;
    // SAFETY: setsockopt reads one c_int from a live local for the borrowed live descriptor.
    let result = unsafe {
        libc::setsockopt(
            tcp.as_raw_fd(),
            libc::IPPROTO_TCP,
            TCP_NOTSENT_LOWAT,
            (&raw const value).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Largest WebSocket message before admission completes: the pairing and admission messages
/// of an unauthenticated peer.
pub const PREAMBLE_MESSAGE_BYTES: usize = 64 * 1024;

/// The WebSocket configuration of a paired socket until admission completes. A peer that has not
/// proved itself sends messages of at most [`PREAMBLE_MESSAGE_BYTES`].
pub fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(PREAMBLE_MESSAGE_BYTES)
        .write_buffer_size(0)
        .max_write_buffer_size(TLS_BUFFER)
        .max_message_size(Some(PREAMBLE_MESSAGE_BYTES))
        .max_frame_size(Some(PREAMBLE_MESSAGE_BYTES))
}

/// A rustls connection on a nonblocking TCP socket.
pub struct TlsSocket {
    tls: rustls::Connection,
    tcp: TcpStream,
    /// TCP bytes written, so a flush reports whether it progressed.
    written: u64,
}

impl TlsSocket {
    pub fn new(mut tls: rustls::Connection, tcp: TcpStream) -> Self {
        tls.set_buffer_limit(Some(TLS_BUFFER));
        Self {
            tls,
            tcp,
            written: 0,
        }
    }

    pub fn tcp(&self) -> &TcpStream {
        &self.tcp
    }

    pub fn tls(&self) -> &rustls::Connection {
        &self.tls
    }

    /// Writes queued ciphertext until the socket would block.
    fn push(&mut self) -> std::io::Result<()> {
        while self.tls.wants_write() {
            match self.tls.write_tls(&mut self.tcp) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(count) => self.written += count as u64,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Reads and processes ciphertext. `Ok(false)` means TCP reached its end.
    fn pull(&mut self) -> std::io::Result<bool> {
        let count = loop {
            match self.tls.read_tls(&mut self.tcp) {
                Ok(count) => break count,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        };
        self.tls
            .process_new_packets()
            .map_err(|_| std::io::Error::from(ErrorKind::InvalidData))?;
        // Alerts and key updates answer at once; a full socket keeps them queued.
        match self.push() {
            Err(error) if error.kind() != ErrorKind::WouldBlock => return Err(error),
            _ => {}
        }
        Ok(count > 0)
    }

    /// Completes the TLS handshake, waiting on the socket's readiness until `deadline`.
    pub fn handshake(&mut self, deadline: std::time::Instant) -> std::io::Result<()> {
        while self.tls.is_handshaking() || self.tls.wants_write() {
            if self.tls.wants_write() {
                match self.push() {
                    Ok(()) => continue,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        if !super::io::wait_fd(self.tcp.as_raw_fd(), true, deadline) {
                            return Err(ErrorKind::TimedOut.into());
                        }
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            match self.pull() {
                Ok(true) => {}
                Ok(false) => return Err(ErrorKind::UnexpectedEof.into()),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if !super::io::wait_fd(self.tcp.as_raw_fd(), false, deadline) {
                        return Err(ErrorKind::TimedOut.into());
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

impl Read for TlsSocket {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match self.tls.reader().read(bytes) {
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                result => return result,
            }
            // After TCP's end the reader reports it, so this loop always ends.
            self.pull()?;
        }
    }
}

impl Write for TlsSocket {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = self.tls.writer().write(bytes)?;
        match self.push() {
            Err(error) if error.kind() != ErrorKind::WouldBlock => return Err(error),
            _ => {}
        }
        if count == 0 && !bytes.is_empty() {
            return Err(ErrorKind::WouldBlock.into());
        }
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.tls.writer().flush()?;
        self.push()
    }
}

pub type Ws = WebSocket<TlsSocket>;

/// Whether a tungstenite error only means the socket would block.
pub fn would_block(error: &tungstenite::Error) -> bool {
    matches!(error, tungstenite::Error::Io(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted))
}

pub struct WssCarrier {
    ws: Ws,
    /// Frames accepted since the last complete flush.
    unflushed: usize,
    closing: bool,
}

impl WssCarrier {
    /// Takes an admitted WebSocket whose socket is already nonblocking and tuned, and raises its
    /// message bound to one whole frame, as admission proved the peer.
    pub fn new(mut ws: Ws) -> Self {
        ws.set_config(|config| {
            config.max_message_size = Some(limits::MAX_FRAME_BYTES);
            config.max_frame_size = Some(limits::MAX_FRAME_BYTES);
        });
        Self {
            ws,
            unflushed: 0,
            closing: false,
        }
    }
}

impl Drop for WssCarrier {
    /// Discards unread input first, so TCP closes in order instead of resetting the connection
    /// before the peer read the goodbye and the WebSocket Close.
    fn drop(&mut self) {
        super::io::discard_unread(self.ws.get_ref().tcp.as_raw_fd());
    }
}

impl Carrier for WssCarrier {
    fn offer(&mut self, frame: &[u8], _priority: Priority) -> Result<Offer, CarrierError> {
        if self.closing {
            return Err(CarrierError::Closed);
        }
        if self.unflushed > 1 {
            return Ok(Offer::Full);
        }
        match self.ws.write(Message::Binary(frame.to_vec().into())) {
            Ok(()) => {}
            // Tungstenite kept the message and writes it during `flush`.
            Err(error) if would_block(&error) => {}
            Err(_) => return Err(CarrierError::Failed),
        }
        self.unflushed += 1;
        Ok(Offer::Accepted)
    }

    fn flush(&mut self) -> Result<Flush, CarrierError> {
        let before = self.ws.get_ref().written;
        let result = self.ws.flush();
        let progressed = self.ws.get_ref().written != before;
        match result {
            Ok(()) => {
                self.unflushed = 0;
                Ok(Flush {
                    progressed,
                    pending: false,
                })
            }
            Err(error) if would_block(&error) => Ok(Flush {
                progressed,
                pending: true,
            }),
            // The Close handshake finished: nothing remains to write.
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed)
                if self.closing =>
            {
                Ok(Flush {
                    progressed,
                    pending: false,
                })
            }
            Err(_) => Err(CarrierError::Failed),
        }
    }

    fn receive(&mut self) -> Result<Option<RawFrame>, CarrierError> {
        if self.closing {
            return Err(CarrierError::Closed);
        }
        loop {
            match self.ws.read() {
                Ok(Message::Binary(bytes)) => return Ok(Some(RawFrame::parse(&bytes)?)),
                // Tungstenite queues the pong itself; the next flush writes it.
                Ok(Message::Ping(_) | Message::Pong(_)) => {}
                Ok(Message::Close(_)) => return Err(CarrierError::Closed),
                Ok(Message::Text(_) | Message::Frame(_)) => {
                    return Err(CarrierError::Decode(
                        crate::protocol::DecodeError::Malformed,
                    ));
                }
                Err(error) if would_block(&error) => return Ok(None),
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    return Err(CarrierError::Closed);
                }
                Err(tungstenite::Error::Capacity(_)) => {
                    return Err(CarrierError::Decode(
                        crate::protocol::DecodeError::Oversized,
                    ));
                }
                Err(_) => return Err(CarrierError::Failed),
            }
        }
    }

    fn readiness(&self) -> Readiness {
        let fd = self.ws.get_ref().tcp.as_raw_fd();
        Readiness {
            read: fd,
            write: Some(fd),
            progress: None,
            deadline: None,
        }
    }

    fn close(&mut self) {
        if !self.closing {
            self.closing = true;
            // Queues the Close frame; `flush` writes it after every accepted frame.
            let _ = self.ws.close(None);
        }
    }
}
