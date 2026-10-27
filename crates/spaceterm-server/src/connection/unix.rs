//! The account-local carrier: frames back to back on a nonblocking Unix socket.
use super::carrier::{Carrier, CarrierError, Flush, Offer, Priority, Readiness};
use crate::protocol::RawFrame;
use crate::protocol::frame::FrameReader;
use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

/// Bytes read from the socket per call.
const READ_CHUNK: usize = 64 * 1024;

pub struct UnixCarrier {
    stream: UnixStream,
    reader: FrameReader,
    buffer: Vec<u8>,
    /// Accepted frames, the first partly written up to `offset`.
    outgoing: VecDeque<Vec<u8>>,
    offset: usize,
    closing: bool,
    eof: bool,
}

impl UnixCarrier {
    pub fn new(stream: UnixStream) -> std::io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            reader: FrameReader::default(),
            buffer: vec![0; READ_CHUNK],
            outgoing: VecDeque::new(),
            offset: 0,
            closing: false,
            eof: false,
        })
    }
}

impl Carrier for UnixCarrier {
    fn offer(&mut self, frame: &[u8], _priority: Priority) -> Result<Offer, CarrierError> {
        if self.closing {
            return Err(CarrierError::Closed);
        }
        if self.outgoing.len() > 1 {
            return Ok(Offer::Full);
        }
        self.outgoing.push_back(frame.to_vec());
        Ok(Offer::Accepted)
    }

    fn flush(&mut self) -> Result<Flush, CarrierError> {
        let mut progressed = false;
        while let Some(front) = self.outgoing.front() {
            match (&self.stream).write(&front[self.offset..]) {
                Ok(0) => return Err(CarrierError::Failed),
                Ok(count) => {
                    progressed = true;
                    self.offset += count;
                    if self.offset == front.len() {
                        self.outgoing.pop_front();
                        self.offset = 0;
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => return Err(CarrierError::Failed),
            }
        }
        if self.closing && self.outgoing.is_empty() {
            let _ = self.stream.shutdown(std::net::Shutdown::Write);
        }
        Ok(Flush {
            progressed,
            pending: !self.outgoing.is_empty(),
        })
    }

    fn receive(&mut self) -> Result<Option<RawFrame>, CarrierError> {
        if self.closing {
            return Err(CarrierError::Closed);
        }
        loop {
            if let Some(frame) = self.reader.next_frame()? {
                return Ok(Some(frame));
            }
            if self.eof {
                return Err(CarrierError::Closed);
            }
            match (&self.stream).read(&mut self.buffer) {
                Ok(0) => self.eof = true,
                Ok(count) => self.reader.feed(&self.buffer[..count]),
                Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => return Err(CarrierError::Failed),
            }
        }
    }

    fn readiness(&self) -> Readiness {
        let fd = self.stream.as_raw_fd();
        Readiness {
            read: fd,
            write: Some(fd),
        }
    }

    fn close(&mut self) {
        self.closing = true;
    }
}
