use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SecureInputError {
    TransitionRejected,
}

/// A failed transition leaves the previously acknowledged physical state intact.
pub(crate) trait SecureInputAdapter {
    fn set_enabled(&mut self, enabled: bool) -> Result<(), SecureInputError>;
}

#[derive(Clone, Copy, Debug, Default)]
struct PaneState {
    hidden_input: bool,
    terminal_input_focus: bool,
}

/// Composition shares one coordinator across every Window and Pane on the UI thread.
#[derive(Clone)]
pub(crate) struct SecureInputHandle(Rc<RefCell<SecureInputCoordinator>>);

impl SecureInputHandle {
    pub(crate) fn new(adapter: Box<dyn SecureInputAdapter>) -> Self {
        Self(Rc::new(RefCell::new(SecureInputCoordinator {
            adapter,
            next_pane_id: 0,
            application_active: false,
            enabled: false,
            release_pending: false,
            owner: None,
            panes: BTreeMap::new(),
        })))
    }

    #[cfg(test)]
    pub(crate) fn testing() -> Self {
        Self::new(Box::new(RecordingAdapter(Rc::default())))
    }

    pub(crate) fn register_pane(&self) -> SecureInputPane {
        let id = self.0.borrow_mut().register();
        SecureInputPane {
            coordinator: self.clone(),
            id: Cell::new(id),
        }
    }

    pub(crate) fn update_application_activation(&self, active: bool) {
        let mut coordinator = self.0.borrow_mut();
        coordinator.application_active = active;
        coordinator.reconcile();
    }
}

/// A single Pane's non-cloneable lease; retirement precedes native resource release.
pub(crate) struct SecureInputPane {
    coordinator: SecureInputHandle,
    id: Cell<Option<u64>>,
}

impl SecureInputPane {
    pub(crate) fn update(&self, hidden_input: bool, terminal_input_focus: bool) {
        if let Some(id) = self.id.get() {
            self.coordinator
                .0
                .borrow_mut()
                .update(id, hidden_input, terminal_input_focus);
        }
    }

    /// Session completion and hierarchy removal permanently revoke this lease.
    pub(crate) fn retire(&self) {
        if let Some(id) = self.id.take() {
            let mut coordinator = self.coordinator.0.borrow_mut();
            coordinator.panes.remove(&id);
            coordinator.reconcile();
        }
    }
}

impl Drop for SecureInputPane {
    fn drop(&mut self) {
        self.retire();
    }
}

struct SecureInputCoordinator {
    adapter: Box<dyn SecureInputAdapter>,
    next_pane_id: u64,
    application_active: bool,
    enabled: bool,
    release_pending: bool,
    owner: Option<u64>,
    panes: BTreeMap<u64, PaneState>,
}

impl SecureInputCoordinator {
    fn register(&mut self) -> Option<u64> {
        let Some(id) = self.next_pane_id.checked_add(1) else {
            eprintln!("secure input registration failed: identity-exhausted");
            return None;
        };
        self.next_pane_id = id;
        self.panes.insert(id, PaneState::default());
        self.reconcile();
        Some(id)
    }

    fn update(&mut self, id: u64, hidden_input: bool, terminal_input_focus: bool) {
        let Some(state) = self.panes.get_mut(&id) else {
            return;
        };
        *state = PaneState {
            hidden_input,
            terminal_input_focus,
        };
        self.reconcile();
    }

    fn eligible_owner(&self) -> Option<u64> {
        if !self.application_active {
            return None;
        }
        let mut eligible = self
            .panes
            .iter()
            .filter(|(_, state)| state.hidden_input && state.terminal_input_focus);
        let (&id, _) = eligible.next()?;
        eligible.next().is_none().then_some(id)
    }

    fn reconcile(&mut self) {
        // Failed release retains physical accounting but never logical ownership. Complete
        // that release before accepting fresh eligibility, even if focus has returned.
        if self.release_pending {
            self.owner = None;
            if !self.transition(false) {
                return;
            }
        }
        let desired_owner = self.eligible_owner();
        let desired = desired_owner.is_some();
        if desired_owner == self.owner && desired == self.enabled {
            return;
        }
        self.owner = None;
        if desired != self.enabled && !self.transition(desired) {
            return;
        }
        self.owner = desired_owner;
    }

    fn transition(&mut self, enabled: bool) -> bool {
        match self.adapter.set_enabled(enabled) {
            Ok(()) => {
                self.enabled = enabled;
                self.release_pending = false;
                true
            }
            Err(error) => {
                self.release_pending = !enabled;
                eprintln!("secure input transition failed: requested={enabled}; class={error:?}");
                false
            }
        }
    }
}

impl Drop for SecureInputCoordinator {
    fn drop(&mut self) {
        self.owner = None;
        if self.enabled {
            self.transition(false);
        }
    }
}

#[cfg(test)]
#[derive(Default)]
struct RecordingState {
    calls: Vec<bool>,
    results: std::collections::VecDeque<Result<(), SecureInputError>>,
}

#[cfg(test)]
struct RecordingAdapter(Rc<RefCell<RecordingState>>);

#[cfg(test)]
impl SecureInputAdapter for RecordingAdapter {
    fn set_enabled(&mut self, enabled: bool) -> Result<(), SecureInputError> {
        let mut state = self.0.borrow_mut();
        state.calls.push(enabled);
        state.results.pop_front().unwrap_or(Ok(()))
    }
}

#[cfg(test)]
pub(crate) fn conformance_secure_input_observation() -> String {
    let recording = Rc::new(RefCell::new(RecordingState::default()));
    let handle = SecureInputHandle::new(Box::new(RecordingAdapter(recording.clone())));
    let pane = handle.register_pane();
    handle.update_application_activation(true);
    pane.update(true, true);
    pane.update(true, false);
    pane.update(true, true);
    handle.update_application_activation(false);
    format!(
        "transitions={:?} enabled={}",
        recording.borrow().calls,
        handle.0.borrow().enabled
    )
}

#[cfg(test)]
impl SecureInputHandle {
    pub(crate) fn same_coordinator(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recording() -> (SecureInputHandle, Rc<RefCell<RecordingState>>) {
        let recording = Rc::new(RefCell::new(RecordingState::default()));
        (
            SecureInputHandle::new(Box::new(RecordingAdapter(recording.clone()))),
            recording,
        )
    }

    #[test]
    fn exactly_one_live_eligible_pane_balances_shared_ownership() {
        let (handle, recording) = recording();
        let first = handle.register_pane();
        let second = handle.clone().register_pane();
        first.update(true, true);
        assert!(recording.borrow().calls.is_empty());
        handle.update_application_activation(true);
        first.update(true, true);
        second.update(true, true);
        first.update(true, false);
        second.retire();
        second.retire();
        drop(second);
        assert_eq!(recording.borrow().calls, [true, false, true, false]);
        assert_eq!(handle.0.borrow().owner, None);
    }

    #[test]
    fn ordinary_input_focus_loss_deactivation_and_session_completion_release() {
        let (handle, recording) = recording();
        let pane = handle.register_pane();
        handle.update_application_activation(true);
        pane.update(false, true);
        pane.update(true, true);
        pane.update(false, true);
        pane.update(true, true);
        pane.update(true, false);
        pane.update(true, true);
        handle.update_application_activation(false);
        handle.update_application_activation(true);
        pane.retire();
        assert_eq!(
            recording.borrow().calls,
            [true, false, true, false, true, false, true, false]
        );
    }

    #[test]
    fn retired_pane_and_stale_identity_cannot_affect_successor() {
        let (handle, recording) = recording();
        handle.update_application_activation(true);
        let old = handle.register_pane();
        let old_id = old.id.get().unwrap();
        old.update(true, true);
        old.retire();
        let successor = handle.register_pane();
        successor.update(true, true);
        old.update(true, true);
        handle.0.borrow_mut().update(old_id, true, true);
        assert_eq!(handle.0.borrow().owner, successor.id.get());
        assert_eq!(recording.borrow().calls, [true, false, true]);
    }

    #[test]
    fn failed_enable_never_claims_ownership_and_fresh_facts_can_retry() {
        let (handle, recording) = recording();
        recording
            .borrow_mut()
            .results
            .push_back(Err(SecureInputError::TransitionRejected));
        let pane = handle.register_pane();
        handle.update_application_activation(true);
        pane.update(true, true);
        assert!(!handle.0.borrow().enabled);
        assert_eq!(handle.0.borrow().owner, None);
        pane.update(true, true);
        assert_eq!(handle.0.borrow().owner, pane.id.get());
        assert_eq!(recording.borrow().calls, [true, true]);
    }

    #[test]
    fn failed_release_refuses_ownership_until_physical_release_succeeds() {
        let (handle, recording) = recording();
        let pane = handle.register_pane();
        handle.update_application_activation(true);
        pane.update(true, true);
        recording.borrow_mut().results.extend([
            Err(SecureInputError::TransitionRejected),
            Err(SecureInputError::TransitionRejected),
            Ok(()),
        ]);
        pane.update(true, false);
        assert!(handle.0.borrow().enabled);
        assert_eq!(handle.0.borrow().owner, None);
        pane.update(true, true);
        assert_eq!(handle.0.borrow().owner, None);
        pane.update(true, true);
        assert!(handle.0.borrow().enabled);
        assert_eq!(handle.0.borrow().owner, pane.id.get());
        assert_eq!(recording.borrow().calls, [true, false, false, false, true]);
    }

    #[test]
    fn dropping_last_pane_releases_once_after_composition_is_dropped() {
        let (handle, recording) = recording();
        let pane = handle.register_pane();
        handle.update_application_activation(true);
        pane.update(true, true);
        drop(handle);
        drop(pane);
        assert_eq!(recording.borrow().calls, [true, false]);
    }

    #[test]
    fn final_owner_drop_retries_failed_release_without_duplicate_success() {
        let (handle, recording) = recording();
        let pane = handle.register_pane();
        handle.update_application_activation(true);
        pane.update(true, true);
        recording
            .borrow_mut()
            .results
            .push_back(Err(SecureInputError::TransitionRejected));
        drop(pane);
        assert_eq!(handle.0.borrow().owner, None);
        drop(handle);
        assert_eq!(recording.borrow().calls, [true, false, false]);
    }

    #[test]
    fn identity_exhaustion_refuses_registration_without_reusing_a_lease() {
        let (handle, _) = recording();
        handle.0.borrow_mut().next_pane_id = u64::MAX;
        assert_eq!(handle.register_pane().id.get(), None);
    }

    #[test]
    fn failures_have_only_closed_content_free_classifications() {
        assert_eq!(
            format!("{:?}", SecureInputError::TransitionRejected),
            "TransitionRejected"
        );
    }
}
