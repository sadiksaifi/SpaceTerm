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

use crate::local_server::ServerError;

/// The client-lifetime control records of one server's Panes.
#[derive(Default)]
pub(crate) struct PaneControls(Mutex<HashMap<Id, Arc<PaneControl>>>);
impl PaneControls {
    pub(super) fn pane(&self, pane: Id) -> Arc<PaneControl> {
        Arc::clone(
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(pane)
                .or_default(),
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
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|pane, _| live.contains(pane));
    }
}

/// Never handed out. The last presenter value seals the record instead of wrapping around.
const RETIRED: u64 = 0;
const EXHAUSTED: u64 = u64::MAX;

#[derive(Default)]
pub(super) struct PaneControl {
    /// Held across each request, so a successor's claim waits for an in-flight request.
    /// Guards the highest focus generation sent for this Pane.
    turn: Mutex<u64>,
    presenter: AtomicU64,
}

/// One presentation worker's exclusive, revocable right to send for its Pane.
pub(super) struct Presenter {
    control: Arc<PaneControl>,
    id: u64,
}

impl PaneControl {
    /// Retires every earlier presenter. A request it already sent may have executed.
    pub(super) fn present(self: &Arc<Self>) -> Presenter {
        let _turn = self.turn.lock().unwrap_or_else(|e| e.into_inner());
        let next = self
            .presenter
            .load(Ordering::Acquire)
            .checked_add(1)
            .filter(|&next| next != EXHAUSTED)
            .unwrap_or(EXHAUSTED);
        self.presenter.store(next, Ordering::Release);
        Presenter {
            control: Arc::clone(self),
            id: if next == EXHAUSTED { RETIRED } else { next },
        }
    }
}

impl Presenter {
    pub(super) fn current(&self) -> bool {
        self.id != RETIRED && self.control.presenter.load(Ordering::Acquire) == self.id
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
        demand: bool,
        mut exchange: impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) {
        let mut floor = self.control.turn.lock().unwrap_or_else(|e| e.into_inner());
        if !self.current() {
            return;
        }
        let _ = self.control.presenter.compare_exchange(
            self.id,
            self.id.saturating_add(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        // Delivery is best effort; a lost reply leaves the next presenter above it anyway.
        if let Some(generation) = floor.checked_add(1) {
            *floor = generation;
            let _ = exchange(&Request::Focus {
                lease,
                generation,
                target: FocusTarget::Unfocused,
            });
        }
        // Without a window the server need not materialize this Pane's screens.
        if demand {
            let _ = exchange(&Request::PresentationDemand {
                lease,
                visible: false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_server::pane::{FocusTarget, Lease};

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
        let old = controls.pane([7; 32]).present();
        old.send(&focus(15), |_| Ok(Reply::Unchanged)).unwrap();
        let new = controls.pane([7; 32]).present();
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
            controls.pane([8; 32]).present().with_floor(|floor| floor),
            Some(0),
            "each Pane has its own sequence"
        );
    }

    #[test]
    fn a_successor_claims_only_after_the_in_flight_request_completes() {
        let control = PaneControls::default().pane([7; 32]);
        let old = control.present();
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
                let presenter = control.present();
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
}
