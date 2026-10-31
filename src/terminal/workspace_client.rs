//! One authenticated inventory subscription and durable edit journal per desktop client.
//!
//! The server pushes each Workspace Inventory revision on the shared link. An edit takes one
//! request: its reply carries the validated outcome after the revision that holds it, and the
//! acknowledgement that retires the outcome needs no reply. Nothing here polls.
use crate::server_connection::{ServerConnection, ServerError};
use crate::terminal::server_session::PaneControls;
use spaceterm_server::{
    attachments::Attachments,
    connection::link::{Health, LinkEvent, LinkSink, ServerLink},
    hierarchy::{ClientJournal, ClientOperation, Inventory, Mutation, RetainedOutcome},
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

/// What the edit loop needs from the owning server. Production reaches it through
/// [`ServerConnection`].
trait EditServer: Send + 'static {
    type Journal: EditJournal;
    /// The shared link, connecting when none is live. `cancelled` ends a wait for a server
    /// operation early.
    fn link(&self, cancelled: &dyn Fn() -> bool) -> Result<ServerLink, ServerError>;
    fn attachments(&self) -> Result<Attachments, ServerError>;
    /// This client's durable journal of Original Operations.
    fn journal(&self) -> Result<Self::Journal, ServerError>;
    /// Delivers the shared link's pushed inventory and health to `sink`, across reconnects.
    fn subscribe(&self, sink: Weak<dyn LinkSink>);
}

/// This client's durable record of the Original Operation in flight, the last one retired, and
/// the retired outcomes it has not presented yet.
trait EditJournal {
    fn pending(&self) -> Option<&ClientOperation>;
    fn last(&self) -> Option<&RetainedOutcome>;
    fn unseen(&self) -> &[RetainedOutcome];
    /// How many unseen outcomes the bound forgot in total, and how many of those the client has
    /// said went unseen.
    fn forgotten(&self) -> (u64, u64);
    fn submit(&mut self, sequence: u64, mutation: Mutation) -> Result<(), ServerError>;
    fn retire(&mut self, result: RetainedOutcome) -> Result<(), ServerError>;
    fn presented(&mut self, sequence: u64) -> Result<(), ServerError>;
    fn forgotten_presented(&mut self, total: u64) -> Result<(), ServerError>;
}

impl EditServer for ServerConnection {
    type Journal = ClientJournal;
    fn link(&self, cancelled: &dyn Fn() -> bool) -> Result<ServerLink, ServerError> {
        ServerConnection::link_cancellable(self, cancelled)
    }
    fn attachments(&self) -> Result<Attachments, ServerError> {
        ServerConnection::attachments(self)
    }
    fn journal(&self) -> Result<ClientJournal, ServerError> {
        self.operation_journal()
    }
    fn subscribe(&self, sink: Weak<dyn LinkSink>) {
        ServerConnection::subscribe(self, sink);
    }
}

impl EditJournal for ClientJournal {
    fn pending(&self) -> Option<&ClientOperation> {
        ClientJournal::pending(self)
    }
    fn last(&self) -> Option<&RetainedOutcome> {
        ClientJournal::last(self)
    }
    fn unseen(&self) -> &[RetainedOutcome] {
        ClientJournal::unseen(self)
    }
    fn forgotten(&self) -> (u64, u64) {
        ClientJournal::forgotten(self)
    }
    fn submit(&mut self, sequence: u64, mutation: Mutation) -> Result<(), ServerError> {
        ClientJournal::submit(self, sequence, mutation)
    }
    fn retire(&mut self, result: RetainedOutcome) -> Result<(), ServerError> {
        ClientJournal::retire(self, result)
    }
    fn presented(&mut self, sequence: u64) -> Result<(), ServerError> {
        ClientJournal::presented(self, sequence)
    }
    fn forgotten_presented(&mut self, total: u64) -> Result<(), ServerError> {
        ClientJournal::forgotten_presented(self, total)
    }
}

/// What wakes the edit loop.
enum Event {
    /// An edit `WorkspaceClient::submit` admitted.
    Edit(Mutation),
    /// The shared link received an inventory revision or ended.
    Link,
    /// A detached Workspace left a release to send.
    Released,
    /// The person asked to reconnect now instead of after the reconnect delay.
    Retry,
    /// A surface presented the outcome of this operation.
    Presented(u64),
    /// A surface said that the first this many forgotten outcomes went unseen.
    ForgottenPresented(u64),
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
pub(crate) struct WorkspaceSnapshot {
    pub inventory: Option<Inventory>,
    pub connected: bool,
    pub pending: bool,
    pub pending_operation: Option<Mutation>,
    pub rejected: Option<Mutation>,
    pub outcome: Option<RetainedOutcome>,
    /// Retired outcomes no surface has presented yet, oldest first, from this run or an earlier
    /// one. Readers present each once from here.
    pub unseen: Vec<RetainedOutcome>,
    /// Presented outcomes the journal has not recorded yet. They never return to `unseen`.
    pub presenting: Vec<u64>,
    /// How many unseen outcomes the journal's bound forgot in total, and how many of those a
    /// surface has said went unseen. A notice is due while the first exceeds the second.
    pub forgotten: u64,
    pub forgotten_presented: u64,
    pub error: Option<ServerError>,
    /// How many connection attempts have failed, so a reader can tell that an attempt it asked
    /// for has finished.
    pub failed_attempts: u64,
}
impl WorkspaceSnapshot {
    /// Publishes the journal's unseen outcomes, except those already presented.
    fn unseen_from(&mut self, journal: &impl EditJournal) {
        self.unseen = journal
            .unseen()
            .iter()
            .filter(|unseen| !self.presenting.contains(&unseen.sequence))
            .cloned()
            .collect();
        let (forgotten, presented) = journal.forgotten();
        self.forgotten = forgotten;
        self.forgotten_presented = self.forgotten_presented.max(presented);
    }
}
pub(crate) struct WorkspaceClient {
    snapshot: Arc<PublishedSnapshot>,
    commands: mpsc::Sender<Event>,
    alive: Arc<AtomicBool>,
}

struct PublishedSnapshot {
    value: Mutex<WorkspaceSnapshot>,
    subscribers: Mutex<Vec<async_channel::Sender<()>>>,
}
impl PublishedSnapshot {
    fn new(value: WorkspaceSnapshot) -> Self {
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
    fn publish<R>(&self, update: impl FnOnce(&mut WorkspaceSnapshot) -> R) -> R {
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
impl Drop for WorkspaceClient {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        let _ = self.commands.send(Event::Stop);
    }
}
impl WorkspaceClient {
    /// Pane control records last only as long as the server lists their Panes.
    pub(crate) fn new(server: ServerConnection, controls: Arc<PaneControls>) -> Self {
        Self::start(server, controls)
    }
    fn start(server: impl EditServer, controls: Arc<PaneControls>) -> Self {
        let snapshot = Arc::new(PublishedSnapshot::new(WorkspaceSnapshot {
            inventory: None,
            connected: false,
            pending: false,
            pending_operation: None,
            rejected: None,
            outcome: None,
            unseen: Vec::new(),
            presenting: Vec::new(),
            forgotten: 0,
            forgotten_presented: 0,
            error: None,
            failed_attempts: 0,
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
            .name("spaceterm-workspaces".into())
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
    pub(crate) fn snapshot(&self) -> WorkspaceSnapshot {
        self.snapshot
            .value
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Reconnects now when the connection is down. A connected client ignores it.
    pub(crate) fn retry(&self) {
        let _ = self.commands.send(Event::Retry);
    }
    /// Records that a surface presented the outcome of `sequence`, so no reader in this process
    /// or a later one presents it again.
    pub(crate) fn presented(&self, sequence: u64) {
        self.snapshot.publish(|state| {
            state.unseen.retain(|unseen| unseen.sequence != sequence);
            if !state.presenting.contains(&sequence) {
                state.presenting.push(sequence);
            }
        });
        let _ = self.commands.send(Event::Presented(sequence));
    }
    /// Records that a surface said the first `total` forgotten outcomes went unseen.
    pub(crate) fn forgotten_presented(&self, total: u64) {
        self.snapshot.publish(|state| {
            state.forgotten_presented = state.forgotten_presented.max(total);
        });
        let _ = self.commands.send(Event::ForgottenPresented(total));
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
    journal: &mut impl EditJournal,
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
        snapshot.unseen_from(&*journal);
    });
    link.acknowledge(pending.sequence)?;
    Ok(FinishedOperation {
        outcome: Some(result),
        reconciled,
    })
}

fn run(
    server: impl EditServer,
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
            // Events before this attempt are stale; the link and its newest revision are read
            // below. Clearing the hint lets the next event wake the loop again.
            wakes.link.store(false, Ordering::Release);
            // Validate the transport before opening this client's retained writers.
            let link = server.link(&|| !alive.load(Ordering::Acquire))?;
            let attachments = server.attachments()?;
            if !watched {
                attachments.watch(Arc::downgrade(&released));
                watched = true;
            }
            let mut journal = server.journal()?;
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
            // Presentations reported while no journal was open are recorded before readers see
            // the journal's unseen outcomes.
            let presenting = state
                .value
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .presenting
                .clone();
            for sequence in presenting {
                journal.presented(sequence)?;
                state.publish(|s| s.presenting.retain(|presenting| *presenting != sequence));
            }
            let forgotten_presented = state
                .value
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .forgotten_presented;
            journal.forgotten_presented(forgotten_presented)?;
            state.publish(|s| {
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
                s.unseen_from(&journal);
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
                    Ok(Event::Retry) => {}
                    Ok(Event::Presented(sequence)) => {
                        journal.presented(sequence)?;
                        state.publish(|s| {
                            s.presenting.retain(|presenting| *presenting != sequence);
                        });
                    }
                    Ok(Event::ForgottenPresented(total)) => journal.forgotten_presented(total)?,
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
            s.failed_attempts = s.failed_attempts.wrapping_add(1);
        });
        // A command accepted just before loss is either in the durable journal or is rejected.
        // Never carry an unsubmitted edit across a reconnect as a new operation.
        let deadline = std::time::Instant::now() + RECONNECT_DELAY;
        while let Some(wait) = deadline.checked_duration_since(std::time::Instant::now()) {
            match events.recv_timeout(wait) {
                Ok(Event::Edit(mutation)) => reject(&state, mutation),
                Ok(Event::Link) => wakes.link.store(false, Ordering::Release),
                Ok(Event::Released) => {}
                // The journal needs no connection. A presentation it cannot record yet is
                // recorded on the next connection.
                Ok(Event::Presented(sequence)) => {
                    if server
                        .journal()
                        .and_then(|mut journal| journal.presented(sequence))
                        .is_ok()
                    {
                        state.publish(|s| {
                            s.presenting.retain(|presenting| *presenting != sequence);
                        });
                    }
                }
                Ok(Event::ForgottenPresented(total)) => {
                    let _ = server
                        .journal()
                        .and_then(|mut journal| journal.forgotten_presented(total));
                }
                Ok(Event::Retry) => break,
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
        let state = Arc::new(PublishedSnapshot::new(WorkspaceSnapshot {
            inventory: None,
            connected: true,
            pending: false,
            pending_operation: None,
            rejected: None,
            outcome: None,
            unseen: Vec::new(),
            presenting: Vec::new(),
            forgotten: 0,
            forgotten_presented: 0,
            error: None,
            failed_attempts: 0,
        }));
        let client = WorkspaceClient {
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
        let state = Arc::new(PublishedSnapshot::new(WorkspaceSnapshot {
            inventory: None,
            connected: true,
            pending: false,
            pending_operation: None,
            rejected: None,
            outcome: None,
            unseen: Vec::new(),
            presenting: Vec::new(),
            forgotten: 0,
            forgotten_presented: 0,
            error: None,
            failed_attempts: 0,
        }));
        let client = WorkspaceClient {
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

    /// An in-process server: the real Workspace host behind real connections, over
    /// in-memory carriers instead of admitted sockets.
    mod served {
        use super::*;
        use spaceterm_server::connection::{
            Wake,
            io::{Waker, serve, spawn_link},
            loopback::Loopback,
            outbox::Outbox,
            server::AdmittedConnection,
            service::{Peer, SharedPaneService},
            sync::SyncWorker,
        };
        use std::time::Instant;

        #[derive(Default)]
        pub(super) struct Record {
            pub pending: Option<ClientOperation>,
            pub last: Option<RetainedOutcome>,
            pub unseen: Vec<RetainedOutcome>,
        }

        /// A journal that keeps its record in memory, shared with the test.
        pub(super) struct MemoryJournal {
            record: Record,
            binding: Arc<Mutex<Record>>,
        }

        impl EditJournal for MemoryJournal {
            fn pending(&self) -> Option<&ClientOperation> {
                self.record.pending.as_ref()
            }
            fn last(&self) -> Option<&RetainedOutcome> {
                self.record.last.as_ref()
            }
            fn unseen(&self) -> &[RetainedOutcome] {
                &self.record.unseen
            }
            fn forgotten(&self) -> (u64, u64) {
                (0, 0)
            }
            fn forgotten_presented(&mut self, _: u64) -> Result<(), ServerError> {
                Ok(())
            }
            fn presented(&mut self, sequence: u64) -> Result<(), ServerError> {
                self.record
                    .unseen
                    .retain(|unseen| unseen.sequence != sequence);
                self.binding
                    .lock()
                    .unwrap()
                    .unseen
                    .retain(|unseen| unseen.sequence != sequence);
                Ok(())
            }
            fn submit(&mut self, sequence: u64, mutation: Mutation) -> Result<(), ServerError> {
                let operation = ClientOperation { sequence, mutation };
                self.record.pending = Some(operation.clone());
                self.binding.lock().unwrap().pending = Some(operation);
                Ok(())
            }
            fn retire(&mut self, result: RetainedOutcome) -> Result<(), ServerError> {
                self.record.pending = None;
                self.record.last = Some(result.clone());
                if result.outcome.needs_presentation() {
                    self.record.unseen.push(result.clone());
                }
                let mut binding = self.binding.lock().unwrap();
                binding.pending = None;
                binding.unseen.clone_from(&self.record.unseen);
                binding.last = Some(result);
                Ok(())
            }
        }

        type Connection = (Arc<AtomicBool>, Arc<dyn Wake>);

        pub(super) struct Server {
            service: SharedPaneService,
            sync: SyncWorker,
            pub client: [u8; 32],
            binding: Arc<Mutex<Option<(ServerLink, Connection)>>>,
            subscribers: Arc<Mutex<Vec<Weak<dyn LinkSink>>>>,
            pub journal: Arc<Mutex<Record>>,
        }

        impl Server {
            pub fn new(client: [u8; 32]) -> Self {
                let (sync, _) = SyncWorker::spawn().unwrap();
                let host = crate::runtime::WorkspaceHost::local(sync.clone()).unwrap();
                Self {
                    service: Arc::new(Mutex::new(Box::new(crate::runtime::Served::new(host)))),
                    sync,
                    client,
                    binding: Arc::default(),
                    subscribers: Arc::default(),
                    journal: Arc::default(),
                }
            }

            pub fn handle(&self) -> Self {
                Self {
                    service: Arc::clone(&self.service),
                    sync: self.sync.clone(),
                    client: self.client,
                    binding: Arc::clone(&self.binding),
                    subscribers: Arc::clone(&self.subscribers),
                    journal: Arc::clone(&self.journal),
                }
            }

            /// A new admitted connection for `client`.
            pub fn connect(
                &self,
                client: [u8; 32],
                events: Arc<dyn LinkSink>,
            ) -> (ServerLink, Connection) {
                let (near, far) = Loopback::pair(1 << 20).unwrap();
                let (waker, wake) = Waker::new().unwrap();
                let outbox = Outbox::new(Arc::clone(&wake));
                let mut connection = [0; 32];
                getrandom::fill(&mut connection).unwrap();
                let peer = Peer {
                    client,
                    connection,
                    pairing: None,
                };
                self.service
                    .lock()
                    .unwrap()
                    .admitted(&peer, "Test".into(), outbox.clone());
                let connection =
                    AdmittedConnection::new(peer, outbox, self.sync.clone(), None, Instant::now());
                let stopping = Arc::new(AtomicBool::new(false));
                let service = Arc::clone(&self.service);
                let stop = Arc::clone(&stopping);
                std::thread::spawn(move || {
                    serve(Box::new(far), connection, &waker, &service, &stop);
                });
                (
                    spawn_link(Box::new(near), events).unwrap(),
                    (stopping, wake),
                )
            }

            /// Ends the shared link from the server's side.
            pub fn end(&self) {
                if let Some((_, (stopping, wake))) = self.binding.lock().unwrap().take() {
                    stopping.store(true, Ordering::Release);
                    wake.wake();
                }
            }
        }

        impl EditServer for Server {
            type Journal = MemoryJournal;
            fn link(&self, _: &dyn Fn() -> bool) -> Result<ServerLink, ServerError> {
                let mut binding = self.binding.lock().unwrap();
                if let Some((link, _)) = binding.as_ref()
                    && link.health() == Health::Live
                {
                    return Ok(link.clone());
                }
                let subscribers = Arc::clone(&self.subscribers);
                let fanout = Arc::new(move |event: LinkEvent| {
                    let sinks: Vec<_> = subscribers
                        .lock()
                        .unwrap()
                        .iter()
                        .filter_map(Weak::upgrade)
                        .collect();
                    for sink in sinks {
                        sink.deliver(match &event {
                            LinkEvent::Inventory(revision) => {
                                LinkEvent::Inventory(Arc::clone(revision))
                            }
                            LinkEvent::Health(health) => LinkEvent::Health(*health),
                        });
                    }
                });
                let (link, connection) = self.connect(self.client, fanout);
                *binding = Some((link.clone(), connection));
                Ok(link)
            }
            fn attachments(&self) -> Result<Attachments, ServerError> {
                Ok(Attachments::memory())
            }
            fn journal(&self) -> Result<MemoryJournal, ServerError> {
                let binding = self.journal.lock().unwrap();
                Ok(MemoryJournal {
                    record: Record {
                        pending: binding.pending.clone(),
                        last: binding.last.clone(),
                        unseen: binding.unseen.clone(),
                    },
                    binding: Arc::clone(&self.journal),
                })
            }
            fn subscribe(&self, sink: Weak<dyn LinkSink>) {
                self.subscribers.lock().unwrap().push(sink);
            }
        }
    }

    fn eventually(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn create(link: &ServerLink, name: &str) -> RetainedOutcome {
        let (_, revision) = link.next_inventory(0).unwrap();
        match link
            .request(Operation::Mutate {
                sequence: revision.next_sequence,
                mutation: Mutation::CreateWorkspace {
                    name: name.into(),
                    directory: None,
                    control: spaceterm_server::hierarchy::CreateControl::Creator,
                },
            })
            .unwrap()
            .wait()
        {
            Ok(ReplyBody::Outcome(outcome)) => outcome,
            _ => panic!("creation unavailable"),
        }
    }

    /// Submits `mutation` through `edits` and waits for its answer.
    fn edit(edits: &WorkspaceClient, mutation: Mutation) -> RetainedOutcome {
        eventually("the client accepts the edit", || {
            edits.submit(mutation.clone())
        });
        let mut answer = None;
        eventually("the edit is answered", || {
            let snapshot = edits.snapshot();
            answer = snapshot
                .outcome
                .filter(|outcome| !snapshot.pending && outcome.mutation == mutation);
            answer.is_some()
        });
        answer.expect("answered")
    }

    /// A rename the server refused waits for draft review while later edits retire. The process
    /// ends before anything presents it, so the next one presents both refusals, and a
    /// presented one never returns.
    #[test]
    fn unseen_outcomes_should_survive_later_edits_and_a_relaunch_until_presented() {
        let client = [7; 32];
        let server = served::Server::new(client);
        let ignore: Arc<dyn LinkSink> = Arc::new(|_: LinkEvent| {});
        let edits = WorkspaceClient::start(server.handle(), Arc::default());
        eventually("the client connects", || edits.snapshot().connected);
        let created = edit(
            &edits,
            Mutation::CreateWorkspace {
                name: "Kept".into(),
                directory: None,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            },
        );
        let spaceterm_server::hierarchy::Outcome::Accepted { target } = created.outcome else {
            panic!("the Workspace is created");
        };
        let rename = |name: &str| Mutation::RenameWorkspace {
            workspace: target,
            version: 0,
            name: name.into(),
        };
        let (other, _) = server.connect([8; 32], Arc::clone(&ignore));
        let (_, revision) = other.next_inventory(0).unwrap();
        other
            .request(Operation::Mutate {
                sequence: revision.next_sequence,
                mutation: rename("Theirs"),
            })
            .unwrap()
            .wait()
            .unwrap();
        let conflict = edit(&edits, rename("Mine"));
        let missing = edit(
            &edits,
            Mutation::CreateTab {
                workspace: [9; 32],
                source: None,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            },
        );
        let published = edits.snapshot().unseen;
        drop(edits);

        let relaunched = WorkspaceClient::start(server.handle(), Arc::default());
        eventually("the relaunched client connects", || {
            relaunched.snapshot().connected
        });
        let recovered = relaunched.snapshot().unseen;
        relaunched.presented(conflict.sequence);
        eventually("the presentation is recorded", || {
            server.journal.lock().unwrap().unseen.len() == 1
        });
        drop(relaunched);
        let later = WorkspaceClient::start(server.handle(), Arc::default());
        eventually("the later client connects", || later.snapshot().connected);

        assert_eq!(
            conflict.outcome,
            spaceterm_server::hierarchy::Outcome::Conflict
        );
        assert_eq!(
            missing.outcome,
            spaceterm_server::hierarchy::Outcome::Missing
        );
        assert_eq!(published, vec![conflict.clone(), missing.clone()]);
        assert_eq!(recovered, vec![conflict, missing.clone()]);
        assert_eq!(later.snapshot().unseen, vec![missing]);
    }

    #[test]
    fn the_edit_loop_repeats_its_acknowledgement_and_keeps_receiving_inventory_after_reconnecting()
    {
        let client = [7; 32];
        let server = served::Server::new(client);
        let ignore: Arc<dyn LinkSink> = Arc::new(|_: LinkEvent| {});
        // An earlier run retired this outcome, but its acknowledgement never arrived.
        let (setup, _) = server.connect(client, Arc::clone(&ignore));
        let lost = create(&setup, "Kept");
        drop(setup);
        server.journal.lock().unwrap().last = Some(lost.clone());

        let edits = WorkspaceClient::start(server.handle(), Arc::default());
        let workspaces = |edits: &WorkspaceClient| {
            edits
                .snapshot()
                .inventory
                .map_or(0, |inventory| inventory.workspaces.len())
        };
        eventually("the client connects", || {
            edits.snapshot().connected && workspaces(&edits) == 1
        });
        let (probe, _) = server.connect(client, Arc::clone(&ignore));
        eventually("the repeated acknowledgement retires the outcome", || {
            matches!(
                probe
                    .request(Operation::Resolve {
                        sequence: lost.sequence
                    })
                    .unwrap()
                    .wait(),
                Err(spaceterm_server::connection::link::RequestError::Refused(
                    spaceterm_server::pane::Refusal::Missing
                ))
            )
        });

        server.end();
        eventually("the client sees the link end", || {
            !edits.snapshot().connected
        });
        // A Pane reconnects the shared link while the edit loop waits to reconnect, so its
        // admission revision arrives during that wait.
        server.link(&|| false).unwrap();
        eventually("the client reconnects", || edits.snapshot().connected);
        let (other, _) = server.connect([8; 32], Arc::clone(&ignore));
        create(&other, "Another client's");
        eventually(
            "another client's edit reaches the reconnected client",
            || workspaces(&edits) == 2,
        );
    }
}
