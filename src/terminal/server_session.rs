//! Desktop presentation and fenced input for an authenticated server Pane.
mod accessibility;
mod claim;
mod control;
mod effects;
use super::server_graphics as graphics;
mod reading;
mod recovery;
mod schedule;
use super::pointer_input::{PointerInput, WheelInput};
use super::session::clipboard::ClipboardAuthority;
use super::session::*;
use super::{emulator::*, geometry::TerminalGeometry, key::*};
use crate::server_connection::{ServerConnection, ServerError};
pub(crate) use control::{ControlRoute, PaneControlRegistry, PaneControls};
use effects::ClientEffects;
use spaceterm_server::measurement::{
    self, Event as MeasurementEvent, Operation as MeasurementOperation,
};
use spaceterm_server::pane::{self, Lease, PaneSize, Reply, Request};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::{
    sync::{atomic::AtomicU64, mpsc},
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DevelopmentPaneStatus {
    Connecting,
    Restored,
    Connected,
    Takeover,
    TakeoverReconnecting,
    TakeoverFailed(ServerError),
    TakingOver,
    Reconnecting,
    SessionEnded,
    Ended,
    Failed(ServerError),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Admission {
    Pending,
    Waiting,
    Granted,
}
pub(crate) struct DevelopmentPaneState {
    status: Mutex<DevelopmentPaneStatus>,
    /// Admission settles before a hidden Pane receives its input-enabling screen.
    admission: Mutex<Admission>,
    /// Who controlled the Pane when the server last offered Take Over.
    controller: Mutex<Option<pane::control::Controller>>,
    session_dormant: AtomicBool,
    retry: AtomicBool,
    selected: AtomicBool,
    created_start: AtomicBool,
    take_over: AtomicBool,
    start_restored: AtomicBool,
    connection_recovery: Arc<Mutex<recovery::Recovery>>,
    recovery: Mutex<Option<pane::directories::Target>>,
    repair: Mutex<Option<(pane::directories::Target, std::path::PathBuf)>>,
    directory_server: Mutex<Option<ServerConnection>>,
    directory_busy: AtomicBool,
    directory_note: Mutex<Option<&'static str>>,
    epoch: AtomicU64,
    connected_epoch: AtomicU64,
    paste_unconfirmed: AtomicBool,
    interaction: Mutex<Interaction>,
    effect_authority: Arc<ClipboardAuthority>,
    hidden_input: AtomicBool,
    reading: Mutex<reading::ReadingState>,
    accessibility: Mutex<accessibility::RetainedSemantics>,
    accessibility_demand: AtomicBool,
    client_effects: Mutex<std::collections::VecDeque<ClientEffectRequest>>,
    presentable: AtomicBool,
    preparation: AtomicBool,
    wake: schedule::Wake,
}
impl DevelopmentPaneState {
    fn new() -> Self {
        Self {
            status: Mutex::new(DevelopmentPaneStatus::Connecting),
            admission: Mutex::new(Admission::Pending),
            controller: Mutex::default(),
            session_dormant: AtomicBool::new(false),
            retry: AtomicBool::new(false),
            selected: AtomicBool::new(false),
            created_start: AtomicBool::new(false),
            take_over: AtomicBool::new(false),
            start_restored: AtomicBool::new(false),
            connection_recovery: Arc::default(),
            recovery: Mutex::default(),
            repair: Mutex::default(),
            directory_server: Mutex::default(),
            directory_busy: AtomicBool::new(false),
            directory_note: Mutex::default(),
            epoch: AtomicU64::new(0),
            connected_epoch: AtomicU64::new(0),
            paste_unconfirmed: AtomicBool::new(false),
            interaction: Mutex::new(Interaction::default()),
            hidden_input: AtomicBool::new(false),
            reading: Mutex::default(),
            accessibility: Mutex::default(),
            accessibility_demand: AtomicBool::new(false),
            client_effects: Mutex::new(std::collections::VecDeque::new()),
            presentable: AtomicBool::new(true),
            preparation: AtomicBool::new(false),
            wake: schedule::Wake::default(),
            effect_authority: {
                let authority = ClipboardAuthority::default();
                authority.route();
                Arc::new(authority)
            },
        }
    }

    pub(crate) fn status(&self) -> DevelopmentPaneStatus {
        *self.status.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn startup_pending(&self) -> bool {
        *self.admission.lock().unwrap_or_else(|e| e.into_inner()) == Admission::Pending
            || self.start_unconfirmed()
    }
    fn admission(&self, admission: Admission) {
        *self.admission.lock().unwrap_or_else(|e| e.into_inner()) = admission;
    }
    /// Who controlled the Pane when the server last offered Take Over, if it ever did.
    pub(crate) fn controller(&self) -> Option<pane::control::Controller> {
        self.controller
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(crate) fn session_dormant(&self) -> bool {
        self.session_dormant.load(Ordering::Acquire)
    }
    /// What the current connection answered about interactive terminal input.
    ///
    /// Only a Connected status carries an answer: attachment records the negotiated capability
    /// before the status becomes Connected, and a focus barrier the server does not support
    /// withdraws it while Connected. The status lock is held while the capability is read,
    /// because an invalidation leaves Connected and clears the capability under that lock, so
    /// an interrupted connection reads as Pending and never as a refusal.
    pub(crate) fn interactive_input(&self) -> super::InteractiveInput {
        let status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if *status != DevelopmentPaneStatus::Connected {
            return super::InteractiveInput::Pending;
        }
        let interactive = self
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .interactive;
        drop(status);
        if interactive {
            super::InteractiveInput::Accepted
        } else {
            super::InteractiveInput::Refused
        }
    }
    pub(crate) fn connection_generation(&self) -> u64 {
        self.connected_epoch.load(Ordering::Acquire)
    }
    /// A bound creation requests one start under the creator's existing control grant.
    pub(crate) fn start_created(&self) {
        if !self.start_unconfirmed() {
            self.created_start.store(true, Ordering::Release);
            self.wake.notify();
        }
    }

    pub(crate) fn select(&self) {
        let status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if !self.start_unconfirmed()
            && (*status != DevelopmentPaneStatus::Connecting
                || *self.admission.lock().unwrap_or_else(|e| e.into_inner()) != Admission::Granted)
            && matches!(
                *status,
                DevelopmentPaneStatus::Connecting
                    | DevelopmentPaneStatus::Restored
                    | DevelopmentPaneStatus::SessionEnded
                    | DevelopmentPaneStatus::Takeover
            )
        {
            self.selected.store(true, Ordering::Release);
            self.retry.store(true, Ordering::Release);
            self.wake.notify();
        }
    }
    pub(crate) fn retry(&self) {
        self.connection_recovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retry();
        self.retry.store(true, Ordering::Release);
        self.wake.notify();
    }
    /// The original shell start may still be in flight. Retry observes it without starting again.
    pub(crate) fn start_unconfirmed(&self) -> bool {
        self.connection_recovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .start_unconfirmed()
    }
    pub(crate) fn start_restored(&self) {
        let status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if !self.start_unconfirmed()
            && matches!(
                *status,
                DevelopmentPaneStatus::Restored
                    | DevelopmentPaneStatus::SessionEnded
                    | DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
            )
            && !self.directory_busy.swap(true, Ordering::AcqRel)
        {
            *self
                .directory_note
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            self.start_restored.store(true, Ordering::Release);
            self.wake.notify();
        }
    }
    pub(crate) fn directory_progress(&self) -> (bool, Option<&'static str>) {
        (
            self.directory_busy.load(Ordering::Acquire),
            *self
                .directory_note
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }
    pub(crate) fn recovery_target(&self) -> Option<pane::directories::Target> {
        self.recovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(crate) fn directory_server(&self) -> Option<ServerConnection> {
        self.directory_server
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(crate) fn repair_directory(
        &self,
        target: pane::directories::Target,
        replacement: std::path::PathBuf,
    ) {
        if self.status() == DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
            && !self.start_unconfirmed()
            && self.recovery_target().as_ref() == Some(&target)
        {
            self.directory_busy.store(true, Ordering::Release);
            *self
                .directory_note
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            *self.repair.lock().unwrap_or_else(|e| e.into_inner()) = Some((target, replacement));
            self.wake.notify();
        }
    }
    pub(crate) fn take_over(&self) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if *status == DevelopmentPaneStatus::Takeover && !self.start_unconfirmed() {
            // This action supersedes earlier selection; a later eligible selection stays queued.
            self.selected.store(false, Ordering::Release);
            *status = DevelopmentPaneStatus::TakingOver;
            self.take_over.store(true, Ordering::Release);
            self.retry.store(true, Ordering::Release);
            self.wake.notify();
        }
    }
    /// The server offered Take Over and named who controls the Pane. Every offer replaces the
    /// last one, so the Takeover Screen follows control as it moves. A Take Over in progress
    /// stays in progress until the server answers it.
    fn offered_take_over(&self, controller: pane::control::Controller, session_ended: bool) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        self.session_dormant.store(session_ended, Ordering::Release);
        *self.controller.lock().unwrap_or_else(|e| e.into_inner()) = Some(controller);
        if *status != DevelopmentPaneStatus::TakingOver {
            *status = DevelopmentPaneStatus::Takeover;
        }
    }
    fn invalidate(&self) -> u64 {
        self.reading
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .disconnect();
        // A new connection epoch resolves Selection only against semantics it fetched itself.
        *self.accessibility.lock().unwrap_or_else(|e| e.into_inner()) =
            accessibility::RetainedSemantics::default();
        self.effect_authority.client_focus(None);
        self.client_effects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.hidden_input.store(false, Ordering::Release);
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if *status == DevelopmentPaneStatus::Connected {
            *status = DevelopmentPaneStatus::Reconnecting;
        }
        self.paste_unconfirmed.store(false, Ordering::Release);
        self.interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .disconnect();
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.connected_epoch.store(epoch, Ordering::Release);
        epoch
    }
    /// Starts a connection epoch whose focus continues above every generation this client
    /// sent for the Pane, including through presentations that preceded this one.
    /// An exhausted sequence fails closed rather than reusing a generation.
    fn attach(&self, presenter: &control::Presenter) -> Result<u64, ServerError> {
        presenter
            .with_floor(|floor| {
                let next = floor.checked_add(1).ok_or(ServerError::Unsupported)?;
                let epoch = self.invalidate();
                let mut interaction = self.interaction.lock().unwrap_or_else(|e| e.into_inner());
                interaction.focus_generation = interaction.focus_generation.max(next);
                Ok(epoch)
            })
            .unwrap_or(Err(ServerError::NotRunning))
    }
    fn connection_failed(&self, error: ServerError, has_takeover: bool) -> bool {
        // An admitted server update refuses connections only until it resolves, so the Pane
        // keeps reconnecting on its backoff in every client, whoever started the update.
        let mut retryable = matches!(
            error,
            ServerError::DirectoryUnavailable
                | ServerError::Protocol
                | ServerError::NotRunning
                | ServerError::StaleControl
                | ServerError::Controlled
                | ServerError::ActivationUnresolved
        );
        let error = if retryable
            && (error != ServerError::DirectoryUnavailable || self.start_unconfirmed())
        {
            let (cause, allowed) = self
                .connection_recovery
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .failed(error);
            retryable = allowed;
            cause
        } else {
            error
        };
        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            // A click accepted before this interruption is never replayed after it.
            self.take_over.store(false, Ordering::Release);
            self.start_restored.store(false, Ordering::Release);
            self.directory_busy.store(false, Ordering::Release);
            self.repair.lock().unwrap_or_else(|e| e.into_inner()).take();

            self.selected.store(false, Ordering::Release);
            *status = if error == ServerError::DirectoryUnavailable {
                DevelopmentPaneStatus::Failed(error)
            } else if has_takeover && retryable {
                DevelopmentPaneStatus::TakeoverReconnecting
            } else if has_takeover {
                DevelopmentPaneStatus::TakeoverFailed(error)
            } else if retryable {
                DevelopmentPaneStatus::Reconnecting
            } else {
                DevelopmentPaneStatus::Failed(error)
            };
        }
        if !retryable
            || !matches!(
                error,
                ServerError::Protocol | ServerError::NotRunning | ServerError::ActivationUnresolved
            )
        {
            self.admission(Admission::Waiting);
        }
        retryable
    }
    fn set(&self, status: DevelopmentPaneStatus) {
        let mut current = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if status != DevelopmentPaneStatus::Connected {
            self.reading
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .disconnect();
            self.effect_authority.client_focus(None);
            self.hidden_input.store(false, Ordering::Release);
            self.paste_unconfirmed.store(false, Ordering::Release);
        }
        *current = status;
    }
    fn connected(
        &self,
        epoch: u64,
        screen: ScreenMapping,
        paste_unconfirmed: bool,
    ) -> Result<(), ServerError> {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if self.epoch.load(Ordering::Acquire) != epoch {
            return Err(ServerError::Protocol);
        }
        {
            let mut interaction = self.interaction.lock().unwrap_or_else(|e| e.into_inner());
            let mapping = Some(screen).filter(|mapping| {
                interaction
                    .requested_size
                    .is_none_or(|size| size == mapping.size)
            });
            let same_geometry = interaction
                .screen
                .zip(mapping)
                .is_some_and(|(old, new)| old.epoch == new.epoch && old.size == new.size);
            if !same_geometry {
                interaction.pointer_geometry_start = mapping.map(|mapping| mapping.local);
            }
            interaction.screen = mapping;
        }
        self.connected_epoch.store(epoch, Ordering::Release);
        if paste_unconfirmed {
            self.paste_unconfirmed.store(true, Ordering::Release);
        }
        if *status != DevelopmentPaneStatus::Connected {
            let interaction = self.interaction.lock().unwrap_or_else(|e| e.into_inner());
            self.effect_authority
                .client_focus(Some(interaction.target == pane::FocusTarget::Terminal));
        }
        *status = DevelopmentPaneStatus::Connected;
        Ok(())
    }
}
#[derive(Clone)]
pub(crate) struct DevelopmentServerSessionFactory {
    server: ServerConnection,
    local_files: Option<crate::platform::local_filesystem::LocalFilesystemAuthority>,
    remote_metadata: Option<super::metadata::RemoteTerminalMetadataContext>,
    pane_id: spaceterm_server::hierarchy::Id,
    attachment: Option<spaceterm_server::attachments::AttachmentPermit>,
    control: Arc<control::PaneControl>,
    state: Arc<DevelopmentPaneState>,
}
impl DevelopmentServerSessionFactory {
    /// Presents one server Pane. Construction connects to nothing.
    pub(crate) fn for_pane(
        server: ServerConnection,
        controls: &PaneControls,
        local_files: crate::platform::local_filesystem::LocalFilesystemAuthority,
        pane_id: spaceterm_server::hierarchy::Id,
    ) -> (Self, Arc<DevelopmentPaneState>) {
        let local_files = server.is_account_local().then_some(local_files);
        Self::for_origin(server, controls, local_files, None, pane_id)
    }

    pub(crate) fn for_remote_pane(
        server: ServerConnection,
        controls: &PaneControls,
        server_display: String,
        pane_id: spaceterm_server::hierarchy::Id,
    ) -> (Self, Arc<DevelopmentPaneState>) {
        let metadata = super::metadata::RemoteTerminalMetadataContext::new(
            server_display,
            crate::domain::RemoteDirectory::home(),
        );
        Self::for_origin(server, controls, None, Some(metadata), pane_id)
    }

    fn for_origin(
        server: ServerConnection,
        controls: &PaneControls,
        local_files: Option<crate::platform::local_filesystem::LocalFilesystemAuthority>,
        remote_metadata: Option<super::metadata::RemoteTerminalMetadataContext>,
        pane_id: spaceterm_server::hierarchy::Id,
    ) -> (Self, Arc<DevelopmentPaneState>) {
        let control = controls.pane(pane_id);
        let state = Arc::new(DevelopmentPaneState {
            connection_recovery: Arc::clone(&control.recovery),
            ..DevelopmentPaneState::new()
        });
        *state
            .directory_server
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(server.clone());
        (
            Self {
                server,
                local_files,
                remote_metadata,
                pane_id,
                attachment: None,
                control,
                state: Arc::clone(&state),
            },
            state,
        )
    }
    pub(crate) fn with_attachment(
        mut self,
        permit: spaceterm_server::attachments::AttachmentPermit,
    ) -> Self {
        self.attachment = Some(permit);
        self
    }
}

enum Input {
    ClearScreenAndScrollback,
    Read(pane::reading::Operation),
    Key(KeyInput),
    Commit(String, async_channel::Sender<InputMethodOutcome>),
    CopyOrForward(InputModifiers),
    Pointer(u64, pane::Pointer),
    Wheel(u64, pane::Wheel),
    Paste(
        String,
        async_channel::Sender<Result<super::PasteRequestOutcome, String>>,
    ),
    ResolvePaste(
        PendingPaste,
        super::PasteDecision,
        async_channel::Sender<Result<super::PasteResolution, String>>,
    ),
    Resize(PaneSize),
    Style(pane::PresentationStyle),
    AccessibilitySelection(super::accessibility::AccessibilitySelectionRequest),
}
impl Input {
    fn eligible(&self, target: pane::FocusTarget) -> bool {
        match self {
            Self::Read(_) | Self::ClearScreenAndScrollback => true,
            Self::Commit(..) => matches!(
                target,
                pane::FocusTarget::Terminal | pane::FocusTarget::FocusedPane
            ),
            Self::Key(key) if key.is_input_method_commit() => matches!(
                target,
                pane::FocusTarget::Terminal | pane::FocusTarget::FocusedPane
            ),
            Self::Key(_)
            | Self::CopyOrForward(_)
            | Self::Pointer(..)
            | Self::Wheel(..)
            | Self::AccessibilitySelection(_) => target == pane::FocusTarget::Terminal,
            Self::Paste(_, reply) => target == pane::FocusTarget::Terminal && !reply.is_closed(),
            Self::ResolvePaste(pending, _, _) => {
                target == pane::FocusTarget::Terminal
                    || target
                        == (pane::FocusTarget::PasteConfirmation {
                            id: pending.wire_id,
                        })
            }
            Self::Resize(_) | Self::Style(_) => true,
        }
    }
}
struct SubmittedInput {
    epoch: u64,
    focus_generation: u64,
    command: Input,
}
impl Input {
    fn measurement_operation(&self) -> MeasurementOperation {
        match self {
            Self::Key(_) => MeasurementOperation::Key,
            Self::Commit(..) => MeasurementOperation::TextCommit,
            Self::Resize(_) => MeasurementOperation::Resize,
            _ => MeasurementOperation::Other,
        }
    }
}
#[derive(Clone, Copy)]
struct PendingPaste {
    id: super::paste::PasteConfirmationId,
    wire_id: u64,
    epoch: u64,
    focus_generation: u64,
    deadline: std::time::Instant,
}
struct ResolvingPaste {
    pending: PendingPaste,
    restored_generation: Option<u64>,
}
#[derive(Clone, Copy)]
struct ScreenMapping {
    epoch: u64,
    local: PresentationGeneration,
    wire: u64,
    selection: u64,
    size: PaneSize,
    mouse_tracking: bool,
    alternate: bool,
    alternate_scroll: bool,
}
struct Interaction {
    interactive: bool,
    reading: bool,
    focus_generation: u64,
    target: pane::FocusTarget,
    screen: Option<ScreenMapping>,
    pointer_geometry_start: Option<PresentationGeneration>,
    requested_size: Option<PaneSize>,
    application_pointer_active: bool,
    pending: Option<PendingPaste>,
    resolving: Option<ResolvingPaste>,
    paste_request: Option<(u64, u64)>,
    next_paste: u64,
}
impl Default for Interaction {
    fn default() -> Self {
        Self {
            interactive: false,
            reading: false,
            focus_generation: 1,
            target: pane::FocusTarget::Unfocused,
            screen: None,
            pointer_geometry_start: None,
            requested_size: None,
            application_pointer_active: false,
            pending: None,
            resolving: None,
            paste_request: None,
            next_paste: 0,
        }
    }
}
impl Interaction {
    fn disconnect(&mut self) {
        self.application_pointer_active = false;
        self.interactive = false;
        self.reading = false;
        self.screen = None;
        self.pointer_geometry_start = None;
        self.pending = None;
        self.resolving = None;
        self.paste_request = None;
        if matches!(self.target, pane::FocusTarget::PasteConfirmation { .. }) {
            self.target = pane::FocusTarget::Unfocused;
        }
        self.focus_generation = self.focus_generation.saturating_add(1);
    }
    fn focus(&mut self, target: pane::FocusTarget) {
        if self.target != target {
            let restore = target == pane::FocusTarget::Terminal
                && matches!(self.target, pane::FocusTarget::PasteConfirmation { .. })
                && self.resolving.is_some();
            self.focus_generation = self.focus_generation.saturating_add(1);
            self.target = target;
            self.pending = None;
            if restore {
                if let Some(resolving) = &mut self.resolving {
                    resolving.restored_generation = Some(self.focus_generation);
                }
            } else {
                self.resolving = None;
            }
        }
    }
    fn wire_focus(&self) -> (u64, pane::FocusTarget) {
        // Modal dismissal restores local focus immediately. Preserve the wire
        // authority until its already-approved paste has been resolved in order.
        if let Some(resolving) = &self.resolving
            && resolving.restored_generation == Some(self.focus_generation)
        {
            return (
                resolving.pending.focus_generation,
                pane::FocusTarget::PasteConfirmation {
                    id: resolving.pending.wire_id,
                },
            );
        }
        (self.focus_generation, self.target)
    }
    fn resolution_is_current(&self, pending: PendingPaste) -> bool {
        self.resolving.as_ref().is_some_and(|resolving| {
            resolving.pending.id == pending.id
                && resolving.pending.epoch == pending.epoch
                && (self.focus_generation == pending.focus_generation
                    || resolving.restored_generation == Some(self.focus_generation))
        })
    }
}
struct PresentationHandle {
    input: mpsc::SyncSender<SubmittedInput>,
    state: Arc<DevelopmentPaneState>,
    alive: Arc<AtomicBool>,
    size: Arc<Mutex<PaneSize>>,
    appearance: Arc<Mutex<TerminalAppearanceUpdate>>,
    geometry: Mutex<TerminalGeometry>,
}
impl Drop for PresentationHandle {
    fn drop(&mut self) {
        self.state.effect_authority.client_focus(None);
        self.alive.store(false, Ordering::Release);
        // The worker retires this presentation now rather than at its next output check.
        self.state.wake.notify();
    }
}
impl PresentationHandle {
    fn pointer_position(
        &self,
        generation: PresentationGeneration,
        position: super::pointer_input::SurfacePosition,
        continuation: bool,
    ) -> Option<(ScreenMapping, f32, f32)> {
        let interaction = self
            .state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mapping = interaction.screen?;
        let generation_matches = if continuation {
            interaction
                .pointer_geometry_start
                .is_some_and(|start| start <= generation && generation <= mapping.local)
        } else {
            generation == mapping.local
        };
        if !generation_matches || !position.x.is_finite() || !position.y.is_finite() {
            return None;
        }
        drop(interaction);
        let geometry = *self.geometry.lock().unwrap_or_else(|e| e.into_inner());
        let cell = geometry.logical_cell_size();
        let backing_cell = geometry.to_backing_position(super::geometry::LogicalPosition::new(
            cell.width,
            cell.height,
        ));
        let x = position.x * f32::from(mapping.size.cell_width) / backing_cell.x;
        let y = position.y * f32::from(mapping.size.cell_height) / backing_cell.y;
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        Some((mapping, x, y))
    }
    fn send(&self, input: Input) {
        submit_input(&self.state, &self.input, input);
    }
}

/// Tags input with the current epoch and focus generation, or discards it now.
fn submit_input(
    state: &DevelopmentPaneState,
    sender: &mpsc::SyncSender<SubmittedInput>,
    input: Input,
) -> bool {
    let current_epoch = state.epoch.load(Ordering::Acquire);
    let epoch = match &input {
        Input::ResolvePaste(pending, ..) => pending.epoch,
        Input::Pointer(epoch, _) | Input::Wheel(epoch, _) => *epoch,
        _ => current_epoch,
    };
    let connected = state.status() == DevelopmentPaneStatus::Connected;
    let mut interaction = state.interaction.lock().unwrap_or_else(|e| e.into_inner());
    let eligible = input.eligible(interaction.target)
        && match &input {
            Input::ResolvePaste(pending, ..) => interaction.resolution_is_current(*pending),
            _ => true,
        };
    let available = connected
        && epoch == current_epoch
        && (interaction.interactive
            || matches!(
                input,
                Input::Resize(_) | Input::Style(_) | Input::ClearScreenAndScrollback
            )
            || matches!(input, Input::Read(_)) && interaction.reading)
        && eligible;
    if !available {
        drop(interaction);
        discard_input(state, epoch, input);
        return false;
    }
    if let Input::Paste(_, reply) = &input {
        if interaction
            .pending
            .is_some_and(|pending| std::time::Instant::now() >= pending.deadline)
        {
            interaction.pending = None;
        }
        if interaction.paste_request.is_some()
            || interaction.pending.is_some()
            || interaction.resolving.is_some()
        {
            let _ = reply.try_send(Ok(super::PasteRequestOutcome::Rejected(
                super::paste::PasteRejection::ConfirmationPending,
            )));
            return false;
        }
        interaction.paste_request = Some((epoch, interaction.focus_generation));
    }
    let focus_generation = match &input {
        Input::ResolvePaste(pending, ..) => pending.focus_generation,
        _ => interaction.focus_generation,
    };
    let submitted = SubmittedInput {
        epoch,
        focus_generation,
        command: input,
    };
    let operation = submitted.command.measurement_operation();
    let sent = sender.try_send(submitted);
    drop(interaction);
    let accepted = sent.is_ok();
    measurement::record(MeasurementEvent::InputAdmission {
        accepted,
        operation,
    });
    if accepted {
        state.wake.notify();
    }
    if let Err(error) = sent {
        discard_input(
            state,
            epoch,
            match error {
                mpsc::TrySendError::Full(input) | mpsc::TrySendError::Disconnected(input) => {
                    input.command
                }
            },
        );
        state.invalidate();
    }
    accepted
}
fn discard_input(state: &DevelopmentPaneState, epoch: u64, input: Input) {
    match input {
        Input::Commit(_, reply) => {
            let _ = reply.try_send(InputMethodOutcome::Interrupted);
        }
        Input::Paste(_, reply) => {
            let mut interaction = state.interaction.lock().unwrap_or_else(|e| e.into_inner());
            if interaction
                .paste_request
                .is_some_and(|(submitted_epoch, _)| submitted_epoch == epoch)
            {
                interaction.paste_request = None;
            }
            let _ = reply.try_send(Ok(super::PasteRequestOutcome::Rejected(
                super::paste::PasteRejection::TerminalUnfocused,
            )));
        }
        Input::ResolvePaste(pending, _, reply) => {
            let mut interaction = state.interaction.lock().unwrap_or_else(|e| e.into_inner());
            if interaction.resolving.as_ref().is_some_and(|resolving| {
                resolving.pending.id == pending.id && resolving.pending.epoch == pending.epoch
            }) {
                interaction.resolving = None;
            }
            let _ = reply.try_send(Ok(super::PasteResolution::Stale));
        }
        _ => {}
    }
}

/// The most queued commands a Pane worker sends before it checks the screen again, so typing
/// shows as it arrives. A key batch never holds more.
const INPUT_DRAIN: usize = pane::MAX_KEY_BATCH;
const _: () = assert!(INPUT_DRAIN > 0);

struct ActiveInput {
    epoch: u64,
    lease: Lease,
    focus_sent: Option<(u64, pane::FocusTarget)>,
}
impl ActiveInput {
    fn sync_focus(
        &mut self,
        state: &DevelopmentPaneState,
        exchange: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) -> Result<(), ServerError> {
        let focus = {
            let interaction = state.interaction.lock().unwrap_or_else(|e| e.into_inner());
            if !interaction.interactive {
                return Ok(());
            }
            interaction.wire_focus()
        };
        if self.focus_sent != Some(focus) {
            match exchange(&Request::Focus {
                lease: self.lease,
                generation: focus.0,
                target: focus.1,
            }) {
                Ok(Reply::Unchanged) => self.focus_sent = Some(focus),
                Err(ServerError::Unsupported) => self.disable(state),
                Err(error) => return Err(error),
                _ => return Err(ServerError::Protocol),
            }
        }
        Ok(())
    }
    fn disable(&self, state: &DevelopmentPaneState) {
        let mut interaction = state.interaction.lock().unwrap_or_else(|e| e.into_inner());
        interaction.interactive = false;
        interaction.pending = None;
        interaction.resolving = None;
    }
    /// Sends up to [`INPUT_DRAIN`] queued commands in order before the worker next checks the
    /// screen. Where the server takes key batches, adjacent queued keys travel in one exchange,
    /// and any other command first sends the keys queued before it. It never waits for more.
    fn drain(
        &mut self,
        state: &DevelopmentPaneState,
        input: &mpsc::Receiver<SubmittedInput>,
        batches: bool,
        exchange: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
        paste_unconfirmed: &mut bool,
    ) -> Result<(), ServerError> {
        let mut keys = Vec::new();
        for _ in 0..INPUT_DRAIN {
            let Ok(submitted) = input.try_recv() else {
                break;
            };
            measurement::record(MeasurementEvent::InputDequeued {
                operation: submitted.command.measurement_operation(),
            });
            if state.epoch.load(Ordering::Acquire) != self.epoch {
                return Err(ServerError::Protocol);
            }
            if batches && matches!(submitted.command, Input::Key(_)) {
                keys.push(submitted);
                continue;
            }
            self.send_keys(state, std::mem::take(&mut keys), exchange)?;
            self.process(state, submitted, exchange, paste_unconfirmed)?;
        }
        self.send_keys(state, keys, exchange)
    }
    /// Sends queued keys as one batch, keeping only those a single key exchange would send now:
    /// this lease's epoch, the current focus generation, and a target that takes them. The
    /// server checks the whole batch before it enters any key. A batch whose outcome is
    /// unknown invalidates the connection like a single key would, and is never sent again.
    fn send_keys(
        &mut self,
        state: &DevelopmentPaneState,
        submitted: Vec<SubmittedInput>,
        exchange: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) -> Result<(), ServerError> {
        if submitted.is_empty() || state.epoch.load(Ordering::Acquire) != self.epoch {
            return Ok(());
        }
        if let Err(error) = self.sync_focus(state, exchange) {
            state.invalidate();
            return Err(error);
        }
        let (generation, keys) = {
            let interaction = state.interaction.lock().unwrap_or_else(|e| e.into_inner());
            if state.epoch.load(Ordering::Acquire) != self.epoch || !interaction.interactive {
                return Ok(());
            }
            let generation = interaction.focus_generation;
            let keys: Vec<_> = submitted
                .into_iter()
                .filter(|submitted| {
                    submitted.epoch == self.epoch
                        && submitted.focus_generation == generation
                        && submitted.command.eligible(interaction.target)
                })
                .filter_map(|submitted| match submitted.command {
                    Input::Key(key) => Some(encode_key(key)),
                    _ => None,
                })
                .collect();
            (generation, keys)
        };
        if keys.is_empty() {
            return Ok(());
        }
        match exchange(&Request::Keys {
            lease: self.lease,
            generation,
            keys,
        }) {
            Ok(Reply::Unchanged) => Ok(()),
            Ok(_) => Err(ServerError::Protocol),
            Err(ServerError::Unsupported) => {
                self.disable(state);
                Ok(())
            }
            Err(error) => {
                state.invalidate();
                Err(error)
            }
        }
    }
    /// One queue slot holds the complete bounded commit. Its UTF-8 chunks finish in order
    /// before the next command; a failed or stale chunk ends the commit without replay.
    fn write_input_method(
        &mut self,
        state: &DevelopmentPaneState,
        generation: u64,
        text: &str,
        exchange: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) -> Result<InputMethodOutcome, ServerError> {
        let mut remaining = text;
        while !remaining.is_empty() {
            {
                let interaction = state.interaction.lock().unwrap_or_else(|e| e.into_inner());
                if state.epoch.load(Ordering::Acquire) != self.epoch
                    || generation != interaction.focus_generation
                    || !interaction.interactive
                    || !matches!(
                        interaction.target,
                        pane::FocusTarget::Terminal | pane::FocusTarget::FocusedPane
                    )
                {
                    // New local focus retires the remaining input without turning a healthy
                    // transport into a connection failure or taking focus back.
                    return Ok(InputMethodOutcome::Interrupted);
                }
            }
            let mut end = remaining.len().min(pane::MAX_KEY_TEXT_BYTES);
            while !remaining.is_char_boundary(end) {
                end -= 1;
            }
            let result = exchange(&Request::Input {
                lease: self.lease,
                generation,
                input: pane::InteractiveInput::Key(encode_key(KeyInput::input_method_commit(
                    &remaining[..end],
                ))),
            });
            match result {
                Ok(Reply::Unchanged) => remaining = &remaining[end..],
                other => {
                    // A lost acknowledgement may follow a successful write. Neither that
                    // chunk nor the unsent suffix can be replayed on this or a new connection.
                    state.invalidate();
                    return Err(match other {
                        Err(error) => error,
                        _ => ServerError::Protocol,
                    });
                }
            }
        }
        Ok(InputMethodOutcome::Written)
    }

    fn process(
        &mut self,
        state: &DevelopmentPaneState,
        submitted: SubmittedInput,
        exchange: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
        paste_unconfirmed: &mut bool,
    ) -> Result<(), ServerError> {
        if submitted.epoch != self.epoch || state.epoch.load(Ordering::Acquire) != self.epoch {
            discard_input(state, submitted.epoch, submitted.command);
            return Ok(());
        }
        if let Err(error) = self.sync_focus(state, exchange) {
            state.invalidate();
            discard_input(state, submitted.epoch, submitted.command);
            return Err(error);
        }
        let valid = {
            let interaction = state.interaction.lock().unwrap_or_else(|e| e.into_inner());
            state.epoch.load(Ordering::Acquire) == self.epoch
                && (matches!(
                    submitted.command,
                    Input::Resize(_)
                        | Input::Style(_)
                        | Input::Read(_)
                        | Input::ClearScreenAndScrollback
                ) || (interaction.interactive
                    && (submitted.focus_generation == interaction.focus_generation
                        || matches!(&submitted.command, Input::ResolvePaste(pending, ..) if interaction.resolution_is_current(*pending)))
                    && submitted.command.eligible(interaction.target)))
        };
        if !valid {
            discard_input(state, submitted.epoch, submitted.command);
            return Ok(());
        }
        let generation = submitted.focus_generation;
        let input_request = |input| Request::Input {
            lease: self.lease,
            generation,
            input,
        };
        let (request, paste_reply) = match submitted.command {
            Input::Commit(text, reply) => {
                let result = self.write_input_method(state, generation, &text, exchange);
                let _ = reply.try_send(match &result {
                    Ok(outcome) => *outcome,
                    Err(_) => InputMethodOutcome::Interrupted,
                });
                return result.map(|_| ());
            }
            Input::ClearScreenAndScrollback => (
                Request::ClearScreenAndScrollback { lease: self.lease },
                None,
            ),
            Input::Read(operation) => {
                state
                    .reading
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .enqueue(operation);
                return Ok(());
            }
            Input::CopyOrForward(modifiers) => {
                state
                    .reading
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pending_copy = Some((self.epoch, generation, modifiers));
                return Ok(());
            }
            Input::Key(key) => (
                input_request(pane::InteractiveInput::Key(encode_key(key))),
                None,
            ),
            Input::Pointer(_, pointer) => (
                input_request(pane::InteractiveInput::Pointer(pointer)),
                None,
            ),
            Input::Wheel(_, wheel) => (input_request(pane::InteractiveInput::Wheel(wheel)), None),
            Input::Resize(size) => (
                Request::Resize {
                    lease: self.lease,
                    size,
                },
                None,
            ),
            Input::Style(style) => (
                Request::Style {
                    lease: self.lease,
                    style,
                },
                None,
            ),
            Input::AccessibilitySelection(request) => {
                // The server revalidates the refs; this client resolves only its own epoch's rows.
                let Some(selection) = state
                    .accessibility
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .resolve_selection(&request)
                else {
                    return Ok(());
                };
                return match exchange(&Request::AccessibilitySelection {
                    lease: self.lease,
                    focus_generation: generation,
                    selection,
                })? {
                    Reply::Reading { .. } => Ok(()),
                    _ => Err(ServerError::Protocol),
                };
            }
            Input::Paste(text, reply) => (
                input_request(pane::InteractiveInput::Paste { text }),
                Some(PasteReply::Request(reply)),
            ),
            Input::ResolvePaste(pending, decision, reply) => {
                if pending.epoch != self.epoch
                    || pending.focus_generation != generation
                    || std::time::Instant::now() >= pending.deadline
                {
                    discard_input(
                        state,
                        self.epoch,
                        Input::ResolvePaste(pending, decision, reply),
                    );
                    return Ok(());
                }
                (
                    input_request(pane::InteractiveInput::ResolvePaste {
                        id: pending.wire_id,
                        confirm: decision == super::PasteDecision::Confirm,
                    }),
                    Some(PasteReply::Resolution(reply)),
                )
            }
        };
        let result = exchange(&request);
        if let Request::Input {
            input: pane::InteractiveInput::ResolvePaste { id, .. },
            ..
        } = &request
        {
            let mut interaction = state.interaction.lock().unwrap_or_else(|e| e.into_inner());
            if interaction.resolving.as_ref().is_some_and(|resolving| {
                resolving.pending.wire_id == *id && resolving.pending.epoch == self.epoch
            }) {
                interaction.resolving = None;
            }
        }
        let uncertain_write = matches!(
            request,
            Request::Input {
                input: pane::InteractiveInput::Paste { .. }
                    | pane::InteractiveInput::ResolvePaste { confirm: true, .. },
                ..
            }
        );
        let result = match result {
            Err(ServerError::Unsupported) if matches!(request, Request::Input { .. }) => {
                self.disable(state);
                self.finish_paste_request(state, generation);
                if let Some(reply) = paste_reply {
                    reply.stale();
                }
                return Ok(());
            }
            Err(error) => {
                let refused = matches!(
                    error,
                    ServerError::StaleControl
                        | ServerError::Controlled
                        | ServerError::SessionUnavailable
                        | ServerError::Unsupported
                );
                if uncertain_write && !refused {
                    *paste_unconfirmed = true;
                }
                // Native async readers must observe the lost epoch before their
                // outcome is published, even if the UI has not polled status yet.
                state.invalidate();
                self.finish_paste_request(state, generation);
                if let Some(reply) = paste_reply {
                    if uncertain_write && !refused {
                        reply.unconfirmed();
                    } else {
                        reply.stale();
                    }
                }
                return Err(error);
            }
            Ok(reply) => reply,
        };
        let Some(reply) = paste_reply else {
            return if matches!(result, Reply::Unchanged) {
                Ok(())
            } else {
                Err(ServerError::Protocol)
            };
        };
        let Reply::Paste(outcome) = result else {
            self.finish_paste_request(state, generation);
            if uncertain_write {
                *paste_unconfirmed = true;
                reply.unconfirmed();
            } else {
                reply.stale();
            }
            return Err(ServerError::Protocol);
        };
        let mut interaction = state.interaction.lock().unwrap_or_else(|e| e.into_inner());
        if interaction.paste_request == Some((self.epoch, generation)) {
            interaction.paste_request = None;
        }
        let current = state.epoch.load(Ordering::Acquire) == self.epoch
            && interaction.focus_generation == generation;
        match reply {
            PasteReply::Request(reply) => {
                let outcome = match outcome {
                    pane::PasteOutcome::Written => super::PasteRequestOutcome::Written,
                    pane::PasteOutcome::Rejected(rejection) => {
                        super::PasteRequestOutcome::Rejected(decode_paste_rejection(rejection))
                    }
                    pane::PasteOutcome::ConfirmationRequired(confirmation) => {
                        if !current || reply.is_closed() {
                            drop(interaction);
                            let _ =
                                exchange(&input_request(pane::InteractiveInput::ResolvePaste {
                                    id: confirmation.id,
                                    confirm: false,
                                }));
                            let _ = reply.try_send(Ok(super::PasteRequestOutcome::Rejected(
                                super::paste::PasteRejection::TerminalUnfocused,
                            )));
                            return Ok(());
                        }
                        interaction.next_paste = interaction.next_paste.saturating_add(1);
                        let id = super::paste::PasteConfirmationId::new(interaction.next_paste);
                        interaction.pending = Some(PendingPaste {
                            id,
                            wire_id: confirmation.id,
                            epoch: self.epoch,
                            focus_generation: generation,
                            deadline: std::time::Instant::now() + Duration::from_secs(30),
                        });
                        super::PasteRequestOutcome::ConfirmationRequired(
                            super::paste::PasteConfirmation {
                                id,
                                byte_len: confirmation.byte_len as usize,
                                line_count: confirmation.line_count as usize,
                                risk: super::paste::PasteRisk {
                                    multiline: confirmation.multiline,
                                    control_bytes: confirmation.control_bytes,
                                    closing_fence: confirmation.closing_fence,
                                },
                            },
                        )
                    }
                    _ => {
                        drop(interaction);
                        *paste_unconfirmed = true;
                        let _ = reply.try_send(Ok(super::PasteRequestOutcome::Unconfirmed));
                        return Err(ServerError::Protocol);
                    }
                };
                if reply.try_send(Ok(outcome)).is_err()
                    && let Some(pending) = interaction.pending.take()
                {
                    drop(interaction);
                    let _ = exchange(&input_request(pane::InteractiveInput::ResolvePaste {
                        id: pending.wire_id,
                        confirm: false,
                    }));
                }
            }
            PasteReply::Resolution(reply) => {
                let resolution = match outcome {
                    pane::PasteOutcome::Written => super::PasteResolution::Written,
                    pane::PasteOutcome::Cancelled => super::PasteResolution::Cancelled,
                    pane::PasteOutcome::Stale => super::PasteResolution::Stale,
                    _ => {
                        if uncertain_write {
                            *paste_unconfirmed = true;
                            let _ = reply.try_send(Ok(super::PasteResolution::Unconfirmed));
                        } else {
                            let _ = reply.try_send(Ok(super::PasteResolution::Stale));
                        }
                        return Err(ServerError::Protocol);
                    }
                };
                let _ = reply.try_send(Ok(resolution));
            }
        }
        Ok(())
    }
    fn finish_paste_request(&self, state: &DevelopmentPaneState, generation: u64) {
        let mut interaction = state.interaction.lock().unwrap_or_else(|e| e.into_inner());
        if interaction.paste_request == Some((self.epoch, generation)) {
            interaction.paste_request = None;
        }
    }
}
enum PasteReply {
    Request(async_channel::Sender<Result<super::PasteRequestOutcome, String>>),
    Resolution(async_channel::Sender<Result<super::PasteResolution, String>>),
}
impl PasteReply {
    fn stale(self) {
        match self {
            Self::Request(reply) => {
                let _ = reply.try_send(Ok(super::PasteRequestOutcome::Rejected(
                    super::paste::PasteRejection::TerminalUnfocused,
                )));
            }
            Self::Resolution(reply) => {
                let _ = reply.try_send(Ok(super::PasteResolution::Stale));
            }
        }
    }
    fn unconfirmed(self) {
        match self {
            Self::Request(reply) => {
                let _ = reply.try_send(Ok(super::PasteRequestOutcome::Unconfirmed));
            }
            Self::Resolution(reply) => {
                let _ = reply.try_send(Ok(super::PasteResolution::Unconfirmed));
            }
        }
    }
}
fn decode_paste_rejection(rejection: pane::PasteRejection) -> super::paste::PasteRejection {
    match rejection {
        pane::PasteRejection::Empty => super::paste::PasteRejection::Empty,
        pane::PasteRejection::TooLarge { limit } => super::paste::PasteRejection::TooLarge {
            limit: limit as usize,
        },
        pane::PasteRejection::ConfirmationPending => {
            super::paste::PasteRejection::ConfirmationPending
        }
        pane::PasteRejection::TerminalUnfocused => super::paste::PasteRejection::TerminalUnfocused,
    }
}
fn pane_size(geometry: TerminalGeometry) -> PaneSize {
    let grid = geometry.grid();
    let cell = geometry.backing_cell_size();
    PaneSize {
        columns: grid.cols.max(2),
        rows: grid.rows.max(2),
        cell_width: cell.width.clamp(1, 256) as u16,
        cell_height: cell.height.clamp(1, 256) as u16,
    }
}
impl TerminalSessionFactory for DevelopmentServerSessionFactory {
    fn is_server_backed(&self) -> bool {
        true
    }
    fn allows_local_file_insertion(&self) -> bool {
        self.local_files.is_some()
    }
    fn local_file_authority(
        &self,
    ) -> Option<&crate::platform::local_filesystem::LocalFilesystemAuthority> {
        self.local_files.as_ref()
    }
    fn fallback_title(&self) -> String {
        "Terminal".into()
    }
    fn start(
        &self,
        geometry: TerminalGeometry,
        _: LocalTerminalLaunchPlan,
        appearance: TerminalAppearanceUpdate,
    ) -> Result<StartedTerminalSession, TerminalSessionError> {
        let (input, receiver) = mpsc::sync_channel(512);
        let (events, output) = async_channel::bounded(2);
        let (accessibility_sender, accessibility) = async_channel::bounded(1);
        let (effects, clipboard) = ClientEffects::new();
        let alive = Arc::new(AtomicBool::new(true));
        let measured_size = Arc::new(Mutex::new(pane_size(geometry)));
        self.state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .requested_size = Some(pane_size(geometry));
        let appearance = Arc::new(Mutex::new(appearance));
        let handle = PresentationHandle {
            input,
            state: Arc::clone(&self.state),
            alive: Arc::clone(&alive),
            size: Arc::clone(&measured_size),
            appearance: Arc::clone(&appearance),
            geometry: Mutex::new(geometry),
        };
        let factory = self.clone();
        // Ownership follows creation order, however late each worker first runs.
        let reservation = self.control.reserve();
        std::thread::Builder::new()
            .name("spaceterm-pane-connection".into())
            .spawn(move || {
                present(
                    factory,
                    (reservation, alive),
                    receiver,
                    (events, accessibility_sender),
                    measured_size,
                    appearance,
                    effects,
                );
            })
            .map_err(TerminalSessionError::from)?;
        Ok(StartedTerminalSession {
            handle: Box::new(handle),
            events: output,
            accessibility,
            clipboard,
        })
    }
}

fn exchange(
    attachment: &Option<spaceterm_server::attachments::AttachmentPermit>,
    connection: &mut pane::PaneConnection,
    request: &Request,
) -> Result<Reply, ServerError> {
    match attachment {
        Some(permit) => permit.exchange(connection, request),
        None => connection.exchange(request),
    }
}

/// Sends in this worker's turn. A retired or dismissed presentation sends nothing.
fn send(
    presenter: &control::Presenter,
    alive: &AtomicBool,
    attachment: &Option<spaceterm_server::attachments::AttachmentPermit>,
    connection: &mut pane::PaneConnection,
    request: &Request,
) -> Result<Reply, ServerError> {
    presenter.send(request, |request| {
        if alive.load(Ordering::Acquire) {
            exchange(attachment, connection, request)
        } else {
            Err(ServerError::NotRunning)
        }
    })
}

fn expect_unchanged(reply: Reply) -> Result<(), ServerError> {
    if matches!(reply, Reply::Unchanged) {
        Ok(())
    } else {
        Err(ServerError::Protocol)
    }
}

fn present(
    factory: DevelopmentServerSessionFactory,
    (reservation, alive): (control::Reservation, Arc<AtomicBool>),
    input: mpsc::Receiver<SubmittedInput>,
    (events, semantics): (
        async_channel::Sender<TerminalSessionEvent>,
        async_channel::Sender<Arc<super::accessibility::TerminalAccessibilityModel>>,
    ),
    size: Arc<Mutex<PaneSize>>,
    appearance: Arc<Mutex<TerminalAppearanceUpdate>>,
    mut effects: ClientEffects,
) {
    let DevelopmentServerSessionFactory {
        server,
        local_files,
        remote_metadata,
        pane_id,
        attachment,
        control: _,
        state,
    } = factory;
    let presenter = reservation.activate(&alive);
    let mut attached_session = false;
    let mut generation = PresentationGeneration::default();
    let mut takeover = false;
    let mut paste_unconfirmed = false;
    while alive.load(Ordering::Acquire) {
        if attachment.as_ref().is_some_and(|permit| !permit.active()) {
            return;
        }
        state.retry.store(false, Ordering::Release);
        let connected = (|| {
            let mut connection = server.connect_shared_pane()?;
            let Some(claim::Granted {
                lease,
                size: grant_size,
                appearance: grant_appearance,
            }) = (claim::ClaimWorker {
                state: &state,
                pane_id,
                alive: &alive,
                attachment: &attachment,
                size: &size,
                appearance: &appearance,
                directory_recovery: connection.supports_directory_recovery(),
                attached_session: &mut attached_session,
                takeover: &mut takeover,
                paste_unconfirmed: &mut paste_unconfirmed,
            })
            .run(
                |request| {
                    presenter.send(request, |request| {
                        if !alive.load(Ordering::Acquire) {
                            return Err(ServerError::NotRunning);
                        }
                        // Complete recovery bookkeeping before another presentation can send.
                        // No Recovery lock is held while exchanging with the server.
                        claim::exchange(&state, pane_id, request, |request| {
                            exchange(&attachment, &mut connection, request)
                        })
                    })
                },
                || {
                    drain_input(&state, &input);
                    clear_presentation(&events, &mut generation, &appearance);
                },
            )?
            else {
                return Ok(());
            };
            state.session_dormant.store(false, Ordering::Release);
            attached_session = true;
            // Authenticate and receive the authoritative screen before enabling input.
            let epoch = state.attach(&presenter)?;
            drain_input(&state, &input);
            let mut active_input = ActiveInput {
                epoch,
                lease,
                focus_sent: None,
            };
            let mut active_size = grant_size;
            let mut ready = false;
            let mut reader = reading::Reader::default();
            let mut graphics = graphics::Graphics::default();
            let mut semantic = accessibility::Fetcher::new(lease);
            let mut latest_screen = None;
            let changes = connection.supports_presentation_changes();
            let batches = connection.supports_key_input_batches();
            // Row changes apply only to this exact lease's own previous screen.
            let mut decoder = pane::presentation::ScreenDecoder::default();
            let mut cadence = schedule::Cadence::new(
                std::time::Instant::now(),
                state.presentable.load(Ordering::Acquire),
            );
            let attached = (|| loop {
                if !alive.load(Ordering::Acquire) {
                    return Ok(());
                }
                if attachment.as_ref().is_some_and(|permit| !permit.active()) {
                    return Ok(());
                }
                if state.epoch.load(Ordering::Acquire) != epoch {
                    return Err(ServerError::Protocol);
                }
                // Queued input is sent before any output check.
                active_input.drain(
                    &state,
                    &input,
                    batches,
                    &mut |request| {
                        let reply =
                            send(&presenter, &alive, &attachment, &mut connection, request)?;
                        if let Request::Resize { size, .. } = request {
                            active_size = *size;
                        }
                        Ok(reply)
                    },
                    &mut paste_unconfirmed,
                )?;
                let now = std::time::Instant::now();
                cadence.set_visible(state.presentable.load(Ordering::Acquire), now);
                cadence.set_preparation(state.preparation.load(Ordering::Acquire));
                if changes && let Some(visible) = cadence.take_demand() {
                    expect_unchanged(send(
                        &presenter,
                        &alive,
                        &attachment,
                        &mut connection,
                        &Request::PresentationDemand { lease, visible },
                    )?)?;
                    // A changed demand restarts row changes from a complete screen.
                    decoder = pane::presentation::ScreenDecoder::default();
                }
                let polled = cadence.poll_due(now);
                if polled {
                    let reply = if changes {
                        let base = decoder.baseline();
                        let reply = send(
                            &presenter,
                            &alive,
                            &attachment,
                            &mut connection,
                            &Request::PollChanges { lease, base },
                        )?;
                        decoder.apply(reply)?
                    } else {
                        send(
                            &presenter,
                            &alive,
                            &attachment,
                            &mut connection,
                            &Request::Poll { lease },
                        )?
                    };
                    // A reply that completed after a successor claimed the Pane is not published.
                    if !presenter.current() {
                        return Err(ServerError::NotRunning);
                    }
                    if state.epoch.load(Ordering::Acquire) != epoch {
                        return Err(ServerError::Protocol);
                    }
                    match reply {
                        Reply::Screen(mut screen) => {
                            semantic.note_revision(screen.accessibility_revision);
                            let graphics_error = graphics
                                .update(screen.graphics.take(), screen.alternate)
                                .err();
                            if let Some(error) = graphics_error {
                                graphics.clear();
                                graphics.failure(error);
                            }
                            graphics.recovered();
                            generation = generation.next();
                            let mapping = ScreenMapping {
                                epoch,
                                local: generation,
                                wire: screen.generation,
                                selection: screen
                                    .reading
                                    .as_ref()
                                    .map_or(0, |reading| reading.selection_revision),
                                size: PaneSize {
                                    columns: screen.columns,
                                    rows: screen.rows.len() as u16,
                                    cell_width: screen
                                        .cell_pixels
                                        .map_or(active_size.cell_width, |p| p.0),
                                    cell_height: screen
                                        .cell_pixels
                                        .map_or(active_size.cell_height, |p| p.1),
                                },
                                mouse_tracking: screen.mouse_tracking,
                                alternate: screen.alternate,
                                alternate_scroll: screen.alternate_scroll,
                            };
                            let mut screen = decode_screen(
                                *screen,
                                generation,
                                &appearance.lock().unwrap_or_else(|e| e.into_inner()),
                                local_files.as_ref(),
                            )?;
                            if let Some(metadata) = &remote_metadata {
                                Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata).context =
                                    super::metadata::TerminalMetadataContext::Remote(
                                        metadata.clone(),
                                    );
                            }
                            Arc::make_mut(&mut screen).graphics = graphics.snapshot();
                            if !ready {
                                // Claims apply their measured controller facts once.
                                // Reconcile changes made while attachment was in flight.
                                let current_size = *size.lock().unwrap_or_else(|e| e.into_inner());
                                let update =
                                    appearance.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                if current_size != grant_size {
                                    expect_unchanged(send(
                                        &presenter,
                                        &alive,
                                        &attachment,
                                        &mut connection,
                                        &Request::Resize {
                                            lease,
                                            size: current_size,
                                        },
                                    )?)?;
                                    active_size = current_size;
                                }
                                if grant_appearance != Some(update.generation) {
                                    expect_unchanged(send(
                                        &presenter,
                                        &alive,
                                        &attachment,
                                        &mut connection,
                                        &Request::Style {
                                            lease,
                                            style: encode_style(&update),
                                        },
                                    )?)?;
                                }
                                state
                                    .interaction
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .interactive = connection.supports_interactive_input();
                                state
                                    .interaction
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .reading = connection.supports_reading();
                                // Publish the authorized screen first. Input stays queued
                                // until this worker's focus barrier is acknowledged.
                                ready = true;
                            }
                            state.connected(epoch, mapping, paste_unconfirmed)?;
                            reader.screen(&state, mapping, screen.selection_present);
                            latest_screen = Some((Arc::clone(&screen), mapping));
                            measurement::record(MeasurementEvent::ScreenDecoded {
                                generation: generation.as_u64(),
                                wire_generation: mapping.wire,
                            });
                            let _ = events.force_send(TerminalSessionEvent::Screen(screen));
                            if let Some(model) = semantic.rebind(&state, generation) {
                                let _ = semantics.force_send(model);
                            }
                            takeover = false;
                            if paste_unconfirmed {
                                paste_unconfirmed = false;
                                let _ = events.force_send(TerminalSessionEvent::PasteUnconfirmed);
                            }
                        }
                        Reply::Ended => {
                            state.set(DevelopmentPaneStatus::Ended);
                            let _ = events.force_send(TerminalSessionEvent::Exited(
                                TerminalSessionExit::Success,
                            ));
                            return Ok(());
                        }
                        Reply::Unchanged => {}
                        Reply::AccessibilityRevision { revision } => {
                            semantic.note_revision(revision)
                        }
                        _ => return Err(ServerError::Protocol),
                    }
                }
                if ready {
                    active_input.sync_focus(&state, &mut |request| {
                        send(&presenter, &alive, &attachment, &mut connection, request)
                    })?;
                }
                let mut next = reading::Next::Idle;
                if ready && connection.supports_reading() {
                    next = reader.poll(&state, lease, &mut |request| {
                        send(&presenter, &alive, &attachment, &mut connection, request)
                    })?;
                    let mut reading = state.reading.lock().unwrap_or_else(|e| e.into_inner());
                    reading.publish_ready(&events);
                }
                if polled
                    && ready
                    && connection.supports_native_effects()
                    && let Some((generation, _)) = active_input.focus_sent
                {
                    effects.poll(&state, lease, generation, &events, &mut |request| {
                        send(&presenter, &alive, &attachment, &mut connection, request)
                    })?;
                }
                // A hidden Pane transfers no images.
                if polled && cadence.visible() && ready && connection.supports_graphics() {
                    let result = graphics.poll(lease, &mut |request| {
                        send(&presenter, &alive, &attachment, &mut connection, request)
                    })?;
                    if state.epoch.load(Ordering::Acquire) != epoch {
                        return Err(ServerError::Protocol);
                    }
                    let changed = match result {
                        Ok(changed) => changed,
                        Err(error) => graphics.failure(error),
                    };
                    let recovered = graphics.recovered();
                    if (changed || recovered)
                        && let Some((screen, mapping)) = &mut latest_screen
                    {
                        generation = generation.next();
                        let next = Arc::make_mut(screen);
                        next.generation = generation;
                        next.graphics = graphics.snapshot();
                        mapping.local = generation;
                        state.connected(epoch, *mapping, paste_unconfirmed)?;
                        reader.screen(&state, *mapping, next.selection_present);
                        let _ = events.force_send(TerminalSessionEvent::Screen(Arc::clone(screen)));
                        if let Some(model) = semantic.rebind(&state, generation) {
                            let _ = semantics.force_send(model);
                        }
                    }
                }
                // Semantic transfer follows its own explicit demand, even while hidden, and a
                // partial document continues one chunk per turn.
                if (polled || semantic.can_continue())
                    && ready
                    && connection.supports_accessibility()
                {
                    let model = semantic.poll(&state, generation, &mut |request| {
                        send(&presenter, &alive, &attachment, &mut connection, request)
                    })?;
                    if state.epoch.load(Ordering::Acquire) != epoch {
                        return Err(ServerError::Protocol);
                    }
                    if let Some(model) = model {
                        let _ = semantics.force_send(model);
                    }
                }
                if semantic.can_continue() {
                    next = next.max(reading::Next::Continue);
                }
                if polled && ready {
                    // A complete healthy round renews recovery even when its screen is unchanged.
                    state
                        .connection_recovery
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .connected(std::time::Instant::now());
                }
                let now = std::time::Instant::now();
                match next {
                    reading::Next::Idle => state.wake.wait_until(cadence.deadline()),
                    reading::Next::Recheck => state.wake.wait_until(cadence.recheck_deadline(now)),
                    reading::Next::Continue => {}
                    reading::Next::Refresh => cadence.poll_now(now),
                }
            })();
            if alive.load(Ordering::Acquire) {
                return attached;
            }
            // Dismissal, even mid-exchange, unfocuses the Pane in order; its control remains.
            presenter.retire(lease, changes, |request| {
                exchange(&attachment, &mut connection, request)
            });
            Ok(())
        })();
        match connected {
            Ok(()) => return,
            // A successor owns this Pane's state now; leave it untouched.
            Err(_) if !alive.load(Ordering::Acquire) || !presenter.current() => return,
            Err(error) => {
                // A missing old Session does not mean its retained Pane was removed. Reconcile
                // by Pane identity before deciding, even after several End All operations.
                let error = if error == ServerError::SessionUnavailable
                    && std::mem::take(&mut attached_session)
                {
                    ServerError::StaleControl
                } else {
                    error
                };
                if error == ServerError::SessionUnavailable {
                    state.set(DevelopmentPaneStatus::Ended);
                    let _ = events
                        .force_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success));
                    return;
                }
                if !takeover || state.start_unconfirmed() {
                    state.invalidate();
                }
                drain_input(&state, &input);
                if matches!(error, ServerError::StaleControl | ServerError::Controlled) {
                    takeover = true;
                    paste_unconfirmed = false;
                    clear_presentation(&events, &mut generation, &appearance);
                }
                let retryable = state.connection_failed(error, takeover);
                // Retry waits for its backoff, or wakes for Retry, Take Over or dismissal.
                let backoff = std::time::Instant::now() + Duration::from_secs(1);
                while alive.load(Ordering::Acquire) && !state.retry.swap(false, Ordering::AcqRel) {
                    let now = std::time::Instant::now();
                    if retryable && now >= backoff {
                        break;
                    }
                    state.wake.wait_until(if retryable {
                        backoff
                    } else {
                        now + Duration::from_secs(1)
                    });
                }
            }
        }
    }
}

fn clear_presentation(
    events: &async_channel::Sender<TerminalSessionEvent>,
    generation: &mut PresentationGeneration,
    appearance: &Mutex<TerminalAppearanceUpdate>,
) {
    *generation = generation.next();
    let mut empty = (*ScreenSnapshot::empty_with_appearance(
        crate::local_path::LocalPathSemantics::Posix,
        &appearance.lock().unwrap_or_else(|e| e.into_inner()),
    ))
    .clone();
    empty.generation = *generation;
    let _ = events.force_send(TerminalSessionEvent::Screen(Arc::new(empty)));
}

fn drain_input(state: &DevelopmentPaneState, input: &mpsc::Receiver<SubmittedInput>) {
    while let Ok(submitted) = input.try_recv() {
        discard_input(state, submitted.epoch, submitted.command);
    }
}

fn encode_modifiers(m: InputModifiers) -> u16 {
    u16::from(m.shift)
        | (u16::from(m.alt) << 1)
        | (u16::from(m.control) << 2)
        | (u16::from(m.platform) << 3)
        | (u16::from(m.caps_lock) << 4)
        | (u16::from(m.num_lock) << 5)
        | (u16::from(m.shift_right) << 6)
        | (u16::from(m.alt_right) << 7)
        | (u16::from(m.control_right) << 8)
        | (u16::from(m.platform_right) << 9)
}
fn encode_key(k: KeyInput) -> pane::Key {
    pane::Key {
        physical: k.physical_key as u32,
        action: match k.action {
            KeyAction::Press => 0,
            KeyAction::Repeat => 1,
            KeyAction::Release => 2,
        },
        logical: k.logical_key,
        text: k.text,
        unshifted: k.unshifted_codepoint,
        modifiers: encode_modifiers(k.modifiers),
        consumed: encode_modifiers(k.consumed_modifiers),
        option_as_alt: k.option_as_alt == OptionAsAltPolicy::Both,
    }
}
impl TerminalSessionHandle for PresentationHandle {
    fn set_preparation_demand(&self, prepare: bool) {
        if self.state.preparation.swap(prepare, Ordering::AcqRel) != prepare {
            self.state.wake.notify();
        }
    }
    fn accessibility_selection_sender(&self) -> Option<super::AccessibilitySelectionSender> {
        let state = Arc::clone(&self.state);
        let input = self.input.clone();
        Some(super::AccessibilitySelectionSender::server(Arc::new(
            move |request| {
                submit_input(&state, &input, Input::AccessibilitySelection(request));
            },
        )))
    }
    fn accessibility_demand_sender(&self) -> Option<super::AccessibilityDemandSender> {
        let state = Arc::clone(&self.state);
        Some(super::AccessibilityDemandSender::server(Arc::new(
            move || {
                state.accessibility_demand.store(true, Ordering::Release);
                state.wake.notify();
            },
        )))
    }
    fn set_presentable(&self, presentable: bool) {
        // A hidden Pane keeps its control and Session; only its visual demand ends.
        if !presentable {
            self.state
                .accessibility_demand
                .store(false, Ordering::Release);
        }
        if self.state.presentable.swap(presentable, Ordering::AcqRel) != presentable {
            if !presentable {
                // A hidden view continues no drag; the server cancels its own on hidden demand.
                self.state
                    .reading
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .abandon_gesture();
            }
            self.state.wake.notify();
        }
    }
    fn reading_available(&self) -> bool {
        self.state.status() == DevelopmentPaneStatus::Connected
            && self
                .state
                .interaction
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .reading
    }
    fn copy_or_forward(
        &self,
        modifiers: InputModifiers,
    ) -> Result<Option<super::SelectionCopy>, super::SelectionCopyError> {
        if let Some(copy) = self.prepared_selection(None) {
            return Ok(Some(copy));
        }
        let selection_present = self
            .state
            .reading
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .selection_present;
        if !selection_present {
            self.send(Input::CopyOrForward(modifiers));
        }
        Ok(None)
    }
    fn selection_copy_ready(&self) -> bool {
        self.prepared_selection(None).is_some()
    }
    fn key(&self, k: KeyInput) {
        if k.validate().is_err()
            || k.text
                .as_ref()
                .is_some_and(|s| s.len() > pane::MAX_KEY_TEXT_BYTES)
            || k.logical_key.len() > 128
        {
            return;
        }
        self.send(Input::Key(k));
    }
    fn commit_input_method(
        &self,
        text: &str,
    ) -> Result<async_channel::Receiver<InputMethodOutcome>, InputMethodRefusal> {
        if text.len() > MAX_INPUT_METHOD_BYTES {
            return Err(InputMethodRefusal::TooLarge);
        }
        let (reply, receiver) = async_channel::bounded(1);
        if submit_input(
            &self.state,
            &self.input,
            Input::Commit(text.to_owned(), reply),
        ) {
            Ok(receiver)
        } else {
            Err(InputMethodRefusal::Unavailable)
        }
    }
    fn resize(&self, geometry: TerminalGeometry) {
        let size = pane_size(geometry);
        *self.geometry.lock().unwrap_or_else(|e| e.into_inner()) = geometry;
        {
            let mut interaction = self
                .state
                .interaction
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            interaction.requested_size = Some(size);
            interaction.screen = interaction.screen.filter(|mapping| mapping.size == size);
            if interaction.screen.is_none() {
                interaction.pointer_geometry_start = None;
            }
        }
        *self.size.lock().unwrap_or_else(|e| e.into_inner()) = size;
        self.send(Input::Resize(size));
    }
    fn take_client_effects(&self) -> Vec<ClientEffectRequest> {
        self.state
            .client_effects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }
    fn hidden_input(&self) -> Option<bool> {
        Some(self.state.hidden_input.load(Ordering::Acquire))
    }
    fn focus(&self, focused: bool) {
        self.interactive_focus(focused, focused);
    }
    fn interactive_focus(&self, terminal_input: bool, input_method: bool) {
        let target = if terminal_input {
            pane::FocusTarget::Terminal
        } else if input_method {
            pane::FocusTarget::FocusedPane
        } else {
            pane::FocusTarget::Unfocused
        };
        let mut interaction = self
            .state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if interaction.target != target {
            self.state
                .effect_authority
                .client_focus(if interaction.interactive {
                    Some(terminal_input)
                } else {
                    None
                });
        }
        interaction.focus(target);
        drop(interaction);
        self.state.wake.notify();
    }
    fn interactive_input(&self) -> super::InteractiveInput {
        self.state.interactive_input()
    }
    fn input_generation(&self) -> u64 {
        self.state.epoch.load(Ordering::Acquire)
    }
    fn take_paste_unconfirmed(&self) -> bool {
        self.state.paste_unconfirmed.swap(false, Ordering::AcqRel)
    }
    fn focus_paste_confirmation(&self, id: super::paste::PasteConfirmationId) {
        let mut interaction = self
            .state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.state.effect_authority.client_focus(Some(false));
        if let Some(pending) = interaction.pending.filter(|pending| {
            pending.id == id
                && pending.epoch == self.state.epoch.load(Ordering::Acquire)
                && pending.focus_generation == interaction.focus_generation
                && std::time::Instant::now() < pending.deadline
        }) {
            interaction.target = pane::FocusTarget::PasteConfirmation {
                id: pending.wire_id,
            };
        } else {
            self.state.effect_authority.client_focus(Some(false));
            interaction.focus(pane::FocusTarget::Unfocused);
        }
        drop(interaction);
        self.state.wake.notify();
    }
    fn clear_screen_and_scrollback(&self) {
        self.send(Input::ClearScreenAndScrollback);
    }
    fn pointer(&self, input: PointerInput) {
        use super::pointer_input::{PointerButton, PointerPhase, ShiftSelectionPolicy};
        let continuation = input.phase == PointerPhase::Release
            || (input.phase == PointerPhase::Motion && input.button.is_some());
        let Some((mapping, x, y)) =
            self.pointer_position(input.generation, input.position, continuation)
        else {
            return;
        };

        let width = f32::from(mapping.size.columns) * f32::from(mapping.size.cell_width);
        let height = f32::from(mapping.size.rows) * f32::from(mapping.size.cell_height);
        let mut pointer = pane::Pointer {
            screen_generation: mapping.wire,
            phase: match input.phase {
                PointerPhase::Press => pane::PointerPhase::Press,
                PointerPhase::Motion => pane::PointerPhase::Motion,
                PointerPhase::Release => pane::PointerPhase::Release,
            },
            button: input.button.map(|button| match button {
                PointerButton::Left => pane::PointerButton::Left,
                PointerButton::Middle => pane::PointerButton::Middle,
                PointerButton::Right => pane::PointerButton::Right,
            }),
            x,
            y,
            modifiers: encode_modifiers(input.modifiers),
            shift_selection: input.shift_selection
                == ShiftSelectionPolicy::OverrideApplicationMouse,
        };
        let application = {
            let mut interaction = self
                .state
                .interaction
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if input.phase == PointerPhase::Press {
                interaction.application_pointer_active =
                    mapping.mouse_tracking && !(pointer.shift_selection && input.modifiers.shift);
            }
            let application = interaction.application_pointer_active;
            if input.phase == PointerPhase::Release {
                interaction.application_pointer_active = false;
            }
            application
                || input.phase == PointerPhase::Motion
                    && input.button.is_none()
                    && mapping.mouse_tracking
        };
        if !application {
            pointer.x = pointer.x.clamp(-width, 2.0 * width);
            pointer.y = pointer.y.clamp(-height, 2.0 * height);
            self.read(pane::reading::Operation::Pointer(pointer));
        } else {
            pointer.x = pointer.x.clamp(0.0, width - 1.0);
            pointer.y = pointer.y.clamp(0.0, height - 1.0);
            self.send(Input::Pointer(mapping.epoch, pointer));
        }
    }
    fn pointer_and_copy_selection(
        &self,
        input: PointerInput,
    ) -> Result<Option<super::selection::SelectionCopy>, SelectionCopyError> {
        self.pointer(input);
        self.request_pointer_copy();
        Ok(None)
    }
    fn wheel(&self, input: WheelInput) {
        use super::pointer_input::{ShiftSelectionPolicy, WheelPhase};
        let Some((mapping, x, y)) = self.pointer_position(input.generation, input.position, false)
        else {
            return;
        };

        let x = x.clamp(
            0.0,
            f32::from(mapping.size.columns) * f32::from(mapping.size.cell_width) - 1.0,
        );
        let y = y.clamp(
            0.0,
            f32::from(mapping.size.rows) * f32::from(mapping.size.cell_height) - 1.0,
        );
        let wheel = pane::Wheel {
            screen_generation: mapping.wire,
            horizontal_steps: input.horizontal_steps,
            vertical_steps: input.vertical_steps,
            phase: match input.phase {
                WheelPhase::GestureStarted => pane::WheelPhase::GestureStarted,
                WheelPhase::GestureChanged => pane::WheelPhase::GestureChanged,
                WheelPhase::GestureEnded => pane::WheelPhase::GestureEnded,
                WheelPhase::GestureCancelled => pane::WheelPhase::GestureCancelled,
                WheelPhase::MomentumStarted => pane::WheelPhase::MomentumStarted,
                WheelPhase::MomentumChanged => pane::WheelPhase::MomentumChanged,
                WheelPhase::MomentumEnded => pane::WheelPhase::MomentumEnded,
                WheelPhase::MomentumCancelled => pane::WheelPhase::MomentumCancelled,
            },
            x,
            y,
            modifiers: encode_modifiers(input.modifiers),
            shift_selection: input.shift_selection
                == ShiftSelectionPolicy::OverrideApplicationMouse,
        };
        let scrollback = !mapping.mouse_tracking
            && !(mapping.alternate && mapping.alternate_scroll)
            || mapping.mouse_tracking && wheel.shift_selection && input.modifiers.shift;
        // A large native delta becomes several bounded operations, never a refused one.
        for wheel in reading::split_wheel(wheel) {
            if scrollback {
                self.read(pane::reading::Operation::Wheel(wheel));
            } else {
                self.send(Input::Wheel(mapping.epoch, wheel));
            }
        }
    }
    fn scroll_to(&self, offset: u64, generation: PresentationGeneration) {
        let mapping = self
            .state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .screen;
        if let Some(mapping) = mapping.filter(|m| m.local == generation) {
            self.read(pane::reading::Operation::ScrollTo {
                offset,
                screen: mapping.wire,
            });
        }
    }
    fn scroll_scrollback(&self, movement: ScrollbackMovement) {
        use pane::reading::Scroll;
        self.read(pane::reading::Operation::Scroll(match movement {
            ScrollbackMovement::PageUp => Scroll::PageUp,
            ScrollbackMovement::PageDown => Scroll::PageDown,
            ScrollbackMovement::Top => Scroll::Top,
            ScrollbackMovement::Bottom => Scroll::Bottom,
        }));
    }
    fn set_find_query(&self, generation: super::FindQueryGeneration, text: String) {
        if text.len() <= pane::reading::MAX_QUERY_BYTES {
            self.read(pane::reading::Operation::Query {
                generation: generation.value(),
                text,
            });
        } else {
            // Retire the previous search while the field presents this generation's limit.
            self.end_find(generation);
        }
    }
    fn navigate_find(
        &self,
        generation: super::FindQueryGeneration,
        direction: super::FindDirection,
    ) {
        self.read(pane::reading::Operation::Navigate {
            generation: generation.value(),
            backwards: direction == super::FindDirection::Previous,
        });
    }
    fn end_find(&self, generation: super::FindQueryGeneration) {
        self.read(pane::reading::Operation::EndFind {
            generation: generation.value(),
        });
    }
    fn request_paste(
        &self,
        payload: super::PastePayload,
    ) -> async_channel::Receiver<Result<super::PasteRequestOutcome, String>> {
        let (tx, rx) = async_channel::bounded(1);
        if payload.text().len() > super::paste::MAX_PASTE_BYTES {
            let _ = tx.try_send(Ok(super::PasteRequestOutcome::Rejected(
                super::paste::PasteRejection::TooLarge {
                    limit: super::paste::MAX_PASTE_BYTES,
                },
            )));
            return rx;
        }
        self.send(Input::Paste(payload.text().to_owned(), tx));
        rx
    }
    fn resolve_paste(
        &self,
        id: super::paste::PasteConfirmationId,
        decision: super::PasteDecision,
    ) -> async_channel::Receiver<Result<super::PasteResolution, String>> {
        let (tx, rx) = async_channel::bounded(1);
        let pending = {
            let mut interaction = self
                .state
                .interaction
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if interaction.pending.is_some_and(|pending| pending.id == id) {
                let pending = interaction.pending.take();
                if let Some(pending) = pending {
                    interaction.resolving = Some(ResolvingPaste {
                        pending,
                        restored_generation: None,
                    });
                }
                pending
            } else {
                None
            }
        };
        if let Some(pending) = pending {
            self.send(Input::ResolvePaste(pending, decision, tx));
        } else {
            let _ = tx.try_send(Ok(super::PasteResolution::Stale));
        }
        rx
    }
    fn copy_selection(&self) -> Result<Option<super::SelectionCopy>, SelectionCopyError> {
        Ok(self.prepared_selection(None))
    }
    fn take_prepared_pointer_copy(&self) -> Option<super::SelectionCopy> {
        self.take_pointer_copy()
    }
    fn copy_selection_at(
        &self,
        generation: PresentationGeneration,
    ) -> Result<Option<super::SelectionCopy>, SelectionCopyError> {
        Ok(self.prepared_selection(Some(generation)))
    }
    fn update_appearance(&self, update: TerminalAppearanceUpdate) {
        let style = encode_style(&update);
        *self.appearance.lock().unwrap_or_else(|e| e.into_inner()) = update;
        self.send(Input::Style(style));
    }
}

fn encode_style(update: &TerminalAppearanceUpdate) -> pane::PresentationStyle {
    let colors = &update.appearance.colors;
    let rgb = |c: crate::appearance::Color| [c.r, c.g, c.b];
    let mut palette = [[0; 3]; 16];
    for (index, color) in colors
        .normal
        .iter()
        .chain(colors.bright.iter())
        .copied()
        .enumerate()
    {
        palette[index] = rgb(color);
    }
    pane::PresentationStyle {
        light: update.appearance.appearance == crate::appearance::Appearance::Light,
        foreground: rgb(colors.foreground),
        background: rgb(colors.background),
        palette,
        cursor: rgb(colors.cursor),
        cursor_text: colors.cursor_text.map(rgb),
        bold_as_bright: update.appearance.bold_as_bright,
    }
}
fn color(c: [u8; 3]) -> crate::appearance::Color {
    crate::appearance::Color::from_rgb_components(c[0], c[1], c[2])
}
fn decode_color(c: pane::Color) -> TerminalColor {
    match c {
        pane::Color::Default => TerminalColor::Default,
        pane::Color::Palette(i) => TerminalColor::Palette(i),
        pane::Color::Rgb(c) => TerminalColor::Rgb(color(c)),
    }
}
fn decode_hyperlink(
    link: &pane::effects::Hyperlink,
    local_files: Option<&crate::platform::local_filesystem::LocalFilesystemAuthority>,
) -> Option<Arc<super::hyperlink::HyperlinkTarget>> {
    use super::hyperlink::{HyperlinkTarget, MAX_LINK_BYTES};
    let target = match link {
        pane::effects::Hyperlink::Url(value) => HyperlinkTarget::url(value)?,
        pane::effects::Hyperlink::File(value) => {
            let filesystem = local_files?;
            if value.len() > MAX_LINK_BYTES
                || !filesystem
                    .path_semantics()
                    .is_absolute(std::path::Path::new(value))
            {
                return None;
            }
            HyperlinkTarget::from_local_file(
                filesystem.local_file(value, std::path::Path::new("/"))?,
            )?
        }
    };
    Some(Arc::new(target))
}

fn decode_screen(
    wire: pane::Screen,
    generation: PresentationGeneration,
    appearance: &TerminalAppearanceUpdate,
    local_files: Option<&crate::platform::local_filesystem::LocalFilesystemAuthority>,
) -> Result<Arc<ScreenSnapshot>, ServerError> {
    if !(2..=pane::MAX_COLUMNS).contains(&wire.columns)
        || wire.rows.len() > usize::from(pane::MAX_ROWS)
        || wire.rows.len() < 2
        || wire.palette.len() != 256
        || wire.palette_overrides.len() != 256
        || wire.wrapped.len() != wire.rows.len()
        || wire.rows.iter().any(|r| {
            r.len() != usize::from(wire.columns) || r.iter().any(|c| c.text.chars().count() > 64)
        })
    {
        return Err(ServerError::Protocol);
    }
    let mut screen = (*ScreenSnapshot::empty_with_appearance(
        crate::local_path::LocalPathSemantics::Posix,
        appearance,
    ))
    .clone();
    screen.generation = generation;
    if let Some((width, height)) = wire.cell_pixels {
        if !(1..=256).contains(&width) || !(1..=256).contains(&height) {
            return Err(ServerError::Protocol);
        }
        screen.cell_pixels = (u32::from(width), u32::from(height));
    }
    if let Some(reading) = wire.reading {
        if reading.visible_rows != wire.rows.len() as u64
            || reading.offset_rows > reading.total_rows.saturating_sub(reading.visible_rows)
        {
            return Err(ServerError::Protocol);
        }
        screen.viewport = ViewportSnapshot {
            offset_rows: reading.offset_rows,
            visible_rows: reading.visible_rows,
        };
        screen.scrollbar = ScrollbarSnapshot {
            total_rows: reading.total_rows,
            offset_rows: reading.offset_rows,
            visible_rows: reading.visible_rows,
        };
        screen.selection_present = reading.selection_present;
        screen.find = reading.find.map(|find| {
            Arc::new(super::TerminalFindSnapshot {
                generation: super::FindQueryGeneration::from_value(find.generation),
                total_matches: find.total,
                current_match: find.current,
                visible_spans: find
                    .spans
                    .into_iter()
                    .map(
                        |(row, start_column, end_column, current)| super::FindHighlightSpan {
                            row,
                            start_column,
                            end_column,
                            current,
                        },
                    )
                    .collect(),
            })
        });
    }
    screen.mouse_tracking = wire.mouse_tracking;
    screen.alternate_scroll = wire.alternate_scroll;
    screen.size = ScreenSizeSnapshot {
        cols: wire.columns,
        rows: wire.rows.len() as u16,
    };
    let links = wire
        .hyperlinks
        .iter()
        .take(pane::effects::MAX_SCREEN_HYPERLINKS)
        .map(|link| decode_hyperlink(link, local_files))
        .collect::<Vec<_>>();
    screen.rows = wire
        .rows
        .into_iter()
        .map(|row| {
            Arc::from(
                row.into_iter()
                    .map(|c| CellSnapshot {
                        text: c.text,
                        foreground_source: decode_color(c.foreground),
                        background_source: decode_color(c.background),
                        underline_source: decode_color(c.underline_color),
                        inverse: c.flags & 1 != 0,
                        bold: c.flags & 2 != 0,
                        faint: c.flags & 4 != 0,
                        italic: c.flags & 8 != 0,
                        blinking: c.flags & 16 != 0,
                        invisible: c.flags & 32 != 0,
                        strikethrough: c.flags & 64 != 0,
                        overline: c.flags & 128 != 0,
                        spacer_tail: c.flags & 256 != 0,
                        underline: match c.underline {
                            1 => TerminalUnderlineSnapshot::Single,
                            2 => TerminalUnderlineSnapshot::Double,
                            3 => TerminalUnderlineSnapshot::Curly,
                            4 => TerminalUnderlineSnapshot::Dotted,
                            5 => TerminalUnderlineSnapshot::Dashed,
                            _ => TerminalUnderlineSnapshot::None,
                        },
                        selected: c.flags & 512 != 0,
                        semantic_content: CellSemanticSnapshot::Output,
                        hyperlink: c
                            .hyperlink
                            .and_then(|index| links.get(usize::from(index)))
                            .cloned()
                            .flatten(),
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>()
        .into();
    screen.row_soft_wrapped = wire.wrapped.into();
    screen.colors.foreground_source = if wire.foreground_overridden {
        TerminalDefaultColorSource::ProgramOverride
    } else {
        TerminalDefaultColorSource::HostDefault
    };
    screen.colors.background_source = if wire.background_overridden {
        TerminalDefaultColorSource::ProgramOverride
    } else {
        TerminalDefaultColorSource::HostDefault
    };
    screen.colors.cursor_source = if wire.cursor_overridden {
        TerminalDefaultColorSource::ProgramOverride
    } else {
        TerminalDefaultColorSource::HostDefault
    };
    screen.colors.foreground = if wire.foreground_overridden {
        color(wire.foreground)
    } else {
        appearance.appearance.colors.foreground
    };
    screen.colors.background = if wire.background_overridden {
        color(wire.background)
    } else {
        appearance.appearance.colors.background
    };
    let mut palette = [crate::appearance::Color::BLACK; 256];
    for (dest, source) in palette.iter_mut().zip(wire.palette) {
        *dest = color(source);
    }
    for (index, destination) in palette.iter_mut().enumerate().take(16) {
        if !wire.palette_overrides[index] {
            *destination = if index < 8 {
                appearance.appearance.colors.normal[index]
            } else {
                appearance.appearance.colors.bright[index - 8]
            };
        }
    }
    screen.colors.palette_overrides = Arc::new(
        wire.palette_overrides
            .try_into()
            .map_err(|_| ServerError::Protocol)?,
    );
    screen.colors.palette = Arc::new(palette);
    screen.colors.reversed = wire.reversed;
    screen.background = screen.colors.effective_background();
    screen.active_screen = if wire.alternate {
        ActiveScreenSnapshot::Alternate
    } else {
        ActiveScreenSnapshot::Primary
    };
    screen.cursor = CursorSnapshot {
        position: wire
            .cursor
            .map(|(column, row, width_cells)| CursorPositionSnapshot {
                column,
                row,
                width_cells,
            }),
        visible: wire.cursor_visible,
        blinking: wire.cursor_blinking,
        password_input: false,
        shape: match wire.cursor_shape {
            1 => CursorShapeSnapshot::Bar,
            2 => CursorShapeSnapshot::Underline,
            3 => CursorShapeSnapshot::BlockHollow,
            _ => CursorShapeSnapshot::Block,
        },
        color: if wire.cursor_overridden {
            color(wire.cursor_color)
        } else {
            appearance.appearance.colors.cursor
        },
        text_color: if wire.cursor_overridden {
            color(wire.cursor_text)
        } else {
            appearance
                .appearance
                .colors
                .cursor_text
                .unwrap_or(screen.background)
        },
    };
    screen.text_blinking = screen.rows.iter().any(|row| row.iter().any(|c| c.blinking));
    Ok(Arc::new(screen))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paired_panes_have_no_local_file_authority_even_with_local_composition() {
        struct Route;
        impl spaceterm_server::direct::DirectConnector for Route {
            fn connect(
                &self,
            ) -> Result<
                spaceterm_server::direct::DirectConnection,
                spaceterm_server::direct::DirectError,
            > {
                panic!("factory construction must not connect");
            }
        }
        let server = ServerConnection::direct(
            "spaceterm-development",
            [4; 32],
            spaceterm_server::direct::PairedDevice {
                pin: spaceterm_server::direct::ServerPin {
                    build: "spaceterm-development".into(),
                    server: [9; 32],
                    public_key: [2; 32],
                    certificate: [3; 32],
                },
                pairing: [5; 32],
                client: [6; 32],
            },
            Arc::new(Route),
        )
        .unwrap();
        let (factory, _) = DevelopmentServerSessionFactory::for_pane(
            server,
            &PaneControls::default(),
            crate::platform::runtime_local_filesystem(),
            [8; 32],
        );
        assert!(!factory.allows_local_file_insertion());
        assert!(factory.local_file_authority().is_none());
    }

    #[test]
    fn reading_commands_are_queued_without_terminal_input_focus() {
        let adapter = Adapter::new();
        adapter.handle.interactive_focus(false, true);
        adapter.handle.scroll_scrollback(ScrollbackMovement::Top);
        assert!(
            adapter.input.try_recv().is_ok(),
            "Scroll must reach the server worker"
        );
    }

    #[test]
    fn clear_screen_and_scrollback_uses_the_current_epoch_and_exact_lease_without_focus() {
        let mut adapter = Adapter::new();
        adapter.handle.interactive_focus(false, false);
        adapter.handle.clear_screen_and_scrollback();
        let mut cleared = Vec::new();
        adapter
            .drain(|request| {
                match request {
                    Request::ClearScreenAndScrollback { lease } => cleared.push(*lease),
                    Request::Focus { .. } => {}
                    _ => panic!("unexpected request: {request:?}"),
                }
                Ok(Reply::Unchanged)
            })
            .unwrap();
        assert_eq!(cleared, vec![adapter.active.lease]);

        adapter.handle.clear_screen_and_scrollback();
        adapter.handle.state.invalidate();
        adapter
            .drain(|_| panic!("clear from a retired epoch was replayed"))
            .unwrap();
        adapter.handle.clear_screen_and_scrollback();
        assert!(
            adapter.input.try_recv().is_err(),
            "disconnected clear was queued"
        );
    }

    #[test]
    fn copy_fallback_waits_for_queued_selection_input() {
        let mut adapter = Adapter::new();
        adapter
            .handle
            .read(pane::reading::Operation::Scroll(pane::reading::Scroll::Top));
        adapter
            .handle
            .copy_or_forward(InputModifiers::default())
            .unwrap();
        adapter
            .drain(|request| {
                assert!(
                    !matches!(
                        request,
                        Request::Input {
                            input: pane::InteractiveInput::CopyOrForward { .. },
                            ..
                        }
                    ),
                    "Copy overtook pending reading input"
                );
                Ok(Reply::Unchanged)
            })
            .unwrap();
        let mapping = adapter.tracking_screen(4, 17, false);
        let mut reader = reading::Reader::default();
        reader.screen(&adapter.handle.state, mapping, false);
        for _ in 0..3 {
            reader
                .poll(
                    &adapter.handle.state,
                    adapter.active.lease,
                    &mut |request| match request {
                        Request::Read { .. } => Ok(Reply::Reading { accepted: true }),
                        Request::ReadingStatus { .. } => Ok(Reply::ReadingReady {
                            generation: mapping.wire,
                            selection_revision: mapping.selection,
                            selection_present: true,
                        }),
                        Request::SelectionChunk { .. } => Ok(Reply::SelectionPending),
                        _ => panic!(
                            "Copy must not reach the application after selection input completes"
                        ),
                    },
                )
                .unwrap();
        }
    }

    #[test]
    fn copy_without_a_prepared_selection_reaches_the_fenced_server_input_path() {
        let mut adapter = Adapter::new();
        assert!(
            adapter
                .handle
                .copy_or_forward(InputModifiers {
                    platform: true,
                    ..Default::default()
                })
                .unwrap()
                .is_none()
        );
        let mut requests = 0;
        adapter
            .drain(|request| {
                if matches!(
                    request,
                    Request::Input {
                        input: pane::InteractiveInput::CopyOrForward { modifiers: 8 },
                        ..
                    }
                ) {
                    requests += 1;
                }
                Ok(Reply::Unchanged)
            })
            .unwrap();
        let mut reader = reading::Reader::default();
        reader
            .poll(
                &adapter.handle.state,
                adapter.active.lease,
                &mut |request| {
                    if matches!(
                        request,
                        Request::Input {
                            input: pane::InteractiveInput::CopyOrForward { modifiers: 8 },
                            ..
                        }
                    ) {
                        requests += 1;
                    }
                    Ok(Reply::Unchanged)
                },
            )
            .unwrap();
        assert_eq!(
            requests, 1,
            "the server worker must decide whether Copy forwards a key"
        );
        adapter.handle.copy_or_forward(Default::default()).unwrap();
        adapter.drain(|_| Ok(Reply::Unchanged)).unwrap();
        adapter.handle.interactive_focus(false, true);
        reader
            .poll(&adapter.handle.state, adapter.active.lease, &mut |_| {
                panic!("deferred Copy must not cross a changed focus generation")
            })
            .unwrap();
        adapter
            .drain(|request| {
                assert!(
                    !matches!(request, Request::Input { .. }),
                    "Copy must not cross a changed focus generation"
                );
                Ok(Reply::Unchanged)
            })
            .unwrap();
        adapter.handle.interactive_focus(true, true);
        adapter
            .handle
            .state
            .reading
            .lock()
            .unwrap()
            .selection_present = true;
        adapter.handle.copy_or_forward(Default::default()).unwrap();
        assert!(
            adapter.input.try_recv().is_err(),
            "an unprepared selection must not forward Copy"
        );
    }

    #[test]
    fn a_sustained_drag_coalesces_motion_and_keeps_its_press_and_release() {
        use super::super::pointer_input::{
            PointerButton, PointerPhase, ShiftSelectionPolicy, SurfacePosition,
        };
        let mut adapter = Adapter::new();
        let mapping = adapter.tracking_screen(4, 17, false);
        let pointer = |phase, x| PointerInput {
            generation: mapping.local,
            phase,
            button: Some(PointerButton::Left),
            position: SurfacePosition { x, y: 1.0 },
            modifiers: Default::default(),
            shift_selection: ShiftSelectionPolicy::ReportToApplication,
        };
        adapter.handle.pointer(pointer(PointerPhase::Press, 1.0));
        adapter.drain(|_| Ok(Reply::Unchanged)).unwrap();
        for index in 0..1000 {
            adapter
                .handle
                .pointer(pointer(PointerPhase::Motion, 1.0 + (index % 100) as f32));
            adapter.drain(|_| Ok(Reply::Unchanged)).unwrap();
        }
        adapter
            .handle
            .pointer(pointer(PointerPhase::Release, 100.0));
        adapter.drain(|_| Ok(Reply::Unchanged)).unwrap();
        let mut reader = reading::Reader::default();
        reader.screen(&adapter.handle.state, mapping, false);
        let mut delivered = vec![];
        for _ in 0..8 {
            reader
                .poll(
                    &adapter.handle.state,
                    adapter.active.lease,
                    &mut |request| match request {
                        Request::Read {
                            operation: pane::reading::Operation::Pointer(p),
                            ..
                        } => {
                            delivered.push((
                                match p.phase {
                                    pane::PointerPhase::Press => "press",
                                    pane::PointerPhase::Motion => "motion",
                                    pane::PointerPhase::Release => "release",
                                },
                                p.x,
                            ));
                            Ok(Reply::Reading { accepted: true })
                        }
                        Request::ReadingStatus { .. } => Ok(Reply::ReadingReady {
                            generation: 17,
                            selection_revision: 17,
                            selection_present: false,
                        }),
                        _ => panic!("unexpected reading request"),
                    },
                )
                .unwrap();
        }
        assert_eq!(
            delivered,
            vec![("press", 1.0), ("motion", 100.0), ("release", 100.0)]
        );
        assert_eq!(
            adapter.handle.state.status(),
            DevelopmentPaneStatus::Connected
        );
        assert_eq!(
            adapter.handle.state.epoch.load(Ordering::Acquire),
            mapping.epoch
        );
    }

    pub(super) struct Adapter {
        pub(super) handle: PresentationHandle,
        input: mpsc::Receiver<SubmittedInput>,
        pub(super) active: ActiveInput,
        unconfirmed: bool,
    }
    impl Adapter {
        pub(super) fn new() -> Self {
            Self::with_epoch(|state| Ok(state.invalidate())).unwrap()
        }
        /// A presentation attached through the client's control record for its Pane.
        fn attached(presenter: &control::Presenter) -> Result<Self, ServerError> {
            Self::with_epoch(|state| state.attach(presenter))
        }
        fn with_epoch(
            attach: impl FnOnce(&DevelopmentPaneState) -> Result<u64, ServerError>,
        ) -> Result<Self, ServerError> {
            use super::super::geometry::{BackingScale, CellGridSize, LogicalCellSize};
            let geometry = TerminalGeometry::from_grid(
                CellGridSize::new(20, 6),
                LogicalCellSize::new(10.0, 20.0),
                BackingScale::ONE,
            );
            let state = Arc::new(DevelopmentPaneState::new());
            let epoch = attach(&state)?;
            state.set(DevelopmentPaneStatus::Connected);
            state.interaction.lock().unwrap().interactive = true;
            state.interaction.lock().unwrap().reading = true;
            let (sender, input) = mpsc::sync_channel(512);
            let handle = PresentationHandle {
                input: sender,
                state,
                alive: Arc::new(AtomicBool::new(true)),
                size: Arc::new(Mutex::new(pane_size(geometry))),
                appearance: Arc::new(Mutex::new(TerminalAppearanceUpdate::builtin().unwrap())),
                geometry: Mutex::new(geometry),
            };
            handle.interactive_focus(true, true);
            Ok(Self {
                handle,
                input,
                active: ActiveInput {
                    epoch,
                    lease: Lease {
                        server: [1; 32],
                        session: [2; 32],
                        control: [3; 32],
                        connection: [4; 32],
                    },
                    focus_sent: None,
                },
                unconfirmed: false,
            })
        }
        pub(super) fn drain(
            &mut self,
            mut exchange: impl FnMut(&Request) -> Result<Reply, ServerError>,
        ) -> Result<(), ServerError> {
            while let Ok(input) = self.input.try_recv() {
                self.active.process(
                    &self.handle.state,
                    input,
                    &mut exchange,
                    &mut self.unconfirmed,
                )?;
            }
            Ok(())
        }
        /// Sends queued input as the worker does, in `rounds` bounded drains. A drain with
        /// nothing queued sends nothing.
        fn drain_rounds(
            &mut self,
            rounds: usize,
            batches: bool,
            mut exchange: impl FnMut(&Request) -> Result<Reply, ServerError>,
        ) -> Result<(), ServerError> {
            for _ in 0..rounds {
                self.active.drain(
                    &self.handle.state,
                    &self.input,
                    batches,
                    &mut exchange,
                    &mut self.unconfirmed,
                )?;
            }
            Ok(())
        }
        fn tracking_screen(&self, local: u64, wire: u64, mouse_tracking: bool) -> ScreenMapping {
            let mapping = ScreenMapping {
                epoch: self.active.epoch,
                local: PresentationGeneration::test(local),
                wire,
                selection: wire,
                size: *self.handle.size.lock().unwrap(),
                mouse_tracking,
                alternate: false,
                alternate_scroll: false,
            };
            self.handle
                .state
                .connected(self.active.epoch, mapping, false)
                .unwrap();
            mapping
        }
        fn confirmation(&mut self) -> super::super::paste::PasteConfirmation {
            let reply = self
                .handle
                .request_paste(super::super::PastePayload::from("one\ntwo".to_owned()));
            self.drain(|request| {
                Ok(
                    if matches!(
                        request,
                        Request::Input {
                            input: pane::InteractiveInput::Paste { .. },
                            ..
                        }
                    ) {
                        Reply::Paste(pane::PasteOutcome::ConfirmationRequired(
                            pane::PasteConfirmation {
                                id: 17,
                                byte_len: 7,
                                line_count: 2,
                                multiline: true,
                                control_bytes: false,
                                closing_fence: false,
                            },
                        ))
                    } else {
                        Reply::Unchanged
                    },
                )
            })
            .unwrap();
            let Ok(Ok(super::super::PasteRequestOutcome::ConfirmationRequired(confirmation))) =
                reply.try_recv()
            else {
                panic!("expected paste confirmation")
            };
            confirmation
        }
    }

    fn presenter_for(control: &Arc<control::PaneControl>) -> control::Presenter {
        control.reserve().activate(&AtomicBool::new(true))
    }

    /// The server's focus rule for one Pane on one connection, stated independently.
    #[derive(Default)]
    struct FocusServer {
        generation: u64,
        target: Option<pane::FocusTarget>,
        keys: Vec<u64>,
        visible: Option<bool>,
    }
    impl FocusServer {
        fn handle(&mut self, request: &Request) -> Result<Reply, ServerError> {
            match request {
                Request::Focus {
                    generation, target, ..
                } => {
                    if *generation == self.generation && Some(*target) == self.target {
                        return Ok(Reply::Unchanged);
                    }
                    if *generation <= self.generation {
                        return Err(ServerError::StaleControl);
                    }
                    self.generation = *generation;
                    self.target = Some(*target);
                    Ok(Reply::Unchanged)
                }
                Request::Input {
                    generation,
                    input: pane::InteractiveInput::Key(_),
                    ..
                } => {
                    if *generation != self.generation
                        || self.target != Some(pane::FocusTarget::Terminal)
                    {
                        return Err(ServerError::StaleControl);
                    }
                    self.keys.push(*generation);
                    Ok(Reply::Unchanged)
                }
                Request::PresentationDemand { visible, .. } => {
                    self.visible = Some(*visible);
                    Ok(Reply::Unchanged)
                }
                _ => Ok(Reply::Unchanged),
            }
        }
    }

    #[test]
    fn a_reopened_presentation_is_interactive_at_once_after_many_focus_changes() {
        let controls = PaneControls::default();
        let mut server = FocusServer::default();
        for changes in [0, 1, 15, 100] {
            let presenter = presenter_for(&controls.pane([7; 32]));
            let mut old = Adapter::attached(&presenter).unwrap();
            for change in 0..changes {
                old.handle.interactive_focus(change % 2 == 1, true);
                old.active
                    .sync_focus(&old.handle.state, &mut |request| {
                        presenter.send(request, |request| server.handle(request))
                    })
                    .unwrap();
            }
            // Command-Q closes the window; the same client reopens a new one.
            presenter.retire(old.active.lease, true, |request| server.handle(request));
            // Closing every window leaves the Pane unfocused and without visual demand.
            assert_eq!(server.target, Some(pane::FocusTarget::Unfocused));
            assert_eq!(server.visible, Some(false));
            let successor = presenter_for(&controls.pane([7; 32]));
            let mut new = Adapter::attached(&successor).unwrap();
            new.handle.key(KeyInput::text_input("x"));
            new.drain(|request| successor.send(request, |request| server.handle(request)))
                .unwrap_or_else(|error| {
                    panic!("after {changes} focus changes the first focus was refused: {error:?}")
                });
            assert_eq!(server.target, Some(pane::FocusTarget::Terminal));
            assert_eq!(server.keys.last(), Some(&server.generation));
        }
    }

    #[test]
    fn a_dismissed_presentations_paused_input_cannot_follow_its_successors_focus() {
        let controls = PaneControls::default();
        let mut server = FocusServer::default();
        let presenter = presenter_for(&controls.pane([7; 32]));
        let mut old = Adapter::attached(&presenter).unwrap();
        old.handle.key(KeyInput::text_input("old"));
        let successor = presenter_for(&controls.pane([7; 32]));
        let mut new = Adapter::attached(&successor).unwrap();
        new.drain(|request| successor.send(request, |request| server.handle(request)))
            .unwrap();
        let acknowledged = (server.generation, server.target);
        // The predecessor resumes its paused admission after the successor's focus barrier.
        let mut reached = 0;
        let _ = old.drain(|request| {
            presenter.send(request, |request| {
                reached += 1;
                server.handle(request)
            })
        });
        presenter.retire(old.active.lease, true, |request| {
            reached += 1;
            server.handle(request)
        });
        assert_eq!(reached, 0, "a retired presenter reaches no server");
        assert!(server.keys.is_empty());
        assert_eq!((server.generation, server.target), acknowledged);
    }

    #[test]
    fn a_dismissed_window_whose_worker_starts_late_cannot_revoke_its_reopened_successor() {
        let controls = PaneControls::default();
        let (old, _) = DevelopmentServerSessionFactory::for_pane(
            ServerConnection::for_identity("spaceterm-development").unwrap(),
            &controls,
            crate::platform::runtime_local_filesystem(),
            [7; 32],
        );
        // The old window was created first and closed before its worker ran.
        let reservation = old.control.reserve();
        let successor = presenter_for(&controls.pane([7; 32]));
        let focus = Request::Focus {
            lease: Adapter::new().active.lease,
            generation: 10,
            target: pane::FocusTarget::Terminal,
        };
        successor.send(&focus, |_| Ok(Reply::Unchanged)).unwrap();
        let (_, input) = mpsc::sync_channel(1);
        let (events, _output) = async_channel::bounded(2);
        let (semantics, _semantics) = async_channel::bounded(1);
        let (effects, _clipboard) = ClientEffects::new();
        // The dismissed worker's production entry point. Its view is gone, so it connects to
        // nothing.
        present(
            old,
            (reservation, Arc::new(AtomicBool::new(false))),
            input,
            (events, semantics),
            Arc::new(Mutex::new(PaneSize {
                columns: 20,
                rows: 6,
                cell_width: 10,
                cell_height: 20,
            })),
            Arc::new(Mutex::new(TerminalAppearanceUpdate::builtin().unwrap())),
            effects,
        );
        assert!(
            successor.send(&focus, |_| Ok(Reply::Unchanged)).is_ok(),
            "the reopened window keeps its Pane"
        );
    }

    #[test]
    fn an_exhausted_focus_sequence_fails_closed_instead_of_reusing_a_generation() {
        let controls = PaneControls::default();
        let presenter = presenter_for(&controls.pane([7; 32]));
        presenter
            .send(
                &Request::Focus {
                    lease: Adapter::new().active.lease,
                    generation: u64::MAX,
                    target: pane::FocusTarget::Unfocused,
                },
                |_| Ok(Reply::Unchanged),
            )
            .unwrap();
        let successor = presenter_for(&controls.pane([7; 32]));
        assert!(matches!(
            Adapter::attached(&successor),
            Err(ServerError::Unsupported)
        ));
    }

    /// A typed character: its press carries the text and its release carries none.
    fn typed(text: &str) -> [KeyInput; 2] {
        let press = KeyInput {
            action: KeyAction::Press,
            physical_key: PhysicalKey::A,
            native_key_code: Some(0),
            logical_key: "a".to_owned(),
            text: Some(text.to_owned()),
            unshifted_codepoint: Some('a'),
            modifiers: InputModifiers::default(),
            consumed_modifiers: InputModifiers::default(),
            option_as_alt: OptionAsAltPolicy::None,
        };
        let release = KeyInput {
            action: KeyAction::Release,
            text: None,
            ..press.clone()
        };
        [press, release]
    }

    /// What reached the server: each key exchange as the keys it carried, in order.
    #[derive(Debug, PartialEq)]
    enum Sent {
        Batch(Vec<(u8, Option<String>, String)>),
        Key(u8, Option<String>),
        Resize,
    }

    fn record(sent: &mut Vec<Sent>, request: &Request) -> Result<Reply, ServerError> {
        match request {
            Request::Keys { keys, .. } => sent.push(Sent::Batch(
                keys.iter()
                    .map(|key| (key.action, key.text.clone(), key.logical.clone()))
                    .collect(),
            )),
            Request::Input {
                input: pane::InteractiveInput::Key(key),
                ..
            } => sent.push(Sent::Key(key.action, key.text.clone())),
            Request::Resize { .. } => sent.push(Sent::Resize),
            _ => {}
        }
        Ok(Reply::Unchanged)
    }

    /// Typing 53 characters queues 106 key events. A server that takes key batches gets them
    /// in at most four exchanges, every press and release in order; one that doesn't gets the
    /// same events one exchange each, releases included.
    #[test]
    fn queued_keys_travel_in_bounded_batches_only_to_a_server_that_takes_them() {
        for batches in [true, false] {
            let mut adapter = Adapter::new();
            let typed_text: Vec<String> = (0..53).map(|index| format!("{}", index % 10)).collect();
            for text in &typed_text {
                for key in typed(text) {
                    adapter.handle.key(key);
                }
            }
            let mut sent = Vec::new();
            adapter
                .drain_rounds(8, batches, |request| record(&mut sent, request))
                .unwrap();

            let flat: Vec<(u8, Option<String>)> = sent
                .iter()
                .flat_map(|sent| match sent {
                    Sent::Batch(keys) => keys
                        .iter()
                        .map(|(action, text, _)| (*action, text.clone()))
                        .collect(),
                    Sent::Key(action, text) => vec![(*action, text.clone())],
                    Sent::Resize => Vec::new(),
                })
                .collect();
            let expected: Vec<(u8, Option<String>)> = typed_text
                .iter()
                .flat_map(|text| [(0, Some(text.clone())), (2, None)])
                .collect();
            assert_eq!(flat, expected, "every press and release, in order");
            if batches {
                assert_eq!(sent.len(), 4, "106 events in at most four exchanges");
                assert!(sent.iter().all(|sent| matches!(
                    sent,
                    Sent::Batch(keys) if (1..=pane::MAX_KEY_BATCH).contains(&keys.len())
                )));
            } else {
                assert_eq!(sent.len(), 106, "one exchange per event without batches");
                assert!(sent.iter().all(|sent| matches!(sent, Sent::Key(..))));
            }
        }
    }

    /// A batch holds only keys queued next to each other: any other command first sends the
    /// keys before it. Input method text keeps its own payload inside a batch.
    #[test]
    fn a_key_batch_ends_at_any_other_command_and_keeps_input_method_text() {
        use super::super::geometry::{BackingScale, CellGridSize, LogicalCellSize};
        let mut adapter = Adapter::new();
        let [press, release] = typed("a");
        adapter.handle.key(press);
        adapter.handle.key(KeyInput::input_method_commit("日本"));
        adapter.handle.key(release);
        adapter.handle.resize(TerminalGeometry::from_grid(
            CellGridSize::new(30, 8),
            LogicalCellSize::new(10.0, 20.0),
            BackingScale::ONE,
        ));
        adapter.handle.key(typed("b")[0].clone());
        let mut sent = Vec::new();
        adapter
            .drain_rounds(1, true, |request| record(&mut sent, request))
            .unwrap();
        assert_eq!(
            sent,
            vec![
                Sent::Batch(vec![
                    (0, Some("a".into()), "a".into()),
                    (0, Some("日本".into()), "input-method-commit".into()),
                    (2, None, "a".into()),
                ]),
                Sent::Resize,
                Sent::Batch(vec![(0, Some("b".into()), "a".into())]),
            ]
        );
    }

    #[test]
    fn a_large_input_method_commit_reaches_the_real_server_pty_once_in_order() {
        use sha2::{Digest, Sha256};
        use spaceterm_server::pane::PaneService as _;
        let mut host = crate::runtime::directory_testing::terminal_host();
        let size = PaneSize {
            columns: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        };
        let Reply::Attached { lease } = host.request(
            [1; 32],
            [1; 32],
            Request::Attach {
                create: true,
                size,
                resume: None,
                style: None,
            },
        ) else {
            panic!("fixture Pane did not start")
        };
        let text = "界🙂".repeat(2200) + "尾";
        let expected = text.clone() + "!";
        let command = format!(
            r#"stty raw -echo; python3 -c 'import sys,hashlib; sys.stdout.write("READY\r\n"); sys.stdout.flush(); b=sys.stdin.buffer.read({}); sys.stdout.write(hashlib.sha256(b).hexdigest()+"\r\n"); sys.stdout.flush()'"#,
            expected.len()
        );
        let send = |host: &mut crate::runtime::SharedWorkspaceHost, key| {
            host.request(
                [1; 32],
                [1; 32],
                Request::Key {
                    lease,
                    key: encode_key(key),
                },
            )
        };
        let wait = |host: &mut crate::runtime::SharedWorkspaceHost, marker: Option<&str>| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                if let Reply::Screen(screen) =
                    host.request([1; 32], [1; 32], Request::Poll { lease })
                    && screen.rows.iter().any(|row| {
                        let line = row
                            .iter()
                            .map(|cell| cell.text.as_str())
                            .collect::<String>();
                        marker.map_or(!line.trim().is_empty(), |marker| line.trim_end() == marker)
                    })
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "fixture PTY did not reach {marker:?}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        // Wait for the shell to finish its initial terminal setup before sending typeahead.
        wait(&mut host, None);
        assert!(matches!(
            send(&mut host, KeyInput::text_input(command)),
            Reply::Unchanged
        ));
        let mut enter = KeyInput::text_input("");
        enter.physical_key = PhysicalKey::Enter;
        enter.logical_key = "enter".into();
        enter.text = None;
        assert!(matches!(send(&mut host, enter), Reply::Unchanged));
        wait(&mut host, Some("READY"));
        let mut adapter = Adapter::new();
        adapter.active.lease = lease;
        let outcome = adapter.handle.commit_input_method(&text).unwrap();
        adapter.handle.key(KeyInput::text_input("!"));
        let mut chunks = 0;
        adapter
            .drain_rounds(2, true, |request| {
                if let Request::Input {
                    input: pane::InteractiveInput::Key(key),
                    ..
                } = request
                {
                    chunks += 1;
                    assert!(key.text.as_ref().unwrap().len() <= pane::MAX_KEY_TEXT_BYTES);
                }
                match host.request([1; 32], [1; 32], request.clone()) {
                    Reply::Refused(refusal) => Err(refusal.into()),
                    reply => Ok(reply),
                }
            })
            .unwrap();
        assert!(chunks >= 4);
        assert_eq!(outcome.try_recv().unwrap(), InputMethodOutcome::Written);
        let digest = format!("{:x}", Sha256::digest(expected.as_bytes()));
        wait(&mut host, Some(&digest));
        host.shutdown().unwrap();
    }

    #[test]
    fn an_interrupted_multibyte_commit_never_replays_a_chunk_or_its_suffix() {
        for lost_chunk in [1, 2] {
            let mut adapter = Adapter::new();
            let outcome = adapter
                .handle
                .commit_input_method(&"界🙂".repeat(2500))
                .unwrap();
            adapter.handle.key(KeyInput::text_input("later"));
            let mut chunks = Vec::new();
            let result = adapter.drain_rounds(1, true, |request| {
                if let Request::Input {
                    input: pane::InteractiveInput::Key(key),
                    ..
                } = request
                {
                    chunks.push(key.text.clone().unwrap());
                    if chunks.len() == lost_chunk {
                        // The server could have accepted this chunk before its reply was lost.
                        return Err(ServerError::NotRunning);
                    }
                }
                Ok(Reply::Unchanged)
            });
            assert_eq!(result, Err(ServerError::NotRunning));
            assert_eq!(outcome.try_recv().unwrap(), InputMethodOutcome::Interrupted);
            let _ =
                adapter.drain_rounds(2, true, |_| panic!("an uncertain commit must never replay"));
            assert_eq!(chunks.len(), lost_chunk);
        }
    }

    #[test]
    fn focus_moving_during_a_commit_drops_its_suffix_without_reconnecting() {
        let mut adapter = Adapter::new();
        let outcome = adapter
            .handle
            .commit_input_method(&"界🙂".repeat(2500))
            .unwrap();
        let epoch = adapter.active.epoch;
        let state = Arc::clone(&adapter.handle.state);
        let mut chunks = 0;
        adapter
            .drain_rounds(1, true, |request| {
                if matches!(
                    request,
                    Request::Input {
                        input: pane::InteractiveInput::Key(_),
                        ..
                    }
                ) {
                    chunks += 1;
                    state
                        .interaction
                        .lock()
                        .unwrap()
                        .focus(pane::FocusTarget::Unfocused);
                }
                Ok(Reply::Unchanged)
            })
            .unwrap();
        assert_eq!(chunks, 1);
        assert_eq!(outcome.try_recv().unwrap(), InputMethodOutcome::Interrupted);
        assert_eq!(adapter.handle.state.epoch.load(Ordering::Acquire), epoch);
        assert_eq!(
            adapter.handle.state.status(),
            DevelopmentPaneStatus::Connected
        );
    }

    #[test]
    fn input_method_commit_admission_is_whole_and_bounded() {
        let adapter = Adapter::new();
        assert!(matches!(
            adapter
                .handle
                .commit_input_method(&"a".repeat(MAX_INPUT_METHOD_BYTES + 1)),
            Err(InputMethodRefusal::TooLarge)
        ));
        assert!(adapter.input.try_recv().is_err());
        for _ in 0..512 {
            adapter.handle.key(KeyInput::text_input("queued"));
        }
        assert!(matches!(
            adapter.handle.commit_input_method("界"),
            Err(InputMethodRefusal::Unavailable)
        ));
        assert_eq!(
            adapter.input.try_iter().count(),
            512,
            "no prefix of the refused commit is queued"
        );
    }

    /// Keys queued before focus moved are dropped from a batch exactly as single keys would be.
    #[test]
    fn a_key_batch_carries_only_keys_of_the_current_focus() {
        let mut adapter = Adapter::new();
        adapter.handle.key(typed("stale")[0].clone());
        adapter.handle.interactive_focus(false, false);
        adapter.handle.interactive_focus(true, true);
        adapter.handle.key(typed("current")[0].clone());
        let mut sent = Vec::new();
        adapter
            .drain_rounds(1, true, |request| record(&mut sent, request))
            .unwrap();
        assert_eq!(
            sent,
            vec![Sent::Batch(vec![(0, Some("current".into()), "a".into())])]
        );
    }

    /// A batch whose outcome is unknown ends the connection's input like a single key does, and
    /// neither it nor the keys queued after it are sent again.
    #[test]
    fn an_uncertain_key_batch_is_never_sent_again() {
        let mut adapter = Adapter::new();
        for text in ["a", "b"] {
            for key in typed(text) {
                adapter.handle.key(key);
            }
        }
        let epoch = adapter.active.epoch;
        let mut batches = 0;
        let failed = adapter.drain_rounds(1, true, |request| {
            if matches!(request, Request::Keys { .. }) {
                batches += 1;
                return Err(ServerError::NotRunning);
            }
            Ok(Reply::Unchanged)
        });
        assert_eq!(failed, Err(ServerError::NotRunning));
        assert_ne!(
            adapter.handle.state.epoch.load(Ordering::Acquire),
            epoch,
            "the connection's input is invalidated"
        );
        for key in typed("c") {
            adapter.handle.key(key);
        }
        let _ = adapter.drain_rounds(2, true, |request| {
            if matches!(request, Request::Keys { .. }) {
                batches += 1;
            }
            Ok(Reply::Unchanged)
        });
        assert_eq!(batches, 1, "nothing is replayed");
    }

    #[test]
    fn queued_key_is_discarded_when_focus_moves_to_its_paste_confirmation() {
        let mut adapter = Adapter::new();
        let confirmation = adapter.confirmation();
        adapter.handle.key(KeyInput::text_input("queued"));
        adapter.handle.focus_paste_confirmation(confirmation.id);
        let mut written = false;
        adapter
            .drain(|request| {
                written |= matches!(
                    request,
                    Request::Input {
                        input: pane::InteractiveInput::Key(_),
                        ..
                    }
                );
                Ok(Reply::Unchanged)
            })
            .unwrap();
        assert!(
            !written,
            "a queued key must not enter the terminal after modal focus transfer"
        );
    }

    #[test]
    fn blur_and_refocus_discard_queued_keys_but_allow_current_input_method_text() {
        let mut adapter = Adapter::new();
        adapter.handle.key(KeyInput::text_input("stale"));
        adapter.handle.interactive_focus(false, false);
        adapter.handle.interactive_focus(false, true);
        adapter.handle.key(KeyInput::text_input("not-focused"));
        adapter.handle.key(KeyInput::input_method_commit("日本語"));
        let mut text = Vec::new();
        adapter
            .drain(|request| {
                if let Request::Input {
                    input: pane::InteractiveInput::Key(key),
                    ..
                } = request
                {
                    text.push(key.text.clone());
                }
                Ok(Reply::Unchanged)
            })
            .unwrap();
        assert_eq!(text, vec![Some("日本語".into())]);
    }

    #[test]
    fn an_inflight_paste_confirmation_is_cancelled_after_local_focus_loss() {
        let mut adapter = Adapter::new();
        let response = adapter
            .handle
            .request_paste(super::super::PastePayload::from("one\ntwo".to_owned()));
        let state = Arc::clone(&adapter.handle.state);
        let mut cancelled = Vec::new();
        adapter
            .drain(|request| match request {
                Request::Input {
                    input: pane::InteractiveInput::Paste { .. },
                    ..
                } => {
                    state
                        .interaction
                        .lock()
                        .unwrap()
                        .focus(pane::FocusTarget::Unfocused);
                    Ok(Reply::Paste(pane::PasteOutcome::ConfirmationRequired(
                        pane::PasteConfirmation {
                            id: 42,
                            byte_len: 7,
                            line_count: 2,
                            multiline: true,
                            control_bytes: false,
                            closing_fence: false,
                        },
                    )))
                }
                Request::Input {
                    input: pane::InteractiveInput::ResolvePaste { id, confirm },
                    ..
                } => {
                    cancelled.push((*id, *confirm));
                    Ok(Reply::Paste(pane::PasteOutcome::Cancelled))
                }
                _ => Ok(Reply::Unchanged),
            })
            .unwrap();
        assert_eq!(cancelled, vec![(42, false)]);
        assert_eq!(
            response.try_recv().unwrap().unwrap(),
            super::super::PasteRequestOutcome::Rejected(
                super::super::paste::PasteRejection::TerminalUnfocused
            )
        );
        assert!(
            adapter
                .handle
                .state
                .interaction
                .lock()
                .unwrap()
                .pending
                .is_none()
        );
    }

    #[test]
    fn a_confirmation_queued_before_reconnection_cannot_authorize_the_replacement_lease() {
        let mut adapter = Adapter::new();
        let confirmation = adapter.confirmation();
        adapter.handle.focus_paste_confirmation(confirmation.id);
        let response = adapter
            .handle
            .resolve_paste(confirmation.id, super::super::PasteDecision::Confirm);
        let previous_generation = adapter.handle.input_generation();
        let epoch = adapter.handle.state.invalidate();
        adapter.handle.state.set(DevelopmentPaneStatus::Connected);
        adapter.handle.state.interaction.lock().unwrap().interactive = true;
        adapter.handle.interactive_focus(true, true);
        adapter.active.epoch = epoch;
        adapter.active.lease.connection = [9; 32];
        adapter.active.focus_sent = None;
        adapter
            .drain(|_| panic!("stale approval reached the replacement connection"))
            .unwrap();
        assert_ne!(adapter.handle.input_generation(), previous_generation);
        assert_eq!(
            response.try_recv().unwrap().unwrap(),
            super::super::PasteResolution::Stale
        );
    }

    #[test]
    fn modal_paste_is_single_use_and_lost_confirmation_reply_is_typed_as_uncertain() {
        for acknowledgement in [
            Err(ServerError::Protocol),
            Ok(Reply::Unchanged),
            Ok(Reply::Paste(pane::PasteOutcome::Rejected(
                pane::PasteRejection::Empty,
            ))),
        ] {
            let mut adapter = Adapter::new();
            let confirmation = adapter.confirmation();
            adapter.handle.focus_paste_confirmation(confirmation.id);
            let response = adapter
                .handle
                .resolve_paste(confirmation.id, super::super::PasteDecision::Confirm);
            let expected_lease = adapter.active.lease;
            let result = adapter.drain(|request| match request {
                Request::Input {
                    lease,
                    input:
                        pane::InteractiveInput::ResolvePaste {
                            id: 17,
                            confirm: true,
                        },
                    ..
                } => {
                    assert_eq!(*lease, expected_lease);
                    acknowledgement.clone()
                }
                _ => Ok(Reply::Unchanged),
            });
            assert_eq!(result, Err(ServerError::Protocol));
            assert_eq!(
                response.try_recv().unwrap().unwrap(),
                super::super::PasteResolution::Unconfirmed
            );
            assert!(adapter.unconfirmed);
            let duplicate = adapter
                .handle
                .resolve_paste(confirmation.id, super::super::PasteDecision::Confirm);
            assert_eq!(
                duplicate.try_recv().unwrap().unwrap(),
                super::super::PasteResolution::Stale
            );

            // Reconnection publishes the retained outcome before its replaceable
            // wakeup. Later screen frames may replace that wakeup while UI is busy.
            let epoch = adapter.handle.state.invalidate();
            let mapping = ScreenMapping {
                epoch,
                local: PresentationGeneration::test(10),
                wire: 3,
                selection: 3,
                size: *adapter.handle.size.lock().unwrap(),
                mouse_tracking: false,
                alternate: false,
                alternate_scroll: false,
            };
            adapter
                .handle
                .state
                .connected(epoch, mapping, true)
                .unwrap();
            let (events, output) = async_channel::bounded(2);
            events
                .force_send(TerminalSessionEvent::PasteUnconfirmed)
                .unwrap();
            let screen = ScreenSnapshot::empty_with_appearance(
                crate::local_path::LocalPathSemantics::Posix,
                &adapter.handle.appearance.lock().unwrap(),
            );
            for _ in 0..2 {
                events
                    .force_send(TerminalSessionEvent::Screen(Arc::clone(&screen)))
                    .unwrap();
            }
            while let Ok(event) = output.try_recv() {
                assert!(matches!(event, TerminalSessionEvent::Screen(_)));
            }
            assert!(
                adapter.handle.take_paste_unconfirmed(),
                "screen replacement lost the retained paste warning"
            );
            assert!(
                !adapter.handle.take_paste_unconfirmed(),
                "warning was delivered twice"
            );

            adapter
                .handle
                .state
                .connected(epoch, mapping, true)
                .unwrap();
            adapter.handle.state.invalidate();
            assert!(
                !adapter.handle.take_paste_unconfirmed(),
                "a stale warning crossed a connection epoch"
            );
        }
    }

    #[test]
    fn incompatible_paste_request_acknowledgements_do_not_claim_that_nothing_was_written() {
        for acknowledgement in [
            Reply::Unchanged,
            Reply::Paste(pane::PasteOutcome::Cancelled),
        ] {
            let mut adapter = Adapter::new();
            let response = adapter
                .handle
                .request_paste(super::super::PastePayload::from("safe".to_owned()));
            let result = adapter.drain(|request| {
                Ok(
                    if matches!(
                        request,
                        Request::Input {
                            input: pane::InteractiveInput::Paste { .. },
                            ..
                        }
                    ) {
                        acknowledgement.clone()
                    } else {
                        Reply::Unchanged
                    },
                )
            });
            assert_eq!(result, Err(ServerError::Protocol));
            assert_eq!(
                response.try_recv().unwrap().unwrap(),
                super::super::PasteRequestOutcome::Unconfirmed
            );
            assert!(adapter.unconfirmed);
        }
    }

    #[test]
    fn modal_dismissal_restores_focus_after_the_already_approved_paste() {
        let mut adapter = Adapter::new();
        let confirmation = adapter.confirmation();
        adapter.handle.focus_paste_confirmation(confirmation.id);
        let response = adapter
            .handle
            .resolve_paste(confirmation.id, super::super::PasteDecision::Confirm);
        adapter.handle.interactive_focus(true, true);
        adapter.handle.key(KeyInput::text_input("after"));
        let mut order = Vec::new();
        adapter
            .drain(|request| match request {
                Request::Input {
                    input: pane::InteractiveInput::ResolvePaste { confirm: true, .. },
                    ..
                } => {
                    order.push("paste");
                    Ok(Reply::Paste(pane::PasteOutcome::Written))
                }
                Request::Input {
                    input: pane::InteractiveInput::Key(_),
                    ..
                } => {
                    order.push("key");
                    Ok(Reply::Unchanged)
                }
                Request::Focus {
                    target: pane::FocusTarget::Terminal,
                    ..
                } => {
                    order.push("focus");
                    Ok(Reply::Unchanged)
                }
                _ => Ok(Reply::Unchanged),
            })
            .unwrap();
        assert_eq!(order, vec!["paste", "focus", "key"]);
        assert_eq!(
            response.try_recv().unwrap().unwrap(),
            super::super::PasteResolution::Written
        );
    }

    #[test]
    fn leaving_the_pane_cancels_an_approval_even_after_modal_focus_restoration() {
        let mut adapter = Adapter::new();
        let confirmation = adapter.confirmation();
        adapter.handle.focus_paste_confirmation(confirmation.id);
        let response = adapter
            .handle
            .resolve_paste(confirmation.id, super::super::PasteDecision::Confirm);
        adapter.handle.interactive_focus(true, true);
        adapter.handle.interactive_focus(false, false);
        adapter.handle.interactive_focus(true, true);
        adapter
            .drain(|request| {
                assert!(
                    !matches!(request, Request::Input { .. }),
                    "an approval crossed unrelated focus loss"
                );
                Ok(Reply::Unchanged)
            })
            .unwrap();
        assert_eq!(
            response.try_recv().unwrap().unwrap(),
            super::super::PasteResolution::Stale
        );
    }

    #[test]
    fn unsupported_interaction_disables_typing_without_starting_reconnection() {
        let mut adapter = Adapter::new();
        adapter.handle.key(KeyInput::text_input("before"));
        adapter
            .drain(|request| {
                if matches!(request, Request::Input { .. }) {
                    Err(ServerError::Unsupported)
                } else {
                    Ok(Reply::Unchanged)
                }
            })
            .unwrap();
        assert!(!adapter.handle.interactive_input_available());
        assert_eq!(
            adapter.handle.state.status(),
            DevelopmentPaneStatus::Connected
        );
        adapter.handle.key(KeyInput::input_method_commit("after"));
        adapter
            .drain(|_| panic!("input was sent after capability refusal"))
            .unwrap();
    }

    fn assert_tracked_continuation_after_output(tracking_after_output: bool) {
        use super::super::pointer_input::*;
        let mut adapter = Adapter::new();
        let press_screen = adapter.tracking_screen(8, 91, true);
        let pointer = PointerInput {
            generation: press_screen.local,
            phase: PointerPhase::Press,
            button: Some(PointerButton::Left),
            position: SurfacePosition { x: 15.0, y: 50.0 },
            modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::ReportToApplication,
        };
        let mut delivered = Vec::new();
        let lease = adapter.active.lease;
        let mut server = |request: &Request| {
            if matches!(
                request,
                Request::Input {
                    input: pane::InteractiveInput::Wheel(_),
                    ..
                }
            ) {
                panic!("a stale wheel borrowed the continuation range");
            }
            if let Request::Input {
                lease: actual,
                input: pane::InteractiveInput::Pointer(pointer),
                ..
            } = request
            {
                assert_eq!(*actual, lease);
                delivered.push((
                    match pointer.phase {
                        pane::PointerPhase::Press => "press",
                        pane::PointerPhase::Motion => "motion",
                        pane::PointerPhase::Release => "release",
                    },
                    pointer.screen_generation,
                ));
            }
            Ok(Reply::Unchanged)
        };
        adapter.handle.pointer(pointer);
        adapter.drain(&mut server).unwrap();
        let next_screen = adapter.tracking_screen(9, 92, tracking_after_output);
        // Starts and buttonless motion must not borrow the continuation range.
        adapter.handle.pointer(pointer);
        adapter.handle.pointer(PointerInput {
            phase: PointerPhase::Motion,
            button: None,
            ..pointer
        });
        adapter.handle.wheel(WheelInput {
            generation: pointer.generation,
            horizontal_steps: 0,
            vertical_steps: 1,
            phase: WheelPhase::GestureChanged,
            position: pointer.position,
            modifiers: pointer.modifiers,
            shift_selection: pointer.shift_selection,
        });
        let continuation = PointerInput {
            generation: if tracking_after_output {
                pointer.generation
            } else {
                next_screen.local
            },
            ..pointer
        };
        adapter.handle.pointer(PointerInput {
            phase: PointerPhase::Motion,
            ..continuation
        });
        adapter.handle.pointer(PointerInput {
            phase: PointerPhase::Release,
            ..continuation
        });
        adapter.drain(&mut server).unwrap();
        let clickable = if tracking_after_output {
            next_screen
        } else {
            adapter.tracking_screen(10, 93, true)
        };
        adapter.handle.pointer(PointerInput {
            generation: clickable.local,
            ..pointer
        });
        adapter.drain(&mut server).unwrap();
        assert_eq!(
            delivered,
            vec![
                ("press", 91),
                ("motion", 92),
                ("release", 92),
                ("press", clickable.wire)
            ],
            "tracking after output: {tracking_after_output}"
        );
    }

    #[test]
    fn tracked_continuations_survive_a_newer_screen() {
        assert_tracked_continuation_after_output(true);
    }

    #[test]
    fn tracked_continuations_survive_tracking_exit() {
        assert_tracked_continuation_after_output(false);
    }

    #[test]
    fn continuations_never_retag_a_previous_epoch_or_replaced_geometry() {
        use super::super::{
            geometry::{BackingScale, CellGridSize, LogicalCellSize},
            pointer_input::*,
        };
        for reconnect in [false, true] {
            let mut adapter = Adapter::new();
            let old_screen = adapter.tracking_screen(8, 91, true);
            let pointer = PointerInput {
                generation: old_screen.local,
                phase: PointerPhase::Press,
                button: Some(PointerButton::Left),
                position: SurfacePosition { x: 15.0, y: 50.0 },
                modifiers: InputModifiers::default(),
                shift_selection: ShiftSelectionPolicy::ReportToApplication,
            };
            adapter.handle.pointer(pointer);
            adapter.drain(|_| Ok(Reply::Unchanged)).unwrap();
            if reconnect {
                adapter.active.epoch = adapter.handle.state.invalidate();
                adapter.active.lease.connection = [9; 32];
                adapter.active.focus_sent = None;
                adapter.handle.state.interaction.lock().unwrap().interactive = true;
            } else {
                let original = *adapter.handle.geometry.lock().unwrap();
                adapter.handle.resize(TerminalGeometry::from_grid(
                    CellGridSize::new(21, 6),
                    LogicalCellSize::new(10.0, 20.0),
                    BackingScale::ONE,
                ));
                adapter.handle.pointer(PointerInput {
                    phase: PointerPhase::Release,
                    ..pointer
                });
                // Returning to the same size must not resurrect the old range.
                adapter.handle.resize(original);
                adapter
                    .drain(|request| {
                        assert!(!matches!(request, Request::Input { .. }));
                        Ok(Reply::Unchanged)
                    })
                    .unwrap();
            }
            let replacement = adapter.tracking_screen(9, 92, true);
            adapter.handle.pointer(PointerInput {
                phase: PointerPhase::Release,
                ..pointer
            });
            adapter.handle.pointer(PointerInput {
                phase: PointerPhase::Motion,
                ..pointer
            });
            adapter.handle.pointer(PointerInput {
                generation: replacement.local,
                ..pointer
            });
            let mut delivered = Vec::new();
            adapter
                .drain(|request| {
                    if let Request::Input {
                        input: pane::InteractiveInput::Pointer(pointer),
                        ..
                    } = request
                    {
                        delivered.push((
                            matches!(pointer.phase, pane::PointerPhase::Press),
                            pointer.screen_generation,
                        ));
                    }
                    Ok(Reply::Unchanged)
                })
                .unwrap();
            assert_eq!(delivered, vec![(true, 92)], "reconnect: {reconnect}");
        }
    }

    #[test]
    fn same_wire_size_resize_keeps_quiet_pointer_and_wheel_input_at_the_new_fitted_cells() {
        use super::super::{
            geometry::{BackingScale, CellGridSize, LogicalCellSize, LogicalPosition, LogicalSize},
            pointer_input::*,
        };
        let mut adapter = Adapter::new();
        let geometry = |width| {
            TerminalGeometry::from_viewport(
                LogicalSize::new(width, 432.0),
                LogicalCellSize::new(8.0, 18.0),
                BackingScale::ONE,
                CellGridSize::new(2, 2),
            )
        };
        let original = geometry(640.0);
        adapter.handle.resize(original);
        adapter.drain(|_| Ok(Reply::Unchanged)).unwrap();
        adapter
            .handle
            .state
            .connected(
                adapter.active.epoch,
                ScreenMapping {
                    epoch: adapter.active.epoch,
                    local: PresentationGeneration::test(8),
                    wire: 91,
                    selection: 91,
                    size: pane_size(original),
                    mouse_tracking: true,
                    alternate: false,
                    alternate_scroll: false,
                },
                false,
            )
            .unwrap();
        let resized = geometry(641.0);
        assert_eq!(pane_size(original), pane_size(resized));
        adapter.handle.resize(resized);
        // A quiet server acknowledges this unchanged wire size without a Screen.
        adapter.drain(|_| Ok(Reply::Unchanged)).unwrap();
        let cell = resized.logical_cell_size();
        let position =
            resized.to_backing_position(LogicalPosition::new(cell.width * 10.5, cell.height * 5.5));
        let position = SurfacePosition {
            x: position.x,
            y: position.y,
        };
        adapter.handle.pointer(PointerInput {
            generation: PresentationGeneration::test(8),
            phase: PointerPhase::Press,
            button: Some(PointerButton::Left),
            position,
            modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::ReportToApplication,
        });
        adapter.handle.wheel(WheelInput {
            generation: PresentationGeneration::test(8),
            horizontal_steps: 0,
            vertical_steps: 1,
            phase: WheelPhase::GestureChanged,
            position,
            modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::ReportToApplication,
        });
        let mut delivered = Vec::new();
        let lease = adapter.active.lease;
        adapter
            .drain(|request| {
                if let Request::Input {
                    lease: actual,
                    input,
                    ..
                } = request
                {
                    assert_eq!(*actual, lease);
                    match input {
                        pane::InteractiveInput::Pointer(pointer) => delivered.push((
                            "pointer",
                            pointer.screen_generation,
                            pointer.x,
                            pointer.y,
                        )),
                        pane::InteractiveInput::Wheel(wheel) => {
                            delivered.push(("wheel", wheel.screen_generation, wheel.x, wheel.y))
                        }
                        _ => panic!("unexpected interactive input"),
                    }
                }
                Ok(Reply::Unchanged)
            })
            .unwrap();
        assert_eq!(
            delivered.len(),
            2,
            "same-grid resize disabled quiet application mouse input"
        );
        assert_eq!((delivered[0].0, delivered[0].1), ("pointer", 91));
        assert_eq!((delivered[1].0, delivered[1].1), ("wheel", 91));
        for (_, _, x, y) in delivered {
            assert!(
                (x - 84.0).abs() < 0.001,
                "pointer mapping used the old fitted width: {x}"
            );
            assert!((y - 99.0).abs() < 0.001);
        }
    }

    #[test]
    fn a_wire_size_change_waits_for_its_authoritative_pointer_mapping() {
        use super::super::{
            geometry::{BackingScale, CellGridSize, LogicalCellSize},
            pointer_input::*,
        };
        let mut adapter = Adapter::new();
        let original = *adapter.handle.size.lock().unwrap();
        let mapping = ScreenMapping {
            epoch: adapter.active.epoch,
            local: PresentationGeneration::test(8),
            wire: 91,
            selection: 91,
            size: original,
            mouse_tracking: true,
            alternate: false,
            alternate_scroll: false,
        };
        adapter
            .handle
            .state
            .connected(adapter.active.epoch, mapping, false)
            .unwrap();
        let resized = TerminalGeometry::from_grid(
            CellGridSize::new(21, 6),
            LogicalCellSize::new(10.0, 20.0),
            BackingScale::ONE,
        );
        adapter.handle.resize(resized);
        adapter.drain(|_| Ok(Reply::Unchanged)).unwrap();
        // Initial readiness can receive the old Screen before its resize is applied.
        adapter
            .handle
            .state
            .connected(adapter.active.epoch, mapping, false)
            .unwrap();
        let pointer = PointerInput {
            generation: mapping.local,
            phase: PointerPhase::Press,
            button: Some(PointerButton::Left),
            position: SurfacePosition { x: 15.0, y: 50.0 },
            modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::ReportToApplication,
        };
        adapter.handle.pointer(pointer);
        adapter
            .drain(|_| panic!("obsolete dimensions restored pointer mapping"))
            .unwrap();
        // The resize itself publishes this authoritative replacement, without shell output.
        let replacement = ScreenMapping {
            local: PresentationGeneration::test(9),
            wire: 92,
            selection: 92,
            size: pane_size(resized),
            ..mapping
        };
        adapter
            .handle
            .state
            .connected(adapter.active.epoch, replacement, false)
            .unwrap();
        adapter.handle.pointer(PointerInput {
            generation: replacement.local,
            ..pointer
        });
        let mut delivered = Vec::new();
        adapter
            .drain(|request| {
                if let Request::Input {
                    input: pane::InteractiveInput::Pointer(pointer),
                    ..
                } = request
                {
                    delivered.push((pointer.screen_generation, pointer.x, pointer.y));
                }
                Ok(Reply::Unchanged)
            })
            .unwrap();
        assert_eq!(delivered, vec![(92, 15.0, 50.0)]);
    }

    #[test]
    fn pointer_coordinates_use_fitted_cells_and_the_authoritative_screen_revision() {
        use super::super::{
            geometry::{BackingScale, CellGridSize, LogicalCellSize, LogicalPosition, LogicalSize},
            pointer_input::*,
        };
        let mut adapter = Adapter::new();
        let geometry = TerminalGeometry::from_viewport(
            LogicalSize::new(201.0, 122.0),
            LogicalCellSize::new(10.0, 20.0),
            BackingScale::new(1.5).unwrap(),
            CellGridSize::new(2, 2),
        );
        adapter.handle.resize(geometry);
        adapter.drain(|_| Ok(Reply::Unchanged)).unwrap();
        adapter.handle.state.interaction.lock().unwrap().screen = Some(ScreenMapping {
            epoch: adapter.active.epoch,
            local: PresentationGeneration::test(8),
            wire: 91,
            selection: 91,
            size: pane_size(geometry),
            mouse_tracking: true,
            alternate: false,
            alternate_scroll: false,
        });
        let cell = geometry.logical_cell_size();
        let position =
            geometry.to_backing_position(LogicalPosition::new(cell.width * 1.5, cell.height * 2.5));
        let input = PointerInput {
            generation: PresentationGeneration::test(8),
            phase: PointerPhase::Press,
            button: Some(PointerButton::Left),
            position: SurfacePosition {
                x: position.x,
                y: position.y,
            },
            modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::ReportToApplication,
        };
        adapter.handle.pointer(PointerInput {
            generation: PresentationGeneration::test(7),
            ..input
        });
        adapter.handle.pointer(input);
        let mut coordinates = Vec::new();
        adapter
            .drain(|request| {
                if let Request::Input {
                    input: pane::InteractiveInput::Pointer(pointer),
                    ..
                } = request
                {
                    coordinates.push((pointer.screen_generation, pointer.x, pointer.y));
                }
                Ok(Reply::Unchanged)
            })
            .unwrap();
        assert_eq!(
            coordinates.len(),
            1,
            "stale presentation input must be discarded"
        );
        assert_eq!(coordinates[0].0, 91);
        assert!(
            (coordinates[0].1 - 22.5).abs() < 0.001,
            "wrong fitted-cell x: {:?}",
            coordinates[0]
        );
        assert!(
            (coordinates[0].2 - 75.0).abs() < 0.001,
            "wrong fitted-cell y: {:?}",
            coordinates[0]
        );
    }

    #[test]
    fn an_ended_session_is_a_permanent_failure_after_control_loss() {
        let state = DevelopmentPaneState::new();
        state.set(DevelopmentPaneStatus::Takeover);
        assert!(!state.connection_failed(ServerError::SessionUnavailable, true));
        assert_eq!(
            state.status(),
            DevelopmentPaneStatus::TakeoverFailed(ServerError::SessionUnavailable)
        );
    }

    #[test]
    fn only_a_connected_answer_reads_as_refused_input() {
        use crate::terminal::InteractiveInput;
        let state = DevelopmentPaneState::new();
        assert_eq!(
            state.interactive_input(),
            InteractiveInput::Pending,
            "a Pane still attaching has no answer"
        );
        state.interaction.lock().unwrap().interactive = true;
        state.set(DevelopmentPaneStatus::Connected);
        assert_eq!(state.interactive_input(), InteractiveInput::Accepted);
        state.invalidate();
        assert_eq!(
            state.interactive_input(),
            InteractiveInput::Pending,
            "an interrupted connection is not a refusal"
        );
        state.set(DevelopmentPaneStatus::Connected);
        assert_eq!(
            state.interactive_input(),
            InteractiveInput::Refused,
            "a connection that negotiated no input refuses it"
        );
    }

    #[test]
    fn a_resolving_update_keeps_the_pane_reconnecting_without_carrying_a_click() {
        for (takeover, reconnecting) in [
            (false, DevelopmentPaneStatus::Reconnecting),
            (true, DevelopmentPaneStatus::TakeoverReconnecting),
        ] {
            let state = DevelopmentPaneState::new();
            state.take_over.store(true, Ordering::Release);
            state.start_restored.store(true, Ordering::Release);
            assert!(state.connection_failed(ServerError::ActivationUnresolved, takeover));
            assert_eq!(state.status(), reconnecting);
            // The reconnect neither takes control nor starts a shell on the person's behalf.
            assert!(!state.take_over.load(Ordering::Acquire));
            assert!(!state.start_restored.load(Ordering::Acquire));
        }
        // A deliberate Stop and permanent refusals still wait for the person.
        for error in [
            ServerError::Stopped,
            ServerError::AuthenticationRefused,
            ServerError::MissingRequiredCapability,
        ] {
            let state = DevelopmentPaneState::new();
            assert!(!state.connection_failed(error, false), "{error:?}");
            assert_eq!(state.status(), DevelopmentPaneStatus::Failed(error));
        }
    }

    #[test]
    fn each_take_over_offer_names_the_current_controlling_client() {
        use pane::control::Controller;
        let client = |name: &str| Controller::Client {
            name: name.into(),
            this_client: false,
        };
        let state = DevelopmentPaneState::new();
        assert_eq!(state.controller(), None);
        state.offered_take_over(client("Studio"), false);
        assert_eq!(state.status(), DevelopmentPaneStatus::Takeover);
        assert_eq!(state.controller(), Some(client("Studio")));
        state.offered_take_over(Controller::Nobody, true);
        assert_eq!(state.controller(), Some(Controller::Nobody));
        assert!(state.session_dormant());
        state.take_over();
        state.offered_take_over(client("Laptop"), false);
        assert!(!state.session_dormant());
        assert_eq!(
            state.status(),
            DevelopmentPaneStatus::TakingOver,
            "an offer that crosses Take Over leaves it waiting for its answer"
        );
        assert_eq!(state.controller(), Some(client("Laptop")));
    }

    #[test]
    fn a_transport_outage_keeps_the_takeover_screen_reconnecting() {
        let state = DevelopmentPaneState::new();
        state.set(DevelopmentPaneStatus::Takeover);
        assert!(state.connection_failed(ServerError::NotRunning, true));
        assert_eq!(state.status(), DevelopmentPaneStatus::TakeoverReconnecting);
    }

    #[test]
    fn persistent_protocol_failure_stops_after_three_automatic_reconnections() {
        for takeover in [false, true] {
            let state = DevelopmentPaneState::new();
            for _ in 0..3 {
                assert!(state.connection_failed(ServerError::Protocol, takeover));
            }
            assert!(!state.connection_failed(ServerError::Protocol, takeover));
            assert_eq!(
                state.status(),
                if takeover {
                    DevelopmentPaneStatus::TakeoverFailed(ServerError::Protocol)
                } else {
                    DevelopmentPaneStatus::Failed(ServerError::Protocol)
                }
            );
        }
    }

    #[test]
    fn a_brief_screen_between_protocol_failures_does_not_renew_automatic_retries() {
        let adapter = Adapter::new();
        let state = &adapter.handle.state;
        for sequence in 1..=3 {
            assert!(state.connection_failed(ServerError::Protocol, false));
            adapter.tracking_screen(sequence, sequence, false);
        }
        assert!(!state.connection_failed(ServerError::Protocol, false));
        assert_eq!(
            state.status(),
            DevelopmentPaneStatus::Failed(ServerError::Protocol)
        );
        state.retry();
        assert!(state.connection_failed(ServerError::Protocol, false));
    }

    #[test]
    fn an_outage_during_protocol_recovery_retains_the_original_failure() {
        let state = DevelopmentPaneState::new();
        assert!(state.connection_failed(ServerError::Protocol, false));
        assert!(state.connection_failed(ServerError::NotRunning, false));
        assert!(state.connection_failed(ServerError::NotRunning, false));
        assert!(!state.connection_failed(ServerError::NotRunning, false));
        assert_eq!(
            state.status(),
            DevelopmentPaneStatus::Failed(ServerError::Protocol)
        );
    }

    #[test]
    fn server_file_links_need_client_authority_and_revalidate_before_preview() {
        use crate::terminal::{TerminalLocalFileCapabilities, native_services::FilePreviewTarget};
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-client-link-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let directory = std::fs::canonicalize(directory).unwrap();
        let path = directory.join("preview");
        std::fs::write(&path, b"fixture").unwrap();
        let link = pane::effects::Hyperlink::File(path.to_str().unwrap().into());
        assert!(decode_hyperlink(&link, None).is_none());
        let filesystem = crate::platform::runtime_local_filesystem();
        let target = decode_hyperlink(&link, Some(&filesystem)).unwrap();
        assert!(
            FilePreviewTarget::from_link(&target, TerminalLocalFileCapabilities::Disabled)
                .is_none()
        );
        let preview =
            FilePreviewTarget::from_link(&target, TerminalLocalFileCapabilities::Enabled).unwrap();
        assert_eq!(preview.revalidated_path(), Some(path.clone()));
        std::fs::rename(&path, directory.join("original")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        assert!(preview.revalidated_path().is_none());
        assert!(
            decode_hyperlink(
                &pane::effects::Hyperlink::File("relative".into()),
                Some(&filesystem)
            )
            .is_none()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn screen_reconciliation_keeps_program_colors_and_projects_client_defaults() {
        let mut appearance = TerminalAppearanceUpdate::builtin().unwrap();
        let configured = Arc::make_mut(&mut appearance.appearance);
        configured.colors.background = crate::appearance::Color::rgb(0xffffff);
        configured.colors.normal[0] = crate::appearance::Color::rgb(0x112233);
        let cell = pane::Cell {
            hyperlink: None,
            text: "x".into(),
            foreground: pane::Color::Default,
            background: pane::Color::Default,
            underline_color: pane::Color::Default,
            flags: 0,
            underline: 0,
        };
        let mut overrides = vec![false; 256];
        overrides[1] = true;
        let screen = pane::Screen {
            accessibility_revision: 0,
            graphics: None,
            cell_pixels: None,
            reading: None,
            hyperlinks: Vec::new(),
            generation: 1,
            mouse_tracking: false,
            alternate_scroll: false,
            columns: 2,
            rows: vec![vec![cell.clone(); 2]; 2],
            wrapped: vec![false; 2],
            foreground: [10, 20, 30],
            background: [0, 0, 0],
            palette: vec![[40, 50, 60]; 256],
            reversed: false,
            foreground_overridden: true,
            background_overridden: false,
            cursor_overridden: false,
            palette_overrides: overrides,
            cursor: None,
            cursor_visible: false,
            cursor_blinking: false,
            cursor_shape: 0,
            cursor_color: [0, 0, 0],
            cursor_text: [0, 0, 0],
            alternate: false,
        };
        let mut linked = screen.clone();
        linked.hyperlinks = vec![
            pane::effects::Hyperlink::Url("https://example.test".into()),
            pane::effects::Hyperlink::File("/etc/passwd".into()),
        ];
        linked.rows[0][0].hyperlink = Some(0);
        linked.rows[0][1].hyperlink = Some(1);
        linked.rows[1][0].hyperlink = Some(u16::MAX);
        let linked =
            decode_screen(linked, PresentationGeneration::test(1), &appearance, None).unwrap();
        assert_eq!(
            linked.rows[0][0].hyperlink.as_ref().unwrap().kind,
            super::super::hyperlink::HyperlinkKind::Url
        );
        assert!(linked.rows[0][1].hyperlink.is_none());
        assert!(linked.rows[1][0].hyperlink.is_none());
        let decoded =
            decode_screen(screen, PresentationGeneration::test(1), &appearance, None).unwrap();
        assert_eq!(
            decoded.colors.foreground,
            crate::appearance::Color::rgb(0x0a141e)
        );
        assert_eq!(decoded.background, crate::appearance::Color::rgb(0xffffff));
        assert_eq!(
            decoded.colors.palette[0],
            crate::appearance::Color::rgb(0x112233)
        );
        assert_eq!(
            decoded.colors.palette[1],
            crate::appearance::Color::rgb(0x28323c)
        );
    }
}
