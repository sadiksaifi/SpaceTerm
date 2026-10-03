//! Permission Setup: one guided pass through System Settings that adds SpaceTerm to the privacy
//! lists computer-use tools need.
//!
//! A setup starts only when someone asks for it: a person choosing Set Up in Settings, or
//! accepting a Permission Request from a tool in a Terminal Session. It verifies the permission,
//! clears an entry that does not grant it, opens System Settings at the permission's list, and
//! docks the Setup Guide on System Settings' window while that window is in front. The guide offers SpaceTerm itself
//! to drag into the list and reports the grant as soon as a tool started now would receive it.
//! Closing System Settings ends the setup.

mod guide;
mod placement;
#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, Bounds, Context, DisplayId, Entity, Pixels, Task};

use crate::platform::computer_use_access::{
    AccessibilityNaming, ComputerUseAccess, ComputerUseAccessSubscription,
    ComputerUseAuthorization, ComputerUsePermission, ComputerUseSetupReadiness,
};
use crate::platform::setup_guide_host::{ApplicationBundle, SetupGuideHost, SystemSettingsWindow};

pub(crate) use guide::SetupGuide;

/// How often the guide follows System Settings' window, which a person can move at any time.
const TRACKING_INTERVAL: Duration = Duration::from_millis(33);
/// How often the setup looks for System Settings while it is covered, when no guide follows it.
const COVERED_TRACKING_INTERVAL: Duration = Duration::from_millis(250);
/// How long System Settings may keep showing the previous list after it is asked for another.
/// The guide waits this long after opening a list, so it never points at the wrong one.
const OPENING_SETTLE: Duration = Duration::from_millis(250);
/// How many tracking intervals in front pass between authorization reads. System Settings reports
/// no Screen Recording change, so the setup reads it about once a second while the person works
/// in System Settings.
const AUTHORIZATION_INTERVALS: u32 = 30;
/// How long System Settings may take to come forward before the setup reports it did not.
const OPENING_TIMEOUT: Duration = Duration::from_secs(10);

/// How SpaceTerm names one permission and the System Settings list that holds it.
pub(crate) struct PermissionCopy {
    /// The permission's name in running text.
    pub(crate) name: &'static str,
    /// The System Settings list that holds the grant.
    pub(crate) pane: &'static str,
    /// What a computer-use tool does with the grant.
    pub(crate) purpose: &'static str,
}

/// Names `permission` as System Settings does under `naming`.
pub(crate) const fn permission_copy(
    permission: ComputerUsePermission,
    naming: AccessibilityNaming,
) -> &'static PermissionCopy {
    match (permission, naming) {
        (ComputerUsePermission::ScreenRecording, _) => &PermissionCopy {
            name: "Screen Recording",
            pane: "Screen & System Audio Recording",
            purpose: "take screenshots",
        },
        (ComputerUsePermission::Accessibility, AccessibilityNaming::Accessibility) => {
            &PermissionCopy {
                name: "Accessibility",
                pane: "Accessibility",
                purpose: "click and type in other apps",
            }
        }
        (ComputerUsePermission::Accessibility, AccessibilityNaming::DeviceControl) => {
            &PermissionCopy {
                name: "Device Control",
                pane: "Device Control and Data Access",
                purpose: "click and type in other apps",
            }
        }
    }
}

/// Where one permission's setup stands, as Settings presents it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PermissionSetupStatus {
    Idle,
    /// The permission is being set up now or is next in the running setup.
    Running,
    /// The latest setup of the permission ended without reaching the guide.
    Failed(PermissionSetupFailure),
}

/// Why a setup ended before the guide could help.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PermissionSetupFailure {
    /// System Settings could not be opened.
    SettingsUnavailable,
    /// System Settings did not come forward in time.
    SettingsNotShown,
}

/// One permission's progress through a running setup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SetupStep {
    /// Verifying the authorization and clearing a stale entry.
    Preparing,
    /// System Settings was asked for the permission's list and has not come forward yet.
    Opening,
    /// The guide waits on System Settings for the person to add SpaceTerm.
    Guiding,
    /// Tools started now receive the permission.
    Granted,
}

/// What the Setup Guide shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GuidePresentation {
    pub(crate) permission: ComputerUsePermission,
    pub(crate) granted: bool,
    /// Whether the setup removed any earlier entry for SpaceTerm, so the guide says why the list
    /// no longer holds it.
    pub(crate) cleared: bool,
    /// The permission set up after this one, when the setup holds another.
    pub(crate) next: Option<ComputerUsePermission>,
    pub(crate) naming: AccessibilityNaming,
}

/// Owns the one running setup and the Setup Guide window it presents.
pub(crate) struct PermissionSetup {
    access: Rc<dyn ComputerUseAccess>,
    /// What System Settings calls the Accessibility permission, which holds for the process.
    naming: AccessibilityNaming,
    host: Arc<dyn SetupGuideHost>,
    run: Option<SetupRun>,
    /// The latest failure of each permission's setup, cleared when its next setup starts.
    failures: Vec<(ComputerUsePermission, PermissionSetupFailure)>,
    /// The running application as the guide offers it, resolved once on first use.
    bundle: Option<Option<ApplicationBundle>>,
    /// Notifies this setup's observers of authorization changes, from the first
    /// [`PermissionSetup::watch_authorization`] on.
    watch: Option<(Box<dyn ComputerUseAccessSubscription>, Task<()>)>,
}

struct SetupRun {
    permission: ComputerUsePermission,
    queued: VecDeque<ComputerUsePermission>,
    step: SetupStep,
    /// Whether preparing the current permission removed any earlier entry for SpaceTerm.
    cleared: bool,
    guide: Option<GuideWindow>,
    /// Tracking intervals with System Settings in front.
    intervals: u32,
    /// When System Settings was last asked for a list.
    opened_at: Instant,
    /// Waits for the current step: the preparation's result or the opening timeout.
    _step: Option<Task<()>>,
    _tracking: Task<()>,
    _changes: Option<(Box<dyn ComputerUseAccessSubscription>, Task<()>)>,
}

struct GuideWindow {
    handle: gpui::WindowHandle<SetupGuide>,
    display: DisplayId,
    bounds: Bounds<Pixels>,
}

impl GuideWindow {
    /// Whether the window is still open. GPUI refuses an update while the window handles an
    /// event, so a refused update alone does not mean the window closed.
    fn is_open(&self, cx: &App) -> bool {
        cx.windows()
            .iter()
            .any(|window| window.window_id() == self.handle.window_id())
    }
}

impl PermissionSetup {
    /// Creates the application's one Permission Setup, which the composition hands to Settings
    /// and every Pane.
    pub(crate) fn create(
        access: Rc<dyn ComputerUseAccess>,
        host: Arc<dyn SetupGuideHost>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|_| Self::new(access, host))
    }

    fn new(access: Rc<dyn ComputerUseAccess>, host: Arc<dyn SetupGuideHost>) -> Self {
        Self {
            naming: access.accessibility_naming(),
            access,
            host,
            run: None,
            failures: Vec::new(),
            bundle: None,
            watch: None,
        }
    }

    /// Notifies this setup's observers whenever an authorization may have changed, so a Pane
    /// waiting on a Permission Request reads again and withdraws or renews its offer.
    pub(crate) fn watch_authorization(&mut self, cx: &mut Context<Self>) {
        if self.watch.is_some() {
            return;
        }
        self.watch = self.access.observe().map(|observation| {
            let changed = observation.changed;
            let task = cx.spawn(async move |setup, cx| {
                while changed.recv().await.is_ok() {
                    while changed.try_recv().is_ok() {}
                    if setup.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            });
            (observation.subscription, task)
        });
    }

    pub(crate) fn status(&self, permission: ComputerUsePermission) -> PermissionSetupStatus {
        if self
            .run
            .as_ref()
            .is_some_and(|run| run.permission == permission || run.queued.contains(&permission))
        {
            return PermissionSetupStatus::Running;
        }
        self.failures
            .iter()
            .find(|(failed, _)| *failed == permission)
            .map_or(PermissionSetupStatus::Idle, |(_, failure)| {
                PermissionSetupStatus::Failed(*failure)
            })
    }

    /// The permissions among `permissions` that a tool started now would not receive, in order.
    pub(crate) fn ungranted(
        &self,
        permissions: &[ComputerUsePermission],
    ) -> Vec<ComputerUsePermission> {
        permissions
            .iter()
            .copied()
            .filter(|permission| {
                self.access.authorization(*permission) != Ok(ComputerUseAuthorization::Granted)
            })
            .collect()
    }

    /// The permission being set up now and its step.
    pub(crate) fn current(&self) -> Option<(ComputerUsePermission, SetupStep)> {
        self.run.as_ref().map(|run| (run.permission, run.step))
    }

    /// Sets up each permission in order. A running setup takes the permissions it does not hold
    /// yet and brings System Settings forward again.
    pub(crate) fn start(&mut self, permissions: &[ComputerUsePermission], cx: &mut Context<Self>) {
        let mut requested = VecDeque::new();
        for permission in permissions {
            if !requested.contains(permission) {
                requested.push_back(*permission);
            }
        }
        self.failures
            .retain(|(permission, _)| !requested.contains(permission));
        if let Some(run) = &mut self.run {
            for permission in requested {
                if permission != run.permission && !run.queued.contains(&permission) {
                    run.queued.push_back(permission);
                }
            }
            match run.step {
                SetupStep::Opening | SetupStep::Guiding => {
                    let _ = self.access.open_settings(run.permission);
                }
                // The person asked for more after a grant, so the setup moves on rather than
                // waiting for Continue in a guide that may be hidden.
                SetupStep::Granted if !run.queued.is_empty() => {
                    self.advance(cx);
                    return;
                }
                SetupStep::Preparing | SetupStep::Granted => {}
            }
            self.present(cx);
            cx.notify();
            return;
        }
        let Some(permission) = requested.pop_front() else {
            return;
        };
        let changes = self.access.observe().map(|observation| {
            let changed = observation.changed;
            let task = cx.spawn(async move |setup, cx| {
                while changed.recv().await.is_ok() {
                    while changed.try_recv().is_ok() {}
                    if setup
                        .update(cx, |setup, cx| setup.read_authorization(cx))
                        .is_err()
                    {
                        break;
                    }
                }
            });
            (observation.subscription, task)
        });
        let host = Arc::clone(&self.host);
        let tracking = cx.spawn(async move |setup, cx| {
            let mut interval = TRACKING_INTERVAL;
            loop {
                cx.background_executor().timer(interval).await;
                let host = Arc::clone(&host);
                let located = cx
                    .background_spawn(async move { host.locate_system_settings() })
                    .await;
                match setup.update(cx, |setup, cx| setup.follow(located, cx)) {
                    Ok(next) => interval = next,
                    Err(_) => break,
                }
            }
        });
        self.run = Some(SetupRun {
            permission,
            queued: requested,
            step: SetupStep::Preparing,
            cleared: false,
            guide: None,
            intervals: 0,
            opened_at: cx.background_executor().now(),
            _step: None,
            _tracking: tracking,
            _changes: changes,
        });
        self.prepare(cx);
        cx.notify();
    }

    /// Ends the running setup and closes the guide. System Settings stays as the person left it.
    pub(crate) fn cancel(&mut self, cx: &mut Context<Self>) {
        self.finish(cx);
    }

    /// Ends a setup whose grant arrived and returns the person to SpaceTerm.
    pub(crate) fn done(&mut self, cx: &mut Context<Self>) {
        if self
            .current()
            .is_some_and(|(_, step)| step == SetupStep::Granted)
        {
            self.finish(cx);
            cx.activate(true);
        }
    }

    /// Moves from a granted permission to the next one.
    pub(crate) fn continue_setup(&mut self, cx: &mut Context<Self>) {
        if self
            .current()
            .is_some_and(|(_, step)| step == SetupStep::Granted)
        {
            self.advance(cx);
        }
    }

    fn prepare(&mut self, cx: &mut Context<Self>) {
        let Some(run) = &mut self.run else {
            return;
        };
        run.step = SetupStep::Preparing;
        run.cleared = false;
        let permission = run.permission;
        let (sender, receiver) = async_channel::bounded(1);
        let started = self.access.prepare_setup(
            permission,
            Box::new(move |readiness| {
                let _ = sender.try_send(readiness);
            }),
        );
        run._step = Some(cx.spawn(async move |setup, cx| {
            let readiness = match started {
                Ok(()) => receiver.recv().await.ok().and_then(Result::ok),
                Err(_) => None,
            };
            let _ = setup.update(cx, |setup, cx| setup.prepared(permission, readiness, cx));
        }));
    }

    /// A failed preparation leaves any existing entry in place, and the guide also explains how
    /// to turn one on.
    fn prepared(
        &mut self,
        permission: ComputerUsePermission,
        readiness: Option<ComputerUseSetupReadiness>,
        cx: &mut Context<Self>,
    ) {
        if self.current() != Some((permission, SetupStep::Preparing)) {
            return;
        }
        match readiness {
            Some(ComputerUseSetupReadiness::AlreadyGranted) => self.advance(cx),
            Some(ComputerUseSetupReadiness::Ready { cleared }) => {
                if let Some(run) = &mut self.run {
                    run.cleared = cleared;
                }
                self.open(cx);
            }
            None => self.open(cx),
        }
    }

    fn open(&mut self, cx: &mut Context<Self>) {
        let Some(run) = &mut self.run else {
            return;
        };
        let permission = run.permission;
        if self.access.open_settings(permission).is_err() {
            self.fail(PermissionSetupFailure::SettingsUnavailable, cx);
            return;
        }
        run.step = SetupStep::Opening;
        run.opened_at = cx.background_executor().now();
        run._step = Some(cx.spawn(async move |setup, cx| {
            cx.background_executor().timer(OPENING_TIMEOUT).await;
            let _ = setup.update(cx, |setup, cx| {
                if setup.current() == Some((permission, SetupStep::Opening)) {
                    setup.fail(PermissionSetupFailure::SettingsNotShown, cx);
                }
            });
        }));
        cx.notify();
    }

    fn advance(&mut self, cx: &mut Context<Self>) {
        let Some(run) = &mut self.run else {
            return;
        };
        let Some(next) = run.queued.pop_front() else {
            self.finish(cx);
            return;
        };
        run.permission = next;
        // System Settings moves to the next list, so the guide waits until it comes forward.
        self.dismiss_guide(cx);
        self.prepare(cx);
        cx.notify();
    }

    fn fail(&mut self, failure: PermissionSetupFailure, cx: &mut Context<Self>) {
        if let Some((permission, _)) = self.current() {
            self.failures.retain(|(failed, _)| *failed != permission);
            self.failures.push((permission, failure));
        }
        self.finish(cx);
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        self.dismiss_guide(cx);
        if self.run.take().is_some() {
            cx.notify();
        }
    }

    /// Follows System Settings' window and returns how long to wait before looking again.
    fn follow(&mut self, located: SystemSettingsWindow, cx: &mut Context<Self>) -> Duration {
        let now = cx.background_executor().now();
        let Some(run) = &mut self.run else {
            return COVERED_TRACKING_INTERVAL;
        };
        let frontmost = matches!(located, SystemSettingsWindow::Frontmost { .. });
        if frontmost {
            run.intervals = run.intervals.wrapping_add(1);
        }
        // Authorization is read only while the person can change it in System Settings, and once
        // more as System Settings leaves the front, so a grant made just before is not missed.
        let read_due = if frontmost {
            run.intervals.is_multiple_of(AUTHORIZATION_INTERVALS)
        } else {
            run.guide.is_some()
        };
        let next = if frontmost || run.step == SetupStep::Opening {
            TRACKING_INTERVAL
        } else {
            COVERED_TRACKING_INTERVAL
        };
        if read_due {
            self.read_authorization(cx);
        }
        let Some(run) = &mut self.run else {
            return next;
        };
        match (run.step, located) {
            (SetupStep::Preparing, _) => {}
            // System Settings may still be launching or switching lists.
            (SetupStep::Opening, SystemSettingsWindow::Closed | SystemSettingsWindow::Covered) => {}
            (SetupStep::Opening, SystemSettingsWindow::Frontmost { display, content }) => {
                if now.saturating_duration_since(run.opened_at) >= OPENING_SETTLE {
                    run.step = SetupStep::Guiding;
                    run._step = None;
                    cx.notify();
                    self.present_guide(display, content, cx);
                }
            }
            (_, SystemSettingsWindow::Closed) => self.finish(cx),
            (_, SystemSettingsWindow::Covered) => self.dismiss_guide(cx),
            (_, SystemSettingsWindow::Frontmost { display, content }) => {
                self.present_guide(display, content, cx);
            }
        }
        next
    }

    /// Reads whether a tool started now receives the permission, and reports a grant.
    fn read_authorization(&mut self, cx: &mut Context<Self>) {
        let Some(run) = &mut self.run else {
            return;
        };
        if run.step != SetupStep::Guiding {
            return;
        }
        if self.access.authorization(run.permission) == Ok(ComputerUseAuthorization::Granted) {
            run.step = SetupStep::Granted;
            self.present(cx);
            cx.notify();
        }
    }

    fn presentation(&self) -> Option<GuidePresentation> {
        let run = self.run.as_ref()?;
        Some(GuidePresentation {
            permission: run.permission,
            granted: run.step == SetupStep::Granted,
            cleared: run.cleared,
            next: run.queued.front().copied(),
            naming: self.naming,
        })
    }

    /// Names `permission` as System Settings does on the running system.
    pub(crate) fn copy(&self, permission: ComputerUsePermission) -> &'static PermissionCopy {
        permission_copy(permission, self.naming)
    }

    /// Shows the latest presentation in an open guide.
    fn present(&mut self, cx: &mut Context<Self>) {
        let Some(presentation) = self.presentation() else {
            return;
        };
        let Some(run) = &mut self.run else {
            return;
        };
        if let Some(guide) = &run.guide {
            let presented = guide
                .handle
                .update(cx, |guide, _, cx| guide.present(presentation, cx));
            // A window busy with an event keeps its guide; the next tick presents again.
            if presented.is_err() && !guide.is_open(cx) {
                run.guide = None;
            }
        }
    }

    /// Docks the guide on System Settings' content column, opening it when it is not open.
    fn present_guide(
        &mut self,
        display: DisplayId,
        content: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(visible) = cx
            .find_display(display)
            .map(|display| display.visible_bounds())
        else {
            self.dismiss_guide(cx);
            return;
        };
        let Some(presentation) = self.presentation() else {
            return;
        };
        let bounds = placement::place_guide(content, visible, guide::height(presentation));
        if self.bundle.is_none() {
            self.bundle = Some(self.host.application_bundle());
        }
        let bundle = self.bundle.clone().flatten();
        let Some(run) = &mut self.run else {
            return;
        };
        if let Some(guide) = &mut run.guide {
            let moved = guide.display != display || guide.bounds != bounds;
            let presented = guide.handle.update(cx, |view, window, cx| {
                if moved {
                    window.set_bounds(bounds, Some(display));
                }
                view.present(presentation, cx);
            });
            if presented.is_ok() {
                guide.display = display;
                guide.bounds = bounds;
                return;
            }
            // A window busy with an event is still the guide; opening another would leave two.
            if guide.is_open(cx) {
                return;
            }
            run.guide = None;
        }
        let setup = cx.weak_entity();
        let host = Arc::clone(&self.host);
        run.guide =
            guide::open(display, bounds, presentation, bundle, host, setup, cx).map(|handle| {
                GuideWindow {
                    handle,
                    display,
                    bounds,
                }
            });
    }

    /// Closes the guide. A guide busy with an event closes once the event ends, so no guide
    /// outlives its setup.
    fn dismiss_guide(&mut self, cx: &mut Context<Self>) {
        let Some(guide) = self.run.as_mut().and_then(|run| run.guide.take()) else {
            return;
        };
        let handle = guide.handle;
        let removed = handle.update(cx, |_, window, _| window.remove_window());
        if removed.is_err() && guide.is_open(cx) {
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        }
    }
}
