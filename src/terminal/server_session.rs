//! Desktop presentation and fenced input for an authenticated server Pane.
mod accessibility;
mod claim;
mod control;
mod effects;
mod inbox;
#[cfg(test)]
mod push_tests;
use super::server_graphics as graphics;
mod link;
mod prediction;
mod reading;
mod recovery;
mod screen;
mod screen_decoder;
#[cfg(test)]
mod screen_decoder_tests;
mod scrollback;
#[cfg(test)]
mod tests;
use super::pointer_input::{PointerInput, WheelInput};
use super::session::clipboard::ClipboardAuthority;
use super::session::*;
use super::{emulator::*, geometry::TerminalGeometry, key::*};
use crate::server_connection::{ServerConnection, ServerError};
pub(crate) use control::{ControlRoute, PaneControlRegistry, PaneControls};
use effects::ClientEffects;
#[cfg(test)]
use link::encode_key;
use link::{encode_modifiers, expect_unchanged, submit_input};
use screen::{ScreenReceiver, clear_presentation, encode_style};
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
    SessionEnded(pane::SessionEnd),
    Ended,
    Failed(ServerError),
    /// The client is not connected to the Pane's server, so it presents the Pane from what it
    /// retained and reaches no Terminal Session.
    Offline,
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
    /// Why the Pane's Session ended, as the server last said, while nobody started a new one.
    session_end: Mutex<Option<pane::SessionEnd>>,
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
    reading: Mutex<reading::Reading>,
    accessibility: Mutex<accessibility::RetainedSemantics>,
    accessibility_demand: AtomicBool,
    client_effects: Mutex<std::collections::VecDeque<ClientEffectRequest>>,
    presentable: AtomicBool,
    preparation: AtomicBool,
    wake: inbox::Wake,
    /// Hints the views that read what this Pane presents.
    changes: crate::terminal::changes::Changes,
}
impl DevelopmentPaneState {
    fn new() -> Self {
        Self {
            status: Mutex::new(DevelopmentPaneStatus::Connecting),
            admission: Mutex::new(Admission::Pending),
            controller: Mutex::default(),
            session_end: Mutex::default(),
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
            wake: inbox::Wake::default(),
            changes: crate::terminal::changes::Changes::default(),
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
    /// Sends `hint` a data-free signal whenever the status, connection generation, directory
    /// progress, controller, dormancy or start confirmation may have changed, so a view reads
    /// them on change instead of on a timer.
    pub(crate) fn observe(&self, hint: async_channel::Sender<()>) {
        self.changes.observe(hint);
    }
    fn changed(&self) {
        self.changes.publish();
    }
    pub(crate) fn startup_pending(&self) -> bool {
        *self.admission.lock().unwrap_or_else(|e| e.into_inner()) == Admission::Pending
            || self.start_unconfirmed()
    }
    fn admission(&self, admission: Admission) {
        *self.admission.lock().unwrap_or_else(|e| e.into_inner()) = admission;
        self.changed();
    }
    /// Who controlled the Pane when the server last offered Take Over, if it ever did.
    pub(crate) fn controller(&self) -> Option<pane::control::Controller> {
        self.controller
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(crate) fn session_end(&self) -> Option<pane::SessionEnd> {
        *self.session_end.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn set_session_end(&self, end: Option<pane::SessionEnd>) {
        *self.session_end.lock().unwrap_or_else(|e| e.into_inner()) = end;
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
            self.changed();
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
                    | DevelopmentPaneStatus::SessionEnded(_)
                    | DevelopmentPaneStatus::Takeover
            )
        {
            self.selected.store(true, Ordering::Release);
            self.retry.store(true, Ordering::Release);
            self.wake.notify();
            self.changed();
        }
    }
    pub(crate) fn retry(&self) {
        self.connection_recovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retry();
        self.retry.store(true, Ordering::Release);
        self.wake.notify();
        self.changed();
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
                    | DevelopmentPaneStatus::SessionEnded(_)
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
            self.changed();
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
            self.changed();
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
            self.changed();
        }
    }
    /// The server offered Take Over and named who controls the Pane. Every offer replaces the
    /// last one, so the Takeover Screen follows control as it moves. A Take Over in progress
    /// stays in progress until the server answers it.
    fn offered_take_over(
        &self,
        controller: pane::control::Controller,
        session_ended: Option<pane::SessionEnd>,
    ) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        self.set_session_end(session_ended);
        *self.controller.lock().unwrap_or_else(|e| e.into_inner()) = Some(controller);
        if *status != DevelopmentPaneStatus::TakingOver {
            *status = DevelopmentPaneStatus::Takeover;
        }
        drop(status);
        self.changed();
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
        // A worker waiting for pushed events learns of the new epoch at once.
        self.wake.notify();
        drop(status);
        self.changed();
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
                .failed(error, std::time::Instant::now());
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
        self.changed();
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
        drop(current);
        self.changed();
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
        let previous = self.connected_epoch.swap(epoch, Ordering::AcqRel);
        if paste_unconfirmed {
            self.paste_unconfirmed.store(true, Ordering::Release);
        }
        let changed = previous != epoch || *status != DevelopmentPaneStatus::Connected;
        if *status != DevelopmentPaneStatus::Connected {
            let interaction = self.interaction.lock().unwrap_or_else(|e| e.into_inner());
            self.effect_authority
                .client_focus(Some(interaction.target == pane::FocusTarget::Terminal));
        }
        *status = DevelopmentPaneStatus::Connected;
        drop(status);
        // Each frame reaches here. Only a new status or generation concerns a view.
        if changed {
            self.changed();
        }
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
            Self::ClearScreenAndScrollback => true,
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
        // Wakes the worker, which retires this presentation at once.
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
                self.cancel_gesture();
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
        if self.selection_present() {
            return Ok(self.copy());
        }
        self.send(Input::CopyOrForward(modifiers));
        Ok(None)
    }
    fn selection_copy_ready(&self) -> bool {
        self.copy_ready()
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
        if !k.is_modifier_key() && matches!(k.action, KeyAction::Press | KeyAction::Repeat) {
            self.typed();
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
        self.typed();
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
        self.route_pointer(input);
    }
    fn pointer_and_copy_selection(
        &self,
        input: PointerInput,
    ) -> Result<Option<super::selection::SelectionCopy>, SelectionCopyError> {
        Ok(self.route_pointer(input))
    }
    fn wheel(&self, input: WheelInput) {
        self.route_wheel(input);
    }
    fn scroll_to(&self, offset: u64, _generation: PresentationGeneration) {
        self.read(|scrollback| scrollback.scroll_to(offset));
    }
    fn scroll_scrollback(&self, movement: ScrollbackMovement) {
        self.read(|scrollback| scrollback.scroll(movement));
    }
    fn set_find_query(&self, generation: super::FindQueryGeneration, text: String) {
        if text.len() <= super::find::MAX_QUERY_BYTES {
            self.read(|scrollback| scrollback.set_find_query(generation, text));
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
        self.read(|scrollback| scrollback.navigate_find(generation, direction));
    }
    fn end_find(&self, generation: super::FindQueryGeneration) {
        self.read(|scrollback| scrollback.end_find(generation));
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
        self.typed();
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
        Ok(self.copy())
    }
    fn take_prepared_pointer_copy(&self) -> Option<super::SelectionCopy> {
        self.take_deferred_copy()
    }
    /// The Selection is this client's own and keeps its rows while output arrives, so the copy
    /// of an earlier presentation is the current Selection's.
    fn copy_selection_at(
        &self,
        _generation: PresentationGeneration,
    ) -> Result<Option<super::SelectionCopy>, SelectionCopyError> {
        Ok(self.copy())
    }
    fn update_appearance(&self, update: TerminalAppearanceUpdate) {
        let style = encode_style(&update);
        *self.appearance.lock().unwrap_or_else(|e| e.into_inner()) = update;
        self.send(Input::Style(style));
    }
}
