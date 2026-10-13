//! Pane-only repair callbacks over the installed desktop runtime.
//! A directory observation never starts a Session. The original durable operation
//! must apply and reach the replica before this controller offers deliberate use.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the Pane owner installs these callbacks in its final consumer hook"
    )
)]

use gpui::{AppContext as _, Context, Entity, EventEmitter, Task, Window};
use spaceterm_client::{
    directories::{DirectoryClient, DirectoryConnectionPort},
    git::rpc::GitConnectionState,
};
use spaceterm_core::{
    authority::{
        ConnectionBinding, ControlGeneration, FieldVersion, ServerEpoch, SessionGeneration,
    },
    identity::{ClientId, PairingId, TabId, WorkspaceId, WorktreeId},
    operations::{Command, OperationFailure, OperationId, Outcome},
    organization::{ControlRecord, ServerDirectory, SessionPhase},
    server_directories::{DirectoryPurpose, DirectoryValidationClass},
};
use std::{rc::Rc, sync::Arc};

use super::{
    DirectoryPicker, DirectoryPickerEvent, ServerDirectorySelected,
    server_source::ServerDirectorySource,
};
use crate::ui::{
    server_client_runtime::{DesktopClientEvent, DesktopClientRuntime},
    server_desktop_contracts::{
        ConnectionState, DesktopClientError, DesktopRequest, PaneId, PaneViewToken, ReplicaStatus,
        SelectionSource, UiRequestId,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaneRepairStatus {
    Choosing,
    Pending,
    Unresolved,
    StillUnavailable,
    Conflict,
    Cancelled,
    Completed,
}
impl PaneRepairStatus {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Choosing => "Choose Directory",
            Self::Pending => "Checking directory...",
            Self::Unresolved => "Resolving the original directory repair...",
            Self::StillUnavailable => "Still unavailable.",
            Self::Conflict => "The Pane directory changed. Choose Directory again.",
            Self::Cancelled => "Directory repair cancelled.",
            Self::Completed => "Directory available.",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PaneRepairEvent {
    StateChanged,
    /// The caller rechecks the token and routes this through its measured-use path.
    StartRequested {
        token: PaneViewToken,
        request: Box<DesktopRequest>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Target {
    token: PaneViewToken,
    epoch: ServerEpoch,
    connection: ConnectionBinding,
    attempt: u64,
    client: ClientId,
    pairing: PairingId,
    tab: TabId,
    workspace: WorkspaceId,
    worktree: Option<WorktreeId>,
    control: ControlGeneration,
    session: SessionGeneration,
    version: FieldVersion,
    directory: ServerDirectory,
}

impl Target {
    fn capture(runtime: &DesktopClientRuntime, pane: PaneId) -> Result<Self, DesktopClientError> {
        let frame = runtime
            .frame(pane.server())
            .ok_or(DesktopClientError::TargetGone)?;
        let ConnectionState::Connected { info } = frame.connection else {
            return Err(DesktopClientError::Offline);
        };
        if frame.replica_status != ReplicaStatus::Ready {
            return Err(DesktopClientError::Reconciling);
        }
        let record = frame
            .organization
            .state()
            .panes
            .iter()
            .find(|p| p.id == pane)
            .ok_or(DesktopClientError::TargetGone)?;
        let tab = frame
            .organization
            .state()
            .tabs
            .iter()
            .find(|t| t.id == record.tab)
            .ok_or(DesktopClientError::TargetGone)?;
        let facts = frame
            .organization
            .runtime()
            .iter()
            .find(|p| p.pane == pane)
            .ok_or(DesktopClientError::TargetGone)?;
        if !matches!(facts.control, ControlRecord::Held { client, .. } if client == info.client) {
            return Err(DesktopClientError::StaleContext);
        }
        if matches!(
            facts.phase,
            SessionPhase::Starting { .. } | SessionPhase::Running { .. }
        ) {
            return Err(DesktopClientError::StaleContext);
        }
        let token = runtime.pane_view_token(pane)?;
        if token.pane != pane
            || info.server != pane.server()
            || frame.organization.state().server != pane.server()
            || frame.organization.epoch() != info.epoch
        {
            return Err(DesktopClientError::StaleContext);
        }
        Ok(Self {
            token,
            epoch: info.epoch,
            connection: info.binding,
            attempt: info.attempt,
            client: info.client,
            pairing: info.pairing,
            tab: tab.id,
            workspace: tab.workspace,
            worktree: tab.worktree,
            control: facts.control_generation,
            session: facts.session_generation,
            version: record.confirmed_restoration_directory.version,
            directory: record.restoration_directory().clone(),
        })
    }

    fn same_incarnation(&self, current: &Self) -> bool {
        self.token == current.token
            && self.epoch == current.epoch
            && self.connection == current.connection
            && self.attempt == current.attempt
            && self.client == current.client
            && self.pairing == current.pairing
            && self.tab == current.tab
            && self.workspace == current.workspace
            && self.worktree == current.worktree
            && self.control == current.control
            && self.session == current.session
    }
}

pub(crate) struct ServerPaneRepair {
    runtime: Entity<DesktopClientRuntime>,
    picker: Entity<DirectoryPicker>,
    target: Target,
    source: SelectionSource,
    status: PaneRepairStatus,
    request: Option<UiRequestId>,
    operation: Option<OperationId>,
    command: Option<Command>,
    cancelled: bool,
    directory: DirectoryClient,
    directory_port: Arc<dyn DirectoryConnectionPort>,
    directory_context: GitConnectionState,
    retry_task: Option<Task<()>>,
}
impl EventEmitter<PaneRepairEvent> for ServerPaneRepair {}

impl ServerPaneRepair {
    /// Uses the existing runtime and authenticated port. It owns neither a journal nor a connection.
    pub(crate) fn new(
        runtime: Entity<DesktopClientRuntime>,
        pane: PaneId,
        source: SelectionSource,
        name: &str,
        directory: Arc<dyn DirectoryConnectionPort>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Self, DesktopClientError> {
        let target = Target::capture(runtime.read(cx), pane)?;
        let state = directory.state().map_err(|_| DesktopClientError::Offline)?;
        let context = state.context.parts();
        if context.server != pane.server()
            || context.epoch != target.epoch
            || context.client != target.client
            || context.pairing != target.pairing
            || state.generation != target.connection.generation
        {
            return Err(DesktopClientError::StaleContext);
        }
        let directory_client = DirectoryClient::new(pane.server(), directory.clone())
            .map_err(|_| DesktopClientError::Offline)?;
        let directory_port = directory.clone();
        let directory = ServerDirectorySource::new(
            pane.server(),
            name,
            directory,
            DirectoryPurpose::PaneRepair,
        )
        .map_err(|_| DesktopClientError::Offline)?;
        let picker =
            cx.new(|cx| DirectoryPicker::new(Rc::new(directory), "Choose Directory", window, cx));
        cx.subscribe_in(
            &picker,
            window,
            |this, _, event: &ServerDirectorySelected, window, cx| {
                this.choose(event.0.clone(), window, cx);
            },
        )
        .detach();
        cx.subscribe_in(
            &picker,
            window,
            |this, _, event: &DirectoryPickerEvent, _, cx| {
                if matches!(event, DirectoryPickerEvent::Dismissed) && this.command.is_none() {
                    this.cancelled = true;
                    this.status = PaneRepairStatus::Cancelled;
                    cx.emit(PaneRepairEvent::StateChanged);
                }
            },
        )
        .detach();
        cx.subscribe_in(
            &runtime,
            window,
            |this, _, event: &DesktopClientEvent, window, cx| {
                this.runtime_event(event, window, cx);
            },
        )
        .detach();
        Ok(Self {
            runtime,
            picker,
            target,
            source,
            status: PaneRepairStatus::Choosing,
            request: None,
            operation: None,
            command: None,
            cancelled: false,
            directory: directory_client,
            directory_port,
            directory_context: state,
            retry_task: None,
        })
    }

    pub(in crate::ui) fn picker(&self) -> Entity<DirectoryPicker> {
        self.picker.clone()
    }
    pub(crate) fn status(&self) -> PaneRepairStatus {
        self.status
    }
    pub(crate) fn original_operation(&self) -> Option<OperationId> {
        self.operation
    }
    pub(crate) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.cancelled || self.command.is_some() || self.retry_task.is_some() {
            return false;
        }
        self.picker.update(cx, |picker, cx| picker.open(window, cx))
    }

    /// Retry observes the exact retained target without repairing any metadata.
    /// The existing Retry request still owns measured startup and final validation.
    pub(crate) fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cancelled
            || self.command.is_some()
            || self.retry_task.is_some()
            || self.picker.read(cx).blocks_terminal_input()
        {
            return;
        }
        if self.directory_port.state().ok() != Some(self.directory_context) {
            self.cancel(window, cx);
            return;
        }
        let directory = self.directory.clone();
        let target = self.target.clone();
        self.status = PaneRepairStatus::Pending;
        self.retry_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = directory
                .validate(target.directory.clone(), DirectoryPurpose::SessionLaunch)
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.retry_task = None;
                if this.cancelled {
                    return;
                }
                let current = Target::capture(this.runtime.read(cx), target.token.pane);
                if !current.is_ok_and(|current| current == target)
                    || this.directory_port.state().ok() != Some(this.directory_context)
                {
                    this.cancel(window, cx);
                    return;
                }
                if result
                    .is_ok_and(|value| value.class == DirectoryValidationClass::AvailableDirectory)
                {
                    this.status = PaneRepairStatus::Completed;
                    this.cancelled = true;
                    cx.emit(PaneRepairEvent::StartRequested {
                        token: target.token,
                        request: Box::new(DesktopRequest::Retry {
                            pane: target.token.pane,
                            source: this.source,
                        }),
                    });
                } else {
                    this.status = PaneRepairStatus::StillUnavailable;
                }
                cx.emit(PaneRepairEvent::StateChanged);
            });
        }));
        cx.emit(PaneRepairEvent::StateChanged);
    }

    fn choose(&mut self, directory: ServerDirectory, window: &mut Window, cx: &mut Context<Self>) {
        if directory.server() != self.target.token.pane.server() {
            return;
        }
        self.begin(
            Command::RepairPaneDirectory {
                pane: self.target.token.pane,
                expected_version: self.target.version,
                value: directory,
            },
            window,
            cx,
        );
    }

    fn begin(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        if self.cancelled || self.command.is_some() || self.retry_task.is_some() {
            return;
        }
        let current = Target::capture(self.runtime.read(cx), self.target.token.pane);
        if !current.is_ok_and(|current| current == self.target)
            || self.directory_port.state().ok() != Some(self.directory_context)
        {
            self.cancel(window, cx);
            return;
        }
        let result = self.runtime.update(cx, |runtime, cx| {
            runtime.request(
                DesktopRequest::Shared {
                    server: self.target.token.pane.server(),
                    command: command.clone(),
                },
                cx,
            )
        });
        match result {
            Ok(request) => {
                self.request = Some(request);
                self.operation = None;
                self.command = Some(command);
                self.status = PaneRepairStatus::Pending;
            }
            Err(_) => {
                self.status = PaneRepairStatus::StillUnavailable;
                self.picker
                    .update(cx, |picker, cx| picker.activation_failed(window, cx));
            }
        }
        cx.emit(PaneRepairEvent::StateChanged);
    }

    fn runtime_event(
        &mut self,
        event: &DesktopClientEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.cancelled {
            return;
        }
        match event {
            DesktopClientEvent::RequestCompleted { request, operation }
                if Some(*request) == self.request =>
            {
                self.request = None;
                // A shared edit without its retained operation is never evidence of repair.
                self.operation = operation.filter(|operation| {
                    let stream = operation.stream();
                    stream.server() == self.target.token.pane.server()
                        && stream.pairing() == self.target.pairing
                        && stream.client() == self.target.client
                });
                if self.operation.is_none() {
                    self.status = PaneRepairStatus::Unresolved;
                }
            }
            DesktopClientEvent::RequestFailed { request, .. } if Some(*request) == self.request => {
                self.request = None;
                // The journal may already contain the operation. Do not replace it.
                self.status = PaneRepairStatus::Unresolved;
            }
            DesktopClientEvent::FrameChanged(server)
                if *server == self.target.token.pane.server() => {}
            DesktopClientEvent::OperationOutcome { operation, .. }
                if Some(*operation) == self.operation => {}
            _ => return,
        }
        self.advance(window, cx);
        cx.emit(PaneRepairEvent::StateChanged);
    }

    fn advance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = Target::capture(self.runtime.read(cx), self.target.token.pane);
        let Ok(current) = current else {
            self.cancel(window, cx);
            return;
        };
        if !self.target.same_incarnation(&current)
            || self.directory_port.state().ok() != Some(self.directory_context)
        {
            self.cancel(window, cx);
            return;
        }
        let Some(operation) = self.operation else {
            return;
        };
        let runtime = self.runtime.read(cx);
        let Some(outcome) = runtime.outcome(operation) else {
            self.status = PaneRepairStatus::Unresolved;
            return;
        };
        match outcome.as_ref() {
            Outcome::Applied => {
                let Some(revision) = runtime.outcome_revision(operation) else {
                    return;
                };
                let Some(frame) = runtime.frame(self.target.token.pane.server()) else {
                    return;
                };
                if frame.organization.state().revision < revision {
                    return;
                }
                let desired = match self.command.as_ref() {
                    Some(Command::RepairPaneDirectory { value, .. }) => value,
                    _ => return,
                };
                if &current.directory != desired {
                    self.status = PaneRepairStatus::Conflict;
                    self.command = None;
                    self.target = current;
                    self.picker
                        .update(cx, |picker, cx| picker.activation_failed(window, cx));
                    return;
                }
                let unavailable = frame.organization.runtime().iter().any(|p| {
                    p.pane == self.target.token.pane
                        && p.phase == SessionPhase::DirectoryUnavailable
                });
                if unavailable {
                    self.status = PaneRepairStatus::StillUnavailable;
                    self.command = None;
                    self.target = current;
                    self.picker
                        .update(cx, |picker, cx| picker.repair_unavailable(window, cx));
                    return;
                }
                self.status = PaneRepairStatus::Completed;
                self.cancelled = true; // Explicit use is offered exactly once.
                self.picker.update(cx, |picker, cx| {
                    picker.complete_activation(window, cx);
                });
                cx.emit(PaneRepairEvent::StartRequested {
                    token: self.target.token,
                    request: Box::new(DesktopRequest::StartSession {
                        pane: self.target.token.pane,
                        source: self.source,
                    }),
                });
            }
            Outcome::Conflict(_) => {
                self.command = None;
                self.target = current;
                self.status = PaneRepairStatus::Conflict;
                self.picker
                    .update(cx, |picker, cx| picker.activation_failed(window, cx));
            }
            Outcome::Failed(OperationFailure::DirectoryUnavailable) => {
                self.command = None;
                self.target = current;
                self.status = PaneRepairStatus::StillUnavailable;
                self.picker
                    .update(cx, |picker, cx| picker.repair_unavailable(window, cx));
            }
            Outcome::Failed(_) => {
                self.command = None;
                self.target = current;
                self.status = PaneRepairStatus::StillUnavailable;
                self.picker
                    .update(cx, |picker, cx| picker.repair_unavailable(window, cx));
            }
            _ => self.status = PaneRepairStatus::Unresolved,
        }
    }

    /// Cancels local startup intent, not a possibly committed server edit.
    pub(crate) fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancelled = true;
        self.retry_task.take();
        self.status = PaneRepairStatus::Cancelled;
        self.picker
            .update(cx, |picker, cx| picker.cancel(window, cx));
        cx.emit(PaneRepairEvent::StateChanged);
    }
}

#[cfg(test)]
mod tests;
