//! Request dispatch, and the connection-facing service boundary of the server's Pane host.
use super::publish::{Observed, Publication};
use super::*;
use crate::terminal::session::SubscriptionNotifier;
use spaceterm_server::connection::Wake;
use spaceterm_server::connection::outbox::Outbox;
use spaceterm_server::connection::service::{Dispatch, PaneService as ConnectionService, Peer};
use spaceterm_server::protocol::{
    self, Channel, Claim, ClaimReply, InputEvent, InputOutcome, Operation, PaneEvent, ReplyBody,
    RequestId,
};
use std::collections::BTreeMap;

impl ServerPaneHost {
    /// Answers one command for this Pane. A refusal is the reply [`Reply::Refused`].
    pub(super) fn request(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        request: Request,
    ) -> Reply {
        self.handle(device, connection, request)
            .unwrap_or_else(Reply::Refused)
    }
    fn handle(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        request: Request,
    ) -> Result<Reply, Refusal> {
        self.retain_directory();
        if let Request::TakeOver {
            target,
            size,
            style,
        } = request
        {
            self.reconcile_selection()?;
            // The shell exited and kept its Pane. A new claim offers Session Ended.
            if self.pane.is_none() && self.dormant.is_some() {
                return Err(Refusal::Stale);
            }
            let pane = self.pane.as_mut().ok_or(Refusal::Missing)?;
            if PaneTarget::from(pane.lease) != target {
                return Err(Refusal::Stale);
            }
            return pane.claim(device, connection, size, style, true);
        }
        // The protocol bounds an Input message too. Check before decoding or dispatching any
        // key, including malformed later events in a batch.
        if let Request::Keys { keys, .. } = &request
            && (keys.is_empty() || keys.len() > MAX_KEY_BATCH)
        {
            return Err(Refusal::Unsupported);
        }
        let pane = self.pane.as_mut().ok_or(Refusal::Missing)?;
        let lease = match &request {
            Request::Accessibility { lease, .. }
            | Request::EndAccessibilityDemand { lease }
            | Request::AccessibilitySelection { lease, .. }
            | Request::GraphicsChunk { lease, .. } => lease,
            Request::CompleteClipboard { lease, .. }
            | Request::Focus { lease, .. }
            | Request::Input { lease, .. }
            | Request::Keys { lease, .. }
            | Request::Resize { lease, .. }
            | Request::ClearScreenAndScrollback { lease }
            | Request::Close { lease }
            | Request::Style { lease, .. }
            | Request::Release { lease } => lease,
            _ => return Err(Refusal::Unsupported),
        };
        if Some(device) != pane.device
            || connection != pane.lease.connection
            || *lease != pane.lease
            || !pane.connected
        {
            return Err(Refusal::Stale);
        }
        pane.drain();
        match request {
            Request::EndAccessibilityDemand { .. } => {
                pane.runtime.end_server_accessibility_demand();
                Ok(Reply::Unchanged)
            }
            Request::Accessibility {
                revision,
                offset,
                base,
                ..
            } => {
                if pane.ended || pane.failed {
                    return Err(Refusal::Missing);
                }
                if let Some(demand) = pane.runtime.accessibility_demand_sender() {
                    demand.request();
                }
                Ok(Reply::Accessibility(
                    pane.accessibility.chunk(revision, offset, base),
                ))
            }
            Request::AccessibilitySelection {
                focus_generation,
                selection,
                ..
            } => {
                if pane.ended || pane.failed {
                    return Err(Refusal::Missing);
                }
                if focus_generation == 0 || focus_generation != pane.focus_generation {
                    return Err(Refusal::Stale);
                }
                pane.require_terminal_focus()?;
                let _completion = pane.runtime.set_server_accessibility_selection(
                    selection
                        .map(accessibility::decode_selection)
                        .transpose()
                        .map_err(|_| Refusal::Unsupported)?,
                );
                Ok(Reply::Unchanged)
            }
            Request::GraphicsChunk { image, offset, .. } => {
                Ok(Reply::GraphicsChunk(pane.graphics_chunk(image, offset)))
            }
            Request::CompleteClipboard {
                generation,
                id,
                completion,
                ..
            } => {
                if generation != pane.focus_generation {
                    return Err(Refusal::Stale);
                }
                pane.effects.complete(id, completion);
                Ok(Reply::Unchanged)
            }
            Request::Focus {
                generation, target, ..
            } => pane.focus(generation, target),
            Request::Input {
                generation, input, ..
            } => {
                let reply = pane.input(generation, input);
                if matches!(reply, Err(Refusal::Failed)) {
                    pane.invalidate_input();
                }
                reply
            }
            Request::Keys {
                generation, keys, ..
            } => {
                let reply = pane.keys(generation, keys);
                if matches!(reply, Err(Refusal::Failed)) {
                    pane.invalidate_input();
                }
                reply
            }
            Request::Resize { size, .. } => {
                size.validate().map_err(|_| Refusal::Unsupported)?;
                if pane.ended || pane.failed {
                    return Err(Refusal::Missing);
                }
                pane.runtime.resize(geometry(size));
                pane.size = size;
                Ok(Reply::Unchanged)
            }
            Request::ClearScreenAndScrollback { .. } => {
                if pane.ended || pane.failed {
                    return Err(Refusal::Missing);
                }
                pane.runtime.clear_screen_and_scrollback();
                Ok(Reply::Unchanged)
            }
            Request::Style { style, .. } => {
                pane.appearance_generation += 1;
                pane.runtime.update_appearance(
                    appearance(Some(style), pane.appearance_generation)
                        .map_err(|_| Refusal::Failed)?,
                );
                Ok(Reply::Unchanged)
            }
            Request::Release { .. } => {
                pane.invalidate_input();
                pane.lease.control = nonce().map_err(|_| Refusal::Failed)?;
                pane.device = None;
                pane.connected = false;
                Ok(Reply::Unchanged)
            }
            Request::Close { .. } => {
                pane.runtime
                    .shutdown_and_wait(Duration::from_secs(5))
                    .map_err(|_| Refusal::CleanupUnresolved)?;
                self.identity = None;
                self.pane = None;
                Ok(Reply::Closed)
            }
            _ => Err(Refusal::Unsupported),
        }
    }
}
impl ServerPaneHost {
    pub(super) fn admitted(&mut self, client: [u8; 32], name: control::ClientName) {
        self.names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .admit(client, name);
    }
    pub(super) fn lease_live(&self, lease: &Lease) -> bool {
        self.pane.as_ref().is_some_and(|pane| pane.lease == *lease)
    }
    /// See [`HostCommands::unbound`].
    pub(super) fn unbound(&mut self, connection: [u8; 32], lease: &Lease) {
        if let Some(pane) = &mut self.pane
            && pane.lease == *lease
            && pane.lease.connection == connection
        {
            pane.invalidate_input();
            pane.connected = false;
        }
    }
    pub(super) fn disconnected(&mut self, connection: [u8; 32]) {
        if let Some(pane) = &mut self.pane
            && pane.lease.connection == connection
        {
            pane.invalidate_input();
            pane.connected = false;
        }
    }
    /// See [`HostCommands::release_control`].
    pub(super) fn release_control(&mut self, client: [u8; 32]) {
        if let Some(dormant) = &mut self.dormant
            && dormant.device == Some(client)
        {
            dormant.device = None;
            dormant.attachment_generation = None;
        }
        if let Some(pane) = &mut self.pane
            && pane.device == Some(client)
        {
            pane.invalidate_input();
            pane.device = None;
            pane.connected = false;
            pane.attachment_generation = None;
            pane.lease.control = [0; 32];
        }
    }
    /// Ends this Pane's Terminal Session, keeping the directory its shell confirmed.
    pub(super) fn shutdown(&mut self) -> Result<(), ServerError> {
        self.retain_directory();
        if let Some(pane) = &mut self.pane {
            pane.invalidate_input();
            pane.connected = false;
            pane.runtime
                .shutdown_and_wait(Duration::from_secs(5))
                .map_err(|_| ServerError::CleanupUnresolved)?;
        }
        self.pane = None;
        Ok(())
    }
}

/// A host's in-process commands, one connection's lease at a time. [`Served`] maps the protocol
/// onto them; tests drive them directly.
pub trait HostCommands: Send {
    fn restore(&mut self, _store: OrganizationStore) -> Result<(), ServerError> {
        Ok(())
    }
    /// Runs timed work and returns when it is next due. `None` means no timed work is pending.
    fn tick(&mut self, _now: std::time::Instant) -> Option<std::time::Instant> {
        None
    }
    /// The host's wake, which makes it tick again; see `PaneService::ticker`.
    fn ticker(&mut self, _wake: Arc<dyn spaceterm_server::connection::Wake>) {}
    /// A Terminal Session that no channel presents, a request, or an observed change rang the
    /// bell, so the next tick carries what it changed. A host may pace what such ticks push.
    fn rung(&mut self) {}
    /// Whether an [`Self::observe`] since the last call drained a change that only the host's
    /// timed work pushes, such as an exit or new Terminal Metadata. [`Served`] then rings the
    /// bell.
    fn take_observed_change(&mut self) -> bool {
        false
    }
    /// The host is stopping; refuse every deferred reply still owed. See `PaneService::stopping`.
    fn stopping(&mut self) {}
    /// The live Terminal Sessions. See `PaneService::sessions`.
    fn sessions(&self) -> Vec<[u8; 32]> {
        Vec::new()
    }
    /// The Terminal Diagnostics counters this host's Terminal Sessions and publications record
    /// into.
    fn terminal_counters(&self) -> Arc<spaceterm_server::diagnostics::TerminalCounters>;
    /// Records the Client Name an admitted client supplied, before any of its requests.
    fn admitted(&mut self, _client: [u8; 32], _name: control::ClientName) {}
    /// A connection was admitted. A host that pushes Workspace Inventory keeps `outbox` until
    /// [`Self::disconnected`].
    fn connected(&mut self, _client: [u8; 32], _connection: [u8; 32], _outbox: &Outbox) {}
    /// A Workspace edit, or with `None` the resolution of a recorded one, replied to after
    /// the inventory revision that holds its outcome.
    fn mutate(
        &mut self,
        _client: [u8; 32],
        _connection: [u8; 32],
        _id: RequestId,
        _sequence: u64,
        _mutation: Option<spaceterm_server::hierarchy::Mutation>,
    ) -> Dispatch {
        Dispatch::reply(Err(Refusal::Unsupported))
    }
    /// Retires a delivered edit outcome. Nothing replies.
    fn acknowledge(&mut self, _client: [u8; 32], _sequence: u64) {}
    /// Permanently releases this paired identity's live and dormant control grants.
    fn revoked(&mut self, _client: [u8; 32]) {}
    /// Releases only this identity's live and dormant control grants, and keeps its edit
    /// outcomes and run receipts.
    fn release_control(&mut self, _client: [u8; 32]) {}
    /// Whether this identity keeps an unacknowledged edit outcome or run receipt. See
    /// `PaneService::holds`.
    fn holds(&self, _client: [u8; 32]) -> bool {
        false
    }
    fn request(&mut self, device: [u8; 32], connection: [u8; 32], request: Request) -> Reply;
    /// Starts read `id` of `connection`. Returns its reply when it settles at once. Otherwise the
    /// read runs on another thread, which wakes the host when it settles, and the next
    /// [`Self::settled_reads`] returns its reply.
    fn read(
        &mut self,
        _device: [u8; 32],
        _connection: [u8; 32],
        _id: RequestId,
        _read: HostRead,
    ) -> Option<Result<ReplyBody, Refusal>> {
        Some(Err(Refusal::Unsupported))
    }
    /// Stops read `id` of `connection`. It never settles. A directory creation that already
    /// started keeps what it created.
    fn cancel_read(&mut self, _connection: [u8; 32], _id: RequestId) {}
    /// The replies of reads that settled since the last call.
    fn settled_reads(&mut self) -> Vec<SettledRead> {
        Vec::new()
    }
    /// Whether `lease` still names a Pane's Terminal Session, ended or not, under its current
    /// control. A channel bound to a lease that is not live is closed.
    fn lease_live(&self, _lease: &Lease) -> bool {
        true
    }
    /// The connection's channel bound to `lease` is gone, as if the connection had ended for that
    /// Pane: its input state resets and the next claim starts afresh.
    fn unbound(&mut self, _connection: [u8; 32], _lease: &Lease) {}
    /// Every Terminal Session started from now on calls the notifier `notifier` makes from its
    /// session id when it has something to publish.
    fn set_notifier(&mut self, _notifier: SessionNotifiers) {}
    /// Drains the Pane `lease` controls for the channel bound to it, which `device` claimed. It
    /// may run on a connection's I/O thread; see [`Self::take_observed_change`].
    fn observe(&mut self, device: [u8; 32], lease: &Lease) -> Observed;
    /// Applies a client `View` to the Pane `lease` controls.
    fn view(&mut self, lease: &Lease, visible: bool, max_hz: u8);
    /// Marks the end of Input `serial`, so screens captured once its bytes reach the PTY carry it.
    fn input_written(&mut self, lease: &Lease, serial: u32);
    /// A channel was bound to `lease`, and its Input serials start at 1. Screens report no serial
    /// of an earlier binding.
    fn restart_input(&mut self, lease: &Lease);
    /// Reads Scrollback rows of the Pane `lease` controls. `reply` runs exactly once, on the
    /// Pane's terminal worker, unless this refuses the read.
    fn scrollback_page(
        &mut self,
        _device: [u8; 32],
        _connection: [u8; 32],
        _lease: &Lease,
        _read: ScrollbackRead,
        _reply: PageReply,
    ) -> Result<(), Refusal> {
        Err(Refusal::Unsupported)
    }
    /// Writes one command at the prompt of the Terminal Session the request's lease controls,
    /// as `Operation::Run` describes.
    fn run(&mut self, _request: RunRequest, _answer: RunAnswer) -> Result<(), Refusal> {
        Err(Refusal::Unsupported)
    }
    /// The receipt `device` holds for `run`. It never writes the run's command again.
    fn run_status(
        &mut self,
        _device: [u8; 32],
        _run: protocol::RunId,
    ) -> Result<protocol::RunReply, Refusal> {
        Err(Refusal::Unsupported)
    }
    /// Retires a settled receipt of `device`.
    fn run_acknowledge(&mut self, _device: [u8; 32], _run: protocol::RunId) -> Result<(), Refusal> {
        Err(Refusal::Unsupported)
    }
    /// Starts observing `pane`'s running Terminal Session, fenced by `session` when given. It
    /// changes nothing about the Pane.
    fn observe_start(
        &mut self,
        _pane: [u8; 32],
        _session: Option<[u8; 32]>,
    ) -> Result<protocol::ObserveReply, Refusal> {
        Err(Refusal::Unsupported)
    }
    /// Drains `pane` for an observer channel of `session`. It never carries effects or
    /// accessibility.
    fn observe_session(&mut self, _pane: [u8; 32], _session: [u8; 32]) -> Observed {
        Observed::Gone {
            last: None,
            event: PaneEvent::Ended,
        }
    }
    /// Whether `session` is still `pane`'s running Terminal Session.
    fn observer_live(&self, _pane: [u8; 32], _session: [u8; 32]) -> bool {
        false
    }
    /// Reads Scrollback rows of `session` for an observer. `reply` runs exactly once, on the
    /// Pane's terminal worker, unless this refuses the read.
    fn observer_scrollback_page(
        &mut self,
        _pane: [u8; 32],
        _session: [u8; 32],
        _read: ScrollbackRead,
        _reply: PageReply,
    ) -> Result<(), Refusal> {
        Err(Refusal::Unsupported)
    }
    /// The observers of each Terminal Session. A session without a visible observer publishes
    /// frames only for its controller.
    fn observers(&mut self, _observers: &BTreeMap<[u8; 32], Observers>) {}
    /// The size of `pane`'s running Terminal Session.
    fn session_size(&self, _pane: [u8; 32]) -> Option<PaneSize> {
        None
    }
    /// Receipts that settled since the last call, each with the channel that admitted it.
    fn take_settled_runs(&mut self) -> Vec<spaceterm_server::pane::run::Settled> {
        Vec::new()
    }
    fn disconnected(&mut self, connection: [u8; 32]);
    fn shutdown(&mut self) -> Result<(), ServerError>;
}

/// The size of a Session that a claim without a size starts: 120 by 40.
pub const DEFAULT_SIZE: PaneSize = PaneSize {
    columns: 120,
    rows: 40,
    cell_width: 8,
    cell_height: 16,
};

/// The observer channels of one Terminal Session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observers {
    /// Whether any of them is visible.
    pub visible: bool,
    /// The Client Identities observing it.
    pub clients: std::collections::BTreeSet<[u8; 32]>,
}

/// Answers a run admission once the Terminal Session's worker accepts or refuses it.
pub type RunAnswer = Box<dyn FnOnce(Result<protocol::RunReply, Refusal>) + Send>;

/// A run to admit, as `Operation::Run` names it on a channel.
pub struct RunRequest {
    pub device: [u8; 32],
    pub connection: [u8; 32],
    pub lease: pane::Lease,
    pub channel: protocol::Channel,
    pub run: protocol::RunId,
    pub command: String,
    pub session: [u8; 32],
}

/// How soon a host's timed work runs again while it waits on another thread for a journal or Git
/// write. A live terminal, and a read that settles, wake the host instead.
pub(super) const TICK: Duration = Duration::from_millis(10);

/// Makes the notifier of a Terminal Session a host starts, from its session id.
pub(crate) type SessionNotifiers = Arc<dyn Fn([u8; 32]) -> SubscriptionNotifier + Send + Sync>;

/// A host served over SpaceTerm Protocol connections. It binds each claimed channel to its lease,
/// opens and closes the channel in the connection's outbox, turns input into host commands, and
/// pushes each bound Pane's presentation when a client `View`, its connection or the host's wake
/// calls for it. Scrollback pages are read on the Pane's terminal worker and replied from there.
///
/// A Terminal Session's notifier asks each connection with a channel bound to that session to
/// publish, and that connection's I/O thread publishes it, so a frame never waits for the host's
/// thread. A session no channel is bound to rings the host's bell instead.
pub struct Served<H> {
    host: H,
    connections: BTreeMap<[u8; 32], Connection>,
    /// Rung when a Terminal Session that no channel presents has something to publish, or a
    /// request may have ended another channel's binding. Only a tick after a ring publishes.
    bell: Arc<Bell>,
    /// The connections each Terminal Session's notifier wakes.
    routes: Arc<Routes>,
    /// Reads whose replies wait for the host to settle them.
    reads: Vec<PendingRead>,
    /// The host's counters, which each channel's publication records into.
    counters: Arc<spaceterm_server::diagnostics::TerminalCounters>,
}

/// Which connections present each Terminal Session. [`Served`] rebuilds it under the
/// `PaneService` lock whenever a binding changes; notifiers read it from the session's worker.
/// Its lock is the last one taken: it calls only [`Outbox::request_publication`] and the bell.
struct Routes {
    sessions: std::sync::Mutex<BTreeMap<[u8; 32], Vec<Outbox>>>,
    bell: Arc<Bell>,
}

impl Routes {
    /// Wakes each connection presenting `session`, or the host when none does.
    fn notify(&self, session: [u8; 32]) {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        match sessions.get(&session) {
            Some(outboxes) => {
                for outbox in outboxes {
                    outbox.request_publication();
                }
            }
            None => {
                drop(sessions);
                self.bell.ring();
            }
        }
    }

    /// Replaces the routes. A session that loses its last route rings the host, so a notification
    /// that went to a connection which no longer presents it is still handled.
    fn replace(&self, routes: BTreeMap<[u8; 32], Vec<Outbox>>) {
        let orphaned = {
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            let orphaned = sessions.keys().any(|session| !routes.contains_key(session));
            *sessions = routes;
            orphaned
        };
        if orphaned {
            self.bell.ring();
        }
    }
}

/// Records a ring for the next tick and wakes the host to run it.
#[derive(Default)]
struct Bell {
    rung: std::sync::atomic::AtomicBool,
    wake: std::sync::OnceLock<Arc<dyn Wake>>,
}

impl Bell {
    fn ring(&self) {
        self.rung.store(true, std::sync::atomic::Ordering::Release);
        if let Some(wake) = self.wake.get() {
            wake.wake();
        }
    }

    fn take(&self) -> bool {
        self.rung.swap(false, std::sync::atomic::Ordering::AcqRel)
    }
}

/// A read that runs on the host: a directory read, a directory creation or a Worktree read.
pub enum HostRead {
    Directory(pane::directories::Operation),
    Creation(pane::directories::Creation),
    Worktree(spaceterm_server::worktree::Read),
}

/// The reply to read `id` of `connection`, which settled after [`HostCommands::read`] returned.
pub struct SettledRead {
    pub connection: [u8; 32],
    pub id: RequestId,
    pub reply: ReplyBody,
}

/// A read whose reply waits for the host to settle it.
#[derive(PartialEq, Eq)]
struct PendingRead {
    connection: [u8; 32],
    id: RequestId,
}

struct Connection {
    client: [u8; 32],
    outbox: Outbox,
    channels: BTreeMap<Channel, Bound>,
    observers: BTreeMap<Channel, Watch>,
}

/// An observer channel: it presents one Terminal Session without its lease.
struct Watch {
    pane: [u8; 32],
    session: [u8; 32],
    visible: bool,
    publication: Publication,
}

/// A channel bound to the lease its claim attached.
struct Bound {
    /// The Pane the claim named.
    pane: [u8; 32],
    lease: Lease,
    paste: Option<PendingPaste>,
    publication: Publication,
}

/// The chunks of one paste received so far. Nothing reaches the PTY before its last chunk.
struct PendingPaste {
    generation: u64,
    text: String,
    too_large: bool,
    /// A chunk was discarded, so the whole paste is.
    aborted: Option<Refusal>,
}

impl<H: HostCommands> Served<H> {
    pub fn new(host: H) -> Self {
        let bell = Arc::<Bell>::default();
        Self {
            counters: host.terminal_counters(),
            host,
            connections: BTreeMap::new(),
            reads: Vec::new(),
            routes: Arc::new(Routes {
                sessions: std::sync::Mutex::default(),
                bell: Arc::clone(&bell),
            }),
            bell,
        }
    }

    #[cfg(test)]
    pub(super) fn host(&self) -> &H {
        &self.host
    }

    /// Rings the bell when an observation drained a change that only the host's timed work
    /// pushes, so the host's next tick carries it.
    fn ring_for_observed_change(&mut self) {
        if self.host.take_observed_change() {
            self.bell.ring();
        }
    }

    /// Rebuilds the routes from the current bindings. Call it after every change to them.
    fn reroute(&self) {
        let mut routes: BTreeMap<[u8; 32], Vec<Outbox>> = BTreeMap::new();
        for connection in self.connections.values() {
            let sessions = connection
                .channels
                .values()
                .map(|bound| bound.lease.session)
                .chain(connection.observers.values().map(|watch| watch.session));
            for session in sessions {
                routes
                    .entry(session)
                    .or_default()
                    .push(connection.outbox.clone());
            }
        }
        self.routes.replace(routes);
    }

    /// Tells the host which Terminal Sessions observers show, after every change to them.
    fn update_observers(&mut self) {
        let mut observers: BTreeMap<[u8; 32], Observers> = BTreeMap::new();
        for connection in self.connections.values() {
            for watch in connection.observers.values() {
                let session = observers.entry(watch.session).or_default();
                session.visible |= watch.visible;
                session.clients.insert(connection.client);
            }
        }
        self.host.observers(&observers);
    }

    fn watch(&mut self, peer: &Peer, channel: Channel) -> Option<&mut Watch> {
        self.connections
            .get_mut(&peer.connection)?
            .observers
            .get_mut(&channel)
    }

    fn observing(&self, peer: &Peer, channel: Channel) -> bool {
        self.connections
            .get(&peer.connection)
            .is_some_and(|connection| connection.observers.contains_key(&channel))
    }

    /// Opens an observer channel on `pane`. A Pane without a running Session leaves the channel
    /// closed.
    fn observe(
        &mut self,
        peer: &Peer,
        channel: Channel,
        pane: [u8; 32],
        session: Option<[u8; 32]>,
    ) -> Result<ReplyBody, Refusal> {
        let reply = self.host.observe_start(pane, session);
        let Some(connection) = self.connections.get_mut(&peer.connection) else {
            return Err(Refusal::Stale);
        };
        let session = match &reply {
            Ok(protocol::ObserveReply::Observing { session, .. }) => *session,
            _ => {
                connection.outbox.close(channel);
                return reply.map(ReplyBody::Observe);
            }
        };
        let publication =
            Publication::bind(&connection.outbox, channel, Arc::clone(&self.counters))
                .map_err(|_| Refusal::Capacity)?;
        connection.observers.insert(
            channel,
            Watch {
                pane,
                session,
                visible: false,
                publication,
            },
        );
        self.reroute();
        self.update_observers();
        reply.map(ReplyBody::Observe)
    }

    /// Closes an observer channel.
    fn unwatch(&mut self, peer: &Peer, channel: Channel) {
        if let Some(connection) = self.connections.get_mut(&peer.connection)
            && connection.observers.remove(&channel).is_some()
        {
            connection.outbox.close(channel);
            self.reroute();
            self.update_observers();
        }
    }

    /// Pushes what changed for one observer channel, and forgets it once its Session ended.
    fn publish_watch(&mut self, connection: [u8; 32], channel: Channel) {
        let Some((pane, session)) = self
            .connections
            .get(&connection)
            .and_then(|connection| connection.observers.get(&channel))
            .map(|watch| (watch.pane, watch.session))
        else {
            return;
        };
        let observed = self.host.observe_session(pane, session);
        let Some(connection) = self.connections.get_mut(&connection) else {
            return;
        };
        if let Some(watch) = connection.observers.get_mut(&channel)
            && !watch.publication.publish(observed)
        {
            connection.observers.remove(&channel);
            self.reroute();
            self.update_observers();
        }
    }

    /// Pushes what changed for one bound channel, and forgets the binding once it ended.
    fn publish_channel(&mut self, connection: [u8; 32], channel: Channel) {
        let Some(bound) = self.connections.get(&connection) else {
            return;
        };
        let client = bound.client;
        let Some(lease) = bound.channels.get(&channel).map(|bound| bound.lease) else {
            return;
        };
        let observed = self.host.observe(client, &lease);
        let Some(connection) = self.connections.get_mut(&connection) else {
            return;
        };
        if let Some(bound) = connection.channels.get_mut(&channel)
            && !bound.publication.publish(observed)
        {
            connection.channels.remove(&channel);
            self.reroute();
        }
    }

    /// Pushes what changed for every bound channel.
    fn publish_all(&mut self) {
        let bound: Vec<_> = self
            .connections
            .iter()
            .flat_map(|(id, connection)| connection.channels.keys().map(|channel| (*id, *channel)))
            .collect();
        for (connection, channel) in bound {
            self.publish_channel(connection, channel);
        }
        let watched: Vec<_> = self
            .connections
            .iter()
            .flat_map(|(id, connection)| connection.observers.keys().map(|channel| (*id, *channel)))
            .collect();
        for (connection, channel) in watched {
            self.publish_watch(connection, channel);
        }
    }

    /// Sends each settled run's receipt to the channel that admitted it. A channel that closed
    /// meanwhile no longer needs it; its client resolves the run with `RunStatus`.
    fn deliver_runs(&mut self) {
        for settled in self.host.take_settled_runs() {
            if let Some(connection) = self.connections.get(&settled.connection) {
                let _ = connection.outbox.pane(
                    settled.channel,
                    PaneEvent::RunFinished(Box::new(settled.receipt)),
                );
            }
        }
    }

    /// Starts a read and replies when it settles.
    fn read(&mut self, peer: &Peer, id: RequestId, read: HostRead) -> Dispatch {
        match self.host.read(peer.client, peer.connection, id, read) {
            Some(result) => Dispatch::Reply(Box::new(result)),
            None => {
                self.reads.push(PendingRead {
                    connection: peer.connection,
                    id,
                });
                Dispatch::Deferred
            }
        }
    }

    /// Replies to each read that settled. A read cancelled or disconnected meanwhile has no
    /// reply left to send.
    fn answer_reads(&mut self) {
        for settled in self.host.settled_reads() {
            let read = PendingRead {
                connection: settled.connection,
                id: settled.id,
            };
            let Some(index) = self.reads.iter().position(|pending| *pending == read) else {
                continue;
            };
            self.reads.swap_remove(index);
            if let Some(connection) = self.connections.get(&read.connection) {
                // A closing connection no longer needs the reply.
                let _ = connection
                    .outbox
                    .reply_or_refuse(read.id, Ok(settled.reply));
            }
        }
    }

    fn lease(&self, peer: &Peer, channel: Channel) -> Result<Lease, Refusal> {
        self.connections
            .get(&peer.connection)
            .and_then(|connection| connection.channels.get(&channel))
            .map(|bound| bound.lease)
            .ok_or(Refusal::Stale)
    }

    fn command(&mut self, peer: &Peer, request: Request) -> Reply {
        self.host.request(peer.client, peer.connection, request)
    }

    /// Runs a command on the channel's lease.
    fn on_channel(
        &mut self,
        peer: &Peer,
        channel: Channel,
        request: impl FnOnce(Lease) -> Request,
    ) -> Result<ReplyBody, Refusal> {
        let lease = self.lease(peer, channel)?;
        let result = body(self.command(peer, request(lease)));
        // A lease the host no longer knows frees the channel.
        if !self.host.lease_live(&lease) {
            self.unbind(peer, channel);
        }
        result
    }

    /// Ends every channel whose lease the host no longer knows: a Session that ended, a closed
    /// Pane, End All Sessions, or control another client took. Each receives its last event, so
    /// timed work that retired a Pane never closes a channel silently.
    fn sweep(&mut self) {
        let dead: Vec<_> = self
            .connections
            .iter()
            .flat_map(|(id, connection)| {
                connection
                    .channels
                    .iter()
                    .filter(|(_, bound)| !self.host.lease_live(&bound.lease))
                    .map(move |(channel, _)| (*id, *channel))
            })
            .collect();
        let unwatched: Vec<_> = self
            .connections
            .iter()
            .flat_map(|(id, connection)| {
                connection
                    .observers
                    .iter()
                    .filter(|(_, watch)| !self.host.observer_live(watch.pane, watch.session))
                    .map(move |(channel, _)| (*id, *channel))
            })
            .collect();
        for (connection, channel) in unwatched {
            // Publication delivers the last event and forgets the channel.
            self.publish_watch(connection, channel);
        }
        for (connection, channel) in dead {
            self.publish_channel(connection, channel);
            // A host that still answers `Live` for a lease it no longer knows is not trusted.
            if let Some(connection) = self.connections.get_mut(&connection)
                && connection.channels.remove(&channel).is_some()
            {
                connection.outbox.close(channel);
                self.reroute();
            }
        }
    }

    /// Forgets the channel's binding and closes it in the outbox.
    fn unbind(&mut self, peer: &Peer, channel: Channel) {
        if let Some(connection) = self.connections.get_mut(&peer.connection) {
            connection.channels.remove(&channel);
            connection.outbox.close(channel);
            self.reroute();
        }
    }

    fn claim(&mut self, peer: &Peer, claim: Claim) -> Result<ReplyBody, Refusal> {
        // A claim without a size keeps the running Session's size, and starts a fresh one at
        // the server default.
        let size = claim
            .size
            .or_else(|| self.host.session_size(claim.pane))
            .unwrap_or(DEFAULT_SIZE);
        let channel = claim.channel;
        let pane = claim.pane;
        // A new claim of a Pane this connection already holds replaces that binding. The host
        // resets the Pane's input state, focus and transfers as a disconnect would, so recovery
        // over the shared connection starts afresh.
        if let Some(connection) = self.connections.get_mut(&peer.connection) {
            let replaced: Vec<_> = connection
                .channels
                .iter()
                .filter(|(_, bound)| bound.pane == pane)
                .map(|(channel, bound)| (*channel, bound.lease))
                .collect();
            for (old, lease) in replaced {
                connection.channels.remove(&old);
                connection.outbox.close(old);
                self.host.unbound(peer.connection, &lease);
            }
            self.reroute();
        }
        let reply = self.command(
            peer,
            Request::ClaimPane {
                attachment: claim.attachment,
                pane: claim.pane,
                claim: claim.claim,
                size,
                style: claim.style,
            },
        );
        let result = body(reply);
        match &result {
            Ok(ReplyBody::Claim(ClaimReply::Attached { lease })) => {
                self.bind(peer, channel, pane, *lease)?;
            }
            // A claim that ends without binding leaves the channel closed.
            _ => {
                if let Some(connection) = self.connections.get(&peer.connection) {
                    connection.outbox.close(channel);
                }
            }
        }
        result
    }

    /// Binds `channel` to the lease its claim attached and opens it in the connection's outbox.
    /// A Pane is bound to at most one channel of a connection, so an older binding of the same
    /// Terminal Session closes.
    fn bind(
        &mut self,
        peer: &Peer,
        channel: Channel,
        pane: [u8; 32],
        lease: Lease,
    ) -> Result<(), Refusal> {
        let Some(connection) = self.connections.get_mut(&peer.connection) else {
            return Err(Refusal::Stale);
        };
        let replaced: Vec<_> = connection
            .channels
            .iter()
            .filter(|(_, bound)| bound.lease.session == lease.session)
            .map(|(channel, _)| *channel)
            .collect();
        for old in replaced {
            connection.channels.remove(&old);
            connection.outbox.close(old);
        }
        let publication =
            Publication::bind(&connection.outbox, channel, Arc::clone(&self.counters))
                .map_err(|_| Refusal::Capacity)?;
        connection.channels.insert(
            channel,
            Bound {
                pane,
                lease,
                paste: None,
                publication,
            },
        );
        self.reroute();
        // The new channel numbers its Input from 1, before any Input of it reaches the session.
        self.host.restart_input(&lease);
        // The Terminal Session publishes nothing until the client's first `View` shows it.
        self.host
            .view(&lease, false, crate::terminal::session::DEFAULT_FRAME_RATE);
        Ok(())
    }

    fn report(&self, peer: &Peer, channel: Channel, serial: u32, outcome: InputOutcome) {
        if let Some(connection) = self.connections.get(&peer.connection) {
            // A closing connection no longer needs the report.
            let _ = connection
                .outbox
                .pane(channel, PaneEvent::Input { serial, outcome });
        }
    }

    fn bound(&mut self, peer: &Peer, channel: Channel) -> Option<&mut Bound> {
        self.connections
            .get_mut(&peer.connection)?
            .channels
            .get_mut(&channel)
    }

    /// Applies one Input message's events in order and reports a discard or a settled paste. The
    /// first refusal discards that event and every later one, and aborts a paste in progress.
    fn input_message(&mut self, peer: &Peer, input: protocol::Input) {
        let (channel, serial) = (input.channel, input.serial);
        let outcome = self.apply(peer, input);
        // A paste waiting for later chunks or for confirmation has written nothing yet, so its
        // serial waits for the message that writes it. Even a discarded message may have applied
        // the events before its refusal.
        let waiting = matches!(
            outcome,
            Some(InputOutcome::Paste(PasteOutcome::ConfirmationRequired(_)))
        ) || self
            .bound(peer, channel)
            .is_some_and(|bound| bound.paste.is_some());
        if let Some(outcome) = outcome {
            self.report(peer, channel, serial, outcome);
        }
        if !waiting && let Ok(lease) = self.lease(peer, channel) {
            self.host.input_written(&lease, serial);
        }
    }

    fn apply(&mut self, peer: &Peer, input: protocol::Input) -> Option<InputOutcome> {
        let protocol::Input {
            channel,
            generation,
            events,
            ..
        } = input;
        if self.observing(peer, channel) {
            return Some(InputOutcome::Discarded(Refusal::Unsupported));
        }
        let Ok(lease) = self.lease(peer, channel) else {
            return Some(InputOutcome::Discarded(Refusal::Stale));
        };
        let mut events = events.into_iter().peekable();
        let mut outcome = None;
        while let Some(event) = events.next() {
            if !matches!(event, InputEvent::Paste { .. })
                && let Some(bound) = self.bound(peer, channel)
                && let Some(paste) = &mut bound.paste
            {
                // A paste's chunks travel back to back. Anything between them means a chunk is
                // missing.
                paste.aborted.get_or_insert(Refusal::Stale);
            }
            let request = |input| Request::Input {
                lease,
                generation,
                input,
            };
            let result = match event {
                InputEvent::Focus(target) => body(self.command(
                    peer,
                    Request::Focus {
                        lease,
                        generation,
                        target,
                    },
                )),
                InputEvent::Key(key) => {
                    let mut keys = vec![key];
                    while let Some(InputEvent::Key(_)) = events.peek() {
                        if let Some(InputEvent::Key(key)) = events.next() {
                            keys.push(key);
                        }
                    }
                    body(self.command(
                        peer,
                        Request::Keys {
                            lease,
                            generation,
                            keys,
                        },
                    ))
                }
                InputEvent::Pointer(pointer) => {
                    body(self.command(peer, request(InteractiveInput::Pointer(pointer))))
                }
                InputEvent::Wheel(wheel) => {
                    body(self.command(peer, request(InteractiveInput::Wheel(wheel))))
                }
                InputEvent::CopyOrForward { modifiers } => {
                    body(self.command(peer, request(InteractiveInput::CopyOrForward { modifiers })))
                }
                InputEvent::Resize(size) => {
                    body(self.command(peer, Request::Resize { lease, size }))
                }
                InputEvent::ResolvePaste { id, confirm } => {
                    match self.command(
                        peer,
                        request(InteractiveInput::ResolvePaste { id, confirm }),
                    ) {
                        Reply::Paste(settled) => {
                            outcome = Some(InputOutcome::Paste(settled));
                            Ok(ReplyBody::Done)
                        }
                        reply => body(reply),
                    }
                }
                InputEvent::Paste { text, last } => {
                    match self.paste(peer, channel, lease, generation, text, last) {
                        Ok(Some(settled)) => {
                            outcome = Some(settled);
                            Ok(ReplyBody::Done)
                        }
                        Ok(None) => Ok(ReplyBody::Done),
                        Err(refusal) => Err(refusal),
                    }
                }
            };
            if let Err(refusal) = result {
                if let Some(bound) = self.bound(peer, channel)
                    && let Some(paste) = &mut bound.paste
                {
                    paste.aborted.get_or_insert(refusal);
                }
                return Some(InputOutcome::Discarded(refusal));
            }
        }
        outcome
    }

    /// Adds one paste chunk. The last chunk settles the whole paste: written, rejected as too
    /// large, or discarded because an earlier chunk was.
    fn paste(
        &mut self,
        peer: &Peer,
        channel: Channel,
        lease: Lease,
        generation: u64,
        text: String,
        last: bool,
    ) -> Result<Option<InputOutcome>, Refusal> {
        let bound = self.bound(peer, channel).ok_or(Refusal::Stale)?;
        let paste = bound.paste.get_or_insert_with(|| PendingPaste {
            generation,
            text: String::new(),
            too_large: false,
            aborted: None,
        });
        if paste.generation != generation {
            paste.aborted.get_or_insert(Refusal::Stale);
        }
        if paste.text.len() + text.len() > protocol::limits::MAX_PASTE_BYTES {
            paste.too_large = true;
            paste.text = String::new();
        } else if !paste.too_large {
            paste.text.push_str(&text);
        }
        if !last {
            return Ok(None);
        }
        let Some(paste) = bound.paste.take() else {
            return Ok(None);
        };
        if let Some(refusal) = paste.aborted {
            return Err(refusal);
        }
        if paste.too_large {
            return Ok(Some(InputOutcome::Paste(PasteOutcome::Rejected(
                pane::PasteRejection::TooLarge {
                    limit: protocol::limits::MAX_PASTE_BYTES as u32,
                },
            ))));
        }
        match self.command(
            peer,
            Request::Input {
                lease,
                generation,
                input: InteractiveInput::Paste { text: paste.text },
            },
        ) {
            Reply::Paste(outcome) => Ok(Some(InputOutcome::Paste(outcome))),
            Reply::Refused(refusal) => Err(refusal),
            _ => Err(Refusal::Failed),
        }
    }
}

/// The bytes `reply` takes on the wire as a reply, or `None` when it
/// exceeds the protocol's bounds.
#[cfg(test)]
pub(super) fn wire_bytes(reply: Reply) -> Option<usize> {
    let message = protocol::ServerMessage::Reply(Box::new(protocol::Reply {
        id: RequestId(1),
        result: body(reply),
    }));
    protocol::frame::encode_server(&message)
        .ok()
        .map(|wire| wire.bytes())
}

/// The protocol reply to a host command.
fn body(reply: Reply) -> Result<ReplyBody, Refusal> {
    Ok(match reply {
        Reply::Refused(refusal) => return Err(refusal),
        Reply::Unchanged | Reply::Closed => ReplyBody::Done,
        Reply::Attached { lease } => ReplyBody::Claim(ClaimReply::Attached { lease }),
        Reply::Takeover {
            session_ended,
            target,
            controller,
        } => ReplyBody::Claim(ClaimReply::Takeover(protocol::Takeover {
            session_ended,
            target,
            controller,
        })),
        Reply::Restored {
            fresh,
            session_ended,
            recovery,
            directory_unavailable,
            server,
            workspace,
            tab,
            pane,
        } => ReplyBody::Claim(ClaimReply::Restored(Box::new(protocol::Restored {
            fresh,
            session_ended,
            recovery,
            directory_unavailable,
            server,
            workspace,
            tab,
            pane,
        }))),
        Reply::Outcome(outcome) => ReplyBody::Outcome(outcome),
        Reply::GraphicsChunk(chunk) => ReplyBody::Graphics(chunk),
        Reply::Accessibility(chunk) => ReplyBody::Accessibility(chunk),
        Reply::MutationPending | Reply::Inventory { .. } => return Err(Refusal::Failed),
        Reply::Paste(_) => return Err(Refusal::Failed),
    })
}

impl<H: HostCommands> ConnectionService for Served<H> {
    fn restore(&mut self, store: OrganizationStore) -> Result<(), ServerError> {
        self.host.restore(store)
    }
    fn admitted(&mut self, peer: &Peer, name: control::ClientName, outbox: Outbox) {
        self.host.admitted(peer.client, name);
        self.host.connected(peer.client, peer.connection, &outbox);
        self.connections.insert(
            peer.connection,
            Connection {
                client: peer.client,
                outbox,
                channels: BTreeMap::new(),
                observers: BTreeMap::new(),
            },
        );
    }
    fn revoked(&mut self, client: [u8; 32]) {
        self.host.revoked(client);
        self.bell.ring();
    }
    fn release_control(&mut self, client: [u8; 32]) {
        self.host.release_control(client);
        self.bell.ring();
    }
    fn holds(&self, client: [u8; 32]) -> bool {
        self.host.holds(client)
    }
    fn request(&mut self, peer: &Peer, id: RequestId, operation: Operation) -> Dispatch {
        let dispatch = self.dispatch(peer, id, operation);
        self.deliver_runs();
        // A claim, close, release or edit may have ended another channel's binding.
        self.bell.ring();
        dispatch
    }
    fn input(&mut self, peer: &Peer, input: protocol::Input) {
        self.input_message(peer, input);
    }
    /// The connection already applied `view` to the outbox. A revealed channel receives the
    /// current screen again.
    fn view(&mut self, peer: &Peer, view: protocol::View) {
        if let Some(watch) = self.watch(peer, view.channel) {
            watch.publication.view(view.visible);
            watch.visible = view.visible;
            self.update_observers();
            if view.visible {
                self.publish_watch(peer.connection, view.channel);
            }
            return;
        }
        let Some(bound) = self.bound(peer, view.channel) else {
            return;
        };
        bound.publication.view(view.visible);
        let lease = bound.lease;
        self.host.view(&lease, view.visible, view.max_hz);
        if view.visible {
            self.publish_channel(peer.connection, view.channel);
            self.ring_for_observed_change();
        }
    }
    fn effect_done(&mut self, peer: &Peer, done: protocol::EffectDone) {
        if let Ok(lease) = self.lease(peer, done.channel) {
            self.command(
                peer,
                Request::CompleteClipboard {
                    lease,
                    generation: done.generation,
                    id: done.effect,
                    completion: done.completion,
                },
            );
        }
    }
    fn acknowledge(&mut self, peer: &Peer, sequence: u64) {
        self.host.acknowledge(peer.client, sequence);
    }
    /// Terminal Sessions wake the connections presenting them, or ring the bell, which wakes the
    /// host to tick and publish.
    fn ticker(&mut self, wake: Arc<dyn Wake>) {
        let _ = self.bell.wake.set(Arc::clone(&wake));
        let routes = Arc::clone(&self.routes);
        self.host.set_notifier(Arc::new(move |session| {
            let routes = Arc::clone(&routes);
            Arc::new(move || routes.notify(session))
        }));
        self.host.ticker(wake);
    }
    /// Pushes what changed for each channel of this connection, on its I/O thread. A closing
    /// connection rings the bell instead, so the host's tick handles what the session changed.
    fn publish(&mut self, peer: &Peer) {
        let Some(connection) = self.connections.get(&peer.connection) else {
            return;
        };
        if connection.outbox.is_closing() {
            self.bell.ring();
            return;
        }
        let channels: Vec<_> = connection.channels.keys().copied().collect();
        let watched: Vec<_> = connection.observers.keys().copied().collect();
        for channel in channels {
            self.publish_channel(peer.connection, channel);
        }
        for channel in watched {
            self.publish_watch(peer.connection, channel);
        }
        self.deliver_runs();
        self.ring_for_observed_change();
    }
    /// Pushes what every bound Pane published when the bell rang, then runs the host's timed
    /// work when it is due. A host deadline alone publishes nothing.
    fn tick(&mut self, now: std::time::Instant) -> Option<std::time::Instant> {
        // Publication ends each channel whose Pane is gone with its last event, and hands an
        // ended shell's final screen over before the host's timed work retires its Pane.
        if self.bell.take() {
            self.publish_all();
            // This tick already carries what the observations drained.
            self.host.take_observed_change();
            self.host.rung();
        }
        let host = self.host.tick(now);
        self.deliver_runs();
        self.sweep();
        self.ring_for_observed_change();
        self.answer_reads();
        host
    }
    fn stopping(&mut self) {
        for read in std::mem::take(&mut self.reads) {
            if let Some(connection) = self.connections.get(&read.connection) {
                let _ = connection.outbox.reply(read.id, Err(Refusal::Failed));
            }
        }
        self.host.stopping();
    }
    /// The client abandoned a read. It replies `Stale` at once, and the host stops the read.
    fn cancel(&mut self, peer: &Peer, id: RequestId) {
        let read = PendingRead {
            connection: peer.connection,
            id,
        };
        let Some(index) = self.reads.iter().position(|pending| *pending == read) else {
            return;
        };
        self.reads.swap_remove(index);
        self.host.cancel_read(peer.connection, id);
        if let Some(connection) = self.connections.get(&peer.connection) {
            let _ = connection.outbox.reply(id, Err(Refusal::Stale));
        }
    }
    fn disconnected(&mut self, peer: &Peer) {
        self.reads.retain(|read| read.connection != peer.connection);
        self.connections.remove(&peer.connection);
        self.reroute();
        self.update_observers();
        self.host.disconnected(peer.connection);
    }
    fn shutdown(&mut self) -> Result<(), ServerError> {
        self.host.shutdown()
    }
    fn sessions(&self) -> Vec<[u8; 32]> {
        self.host.sessions()
    }
}

impl<H: HostCommands> Served<H> {
    /// Answers one request.
    fn dispatch(&mut self, peer: &Peer, id: RequestId, operation: Operation) -> Dispatch {
        if let Operation::ScrollbackPage {
            channel,
            epoch,
            start,
            rows,
        } = operation
        {
            let read = ScrollbackRead { epoch, start, rows };
            return self.scrollback_page(peer, id, channel, read);
        }
        if let Some(channel) = channel_of(&operation)
            && self.observing(peer, channel)
        {
            // An observer channel only presents and reads. Release ends the observation.
            return Dispatch::reply(match operation {
                Operation::Release { .. } => {
                    self.unwatch(peer, channel);
                    Ok(ReplyBody::Done)
                }
                _ => Err(Refusal::Unsupported),
            });
        }
        Dispatch::reply(match operation {
            Operation::Claim(claim) => self.claim(peer, claim),
            Operation::ReleaseAttachment { attachment } => {
                body(self.command(peer, Request::ReleaseAttachment { attachment }))
            }
            Operation::Release { channel } => {
                let result = self.on_channel(peer, channel, |lease| Request::Release { lease });
                self.unbind(peer, channel);
                result
            }
            Operation::Close { channel } => {
                let result = self.on_channel(peer, channel, |lease| Request::Close { lease });
                if result.is_ok() {
                    self.unbind(peer, channel);
                }
                result
            }
            Operation::ClearScrollback { channel } => self.on_channel(peer, channel, |lease| {
                Request::ClearScreenAndScrollback { lease }
            }),
            Operation::Style { channel, style } => {
                self.on_channel(peer, channel, |lease| Request::Style { lease, style })
            }
            Operation::Mutate { sequence, mutation } => {
                return self.host.mutate(
                    peer.client,
                    peer.connection,
                    id,
                    sequence,
                    Some(mutation),
                );
            }
            Operation::Resolve { sequence } => {
                return self
                    .host
                    .mutate(peer.client, peer.connection, id, sequence, None);
            }
            Operation::Graphics {
                channel,
                image,
                offset,
            } => self.on_channel(peer, channel, |lease| Request::GraphicsChunk {
                lease,
                image,
                offset: offset as usize,
            }),
            Operation::Accessibility {
                channel,
                revision,
                offset,
                base,
            } => self.on_channel(peer, channel, |lease| Request::Accessibility {
                lease,
                revision,
                offset: offset as usize,
                base,
            }),
            Operation::AccessibilitySelection {
                channel,
                focus_generation,
                selection,
            } => self.on_channel(peer, channel, |lease| Request::AccessibilitySelection {
                lease,
                focus_generation,
                selection,
            }),
            Operation::EndAccessibility { channel } => self.on_channel(peer, channel, |lease| {
                Request::EndAccessibilityDemand { lease }
            }),
            Operation::DirectoryRead(operation) => {
                return self.read(peer, id, HostRead::Directory(operation));
            }
            Operation::DirectoryCreate(creation) => {
                return self.read(peer, id, HostRead::Creation(creation));
            }
            Operation::WorktreeRead(read) => {
                return self.read(peer, id, HostRead::Worktree(read));
            }
            Operation::Shutdown => body(self.command(peer, Request::Shutdown)),
            // Scrollback pages reply from the terminal worker. Direct-network administration never
            // reaches the service.
            Operation::ScrollbackPage { .. } | Operation::Direct(_) => Err(Refusal::Unsupported),
            Operation::Observe {
                channel,
                pane,
                session,
            } => self.observe(peer, channel, pane, session),
            Operation::Run {
                channel,
                run,
                command,
                session,
            } => {
                let lease = match self.lease(peer, channel) {
                    Ok(lease) => lease,
                    Err(refusal) => return Dispatch::reply(Err(refusal)),
                };
                let Some(connection) = self.connections.get(&peer.connection) else {
                    return Dispatch::reply(Err(Refusal::Stale));
                };
                let outbox = connection.outbox.clone();
                let answer = Box::new(move |result: Result<protocol::RunReply, Refusal>| {
                    let _ = outbox.reply_or_refuse(id, result.map(ReplyBody::Run));
                });
                return match self.host.run(
                    RunRequest {
                        device: peer.client,
                        connection: peer.connection,
                        lease,
                        channel,
                        run,
                        command,
                        session,
                    },
                    answer,
                ) {
                    Ok(()) => Dispatch::Deferred,
                    Err(refusal) => Dispatch::reply(Err(refusal)),
                };
            }
            Operation::RunStatus { run } => {
                self.host.run_status(peer.client, run).map(ReplyBody::Run)
            }
            Operation::RunAcknowledge { run } => self
                .host
                .run_acknowledge(peer.client, run)
                .map(|()| ReplyBody::Done),
        })
    }

    /// Reads a Scrollback page for the channel's controlling client. The reply comes from the
    /// Pane's terminal worker once it read the rows, so no lock is held while it reads.
    fn scrollback_page(
        &mut self,
        peer: &Peer,
        id: RequestId,
        channel: Channel,
        read: ScrollbackRead,
    ) -> Dispatch {
        let Some(connection) = self.connections.get(&peer.connection) else {
            return Dispatch::reply(Err(Refusal::Stale));
        };
        let outbox = connection.outbox.clone();
        let watched = connection
            .observers
            .get(&channel)
            .map(|watch| (watch.pane, watch.session));
        let reply = Box::new(move |result: Result<protocol::Page, Refusal>| {
            // A connection that ended no longer owes the reply.
            let _ = outbox.reply_or_refuse(id, result.map(ReplyBody::Page));
        });
        if let Some((pane, session)) = watched {
            return match self
                .host
                .observer_scrollback_page(pane, session, read, reply)
            {
                Ok(()) => Dispatch::Deferred,
                Err(refusal) => Dispatch::reply(Err(refusal)),
            };
        }
        let Ok(lease) = self.lease(peer, channel) else {
            return Dispatch::reply(Err(Refusal::Stale));
        };
        match self
            .host
            .scrollback_page(peer.client, peer.connection, &lease, read, reply)
        {
            Ok(()) => Dispatch::Deferred,
            Err(refusal) => Dispatch::reply(Err(refusal)),
        }
    }
}

/// The channel an operation acts on, other than a claim, an observation or a Scrollback page.
fn channel_of(operation: &Operation) -> Option<Channel> {
    match operation {
        Operation::Release { channel }
        | Operation::Close { channel }
        | Operation::ClearScrollback { channel }
        | Operation::Style { channel, .. }
        | Operation::Graphics { channel, .. }
        | Operation::Accessibility { channel, .. }
        | Operation::AccessibilitySelection { channel, .. }
        | Operation::EndAccessibility { channel }
        | Operation::Run { channel, .. } => Some(*channel),
        _ => None,
    }
}

#[cfg(test)]
mod routes_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn routes() -> (Routes, Arc<AtomicUsize>) {
        let rings = Arc::new(AtomicUsize::new(0));
        let bell = Arc::<Bell>::default();
        let _ = bell.wake.set(Arc::new({
            let rings = Arc::clone(&rings);
            move || {
                rings.fetch_add(1, Ordering::SeqCst);
            }
        }));
        let routes = Routes {
            sessions: std::sync::Mutex::default(),
            bell,
        };
        (routes, rings)
    }

    /// A session that loses its last route rings the host, so a notification that went to the
    /// connection before it stopped presenting the session is still handled. Moving or adding
    /// routes rings nothing.
    #[test]
    fn only_a_session_that_loses_its_last_route_rings_the_host() {
        let (routes, rings) = routes();
        let (first, second) = (Outbox::new(Arc::new(|| {})), Outbox::new(Arc::new(|| {})));
        routes.replace(BTreeMap::from([([1; 32], vec![first.clone()])]));
        routes.replace(BTreeMap::from([([1; 32], vec![second.clone()])]));
        assert_eq!(
            rings.load(Ordering::SeqCst),
            0,
            "a moved route rings nothing"
        );

        routes.replace(BTreeMap::new());
        assert_eq!(rings.load(Ordering::SeqCst), 1, "the orphaned session rang");
        assert!(!first.take_publication() && !second.take_publication());

        routes.notify([1; 32]);
        assert_eq!(
            rings.load(Ordering::SeqCst),
            2,
            "an unrouted session rings the host"
        );
        assert!(!second.take_publication());
    }
}
