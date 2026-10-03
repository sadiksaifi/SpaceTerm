//! Portable authorization seam for the system permissions terminal-hosted computer-use tools need.
//!
//! A computer-use tool running in a Terminal Session takes screenshots and sends input through
//! SpaceTerm's grants, so SpaceTerm reads, sets up, and recovers them for the tool.

/// One system permission that computer-use tools in a Terminal Session inherit from SpaceTerm.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ComputerUsePermission {
    /// Capturing the screen, which a tool needs to take screenshots.
    ScreenRecording,
    /// Controlling other applications, which a tool needs to click and type. macOS presents it as
    /// Device Control and Data Access.
    Accessibility,
}

/// The authorization a computer-use tool started now in a Terminal Session receives for one
/// permission.
///
/// The Operating System reports only whether the grant is usable. A denial, a restriction, and a
/// request nobody answered read the same. A tool that was already running keeps the authorization
/// it started with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ComputerUseAuthorization {
    NotGranted,
    Granted,
}

/// Content-free failures from native computer-use authorization and recovery operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ComputerUseAccessError {
    #[error("computer-use access is unavailable off the main thread")]
    OffMainThread,
    #[error("computer-use access is unavailable on this platform")]
    PlatformUnavailable,
    #[error("the platform rejected the computer-use permission operation")]
    PlatformRejected,
}

pub(crate) type ComputerUseResetCompletion =
    Box<dyn FnOnce(Result<(), ComputerUseAccessError>) + Send>;

/// What preparing a Permission Setup found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ComputerUseSetupReadiness {
    /// Tools started now already receive the permission, so there is nothing to set up.
    AlreadyGranted,
    /// System Settings is ready to accept SpaceTerm into the permission's list. `cleared` says
    /// whether the setup removed any earlier entry for SpaceTerm first. The system does not say
    /// whether an entry existed, so the removal may have found none.
    Ready { cleared: bool },
}

pub(crate) type ComputerUseSetupCompletion =
    Box<dyn FnOnce(Result<ComputerUseSetupReadiness, ComputerUseAccessError>) + Send>;

/// Keeps a native authorization-change observation alive until its owner drops it.
pub(crate) trait ComputerUseAccessSubscription {}

/// Signals that the Operating System reported a computer-use authorization change.
///
/// Each signal means authorization may differ from the last read, so the owner reads it again.
pub(crate) struct ComputerUseAccessObservation {
    pub(crate) changed: async_channel::Receiver<()>,
    pub(crate) subscription: Box<dyn ComputerUseAccessSubscription>,
}

/// Native computer-use authorization and its explicit recovery operations.
///
/// Every operation acts on the running application's own grant for one permission. None of them
/// captures the screen or sends input to test access.
pub(crate) trait ComputerUseAccess {
    /// Returns the latest known authorization.
    ///
    /// A read may also start a background verification. A verified value that differs from the
    /// last one arrives as a signal through [`Self::observe`], so an owner that reads again on
    /// each signal converges on what the system reports.
    fn authorization(
        &self,
        permission: ComputerUsePermission,
    ) -> Result<ComputerUseAuthorization, ComputerUseAccessError>;

    /// Observes authorization changes the Operating System reports or a verification finds.
    ///
    /// A read can return a value the system cached before the change until the system delivers its
    /// change report, so a read made only when the application becomes active can miss a grant.
    /// `None` means the platform reports no changes.
    fn observe(&self) -> Option<ComputerUseAccessObservation>;

    /// Prepares System Settings for a Permission Setup of one permission.
    ///
    /// It verifies the authorization first and reports [`ComputerUseSetupReadiness::AlreadyGranted`]
    /// without changing anything when tools already receive the permission. Otherwise it removes
    /// any entry for the running application when it can, because System Settings ignores an
    /// application dropped onto a list that already holds it, and an entry from an earlier build
    /// grants nothing. The completion may run on any thread.
    fn prepare_setup(
        &self,
        permission: ComputerUsePermission,
        completion: ComputerUseSetupCompletion,
    ) -> Result<(), ComputerUseAccessError>;

    fn open_settings(
        &self,
        permission: ComputerUsePermission,
    ) -> Result<(), ComputerUseAccessError>;

    /// Whether [`Self::reset`] can act on exactly the running application's identity.
    fn can_reset(&self) -> bool;

    /// Forgets the Operating System's decision for one permission and the running application
    /// only, so a later Permission Setup adds the application again.
    ///
    /// The completion may run on any thread.
    fn reset(
        &self,
        permission: ComputerUsePermission,
        completion: ComputerUseResetCompletion,
    ) -> Result<(), ComputerUseAccessError>;
}

#[cfg(test)]
pub(crate) mod testing {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;

    type Authorization = Result<ComputerUseAuthorization, ComputerUseAccessError>;

    /// A capability whose authorization, failures, and pending resets the test controls.
    pub(crate) struct ScriptedComputerUseAccess {
        screen_recording: Cell<Authorization>,
        accessibility: Cell<Authorization>,
        pub(crate) setup_failure: Cell<Option<ComputerUseAccessError>>,
        pub(crate) open_failure: Cell<Option<ComputerUseAccessError>>,
        pub(crate) resettable: Cell<bool>,
        pub(crate) reset_failure: Cell<Option<ComputerUseAccessError>>,
        pub(crate) prepared: RefCell<Vec<ComputerUsePermission>>,
        pub(crate) opened: RefCell<Vec<ComputerUsePermission>>,
        pub(crate) resets: RefCell<Vec<ComputerUsePermission>>,
        pending_resets: RefCell<Vec<ComputerUseResetCompletion>>,
        /// Reports a change to every observer, as the system does after a grant changes.
        changes: Rc<RefCell<Vec<async_channel::Sender<()>>>>,
        observers: Rc<Cell<usize>>,
        reads: Cell<usize>,
    }

    /// Counts an observation until its owner drops it.
    struct ScriptedSubscription(Rc<Cell<usize>>);

    impl ComputerUseAccessSubscription for ScriptedSubscription {}

    impl Drop for ScriptedSubscription {
        fn drop(&mut self) {
            self.0.set(self.0.get() - 1);
        }
    }

    impl ScriptedComputerUseAccess {
        pub(crate) fn new(
            screen_recording: Authorization,
            accessibility: Authorization,
        ) -> Rc<Self> {
            Rc::new(Self {
                screen_recording: Cell::new(screen_recording),
                accessibility: Cell::new(accessibility),
                setup_failure: Cell::new(None),
                open_failure: Cell::new(None),
                resettable: Cell::new(true),
                reset_failure: Cell::new(None),
                prepared: RefCell::default(),
                opened: RefCell::default(),
                resets: RefCell::default(),
                pending_resets: RefCell::default(),
                changes: Rc::default(),
                observers: Rc::default(),
                reads: Cell::new(0),
            })
        }

        /// How many authorization reads were made. A native read can start a verification.
        pub(crate) fn reads(&self) -> usize {
            self.reads.get()
        }

        /// How many observations are alive.
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

        pub(crate) fn set(&self, permission: ComputerUsePermission, authorization: Authorization) {
            match permission {
                ComputerUsePermission::ScreenRecording => self.screen_recording.set(authorization),
                ComputerUsePermission::Accessibility => self.accessibility.set(authorization),
            }
        }

        pub(crate) fn take_reset(&self) -> ComputerUseResetCompletion {
            self.pending_resets
                .borrow_mut()
                .pop()
                .expect("a reset should be awaiting its result")
        }
    }

    impl ComputerUseAccess for ScriptedComputerUseAccess {
        fn authorization(&self, permission: ComputerUsePermission) -> Authorization {
            self.reads.set(self.reads.get() + 1);
            match permission {
                ComputerUsePermission::ScreenRecording => self.screen_recording.get(),
                ComputerUsePermission::Accessibility => self.accessibility.get(),
            }
        }

        fn observe(&self) -> Option<ComputerUseAccessObservation> {
            let (sender, changed) = async_channel::bounded(1);
            let mut changes = self.changes.borrow_mut();
            changes.retain(|sender| !sender.is_closed());
            changes.push(sender);
            self.observers.set(self.observers.get() + 1);
            Some(ComputerUseAccessObservation {
                changed,
                subscription: Box::new(ScriptedSubscription(self.observers.clone())),
            })
        }

        /// Completes at once with what the scripted authorization reports.
        fn prepare_setup(
            &self,
            permission: ComputerUsePermission,
            completion: ComputerUseSetupCompletion,
        ) -> Result<(), ComputerUseAccessError> {
            self.prepared.borrow_mut().push(permission);
            if let Some(error) = self.setup_failure.get() {
                return Err(error);
            }
            completion(
                self.authorization(permission)
                    .map(|authorization| match authorization {
                        ComputerUseAuthorization::Granted => {
                            ComputerUseSetupReadiness::AlreadyGranted
                        }
                        ComputerUseAuthorization::NotGranted => ComputerUseSetupReadiness::Ready {
                            cleared: self.resettable.get(),
                        },
                    }),
            );
            Ok(())
        }

        fn open_settings(
            &self,
            permission: ComputerUsePermission,
        ) -> Result<(), ComputerUseAccessError> {
            self.opened.borrow_mut().push(permission);
            self.open_failure.get().map_or(Ok(()), Err)
        }

        fn can_reset(&self) -> bool {
            self.resettable.get()
        }

        fn reset(
            &self,
            permission: ComputerUsePermission,
            completion: ComputerUseResetCompletion,
        ) -> Result<(), ComputerUseAccessError> {
            self.resets.borrow_mut().push(permission);
            if let Some(error) = self.reset_failure.get() {
                return Err(error);
            }
            self.pending_resets.borrow_mut().push(completion);
            Ok(())
        }
    }
}
