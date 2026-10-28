//! The server's terminal and Workspace authority, as the connection layer sees it.
//!
//! One `PaneService` serves every connection behind one mutex ([`SharedPaneService`]). Its methods
//! change state and capture snapshots only. Encoding runs on each connection's I/O thread through
//! [`super::outbox::Presentation`]. Disk syncs run on the server's [`super::sync::SyncWorker`],
//! either as [`Dispatch::After`] work or as jobs the service submits itself. The service pushes
//! replies, Pane events, inventory and presentation into the [`Outbox`] it receives in
//! [`PaneService::admitted`].
use super::outbox::Outbox;
use crate::ServerError;
use crate::direct::{DirectError, Management, ManagementReply, PairingId};
use crate::pane::control::ClientName;
use crate::protocol::{EffectDone, Input, Operation, Refusal, ReplyBody, RequestId, View};
use std::sync::{Arc, Mutex};

pub type SharedPaneService = Arc<Mutex<Box<dyn PaneService>>>;

/// The authenticated facts of one admitted connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    /// The nonsecret Client Identity.
    pub client: [u8; 32],
    /// Unique per connection. Leases bind to it.
    pub connection: [u8; 32],
    /// Present for a paired QUIC device, which cannot administer the owning account.
    pub pairing: Option<Pairing>,
}

/// The pairing a paired connection was admitted under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pairing {
    pub pairing: PairingId,
    /// The direct-network listener generation at admission. Disabling access or an uncertain
    /// commit starts a new generation and ends every connection of the old one.
    pub generation: [u8; 32],
}

impl Peer {
    pub fn paired(&self) -> bool {
        self.pairing.is_some()
    }
}

/// Work that runs on the sync worker after the `PaneService` lock is released, such as a disk
/// sync. Its result is the reply.
pub type AfterLock = Box<dyn FnOnce() -> Result<ReplyBody, Refusal> + Send>;

pub enum Dispatch {
    /// Built with [`Dispatch::reply`]. Encoded and queued under the lock.
    Reply(Box<Result<ReplyBody, Refusal>>),
    /// The service replies later with `Outbox::reply_or_refuse`, for example after the inventory
    /// revision that contains an edit's outcome.
    Deferred,
    /// Submitted to the sync worker while the lock is still held, so syncs keep lock order.
    After(AfterLock),
}

impl Dispatch {
    pub fn reply(result: Result<ReplyBody, Refusal>) -> Self {
        Self::Reply(Box::new(result))
    }
}

pub trait PaneService: Send {
    fn restore(
        &mut self,
        _store: crate::organization::OrganizationStore,
    ) -> Result<(), ServerError> {
        Ok(())
    }
    /// Called once per connection before any of its messages. The service keeps `outbox` to push
    /// to this connection until [`PaneService::disconnected`].
    fn admitted(&mut self, peer: &Peer, name: ClientName, outbox: Outbox);
    /// Permanently releases this paired identity's live and dormant control grants.
    fn revoked(&mut self, _client: [u8; 32]) {}
    /// Every operation except `Operation::Direct`, which goes to [`DirectManagement`], and
    /// account administration from paired peers, which the connection refuses.
    fn request(&mut self, peer: &Peer, id: RequestId, operation: Operation) -> Dispatch;
    /// Applies or discards the message as a whole. A discard on a channel the service knows is
    /// reported as `PaneEvent::Input`.
    fn input(&mut self, peer: &Peer, input: Input);
    /// The connection already applied `view` to its outbox. A newly visible channel needs its
    /// current presentation published again.
    fn view(&mut self, peer: &Peer, view: View);
    fn effect_done(&mut self, peer: &Peer, done: EffectDone);
    fn acknowledge(&mut self, peer: &Peer, sequence: u64);
    /// The client abandoned a cancellable read. The reply is still owed.
    fn cancel(&mut self, _peer: &Peer, _id: RequestId) {}
    /// The host is stopping. Refuse every deferred reply still owed, so each connection drains
    /// at once instead of waiting for work that no longer runs.
    fn stopping(&mut self) {}
    /// Gives the service the host's wake, once before any connection. A message that starts
    /// timed work due before the host's current deadline calls it, so the host ticks again.
    fn ticker(&mut self, _wake: Arc<dyn super::Wake>) {}
    /// Runs timed work and returns when it is next due. `None` means no timed work is pending.
    /// The host waits for that deadline, the wake from [`Self::ticker`] or a new connection,
    /// never a fixed interval.
    fn tick(&mut self, _now: std::time::Instant) -> Option<std::time::Instant> {
        None
    }
    fn disconnected(&mut self, peer: &Peer);
    fn shutdown(&mut self) -> Result<(), ServerError>;
}

/// The host's direct-network administration, answering `Operation::Direct`. It runs on the sync
/// worker, without the `PaneService` lock. It must not hold the direct-network lock while it takes
/// the `PaneService` lock, for example to call [`PaneService::revoked`].
///
/// A revocation ends the affected connections through
/// [`super::registry::PairedConnections::revoke`] or `retire_generation` before it calls
/// [`PaneService::revoked`]. The goodbye those queue makes
/// [`super::server::AdmittedConnection::dispatch`] drop every later message under the
/// `PaneService` lock, so no request or input of a revoked device runs after `revoked`.
pub trait DirectManagement: Send + Sync {
    fn manage(&self, peer: &Peer, request: Management) -> Result<ManagementReply, DirectError>;
}
