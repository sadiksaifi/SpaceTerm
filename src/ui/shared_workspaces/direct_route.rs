//! What one paired route's presentation reaches while its connections come and go.
//!
//! A route's Workspaces stay presented from launch until the person removes its Saved Connection.
//! While the route has a connection, its Panes subscribe to that server. Without one they present
//! what this client retained, reach no Terminal Session, and shared edits stay unavailable.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spaceterm_server::hierarchy::{Id, Mutation};

use super::{DirectServer, SharedPaneOpener, SharedPaneSubscription, SharedWorkspaceEdits};
use crate::direct_connections::DirectRouteId;
use crate::server_connection::ServerError;
use crate::terminal::WorkspaceTerminalSessionFactory;
use crate::terminal::server_session::DevelopmentPaneStatus;
use crate::terminal::shared_workspace::SharedWorkspaceSnapshot;
use crate::ui::server_pane::ServerPaneConnection;

/// Opens one paired route's Panes through its current connection, or as unavailable without one.
pub(crate) struct DirectRoutePanes {
    direct: DirectServer,
    connected: RefCell<Option<Rc<dyn SharedPaneOpener>>>,
    /// The title each of the route's Panes presents without a connection.
    titles: RefCell<BTreeMap<Id, String>>,
    generation: Cell<u64>,
}

impl DirectRoutePanes {
    pub(crate) fn new(direct: DirectServer) -> Self {
        Self {
            direct,
            connected: RefCell::new(None),
            titles: RefCell::default(),
            generation: Cell::new(0),
        }
    }

    /// Opens Panes through `connected`, or as unavailable when `None`. Every Pane opened earlier
    /// opens again.
    pub(crate) fn follow(&self, connected: Option<Rc<dyn SharedPaneOpener>>) {
        *self.connected.borrow_mut() = connected;
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    /// Names each unavailable Pane by its title in `titles`.
    pub(crate) fn retain_titles(&self, titles: BTreeMap<Id, String>) {
        *self.titles.borrow_mut() = titles;
    }
}

impl SharedPaneOpener for DirectRoutePanes {
    fn open(&self, pane: Id) -> Option<SharedPaneSubscription> {
        if let Some(connected) = self.connected.borrow().as_ref() {
            return connected.open(pane);
        }
        let title = self.titles.borrow().get(&pane).cloned();
        Some((
            WorkspaceTerminalSessionFactory::unavailable(title),
            Arc::new(UnavailablePane),
        ))
    }

    fn placeholder(&self) -> WorkspaceTerminalSessionFactory {
        WorkspaceTerminalSessionFactory::pending()
    }

    fn direct_route(&self) -> Option<&DirectServer> {
        Some(&self.direct)
    }

    fn generation(&self) -> u64 {
        self.generation.get()
    }
}

/// A Pane of a server this client is not connected to. Nothing it is asked reaches a server.
struct UnavailablePane;

impl ServerPaneConnection for UnavailablePane {
    fn status(&self) -> DevelopmentPaneStatus {
        DevelopmentPaneStatus::Offline
    }
    fn controller(&self) -> Option<spaceterm_server::pane::control::Controller> {
        None
    }
    fn connection_generation(&self) -> u64 {
        0
    }
    // The status never changes; a connection opens the Pane again.
    fn observe(&self, hint: async_channel::Sender<()>) {
        let _ = hint.try_send(());
    }
    fn select(&self) {}
    fn retry(&self) {}
    fn take_over(&self) {}
    fn start_restored(&self) {}
}

/// Where a paired route with no connection records what a surface presented: this client's own
/// journal, which needs no connection, so the presentation outlives the Workspace Window. A write
/// fails while another writer, such as the route's previous edit loop, still holds the journal.
pub(crate) trait PresentationRecord: Send + Sync {
    fn presented(&self, sequence: u64) -> Result<(), ServerError>;
    fn forgotten_presented(&self, total: u64) -> Result<(), ServerError>;
}

/// A route's client storage, as [`crate::direct_connections::DirectConnections::storage`] opens
/// it.
impl PresentationRecord for spaceterm_server::ServerConnection {
    fn presented(&self, sequence: u64) -> Result<(), ServerError> {
        self.shared_journal()?.presented(sequence)
    }

    fn forgotten_presented(&self, total: u64) -> Result<(), ServerError> {
        self.shared_journal()?.forgotten_presented(total)
    }
}

/// The first wait before writing a failed presentation again, and the longest.
const FIRST_RETRY: Duration = Duration::from_millis(250);
const LAST_RETRY: Duration = Duration::from_secs(4);

/// Presentations a route's journal refused, kept until a write succeeds. They outlive every
/// window and connection. A thread writes them again with a growing wait while the route has no
/// connection, and stops once none wait. The route's next connection takes the rest, and its
/// edit loop records them.
#[derive(Clone, Default)]
pub(crate) struct PresentationRetries(Arc<Mutex<Retries>>);

impl gpui::Global for PresentationRetries {}

#[derive(Default)]
struct Retries {
    routes: BTreeMap<DirectRouteId, Waiting>,
    retrying: bool,
}

struct Waiting {
    record: Arc<dyn PresentationRecord>,
    sequences: BTreeSet<u64>,
    /// The forgotten total to record, or zero.
    forgotten: u64,
}

impl Waiting {
    /// Writes what waits. Returns whether nothing waits any longer.
    fn write(&mut self) -> bool {
        let record = &self.record;
        self.sequences
            .retain(|sequence| record.presented(*sequence).is_err());
        if self.forgotten != 0 && record.forgotten_presented(self.forgotten).is_ok() {
            self.forgotten = 0;
        }
        self.sequences.is_empty() && self.forgotten == 0
    }
}

/// What a route's journal still has to record.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct WaitingPresentations {
    pub sequences: Vec<u64>,
    pub forgotten: u64,
}

impl PresentationRetries {
    /// Records the presentation of `sequence` through `record`, or keeps it to write again.
    fn presented(&self, route: DirectRouteId, record: &Arc<dyn PresentationRecord>, sequence: u64) {
        if record.presented(sequence).is_err() {
            self.wait(route, record, |waiting| {
                waiting.sequences.insert(sequence);
            });
        }
    }

    fn forgotten_presented(
        &self,
        route: DirectRouteId,
        record: &Arc<dyn PresentationRecord>,
        total: u64,
    ) {
        if record.forgotten_presented(total).is_err() {
            self.wait(route, record, |waiting| {
                waiting.forgotten = waiting.forgotten.max(total);
            });
        }
    }

    fn wait(
        &self,
        route: DirectRouteId,
        record: &Arc<dyn PresentationRecord>,
        add: impl FnOnce(&mut Waiting),
    ) {
        let mut retries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        add(retries.routes.entry(route).or_insert_with(|| Waiting {
            record: Arc::clone(record),
            sequences: BTreeSet::new(),
            forgotten: 0,
        }));
        if std::mem::replace(&mut retries.retrying, true) {
            return;
        }
        let shared = Arc::clone(&self.0);
        let started = std::thread::Builder::new()
            .name("spaceterm-presentation-retries".into())
            .spawn(move || retry(&shared));
        // Without the thread, the route's next connection still takes what waits.
        retries.retrying = started.is_ok();
    }

    /// Takes what waits for `route`, for its new connection to record.
    pub(crate) fn take(&self, route: &DirectRouteId) -> Option<WaitingPresentations> {
        let waiting = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .routes
            .remove(route)?;
        Some(WaitingPresentations {
            sequences: waiting.sequences.into_iter().collect(),
            forgotten: waiting.forgotten,
        })
    }

    /// Whether any presentation still waits to be written.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .routes
            .is_empty()
    }
}

/// Writes what waits until nothing does.
fn retry(retries: &Mutex<Retries>) {
    let mut wait = FIRST_RETRY;
    loop {
        std::thread::sleep(wait);
        let mut retries = retries.lock().unwrap_or_else(|e| e.into_inner());
        retries.routes.retain(|_, waiting| !waiting.write());
        if retries.routes.is_empty() {
            retries.retrying = false;
            return;
        }
        drop(retries);
        wait = (wait * 2).min(LAST_RETRY);
    }
}

/// The transport of a paired route with no connection. It publishes no inventory and accepts no
/// edit, so the retained copy stands in and shared edits stay unavailable.
#[derive(Default)]
pub(crate) struct UnavailableEdits {
    subscribers: RefCell<Vec<async_channel::Sender<()>>>,
    record: Option<(
        DirectRouteId,
        Arc<dyn PresentationRecord>,
        PresentationRetries,
    )>,
}

impl UnavailableEdits {
    /// The transport of `route`, whose presentations `record` keeps, or else `retries`.
    pub(crate) fn recording(
        route: DirectRouteId,
        record: Arc<dyn PresentationRecord>,
        retries: PresentationRetries,
    ) -> Self {
        Self {
            subscribers: RefCell::default(),
            record: Some((route, record, retries)),
        }
    }
}

impl SharedWorkspaceEdits for UnavailableEdits {
    fn snapshot(&self) -> SharedWorkspaceSnapshot {
        SharedWorkspaceSnapshot {
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
            error: Some(spaceterm_server::ServerError::Direct(
                spaceterm_server::direct::DirectError::Transport,
            )),
            failed_attempts: 0,
        }
    }

    fn availability(&self) -> (bool, bool) {
        (false, false)
    }

    fn submit(&self, _: Mutation) -> bool {
        false
    }

    fn subscribe(&self) -> async_channel::Receiver<()> {
        let (sender, receiver) = async_channel::bounded(1);
        let _ = sender.try_send(());
        // The snapshot never changes, so the sender only keeps the subscription open.
        self.subscribers
            .borrow_mut()
            .retain(|sender| !sender.is_closed());
        self.subscribers.borrow_mut().push(sender);
        receiver
    }

    fn presented(&self, sequence: u64) {
        if let Some((route, record, retries)) = &self.record {
            retries.presented(*route, record, sequence);
        }
    }

    fn forgotten_presented(&self, total: u64) {
        if let Some((route, record, retries)) = &self.record {
            retries.forgotten_presented(*route, record, total);
        }
    }
}
