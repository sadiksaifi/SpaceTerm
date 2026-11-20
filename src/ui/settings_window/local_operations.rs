//! Completion of admitted account-local service changes belongs to the desktop application.

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Task};

use super::SettingsWindow;
use super::machines::ServiceState;
use crate::machine_connection::CoreError;
use crate::machine_connection::service::ServiceStatus;

struct InstalledOperations(Entity<LocalCoreOperations>);
impl Global for InstalledOperations {}

#[derive(Clone, Copy)]
pub(crate) enum CompletedOperation {
    Service,
    /// A service change made outside Settings, such as from the status item. Settings reads the
    /// service again rather than trusting what it observed before.
    Elsewhere,
}

/// Retains admitted progress and typed outcomes across Settings Window replacement. Preparation
/// and destructive confirmation stay with the window that requested them.
#[derive(Default)]
pub(crate) struct LocalCoreOperations {
    pub(super) service: Option<ServiceState>,
    /// Admitted failures survive passive reads; a successful read still clears a read failure.
    pub(super) service_failure: Option<CoreError>,
    operation: Option<Task<()>>,
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
                owner.service = Some(state);
                owner.finished(CompletedOperation::Service, running, cx);
            });
        }));
    }

    /// Records a service change that finished outside Settings, such as from the status item, and
    /// leaves `status` as the service last reported it. Every Settings window reads the service
    /// again, and Workspace launches waiting on a running server resume, as after a change made
    /// in Settings.
    pub(crate) fn changed_elsewhere(cx: &mut App, status: ServiceStatus) {
        Self::elsewhere(cx, Some(status.running && !status.stopped));
    }

    /// Records that a Workspace Window's launch reached this computer's SpaceTerm Core, which may
    /// have started it, as the person's Start in that window does. Every Settings window reads
    /// the service again. The window that launched already presents the running Core, so no
    /// waiting launch resumes.
    pub(crate) fn launched_elsewhere(cx: &mut App) {
        Self::elsewhere(cx, None);
    }

    /// `running` says whether the change left SpaceTerm Core running, so waiting Workspace
    /// launches resume. `None` resumes none.
    fn elsewhere(cx: &mut App, running: Option<bool>) {
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
            match running {
                Some(running) => owner.finished(CompletedOperation::Elsewhere, running, cx),
                None => cx.emit(CompletedOperation::Elsewhere),
            }
        });
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
        if let (Some(row), Some(state)) = (&mut self.core_service, &owner.service) {
            row.state = state.clone();
        }
    }
}
