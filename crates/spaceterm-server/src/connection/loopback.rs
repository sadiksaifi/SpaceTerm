//! An in-memory carrier pair for tests, with real descriptors for readiness.
use super::carrier::{Carrier, CarrierError, Flush, Offer, Readiness};
use crate::protocol::RawFrame;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

struct Queue {
    frames: Mutex<VecDeque<Vec<u8>>>,
    closed: AtomicBool,
}

/// One end of a loopback connection. Each direction holds at most `capacity` frames, so a full
/// peer exercises `Offer::Full`.
pub struct Loopback {
    incoming: Arc<Queue>,
    outgoing: Arc<Queue>,
    capacity: usize,
    /// Readable after the peer queued a frame for this end or took one this end sent.
    bell: UnixStream,
    /// Rings the peer's bell.
    peer: UnixStream,
}

impl Loopback {
    pub fn pair(capacity: usize) -> std::io::Result<(Self, Self)> {
        let (a_bell, a_ring) = UnixStream::pair()?;
        let (b_bell, b_ring) = UnixStream::pair()?;
        for stream in [&a_bell, &a_ring, &b_bell, &b_ring] {
            stream.set_nonblocking(true)?;
        }
        let queue = || {
            Arc::new(Queue {
                frames: Mutex::new(VecDeque::new()),
                closed: AtomicBool::new(false),
            })
        };
        let (a_to_b, b_to_a) = (queue(), queue());
        Ok((
            Self {
                incoming: Arc::clone(&b_to_a),
                outgoing: Arc::clone(&a_to_b),
                capacity,
                bell: a_bell,
                peer: b_ring,
            },
            Self {
                incoming: a_to_b,
                outgoing: b_to_a,
                capacity,
                bell: b_bell,
                peer: a_ring,
            },
        ))
    }

    fn ring(&self) {
        // A full bell already wakes the peer, so a refused byte is harmless.
        let _ = (&self.peer).write(&[1]);
    }

    fn closed(&self) -> bool {
        self.incoming.closed.load(Ordering::Acquire) || self.outgoing.closed.load(Ordering::Acquire)
    }
}

impl Carrier for Loopback {
    fn offer(&mut self, frame: &[u8]) -> Result<Offer, CarrierError> {
        if self.closed() {
            return Err(CarrierError::Closed);
        }
        RawFrame::parse(frame)?;
        let mut frames = self
            .outgoing
            .frames
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if frames.len() >= self.capacity {
            return Ok(Offer::Full);
        }
        frames.push_back(frame.to_vec());
        drop(frames);
        self.ring();
        Ok(Offer::Accepted)
    }

    fn flush(&mut self) -> Result<Flush, CarrierError> {
        if self.closed() {
            return Err(CarrierError::Closed);
        }
        Ok(Flush::Done)
    }

    fn receive(&mut self) -> Result<Option<RawFrame>, CarrierError> {
        let mut drained = [0; 64];
        while matches!((&self.bell).read(&mut drained), Ok(count) if count > 0) {}
        let frame = self
            .incoming
            .frames
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front();
        match frame {
            Some(frame) => {
                self.ring();
                Ok(Some(RawFrame::parse(&frame)?))
            }
            None if self.closed() => Err(CarrierError::Closed),
            None => Ok(None),
        }
    }

    fn readiness(&self) -> Readiness {
        Readiness {
            read: self.bell.as_raw_fd(),
            write: None,
        }
    }

    fn close(&mut self) {
        self.outgoing.closed.store(true, Ordering::Release);
        self.incoming.closed.store(true, Ordering::Release);
        self.ring();
    }
}
