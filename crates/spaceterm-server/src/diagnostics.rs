//! Terminal Diagnostics counters: typed totals of terminal pipeline stages.
//!
//! Application composition makes one [`TerminalCounters`] for a server and one for a desktop,
//! and hands it to each module that records into it. Each counter is a relaxed atomic add. A
//! reader takes [`TerminalCounters::snapshot`] twice and divides the differences, such as bytes
//! by reads. Counters hold counts only, never terminal content or other values.
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// The kind of command a Pane worker took from its queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaneWorkerCommand {
    /// The PTY reader delivered output.
    Output,
    /// Client Input, a paste, a pointer report or a command run.
    Input,
    /// Accessibility demand, selection or publication.
    Accessibility,
    /// A client view change, such as focus, size, visibility or a subscription.
    Client,
    /// Another command, such as a graphics or clipboard completion.
    Other,
}

/// What ended a Pane worker's blocking wait for a command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaneWorkerWaitEnd {
    /// A command of this kind arrived.
    Command(PaneWorkerCommand),
    /// The worker's next deadline passed first.
    Deadline,
    /// Every sender of the worker's queue was dropped.
    Disconnected,
}

const COMMAND_KINDS: usize = 5;
const WAIT_ENDS: usize = COMMAND_KINDS + 2;

impl PaneWorkerWaitEnd {
    const fn index(self) -> usize {
        match self {
            Self::Command(kind) => kind as usize,
            Self::Deadline => COMMAND_KINDS,
            Self::Disconnected => COMMAND_KINDS + 1,
        }
    }
}

/// The counters of one server or desktop.
#[derive(Debug, Default)]
pub struct TerminalCounters {
    accessibility_revisions: AtomicU64,
    accessibility_revision_bytes: AtomicU64,
    accessibility_rebinds: AtomicU64,
    line_mode_reads: AtomicU64,
    pty_read_attempts: AtomicU64,
    pty_data_reads: AtomicU64,
    pty_read_bytes: AtomicU64,
    frames_published: AtomicU64,
    frames_decoded: AtomicU64,
    frames_painted: AtomicU64,
    pane_worker_waits: [AtomicU64; WAIT_ENDS],
    pane_worker_commands_without_wait: [AtomicU64; COMMAND_KINDS],
}

impl TerminalCounters {
    /// The server serialized one accessibility revision of `bytes` for a client.
    pub fn record_accessibility_revision(&self, bytes: usize) {
        add(&self.accessibility_revisions, 1);
        add(&self.accessibility_revision_bytes, self::bytes(bytes));
    }

    /// The client bound its retained accessibility document to a newer Screen.
    pub fn record_accessibility_rebind(&self) {
        add(&self.accessibility_rebinds, 1);
    }

    /// The server read a PTY's Line Mode.
    pub fn record_line_mode_read(&self) {
        add(&self.line_mode_reads, 1);
    }

    /// One native read of a PTY returned `result`. Every return is an attempt, including would
    /// block, interruption, end of file and failure. A successful read of at least one byte is
    /// also a data read.
    pub fn record_pty_read(&self, result: &std::io::Result<usize>) {
        add(&self.pty_read_attempts, 1);
        if let Ok(read @ 1..) = result {
            add(&self.pty_data_reads, 1);
            add(&self.pty_read_bytes, self::bytes(*read));
        }
    }

    /// The server encoded one screen frame for a client.
    pub fn record_frame_published(&self) {
        add(&self.frames_published, 1);
    }

    /// The client decoded one screen frame.
    pub fn record_frame_decoded(&self) {
        add(&self.frames_decoded, 1);
    }

    /// The client painted one terminal grid.
    pub fn record_frame_painted(&self) {
        add(&self.frames_painted, 1);
    }

    /// A Pane worker's blocking wait for a command ended.
    pub fn record_pane_worker_wait(&self, end: PaneWorkerWaitEnd) {
        add(&self.pane_worker_waits[end.index()], 1);
    }

    /// A Pane worker took an already queued command without blocking.
    pub fn record_pane_worker_command_without_wait(&self, kind: PaneWorkerCommand) {
        add(&self.pane_worker_commands_without_wait[kind as usize], 1);
    }

    pub fn snapshot(&self) -> TerminalCounterSnapshot {
        let wait = |end: PaneWorkerWaitEnd| read(&self.pane_worker_waits[end.index()]);
        let waited = |kind| wait(PaneWorkerWaitEnd::Command(kind));
        let queued =
            |kind: PaneWorkerCommand| read(&self.pane_worker_commands_without_wait[kind as usize]);
        TerminalCounterSnapshot {
            accessibility_revisions: read(&self.accessibility_revisions),
            accessibility_revision_bytes: read(&self.accessibility_revision_bytes),
            accessibility_rebinds: read(&self.accessibility_rebinds),
            line_mode_reads: read(&self.line_mode_reads),
            pty_read_attempts: read(&self.pty_read_attempts),
            pty_data_reads: read(&self.pty_data_reads),
            pty_read_bytes: read(&self.pty_read_bytes),
            frames_published: read(&self.frames_published),
            frames_decoded: read(&self.frames_decoded),
            frames_painted: read(&self.frames_painted),
            pane_worker_waits: PaneWorkerWaits {
                output: waited(PaneWorkerCommand::Output),
                input: waited(PaneWorkerCommand::Input),
                accessibility: waited(PaneWorkerCommand::Accessibility),
                client: waited(PaneWorkerCommand::Client),
                other: waited(PaneWorkerCommand::Other),
                deadline: wait(PaneWorkerWaitEnd::Deadline),
                disconnected: wait(PaneWorkerWaitEnd::Disconnected),
            },
            pane_worker_commands_without_wait: PaneWorkerCommands {
                output: queued(PaneWorkerCommand::Output),
                input: queued(PaneWorkerCommand::Input),
                accessibility: queued(PaneWorkerCommand::Accessibility),
                client: queued(PaneWorkerCommand::Client),
                other: queued(PaneWorkerCommand::Other),
            },
        }
    }
}

fn add(counter: &AtomicU64, amount: u64) {
    counter.fetch_add(amount, Ordering::Relaxed);
}

fn bytes(count: usize) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}

fn read(counter: &AtomicU64) -> u64 {
    counter.load(Ordering::Relaxed)
}

/// Totals since the counters were made. Each counter wraps at `u64::MAX`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalCounterSnapshot {
    pub accessibility_revisions: u64,
    pub accessibility_revision_bytes: u64,
    pub accessibility_rebinds: u64,
    pub line_mode_reads: u64,
    /// Native PTY reads, including those that would block, were interrupted, or ended output.
    pub pty_read_attempts: u64,
    /// Native PTY reads that returned at least one byte.
    pub pty_data_reads: u64,
    pub pty_read_bytes: u64,
    pub frames_published: u64,
    pub frames_decoded: u64,
    pub frames_painted: u64,
    pub pane_worker_waits: PaneWorkerWaits,
    pub pane_worker_commands_without_wait: PaneWorkerCommands,
}

/// Pane worker blocking waits for a command, by what ended each: a command of one kind, the
/// next deadline, or a closed queue.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneWorkerWaits {
    pub output: u64,
    pub input: u64,
    pub accessibility: u64,
    pub client: u64,
    pub other: u64,
    pub deadline: u64,
    pub disconnected: u64,
}

/// Commands a Pane worker took from its queue without blocking, by [`PaneWorkerCommand`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneWorkerCommands {
    pub output: u64,
    pub input: u64,
    pub accessibility: u64,
    pub client: u64,
    pub other: u64,
}

impl TerminalCounterSnapshot {
    /// Each counter as a `name=value` line, in declaration order.
    pub fn lines(&self) -> String {
        let waits = &self.pane_worker_waits;
        let queued = &self.pane_worker_commands_without_wait;
        [
            ("accessibility_revisions", self.accessibility_revisions),
            (
                "accessibility_revision_bytes",
                self.accessibility_revision_bytes,
            ),
            ("accessibility_rebinds", self.accessibility_rebinds),
            ("line_mode_reads", self.line_mode_reads),
            ("pty_read_attempts", self.pty_read_attempts),
            ("pty_data_reads", self.pty_data_reads),
            ("pty_read_bytes", self.pty_read_bytes),
            ("frames_published", self.frames_published),
            ("frames_decoded", self.frames_decoded),
            ("frames_painted", self.frames_painted),
            ("pane_worker_waits.output", waits.output),
            ("pane_worker_waits.input", waits.input),
            ("pane_worker_waits.accessibility", waits.accessibility),
            ("pane_worker_waits.client", waits.client),
            ("pane_worker_waits.other", waits.other),
            ("pane_worker_waits.deadline", waits.deadline),
            ("pane_worker_waits.disconnected", waits.disconnected),
            ("pane_worker_commands_without_wait.output", queued.output),
            ("pane_worker_commands_without_wait.input", queued.input),
            (
                "pane_worker_commands_without_wait.accessibility",
                queued.accessibility,
            ),
            ("pane_worker_commands_without_wait.client", queued.client),
            ("pane_worker_commands_without_wait.other", queued.other),
        ]
        .into_iter()
        .map(|(name, value)| format!("{name}={value}\n"))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn each_record_moves_only_its_own_counters() {
        let counters = TerminalCounters::default();
        counters.record_accessibility_revision(700);
        counters.record_accessibility_revision(300);
        counters.record_accessibility_rebind();
        counters.record_line_mode_read();
        counters.record_line_mode_read();
        for result in [
            Ok(4096),
            Err(io::ErrorKind::WouldBlock.into()),
            Ok(1),
            Err(io::ErrorKind::Interrupted.into()),
            Ok(3),
            Ok(0),
        ] {
            counters.record_pty_read(&result);
        }
        counters.record_frame_published();
        counters.record_frame_decoded();
        counters.record_frame_decoded();
        counters.record_frame_painted();
        for end in [
            PaneWorkerWaitEnd::Command(PaneWorkerCommand::Output),
            PaneWorkerWaitEnd::Command(PaneWorkerCommand::Output),
            PaneWorkerWaitEnd::Command(PaneWorkerCommand::Input),
            PaneWorkerWaitEnd::Command(PaneWorkerCommand::Accessibility),
            PaneWorkerWaitEnd::Command(PaneWorkerCommand::Client),
            PaneWorkerWaitEnd::Command(PaneWorkerCommand::Other),
            PaneWorkerWaitEnd::Deadline,
            PaneWorkerWaitEnd::Deadline,
            PaneWorkerWaitEnd::Deadline,
            PaneWorkerWaitEnd::Disconnected,
        ] {
            counters.record_pane_worker_wait(end);
        }
        for kind in [
            PaneWorkerCommand::Input,
            PaneWorkerCommand::Input,
            PaneWorkerCommand::Client,
            PaneWorkerCommand::Other,
        ] {
            counters.record_pane_worker_command_without_wait(kind);
        }

        assert_eq!(
            counters.snapshot(),
            TerminalCounterSnapshot {
                accessibility_revisions: 2,
                accessibility_revision_bytes: 1000,
                accessibility_rebinds: 1,
                line_mode_reads: 2,
                pty_read_attempts: 6,
                pty_data_reads: 3,
                pty_read_bytes: 4100,
                frames_published: 1,
                frames_decoded: 2,
                frames_painted: 1,
                pane_worker_waits: PaneWorkerWaits {
                    output: 2,
                    input: 1,
                    accessibility: 1,
                    client: 1,
                    other: 1,
                    deadline: 3,
                    disconnected: 1,
                },
                pane_worker_commands_without_wait: PaneWorkerCommands {
                    output: 0,
                    input: 2,
                    accessibility: 0,
                    client: 1,
                    other: 1,
                },
            }
        );
    }

    #[test]
    fn lines_name_every_counter_with_its_value() {
        let snapshot = TerminalCounterSnapshot {
            accessibility_revisions: 1,
            accessibility_revision_bytes: 2,
            accessibility_rebinds: 3,
            line_mode_reads: 4,
            pty_read_attempts: 5,
            pty_data_reads: 6,
            pty_read_bytes: 7,
            frames_published: 8,
            frames_decoded: 9,
            frames_painted: 10,
            pane_worker_waits: PaneWorkerWaits {
                output: 11,
                input: 12,
                accessibility: 13,
                client: 14,
                other: 15,
                deadline: 16,
                disconnected: 17,
            },
            pane_worker_commands_without_wait: PaneWorkerCommands {
                output: 18,
                input: 19,
                accessibility: 20,
                client: 21,
                other: 22,
            },
        };
        assert_eq!(
            snapshot.lines(),
            "accessibility_revisions=1\n\
             accessibility_revision_bytes=2\n\
             accessibility_rebinds=3\n\
             line_mode_reads=4\n\
             pty_read_attempts=5\n\
             pty_data_reads=6\n\
             pty_read_bytes=7\n\
             frames_published=8\n\
             frames_decoded=9\n\
             frames_painted=10\n\
             pane_worker_waits.output=11\n\
             pane_worker_waits.input=12\n\
             pane_worker_waits.accessibility=13\n\
             pane_worker_waits.client=14\n\
             pane_worker_waits.other=15\n\
             pane_worker_waits.deadline=16\n\
             pane_worker_waits.disconnected=17\n\
             pane_worker_commands_without_wait.output=18\n\
             pane_worker_commands_without_wait.input=19\n\
             pane_worker_commands_without_wait.accessibility=20\n\
             pane_worker_commands_without_wait.client=21\n\
             pane_worker_commands_without_wait.other=22\n"
        );
    }
}
