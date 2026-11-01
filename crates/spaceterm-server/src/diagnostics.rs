//! Terminal Diagnostics counters: typed totals of terminal pipeline stages in this process.
//!
//! Each counter is a relaxed atomic, so a hot path pays one uncontended add. A reader takes
//! [`TerminalCounters::snapshot`] twice and divides the differences, such as bytes by reads.
//! Counters hold counts only, never terminal content or other values.
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// What woke a Pane worker from its wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaneWorkerWake {
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
    /// A scheduled deadline passed without a command.
    Deadline,
}

/// The counters of one process. [`TerminalCounters::process`] is the one that terminal code
/// records into.
#[derive(Debug, Default)]
pub struct TerminalCounters {
    accessibility_revisions: AtomicU64,
    accessibility_revision_bytes: AtomicU64,
    accessibility_rebinds: AtomicU64,
    line_mode_reads: AtomicU64,
    pty_reads: AtomicU64,
    pty_read_bytes: AtomicU64,
    frames_published: AtomicU64,
    frames_decoded: AtomicU64,
    frames_painted: AtomicU64,
    wakes: [AtomicU64; WAKE_REASONS],
}

const WAKE_REASONS: usize = 6;

static PROCESS: TerminalCounters = TerminalCounters::new();

impl TerminalCounters {
    pub const fn new() -> Self {
        Self {
            accessibility_revisions: AtomicU64::new(0),
            accessibility_revision_bytes: AtomicU64::new(0),
            accessibility_rebinds: AtomicU64::new(0),
            line_mode_reads: AtomicU64::new(0),
            pty_reads: AtomicU64::new(0),
            pty_read_bytes: AtomicU64::new(0),
            frames_published: AtomicU64::new(0),
            frames_decoded: AtomicU64::new(0),
            frames_painted: AtomicU64::new(0),
            wakes: [const { AtomicU64::new(0) }; WAKE_REASONS],
        }
    }

    /// This process's counters.
    pub fn process() -> &'static Self {
        &PROCESS
    }

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

    /// A PTY reader read `bytes` of output.
    pub fn record_pty_read(&self, bytes: usize) {
        add(&self.pty_reads, 1);
        add(&self.pty_read_bytes, self::bytes(bytes));
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

    pub fn record_pane_worker_wake(&self, wake: PaneWorkerWake) {
        add(&self.wakes[wake as usize], 1);
    }

    pub fn snapshot(&self) -> TerminalCounterSnapshot {
        let wake = |reason: PaneWorkerWake| read(&self.wakes[reason as usize]);
        TerminalCounterSnapshot {
            accessibility_revisions: read(&self.accessibility_revisions),
            accessibility_revision_bytes: read(&self.accessibility_revision_bytes),
            accessibility_rebinds: read(&self.accessibility_rebinds),
            line_mode_reads: read(&self.line_mode_reads),
            pty_reads: read(&self.pty_reads),
            pty_read_bytes: read(&self.pty_read_bytes),
            frames_published: read(&self.frames_published),
            frames_decoded: read(&self.frames_decoded),
            frames_painted: read(&self.frames_painted),
            pane_worker_wakes: PaneWorkerWakes {
                output: wake(PaneWorkerWake::Output),
                input: wake(PaneWorkerWake::Input),
                accessibility: wake(PaneWorkerWake::Accessibility),
                client: wake(PaneWorkerWake::Client),
                other: wake(PaneWorkerWake::Other),
                deadline: wake(PaneWorkerWake::Deadline),
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

/// Totals since the process started. Each counter wraps at `u64::MAX`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TerminalCounterSnapshot {
    pub accessibility_revisions: u64,
    pub accessibility_revision_bytes: u64,
    pub accessibility_rebinds: u64,
    pub line_mode_reads: u64,
    pub pty_reads: u64,
    pub pty_read_bytes: u64,
    pub frames_published: u64,
    pub frames_decoded: u64,
    pub frames_painted: u64,
    pub pane_worker_wakes: PaneWorkerWakes,
}

/// Pane worker wakes by [`PaneWorkerWake`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PaneWorkerWakes {
    pub output: u64,
    pub input: u64,
    pub accessibility: u64,
    pub client: u64,
    pub other: u64,
    pub deadline: u64,
}

impl TerminalCounterSnapshot {
    /// Each counter as a `name=value` line, in declaration order.
    pub fn lines(&self) -> String {
        let wakes = &self.pane_worker_wakes;
        [
            ("accessibility_revisions", self.accessibility_revisions),
            (
                "accessibility_revision_bytes",
                self.accessibility_revision_bytes,
            ),
            ("accessibility_rebinds", self.accessibility_rebinds),
            ("line_mode_reads", self.line_mode_reads),
            ("pty_reads", self.pty_reads),
            ("pty_read_bytes", self.pty_read_bytes),
            ("frames_published", self.frames_published),
            ("frames_decoded", self.frames_decoded),
            ("frames_painted", self.frames_painted),
            ("pane_worker_wakes.output", wakes.output),
            ("pane_worker_wakes.input", wakes.input),
            ("pane_worker_wakes.accessibility", wakes.accessibility),
            ("pane_worker_wakes.client", wakes.client),
            ("pane_worker_wakes.other", wakes.other),
            ("pane_worker_wakes.deadline", wakes.deadline),
        ]
        .into_iter()
        .map(|(name, value)| format!("{name}={value}\n"))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_record_moves_only_its_own_counters() {
        let counters = TerminalCounters::new();
        counters.record_accessibility_revision(700);
        counters.record_accessibility_revision(300);
        counters.record_accessibility_rebind();
        counters.record_line_mode_read();
        counters.record_line_mode_read();
        counters.record_pty_read(4096);
        counters.record_pty_read(1);
        counters.record_pty_read(3);
        counters.record_frame_published();
        counters.record_frame_decoded();
        counters.record_frame_decoded();
        counters.record_frame_painted();
        for wake in [
            PaneWorkerWake::Output,
            PaneWorkerWake::Output,
            PaneWorkerWake::Input,
            PaneWorkerWake::Accessibility,
            PaneWorkerWake::Client,
            PaneWorkerWake::Other,
            PaneWorkerWake::Deadline,
            PaneWorkerWake::Deadline,
            PaneWorkerWake::Deadline,
        ] {
            counters.record_pane_worker_wake(wake);
        }

        assert_eq!(
            counters.snapshot(),
            TerminalCounterSnapshot {
                accessibility_revisions: 2,
                accessibility_revision_bytes: 1000,
                accessibility_rebinds: 1,
                line_mode_reads: 2,
                pty_reads: 3,
                pty_read_bytes: 4100,
                frames_published: 1,
                frames_decoded: 2,
                frames_painted: 1,
                pane_worker_wakes: PaneWorkerWakes {
                    output: 2,
                    input: 1,
                    accessibility: 1,
                    client: 1,
                    other: 1,
                    deadline: 3,
                },
            }
        );
    }

    #[test]
    fn lines_name_every_counter_once() {
        let counters = TerminalCounters::new();
        counters.record_pty_read(9);
        counters.record_pane_worker_wake(PaneWorkerWake::Deadline);
        let lines = counters.snapshot().lines();
        assert_eq!(lines.lines().count(), 15);
        assert!(lines.contains("pty_reads=1\npty_read_bytes=9\n"));
        assert!(lines.ends_with("pane_worker_wakes.deadline=1\n"));
        let names: std::collections::BTreeSet<_> = lines
            .lines()
            .map(|line| line.split_once('=').unwrap().0)
            .collect();
        assert_eq!(names.len(), 15);
    }
}
