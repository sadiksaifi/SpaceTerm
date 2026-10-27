//! The Pane worker's server connection, request sending, polling loop and input.
use super::*;

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
    pub(super) fn drain(
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
            let mut screens = ScreenReceiver::new(local_files.as_ref(), remote_metadata.as_ref());
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
                    screens.restart();
                }
                let polled = cadence.poll_due(now);
                if polled {
                    let reply = if changes {
                        let base = screens.baseline();
                        let reply = send(
                            &presenter,
                            &alive,
                            &attachment,
                            &mut connection,
                            &Request::PollChanges { lease, base },
                        )?;
                        screens.apply(reply)?
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
                        Reply::Screen(screen) => {
                            semantic.note_revision(screen.accessibility_revision);
                            let (screen, mapping) = screens.decode(
                                screen,
                                epoch,
                                &mut generation,
                                active_size,
                                &mut graphics,
                                &appearance,
                            )?;
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
}
