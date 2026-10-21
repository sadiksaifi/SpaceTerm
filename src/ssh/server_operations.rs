//! The desktop's submitted remote maintenance operations and what survives interruption.
//!
//! One owner holds every remote service and update operation this client submitted, together with
//! the service facts its last validated read found. The Servers surface reads a snapshot, so
//! rendering asks no computer anything and construction reaches no host.
//!
//! A submitted operation outlives the attempt that submitted it. It survives the server being
//! stopped or unreachable, the desktop quitting or being killed, and removal of its saved
//! connection: removal ends presentation and ordinary reconnect intent, while the operation's own
//! record stays until its outcome is validated and the person acknowledges it. Recovery resolves
//! that record alone. It never recreates the removed connection, writes saved connection intent,
//! or lets a connection saved again under the same destination inherit another connection's
//! pending original.
//!
//! The person never handles an operation identifier, a confirmation token or a helper lifetime.
//! They choose an action, confirm what it affects, and later see its validated result or a
//! truthful unconfirmed outcome.

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use spaceterm_server::service::maintenance::{
    Action, MaintenanceFacts, OperationKind, OperationOutcome, OperationResolution, PreparedChange,
};

use super::cancellation::SshCancellationToken;
use super::server_connection::{
    ContinuityChange, InteractionReason, SshConnectionRemovals, SshIntentStoreError,
    SshServerConnections, UnavailableKind, wait,
};
use super::server_maintenance::{
    MaintenanceAuthority, MaintenancePhase, MaintenanceTarget, OperationSubmission,
    SshMaintenanceBackend, SshMaintenanceFailure, SubmittedKind,
};
use crate::domain::SshDestination;

/// What the person asked for. Start ends no work, so it needs no confirmation; the others
/// describe their effects and wait for one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteMaintenanceRequest {
    Service(Action),
    /// The exact verified artifact this client holds for that machine.
    Update,
    /// A return to the previous runtime of an update that never became ready.
    Return,
}

/// Names one described change or one validated result inside the owner. It is an opaque handle,
/// so no surface composes an operation identity or a confirmation token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RemoteOperationHandle(pub(crate) u64);

/// One remote server's maintenance state, derived from what the owner already knows.
#[derive(Clone, Debug)]
pub(crate) enum RemoteOperationState {
    /// Nothing has been read for this server and nothing is submitted.
    Unread,
    /// A read, description, submission or resolution is underway. `kind` names the submitted
    /// operation it belongs to, when one is submitted.
    Working {
        phase: MaintenancePhase,
        kind: Option<SubmittedKind>,
        cancellable: bool,
    },
    /// The service facts the last validated read found.
    Read(Box<MaintenanceFacts>),
    /// A change the service described for one exact observation, awaiting confirmation. Only
    /// confirming this handle performs it, once.
    Described {
        handle: RemoteOperationHandle,
        request: RemoteMaintenanceRequest,
        change: Box<PreparedChange>,
    },
    /// A submitted original whose outcome the service validated, awaiting acknowledgement. The
    /// outcome is the retained form, so a result read before relaunch presents what it presented
    /// then without inventing facts this client never read.
    Resolved {
        handle: RemoteOperationHandle,
        kind: SubmittedKind,
        outcome: RetainedOutcome,
    },
    /// A submitted original whose result could not be validated. It stays unconfirmed until
    /// resolving the same original settles it; nothing here repeats it.
    Unconfirmed { kind: SubmittedKind },
    /// Continuation needs the person. Background work never asks on its own.
    NeedsPerson {
        reason: InteractionReason,
        kind: Option<SubmittedKind>,
    },
    /// The route, account, application identity or server is not the one the original bound, so
    /// continuation paused without sending anything.
    Paused {
        reason: ContinuityChange,
        kind: Option<SubmittedKind>,
    },
    /// The last attempt reached no validated result.
    Failed {
        failure: SshMaintenanceFailure,
        kind: Option<SubmittedKind>,
    },
}

/// One server the Servers surface presents maintenance for.
#[derive(Clone, Debug)]
pub(crate) struct RemoteOperationEntry {
    /// The destination the original bound, which names the machine.
    pub destination: SshDestination,
    /// Whether its saved connection was removed while this operation was unresolved. Such an
    /// entry offers only resolving and acknowledging the original, never reconnecting.
    pub removed: bool,
    pub state: RemoteOperationState,
}

/// What the Servers surface asks of the owner. Reading never reaches a host; every other method
/// hands work to the owner's worker and returns at once.
pub(crate) trait RemoteServerOperations: Send + Sync {
    /// Signals every state change, so a surface repaints without polling.
    fn subscribe(&self) -> async_channel::Receiver<()>;

    /// Every server with maintenance state, including one whose saved connection was removed,
    /// ordered by destination.
    fn entries(&self) -> Vec<RemoteOperationEntry>;

    /// One server's state, or [`RemoteOperationState::Unread`] when it has none.
    fn state(&self, destination: &SshDestination) -> RemoteOperationState;

    /// Reads the service facts on the bound route, with the authority the person just granted by
    /// asking. Starts, stages, installs and stops nothing.
    fn refresh(&self, destination: &SshDestination);

    /// Describes `request` without performing it, then waits for a confirmation.
    fn describe(&self, destination: &SshDestination, request: RemoteMaintenanceRequest);

    /// Performs exactly the described change this handle names, once. A changed observation
    /// refuses and asks for a new confirmation instead of acting.
    fn confirm(&self, handle: RemoteOperationHandle);

    /// Starts the server explicitly. This is the only remote operation that reverses a deliberate
    /// Stop, and it ends no work, so it needs no confirmation.
    fn start(&self, destination: &SshDestination);

    /// Abandons a described change, or stops waiting for the work underway. Cancelling does not
    /// establish that effects the service already admitted were undone.
    fn cancel(&self, destination: &SshDestination);

    /// Resolves an unresolved original the person asks about, acquiring fresh query authority on
    /// its original route. It repeats no dispatch and renews no approval.
    fn resume(&self, destination: &SshDestination);

    /// Retires a validated result the person acknowledged, and its retained record with it.
    fn acknowledge(&self, handle: RemoteOperationHandle);

    /// Continues unresolved originals quietly after relaunch, using available noninteractive
    /// authority only. Called once, after a Workspace Window exists.
    fn resume_retained(&self);
}

/// One submitted operation as it survives relaunch and saved-connection removal. It holds only
/// what resolving the same original needs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RetainedRemoteOperation {
    pub target: MaintenanceTarget,
    /// The client that submitted it. A stable nonsecret identifier that grants no authority.
    pub client: [u8; 32],
    pub operation: [u8; 32],
    pub kind: SubmittedKind,
    /// The validated outcome awaiting acknowledgement, or `None` while it is unresolved.
    pub outcome: Option<RetainedOutcome>,
    /// Whether its saved connection has been removed.
    pub removed: bool,
}

/// A validated outcome as it survives relaunch, holding what a person must still be told.
///
/// The service's own reply cannot be kept: it names a run that has moved on. What is kept is the
/// state the service validated and, for an update or a return, the build it validated as active,
/// so the same sentence reads the same after the desktop reopens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RetainedOutcome {
    /// The service performed it and validated the result.
    Completed {
        /// The service policy observed after it resolved, when that could be read.
        status: Option<spaceterm_server::service::ServiceStatus>,
        /// The build the server runs after an update or a return, as the service named it.
        build: Option<spaceterm_server::service::maintenance::RemoteBuild>,
    },
    Refused(spaceterm_server::service::maintenance::MaintenanceRefusal),
    Failed {
        can_return: bool,
    },
}

/// Private retained operation records, separate from saved connections and shipped Settings.
pub(crate) trait RemoteOperationStore: Send + Sync {
    fn load(&self) -> Result<Vec<RetainedRemoteOperation>, SshIntentStoreError>;
    fn retain(&self, operation: RetainedRemoteOperation) -> Result<(), SshIntentStoreError>;
    fn retire(&self, operation: [u8; 32]) -> Result<(), SshIntentStoreError>;
    /// The nonsecret identifier this client records on the operations it submits, created on
    /// first use and retained afterwards.
    fn client(&self) -> Result<[u8; 32], SshIntentStoreError>;
}

/// The desktop's owner, or `None` in a build or test that installs none.
pub(crate) fn installed(cx: &gpui::App) -> Option<Arc<dyn RemoteServerOperations>> {
    cx.try_global::<InstalledRemoteOperations>()
        .map(|installed| Arc::clone(&installed.0))
}

/// Makes `operations` the desktop's owner.
pub(crate) fn install(operations: Arc<dyn RemoteServerOperations>, cx: &mut gpui::App) {
    cx.set_global(InstalledRemoteOperations(operations));
}

struct InstalledRemoteOperations(Arc<dyn RemoteServerOperations>);

impl gpui::Global for InstalledRemoteOperations {}

/// How long resolution waits before reading an admitted original again. Reading an original is
/// not a dispatch, so repeating the read renews no approval and performs no work.
fn resolve_delay(attempt: u32) -> Duration {
    Duration::from_secs(1_u64 << attempt.min(5))
}
/// How many times one exchange reads an admitted original before leaving it unconfirmed.
const RESOLVE_READS: u32 = 8;

/// The original one entry submitted. Its presence means the dispatch left this client, so
/// nothing may later claim its effects were undone.
#[derive(Clone)]
struct Submitted {
    operation: [u8; 32],
    kind: SubmittedKind,
}

/// One destination's maintenance state. An entry whose saved connection was removed is sealed:
/// it resolves and acknowledges its own original and admits no new work.
struct Entry {
    destination: SshDestination,
    state: RemoteOperationState,
    /// The route, account, application identity and server every exchange for this entry
    /// validates. A removed entry keeps the one its original bound.
    target: Option<MaintenanceTarget>,
    /// The facts the last validated read found, which a description needing no change returns to.
    facts: Option<MaintenanceFacts>,
    submitted: Option<Submitted>,
    removed: bool,
    cancellation: SshCancellationToken,
    generation: u64,
    running: bool,
}

/// What one admitted exchange does on the bound route.
enum Work {
    /// Reads the service facts.
    Read,
    /// Reads the facts, then describes one change for exactly those facts.
    Describe(RemoteMaintenanceRequest),
    /// Submits one bound original, then reads its validated outcome.
    Submit(OperationSubmission),
    /// Reads an already-submitted original's validated outcome.
    Resolve([u8; 32]),
}

/// One exchange this owner admitted. Its generation fences every later report, so a result from
/// a removed or superseded exchange lands nowhere.
struct Admitted {
    destination: SshDestination,
    target: MaintenanceTarget,
    authority: MaintenanceAuthority,
    kind: Option<SubmittedKind>,
    generation: u64,
    cancellation: SshCancellationToken,
}

struct State {
    entries: Vec<Entry>,
    subscribers: Vec<async_channel::Sender<()>>,
    next_handle: u64,
    /// Generations come from one counter, so a removed connection's late work never matches a
    /// connection saved again under the same destination.
    next_generation: u64,
    /// This client's retained identifier, read once and kept for the process.
    client: Option<[u8; 32]>,
    loaded: bool,
    loading: bool,
    persistence_error: Option<SshIntentStoreError>,
}

struct Inner {
    state: Mutex<State>,
    backend: Arc<dyn SshMaintenanceBackend>,
    store: Arc<dyn RemoteOperationStore>,
    connections: SshServerConnections,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(state) = self.state.lock() {
            for entry in &state.entries {
                entry.cancellation.cancel();
            }
        }
    }
}

/// The desktop's one owner of submitted remote maintenance operations. Constructing it reads
/// nothing and reaches no host, and reading its state asks no computer anything.
pub(crate) struct SshServerOperations {
    inner: Arc<Inner>,
}

impl SshServerOperations {
    /// Takes the exchanges, the retained records and the saved connections whose admitted
    /// identities an operation binds, and watches removal of those connections.
    pub(crate) fn new(
        backend: Arc<dyn SshMaintenanceBackend>,
        store: Arc<dyn RemoteOperationStore>,
        connections: SshServerConnections,
    ) -> Arc<Self> {
        let owner = Arc::new(Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    entries: Vec::new(),
                    subscribers: Vec::new(),
                    next_handle: 0,
                    next_generation: 0,
                    client: None,
                    loaded: false,
                    loading: false,
                    persistence_error: None,
                }),
                backend,
                store,
                connections,
            }),
        });
        let observer: Arc<dyn SshConnectionRemovals> = owner.clone();
        owner.inner.connections.watch_removals(&observer);
        owner
    }

    /// Finds the entry that admits new work for `destination`, creating it when the registry
    /// knows the identity an exchange must bind. A removed entry admits no new work, and an
    /// identity this client has never validated asks the person to connect instead of guessing.
    fn admitting(&self, state: &mut State, destination: &SshDestination) -> Option<usize> {
        let existing = state
            .entries
            .iter()
            .position(|entry| entry.destination == *destination && !entry.removed);
        let index = match existing {
            Some(index) => index,
            None => {
                state.entries.push(Entry {
                    destination: destination.clone(),
                    state: RemoteOperationState::Unread,
                    target: None,
                    facts: None,
                    submitted: None,
                    removed: false,
                    cancellation: SshCancellationToken::default(),
                    generation: 0,
                    running: false,
                });
                state.entries.len() - 1
            }
        };
        if state.entries[index].target.is_none() {
            state.entries[index].target =
                self.inner
                    .connections
                    .continuity(destination)
                    .map(|continuity| MaintenanceTarget {
                        destination: destination.clone(),
                        continuity,
                    });
        }
        if state.entries[index].target.is_none() {
            state.entries[index].state = RemoteOperationState::NeedsPerson {
                reason: InteractionReason::Authentication,
                kind: None,
            };
            notify(state);
            return None;
        }
        Some(index)
    }

    fn spawn(&self, admitted: Admitted, work: Work) {
        let inner = Arc::downgrade(&self.inner);
        let backend = Arc::clone(&self.inner.backend);
        let store = Arc::clone(&self.inner.store);
        let destination = admitted.destination.clone();
        let generation = admitted.generation;
        if std::thread::Builder::new()
            .name("spaceterm-ssh-operation".into())
            .spawn(move || run(inner, backend, store, admitted, work))
            .is_err()
        {
            settle(
                &Arc::downgrade(&self.inner),
                &destination,
                generation,
                SshMaintenanceFailure::Unavailable(UnavailableKind::ServiceUnavailable),
            );
        }
    }

    /// Admits one exchange on an entry that holds an unresolved original.
    fn resolving(&self, destination: &SshDestination, authority: MaintenanceAuthority) {
        let admitted = {
            let Ok(mut state) = self.inner.state.lock() else {
                return;
            };
            let unresolved: Vec<_> = state
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| {
                    entry.destination == *destination
                        && entry.submitted.is_some()
                        && !matches!(entry.state, RemoteOperationState::Resolved { .. })
                })
                .map(|(index, _)| index)
                .collect();
            unresolved
                .into_iter()
                .filter_map(|index| {
                    let submitted = state.entries[index].submitted.clone()?;
                    let admitted = admit(
                        &mut state,
                        index,
                        MaintenancePhase::Resolving,
                        authority,
                        Some(submitted.kind),
                    )?;
                    let operation = submitted.operation;
                    Some((admitted, operation))
                })
                .collect::<Vec<_>>()
        };
        for (admitted, operation) in admitted {
            self.spawn(admitted, Work::Resolve(operation));
        }
    }
}

impl RemoteServerOperations for SshServerOperations {
    fn subscribe(&self) -> async_channel::Receiver<()> {
        let (sender, receiver) = async_channel::bounded(1);
        if let Ok(mut state) = self.inner.state.lock() {
            state.subscribers.push(sender);
        }
        receiver
    }

    fn entries(&self) -> Vec<RemoteOperationEntry> {
        let mut entries: Vec<_> = self
            .inner
            .state
            .lock()
            .ok()
            .map(|state| {
                state
                    .entries
                    .iter()
                    .map(|entry| RemoteOperationEntry {
                        destination: entry.destination.clone(),
                        removed: entry.removed,
                        state: entry.state.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by(|left, right| left.destination.cmp(&right.destination));
        entries
    }

    fn state(&self, destination: &SshDestination) -> RemoteOperationState {
        self.inner
            .state
            .lock()
            .ok()
            .and_then(|state| {
                state
                    .entries
                    .iter()
                    .find(|entry| entry.destination == *destination && !entry.removed)
                    .or_else(|| {
                        state
                            .entries
                            .iter()
                            .find(|entry| entry.destination == *destination)
                    })
                    .map(|entry| entry.state.clone())
            })
            .unwrap_or(RemoteOperationState::Unread)
    }

    fn refresh(&self, destination: &SshDestination) {
        let admitted = {
            let Ok(mut state) = self.inner.state.lock() else {
                return;
            };
            let Some(index) = self.admitting(&mut state, destination) else {
                return;
            };
            admit(
                &mut state,
                index,
                MaintenancePhase::Authenticating,
                MaintenanceAuthority::Requested,
                None,
            )
        };
        if let Some(admitted) = admitted {
            self.spawn(admitted, Work::Read);
        }
    }

    fn describe(&self, destination: &SshDestination, request: RemoteMaintenanceRequest) {
        let admitted = {
            let Ok(mut state) = self.inner.state.lock() else {
                return;
            };
            let Some(index) = self.admitting(&mut state, destination) else {
                return;
            };
            admit(
                &mut state,
                index,
                MaintenancePhase::Authenticating,
                MaintenanceAuthority::Requested,
                None,
            )
        };
        if let Some(admitted) = admitted {
            self.spawn(admitted, Work::Describe(request));
        }
    }

    fn confirm(&self, handle: RemoteOperationHandle) {
        let admitted = {
            let Ok(mut state) = self.inner.state.lock() else {
                return;
            };
            let Some(index) = state.entries.iter().position(|entry| {
                matches!(entry.state, RemoteOperationState::Described { handle: named, .. }
                    if named == handle)
            }) else {
                return;
            };
            let RemoteOperationState::Described {
                request, change, ..
            } = state.entries[index].state.clone()
            else {
                return;
            };
            let Some(kind) = submitted_kind(request, &change) else {
                // Without a named artifact nothing identifies what activation would install.
                state.entries[index].state = RemoteOperationState::Failed {
                    failure: SshMaintenanceFailure::Refused(
                        spaceterm_server::service::maintenance::MaintenanceRefusal::CandidateUnavailable,
                    ),
                    kind: None,
                };
                notify(&mut state);
                return;
            };
            let Some(submission) = bind(kind, Some(change.confirmation.clone())) else {
                state.entries[index].state = RemoteOperationState::Failed {
                    failure: SshMaintenanceFailure::Unavailable(
                        UnavailableKind::ServiceUnavailable,
                    ),
                    kind: None,
                };
                notify(&mut state);
                return;
            };
            start_submission(&mut state, index, submission)
        };
        if let Some((admitted, submission)) = admitted {
            self.spawn(admitted, Work::Submit(submission));
        }
    }

    fn start(&self, destination: &SshDestination) {
        let admitted = {
            let Ok(mut state) = self.inner.state.lock() else {
                return;
            };
            let Some(index) = self.admitting(&mut state, destination) else {
                return;
            };
            // Start ends no work, so the service needs no exact-observation confirmation for it.
            let Some(submission) = bind(SubmittedKind::Start, None) else {
                return;
            };
            start_submission(&mut state, index, submission)
        };
        if let Some((admitted, submission)) = admitted {
            self.spawn(admitted, Work::Submit(submission));
        }
    }

    fn cancel(&self, destination: &SshDestination) {
        let Ok(mut state) = self.inner.state.lock() else {
            return;
        };
        for entry in state
            .entries
            .iter_mut()
            .filter(|entry| entry.destination == *destination)
        {
            entry.cancellation.cancel();
            match &entry.state {
                RemoteOperationState::Working { phase, kind, .. } => {
                    entry.state = RemoteOperationState::Working {
                        phase: *phase,
                        kind: kind.clone(),
                        cancellable: false,
                    };
                }
                // Abandoning a described change returns to the observation it described.
                RemoteOperationState::Described { .. } => {
                    entry.state = entry
                        .facts
                        .clone()
                        .map_or(RemoteOperationState::Unread, |facts| {
                            RemoteOperationState::Read(Box::new(facts))
                        });
                }
                _ => {}
            }
        }
        notify(&mut state);
    }

    fn resume(&self, destination: &SshDestination) {
        self.resolving(destination, MaintenanceAuthority::Requested);
    }

    fn acknowledge(&self, handle: RemoteOperationHandle) {
        let acknowledged = {
            let Ok(state) = self.inner.state.lock() else {
                return;
            };
            state
                .entries
                .iter()
                .find(|entry| {
                    matches!(entry.state, RemoteOperationState::Resolved { handle: named, .. }
                        if named == handle)
                })
                .and_then(|entry| {
                    Some((
                        entry.destination.clone(),
                        entry.generation,
                        entry.submitted.as_ref()?.operation,
                    ))
                })
        };
        let Some((destination, generation, operation)) = acknowledged else {
            return;
        };
        let owner = Arc::downgrade(&self.inner);
        let inner = Arc::downgrade(&self.inner);
        let store = Arc::clone(&self.inner.store);
        if std::thread::Builder::new()
            .name("spaceterm-ssh-operation".into())
            .spawn(move || match store.retire(operation) {
                // The result is retired with its record, so relaunch never presents it again.
                Ok(()) => {
                    update(&inner, &destination, generation, |state, index| {
                        let entry = &mut state.entries[index];
                        entry.submitted = None;
                        if entry.removed {
                            state.entries.remove(index);
                        } else {
                            entry.state = entry
                                .facts
                                .clone()
                                .map_or(RemoteOperationState::Unread, |facts| {
                                    RemoteOperationState::Read(Box::new(facts))
                                });
                        }
                    });
                }
                // An unretired record still awaits acknowledgement, so the result stays.
                Err(error) => note_storage(&inner, Some(error)),
            })
            .is_err()
        {
            note_storage(&owner, Some(SshIntentStoreError::Unavailable));
        }
    }

    fn resume_retained(&self) {
        {
            let Ok(mut state) = self.inner.state.lock() else {
                return;
            };
            if state.loaded || state.loading {
                return;
            }
            state.loading = true;
        }
        let owner = Self {
            inner: Arc::clone(&self.inner),
        };
        if std::thread::Builder::new()
            .name("spaceterm-ssh-operation".into())
            .spawn(move || owner.load())
            .is_err()
        {
            if let Ok(mut state) = self.inner.state.lock() {
                state.loading = false;
            }
            note_storage(
                &Arc::downgrade(&self.inner),
                Some(SshIntentStoreError::Unavailable),
            );
        }
    }
}

impl SshServerOperations {
    /// Reads the retained records once a Workspace Window exists and continues each unresolved
    /// original quietly. A read that fails is tried again by a later call.
    fn load(&self) {
        let records = match self.inner.store.load() {
            Ok(records) => records,
            Err(error) => {
                if let Ok(mut state) = self.inner.state.lock() {
                    state.loading = false;
                }
                note_storage(&Arc::downgrade(&self.inner), Some(error));
                return;
            }
        };
        if let Ok(mut state) = self.inner.state.lock() {
            state.loaded = true;
            state.loading = false;
        }
        for record in records {
            let admitted = {
                let Ok(mut state) = self.inner.state.lock() else {
                    continue;
                };
                adopt(&mut state, record)
            };
            if let Some((admitted, operation)) = admitted {
                self.spawn(admitted, Work::Resolve(operation));
            }
        }
    }
}

/// Takes one retained record into an entry and admits its quiet continuation. A record with a
/// validated outcome needs no exchange: it waits for acknowledgement.
fn adopt(state: &mut State, record: RetainedRemoteOperation) -> Option<(Admitted, [u8; 32])> {
    // One live entry presents a destination, so a record for a connection still saved joins it.
    let existing = state.entries.iter().position(|entry| {
        entry.destination == record.target.destination
            && entry.submitted.is_none()
            && entry.removed == record.removed
            && !entry.running
    });
    let index = match existing {
        Some(index) => index,
        None => {
            state.entries.push(Entry {
                destination: record.target.destination.clone(),
                state: RemoteOperationState::Unread,
                target: None,
                facts: None,
                submitted: None,
                removed: record.removed,
                cancellation: SshCancellationToken::default(),
                generation: 0,
                running: false,
            });
            state.entries.len() - 1
        }
    };
    let handle = next_handle(state);
    // Every entry owns a distinct generation, so one destination's two records never report
    // each other's results.
    let generation = next_generation(state);
    let entry = &mut state.entries[index];
    entry.generation = generation;
    entry.removed = record.removed;
    entry.target = Some(record.target);
    entry.submitted = Some(Submitted {
        operation: record.operation,
        kind: record.kind.clone(),
    });
    let kind = record.kind;
    if let Some(outcome) = record.outcome {
        entry.state = RemoteOperationState::Resolved {
            handle,
            kind,
            outcome,
        };
        notify(state);
        return None;
    }
    let admitted = admit(
        state,
        index,
        MaintenancePhase::Resolving,
        MaintenanceAuthority::Quiet,
        Some(kind),
    )?;
    Some((admitted, record.operation))
}

/// Binds one submission: the identity the service recognizes, what it asks for, and the
/// exact-observation confirmation that authorizes it.
fn bind(kind: SubmittedKind, confirmation: Option<String>) -> Option<OperationSubmission> {
    let mut operation = [0; 32];
    getrandom::fill(&mut operation).ok()?;
    Some(OperationSubmission {
        operation,
        kind,
        confirmation,
    })
}

/// Admits one dispatch and records its original. An entry that already runs an exchange admits
/// none, so nothing replaces the original an exchange underway is settling.
fn start_submission(
    state: &mut State,
    index: usize,
    submission: OperationSubmission,
) -> Option<(Admitted, OperationSubmission)> {
    let admitted = admit(
        state,
        index,
        MaintenancePhase::Dispatching,
        MaintenanceAuthority::Requested,
        Some(submission.kind.clone()),
    )?;
    // The record's presence means the dispatch left this client, so only reading the same
    // original can settle it.
    state.entries[index].submitted = Some(Submitted {
        operation: submission.operation,
        kind: submission.kind.clone(),
    });
    Some((admitted, submission))
}

/// What confirming a described change submits. An update names the exact artifact the service
/// described, so a later approved build cannot be activated by this confirmation.
fn submitted_kind(
    request: RemoteMaintenanceRequest,
    change: &PreparedChange,
) -> Option<SubmittedKind> {
    match request {
        RemoteMaintenanceRequest::Service(action) => Some(SubmittedKind::Service(action)),
        RemoteMaintenanceRequest::Update => Some(SubmittedKind::Update {
            candidate: change.candidate.as_ref()?.digest.clone(),
        }),
        RemoteMaintenanceRequest::Return => Some(SubmittedKind::Return),
    }
}

/// Admits one exchange on `index`, refusing while another runs for the same entry.
fn admit(
    state: &mut State,
    index: usize,
    phase: MaintenancePhase,
    authority: MaintenanceAuthority,
    kind: Option<SubmittedKind>,
) -> Option<Admitted> {
    if state.entries[index].running {
        return None;
    }
    let generation = next_generation(state);
    let entry = &mut state.entries[index];
    entry.generation = generation;
    entry.cancellation = SshCancellationToken::default();
    entry.running = true;
    entry.state = RemoteOperationState::Working {
        phase,
        kind: kind.clone(),
        cancellable: true,
    };
    let admitted = Admitted {
        destination: entry.destination.clone(),
        target: entry.target.clone()?,
        authority,
        kind,
        generation,
        cancellation: entry.cancellation.clone(),
    };
    notify(state);
    Some(admitted)
}

fn next_generation(state: &mut State) -> u64 {
    state.next_generation = state.next_generation.saturating_add(1);
    state.next_generation
}

fn next_handle(state: &mut State) -> RemoteOperationHandle {
    state.next_handle = state.next_handle.saturating_add(1);
    RemoteOperationHandle(state.next_handle)
}

fn notify(state: &mut State) {
    state
        .subscribers
        .retain(|sender| match sender.try_send(()) {
            Ok(()) | Err(async_channel::TrySendError::Full(_)) => true,
            Err(async_channel::TrySendError::Closed(_)) => false,
        });
}

/// Applies `change` to the entry the admitted exchange owns, or reports that it owns none.
fn update(
    inner: &Weak<Inner>,
    destination: &SshDestination,
    generation: u64,
    change: impl FnOnce(&mut State, usize),
) -> bool {
    let Some(owner) = inner.upgrade() else {
        return false;
    };
    let Ok(mut state) = owner.state.lock() else {
        return false;
    };
    let Some(index) = state
        .entries
        .iter()
        .position(|entry| entry.destination == *destination && entry.generation == generation)
    else {
        return false;
    };
    change(&mut state, index);
    notify(&mut state);
    true
}

fn note_storage(inner: &Weak<Inner>, error: Option<SshIntentStoreError>) {
    let Some(owner) = inner.upgrade() else {
        return;
    };
    if let Ok(mut state) = owner.state.lock()
        && state.persistence_error != error
    {
        state.persistence_error = error;
        notify(&mut state);
    }
}

fn run(
    inner: Weak<Inner>,
    backend: Arc<dyn SshMaintenanceBackend>,
    store: Arc<dyn RemoteOperationStore>,
    admitted: Admitted,
    work: Work,
) {
    let Admitted {
        destination,
        target,
        authority,
        kind,
        generation,
        cancellation,
    } = admitted;
    let progress = |phase| {
        if !cancellation.is_cancelled() {
            update(&inner, &destination, generation, |state, index| {
                state.entries[index].state = RemoteOperationState::Working {
                    phase,
                    kind: kind.clone(),
                    cancellable: true,
                };
            });
        }
    };
    match work {
        Work::Read => match backend.observe(&target, authority, &cancellation, &progress) {
            Ok(facts) => read(&inner, &destination, generation, facts),
            Err(failure) => settle(&inner, &destination, generation, failure),
        },
        Work::Describe(request) => {
            describe(
                &inner,
                &backend,
                &destination,
                &target,
                request,
                authority,
                generation,
                &cancellation,
                &progress,
            );
        }
        Work::Submit(submission) => {
            submit(
                &inner,
                &backend,
                &store,
                &destination,
                &target,
                submission,
                authority,
                generation,
                &cancellation,
                &progress,
            );
        }
        Work::Resolve(operation) => {
            match backend.resolve(&target, operation, authority, &cancellation, &progress) {
                Ok(outcome) => await_outcome(
                    &inner,
                    &backend,
                    &store,
                    &destination,
                    &target,
                    operation,
                    authority,
                    generation,
                    &cancellation,
                    &progress,
                    outcome,
                ),
                // The service holds no record of the original, so nothing can ever resolve it.
                Err(SshMaintenanceFailure::Forgotten) => forget(
                    &inner,
                    &store,
                    &destination,
                    generation,
                    operation,
                    SshMaintenanceFailure::Forgotten,
                ),
                Err(failure) => settle(&inner, &destination, generation, failure),
            }
        }
    }
    release(&inner, &backend, &destination);
}

/// Describes one change for the observation this same exchange read, so confirming it performs
/// exactly what the person reviewed.
#[expect(
    clippy::too_many_arguments,
    reason = "one exchange owns one admitted generation and its immutable inputs"
)]
fn describe(
    inner: &Weak<Inner>,
    backend: &Arc<dyn SshMaintenanceBackend>,
    destination: &SshDestination,
    target: &MaintenanceTarget,
    request: RemoteMaintenanceRequest,
    authority: MaintenanceAuthority,
    generation: u64,
    cancellation: &SshCancellationToken,
    progress: &super::server_maintenance::MaintenanceProgress<'_>,
) {
    let facts = match backend.observe(target, authority, cancellation, progress) {
        Ok(facts) => facts,
        Err(failure) => return settle(inner, destination, generation, failure),
    };
    update(inner, destination, generation, |state, index| {
        state.entries[index].facts = Some(facts.clone());
    });
    let prepared = match request {
        RemoteMaintenanceRequest::Service(action) => backend
            .prepare(target, action, authority, cancellation, progress)
            .map(Some),
        RemoteMaintenanceRequest::Update => {
            backend.prepare_update(target, authority, cancellation, progress)
        }
        RemoteMaintenanceRequest::Return => backend
            .prepare_return(target, authority, cancellation, progress)
            .map(Some),
    };
    match prepared {
        // The artifact this client holds is already the approved build, so nothing changes.
        Ok(None) => read(inner, destination, generation, facts),
        Ok(Some(change)) => {
            update(inner, destination, generation, |state, index| {
                let handle = next_handle(state);
                let entry = &mut state.entries[index];
                entry.running = false;
                entry.state = RemoteOperationState::Described {
                    handle,
                    request,
                    change: Box::new(change),
                };
            });
        }
        Err(failure) => settle(inner, destination, generation, failure),
    }
}

/// Submits exactly one bound original. Nothing is dispatched before the record that lets this
/// client resolve the same original exists.
#[expect(
    clippy::too_many_arguments,
    reason = "one exchange owns one admitted generation and its immutable inputs"
)]
fn submit(
    inner: &Weak<Inner>,
    backend: &Arc<dyn SshMaintenanceBackend>,
    store: &Arc<dyn RemoteOperationStore>,
    destination: &SshDestination,
    target: &MaintenanceTarget,
    submission: OperationSubmission,
    authority: MaintenanceAuthority,
    generation: u64,
    cancellation: &SshCancellationToken,
    progress: &super::server_maintenance::MaintenanceProgress<'_>,
) {
    let client = match client(inner, store) {
        Ok(client) => client,
        Err(error) => {
            note_storage(inner, Some(error));
            // Nothing reached the other computer, and the fault is this client's storage.
            return unsubmitted(
                inner,
                destination,
                generation,
                SshMaintenanceFailure::NotRetained,
            );
        }
    };
    if cancellation.is_cancelled() {
        return unsubmitted(
            inner,
            destination,
            generation,
            SshMaintenanceFailure::Cancelled,
        );
    }
    match retain_record(inner, store, destination, generation, client) {
        Ok(true) => {}
        // The exchange no longer owns its entry, so nothing may be dispatched for it.
        Ok(false) => return,
        Err(error) => {
            note_storage(inner, Some(error));
            // Nothing reached the other computer, and the fault is this client's storage.
            return unsubmitted(
                inner,
                destination,
                generation,
                SshMaintenanceFailure::NotRetained,
            );
        }
    }
    match backend.apply(target, &submission, cancellation, progress) {
        Ok(outcome) => await_outcome(
            inner,
            backend,
            store,
            destination,
            target,
            submission.operation,
            authority,
            generation,
            cancellation,
            progress,
            outcome,
        ),
        Err(failure) if reached_no_effect(failure) => {
            forget(
                inner,
                store,
                destination,
                generation,
                submission.operation,
                failure,
            );
        }
        Err(failure) => settle(inner, destination, generation, failure),
    }
}

/// Whether one dispatch attempt proves the service performed nothing, so this client keeps no
/// original for it. A lost or cancelled reply proves nothing and is never counted here.
fn reached_no_effect(failure: SshMaintenanceFailure) -> bool {
    matches!(
        failure,
        SshMaintenanceFailure::Refused(_)
            | SshMaintenanceFailure::Changed(_)
            | SshMaintenanceFailure::NeedsPerson(_)
            | SshMaintenanceFailure::Incompatible(_)
            | SshMaintenanceFailure::Forgotten
    )
}

/// Retires an original nothing can resolve: the service holds no record of it, or refused it
/// before any effect. The entry keeps no original, so recovery never reads it again.
fn forget(
    inner: &Weak<Inner>,
    store: &Arc<dyn RemoteOperationStore>,
    destination: &SshDestination,
    generation: u64,
    operation: [u8; 32],
    failure: SshMaintenanceFailure,
) {
    note_storage(inner, store.retire(operation).err());
    unsubmitted(inner, destination, generation, failure);
}

/// Reads an admitted original until the service validates its outcome. Reading repeats no
/// dispatch, and an original left underway stays unconfirmed rather than being sent again.
#[expect(
    clippy::too_many_arguments,
    reason = "one exchange owns one admitted generation and its immutable inputs"
)]
fn await_outcome(
    inner: &Weak<Inner>,
    backend: &Arc<dyn SshMaintenanceBackend>,
    store: &Arc<dyn RemoteOperationStore>,
    destination: &SshDestination,
    target: &MaintenanceTarget,
    operation: [u8; 32],
    authority: MaintenanceAuthority,
    generation: u64,
    cancellation: &SshCancellationToken,
    progress: &super::server_maintenance::MaintenanceProgress<'_>,
    mut outcome: OperationOutcome,
) {
    let mut attempt = 0;
    while outcome.resolution == OperationResolution::Admitted {
        if attempt >= RESOLVE_READS || !wait(cancellation, resolve_delay(attempt)) {
            return settle(
                inner,
                destination,
                generation,
                SshMaintenanceFailure::Unconfirmed,
            );
        }
        attempt += 1;
        match backend.resolve(target, operation, authority, cancellation, progress) {
            Ok(next) => outcome = next,
            Err(SshMaintenanceFailure::Forgotten) => {
                return forget(
                    inner,
                    store,
                    destination,
                    generation,
                    operation,
                    SshMaintenanceFailure::Forgotten,
                );
            }
            Err(failure) => return settle(inner, destination, generation, failure),
        }
    }
    let retained = match outcome.resolution {
        OperationResolution::Completed => Some(RetainedOutcome::Completed {
            status: outcome.status,
            // Only an update or a return changes which build runs, so only they name one.
            build: match &outcome.kind {
                OperationKind::Update { candidate } => Some(candidate.clone()),
                OperationKind::Return { previous } => Some(previous.clone()),
                OperationKind::Start | OperationKind::Service { .. } => None,
            },
        }),
        OperationResolution::Refused(refusal) => Some(RetainedOutcome::Refused(refusal)),
        OperationResolution::Failed { can_return } => Some(RetainedOutcome::Failed { can_return }),
        OperationResolution::Admitted | OperationResolution::Unconfirmed => None,
    };
    let Some(retained) = retained else {
        return settle(
            inner,
            destination,
            generation,
            SshMaintenanceFailure::Unconfirmed,
        );
    };
    let resolved = update(inner, destination, generation, |state, index| {
        let handle = next_handle(state);
        let entry = &mut state.entries[index];
        entry.running = false;
        // The operation moved the server on from the observation its description bound, so the
        // facts the row returns to once the result is acknowledged are the ones the service
        // validated, not the ones it described against.
        if let (Some(facts), RetainedOutcome::Completed { status, build }) =
            (entry.facts.as_mut(), &retained)
        {
            if let Some(status) = status {
                facts.status = *status;
            }
            if let Some(build) = build {
                facts.approved = Some(build.clone());
            }
        }
        if let Some(submitted) = &entry.submitted {
            entry.state = RemoteOperationState::Resolved {
                handle,
                kind: submitted.kind.clone(),
                outcome: retained,
            };
        }
    });
    // The validated outcome is retained until the person acknowledges it. A write that fails
    // leaves the original unresolved, and reading it again returns the same outcome.
    if resolved && let Ok(client) = client(inner, store) {
        let result = retain_record(inner, store, destination, generation, client);
        note_storage(inner, result.err());
    }
}

/// Presents the facts the last validated read found.
fn read(
    inner: &Weak<Inner>,
    destination: &SshDestination,
    generation: u64,
    facts: MaintenanceFacts,
) {
    update(inner, destination, generation, |state, index| {
        let entry = &mut state.entries[index];
        entry.running = false;
        entry.facts = Some(facts.clone());
        entry.state = RemoteOperationState::Read(Box::new(facts));
    });
}

/// Presents one failed exchange. An original already dispatched stays unresolved, because
/// nothing here establishes that effects the service admitted were undone.
fn settle(
    inner: &Weak<Inner>,
    destination: &SshDestination,
    generation: u64,
    failure: SshMaintenanceFailure,
) {
    update(inner, destination, generation, |state, index| {
        let entry = &mut state.entries[index];
        entry.running = false;
        let kind = entry
            .submitted
            .as_ref()
            .map(|submitted| submitted.kind.clone());
        let failure = if entry.cancellation.is_cancelled()
            && !matches!(failure, SshMaintenanceFailure::Unconfirmed)
        {
            SshMaintenanceFailure::Cancelled
        } else {
            failure
        };
        entry.state = match (failure, kind) {
            (SshMaintenanceFailure::Cancelled | SshMaintenanceFailure::Unconfirmed, Some(kind)) => {
                RemoteOperationState::Unconfirmed { kind }
            }
            (SshMaintenanceFailure::NeedsPerson(reason), kind) => {
                RemoteOperationState::NeedsPerson { reason, kind }
            }
            (SshMaintenanceFailure::Changed(reason), kind) => {
                RemoteOperationState::Paused { reason, kind }
            }
            (failure, kind) => RemoteOperationState::Failed { failure, kind },
        };
    });
}

/// Presents a failure that reached no dispatch, so the entry keeps no original and the person
/// may ask again.
fn unsubmitted(
    inner: &Weak<Inner>,
    destination: &SshDestination,
    generation: u64,
    failure: SshMaintenanceFailure,
) {
    update(inner, destination, generation, |state, index| {
        state.entries[index].submitted = None;
    });
    settle(inner, destination, generation, failure);
}

/// This client's retained identifier, read once and kept for the process.
fn client(
    inner: &Weak<Inner>,
    store: &Arc<dyn RemoteOperationStore>,
) -> Result<[u8; 32], SshIntentStoreError> {
    if let Some(owner) = inner.upgrade()
        && let Ok(state) = owner.state.lock()
        && let Some(client) = state.client
    {
        return Ok(client);
    }
    let client = store.client()?;
    if let Some(owner) = inner.upgrade()
        && let Ok(mut state) = owner.state.lock()
    {
        state.client = Some(client);
    }
    Ok(client)
}

/// Writes the record this entry's original needs, with the removal state and outcome it has now.
/// Reports `false` when the exchange no longer owns an entry, so nothing resurrects a retired
/// original or an original for a connection that kept none.
fn retain_record(
    inner: &Weak<Inner>,
    store: &Arc<dyn RemoteOperationStore>,
    destination: &SshDestination,
    generation: u64,
    client: [u8; 32],
) -> Result<bool, SshIntentStoreError> {
    let Some(owner) = inner.upgrade() else {
        return Ok(false);
    };
    let record = {
        let Ok(state) = owner.state.lock() else {
            return Ok(false);
        };
        let Some(entry) = state
            .entries
            .iter()
            .find(|entry| entry.destination == *destination && entry.generation == generation)
        else {
            return Ok(false);
        };
        record_for(entry, client)
    };
    match record {
        Some(record) => store.retain(record).map(|()| true),
        None => Ok(false),
    }
}

/// One entry's original as it survives relaunch and saved-connection removal.
fn record_for(entry: &Entry, client: [u8; 32]) -> Option<RetainedRemoteOperation> {
    let submitted = entry.submitted.as_ref()?;
    Some(RetainedRemoteOperation {
        target: entry.target.clone()?,
        client,
        operation: submitted.operation,
        kind: submitted.kind.clone(),
        outcome: match &entry.state {
            RemoteOperationState::Resolved { outcome, .. } => Some(outcome.clone()),
            _ => None,
        },
        removed: entry.removed,
    })
}

/// Ends the bounded route this exchange used, once no other exchange for that destination needs
/// it. A pending original is unaffected.
fn release(
    inner: &Weak<Inner>,
    backend: &Arc<dyn SshMaintenanceBackend>,
    destination: &SshDestination,
) {
    let busy = inner.upgrade().is_some_and(|owner| {
        owner.state.lock().is_ok_and(|state| {
            state
                .entries
                .iter()
                .any(|entry| entry.destination == *destination && entry.running)
        })
    });
    if !busy {
        backend.release(destination);
    }
}

impl SshConnectionRemovals for SshServerOperations {
    /// Removal ends presentation and ordinary reconnect intent at once. An original this client
    /// submitted keeps its own record, so the person can still finish and acknowledge it.
    fn removed(&self, destination: &SshDestination) {
        let sealed = {
            let Ok(mut state) = self.inner.state.lock() else {
                return;
            };
            let mut sealed = Vec::new();
            state.entries.retain_mut(|entry| {
                if entry.destination != *destination || entry.removed {
                    return true;
                }
                if entry.submitted.is_none() {
                    entry.cancellation.cancel();
                    return false;
                }
                entry.removed = true;
                sealed.push(entry.generation);
                true
            });
            notify(&mut state);
            sealed
        };
        for generation in sealed {
            let inner = Arc::downgrade(&self.inner);
            let store = Arc::clone(&self.inner.store);
            let destination = destination.clone();
            let _ = std::thread::Builder::new()
                .name("spaceterm-ssh-operation".into())
                .spawn(move || {
                    let Ok(client) = client(&inner, &store) else {
                        return;
                    };
                    let result = retain_record(&inner, &store, &destination, generation, client);
                    note_storage(&inner, result.err());
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use spaceterm_server::LocalServer;
    use spaceterm_server::service::maintenance::{MaintenanceRefusal, OperationKind};
    use spaceterm_server::service::{ServiceStartup, ServiceStatus};

    use super::super::server_connection::{
        ConnectMode, ConnectProgress, ConnectedSshServer, RetainedSshConnection, SshConnectBackend,
        SshConnectFailure, SshConnectionIntentStore, SshServerContinuity, SshServerFacts,
        SshServerLease, SshServerObservation,
    };
    use super::super::server_maintenance::MaintenanceProgress;

    /// Holds one exchange until the test releases it.
    struct Gate {
        entered: async_channel::Sender<()>,
        release: async_channel::Receiver<()>,
    }

    /// A backend that answers each exchange from a script and records what it was asked.
    #[derive(Default)]
    struct Script {
        observations: Mutex<VecDeque<Result<MaintenanceFacts, SshMaintenanceFailure>>>,
        preparations: Mutex<VecDeque<Result<Option<PreparedChange>, SshMaintenanceFailure>>>,
        applications: Mutex<VecDeque<Result<OperationOutcome, SshMaintenanceFailure>>>,
        resolutions: Mutex<VecDeque<Result<OperationOutcome, SshMaintenanceFailure>>>,
        observed: Mutex<Vec<MaintenanceAuthority>>,
        applied: Mutex<Vec<OperationSubmission>>,
        resolved: Mutex<Vec<([u8; 32], MaintenanceAuthority)>>,
        /// The target every resolution bound, which names the route it reached.
        bound: Mutex<Vec<MaintenanceTarget>>,
        releases: AtomicUsize,
        gate: Mutex<Option<Gate>>,
    }
    impl Script {
        fn next<T>(
            queue: &Mutex<VecDeque<Result<T, SshMaintenanceFailure>>>,
        ) -> Result<T, SshMaintenanceFailure> {
            queue
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(SshMaintenanceFailure::Unavailable(
                    UnavailableKind::ServiceUnavailable,
                )))
        }
        fn observes(&self, facts: Result<MaintenanceFacts, SshMaintenanceFailure>) -> &Self {
            self.observations.lock().unwrap().push_back(facts);
            self
        }
        fn prepares(&self, change: Result<Option<PreparedChange>, SshMaintenanceFailure>) -> &Self {
            self.preparations.lock().unwrap().push_back(change);
            self
        }
        fn applies(&self, outcome: Result<OperationOutcome, SshMaintenanceFailure>) -> &Self {
            self.applications.lock().unwrap().push_back(outcome);
            self
        }
        fn resolves(&self, outcome: Result<OperationOutcome, SshMaintenanceFailure>) -> &Self {
            self.resolutions.lock().unwrap().push_back(outcome);
            self
        }
    }
    impl SshMaintenanceBackend for Script {
        fn observe(
            &self,
            _: &MaintenanceTarget,
            authority: MaintenanceAuthority,
            _: &SshCancellationToken,
            _: &MaintenanceProgress<'_>,
        ) -> Result<MaintenanceFacts, SshMaintenanceFailure> {
            self.observed.lock().unwrap().push(authority);
            Self::next(&self.observations)
        }
        fn prepare(
            &self,
            _: &MaintenanceTarget,
            _: Action,
            _: MaintenanceAuthority,
            _: &SshCancellationToken,
            _: &MaintenanceProgress<'_>,
        ) -> Result<PreparedChange, SshMaintenanceFailure> {
            Self::next(&self.preparations).map(|change| change.expect("a described change"))
        }
        fn prepare_update(
            &self,
            _: &MaintenanceTarget,
            _: MaintenanceAuthority,
            _: &SshCancellationToken,
            _: &MaintenanceProgress<'_>,
        ) -> Result<Option<PreparedChange>, SshMaintenanceFailure> {
            Self::next(&self.preparations)
        }
        fn prepare_return(
            &self,
            _: &MaintenanceTarget,
            _: MaintenanceAuthority,
            _: &SshCancellationToken,
            _: &MaintenanceProgress<'_>,
        ) -> Result<PreparedChange, SshMaintenanceFailure> {
            Self::next(&self.preparations).map(|change| change.expect("a described change"))
        }
        fn apply(
            &self,
            _: &MaintenanceTarget,
            submission: &OperationSubmission,
            _: &SshCancellationToken,
            _: &MaintenanceProgress<'_>,
        ) -> Result<OperationOutcome, SshMaintenanceFailure> {
            self.applied.lock().unwrap().push(submission.clone());
            let gate = self.gate.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.entered.send_blocking(()).unwrap();
                let _ = gate.release.recv_blocking();
            }
            Self::next(&self.applications)
        }
        fn resolve(
            &self,
            target: &MaintenanceTarget,
            operation: [u8; 32],
            authority: MaintenanceAuthority,
            _: &SshCancellationToken,
            _: &MaintenanceProgress<'_>,
        ) -> Result<OperationOutcome, SshMaintenanceFailure> {
            self.resolved.lock().unwrap().push((operation, authority));
            self.bound.lock().unwrap().push(target.clone());
            Self::next(&self.resolutions)
        }
        fn release(&self, _: &SshDestination) {
            self.releases.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Default)]
    struct Records {
        operations: Mutex<Vec<RetainedRemoteOperation>>,
        identifier: Mutex<Option<[u8; 32]>>,
        fails: Mutex<Option<SshIntentStoreError>>,
        /// The failure every write returns while set, leaving reads working.
        write_fails: Mutex<Option<SshIntentStoreError>>,
        loads: AtomicUsize,
        writes: AtomicUsize,
        /// How many writes the store refused, so a test can wait for a refused attempt.
        refused: AtomicUsize,
    }
    impl RemoteOperationStore for Records {
        fn load(&self) -> Result<Vec<RetainedRemoteOperation>, SshIntentStoreError> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = *self.fails.lock().unwrap() {
                return Err(error);
            }
            Ok(self.operations.lock().unwrap().clone())
        }
        fn retain(&self, operation: RetainedRemoteOperation) -> Result<(), SshIntentStoreError> {
            if let Some(error) = (*self.fails.lock().unwrap()).or(*self.write_fails.lock().unwrap())
            {
                return Err(error);
            }
            let mut operations = self.operations.lock().unwrap();
            operations.retain(|other| other.operation != operation.operation);
            operations.push(operation);
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn retire(&self, operation: [u8; 32]) -> Result<(), SshIntentStoreError> {
            if let Some(error) = (*self.fails.lock().unwrap()).or(*self.write_fails.lock().unwrap())
            {
                self.refused.fetch_add(1, Ordering::SeqCst);
                return Err(error);
            }
            self.operations
                .lock()
                .unwrap()
                .retain(|other| other.operation != operation);
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn client(&self) -> Result<[u8; 32], SshIntentStoreError> {
            if let Some(error) = *self.fails.lock().unwrap() {
                return Err(error);
            }
            Ok(*self.identifier.lock().unwrap().get_or_insert([9; 32]))
        }
    }
    impl Records {
        fn only(&self) -> RetainedRemoteOperation {
            let operations = self.operations.lock().unwrap();
            assert_eq!(operations.len(), 1, "exactly one original is retained");
            operations[0].clone()
        }
    }

    struct Route;
    impl SshServerLease for Route {
        fn is_alive(&self) -> bool {
            true
        }
    }
    /// A connection backend that admits every connection with the identity one maintenance
    /// operation binds, so the registry knows that identity.
    struct Admits;
    impl SshConnectBackend for Admits {
        fn connect(
            &self,
            _: &SshDestination,
            _: ConnectMode,
            _: Option<&SshServerContinuity>,
            _: &SshCancellationToken,
            _: &ConnectProgress<'_>,
        ) -> Result<ConnectedSshServer, SshConnectFailure> {
            Ok(ConnectedSshServer {
                server: LocalServer::for_identity("spaceterm-development").unwrap(),
                continuity: pin(),
                lease: Arc::new(Route),
                protocol_major: 1,
                facts: SshServerFacts {
                    platform: ("linux".into(), "x86_64".into()),
                    version: Some("1.2.3".into()),
                    starts_automatically: true,
                },
            })
        }
        fn observe(
            &self,
            _: &SshDestination,
            _: Option<&SshServerContinuity>,
            _: &SshCancellationToken,
            _: &ConnectProgress<'_>,
        ) -> Result<SshServerObservation, SshConnectFailure> {
            Err(SshConnectFailure::Stopped)
        }
    }
    #[derive(Default)]
    struct Intents(Mutex<Vec<RetainedSshConnection>>);
    impl SshConnectionIntentStore for Intents {
        fn load(&self) -> Result<Vec<RetainedSshConnection>, SshIntentStoreError> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn retain(&self, connection: RetainedSshConnection) -> Result<(), SshIntentStoreError> {
            let mut saved = self.0.lock().unwrap();
            saved.retain(|other| other.destination != connection.destination);
            saved.push(connection);
            Ok(())
        }
        fn remove(&self, destination: &SshDestination) -> Result<(), SshIntentStoreError> {
            self.0
                .lock()
                .unwrap()
                .retain(|other| other.destination != *destination);
            Ok(())
        }
    }

    fn destination() -> SshDestination {
        SshDestination::new("penguin".into()).unwrap()
    }
    fn pin() -> SshServerContinuity {
        SshServerContinuity {
            route: [1; 32],
            account: 501,
            build: "spaceterm-development".into(),
            server: [2; 32],
        }
    }
    fn facts() -> MaintenanceFacts {
        MaintenanceFacts {
            server: [2; 32],
            account: 501,
            build: "spaceterm-development".into(),
            status: ServiceStatus {
                installed: true,
                enabled: true,
                running: true,
                stopped: false,
            },
            startup: ServiceStartup::AtLogin,
            approved: None,
            activation: None,
        }
    }
    fn change() -> PreparedChange {
        PreparedChange {
            confirmation: "exact-observation".into(),
            running: true,
            stopped: false,
            startup: ServiceStartup::AtLogin,
            previous: None,
            candidate: None,
        }
    }
    /// The validated result the scripted service reports, as it survives relaunch.
    fn stopped_outcome() -> RetainedOutcome {
        RetainedOutcome::Completed {
            status: Some(ServiceStatus {
                installed: true,
                enabled: true,
                running: false,
                stopped: true,
            }),
            build: None,
        }
    }

    fn outcome(resolution: OperationResolution) -> OperationOutcome {
        OperationOutcome {
            operation: [0; 32],
            kind: OperationKind::Service {
                action: Action::Stop,
            },
            resolution,
            status: Some(ServiceStatus {
                installed: true,
                enabled: true,
                running: false,
                stopped: true,
            }),
        }
    }
    fn record(outcome: Option<RetainedOutcome>) -> RetainedRemoteOperation {
        RetainedRemoteOperation {
            target: MaintenanceTarget {
                destination: destination(),
                continuity: pin(),
            },
            client: [9; 32],
            operation: [7; 32],
            kind: SubmittedKind::Service(Action::Stop),
            outcome,
            removed: false,
        }
    }
    /// A registry that has not reached any host.
    fn registry() -> (SshServerConnections, Arc<Intents>) {
        let intents = Arc::new(Intents::default());
        (
            SshServerConnections::new(Arc::new(Admits), intents.clone()),
            intents,
        )
    }
    /// A registry that has admitted one connection to the destination, as Connect does.
    fn connected() -> (SshServerConnections, Arc<Intents>) {
        let (connections, intents) = registry();
        connections.connect(destination());
        eventually(|| connections.continuity(&destination()).is_some());
        (connections, intents)
    }
    fn eventually(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !predicate() {
            assert!(Instant::now() < deadline, "the owner did not reach it");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    /// The handle of the change the owner describes, once it has one.
    fn described(owner: &SshServerOperations) -> RemoteOperationHandle {
        eventually(|| {
            matches!(
                owner.state(&destination()),
                RemoteOperationState::Described { .. }
            )
        });
        let RemoteOperationState::Described { handle, .. } = owner.state(&destination()) else {
            unreachable!()
        };
        handle
    }
    /// The handle of the validated result the owner presents, once it has one.
    fn resolved(owner: &SshServerOperations) -> RemoteOperationHandle {
        eventually(|| {
            matches!(
                owner.state(&destination()),
                RemoteOperationState::Resolved { .. }
            )
        });
        let RemoteOperationState::Resolved { handle, .. } = owner.state(&destination()) else {
            unreachable!()
        };
        handle
    }
    /// Describes a Stop and confirms it, answering the dispatch with `dispatch`.
    fn submit(
        script: &Arc<Script>,
        records: &Arc<Records>,
        dispatch: Result<OperationOutcome, SshMaintenanceFailure>,
    ) -> (Arc<SshServerOperations>, SshServerConnections, Arc<Intents>) {
        script
            .observes(Ok(facts()))
            .prepares(Ok(Some(change())))
            .applies(dispatch);
        let (connections, intents) = connected();
        let owner = SshServerOperations::new(script.clone(), records.clone(), connections.clone());
        owner.describe(
            &destination(),
            RemoteMaintenanceRequest::Service(Action::Stop),
        );
        let handle = described(owner.as_ref());
        assert!(
            script.applied.lock().unwrap().is_empty(),
            "describing a change performs none of it"
        );
        owner.confirm(handle);
        (owner, connections, intents)
    }

    #[test]
    fn constructing_and_reading_state_reaches_no_host_and_no_storage() {
        let script = Arc::new(Script::default());
        let records = Arc::new(Records::default());
        let (connections, _) = registry();
        let owner = SshServerOperations::new(script.clone(), records.clone(), connections.clone());
        assert!(owner.entries().is_empty());
        assert!(matches!(
            owner.state(&destination()),
            RemoteOperationState::Unread
        ));
        assert_eq!(
            (
                script.observed.lock().unwrap().len(),
                records.loads.load(Ordering::SeqCst),
                records.writes.load(Ordering::SeqCst)
            ),
            (0, 0, 0)
        );

        // An identity this client never admitted is not a discovery target.
        owner.refresh(&destination());
        std::thread::sleep(Duration::from_millis(30));
        assert!(matches!(
            owner.state(&destination()),
            RemoteOperationState::NeedsPerson {
                reason: InteractionReason::Authentication,
                kind: None
            }
        ));
        assert!(script.observed.lock().unwrap().is_empty());
        assert!(connections.connections().is_empty());
    }

    #[test]
    fn a_described_change_is_performed_once_by_confirming_its_handle() {
        let script = Arc::new(Script::default());
        let records = Arc::new(Records::default());
        let (owner, _, _) = submit(
            &script,
            &records,
            Ok(outcome(OperationResolution::Completed)),
        );
        let handle = resolved(owner.as_ref());

        // A spent handle names nothing, so nothing is performed again.
        owner.confirm(handle);
        std::thread::sleep(Duration::from_millis(30));
        let applied = script.applied.lock().unwrap().clone();
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].kind, SubmittedKind::Service(Action::Stop));
        assert_eq!(
            applied[0].confirmation.as_deref(),
            Some("exact-observation"),
            "the dispatch carries the confirmation for the observation it described"
        );
        assert_eq!(records.only().outcome, Some(stopped_outcome()));
        assert_eq!(script.observed.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_described_change_whose_observation_changed_refuses_and_performs_nothing() {
        let script = Arc::new(Script::default());
        let records = Arc::new(Records::default());
        let (owner, _, _) = submit(
            &script,
            &records,
            Err(SshMaintenanceFailure::Refused(MaintenanceRefusal::Changed)),
        );
        eventually(|| {
            matches!(
                owner.state(&destination()),
                RemoteOperationState::Failed {
                    failure: SshMaintenanceFailure::Refused(MaintenanceRefusal::Changed),
                    kind: None,
                }
            )
        });
        assert_eq!(script.applied.lock().unwrap().len(), 1);
        assert!(
            records.operations.lock().unwrap().is_empty(),
            "a refusal before any effect leaves no original to resolve"
        );

        // The person must confirm a fresh description instead.
        script
            .observes(Ok(facts()))
            .prepares(Ok(Some(change())))
            .applies(Ok(outcome(OperationResolution::Completed)));
        owner.describe(
            &destination(),
            RemoteMaintenanceRequest::Service(Action::Stop),
        );
        owner.confirm(described(owner.as_ref()));
        resolved(owner.as_ref());
        assert_eq!(script.applied.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_submitted_operation_survives_a_store_reload_and_is_resolved_exactly_once() {
        let script = Arc::new(Script::default());
        let records = Arc::new(Records::default());
        let (owner, connections, _) =
            submit(&script, &records, Err(SshMaintenanceFailure::Unconfirmed));
        eventually(|| {
            matches!(
                owner.state(&destination()),
                RemoteOperationState::Unconfirmed { .. }
            )
        });
        let submitted = records.only();
        assert_eq!(submitted.outcome, None);
        assert_eq!(submitted.client, [9; 32]);
        drop(owner);

        // A later launch reads the same records and resolves the same original.
        let reopened = Arc::new(Script::default());
        reopened.resolves(Ok(outcome(OperationResolution::Completed)));
        let owner = SshServerOperations::new(reopened.clone(), records.clone(), connections);
        owner.resume_retained();
        owner.resume_retained();
        resolved(owner.as_ref());
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(
            *reopened.resolved.lock().unwrap(),
            vec![(submitted.operation, MaintenanceAuthority::Quiet)],
            "the same original is read once, quietly"
        );
        assert!(
            reopened.applied.lock().unwrap().is_empty(),
            "resolution repeats no dispatch"
        );
        assert_eq!(records.only().outcome, Some(stopped_outcome()));
    }

    #[test]
    fn a_result_reloaded_after_relaunch_waits_for_acknowledgement_without_asking_the_host() {
        let script = Arc::new(Script::default());
        let records = Arc::new(Records::default());
        records
            .operations
            .lock()
            .unwrap()
            .push(record(Some(stopped_outcome())));
        let (connections, _) = registry();
        let owner = SshServerOperations::new(script.clone(), records.clone(), connections);
        owner.resume_retained();
        let handle = resolved(owner.as_ref());
        assert!(matches!(
            owner.state(&destination()),
            RemoteOperationState::Resolved {
                kind: SubmittedKind::Service(Action::Stop),
                outcome: RetainedOutcome::Completed { .. },
                ..
            }
        ));
        assert!(
            script.resolved.lock().unwrap().is_empty()
                && script.observed.lock().unwrap().is_empty(),
            "a validated result needs no computer to present it again"
        );

        owner.acknowledge(handle);
        eventually(|| records.operations.lock().unwrap().is_empty());
        eventually(|| matches!(owner.state(&destination()), RemoteOperationState::Unread));
    }

    #[test]
    fn quiet_continuation_never_prompts_and_leaves_unavailable_authority_unconfirmed() {
        let script = Arc::new(Script::default());
        script.resolves(Err(SshMaintenanceFailure::NeedsPerson(
            InteractionReason::CredentialProvider,
        )));
        let records = Arc::new(Records::default());
        records.operations.lock().unwrap().push(record(None));
        let (connections, intents) = registry();
        let owner = SshServerOperations::new(script.clone(), records.clone(), connections.clone());
        owner.resume_retained();
        eventually(|| {
            matches!(
                owner.state(&destination()),
                RemoteOperationState::NeedsPerson {
                    reason: InteractionReason::CredentialProvider,
                    kind: Some(SubmittedKind::Service(Action::Stop)),
                }
            )
        });
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(
            *script.resolved.lock().unwrap(),
            vec![([7; 32], MaintenanceAuthority::Quiet)],
            "it asks once, with the authority already available"
        );
        assert!(
            script.applied.lock().unwrap().is_empty(),
            "it starts nothing and submits no replacement work"
        );
        assert_eq!(
            records.only().outcome,
            None,
            "the original stays resolvable"
        );
        assert_eq!(
            records.writes.load(Ordering::SeqCst),
            0,
            "recovery writes no new intent"
        );
        assert!(
            intents.0.lock().unwrap().is_empty() && connections.connections().is_empty(),
            "recovery never recreates the connection or its saved intent"
        );

        // The person asking may authenticate a fresh query route on the original's route.
        script.resolves(Ok(outcome(OperationResolution::Completed)));
        owner.resume(&destination());
        resolved(owner.as_ref());
        assert_eq!(
            script.resolved.lock().unwrap()[1],
            ([7; 32], MaintenanceAuthority::Requested)
        );
    }

    #[test]
    fn cancelling_an_admitted_operation_does_not_record_that_its_effects_were_undone() {
        let script = Arc::new(Script::default());
        let (entered, arrived) = async_channel::unbounded();
        let (release, held) = async_channel::unbounded();
        *script.gate.lock().unwrap() = Some(Gate {
            entered,
            release: held,
        });
        let records = Arc::new(Records::default());
        let (owner, _, _) = submit(&script, &records, Err(SshMaintenanceFailure::Cancelled));
        arrived.recv_blocking().unwrap();
        assert_eq!(records.only().outcome, None, "the record precedes dispatch");

        owner.cancel(&destination());
        assert!(matches!(
            owner.state(&destination()),
            RemoteOperationState::Working {
                cancellable: false,
                ..
            }
        ));
        release.send_blocking(()).unwrap();
        eventually(|| {
            matches!(
                owner.state(&destination()),
                RemoteOperationState::Unconfirmed {
                    kind: SubmittedKind::Service(Action::Stop),
                    ..
                }
            )
        });
        assert_eq!(
            records.only().outcome,
            None,
            "nothing says the service undid what it admitted"
        );
    }

    #[test]
    fn a_validated_outcome_stays_until_it_is_acknowledged() {
        let script = Arc::new(Script::default());
        let records = Arc::new(Records::default());
        let (owner, _, _) = submit(
            &script,
            &records,
            Ok(outcome(OperationResolution::Completed)),
        );
        let handle = resolved(owner.as_ref());
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            matches!(
                owner.state(&destination()),
                RemoteOperationState::Resolved { .. }
            ),
            "an unacknowledged result is not retired on its own"
        );

        // A retirement that cannot be saved leaves the result waiting.
        *records.fails.lock().unwrap() = Some(SshIntentStoreError::Unavailable);
        owner.acknowledge(handle);
        eventually(|| records.refused.load(Ordering::SeqCst) == 1);
        assert!(matches!(
            owner.state(&destination()),
            RemoteOperationState::Resolved { .. }
        ));

        *records.fails.lock().unwrap() = None;
        owner.acknowledge(handle);
        eventually(|| records.operations.lock().unwrap().is_empty());
        eventually(|| matches!(owner.state(&destination()), RemoteOperationState::Read(_)));
        // The row returns to the state the service validated, not the one the Stop was
        // described against, so it never says the server is running after it stopped it.
        let RemoteOperationState::Read(facts) = owner.state(&destination()) else {
            unreachable!("matched above")
        };
        assert_eq!(
            (facts.status.running, facts.status.stopped),
            (false, true),
            "an acknowledged result leaves the facts the operation produced"
        );
        owner.acknowledge(handle);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(owner.entries().len(), 1);
    }

    #[test]
    fn removal_keeps_the_original_resolvable_and_never_recreates_the_connection() {
        let script = Arc::new(Script::default());
        let records = Arc::new(Records::default());
        let (owner, connections, intents) =
            submit(&script, &records, Err(SshMaintenanceFailure::Unconfirmed));
        eventually(|| {
            matches!(
                owner.state(&destination()),
                RemoteOperationState::Unconfirmed { .. }
            )
        });

        connections.remove(&destination());
        assert!(connections.connections().is_empty(), "removal is immediate");
        eventually(|| records.only().removed);
        let entries = owner.entries();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].removed);
        assert!(matches!(
            entries[0].state,
            RemoteOperationState::Unconfirmed { .. }
        ));

        // The person can still finish and acknowledge the original.
        script.resolves(Ok(outcome(OperationResolution::Completed)));
        owner.resume(&destination());
        let handle = resolved(owner.as_ref());
        assert!(
            intents.0.lock().unwrap().is_empty() && connections.connections().is_empty(),
            "resolution writes no saved-connection intent"
        );
        owner.acknowledge(handle);
        eventually(|| owner.entries().is_empty());
        assert!(records.operations.lock().unwrap().is_empty());
    }

    #[test]
    fn a_destination_saved_again_does_not_inherit_the_removed_record_or_its_late_result() {
        let script = Arc::new(Script::default());
        let (entered, arrived) = async_channel::unbounded();
        let (release, held) = async_channel::unbounded();
        *script.gate.lock().unwrap() = Some(Gate {
            entered,
            release: held,
        });
        let records = Arc::new(Records::default());
        let (owner, connections, _) =
            submit(&script, &records, Err(SshMaintenanceFailure::Unconfirmed));
        arrived.recv_blocking().unwrap();

        connections.remove(&destination());
        connections.connect(destination());
        eventually(|| connections.continuity(&destination()).is_some());
        script.observes(Ok(facts()));
        owner.refresh(&destination());
        eventually(|| matches!(owner.state(&destination()), RemoteOperationState::Read(_)));

        // The removed connection's late result reaches only its own sealed entry.
        release.send_blocking(()).unwrap();
        eventually(|| {
            owner.entries().iter().any(|entry| {
                entry.removed && matches!(entry.state, RemoteOperationState::Unconfirmed { .. })
            })
        });
        assert!(
            matches!(owner.state(&destination()), RemoteOperationState::Read(_)),
            "the connection saved again inherits no original and no late result"
        );
        let entries = owner.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries.iter().filter(|entry| entry.removed).count(),
            1,
            "only the removed connection keeps the original"
        );
    }

    #[test]
    fn starting_a_server_needs_no_confirmation_and_is_refused_while_work_runs() {
        let script = Arc::new(Script::default());
        script.applies(Ok(outcome(OperationResolution::Completed)));
        let (entered, arrived) = async_channel::unbounded();
        let (release, held) = async_channel::unbounded();
        *script.gate.lock().unwrap() = Some(Gate {
            entered,
            release: held,
        });
        let records = Arc::new(Records::default());
        let (connections, _) = connected();
        let owner = SshServerOperations::new(script.clone(), records.clone(), connections);
        owner.start(&destination());
        arrived.recv_blocking().unwrap();

        owner.start(&destination());
        release.send_blocking(()).unwrap();
        resolved(owner.as_ref());
        std::thread::sleep(Duration::from_millis(30));
        let applied = script.applied.lock().unwrap().clone();
        assert_eq!(
            applied.len(),
            1,
            "an entry already working admits no dispatch"
        );
        assert_eq!(applied[0].kind, SubmittedKind::Start);
        assert_eq!(
            applied[0].confirmation, None,
            "Start ends no work, so it needs no exact-observation confirmation"
        );
        let retained = records.only();
        assert_eq!(
            retained.operation, applied[0].operation,
            "the record names the original that was dispatched"
        );
        assert_eq!(retained.kind, SubmittedKind::Start);
    }

    #[test]
    fn recovery_binds_the_route_the_original_bound_rather_than_a_current_alias() {
        let script = Arc::new(Script::default());
        script.resolves(Ok(outcome(OperationResolution::Completed)));
        let records = Arc::new(Records::default());
        let mut original = record(None);
        original.target.continuity.route = [5; 32];
        records.operations.lock().unwrap().push(original.clone());
        // The alias now reaches another computer, which the registry has admitted.
        let (connections, _) = connected();
        assert_eq!(connections.continuity(&destination()), Some(pin()));
        assert_ne!(original.target.continuity, pin());
        let owner = SshServerOperations::new(script.clone(), records.clone(), connections);
        owner.resume_retained();
        resolved(owner.as_ref());
        assert_eq!(
            *script.bound.lock().unwrap(),
            vec![original.target],
            "resolution reaches the route the original bound, not the current alias"
        );
    }

    #[test]
    fn nothing_is_dispatched_until_the_record_that_resolves_it_is_retained() {
        let script = Arc::new(Script::default());
        script.observes(Ok(facts())).prepares(Ok(Some(change())));
        let records = Arc::new(Records::default());
        *records.write_fails.lock().unwrap() = Some(SshIntentStoreError::Unavailable);
        let (connections, _) = connected();
        let owner = SshServerOperations::new(script.clone(), records.clone(), connections.clone());
        owner.describe(
            &destination(),
            RemoteMaintenanceRequest::Service(Action::Stop),
        );
        owner.confirm(described(owner.as_ref()));
        eventually(|| {
            matches!(
                owner.state(&destination()),
                RemoteOperationState::Failed {
                    failure: SshMaintenanceFailure::NotRetained,
                    kind: None,
                }
            )
        });
        assert!(
            script.applied.lock().unwrap().is_empty(),
            "an operation this client could not resolve is never dispatched"
        );

        // Storage works again, so the same request can be confirmed and dispatched.
        *records.write_fails.lock().unwrap() = None;
        script
            .observes(Ok(facts()))
            .prepares(Ok(Some(change())))
            .applies(Ok(outcome(OperationResolution::Completed)));
        owner.describe(
            &destination(),
            RemoteMaintenanceRequest::Service(Action::Stop),
        );
        owner.confirm(described(owner.as_ref()));
        resolved(owner.as_ref());
        assert_eq!(script.applied.lock().unwrap().len(), 1);
    }
}
