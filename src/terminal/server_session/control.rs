//! Keeps each server Pane's wire sender and focus sequence for the client's lifetime.
//!
//! Windows come and go while the client keeps its server connection, so a Pane's
//! presentation worker is replaceable but its control sequencing is not. One record per
//! Pane admits requests from exactly one current presenter and remembers the highest
//! focus generation this client ever sent, so a successor never starts below it.
use spaceterm_server::{
    hierarchy::{Id, Inventory},
    pane::{FocusTarget, Lease, Reply, Request},
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::server_connection::ServerError;

/// The client-lifetime control records of one server's Panes.
#[derive(Default)]
pub(crate) struct PaneControls {
    panes: Mutex<HashMap<Id, Arc<PaneControl>>>,
    refusal: Option<ServerError>,
}

#[derive(Clone, Eq, PartialEq, Hash)]
pub(crate) enum ControlRoute {
    Direct(Id),
}

/// Process-lifetime controls, independent of a presented writer or live transport.
/// A route's original identity can reconnect; a different identity gets independent controls.
#[derive(Default)]
pub(crate) struct PaneControlRegistry {
    clients: HashMap<(ControlRoute, spaceterm_server::ClientScope), Arc<PaneControls>>,
}
impl PaneControlRegistry {
    pub(crate) fn controls(
        &mut self,
        route: ControlRoute,
        scope: Result<spaceterm_server::ClientScope, ServerError>,
    ) -> Arc<PaneControls> {
        match scope {
            Ok(scope) => Arc::clone(self.clients.entry((route, scope)).or_default()),
            Err(error) => Arc::new(PaneControls {
                refusal: Some(error),
                ..PaneControls::default()
            }),
        }
    }
}

impl PaneControls {
    pub(crate) fn admission(&self) -> Result<(), ServerError> {
        self.refusal.map_or(Ok(()), Err)
    }

    pub(super) fn pane(&self, pane: Id) -> Arc<PaneControl> {
        Arc::clone(
            self.panes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(pane)
                .or_insert_with(|| {
                    Arc::new(PaneControl {
                        refusal: self.refusal,
                        ..PaneControl::default()
                    })
                }),
        )
    }

    /// Forgets the Panes an authoritative inventory no longer lists.
    pub(crate) fn retain(&self, inventory: &Inventory) {
        let live: std::collections::HashSet<_> = inventory
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| &tab.panes)
            .map(|pane| pane.id)
            .collect();
        self.panes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|pane, _| live.contains(pane));
    }
}

/// Never reserved, so it never becomes current. Exhausted reservations fail closed with it.
const NONE: u64 = 0;
/// The last reservable presentation; its retired state must still fit in `presenter`.
const LAST: u64 = (u64::MAX - 1) / 2;

#[derive(Default)]
pub(super) struct PaneControl {
    refusal: Option<ServerError>,
    /// An interrupted start belongs to the retained Pane, including across window replacement.
    pub(super) recovery: Arc<Mutex<super::recovery::Recovery>>,
    /// The view a closed window left of this Pane, which the next presentation reads from.
    pub(super) view: Arc<Mutex<Option<super::reading::Reading>>>,
    /// Held across each request, so a successor's claim waits for an in-flight request.
    /// Guards the highest focus generation sent for this Pane.
    turn: Mutex<u64>,
    /// The last presentation reserved, in creation order.
    reserved: AtomicU64,
    /// Twice the current presentation, plus one once it has retired. It only increases.
    presenter: AtomicU64,
}

/// A presentation's place in creation order, taken without blocking when it is created.
pub(super) struct Reservation {
    control: Arc<PaneControl>,
    id: u64,
}

/// One presentation worker's exclusive, revocable right to send for its Pane.
pub(super) struct Presenter {
    control: Arc<PaneControl>,
    id: u64,
}

impl PaneControl {
    /// Orders a new presentation after every earlier one. This never waits for a request.
    pub(super) fn reserve(self: &Arc<Self>) -> Reservation {
        let id = self
            .reserved
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |last| {
                last.checked_add(1).filter(|&next| next <= LAST)
            })
            .map_or(NONE, |last| last + 1);
        Reservation {
            control: Arc::clone(self),
            id,
        }
    }
}

impl Reservation {
    /// Makes this presentation current once any in-flight request completes, retiring every
    /// earlier one. A dismissed presentation, or one a later presentation already superseded,
    /// activates nothing, however late its worker runs.
    pub(super) fn activate(self, alive: &std::sync::atomic::AtomicBool) -> Presenter {
        let id = {
            let _turn = self.control.turn.lock().unwrap_or_else(|e| e.into_inner());
            let state = self.id.saturating_mul(2);
            if self.id != NONE
                && alive.load(Ordering::Acquire)
                && state > self.control.presenter.load(Ordering::Acquire)
            {
                self.control.presenter.store(state, Ordering::Release);
                self.id
            } else {
                NONE
            }
        };
        Presenter {
            control: self.control,
            id,
        }
    }
}

impl Presenter {
    pub(super) fn current(&self) -> bool {
        self.id != NONE && self.control.presenter.load(Ordering::Acquire) == self.id * 2
    }

    /// Runs `apply` with the highest focus generation sent, unless a successor exists.
    pub(super) fn with_floor<R>(&self, apply: impl FnOnce(u64) -> R) -> Option<R> {
        let floor = self.control.turn.lock().unwrap_or_else(|e| e.into_inner());
        self.current().then(|| apply(*floor))
    }

    /// Sends one request in this presenter's turn. A retired presenter sends nothing.
    pub(super) fn send(
        &self,
        request: &Request,
        exchange: impl FnOnce(&Request) -> Result<Reply, ServerError>,
    ) -> Result<Reply, ServerError> {
        let mut floor = self.control.turn.lock().unwrap_or_else(|e| e.into_inner());
        if !self.current() {
            return Err(ServerError::NotRunning);
        }
        if let Some(error) = self.control.refusal {
            return Err(error);
        }
        if let Request::Focus { generation, .. } = request {
            // Count every generation that reaches the wire, even if its reply is lost.
            *floor = (*floor).max(*generation);
        }
        exchange(request)
    }

    /// Ends this presentation: unfocuses the Pane and, where the server supports visual
    /// demand, hides it, in order before any successor; then admits no further request.
    /// Control remains granted; only a successor or an explicit release changes it.
    pub(super) fn retire(
        &self,
        lease: Lease,
        mut exchange: impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) {
        let mut floor = self.control.turn.lock().unwrap_or_else(|e| e.into_inner());
        if !self.current() {
            return;
        }
        self.control
            .presenter
            .store(self.id * 2 + 1, Ordering::Release);
        // Delivery is best effort; a lost reply leaves the next presenter above it anyway.
        if let Some(generation) = floor.checked_add(1) {
            *floor = generation;
            let _ = exchange(&Request::Focus {
                lease,
                generation,
                target: FocusTarget::Unfocused,
            });
        }
        // Without a window the server need not publish this Pane's screens.
        let _ = exchange(&Request::View {
            lease,
            visible: false,
            max_hz: crate::terminal::session::DEFAULT_FRAME_RATE,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_server::pane::{FocusTarget, Lease};

    fn present(control: &Arc<PaneControl>) -> Presenter {
        control
            .reserve()
            .activate(&std::sync::atomic::AtomicBool::new(true))
    }

    fn focus(generation: u64) -> Request {
        Request::Focus {
            lease: Lease {
                server: [1; 32],
                session: [2; 32],
                control: [3; 32],
                connection: [4; 32],
            },
            generation,
            target: FocusTarget::Terminal,
        }
    }

    #[test]
    fn a_successor_fences_its_predecessor_and_continues_its_focus_sequence() {
        let controls = PaneControls::default();
        let old = present(&controls.pane([7; 32]));
        old.send(&focus(15), |_| Ok(Reply::Unchanged)).unwrap();
        let new = present(&controls.pane([7; 32]));
        let mut sent = false;
        assert!(matches!(
            old.send(&focus(16), |_| {
                sent = true;
                Ok(Reply::Unchanged)
            }),
            Err(ServerError::NotRunning)
        ));
        assert!(!sent, "a retired presenter must not reach the wire");
        assert_eq!(new.with_floor(|floor| floor), Some(15));
        assert_eq!(
            present(&controls.pane([8; 32])).with_floor(|floor| floor),
            Some(0),
            "each Pane has its own sequence"
        );
    }

    #[test]
    fn a_successor_claims_only_after_the_in_flight_request_completes() {
        let control = PaneControls::default().pane([7; 32]);
        let old = present(&control);
        let (entered, wait) = std::sync::mpsc::channel();
        let (finish, finished) = std::sync::mpsc::channel::<()>();
        let sender = std::thread::spawn(move || {
            old.send(&focus(3), |_| {
                entered.send(()).unwrap();
                finished.recv().unwrap();
                Ok(Reply::Unchanged)
            })
        });
        wait.recv().unwrap();
        let (claimed, claim) = std::sync::mpsc::channel();
        let successor = {
            let control = Arc::clone(&control);
            std::thread::spawn(move || {
                let presenter = present(&control);
                claimed.send(()).unwrap();
                presenter.with_floor(|floor| floor)
            })
        };
        assert!(
            claim
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "a successor must not claim during its predecessor's request"
        );
        finish.send(()).unwrap();
        assert!(matches!(sender.join().unwrap(), Ok(Reply::Unchanged)));
        assert_eq!(successor.join().unwrap(), Some(3));
    }

    #[test]
    fn a_late_worker_of_an_earlier_window_never_supersedes_a_later_one() {
        let control = PaneControls::default().pane([7; 32]);
        let alive = std::sync::atomic::AtomicBool::new(true);
        let earlier = control.reserve();
        let later = control.reserve().activate(&alive);
        let late = earlier.activate(&alive);
        assert!(later.current(), "creation order decides ownership");
        assert!(!late.current());
        assert!(matches!(
            late.send(&focus(1), |_| Ok(Reply::Unchanged)),
            Err(ServerError::NotRunning)
        ));
        // A retired presentation still yields to one created after it.
        later.retire(
            Lease {
                server: [1; 32],
                session: [2; 32],
                control: [3; 32],
                connection: [4; 32],
            },
            |_| Ok(Reply::Unchanged),
        );
        assert!(present(&control).current());
    }
}
