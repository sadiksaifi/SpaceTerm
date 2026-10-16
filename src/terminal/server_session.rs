//! Desktop presentation and fenced input for an authenticated server Pane.
use super::pointer_input::{PointerInput, WheelInput};
use super::session::*;
use super::{emulator::*, geometry::TerminalGeometry, key::*};
use crate::local_server::{LocalServer, ServerError};
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
    Ended,
    Failed(ServerError),
}
pub(crate) struct DevelopmentPaneState {
    status: Mutex<DevelopmentPaneStatus>,
    retry: AtomicBool,
    selected: AtomicBool,
    take_over: AtomicBool,
    start_restored: AtomicBool,
    close: AtomicU64,
    epoch: AtomicU64,
    connected_epoch: AtomicU64,
    interaction: Mutex<Interaction>,
}
impl DevelopmentPaneState {
    fn new() -> Self {
        Self {
            status: Mutex::new(DevelopmentPaneStatus::Connecting),
            retry: AtomicBool::new(false),
            selected: AtomicBool::new(false),
            take_over: AtomicBool::new(false),
            start_restored: AtomicBool::new(false),
            close: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            connected_epoch: AtomicU64::new(0),
            interaction: Mutex::new(Interaction::default()),
        }
    }

    pub(crate) fn status(&self) -> DevelopmentPaneStatus {
        *self.status.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn connection_generation(&self) -> u64 {
        self.connected_epoch.load(Ordering::Acquire)
    }
    pub(crate) fn select(&self) {
        if matches!(
            self.status(),
            DevelopmentPaneStatus::Connecting
                | DevelopmentPaneStatus::Restored
                | DevelopmentPaneStatus::Takeover
        ) {
            self.selected.store(true, Ordering::Release);
            self.retry.store(true, Ordering::Release);
        }
    }
    pub(crate) fn retry(&self) {
        self.retry.store(true, Ordering::Release);
    }
    pub(crate) fn start_restored(&self) {
        if self.status() == DevelopmentPaneStatus::Restored {
            self.start_restored.store(true, Ordering::Release);
        }
    }
    pub(crate) fn take_over(&self) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if *status == DevelopmentPaneStatus::Takeover {
            *status = DevelopmentPaneStatus::TakingOver;
            self.take_over.store(true, Ordering::Release);
            self.retry.store(true, Ordering::Release);
        }
    }
    fn invalidate(&self) -> u64 {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if *status == DevelopmentPaneStatus::Connected {
            *status = DevelopmentPaneStatus::Reconnecting;
        }
        self.close.store(0, Ordering::Release);
        self.interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .disconnect();
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.connected_epoch.store(epoch, Ordering::Release);
        epoch
    }
    pub(crate) fn close(&self, approved_generation: u64) {
        // The worker compares this exact approval with its active epoch. A reconnect
        // between the UI's check and this submission must never retag the request.
        self.close.store(approved_generation, Ordering::Release);
    }
    fn connection_failed(&self, error: ServerError, has_takeover: bool) -> bool {
        let retryable = matches!(
            error,
            ServerError::Protocol
                | ServerError::NotRunning
                | ServerError::StaleControl
                | ServerError::Controlled
        );
        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            // A click accepted before this interruption is never replayed after it.
            self.take_over.store(false, Ordering::Release);
            self.start_restored.store(false, Ordering::Release);
            self.selected.store(false, Ordering::Release);
            *status = if has_takeover && retryable {
                DevelopmentPaneStatus::TakeoverReconnecting
            } else if has_takeover {
                DevelopmentPaneStatus::TakeoverFailed(error)
            } else if retryable {
                DevelopmentPaneStatus::Reconnecting
            } else {
                DevelopmentPaneStatus::Failed(error)
            };
        }
        retryable
    }
    fn set(&self, status: DevelopmentPaneStatus) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status;
    }
    fn connected(&self, epoch: u64, screen: ScreenMapping) -> Result<(), ServerError> {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if self.epoch.load(Ordering::Acquire) != epoch {
            return Err(ServerError::Protocol);
        }
        self.interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .screen = Some(screen);
        self.connected_epoch.store(epoch, Ordering::Release);
        *status = DevelopmentPaneStatus::Connected;
        Ok(())
    }
}
#[derive(Clone)]
pub(crate) struct DevelopmentServerSessionFactory {
    server: LocalServer,
    pane_id: Option<spaceterm_server::hierarchy::Id>,
    attachment: Option<spaceterm_server::attachments::AttachmentPermit>,
    state: Arc<DevelopmentPaneState>,
}
impl DevelopmentServerSessionFactory {
    pub(crate) fn new(server: LocalServer) -> (Self, Arc<DevelopmentPaneState>) {
        let state = Arc::new(DevelopmentPaneState::new());
        (
            Self {
                server,
                pane_id: None,
                attachment: None,
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
    pub(crate) fn for_pane(
        server: LocalServer,
        pane: spaceterm_server::hierarchy::Id,
    ) -> (Self, Arc<DevelopmentPaneState>) {
        let (mut factory, state) = Self::new(server);
        factory.pane_id = Some(pane);
        (factory, state)
    }
}

enum Input {
    Key(KeyInput),
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
}
impl Input {
    fn eligible(&self, target: pane::FocusTarget) -> bool {
        match self {
            Self::Key(key) if key.is_input_method_commit() => matches!(
                target,
                pane::FocusTarget::Terminal | pane::FocusTarget::FocusedPane
            ),
            Self::Key(_) | Self::Pointer(..) | Self::Wheel(..) => {
                target == pane::FocusTarget::Terminal
            }
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
    focus_generation: u64,
    target: pane::FocusTarget,
    screen: Option<ScreenMapping>,
    pending: Option<PendingPaste>,
    resolving: Option<ResolvingPaste>,
    paste_request: Option<(u64, u64)>,
    next_paste: u64,
}
impl Default for Interaction {
    fn default() -> Self {
        Self {
            interactive: false,
            focus_generation: 1,
            target: pane::FocusTarget::Unfocused,
            screen: None,
            pending: None,
            resolving: None,
            paste_request: None,
            next_paste: 0,
        }
    }
}
impl Interaction {
    fn disconnect(&mut self) {
        self.interactive = false;
        self.screen = None;
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
        self.alive.store(false, Ordering::Release);
    }
}
impl PresentationHandle {
    fn pointer_position(
        &self,
        generation: PresentationGeneration,
        position: super::pointer_input::SurfacePosition,
    ) -> Option<(ScreenMapping, f32, f32)> {
        let mapping = self
            .state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .screen?;
        if mapping.local != generation || !position.x.is_finite() || !position.y.is_finite() {
            return None;
        }
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
        let width = f32::from(mapping.size.columns) * f32::from(mapping.size.cell_width);
        let height = f32::from(mapping.size.rows) * f32::from(mapping.size.cell_height);
        Some((
            mapping,
            x.clamp(0.0, width - 1.0),
            y.clamp(0.0, height - 1.0),
        ))
    }
    fn send(&self, input: Input) {
        let current_epoch = self.state.epoch.load(Ordering::Acquire);
        let epoch = match &input {
            Input::ResolvePaste(pending, ..) => pending.epoch,
            Input::Pointer(epoch, _) | Input::Wheel(epoch, _) => *epoch,
            _ => current_epoch,
        };
        let connected = self.state.status() == DevelopmentPaneStatus::Connected;
        let mut interaction = self
            .state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let eligible = input.eligible(interaction.target)
            && match &input {
                Input::ResolvePaste(pending, ..) => interaction.resolution_is_current(*pending),
                _ => true,
            };
        let available = connected
            && epoch == current_epoch
            && (interaction.interactive || matches!(input, Input::Resize(_) | Input::Style(_)))
            && eligible;
        if !available {
            drop(interaction);
            discard_input(&self.state, epoch, input);
            return;
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
                return;
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
        let sent = self.input.try_send(submitted);
        drop(interaction);
        if let Err(error) = sent {
            discard_input(
                &self.state,
                epoch,
                match error {
                    mpsc::TrySendError::Full(input) | mpsc::TrySendError::Disconnected(input) => {
                        input.command
                    }
                },
            );
            self.state.invalidate();
        }
    }
}
fn discard_input(state: &DevelopmentPaneState, epoch: u64, input: Input) {
    match input {
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
                && (matches!(submitted.command, Input::Resize(_) | Input::Style(_))
                    || (interaction.interactive
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
        columns: grid.cols.clamp(2, pane::MAX_COLUMNS),
        rows: grid.rows.clamp(2, pane::MAX_ROWS),
        cell_width: cell.width.clamp(1, 256) as u16,
        cell_height: cell.height.clamp(1, 256) as u16,
    }
}
impl TerminalSessionFactory for DevelopmentServerSessionFactory {
    fn is_server_backed(&self) -> bool {
        true
    }
    fn fallback_title(&self) -> String {
        "Development Pane".into()
    }
    fn start(
        &self,
        geometry: TerminalGeometry,
        _: TerminalLaunchPlan,
        appearance: TerminalAppearanceUpdate,
    ) -> Result<StartedTerminalSession, TerminalSessionError> {
        let (input, receiver) = mpsc::sync_channel(512);
        let (events, output) = async_channel::bounded(2);
        let (_, accessibility) = async_channel::bounded(1);
        let (_, clipboard) = async_channel::bounded(1);
        let alive = Arc::new(AtomicBool::new(true));
        let measured_size = Arc::new(Mutex::new(pane_size(geometry)));
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
        std::thread::Builder::new()
            .name("spaceterm-pane-connection".into())
            .spawn(move || {
                present(factory, alive, receiver, events, measured_size, appearance);
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

fn expect_unchanged(reply: Reply) -> Result<(), ServerError> {
    if matches!(reply, Reply::Unchanged) {
        Ok(())
    } else {
        Err(ServerError::Protocol)
    }
}

fn present(
    factory: DevelopmentServerSessionFactory,
    alive: Arc<AtomicBool>,
    input: mpsc::Receiver<SubmittedInput>,
    events: async_channel::Sender<TerminalSessionEvent>,
    size: Arc<Mutex<PaneSize>>,
    appearance: Arc<Mutex<TerminalAppearanceUpdate>>,
) {
    let DevelopmentServerSessionFactory {
        server,
        pane_id,
        attachment,
        state,
    } = factory;
    let mut create = true;
    let mut expected_session: Option<Lease> = None;
    let mut expected_target: Option<pane::PaneTarget> = None;
    let mut generation = PresentationGeneration::default();
    let mut takeover = false;
    let mut paste_unconfirmed = false;
    while alive.load(Ordering::Acquire) {
        if attachment.as_ref().is_some_and(|permit| !permit.active()) {
            return;
        }
        state.retry.store(false, Ordering::Release);
        let connected = (|| {
            let mut connection = if pane_id.is_some() {
                server.connect_shared_pane()?
            } else {
                server.connect_pane()?
            };
            let intent = create;
            create = false;
            let mut grant_size = *size.lock().unwrap_or_else(|e| e.into_inner());
            let grant_update = appearance.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let mut grant_appearance = Some(grant_update.generation);
            let style = Some(encode_style(&grant_update));
            let request = if let Some(pane) = pane_id {
                Request::SelectPane {
                    pane,
                    explicit: state.selected.swap(false, Ordering::AcqRel),
                    size: grant_size,
                    style,
                }
            } else if connection.supports_takeover() {
                Request::Select {
                    create: intent,
                    size: grant_size,
                    resume: expected_target,
                    style,
                }
            } else {
                Request::Attach {
                    create: intent,
                    size: grant_size,
                    resume: expected_session,
                    style,
                }
            };
            let mut reply =
                exchange(&attachment, &mut connection, &request).map_err(|error| match error {
                    ServerError::Controlled => ServerError::Unsupported,
                    ServerError::StaleControl => ServerError::SessionUnavailable,
                    other => other,
                })?;
            let mut dormant = false;
            while let Reply::Restored {
                server: boot, pane, ..
            } = reply
            {
                if !dormant {
                    dormant = true;
                    expected_session = None;
                    expected_target = None;
                    takeover = false;
                    paste_unconfirmed = false;
                    state.invalidate();
                    state.start_restored.store(false, Ordering::Release);
                    drain_input(&state, &input);
                    clear_presentation(&events, &mut generation, &appearance);
                }
                state.set(DevelopmentPaneStatus::Restored);
                if !alive.load(Ordering::Acquire)
                    || attachment.as_ref().is_some_and(|permit| !permit.active())
                {
                    return Ok(());
                }
                if state.start_restored.swap(false, Ordering::AcqRel)
                    || state.selected.swap(false, Ordering::AcqRel)
                {
                    grant_size = *size.lock().unwrap_or_else(|e| e.into_inner());
                    let update = appearance.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    grant_appearance = Some(update.generation);
                    reply = exchange(
                        &attachment,
                        &mut connection,
                        &Request::StartRestored {
                            server: boot,
                            pane,
                            size: grant_size,
                            style: Some(encode_style(&update)),
                        },
                    )?;
                } else {
                    std::thread::sleep(Duration::from_millis(100));
                    let size = *size.lock().unwrap_or_else(|e| e.into_inner());
                    grant_size = size;
                    grant_appearance = None;
                    reply = exchange(
                        &attachment,
                        &mut connection,
                        &if let Some(pane) = pane_id {
                            Request::SelectPane {
                                pane,
                                explicit: false,
                                size,
                                style: None,
                            }
                        } else {
                            Request::Select {
                                create: false,
                                size,
                                resume: None,
                                style: None,
                            }
                        },
                    )?;
                }
            }
            while let Reply::Takeover { target } = reply {
                if pane_id.is_none() && expected_target.is_some_and(|old| old != target) {
                    return Err(ServerError::SessionUnavailable);
                }
                expected_target = Some(target);
                if !takeover {
                    takeover = true;
                    paste_unconfirmed = false;
                    state.invalidate();
                    drain_input(&state, &input);
                    // Erase cached terminal presentation before the window replaces its surface.
                    clear_presentation(&events, &mut generation, &appearance);
                }
                {
                    let mut status = state.status.lock().unwrap_or_else(|e| e.into_inner());
                    if *status != DevelopmentPaneStatus::TakingOver {
                        *status = DevelopmentPaneStatus::Takeover;
                    }
                }
                if !alive.load(Ordering::Acquire)
                    || attachment.as_ref().is_some_and(|permit| !permit.active())
                {
                    return Ok(());
                }
                if state.take_over.swap(false, Ordering::AcqRel) {
                    state.set(DevelopmentPaneStatus::TakingOver);
                    grant_size = *size.lock().unwrap_or_else(|e| e.into_inner());
                    let update = appearance.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    grant_appearance = Some(update.generation);
                    reply = exchange(
                        &attachment,
                        &mut connection,
                        &Request::TakeOver {
                            target,
                            size: grant_size,
                            style: Some(encode_style(&update)),
                        },
                    )?;
                } else {
                    std::thread::sleep(Duration::from_millis(100));
                    let size = *size.lock().unwrap_or_else(|e| e.into_inner());
                    grant_size = size;
                    grant_appearance = None;
                    reply = exchange(
                        &attachment,
                        &mut connection,
                        &if let Some(pane) = pane_id {
                            Request::SelectPane {
                                pane,
                                explicit: state.selected.swap(false, Ordering::AcqRel),
                                size,
                                style: None,
                            }
                        } else {
                            Request::Select {
                                create: false,
                                size,
                                resume: Some(target),
                                style: None,
                            }
                        },
                    )?;
                }
            }
            let Reply::Attached { lease } = reply else {
                return Err(ServerError::Protocol);
            };
            if pane_id.is_none() && expected_target.is_some_and(|old| old != lease.into()) {
                return Err(ServerError::SessionUnavailable);
            }
            expected_session = Some(lease);
            expected_target = Some(lease.into());
            // Authenticate and receive the authoritative screen before enabling input.
            let epoch = state.invalidate();
            drain_input(&state, &input);
            let mut active_input = ActiveInput {
                epoch,
                lease,
                focus_sent: None,
            };
            let mut active_size = grant_size;
            let mut ready = false;
            loop {
                if !alive.load(Ordering::Acquire)
                    || attachment.as_ref().is_some_and(|permit| !permit.active())
                {
                    return Ok(());
                }
                if state.epoch.load(Ordering::Acquire) != epoch {
                    return Err(ServerError::Protocol);
                }
                let reply = exchange(&attachment, &mut connection, &Request::Poll { lease })?;
                if state.epoch.load(Ordering::Acquire) != epoch {
                    return Err(ServerError::Protocol);
                }
                match reply {
                    Reply::Screen(screen) => {
                        generation = generation.next();
                        let mapping = ScreenMapping {
                            epoch,
                            local: generation,
                            wire: screen.generation,
                            size: active_size,
                            mouse_tracking: screen.mouse_tracking,
                            alternate: screen.alternate,
                            alternate_scroll: screen.alternate_scroll,
                        };
                        let screen = decode_screen(
                            *screen,
                            generation,
                            &appearance.lock().unwrap_or_else(|e| e.into_inner()),
                        )?;
                        if !ready {
                            // Claims apply their measured controller facts once.
                            // Reconcile changes made while attachment was in flight.
                            let current_size = *size.lock().unwrap_or_else(|e| e.into_inner());
                            let update =
                                appearance.lock().unwrap_or_else(|e| e.into_inner()).clone();
                            if current_size != grant_size {
                                expect_unchanged(exchange(
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
                                expect_unchanged(exchange(
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
                            active_input.sync_focus(&state, &mut |request| {
                                exchange(&attachment, &mut connection, request)
                            })?;
                            ready = true;
                        }
                        state.connected(epoch, mapping)?;
                        let _ = events.force_send(TerminalSessionEvent::Screen(screen));
                        takeover = false;
                        if paste_unconfirmed {
                            paste_unconfirmed = false;
                            let _ = events.force_send(TerminalSessionEvent::PasteUnconfirmed);
                        }
                    }
                    Reply::Ended => {
                        state.set(DevelopmentPaneStatus::Ended);
                        let _ = events
                            .force_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success));
                        return Ok(());
                    }
                    Reply::Unchanged => {}
                    _ => return Err(ServerError::Protocol),
                }
                if ready {
                    active_input.sync_focus(&state, &mut |request| {
                        exchange(&attachment, &mut connection, request)
                    })?;
                }
                if state.close.swap(0, Ordering::AcqRel) == epoch {
                    if !matches!(
                        exchange(&attachment, &mut connection, &Request::Close { lease })?,
                        Reply::Closed
                    ) {
                        return Err(ServerError::Protocol);
                    }
                    state.set(DevelopmentPaneStatus::Ended);
                    let _ = events.force_send(TerminalSessionEvent::Exited(
                        TerminalSessionExit::GracefulShutdown,
                    ));
                    return Ok(());
                }
                for _ in 0..32 {
                    let Ok(submitted) = input.try_recv() else {
                        break;
                    };
                    if state.epoch.load(Ordering::Acquire) != epoch {
                        return Err(ServerError::Protocol);
                    }
                    active_input.process(
                        &state,
                        submitted,
                        &mut |request| {
                            let reply = exchange(&attachment, &mut connection, request)?;
                            if let Request::Resize { size, .. } = request {
                                active_size = *size;
                            }
                            Ok(reply)
                        },
                        &mut paste_unconfirmed,
                    )?;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        })();
        match connected {
            Ok(()) => return,
            Err(error) => {
                if pane_id.is_some() && error == ServerError::SessionUnavailable {
                    state.set(DevelopmentPaneStatus::Ended);
                    let _ = events
                        .force_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success));
                    return;
                }
                if !takeover {
                    state.invalidate();
                }
                drain_input(&state, &input);
                if matches!(error, ServerError::StaleControl | ServerError::Controlled) {
                    takeover = true;
                    paste_unconfirmed = false;
                    clear_presentation(&events, &mut generation, &appearance);
                }
                let retryable = state.connection_failed(error, takeover);
                let mut ticks = 0;
                while alive.load(Ordering::Acquire) && !state.retry.swap(false, Ordering::AcqRel) {
                    if retryable && ticks >= 20 {
                        break;
                    }
                    ticks += 1;
                    std::thread::sleep(Duration::from_millis(50));
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
    fn key(&self, k: KeyInput) {
        if k.validate().is_err()
            || k.text.as_ref().is_some_and(|s| s.len() > 4096)
            || k.logical_key.len() > 128
        {
            return;
        }
        self.send(Input::Key(k));
    }
    fn resize(&self, geometry: TerminalGeometry) {
        let size = pane_size(geometry);
        *self.geometry.lock().unwrap_or_else(|e| e.into_inner()) = geometry;
        self.state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .screen = None;
        *self.size.lock().unwrap_or_else(|e| e.into_inner()) = size;
        self.send(Input::Resize(size));
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
        self.state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .focus(target);
    }
    fn interactive_input_available(&self) -> bool {
        self.state.status() == DevelopmentPaneStatus::Connected
            && self
                .state
                .interaction
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .interactive
    }
    fn input_generation(&self) -> u64 {
        self.state.epoch.load(Ordering::Acquire)
    }
    fn focus_paste_confirmation(&self, id: super::paste::PasteConfirmationId) {
        let mut interaction = self
            .state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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
            interaction.focus(pane::FocusTarget::Unfocused);
        }
    }
    fn clear_screen_and_scrollback(&self) {}
    fn pointer(&self, input: PointerInput) {
        use super::pointer_input::{PointerButton, PointerPhase, ShiftSelectionPolicy};
        let Some((mapping, x, y)) = self.pointer_position(input.generation, input.position) else {
            return;
        };
        if !mapping.mouse_tracking {
            return;
        }
        self.send(Input::Pointer(
            mapping.epoch,
            pane::Pointer {
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
            },
        ));
    }
    fn pointer_and_copy_selection(
        &self,
        input: PointerInput,
    ) -> Result<Option<super::selection::SelectionCopy>, SelectionCopyError> {
        self.pointer(input);
        Ok(None)
    }
    fn wheel(&self, input: WheelInput) {
        use super::pointer_input::{ShiftSelectionPolicy, WheelPhase};
        let Some((mapping, x, y)) = self.pointer_position(input.generation, input.position) else {
            return;
        };
        if !mapping.mouse_tracking && !(mapping.alternate && mapping.alternate_scroll) {
            return;
        }
        self.send(Input::Wheel(
            mapping.epoch,
            pane::Wheel {
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
            },
        ));
    }
    fn scroll_to(&self, _: u64, _: PresentationGeneration) {}
    fn scroll_scrollback(&self, _: ScrollbackMovement) {}
    fn set_find_query(&self, _: super::FindQueryGeneration, _: String) {}
    fn navigate_find(&self, _: super::FindQueryGeneration, _: super::FindDirection) {}
    fn end_find(&self, _: super::FindQueryGeneration) {}
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
        Ok(None)
    }
    fn copy_selection_at(
        &self,
        _: PresentationGeneration,
    ) -> Result<Option<super::SelectionCopy>, SelectionCopyError> {
        Ok(None)
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
fn decode_screen(
    wire: pane::Screen,
    generation: PresentationGeneration,
    appearance: &TerminalAppearanceUpdate,
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
    screen.mouse_tracking = wire.mouse_tracking;
    screen.alternate_scroll = wire.alternate_scroll;
    screen.size = ScreenSizeSnapshot {
        cols: wire.columns,
        rows: wire.rows.len() as u16,
    };
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
                        selected: false,
                        semantic_content: CellSemanticSnapshot::Output,
                        hyperlink: None,
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

    struct Adapter {
        handle: PresentationHandle,
        input: mpsc::Receiver<SubmittedInput>,
        active: ActiveInput,
        unconfirmed: bool,
    }
    impl Adapter {
        fn new() -> Self {
            use super::super::geometry::{BackingScale, CellGridSize, LogicalCellSize};
            let geometry = TerminalGeometry::from_grid(
                CellGridSize::new(20, 6),
                LogicalCellSize::new(10.0, 20.0),
                BackingScale::ONE,
            );
            let state = Arc::new(DevelopmentPaneState::new());
            let epoch = state.invalidate();
            state.set(DevelopmentPaneStatus::Connected);
            state.interaction.lock().unwrap().interactive = true;
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
            Self {
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
            }
        }
        fn drain(
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
                Err(ServerError::Protocol)
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
    fn end_session_approval_cannot_transfer_to_a_new_connection() {
        let state = DevelopmentPaneState::new();
        let approved = state.invalidate();
        state.set(DevelopmentPaneStatus::Connected);
        let reconnected = state.invalidate();
        state.set(DevelopmentPaneStatus::Connected);
        state.close(approved);
        assert_ne!(state.close.swap(0, Ordering::AcqRel), reconnected);
        assert_eq!(state.status(), DevelopmentPaneStatus::Connected);
        state.close(reconnected);
        assert_eq!(state.close.swap(0, Ordering::AcqRel), reconnected);
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
    fn a_transport_outage_keeps_the_takeover_screen_reconnecting() {
        let state = DevelopmentPaneState::new();
        state.set(DevelopmentPaneStatus::Takeover);
        assert!(state.connection_failed(ServerError::NotRunning, true));
        assert_eq!(state.status(), DevelopmentPaneStatus::TakeoverReconnecting);
    }

    #[test]
    fn screen_reconciliation_keeps_program_colors_and_projects_client_defaults() {
        let mut appearance = TerminalAppearanceUpdate::builtin().unwrap();
        let configured = Arc::make_mut(&mut appearance.appearance);
        configured.colors.background = crate::appearance::Color::rgb(0xffffff);
        configured.colors.normal[0] = crate::appearance::Color::rgb(0x112233);
        let cell = pane::Cell {
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
        let decoded = decode_screen(screen, PresentationGeneration::test(1), &appearance).unwrap();
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
