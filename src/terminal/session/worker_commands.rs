//! The Pane worker's command queue, and the one place the worker blocks for a command.
//!
//! Terminal Diagnostics count each command the worker takes from the queue without blocking, and
//! each blocking wait by what ended it. A blocking wait is one receive call that found the queue
//! empty and slept; wakeups inside the channel's own implementation are not visible here.
use super::Command;
use spaceterm_server::diagnostics::{PaneWorkerWaitEnd, TerminalCounters};
use std::sync::Arc;
use std::sync::mpsc::{self, TryRecvError};
use std::time::Duration;

pub(super) struct WorkerCommands {
    pub(super) receiver: mpsc::Receiver<Command>,
    counters: Arc<TerminalCounters>,
}

/// How [`WorkerCommands::wait`] ended.
pub(super) enum Waited {
    Command(Command),
    /// The timeout passed, or was already zero, without a command.
    Deadline,
    /// Every sender is gone and the queue is empty.
    Disconnected,
}

impl WorkerCommands {
    pub(super) fn new(receiver: mpsc::Receiver<Command>, counters: Arc<TerminalCounters>) -> Self {
        Self { receiver, counters }
    }

    /// Takes an already queued command without blocking.
    pub(super) fn try_recv(&self) -> Result<Command, TryRecvError> {
        let command = self.receiver.try_recv()?;
        self.counters
            .record_pane_worker_command_without_wait(command.kind());
        Ok(command)
    }

    /// Takes a queued command, or blocks until a command arrives, `timeout` passes, or every
    /// sender is gone. `None` waits without a deadline. A zero timeout never blocks.
    pub(super) fn wait(&self, timeout: Option<Duration>) -> Waited {
        match self.try_recv() {
            Ok(command) => return Waited::Command(command),
            Err(TryRecvError::Disconnected) => return Waited::Disconnected,
            Err(TryRecvError::Empty) => {}
        }
        let waited = match timeout {
            None => self
                .receiver
                .recv()
                .map_or(Waited::Disconnected, Waited::Command),
            Some(timeout) if timeout.is_zero() => return Waited::Deadline,
            Some(timeout) => match self.receiver.recv_timeout(timeout) {
                Ok(command) => Waited::Command(command),
                Err(mpsc::RecvTimeoutError::Timeout) => Waited::Deadline,
                Err(mpsc::RecvTimeoutError::Disconnected) => Waited::Disconnected,
            },
        };
        self.counters.record_pane_worker_wait(match &waited {
            Waited::Command(command) => PaneWorkerWaitEnd::Command(command.kind()),
            Waited::Deadline => PaneWorkerWaitEnd::Deadline,
            Waited::Disconnected => PaneWorkerWaitEnd::Disconnected,
        });
        waited
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_server::diagnostics::{
        PaneWorkerCommands, PaneWorkerWaits, TerminalCounterSnapshot,
    };
    use std::thread;

    fn queue() -> (mpsc::Sender<Command>, WorkerCommands, Arc<TerminalCounters>) {
        let (sender, receiver) = mpsc::channel();
        let counters = Arc::new(TerminalCounters::default());
        (
            sender,
            WorkerCommands::new(receiver, Arc::clone(&counters)),
            counters,
        )
    }

    /// Sends `command` after the receiver has had time to block on an empty queue.
    fn send_later(sender: &mpsc::Sender<Command>, command: Command) -> thread::JoinHandle<()> {
        let sender = sender.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            sender.send(command).unwrap();
        })
    }

    #[test]
    fn queued_commands_count_by_kind_without_a_wait() {
        let (sender, commands, counters) = queue();
        for command in [
            Command::FlushInput,
            Command::FlushInput,
            Command::Resize,
            Command::AccessibilityDemand,
            Command::ReaderReady(Some(1)),
            Command::PublishMetadata,
        ] {
            sender.send(command).unwrap();
        }

        assert!(matches!(commands.try_recv(), Ok(Command::FlushInput)));
        for _ in 0..5 {
            assert!(matches!(commands.wait(None), Waited::Command(_)));
        }
        assert!(matches!(
            commands.wait(Some(Duration::ZERO)),
            Waited::Deadline
        ));
        drop(sender);
        assert!(matches!(commands.wait(None), Waited::Disconnected));

        assert_eq!(
            counters.snapshot(),
            TerminalCounterSnapshot {
                pane_worker_commands_without_wait: PaneWorkerCommands {
                    output: 1,
                    input: 2,
                    accessibility: 1,
                    client: 1,
                    other: 1,
                },
                ..Default::default()
            }
        );
    }

    #[test]
    fn each_blocking_wait_counts_once_by_what_ended_it() {
        let (sender, commands, counters) = queue();

        let input = send_later(&sender, Command::FlushInput);
        assert!(matches!(
            commands.wait(None),
            Waited::Command(Command::FlushInput)
        ));
        input.join().unwrap();
        let client = send_later(&sender, Command::Resize);
        assert!(matches!(
            commands.wait(Some(Duration::from_secs(30))),
            Waited::Command(Command::Resize)
        ));
        client.join().unwrap();
        assert!(matches!(
            commands.wait(Some(Duration::from_millis(1))),
            Waited::Deadline
        ));
        let closing = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            drop(sender);
        });
        assert!(matches!(commands.wait(None), Waited::Disconnected));
        closing.join().unwrap();

        assert_eq!(
            counters.snapshot(),
            TerminalCounterSnapshot {
                pane_worker_waits: PaneWorkerWaits {
                    input: 1,
                    client: 1,
                    deadline: 1,
                    disconnected: 1,
                    ..Default::default()
                },
                ..Default::default()
            }
        );
    }
}
