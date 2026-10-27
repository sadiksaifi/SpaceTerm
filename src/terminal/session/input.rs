//! Keeps PTY input ordered and bounded while the child applies backpressure, and reports the
//! highest client Input serial whose bytes the PTY has accepted.
use std::collections::VecDeque;
use std::io::{self, Write};
use std::time::{Duration, Instant};

const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;
const WRITE_BUDGET: usize = 64 * 1024;
const RETRY_INTERVAL: Duration = Duration::from_millis(10);
/// Serials waiting for their bytes to be written. A newer serial replaces the newest waiting one
/// beyond this bound, so the reported serial is late but never ahead of the PTY.
const MAX_PENDING_SERIALS: usize = 256;

#[derive(Default)]
pub(super) struct PtyInput {
    chunks: VecDeque<Vec<u8>>,
    offset: usize,
    retained_bytes: usize,
    retry: Option<Instant>,
    /// Bytes ever enqueued and ever written, to place serials in the byte stream.
    enqueued: u64,
    written: u64,
    /// Each pending serial and the byte count that must be written before it applies.
    serials: VecDeque<(u64, u32)>,
    serial: u32,
}

impl PtyInput {
    /// Notes that every byte of client Input `serial` has been enqueued. The serial is written
    /// once those bytes reach the PTY.
    pub(super) fn note_serial(&mut self, serial: u32) {
        if self.written == self.enqueued {
            self.serial = self.serial.max(serial);
        } else if self.serials.len() < MAX_PENDING_SERIALS {
            self.serials.push_back((self.enqueued, serial));
        } else if let Some(newest) = self.serials.back_mut() {
            *newest = (self.enqueued, serial);
        }
    }

    /// A new client claim numbers its Input from 1 again. Bytes still queued are written, but no
    /// earlier serial is reported for them.
    pub(super) fn restart_serials(&mut self) {
        self.serials.clear();
        self.serial = 0;
    }

    /// The highest client Input serial whose bytes the PTY has accepted. Zero before any.
    pub(super) fn written_serial(&self) -> u32 {
        self.serial
    }

    fn advance_serials(&mut self) {
        while let Some(&(end, serial)) = self.serials.front()
            && end <= self.written
        {
            self.serial = self.serial.max(serial);
            self.serials.pop_front();
        }
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.retry
    }

    pub(super) fn enqueue(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > MAX_PENDING_BYTES.saturating_sub(self.retained_bytes) {
            return Err(io::Error::other("PTY input queue is full"));
        }
        if !bytes.is_empty() {
            self.retained_bytes += bytes.len();
            self.enqueued += bytes.len() as u64;
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
                self.advance_serials();
                return writer.flush();
            };
            let end = chunk.len().min(self.offset + budget);
            match writer.write(&chunk[self.offset..end]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(written) => {
                    self.offset += written;
                    self.written += written as u64;
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
        self.advance_serials();
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

    /// Accepts at most `budget` bytes per call, then blocks.
    struct Slow {
        written: Vec<u8>,
        budget: usize,
    }
    impl Write for Slow {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.budget == 0 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let count = bytes.len().min(self.budget);
            self.budget -= count;
            self.written.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_serial_is_written_only_after_all_of_its_bytes() {
        let mut input = PtyInput::default();
        let mut pty = Slow {
            written: Vec::new(),
            budget: 3,
        };
        input.enqueue(b"ab").unwrap();
        input.note_serial(1);
        input.enqueue(b"cd").unwrap();
        input.note_serial(2);

        input.drain(&mut pty).unwrap();
        assert_eq!(pty.written, b"abc");
        assert_eq!(input.written_serial(), 1);

        pty.budget = 1;
        input.drain(&mut pty).unwrap();
        assert_eq!(input.written_serial(), 2);
    }

    #[test]
    fn a_serial_without_bytes_applies_once_earlier_bytes_are_written() {
        let mut input = PtyInput::default();
        input.note_serial(1);
        assert_eq!(input.written_serial(), 1);

        let mut pty = Slow {
            written: Vec::new(),
            budget: 0,
        };
        input.enqueue(b"x").unwrap();
        input.drain(&mut pty).unwrap();
        input.note_serial(2);
        assert_eq!(input.written_serial(), 1);

        pty.budget = 1;
        input.drain(&mut pty).unwrap();
        assert_eq!(input.written_serial(), 2);
    }

    #[test]
    fn many_waiting_serials_report_late_but_never_early() {
        let mut input = PtyInput::default();
        let mut pty = Slow {
            written: Vec::new(),
            budget: 0,
        };
        for serial in 1..=(MAX_PENDING_SERIALS as u32 + 10) {
            input.enqueue(b"x").unwrap();
            input.note_serial(serial);
        }
        input.drain(&mut pty).unwrap();

        pty.budget = MAX_PENDING_SERIALS - 1;
        while pty.budget > 0 {
            input.drain(&mut pty).unwrap();
        }
        assert_eq!(input.written_serial(), MAX_PENDING_SERIALS as u32 - 1);

        pty.budget = 64;
        while input.deadline().is_some() {
            input.drain(&mut pty).unwrap();
        }
        assert_eq!(input.written_serial(), MAX_PENDING_SERIALS as u32 + 10);
    }

    /// A new claim numbers its Input from 1 again. Its serials are reported only once its own
    /// bytes reach the PTY, never because an earlier claim reached a higher serial.
    #[test]
    fn serials_restart_with_each_claim_without_running_ahead_of_the_pty() {
        let mut input = PtyInput::default();
        let mut pty = Slow {
            written: Vec::new(),
            budget: b"first claim".len(),
        };
        input.enqueue(b"first claim").unwrap();
        input.note_serial(40);
        input.enqueue(b"pending").unwrap();
        input.note_serial(41);
        input.drain(&mut pty).unwrap();
        assert_eq!(input.written_serial(), 40);

        input.restart_serials();
        assert_eq!(input.written_serial(), 0, "no serial of the earlier claim");
        input.enqueue(b"x").unwrap();
        input.note_serial(1);
        assert_eq!(
            input.written_serial(),
            0,
            "the new bytes are not written yet"
        );
        pty.budget = usize::MAX;
        input.drain(&mut pty).unwrap();
        assert_eq!(input.written_serial(), 1);
        assert_eq!(pty.written, b"first claimpendingx");
    }
}
