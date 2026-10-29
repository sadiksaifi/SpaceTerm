//! `pane run` on a shared server: admission under the `PaneService` lock, and receipts that
//! follow each run's Terminal Session until its client acknowledges them.
use super::super::RunRequest;
use super::*;
use spaceterm_server::pane::run::{Admission, Settled};
use spaceterm_server::protocol::{self, HistoryPosition, RunId, RunReply};
use std::sync::mpsc;

/// How long admission waits for the Terminal Session's worker. A run the worker answers later
/// is still followed, from an unknown start.
const ADMISSION_WAIT: Duration = Duration::from_secs(5);

/// A run recorded before its worker answered.
pub(super) struct LateRun {
    pane: Id,
    reply: mpsc::Receiver<Result<crate::terminal::session::RunAdmission, protocol::RunRefusal>>,
}

impl SharedWorkspaceHost {
    pub(super) fn admit_run(&mut self, request: RunRequest) -> Result<RunReply, Refusal> {
        let RunRequest {
            device,
            connection,
            lease,
            channel,
            run,
            command,
            session,
        } = request;
        let pane_id = self
            .panes
            .iter()
            .find_map(|(id, host)| {
                host.pane
                    .as_ref()
                    .is_some_and(|pane| pane.lease == lease)
                    .then_some(*id)
            })
            .ok_or(Refusal::Stale)?;
        let pane = self
            .panes
            .get_mut(&pane_id)
            .and_then(|host| host.pane.as_mut())
            .ok_or(Refusal::Stale)?;
        if pane.device != Some(device) || !pane.connected || lease.connection != connection {
            return Err(Refusal::Stale);
        }
        pane.drain();
        if pane.ended || pane.failed {
            return Err(Refusal::Missing);
        }
        if session != lease.session {
            return Ok(RunReply::Refused(protocol::RunRefusal::SessionChanged));
        }
        // A reused id is answered, never written again.
        if let Some(receipt) = self.runs.receipt(&device, &run) {
            return Ok(RunReply::Receipt(receipt));
        }
        if let Err(refusal) = self.runs.check(&pane_id) {
            return Ok(RunReply::Refused(refusal));
        }
        let at = Instant::now();
        let reply = pane.runtime.run(command.clone());
        let (start, finished_commands, late) = match reply.recv_timeout(ADMISSION_WAIT) {
            Ok(Ok(admission)) => (admission.start, admission.finished_commands, None),
            Ok(Err(refusal)) => return Ok(RunReply::Refused(refusal)),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(Refusal::Missing),
            // Epoch 0 names no history. The finish binds by the run's markers alone.
            Err(mpsc::RecvTimeoutError::Timeout) => (
                HistoryPosition { epoch: 0, row: 0 },
                0,
                Some(LateRun {
                    pane: pane_id,
                    reply,
                }),
            ),
        };
        self.runs
            .admit(Admission {
                pane: pane_id,
                session,
                run,
                client: device,
                connection,
                channel,
                command,
                start,
                finished_commands,
                at,
            })
            .map_err(|_| Refusal::Failed)?;
        self.late_runs.extend(late);
        Ok(RunReply::Admitted { start })
    }

    pub(super) fn run_receipt(&mut self, device: Id, run: RunId) -> Result<RunReply, Refusal> {
        self.settle_runs();
        self.runs
            .receipt(&device, &run)
            .map(RunReply::Receipt)
            .ok_or(Refusal::Missing)
    }

    pub(super) fn retire_run(&mut self, device: Id, run: RunId) -> Result<(), Refusal> {
        self.runs
            .acknowledge(&device, &run)
            .then_some(())
            .ok_or(Refusal::Missing)
    }

    /// Applies what each pending run's Terminal Session observed since the last call: its
    /// markers, a take over, and the end of the Session.
    pub(super) fn settle_runs(&mut self) {
        let mut late = std::mem::take(&mut self.late_runs);
        late.retain(|late| match late.reply.try_recv() {
            Ok(Ok(_)) => false,
            Err(mpsc::TryRecvError::Empty) => true,
            Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                self.settled_runs.extend(self.runs.abandon(&late.pane));
                false
            }
        });
        self.late_runs = late;
        for (pane_id, session, client) in self.runs.pending_runs() {
            let pane = self
                .panes
                .get_mut(&pane_id)
                .and_then(|host| host.pane.as_mut())
                .filter(|pane| pane.lease.session == session);
            let Some(pane) = pane else {
                self.settled_runs.extend(self.runs.session_ended(&pane_id));
                continue;
            };
            for observation in pane.subscription.take_run_observations() {
                self.settled_runs
                    .extend(self.runs.observe_marker(&pane_id, &observation));
            }
            if pane.device != Some(client) {
                self.runs.taken_over(&pane_id);
            }
            pane.drain();
            if pane.ended || pane.failed {
                self.settled_runs.extend(self.runs.session_ended(&pane_id));
            }
        }
    }

    pub(super) fn drain_settled_runs(&mut self) -> Vec<Settled> {
        self.settle_runs();
        std::mem::take(&mut self.settled_runs)
    }
}

#[cfg(test)]
impl SharedWorkspaceHost {
    /// The lease of the Pane's running Terminal Session.
    pub(in crate::runtime::server) fn running_lease(&self, pane: Id) -> Option<Lease> {
        self.panes.get(&pane)?.pane.as_ref().map(|pane| pane.lease)
    }
}
