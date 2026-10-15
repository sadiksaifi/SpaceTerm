//! Desktop presentation subscription to the single Development server Pane.
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
    Key(pane::Key),
    Resize(PaneSize),
    Style(pane::PresentationStyle),
}
struct PresentationHandle {
    input: mpsc::SyncSender<(u64, Input)>,
    state: Arc<DevelopmentPaneState>,
    alive: Arc<AtomicBool>,
    size: Arc<Mutex<PaneSize>>,
    appearance: Arc<Mutex<TerminalAppearanceUpdate>>,
}
impl Drop for PresentationHandle {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
    }
}
impl PresentationHandle {
    fn send(&self, input: Input) {
        let epoch = self.state.epoch.load(Ordering::Acquire);
        if self.state.status() == DevelopmentPaneStatus::Connected
            && self.input.try_send((epoch, input)).is_err()
        {
            self.state.epoch.fetch_add(1, Ordering::AcqRel);
            self.state.set(DevelopmentPaneStatus::Reconnecting);
        }
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

fn present(
    factory: DevelopmentServerSessionFactory,
    alive: Arc<AtomicBool>,
    input: mpsc::Receiver<(u64, Input)>,
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
            let measured = *size.lock().unwrap_or_else(|e| e.into_inner());
            let style = Some(encode_style(
                &appearance.lock().unwrap_or_else(|e| e.into_inner()),
            ));
            let request = if let Some(pane) = pane_id {
                Request::SelectPane {
                    pane,
                    explicit: state.selected.swap(false, Ordering::AcqRel),
                    size: measured,
                    style,
                }
            } else if connection.supports_takeover() {
                Request::Select {
                    create: intent,
                    size: measured,
                    resume: expected_target,
                    style,
                }
            } else {
                Request::Attach {
                    create: intent,
                    size: measured,
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
                    state.invalidate();
                    state.start_restored.store(false, Ordering::Release);
                    while input.try_recv().is_ok() {}
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
                    reply = exchange(
                        &attachment,
                        &mut connection,
                        &Request::StartRestored {
                            server: boot,
                            pane,
                            size: *size.lock().unwrap_or_else(|e| e.into_inner()),
                            style: Some(encode_style(
                                &appearance.lock().unwrap_or_else(|e| e.into_inner()),
                            )),
                        },
                    )?;
                } else {
                    std::thread::sleep(Duration::from_millis(100));
                    let size = *size.lock().unwrap_or_else(|e| e.into_inner());
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
                    state.invalidate();
                    while input.try_recv().is_ok() {}
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
                    reply = exchange(
                        &attachment,
                        &mut connection,
                        &Request::TakeOver {
                            target,
                            size: *size.lock().unwrap_or_else(|e| e.into_inner()),
                            style: Some(encode_style(
                                &appearance.lock().unwrap_or_else(|e| e.into_inner()),
                            )),
                        },
                    )?;
                } else {
                    std::thread::sleep(Duration::from_millis(100));
                    let size = *size.lock().unwrap_or_else(|e| e.into_inner());
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
            while input.try_recv().is_ok() {}
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
                        let screen = decode_screen(
                            *screen,
                            generation,
                            &appearance.lock().unwrap_or_else(|e| e.into_inner()),
                        )?;
                        let _ = events.force_send(TerminalSessionEvent::Screen(screen));
                        state.connected_epoch.store(epoch, Ordering::Release);
                        takeover = false;
                        state.set(DevelopmentPaneStatus::Connected);
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
                    let Ok((submitted_epoch, command)) = input.try_recv() else {
                        break;
                    };
                    if state.epoch.load(Ordering::Acquire) != epoch {
                        return Err(ServerError::Protocol);
                    }
                    if epoch != submitted_epoch {
                        continue;
                    }
                    let request = match command {
                        Input::Key(key) => Request::Key { lease, key },
                        Input::Resize(size) => Request::Resize { lease, size },
                        Input::Style(style) => Request::Style { lease, style },
                    };
                    exchange(&attachment, &mut connection, &request)?;
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
                while input.try_recv().is_ok() {}
                if matches!(error, ServerError::StaleControl | ServerError::Controlled) {
                    takeover = true;
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
impl TerminalSessionHandle for PresentationHandle {
    fn key(&self, k: KeyInput) {
        if k.text.as_ref().is_some_and(|s| s.len() > 4096) || k.logical_key.len() > 128 {
            return;
        }
        self.send(Input::Key(pane::Key {
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
        }));
    }
    fn resize(&self, geometry: TerminalGeometry) {
        let size = pane_size(geometry);
        *self.size.lock().unwrap_or_else(|e| e.into_inner()) = size;
        self.send(Input::Resize(size));
    }
    fn focus(&self, _: bool) {}
    fn clear_screen_and_scrollback(&self) {}
    fn pointer(&self, _: super::pointer_input::PointerInput) {}
    fn pointer_and_copy_selection(
        &self,
        _: super::pointer_input::PointerInput,
    ) -> Result<Option<super::selection::SelectionCopy>, SelectionCopyError> {
        Ok(None)
    }
    fn wheel(&self, _: super::pointer_input::WheelInput) {}
    fn scroll_to(&self, _: u64, _: PresentationGeneration) {}
    fn scroll_scrollback(&self, _: ScrollbackMovement) {}
    fn set_find_query(&self, _: super::FindQueryGeneration, _: String) {}
    fn navigate_find(&self, _: super::FindQueryGeneration, _: super::FindDirection) {}
    fn end_find(&self, _: super::FindQueryGeneration) {}
    fn request_paste(
        &self,
        _: super::PastePayload,
    ) -> async_channel::Receiver<Result<super::PasteRequestOutcome, String>> {
        let (tx, rx) = async_channel::bounded(1);
        let _ = tx.try_send(Err("Paste is unavailable for this Development Pane.".into()));
        rx
    }
    fn resolve_paste(
        &self,
        _: super::paste::PasteConfirmationId,
        _: super::PasteDecision,
    ) -> async_channel::Receiver<Result<super::PasteResolution, String>> {
        let (tx, rx) = async_channel::bounded(1);
        let _ = tx.try_send(Err("Paste is unavailable for this Development Pane.".into()));
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
