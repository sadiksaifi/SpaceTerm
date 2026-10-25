//! One authenticated inventory subscription and durable edit journal per desktop client.
use crate::server_connection::{ServerConnection, ServerError};
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
    pub end_all_sessions: bool,
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
    snapshot: Arc<PublishedSnapshot>,
    commands: mpsc::SyncSender<Mutation>,
    alive: Arc<AtomicBool>,
}

struct PublishedSnapshot {
    value: Mutex<SharedWorkspaceSnapshot>,
    subscribers: Mutex<Vec<async_channel::Sender<()>>>,
}
impl PublishedSnapshot {
    fn new(value: SharedWorkspaceSnapshot) -> Self {
        Self {
            value: Mutex::new(value),
            subscribers: Mutex::default(),
        }
    }
    fn subscribe(&self) -> async_channel::Receiver<()> {
        let (sender, receiver) = async_channel::bounded(1);
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        subscribers.retain(|sender| !sender.is_closed());
        let _ = sender.try_send(());
        subscribers.push(sender);
        receiver
    }
    fn publish<R>(&self, update: impl FnOnce(&mut SharedWorkspaceSnapshot) -> R) -> R {
        let result = {
            let mut value = self.value.lock().unwrap_or_else(|e| e.into_inner());
            update(&mut value)
        };
        // The value is visible before the hint; consumers may immediately read it on another thread.
        self.subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|sender| {
                !matches!(
                    sender.try_send(()),
                    Err(async_channel::TrySendError::Closed(()))
                )
            });
        result
    }
}
impl Drop for SharedWorkspaceClient {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
    }
}
impl SharedWorkspaceClient {
    /// Pane control records last only as long as the server lists their Panes.
    pub(crate) fn new(server: ServerConnection, controls: Arc<PaneControls>) -> Self {
        let snapshot = Arc::new(PublishedSnapshot::new(SharedWorkspaceSnapshot {
            end_all_sessions: false,
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
        if let Err(error) = controls.admission() {
            snapshot.publish(|snapshot| snapshot.error = Some(error));
            alive.store(false, Ordering::Release);
            return Self {
                snapshot,
                commands,
                alive,
            };
        }
        if std::thread::Builder::new()
            .name("spaceterm-shared-workspaces".into())
            .spawn(move || run(server, controls, state, receiver, running))
            .is_err()
        {
            snapshot.publish(|snapshot| snapshot.error = Some(ServerError::StartupFailed));
        }
        Self {
            snapshot,
            commands,
            alive,
        }
    }
    pub(crate) fn supports_worktree_writes(&self) -> bool {
        self.snapshot
            .value
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .worktree_writes
    }
    pub(crate) fn snapshot(&self) -> SharedWorkspaceSnapshot {
        self.snapshot
            .value
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Coalesced change hints, including an initial hint. Read `snapshot` after each wake.
    /// Dropping the receiver unsubscribes; closed senders are pruned on publication or subscription.
    pub(crate) fn subscribe(&self) -> async_channel::Receiver<()> {
        self.snapshot.subscribe()
    }
    /// Availability checks do not copy the hierarchy or terminal directory metadata.
    pub(crate) fn availability(&self) -> (bool, bool) {
        let state = self
            .snapshot
            .value
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        (state.connected, state.pending)
    }
    /// Offline submissions are refused here even if an action's rendered enabled state is stale.
    pub(crate) fn submit(&self, mutation: Mutation) -> bool {
        self.snapshot.publish(|state| {
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
        })
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
struct FinishedOperation {
    outcome: Option<RetainedOutcome>,
    reconciled: Option<(Inventory, u64)>,
}

fn finish(
    connection: &mut PaneConnection,
    journal: &mut ClientJournal,
    controls: &PaneControls,
    state: &PublishedSnapshot,
) -> Result<FinishedOperation, ServerError> {
    let Some(pending) = journal.pending().cloned() else {
        return Ok(FinishedOperation {
            outcome: journal.last().cloned(),
            reconciled: None,
        });
    };
    let mut reconciled = None;
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
        reconciled = Some(inventory(connection, controls)?);
        journal.complete(outcome.clone())?;
        outcome
    };
    // Validation precedes acknowledgement, including when a saved completion resumes after quit.
    if result.sequence != pending.sequence || result.mutation != pending.mutation {
        return Err(ServerError::Protocol);
    }
    // Publish validated membership before acknowledgement. A lost acknowledgement keeps this
    // Original Operation in the journal, while its committed creation can replace a provisional
    // presentation without displaying both identities or waiting through a reconnect.
    state.publish(|snapshot| {
        if let Some((inventory, _)) = &reconciled {
            snapshot.inventory = Some(inventory.clone());
        }
        snapshot.outcome = Some(result.clone());
    });
    connection.exchange(&Request::Acknowledge {
        sequence: pending.sequence,
    })?;
    journal.retire()?;
    Ok(FinishedOperation {
        outcome: Some(result),
        reconciled,
    })
}
fn run(
    server: ServerConnection,
    controls: Arc<PaneControls>,
    state: Arc<PublishedSnapshot>,
    commands: mpsc::Receiver<Mutation>,
    alive: Arc<AtomicBool>,
) {
    let mut active: Option<(u64, Mutation)> = None;
    while alive.load(Ordering::Acquire) {
        let result = (|| {
            // Validate the transport before opening this client's retained writers.
            let mut connection = server.connect_shared_pane()?;
            let attachments = server.attachments()?;
            let mut journal = server.shared_journal()?;
            // Authoritative reconciliation always precedes a retry of a previously submitted edit.
            let (initial, mut sequence) = inventory(&mut connection, &controls)?;
            state.publish(|s| {
                s.worktree_writes = connection.worktree_writes();
                s.end_all_sessions = connection.supports_end_all_sessions();
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
            });
            let finished = finish(&mut connection, &mut journal, &controls, &state)?;
            attachments.release_pending(&mut connection)?;
            state.publish(|s| {
                if let Some((current, next_sequence)) = finished.reconciled {
                    s.inventory = Some(current);
                    sequence = next_sequence;
                }
                s.connected = true;
                s.error = None;
                s.outcome = finished.outcome;
                s.pending = false;
                s.pending_operation = None;
                active = None;
            });
            loop {
                if !alive.load(Ordering::Acquire) {
                    return Ok(());
                }
                attachments.release_pending(&mut connection)?;
                match commands.recv_timeout(Duration::from_millis(150)) {
                    Ok(mutation) => {
                        active = Some((sequence, mutation.clone()));
                        journal.submit(sequence, mutation)?;
                        let finished = finish(&mut connection, &mut journal, &controls, &state)?;
                        let (current, next_sequence) =
                            finished.reconciled.ok_or(ServerError::Protocol)?;
                        sequence = next_sequence;
                        state.publish(|s| {
                            s.inventory = Some(current);
                            s.outcome = finished.outcome;
                            s.pending = false;
                            s.pending_operation = None;
                            active = None;
                        });
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let (current, next_sequence) = inventory(&mut connection, &controls)?;
                        sequence = next_sequence;
                        state.publish(|s| s.inventory = Some(current));
                    }
                }
            }
        })();
        if let Err(error) = result {
            state.publish(|s| {
                s.connected = false;
                s.error = Some(error);
            });
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

fn reject_queued(state: &PublishedSnapshot, commands: &mpsc::Receiver<Mutation>) {
    while let Ok(mutation) = commands.try_recv() {
        state.publish(|snapshot| {
            snapshot.rejected = Some(mutation);
            snapshot.pending = false;
            snapshot.pending_operation = None;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn each_subscriber_wakes_after_a_coalesced_publication_without_holding_state() {
        let (commands, _receiver) = mpsc::sync_channel(1);
        let state = Arc::new(PublishedSnapshot::new(SharedWorkspaceSnapshot {
            end_all_sessions: true,
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
            snapshot: Arc::clone(&state),
            commands,
            alive: Arc::new(AtomicBool::new(true)),
        };
        let first = client.subscribe();
        let second = client.subscribe();
        first.try_recv().unwrap();
        second.try_recv().unwrap();
        state.publish(|snapshot| snapshot.connected = false);
        state.publish(|snapshot| snapshot.error = Some(ServerError::NotRunning));
        for subscriber in [&first, &second] {
            subscriber
                .try_recv()
                .expect("a producer publication must wake every subscriber");
            assert!(subscriber.try_recv().is_err(), "pending hints coalesce");
            let snapshot = client.snapshot();
            assert!(!snapshot.connected);
            assert_eq!(snapshot.error, Some(ServerError::NotRunning));
        }
        drop(second);
        state.publish(|snapshot| snapshot.connected = true);
        first.try_recv().unwrap();
        assert_eq!(state.subscribers.lock().unwrap().len(), 1);
    }

    #[test]
    fn disconnect_rejects_an_enqueued_edit_without_carrying_it_across_reconnect() {
        let (commands, receiver) = mpsc::sync_channel(1);
        let state = Arc::new(PublishedSnapshot::new(SharedWorkspaceSnapshot {
            end_all_sessions: true,
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
        let changes = client.subscribe();
        changes.try_recv().unwrap();
        assert!(client.submit(operation.clone()));
        changes
            .try_recv()
            .expect("submission publishes its pending state");
        assert!(client.snapshot().pending);
        state.publish(|snapshot| snapshot.connected = false);
        reject_queued(&state, &receiver);
        changes
            .try_recv()
            .expect("disconnect and rejection wake the consumer");
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
