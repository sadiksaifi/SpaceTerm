//! One authenticated inventory subscription and durable edit journal per desktop client.
//!
//! The server pushes each Workspace Inventory revision on the shared link. An edit takes one
//! request: its reply carries the validated outcome after the revision that holds it, and the
//! acknowledgement that retires the outcome needs no reply. Nothing here polls.
use crate::server_connection::{ServerConnection, ServerError};
use crate::terminal::server_session::PaneControls;
use spaceterm_server::{
    connection::link::{Health, LinkEvent, LinkSink, ServerLink},
    hierarchy::{ClientJournal, Inventory, Mutation, RetainedOutcome},
    protocol::{InventoryRevision, Operation, ReplyBody},
};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

/// How long the client waits before connecting again after the link ended.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// What wakes the edit loop.
enum Event {
    /// An edit `SharedWorkspaceClient::submit` admitted.
    Edit(Mutation),
    /// The shared link received an inventory revision or ended.
    Link,
    /// A detached Workspace left a release to send.
    Released,
    /// The client is gone.
    Stop,
}

/// Turns the shared link's events and detaches into one wake of the edit loop. Revisions
/// coalesce: the loop reads the newest from the link.
struct Wakes {
    events: Mutex<mpsc::Sender<Event>>,
    link: AtomicBool,
}
impl Wakes {
    fn send(&self, event: Event) {
        let _ = self
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(event);
    }
}
impl LinkSink for Wakes {
    fn deliver(&self, _event: LinkEvent) {
        if !self.link.swap(true, Ordering::AcqRel) {
            self.send(Event::Link);
        }
    }
}

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
    commands: mpsc::Sender<Event>,
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
        let _ = self.commands.send(Event::Stop);
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
        let (commands, receiver) = mpsc::channel();
        let wakes = Arc::new(Wakes {
            events: Mutex::new(commands.clone()),
            link: AtomicBool::new(false),
        });
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
            .spawn(move || run(server, controls, state, receiver, wakes, running))
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
            if self.commands.send(Event::Edit(mutation)).is_err() {
                state.pending = false;
                state.pending_operation = None;
                return false;
            }
            true
        })
    }
}
/// The newest inventory revision on `link`, after the `seen`th, with its Pane control records
/// retained.
fn newer(
    link: &ServerLink,
    seen: u64,
    controls: &PaneControls,
) -> Option<(u64, Arc<InventoryRevision>)> {
    let (count, revision) = link.inventory().filter(|(count, _)| *count > seen)?;
    controls.retain(&revision.inventory);
    Some((count, revision))
}

struct FinishedOperation {
    outcome: Option<RetainedOutcome>,
    reconciled: Option<(u64, Arc<InventoryRevision>)>,
}

/// Dispatches the journal's Original Operation, or does so again with its original identity
/// after a lost reply. The server replays an outcome it already recorded, so a retry never
/// repeats the edit, and its reply waits for any Worktree write the outcome depends on.
fn finish(
    link: &ServerLink,
    journal: &mut ClientJournal,
    controls: &PaneControls,
    state: &PublishedSnapshot,
    seen: u64,
) -> Result<FinishedOperation, ServerError> {
    let Some(pending) = journal.pending().cloned() else {
        return Ok(FinishedOperation {
            outcome: journal.last().cloned(),
            reconciled: None,
        });
    };
    let ReplyBody::Outcome(result) = link
        .request(Operation::Mutate {
            sequence: pending.sequence,
            mutation: pending.mutation.clone(),
        })?
        .wait()?
    else {
        return Err(ServerError::Protocol);
    };
    // Validation precedes acknowledgement.
    if result.sequence != pending.sequence || result.mutation != pending.mutation {
        return Err(ServerError::Protocol);
    }
    // The revision that holds the outcome arrived before its reply. Reconcile that membership
    // before retiring the operation identity.
    let reconciled = newer(link, seen, controls);
    journal.retire(result.clone())?;
    // Publish validated membership before acknowledgement. A lost acknowledgement is repeated
    // after reconnecting, and the committed creation replaces a provisional presentation without
    // displaying both identities or waiting through a reconnect.
    state.publish(|snapshot| {
        if let Some((_, revision)) = &reconciled {
            snapshot.inventory = Some(revision.inventory.clone());
        }
        snapshot.outcome = Some(result.clone());
    });
    link.acknowledge(pending.sequence)?;
    Ok(FinishedOperation {
        outcome: Some(result),
        reconciled,
    })
}

fn run(
    server: ServerConnection,
    controls: Arc<PaneControls>,
    state: Arc<PublishedSnapshot>,
    events: mpsc::Receiver<Event>,
    wakes: Arc<Wakes>,
    alive: Arc<AtomicBool>,
) {
    server.subscribe(Arc::downgrade(&wakes) as Weak<dyn LinkSink>);
    let released: Arc<dyn Fn() + Send + Sync> = Arc::new({
        let wakes = Arc::clone(&wakes);
        move || wakes.send(Event::Released)
    });
    let mut watched = false;
    let mut active: Option<(u64, Mutation)> = None;
    while alive.load(Ordering::Acquire) {
        let result = (|| {
            // Validate the transport before opening this client's retained writers.
            let link = server.link()?;
            let attachments = server.attachments()?;
            if !watched {
                attachments.watch(Arc::downgrade(&released));
                watched = true;
            }
            let mut journal = server.shared_journal()?;
            // Authoritative reconciliation always precedes a retry of a previously submitted
            // edit. The server pushes the inventory when it admits the link.
            let (mut seen, initial) = link.next_inventory(0)?;
            controls.retain(&initial.inventory);
            let mut sequence = initial.next_sequence;
            // The server forgets an outcome only once acknowledged, and nothing confirms that
            // the last acknowledgement arrived. Repeating it is harmless.
            if let Some(last) = journal.last() {
                link.acknowledge(last.sequence)?;
            }
            state.publish(|s| {
                // Protocol major 2 servers write Worktrees and end all Sessions.
                s.worktree_writes = true;
                s.end_all_sessions = true;
                s.inventory = Some(initial.inventory.clone());
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
            let finished = finish(&link, &mut journal, &controls, &state, seen)?;
            attachments.release_pending(&link)?;
            state.publish(|s| {
                if let Some((count, revision)) = finished.reconciled {
                    s.inventory = Some(revision.inventory.clone());
                    sequence = revision.next_sequence;
                    seen = count;
                }
                s.connected = true;
                s.error = None;
                s.outcome = finished.outcome;
                s.pending = false;
                s.pending_operation = None;
                active = None;
            });
            loop {
                match events.recv() {
                    Ok(Event::Edit(mutation)) => {
                        active = Some((sequence, mutation.clone()));
                        journal.submit(sequence, mutation)?;
                        let finished = finish(&link, &mut journal, &controls, &state, seen)?;
                        state.publish(|s| {
                            if let Some((count, revision)) = finished.reconciled {
                                s.inventory = Some(revision.inventory.clone());
                                sequence = revision.next_sequence;
                                seen = count;
                            }
                            s.outcome = finished.outcome;
                            s.pending = false;
                            s.pending_operation = None;
                            active = None;
                        });
                    }
                    Ok(Event::Link) => {
                        wakes.link.store(false, Ordering::Release);
                        if let Health::Closed(error) = link.health() {
                            return Err(error.into());
                        }
                        if let Some((count, revision)) = newer(&link, seen, &controls) {
                            seen = count;
                            sequence = revision.next_sequence;
                            state.publish(|s| s.inventory = Some(revision.inventory.clone()));
                        }
                    }
                    Ok(Event::Released) => attachments.release_pending(&link)?,
                    Ok(Event::Stop) | Err(_) => return Ok(()),
                }
            }
        })();
        let Err(error) = result else {
            return;
        };
        state.publish(|s| {
            s.connected = false;
            s.error = Some(error);
        });
        // A command accepted just before loss is either in the durable journal or is rejected.
        // Never carry an unsubmitted edit across a reconnect as a new operation.
        let deadline = std::time::Instant::now() + RECONNECT_DELAY;
        while let Some(wait) = deadline.checked_duration_since(std::time::Instant::now()) {
            match events.recv_timeout(wait) {
                Ok(Event::Edit(mutation)) => reject(&state, mutation),
                Ok(Event::Link | Event::Released) => {}
                Ok(Event::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => break,
            }
        }
    }
}

fn reject(state: &PublishedSnapshot, mutation: Mutation) {
    state.publish(|snapshot| {
        snapshot.rejected = Some(mutation);
        snapshot.pending = false;
        snapshot.pending_operation = None;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn each_subscriber_wakes_after_a_coalesced_publication_without_holding_state() {
        let (commands, _receiver) = mpsc::channel();
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
        let (commands, receiver) = mpsc::channel();
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
        let Ok(Event::Edit(queued)) = receiver.try_recv() else {
            panic!("the submission reaches the loop");
        };
        reject(&state, queued);
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
