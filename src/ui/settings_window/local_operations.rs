//! Completion of admitted account-local service changes belongs to the desktop application.

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Task};

use super::SettingsWindow;
use super::core_update::UpdateState;
use super::machines::ServiceState;
use crate::machine_connection::CoreError;
use crate::machine_connection::service::ServiceStatus;
use crate::machine_connection::service::activation::{ActivationPhase, ActivationStatus};
use crate::updates::machines::MachineAction;
use crate::updates::machines::store::{MachineUpdates, Surface};

struct InstalledOperations(Entity<LocalCoreOperations>);
impl Global for InstalledOperations {}

#[derive(Clone, Copy)]
pub(crate) enum CompletedOperation {
    Service,
    Update,
    /// A service change made outside Settings, such as from the status item. Settings reads the
    /// service again rather than trusting what it observed before.
    Elsewhere,
}

pub(super) struct UpdateCompletion {
    pub(super) state: UpdateState,
    pub(super) outcome: Result<Option<ActivationStatus>, CoreError>,
}

/// Retains admitted progress and typed outcomes across Settings Window replacement. Preparation
/// and destructive confirmation stay with the window that requested them.
#[derive(Default)]
pub(crate) struct LocalCoreOperations {
    pub(super) service: Option<ServiceState>,
    pub(super) update: Option<UpdateState>,
    /// Admitted failures survive passive reads; a successful read still clears a read failure.
    pub(super) service_failure: Option<CoreError>,
    pub(super) update_failure: Option<CoreError>,
    last_update: Option<Result<Option<ActivationStatus>, CoreError>>,
    operation: Option<Task<()>>,
    /// The service refused the person's Start, from any surface, because SpaceTerm Core was off
    /// in Login Items. The first surface that finds the item on runs it, once. Any service change
    /// that finishes first forgets it, so it never reverses a later Stop.
    refused_start: bool,
}

impl EventEmitter<CompletedOperation> for LocalCoreOperations {}

impl LocalCoreOperations {
    pub(crate) fn get(cx: &mut App) -> Entity<Self> {
        if let Some(installed) = cx.try_global::<InstalledOperations>() {
            return installed.0.clone();
        }
        let owner = cx.new(|_| Self::default());
        cx.set_global(InstalledOperations(owner.clone()));
        owner
    }

    /// Records the person's Start the service refused because SpaceTerm Core is off in Login
    /// Items, made from launch, the menu bar or the title bar.
    pub(crate) fn start_refused(cx: &mut App) {
        Self::get(cx).update(cx, |owner, _| owner.refused_start = true);
    }

    /// Whether a refused Start waits for SpaceTerm Core to be on in Login Items.
    pub(crate) fn start_waits(cx: &App) -> bool {
        cx.try_global::<InstalledOperations>()
            .is_some_and(|installed| installed.0.read(cx).refused_start)
    }

    /// Takes the refused Start, so the surface that found the item on runs it and no other
    /// surface does.
    pub(crate) fn take_refused_start(cx: &mut App) -> bool {
        Self::start_waits(cx)
            && Self::get(cx).update(cx, |owner, _| std::mem::take(&mut owner.refused_start))
    }

    /// Forgets a refused Start because this computer's SpaceTerm Core is running now.
    pub(crate) fn core_running(cx: &mut App) {
        if Self::start_waits(cx) {
            Self::get(cx).update(cx, |owner, _| owner.refused_start = false);
        }
    }

    /// Runs a refused Start through `surface` once `off` reads SpaceTerm Core on in Login Items,
    /// reading for at most [`crate::local_startup::LOGIN_ITEM_WAIT`]. Another surface that runs
    /// it first, or any service change, leaves nothing to run. Dropping the task ends the wait.
    pub(crate) fn start_when_login_item_on(
        off: crate::local_startup::LoginItemRead,
        surface: Surface,
        cx: &App,
    ) -> Task<()> {
        crate::local_startup::when_login_item_on(off, cx, move |cx| {
            Self::run_refused_start(surface, cx);
        })
    }

    /// Reads SpaceTerm Core's item once, off the event thread, while a refused Start waits, and
    /// runs that Start through `surface` if the item is on.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "only the native status menu calls it")
    )]
    pub(crate) fn start_if_login_item_on(
        off: crate::local_startup::LoginItemRead,
        surface: Surface,
        cx: &mut App,
    ) {
        if !Self::start_waits(cx) {
            return;
        }
        let read = cx.background_spawn(async move { off() });
        cx.spawn(async move |cx| {
            if !read.await {
                cx.update(|cx| Self::run_refused_start(surface, cx));
            }
        })
        .detach();
    }

    fn run_refused_start(surface: Surface, cx: &mut App) {
        let Some(store) = MachineUpdates::global(cx) else {
            return;
        };
        let Some(local) = store
            .read(cx)
            .machines()
            .iter()
            .find(|machine| machine.local)
            .map(|machine| machine.machine)
        else {
            return;
        };
        if Self::take_refused_start(cx) {
            store.update(cx, |store, cx| {
                store.perform(local, MachineAction::Start, surface, cx);
            });
        }
    }

    /// A Start, Restart or Stop of this computer's Core that the menu bar or the title bar
    /// performed succeeded. It reaches Settings and every waiting Workspace Window, as the same
    /// change made in Settings does.
    pub(crate) fn machine_action_succeeded(action: &MachineAction, cx: &mut App) {
        let running = match action {
            MachineAction::Start | MachineAction::Restart => true,
            MachineAction::Stop => false,
            _ => return,
        };
        // Settings reads the service again; only `running` and `stopped` matter here.
        Self::changed_elsewhere(
            cx,
            ServiceStatus {
                installed: true,
                enabled: true,
                running,
                stopped: !running,
            },
        );
    }

    pub(super) fn run_service(
        &mut self,
        state: ServiceState,
        completion: Task<ServiceState>,
        cx: &mut Context<Self>,
    ) {
        self.service_failure = None;
        self.service = Some(state);
        self.operation = Some(cx.spawn(async move |owner, cx| {
            let state = completion.await;
            let _ = owner.update(cx, |owner, cx| {
                let running = state
                    .status
                    .is_some_and(|status| status.running && !status.stopped);
                // Authorization refusal admits no service change and may clear on a later read.
                owner.service_failure = state
                    .failure
                    .filter(|error| *error != CoreError::AuthorizationRequired);
                let refused = state.start_refused;
                owner.service = Some(state);
                owner.finished(CompletedOperation::Service, running, cx);
                // A Start this operation had refused waits for the item, as one from any other
                // surface does.
                owner.refused_start = refused;
            });
        }));
    }

    pub(super) fn run_update(
        &mut self,
        state: UpdateState,
        completion: Task<UpdateCompletion>,
        cx: &mut Context<Self>,
    ) {
        self.update_failure = None;
        self.last_update = None;
        self.update = Some(state);
        self.operation = Some(cx.spawn(async move |owner, cx| {
            let UpdateCompletion { state, outcome } = completion.await;
            let _ = owner.update(cx, |owner, cx| {
                owner.last_update = Some(outcome);
                let running = state
                    .facts
                    .as_ref()
                    .and_then(|facts| facts.status.ok())
                    .map(|status| status.running && !status.stopped)
                    .unwrap_or_else(|| {
                        matches!(
                            owner.last_update.as_ref(),
                            Some(Ok(Some(ActivationStatus {
                                stopped: false,
                                phase: ActivationPhase::Completed { running: true },
                                ..
                            })))
                        )
                    });
                // A follow-up read can fail after activation succeeded. Retain the operation's
                // own failure; a successful later read clears only the observation failure.
                owner.update_failure = owner
                    .last_update
                    .as_ref()
                    .and_then(|outcome| outcome.as_ref().err())
                    .copied();
                owner.update = Some(state);
                owner.finished(CompletedOperation::Update, running, cx);
            });
        }));
    }

    /// Records a service change that finished outside Settings, such as from the status item, and
    /// leaves `status` as the service last reported it. Every Settings window reads the service
    /// again, and Workspace launches waiting on a running server resume, as after a change made
    /// in Settings.
    pub(crate) fn changed_elsewhere(cx: &mut App, status: ServiceStatus) {
        let owner = Self::get(cx);
        owner.update(cx, |owner, cx| {
            // A change Settings has underway keeps its own progress; anything Settings observed
            // before no longer describes the service.
            if owner
                .service
                .as_ref()
                .is_none_or(|state| state.working.is_none())
            {
                owner.service = None;
                owner.service_failure = None;
            }
            owner.finished(
                CompletedOperation::Elsewhere,
                status.running && !status.stopped,
                cx,
            );
        });
    }

    /// Every finished service change forgets a refused Start, so it can't reverse what the
    /// change did.
    fn finished(&mut self, operation: CompletedOperation, running: bool, cx: &mut Context<Self>) {
        self.refused_start = false;
        // This notification must reach waiting Workspace launches even with no Settings Window.
        if running {
            crate::ui::workspace_manager::resume_waiting_launches(cx);
        }
        cx.emit(operation);
    }
}

impl SettingsWindow {
    pub(super) fn synchronize_local_operations(&mut self, cx: &App) {
        let owner = self.local_operations.read(cx);
        if let (Some(row), Some(state)) = (&mut self.core_service, &owner.service) {
            row.state = state.clone();
        }
        if let (Some(row), Some(state)) = (&mut self.core_update, &owner.update) {
            row.state = state.clone();
        }
    }
}
