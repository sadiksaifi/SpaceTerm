//! One authenticated inventory subscription and durable edit journal per desktop client.
use crate::local_server::{LocalServer, ServerError};
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
    pub inventory: Option<Inventory>,
    pub connected: bool,
    pub pending: bool,
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
    pub(crate) fn new(server: LocalServer) -> Self {
        let snapshot = Arc::new(Mutex::new(SharedWorkspaceSnapshot {
            inventory: None,
            connected: false,
            pending: false,
            outcome: None,
            error: None,
        }));
        let alive = Arc::new(AtomicBool::new(true));
        let (commands, receiver) = mpsc::sync_channel(1);
        let state = Arc::clone(&snapshot);
        let running = Arc::clone(&alive);
        if std::thread::Builder::new()
            .name("spaceterm-shared-workspaces".into())
            .spawn(move || run(server, state, receiver, running))
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
        if self.commands.try_send(mutation).is_err() {
            state.pending = false;
            return false;
        }
        true
    }
}
fn inventory(connection: &mut PaneConnection) -> Result<(Inventory, u64), ServerError> {
    let Reply::Inventory {
        inventory,
        next_sequence,
    } = connection.exchange(&Request::Inventory)?
    else {
        return Err(ServerError::Protocol);
    };
    Ok((inventory, next_sequence))
}
fn finish(
    connection: &mut PaneConnection,
    journal: &mut ClientJournal,
) -> Result<Option<RetainedOutcome>, ServerError> {
    let Some(pending) = journal.pending().cloned() else {
        return Ok(journal.last().cloned());
    };
    let result = if let Some(result) = pending.result {
        result
    } else {
        let resolved = match connection.exchange(&Request::Resolve {
            sequence: pending.sequence,
        }) {
            Ok(reply) => reply,
            Err(ServerError::SessionUnavailable) => connection.exchange(&Request::Mutate {
                sequence: pending.sequence,
                mutation: pending.mutation.clone(),
            })?,
            Err(error) => return Err(error),
        };
        let Reply::Outcome(outcome) = resolved else {
            return Err(ServerError::Protocol);
        };
        // Reconcile the accepted membership before retiring the operation identity.
        inventory(connection)?;
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
    state: Arc<Mutex<SharedWorkspaceSnapshot>>,
    commands: mpsc::Receiver<Mutation>,
    alive: Arc<AtomicBool>,
) {
    while alive.load(Ordering::Acquire) {
        let result = (|| {
            let mut journal = server.shared_journal()?;
            let mut connection = server.connect_pane()?;
            // Authoritative reconciliation always precedes a retry of a previously submitted edit.
            let (initial, _) = inventory(&mut connection)?;
            {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.inventory = Some(initial);
                s.pending = journal.pending().is_some();
            }
            let outcome = finish(&mut connection, &mut journal)?;
            {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.outcome = outcome;
                s.pending = false;
            }
            loop {
                if !alive.load(Ordering::Acquire) {
                    return Ok(());
                }
                let (current, sequence) = inventory(&mut connection)?;
                {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.inventory = Some(current);
                    s.connected = true;
                    s.error = None;
                }
                match commands.recv_timeout(Duration::from_millis(150)) {
                    Ok(mutation) => {
                        journal.submit(sequence, mutation)?;
                        let outcome = finish(&mut connection, &mut journal)?;
                        let (current, _) = inventory(&mut connection)?;
                        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                        s.inventory = Some(current);
                        s.outcome = outcome;
                        s.pending = false;
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
            while commands.try_recv().is_ok() {}
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
