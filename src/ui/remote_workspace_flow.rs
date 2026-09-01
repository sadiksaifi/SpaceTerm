#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the Remote Workspace Flow lands before its Workspace Manager integration"
    )
)]

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui::prelude::*;
use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Render, Task, Window, div};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, CommandPaletteReplacementFocus, ModalAction,
    ModalActionIntent, ModalActionRole, ModalId, ModalPresentationHandle, ProgressCancelDecision,
    ProgressCancellation, ProgressDialog, ProgressDialogHandle, ProgressDialogOutcome,
    ProgressDialogUpdate, ProgressState,
};
use thiserror::Error;

use super::remote_workspace_picker::{
    RemoteWorkspaceAccount, RemoteWorkspacePicker, RemoteWorkspacePickerEvent,
    RemoteWorkspaceProvider, RemoteWorkspaceSelection,
};
use super::ssh_host_form::{
    ManagedHostFormBackend, ManagedHostFormBackendError, SshHostForm, SshHostFormEvent,
    SshHostFormMode,
};
use super::ssh_host_picker::{
    HostDiscoveryProvider, SshHostPicker, SshHostPickerEvent, SshHostPickerLifecycleEvent,
};
use crate::domain::{RemoteDirectoryIdentity, RemoteWorkspaceDirectory, SshDestination};
use crate::ssh::command::ValidatedRemoteLoginShell;
use crate::ssh::destination::SshHostAlias;
use crate::ssh::host_config::HostDiscovery;
use crate::ssh::live_connection::ControlConnectionObserver;
use crate::ssh::managed_hosts::ManagedSshHost;
use crate::ssh::process::TransientSshErrorOutput;
use crate::terminal::RemoteTerminalChannelProvider;

const CONNECTION_PROGRESS_ID: &str = "remote-workspace-connection-progress";
const CONNECTION_ERROR_ID: &str = "remote-workspace-connection-error";
const OPENSSH_ERROR_DETAIL_HEADING: &str = "OpenSSH reported:";
const DELETE_CONFIRMATION_ID: &str = "remote-workspace-delete-host";
const DELETE_ERROR_ID: &str = "remote-workspace-delete-error";

/// Content-free progress phases reported by the native SSH connector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RemoteWorkspaceConnectionProgress {
    CheckingCompatibility,
    Connecting,
    Authenticating,
}

impl RemoteWorkspaceConnectionProgress {
    pub(super) const fn status(self) -> &'static str {
        match self {
            Self::CheckingCompatibility => "Checking remote compatibility",
            Self::Connecting => "Connecting securely",
            Self::Authenticating => "Waiting for authentication",
        }
    }
}

/// Cloneable, bounded progress and cancellation authority passed to one connect attempt.
#[derive(Clone)]
pub(super) struct RemoteWorkspaceConnectContext {
    progress: async_channel::Sender<RemoteWorkspaceConnectionProgress>,
    cancelled: Arc<AtomicBool>,
}

impl RemoteWorkspaceConnectContext {
    pub(super) fn new(
        progress: async_channel::Sender<RemoteWorkspaceConnectionProgress>,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            progress,
            cancelled,
        }
    }

    pub(super) fn report(&self, progress: RemoteWorkspaceConnectionProgress) {
        let _ = self.progress.try_send(progress);
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Eq, Error, PartialEq)]
/// Actionable flow failure with authentication material and raw remote output excluded.
///
/// Connection detail, when present, is already control-free and bounded to the transient alert
/// lifetime. Its `Debug` representation remains redacted.
pub(super) enum RemoteWorkspaceFlowBackendError {
    #[error("the managed SSH host could not be deleted")]
    DeleteFailed,
    #[error("the SSH host is already in use")]
    HostInUse,
    #[error("the remote server is incompatible")]
    IncompatibleServer,
    #[error("the required OpenSSH version is unavailable")]
    OpenSshUnavailable,
    #[error("SSH authentication was cancelled")]
    AuthenticationCancelled,
    #[error("the SSH connection failed")]
    ConnectionFailed,
    #[error("the SSH connection failed")]
    ConnectionFailedWithDetail(TransientSshErrorOutput),
}

impl RemoteWorkspaceFlowBackendError {
    /// Borrows the sanitized connection tail intended only for the active failure alert.
    pub(super) fn connection_detail(&self) -> Option<&str> {
        match self {
            Self::ConnectionFailedWithDetail(detail) => Some(detail.as_str()),
            _ => None,
        }
    }

    /// Transfers the bounded diagnostic without converting it into an inspectable string.
    pub(super) fn into_connection_detail(self) -> Option<TransientSshErrorOutput> {
        match self {
            Self::ConnectionFailedWithDetail(detail) => Some(detail),
            _ => None,
        }
    }
}

impl fmt::Debug for RemoteWorkspaceFlowBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::DeleteFailed => "DeleteFailed",
            Self::HostInUse => "HostInUse",
            Self::IncompatibleServer => "IncompatibleServer",
            Self::OpenSshUnavailable => "OpenSshUnavailable",
            Self::AuthenticationCancelled => "AuthenticationCancelled",
            Self::ConnectionFailed => "ConnectionFailed",
            Self::ConnectionFailedWithDetail(_) => "ConnectionFailedWithDetail(<redacted>)",
        })
    }
}

struct ConnectionErrorContent {
    message: &'static str,
    detail: Option<String>,
}

fn connection_error_content(
    error: Option<&RemoteWorkspaceFlowBackendError>,
) -> ConnectionErrorContent {
    let message = match error {
        Some(RemoteWorkspaceFlowBackendError::OpenSshUnavailable) => {
            "SpaceTerm requires OpenSSH 8.2 or newer at /usr/bin/ssh."
        }
        Some(RemoteWorkspaceFlowBackendError::IncompatibleServer) => {
            "This host does not provide the remote capabilities SpaceTerm requires."
        }
        _ => "SpaceTerm couldn\u{2019}t establish the remote connection.",
    };
    let detail = error.and_then(|error| {
        error
            .connection_detail()
            .map(|detail| format!("{OPENSSH_ERROR_DETAIL_HEADING}\n{detail}"))
    });
    ConnectionErrorContent { message, detail }
}

/// Opaque Workspace-lifetime authority that keeps one configured SSH alias immutable.
///
/// This value is intentionally non-Clone and non-Debug. Dropping it releases exactly its own
/// registry count without affecting the connected session's independent alias lease.
pub(super) struct RemoteWorkspaceAliasPin {
    _owner: Box<dyn Send>,
}

impl RemoteWorkspaceAliasPin {
    pub(super) fn new(owner: impl Send + 'static) -> Self {
        Self {
            _owner: Box::new(owner),
        }
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("the configured SSH alias could not be pinned for Workspace ownership")]
pub(super) struct RemoteWorkspaceAliasPinError;

/// The opaque lifetime owner for one connected SSH control path.
///
/// This trait deliberately exposes no command or transport access to UI code. Implementations are
/// non-clone owners and must make `close` idempotent and non-blocking for the calling GPUI thread.
/// Retained background ownership remains responsible for bounded exact process, socket,
/// authentication, cancellation, and per-session alias cleanup after `close` returns.
pub(super) trait RemoteWorkspaceSessionOwner: Send + 'static {
    /// Acquires an independent Workspace-lifetime alias count without consuming session ownership.
    fn acquire_workspace_alias_pin(
        &self,
    ) -> Result<Option<RemoteWorkspaceAliasPin>, RemoteWorkspaceAliasPinError>;

    /// Binds a fallible channel provider to the visible directory and its physical identity.
    fn bind_terminal_channels_for_identity(
        &self,
        directory: &RemoteWorkspaceDirectory,
        expected_identity: &RemoteDirectoryIdentity,
        login_shell: &ValidatedRemoteLoginShell,
    ) -> Result<Arc<dyn RemoteTerminalChannelProvider>, RemoteWorkspaceFlowBackendError>;

    /// Transfers the content-free observer paired with this exact session at most once.
    fn take_lifecycle_observer(&mut self) -> Option<ControlConnectionObserver>;

    /// Cancels and cleans up all session-owned resources exactly once.
    fn close(&mut self);
}

/// A live connected session and its narrow directory-provider capability.
///
/// This value is intentionally non-Clone. Dropping it closes the session exactly once.
pub(super) struct RemoteWorkspaceConnectedSession {
    owner: Option<Box<dyn RemoteWorkspaceSessionOwner>>,
    provider: Arc<dyn RemoteWorkspaceProvider + Send + Sync>,
}

impl RemoteWorkspaceConnectedSession {
    /// Creates a connected session from its singular owner and narrow utility provider.
    pub(super) fn new(
        owner: Box<dyn RemoteWorkspaceSessionOwner>,
        provider: Arc<dyn RemoteWorkspaceProvider + Send + Sync>,
    ) -> Self {
        Self {
            owner: Some(owner),
            provider,
        }
    }

    pub(super) fn provider(&self) -> Arc<dyn RemoteWorkspaceProvider + Send + Sync> {
        Arc::clone(&self.provider)
    }

    /// Creates a provider that requires physical revalidation before every child reservation.
    pub(super) fn bind_terminal_channels_for_identity(
        &self,
        directory: &RemoteWorkspaceDirectory,
        expected_identity: &RemoteDirectoryIdentity,
        login_shell: &ValidatedRemoteLoginShell,
    ) -> Result<Arc<dyn RemoteTerminalChannelProvider>, RemoteWorkspaceFlowBackendError> {
        self.owner
            .as_ref()
            .ok_or(RemoteWorkspaceFlowBackendError::ConnectionFailed)?
            .bind_terminal_channels_for_identity(directory, expected_identity, login_shell)
    }

    /// Transfers the session-paired lifecycle observer at most once.
    pub(super) fn take_lifecycle_observer(&mut self) -> Option<ControlConnectionObserver> {
        self.owner.as_mut()?.take_lifecycle_observer()
    }

    fn acquire_workspace_alias_pin(
        &self,
    ) -> Result<Option<RemoteWorkspaceAliasPin>, RemoteWorkspaceAliasPinError> {
        self.owner
            .as_ref()
            .ok_or(RemoteWorkspaceAliasPinError)?
            .acquire_workspace_alias_pin()
    }
}

impl Drop for RemoteWorkspaceConnectedSession {
    fn drop(&mut self) {
        if let Some(mut owner) = self.owner.take() {
            owner.close();
        }
    }
}

/// All side effects required by the standalone remote-workspace creation flow.
pub(super) trait RemoteWorkspaceFlowBackend: Send + Sync {
    /// Performs fresh bounded host discovery and preserves partial-scan diagnostics.
    fn discover_hosts(&self) -> HostDiscovery;

    fn host_in_active_use(&self, alias: &SshHostAlias) -> bool;

    fn managed_host(&self, alias: &SshHostAlias) -> Option<ManagedSshHost>;

    fn save_managed_host(
        &self,
        host: ManagedSshHost,
        editing_alias: Option<SshHostAlias>,
    ) -> Task<Result<(), ManagedHostFormBackendError>>;

    fn delete_managed_host(
        &self,
        alias: SshHostAlias,
    ) -> Task<Result<(), RemoteWorkspaceFlowBackendError>>;

    /// Connects with attempt-scoped progress and cancellation, returning singular ownership.
    fn connect(
        &self,
        destination: SshDestination,
        context: RemoteWorkspaceConnectContext,
    ) -> Task<Result<RemoteWorkspaceConnectedSession, RemoteWorkspaceFlowBackendError>>;
}

/// Builds the window-bound backend once while its Workspace Manager is initialized.
pub(super) trait RemoteWorkspaceFlowBackendFactory: Send + Sync {
    /// Returns a content-free startup gate reason before AskPass or connection work begins.
    fn unavailable_reason(&self) -> Option<String> {
        None
    }

    /// Creates a backend while the window is available without retaining it in background work.
    fn create(
        &self,
        window: &Window,
        cx: &mut App,
    ) -> Result<Arc<dyn RemoteWorkspaceFlowBackend>, RemoteWorkspaceFlowBackendError>;
}

struct FlowHostDiscoveryProvider {
    backend: Arc<dyn RemoteWorkspaceFlowBackend>,
}

impl HostDiscoveryProvider for FlowHostDiscoveryProvider {
    fn discover(&self) -> HostDiscovery {
        self.backend.discover_hosts()
    }
}

struct FlowManagedHostBackend {
    backend: Arc<dyn RemoteWorkspaceFlowBackend>,
}

impl ManagedHostFormBackend for FlowManagedHostBackend {
    fn save(
        &self,
        host: ManagedSshHost,
        editing_alias: Option<SshHostAlias>,
    ) -> Task<Result<(), ManagedHostFormBackendError>> {
        if editing_alias
            .as_ref()
            .is_some_and(|alias| self.backend.host_in_active_use(alias))
        {
            return Task::ready(Err(ManagedHostFormBackendError::HostInUse));
        }
        self.backend.save_managed_host(host, editing_alias)
    }
}

/// A completed remote workspace creation. Its connected session is live and non-Clone.
pub(super) struct RemoteWorkspaceFlowCompletion {
    session: RemoteWorkspaceConnectedSession,
    destination: SshDestination,
    directory: RemoteWorkspaceDirectory,
    physical_directory: RemoteDirectoryIdentity,
    account: RemoteWorkspaceAccount,
    terminal_channels: Arc<dyn RemoteTerminalChannelProvider>,
    lifecycle: ControlConnectionObserver,
}

impl RemoteWorkspaceFlowCompletion {
    #[cfg(test)]
    pub(super) fn for_test(
        session: RemoteWorkspaceConnectedSession,
        destination: SshDestination,
        directory: RemoteWorkspaceDirectory,
        physical_directory: RemoteDirectoryIdentity,
        account: RemoteWorkspaceAccount,
        terminal_channels: Arc<dyn RemoteTerminalChannelProvider>,
        lifecycle: ControlConnectionObserver,
    ) -> Self {
        Self {
            session,
            destination,
            directory,
            physical_directory,
            account,
            terminal_channels,
            lifecycle,
        }
    }

    pub(super) const fn session(&self) -> &RemoteWorkspaceConnectedSession {
        &self.session
    }

    pub(super) const fn destination(&self) -> &SshDestination {
        &self.destination
    }

    pub(super) const fn directory(&self) -> &RemoteWorkspaceDirectory {
        &self.directory
    }

    pub(super) const fn physical_directory(&self) -> &RemoteDirectoryIdentity {
        &self.physical_directory
    }

    pub(super) fn remote_user(&self) -> &str {
        self.account.user()
    }

    pub(super) const fn remote_home_identity(&self) -> &RemoteDirectoryIdentity {
        self.account.home_identity()
    }

    pub(super) fn login_shell(&self) -> &str {
        self.account.login_shell().as_str()
    }

    pub(super) fn terminal_channels(&self) -> Arc<dyn RemoteTerminalChannelProvider> {
        Arc::clone(&self.terminal_channels)
    }

    /// Acquires the independent alias pin only when Workspace installation is ready to commit.
    ///
    /// Failure borrows no ownership from this completion, so activation can return it intact.
    pub(super) fn acquire_workspace_alias_pin(
        &self,
    ) -> Result<Option<RemoteWorkspaceAliasPin>, RemoteWorkspaceAliasPinError> {
        self.session.acquire_workspace_alias_pin()
    }

    pub(super) fn into_parts(
        self,
    ) -> (
        RemoteWorkspaceConnectedSession,
        SshDestination,
        RemoteWorkspaceDirectory,
        RemoteDirectoryIdentity,
        RemoteWorkspaceAccount,
        Arc<dyn RemoteTerminalChannelProvider>,
        ControlConnectionObserver,
    ) {
        (
            self.session,
            self.destination,
            self.directory,
            self.physical_directory,
            self.account,
            self.terminal_channels,
            self.lifecycle,
        )
    }
}

/// Borrow-safe, exactly-once transfer of a non-Clone flow completion through GPUI events.
#[derive(Clone)]
pub(super) struct RemoteWorkspaceFlowCompletionHandle {
    completion: Arc<Mutex<Option<RemoteWorkspaceFlowCompletion>>>,
}

impl RemoteWorkspaceFlowCompletionHandle {
    fn new(completion: RemoteWorkspaceFlowCompletion) -> Self {
        Self {
            completion: Arc::new(Mutex::new(Some(completion))),
        }
    }

    pub(super) fn take(&self) -> Option<RemoteWorkspaceFlowCompletion> {
        self.completion
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    fn is_same_transfer(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.completion, &other.completion)
    }

    fn is_empty(&self) -> bool {
        self.completion
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
    }
}

#[derive(Clone)]
pub(super) enum RemoteWorkspaceFlowEvent {
    StateChanged,
    Completed(RemoteWorkspaceFlowCompletionHandle),
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RemoteWorkspaceFlowStage {
    Idle,
    HostSelection,
    AddingHost,
    EditingHost,
    DeleteConfirmation,
    DeletingHost,
    Connecting(RemoteWorkspaceConnectionProgress),
    ConnectionError,
    DirectorySelection,
    AwaitingActivation,
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
enum DeleteAction {
    Delete,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AcknowledgeAction {
    Acknowledge,
}

pub(super) struct RemoteWorkspaceFlow {
    backend: Arc<dyn RemoteWorkspaceFlowBackend>,
    host_picker: Entity<SshHostPicker>,
    focus_scope: FocusHandle,
    active_form: Option<Entity<SshHostForm>>,
    remote_picker: Option<Entity<RemoteWorkspacePicker>>,
    stage: RemoteWorkspaceFlowStage,
    action_generation: u64,
    pending_destination: Option<SshDestination>,
    connection_error: Option<RemoteWorkspaceFlowBackendError>,
    connection_cancelled: Option<Arc<AtomicBool>>,
    progress: Option<ProgressDialogHandle>,
    connected: Option<RemoteWorkspaceConnectedSession>,
    retained_lifecycle: Option<ControlConnectionObserver>,
    pending_completion: Option<RemoteWorkspaceFlowCompletionHandle>,
    observed_progress: Vec<RemoteWorkspaceConnectionProgress>,
    delete_alert: Option<ModalPresentationHandle>,
    error_alert: Option<ModalPresentationHandle>,
    cancelled_emitted: bool,
}

impl EventEmitter<RemoteWorkspaceFlowEvent> for RemoteWorkspaceFlow {}

impl RemoteWorkspaceFlow {
    pub(super) fn new(
        backend: Arc<dyn RemoteWorkspaceFlowBackend>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let discovery: Arc<dyn HostDiscoveryProvider> = Arc::new(FlowHostDiscoveryProvider {
            backend: Arc::clone(&backend),
        });
        let active_backend = Arc::clone(&backend);
        let active_use: Arc<dyn Fn(&SshHostAlias) -> bool + Send + Sync> =
            Arc::new(move |alias| active_backend.host_in_active_use(alias));
        let host_picker = cx.new(|cx| SshHostPicker::new(discovery, active_use, window, cx));
        cx.subscribe_in(
            &host_picker,
            window,
            |flow, _, event: &SshHostPickerEvent, window, cx| {
                flow.reduce_host_event(event, window, cx);
            },
        )
        .detach();
        Self {
            backend,
            host_picker,
            focus_scope: cx.focus_handle(),
            active_form: None,
            remote_picker: None,
            stage: RemoteWorkspaceFlowStage::Idle,
            action_generation: 0,
            pending_destination: None,
            connection_error: None,
            connection_cancelled: None,
            progress: None,
            connected: None,
            retained_lifecycle: None,
            pending_completion: None,
            observed_progress: Vec::new(),
            delete_alert: None,
            error_alert: None,
            cancelled_emitted: false,
        }
    }

    pub(super) const fn stage(&self) -> RemoteWorkspaceFlowStage {
        self.stage
    }

    pub(super) fn owns_activation(&self, handle: &RemoteWorkspaceFlowCompletionHandle) -> bool {
        self.stage == RemoteWorkspaceFlowStage::AwaitingActivation
            && self
                .pending_completion
                .as_ref()
                .is_some_and(|pending| pending.is_same_transfer(handle))
            && handle.is_empty()
    }

    #[cfg(test)]
    pub(super) fn emit_completion_for_test(
        &mut self,
        completion: RemoteWorkspaceFlowCompletion,
        cx: &mut Context<Self>,
    ) {
        let handle = RemoteWorkspaceFlowCompletionHandle::new(completion);
        self.pending_completion = Some(handle.clone());
        self.stage = RemoteWorkspaceFlowStage::AwaitingActivation;
        cx.emit(RemoteWorkspaceFlowEvent::Completed(handle));
        self.publish(cx);
    }

    #[cfg(test)]
    pub(super) fn cancel_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_flow(window, cx);
    }

    pub(super) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.open_with_replacement(None, window, cx)
    }

    pub(super) fn open_replacing(
        &mut self,
        replacement: CommandPaletteReplacementFocus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.open_with_replacement(Some(replacement), window, cx)
    }

    fn open_with_replacement(
        &mut self,
        replacement: Option<CommandPaletteReplacementFocus>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(
            self.stage,
            RemoteWorkspaceFlowStage::Idle | RemoteWorkspaceFlowStage::Cancelled
        ) {
            return false;
        }
        self.cancelled_emitted = false;
        self.stage = RemoteWorkspaceFlowStage::HostSelection;
        let blocked_by_modal = spaceterm_ui::window_modal_is_open(window, cx);
        let opened = self.host_picker.update(cx, |picker, cx| match replacement {
            Some(replacement) => picker.open_replacing(replacement, window, cx),
            None => picker.open(window, cx),
        });
        if !opened && !blocked_by_modal {
            self.stage = RemoteWorkspaceFlowStage::Idle;
            return false;
        }
        self.publish(cx);
        true
    }

    pub(super) fn blocks_terminal_input(&self) -> bool {
        !matches!(
            self.stage,
            RemoteWorkspaceFlowStage::Idle
                | RemoteWorkspaceFlowStage::Completed
                | RemoteWorkspaceFlowStage::Cancelled
        )
    }

    pub(super) fn owns_first_responder(&self, window: &Window, cx: &App) -> bool {
        self.focus_scope.contains_focused(window, cx)
    }

    fn reduce_host_event(
        &mut self,
        event: &SshHostPickerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SshHostPickerEvent::Lifecycle(SshHostPickerLifecycleEvent::Opened) => {}
            SshHostPickerEvent::Lifecycle(SshHostPickerLifecycleEvent::Closed(reason)) => {
                if self.stage == RemoteWorkspaceFlowStage::HostSelection
                    && !matches!(reason, spaceterm_ui::CommandPaletteCloseReason::Replaced)
                {
                    self.cancel_flow(window, cx);
                }
            }
            SshHostPickerEvent::SelectDestination(destination)
                if self.stage == RemoteWorkspaceFlowStage::HostSelection =>
            {
                self.start_connection(destination.clone(), window, cx);
            }
            SshHostPickerEvent::RequestAddHost(_)
                if self.stage == RemoteWorkspaceFlowStage::HostSelection =>
            {
                self.present_host_form(SshHostFormMode::Add, window, cx);
            }
            SshHostPickerEvent::RequestEditHost(alias)
                if self.stage == RemoteWorkspaceFlowStage::HostSelection =>
            {
                if self.backend.host_in_active_use(alias) {
                    return;
                }
                if let Some(host) = self.backend.managed_host(alias) {
                    self.present_host_form(SshHostFormMode::Edit(host), window, cx);
                }
            }
            SshHostPickerEvent::RequestDeleteHost(alias)
                if self.stage == RemoteWorkspaceFlowStage::HostSelection =>
            {
                if !self.backend.host_in_active_use(alias) {
                    self.present_delete_confirmation(alias.clone(), window, cx);
                }
            }
            _ => {}
        }
    }

    fn present_host_form(
        &mut self,
        mode: SshHostFormMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.action_generation = self.action_generation.wrapping_add(1);
        let generation = self.action_generation;
        let stage = match mode {
            SshHostFormMode::Add => RemoteWorkspaceFlowStage::AddingHost,
            SshHostFormMode::Edit(_) => RemoteWorkspaceFlowStage::EditingHost,
        };
        let backend: Arc<dyn ManagedHostFormBackend> = Arc::new(FlowManagedHostBackend {
            backend: Arc::clone(&self.backend),
        });
        let form = cx.new(|cx| SshHostForm::new(mode, backend, window, cx));
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
        self.active_form = Some(form);
        self.stage = stage;
        self.publish(cx);
    }

    fn reduce_form_event(
        &mut self,
        generation: u64,
        event: &SshHostFormEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.action_generation != generation {
            return;
        }
        match event {
            SshHostFormEvent::StateChanged => cx.notify(),
            SshHostFormEvent::SavedAndConnect(host)
                if self.stage == RemoteWorkspaceFlowStage::AddingHost =>
            {
                self.active_form = None;
                self.host_picker
                    .update(cx, |picker, cx| picker.refresh(window, cx));
                let Ok(destination) = SshDestination::new(host.alias().as_str().to_owned()) else {
                    self.stage = RemoteWorkspaceFlowStage::HostSelection;
                    self.publish(cx);
                    return;
                };
                self.start_connection(destination, window, cx);
            }
            SshHostFormEvent::Saved(_) if self.stage == RemoteWorkspaceFlowStage::EditingHost => {
                self.active_form = None;
                self.stage = RemoteWorkspaceFlowStage::HostSelection;
                self.host_picker
                    .update(cx, |picker, cx| picker.refresh(window, cx));
                self.publish(cx);
            }
            SshHostFormEvent::Cancelled
                if matches!(
                    self.stage,
                    RemoteWorkspaceFlowStage::AddingHost | RemoteWorkspaceFlowStage::EditingHost
                ) =>
            {
                self.active_form = None;
                self.stage = RemoteWorkspaceFlowStage::HostSelection;
                self.publish(cx);
            }
            _ => {}
        }
    }

    fn present_delete_confirmation(
        &mut self,
        alias: SshHostAlias,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.action_generation = self.action_generation.wrapping_add(1);
        let generation = self.action_generation;
        self.stage = RemoteWorkspaceFlowStage::DeleteConfirmation;
        let flow = cx.weak_entity();
        let window_handle = window.window_handle();
        let expected = alias.clone();
        let result = Alert::new(
            ModalId::new(DELETE_CONFIRMATION_ID),
            "Delete managed SSH host",
            "Delete SSH Host?",
            format!("Delete the managed SSH host {}?", alias.as_str()),
            vec![
                ModalAction::new(
                    DeleteAction::Delete,
                    "Delete",
                    ModalActionRole::Affirmative,
                    "remote-workspace-delete-confirm",
                )
                .with_intent(ModalActionIntent::Destructive),
                ModalAction::new(
                    DeleteAction::Cancel,
                    "Cancel",
                    ModalActionRole::Cancel,
                    "remote-workspace-delete-cancel",
                ),
            ],
        )
        .intent(AlertIntent::Critical)
        .present(window, cx, move |outcome, cx| {
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = flow.update(cx, |flow, cx| {
                    flow.finish_delete_confirmation(generation, expected, outcome, window, cx);
                });
            });
        });
        match result {
            Ok(handle) => self.delete_alert = Some(handle),
            Err(_) => self.stage = RemoteWorkspaceFlowStage::HostSelection,
        }
        self.publish(cx);
    }

    fn finish_delete_confirmation(
        &mut self,
        generation: u64,
        alias: SshHostAlias,
        outcome: AlertOutcome<DeleteAction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.action_generation != generation
            || self.stage != RemoteWorkspaceFlowStage::DeleteConfirmation
        {
            return;
        }
        self.delete_alert = None;
        if matches!(
            outcome,
            AlertOutcome::Activated {
                action_id: DeleteAction::Delete,
                ..
            }
        ) {
            if self.backend.host_in_active_use(&alias) {
                self.stage = RemoteWorkspaceFlowStage::DeletingHost;
                self.present_delete_error(RemoteWorkspaceFlowBackendError::HostInUse, window, cx);
            } else {
                self.start_delete(alias, window, cx);
            }
        } else {
            self.stage = RemoteWorkspaceFlowStage::HostSelection;
            self.publish(cx);
        }
    }

    fn start_delete(&mut self, alias: SshHostAlias, window: &mut Window, cx: &mut Context<Self>) {
        self.action_generation = self.action_generation.wrapping_add(1);
        let generation = self.action_generation;
        self.stage = RemoteWorkspaceFlowStage::DeletingHost;
        let task = self.backend.delete_managed_host(alias);
        cx.spawn_in(window, async move |flow, cx| {
            let result = task.await;
            let _ = flow.update_in(cx, |flow, window, cx| {
                if flow.action_generation != generation
                    || flow.stage != RemoteWorkspaceFlowStage::DeletingHost
                {
                    return;
                }
                match result {
                    Ok(()) => {
                        flow.stage = RemoteWorkspaceFlowStage::HostSelection;
                        flow.host_picker
                            .update(cx, |picker, cx| picker.refresh(window, cx));
                        flow.publish(cx);
                    }
                    Err(error) => flow.present_delete_error(error, window, cx),
                }
            });
        })
        .detach();
        self.publish(cx);
    }

    fn present_delete_error(
        &mut self,
        error: RemoteWorkspaceFlowBackendError,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, message) = match error {
            RemoteWorkspaceFlowBackendError::HostInUse => (
                "SSH Host Is in Use",
                "Close the Remote Project Workspace using this SSH host, then try again.",
            ),
            _ => (
                "Couldn\u{2019}t Delete SSH Host",
                "Check the managed SSH configuration and try again.",
            ),
        };
        let flow = cx.weak_entity();
        let window_handle = window.window_handle();
        let result = Alert::new(
            ModalId::new(DELETE_ERROR_ID),
            "SSH host deletion failed",
            title,
            message,
            vec![ModalAction::new(
                AcknowledgeAction::Acknowledge,
                "OK",
                ModalActionRole::Cancel,
                "remote-workspace-delete-error-ok",
            )],
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, move |_, cx| {
            let _ = window_handle.update(cx, |_, _, cx| {
                let _ = flow.update(cx, |flow, cx| {
                    if flow.stage == RemoteWorkspaceFlowStage::DeletingHost {
                        flow.error_alert = None;
                        flow.stage = RemoteWorkspaceFlowStage::HostSelection;
                        flow.publish(cx);
                    }
                });
            });
        });
        if let Ok(handle) = result {
            self.error_alert = Some(handle);
        } else {
            self.stage = RemoteWorkspaceFlowStage::HostSelection;
        }
        self.publish(cx);
    }

    fn start_connection(
        &mut self,
        destination: SshDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(
            self.stage,
            RemoteWorkspaceFlowStage::HostSelection
                | RemoteWorkspaceFlowStage::AddingHost
                | RemoteWorkspaceFlowStage::ConnectionError
        ) {
            return;
        }
        if self.stage == RemoteWorkspaceFlowStage::HostSelection
            && self.connected.is_some()
            && self.pending_destination.as_ref() == Some(&destination)
        {
            self.connection_error = None;
            self.open_remote_picker(window, cx);
            return;
        }
        self.action_generation = self.action_generation.wrapping_add(1);
        let generation = self.action_generation;
        self.pending_destination = Some(destination.clone());
        self.connection_error = None;
        self.connected = None;
        self.retained_lifecycle = None;
        self.observed_progress.clear();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.connection_cancelled = Some(Arc::clone(&cancelled));
        self.stage = RemoteWorkspaceFlowStage::Connecting(
            RemoteWorkspaceConnectionProgress::CheckingCompatibility,
        );

        let flow = cx.weak_entity();
        let result_flow = flow.clone();
        let window_handle = window.window_handle();
        let result_window = window_handle;
        let dialog = ProgressDialog::new(
            ModalId::new(CONNECTION_PROGRESS_ID),
            "Remote connection progress",
            "Connect to Remote Host",
            RemoteWorkspaceConnectionProgress::CheckingCompatibility.status(),
            ProgressState::Indeterminate,
            ProgressCancellation::Cancellable(ModalAction::new(
                (),
                "Cancel",
                ModalActionRole::Cancel,
                "remote-workspace-connect-cancel",
            )),
        )
        .detail("Authentication prompts may appear in a secure macOS sheet.")
        .present(
            window,
            cx,
            move |_, _, cx| {
                let _ = flow.update(cx, |flow, _| {
                    if flow.action_generation == generation
                        && let Some(cancelled) = &flow.connection_cancelled
                    {
                        cancelled.store(true, Ordering::Release);
                    }
                });
                ProgressCancelDecision::Allow
            },
            move |outcome, cx| {
                let _ = result_window.update(cx, |_, window, cx| {
                    let _ = result_flow.update(cx, |flow, cx| {
                        flow.finish_progress(generation, outcome, window, cx);
                    });
                });
            },
        );
        let Ok(progress) = dialog else {
            self.connection_error = Some(RemoteWorkspaceFlowBackendError::ConnectionFailed);
            self.stage = RemoteWorkspaceFlowStage::ConnectionError;
            self.present_connection_error(generation, window, cx);
            return;
        };
        self.progress = Some(progress);

        let (progress_sender, progress_receiver) = async_channel::bounded(8);
        let context = RemoteWorkspaceConnectContext {
            progress: progress_sender,
            cancelled,
        };
        let task = self.backend.connect(destination, context);
        cx.spawn_in(window, async move |flow, cx| {
            while let Ok(progress) = progress_receiver.recv().await {
                let Ok(keep_receiving) = flow.update_in(cx, |flow, window, cx| {
                    flow.apply_connection_progress(generation, progress, window, cx)
                }) else {
                    break;
                };
                if !keep_receiving {
                    break;
                }
            }
        })
        .detach();
        cx.spawn_in(window, async move |flow, cx| {
            let result = task.await;
            let _ = flow.update_in(cx, |flow, window, cx| {
                flow.finish_connection(generation, result, window, cx);
            });
        })
        .detach();
        self.publish(cx);
    }

    fn apply_connection_progress(
        &mut self,
        generation: u64,
        progress: RemoteWorkspaceConnectionProgress,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.action_generation != generation
            || !matches!(self.stage, RemoteWorkspaceFlowStage::Connecting(_))
        {
            return false;
        }
        self.stage = RemoteWorkspaceFlowStage::Connecting(progress);
        self.observed_progress.push(progress);
        if let Some(handle) = &self.progress {
            let _ = handle.update(
                ProgressDialogUpdate::new().status(progress.status()),
                window,
                cx,
            );
        }
        self.publish(cx);
        true
    }

    fn finish_connection(
        &mut self,
        generation: u64,
        result: Result<RemoteWorkspaceConnectedSession, RemoteWorkspaceFlowBackendError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.action_generation != generation
            || !matches!(self.stage, RemoteWorkspaceFlowStage::Connecting(_))
            || self
                .connection_cancelled
                .as_ref()
                .is_some_and(|cancelled| cancelled.load(Ordering::Acquire))
        {
            return;
        }
        match result {
            Ok(session) => {
                self.connected = Some(session);
                if let Some(handle) = &self.progress {
                    let _ = handle.complete(window, cx);
                }
            }
            Err(error) => {
                self.connection_error = Some(error);
                self.stage = RemoteWorkspaceFlowStage::ConnectionError;
                if let Some(handle) = &self.progress {
                    let _ = handle.fail(window, cx);
                }
            }
        }
        self.publish(cx);
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
        self.progress = None;
        match outcome {
            ProgressDialogOutcome::Completed if self.connected.is_some() => {
                self.open_remote_picker(window, cx);
            }
            ProgressDialogOutcome::Failed
                if matches!(
                    self.connection_error.as_ref(),
                    Some(RemoteWorkspaceFlowBackendError::AuthenticationCancelled)
                ) =>
            {
                self.connection_error = None;
                self.connection_cancelled = None;
                self.pending_destination = None;
                self.stage = RemoteWorkspaceFlowStage::HostSelection;
                self.publish(cx);
            }
            ProgressDialogOutcome::Failed if self.connection_error.is_some() => {
                self.present_connection_error(generation, window, cx);
            }
            ProgressDialogOutcome::Cancelled { .. }
            | ProgressDialogOutcome::DeadlineExpired
            | ProgressDialogOutcome::OwnerRemoved
            | ProgressDialogOutcome::ProgrammaticDismissal
            | ProgressDialogOutcome::Replaced => self.cancel_flow(window, cx),
            ProgressDialogOutcome::Completed => {}
            ProgressDialogOutcome::Failed => {}
        }
    }

    fn present_connection_error(
        &mut self,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.action_generation != generation {
            return;
        }
        self.stage = RemoteWorkspaceFlowStage::ConnectionError;
        let connection_error = self.connection_error.take();
        let content = connection_error_content(connection_error.as_ref());
        let flow = cx.weak_entity();
        let window_handle = window.window_handle();
        let alert = Alert::new(
            ModalId::new(CONNECTION_ERROR_ID),
            "Remote connection failed",
            "Couldn\u{2019}t Connect",
            content.message,
            vec![
                ModalAction::new(
                    ConnectionErrorAction::Retry,
                    "Retry",
                    ModalActionRole::Affirmative,
                    "remote-workspace-retry",
                )
                .default_action(true),
                ModalAction::new(
                    ConnectionErrorAction::Back,
                    "Choose Another Host",
                    ModalActionRole::Auxiliary,
                    "remote-workspace-back-to-hosts",
                ),
                ModalAction::new(
                    ConnectionErrorAction::Cancel,
                    "Cancel",
                    ModalActionRole::Cancel,
                    "remote-workspace-error-cancel",
                ),
            ],
        );
        let alert = if let Some(detail) = content.detail {
            alert.detail(detail)
        } else {
            alert
        };
        let alert = alert
            .intent(AlertIntent::Warning)
            .present(window, cx, move |outcome, cx| {
                let _ = window_handle.update(cx, |_, window, cx| {
                    let _ = flow.update(cx, |flow, cx| {
                        flow.finish_connection_error(generation, outcome, window, cx);
                    });
                });
            });
        match alert {
            Ok(handle) => self.error_alert = Some(handle),
            Err(_) => self.cancel_flow(window, cx),
        }
        self.publish(cx);
    }

    fn finish_connection_error(
        &mut self,
        generation: u64,
        outcome: AlertOutcome<ConnectionErrorAction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.action_generation != generation
            || self.stage != RemoteWorkspaceFlowStage::ConnectionError
        {
            return;
        }
        self.error_alert = None;
        let action = match outcome {
            AlertOutcome::Activated { action_id, .. } => action_id,
            AlertOutcome::Dismissed { .. } => ConnectionErrorAction::Cancel,
        };
        match action {
            ConnectionErrorAction::Retry => {
                let Some(destination) = self.pending_destination.clone() else {
                    self.cancel_flow(window, cx);
                    return;
                };
                cx.defer_in(window, move |flow, window, cx| {
                    flow.start_connection(destination, window, cx);
                });
            }
            ConnectionErrorAction::Back => {
                self.connection_error = None;
                self.pending_destination = None;
                self.connection_cancelled = None;
                self.stage = RemoteWorkspaceFlowStage::HostSelection;
                self.publish(cx);
            }
            ConnectionErrorAction::Cancel => self.cancel_flow(window, cx),
        }
    }

    fn open_remote_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.connected.as_ref() else {
            self.cancel_flow(window, cx);
            return;
        };
        self.action_generation = self.action_generation.wrapping_add(1);
        let generation = self.action_generation;
        let provider = session.provider();
        let picker = cx.new(|cx| RemoteWorkspacePicker::new(provider, window, cx));
        cx.subscribe_in(
            &picker,
            window,
            move |flow, _, event: &RemoteWorkspacePickerEvent, window, cx| {
                flow.reduce_remote_picker_event(generation, event, window, cx);
            },
        )
        .detach();
        let replacement = self
            .host_picker
            .update(cx, |host, cx| host.dismiss_for_replacement(window, cx));
        let opened = picker.update(cx, |picker, cx| match replacement {
            Some(replacement) => picker.open_replacing(replacement, window, cx),
            None => picker.open(window, cx),
        });
        if !opened {
            self.connected = None;
            self.cancel_flow(window, cx);
            return;
        }
        self.remote_picker = Some(picker);
        self.connection_cancelled = None;
        self.connection_error = None;
        self.stage = RemoteWorkspaceFlowStage::DirectorySelection;
        self.publish(cx);
    }

    fn reduce_remote_picker_event(
        &mut self,
        generation: u64,
        event: &RemoteWorkspacePickerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.action_generation != generation
            || self.stage != RemoteWorkspaceFlowStage::DirectorySelection
        {
            return;
        }
        match event {
            RemoteWorkspacePickerEvent::StateChanged => cx.notify(),
            RemoteWorkspacePickerEvent::BackToHost => {
                self.action_generation = self.action_generation.wrapping_add(1);
                self.remote_picker = None;
                self.stage = RemoteWorkspaceFlowStage::HostSelection;
                self.host_picker
                    .update(cx, |picker, cx| picker.open(window, cx));
                self.publish(cx);
            }
            RemoteWorkspacePickerEvent::Dismissed => self.cancel_flow(window, cx),
            RemoteWorkspacePickerEvent::Confirmed(selection) => {
                self.complete(selection.clone(), window, cx);
            }
        }
    }

    fn complete(
        &mut self,
        selection: RemoteWorkspaceSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(mut session), Some(destination)) =
            (self.connected.take(), self.pending_destination.take())
        else {
            if let Some(picker) = &self.remote_picker {
                picker.update(cx, |picker, cx| picker.activation_failed(window, cx));
            }
            return;
        };
        let terminal_channels = match session.bind_terminal_channels_for_identity(
            selection.directory(),
            selection.physical_directory(),
            selection.account().login_shell(),
        ) {
            Ok(provider) => provider,
            Err(error) => {
                self.connected = Some(session);
                self.pending_destination = Some(destination);
                self.connection_error = Some(error);
                if let Some(picker) = &self.remote_picker {
                    picker.update(cx, |picker, cx| picker.activation_failed(window, cx));
                }
                self.publish(cx);
                return;
            }
        };
        let lifecycle = self
            .retained_lifecycle
            .take()
            .or_else(|| session.take_lifecycle_observer());
        let Some(lifecycle) = lifecycle else {
            self.connected = Some(session);
            self.pending_destination = Some(destination);
            self.connection_error = Some(RemoteWorkspaceFlowBackendError::ConnectionFailed);
            if let Some(picker) = &self.remote_picker {
                picker.update(cx, |picker, cx| picker.activation_failed(window, cx));
            }
            self.publish(cx);
            return;
        };
        let completion = RemoteWorkspaceFlowCompletion {
            session,
            destination,
            directory: selection.directory().clone(),
            physical_directory: selection.physical_directory().clone(),
            account: selection.account().clone(),
            terminal_channels,
            lifecycle,
        };
        let handle = RemoteWorkspaceFlowCompletionHandle::new(completion);
        self.pending_completion = Some(handle.clone());
        self.stage = RemoteWorkspaceFlowStage::AwaitingActivation;
        cx.emit(RemoteWorkspaceFlowEvent::Completed(handle));
        self.publish(cx);
    }

    /// Acknowledges that the transferred completion was installed into Workspace ownership.
    pub(super) fn activation_succeeded(
        &mut self,
        handle: &RemoteWorkspaceFlowCompletionHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.owns_activation(handle) {
            return false;
        }
        drop(handle.take());
        if let Some(picker) = &self.remote_picker {
            picker.update(cx, |picker, cx| {
                picker.complete_activation(window, cx);
            });
        }
        self.pending_completion = None;
        self.remote_picker = None;
        self.action_generation = self.action_generation.wrapping_add(1);
        self.stage = RemoteWorkspaceFlowStage::Completed;
        self.publish(cx);
        true
    }

    /// Returns a completion whose Workspace creation failed, restoring the retained picker.
    pub(super) fn activation_failed(
        &mut self,
        handle: &RemoteWorkspaceFlowCompletionHandle,
        completion: RemoteWorkspaceFlowCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), Box<RemoteWorkspaceFlowCompletion>> {
        if !self.owns_activation(handle) {
            return Err(Box::new(completion));
        }
        let RemoteWorkspaceFlowCompletion {
            session,
            destination,
            directory: _,
            physical_directory: _,
            account: _,
            terminal_channels: _,
            lifecycle,
        } = completion;
        self.connected = Some(session);
        self.retained_lifecycle = Some(lifecycle);
        self.pending_destination = Some(destination);
        self.pending_completion = None;
        if let Some(picker) = &self.remote_picker {
            picker.update(cx, |picker, cx| picker.activation_failed(window, cx));
        }
        self.stage = RemoteWorkspaceFlowStage::DirectorySelection;
        self.publish(cx);
        Ok(())
    }

    fn cancel_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(
            self.stage,
            RemoteWorkspaceFlowStage::Cancelled | RemoteWorkspaceFlowStage::Completed
        ) {
            return;
        }
        self.action_generation = self.action_generation.wrapping_add(1);
        if let Some(cancelled) = self.connection_cancelled.take() {
            cancelled.store(true, Ordering::Release);
        }
        if let Some(progress) = self.progress.take() {
            let _ = progress.dismiss(window, cx);
        }
        if let Some(alert) = self.delete_alert.take().or_else(|| self.error_alert.take()) {
            let _ = alert.dismiss(window, cx);
        }
        if self.stage == RemoteWorkspaceFlowStage::AwaitingActivation {
            if let Some(handle) = self.pending_completion.take() {
                drop(handle.take());
            }
            if let Some(picker) = &self.remote_picker {
                picker.update(cx, |picker, cx| picker.activation_failed(window, cx));
            }
        }
        if let Some(picker) = &self.remote_picker {
            picker.update(cx, |picker, cx| {
                picker.dismiss(window, cx);
            });
        }
        self.host_picker
            .update(cx, |picker, cx| picker.dismiss(window, cx));
        self.active_form = None;
        self.remote_picker = None;
        self.pending_completion = None;
        self.connected = None;
        self.retained_lifecycle = None;
        self.pending_destination = None;
        self.connection_error = None;
        self.stage = RemoteWorkspaceFlowStage::Cancelled;
        if !self.cancelled_emitted {
            self.cancelled_emitted = true;
            cx.emit(RemoteWorkspaceFlowEvent::Cancelled);
        }
        self.publish(cx);
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        cx.emit(RemoteWorkspaceFlowEvent::StateChanged);
        cx.notify();
    }
}

impl Drop for RemoteWorkspaceFlow {
    fn drop(&mut self) {
        if let Some(cancelled) = self.connection_cancelled.take() {
            cancelled.store(true, Ordering::Release);
        }
        if let Some(handle) = self.pending_completion.take() {
            drop(handle.take());
        }
        self.connected = None;
    }
}

impl Render for RemoteWorkspaceFlow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus_scope)
            .child(self.host_picker.clone())
            .children(self.remote_picker.iter().cloned())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gpui::{FocusHandle, TestAppContext, VisualTestContext};
    use spaceterm_ui::ModalLayer;

    use super::*;
    use crate::ui::new_workspace_panel::{
        NewWorkspacePanel, NewWorkspacePanelEvent, NewWorkspaceSource,
    };
    use crate::ui::remote_workspace_picker::{
        RemoteWorkspaceDirectoryListing, RemoteWorkspaceExactPathState,
        RemoteWorkspaceProviderError,
    };

    #[derive(Default)]
    struct ReadyRemoteProvider;

    impl RemoteWorkspaceProvider for ReadyRemoteProvider {
        fn discover_account(
            &self,
        ) -> Task<Result<RemoteWorkspaceAccount, RemoteWorkspaceProviderError>> {
            Task::ready(Ok(remote_account()))
        }

        fn list_directories(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> Task<Result<RemoteWorkspaceDirectoryListing, RemoteWorkspaceProviderError>> {
            Task::ready(Ok(RemoteWorkspaceDirectoryListing::new(Vec::new())))
        }

        fn probe_exact_path(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> Task<Result<RemoteWorkspaceExactPathState, RemoteWorkspaceProviderError>> {
            Task::ready(Ok(RemoteWorkspaceExactPathState::ReadableDirectory))
        }

        fn create_directory_recursively(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> Task<Result<(), RemoteWorkspaceProviderError>> {
            Task::ready(Ok(()))
        }

        fn validate_physical_identity(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> Task<Result<RemoteDirectoryIdentity, RemoteWorkspaceProviderError>> {
            Task::ready(Ok(remote_identity("/home/tester")))
        }
    }

    struct CountingOwner {
        closes: Arc<AtomicUsize>,
        observer_takes: Option<Arc<AtomicUsize>>,
    }

    impl RemoteWorkspaceSessionOwner for CountingOwner {
        fn acquire_workspace_alias_pin(
            &self,
        ) -> Result<Option<RemoteWorkspaceAliasPin>, RemoteWorkspaceAliasPinError> {
            Ok(None)
        }

        fn bind_terminal_channels_for_identity(
            &self,
            _: &RemoteWorkspaceDirectory,
            _: &RemoteDirectoryIdentity,
            _: &ValidatedRemoteLoginShell,
        ) -> Result<Arc<dyn RemoteTerminalChannelProvider>, RemoteWorkspaceFlowBackendError>
        {
            Ok(Arc::new(|| Err(crate::terminal::RemoteChannelUnavailable)))
        }

        fn take_lifecycle_observer(&mut self) -> Option<ControlConnectionObserver> {
            if let Some(observer_takes) = &self.observer_takes {
                observer_takes.fetch_add(1, Ordering::SeqCst);
            }
            Some(ControlConnectionObserver::closed())
        }

        fn close(&mut self) {
            self.closes.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct AliasPinningOwner {
        alias: Option<crate::ssh::alias_usage::ActiveSshAliasLease>,
        acquisition_fails: bool,
        closes: Arc<AtomicUsize>,
    }

    impl RemoteWorkspaceSessionOwner for AliasPinningOwner {
        fn acquire_workspace_alias_pin(
            &self,
        ) -> Result<Option<RemoteWorkspaceAliasPin>, RemoteWorkspaceAliasPinError> {
            if self.acquisition_fails {
                return Err(RemoteWorkspaceAliasPinError);
            }
            self.alias
                .as_ref()
                .map(|alias| {
                    alias
                        .try_duplicate()
                        .map(RemoteWorkspaceAliasPin::new)
                        .map_err(|_| RemoteWorkspaceAliasPinError)
                })
                .transpose()
        }

        fn bind_terminal_channels_for_identity(
            &self,
            _: &RemoteWorkspaceDirectory,
            _: &RemoteDirectoryIdentity,
            _: &ValidatedRemoteLoginShell,
        ) -> Result<Arc<dyn RemoteTerminalChannelProvider>, RemoteWorkspaceFlowBackendError>
        {
            Ok(Arc::new(|| Err(crate::terminal::RemoteChannelUnavailable)))
        }

        fn take_lifecycle_observer(&mut self) -> Option<ControlConnectionObserver> {
            Some(ControlConnectionObserver::closed())
        }

        fn close(&mut self) {
            self.alias.take();
            self.closes.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Clone)]
    struct SaveRecord {
        host: ManagedSshHost,
        editing_alias: Option<SshHostAlias>,
    }

    struct FakeBackendState {
        managed: BTreeMap<SshHostAlias, ManagedSshHost>,
        active: BTreeSet<SshHostAlias>,
        connections: VecDeque<
            Task<Result<RemoteWorkspaceConnectedSession, RemoteWorkspaceFlowBackendError>>,
        >,
        saves: VecDeque<Task<Result<(), ManagedHostFormBackendError>>>,
        deletes: VecDeque<Task<Result<(), RemoteWorkspaceFlowBackendError>>>,
        save_records: Vec<SaveRecord>,
        delete_records: Vec<SshHostAlias>,
        connect_records: Vec<SshDestination>,
    }

    struct FakeBackend {
        state: Mutex<FakeBackendState>,
        discoveries: AtomicUsize,
    }

    impl FakeBackend {
        fn new(
            connections: impl IntoIterator<
                Item = Task<
                    Result<RemoteWorkspaceConnectedSession, RemoteWorkspaceFlowBackendError>,
                >,
            >,
        ) -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new(FakeBackendState {
                    managed: BTreeMap::new(),
                    active: BTreeSet::new(),
                    connections: connections.into_iter().collect(),
                    saves: VecDeque::new(),
                    deletes: VecDeque::new(),
                    save_records: Vec::new(),
                    delete_records: Vec::new(),
                    connect_records: Vec::new(),
                }),
                discoveries: AtomicUsize::new(0),
            })
        }

        fn push_save(&self, result: Result<(), ManagedHostFormBackendError>) {
            self.state
                .lock()
                .unwrap()
                .saves
                .push_back(Task::ready(result));
        }

        fn push_delete(&self, result: Result<(), RemoteWorkspaceFlowBackendError>) {
            self.state
                .lock()
                .unwrap()
                .deletes
                .push_back(Task::ready(result));
        }

        fn insert_managed(&self, host: ManagedSshHost) {
            self.state
                .lock()
                .unwrap()
                .managed
                .insert(host.alias().clone(), host);
        }

        fn set_active(&self, alias: SshHostAlias, active: bool) {
            let mut state = self.state.lock().unwrap();
            if active {
                state.active.insert(alias);
            } else {
                state.active.remove(&alias);
            }
        }
    }

    impl RemoteWorkspaceFlowBackend for FakeBackend {
        fn discover_hosts(&self) -> HostDiscovery {
            self.discoveries.fetch_add(1, Ordering::SeqCst);
            HostDiscovery::default()
        }

        fn host_in_active_use(&self, alias: &SshHostAlias) -> bool {
            self.state.lock().unwrap().active.contains(alias)
        }

        fn managed_host(&self, alias: &SshHostAlias) -> Option<ManagedSshHost> {
            self.state.lock().unwrap().managed.get(alias).cloned()
        }

        fn save_managed_host(
            &self,
            host: ManagedSshHost,
            editing_alias: Option<SshHostAlias>,
        ) -> Task<Result<(), ManagedHostFormBackendError>> {
            let mut state = self.state.lock().unwrap();
            state.save_records.push(SaveRecord {
                host: host.clone(),
                editing_alias: editing_alias.clone(),
            });
            if let Some(editing_alias) = editing_alias {
                state.managed.remove(&editing_alias);
            }
            state.managed.insert(host.alias().clone(), host);
            state
                .saves
                .pop_front()
                .unwrap_or_else(|| Task::ready(Ok(())))
        }

        fn delete_managed_host(
            &self,
            alias: SshHostAlias,
        ) -> Task<Result<(), RemoteWorkspaceFlowBackendError>> {
            let mut state = self.state.lock().unwrap();
            state.delete_records.push(alias.clone());
            state.managed.remove(&alias);
            state
                .deletes
                .pop_front()
                .unwrap_or_else(|| Task::ready(Ok(())))
        }

        fn connect(
            &self,
            destination: SshDestination,
            context: RemoteWorkspaceConnectContext,
        ) -> Task<Result<RemoteWorkspaceConnectedSession, RemoteWorkspaceFlowBackendError>>
        {
            context.report(RemoteWorkspaceConnectionProgress::CheckingCompatibility);
            context.report(RemoteWorkspaceConnectionProgress::Connecting);
            context.report(RemoteWorkspaceConnectionProgress::Authenticating);
            assert!(!context.is_cancelled());
            let mut state = self.state.lock().unwrap();
            state.connect_records.push(destination);
            state.connections.pop_front().unwrap_or_else(|| {
                Task::ready(Err(RemoteWorkspaceFlowBackendError::ConnectionFailed))
            })
        }
    }

    #[derive(Default)]
    struct CapturedEvents {
        completions: Vec<RemoteWorkspaceFlowCompletionHandle>,
        cancelled: usize,
    }

    struct FlowHarness {
        flow: Entity<RemoteWorkspaceFlow>,
        events: Rc<RefCell<CapturedEvents>>,
        prior_focus: FocusHandle,
    }

    impl Render for FlowHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(
                div()
                    .size_full()
                    .track_focus(&self.prior_focus)
                    .child(self.flow.clone()),
            )
        }
    }

    struct ReplacementHarness {
        panel: Entity<NewWorkspacePanel>,
        flow: Entity<RemoteWorkspaceFlow>,
        prior_focus: FocusHandle,
        source_callbacks: usize,
        successful_transfers: usize,
    }

    impl ReplacementHarness {
        fn transfer_remote(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
            let Some(replacement) = self
                .panel
                .update(cx, |panel, cx| panel.dismiss_for_replacement(window, cx))
            else {
                return false;
            };
            let transferred = self
                .flow
                .update(cx, |flow, cx| flow.open_replacing(replacement, window, cx));
            self.successful_transfers += usize::from(transferred);
            transferred
        }
    }

    impl Render for ReplacementHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(
                div()
                    .size_full()
                    .track_focus(&self.prior_focus)
                    .child(self.panel.clone())
                    .child(self.flow.clone()),
            )
        }
    }

    fn flow_window(
        backend: Arc<FakeBackend>,
        cx: &mut TestAppContext,
    ) -> (
        Entity<FlowHarness>,
        Entity<RemoteWorkspaceFlow>,
        Rc<RefCell<CapturedEvents>>,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init);
        let injected: Arc<dyn RemoteWorkspaceFlowBackend> = backend;
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let flow = cx.new(|cx| RemoteWorkspaceFlow::new(injected, window, cx));
            let events = Rc::new(RefCell::new(CapturedEvents::default()));
            let captured = Rc::clone(&events);
            cx.subscribe(&flow, move |_, _, event, _| match event {
                RemoteWorkspaceFlowEvent::Completed(completion) => {
                    captured.borrow_mut().completions.push(completion.clone());
                }
                RemoteWorkspaceFlowEvent::Cancelled => captured.borrow_mut().cancelled += 1,
                RemoteWorkspaceFlowEvent::StateChanged => {}
            })
            .detach();
            let prior_focus = cx.focus_handle();
            prior_focus.focus(window);
            FlowHarness {
                flow,
                events,
                prior_focus,
            }
        });
        let (flow, events) = harness.read_with(cx, |harness, _| {
            (harness.flow.clone(), Rc::clone(&harness.events))
        });
        cx.update(|window, cx| {
            window.activate_window();
            flow.update(cx, |flow, cx| assert!(flow.open(window, cx)));
        });
        cx.run_until_parked();
        (harness, flow, events, cx)
    }

    fn replacement_window(
        backend: Arc<FakeBackend>,
        cx: &mut TestAppContext,
    ) -> (
        Entity<ReplacementHarness>,
        Entity<NewWorkspacePanel>,
        Entity<RemoteWorkspaceFlow>,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init);
        let injected: Arc<dyn RemoteWorkspaceFlowBackend> = backend;
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let panel = cx.new(|cx| NewWorkspacePanel::new(window, cx));
            let flow = cx.new(|cx| RemoteWorkspaceFlow::new(injected, window, cx));
            cx.subscribe_in(
                &panel,
                window,
                |harness: &mut ReplacementHarness,
                 _,
                 event: &NewWorkspacePanelEvent,
                 window,
                 cx| {
                    if matches!(
                        event,
                        NewWorkspacePanelEvent::SourceSelected(NewWorkspaceSource::RemoteProject)
                    ) {
                        harness.source_callbacks += 1;
                        harness.transfer_remote(window, cx);
                    }
                },
            )
            .detach();
            ReplacementHarness {
                panel,
                flow,
                prior_focus: cx.focus_handle(),
                source_callbacks: 0,
                successful_transfers: 0,
            }
        });
        let (panel, flow): (Entity<NewWorkspacePanel>, Entity<RemoteWorkspaceFlow>) = harness
            .read_with(cx, |harness, _| {
                (harness.panel.clone(), harness.flow.clone())
            });
        cx.update(|window, cx| {
            harness.read(cx).prior_focus.focus(window);
            panel.update(cx, |panel, cx| panel.open(window, cx));
        });
        cx.run_until_parked();
        (harness, panel, flow, cx)
    }

    fn remote_account() -> RemoteWorkspaceAccount {
        RemoteWorkspaceAccount::new(
            "tester".to_owned(),
            remote_identity("/home/tester"),
            "/bin/zsh".to_owned(),
        )
        .unwrap()
    }

    fn remote_identity(value: &str) -> RemoteDirectoryIdentity {
        RemoteDirectoryIdentity::new(value.to_owned()).unwrap()
    }

    fn destination(value: &str) -> SshDestination {
        SshDestination::new(value.to_owned()).unwrap()
    }

    fn alias(value: &str) -> SshHostAlias {
        SshHostAlias::new(value.to_owned()).unwrap()
    }

    fn managed_host(value: &str) -> ManagedSshHost {
        ManagedSshHost::new(
            value.to_owned(),
            format!("{value}.example"),
            None,
            None,
            None,
        )
        .unwrap()
    }

    fn session(closes: &Arc<AtomicUsize>) -> RemoteWorkspaceConnectedSession {
        RemoteWorkspaceConnectedSession::new(
            Box::new(CountingOwner {
                closes: Arc::clone(closes),
                observer_takes: None,
            }),
            Arc::new(ReadyRemoteProvider),
        )
    }

    fn session_with_observer_takes(
        closes: &Arc<AtomicUsize>,
        observer_takes: &Arc<AtomicUsize>,
    ) -> RemoteWorkspaceConnectedSession {
        RemoteWorkspaceConnectedSession::new(
            Box::new(CountingOwner {
                closes: Arc::clone(closes),
                observer_takes: Some(Arc::clone(observer_takes)),
            }),
            Arc::new(ReadyRemoteProvider),
        )
    }

    fn pinning_completion(
        alias: Option<crate::ssh::alias_usage::ActiveSshAliasLease>,
        acquisition_fails: bool,
        closes: &Arc<AtomicUsize>,
    ) -> RemoteWorkspaceFlowCompletion {
        RemoteWorkspaceFlowCompletion::for_test(
            RemoteWorkspaceConnectedSession::new(
                Box::new(AliasPinningOwner {
                    alias,
                    acquisition_fails,
                    closes: Arc::clone(closes),
                }),
                Arc::new(ReadyRemoteProvider),
            ),
            destination("work"),
            RemoteWorkspaceDirectory::new("~/src".to_owned()).unwrap(),
            remote_identity("/home/tester/src"),
            remote_account(),
            Arc::new(|| Err(crate::terminal::RemoteChannelUnavailable)),
            ControlConnectionObserver::closed(),
        )
    }

    fn select_destination(
        flow: &Entity<RemoteWorkspaceFlow>,
        value: &str,
        cx: &mut VisualTestContext,
    ) {
        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                flow.reduce_host_event(
                    &SshHostPickerEvent::SelectDestination(destination(value)),
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(selector).unwrap();
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn remote_source_replacement_should_transfer_focus_escape_and_reopen_exactly_once(
        cx: &mut TestAppContext,
    ) {
        let backend = FakeBackend::new([]);
        let (harness, panel, flow, cx) = replacement_window(backend, cx);
        assert!(cx.update(|window, cx| panel.read(cx).input_is_focused(window, cx)));

        cx.simulate_keystrokes("down down enter");
        cx.run_until_parked();

        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::HostSelection
        );
        assert!(!panel.read_with(cx, |panel, _| panel.blocks_terminal_input()));
        assert!(flow.read_with(cx, |flow, _| flow.blocks_terminal_input()));
        assert!(cx.update(|window, cx| flow.read(cx).owns_first_responder(window, cx)));
        assert!(!cx.update(|window, cx| harness.read(cx).prior_focus.is_focused(window)));
        assert_eq!(
            harness.read_with(cx, |harness, _| (
                harness.source_callbacks,
                harness.successful_transfers,
            )),
            (1, 1)
        );

        cx.update(|window, cx| {
            harness.update(cx, |harness, cx| {
                assert!(!harness.transfer_remote(window, cx));
            });
        });
        assert_eq!(
            harness.read_with(cx, |harness, _| harness.successful_transfers),
            1
        );
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::HostSelection
        );

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::Cancelled
        );
        assert!(!flow.read_with(cx, |flow, _| flow.blocks_terminal_input()));
        assert!(cx.update(|window, cx| harness.read(cx).prior_focus.is_focused(window)));

        cx.update(|window, cx| panel.update(cx, |panel, cx| panel.open(window, cx)));
        cx.run_until_parked();
        cx.simulate_keystrokes("down down enter");
        cx.run_until_parked();
        assert_eq!(
            harness.read_with(cx, |harness, _| (
                harness.source_callbacks,
                harness.successful_transfers,
            )),
            (2, 2)
        );
        assert!(cx.update(|window, cx| flow.read(cx).owns_first_responder(window, cx)));
    }

    #[gpui::test]
    fn modal_blocker_should_retain_replacement_until_host_picker_can_take_focus(
        cx: &mut TestAppContext,
    ) {
        let backend = FakeBackend::new([]);
        let (harness, panel, flow, cx) = replacement_window(backend, cx);

        cx.update(|window, cx| {
            harness.update(cx, |harness, cx| {
                let replacement = harness
                    .panel
                    .update(cx, |panel, cx| panel.dismiss_for_replacement(window, cx))
                    .unwrap();
                Alert::new(
                    ModalId::new("remote-workspace-focus-blocker"),
                    "Focus blocker",
                    "Focus Blocker",
                    "Wait before opening the Host Picker.",
                    vec![ModalAction::new(
                        AcknowledgeAction::Acknowledge,
                        "OK",
                        ModalActionRole::Cancel,
                        "remote-workspace-focus-blocker-ok",
                    )],
                )
                .present(window, cx, |_, _| {})
                .unwrap();
                assert!(harness.flow.update(cx, |flow, cx| {
                    flow.open_replacing(replacement, window, cx)
                }));
            });
        });
        cx.run_until_parked();

        assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::HostSelection
        );
        assert!(flow.read_with(cx, |flow, _| flow.blocks_terminal_input()));
        assert!(!panel.read_with(cx, |panel, _| panel.blocks_terminal_input()));
        assert!(!cx.update(|window, cx| harness.read(cx).prior_focus.is_focused(window)));

        click("modal-action-remote-workspace-focus-blocker-ok", cx);

        assert!(!cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
        assert!(cx.update(|window, cx| flow.read(cx).owns_first_responder(window, cx)));
        assert!(!cx.update(|window, cx| harness.read(cx).prior_focus.is_focused(window)));
    }

    #[gpui::test]
    fn connection_progress_should_retain_one_modal_through_authentication_and_open_picker(
        cx: &mut TestAppContext,
    ) {
        let closes = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = async_channel::bounded(1);
        let task = cx.update(|cx| {
            cx.background_executor()
                .spawn(async move { receiver.recv().await.unwrap() })
        });
        let backend = FakeBackend::new([task]);
        let (_, flow, _, cx) = flow_window(Arc::clone(&backend), cx);

        select_destination(&flow, "work", cx);

        let (stage, history, presentation) = flow.read_with(cx, |flow, _| {
            (
                flow.stage,
                flow.observed_progress.clone(),
                flow.progress
                    .as_ref()
                    .map(ProgressDialogHandle::presentation_id),
            )
        });
        assert_eq!(
            stage,
            RemoteWorkspaceFlowStage::Connecting(RemoteWorkspaceConnectionProgress::Authenticating)
        );
        assert_eq!(
            history,
            [
                RemoteWorkspaceConnectionProgress::CheckingCompatibility,
                RemoteWorkspaceConnectionProgress::Connecting,
                RemoteWorkspaceConnectionProgress::Authenticating,
            ]
        );
        assert!(presentation.is_some());
        assert!(cx.debug_bounds("modal-progress-status").is_some());

        sender.try_send(Ok(session(&closes))).unwrap();
        cx.run_until_parked();

        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::DirectorySelection
        );
        assert!(flow.read_with(cx, |flow, _| flow.progress.is_none()));
        assert_eq!(backend.discoveries.load(Ordering::SeqCst), 1);
        assert_eq!(closes.load(Ordering::SeqCst), 0);
    }

    #[gpui::test]
    fn add_save_and_connect_then_edit_should_preserve_flow_ownership(cx: &mut TestAppContext) {
        let closes = Arc::new(AtomicUsize::new(0));
        let backend = FakeBackend::new([Task::ready(Ok(session(&closes)))]);
        backend.push_save(Ok(()));
        backend.push_save(Ok(()));
        let (_, flow, _, cx) = flow_window(Arc::clone(&backend), cx);

        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                flow.reduce_host_event(
                    &SshHostPickerEvent::RequestAddHost(destination("work")),
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::AddingHost
        );
        assert!(cx.debug_bounds("managed-ssh-host-alias").is_some());
        cx.simulate_input("work");
        cx.simulate_keystrokes("tab");
        cx.simulate_input("server.example");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::DirectorySelection
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::HostSelection
        );
        assert_eq!(closes.load(Ordering::SeqCst), 0);

        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                flow.reduce_host_event(
                    &SshHostPickerEvent::RequestEditHost(alias("work")),
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::EditingHost
        );
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::HostSelection
        );
        let records = backend.state.lock().unwrap().save_records.clone();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].host.alias().as_str(), "work");
        assert_eq!(records[0].editing_alias, None);
        assert_eq!(records[1].editing_alias, Some(alias("work")));
    }

    #[gpui::test]
    fn active_managed_host_should_disable_delete_and_confirmed_delete_should_refresh(
        cx: &mut TestAppContext,
    ) {
        let backend = FakeBackend::new([]);
        backend.insert_managed(managed_host("work"));
        backend.set_active(alias("work"), true);
        backend.push_delete(Ok(()));
        let (_, flow, _, cx) = flow_window(Arc::clone(&backend), cx);

        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                flow.reduce_host_event(
                    &SshHostPickerEvent::RequestDeleteHost(alias("work")),
                    window,
                    cx,
                );
            });
        });
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::HostSelection
        );

        backend.set_active(alias("work"), false);
        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                flow.reduce_host_event(
                    &SshHostPickerEvent::RequestDeleteHost(alias("work")),
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::DeleteConfirmation
        );
        click("modal-action-remote-workspace-delete-confirm", cx);
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::HostSelection
        );
        assert_eq!(
            backend.state.lock().unwrap().delete_records,
            [alias("work")]
        );
    }

    #[gpui::test]
    fn edit_save_should_recheck_active_use_and_retain_entered_values(cx: &mut TestAppContext) {
        let backend = FakeBackend::new([]);
        backend.insert_managed(managed_host("work"));
        let (_, flow, _, cx) = flow_window(Arc::clone(&backend), cx);
        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                flow.reduce_host_event(
                    &SshHostPickerEvent::RequestEditHost(alias("work")),
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();
        backend.set_active(alias("work"), true);

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::EditingHost
        );
        assert!(cx.debug_bounds("managed-ssh-host-backend-error").is_some());
        assert!(backend.state.lock().unwrap().save_records.is_empty());
        backend.set_active(alias("work"), false);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            backend.state.lock().unwrap().save_records[0]
                .host
                .alias()
                .as_str(),
            "work"
        );
    }

    #[gpui::test]
    fn delete_commit_should_recheck_active_use_before_backend_mutation(cx: &mut TestAppContext) {
        let backend = FakeBackend::new([]);
        backend.insert_managed(managed_host("work"));
        let (_, flow, _, cx) = flow_window(Arc::clone(&backend), cx);
        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                flow.reduce_host_event(
                    &SshHostPickerEvent::RequestDeleteHost(alias("work")),
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();
        backend.set_active(alias("work"), true);

        click("modal-action-remote-workspace-delete-confirm", cx);

        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::DeletingHost
        );
        assert!(
            cx.debug_bounds("modal-action-remote-workspace-delete-error-ok")
                .is_some()
        );
        let state = backend.state.lock().unwrap();
        assert!(state.delete_records.is_empty());
        assert!(state.managed.contains_key(&alias("work")));
    }

    #[gpui::test]
    fn delete_failure_should_show_fixed_recovery_and_return_to_host_selection(
        cx: &mut TestAppContext,
    ) {
        let backend = FakeBackend::new([]);
        backend.insert_managed(managed_host("work"));
        backend.push_delete(Err(RemoteWorkspaceFlowBackendError::DeleteFailed));
        let (_, flow, _, cx) = flow_window(backend, cx);

        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                flow.reduce_host_event(
                    &SshHostPickerEvent::RequestDeleteHost(alias("work")),
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();
        click("modal-action-remote-workspace-delete-confirm", cx);
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::DeletingHost
        );
        assert!(
            cx.debug_bounds("modal-action-remote-workspace-delete-error-ok")
                .is_some()
        );

        click("modal-action-remote-workspace-delete-error-ok", cx);
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::HostSelection
        );
    }

    #[test]
    fn configured_completion_should_keep_a_workspace_alias_pin_after_session_drop() {
        let registry = crate::ssh::alias_usage::ActiveSshAliasRegistry::default();
        let alias = alias("work");
        let connection = registry.acquire(alias.clone()).unwrap();
        let closes = Arc::new(AtomicUsize::new(0));
        let completion = pinning_completion(Some(connection), false, &closes);

        let workspace = completion.acquire_workspace_alias_pin().unwrap().unwrap();
        drop(completion);

        assert!(registry.is_active(&alias));
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        drop(workspace);
        assert!(!registry.is_active(&alias));
    }

    #[test]
    fn unconfigured_completion_should_not_create_a_workspace_alias_pin() {
        let closes = Arc::new(AtomicUsize::new(0));
        let completion = pinning_completion(None, false, &closes);

        assert!(completion.acquire_workspace_alias_pin().unwrap().is_none());
        drop(completion);
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn duplicate_workspace_alias_pins_should_hold_independent_registry_counts() {
        let registry = crate::ssh::alias_usage::ActiveSshAliasRegistry::default();
        let alias = alias("work");
        let connection = registry.acquire(alias.clone()).unwrap();
        let closes = Arc::new(AtomicUsize::new(0));
        let completion = pinning_completion(Some(connection), false, &closes);
        let first = completion.acquire_workspace_alias_pin().unwrap().unwrap();
        let second = completion.acquire_workspace_alias_pin().unwrap().unwrap();

        drop(completion);
        drop(first);
        assert!(registry.is_active(&alias));
        drop(second);
        assert!(!registry.is_active(&alias));
    }

    #[test]
    fn reconnect_generation_swap_should_keep_the_workspace_alias_continuously_pinned() {
        let registry = crate::ssh::alias_usage::ActiveSshAliasRegistry::default();
        let alias = alias("work");
        let closes = Arc::new(AtomicUsize::new(0));
        let first_connection = registry.acquire(alias.clone()).unwrap();
        let first = pinning_completion(Some(first_connection), false, &closes);
        let workspace = first.acquire_workspace_alias_pin().unwrap().unwrap();
        drop(first);

        let replacement_connection = registry.acquire(alias.clone()).unwrap();
        let replacement = pinning_completion(Some(replacement_connection), false, &closes);
        assert!(registry.is_active(&alias));
        drop(replacement);
        assert!(registry.is_active(&alias));
        drop(workspace);
        assert!(!registry.is_active(&alias));
        assert_eq!(closes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failed_workspace_alias_pin_acquisition_should_leave_completion_ownership_intact() {
        let registry = crate::ssh::alias_usage::ActiveSshAliasRegistry::default();
        let alias = alias("work");
        let connection = registry.acquire(alias.clone()).unwrap();
        let closes = Arc::new(AtomicUsize::new(0));
        let completion = pinning_completion(Some(connection), true, &closes);

        let Err(error) = completion.acquire_workspace_alias_pin() else {
            panic!("the injected pin acquisition failure must remain typed");
        };

        assert!(registry.is_active(&alias));
        assert_eq!(closes.load(Ordering::SeqCst), 0);
        assert!(!format!("{error:?}").contains(alias.as_str()));
        drop(completion);
        assert!(!registry.is_active(&alias));
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn detailed_connection_content_should_use_a_fixed_heading_and_sanitized_output() {
        let detail = TransientSshErrorOutput::from_untrusted_bytes(
            b"ssh: connect failed\x1b[31m\ntry another route",
        )
        .unwrap();
        let error = RemoteWorkspaceFlowBackendError::ConnectionFailedWithDetail(detail);

        let content = connection_error_content(Some(&error));

        assert_eq!(
            content.detail.as_deref(),
            Some("OpenSSH reported:\nssh: connect failed [31m try another route")
        );
        assert_eq!(
            content.message,
            "SpaceTerm couldn\u{2019}t establish the remote connection."
        );
    }

    #[test]
    fn generic_connection_content_should_not_retain_a_detail_region() {
        let content =
            connection_error_content(Some(&RemoteWorkspaceFlowBackendError::ConnectionFailed));

        assert_eq!(content.detail, None);
    }

    #[gpui::test]
    fn retry_should_clear_prior_openssh_detail_and_keep_default_modal_focus(
        cx: &mut TestAppContext,
    ) {
        let detail =
            TransientSshErrorOutput::from_untrusted_bytes(b"first failure detail").unwrap();
        let backend = FakeBackend::new([
            Task::ready(Err(
                RemoteWorkspaceFlowBackendError::ConnectionFailedWithDetail(detail),
            )),
            Task::ready(Err(RemoteWorkspaceFlowBackendError::ConnectionFailed)),
        ]);
        let (_, flow, _, cx) = flow_window(Arc::clone(&backend), cx);

        select_destination(&flow, "work", cx);
        assert!(cx.debug_bounds("modal-alert-detail-2").is_some());
        assert!(flow.read_with(cx, |flow, _| flow.connection_error.is_none()));

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        assert_eq!(backend.state.lock().unwrap().connect_records.len(), 2);
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::ConnectionError
        );
        assert!(
            cx.debug_bounds("modal-action-remote-workspace-retry")
                .is_some()
        );
        assert!(
            cx.debug_bounds("modal-alert-detail-4").is_none(),
            "the second connection Alert retained the predecessor detail"
        );
    }

    #[gpui::test]
    fn failed_connection_should_retry_without_reopening_host_picker(cx: &mut TestAppContext) {
        let closes = Arc::new(AtomicUsize::new(0));
        let backend = FakeBackend::new([
            Task::ready(Err(RemoteWorkspaceFlowBackendError::ConnectionFailed)),
            Task::ready(Ok(session(&closes))),
        ]);
        let (_, flow, _, cx) = flow_window(Arc::clone(&backend), cx);

        select_destination(&flow, "work", cx);
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::ConnectionError
        );
        assert!(
            cx.debug_bounds("modal-action-remote-workspace-retry")
                .is_some()
        );
        click("modal-action-remote-workspace-retry", cx);

        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::DirectorySelection
        );
        assert_eq!(backend.state.lock().unwrap().connect_records.len(), 2);
        assert_eq!(backend.discoveries.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    fn connection_error_back_should_retain_host_then_cancel_should_finish_once(
        cx: &mut TestAppContext,
    ) {
        let backend = FakeBackend::new([
            Task::ready(Err(RemoteWorkspaceFlowBackendError::IncompatibleServer)),
            Task::ready(Err(RemoteWorkspaceFlowBackendError::ConnectionFailed)),
        ]);
        let (_, flow, events, cx) = flow_window(Arc::clone(&backend), cx);

        select_destination(&flow, "work", cx);
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::ConnectionError
        );
        click("modal-action-remote-workspace-back-to-hosts", cx);
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::HostSelection
        );

        select_destination(&flow, "work", cx);
        click("modal-action-remote-workspace-error-cancel", cx);
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::Cancelled
        );
        assert_eq!(events.borrow().cancelled, 1);
    }

    #[gpui::test]
    fn authentication_cancel_should_return_to_retained_host_without_error_alert(
        cx: &mut TestAppContext,
    ) {
        let backend = FakeBackend::new([Task::ready(Err(
            RemoteWorkspaceFlowBackendError::AuthenticationCancelled,
        ))]);
        let (_, flow, events, cx) = flow_window(backend, cx);

        select_destination(&flow, "work", cx);

        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::HostSelection
        );
        assert!(flow.read_with(cx, |flow, cx| flow.host_picker.read(cx).is_open()));
        assert!(
            cx.debug_bounds("modal-action-remote-workspace-retry")
                .is_none()
        );
        assert_eq!(events.borrow().cancelled, 0);
    }

    #[gpui::test]
    fn pending_cancel_should_emit_once_and_stale_connection_should_close(cx: &mut TestAppContext) {
        let closes = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = async_channel::bounded(1);
        let task = cx.update(|cx| {
            cx.background_executor()
                .spawn(async move { receiver.recv().await.unwrap() })
        });
        let backend = FakeBackend::new([task]);
        let (_, flow, events, cx) = flow_window(backend, cx);

        select_destination(&flow, "work", cx);
        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| flow.cancel_flow(window, cx));
        });
        cx.run_until_parked();
        assert_eq!(events.borrow().cancelled, 1);
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::Cancelled
        );

        sender.try_send(Ok(session(&closes))).unwrap();
        cx.run_until_parked();
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert_eq!(events.borrow().cancelled, 1);
    }

    #[gpui::test]
    fn replacing_progress_should_cancel_and_close_delayed_success(cx: &mut TestAppContext) {
        let closes = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = async_channel::bounded(1);
        let task = cx.update(|cx| {
            cx.background_executor()
                .spawn(async move { receiver.recv().await.unwrap() })
        });
        let backend = FakeBackend::new([task]);
        let (_, flow, events, cx) = flow_window(backend, cx);
        select_destination(&flow, "work", cx);

        cx.update(|window, cx| {
            flow.update(cx, |_, cx| {
                let replacement = Alert::new(
                    ModalId::new("remote-workspace-test-replacement"),
                    "Replacement",
                    "Replacement",
                    "Replacement",
                    vec![ModalAction::new(
                        AcknowledgeAction::Acknowledge,
                        "OK",
                        ModalActionRole::Cancel,
                        "remote-workspace-test-replacement-ok",
                    )],
                )
                .replace_active(window, cx, |_, _| {}, |_, _| {});
                assert!(replacement.is_ok());
            });
        });
        cx.run_until_parked();
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::Cancelled
        );
        assert_eq!(events.borrow().cancelled, 1);

        sender.try_send(Ok(session(&closes))).unwrap();
        cx.run_until_parked();
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert_eq!(events.borrow().cancelled, 1);
    }

    #[gpui::test]
    fn exact_completion_should_transfer_live_session_and_remote_metadata_once(
        cx: &mut TestAppContext,
    ) {
        let closes = Arc::new(AtomicUsize::new(0));
        let backend = FakeBackend::new([Task::ready(Ok(session(&closes)))]);
        let (_, flow, events, cx) = flow_window(backend, cx);
        select_destination(&flow, "deploy@work", cx);
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::DirectorySelection
        );

        let selection = RemoteWorkspaceSelection::new(
            RemoteWorkspaceDirectory::new("~/src".to_owned()).unwrap(),
            remote_identity("/home/tester/src"),
            remote_account(),
        );
        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                let generation = flow.action_generation;
                flow.reduce_remote_picker_event(
                    generation,
                    &RemoteWorkspacePickerEvent::Confirmed(selection),
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();

        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::AwaitingActivation
        );
        let handle = events.borrow().completions[0].clone();
        let completion = handle.take().unwrap();
        assert_eq!(completion.destination().as_str(), "deploy@work");
        assert_eq!(completion.directory().as_str(), "~/src");
        assert_eq!(completion.physical_directory().as_str(), "/home/tester/src");
        assert_eq!(completion.remote_user(), "tester");
        assert_eq!(completion.remote_home_identity().as_str(), "/home/tester");
        assert_eq!(completion.login_shell(), "/bin/zsh");
        let _ = completion.session();
        assert!(handle.take().is_none());
        assert_eq!(closes.load(Ordering::SeqCst), 0);
        let (session, destination, directory, physical, account, terminal_channels, lifecycle) =
            completion.into_parts();
        assert_eq!(destination.as_str(), "deploy@work");
        assert_eq!(directory.as_str(), "~/src");
        assert_eq!(physical.as_str(), "/home/tester/src");
        assert_eq!(account.login_shell().as_str(), "/bin/zsh");
        assert!(terminal_channels.is_ready());
        let _ = lifecycle;
        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                assert!(flow.activation_succeeded(&handle, window, cx));
            });
        });
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::Completed
        );
        drop(session);
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    fn failed_workspace_creation_should_restore_picker_and_deduplicate_completion(
        cx: &mut TestAppContext,
    ) {
        let closes = Arc::new(AtomicUsize::new(0));
        let backend = FakeBackend::new([Task::ready(Ok(session(&closes)))]);
        let (_, flow, events, cx) = flow_window(backend, cx);
        select_destination(&flow, "work", cx);
        let selection = RemoteWorkspaceSelection::new(
            RemoteWorkspaceDirectory::new("~/src".to_owned()).unwrap(),
            remote_identity("/home/tester/src"),
            remote_account(),
        );

        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                let generation = flow.action_generation;
                flow.reduce_remote_picker_event(
                    generation,
                    &RemoteWorkspacePickerEvent::Confirmed(selection.clone()),
                    window,
                    cx,
                );
                flow.reduce_remote_picker_event(
                    generation,
                    &RemoteWorkspacePickerEvent::Confirmed(selection.clone()),
                    window,
                    cx,
                );
            });
        });
        assert_eq!(events.borrow().completions.len(), 1);
        let first = events.borrow().completions[0].clone();
        let returned = first.take().unwrap();
        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                assert!(flow.activation_failed(&first, returned, window, cx).is_ok());
            });
        });
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::DirectorySelection
        );
        assert_eq!(closes.load(Ordering::SeqCst), 0);

        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                let generation = flow.action_generation;
                flow.reduce_remote_picker_event(
                    generation,
                    &RemoteWorkspacePickerEvent::Confirmed(selection),
                    window,
                    cx,
                );
            });
        });
        assert_eq!(events.borrow().completions.len(), 2);
        let second = events.borrow().completions[1].clone();
        let completion = second.take().unwrap();
        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                assert!(flow.activation_succeeded(&second, window, cx));
            });
        });
        drop(completion);
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    fn back_should_reuse_same_connected_destination_and_ignore_stale_picker_events(
        cx: &mut TestAppContext,
    ) {
        let closes = Arc::new(AtomicUsize::new(0));
        let backend = FakeBackend::new([Task::ready(Ok(session(&closes)))]);
        let (_, flow, _, cx) = flow_window(Arc::clone(&backend), cx);
        select_destination(&flow, "work", cx);
        let stale_generation = flow.read_with(cx, |flow, _| flow.action_generation);

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        select_destination(&flow, "work", cx);
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::DirectorySelection
        );
        assert_ne!(
            flow.read_with(cx, |flow, _| flow.action_generation),
            stale_generation
        );

        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                flow.reduce_remote_picker_event(
                    stale_generation,
                    &RemoteWorkspacePickerEvent::Dismissed,
                    window,
                    cx,
                );
            });
        });
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::DirectorySelection
        );
        assert_eq!(closes.load(Ordering::SeqCst), 0);
        assert_eq!(backend.state.lock().unwrap().connect_records.len(), 1);
    }

    #[gpui::test]
    fn choosing_different_destination_after_back_should_close_retained_connection(
        cx: &mut TestAppContext,
    ) {
        let closes = Arc::new(AtomicUsize::new(0));
        let backend = FakeBackend::new([
            Task::ready(Ok(session(&closes))),
            Task::ready(Ok(session(&closes))),
        ]);
        let (_, flow, _, cx) = flow_window(Arc::clone(&backend), cx);
        select_destination(&flow, "work", cx);

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(closes.load(Ordering::SeqCst), 0);
        select_destination(&flow, "other", cx);

        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::DirectorySelection
        );
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert_eq!(backend.state.lock().unwrap().connect_records.len(), 2);
    }

    #[gpui::test]
    fn failed_activation_then_different_host_should_use_the_new_session_observer(
        cx: &mut TestAppContext,
    ) {
        let closes = Arc::new(AtomicUsize::new(0));
        let first_observer_takes = Arc::new(AtomicUsize::new(0));
        let second_observer_takes = Arc::new(AtomicUsize::new(0));
        let backend = FakeBackend::new([
            Task::ready(Ok(session_with_observer_takes(
                &closes,
                &first_observer_takes,
            ))),
            Task::ready(Ok(session_with_observer_takes(
                &closes,
                &second_observer_takes,
            ))),
        ]);
        let (_, flow, events, cx) = flow_window(backend, cx);
        let selection = RemoteWorkspaceSelection::new(
            RemoteWorkspaceDirectory::new("~/src".to_owned()).unwrap(),
            remote_identity("/home/tester/src"),
            remote_account(),
        );

        select_destination(&flow, "work", cx);
        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| flow.complete(selection.clone(), window, cx));
        });
        let first = events.borrow().completions[0].clone();
        let returned = first.take().unwrap();
        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                assert!(flow.activation_failed(&first, returned, window, cx).is_ok());
            });
        });
        assert_eq!(first_observer_takes.load(Ordering::SeqCst), 1);

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        select_destination(&flow, "other", cx);
        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| flow.complete(selection, window, cx));
        });

        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert_eq!(events.borrow().completions.len(), 2);
        assert_eq!(second_observer_takes.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    fn full_remote_picker_dismissal_should_close_retained_connection(cx: &mut TestAppContext) {
        let closes = Arc::new(AtomicUsize::new(0));
        let backend = FakeBackend::new([Task::ready(Ok(session(&closes)))]);
        let (harness, flow, events, cx) = flow_window(backend, cx);
        select_destination(&flow, "work", cx);

        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                let generation = flow.action_generation;
                flow.reduce_remote_picker_event(
                    generation,
                    &RemoteWorkspacePickerEvent::Dismissed,
                    window,
                    cx,
                );
            });
        });

        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::Cancelled
        );
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert_eq!(events.borrow().cancelled, 1);
        assert!(cx.update(|window, cx| harness.read(cx).prior_focus.is_focused(window)));
    }

    #[test]
    fn connected_session_should_close_exactly_once_on_drop() {
        let closes = Arc::new(AtomicUsize::new(0));
        drop(session(&closes));
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }
}
