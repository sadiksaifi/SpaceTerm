//! `pane run` on a shared server: queued admission on the Terminal Session worker, and receipts that
//! follow each run's Terminal Session until its client acknowledges them.
use super::super::RunRequest;
use super::*;
use spaceterm_server::pane::run::{Admission, Settled};
use spaceterm_server::protocol::{self, HistoryPosition, RunId, RunReply};
use std::sync::mpsc;

/// A reserved run waiting for its Terminal Session's worker. The host polls it on ticks,
/// so neither a worker delay nor a PTY write holds the PaneService lock.
pub(super) struct Admitting {
    pane: Id,
    run: RunId,
    reply: mpsc::Receiver<Result<crate::terminal::session::RunAdmission, protocol::RunRefusal>>,
    answer: super::super::RunAnswer,
}

impl WorkspaceHost {
    pub(super) fn admit_run(
        &mut self,
        request: RunRequest,
        answer: super::super::RunAnswer,
    ) -> Result<(), Refusal> {
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
            answer(Ok(RunReply::Refused(protocol::RunRefusal::SessionChanged)));
            return Ok(());
        }
        // A reused id is answered, never written again.
        if let Some(receipt) = self.runs.receipt(&device, &run) {
            answer(Ok(RunReply::Receipt(receipt)));
            return Ok(());
        }
        if let Err(refusal) = self.runs.check(&pane_id) {
            answer(Ok(RunReply::Refused(refusal)));
            return Ok(());
        }
        let at = Instant::now();
        let start = HistoryPosition { epoch: 0, row: 0 };
        self.runs
            .admit(Admission {
                pane: pane_id,
                session,
                run,
                client: device,
                connection,
                channel,
                command: command.clone(),
                start,
                finished_commands: 0,
                at,
            })
            .map_err(|_| Refusal::Failed)?;
        self.admitting_runs.push(Admitting {
            pane: pane_id,
            run,
            reply: pane.runtime.run(command),
            answer,
        });
        Ok(())
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
        let mut admitting = std::mem::take(&mut self.admitting_runs);
        let mut waiting = Vec::new();
        for pending in admitting.drain(..) {
            let result = match pending.reply.try_recv() {
                Ok(Ok(admission)) => {
                    self.runs
                        .admitted(&pending.run, admission.start, admission.finished_commands);
                    Ok(RunReply::Admitted {
                        start: admission.start,
                    })
                }
                Ok(Err(refusal)) => {
                    self.runs.refused(&pending.run);
                    Ok(RunReply::Refused(refusal))
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.runs.refused(&pending.run);
                    Err(Refusal::Missing)
                }
                Err(mpsc::TryRecvError::Empty) => {
                    waiting.push(pending);
                    continue;
                }
            };
            (pending.answer)(result);
        }
        self.admitting_runs = waiting;
        for (pane_id, session, client) in self.runs.pending_runs() {
            if self
                .admitting_runs
                .iter()
                .any(|pending| pending.pane == pane_id)
            {
                continue;
            }
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
impl WorkspaceHost {
    /// The lease of the Pane's running Terminal Session.
    pub(in crate::runtime::server) fn running_lease(&self, pane: Id) -> Option<Lease> {
        self.panes.get(&pane)?.pane.as_ref().map(|pane| pane.lease)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::native_pty::{
        NativePtyAdapterConstructionFailure, NativePtyAdapterFactory, NativePtyAdapterParts,
        NativePtySize,
    };
    use crate::platform::shell_launch::PreparedShellLaunch;
    use crate::runtime::server::tests::{create_workspace, size, workspace_inventory};
    use std::sync::Mutex;

    /// A stalled external PTY constructor keeps the Session worker from answering admission.
    struct StalledPty(Mutex<mpsc::Receiver<()>>, mpsc::SyncSender<()>);
    impl NativePtyAdapterFactory for StalledPty {
        fn create(
            &self,
            _: PreparedShellLaunch,
            _: NativePtySize,
        ) -> Result<NativePtyAdapterParts, NativePtyAdapterConstructionFailure> {
            self.1.send(()).unwrap();
            let _ = self.0.lock().unwrap().recv_timeout(Duration::from_secs(10));
            Err(NativePtyAdapterConstructionFailure::ProcessStartFailed)
        }
    }

    #[test]
    fn a_stalled_run_admission_leaves_other_panes_available() {
        let (release, blocked) = mpsc::channel();
        let (entered, started) = mpsc::sync_channel(1);
        let prototype = ServerPaneHost::local_with_pty_factory(
            Arc::new(StalledPty(Mutex::new(blocked), entered)),
            Arc::default(),
        )
        .unwrap();
        let mut host = WorkspaceHost::new(prototype);
        let workspace = create_workspace(&mut host, 1);
        let pane = workspace_inventory(&mut host)
            .workspaces
            .iter()
            .find(|entry| entry.id == workspace)
            .unwrap()
            .tabs[0]
            .root_pane;
        let Reply::Attached { lease } = host.request(
            [1; 32],
            [1; 32],
            Request::SelectPane {
                pane,
                explicit: true,
                size: size(),
                style: None,
            },
        ) else {
            panic!("a claim starts the Session worker");
        };
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let (answer, received) = mpsc::sync_channel(1);
        let at = Instant::now();
        host.admit_run(
            RunRequest {
                device: [1; 32],
                connection: [1; 32],
                lease,
                channel: protocol::Channel(1),
                run: RunId([1; 16]),
                command: "true".into(),
                session: lease.session,
            },
            Box::new(move |result| {
                let _ = answer.send(result);
            }),
        )
        .unwrap();
        assert!(at.elapsed() < Duration::from_millis(250));
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_ne!(create_workspace(&mut host, 2), workspace);
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            host.settle_runs();
            if received.try_recv().is_ok() {
                break;
            }
            assert!(Instant::now() < until, "the released worker answers");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(host.runs.pending(&pane).is_none());
        host.shutdown().unwrap();
    }
}
