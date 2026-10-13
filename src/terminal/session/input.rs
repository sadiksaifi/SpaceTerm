//! Keeps PTY input ordered and bounded while the child applies backpressure.
use spaceterm_terminal::runtime::budget::BudgetLease;
use spaceterm_terminal::runtime::legacy::{LegacyInputBudget, LegacyInputReservation};
use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type InputLease = Arc<Mutex<BudgetLease>>;

const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;
const WRITE_BUDGET: usize = 64 * 1024;
const RETRY_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Default)]
pub(super) struct PtyInput {
    chunks: VecDeque<(Vec<u8>, Option<InputLease>)>,
    budget: Option<LegacyInputBudget>,
    command: Option<LegacyInputReservation>,
    offset: usize,
    retained_bytes: usize,
    retry: Option<Instant>,
}

impl PtyInput {
    pub(super) fn with_budget(budget: LegacyInputBudget) -> Self {
        Self {
            budget: Some(budget),
            ..Self::default()
        }
    }
    pub(super) fn begin_command(&mut self, reservation: Option<LegacyInputReservation>) {
        self.command = reservation;
    }
    pub(super) fn take_command_reservation(&mut self) -> Option<LegacyInputReservation> {
        self.command.take()
    }
    pub(super) fn end_command(&mut self) {
        self.command = None;
    }
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.retry
    }

    pub(super) fn enqueue(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.enqueue_with_origin(bytes, false)
    }
    pub(super) fn enqueue_reply(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.enqueue_with_origin(bytes, true)
    }
    fn enqueue_with_origin(&mut self, bytes: &[u8], reply: bool) -> io::Result<()> {
        if bytes.len() > MAX_PENDING_BYTES.saturating_sub(self.retained_bytes) {
            return Err(io::Error::other("PTY input queue is full"));
        }
        if !bytes.is_empty() {
            let lease = if !reply && let Some(reservation) = &mut self.command {
                Some(
                    reservation
                        .retain(bytes.len())
                        .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?,
                )
            } else {
                self.budget
                    .as_ref()
                    .map(|budget| {
                        if reply {
                            budget.reserve_reply(bytes.len())
                        } else {
                            budget.reserve(bytes.len())
                        }
                    })
                    .transpose()
                    .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?
                    .map(|lease| Arc::new(Mutex::new(lease)))
            };
            self.retained_bytes += bytes.len();
            self.chunks.push_back((bytes.to_vec(), lease));
        }
        Ok(())
    }

    /// The writer must return WouldBlock instead of waiting for child input consumption.
    pub(super) fn drain(&mut self, writer: &mut impl Write) -> io::Result<()> {
        let mut budget = WRITE_BUDGET;
        for _ in 0..16 {
            let Some((chunk, _lease)) = self.chunks.front() else {
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
