//! `pane run` on the terminal worker: admission between two PTY reads, the markers a pending
//! run follows, and the input and output progress a client confirms delivery and idleness by.
use super::*;
use spaceterm_server::pane::run::{Marker, Observation};
use spaceterm_server::protocol::{HistoryPosition, RunRefusal};

/// The most run observations a subscription keeps unread. Beyond it the run lost track.
const MAX_RUN_OBSERVATIONS: usize = 64;

/// What the worker admitted a run against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RunAdmission {
    /// The row the prompt is on, where the run's output starts.
    pub(crate) start: HistoryPosition,
    /// The Terminal Session's finished-command count before the run.
    pub(crate) finished_commands: u64,
}

/// Receives one run's admission or refusal, once.
pub(crate) type RunReply = mpsc::SyncSender<Result<RunAdmission, RunRefusal>>;

/// The highest client Input serial the PTY accepted, and the PTY output bytes read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Progress {
    pub(crate) input: u32,
    pub(crate) output_bytes: u64,
}

/// The worker's output count and the progress it last published.
#[derive(Default)]
pub(super) struct ProgressCounter {
    pub(super) output_bytes: u64,
    published: Progress,
}

impl TerminalRuntime {
    /// Writes `command` as one bracketed paste and Return at the owning shell's empty prompt,
    /// after the input and output the worker already received, or refuses it.
    pub(crate) fn run(&self, command: String) -> mpsc::Receiver<Result<RunAdmission, RunRefusal>> {
        let (reply, receiver) = mpsc::sync_channel(1);
        if let Some(commands) = &self.commands {
            // A stopped worker drops the command and its reply, which disconnects the receiver.
            let _ = commands.send(Command::Run(command, reply));
        }
        receiver
    }
}

impl TerminalSubscription {
    /// The progress the worker last published.
    pub(crate) fn progress(&self) -> Progress {
        self.state.progress()
    }

    /// The markers a pending run followed since the last call, oldest first.
    pub(crate) fn take_run_observations(&self) -> Vec<Observation> {
        self.state.take_run_observations()
    }
}

impl TerminalWorker {
    pub(super) fn process_run(&mut self, command: &str, reply: RunReply) -> bool {
        if !self.write_pending_pty_responses() {
            let _ = reply.try_send(Err(RunRefusal::InputPending));
            return false;
        }
        let admitted = match self.emulator.admit_run(command) {
            Ok(admitted) => admitted,
            Err(refusal) => {
                let _ = reply.try_send(Err(refusal));
                return true;
            }
        };
        self.emulator.note_input(admitted.bytes.len());
        if !self.write_pty(&admitted.bytes) {
            let _ = reply.try_send(Err(RunRefusal::InputPending));
            return false;
        }
        self.line_transition();
        let _ = reply.try_send(Ok(RunAdmission {
            start: admitted.start,
            finished_commands: admitted.finished_commands,
        }));
        self.publish_visible_screen_change()
    }

    /// Hands the markers the last output applied to the subscription.
    pub(super) fn deliver_run_observations(&mut self) {
        let observations = self.emulator.take_run_observations();
        if !observations.is_empty() {
            self.events.run_observations(observations);
        }
    }

    /// Publishes progress that advanced. Only a written Input serial notifies, because output
    /// already notifies with its screens.
    pub(super) fn publish_progress(&mut self) {
        let progress = Progress {
            input: self.input.written_serial(),
            output_bytes: self.progress.output_bytes,
        };
        if progress == self.progress.published {
            return;
        }
        let notify = progress.input != self.progress.published.input;
        self.progress.published = progress;
        self.events.set_progress(progress, notify);
    }
}

impl TerminalEventPublisher {
    pub(super) fn progress(&self) -> Progress {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).progress
    }

    pub(super) fn set_progress(&self, progress: Progress, wake: bool) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.progress = progress;
        if wake {
            subscriptions::notify(state);
        }
    }

    /// Keeps a run's markers until the subscriber reads them. Markers beyond the bound end as
    /// one marker without the owner, so the run settles as unconfirmed.
    pub(super) fn run_observations(&self, observations: Vec<Observation>) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        for observation in observations {
            if state.run_observations.len() + 1 < MAX_RUN_OBSERVATIONS {
                state.run_observations.push_back(observation);
            } else if state
                .run_observations
                .back()
                .is_none_or(|last| last.marker != Marker::Unowned)
            {
                state.run_observations.push_back(Observation {
                    marker: Marker::Unowned,
                    ..observation
                });
            }
        }
        subscriptions::notify(state);
    }

    fn take_run_observations(&self) -> Vec<Observation> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.run_observations.drain(..).collect()
    }
}
