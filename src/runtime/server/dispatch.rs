//! Request dispatch, and the connection-facing service boundary of the Development host.
use super::*;
use spaceterm_server::connection::outbox::Outbox;
use spaceterm_server::connection::service::{Dispatch, PaneService as ConnectionService, Peer};
use spaceterm_server::protocol::{
    self, Channel, Claim, ClaimReply, InputEvent, InputOutcome, Operation, PaneEvent, ReplyBody,
    RequestId,
    interim::{Interim, InterimReply},
};
use std::collections::BTreeMap;

impl DevelopmentPaneHost {
    pub(super) fn handle(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        request: Request,
    ) -> Result<Reply, Refusal> {
        let (request, changes) = match request {
            Request::PollChanges { lease, base } => (Request::Poll { lease }, Some(base)),
            request => (request, None),
        };
        self.recover()?;
        self.retain_directory()?;
        if let Request::DirectoryRead(request) = request {
            return Ok(Reply::DirectoryRead(
                self.directories.request(connection, request),
            ));
        }
        if let Request::DirectoryCreate(request) = request {
            return Ok(Reply::DirectoryCreate(
                self.directories.create(connection, request),
            ));
        }
        match request {
            Request::RepairDirectory {
                target,
                replacement,
                size,
                style,
            } => {
                return self.repair_directory(device, connection, target, replacement, size, style);
            }
            Request::StartRestored {
                server,
                pane,
                size,
                style,
            } => {
                if server != self.boot
                    || self
                        .organization
                        .as_ref()
                        .is_none_or(|org| org.pane != pane)
                {
                    return Err(Refusal::Stale);
                }
                if self.pane.is_some() {
                    return self.select(device, connection, false, size, None, style);
                }
                return self.select(device, connection, true, size, None, style);
            }
            Request::Attach {
                create,
                size,
                resume,
                style,
            } => {
                // Legacy clients cannot render a Takeover Screen or request takeover.
                if let (Some(pane), Some(old)) = (&self.pane, resume)
                    && (old.server, old.session, old.control)
                        != (pane.lease.server, pane.lease.session, pane.lease.control)
                {
                    return Err(Refusal::Stale);
                }
                let reply = self.select(
                    device,
                    connection,
                    create,
                    size,
                    resume.map(Into::into),
                    style,
                )?;
                return if matches!(reply, Reply::Takeover { .. }) {
                    Err(Refusal::Controlled)
                } else {
                    Ok(reply)
                };
            }
            Request::Select {
                create,
                size,
                resume,
                style,
            } => {
                return self.select(device, connection, create, size, resume, style);
            }
            Request::TakeOver {
                target,
                size,
                style,
            } => {
                self.reconcile_selection()?;
                let pane = self.pane.as_mut().ok_or(Refusal::Missing)?;
                if PaneTarget::from(pane.lease) != target {
                    return Err(Refusal::Stale);
                }
                return pane.claim(device, connection, size, style, true);
            }
            _ => {}
        }
        if matches!(request, Request::Shutdown) {
            self.shutdown().map_err(|_| Refusal::CleanupUnresolved)?;
            return Ok(Reply::Closed);
        }
        if let Request::Poll { lease } = &request
            && self.pane.is_none()
            && self.failed_directory_session == Some((*lease).into())
        {
            return Err(Refusal::DirectoryUnavailable);
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
            | Request::Read { lease, .. }
            | Request::ReadingStatus { lease }
            | Request::GraphicsChunk { lease, .. }
            | Request::SelectionChunk { lease, .. } => lease,
            Request::Poll { lease }
            | Request::PresentationDemand { lease, .. }
            | Request::NativeEffects { lease, .. }
            | Request::CompleteClipboard { lease, .. }
            | Request::Focus { lease, .. }
            | Request::Input { lease, .. }
            | Request::Keys { lease, .. }
            | Request::Key { lease, .. }
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
        if matches!(request, Request::Poll { .. }) && pane.directory_failed {
            self.retire_exited()?;
            return Err(Refusal::DirectoryUnavailable);
        }
        match request {
            Request::PresentationDemand { visible, .. } => {
                pane.presentation_demand(visible);
                Ok(Reply::Unchanged)
            }
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
                pane.reading.fence(&pane.runtime);
                Ok(Reply::Reading { accepted: true })
            }
            Request::GraphicsChunk { image, offset, .. } => {
                Ok(Reply::GraphicsChunk(pane.graphics_chunk(image, offset)))
            }
            Request::Read { operation, .. } => pane.read(operation),
            Request::ReadingStatus { .. } => pane.reading_status(),
            Request::SelectionChunk {
                generation, offset, ..
            } => {
                if pane
                    .screen
                    .as_ref()
                    .is_none_or(|screen| screen.selection_revision != generation)
                {
                    return Err(Refusal::Stale);
                }
                pane.reading.chunk(generation, offset)
            }
            Request::NativeEffects { generation, .. } => {
                if generation == 0 || generation != pane.focus_generation {
                    return Err(Refusal::Stale);
                }
                Ok(Reply::NativeEffects {
                    request: pane.effects.next(generation),
                    hidden_input: pane.subscription.hidden_input(),
                })
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
            Request::Poll { .. } => {
                if let Some(frame) = pane.screen_frame(changes) {
                    return Ok(frame);
                }
                if pane.failed {
                    return Err(Refusal::Failed);
                }
                if pane.ended {
                    self.retire_exited()?;
                    self.pane = None;
                    Ok(Reply::Ended)
                } else {
                    if let Some(revision) = pane.accessibility.take_changed_revision() {
                        Ok(Reply::AccessibilityRevision { revision })
                    } else {
                        Ok(Reply::Unchanged)
                    }
                }
            }
            Request::Key { key, .. } => {
                if pane.focus_generation != 0 {
                    return Err(Refusal::Unsupported);
                }
                if pane.ended || pane.failed {
                    return Err(Refusal::Missing);
                }
                pane.runtime
                    .key(decode_key(key).ok_or(Refusal::Unsupported)?);
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
                self.retain(None)?;
                self.pane = None;
                Ok(Reply::Closed)
            }
            _ => Err(Refusal::Unsupported),
        }
    }
}
impl HostCommands for DevelopmentPaneHost {
    fn restore(&mut self, store: OrganizationStore) -> Result<(), ServerError> {
        self.organization = store.read()?;
        self.store = Some(store);
        Ok(())
    }
    fn tick(&mut self, now: std::time::Instant) -> Option<std::time::Instant> {
        self.directories.tick();
        if self.recover().is_ok() && self.retain_directory().is_ok() {
            let _ = self.retire_exited();
        }
        (self.pane.is_some() || self.directories.busy()).then_some(now + TICK)
    }
    fn admitted(&mut self, client: [u8; 32], name: control::ClientName) {
        self.names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .admit(client, name);
    }
    fn request(&mut self, device: [u8; 32], connection: [u8; 32], request: Request) -> Reply {
        self.handle(device, connection, request)
            .unwrap_or_else(Reply::Refused)
    }
    fn lease_live(&self, lease: &Lease) -> bool {
        self.pane.as_ref().is_some_and(|pane| pane.lease == *lease)
    }
    fn unbound(&mut self, connection: [u8; 32], lease: &Lease) {
        if let Some(pane) = &mut self.pane
            && pane.lease == *lease
            && pane.lease.connection == connection
        {
            pane.invalidate_input();
            pane.connected = false;
        }
    }
    fn disconnected(&mut self, connection: [u8; 32]) {
        self.directories.cancel_connection(connection);
        if let Some(pane) = &mut self.pane
            && pane.lease.connection == connection
        {
            pane.invalidate_input();
            pane.connected = false;
        }
    }
    fn revoked(&mut self, client: [u8; 32]) {
        if let Some(dormant) = &mut self.dormant
            && dormant.device == Some(client)
        {
            dormant.device = None;
            dormant.attachment_generation = None;
        }
        if let Some(pane) = &mut self.pane
            && pane.device == Some(client)
        {
            self.directories.cancel_connection(pane.lease.connection);
            pane.invalidate_input();
            pane.device = None;
            pane.connected = false;
            pane.attachment_generation = None;
            pane.lease.control = [0; 32];
        }
    }
    fn shutdown(&mut self) -> Result<(), ServerError> {
        self.directories.stop();
        // Persist final confirmed metadata before ending live work. Refuse uncertain shutdown.
        self.recover()
            .map_err(|_| ServerError::StorageUnavailable)?;
        self.retain_directory()
            .map_err(|_| ServerError::StorageUnavailable)?;
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
    /// Records the Client Name an admitted client supplied, before any of its requests.
    fn admitted(&mut self, _client: [u8; 32], _name: control::ClientName) {}
    /// A connection was admitted. A host that pushes Workspace Inventory keeps `outbox` until
    /// [`Self::disconnected`].
    fn connected(&mut self, _client: [u8; 32], _connection: [u8; 32], _outbox: &Outbox) {}
    /// A shared Workspace edit, or with `None` the resolution of a recorded one, replied to after
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
    fn request(&mut self, device: [u8; 32], connection: [u8; 32], request: Request) -> Reply;
    /// Whether `lease` still names a Pane's Terminal Session, ended or not, under its current
    /// control. A channel bound to a lease that is not live is closed.
    fn lease_live(&self, _lease: &Lease) -> bool {
        true
    }
    /// The connection's channel bound to `lease` is gone, as if the connection had ended for that
    /// Pane: its input state resets and the next claim starts afresh.
    fn unbound(&mut self, _connection: [u8; 32], _lease: &Lease) {}
    fn disconnected(&mut self, connection: [u8; 32]);
    fn shutdown(&mut self) -> Result<(), ServerError>;
}

/// How soon a host's timed work runs again while it waits on another thread: a journal or Git
/// write, a directory read, or a live terminal whose metadata and exit the tick observes.
pub(super) const TICK: Duration = Duration::from_millis(10);

/// A host served over SpaceTerm Protocol connections. It binds each claimed channel to its lease,
/// opens and closes the channel in the connection's outbox, and turns input into host commands.
/// Polled presentation, inventory and reading travel as `Operation::Interim` until push,
/// control-plane and scrollback replace them.
pub struct Served<H> {
    host: H,
    connections: BTreeMap<[u8; 32], Connection>,
    /// Reads whose replies wait for the host to finish them.
    reads: Vec<PendingRead>,
    /// The host's wake, so a new read is checked on the next tick.
    ticker: Option<Arc<dyn spaceterm_server::connection::Wake>>,
}

/// A directory or Worktree read, or a directory creation, still running on the host.
struct PendingRead {
    connection: [u8; 32],
    id: RequestId,
    kind: ReadKind,
    /// The host's identity of the read.
    host: u64,
}

#[derive(Clone, Copy)]
enum ReadKind {
    Directory,
    Creation,
    Worktree,
}

impl ReadKind {
    fn poll(self, id: u64) -> Request {
        match self {
            Self::Directory => Request::DirectoryRead(pane::directories::Request::Poll { id }),
            Self::Creation => {
                Request::DirectoryCreate(pane::directories::CreationRequest::Poll { id })
            }
            Self::Worktree => Request::WorktreeRead(spaceterm_server::worktree::Read::Poll { id }),
        }
    }
}

/// A host's answer to a read: settled with its reply, or still running as host read `id`.
enum ReadAnswer {
    Settled(Box<Result<ReplyBody, Refusal>>),
    Running(u64),
}

impl ReadAnswer {
    fn of(reply: Reply) -> Self {
        use pane::directories::{Error, Progress};
        use spaceterm_server::worktree::ReadReply;
        Self::Settled(Box::new(match reply {
            Reply::DirectoryRead(progress) | Reply::DirectoryCreate(progress) => match progress {
                Ok(Progress::Ready(value)) => Ok(ReplyBody::Directory(Ok(value))),
                // The host keeps a running read under the identity the server chose.
                Ok(Progress::Pending) => return Self::Running(0),
                Ok(Progress::Cancelled) => Ok(ReplyBody::Directory(Err(Error::Cancelled))),
                Err(error) => Ok(ReplyBody::Directory(Err(error))),
            },
            Reply::WorktreeRead(Ok(ReadReply::Pending { id })) => return Self::Running(id),
            Reply::WorktreeRead(result) => Ok(ReplyBody::Worktree(result)),
            Reply::Refused(refusal) => Err(refusal),
            _ => Err(Refusal::Failed),
        }))
    }
}

struct Connection {
    client: [u8; 32],
    outbox: Outbox,
    channels: BTreeMap<Channel, Bound>,
}

/// A channel bound to the lease its claim attached.
struct Bound {
    /// The Pane the claim named.
    pane: [u8; 32],
    lease: Lease,
    paste: Option<PendingPaste>,
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
        Self {
            host,
            connections: BTreeMap::new(),
            reads: Vec::new(),
            ticker: None,
        }
    }

    /// Starts a read and replies when it settles, from a later tick when it runs on.
    fn read(&mut self, peer: &Peer, id: RequestId, kind: ReadKind, request: Request) -> Dispatch {
        let host = u64::from(id.0);
        let answer = match ReadAnswer::of(self.command(peer, request)) {
            // Directory reads run under the identity the server chose: the request id.
            ReadAnswer::Running(0) if !matches!(kind, ReadKind::Worktree) => {
                ReadAnswer::Running(host)
            }
            answer => answer,
        };
        match answer {
            ReadAnswer::Settled(result) => Dispatch::Reply(result),
            ReadAnswer::Running(host) => {
                if self.reads.is_empty()
                    && let Some(ticker) = &self.ticker
                {
                    ticker.wake();
                }
                self.reads.push(PendingRead {
                    connection: peer.connection,
                    id,
                    kind,
                    host,
                });
                Dispatch::Deferred
            }
        }
    }

    /// Replies to each read that settled.
    fn answer_reads(&mut self) {
        for read in std::mem::take(&mut self.reads) {
            let reply = self
                .host
                .request(self.peer_of(read.connection), read.connection, read.kind.poll(read.host));
            match ReadAnswer::of(reply) {
                ReadAnswer::Running(_) => self.reads.push(read),
                ReadAnswer::Settled(result) => {
                    if let Some(connection) = self.connections.get(&read.connection) {
                        // A closing connection no longer needs the reply.
                        let _ = connection.outbox.reply_or_refuse(read.id, *result);
                    }
                }
            }
        }
    }

    fn peer_of(&self, connection: [u8; 32]) -> [u8; 32] {
        self.connections
            .get(&connection)
            .map_or([0; 32], |connection| connection.client)
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
        interim: bool,
        request: impl FnOnce(Lease) -> Request,
    ) -> Result<ReplyBody, Refusal> {
        let lease = self.lease(peer, channel)?;
        let result = body(self.command(peer, request(lease)), interim);
        // A Session that ended, or a lease the host no longer knows, frees the channel.
        if matches!(result, Ok(ReplyBody::Interim(InterimReply::Ended)))
            || !self.host.lease_live(&lease)
        {
            self.unbind(peer, channel);
        }
        result
    }

    /// Closes every channel whose lease the host no longer knows: a Session that ended and
    /// reported it, a closed Pane, End All Sessions, or control another client took.
    fn sweep(&mut self) {
        for connection in self.connections.values_mut() {
            let dead: Vec<_> = connection
                .channels
                .iter()
                .filter(|(_, bound)| !self.host.lease_live(&bound.lease))
                .map(|(channel, _)| *channel)
                .collect();
            for channel in dead {
                connection.channels.remove(&channel);
                connection.outbox.close(channel);
            }
        }
    }

    /// Forgets the channel's binding and closes it in the outbox.
    fn unbind(&mut self, peer: &Peer, channel: Channel) {
        if let Some(connection) = self.connections.get_mut(&peer.connection) {
            connection.channels.remove(&channel);
            connection.outbox.close(channel);
        }
    }

    fn claim(&mut self, peer: &Peer, claim: Claim) -> Result<ReplyBody, Refusal> {
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
        }
        let reply = self.command(
            peer,
            Request::ClaimPane {
                attachment: claim.attachment,
                pane: claim.pane,
                claim: claim.claim,
                size: claim.size,
                style: claim.style,
            },
        );
        let result = body(reply, false);
        match &result {
            Ok(ReplyBody::Claim(ClaimReply::Attached { lease })) => {
                self.bind(peer, channel, pane, *lease)?
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
        connection
            .outbox
            .open(channel)
            .map_err(|_| Refusal::Capacity)?;
        connection.channels.insert(
            channel,
            Bound {
                pane,
                lease,
                paste: None,
            },
        );
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
        if let Some(outcome) = self.apply(peer, input) {
            self.report(peer, channel, serial, outcome);
        }
    }

    fn apply(&mut self, peer: &Peer, input: protocol::Input) -> Option<InputOutcome> {
        let protocol::Input {
            channel,
            generation,
            events,
            ..
        } = input;
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
                InputEvent::Focus(target) => body(
                    self.command(
                        peer,
                        Request::Focus {
                            lease,
                            generation,
                            target,
                        },
                    ),
                    false,
                ),
                InputEvent::Key(key) => {
                    let mut keys = vec![key];
                    while let Some(InputEvent::Key(_)) = events.peek() {
                        if let Some(InputEvent::Key(key)) = events.next() {
                            keys.push(key);
                        }
                    }
                    body(
                        self.command(
                            peer,
                            Request::Keys {
                                lease,
                                generation,
                                keys,
                            },
                        ),
                        false,
                    )
                }
                InputEvent::Pointer(pointer) => body(
                    self.command(peer, request(InteractiveInput::Pointer(pointer))),
                    false,
                ),
                InputEvent::Wheel(wheel) => body(
                    self.command(peer, request(InteractiveInput::Wheel(wheel))),
                    false,
                ),
                InputEvent::CopyOrForward { modifiers } => body(
                    self.command(peer, request(InteractiveInput::CopyOrForward { modifiers })),
                    false,
                ),
                InputEvent::Resize(size) => {
                    body(self.command(peer, Request::Resize { lease, size }), false)
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
                        reply => body(reply, false),
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

/// The bytes `reply` takes on the wire as the reply to a polled operation, or `None` when it
/// exceeds the protocol's bounds.
#[cfg(test)]
pub(super) fn wire_bytes(reply: Reply) -> Option<usize> {
    let message = protocol::ServerMessage::Reply(Box::new(protocol::Reply {
        id: RequestId(1),
        result: body(reply, true),
    }));
    protocol::frame::encode_server(&message)
        .ok()
        .map(|wire| wire.bytes())
}

/// The protocol reply to a host command. `interim` keeps `Unchanged` distinct for polled
/// operations, which the client still interprets.
fn body(reply: Reply, interim: bool) -> Result<ReplyBody, Refusal> {
    Ok(match reply {
        Reply::Refused(refusal) => return Err(refusal),
        Reply::Unchanged if interim => ReplyBody::Interim(InterimReply::Unchanged),
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
        // Reads settle through `ReadAnswer`.
        Reply::DirectoryRead(_) | Reply::DirectoryCreate(_) | Reply::WorktreeRead(_) => {
            return Err(Refusal::Failed);
        }
        Reply::MutationPending | Reply::Inventory { .. } => return Err(Refusal::Failed),
        Reply::Screen(screen) => ReplyBody::Interim(InterimReply::Screen(screen)),
        Reply::ScreenChanges(changes) => ReplyBody::Interim(InterimReply::ScreenChanges(changes)),
        Reply::Ended => ReplyBody::Interim(InterimReply::Ended),
        Reply::AccessibilityRevision { revision } => {
            ReplyBody::Interim(InterimReply::AccessibilityRevision { revision })
        }
        Reply::NativeEffects {
            request,
            hidden_input,
        } => ReplyBody::Interim(InterimReply::NativeEffects {
            request,
            hidden_input,
        }),
        Reply::Reading { accepted } => ReplyBody::Interim(InterimReply::Reading { accepted }),
        Reply::ReadingReady {
            generation,
            selection_revision,
            selection_present,
        } => ReplyBody::Interim(InterimReply::ReadingReady {
            generation,
            selection_revision,
            selection_present,
        }),
        Reply::SelectionChunk {
            generation,
            offset,
            total,
            bytes,
        } => ReplyBody::Interim(InterimReply::SelectionChunk {
            generation,
            offset: offset as u64,
            total: total as u64,
            bytes,
        }),
        Reply::SelectionPending => ReplyBody::Interim(InterimReply::SelectionPending),
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
            },
        );
    }
    fn revoked(&mut self, client: [u8; 32]) {
        self.host.revoked(client);
    }
    fn request(&mut self, peer: &Peer, id: RequestId, operation: Operation) -> Dispatch {
        Dispatch::reply(match operation {
            Operation::Claim(claim) => self.claim(peer, claim),
            Operation::ReleaseAttachment { attachment } => body(
                self.command(peer, Request::ReleaseAttachment { attachment }),
                false,
            ),
            Operation::Release { channel } => {
                let result =
                    self.on_channel(peer, channel, false, |lease| Request::Release { lease });
                self.unbind(peer, channel);
                result
            }
            Operation::Close { channel } => {
                let result =
                    self.on_channel(peer, channel, false, |lease| Request::Close { lease });
                if result.is_ok() {
                    self.unbind(peer, channel);
                }
                result
            }
            Operation::ClearScrollback { channel } => {
                self.on_channel(peer, channel, false, |lease| {
                    Request::ClearScreenAndScrollback { lease }
                })
            }
            Operation::Style { channel, style } => self.on_channel(peer, channel, false, |lease| {
                Request::Style { lease, style }
            }),
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
            } => self.on_channel(peer, channel, false, |lease| Request::GraphicsChunk {
                lease,
                image,
                offset: offset as usize,
            }),
            Operation::Accessibility {
                channel,
                revision,
                offset,
                base,
            } => self.on_channel(peer, channel, false, |lease| Request::Accessibility {
                lease,
                revision,
                offset: offset as usize,
                base,
            }),
            Operation::AccessibilitySelection {
                channel,
                focus_generation,
                selection,
            } => self.on_channel(peer, channel, false, |lease| {
                Request::AccessibilitySelection {
                    lease,
                    focus_generation,
                    selection,
                }
            }),
            Operation::EndAccessibility { channel } => {
                self.on_channel(peer, channel, false, |lease| {
                    Request::EndAccessibilityDemand { lease }
                })
            }
            Operation::DirectoryRead(operation) => {
                let request = Request::DirectoryRead(pane::directories::Request::Begin {
                    id: u64::from(id.0),
                    operation,
                });
                return self.read(peer, id, ReadKind::Directory, request);
            }
            Operation::DirectoryCreate(creation) => {
                let request = Request::DirectoryCreate(pane::directories::CreationRequest::Begin {
                    id: u64::from(id.0),
                    path: creation.path,
                });
                return self.read(peer, id, ReadKind::Creation, request);
            }
            // Polling is the server's own business.
            Operation::WorktreeRead(spaceterm_server::worktree::Read::Poll { .. }) => {
                Err(Refusal::Unsupported)
            }
            Operation::WorktreeRead(read) => {
                return self.read(peer, id, ReadKind::Worktree, Request::WorktreeRead(read));
            }
            Operation::Shutdown => body(self.command(peer, Request::Shutdown), false),
            Operation::Interim(interim) => self.interim(peer, interim),
            // Scrollback pages arrive with the client Scrollback cache. Direct-network
            // administration never reaches the service.
            Operation::ScrollbackPage { .. } | Operation::Direct(_) => Err(Refusal::Unsupported),
        })
    }
    fn input(&mut self, peer: &Peer, input: protocol::Input) {
        self.input_message(peer, input);
    }
    /// Presentation is still polled through `Interim::PresentationDemand`. Push publishes on view
    /// changes.
    fn view(&mut self, _peer: &Peer, _view: protocol::View) {}
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
    fn ticker(&mut self, wake: Arc<dyn spaceterm_server::connection::Wake>) {
        self.ticker = Some(Arc::clone(&wake));
        self.host.ticker(wake);
    }
    fn tick(&mut self, now: std::time::Instant) -> Option<std::time::Instant> {
        let host = self.host.tick(now);
        self.sweep();
        self.answer_reads();
        let reads = (!self.reads.is_empty()).then_some(now + TICK);
        [host, reads].into_iter().flatten().min()
    }
    /// The client abandoned a read. It replies `Stale` at once; a directory read stops, while a
    /// creation and a Worktree read finish unobserved.
    fn cancel(&mut self, peer: &Peer, id: RequestId) {
        let Some(index) = self
            .reads
            .iter()
            .position(|read| read.connection == peer.connection && read.id == id)
        else {
            return;
        };
        let read = self.reads.remove(index);
        match read.kind {
            ReadKind::Directory => {
                self.command(
                    peer,
                    Request::DirectoryRead(pane::directories::Request::Cancel { id: read.host }),
                );
            }
            ReadKind::Creation => {
                self.command(
                    peer,
                    Request::DirectoryCreate(pane::directories::CreationRequest::Cancel {
                        id: read.host,
                    }),
                );
            }
            ReadKind::Worktree => {}
        }
        if let Some(connection) = self.connections.get(&peer.connection) {
            let _ = connection.outbox.reply(id, Err(Refusal::Stale));
        }
    }
    fn disconnected(&mut self, peer: &Peer) {
        self.reads.retain(|read| read.connection != peer.connection);
        self.connections.remove(&peer.connection);
        self.host.disconnected(peer.connection);
    }
    fn shutdown(&mut self) -> Result<(), ServerError> {
        self.host.shutdown()
    }
}

impl<H: HostCommands> Served<H> {
    fn interim(&mut self, peer: &Peer, interim: Interim) -> Result<ReplyBody, Refusal> {
        match interim {
            Interim::Poll { channel } => {
                self.on_channel(peer, channel, true, |lease| Request::Poll { lease })
            }
            Interim::PollChanges { channel, base } => {
                self.on_channel(peer, channel, true, |lease| Request::PollChanges {
                    lease,
                    base,
                })
            }
            Interim::NativeEffects {
                channel,
                generation,
            } => self.on_channel(peer, channel, true, |lease| Request::NativeEffects {
                lease,
                generation,
            }),
            Interim::PresentationDemand { channel, visible } => {
                self.on_channel(peer, channel, true, |lease| Request::PresentationDemand {
                    lease,
                    visible,
                })
            }
            Interim::Read { channel, operation } => self.on_channel(peer, channel, true, |lease| {
                Request::Read { lease, operation }
            }),
            Interim::ReadingStatus { channel } => self.on_channel(peer, channel, true, |lease| {
                Request::ReadingStatus { lease }
            }),
            Interim::SelectionChunk {
                channel,
                generation,
                offset,
            } => self.on_channel(peer, channel, true, |lease| Request::SelectionChunk {
                lease,
                generation,
                offset: offset as usize,
            }),
        }
    }
}
