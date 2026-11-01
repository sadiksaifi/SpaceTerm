//! The bounded queue that carries PTY output from the reader thread to the terminal worker.
//!
//! The queue holds at most a fixed number of reads. Its sender can also wait until the worker has
//! received every queued read, which the reader does after a short read: see
//! [`TRICKLE_READ_BYTES`].
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::platform::native_pty::NativePtyOutput;

/// A read shorter than this means the shell writes more slowly than the reader drains the PTY.
///
/// 1 KiB is the whole output queue of a macOS PTY and the most one macOS read returns, so a full
/// read never counts as short. A shell that writes a few bytes at a time, such as `yes | head`
/// writing one line per write, otherwise gets one read per write while the worker is busy. Those
/// reads only split output the worker cannot take yet into more, smaller reads.
pub(super) const TRICKLE_READ_BYTES: usize = 1024;

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
    /// Queues one read, waiting while the queue is full.
    pub(super) fn send(&self, output: NativePtyOutput) -> bool {
        let capacity = self.shared.capacity;
        let Some(mut state) = self
            .shared
            .wait_for_sender(|state| state.queue.len() < capacity)
        else {
            return false;
        };
        state.queue.push_back(output);
        if state.receiver_waiting {
            self.shared.receiver_wakeup.notify_one();
        }
        true
    }

    /// Waits until the worker has received every queued read.
    pub(super) fn wait_until_received(&self) -> bool {
        self.shared
            .wait_for_sender(|state| state.queue.is_empty())
            .is_some()
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
        assert!(sender.send(bytes(1)));
        assert!(sender.send(bytes(2)));
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
                sent.send(sender.send(bytes(value))).unwrap();
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
        assert!(sender.send(bytes(0)));
        assert!(sender.send(bytes(1)));
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
            assert!(sender.send(bytes(0)));
            let reader = thread::spawn(move || {
                if full {
                    sender.send(bytes(1))
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
