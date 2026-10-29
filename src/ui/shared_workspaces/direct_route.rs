//! What one paired route's presentation reaches while its connections come and go.
//!
//! A route's Workspaces stay presented from launch until the person removes its Saved Connection.
//! While the route has a connection, its Panes subscribe to that server. Without one they present
//! what this client retained, reach no Terminal Session, and shared edits stay unavailable.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use spaceterm_server::hierarchy::{Id, Mutation};

use super::{DirectServer, SharedPaneOpener, SharedPaneSubscription, SharedWorkspaceEdits};
use crate::domain::ValidatedLocalDirectory;
use crate::terminal::WorkspaceTerminalSessionFactory;
use crate::terminal::server_session::DevelopmentPaneStatus;
use crate::terminal::shared_workspace::SharedWorkspaceSnapshot;
use crate::ui::server_pane::ServerPaneConnection;

/// Opens one paired route's Panes through its current connection, or as unavailable without one.
pub(crate) struct DirectRoutePanes {
    direct: DirectServer,
    /// Completes the launch plan a Pane's factory ignores. It is never a server directory.
    client_home: ValidatedLocalDirectory,
    connected: RefCell<Option<Rc<dyn SharedPaneOpener>>>,
    /// The title each of the route's Panes presents without a connection.
    titles: RefCell<BTreeMap<Id, String>>,
    generation: Cell<u64>,
}

impl DirectRoutePanes {
    pub(crate) fn new(direct: DirectServer, client_home: ValidatedLocalDirectory) -> Self {
        Self {
            direct,
            client_home,
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
            WorkspaceTerminalSessionFactory::unavailable(self.client_home.clone(), title),
            Arc::new(UnavailablePane),
        ))
    }

    fn placeholder(&self) -> WorkspaceTerminalSessionFactory {
        WorkspaceTerminalSessionFactory::pending(self.client_home.clone())
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
/// journal, which needs no connection, so the presentation outlives the Workspace Window.
pub(crate) trait PresentationRecord {
    fn presented(&self, sequence: u64);
    fn forgotten_presented(&self, total: u64);
}

/// A route's client storage, as [`crate::direct_connections::DirectConnections::storage`] opens
/// it. A journal another writer still holds records nothing; the source records it again on
/// reconnecting.
impl PresentationRecord for spaceterm_server::ServerConnection {
    fn presented(&self, sequence: u64) {
        let _ = self
            .shared_journal()
            .and_then(|mut journal| journal.presented(sequence));
    }

    fn forgotten_presented(&self, total: u64) {
        let _ = self
            .shared_journal()
            .and_then(|mut journal| journal.forgotten_presented(total));
    }
}

/// The transport of a paired route with no connection. It publishes no inventory and accepts no
/// edit, so the retained copy stands in and shared edits stay unavailable.
#[derive(Default)]
pub(crate) struct UnavailableEdits {
    subscribers: RefCell<Vec<async_channel::Sender<()>>>,
    record: Option<Rc<dyn PresentationRecord>>,
}

impl UnavailableEdits {
    /// The transport of a route whose presentations `record` keeps.
    pub(crate) fn recording(record: Rc<dyn PresentationRecord>) -> Self {
        Self {
            subscribers: RefCell::default(),
            record: Some(record),
        }
    }
}

impl SharedWorkspaceEdits for UnavailableEdits {
    fn snapshot(&self) -> SharedWorkspaceSnapshot {
        SharedWorkspaceSnapshot {
            end_all_sessions: false,
            worktree_writes: false,
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
        if let Some(record) = &self.record {
            record.presented(sequence);
        }
    }

    fn forgotten_presented(&self, total: u64) {
        if let Some(record) = &self.record {
            record.forgotten_presented(total);
        }
    }
}
