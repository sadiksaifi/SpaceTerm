//! Opens a Workspace on another computer's SpaceTerm Server, reached through a saved SSH
//! connection.
//!
//! The person chooses an SSH host. Choosing it connects through the desktop's SSH server
//! registry exactly as the Servers Settings Section's Connect does, so the connection becomes a
//! Saved Connection and any sign-in, host approval, or verified installation is asked for here,
//! in front of the person. A connection that already exists is reused. Once the server is
//! presented, the flow either completes at once or browses that server's directories through the
//! server itself. Nothing reads or validates a directory on this computer.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    App, BackgroundExecutor, Context, Entity, EventEmitter, FocusHandle, Render, Task, Window, div,
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionRole, ModalId,
    ModalPresentationHandle, ProgressCancelDecision, ProgressCancellation, ProgressDialog,
    ProgressDialogHandle, ProgressDialogOutcome, ProgressDialogUpdate, ProgressState,
};

use super::directory_picker::{
    DirectoryPicker, DirectoryPickerEvent, DirectorySelection, ServerDirectorySource,
};
use super::ssh_host_form::{
    ManagedHostFormBackend, ManagedHostFormBackendError, SshHostForm, SshHostFormEvent,
};
use super::ssh_host_picker::{
    HostDiscoveryProvider, SshHostPicker, SshHostPickerEvent, SshHostPickerLifecycleEvent,
};
use super::ssh_servers::{SshHostSource, SshServerAction, platform_name, present};
use crate::domain::SshDestination;
use crate::ssh::host_config::HostDiscovery;
use crate::ssh::managed_hosts::ManagedSshHost;
use crate::ssh::server_connection::{
    RemoteConnectionStatus, SshFlowConnectionIntent, SshServerConnections,
};

const CONNECTION_PROGRESS_ID: &str = "remote-workspace-connection-progress";
const CONNECTION_ERROR_ID: &str = "remote-workspace-connection-error";
const INSTALLATION_ID: &str = "remote-workspace-installation";

/// Where a new Remote Workspace's Terminal Sessions start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteWorkspaceStart {
    /// The server account's home directory, unpinned.
    Home,
    /// A directory chosen on the server after connecting, pinned.
    ChosenDirectory,
}

/// The server and directory a flow chose. The directory is a path on the server's computer,
/// which grants no authority on this one. `server` is the exact connection the flow reached,
/// so a request never lands on a later connection to the same destination.
#[derive(Clone)]
pub(crate) struct RemoteWorkspaceChoice {
    pub(crate) destination: SshDestination,
    pub(crate) directory: Option<PathBuf>,
    pub(crate) server: crate::local_server::LocalServer,
}

impl std::fmt::Debug for RemoteWorkspaceChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RemoteWorkspaceChoice(<redacted>)")
    }
}

#[derive(Clone, Debug)]
pub(crate) enum RemoteWorkspaceFlowEvent {
    StateChanged,
    /// The chosen server is connected and the Workspace belongs on it, as
    /// [`RemoteWorkspaceFlow::opening`] describes. The flow keeps showing that it is opening
    /// until [`RemoteWorkspaceFlow::opened`] reports the server took the request, so a server
    /// still finishing its first reading never drops it.
    Ready,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteWorkspaceFlowStage {
    Idle,
    HostSelection,
    AddingHost,
    Connecting,
    ReviewingInstallation,
    ConnectionError,
    ChoosingDirectory,
    Opening,
    Completed,
    Cancelled,
}

/// Why the flow can't continue, as its alert says it.
struct ConnectionProblem {
    title: String,
    state: &'static str,
    explanation: gpui::SharedString,
    /// Whether Try Again can help.
    retry: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionErrorAction {
    Retry,
    Back,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstallationAction {
    Install,
    Cancel,
}

enum FlowState {
    Idle,
    HostSelection,
    AddingHost {
        _form: Entity<SshHostForm>,
    },
    Connecting {
        destination: SshDestination,
        /// Absent while SSH asks the person to sign in, whose prompt takes the progress slot.
        progress: Option<ProgressDialogHandle>,
        _follow: Task<()>,
    },
    ReviewingInstallation {
        destination: SshDestination,
        alert: Option<ModalPresentationHandle>,
    },
    ConnectionError {
        destination: SshDestination,
        alert: Option<ModalPresentationHandle>,
    },
    ChoosingDirectory {
        picker: Entity<DirectoryPicker>,
    },
    /// The Workspace waits for the server's edits to be ready.
    Opening {
        choice: Box<RemoteWorkspaceChoice>,
        progress: Option<ProgressDialogHandle>,
        _follow: Task<()>,
    },
    Completed,
    Cancelled,
}

impl FlowState {
    const fn stage(&self) -> RemoteWorkspaceFlowStage {
        match self {
            Self::Idle => RemoteWorkspaceFlowStage::Idle,
            Self::HostSelection => RemoteWorkspaceFlowStage::HostSelection,
            Self::AddingHost { .. } => RemoteWorkspaceFlowStage::AddingHost,
            Self::Connecting { .. } => RemoteWorkspaceFlowStage::Connecting,
            Self::ReviewingInstallation { .. } => RemoteWorkspaceFlowStage::ReviewingInstallation,
            Self::ConnectionError { .. } => RemoteWorkspaceFlowStage::ConnectionError,
            Self::ChoosingDirectory { .. } => RemoteWorkspaceFlowStage::ChoosingDirectory,
            Self::Opening { .. } => RemoteWorkspaceFlowStage::Opening,
            Self::Completed => RemoteWorkspaceFlowStage::Completed,
            Self::Cancelled => RemoteWorkspaceFlowStage::Cancelled,
        }
    }

    /// Closes whatever this state presents. Cancelling a connection is the caller's decision.
    fn dismiss(&mut self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Connecting { progress, .. } | Self::Opening { progress, .. } => {
                if let Some(progress) = progress.take() {
                    let _ = progress.dismiss(window, cx);
                }
            }
            Self::ReviewingInstallation { alert, .. } | Self::ConnectionError { alert, .. } => {
                if let Some(alert) = alert.take() {
                    let _ = alert.dismiss(window, cx);
                }
            }
            Self::ChoosingDirectory { picker } => {
                picker.update(cx, |picker, cx| picker.cancel(window, cx));
            }
            _ => {}
        }
    }
}

/// Lists hosts from this computer's SSH configuration for the host picker.
struct ConfiguredHosts(Arc<dyn SshHostSource>);

impl HostDiscoveryProvider for ConfiguredHosts {
    fn discover(&self) -> HostDiscovery {
        self.0.discover()
    }
}

/// Saves a host the Add SSH Host form describes into SpaceTerm's managed SSH configuration.
struct ManagedHosts {
    hosts: Arc<dyn SshHostSource>,
    executor: BackgroundExecutor,
}

impl ManagedHostFormBackend for ManagedHosts {
    fn save(&self, host: ManagedSshHost) -> Task<Result<(), ManagedHostFormBackendError>> {
        let hosts = Arc::clone(&self.hosts);
        self.executor
            .spawn(async move { hosts.add_managed_host(host) })
    }
}

pub(crate) struct RemoteWorkspaceFlow {
    connections: SshServerConnections,
    hosts: Arc<dyn SshHostSource>,
    start: RemoteWorkspaceStart,
    host_picker: Entity<SshHostPicker>,
    focus_scope: FocusHandle,
    state: FlowState,
    /// Invalidates results that belong to an earlier step of this flow.
    action_generation: u64,
    cancelled_emitted: bool,
    connection_intent: Option<SshFlowConnectionIntent>,
    prompts: Entity<super::ssh_servers::SshPromptActivity>,
}

impl EventEmitter<RemoteWorkspaceFlowEvent> for RemoteWorkspaceFlow {}

impl RemoteWorkspaceFlow {
    pub(crate) fn new(
        connections: SshServerConnections,
        hosts: Arc<dyn SshHostSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let discovery: Arc<dyn HostDiscoveryProvider> =
            Arc::new(ConfiguredHosts(Arc::clone(&hosts)));
        let host_picker = cx.new(|cx| SshHostPicker::new(discovery, window, cx));
        cx.subscribe_in(
            &host_picker,
            window,
            |flow, _, event: &SshHostPickerEvent, window, cx| {
                flow.reduce_host_event(event, window, cx);
            },
        )
        .detach();
        let prompts = super::ssh_servers::prompt_activity(window, cx);
        cx.subscribe_in(&prompts, window, |flow, _, event, window, cx| {
            if flow.stage() != RemoteWorkspaceFlowStage::Connecting {
                return;
            }
            if matches!(
                event,
                super::ssh_servers::SshPromptEvent::Settled { cancelled: true }
            ) {
                flow.cancel_flow(window, cx);
            } else {
                flow.observe_connection(flow.action_generation, window, cx);
            }
        })
        .detach();
        Self {
            connections,
            hosts,
            start: RemoteWorkspaceStart::Home,
            host_picker,
            focus_scope: cx.focus_handle(),
            state: FlowState::Idle,
            action_generation: 0,
            cancelled_emitted: false,
            connection_intent: None,
            prompts,
        }
    }

    pub(crate) const fn stage(&self) -> RemoteWorkspaceFlowStage {
        self.state.stage()
    }

    /// Opens host selection for a Workspace whose Terminal Sessions start at `start`.
    pub(crate) fn open(
        &mut self,
        start: RemoteWorkspaceStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(
            self.stage(),
            RemoteWorkspaceFlowStage::Idle | RemoteWorkspaceFlowStage::Cancelled
        ) {
            return false;
        }
        self.start = start;
        self.cancelled_emitted = false;
        self.state = FlowState::HostSelection;
        let blocked_by_modal = spaceterm_ui::window_modal_is_open(window, cx);
        let opened = self
            .host_picker
            .update(cx, |picker, cx| picker.open(window, cx));
        if !opened && !blocked_by_modal {
            self.state = FlowState::Idle;
            return false;
        }
        self.publish(cx);
        true
    }

    #[cfg(test)]
    pub(crate) fn owns_first_responder(&self, window: &Window, cx: &App) -> bool {
        self.focus_scope.contains_focused(window, cx)
    }

    pub(crate) fn blocks_terminal_input(&self) -> bool {
        !matches!(
            self.stage(),
            RemoteWorkspaceFlowStage::Idle
                | RemoteWorkspaceFlowStage::Completed
                | RemoteWorkspaceFlowStage::Cancelled
        )
    }

    fn reduce_host_event(
        &mut self,
        event: &SshHostPickerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SshHostPickerEvent::Lifecycle(SshHostPickerLifecycleEvent::Opened) => {}
            SshHostPickerEvent::Lifecycle(SshHostPickerLifecycleEvent::Closed(_)) => {
                // Owned dialogs suspend the picker after advancing the flow. Closing it during
                // host selection ends the flow, including external replacement.
                if self.stage() == RemoteWorkspaceFlowStage::HostSelection {
                    self.cancel_flow(window, cx);
                }
            }
            SshHostPickerEvent::SelectDestination(destination)
                if self.stage() == RemoteWorkspaceFlowStage::HostSelection =>
            {
                self.connect(destination.clone(), window, cx);
            }
            SshHostPickerEvent::RequestAddHost
                if self.stage() == RemoteWorkspaceFlowStage::HostSelection =>
            {
                self.present_host_form(window, cx);
            }
            _ => {}
        }
    }

    fn present_host_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.advance();
        let backend: Arc<dyn ManagedHostFormBackend> = Arc::new(ManagedHosts {
            hosts: Arc::clone(&self.hosts),
            executor: cx.background_executor().clone(),
        });
        let form = cx.new(|cx| {
            SshHostForm::new(
                backend,
                Rc::new(crate::directory_selection::GpuiFileSelection),
                window,
                cx,
            )
        });
        cx.subscribe_in(
            &form,
            window,
            move |flow, _, event: &SshHostFormEvent, window, cx| {
                flow.reduce_form_event(generation, event, window, cx);
            },
        )
        .detach();
        if !form.update(cx, |form, cx| form.present(window, cx)) {
            return;
        }
        self.state = FlowState::AddingHost { _form: form };
        self.publish(cx);
    }

    fn reduce_form_event(
        &mut self,
        generation: u64,
        event: &SshHostFormEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.action_generation != generation
            || self.stage() != RemoteWorkspaceFlowStage::AddingHost
        {
            return;
        }
        match event {
            SshHostFormEvent::StateChanged => cx.notify(),
            SshHostFormEvent::SavedAndConnect(host) => {
                self.host_picker
                    .update(cx, |picker, cx| picker.refresh(window, cx));
                match SshDestination::new(host.alias().as_str().to_owned()) {
                    Ok(destination) => self.connect(destination, window, cx),
                    Err(_) => self.return_to_hosts(window, cx),
                }
            }
            SshHostFormEvent::Cancelled => self.return_to_hosts(window, cx),
        }
    }

    /// Connects to `destination` as Connect in the Servers Settings Section does, or reuses its
    /// connection when one is already up.
    fn connect(
        &mut self,
        destination: SshDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let generation = self.advance();
        // The person asked in this window, so a sign-in or host approval appears here.
        super::ssh_servers::present_prompts_in(window, cx);
        let status = self.connections.status(&destination);
        if !matches!(status, RemoteConnectionStatus::Connected { .. })
            && !(self.connection_intent.is_some()
                && matches!(
                    status,
                    RemoteConnectionStatus::Working { .. }
                        | RemoteConnectionStatus::Reconnecting { .. }
                        | RemoteConnectionStatus::Cancelling
                ))
        {
            self.cancel_connection_intent();
            self.connection_intent = self.connections.connect_for_flow(destination.clone());
        }
        let changes = self.connections.subscribe();
        let follow = cx.spawn_in(window, async move |flow, cx| {
            while changes.recv().await.is_ok() {
                let following = flow.update_in(cx, |flow, window, cx| {
                    flow.observe_connection(generation, window, cx)
                });
                if !matches!(following, Ok(true)) {
                    break;
                }
            }
        });
        self.state = FlowState::Connecting {
            destination,
            progress: None,
            _follow: follow,
        };
        self.observe_connection(generation, window, cx);
        self.publish(cx);
    }

    /// Follows the connection's status. Returns whether the flow still waits on it.
    fn observe_connection(
        &mut self,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.action_generation != generation {
            return false;
        }
        let FlowState::Connecting {
            destination,
            progress,
            ..
        } = &mut self.state
        else {
            return false;
        };
        let destination = destination.clone();
        let status = self.connections.status(&destination);
        match &status {
            RemoteConnectionStatus::Connected { .. } => {
                // The registry presents the server once its route is admitted.
                if self.connections.server(&destination).is_none() {
                    return true;
                }
                if let Some(progress) = progress.take() {
                    let _ = progress.dismiss(window, cx);
                }
                self.connection_intent.take();
                self.connected(destination, window, cx);
                false
            }
            RemoteConnectionStatus::Working { .. }
            | RemoteConnectionStatus::Reconnecting { .. }
            | RemoteConnectionStatus::Cancelling => {
                if self.prompts.read(cx).active {
                    if let Some(progress) = progress.take() {
                        let _ = progress.dismiss(window, cx);
                    }
                    cx.notify();
                    return true;
                }
                let presentation = present(&status);
                let update = ProgressDialogUpdate::new()
                    .status(presentation.explanation.clone())
                    .cancellation_enabled(true);
                match progress {
                    Some(handle) => {
                        let _ = handle.update(update, window, cx);
                    }
                    None => {
                        let handle = self.present_progress(
                            generation,
                            format!("Connecting to \u{201C}{}\u{201D}", destination.as_str()),
                            presentation.explanation.clone(),
                            window,
                            cx,
                        );
                        if let FlowState::Connecting { progress, .. } = &mut self.state {
                            *progress = handle;
                        }
                    }
                }
                cx.notify();
                true
            }
            RemoteConnectionStatus::InstallationOffered {
                offer_id,
                version,
                platform,
            } => {
                if let Some(progress) = progress.take() {
                    let _ = progress.dismiss(window, cx);
                }
                let detail = format!(
                    "Install SpaceTerm Server {version} for {}.",
                    platform_name(&platform.0, &platform.1),
                );
                self.review_installation(destination, *offer_id, detail, window, cx);
                false
            }
            // The connection ended without an outcome to report, as when it was cancelled
            // from Settings.
            RemoteConnectionStatus::Inactive => {
                self.cancel_flow(window, cx);
                false
            }
            RemoteConnectionStatus::NeedsPerson { .. }
            | RemoteConnectionStatus::Checked { .. }
            | RemoteConnectionStatus::Paused { .. }
            | RemoteConnectionStatus::Stopped
            | RemoteConnectionStatus::Incompatible { .. }
            | RemoteConnectionStatus::Refused { .. }
            | RemoteConnectionStatus::Unavailable { .. }
            | RemoteConnectionStatus::Unconfirmed { .. } => {
                if let Some(progress) = progress.take() {
                    let _ = progress.dismiss(window, cx);
                }
                self.present_connection_error(destination, &status, window, cx);
                false
            }
        }
    }

    fn present_progress(
        &mut self,
        generation: u64,
        title: String,
        status: gpui::SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<ProgressDialogHandle> {
        let cancelled = cx.weak_entity();
        let finished = cx.weak_entity();
        let result_window = window.window_handle();
        ProgressDialog::new(
            ModalId::new(CONNECTION_PROGRESS_ID),
            "Connection progress",
            title,
            status,
            ProgressState::Indeterminate,
            ProgressCancellation::Cancellable(ModalAction::new(
                (),
                "Cancel",
                ModalActionRole::Cancel,
                "remote-workspace-connect-cancel",
            )),
        )
        .present(
            window,
            cx,
            move |_, _, cx| {
                let _ = cancelled.update(cx, |flow, _| {
                    if flow.action_generation == generation {
                        flow.cancel_connection_intent();
                    }
                });
                ProgressCancelDecision::Allow
            },
            move |outcome, cx| {
                let _ = result_window.update(cx, |_, window, cx| {
                    let _ = finished.update(cx, |flow, cx| {
                        flow.finish_progress(generation, outcome, window, cx);
                    });
                });
            },
        )
        .ok()
    }

    fn cancel_connection_intent(&mut self) {
        if let Some(intent) = self.connection_intent.take() {
            self.connections.cancel_flow_connection(intent);
        }
    }

    fn finish_progress(
        &mut self,
        generation: u64,
        outcome: ProgressDialogOutcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.action_generation != generation {
            return;
        }
        // A suspended progress dialog may finish closing after the prompt has already
        // settled and a new progress dialog has taken its place.
        if matches!(outcome, ProgressDialogOutcome::ProgrammaticDismissal) {
            return;
        }
        if let FlowState::Connecting { progress, .. } | FlowState::Opening { progress, .. } =
            &mut self.state
        {
            progress.take();
        }
        match outcome {
            ProgressDialogOutcome::Cancelled { .. } => self.cancel_flow(window, cx),
            // The flow dismissed it itself, for a sign-in prompt or the next step.
            ProgressDialogOutcome::ProgrammaticDismissal
            | ProgressDialogOutcome::Completed
            | ProgressDialogOutcome::Failed => {}
            ProgressDialogOutcome::DeadlineExpired
            | ProgressDialogOutcome::OwnerRemoved
            | ProgressDialogOutcome::Replaced => {
                self.cancel_flow(window, cx);
            }
        }
    }

    /// Asks the person to approve the verified installation the server's computer needs, as
    /// Install in Settings does. Nothing is installed until they approve.
    fn review_installation(
        &mut self,
        destination: SshDestination,
        offer_id: u64,
        detail: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let generation = self.advance();
        let flow = cx.weak_entity();
        let window_handle = window.window_handle();
        let title = format!(
            "Install SpaceTerm on \u{201C}{}\u{201D}?",
            destination.as_str()
        );
        let alert = Alert::new(
            ModalId::new(INSTALLATION_ID),
            "Install SpaceTerm Server",
            title,
            detail,
            vec![
                ModalAction::new(
                    InstallationAction::Install,
                    "Install",
                    ModalActionRole::Affirmative,
                    "remote-workspace-install",
                )
                .default_action(true),
                ModalAction::new(
                    InstallationAction::Cancel,
                    "Cancel",
                    ModalActionRole::Cancel,
                    "remote-workspace-install-cancel",
                ),
            ],
        )
        .present(window, cx, move |outcome, cx| {
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = flow.update(cx, |flow, cx| {
                    flow.finish_installation_review(generation, offer_id, outcome, window, cx);
                });
            });
        });
        match alert {
            Ok(handle) => {
                self.state = FlowState::ReviewingInstallation {
                    destination,
                    alert: Some(handle),
                };
                self.publish(cx);
            }
            Err(_) => self.cancel_flow(window, cx),
        }
    }

    fn finish_installation_review(
        &mut self,
        generation: u64,
        offer_id: u64,
        outcome: AlertOutcome<InstallationAction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.action_generation != generation {
            return;
        }
        let FlowState::ReviewingInstallation { destination, alert } = &mut self.state else {
            return;
        };
        alert.take();
        let destination = destination.clone();
        match outcome {
            AlertOutcome::Activated {
                action_id: InstallationAction::Install,
                ..
            } => {
                self.connections.install_for_flow(&destination, offer_id);
                cx.defer_in(window, move |flow, window, cx| {
                    if flow.action_generation == generation {
                        flow.connect(destination, window, cx);
                    }
                });
            }
            // Declining withdraws this flow's intent. Other consumers keep their connection.
            _ => {
                self.cancel_flow(window, cx);
            }
        }
    }

    fn present_connection_error(
        &mut self,
        destination: SshDestination,
        status: &RemoteConnectionStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let presentation = present(status);
        let retry = presentation.actions.contains(&SshServerAction::Connect);
        let title = match self.state {
            FlowState::Opening { .. } => format!(
                "Couldn\u{2019}t Open a Workspace on \u{201C}{}\u{201D}",
                destination.as_str()
            ),
            _ => format!(
                "Couldn\u{2019}t Connect to \u{201C}{}\u{201D}",
                destination.as_str()
            ),
        };
        self.present_connection_problem(
            destination,
            ConnectionProblem {
                title,
                state: presentation.state,
                explanation: presentation.explanation,
                retry,
            },
            window,
            cx,
        );
    }

    /// Reports why the flow can't continue on `destination`, offering Try Again when Connect can
    /// help, another host, or Cancel.
    fn present_connection_problem(
        &mut self,
        destination: SshDestination,
        problem: ConnectionProblem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ConnectionProblem {
            title,
            state,
            explanation,
            retry,
        } = problem;
        let generation = self.advance();
        let flow = cx.weak_entity();
        let window_handle = window.window_handle();
        let mut actions = Vec::with_capacity(3);
        if retry {
            actions.push(
                ModalAction::new(
                    ConnectionErrorAction::Retry,
                    "Try Again",
                    ModalActionRole::Affirmative,
                    "remote-workspace-retry",
                )
                .default_action(true),
            );
        }
        actions.push(ModalAction::new(
            ConnectionErrorAction::Back,
            "Choose Another Host",
            ModalActionRole::Auxiliary,
            "remote-workspace-back-to-hosts",
        ));
        actions.push(ModalAction::new(
            ConnectionErrorAction::Cancel,
            "Cancel",
            ModalActionRole::Cancel,
            "remote-workspace-error-cancel",
        ));
        let alert = Alert::new(
            ModalId::new(CONNECTION_ERROR_ID),
            state,
            title,
            explanation,
            actions,
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, move |outcome, cx| {
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = flow.update(cx, |flow, cx| {
                    flow.finish_connection_error(generation, outcome, window, cx);
                });
            });
        });
        match alert {
            Ok(handle) => {
                self.state = FlowState::ConnectionError {
                    destination,
                    alert: Some(handle),
                };
                self.publish(cx);
            }
            Err(_) => self.cancel_flow(window, cx),
        }
    }

    fn finish_connection_error(
        &mut self,
        generation: u64,
        outcome: AlertOutcome<ConnectionErrorAction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.action_generation != generation {
            return;
        }
        let FlowState::ConnectionError { destination, alert } = &mut self.state else {
            return;
        };
        alert.take();
        let destination = destination.clone();
        let action = match outcome {
            AlertOutcome::Activated { action_id, .. } => action_id,
            AlertOutcome::Dismissed { .. } => ConnectionErrorAction::Cancel,
        };
        match action {
            ConnectionErrorAction::Retry => {
                cx.defer_in(window, move |flow, window, cx| {
                    if flow.action_generation == generation {
                        flow.connect(destination, window, cx);
                    }
                });
            }
            ConnectionErrorAction::Back => {
                // The alert restores focus as it closes; host selection reopens after it.
                cx.defer_in(window, move |flow, window, cx| {
                    if flow.action_generation == generation {
                        flow.return_to_hosts(window, cx);
                    }
                });
            }
            ConnectionErrorAction::Cancel => self.cancel_flow(window, cx),
        }
    }

    /// The server is presented. A Workspace at home completes now; one in a chosen directory
    /// browses the server's own filesystem first.
    fn connected(
        &mut self,
        destination: SshDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.advance();
        match self.start {
            RemoteWorkspaceStart::Home => self.open_on_server(destination, None, window, cx),
            RemoteWorkspaceStart::ChosenDirectory => {
                self.choose_directory(destination, window, cx);
            }
        }
    }

    fn choose_directory(
        &mut self,
        destination: SshDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = self.connections.server(&destination) else {
            self.cancel_flow(window, cx);
            return;
        };
        // Reads stop the moment this connection's server is no longer the presented one.
        let connections = self.connections.clone();
        let reached = destination.clone();
        let expected = server.clone();
        let available: Rc<dyn Fn() -> bool> = Rc::new(move || {
            connections
                .server(&reached)
                .is_some_and(|presented| presented.same_handle(&expected))
        });
        let source = Rc::new(ServerDirectorySource::browsing(
            server,
            Some(destination.host().to_owned()),
            available,
        ));
        let picker = cx.new(|cx| {
            DirectoryPicker::new(
                source,
                crate::keybindings::Command::OpenRemoteDirectory.label(),
                window,
                cx,
            )
        });
        cx.subscribe_in(
            &picker,
            window,
            move |flow, picker, event: &DirectoryPickerEvent, window, cx| {
                flow.reduce_directory_picker_event(&destination, picker, event, window, cx);
            },
        )
        .detach();
        if !picker.update(cx, |picker, cx| picker.open(window, cx)) {
            self.cancel_flow(window, cx);
            return;
        }
        self.state = FlowState::ChoosingDirectory { picker };
        self.publish(cx);
    }

    fn reduce_directory_picker_event(
        &mut self,
        destination: &SshDestination,
        picker: &Entity<DirectoryPicker>,
        event: &DirectoryPickerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(&self.state, FlowState::ChoosingDirectory { picker: current }
            if current == picker)
        {
            return;
        }
        match event {
            DirectoryPickerEvent::Confirmed(DirectorySelection::Server(directory)) => {
                // The Workspace that opens takes the keyboard; the picker must not restore its
                // own focus.
                picker.update(cx, |picker, cx| picker.complete_activation(window, cx));
                self.open_on_server(destination.clone(), Some(directory.clone()), window, cx);
            }
            // A server's picker never pins a directory of this computer.
            DirectoryPickerEvent::Confirmed(DirectorySelection::Pin(_)) => {
                picker.update(cx, |picker, cx| picker.activation_failed(window, cx));
            }
            DirectoryPickerEvent::Dismissed => self.cancel_flow(window, cx),
            // A server's Directory Picker offers no System Directory Selection.
            DirectoryPickerEvent::SystemSelectionRequested => {}
            DirectoryPickerEvent::StateChanged => cx.notify(),
        }
    }

    /// Asks for the Workspace on the connected server and shows that it is opening until the
    /// server's edits take it. The request is fenced to this exact connection: if it ends or is
    /// replaced first, nothing is created and the person can try again.
    fn open_on_server(
        &mut self,
        destination: SshDestination,
        directory: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = self.connections.server(&destination) else {
            self.cancel_flow(window, cx);
            return;
        };
        let generation = self.advance();
        let choice = RemoteWorkspaceChoice {
            destination: destination.clone(),
            directory,
            server,
        };
        let progress = self.present_progress(
            generation,
            "Opening Workspace".to_owned(),
            "Opening the Workspace…".into(),
            window,
            cx,
        );
        let changes = self.connections.subscribe();
        let follow = cx.spawn_in(window, async move |flow, cx| {
            while changes.recv().await.is_ok() {
                let following = flow.update_in(cx, |flow, window, cx| {
                    flow.observe_opening(generation, window, cx)
                });
                if !matches!(following, Ok(true)) {
                    break;
                }
            }
        });
        self.state = FlowState::Opening {
            choice: Box::new(choice),
            progress,
            _follow: follow,
        };
        cx.emit(RemoteWorkspaceFlowEvent::Ready);
        self.publish(cx);
    }

    /// Follows the connection the opening Workspace waits on. Returns whether it still waits.
    fn observe_opening(
        &mut self,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.action_generation != generation {
            return false;
        }
        let FlowState::Opening { choice, .. } = &self.state else {
            return false;
        };
        let destination = choice.destination.clone();
        let same_connection = self
            .connections
            .server(&destination)
            .is_some_and(|presented| presented.same_handle(&choice.server));
        let status = self.connections.status(&destination);
        if same_connection && matches!(status, RemoteConnectionStatus::Connected { .. }) {
            return true;
        }
        if let FlowState::Opening { progress, .. } = &mut self.state
            && let Some(progress) = progress.take()
        {
            let _ = progress.dismiss(window, cx);
        }
        if matches!(status, RemoteConnectionStatus::Connected { .. }) {
            // A newer connection to the destination replaced the one the request was for.
            let problem = ConnectionProblem {
                title: "Couldn't Open Workspace".to_owned(),
                state: "Connection replaced",
                explanation: "The connection changed before the Workspace opened. Try again."
                    .into(),
                retry: true,
            };
            self.present_connection_problem(destination, problem, window, cx);
        } else {
            self.present_connection_error(destination, &status, window, cx);
        }
        false
    }

    /// Continues as confirming `directory` in the server's Directory Picker does, for a test
    /// without a server filesystem to browse.
    #[cfg(test)]
    pub(crate) fn choose_server_directory(
        &mut self,
        destination: SshDestination,
        directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_on_server(destination, Some(directory), window, cx);
        // A person chose the host before the directory, which closed host selection.
        self.host_picker
            .update(cx, |picker, cx| picker.dismiss(window, cx));
    }

    /// The opening Workspace's request, while the flow waits for the server to take it.
    pub(crate) fn opening(&self) -> Option<&RemoteWorkspaceChoice> {
        match &self.state {
            FlowState::Opening { choice, .. } => Some(choice.as_ref()),
            _ => None,
        }
    }

    /// Ends the flow once the server took its request, or the window presented the Workspace
    /// it asked for.
    pub(crate) fn opened(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.state, FlowState::Opening { .. }) {
            return;
        }
        self.advance();
        let mut previous = std::mem::replace(&mut self.state, FlowState::Completed);
        previous.dismiss(window, cx);
        self.publish(cx);
    }

    fn cancel_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.state, FlowState::Cancelled | FlowState::Completed) {
            return;
        }
        self.advance();
        self.cancel_connection_intent();
        let mut previous = std::mem::replace(&mut self.state, FlowState::Cancelled);
        previous.dismiss(window, cx);
        self.host_picker
            .update(cx, |picker, cx| picker.dismiss(window, cx));
        drop(previous);
        if !self.cancelled_emitted {
            self.cancelled_emitted = true;
            cx.emit(RemoteWorkspaceFlowEvent::Cancelled);
        }
        self.publish(cx);
    }

    fn return_to_hosts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.advance();
        self.cancel_connection_intent();
        let mut previous = std::mem::replace(&mut self.state, FlowState::HostSelection);
        previous.dismiss(window, cx);
        if !self
            .host_picker
            .update(cx, |picker, cx| picker.open(window, cx))
            && !spaceterm_ui::window_modal_is_open(window, cx)
        {
            self.cancel_flow(window, cx);
            return;
        }
        self.publish(cx);
    }

    /// Starts a new step, so results that belong to the previous one are ignored.
    fn advance(&mut self) -> u64 {
        self.action_generation = self.action_generation.wrapping_add(1);
        self.action_generation
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        cx.emit(RemoteWorkspaceFlowEvent::StateChanged);
        cx.notify();
    }
}

impl Render for RemoteWorkspaceFlow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let directory_picker = match &self.state {
            FlowState::ChoosingDirectory { picker } => Some(picker.clone()),
            _ => None,
        };
        div()
            .size_full()
            .track_focus(&self.focus_scope)
            .child(self.host_picker.clone())
            .children(directory_picker)
    }
}

impl Drop for RemoteWorkspaceFlow {
    fn drop(&mut self) {
        self.cancel_connection_intent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::cancellation::SshCancellationToken;
    use crate::ssh::server_connection::*;
    use gpui::{TestAppContext, VisualTestContext};
    use spaceterm_ui::ModalLayer;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    struct Hosts;
    impl SshHostSource for Hosts {
        fn discover(&self) -> HostDiscovery {
            HostDiscovery::default()
        }
        fn add_managed_host(&self, _: ManagedSshHost) -> Result<(), ManagedHostFormBackendError> {
            Ok(())
        }
    }
    struct Intents;
    impl SshConnectionIntentStore for Intents {
        fn load(&self) -> Result<Vec<RetainedSshConnection>, SshIntentStoreError> {
            Ok(Vec::new())
        }
        fn retain(&self, _: RetainedSshConnection) -> Result<(), SshIntentStoreError> {
            Ok(())
        }
        fn remove(&self, _: &SshDestination) -> Result<(), SshIntentStoreError> {
            Ok(())
        }
    }
    struct Harness(Entity<RemoteWorkspaceFlow>);
    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(div().size_full().child(self.0.clone()))
        }
    }
    fn window(
        registry: SshServerConnections,
        cx: &mut TestAppContext,
    ) -> (Entity<RemoteWorkspaceFlow>, &mut VisualTestContext) {
        // Connection and AskPass workers notify from their real process-boundary threads.
        cx.executor().allow_parking();
        cx.update(crate::ui::init).unwrap();
        let (harness, cx) = cx.add_window_view(|window, cx| {
            Harness(cx.new(|cx| RemoteWorkspaceFlow::new(registry, Arc::new(Hosts), window, cx)))
        });
        let flow = harness.read_with(cx, |harness, _| harness.0.clone());
        cx.update(|window, _| window.activate_window());
        (flow, cx)
    }
    fn redraw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }
    fn until(
        mut condition: impl FnMut(&mut VisualTestContext) -> bool,
        cx: &mut VisualTestContext,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition(cx) {
            assert!(Instant::now() < deadline, "production flow did not settle");
            std::thread::sleep(Duration::from_millis(5));
            redraw(cx);
        }
    }
    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let position = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"))
            .center();
        cx.simulate_mouse_move(position, None, gpui::Modifiers::none());
        cx.simulate_click(position, gpui::Modifiers::none());
        cx.run_until_parked();
    }
    fn destination() -> SshDestination {
        SshDestination::new("fixture-host".into()).unwrap()
    }
    fn connect(flow: &Entity<RemoteWorkspaceFlow>, cx: &mut VisualTestContext) {
        cx.update(|window, cx| flow.update(cx, |flow, cx| flow.connect(destination(), window, cx)));
        redraw(cx);
    }

    struct Lease(Arc<AtomicBool>);
    impl SshServerLease for Lease {
        fn is_alive(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }
    struct SlowNetwork {
        calls: AtomicUsize,
        disconnects: AtomicUsize,
        initially_connected: bool,
        alive: Arc<AtomicBool>,
        release: async_channel::Receiver<()>,
    }
    impl SshConnectBackend for SlowNetwork {
        fn disconnect(&self, _: &SshDestination) {
            self.disconnects.fetch_add(1, Ordering::SeqCst);
        }
        fn connect(
            &self,
            _: &SshDestination,
            mode: ConnectMode,
            _: Option<&SshServerContinuity>,
            _: &SshCancellationToken,
            progress: &ConnectProgress<'_>,
        ) -> Result<ConnectedSshServer, SshConnectFailure> {
            let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
            if first && self.initially_connected {
                return Ok(ConnectedSshServer {
                    server: spaceterm_server::LocalServer::for_identity("spaceterm-development")
                        .unwrap(),
                    continuity: SshServerContinuity {
                        route: [1; 32],
                        account: 501,
                        build: "fixture".into(),
                        server: [2; 32],
                    },
                    lease: Arc::new(Lease(self.alive.clone())),
                    facts: SshServerFacts {
                        platform: ("linux".into(), "x86_64".into()),
                        version: None,
                        starts_automatically: true,
                    },
                });
            }
            assert_eq!(
                mode,
                if self.initially_connected {
                    ConnectMode::QuietContinuation
                } else {
                    ConnectMode::Requested
                }
            );
            progress(ConnectPhase::Authenticating);
            // Deliberately ignores cancellation until the network returns. UI cancellation must
            // release input without waiting for this boundary.
            let _ = self.release.recv_blocking();
            Err(SshConnectFailure::Unavailable(UnavailableKind::Network))
        }
        fn observe(
            &self,
            _: &SshDestination,
            _: Option<&SshServerContinuity>,
            _: &SshCancellationToken,
            _: &ConnectProgress<'_>,
        ) -> Result<SshServerObservation, SshConnectFailure> {
            unreachable!()
        }
    }
    fn network(
        initially_connected: bool,
    ) -> (
        SshServerConnections,
        Arc<SlowNetwork>,
        async_channel::Sender<()>,
    ) {
        let (release, released) = async_channel::unbounded();
        let backend = Arc::new(SlowNetwork {
            calls: AtomicUsize::new(0),
            disconnects: AtomicUsize::new(0),
            initially_connected,
            alive: Arc::new(AtomicBool::new(true)),
            release: released,
        });
        (
            SshServerConnections::new(backend.clone(), Arc::new(Intents)),
            backend,
            release,
        )
    }

    #[gpui::test]
    fn slow_authentication_without_a_prompt_keeps_progress_and_escape_releases_input(
        cx: &mut TestAppContext,
    ) {
        let (registry, backend, release) = network(false);
        let (flow, cx) = window(registry.clone(), cx);
        connect(&flow, cx);
        until(|_| backend.calls.load(Ordering::SeqCst) == 1, cx);
        redraw(cx);
        assert!(
            cx.debug_bounds("modal-action-remote-workspace-connect-cancel")
                .is_some()
        );
        assert!(flow.read_with(cx, |flow, _| flow.blocks_terminal_input()));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!flow.read_with(cx, |flow, _| flow.blocks_terminal_input()));
        assert!(!registry.intends(&destination()));
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        release.send_blocking(()).unwrap();
    }

    #[gpui::test]
    fn joining_ordinary_network_reconnect_allows_cancel_without_disconnecting_its_consumers(
        cx: &mut TestAppContext,
    ) {
        let (registry, backend, release) = network(true);
        let (flow, cx) = window(registry.clone(), cx);
        registry.connect(destination());
        until(|_| registry.server(&destination()).is_some(), cx);
        backend.alive.store(false, Ordering::SeqCst);
        until(|_| backend.calls.load(Ordering::SeqCst) == 2, cx);
        assert!(matches!(
            registry.status(&destination()),
            RemoteConnectionStatus::Reconnecting { .. }
        ));

        for escape in [false, true] {
            connect(&flow, cx);
            assert!(
                cx.debug_bounds("modal-action-remote-workspace-connect-cancel")
                    .is_some()
            );
            if escape {
                cx.simulate_keystrokes("escape");
                cx.run_until_parked();
            } else {
                click("modal-action-remote-workspace-connect-cancel", cx);
            }
            assert!(!flow.read_with(cx, |flow, _| flow.blocks_terminal_input()));
            assert!(registry.intends(&destination()));
            assert_eq!(backend.disconnects.load(Ordering::SeqCst), 0);
            assert_eq!(backend.calls.load(Ordering::SeqCst), 2);
            assert!(matches!(
                registry.status(&destination()),
                RemoteConnectionStatus::Reconnecting { .. }
            ));
        }
        registry.disconnect(&destination());
        release.send_blocking(()).unwrap();
    }

    #[gpui::test]
    fn native_installer_authorization_takes_the_progress_slot_and_can_cancel_then_retry(
        cx: &mut TestAppContext,
    ) {
        // The initializer needs a window for the actual bridge before choosing the backend.
        let (unused, _, _) = network(false);
        let (flow, cx) = window(unused, cx);
        let presenter = cx.update(|window, cx| {
            crate::ssh::askpass::GpuiAskPassBridge::new(window, cx).presenter()
        });
        let (backend, account, complete) =
            crate::ssh::server_process::testing::authorization_installation_backend(
                cx.background_executor.clone(),
                presenter,
            );
        let registry = SshServerConnections::new(backend, Arc::new(Intents));
        flow.update(cx, |flow, _| flow.connections = registry.clone());

        for cancel in [true, false] {
            connect(&flow, cx);
            until(
                |cx| {
                    cx.debug_bounds("modal-action-remote-workspace-install")
                        .is_some()
                },
                cx,
            );
            click("modal-action-remote-workspace-install", cx);
            until(
                |cx| cx.debug_bounds("modal-action-ssh-askpass-submit").is_some(),
                cx,
            );
            assert!(
                cx.debug_bounds("modal-action-remote-workspace-connect-cancel")
                    .is_none()
            );
            assert!(flow.read_with(cx, |flow, _| flow.blocks_terminal_input()));
            assert_eq!(account.ran("ssh-install"), if cancel { 1 } else { 2 });
            if cancel {
                cx.simulate_keystrokes("escape");
                cx.run_until_parked();
                assert!(!flow.read_with(cx, |flow, _| flow.blocks_terminal_input()));
                until(
                    |_| {
                        matches!(
                            registry.status(&destination()),
                            RemoteConnectionStatus::Inactive
                        )
                    },
                    cx,
                );
            } else {
                cx.simulate_input("fixture-password");
                cx.simulate_keystrokes("enter");
                cx.run_until_parked();
                until(
                    |cx| {
                        cx.debug_bounds("modal-action-remote-workspace-connect-cancel")
                            .is_some()
                    },
                    cx,
                );
                assert!(flow.read_with(cx, |flow, _| flow.blocks_terminal_input()));
                complete.send_blocking(()).unwrap();
                until(|_| account.ran("ssh-install") == 3, cx);
                until(
                    |_| {
                        matches!(
                            registry.status(&destination()),
                            RemoteConnectionStatus::Stopped
                        )
                    },
                    cx,
                );
                until(
                    |cx| {
                        flow.read_with(cx, |flow, _| flow.stage())
                            == RemoteWorkspaceFlowStage::ConnectionError
                    },
                    cx,
                );
                assert_eq!(
                    account.sent("ssh-install"),
                    [
                        b"verified-archive".to_vec(),
                        b"verified-archive".to_vec(),
                        b"fixture-password\nverified-archive".to_vec(),
                    ]
                );
            }
        }
        flow.update(cx, |flow, cx| {
            // The completed installer reports its observed stopped server through the flow.
            assert_eq!(flow.stage(), RemoteWorkspaceFlowStage::ConnectionError);
            cx.notify();
        });
    }
}
