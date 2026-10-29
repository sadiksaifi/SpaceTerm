//! Server-side run receipts. A run binds one command, written as one bracketed paste at the
//! prompt, to the first owner-marked command start after it and the finish that follows.
use crate::hierarchy::Id;
use crate::protocol::{HistoryPosition, RunId, RunReceipt, RunRefusal};
use std::collections::BTreeMap;

/// The most receipts kept per Pane until `RunAcknowledge`.
pub const MAX_PER_PANE: usize = 8;
/// The most receipts kept in total.
pub const MAX_TOTAL: usize = 128;

/// An owner-marked OSC 133 marker, as the run table observes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Marker {
    PromptStart,
    CommandInput,
    CommandStart { cmdline: Option<String> },
    CommandFinished { exit_status: Option<i32> },
}

/// What a run was admitted against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admission {
    pub pane: Id,
    pub session: Id,
    pub run: RunId,
    pub command: String,
    pub start: HistoryPosition,
    /// The Session's finished-command count at admission.
    pub finished_commands: u64,
}

#[derive(Default)]
pub struct RunTable {
    receipts: BTreeMap<RunId, (Admission, RunReceipt)>,
}

impl RunTable {
    pub fn admit(&mut self, _admission: Admission) -> Result<(), RunRefusal> {
        Err(RunRefusal::Capacity)
    }

    /// Updates the Pane's pending run and returns its receipt when the marker settled it.
    pub fn observe_marker(&mut self, _pane: &Id, _marker: &Marker) -> Option<RunReceipt> {
        None
    }

    pub fn receipt(&self, run: &RunId) -> Option<RunReceipt> {
        self.receipts.get(run).map(|(_, receipt)| receipt.clone())
    }

    /// Retires a receipt. Returns whether one existed.
    pub fn acknowledge(&mut self, run: &RunId) -> bool {
        self.receipts.remove(run).is_some()
    }
}
