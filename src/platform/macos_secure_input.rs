use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct SecureInputPaneId(u64);

impl SecureInputPaneId {
    #[cfg(test)]
    const fn test(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct PaneState {
    hidden_input: bool,
    terminal_input_focus: bool,
}

trait SecureInputDriver {
    fn set_enabled(&mut self, enabled: bool) -> Result<(), i32>;
}

struct CarbonSecureInput;

impl SecureInputDriver for CarbonSecureInput {
    fn set_enabled(&mut self, enabled: bool) -> Result<(), i32> {
        let status = if enabled {
            // SAFETY: this coordinator is called synchronously from GPUI's AppKit thread.
            unsafe { EnableSecureEventInput() }
        } else {
            // SAFETY: this coordinator is called synchronously from GPUI's AppKit thread.
            unsafe { DisableSecureEventInput() }
        };
        (status == 0).then_some(()).ok_or(status)
    }
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn EnableSecureEventInput() -> i32;
    fn DisableSecureEventInput() -> i32;
}

struct SecureInputCoordinator<D> {
    driver: D,
    application_active: bool,
    enabled: bool,
    panes: BTreeMap<SecureInputPaneId, PaneState>,
}

impl<D: SecureInputDriver> SecureInputCoordinator<D> {
    fn new(driver: D) -> Self {
        Self {
            driver,
            application_active: false,
            enabled: false,
            panes: BTreeMap::new(),
        }
    }

    fn register(&mut self, id: SecureInputPaneId) {
        self.panes.entry(id).or_default();
        self.reconcile("Pane registered");
    }

    fn update(&mut self, id: SecureInputPaneId, hidden_input: bool, terminal_input_focus: bool) {
        self.panes.insert(
            id,
            PaneState {
                hidden_input,
                terminal_input_focus,
            },
        );
        self.reconcile("Pane security facts changed");
    }

    fn remove(&mut self, id: SecureInputPaneId) {
        self.panes.remove(&id);
        self.reconcile("Pane removed");
    }

    fn set_application_active(&mut self, active: bool) {
        self.application_active = active;
        self.reconcile("application activation changed");
    }

    fn reconcile(&mut self, reason: &'static str) {
        let eligible_panes = self
            .panes
            .values()
            .filter(|pane| pane.hidden_input && pane.terminal_input_focus)
            .count();
        let desired = self.application_active && eligible_panes == 1;
        if desired == self.enabled {
            return;
        }

        match self.driver.set_enabled(desired) {
            Ok(()) => {
                self.enabled = desired;
                eprintln!(
                    "secure event input {}: {reason}; eligible panes={eligible_panes}",
                    if desired { "enabled" } else { "disabled" }
                );
            }
            Err(status) => eprintln!(
                "secure event input transition failed: {reason}; requested={desired}; OSStatus={status}"
            ),
        }
    }
}

thread_local! {
    static COORDINATOR: RefCell<SecureInputCoordinator<CarbonSecureInput>> =
        RefCell::new(SecureInputCoordinator::new(CarbonSecureInput));
}

pub(crate) fn register_pane() -> SecureInputPaneId {
    let id = SecureInputPaneId(NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed));
    COORDINATOR.with_borrow_mut(|coordinator| coordinator.register(id));
    id
}

pub(crate) fn update_pane(id: SecureInputPaneId, hidden_input: bool, terminal_input_focus: bool) {
    COORDINATOR
        .with_borrow_mut(|coordinator| coordinator.update(id, hidden_input, terminal_input_focus));
}

pub(crate) fn remove_pane(id: SecureInputPaneId) {
    COORDINATOR.with_borrow_mut(|coordinator| coordinator.remove(id));
}

pub(crate) fn update_application_activation(active: bool) {
    COORDINATOR.with_borrow_mut(|coordinator| coordinator.set_application_active(active));
}

#[cfg(test)]
pub(crate) fn conformance_secure_input_observation() -> String {
    #[derive(Default)]
    struct RecordingDriver {
        calls: Vec<bool>,
    }

    impl SecureInputDriver for RecordingDriver {
        fn set_enabled(&mut self, enabled: bool) -> Result<(), i32> {
            self.calls.push(enabled);
            Ok(())
        }
    }

    let pane = SecureInputPaneId::test(1);
    let mut coordinator = SecureInputCoordinator::new(RecordingDriver::default());
    coordinator.register(pane);
    coordinator.set_application_active(true);
    coordinator.update(pane, true, true);
    coordinator.update(pane, true, false);
    coordinator.update(pane, true, true);
    coordinator.set_application_active(false);
    format!(
        "transitions={:?} enabled={}",
        coordinator.driver.calls, coordinator.enabled
    )
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    #[derive(Default)]
    struct RecordingDriver {
        calls: Vec<bool>,
        results: VecDeque<Result<(), i32>>,
    }

    impl SecureInputDriver for RecordingDriver {
        fn set_enabled(&mut self, enabled: bool) -> Result<(), i32> {
            self.calls.push(enabled);
            self.results.pop_front().unwrap_or(Ok(()))
        }
    }

    #[test]
    fn only_one_focused_hidden_input_pane_balances_global_ownership() {
        let first = SecureInputPaneId::test(1);
        let second = SecureInputPaneId::test(2);
        let mut coordinator = SecureInputCoordinator::new(RecordingDriver::default());
        coordinator.register(first);
        coordinator.register(second);
        coordinator.set_application_active(true);

        coordinator.update(first, true, true);
        coordinator.update(first, true, true);
        coordinator.update(second, true, true);
        coordinator.update(first, true, false);
        coordinator.remove(second);

        assert_eq!(coordinator.driver.calls, vec![true, false, true, false]);
        assert!(!coordinator.enabled);
    }

    #[test]
    fn ordinary_prompts_focus_loss_and_deactivation_never_leak_ownership() {
        let pane = SecureInputPaneId::test(1);
        let mut coordinator = SecureInputCoordinator::new(RecordingDriver::default());
        coordinator.register(pane);
        coordinator.set_application_active(true);
        coordinator.update(pane, false, true);
        coordinator.update(pane, true, true);
        coordinator.update(pane, true, false);
        coordinator.update(pane, true, true);
        coordinator.set_application_active(false);

        assert_eq!(coordinator.driver.calls, vec![true, false, true, false]);
        assert!(!coordinator.enabled);
    }

    #[test]
    fn failed_transitions_do_not_corrupt_tracked_physical_state() {
        let pane = SecureInputPaneId::test(1);
        let driver = RecordingDriver {
            results: VecDeque::from([Err(-1), Ok(()), Err(-2), Ok(())]),
            ..RecordingDriver::default()
        };
        let mut coordinator = SecureInputCoordinator::new(driver);
        coordinator.register(pane);
        coordinator.set_application_active(true);
        coordinator.update(pane, true, true);
        assert!(!coordinator.enabled);
        coordinator.update(pane, true, true);
        assert!(coordinator.enabled);
        coordinator.update(pane, true, false);
        assert!(coordinator.enabled);
        coordinator.update(pane, true, false);
        assert!(!coordinator.enabled);
        assert_eq!(coordinator.driver.calls, vec![true, true, false, false]);
    }
}
