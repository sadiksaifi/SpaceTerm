//! The Pane worker's server connection, request sending, event-driven loop and input.
use super::*;
use spaceterm_server::connection::link::{
    ChannelEvent, ChannelHandle, ChannelSink, LinkError, PendingReply, ServerLink,
};
use spaceterm_server::protocol::{
    self, Channel, ClaimReply, InputEvent, InputOutcome, Operation, PaneEvent, ReplyBody,
};

/// Tags input with the current epoch and focus generation, or discards it now.
pub(super) fn submit_input(
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
            ))
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
            let _ = reply.try_send(Ok(super::super::PasteRequestOutcome::Rejected(
                super::super::paste::PasteRejection::ConfirmationPending,
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
    let sent = sender.try_send(submitted);
    drop(interaction);
    let accepted = sent.is_ok();
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
            let _ = reply.try_send(Ok(super::super::PasteRequestOutcome::Rejected(
                super::super::paste::PasteRejection::TerminalUnfocused,
            )));
        }
        Input::ResolvePaste(pending, _, reply) => {
            let mut interaction = state.interaction.lock().unwrap_or_else(|e| e.into_inner());
            if interaction.resolving.as_ref().is_some_and(|resolving| {
                resolving.pending.id == pending.id && resolving.pending.epoch == pending.epoch
            }) {
                interaction.resolving = None;
            }
            let _ = reply.try_send(Ok(super::super::PasteResolution::Stale));
        }
        _ => {}
    }
}

/// The most queued commands a Pane worker sends before it checks the screen again, so typing
/// shows as it arrives. A key batch never holds more.
const INPUT_DRAIN: usize = pane::MAX_KEY_BATCH;
const _: () = assert!(INPUT_DRAIN > 0);

pub(super) struct ActiveInput {
    pub(super) epoch: u64,
    pub(super) lease: Lease,
    pub(super) focus_sent: Option<(u64, pane::FocusTarget)>,
}
impl ActiveInput {
    pub(super) fn sync_focus(
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
                Err(error) => return Err(error),
                _ => return Err(ServerError::Protocol),
            }
        }
        Ok(())
    }
    /// Sends up to [`INPUT_DRAIN`] queued commands in order before the worker next checks the
    /// screen. Adjacent queued keys travel in one exchange, and any other command first sends
    /// the keys queued before it. It never waits for more.
    /// Returns whether it stopped at the bound, so more input may wait for the next turn.
    pub(super) fn drain(
        &mut self,
        state: &DevelopmentPaneState,
        input: &mpsc::Receiver<SubmittedInput>,
        exchange: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
        paste_unconfirmed: &mut bool,
    ) -> Result<bool, ServerError> {
        let mut keys = Vec::new();
        let mut bounded = true;
        for _ in 0..INPUT_DRAIN {
            let Ok(submitted) = input.try_recv() else {
                bounded = false;
                break;
            };
            if state.epoch.load(Ordering::Acquire) != self.epoch {
                return Err(ServerError::Protocol);
            }
            if matches!(submitted.command, Input::Key(_)) {
                keys.push(submitted);
                continue;
            }
            self.send_keys(state, std::mem::take(&mut keys), exchange)?;
            self.process(state, submitted, exchange, paste_unconfirmed)?;
        }
        self.send_keys(state, keys, exchange)?;
        Ok(bounded)
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
            // The server refused only this batch, such as a key it can't type.
            Err(ServerError::Unsupported) => Ok(()),
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

    pub(super) fn process(
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
                    Input::Resize(_) | Input::Style(_) | Input::ClearScreenAndScrollback
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
            Input::CopyOrForward(modifiers) => (
                input_request(pane::InteractiveInput::CopyOrForward {
                    modifiers: encode_modifiers(modifiers),
                }),
                None,
            ),
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
                    Reply::Unchanged => Ok(()),
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
                        confirm: decision == super::super::PasteDecision::Confirm,
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
            // The server refused only this message, such as a pointer press with no button.
            Err(ServerError::Unsupported) if matches!(request, Request::Input { .. }) => {
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
                // outcome is published, even if the UI has not read the status yet.
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
                    pane::PasteOutcome::Written => super::super::PasteRequestOutcome::Written,
                    pane::PasteOutcome::Rejected(rejection) => {
                        super::super::PasteRequestOutcome::Rejected(decode_paste_rejection(
                            rejection,
                        ))
                    }
                    pane::PasteOutcome::ConfirmationRequired(confirmation) => {
                        if !current || reply.is_closed() {
                            drop(interaction);
                            let _ =
                                exchange(&input_request(pane::InteractiveInput::ResolvePaste {
                                    id: confirmation.id,
                                    confirm: false,
                                }));
                            let _ =
                                reply.try_send(Ok(super::super::PasteRequestOutcome::Rejected(
                                    super::super::paste::PasteRejection::TerminalUnfocused,
                                )));
                            return Ok(());
                        }
                        interaction.next_paste = interaction.next_paste.saturating_add(1);
                        let id =
                            super::super::paste::PasteConfirmationId::new(interaction.next_paste);
                        interaction.pending = Some(PendingPaste {
                            id,
                            wire_id: confirmation.id,
                            epoch: self.epoch,
                            focus_generation: generation,
                            deadline: std::time::Instant::now() + Duration::from_secs(30),
                        });
                        super::super::PasteRequestOutcome::ConfirmationRequired(
                            super::super::paste::PasteConfirmation {
                                id,
                                byte_len: confirmation.byte_len as usize,
                                line_count: confirmation.line_count as usize,
                                risk: super::super::paste::PasteRisk {
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
                        let _ = reply.try_send(Ok(super::super::PasteRequestOutcome::Unconfirmed));
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
                    pane::PasteOutcome::Written => super::super::PasteResolution::Written,
                    pane::PasteOutcome::Cancelled => super::super::PasteResolution::Cancelled,
                    pane::PasteOutcome::Stale => super::super::PasteResolution::Stale,
                    _ => {
                        if uncertain_write {
                            *paste_unconfirmed = true;
                            let _ = reply.try_send(Ok(super::super::PasteResolution::Unconfirmed));
                        } else {
                            let _ = reply.try_send(Ok(super::super::PasteResolution::Stale));
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
    Request(async_channel::Sender<Result<super::super::PasteRequestOutcome, String>>),
    Resolution(async_channel::Sender<Result<super::super::PasteResolution, String>>),
}
impl PasteReply {
    fn stale(self) {
        match self {
            Self::Request(reply) => {
                let _ = reply.try_send(Ok(super::super::PasteRequestOutcome::Rejected(
                    super::super::paste::PasteRejection::TerminalUnfocused,
                )));
            }
            Self::Resolution(reply) => {
                let _ = reply.try_send(Ok(super::super::PasteResolution::Stale));
            }
        }
    }
    fn unconfirmed(self) {
        match self {
            Self::Request(reply) => {
                let _ = reply.try_send(Ok(super::super::PasteRequestOutcome::Unconfirmed));
            }
            Self::Resolution(reply) => {
                let _ = reply.try_send(Ok(super::super::PasteResolution::Unconfirmed));
            }
        }
    }
}
fn decode_paste_rejection(rejection: pane::PasteRejection) -> super::super::paste::PasteRejection {
    match rejection {
        pane::PasteRejection::Empty => super::super::paste::PasteRejection::Empty,
        pane::PasteRejection::TooLarge { limit } => super::super::paste::PasteRejection::TooLarge {
            limit: limit as usize,
        },
        pane::PasteRejection::ConfirmationPending => {
            super::super::paste::PasteRejection::ConfirmationPending
        }
        pane::PasteRejection::TerminalUnfocused => {
            super::super::paste::PasteRejection::TerminalUnfocused
        }
    }
}

/// How long a paste waits for the server to settle it.
const PASTE_SETTLES: Duration = Duration::from_secs(10);

/// The Pane worker's connection: its channel on the server's shared link. It maps the worker's
/// commands onto the channel: claims bind it, input is sent without waiting, pushed frames and
/// Pane events queue for the worker's loop, and Scrollback pages answer in the background. Discarded input is reported by the server and fails the next command,
/// as a refused exchange did.
pub(super) struct PaneLink {
    link: ServerLink,
    attachment: Option<spaceterm_server::attachments::AttachmentPermit>,
    channel: Option<(ChannelHandle, Lease)>,
    inbox: Arc<Inbox>,
    /// The bound channel's pushed frames and Pane events, waiting for the worker.
    pushed: Option<Arc<super::inbox::Inbox>>,
    notify: Arc<dyn Fn() + Send + Sync>,
    /// The highest Input serial sent on the bound channel.
    last_input: std::cell::Cell<u32>,
}

#[derive(Default)]
struct Inbox {
    state: Mutex<Received>,
    ready: std::sync::Condvar,
}

#[derive(Default)]
struct Received {
    /// Settled pastes and paste resolutions, by the serial of their last Input message.
    settled: std::collections::BTreeMap<u32, InputOutcome>,
    /// Serials whose outcome a caller waits for.
    awaited: std::collections::BTreeSet<u32>,
    discarded: Option<pane::Refusal>,
    /// Serials of Input messages the server discarded, oldest first, for prediction.
    dropped: std::collections::VecDeque<u32>,
    closed: Option<ServerError>,
}

/// Discarded serials kept for the worker. Older ones are forgotten.
const MAX_DROPPED: usize = 64;

impl PaneLink {
    pub(super) fn new(
        link: ServerLink,
        attachment: Option<spaceterm_server::attachments::AttachmentPermit>,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            link,
            attachment,
            channel: None,
            inbox: Arc::default(),
            pushed: None,
            notify,
            last_input: std::cell::Cell::new(0),
        }
    }

    /// The highest Input serial sent on the bound channel, or 0 before any.
    pub(super) fn last_input(&self) -> u32 {
        self.last_input.get()
    }

    /// Takes the serials of Input messages the server discarded since the last call.
    pub(super) fn take_dropped(&self) -> Vec<u32> {
        let mut received = self.inbox.state.lock().unwrap_or_else(|e| e.into_inner());
        received.dropped.drain(..).collect()
    }

    /// When the oldest pushed event held by a Development push delay becomes due.
    pub(super) fn pushed_due(&self) -> Option<std::time::Instant> {
        self.pushed.as_ref().and_then(|pushed| pushed.due())
    }

    fn sent(&self, serial: u32) -> u32 {
        self.last_input.set(self.last_input.get().max(serial));
        serial
    }

    /// Input outcomes and the link's end settle here. Frames and every other Pane event queue in
    /// `pushed` for the worker's loop.
    fn sink(&self, pushed: Arc<super::inbox::Inbox>) -> Arc<dyn ChannelSink> {
        let inbox = Arc::clone(&self.inbox);
        let notify = Arc::clone(&self.notify);
        Arc::new(move |event: ChannelEvent| {
            {
                let mut received = inbox.state.lock().unwrap_or_else(|e| e.into_inner());
                match event {
                    ChannelEvent::Pane(PaneEvent::Input { serial, outcome }) => {
                        if matches!(outcome, InputOutcome::Discarded(_)) {
                            if received.dropped.len() == MAX_DROPPED {
                                received.dropped.pop_front();
                            }
                            received.dropped.push_back(serial);
                        }
                        if received.awaited.contains(&serial) {
                            received.settled.insert(serial, outcome);
                        } else if let InputOutcome::Discarded(refusal) = outcome
                            && refusal != pane::Refusal::Unsupported
                        {
                            // `Unsupported` refuses only that message, such as text no key
                            // types. Any other discard means the lease or focus moved on, as a
                            // refused exchange did.
                            received.discarded.get_or_insert(refusal);
                        }
                    }
                    ChannelEvent::Closed(error) => {
                        received.closed.get_or_insert(error.into());
                    }
                    event @ (ChannelEvent::Frame(_) | ChannelEvent::Pane(_)) => {
                        drop(received);
                        // The inbox wakes the worker.
                        pushed.deliver(event);
                        return;
                    }
                }
            }
            inbox.ready.notify_all();
            notify();
        })
    }

    fn bound(&self, lease: Lease) -> Result<&ChannelHandle, ServerError> {
        match &self.channel {
            Some((channel, bound)) if *bound == lease => Ok(channel),
            _ => Err(ServerError::StaleControl),
        }
    }

    /// A failure the server reported since the last command.
    fn reported(&self) -> Result<(), ServerError> {
        let mut received = self.inbox.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(error) = received.closed {
            return Err(error);
        }
        match received.discarded.take() {
            Some(refusal) => Err(refusal.into()),
            None => Ok(()),
        }
    }

    fn wait(pending: PendingReply) -> Result<ReplyBody, ServerError> {
        pending.wait().map_err(ServerError::from)
    }

    fn request(&self, operation: Operation) -> Result<ReplyBody, ServerError> {
        Self::wait(self.link.request(operation)?)
    }

    fn input(
        &self,
        lease: Lease,
        generation: u64,
        events: Vec<InputEvent>,
    ) -> Result<u32, ServerError> {
        Ok(self.sent(self.bound(lease)?.input(generation, events)?))
    }

    /// Sends pointer or wheel input. While the link's urgent queue is full the event is dropped:
    /// later motion replaces it, and a stall must not end the Pane's connection.
    fn pointer(&self, lease: Lease, generation: u64, event: InputEvent) -> Result<(), ServerError> {
        match self.bound(lease)?.input(generation, vec![event]) {
            Ok(serial) => {
                self.sent(serial);
                Ok(())
            }
            Err(LinkError::Busy) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Waits until the server settles the paste or resolution that ended with `serial`.
    fn settled(&self, serial: u32) -> Result<pane::PasteOutcome, ServerError> {
        let deadline = std::time::Instant::now() + PASTE_SETTLES;
        let mut received = self.inbox.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(outcome) = received.settled.remove(&serial) {
                received.awaited.remove(&serial);
                return match outcome {
                    InputOutcome::Paste(outcome) => Ok(outcome),
                    InputOutcome::Discarded(refusal) => Err(refusal.into()),
                };
            }
            if let Some(error) = received.closed {
                return Err(error);
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                received.awaited.remove(&serial);
                return Err(ServerError::Protocol);
            }
            received = self
                .inbox
                .ready
                .wait_timeout(received, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Sends input whose outcome the caller needs: a paste or a paste resolution.
    fn settle(
        &self,
        lease: Lease,
        send: impl FnOnce(&ChannelHandle) -> Result<u32, LinkError>,
    ) -> Result<Reply, ServerError> {
        let channel = self.bound(lease)?;
        let mut received = self.inbox.state.lock().unwrap_or_else(|e| e.into_inner());
        let serial = self.sent(send(channel)?);
        // The sink waits for this lock, so the outcome cannot arrive before its registration.
        received.awaited.insert(serial);
        drop(received);
        self.settled(serial).map(Reply::Paste)
    }

    fn claim(
        &mut self,
        claim: spaceterm_server::attachments::PaneClaim,
    ) -> Result<Reply, ServerError> {
        if let Some((old, _)) = self.channel.take() {
            old.detach();
        }
        self.pushed = None;
        // A new channel numbers its Input from 1.
        self.last_input.set(0);
        self.inbox
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .dropped
            .clear();
        let pushed = super::inbox::Inbox::new(Arc::clone(&self.notify));
        let channel = self.link.open_channel(self.sink(Arc::clone(&pushed)))?;
        let reply = Self::wait(channel.claim(
            claim.attachment,
            claim.pane,
            claim.claim,
            claim.size,
            claim.style,
        )?);
        if let Ok(ReplyBody::Claim(ClaimReply::Attached { lease })) = reply {
            self.channel = Some((channel, lease));
            self.pushed = Some(pushed);
            return Ok(Reply::Attached { lease });
        }
        // The server already closed a channel its claim did not bind, so it needs no Release.
        channel.detach();
        Ok(match reply? {
            ReplyBody::Claim(ClaimReply::Takeover(takeover)) => Reply::Takeover {
                session_ended: takeover.session_ended,
                target: takeover.target,
                controller: takeover.controller,
            },
            ReplyBody::Claim(ClaimReply::Restored(restored)) => {
                let protocol::Restored {
                    fresh,
                    session_ended,
                    recovery,
                    directory_unavailable,
                    server,
                    workspace,
                    tab,
                    pane,
                } = *restored;
                Reply::Restored {
                    fresh,
                    session_ended,
                    recovery,
                    directory_unavailable,
                    server,
                    workspace,
                    tab,
                    pane,
                }
            }
            _ => return Err(ServerError::Protocol),
        })
    }

    /// Runs one worker command.
    pub(super) fn exchange(&mut self, request: &Request) -> Result<Reply, ServerError> {
        let result = self.command(request);
        // The Terminal Session is gone, so the channel is released and the server frees its slot.
        if matches!(result, Err(ServerError::SessionUnavailable)) {
            self.release();
        }
        result
    }

    /// Asks for a Scrollback page of the bound channel. `then` receives the page, or `None` when
    /// the server refused it or the link ended, on the link's I/O thread. Returns `false`, without
    /// calling `then`, when the link already carries as many requests as it allows.
    pub(super) fn scrollback_page(
        &self,
        lease: Lease,
        request: super::scrollback::PageRequest,
        then: impl FnOnce(Option<protocol::Page>) + Send + 'static,
    ) -> Result<bool, ServerError> {
        let channel = self.bound(lease)?.channel();
        let sent = self.link.request_then(
            Operation::ScrollbackPage {
                channel,
                epoch: request.epoch,
                start: request.start,
                rows: request.rows,
            },
            move |reply| {
                then(match reply {
                    Ok(ReplyBody::Page(page)) => Some(page),
                    _ => None,
                })
            },
        );
        match sent {
            Ok(_) => Ok(true),
            Err(LinkError::Busy) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Releases the channel after its last event or once its Terminal Session is gone.
    pub(super) fn release(&mut self) {
        self.channel = None;
        self.pushed = None;
    }

    /// Applies every pushed event in order: each frame is applied, then acknowledged, and
    /// `deliver` receives the newest visible screen once per drain and every Pane event.
    pub(super) fn drain<A>(
        &self,
        applier: &mut A,
        deliver: impl FnMut(super::inbox::Delivery<A::Screen>),
    ) -> Result<super::inbox::Flow, ServerError>
    where
        A: super::inbox::Apply<Error = ServerError>,
    {
        self.reported()?;
        let (Some((channel, _)), Some(pushed)) = (&self.channel, &self.pushed) else {
            return Ok(super::inbox::Flow::Open);
        };
        pushed.drain(
            applier,
            |frame| channel.acknowledge(frame).map_err(ServerError::from),
            deliver,
        )
    }

    fn command(&mut self, request: &Request) -> Result<Reply, ServerError> {
        self.reported()?;
        if let Some(permit) = &self.attachment
            && let Some(claim) = permit.claim(&self.link, request)?
        {
            return self.claim(claim);
        }
        let channel =
            |lease: Lease| -> Result<Channel, ServerError> { Ok(self.bound(lease)?.channel()) };
        match request.clone() {
            Request::View {
                lease,
                visible,
                max_hz,
            } => {
                self.bound(lease)?.view(visible, max_hz)?;
                Ok(Reply::Unchanged)
            }
            Request::Focus {
                lease,
                generation,
                target,
            } => {
                self.input(lease, generation, vec![InputEvent::Focus(target)])?;
                Ok(Reply::Unchanged)
            }
            Request::Keys {
                lease,
                generation,
                keys,
            } => {
                for batch in key_batches(keys) {
                    self.input(lease, generation, batch)?;
                }
                Ok(Reply::Unchanged)
            }
            Request::Input {
                lease,
                generation,
                input,
            } => match input {
                pane::InteractiveInput::Paste { text } => {
                    self.settle(lease, |channel| channel.paste(generation, &text))
                }
                pane::InteractiveInput::ResolvePaste { id, confirm } => {
                    self.settle(lease, |channel| {
                        channel.input(generation, vec![InputEvent::ResolvePaste { id, confirm }])
                    })
                }
                pane::InteractiveInput::Pointer(pointer) => {
                    self.pointer(lease, generation, InputEvent::Pointer(pointer))?;
                    Ok(Reply::Unchanged)
                }
                pane::InteractiveInput::Wheel(wheel) => {
                    self.pointer(lease, generation, InputEvent::Wheel(wheel))?;
                    Ok(Reply::Unchanged)
                }
                input => {
                    let event = match input {
                        pane::InteractiveInput::Key(key) => InputEvent::Key(key),
                        pane::InteractiveInput::CopyOrForward { modifiers } => {
                            InputEvent::CopyOrForward { modifiers }
                        }
                        pane::InteractiveInput::Paste { .. }
                        | pane::InteractiveInput::ResolvePaste { .. }
                        | pane::InteractiveInput::Pointer(_)
                        | pane::InteractiveInput::Wheel(_) => unreachable!(),
                    };
                    self.input(lease, generation, vec![event])?;
                    Ok(Reply::Unchanged)
                }
            },
            // Resize is never fenced by focus, so its generation is unused.
            Request::Resize { lease, size } => {
                self.input(lease, 0, vec![InputEvent::Resize(size)])?;
                Ok(Reply::Unchanged)
            }
            Request::CompleteClipboard {
                lease,
                generation,
                id,
                completion,
            } => {
                self.bound(lease)?.effect_done(generation, id, completion)?;
                Ok(Reply::Unchanged)
            }
            Request::Style { lease, style } => self
                .request(Operation::Style {
                    channel: channel(lease)?,
                    style,
                })
                .map(|_| Reply::Unchanged),
            Request::ClearScreenAndScrollback { lease } => self
                .request(Operation::ClearScrollback {
                    channel: channel(lease)?,
                })
                .map(|_| Reply::Unchanged),
            Request::GraphicsChunk {
                lease,
                image,
                offset,
            } => match self.request(Operation::Graphics {
                channel: channel(lease)?,
                image,
                offset: offset as u64,
            })? {
                ReplyBody::Graphics(chunk) => Ok(Reply::GraphicsChunk(chunk)),
                _ => Err(ServerError::Protocol),
            },
            Request::Accessibility {
                lease,
                revision,
                offset,
                base,
            } => match self.request(Operation::Accessibility {
                channel: channel(lease)?,
                revision,
                offset: offset as u64,
                base,
            })? {
                ReplyBody::Accessibility(chunk) => Ok(Reply::Accessibility(chunk)),
                _ => Err(ServerError::Protocol),
            },
            Request::AccessibilitySelection {
                lease,
                focus_generation,
                selection,
            } => {
                let body = self.request(Operation::AccessibilitySelection {
                    channel: channel(lease)?,
                    focus_generation,
                    selection,
                })?;
                match body {
                    ReplyBody::Done => Ok(Reply::Unchanged),
                    _ => Err(ServerError::Protocol),
                }
            }
            Request::EndAccessibilityDemand { lease } => self
                .request(Operation::EndAccessibility {
                    channel: channel(lease)?,
                })
                .map(|_| Reply::Unchanged),
            Request::Release { lease } => {
                self.bound(lease)?;
                // Dropping the handle sends `Release` in order behind earlier input.
                self.channel = None;
                Ok(Reply::Unchanged)
            }
            Request::Close { lease } => {
                self.request(Operation::Close {
                    channel: channel(lease)?,
                })?;
                if let Some((channel, _)) = self.channel.take() {
                    channel.detach();
                }
                Ok(Reply::Closed)
            }
            Request::Mutate { sequence, mutation } => {
                mutation_reply(self.request(Operation::Mutate { sequence, mutation })?)
            }
            // The shared Workspace client reads pushed inventory and resolves by dispatching.
            Request::Inventory | Request::Resolve { .. } => Err(ServerError::Unsupported),
            Request::Acknowledge { sequence } => {
                self.link.acknowledge(sequence)?;
                Ok(Reply::Unchanged)
            }
            Request::ReleaseAttachment { attachment } => self
                .request(Operation::ReleaseAttachment { attachment })
                .map(|_| Reply::Unchanged),
            Request::Shutdown => self.request(Operation::Shutdown).map(|_| Reply::Closed),
            // Claims need this Pane's attachment permit.
            Request::SelectPane { .. }
            | Request::StartRestored { .. }
            | Request::TakeOver { .. }
            | Request::RepairDirectory { .. }
            | Request::ClaimPane { .. } => Err(ServerError::Unsupported),
        }
    }
}

impl Drop for PaneLink {
    /// A dismissed presentation keeps its Pane's control, so the channel is detached, not
    /// released.
    fn drop(&mut self) {
        if let Some((channel, _)) = self.channel.take() {
            channel.detach();
        }
    }
}

/// Splits keys into Input messages within the protocol's event and byte bounds, in order.
fn key_batches(keys: Vec<pane::Key>) -> Vec<Vec<InputEvent>> {
    // Room for each key's fixed fields and the message envelope.
    const KEY_OVERHEAD: usize = 64;
    let budget = protocol::limits::MAX_INPUT_BYTES - 1024;
    let mut batches: Vec<Vec<InputEvent>> = Vec::new();
    let mut bytes = 0;
    for key in keys {
        let size = KEY_OVERHEAD + key.logical.len() + key.text.as_ref().map_or(0, String::len);
        let full = batches.last().is_none_or(|batch| {
            batch.len() >= protocol::limits::MAX_INPUT_EVENTS || bytes + size > budget
        });
        if full {
            batches.push(Vec::new());
            bytes = 0;
        }
        bytes += size;
        if let Some(batch) = batches.last_mut() {
            batch.push(InputEvent::Key(key));
        }
    }
    batches
}

/// The worker's reply to a shared edit: its validated outcome.
fn mutation_reply(body: ReplyBody) -> Result<Reply, ServerError> {
    match body {
        ReplyBody::Outcome(outcome) => Ok(Reply::Outcome(outcome)),
        _ => Err(ServerError::Protocol),
    }
}

impl TerminalSessionFactory for DevelopmentServerSessionFactory {
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
        appearance: TerminalAppearanceUpdate,
    ) -> Result<StartedTerminalSession, TerminalSessionError> {
        let (input, receiver) = mpsc::sync_channel(512);
        let (events, output) = async_channel::bounded(2);
        let (accessibility_sender, accessibility) = async_channel::bounded(1);
        let (effects, clipboard) = ClientEffects::new({
            let state = Arc::clone(&self.state);
            Arc::new(move || state.wake.notify())
        });
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
        // The worker cannot read the displays off the main thread.
        let max_hz = crate::platform::display_refresh::frame_rate();
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
                    (effects, max_hz),
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

/// Sends in this worker's turn. A retired or dismissed presentation sends nothing.
fn send(
    presenter: &control::Presenter,
    alive: &AtomicBool,
    connection: &mut PaneLink,
    request: &Request,
) -> Result<Reply, ServerError> {
    presenter.send(request, |request| {
        if alive.load(Ordering::Acquire) {
            connection.exchange(request)
        } else {
            Err(ServerError::NotRunning)
        }
    })
}

pub(super) fn expect_unchanged(reply: Reply) -> Result<(), ServerError> {
    if matches!(reply, Reply::Unchanged) {
        Ok(())
    } else {
        Err(ServerError::Protocol)
    }
}

pub(super) fn present(
    factory: DevelopmentServerSessionFactory,
    (reservation, alive): (control::Reservation, Arc<AtomicBool>),
    input: mpsc::Receiver<SubmittedInput>,
    (events, semantics): (
        async_channel::Sender<TerminalSessionEvent>,
        async_channel::Sender<Arc<super::super::accessibility::TerminalAccessibilityModel>>,
    ),
    size: Arc<Mutex<PaneSize>>,
    appearance: Arc<Mutex<TerminalAppearanceUpdate>>,
    (mut effects, max_hz): (ClientEffects, u8),
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
    let _control =
        claim::ControlWatch::start(&server, Arc::clone(&state), pane_id, attachment.as_ref());
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
            let link = server.link_cancellable(&|| !alive.load(Ordering::Acquire))?;
            let mut connection = PaneLink::new(link, attachment.clone(), {
                let state = Arc::clone(&state);
                Arc::new(move || state.wake.notify())
            });
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
                            connection.exchange(request)
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
            state.set_session_end(None);
            attached_session = true;
            // Authenticate and receive the authoritative screen before enabling input.
            let epoch = state.attach(&presenter)?;
            // Health is judged by time when the connection fails, so a hidden or idle Pane
            // whose worker never turns still renews recovery.
            state
                .connection_recovery
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .connected(std::time::Instant::now());
            drain_input(&state, &input);
            let mut active_input = ActiveInput {
                epoch,
                lease,
                focus_sent: None,
            };
            let mut active_size = grant_size;
            let mut ready = false;
            let mut graphics = graphics::Graphics::default();
            let mut semantic = accessibility::Fetcher::new(lease);
            let mut latest_screen = None;
            let mut shown = None;
            let mut screens = ScreenReceiver::new(local_files.as_ref(), remote_metadata.as_ref());
            let mut predictor = prediction::Predictor::default();
            // Cached rows survive a reconnect to the same Terminal Session.
            state
                .reading
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .scrollback
                .attach(lease.session);
            let mut demand = inbox::Demand::new(state.presentable.load(Ordering::Acquire), max_hz);
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
                // Queued input is sent before pushed output is applied. Input beyond the bound
                // goes on in the next turn without waiting.
                let more_input = active_input.drain(
                    &state,
                    &input,
                    &mut |request| {
                        // A send can wait for a reply, such as a settled paste, so the round trip
                        // starts before it. Only input that took a new serial is timed.
                        let (before, sent_at) =
                            (connection.last_input(), std::time::Instant::now());
                        let reply = send(&presenter, &alive, &mut connection, request)?;
                        let serial =
                            Some(connection.last_input()).filter(|serial| *serial > before);
                        predictor.sent(request, serial, sent_at);
                        if let Request::Resize { size, .. } = request {
                            active_size = *size;
                        }
                        Ok(reply)
                    },
                    &mut paste_unconfirmed,
                )?;
                for serial in connection.take_dropped() {
                    predictor.discarded(serial);
                }
                demand.set_visible(state.presentable.load(Ordering::Acquire));
                demand.set_preparation(state.preparation.load(Ordering::Acquire));
                if let Some((visible, max_hz)) = demand.take_view() {
                    expect_unchanged(send(
                        &presenter,
                        &alive,
                        &mut connection,
                        &Request::View {
                            lease,
                            visible,
                            max_hz,
                        },
                    )?)?;
                }
                // Frames are applied and acknowledged as they arrive; the view hears of the
                // newest visible screen once.
                let mut delivered = Vec::new();
                let flow = connection.drain(
                    &mut screen::Frames {
                        screens: &mut screens,
                        epoch,
                        generation: &mut generation,
                        active_size,
                        graphics: &mut graphics,
                        appearance: &appearance,
                        semantic: &mut semantic,
                        shown: &mut shown,
                        predictor: &mut predictor,
                    },
                    |delivery| delivered.push(delivery),
                )?;
                let mut presented = false;
                for delivery in delivered {
                    // A frame that arrived after a successor claimed the Pane is not published.
                    if !presenter.current() {
                        return Err(ServerError::NotRunning);
                    }
                    if state.epoch.load(Ordering::Acquire) != epoch {
                        return Err(ServerError::Protocol);
                    }
                    let (screen, mapping) = match delivery {
                        inbox::Delivery::Screen(screen) => screen,
                        inbox::Delivery::Pane(event) => {
                            match event {
                                PaneEvent::Effect(request) => effects.request(
                                    &state,
                                    active_input.focus_sent.map(|(generation, _)| generation),
                                    &events,
                                    request,
                                )?,
                                PaneEvent::Line(line) => effects.line(&state, &events, line)?,
                                PaneEvent::Accessibility { revision } => {
                                    semantic.note_revision(revision)
                                }
                                PaneEvent::Ended => {
                                    connection.release();
                                    state.set(DevelopmentPaneStatus::Ended);
                                    let _ = events.force_send(TerminalSessionEvent::Exited(
                                        TerminalSessionExit::Success,
                                    ));
                                    return Ok(());
                                }
                                // Another lease holds the Pane now; a new claim says how.
                                PaneEvent::SessionEnded | PaneEvent::Takeover(_) => {
                                    return Err(ServerError::StaleControl);
                                }
                                PaneEvent::DirectoryUnavailable => {
                                    return Err(ServerError::DirectoryUnavailable);
                                }
                                PaneEvent::Failed => return Err(ServerError::StartupFailed),
                                // The link settles input outcomes before they reach the inbox.
                                // Progress and run receipts are for CLI channels only.
                                PaneEvent::Input { .. }
                                | PaneEvent::Progress { .. }
                                | PaneEvent::RunFinished(_) => {}
                            }
                            continue;
                        }
                    };
                    presented = true;
                    if !ready {
                        // Claims apply their measured controller facts once.
                        // Reconcile changes made while attachment was in flight.
                        let current_size = *size.lock().unwrap_or_else(|e| e.into_inner());
                        let update = appearance.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        if current_size != grant_size {
                            expect_unchanged(send(
                                &presenter,
                                &alive,
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
                                &mut connection,
                                &Request::Style {
                                    lease,
                                    style: encode_style(&update),
                                },
                            )?)?;
                        }
                        {
                            let mut interaction =
                                state.interaction.lock().unwrap_or_else(|e| e.into_inner());
                            interaction.interactive = true;
                            interaction.reading = true;
                        }
                        // Publish the authorized screen first. Input stays queued until this
                        // worker's focus barrier is acknowledged.
                        ready = true;
                    }
                    state
                        .reading
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .predict(predictor.overlay());
                    let mapping = reading::show(
                        &state,
                        &events,
                        Some(Arc::clone(&screen)),
                        (epoch, generation, mapping),
                        paste_unconfirmed,
                    )?;
                    latest_screen = Some((screen, mapping));
                    if let Some(model) = semantic.rebind(&state, generation) {
                        let _ = semantics.force_send(model);
                    }
                    takeover = false;
                    if paste_unconfirmed {
                        paste_unconfirmed = false;
                        let _ = events.force_send(TerminalSessionEvent::PasteUnconfirmed);
                    }
                }
                if flow == inbox::Flow::Ended {
                    return Err(ServerError::Protocol);
                }
                if ready {
                    active_input.sync_focus(&state, &mut |request| {
                        send(&presenter, &alive, &mut connection, request)
                    })?;
                    effects.complete(&state, lease, &mut |request| {
                        send(&presenter, &alive, &mut connection, request)
                    })?;
                }
                // Graphics and semantic transfers continue one chunk per turn without waiting.
                let mut busy = more_input;
                let mut deadline = None;
                if ready && let Some((_, mapping)) = &mut latest_screen {
                    // Pages decode outside the reading lock, which the main thread takes for
                    // every key, pointer and wheel event; the lock is held only to add the rows.
                    let pages = state
                        .reading
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take_pages();
                    if !pages.is_empty() {
                        let decoded = reading::decode_pages(
                            pages,
                            mapping.size.columns,
                            local_files.as_ref(),
                        );
                        state
                            .reading
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .settle_pages(decoded);
                    }
                    let now = std::time::Instant::now();
                    predictor.expire(now);
                    let (turn, predicted) = {
                        let mut reading = state.reading.lock().unwrap_or_else(|e| e.into_inner());
                        (reading.turn(now), reading.predict(predictor.overlay()))
                    };
                    if turn.copy_ready {
                        let _ = events.force_send(TerminalSessionEvent::SelectionCopyReady);
                    }
                    // A changed prediction shows without waiting for a frame.
                    if turn.present || predicted {
                        generation = generation.next();
                        *mapping = reading::show(
                            &state,
                            &events,
                            None,
                            (epoch, generation, *mapping),
                            paste_unconfirmed,
                        )?;
                        if let Some(model) = semantic.rebind(&state, generation) {
                            let _ = semantics.force_send(model);
                        }
                    }
                    deadline = turn.deadline;
                    let requests = state
                        .reading
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .scrollback
                        .requests();
                    for (index, request) in requests.iter().copied().enumerate() {
                        let replies = Arc::clone(&state);
                        let sent = connection.scrollback_page(lease, request, move |page| {
                            replies
                                .reading
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .page(request, page);
                            replies.wake.notify();
                        });
                        if !matches!(sent, Ok(true)) {
                            // Requests the link did not take leave the in-flight count, and their
                            // rows may be asked for again.
                            state
                                .reading
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .unsent(&requests[index..]);
                            // A busy link takes them on a later turn.
                            sent?;
                            let retry = std::time::Instant::now() + reading::PAGE_RETRY;
                            deadline = Some(deadline.map_or(retry, |due| due.min(retry)));
                            break;
                        }
                    }
                }
                // A hidden Pane transfers no images. Every image the manifest names is fetched,
                // one chunk per turn, whether or not a newer frame arrives.
                if ready && demand.visible() && (presented || graphics.wants()) {
                    let result = graphics.poll(lease, &mut |request| {
                        send(&presenter, &alive, &mut connection, request)
                    })?;
                    if state.epoch.load(Ordering::Acquire) != epoch {
                        return Err(ServerError::Protocol);
                    }
                    let completed = matches!(result, Ok(true));
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
                        *mapping = reading::show(
                            &state,
                            &events,
                            Some(Arc::clone(screen)),
                            (epoch, generation, *mapping),
                            paste_unconfirmed,
                        )?;
                        if let Some(model) = semantic.rebind(&state, generation) {
                            let _ = semantics.force_send(model);
                        }
                    }
                    // A failed image waits for the next turn; a finished one goes on at once.
                    busy |= graphics.transferring() || (completed && graphics.wants());
                }
                // Semantic transfer follows its own explicit demand, even while hidden, and a
                // partial document continues one chunk per turn. It sends nothing while the
                // client holds the server's revision.
                if ready {
                    let model = semantic.poll(&state, generation, &mut |request| {
                        send(&presenter, &alive, &mut connection, request)
                    })?;
                    if state.epoch.load(Ordering::Acquire) != epoch {
                        return Err(ServerError::Protocol);
                    }
                    if let Some(model) = model {
                        let _ = semantics.force_send(model);
                    }
                }
                busy |= semantic.can_continue();
                // The worker waits for pushed events, input, a view change or a Scrollback page.
                // A selection drag past an edge, an unechoed prediction and a Development push
                // delay have deadlines.
                let deadline = [deadline, predictor.deadline(), connection.pushed_due()]
                    .into_iter()
                    .flatten()
                    .min();
                if !busy {
                    state.wake.wait(deadline);
                }
            })();
            if alive.load(Ordering::Acquire) {
                return attached;
            }
            // Dismissal, even mid-exchange, unfocuses the Pane in order; its control remains.
            presenter.retire(lease, |request| connection.exchange(request));
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
                // The frozen screen shows only what the server confirmed.
                let withdrawn = state
                    .reading
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .withdraw(generation.next());
                if let Some(screen) = withdrawn {
                    generation = generation.next();
                    let _ = events.force_send(TerminalSessionEvent::Screen(screen));
                }
                if matches!(error, ServerError::StaleControl | ServerError::Controlled) {
                    takeover = true;
                    paste_unconfirmed = false;
                    clear_presentation(&events, &mut generation, &appearance);
                }
                let retryable = state.connection_failed(error, takeover);
                // Retry waits for its backoff, or wakes for Retry, Take Over or dismissal. A
                // failure that needs the person waits for them alone.
                let backoff = std::time::Instant::now() + Duration::from_secs(1);
                while alive.load(Ordering::Acquire) && !state.retry.swap(false, Ordering::AcqRel) {
                    if retryable && std::time::Instant::now() >= backoff {
                        break;
                    }
                    state.wake.wait(retryable.then_some(backoff));
                }
            }
        }
    }
}

fn drain_input(state: &DevelopmentPaneState, input: &mpsc::Receiver<SubmittedInput>) {
    while let Ok(submitted) = input.try_recv() {
        discard_input(state, submitted.epoch, submitted.command);
    }
}

pub(super) fn encode_modifiers(m: InputModifiers) -> u16 {
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
pub(super) fn encode_key(k: KeyInput) -> pane::Key {
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

#[cfg(test)]
mod tests {
    use super::super::tests::Adapter;
    use super::*;

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

    /// Input queued while the worker was blocked can exceed one turn's bound. Each drain that
    /// stops at the bound says so, and the worker turns again without waiting, so the rest is
    /// sent even if the program shows nothing, as at a password prompt.
    #[test]
    fn a_drain_stopped_at_its_bound_reports_the_input_left_for_the_next_turn() {
        let adapter = Adapter::new();
        let Adapter {
            handle,
            input,
            mut active,
            ..
        } = adapter;
        for index in 0..40 {
            for key in typed(&format!("{}", index % 10)) {
                handle.key(key);
            }
        }
        let mut sent = Vec::new();
        let mut unconfirmed = false;
        let mut turns = Vec::new();
        loop {
            let more = active
                .drain(
                    &handle.state,
                    &input,
                    &mut |request| record(&mut sent, request),
                    &mut unconfirmed,
                )
                .unwrap();
            turns.push(more);
            if !more {
                break;
            }
        }

        assert_eq!(turns, [true, true, false], "80 events in bounds of 32");
        let keys: usize = sent
            .iter()
            .map(|sent| match sent {
                Sent::Batch(keys) => keys.len(),
                _ => 1,
            })
            .sum();
        assert_eq!(keys, 80);
    }

    /// Typing 53 characters queues 106 key events. They reach the server in at most four
    /// exchanges, every press and release in order.
    #[test]
    fn queued_keys_travel_in_bounded_batches() {
        let mut adapter = Adapter::new();
        let typed_text: Vec<String> = (0..53).map(|index| format!("{}", index % 10)).collect();
        for text in &typed_text {
            for key in typed(text) {
                adapter.handle.key(key);
            }
        }
        let mut sent = Vec::new();
        adapter
            .drain_rounds(8, |request| record(&mut sent, request))
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
        assert_eq!(sent.len(), 4, "106 events in at most four exchanges");
        assert!(sent.iter().all(|sent| matches!(
            sent,
            Sent::Batch(keys) if (1..=pane::MAX_KEY_BATCH).contains(&keys.len())
        )));
    }

    /// A batch holds only keys queued next to each other: any other command first sends the
    /// keys before it. Input method text keeps its own payload inside a batch.
    #[test]
    fn a_key_batch_ends_at_any_other_command_and_keeps_input_method_text() {
        use super::super::super::geometry::{BackingScale, CellGridSize, LogicalCellSize};
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
            .drain_rounds(1, |request| record(&mut sent, request))
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
        use crate::runtime::HostCommands as _;
        use sha2::{Digest, Sha256};
        let mut host = crate::runtime::directory_testing::terminal_host();
        let size = PaneSize {
            columns: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        };
        let lease = crate::runtime::directory_testing::started_pane(&mut host, size);
        let text = "界🙂".repeat(2200) + "尾";
        let expected = text.clone() + "!";
        let command = format!(
            r#"stty raw -echo; python3 -c 'import sys,hashlib; sys.stdout.write("READY\r\n"); sys.stdout.flush(); b=sys.stdin.buffer.read({}); sys.stdout.write(hashlib.sha256(b).hexdigest()+"\r\n"); sys.stdout.flush()'"#,
            expected.len()
        );
        let send = |host: &mut crate::runtime::SharedWorkspaceHost, key| {
            crate::runtime::directory_testing::type_keys(host, lease, vec![encode_key(key)])
        };
        let wait = |host: &mut crate::runtime::SharedWorkspaceHost, marker: Option<&str>| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                if crate::terminal::server_session::screen_decoder::observed_lines(host, lease)
                    .iter()
                    .any(|line| {
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
            .drain_rounds(2, |request| {
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
            let result = adapter.drain_rounds(1, |request| {
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
            let _ = adapter.drain_rounds(2, |_| panic!("an uncertain commit must never replay"));
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
            .drain_rounds(1, |request| {
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
            .drain_rounds(1, |request| record(&mut sent, request))
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
        let failed = adapter.drain_rounds(1, |request| {
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
        let _ = adapter.drain_rounds(2, |request| {
            if matches!(request, Request::Keys { .. }) {
                batches += 1;
            }
            Ok(Reply::Unchanged)
        });
        assert_eq!(batches, 1, "nothing is replayed");
    }
}
