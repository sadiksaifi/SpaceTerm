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
use super::ssh_servers::{
    SshHostSource, SshServerAction, installation_consequence, platform_name, present,
};
use crate::domain::SshDestination;
use crate::ssh::host_config::HostDiscovery;
use crate::ssh::managed_hosts::ManagedSshHost;
use crate::ssh::server_connection::{ConnectPhase, RemoteConnectionStatus, SshServerConnections};

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

/// The server and directory a completed flow chose. The directory is a path on the server's
/// computer, which grants no authority on this one.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct RemoteWorkspaceChoice {
    pub(crate) destination: SshDestination,
    pub(crate) directory: Option<PathBuf>,
}

impl std::fmt::Debug for RemoteWorkspaceChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RemoteWorkspaceChoice(<redacted>)")
    }
}

#[derive(Clone, Debug)]
pub(crate) enum RemoteWorkspaceFlowEvent {
    StateChanged,
    /// The chosen server is connected; the Workspace belongs on it.
    Completed(RemoteWorkspaceChoice),
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
    Completed,
    Cancelled,
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
            Self::Completed => RemoteWorkspaceFlowStage::Completed,
            Self::Cancelled => RemoteWorkspaceFlowStage::Cancelled,
        }
    }

    /// Closes whatever this state presents. Cancelling a connection is the caller's decision.
    fn dismiss(&mut self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Connecting { progress, .. } => {
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
        Self {
            connections,
            hosts,
            start: RemoteWorkspaceStart::Home,
            host_picker,
            focus_scope: cx.focus_handle(),
            state: FlowState::Idle,
            action_generation: 0,
            cancelled_emitted: false,
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
        if !matches!(
            self.connections.status(&destination),
            RemoteConnectionStatus::Connected { .. }
        ) {
            self.connections.connect(destination.clone());
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
                self.connected(destination, window, cx);
                false
            }
            RemoteConnectionStatus::Working {
                phase: ConnectPhase::Authenticating,
                ..
            } => {
                // SSH's own prompt appears now, in this window. It replaces the progress until
                // signing in ends, so two dialogs never compete for the keyboard.
                if let Some(progress) = progress.take() {
                    let _ = progress.dismiss(window, cx);
                }
                cx.notify();
                true
            }
            RemoteConnectionStatus::Working { .. }
            | RemoteConnectionStatus::Reconnecting { .. }
            | RemoteConnectionStatus::Cancelling => {
                let presentation = present(&status);
                let update = ProgressDialogUpdate::new()
                    .status(presentation.explanation.clone())
                    .cancellation_enabled(presentation.actions.contains(&SshServerAction::Cancel));
                match progress {
                    Some(handle) => {
                        let _ = handle.update(update, window, cx);
                    }
                    None => {
                        let handle = self.present_progress(
                            generation,
                            &destination,
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
                    "This account has no SpaceTerm server. SpaceTerm can install verified version \
                     {version} for {}. {}",
                    platform_name(&platform.0, &platform.1),
                    installation_consequence(&platform.0),
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
        destination: &SshDestination,
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
            format!("Connecting to \u{201C}{}\u{201D}", destination.as_str()),
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
                let _ = cancelled.update(cx, |flow, _| flow.cancel_connection(generation));
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

    /// Stops the connection attempt this flow started. Work the server already accepted may
    /// still take effect, as Settings says while it cancels.
    fn cancel_connection(&mut self, generation: u64) {
        if self.action_generation != generation {
            return;
        }
        if let FlowState::Connecting { destination, .. } = &self.state {
            self.connections.cancel(destination);
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
        if let FlowState::Connecting { progress, .. } = &mut self.state {
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
                self.cancel_connection(generation);
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
            "Install the SpaceTerm server",
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
                self.connections.install(&destination, offer_id);
                cx.defer_in(window, move |flow, window, cx| {
                    if flow.action_generation == generation {
                        flow.connect(destination, window, cx);
                    }
                });
            }
            // Declining keeps the connection saved and ends the intent to keep it up, so
            // nothing waits on an installation the person turned down.
            _ => {
                self.connections.disconnect(&destination);
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
        let generation = self.advance();
        let presentation = present(status);
        let flow = cx.weak_entity();
        let window_handle = window.window_handle();
        let mut actions = Vec::with_capacity(3);
        if presentation.actions.contains(&SshServerAction::Connect) {
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
        let title = format!(
            "Couldn\u{2019}t Connect to \u{201C}{}\u{201D}",
            destination.as_str()
        );
        let alert = Alert::new(
            ModalId::new(CONNECTION_ERROR_ID),
            presentation.state,
            title,
            presentation.explanation,
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
            RemoteWorkspaceStart::Home => self.complete(destination, None, cx),
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
            cx.background_executor().clone(),
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
                self.complete(destination.clone(), Some(directory.clone()), cx);
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

    fn complete(
        &mut self,
        destination: SshDestination,
        directory: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.advance();
        self.state = FlowState::Completed;
        cx.emit(RemoteWorkspaceFlowEvent::Completed(RemoteWorkspaceChoice {
            destination,
            directory,
        }));
        self.publish(cx);
    }

    fn cancel_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.state, FlowState::Cancelled | FlowState::Completed) {
            return;
        }
        self.advance();
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
