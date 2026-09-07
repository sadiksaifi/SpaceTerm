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
const CONNECTION_PROGRESS_DETAIL: &str = "Authentication prompts open in a SpaceTerm dialog.";
const CONNECTION_ERROR_ID: &str = "remote-workspace-connection-error";
const OPENSSH_ERROR_DETAIL_HEADING: &str = "OpenSSH reported:";
const DELETE_CONFIRMATION_ID: &str = "remote-workspace-delete-host";
const DELETE_ERROR_ID: &str = "remote-workspace-delete-error";

/// Content-free progress phases reported by the native SSH connector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteWorkspaceConnectionProgress {
    CheckingCompatibility,
    Connecting,
    Authenticating,
}

impl RemoteWorkspaceConnectionProgress {
    pub(crate) const fn status(self) -> &'static str {
        match self {
            Self::CheckingCompatibility => "Checking remote compatibility",
            Self::Connecting => "Connecting securely",
            Self::Authenticating => "Waiting for authentication",
        }
    }
}

fn connection_progress_dialog_update(
    progress: RemoteWorkspaceConnectionProgress,
) -> ProgressDialogUpdate {
    ProgressDialogUpdate::new()
        .status(progress.status())
        .detail(Some(CONNECTION_PROGRESS_DETAIL))
        .cancellation_enabled(true)
}

/// Cloneable, bounded progress and cancellation authority passed to one connect attempt.
#[derive(Clone)]
pub(crate) struct RemoteWorkspaceConnectContext {
    progress: async_channel::Sender<RemoteWorkspaceConnectionProgress>,
    cancelled: Arc<AtomicBool>,
}

impl RemoteWorkspaceConnectContext {
    pub(crate) fn new(
        progress: async_channel::Sender<RemoteWorkspaceConnectionProgress>,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            progress,
            cancelled,
        }
    }

    pub(crate) fn report(&self, progress: RemoteWorkspaceConnectionProgress) {
        let _ = self.progress.try_send(progress);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Eq, Error, PartialEq)]
/// Actionable flow failure with authentication material and raw remote output excluded.
///
/// Connection detail, when present, is already control-free and bounded to the transient alert
/// lifetime. Its `Debug` representation remains redacted.
pub(crate) enum RemoteWorkspaceFlowBackendError {
    #[error("the managed SSH host could not be deleted")]
    DeleteFailed,
    #[error("the SSH host is already in use")]
    HostInUse,
    #[error("the remote server is incompatible")]
    IncompatibleServer,
    #[error("the required OpenSSH version is unavailable")]
    OpenSshUnavailable,
    #[error("the app-owned SSH configuration is unavailable")]
    SshConfigurationUnavailable,
    #[error("the private SSH runtime is unavailable")]
    SshRuntimeUnavailable,
    #[error("SSH authentication was cancelled")]
    AuthenticationCancelled,
    #[error("the SSH connection failed")]
    ConnectionFailed,
    #[error("the SSH connection failed")]
    ConnectionFailedWithDetail(TransientSshErrorOutput),
}

impl RemoteWorkspaceFlowBackendError {
    /// Borrows the sanitized connection tail intended only for the active failure alert.
    pub(crate) fn connection_detail(&self) -> Option<&str> {
        match self {
            Self::ConnectionFailedWithDetail(detail) => Some(detail.as_str()),
            _ => None,
        }
    }

    /// Transfers the bounded diagnostic without converting it into an inspectable string.
    pub(crate) fn into_connection_detail(self) -> Option<TransientSshErrorOutput> {
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
            Self::SshConfigurationUnavailable => "SshConfigurationUnavailable",
            Self::SshRuntimeUnavailable => "SshRuntimeUnavailable",
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
            "SpaceTerm requires OpenSSH 8.2 or newer. Install or restore the system SSH client, then retry."
        }
        Some(RemoteWorkspaceFlowBackendError::IncompatibleServer) => {
            "This host does not provide the remote capabilities SpaceTerm requires."
        }
        Some(RemoteWorkspaceFlowBackendError::SshConfigurationUnavailable) => {
            "SpaceTerm couldn\u{2019}t prepare its private SSH configuration. Check permissions for the SpaceTerm configuration folder and retry."
        }
        Some(RemoteWorkspaceFlowBackendError::SshRuntimeUnavailable) => {
            "SpaceTerm couldn\u{2019}t prepare its private SSH runtime. Check permissions for SpaceTerm\u{2019}s runtime storage and retry."
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
pub(crate) struct RemoteWorkspaceAliasPin {
    _owner: Box<dyn Send>,
}

impl RemoteWorkspaceAliasPin {
    pub(crate) fn new(owner: impl Send + 'static) -> Self {
        Self {
            _owner: Box::new(owner),
        }
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("the configured SSH alias could not be pinned for Workspace ownership")]
pub(crate) struct RemoteWorkspaceAliasPinError;

/// The opaque lifetime owner for one connected SSH control path.
///
/// This trait deliberately exposes no command or transport access to UI code. Implementations are
/// non-clone owners and must make `close` idempotent and non-blocking for the calling GPUI thread.
/// Retained background ownership remains responsible for bounded exact process, socket,
/// authentication, cancellation, and per-session alias cleanup after `close` returns.
pub(crate) trait RemoteWorkspaceSessionOwner: Send + 'static {
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
pub(crate) struct RemoteWorkspaceConnectedSession {
    owner: Option<Box<dyn RemoteWorkspaceSessionOwner>>,
    provider: Arc<dyn RemoteWorkspaceProvider + Send + Sync>,
}

impl RemoteWorkspaceConnectedSession {
    /// Creates a connected session from its singular owner and narrow utility provider.
    pub(crate) fn new(
        owner: Box<dyn RemoteWorkspaceSessionOwner>,
        provider: Arc<dyn RemoteWorkspaceProvider + Send + Sync>,
    ) -> Self {
        Self {
            owner: Some(owner),
            provider,
        }
    }

    pub(crate) fn provider(&self) -> Arc<dyn RemoteWorkspaceProvider + Send + Sync> {
        Arc::clone(&self.provider)
    }

    /// Creates a provider that requires physical revalidation before every child reservation.
    pub(crate) fn bind_terminal_channels_for_identity(
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
    pub(crate) fn take_lifecycle_observer(&mut self) -> Option<ControlConnectionObserver> {
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
pub(crate) trait RemoteWorkspaceFlowBackend: Send + Sync {
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
pub(crate) trait RemoteWorkspaceFlowBackendFactory: Send + Sync {
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
pub(crate) struct RemoteWorkspaceFlowCompletion {
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
    pub(crate) fn for_test(
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

    pub(crate) const fn destination(&self) -> &SshDestination {
        &self.destination
    }

    pub(crate) const fn directory(&self) -> &RemoteWorkspaceDirectory {
        &self.directory
    }

    pub(crate) const fn physical_directory(&self) -> &RemoteDirectoryIdentity {
        &self.physical_directory
    }

    pub(crate) const fn remote_home_identity(&self) -> &RemoteDirectoryIdentity {
        self.account.home_identity()
    }

    pub(crate) fn terminal_channels(&self) -> Arc<dyn RemoteTerminalChannelProvider> {
        Arc::clone(&self.terminal_channels)
    }

    /// Acquires the independent alias pin only when Workspace installation is ready to commit.
    ///
    /// Failure borrows no ownership from this completion, so activation can return it intact.
    pub(crate) fn acquire_workspace_alias_pin(
        &self,
    ) -> Result<Option<RemoteWorkspaceAliasPin>, RemoteWorkspaceAliasPinError> {
        self.session.acquire_workspace_alias_pin()
    }

    pub(crate) fn into_parts(
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
pub(crate) struct RemoteWorkspaceFlowCompletionHandle {
    completion: Arc<Mutex<Option<RemoteWorkspaceFlowCompletion>>>,
}

impl RemoteWorkspaceFlowCompletionHandle {
    fn new(completion: RemoteWorkspaceFlowCompletion) -> Self {
        Self {
            completion: Arc::new(Mutex::new(Some(completion))),
        }
    }

    pub(crate) fn take(&self) -> Option<RemoteWorkspaceFlowCompletion> {
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
pub(crate) enum RemoteWorkspaceFlowEvent {
    StateChanged,
    Completed(RemoteWorkspaceFlowCompletionHandle),
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteWorkspaceFlowStage {
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

struct ConnectedHost {
    destination: SshDestination,
    session: RemoteWorkspaceConnectedSession,
    retained_lifecycle: Option<ControlConnectionObserver>,
}

struct ConnectionAttempt {
    destination: SshDestination,
    cancelled: Arc<AtomicBool>,
    finished: bool,
    phase: RemoteWorkspaceConnectionProgress,
    progress: Option<ProgressDialogHandle>,
}

impl Drop for ConnectionAttempt {
    fn drop(&mut self) {
        if !self.finished {
            self.cancelled.store(true, Ordering::Release);
        }
    }
}

struct PendingActivation {
    handle: RemoteWorkspaceFlowCompletionHandle,
    picker: Option<Entity<RemoteWorkspacePicker>>,
}

impl Drop for PendingActivation {
    fn drop(&mut self) {
        drop(self.handle.take());
    }
}

enum RemoteWorkspaceFlowState {
    Idle,
    HostSelection {
        retained: Option<ConnectedHost>,
    },
    AddingHost {
        _form: Entity<SshHostForm>,
        retained: Option<ConnectedHost>,
    },
    EditingHost {
        _form: Entity<SshHostForm>,
        retained: Option<ConnectedHost>,
    },
    DeleteConfirmation {
        alert: Option<ModalPresentationHandle>,
        retained: Option<ConnectedHost>,
    },
    DeletingHost {
        alert: Option<ModalPresentationHandle>,
        retained: Option<ConnectedHost>,
    },
    Connecting(ConnectionAttempt),
    ConnectionReady {
        connection: ConnectedHost,
        phase: RemoteWorkspaceConnectionProgress,
        progress: Option<ProgressDialogHandle>,
    },
    ConnectionFailed {
        destination: SshDestination,
        error: RemoteWorkspaceFlowBackendError,
        progress: Option<ProgressDialogHandle>,
    },
    ConnectionError {
        destination: SshDestination,
        alert: Option<ModalPresentationHandle>,
    },
    DirectorySelection {
        connection: ConnectedHost,
        picker: Option<Entity<RemoteWorkspacePicker>>,
    },
    AwaitingActivation(PendingActivation),
    Completed,
    Cancelled,
}

impl RemoteWorkspaceFlowState {
    const fn stage(&self) -> RemoteWorkspaceFlowStage {
        match self {
            Self::Idle => RemoteWorkspaceFlowStage::Idle,
            Self::HostSelection { .. } => RemoteWorkspaceFlowStage::HostSelection,
            Self::AddingHost { .. } => RemoteWorkspaceFlowStage::AddingHost,
            Self::EditingHost { .. } => RemoteWorkspaceFlowStage::EditingHost,
            Self::DeleteConfirmation { .. } => RemoteWorkspaceFlowStage::DeleteConfirmation,
            Self::DeletingHost { .. } => RemoteWorkspaceFlowStage::DeletingHost,
            Self::Connecting(attempt) => RemoteWorkspaceFlowStage::Connecting(attempt.phase),
            Self::ConnectionReady { phase, .. } => RemoteWorkspaceFlowStage::Connecting(*phase),
            Self::ConnectionFailed { .. } | Self::ConnectionError { .. } => {
                RemoteWorkspaceFlowStage::ConnectionError
            }
            Self::DirectorySelection { .. } => RemoteWorkspaceFlowStage::DirectorySelection,
            Self::AwaitingActivation(_) => RemoteWorkspaceFlowStage::AwaitingActivation,
            Self::Completed => RemoteWorkspaceFlowStage::Completed,
            Self::Cancelled => RemoteWorkspaceFlowStage::Cancelled,
        }
    }

    fn take_retained_connection(&mut self) -> Option<ConnectedHost> {
        match self {
            Self::HostSelection { retained }
            | Self::AddingHost { retained, .. }
            | Self::EditingHost { retained, .. }
            | Self::DeleteConfirmation { retained, .. }
            | Self::DeletingHost { retained, .. } => retained.take(),
            _ => None,
        }
    }

    #[cfg(test)]
    fn progress(&self) -> Option<&ProgressDialogHandle> {
        match self {
            Self::Connecting(attempt) => attempt.progress.as_ref(),
            Self::ConnectionReady { progress, .. } | Self::ConnectionFailed { progress, .. } => {
                progress.as_ref()
            }
            _ => None,
        }
    }

    fn take_progress(&mut self) -> Option<ProgressDialogHandle> {
        match self {
            Self::Connecting(attempt) => attempt.progress.take(),
            Self::ConnectionReady { progress, .. } | Self::ConnectionFailed { progress, .. } => {
                progress.take()
            }
            _ => None,
        }
    }

    fn remote_picker(&self) -> Option<&Entity<RemoteWorkspacePicker>> {
        match self {
            Self::DirectorySelection { picker, .. } => picker.as_ref(),
            Self::AwaitingActivation(pending) => pending.picker.as_ref(),
            _ => None,
        }
    }

    #[cfg(test)]
    fn connection_error(&self) -> Option<&RemoteWorkspaceFlowBackendError> {
        match self {
            Self::ConnectionFailed { error, .. } => Some(error),
            _ => None,
        }
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut App) {
        if let Some(progress) = self.take_progress() {
            let _ = progress.dismiss(window, cx);
        }
        match self {
            Self::DeleteConfirmation { alert, .. }
            | Self::DeletingHost { alert, .. }
            | Self::ConnectionError { alert, .. } => {
                if let Some(alert) = alert.take() {
                    let _ = alert.dismiss(window, cx);
                }
            }
            Self::AwaitingActivation(pending) => {
                drop(pending.handle.take());
                if let Some(picker) = &pending.picker {
                    picker.update(cx, |picker, cx| picker.activation_failed(window, cx));
                }
            }
            _ => {}
        }
        if let Some(picker) = self.remote_picker() {
            picker.update(cx, |picker, cx| picker.dismiss(window, cx));
        }
    }
}

pub(crate) struct RemoteWorkspaceFlow {
    backend: Arc<dyn RemoteWorkspaceFlowBackend>,
    host_picker: Entity<SshHostPicker>,
    focus_scope: FocusHandle,
    state: RemoteWorkspaceFlowState,
    action_generation: u64,
    #[cfg(test)]
    observed_progress: Vec<RemoteWorkspaceConnectionProgress>,
    cancelled_emitted: bool,
}

impl EventEmitter<RemoteWorkspaceFlowEvent> for RemoteWorkspaceFlow {}

impl RemoteWorkspaceFlow {
    pub(crate) fn new(
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
            state: RemoteWorkspaceFlowState::Idle,
            action_generation: 0,
            #[cfg(test)]
            observed_progress: Vec::new(),
            cancelled_emitted: false,
        }
    }

    pub(crate) const fn stage(&self) -> RemoteWorkspaceFlowStage {
        self.state.stage()
    }

    pub(crate) fn owns_activation(&self, handle: &RemoteWorkspaceFlowCompletionHandle) -> bool {
        matches!(&self.state, RemoteWorkspaceFlowState::AwaitingActivation(pending)
            if pending.handle.is_same_transfer(handle) && handle.is_empty())
    }

    #[cfg(test)]
    pub(crate) fn emit_completion_for_test(
        &mut self,
        completion: RemoteWorkspaceFlowCompletion,
        cx: &mut Context<Self>,
    ) {
        let handle = RemoteWorkspaceFlowCompletionHandle::new(completion);
        self.state = RemoteWorkspaceFlowState::AwaitingActivation(PendingActivation {
            handle: handle.clone(),
            picker: None,
        });
        cx.emit(RemoteWorkspaceFlowEvent::Completed(handle));
        self.publish(cx);
    }

    #[cfg(test)]
    pub(crate) fn cancel_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_flow(window, cx);
    }

    #[cfg(test)]
    pub(crate) fn select_destination_for_test(
        &mut self,
        destination: SshDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reduce_host_event(
            &SshHostPickerEvent::SelectDestination(destination),
            window,
            cx,
        );
    }

    #[cfg(test)]
    pub(crate) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.open_with_replacement(None, window, cx)
    }

    pub(crate) fn open_replacing(
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
            self.stage(),
            RemoteWorkspaceFlowStage::Idle | RemoteWorkspaceFlowStage::Cancelled
        ) {
            return false;
        }
        self.cancelled_emitted = false;
        self.return_to_hosts();
        let blocked_by_modal = spaceterm_ui::window_modal_is_open(window, cx);
        let opened = self.host_picker.update(cx, |picker, cx| match replacement {
            Some(replacement) => picker.open_replacing(replacement, window, cx),
            None => picker.open(window, cx),
        });
        if !opened && !blocked_by_modal {
            self.state = RemoteWorkspaceFlowState::Idle;
            return false;
        }
        self.publish(cx);
        true
    }

    pub(crate) fn blocks_terminal_input(&self) -> bool {
        !matches!(
            self.stage(),
            RemoteWorkspaceFlowStage::Idle
                | RemoteWorkspaceFlowStage::Completed
                | RemoteWorkspaceFlowStage::Cancelled
        )
    }

    #[cfg(test)]
    pub(crate) fn owns_first_responder(&self, window: &Window, cx: &App) -> bool {
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
                if self.stage() == RemoteWorkspaceFlowStage::HostSelection
                    && !matches!(reason, spaceterm_ui::CommandPaletteCloseReason::Replaced)
                {
                    self.cancel_flow(window, cx);
                }
            }
            SshHostPickerEvent::SelectDestination(destination)
                if self.stage() == RemoteWorkspaceFlowStage::HostSelection =>
            {
                self.start_connection(destination.clone(), window, cx);
            }
            SshHostPickerEvent::RequestAddHost(_)
                if self.stage() == RemoteWorkspaceFlowStage::HostSelection =>
            {
                self.present_host_form(SshHostFormMode::Add, window, cx);
            }
            SshHostPickerEvent::RequestEditHost(alias)
                if self.stage() == RemoteWorkspaceFlowStage::HostSelection =>
            {
                if self.backend.host_in_active_use(alias) {
                    return;
                }
                if let Some(host) = self.backend.managed_host(alias) {
                    self.present_host_form(SshHostFormMode::Edit(host), window, cx);
                }
            }
            SshHostPickerEvent::RequestDeleteHost(alias)
                if self.stage() == RemoteWorkspaceFlowStage::HostSelection =>
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
        let form = cx.new(|cx| {
            SshHostForm::new(
                mode,
                backend,
                std::rc::Rc::new(crate::directory_selection::GpuiFileSelection),
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
        let retained = self.state.take_retained_connection();
        self.state = match stage {
            RemoteWorkspaceFlowStage::AddingHost => RemoteWorkspaceFlowState::AddingHost {
                _form: form,
                retained,
            },
            _ => RemoteWorkspaceFlowState::EditingHost {
                _form: form,
                retained,
            },
        };
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
                if self.stage() == RemoteWorkspaceFlowStage::AddingHost =>
            {
                self.host_picker
                    .update(cx, |picker, cx| picker.refresh(window, cx));
                let Ok(destination) = SshDestination::new(host.alias().as_str().to_owned()) else {
                    self.return_to_hosts();
                    self.publish(cx);
                    return;
                };
                self.start_connection(destination, window, cx);
            }
            SshHostFormEvent::Saved(_) if self.stage() == RemoteWorkspaceFlowStage::EditingHost => {
                self.return_to_hosts();
                self.host_picker
                    .update(cx, |picker, cx| picker.refresh(window, cx));
                self.publish(cx);
            }
            SshHostFormEvent::Cancelled
                if matches!(
                    self.stage(),
                    RemoteWorkspaceFlowStage::AddingHost | RemoteWorkspaceFlowStage::EditingHost
                ) =>
            {
                self.return_to_hosts();
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
        let retained = self.state.take_retained_connection();
        self.state = RemoteWorkspaceFlowState::DeleteConfirmation {
            alert: None,
            retained,
        };
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
            Ok(handle) => {
                if let RemoteWorkspaceFlowState::DeleteConfirmation { alert, .. } = &mut self.state
                {
                    *alert = Some(handle);
                }
            }
            Err(_) => self.return_to_hosts(),
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
            || self.stage() != RemoteWorkspaceFlowStage::DeleteConfirmation
        {
            return;
        }
        if let RemoteWorkspaceFlowState::DeleteConfirmation { alert, .. } = &mut self.state {
            alert.take();
        }
        if matches!(
            outcome,
            AlertOutcome::Activated {
                action_id: DeleteAction::Delete,
                ..
            }
        ) {
            if self.backend.host_in_active_use(&alias) {
                self.state = RemoteWorkspaceFlowState::DeletingHost {
                    retained: self.state.take_retained_connection(),
                    alert: None,
                };
                self.present_delete_error(RemoteWorkspaceFlowBackendError::HostInUse, window, cx);
            } else {
                self.start_delete(alias, window, cx);
            }
        } else {
            self.return_to_hosts();
            self.publish(cx);
        }
    }

    fn start_delete(&mut self, alias: SshHostAlias, window: &mut Window, cx: &mut Context<Self>) {
        self.action_generation = self.action_generation.wrapping_add(1);
        let generation = self.action_generation;
        self.state = RemoteWorkspaceFlowState::DeletingHost {
            retained: self.state.take_retained_connection(),
            alert: None,
        };
        let task = self.backend.delete_managed_host(alias);
        cx.spawn_in(window, async move |flow, cx| {
            let result = task.await;
            let _ = flow.update_in(cx, |flow, window, cx| {
                if flow.action_generation != generation
                    || flow.stage() != RemoteWorkspaceFlowStage::DeletingHost
                {
                    return;
                }
                match result {
                    Ok(()) => {
                        flow.return_to_hosts();
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
                    if flow.stage() == RemoteWorkspaceFlowStage::DeletingHost {
                        flow.return_to_hosts();
                        flow.publish(cx);
                    }
                });
            });
        });
        if let Ok(handle) = result {
            if let RemoteWorkspaceFlowState::DeletingHost { alert, .. } = &mut self.state {
                *alert = Some(handle);
            }
        } else {
            self.return_to_hosts();
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
            self.stage(),
            RemoteWorkspaceFlowStage::HostSelection
                | RemoteWorkspaceFlowStage::AddingHost
                | RemoteWorkspaceFlowStage::ConnectionError
        ) {
            return;
        }
        if matches!(&self.state, RemoteWorkspaceFlowState::HostSelection { retained: Some(connection) }
            if connection.destination == destination)
        {
            self.open_remote_picker(window, cx);
            return;
        }
        self.action_generation = self.action_generation.wrapping_add(1);
        let generation = self.action_generation;
        #[cfg(test)]
        self.observed_progress.clear();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.state = RemoteWorkspaceFlowState::Connecting(ConnectionAttempt {
            destination: destination.clone(),
            cancelled: Arc::clone(&cancelled),
            finished: false,
            phase: RemoteWorkspaceConnectionProgress::CheckingCompatibility,
            progress: None,
        });
        if !self.present_connection_progress(
            generation,
            RemoteWorkspaceConnectionProgress::CheckingCompatibility,
            window,
            cx,
        ) {
            self.fail_connecting(RemoteWorkspaceFlowBackendError::ConnectionFailed);
            self.present_connection_error(generation, window, cx);
            return;
        }
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

    fn present_connection_progress(
        &mut self,
        generation: u64,
        progress: RemoteWorkspaceConnectionProgress,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let flow = cx.weak_entity();
        let result_flow = flow.clone();
        let result_window = window.window_handle();
        let dialog = ProgressDialog::new(
            ModalId::new(CONNECTION_PROGRESS_ID),
            "Remote connection progress",
            "Connect to Remote Host",
            progress.status(),
            ProgressState::Indeterminate,
            ProgressCancellation::Cancellable(ModalAction::new(
                (),
                "Cancel",
                ModalActionRole::Cancel,
                "remote-workspace-connect-cancel",
            )),
        )
        .detail(CONNECTION_PROGRESS_DETAIL)
        .present(
            window,
            cx,
            move |_, _, cx| {
                let _ = flow.update(cx, |flow, _| {
                    if flow.action_generation == generation
                        && let RemoteWorkspaceFlowState::Connecting(attempt) = &flow.state
                    {
                        attempt.cancelled.store(true, Ordering::Release);
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
            return false;
        };
        let RemoteWorkspaceFlowState::Connecting(attempt) = &mut self.state else {
            return false;
        };
        attempt.progress = Some(progress);
        true
    }

    fn apply_connection_progress(
        &mut self,
        generation: u64,
        phase: RemoteWorkspaceConnectionProgress,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.action_generation != generation {
            return false;
        }
        let RemoteWorkspaceFlowState::Connecting(attempt) = &mut self.state else {
            return false;
        };
        attempt.phase = phase;
        #[cfg(test)]
        self.observed_progress.push(phase);
        if phase == RemoteWorkspaceConnectionProgress::Authenticating {
            if let Some(handle) = attempt.progress.take() {
                let _ = handle.dismiss(window, cx);
            }
        } else if let Some(handle) = &attempt.progress {
            let _ = handle.update(connection_progress_dialog_update(phase), window, cx);
        } else if !self.present_connection_progress(generation, phase, window, cx) {
            self.fail_connecting(RemoteWorkspaceFlowBackendError::ConnectionFailed);
            self.present_connection_error(generation, window, cx);
            return false;
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
        if self.action_generation != generation {
            return;
        }
        let RemoteWorkspaceFlowState::Connecting(attempt) = &mut self.state else {
            return;
        };
        if attempt.cancelled.load(Ordering::Acquire) {
            return;
        }
        attempt.finished = true;
        let destination = attempt.destination.clone();
        let phase = attempt.phase;
        let progress = attempt.progress.take();
        self.state = match result {
            Ok(session) => RemoteWorkspaceFlowState::ConnectionReady {
                connection: ConnectedHost {
                    destination,
                    session,
                    retained_lifecycle: None,
                },
                phase,
                progress,
            },
            Err(error) => RemoteWorkspaceFlowState::ConnectionFailed {
                destination,
                error,
                progress,
            },
        };
        match &self.state {
            RemoteWorkspaceFlowState::ConnectionReady {
                progress: Some(handle),
                ..
            } => {
                let _ = handle.complete(window, cx);
            }
            RemoteWorkspaceFlowState::ConnectionReady { .. } => self.open_remote_picker(window, cx),
            RemoteWorkspaceFlowState::ConnectionFailed {
                progress: Some(handle),
                ..
            } => {
                let _ = handle.fail(window, cx);
            }
            RemoteWorkspaceFlowState::ConnectionFailed {
                error: RemoteWorkspaceFlowBackendError::AuthenticationCancelled,
                ..
            } => self.return_to_hosts(),
            RemoteWorkspaceFlowState::ConnectionFailed { .. } => {
                self.present_connection_error(generation, window, cx)
            }
            _ => unreachable!("connection completion installs a result state"),
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
        self.state.take_progress();
        match outcome {
            ProgressDialogOutcome::Completed
                if matches!(self.state, RemoteWorkspaceFlowState::ConnectionReady { .. }) =>
            {
                self.open_remote_picker(window, cx);
            }
            ProgressDialogOutcome::Failed
                if matches!(
                    self.state,
                    RemoteWorkspaceFlowState::ConnectionFailed {
                        error: RemoteWorkspaceFlowBackendError::AuthenticationCancelled,
                        ..
                    }
                ) =>
            {
                self.return_to_hosts();
                self.publish(cx);
            }
            ProgressDialogOutcome::Failed
                if matches!(
                    self.state,
                    RemoteWorkspaceFlowState::ConnectionFailed { .. }
                ) =>
            {
                self.present_connection_error(generation, window, cx);
            }
            ProgressDialogOutcome::ProgrammaticDismissal
                if self.stage()
                    == RemoteWorkspaceFlowStage::Connecting(
                        RemoteWorkspaceConnectionProgress::Authenticating,
                    ) =>
            {
                self.publish(cx);
            }
            ProgressDialogOutcome::Cancelled { .. }
            | ProgressDialogOutcome::DeadlineExpired
            | ProgressDialogOutcome::OwnerRemoved
            | ProgressDialogOutcome::ProgrammaticDismissal
            | ProgressDialogOutcome::Replaced => self.cancel_flow(window, cx),
            ProgressDialogOutcome::Completed | ProgressDialogOutcome::Failed => {}
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
        let state = std::mem::replace(&mut self.state, RemoteWorkspaceFlowState::Idle);
        let RemoteWorkspaceFlowState::ConnectionFailed {
            destination, error, ..
        } = state
        else {
            self.state = state;
            return;
        };
        let content = connection_error_content(Some(&error));
        self.state = RemoteWorkspaceFlowState::ConnectionError {
            destination,
            alert: None,
        };
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
            Ok(handle) => {
                if let RemoteWorkspaceFlowState::ConnectionError { alert, .. } = &mut self.state {
                    *alert = Some(handle);
                }
            }
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
        if self.action_generation != generation {
            return;
        }
        let RemoteWorkspaceFlowState::ConnectionError { destination, alert } = &mut self.state
        else {
            return;
        };
        alert.take();
        let action = match outcome {
            AlertOutcome::Activated { action_id, .. } => action_id,
            AlertOutcome::Dismissed { .. } => ConnectionErrorAction::Cancel,
        };
        match action {
            ConnectionErrorAction::Retry => {
                let destination = destination.clone();
                cx.defer_in(window, move |flow, window, cx| {
                    flow.start_connection(destination, window, cx);
                });
            }
            ConnectionErrorAction::Back => {
                self.return_to_hosts();
                self.publish(cx);
            }
            ConnectionErrorAction::Cancel => self.cancel_flow(window, cx),
        }
    }

    fn open_remote_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let connection = match std::mem::replace(&mut self.state, RemoteWorkspaceFlowState::Idle) {
            RemoteWorkspaceFlowState::ConnectionReady { connection, .. } => connection,
            RemoteWorkspaceFlowState::HostSelection {
                retained: Some(connection),
            } => connection,
            state => {
                self.state = state;
                self.cancel_flow(window, cx);
                return;
            }
        };
        self.action_generation = self.action_generation.wrapping_add(1);
        let generation = self.action_generation;
        let provider = connection.session.provider();
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
            self.cancel_flow(window, cx);
            return;
        }
        self.state = RemoteWorkspaceFlowState::DirectorySelection {
            connection,
            picker: Some(picker),
        };
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
            || self.stage() != RemoteWorkspaceFlowStage::DirectorySelection
        {
            return;
        }
        match event {
            RemoteWorkspacePickerEvent::StateChanged => cx.notify(),
            RemoteWorkspacePickerEvent::BackToHost => {
                self.action_generation = self.action_generation.wrapping_add(1);
                let RemoteWorkspaceFlowState::DirectorySelection { connection, .. } =
                    std::mem::replace(&mut self.state, RemoteWorkspaceFlowState::Idle)
                else {
                    return;
                };
                self.state = RemoteWorkspaceFlowState::HostSelection {
                    retained: Some(connection),
                };
                self.host_picker
                    .update(cx, |picker, cx| picker.open(window, cx));
                self.publish(cx);
            }
            RemoteWorkspacePickerEvent::Dismissed => self.cancel_flow(window, cx),
            RemoteWorkspacePickerEvent::Confirmed(selection) => {
                self.complete(selection.clone(), window, cx)
            }
        }
    }

    fn complete(
        &mut self,
        selection: RemoteWorkspaceSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let RemoteWorkspaceFlowState::DirectorySelection { connection, picker } = &mut self.state
        else {
            return;
        };
        let terminal_channels = match connection.session.bind_terminal_channels_for_identity(
            selection.directory(),
            selection.physical_directory(),
            selection.account().login_shell(),
        ) {
            Ok(provider) => provider,
            Err(_) => {
                if let Some(picker) = picker {
                    picker.update(cx, |picker, cx| picker.activation_failed(window, cx));
                }
                self.publish(cx);
                return;
            }
        };
        let lifecycle = connection
            .retained_lifecycle
            .take()
            .or_else(|| connection.session.take_lifecycle_observer());
        let Some(lifecycle) = lifecycle else {
            if let Some(picker) = picker {
                picker.update(cx, |picker, cx| picker.activation_failed(window, cx));
            }
            self.publish(cx);
            return;
        };
        let RemoteWorkspaceFlowState::DirectorySelection { connection, picker } =
            std::mem::replace(&mut self.state, RemoteWorkspaceFlowState::Idle)
        else {
            unreachable!();
        };
        let completion = RemoteWorkspaceFlowCompletion {
            session: connection.session,
            destination: connection.destination,
            directory: selection.directory().clone(),
            physical_directory: selection.physical_directory().clone(),
            account: selection.account().clone(),
            terminal_channels,
            lifecycle,
        };
        let handle = RemoteWorkspaceFlowCompletionHandle::new(completion);
        self.state = RemoteWorkspaceFlowState::AwaitingActivation(PendingActivation {
            handle: handle.clone(),
            picker,
        });
        cx.emit(RemoteWorkspaceFlowEvent::Completed(handle));
        self.publish(cx);
    }

    /// Acknowledges that the transferred completion was installed into Workspace ownership.
    pub(crate) fn activation_succeeded(
        &mut self,
        handle: &RemoteWorkspaceFlowCompletionHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.owns_activation(handle) {
            return false;
        }
        if let Some(picker) = self.state.remote_picker() {
            picker.update(cx, |picker, cx| picker.complete_activation(window, cx));
        }
        self.state = RemoteWorkspaceFlowState::Completed;
        self.action_generation = self.action_generation.wrapping_add(1);
        self.publish(cx);
        true
    }

    /// Returns a completion whose Workspace creation failed, restoring the retained picker.
    pub(crate) fn activation_failed(
        &mut self,
        handle: &RemoteWorkspaceFlowCompletionHandle,
        completion: RemoteWorkspaceFlowCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), Box<RemoteWorkspaceFlowCompletion>> {
        if !self.owns_activation(handle) {
            return Err(Box::new(completion));
        }
        let RemoteWorkspaceFlowState::AwaitingActivation(mut pending) =
            std::mem::replace(&mut self.state, RemoteWorkspaceFlowState::Idle)
        else {
            unreachable!();
        };
        let RemoteWorkspaceFlowCompletion {
            session,
            destination,
            lifecycle,
            ..
        } = completion;
        let connection = ConnectedHost {
            session,
            destination,
            retained_lifecycle: Some(lifecycle),
        };
        let picker = pending.picker.take();
        if let Some(picker) = &picker {
            picker.update(cx, |picker, cx| picker.activation_failed(window, cx));
        }
        self.state = RemoteWorkspaceFlowState::DirectorySelection { connection, picker };
        self.publish(cx);
        Ok(())
    }

    fn cancel_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(
            self.state,
            RemoteWorkspaceFlowState::Cancelled | RemoteWorkspaceFlowState::Completed
        ) {
            return;
        }
        self.action_generation = self.action_generation.wrapping_add(1);
        let mut previous = std::mem::replace(&mut self.state, RemoteWorkspaceFlowState::Cancelled);
        if let RemoteWorkspaceFlowState::Connecting(attempt) = &previous {
            attempt.cancelled.store(true, Ordering::Release);
        }
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

    fn return_to_hosts(&mut self) {
        let retained = self.state.take_retained_connection();
        self.state = RemoteWorkspaceFlowState::HostSelection { retained };
    }

    fn fail_connecting(&mut self, error: RemoteWorkspaceFlowBackendError) {
        let RemoteWorkspaceFlowState::Connecting(attempt) = &mut self.state else {
            return;
        };
        let destination = attempt.destination.clone();
        let progress = attempt.progress.take();
        self.state = RemoteWorkspaceFlowState::ConnectionFailed {
            destination,
            error,
            progress,
        };
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        cx.emit(RemoteWorkspaceFlowEvent::StateChanged);
        cx.notify();
    }
}

impl Render for RemoteWorkspaceFlow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus_scope)
            .child(self.host_picker.clone())
            .children(self.state.remote_picker().cloned())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gpui::{
        FocusHandle, KeyDownEvent, KeyUpEvent, Keystroke, TestAppContext, VisualTestContext,
    };
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
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
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
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
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

    fn press_return(cx: &mut VisualTestContext) {
        let keystroke = Keystroke::parse("enter").unwrap_or_default();
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });
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
    fn connection_progress_should_suspend_during_authentication_and_resume_before_opening_picker(
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
                flow.stage(),
                flow.observed_progress.clone(),
                flow.state
                    .progress()
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
        assert!(presentation.is_none());

        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                let generation = flow.action_generation;
                assert!(flow.apply_connection_progress(
                    generation,
                    RemoteWorkspaceConnectionProgress::Connecting,
                    window,
                    cx,
                ));
            });
        });
        cx.run_until_parked();

        assert_eq!(
            connection_progress_dialog_update(RemoteWorkspaceConnectionProgress::Connecting),
            ProgressDialogUpdate::new()
                .status(RemoteWorkspaceConnectionProgress::Connecting.status())
                .detail(Some(CONNECTION_PROGRESS_DETAIL))
                .cancellation_enabled(true)
        );
        assert!(flow.read_with(cx, |flow, _| flow.state.progress().is_some()));

        sender.try_send(Ok(session(&closes))).unwrap();
        cx.run_until_parked();

        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::DirectorySelection
        );
        assert!(flow.read_with(cx, |flow, _| flow.state.progress().is_none()));
        assert_eq!(backend.discoveries.load(Ordering::SeqCst), 1);
        assert_eq!(closes.load(Ordering::SeqCst), 0);
    }

    #[gpui::test]
    fn authentication_finish_should_restore_progress_cancellation_before_connection_finishes(
        cx: &mut TestAppContext,
    ) {
        let (sender, receiver) = async_channel::bounded(1);
        let task = cx.update(|cx| {
            cx.background_executor()
                .spawn(async move { receiver.recv().await.unwrap() })
        });
        let backend = FakeBackend::new([task]);
        let (_, flow, events, cx) = flow_window(Arc::clone(&backend), cx);

        select_destination(&flow, "work", cx);
        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                let generation = flow.action_generation;
                assert!(flow.apply_connection_progress(
                    generation,
                    RemoteWorkspaceConnectionProgress::Authenticating,
                    window,
                    cx,
                ));
            });
        });
        cx.run_until_parked();
        assert!(flow.read_with(cx, |flow, _| flow.state.progress().is_none()));

        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                let generation = flow.action_generation;
                assert!(flow.apply_connection_progress(
                    generation,
                    RemoteWorkspaceConnectionProgress::Connecting,
                    window,
                    cx,
                ));
            });
        });
        cx.run_until_parked();
        assert_eq!(
            connection_progress_dialog_update(RemoteWorkspaceConnectionProgress::Connecting),
            ProgressDialogUpdate::new()
                .status(RemoteWorkspaceConnectionProgress::Connecting.status())
                .detail(Some(CONNECTION_PROGRESS_DETAIL))
                .cancellation_enabled(true)
        );

        click("modal-action-remote-workspace-connect-cancel", cx);
        cx.run_until_parked();

        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::Cancelled
        );
        assert!(flow.read_with(cx, |flow, _| flow.state.progress().is_none()));
        assert_eq!(events.borrow().cancelled, 1);

        sender
            .try_send(Err(RemoteWorkspaceFlowBackendError::ConnectionFailed))
            .unwrap();
        cx.run_until_parked();
    }

    #[gpui::test]
    fn application_confirmation_should_queue_while_progress_suspends_and_then_restore_it(
        cx: &mut TestAppContext,
    ) {
        let (sender, receiver) = async_channel::bounded(1);
        let task = cx.update(|cx| {
            cx.background_executor()
                .spawn(async move { receiver.recv().await.unwrap() })
        });
        let backend = FakeBackend::new([task]);
        let (_, flow, events, cx) = flow_window(Arc::clone(&backend), cx);
        let original = cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                flow.reduce_host_event(
                    &SshHostPickerEvent::SelectDestination(destination("work")),
                    window,
                    cx,
                );
            });
            let original = flow
                .read(cx)
                .state
                .progress()
                .map(ProgressDialogHandle::presentation_id)
                .unwrap();
            flow.update(cx, |_, cx| {
                Alert::new(
                    ModalId::new("ssh-confirmation-test"),
                    "Verify SSH host",
                    "Verify SSH Host",
                    "Verify the host key fingerprint before connecting.",
                    vec![
                        ModalAction::new(
                            DeleteAction::Delete,
                            "Trust & Connect",
                            ModalActionRole::Affirmative,
                            "ssh-confirmation-test-confirm",
                        ),
                        ModalAction::new(
                            DeleteAction::Cancel,
                            "Cancel",
                            ModalActionRole::Cancel,
                            "ssh-confirmation-test-cancel",
                        ),
                    ],
                )
                .present_with_lifecycle(window, cx, |_, _| {}, |_, _| {})
                .unwrap();
            });
            original
        });
        cx.run_until_parked();

        assert!(flow.read_with(cx, |flow, _| flow.state.progress().is_none()));
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::Connecting(RemoteWorkspaceConnectionProgress::Authenticating)
        );
        assert_eq!(events.borrow().cancelled, 0);

        click("modal-action-ssh-confirmation-test-cancel", cx);
        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                let generation = flow.action_generation;
                assert!(flow.apply_connection_progress(
                    generation,
                    RemoteWorkspaceConnectionProgress::Connecting,
                    window,
                    cx,
                ));
            });
        });
        cx.run_until_parked();

        let restored = flow
            .read_with(cx, |flow, _| {
                flow.state
                    .progress()
                    .map(ProgressDialogHandle::presentation_id)
            })
            .unwrap();
        assert_ne!(restored, original);
        assert_eq!(events.borrow().cancelled, 0);

        click("modal-action-remote-workspace-connect-cancel", cx);
        sender
            .try_send(Err(RemoteWorkspaceFlowBackendError::ConnectionFailed))
            .unwrap();
        cx.run_until_parked();
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

    #[test]
    fn openssh_availability_content_should_not_disclose_the_host_executable() {
        let error = RemoteWorkspaceFlowBackendError::OpenSshUnavailable;

        let content = connection_error_content(Some(&error));

        assert_eq!(
            (content.message, content.detail),
            (
                "SpaceTerm requires OpenSSH 8.2 or newer. Install or restore the system SSH client, then retry.",
                None,
            )
        );
    }

    #[test]
    fn ssh_configuration_failure_content_should_be_actionable_and_content_free() {
        let error = RemoteWorkspaceFlowBackendError::SshConfigurationUnavailable;

        let content = connection_error_content(Some(&error));

        assert_eq!(
            (content.message, content.detail, format!("{error:?}")),
            (
                "SpaceTerm couldn\u{2019}t prepare its private SSH configuration. Check permissions for the SpaceTerm configuration folder and retry.",
                None,
                "SshConfigurationUnavailable".to_owned(),
            )
        );
    }

    #[test]
    fn ssh_runtime_failure_content_should_be_actionable_and_content_free() {
        let error = RemoteWorkspaceFlowBackendError::SshRuntimeUnavailable;

        let content = connection_error_content(Some(&error));

        assert_eq!(
            (content.message, content.detail, format!("{error:?}")),
            (
                "SpaceTerm couldn\u{2019}t prepare its private SSH runtime. Check permissions for SpaceTerm\u{2019}s runtime storage and retry.",
                None,
                "SshRuntimeUnavailable".to_owned(),
            )
        );
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
        assert!(flow.read_with(cx, |flow, _| flow.state.connection_error().is_none()));

        press_return(cx);
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
            flow.update(cx, |flow, cx| {
                let generation = flow.action_generation;
                assert!(flow.apply_connection_progress(
                    generation,
                    RemoteWorkspaceConnectionProgress::Connecting,
                    window,
                    cx,
                ));
            });
        });
        cx.run_until_parked();

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
        assert_eq!(completion.remote_home_identity().as_str(), "/home/tester");
        assert!(handle.take().is_none());
        assert_eq!(closes.load(Ordering::SeqCst), 0);
        let (session, destination, directory, physical, account, terminal_channels, lifecycle) =
            completion.into_parts();
        assert_eq!(destination.as_str(), "deploy@work");
        assert_eq!(directory.as_str(), "~/src");
        assert_eq!(physical.as_str(), "/home/tester/src");
        assert_eq!(account.user(), "tester");
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
