//! Portable authorization seam for the System Permissions that programs in a Terminal Session
//! inherit from SpaceTerm.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// One permission that programs in a Terminal Session inherit from SpaceTerm and that only System
/// Settings grants.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SystemPermission {
    /// Capturing the screen, which a program needs to take screenshots.
    ScreenRecording,
    /// Controlling other applications, which a program needs to click and type. System Settings names
    /// it as [`AccessibilityNaming`] describes.
    Accessibility,
}

/// What System Settings calls the Accessibility permission on the running system.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum AccessibilityNaming {
    /// The Accessibility list, before macOS 27.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only the macOS permission adapter produces the earlier System Settings name in production"
        )
    )]
    Accessibility,
    /// The Device Control and Data Access list, from macOS 27.
    #[default]
    DeviceControl,
}

/// The authorization a program started now in a Terminal Session receives for one permission.
///
/// The Operating System reports only whether the grant is usable. A denial, a restriction, and a
/// request nobody answered read the same. A program that was already running keeps the authorization
/// it started with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PermissionAuthorization {
    NotGranted,
    Granted,
}

/// Content-free failures from native permission authorization and recovery operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only macOS permission adapters produce native permission failures in production"
    )
)]
pub(crate) enum PermissionAccessError {
    #[error("permission access is unavailable off the main thread")]
    OffMainThread,
    #[error("permission access is unavailable on this platform")]
    PlatformUnavailable,
    #[error("the platform rejected the permission operation")]
    PlatformRejected,
}

pub(crate) type PermissionResetCompletion =
    Box<dyn FnOnce(Result<(), PermissionAccessError>) + Send>;

/// What preparing a Permission Setup found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only the macOS permission adapter produces preparation results in production"
    )
)]
pub(crate) enum PermissionSetupReadiness {
    /// Tools started now already receive the permission, so there is nothing to set up.
    AlreadyGranted,
    /// System Settings is ready to accept SpaceTerm into the permission's list. `cleared` says
    /// whether the setup removed any earlier entry for SpaceTerm first. The system does not say
    /// whether an entry existed, so the removal may have found none.
    Ready { cleared: bool },
}

pub(crate) type PermissionSetupCompletion =
    Box<dyn FnOnce(Result<PermissionSetupReadiness, PermissionAccessError>) + Send>;

/// A preparation [`PermissionAccess::prepare_setup`] started. Dropping it cancels the
/// preparation: an entry it has not begun removing stays, and its completion may never run.
pub(crate) struct PermissionSetupPreparation {
    cancelled: Arc<AtomicBool>,
}

impl PermissionSetupPreparation {
    /// A preparation and the signal its native work reads to learn of the cancellation.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only the macOS permission adapter starts native setup preparations in production"
        )
    )]
    pub(crate) fn new() -> (Self, PermissionSetupCancellation) {
        let cancelled = Arc::default();
        (
            Self {
                cancelled: Arc::clone(&cancelled),
            },
            PermissionSetupCancellation { cancelled },
        )
    }
}

impl Drop for PermissionSetupPreparation {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// Whether the owner of a [`PermissionSetupPreparation`] cancelled it.
#[derive(Clone)]
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only the macOS permission worker retains setup cancellation signals in production"
    )
)]
pub(crate) struct PermissionSetupCancellation {
    cancelled: Arc<AtomicBool>,
}

impl PermissionSetupCancellation {
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only the macOS permission worker checks setup cancellation in production"
        )
    )]
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Keeps a native authorization-change observation alive until its owner drops it.
pub(crate) trait PermissionAccessSubscription {}

/// Signals that the Operating System reported a permission authorization change.
///
/// Each signal means authorization may differ from the last read, so the owner reads it again.
pub(crate) struct PermissionAccessObservation {
    pub(crate) changed: async_channel::Receiver<()>,
    pub(crate) subscription: Box<dyn PermissionAccessSubscription>,
}

/// Native permission authorization and its explicit recovery operations.
///
/// Every operation acts on the running application's own grant for one permission. None of them
/// captures the screen or sends input to test access.
pub(crate) trait PermissionAccess {
    /// Returns the latest known authorization.
    ///
    /// A read may also start a background verification. A verified value that differs from the
    /// last one arrives as a signal through [`Self::observe`], so an owner that reads again on
    /// each signal converges on what the system reports.
    fn authorization(
        &self,
        permission: SystemPermission,
    ) -> Result<PermissionAuthorization, PermissionAccessError>;

    /// Observes authorization changes the Operating System reports or a verification finds.
    ///
    /// A read can return a value the system cached before the change until the system delivers its
    /// change report, so a read made only when the application becomes active can miss a grant.
    /// `None` means the platform reports no changes.
    fn observe(&self) -> Option<PermissionAccessObservation>;

    /// Prepares System Settings for a Permission Setup of one permission.
    ///
    /// It verifies the authorization first and reports [`PermissionSetupReadiness::AlreadyGranted`]
    /// without changing anything when tools already receive the permission. Otherwise it removes
    /// any entry for the running application when it can, because System Settings ignores an
    /// application dropped onto a list that already holds it, and an entry from an earlier build
    /// grants nothing. The completion may run on any thread. The caller keeps the returned
    /// preparation for as long as it wants the result.
    fn prepare_setup(
        &self,
        permission: SystemPermission,
        completion: PermissionSetupCompletion,
    ) -> Result<PermissionSetupPreparation, PermissionAccessError>;

    fn open_settings(&self, permission: SystemPermission) -> Result<(), PermissionAccessError>;

    /// What System Settings calls the Accessibility permission, so SpaceTerm sends a person to a
    /// list they can find.
    fn accessibility_naming(&self) -> AccessibilityNaming;

    /// Whether [`Self::reset`] can act on exactly the running application's identity.
    fn can_reset(&self) -> bool;

    /// Forgets the Operating System's decision for one permission and the running application
    /// only, so a later Permission Setup adds the application again.
    ///
    /// The result reports the reset alone. The completion follows a verification, so a read made
    /// on completion reflects the reset. When that verification fails after a successful reset,
    /// the permission reads as not granted, because a removed entry grants nothing. The completion
    /// may run on any thread.
    fn reset(
        &self,
        permission: SystemPermission,
        completion: PermissionResetCompletion,
    ) -> Result<(), PermissionAccessError>;
}

#[cfg(test)]
pub(crate) mod testing {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;

    type Authorization = Result<PermissionAuthorization, PermissionAccessError>;

    /// A capability whose authorization, failures, and pending resets the test controls.
    pub(crate) struct ScriptedPermissionAccess {
        screen_recording: Cell<Authorization>,
        accessibility: Cell<Authorization>,
        pub(crate) setup_failure: Cell<Option<PermissionAccessError>>,
        pub(crate) defer_preparation: Cell<bool>,
        pending_preparations: RefCell<Vec<PermissionSetupCompletion>>,
        pub(crate) open_failure: Cell<Option<PermissionAccessError>>,
        pub(crate) resettable: Cell<bool>,
        pub(crate) reset_failure: Cell<Option<PermissionAccessError>>,
        pub(crate) prepared: RefCell<Vec<SystemPermission>>,
        /// One signal for each preparation, in order, which tells whether its owner cancelled it.
        pub(crate) preparations: RefCell<Vec<PermissionSetupCancellation>>,
        pub(crate) opened: RefCell<Vec<SystemPermission>>,
        pub(crate) resets: RefCell<Vec<SystemPermission>>,
        pending_resets: RefCell<Vec<PermissionResetCompletion>>,
        /// Reports a change to every observer, as the system does after a grant changes.
        changes: Rc<RefCell<Vec<async_channel::Sender<()>>>>,
        observers: Rc<Cell<usize>>,
        reads: Cell<usize>,
        pub(crate) naming: Cell<AccessibilityNaming>,
    }

    /// Counts an observation until its owner drops it.
    struct ScriptedSubscription(Rc<Cell<usize>>);

    impl PermissionAccessSubscription for ScriptedSubscription {}

    impl Drop for ScriptedSubscription {
        fn drop(&mut self) {
            self.0.set(self.0.get() - 1);
        }
    }

    impl ScriptedPermissionAccess {
        pub(crate) fn new(
            screen_recording: Authorization,
            accessibility: Authorization,
        ) -> Rc<Self> {
            Rc::new(Self {
                screen_recording: Cell::new(screen_recording),
                accessibility: Cell::new(accessibility),
                setup_failure: Cell::new(None),
                defer_preparation: Cell::new(false),
                pending_preparations: RefCell::default(),
                open_failure: Cell::new(None),
                resettable: Cell::new(true),
                reset_failure: Cell::new(None),
                prepared: RefCell::default(),
                preparations: RefCell::default(),
                opened: RefCell::default(),
                resets: RefCell::default(),
                pending_resets: RefCell::default(),
                changes: Rc::default(),
                observers: Rc::default(),
                reads: Cell::new(0),
                naming: Cell::new(AccessibilityNaming::default()),
            })
        }

        pub(crate) fn reads(&self) -> usize {
            self.reads.get()
        }

        pub(crate) fn observers(&self) -> usize {
            self.observers.get()
        }

        /// Reports a change the way the system does: without any window becoming active.
        pub(crate) fn report_change(&self) {
            let changes = self.changes.borrow();
            assert!(!changes.is_empty(), "something should observe changes");
            for sender in changes.iter() {
                let _ = sender.try_send(());
            }
        }

        pub(crate) fn set(&self, permission: SystemPermission, authorization: Authorization) {
            match permission {
                SystemPermission::ScreenRecording => self.screen_recording.set(authorization),
                SystemPermission::Accessibility => self.accessibility.set(authorization),
            }
        }

        pub(crate) fn take_preparation(&self) -> PermissionSetupCompletion {
            self.pending_preparations
                .borrow_mut()
                .pop()
                .expect("a preparation is pending")
        }

        pub(crate) fn take_reset(&self) -> PermissionResetCompletion {
            self.pending_resets
                .borrow_mut()
                .pop()
                .expect("a reset should be awaiting its result")
        }
    }

    impl PermissionAccess for ScriptedPermissionAccess {
        fn authorization(&self, permission: SystemPermission) -> Authorization {
            self.reads.set(self.reads.get() + 1);
            match permission {
                SystemPermission::ScreenRecording => self.screen_recording.get(),
                SystemPermission::Accessibility => self.accessibility.get(),
            }
        }

        fn observe(&self) -> Option<PermissionAccessObservation> {
            let (sender, changed) = async_channel::bounded(1);
            let mut changes = self.changes.borrow_mut();
            changes.retain(|sender| !sender.is_closed());
            changes.push(sender);
            self.observers.set(self.observers.get() + 1);
            Some(PermissionAccessObservation {
                changed,
                subscription: Box::new(ScriptedSubscription(self.observers.clone())),
            })
        }

        /// Completes with scripted authorization unless the test defers completion.
        fn prepare_setup(
            &self,
            permission: SystemPermission,
            completion: PermissionSetupCompletion,
        ) -> Result<PermissionSetupPreparation, PermissionAccessError> {
            self.prepared.borrow_mut().push(permission);
            if let Some(error) = self.setup_failure.get() {
                return Err(error);
            }
            let (preparation, cancellation) = PermissionSetupPreparation::new();
            self.preparations.borrow_mut().push(cancellation);
            if self.defer_preparation.get() {
                self.pending_preparations.borrow_mut().push(completion);
                return Ok(preparation);
            }
            completion(
                self.authorization(permission)
                    .map(|authorization| match authorization {
                        PermissionAuthorization::Granted => {
                            PermissionSetupReadiness::AlreadyGranted
                        }
                        PermissionAuthorization::NotGranted => PermissionSetupReadiness::Ready {
                            cleared: self.resettable.get(),
                        },
                    }),
            );
            Ok(preparation)
        }

        fn open_settings(&self, permission: SystemPermission) -> Result<(), PermissionAccessError> {
            self.opened.borrow_mut().push(permission);
            self.open_failure.get().map_or(Ok(()), Err)
        }

        fn accessibility_naming(&self) -> AccessibilityNaming {
            self.naming.get()
        }

        fn can_reset(&self) -> bool {
            self.resettable.get()
        }

        fn reset(
            &self,
            permission: SystemPermission,
            completion: PermissionResetCompletion,
        ) -> Result<(), PermissionAccessError> {
            self.resets.borrow_mut().push(permission);
            if let Some(error) = self.reset_failure.get() {
                return Err(error);
            }
            self.pending_resets.borrow_mut().push(completion);
            Ok(())
        }
    }
}
