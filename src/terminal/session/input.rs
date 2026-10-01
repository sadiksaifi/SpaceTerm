//! Keeps PTY input ordered and bounded while the child applies backpressure.
use std::collections::VecDeque;
use std::io::{self, Write};
use std::time::{Duration, Instant};

const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;
const WRITE_BUDGET: usize = 64 * 1024;
const RETRY_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Default)]
pub(super) struct PtyInput {
    chunks: VecDeque<Vec<u8>>,
    offset: usize,
    retained_bytes: usize,
    retry: Option<Instant>,
}

impl PtyInput {
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.retry
    }

    pub(super) fn enqueue(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > MAX_PENDING_BYTES.saturating_sub(self.retained_bytes) {
            return Err(io::Error::other("PTY input queue is full"));
        }
        if !bytes.is_empty() {
            self.retained_bytes += bytes.len();
            self.chunks.push_back(bytes.to_vec());
        }
        Ok(())
    }

    /// The writer must return WouldBlock instead of waiting for child input consumption.
    pub(super) fn drain(&mut self, writer: &mut impl Write) -> io::Result<()> {
        let mut budget = WRITE_BUDGET;
        for _ in 0..16 {
            let Some(chunk) = self.chunks.front() else {
                self.retry = None;
                return writer.flush();
            };
            let end = chunk.len().min(self.offset + budget);
            match writer.write(&chunk[self.offset..end]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(written) => {
                    self.offset += written;
                    budget -= written;
                    if self.offset == chunk.len() {
                        self.retained_bytes -= chunk.len();
                        self.chunks.pop_front();
                        self.offset = 0;
                    }
                    if budget == 0 {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        self.retry = (!self.chunks.is_empty()).then(|| Instant::now() + RETRY_INTERVAL);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_input_is_bounded_without_discarding_accepted_bytes() {
        struct Blocked;
        impl Write for Blocked {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::WouldBlock.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut input = PtyInput::default();
        input.enqueue(&vec![b'x'; MAX_PENDING_BYTES]).unwrap();
        input.drain(&mut Blocked).unwrap();
        assert!(input.enqueue(b"overflow").is_err());
        let mut written = Vec::new();
        while input.deadline().is_some() {
            input.drain(&mut written).unwrap();
        }
        assert_eq!(written, vec![b'x'; MAX_PENDING_BYTES]);
    }
}
