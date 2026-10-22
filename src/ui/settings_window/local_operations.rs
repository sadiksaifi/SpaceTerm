//! Completion of admitted account-local service changes belongs to the desktop application.

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Task};

use super::SettingsWindow;
use super::server_update::UpdateState;
use super::servers::ServiceState;
use crate::local_server::ServerError;
use crate::local_server::service::activation::{ActivationPhase, ActivationStatus};

struct InstalledOperations(Entity<LocalServerOperations>);
impl Global for InstalledOperations {}

#[derive(Clone, Copy)]
pub(super) enum CompletedOperation {
    Service,
    Update,
}

pub(super) struct UpdateCompletion {
    pub(super) state: UpdateState,
    pub(super) outcome: Result<Option<ActivationStatus>, ServerError>,
}

/// Retains admitted progress and typed outcomes across Settings Window replacement. Preparation
/// and destructive confirmation stay with the window that requested them.
#[derive(Default)]
pub(super) struct LocalServerOperations {
    pub(super) service: Option<ServiceState>,
    pub(super) update: Option<UpdateState>,
    /// Admitted failures survive passive reads; a successful read still clears a read failure.
    pub(super) service_failure: Option<ServerError>,
    pub(super) update_failure: Option<ServerError>,
    last_update: Option<Result<Option<ActivationStatus>, ServerError>>,
    operation: Option<Task<()>>,
}

impl EventEmitter<CompletedOperation> for LocalServerOperations {}

impl LocalServerOperations {
    pub(super) fn get(cx: &mut App) -> Entity<Self> {
        if let Some(installed) = cx.try_global::<InstalledOperations>() {
            return installed.0.clone();
        }
        let owner = cx.new(|_| Self::default());
        cx.set_global(InstalledOperations(owner.clone()));
        owner
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
                owner.service_failure = state.failure;
                owner.service = Some(state);
                owner.finished(CompletedOperation::Service, running, cx);
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
                owner.update_failure = state.failure;
                owner.update = Some(state);
                owner.finished(CompletedOperation::Update, running, cx);
            });
        }));
    }

    fn finished(&mut self, operation: CompletedOperation, running: bool, cx: &mut Context<Self>) {
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
        if let (Some(row), Some(state)) = (&mut self.server_service, &owner.service) {
            row.state = state.clone();
        }
        if let (Some(row), Some(state)) = (&mut self.server_update, &owner.update) {
            row.state = state.clone();
        }
    }
}
