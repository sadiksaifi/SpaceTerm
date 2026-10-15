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
    Connected,
    Reconnecting,
    Ended,
    Failed(ServerError),
}
pub(crate) struct DevelopmentPaneState {
    status: Mutex<DevelopmentPaneStatus>,
    retry: AtomicBool,
    close: AtomicBool,
    epoch: AtomicU64,
}
impl DevelopmentPaneState {
    pub(crate) fn status(&self) -> DevelopmentPaneStatus {
        *self.status.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn retry(&self) {
        self.retry.store(true, Ordering::Release);
    }
    pub(crate) fn close(&self) {
        if self.status() == DevelopmentPaneStatus::Connected {
            self.close.store(true, Ordering::Release);
        } else {
            self.set(DevelopmentPaneStatus::Failed(ServerError::NotRunning));
        }
    }
    fn set(&self, status: DevelopmentPaneStatus) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status;
    }
}
pub(crate) struct DevelopmentServerSessionFactory {
    server: LocalServer,
    state: Arc<DevelopmentPaneState>,
}
impl DevelopmentServerSessionFactory {
    pub(crate) fn new(server: LocalServer) -> (Self, Arc<DevelopmentPaneState>) {
        let state = Arc::new(DevelopmentPaneState {
            status: Mutex::new(DevelopmentPaneStatus::Connecting),
            retry: AtomicBool::new(false),
            close: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
        });
        (
            Self {
                server,
                state: Arc::clone(&state),
            },
            state,
        )
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
        if self.state.status() == DevelopmentPaneStatus::Connected
            && self
                .input
                .try_send((self.state.epoch.load(Ordering::Acquire), input))
                .is_err()
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
        let server = self.server.clone();
        let state = Arc::clone(&self.state);
        std::thread::Builder::new()
            .name("spaceterm-pane-connection".into())
            .spawn(move || {
                present(
                    server,
                    state,
                    alive,
                    receiver,
                    events,
                    measured_size,
                    appearance,
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

fn present(
    server: LocalServer,
    state: Arc<DevelopmentPaneState>,
    alive: Arc<AtomicBool>,
    input: mpsc::Receiver<(u64, Input)>,
    events: async_channel::Sender<TerminalSessionEvent>,
    size: Arc<Mutex<PaneSize>>,
    appearance: Arc<Mutex<TerminalAppearanceUpdate>>,
) {
    let mut create = true;
    let mut expected_session: Option<Lease> = None;
    let mut generation = PresentationGeneration::default();
    while alive.load(Ordering::Acquire) {
        state.retry.store(false, Ordering::Release);
        let connected = (|| {
            let mut connection = server.connect_pane()?;
            let intent = create;
            create = false;
            let Reply::Attached { lease } = connection.exchange(&Request::Attach {
                create: intent,
                size: *size.lock().unwrap_or_else(|e| e.into_inner()),
                resume: expected_session,
                style: Some(encode_style(
                    &appearance.lock().unwrap_or_else(|e| e.into_inner()),
                )),
            })?
            else {
                return Err(ServerError::Protocol);
            };
            if expected_session.is_some_and(|old| {
                (old.server, old.session, old.control)
                    != (lease.server, lease.session, lease.control)
            }) {
                return Err(ServerError::StaleControl);
            }
            expected_session = Some(lease);
            // Authenticate and receive the authoritative screen before enabling input.
            let epoch = state.epoch.fetch_add(1, Ordering::AcqRel) + 1;
            while input.try_recv().is_ok() {}
            loop {
                if !alive.load(Ordering::Acquire) {
                    return Ok(());
                }
                if state.epoch.load(Ordering::Acquire) != epoch {
                    return Err(ServerError::Protocol);
                }
                let reply = connection.exchange(&Request::Poll { lease })?;
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
                if state.close.swap(false, Ordering::AcqRel) {
                    if !matches!(
                        connection.exchange(&Request::Close { lease })?,
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
                    connection.exchange(&request)?;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        })();
        match connected {
            Ok(()) => return,
            Err(error) => {
                state.epoch.fetch_add(1, Ordering::AcqRel);
                while input.try_recv().is_ok() {}
                state.close.store(false, Ordering::Release);
                let retryable = matches!(error, ServerError::Protocol | ServerError::NotRunning);
                state.set(if retryable {
                    DevelopmentPaneStatus::Reconnecting
                } else {
                    DevelopmentPaneStatus::Failed(error)
                });
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
