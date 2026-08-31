use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_SECRET_OWNER_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct SecureInputPaneId(u64);

impl SecureInputPaneId {
    #[cfg(test)]
    const fn test(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct SecureInputSecretOwnerId {
    key: SecureInputSecretOwnerKey,
    not_send_or_sync: PhantomData<Rc<()>>,
}

impl SecureInputSecretOwnerId {
    pub(crate) fn new() -> Self {
        Self {
            key: SecureInputSecretOwnerKey(NEXT_SECRET_OWNER_ID.fetch_add(1, Ordering::Relaxed)),
            not_send_or_sync: PhantomData,
        }
    }

    const fn key(&self) -> SecureInputSecretOwnerKey {
        self.key
    }

    #[cfg(test)]
    const fn test(value: u64) -> Self {
        Self {
            key: SecureInputSecretOwnerKey(value),
            not_send_or_sync: PhantomData,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SecureInputSecretOwnerKey(u64);

#[must_use = "dropping the lease releases secure event input ownership"]
pub(crate) struct SecureInputSecretLease {
    owner: Option<SecureInputSecretOwnerId>,
    release: Option<Box<dyn FnOnce(SecureInputSecretOwnerKey)>>,
}

impl Drop for SecureInputSecretLease {
    fn drop(&mut self) {
        let Some(owner) = self.owner.take() else {
            return;
        };
        let Some(release) = self.release.take() else {
            return;
        };
        release(owner.key());
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
    secret_owners: BTreeSet<SecureInputSecretOwnerKey>,
}

impl<D: SecureInputDriver> SecureInputCoordinator<D> {
    fn new(driver: D) -> Self {
        Self {
            driver,
            application_active: false,
            enabled: false,
            panes: BTreeMap::new(),
            secret_owners: BTreeSet::new(),
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

    fn acquire_secret(&mut self, owner: SecureInputSecretOwnerKey) {
        self.secret_owners.insert(owner);
        self.reconcile("Secret input acquired");
    }

    fn release_secret(&mut self, owner: SecureInputSecretOwnerKey) {
        self.secret_owners.remove(&owner);
        self.reconcile("Secret input released");
    }

    fn reconcile(&mut self, reason: &'static str) {
        let eligible_panes = self
            .panes
            .values()
            .filter(|pane| pane.hidden_input && pane.terminal_input_focus)
            .count();
        let eligible = eligible_panes + self.secret_owners.len();
        let desired = self.application_active && eligible == 1;
        if desired == self.enabled {
            return;
        }

        match self.driver.set_enabled(desired) {
            Ok(()) => {
                self.enabled = desired;
                eprintln!(
                    "secure event input {}: {reason}; eligible owners={eligible}",
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

pub(crate) fn acquire_secret_input(owner: SecureInputSecretOwnerId) -> SecureInputSecretLease {
    acquire_secret_input_with(
        owner,
        |owner| COORDINATOR.with_borrow_mut(|coordinator| coordinator.acquire_secret(owner)),
        |owner| {
            let _ = COORDINATOR.try_with(|coordinator| {
                coordinator.borrow_mut().release_secret(owner);
            });
        },
    )
}

fn acquire_secret_input_with(
    owner: SecureInputSecretOwnerId,
    acquire: impl FnOnce(SecureInputSecretOwnerKey),
    release: impl FnOnce(SecureInputSecretOwnerKey) + 'static,
) -> SecureInputSecretLease {
    acquire(owner.key());
    SecureInputSecretLease {
        owner: Some(owner),
        release: Some(Box::new(release)),
    }
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
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

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

    #[test]
    fn pane_to_secret_handoff_fails_closed_until_only_the_secret_owner_remains() {
        let pane = SecureInputPaneId::test(1);
        let secret = SecureInputSecretOwnerId::test(1);
        let mut coordinator = SecureInputCoordinator::new(RecordingDriver::default());
        coordinator.register(pane);
        coordinator.set_application_active(true);

        coordinator.update(pane, true, true);
        coordinator.acquire_secret(secret.key());
        coordinator.update(pane, true, false);
        coordinator.release_secret(secret.key());

        assert_eq!(coordinator.driver.calls, vec![true, false, true, false]);
        assert!(!coordinator.enabled);
    }

    #[test]
    fn acquiring_and_dropping_a_secret_lease_balances_ownership() {
        let coordinator = Rc::new(RefCell::new(SecureInputCoordinator::new(
            RecordingDriver::default(),
        )));
        coordinator.borrow_mut().set_application_active(true);
        let acquire_coordinator = coordinator.clone();
        let release_coordinator = coordinator.clone();

        let lease = acquire_secret_input_with(
            SecureInputSecretOwnerId::test(1),
            move |owner| acquire_coordinator.borrow_mut().acquire_secret(owner),
            move |owner| release_coordinator.borrow_mut().release_secret(owner),
        );
        assert!(coordinator.borrow().enabled);

        drop(lease);

        let coordinator = coordinator.borrow();
        assert_eq!(coordinator.driver.calls, vec![true, false]);
        assert!(!coordinator.enabled);
    }

    #[test]
    fn multiple_secret_owners_fail_closed() {
        let first = SecureInputSecretOwnerId::test(1);
        let second = SecureInputSecretOwnerId::test(2);
        let mut coordinator = SecureInputCoordinator::new(RecordingDriver::default());
        coordinator.set_application_active(true);

        coordinator.acquire_secret(first.key());
        coordinator.acquire_secret(second.key());
        coordinator.release_secret(first.key());
        coordinator.release_secret(second.key());

        assert_eq!(coordinator.driver.calls, vec![true, false, true, false]);
        assert!(!coordinator.enabled);
    }

    #[test]
    fn application_activation_controls_an_acquired_secret_owner() {
        let secret = SecureInputSecretOwnerId::test(1);
        let mut coordinator = SecureInputCoordinator::new(RecordingDriver::default());

        coordinator.acquire_secret(secret.key());
        coordinator.set_application_active(true);
        coordinator.set_application_active(false);
        coordinator.set_application_active(true);
        coordinator.release_secret(secret.key());

        assert_eq!(coordinator.driver.calls, vec![true, false, true, false]);
        assert!(!coordinator.enabled);
    }

    #[test]
    fn failed_secret_transitions_do_not_corrupt_tracked_physical_state() {
        let secret = SecureInputSecretOwnerId::test(1);
        let driver = RecordingDriver {
            results: VecDeque::from([Err(-1), Ok(()), Err(-2), Ok(())]),
            ..RecordingDriver::default()
        };
        let mut coordinator = SecureInputCoordinator::new(driver);
        coordinator.set_application_active(true);

        coordinator.acquire_secret(secret.key());
        assert!(!coordinator.enabled);
        coordinator.set_application_active(true);
        assert!(coordinator.enabled);
        coordinator.release_secret(secret.key());
        assert!(coordinator.enabled);
        coordinator.set_application_active(false);

        assert_eq!(coordinator.driver.calls, vec![true, true, false, false]);
        assert!(!coordinator.enabled);
    }

    #[test]
    fn sequential_secret_leases_each_balance_ownership() {
        let coordinator = Rc::new(RefCell::new(SecureInputCoordinator::new(
            RecordingDriver::default(),
        )));
        coordinator.borrow_mut().set_application_active(true);

        for owner in [
            SecureInputSecretOwnerId::test(1),
            SecureInputSecretOwnerId::test(2),
        ] {
            let acquire_coordinator = coordinator.clone();
            let release_coordinator = coordinator.clone();
            let lease = acquire_secret_input_with(
                owner,
                move |owner| acquire_coordinator.borrow_mut().acquire_secret(owner),
                move |owner| release_coordinator.borrow_mut().release_secret(owner),
            );
            drop(lease);
        }

        assert_eq!(
            coordinator.borrow().driver.calls,
            vec![true, false, true, false]
        );
    }
}
