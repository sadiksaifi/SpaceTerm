//! Permission Setup: one guided pass through System Settings that adds SpaceTerm to the privacy
//! lists computer-use tools need.
//!
//! A setup starts only when someone asks for it: a person choosing Set Up in Settings, or
//! accepting a Permission Request from a tool in a Terminal Session. It verifies the permission,
//! clears a stale entry, opens System Settings at the permission's list, and docks the Setup Guide
//! beside System Settings' window while that window is in front. The guide offers SpaceTerm itself
//! to drag into the list and reports the grant as soon as a tool started now would receive it.
//! Closing System Settings ends the setup.

mod guide;
mod placement;
#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, AppContext as _, Bounds, Context, DisplayId, Entity, Global, Pixels, Task};

use crate::platform::computer_use_access::{
    ComputerUseAccess, ComputerUseAccessSubscription, ComputerUseAuthorization,
    ComputerUsePermission, ComputerUseSetupReadiness,
};
use crate::platform::setup_guide_host::{ApplicationBundle, SetupGuideHost, SystemSettingsWindow};

pub(crate) use guide::SetupGuide;

/// How often the guide follows System Settings' window, which a person can move at any time.
const TRACKING_INTERVAL: Duration = Duration::from_millis(33);
/// How many tracking intervals pass between authorization reads. System Settings reports no
/// Screen Recording change, so the setup reads it about once a second while guiding.
const AUTHORIZATION_INTERVALS: u32 = 30;
/// How long System Settings may take to come forward before the setup reports it did not.
const OPENING_TIMEOUT: Duration = Duration::from_secs(10);

/// The installed Permission Setup, shared by Settings and every Pane.
struct InstalledPermissionSetup(Entity<PermissionSetup>);

impl Global for InstalledPermissionSetup {}

/// Installs the Permission Setup a host with computer-use access and a Setup Guide composes.
pub(crate) fn install(
    access: Rc<dyn ComputerUseAccess>,
    host: Arc<dyn SetupGuideHost>,
    cx: &mut App,
) {
    let setup = cx.new(|_| PermissionSetup::new(access, host));
    cx.set_global(InstalledPermissionSetup(setup));
}

/// The installed Permission Setup, or `None` when the host composes none.
pub(crate) fn installed(cx: &App) -> Option<Entity<PermissionSetup>> {
    cx.try_global::<InstalledPermissionSetup>()
        .map(|installed| installed.0.clone())
}

/// How SpaceTerm names one permission and the System Settings list that holds it.
pub(crate) struct PermissionCopy {
    /// The permission's name in running text.
    pub(crate) name: &'static str,
    /// The System Settings list that holds the grant.
    pub(crate) pane: &'static str,
    /// What a computer-use tool does with the grant.
    pub(crate) purpose: &'static str,
}

pub(crate) const fn permission_copy(permission: ComputerUsePermission) -> &'static PermissionCopy {
    match permission {
        ComputerUsePermission::ScreenRecording => &PermissionCopy {
            name: "Screen Recording",
            pane: "Screen & System Audio Recording",
            purpose: "take screenshots",
        },
        ComputerUsePermission::Accessibility => &PermissionCopy {
            name: "Device Control",
            pane: "Device Control and Data Access",
            purpose: "click and type in other apps",
        },
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
    /// The guide waits beside System Settings for the person to add SpaceTerm.
    Guiding,
    /// Tools started now receive the permission.
    Granted,
}

/// What the Setup Guide shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GuidePresentation {
    pub(crate) permission: ComputerUsePermission,
    pub(crate) granted: bool,
    /// The permission set up after this one, when the setup holds another.
    pub(crate) next: Option<ComputerUsePermission>,
}

/// Owns the one running setup and the Setup Guide window it presents.
pub(crate) struct PermissionSetup {
    access: Rc<dyn ComputerUseAccess>,
    host: Arc<dyn SetupGuideHost>,
    run: Option<SetupRun>,
    /// The latest failure of each permission's setup, cleared when its next setup starts.
    failures: Vec<(ComputerUsePermission, PermissionSetupFailure)>,
    /// The running application as the guide offers it, resolved once on first use.
    bundle: Option<Option<ApplicationBundle>>,
}

struct SetupRun {
    permission: ComputerUsePermission,
    queued: VecDeque<ComputerUsePermission>,
    step: SetupStep,
    guide: Option<GuideWindow>,
    intervals: u32,
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

impl PermissionSetup {
    fn new(access: Rc<dyn ComputerUseAccess>, host: Arc<dyn SetupGuideHost>) -> Self {
        Self {
            access,
            host,
            run: None,
            failures: Vec::new(),
            bundle: None,
        }
    }

    pub(crate) fn status(&self, permission: ComputerUsePermission) -> PermissionSetupStatus {
        if self.run.as_ref().is_some_and(|run| {
            run.permission == permission || run.queued.contains(&permission)
        }) {
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
            if matches!(run.step, SetupStep::Opening | SetupStep::Guiding) {
                let _ = self.access.open_settings(run.permission);
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
            loop {
                cx.background_executor().timer(TRACKING_INTERVAL).await;
                let host = Arc::clone(&host);
                let located = cx
                    .background_spawn(async move { host.locate_system_settings() })
                    .await;
                if setup
                    .update(cx, |setup, cx| setup.follow(located, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.run = Some(SetupRun {
            permission,
            queued: requested,
            step: SetupStep::Preparing,
            guide: None,
            intervals: 0,
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
        if self.current().is_some_and(|(_, step)| step == SetupStep::Granted) {
            self.finish(cx);
            cx.activate(true);
        }
    }

    /// Moves from a granted permission to the next one.
    pub(crate) fn continue_setup(&mut self, cx: &mut Context<Self>) {
        if self.current().is_some_and(|(_, step)| step == SetupStep::Granted) {
            self.advance(cx);
        }
    }

    fn prepare(&mut self, cx: &mut Context<Self>) {
        let Some(run) = &mut self.run else {
            return;
        };
        run.step = SetupStep::Preparing;
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
            Some(ComputerUseSetupReadiness::Ready) | None => self.open(cx),
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

    fn follow(&mut self, located: SystemSettingsWindow, cx: &mut Context<Self>) {
        let Some(run) = &mut self.run else {
            return;
        };
        run.intervals = run.intervals.wrapping_add(1);
        let read_due = run.intervals % AUTHORIZATION_INTERVALS == 0;
        match (run.step, located) {
            (SetupStep::Preparing, _) => {}
            // System Settings may still be launching or switching lists.
            (SetupStep::Opening, SystemSettingsWindow::Closed | SystemSettingsWindow::Covered) => {}
            (SetupStep::Opening, SystemSettingsWindow::Frontmost { display, bounds }) => {
                run.step = SetupStep::Guiding;
                run._step = None;
                cx.notify();
                self.present_guide(display, bounds, cx);
            }
            (_, SystemSettingsWindow::Closed) => self.finish(cx),
            (_, SystemSettingsWindow::Covered) => self.dismiss_guide(cx),
            (_, SystemSettingsWindow::Frontmost { display, bounds }) => {
                self.present_guide(display, bounds, cx);
            }
        }
        if read_due {
            self.read_authorization(cx);
        }
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
            next: run.queued.front().copied(),
        })
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
            if presented.is_err() {
                run.guide = None;
            }
        }
    }

    /// Docks the guide on System Settings' window, opening it when it is not open.
    fn present_guide(
        &mut self,
        display: DisplayId,
        settings: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(visible) = cx
            .find_display(display)
            .map(|display| display.visible_bounds())
        else {
            self.dismiss_guide(cx);
            return;
        };
        let bounds = placement::place_guide(settings, visible, guide::GUIDE_HEIGHT);
        let Some(presentation) = self.presentation() else {
            return;
        };
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
            run.guide = None;
        }
        let setup = cx.weak_entity();
        let host = Arc::clone(&self.host);
        run.guide = guide::open(display, bounds, presentation, bundle, host, setup, cx).map(|handle| {
            GuideWindow {
                handle,
                display,
                bounds,
            }
        });
    }

    /// Closes the guide. The guide's own buttons defer to the setup, so the guide never closes
    /// itself while it handles an event.
    fn dismiss_guide(&mut self, cx: &mut Context<Self>) {
        if let Some(guide) = self.run.as_mut().and_then(|run| run.guide.take()) {
            let _ = guide
                .handle
                .update(cx, |_, window, _| window.remove_window());
        }
    }
}
