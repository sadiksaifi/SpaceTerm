//! The bounded queue that carries PTY output from the reader thread to the terminal worker.
//!
//! The queue holds at most a fixed number of reads. When the shell trickles output a few bytes per
//! write, the reader waits after each read until the worker has received every queued read: see
//! [`NextRead`].
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::platform::native_pty::NativePtyOutput;

/// A read shorter than this means the shell writes more slowly than the reader drains the PTY.
///
/// 1 KiB is the whole output queue of a macOS PTY and the most one macOS read returns, so a full
/// read never counts as short. A shell that writes a few bytes at a time, such as `yes | head`
/// writing one line per write, otherwise gets one read per write while the worker is busy. Those
/// reads only split output the worker cannot take yet into more, smaller reads.
const TRICKLE_READ_BYTES: usize = 1024;

/// A pause this long before a read means the shell was quiet, so the read starts a new burst.
///
/// Inside a burst the reader goes straight from one read to the next, which takes microseconds.
const QUIET_BEFORE_BURST: Duration = Duration::from_millis(1);

/// What the reader does after it queues a read and announces it to the worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NextRead {
    /// Read again at once.
    Now,
    /// The shell trickles. Read again once the worker has received every queued read, so the
    /// shell's output collects in the PTY instead of arriving a few bytes per read.
    AfterReceipt,
}

/// Creates a queue that holds at most `capacity` reads.
pub(super) fn reader_output(capacity: usize) -> (ReaderOutputSender, ReaderOutputReceiver) {
    let shared = Arc::new(Shared {
        capacity,
        state: Mutex::new(State {
            queue: VecDeque::with_capacity(capacity),
            sender_open: true,
            receiver_open: true,
            sender_waiting: false,
            receiver_waiting: false,
            burst_short_reads: 0,
            reader_resumed: Instant::now(),
        }),
        sender_wakeup: Condvar::new(),
        receiver_wakeup: Condvar::new(),
    });
    (
        ReaderOutputSender {
            shared: shared.clone(),
        },
        ReaderOutputReceiver { shared },
    )
}

/// The reader dropped its end and the worker has received every read it queued.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReaderOutputClosed;

struct Shared {
    capacity: usize,
    state: Mutex<State>,
    sender_wakeup: Condvar,
    receiver_wakeup: Condvar,
}

struct State {
    queue: VecDeque<NativePtyOutput>,
    sender_open: bool,
    receiver_open: bool,
    sender_waiting: bool,
    receiver_waiting: bool,
    /// Short reads since the shell was last quiet.
    burst_short_reads: usize,
    /// When the reader last went back to reading.
    reader_resumed: Instant,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Waits on the sender's wakeup until `ready` holds, or returns None once the worker's end is
    /// gone.
    fn wait_for_sender(&self, ready: impl Fn(&State) -> bool) -> Option<MutexGuard<'_, State>> {
        let mut state = self.lock();
        while state.receiver_open && !ready(&state) {
            state.sender_waiting = true;
            state = self
                .sender_wakeup
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        state.sender_waiting = false;
        state.receiver_open.then_some(state)
    }
}

/// The reader's end. It waits while the queue is full, and returns false once the worker's end is
/// gone.
pub(super) struct ReaderOutputSender {
    shared: Arc<Shared>,
}

impl ReaderOutputSender {
    /// Queues one read, waiting while the queue is full, and returns None once the worker's end is
    /// gone.
    ///
    /// A burst of short reads after the shell was quiet, such as a prompt or an echo, keeps a
    /// queue's worth of read-ahead. The queue's capacity is also the worker's batch limit, so the
    /// worker's first batch after a busy period holds the whole burst, as it would without a wait.
    /// Short reads beyond that are a sustained trickle and wait for receipt.
    pub(super) fn send(&self, output: NativePtyOutput) -> Option<NextRead> {
        let now = Instant::now();
        let short =
            matches!(&output, NativePtyOutput::Bytes(bytes) if bytes.len() < TRICKLE_READ_BYTES);
        let capacity = self.shared.capacity;
        let mut state = self
            .shared
            .wait_for_sender(|state| state.queue.len() < capacity)?;
        state.queue.push_back(output);
        if state.receiver_waiting {
            self.shared.receiver_wakeup.notify_one();
        }
        if !short {
            state.reader_resumed = now;
            return Some(NextRead::Now);
        }
        if now.saturating_duration_since(state.reader_resumed) >= QUIET_BEFORE_BURST {
            state.burst_short_reads = 0;
        }
        state.burst_short_reads = state.burst_short_reads.saturating_add(1);
        state.reader_resumed = now;
        Some(if state.burst_short_reads > capacity {
            NextRead::AfterReceipt
        } else {
            NextRead::Now
        })
    }

    /// Waits until the worker has received every queued read, and returns false once the worker's
    /// end is gone.
    pub(super) fn wait_until_received(&self) -> bool {
        let Some(mut state) = self.shared.wait_for_sender(|state| state.queue.is_empty()) else {
            return false;
        };
        state.reader_resumed = Instant::now();
        true
    }
}

impl Drop for ReaderOutputSender {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.sender_open = false;
        if state.receiver_waiting {
            self.shared.receiver_wakeup.notify_one();
        }
    }
}

/// The worker's end.
pub(super) struct ReaderOutputReceiver {
    shared: Arc<Shared>,
}

impl ReaderOutputReceiver {
    /// Receives the oldest read, waiting for one while the reader's end is open.
    pub(super) fn recv(&self) -> Result<NativePtyOutput, ReaderOutputClosed> {
        let mut state = self.shared.lock();
        loop {
            if let Some(output) = state.queue.pop_front() {
                state.receiver_waiting = false;
                if state.sender_waiting {
                    self.shared.sender_wakeup.notify_one();
                }
                return Ok(output);
            }
            if !state.sender_open {
                state.receiver_waiting = false;
                return Err(ReaderOutputClosed);
            }
            state.receiver_waiting = true;
            state = self
                .shared
                .receiver_wakeup
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }
}

impl Drop for ReaderOutputReceiver {
    fn drop(&mut self) {
        let queued = {
            let mut state = self.shared.lock();
            state.receiver_open = false;
            if state.sender_waiting {
                self.shared.sender_wakeup.notify_one();
            }
            std::mem::take(&mut state.queue)
        };
        drop(queued);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    fn bytes(value: u8) -> NativePtyOutput {
        NativePtyOutput::Bytes(vec![value])
    }

    fn received_byte(receiver: &ReaderOutputReceiver) -> u8 {
        match receiver.recv() {
            Ok(NativePtyOutput::Bytes(bytes)) => bytes[0],
            _ => panic!("the queue should deliver the read"),
        }
    }

    #[test]
    fn reads_arrive_in_order_and_close_after_the_reader_ends() {
        let (sender, receiver) = reader_output(2);
        assert!(sender.send(bytes(1)).is_some());
        assert!(sender.send(bytes(2)).is_some());
        drop(sender);

        assert_eq!(received_byte(&receiver), 1);
        assert_eq!(received_byte(&receiver), 2);
        assert_eq!(receiver.recv().err(), Some(ReaderOutputClosed));
    }

    #[test]
    fn a_full_queue_holds_the_reader_until_the_worker_receives() {
        let (sender, receiver) = reader_output(1);
        let (sent, sends) = mpsc::channel();
        let reader = thread::spawn(move || {
            for value in 0..2 {
                sent.send(sender.send(bytes(value)).is_some()).unwrap();
            }
        });
        assert!(sends.recv().unwrap());
        assert!(sends.recv_timeout(Duration::from_millis(50)).is_err());

        assert_eq!(received_byte(&receiver), 0);
        assert!(sends.recv().unwrap());
        assert_eq!(received_byte(&receiver), 1);
        reader.join().unwrap();
    }

    #[test]
    fn waiting_for_receipt_ends_when_the_worker_receives_every_read() {
        let (sender, receiver) = reader_output(4);
        assert!(sender.send(bytes(0)).is_some());
        assert!(sender.send(bytes(1)).is_some());
        let (waited, waits) = mpsc::channel();
        let reader = thread::spawn(move || waited.send(sender.wait_until_received()).unwrap());

        assert_eq!(received_byte(&receiver), 0);
        assert!(waits.recv_timeout(Duration::from_millis(50)).is_err());
        assert_eq!(received_byte(&receiver), 1);
        assert!(waits.recv().unwrap());
        reader.join().unwrap();
    }

    #[test]
    fn dropping_the_workers_end_releases_a_waiting_reader() {
        for full in [true, false] {
            let (sender, receiver) = reader_output(1);
            assert!(sender.send(bytes(0)).is_some());
            let reader = thread::spawn(move || {
                if full {
                    sender.send(bytes(1)).is_some()
                } else {
                    sender.wait_until_received()
                }
            });
            thread::sleep(Duration::from_millis(20));
            drop(receiver);
            assert!(!reader.join().unwrap());
        }
    }
}
