//! Server-side run receipts. A run binds one command, written as one bracketed paste at the
//! prompt, to the first owner-marked command start after it and the finish that follows.
//!
//! The table never writes to a PTY. The host checks [`RunTable::check`], writes the command on
//! the Terminal Session's worker, then records the admission. Each owner-marked marker the worker
//! saw after it passes through [`RunTable::observe_marker`] in order. A receipt stays until its
//! client acknowledges it, so a client that lost a reply resolves the run without resending it.
use crate::hierarchy::Id;
use crate::protocol::{
    Channel, HistoryPosition, RunDoubt, RunId, RunReceipt, RunRefusal, RunState,
};
use std::collections::BTreeMap;
use std::time::Instant;

/// The most receipts kept per Pane until `RunAcknowledge`.
pub const MAX_PER_PANE: usize = 8;
/// The most receipts kept in total.
pub const MAX_TOTAL: usize = 128;

/// An OSC 133 marker, as the run table observes it. Every marker except `Unowned` carries the
/// Terminal Session's Prompt Owner, and `PromptStart` is a fresh prompt, never a redraw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Marker {
    PromptStart,
    CommandInput,
    /// `cmdline` is the decoded text a shell reports, when it reports one.
    CommandStart {
        cmdline: Option<String>,
    },
    CommandFinished {
        exit_status: Option<i32>,
    },
    /// A prompt or command marker without the owner, such as a nested shell's, or markers the
    /// worker could not keep.
    Unowned,
}

/// One marker with the facts the Terminal Session held when it applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub marker: Marker,
    /// The Session's finished-command count once the marker applied.
    pub finished_commands: u64,
    pub position: HistoryPosition,
    pub at: Instant,
}

/// What a run was admitted against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admission {
    pub pane: Id,
    pub session: Id,
    pub run: RunId,
    /// The Client Identity that admitted the run. Only it reads or retires the receipt.
    pub client: [u8; 32],
    /// Where `PaneEvent::RunFinished` goes when the run settles.
    pub connection: [u8; 32],
    pub channel: Channel,
    pub command: String,
    pub start: HistoryPosition,
    /// The Session's finished-command count at admission.
    pub finished_commands: u64,
    pub at: Instant,
}

/// A receipt that settled, and the channel that admitted it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settled {
    pub connection: [u8; 32],
    pub channel: Channel,
    pub receipt: RunReceipt,
}

struct Entry {
    admission: Admission,
    state: RunState,
    end: Option<HistoryPosition>,
    started: Option<Instant>,
    /// The first reason the finish may not belong to the run. It settles as this doubt.
    doubt: Option<RunDoubt>,
    taken_over: bool,
}

impl Entry {
    fn pending(&self) -> bool {
        matches!(self.state, RunState::Admitted | RunState::Running)
    }

    fn receipt(&self) -> RunReceipt {
        RunReceipt {
            run: self.admission.run,
            session: self.admission.session,
            state: if self.pending() && self.taken_over {
                RunState::TakenOver
            } else {
                self.state
            },
            end: self.end,
        }
    }

    fn settle(&mut self, state: RunState, end: Option<HistoryPosition>) -> Settled {
        self.state = match (state, self.doubt) {
            (RunState::Finished { .. }, Some(doubt)) => RunState::Unconfirmed(doubt),
            (state, _) => state,
        };
        self.end = end;
        Settled {
            connection: self.admission.connection,
            channel: self.admission.channel,
            receipt: self.receipt(),
        }
    }

    /// Applies one marker. Returns the receipt when the marker settled the run.
    fn observe(&mut self, observation: &Observation) -> Option<Settled> {
        if !self.pending() {
            return None;
        }
        let expected = self.admission.finished_commands.saturating_add(1);
        match (&observation.marker, self.state) {
            (Marker::CommandInput, _) => None,
            (Marker::Unowned, _) => {
                self.doubt.get_or_insert(RunDoubt::IntegrationLost);
                None
            }
            (Marker::CommandStart { cmdline }, RunState::Admitted) => {
                if cmdline
                    .as_ref()
                    .is_some_and(|line| *line != self.admission.command)
                {
                    self.doubt.get_or_insert(RunDoubt::CmdlineMismatch);
                }
                self.state = RunState::Running;
                self.started = Some(observation.at);
                None
            }
            // A second command start belongs to something other than the run.
            (Marker::CommandStart { .. }, _) => {
                self.doubt.get_or_insert(RunDoubt::IntegrationLost);
                None
            }
            (Marker::PromptStart | Marker::CommandFinished { .. }, RunState::Admitted) => {
                Some(self.settle(
                    RunState::Unconfirmed(RunDoubt::NoCommandStart),
                    Some(observation.position),
                ))
            }
            (Marker::CommandFinished { exit_status }, _) => {
                if observation.finished_commands != expected {
                    self.doubt.get_or_insert(RunDoubt::IntegrationLost);
                }
                let state = match exit_status {
                    Some(_) => RunState::Finished {
                        exit_status: *exit_status,
                        duration_ms: self.duration_ms(observation.at),
                    },
                    None => RunState::Unconfirmed(RunDoubt::NoExitStatus),
                };
                Some(self.settle(state, Some(observation.position)))
            }
            // A prompt while the command runs finishes it without a status.
            (Marker::PromptStart, _) => Some(self.settle(
                RunState::Unconfirmed(RunDoubt::NoExitStatus),
                Some(observation.position),
            )),
        }
    }

    fn duration_ms(&self, at: Instant) -> u64 {
        let started = self.started.unwrap_or(self.admission.at);
        u64::try_from(at.saturating_duration_since(started).as_millis()).unwrap_or(u64::MAX)
    }
}

/// Every run receipt the server keeps, across Panes and Terminal Sessions.
#[derive(Default)]
pub struct RunTable {
    receipts: BTreeMap<RunId, Entry>,
}

impl RunTable {
    /// Whether a run on `pane` could be admitted now. Call it before writing the command.
    pub fn check(&self, pane: &Id) -> Result<(), RunRefusal> {
        if self
            .receipts
            .values()
            .any(|entry| entry.admission.pane == *pane && entry.pending())
        {
            return Err(RunRefusal::RunPending);
        }
        let on_pane = self
            .receipts
            .values()
            .filter(|entry| entry.admission.pane == *pane)
            .count();
        if self.receipts.len() >= MAX_TOTAL || on_pane >= MAX_PER_PANE {
            return Err(RunRefusal::Capacity);
        }
        Ok(())
    }

    /// Whether `client` already used `run`. A reused id is never written again.
    pub fn known(&self, client: &[u8; 32], run: &RunId) -> bool {
        self.receipts
            .get(run)
            .is_some_and(|entry| entry.admission.client == *client)
    }

    /// Records a run whose command was written. [`Self::check`] passed under the same lock.
    pub fn admit(&mut self, admission: Admission) -> Result<(), RunRefusal> {
        self.check(&admission.pane)?;
        if self.receipts.contains_key(&admission.run) {
            return Err(RunRefusal::RunPending);
        }
        self.receipts.insert(
            admission.run,
            Entry {
                admission,
                state: RunState::Admitted,
                end: None,
                started: None,
                doubt: None,
                taken_over: false,
            },
        );
        Ok(())
    }

    /// Updates the Pane's pending run and returns its receipt when the marker settled it.
    pub fn observe_marker(&mut self, pane: &Id, observation: &Observation) -> Option<Settled> {
        self.pending_mut(pane)?.observe(observation)
    }

    /// The Pane's pending run, if any, and the Terminal Session it was admitted on.
    pub fn pending(&self, pane: &Id) -> Option<(RunId, Id)> {
        self.receipts
            .values()
            .find(|entry| entry.admission.pane == *pane && entry.pending())
            .map(|entry| (entry.admission.run, entry.admission.session))
    }

    /// Every pending run: its Pane, Terminal Session and Client Identity.
    pub fn pending_runs(&self) -> Vec<(Id, Id, [u8; 32])> {
        self.receipts
            .values()
            .filter(|entry| entry.pending())
            .map(|entry| {
                (
                    entry.admission.pane,
                    entry.admission.session,
                    entry.admission.client,
                )
            })
            .collect()
    }

    /// Settles a pending run whose command the worker never wrote, after it was recorded.
    pub fn abandon(&mut self, pane: &Id) -> Option<Settled> {
        Some(
            self.pending_mut(pane)?
                .settle(RunState::Unconfirmed(RunDoubt::NoCommandStart), None),
        )
    }

    /// Settles a pending run whose Terminal Session ended or whose Pane closed.
    pub fn session_ended(&mut self, pane: &Id) -> Option<Settled> {
        Some(self.pending_mut(pane)?.settle(RunState::SessionEnded, None))
    }

    /// Another client took over the Pane. The run keeps following its command, and reads as
    /// `TakenOver` until it settles.
    pub fn taken_over(&mut self, pane: &Id) {
        if let Some(entry) = self.pending_mut(pane) {
            entry.taken_over = true;
        }
    }

    /// The receipt `client` holds for `run`.
    pub fn receipt(&self, client: &[u8; 32], run: &RunId) -> Option<RunReceipt> {
        self.receipts
            .get(run)
            .filter(|entry| entry.admission.client == *client)
            .map(Entry::receipt)
    }

    /// Where the run's output starts, for the client that admitted it.
    pub fn start(&self, client: &[u8; 32], run: &RunId) -> Option<HistoryPosition> {
        self.receipts
            .get(run)
            .filter(|entry| entry.admission.client == *client)
            .map(|entry| entry.admission.start)
    }

    /// Retires a settled receipt of `client`. Returns whether one existed. A pending run stays,
    /// because its command still runs.
    pub fn acknowledge(&mut self, client: &[u8; 32], run: &RunId) -> bool {
        let retire = self
            .receipts
            .get(run)
            .is_some_and(|entry| entry.admission.client == *client && !entry.pending());
        retire && self.receipts.remove(run).is_some()
    }

    /// Forgets every receipt of a revoked Client Identity.
    pub fn forget(&mut self, client: &[u8; 32]) {
        self.receipts
            .retain(|_, entry| entry.admission.client != *client);
    }

    fn pending_mut(&mut self, pane: &Id) -> Option<&mut Entry> {
        self.receipts
            .values_mut()
            .find(|entry| entry.admission.pane == *pane && entry.pending())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const CLIENT: [u8; 32] = [7; 32];
    const PANE: Id = [1; 32];

    fn position(row: u64) -> HistoryPosition {
        HistoryPosition { epoch: 1, row }
    }

    fn admission(run: u8, pane: Id, at: Instant) -> Admission {
        Admission {
            pane,
            session: [2; 32],
            run: RunId([run; 16]),
            client: CLIENT,
            connection: [3; 32],
            channel: Channel(1),
            command: "echo 'a b'".to_owned(),
            start: position(10),
            finished_commands: 4,
            at,
        }
    }

    fn marker(marker: Marker, finished_commands: u64, row: u64, at: Instant) -> Observation {
        Observation {
            marker,
            finished_commands,
            position: position(row),
            at,
        }
    }

    fn admitted(at: Instant) -> RunTable {
        let mut table = RunTable::default();
        table.admit(admission(1, PANE, at)).unwrap();
        table
    }

    fn state(table: &RunTable, run: u8) -> RunState {
        table.receipt(&CLIENT, &RunId([run; 16])).unwrap().state
    }

    #[test]
    fn a_finish_after_the_command_start_settles_with_its_status_and_duration() {
        let at = Instant::now();
        let mut table = admitted(at);
        let start = Marker::CommandStart {
            cmdline: Some("echo 'a b'".to_owned()),
        };
        assert_eq!(
            table.observe_marker(&PANE, &marker(start, 4, 10, at + Duration::from_millis(5))),
            None
        );
        assert_eq!(state(&table, 1), RunState::Running);

        let finish = Marker::CommandFinished {
            exit_status: Some(3),
        };
        let settled = table
            .observe_marker(
                &PANE,
                &marker(finish, 5, 12, at + Duration::from_millis(30)),
            )
            .unwrap();

        assert_eq!(
            settled.receipt.state,
            RunState::Finished {
                exit_status: Some(3),
                duration_ms: 25
            }
        );
        assert_eq!(settled.receipt.end, Some(position(12)));
        assert_eq!(settled.channel, Channel(1));
    }

    #[test]
    fn every_doubt_settles_as_unconfirmed_and_never_as_success() {
        let at = Instant::now();
        let start = |cmdline: &str| Marker::CommandStart {
            cmdline: Some(cmdline.to_owned()),
        };
        let finish = |status| Marker::CommandFinished {
            exit_status: status,
        };
        let cases = [
            (
                vec![(start("echo 'a  b'"), 4), (finish(Some(0)), 5)],
                RunDoubt::CmdlineMismatch,
            ),
            (vec![(Marker::PromptStart, 4)], RunDoubt::NoCommandStart),
            (
                vec![
                    (Marker::CommandStart { cmdline: None }, 4),
                    (finish(None), 5),
                ],
                RunDoubt::NoExitStatus,
            ),
            (
                vec![
                    (Marker::CommandStart { cmdline: None }, 4),
                    (Marker::PromptStart, 5),
                ],
                RunDoubt::NoExitStatus,
            ),
            (
                vec![
                    (Marker::CommandStart { cmdline: None }, 4),
                    (Marker::Unowned, 5),
                    (finish(Some(0)), 6),
                ],
                RunDoubt::IntegrationLost,
            ),
            (
                vec![
                    (Marker::CommandStart { cmdline: None }, 4),
                    (finish(Some(0)), 7),
                ],
                RunDoubt::IntegrationLost,
            ),
        ];
        for (markers, doubt) in cases {
            let mut table = admitted(at);
            let mut settled = None;
            for (index, (observed, finished)) in markers.into_iter().enumerate() {
                settled =
                    table.observe_marker(&PANE, &marker(observed, finished, index as u64, at));
            }
            assert_eq!(
                settled.map(|settled| settled.receipt.state),
                Some(RunState::Unconfirmed(doubt))
            );
        }
    }

    #[test]
    fn a_second_run_on_one_pane_waits_for_the_first_to_settle() {
        let at = Instant::now();
        let mut table = admitted(at);
        assert_eq!(table.check(&PANE), Err(RunRefusal::RunPending));
        assert_eq!(table.check(&[9; 32]), Ok(()));

        table.session_ended(&PANE).unwrap();

        assert_eq!(state(&table, 1), RunState::SessionEnded);
        assert_eq!(table.check(&PANE), Ok(()));
        table.admit(admission(2, PANE, at)).unwrap();
    }

    #[test]
    fn receipts_stay_until_their_own_client_acknowledges_them() {
        let at = Instant::now();
        let mut table = admitted(at);
        let run = RunId([1; 16]);
        assert!(!table.acknowledge(&CLIENT, &run), "a pending run stays");
        table.session_ended(&PANE);
        assert_eq!(table.receipt(&[8; 32], &run), None);
        assert!(!table.acknowledge(&[8; 32], &run));

        assert!(table.acknowledge(&CLIENT, &run));
        assert_eq!(table.receipt(&CLIENT, &run), None);
    }

    #[test]
    fn capacity_bounds_receipts_per_pane_and_in_total() {
        let at = Instant::now();
        let mut table = RunTable::default();
        for run in 0..MAX_PER_PANE as u8 {
            table.admit(admission(run, PANE, at)).unwrap();
            table.session_ended(&PANE);
        }
        assert_eq!(table.check(&PANE), Err(RunRefusal::Capacity));

        let mut table = RunTable::default();
        for run in 0..MAX_TOTAL as u8 {
            table.admit(admission(run, [run; 32], at)).unwrap();
        }
        assert_eq!(table.check(&[200; 32]), Err(RunRefusal::Capacity));
    }

    #[test]
    fn a_take_over_reads_as_taken_over_until_the_command_finishes() {
        let at = Instant::now();
        let mut table = admitted(at);
        table.observe_marker(
            &PANE,
            &marker(Marker::CommandStart { cmdline: None }, 4, 10, at),
        );
        table.taken_over(&PANE);
        assert_eq!(state(&table, 1), RunState::TakenOver);

        table.observe_marker(
            &PANE,
            &marker(
                Marker::CommandFinished {
                    exit_status: Some(0),
                },
                5,
                11,
                at,
            ),
        );

        assert!(matches!(state(&table, 1), RunState::Finished { .. }));
    }

    #[test]
    fn revocation_forgets_the_clients_receipts() {
        let mut table = admitted(Instant::now());
        table.forget(&CLIENT);
        assert_eq!(table.pending(&PANE), None);
    }
}
