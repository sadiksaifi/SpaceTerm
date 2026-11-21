//! Terminal Diagnostics counters carried on the service lane and Desktop Channel.
use serde::{Deserialize, Serialize};

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

/// Commands a Pane worker took from its queue without blocking, by kind.
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
