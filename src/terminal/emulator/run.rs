//! `pane run` admission at the owning shell's prompt, and the markers a pending run follows.
//!
//! The terminal worker calls [`TerminalEmulator::admit_run`] between two PTY reads, so no marker
//! and no input can arrive between its checks and the write it returns.
use super::*;
use crate::terminal::metadata::PromptZone;
use spaceterm_server::pane::run::Observation;
use spaceterm_server::protocol::{HistoryPosition, RunRefusal};

/// A run the owning shell's prompt accepted. The worker writes `bytes` as one write.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AdmittedRun {
    pub(crate) bytes: Vec<u8>,
    /// The row the prompt is on, where the run's output starts.
    pub(crate) start: HistoryPosition,
    /// The Terminal Session's finished-command count before the run.
    pub(crate) finished_commands: u64,
}

impl TerminalEmulator {
    /// Counts input written to the PTY since the owning shell's last fresh prompt.
    pub(crate) fn note_input(&mut self, bytes: usize) {
        self.metadata.note_input(bytes);
    }

    /// Accepts `command` only at an empty prompt of the owning shell with bracketed paste on,
    /// and returns it as one bracketed paste followed by Return. From then on the markers the
    /// run follows are kept for [`Self::take_run_observations`].
    pub(crate) fn admit_run(&mut self, command: &str) -> Result<AdmittedRun, RunRefusal> {
        let prompt = self.metadata.owned_prompt();
        match prompt.zone {
            None => return Err(RunRefusal::NoIntegration),
            Some(PromptZone::Prompt | PromptZone::CommandInput) => {}
            Some(_) => return Err(RunRefusal::NotAtPrompt),
        }
        if prompt.input_bytes != 0 {
            return Err(RunRefusal::InputPending);
        }
        if !self.bracketed_paste_mode().unwrap_or(false) {
            return Err(RunRefusal::NoBracketedPaste);
        }
        let start = self
            .cursor_history_position()
            .map_err(|_| RunRefusal::NotAtPrompt)?;
        let mut bytes = Vec::with_capacity(command.len() + 13);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(command.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~\r");
        self.run_observations.clear();
        self.metadata.follow_run();
        self.terminal.scroll_viewport(ScrollViewport::Bottom);
        Ok(AdmittedRun {
            bytes,
            start,
            finished_commands: self.metadata.snapshot().finished_commands,
        })
    }

    /// The markers the pending run followed since the last call, oldest first.
    pub(crate) fn take_run_observations(&mut self) -> Vec<Observation> {
        std::mem::take(&mut self.run_observations)
    }

    /// Places the markers the last output applied at the cursor's row. One read places every
    /// marker it held on the row its output ended on.
    pub(super) fn observe_run_markers(&mut self) {
        let markers = self.metadata.take_run_markers();
        if markers.is_empty() {
            return;
        }
        // Epoch 0 names no history, so a client reads such a position as a reset history.
        let position = self
            .cursor_history_position()
            .unwrap_or(HistoryPosition { epoch: 0, row: 0 });
        self.run_observations
            .extend(markers.into_iter().map(|marker| Observation {
                marker: marker.marker,
                finished_commands: marker.finished_commands,
                position,
                at: marker.at,
            }));
    }

    fn cursor_history_position(&mut self) -> Result<HistoryPosition, Error> {
        let history = self.history.update(&mut self.terminal)?;
        let row = self.terminal.cursor_y()?;
        Ok(HistoryPosition {
            epoch: history.epoch,
            row: history.total + u64::from(row),
        })
    }
}
