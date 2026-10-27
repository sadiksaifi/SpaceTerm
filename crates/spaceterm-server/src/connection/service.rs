//! The server's terminal and Workspace authority, as the connection layer sees it.
//!
//! One `PaneService` serves every connection behind one mutex ([`SharedPaneService`]). Its methods
//! change state and capture snapshots only. Encoding runs on each connection's I/O thread through
//! [`super::outbox::Presentation`], and disk syncs run in [`Dispatch::After`] work after the
//! mutex is released. The service pushes replies, Pane events, inventory and presentation into the
//! [`Outbox`] it receives in [`PaneService::admitted`].
use super::outbox::Outbox;
use crate::ServerError;
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
    /// A paired WSS device, which cannot administer the owning account.
    pub paired: bool,
}

/// Work that runs after the `PaneService` lock is released, such as a disk sync. Its result is
/// the reply.
pub type AfterLock = Box<dyn FnOnce() -> Result<ReplyBody, Refusal> + Send>;

pub enum Dispatch {
    /// Built with [`Dispatch::reply`].
    Reply(Box<Result<ReplyBody, Refusal>>),
    /// The service replies later with `Outbox::reply`, for example after the inventory revision
    /// that contains an edit's outcome.
    Deferred,
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
    /// Every operation except `Direct`, which the host's direct-network state answers, and
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
    /// Runs timed work and returns when it is next due. `None` means no timed work is pending.
    /// The host waits for that deadline or the next message, never a fixed interval.
    fn tick(&mut self, _now: std::time::Instant) -> Option<std::time::Instant> {
        None
    }
    fn disconnected(&mut self, peer: &Peer);
    fn shutdown(&mut self) -> Result<(), ServerError>;
}
