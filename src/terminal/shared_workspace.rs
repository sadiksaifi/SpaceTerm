//! One authenticated inventory subscription and durable edit journal per desktop client.
use crate::local_server::{LocalServer, ServerError};
use crate::terminal::server_session::PaneControls;
use spaceterm_server::{
    hierarchy::{ClientJournal, Inventory, Mutation, RetainedOutcome},
    pane::{PaneConnection, Reply, Request},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

#[derive(Clone)]
pub(crate) struct SharedWorkspaceSnapshot {
    pub worktree_writes: bool,
    pub inventory: Option<Inventory>,
    pub connected: bool,
    pub pending: bool,
    pub pending_operation: Option<Mutation>,
    pub rejected: Option<Mutation>,
    pub outcome: Option<RetainedOutcome>,
    pub error: Option<ServerError>,
}
pub(crate) struct SharedWorkspaceClient {
    snapshot: Arc<Mutex<SharedWorkspaceSnapshot>>,
    commands: mpsc::SyncSender<Mutation>,
    alive: Arc<AtomicBool>,
}
impl Drop for SharedWorkspaceClient {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
    }
}
impl SharedWorkspaceClient {
    /// Pane control records last only as long as the server lists their Panes.
    pub(crate) fn new(
        server: LocalServer,
        controls: Arc<PaneControls>,
        legacy_location: String,
    ) -> Self {
        let snapshot = Arc::new(Mutex::new(SharedWorkspaceSnapshot {
            worktree_writes: false,
            inventory: None,
            connected: false,
            pending: false,
            pending_operation: None,
            rejected: None,
            outcome: None,
            error: None,
        }));
        let alive = Arc::new(AtomicBool::new(true));
        let (commands, receiver) = mpsc::sync_channel(1);
        let state = Arc::clone(&snapshot);
        let running = Arc::clone(&alive);
        if std::thread::Builder::new()
            .name("spaceterm-shared-workspaces".into())
            .spawn(move || run(server, controls, state, receiver, running, legacy_location))
            .is_err()
        {
            snapshot.lock().unwrap_or_else(|e| e.into_inner()).error =
                Some(ServerError::StartupFailed);
        }
        Self {
            snapshot,
            commands,
            alive,
        }
    }
    pub(crate) fn supports_worktree_writes(&self) -> bool {
        self.snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .worktree_writes
    }
    pub(crate) fn snapshot(&self) -> SharedWorkspaceSnapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Availability checks do not copy the hierarchy or terminal directory metadata.
    pub(crate) fn availability(&self) -> (bool, bool) {
        let state = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        (state.connected, state.pending)
    }
    /// Offline submissions are refused here even if an action's rendered enabled state is stale.
    pub(crate) fn submit(&self, mutation: Mutation) -> bool {
        let mut state = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        if !state.connected || state.pending {
            return false;
        }
        if !mutation.is_bounded() {
            state.error = Some(ServerError::Unsupported);
            return false;
        }
        state.pending = true;
        state.outcome = None;
        state.rejected = None;
        state.pending_operation = Some(mutation.clone());
        if self.commands.try_send(mutation).is_err() {
            state.pending = false;
            state.pending_operation = None;
            return false;
        }
        true
    }
}
fn inventory(
    connection: &mut PaneConnection,
    controls: &PaneControls,
) -> Result<(Inventory, u64), ServerError> {
    let Reply::Inventory {
        inventory,
        next_sequence,
    } = connection.exchange(&Request::Inventory)?
    else {
        return Err(ServerError::Protocol);
    };
    controls.retain(&inventory);
    Ok((inventory, next_sequence))
}
fn finish(
    connection: &mut PaneConnection,
    journal: &mut ClientJournal,
    controls: &PaneControls,
) -> Result<Option<RetainedOutcome>, ServerError> {
    let Some(pending) = journal.pending().cloned() else {
        return Ok(journal.last().cloned());
    };
    let result = if let Some(result) = pending.result {
        result
    } else {
        let resolved = loop {
            let reply = match connection.exchange(&Request::Resolve {
                sequence: pending.sequence,
            }) {
                Ok(reply) => reply,
                Err(ServerError::SessionUnavailable) => connection.exchange(&Request::Mutate {
                    sequence: pending.sequence,
                    mutation: pending.mutation.clone(),
                })?,
                Err(error) => return Err(error),
            };
            if matches!(reply, Reply::MutationPending) {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            break reply;
        };
        let Reply::Outcome(outcome) = resolved else {
            return Err(ServerError::Protocol);
        };
        // Reconcile the accepted membership before retiring the operation identity.
        inventory(connection, controls)?;
        journal.complete(outcome.clone())?;
        outcome
    };
    // Validation precedes acknowledgement, including when a saved completion resumes after quit.
    if result.sequence != pending.sequence || result.mutation != pending.mutation {
        return Err(ServerError::Protocol);
    }
    connection.exchange(&Request::Acknowledge {
        sequence: pending.sequence,
    })?;
    journal.retire()?;
    Ok(Some(result))
}
fn run(
    server: LocalServer,
    controls: Arc<PaneControls>,
    state: Arc<Mutex<SharedWorkspaceSnapshot>>,
    commands: mpsc::Receiver<Mutation>,
    alive: Arc<AtomicBool>,
    legacy_location: String,
) {
    let mut active: Option<(u64, Mutation)> = None;
    while alive.load(Ordering::Acquire) {
        let result = (|| {
            let attachments = server.attachments()?;
            let mut journal = server.shared_journal()?;
            let mut connection = server.connect_shared_pane()?;
            if connection.worktree_writes() {
                connection.exchange(&Request::WorktreeRead(
                    spaceterm_server::worktree::Read::SeedLocation {
                        template: legacy_location.clone(),
                    },
                ))?;
            }
            // Authoritative reconciliation always precedes a retry of a previously submitted edit.
            let (initial, _) = inventory(&mut connection, &controls)?;
            {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.worktree_writes = connection.worktree_writes();
                s.inventory = Some(initial);
                s.pending = journal.pending().is_some();
                s.pending_operation = journal.pending().map(|p| p.mutation.clone());
                if let Some((sequence, mutation)) = active.as_ref()
                    && !journal
                        .pending()
                        .is_some_and(|p| p.sequence == *sequence && p.mutation == *mutation)
                    && !journal
                        .last()
                        .is_some_and(|p| p.sequence == *sequence && p.mutation == *mutation)
                {
                    s.rejected = Some(mutation.clone());
                    active = None;
                }
            }
            let outcome = finish(&mut connection, &mut journal, &controls)?;
            {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.outcome = outcome;
                s.pending = false;
                s.pending_operation = None;
                active = None;
            }
            loop {
                if !alive.load(Ordering::Acquire) {
                    return Ok(());
                }
                attachments.release_pending(&mut connection)?;
                let (current, sequence) = inventory(&mut connection, &controls)?;
                {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.inventory = Some(current);
                    s.connected = true;
                    s.error = None;
                }
                match commands.recv_timeout(Duration::from_millis(150)) {
                    Ok(mutation) => {
                        active = Some((sequence, mutation.clone()));
                        journal.submit(sequence, mutation)?;
                        let outcome = finish(&mut connection, &mut journal, &controls)?;
                        let (current, _) = inventory(&mut connection, &controls)?;
                        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                        s.inventory = Some(current);
                        s.outcome = outcome;
                        s.pending = false;
                        s.pending_operation = None;
                        active = None;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        })();
        if let Err(error) = result {
            {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.connected = false;
                s.error = Some(error);
            }
            // A command accepted just before loss is either in the durable journal or is rejected.
            // Never carry an unsubmitted edit across a reconnect as a new operation.
            reject_queued(&state, &commands);
            for _ in 0..10 {
                if !alive.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        } else {
            return;
        }
    }
}

fn reject_queued(state: &Mutex<SharedWorkspaceSnapshot>, commands: &mpsc::Receiver<Mutation>) {
    while let Ok(mutation) = commands.try_recv() {
        let mut snapshot = state.lock().unwrap_or_else(|e| e.into_inner());
        snapshot.rejected = Some(mutation);
        snapshot.pending = false;
        snapshot.pending_operation = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disconnect_rejects_an_enqueued_edit_without_carrying_it_across_reconnect() {
        let (commands, receiver) = mpsc::sync_channel(1);
        let state = Arc::new(Mutex::new(SharedWorkspaceSnapshot {
            worktree_writes: true,
            inventory: None,
            connected: true,
            pending: false,
            pending_operation: None,
            rejected: None,
            outcome: None,
            error: None,
        }));
        let client = SharedWorkspaceClient {
            snapshot: state.clone(),
            commands,
            alive: Arc::new(AtomicBool::new(true)),
        };
        let operation = Mutation::Worktree(spaceterm_server::worktree::Operation::SetLocation {
            version: 0,
            template: "~/.worktrees/{repository}/{branch}".into(),
        });
        assert!(client.submit(operation.clone()));
        state.lock().unwrap().connected = false;
        reject_queued(&state, &receiver);
        let snapshot = client.snapshot();
        assert_eq!(snapshot.rejected, Some(operation));
        assert!(!snapshot.pending);
        assert!(snapshot.pending_operation.is_none());
        assert!(
            receiver.try_recv().is_err(),
            "reconnection cannot repeat the refused request"
        );
    }
}
