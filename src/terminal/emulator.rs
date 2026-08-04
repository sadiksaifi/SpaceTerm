use std::cell::RefCell;
use std::mem;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use libghostty_vt::fmt::Format;
use libghostty_vt::key::{
    Action as KeyAction, Encoder as KeyEncoder, Event as KeyEvent, Key as GhosttyKey, Mods,
    OptionAsAlt,
};
use libghostty_vt::mouse::{
    Action as MouseAction, Button as MouseButton, Encoder as MouseEncoder,
    EncoderSize as MouseEncoderSize, Event as MouseEvent, Position as MousePosition,
};
use libghostty_vt::paste;
use libghostty_vt::render::{CellIterator, Dirty, RowIterator};
use libghostty_vt::screen::{CellWide, Screen};
use libghostty_vt::selection::FormatOptions;
use libghostty_vt::selection::gesture::{
    DragEvent, Geometry as SelectionGeometry, Gesture, PressEvent, ReleaseEvent,
};
use libghostty_vt::style::{PaletteIndex, RgbColor};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, ScrollViewport};
use libghostty_vt::{Error, RenderState, Terminal, TerminalOptions};

use crate::terminal::session::{
    InputModifiers, KeyCode, KeyInput, PointerButton, PointerInput, PointerPhase, SurfacePosition,
    WheelInput,
};
use crate::theme::{ACTIVE_THEME, Color};

const MAX_WHEEL_STEPS: i32 = 100;
const REPEAT_CLICK_DISTANCE_PX: f64 = 5.0;
const REPEAT_CLICK_INTERVAL: Duration = Duration::from_millis(500);

impl From<RgbColor> for Color {
    fn from(value: RgbColor) -> Self {
        Self::from_rgb_components(value.r, value.g, value.b)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CellSnapshot {
    pub(crate) text: String,
    pub(crate) foreground: Color,
    pub(crate) background: Color,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) cursor: bool,
    pub(crate) selected: bool,
    pub(crate) spacer_tail: bool,
}

pub(crate) type RowSnapshot = Arc<[CellSnapshot]>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ScrollbarSnapshot {
    pub(crate) total_rows: u64,
    pub(crate) offset_rows: u64,
    pub(crate) visible_rows: u64,
}

#[derive(Debug)]
pub(crate) struct ScreenSnapshot {
    pub(crate) rows: Arc<[RowSnapshot]>,
    pub(crate) background: Color,
    pub(crate) scrollbar: ScrollbarSnapshot,
}

impl ScreenSnapshot {
    pub(crate) fn empty() -> Arc<Self> {
        Arc::new(Self {
            rows: Arc::from([]),
            background: ACTIVE_THEME.terminal_background,
            scrollbar: ScrollbarSnapshot::default(),
        })
    }
}

pub(crate) struct TerminalEmulator {
    terminal: Terminal<'static, 'static>,
    render_state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    key_encoder: KeyEncoder<'static>,
    key_event: KeyEvent<'static>,
    mouse_encoder: MouseEncoder<'static>,
    mouse_event: MouseEvent<'static>,
    cached_mouse_modes: Option<MouseModeState>,
    cached_mouse_size: Option<MouseEncoderSize>,
    selection_gesture: Gesture<'static>,
    selection_press: PressEvent<'static>,
    selection_drag: DragEvent<'static>,
    selection_release: ReleaseEvent<'static>,
    pty_responses: Rc<RefCell<Vec<u8>>>,
    row_cache: Vec<RowSnapshot>,
    cached_cols: u16,
    cached_foreground: Option<Color>,
    cached_background: Option<Color>,
    cached_cursor: Option<(u16, u16)>,
    cached_scrollbar: Option<ScrollbarSnapshot>,
    cols: u16,
    rows_count: u16,
    cell_width_px: u32,
    cell_height_px: u32,
    active_pointer: Option<ActivePointer>,
    gesture_epoch: Instant,
}

#[derive(Clone, Copy, Debug)]
struct ActivePointer {
    button: PointerButton,
    route: PointerRoute,
}

#[derive(Clone, Copy, Debug)]
enum PointerRoute {
    Application,
    Selection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MouseModeState {
    x10: bool,
    normal: bool,
    button: bool,
    any: bool,
    utf8: bool,
    sgr: bool,
    urxvt: bool,
    sgr_pixels: bool,
}

#[derive(Debug)]
pub(crate) struct EmulatorAction {
    pub(crate) bytes: Vec<u8>,
    pub(crate) screen_changed: bool,
}

impl EmulatorAction {
    fn bytes(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            screen_changed: false,
        }
    }

    pub(crate) fn screen_changed() -> Self {
        Self {
            bytes: Vec::new(),
            screen_changed: true,
        }
    }

    fn none() -> Self {
        Self {
            bytes: Vec::new(),
            screen_changed: false,
        }
    }
}

impl TerminalEmulator {
    pub(crate) fn new(
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> Result<Self, Error> {
        let pty_responses = Rc::new(RefCell::new(Vec::new()));
        let mut terminal: Terminal<'static, 'static> = Terminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback: 10_000,
        })?;

        apply_theme(&mut terminal)?;
        terminal.resize(cols, rows, cell_width_px, cell_height_px)?;
        terminal.on_pty_write({
            let pty_responses = Rc::clone(&pty_responses);
            move |_, data| pty_responses.borrow_mut().extend_from_slice(data)
        })?;

        let mut mouse_encoder = MouseEncoder::new()?;
        mouse_encoder.set_track_last_cell(true);

        Ok(Self {
            terminal,
            render_state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            key_encoder: KeyEncoder::new()?,
            key_event: KeyEvent::new()?,
            mouse_encoder,
            mouse_event: MouseEvent::new()?,
            cached_mouse_modes: None,
            cached_mouse_size: None,
            selection_gesture: Gesture::new()?,
            selection_press: PressEvent::new()?,
            selection_drag: DragEvent::new()?,
            selection_release: ReleaseEvent::new()?,
            pty_responses,
            row_cache: Vec::new(),
            cached_cols: 0,
            cached_foreground: None,
            cached_background: None,
            cached_cursor: None,
            cached_scrollbar: None,
            cols,
            rows_count: rows,
            cell_width_px,
            cell_height_px,
            active_pointer: None,
            gesture_epoch: Instant::now(),
        })
    }

    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }

    pub(crate) fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> Result<(), Error> {
        self.terminal
            .resize(cols, rows, cell_width_px, cell_height_px)?;
        self.cols = cols;
        self.rows_count = rows;
        self.cell_width_px = cell_width_px;
        self.cell_height_px = cell_height_px;
        Ok(())
    }

    pub(crate) fn take_pty_responses(&self) -> Vec<u8> {
        mem::take(&mut *self.pty_responses.borrow_mut())
    }

    pub(crate) fn key(&mut self, input: KeyInput) -> Result<EmulatorAction, String> {
        self.clear_selection()?;
        self.terminal.scroll_viewport(ScrollViewport::Bottom);
        let mut bytes = Vec::new();
        self.encode_key(&input, &mut bytes)?;
        Ok(EmulatorAction {
            bytes,
            screen_changed: true,
        })
    }

    pub(crate) fn pointer(&mut self, input: PointerInput) -> Result<EmulatorAction, String> {
        match input.phase {
            PointerPhase::Press => self.pointer_press(input),
            PointerPhase::Motion => self.pointer_motion(input),
            PointerPhase::Release => self.pointer_release(input),
        }
    }

    pub(crate) fn scroll_to(&mut self, offset_rows: u64) -> EmulatorAction {
        let row = usize::try_from(offset_rows).unwrap_or(usize::MAX);
        self.terminal.scroll_viewport(ScrollViewport::Row(row));
        EmulatorAction::screen_changed()
    }

    pub(crate) fn wheel(&mut self, input: WheelInput) -> Result<EmulatorAction, String> {
        let steps = input.steps.clamp(-MAX_WHEEL_STEPS, MAX_WHEEL_STEPS);
        if steps == 0 {
            return Ok(EmulatorAction::none());
        }

        let tracking = self
            .terminal
            .is_mouse_tracking()
            .map_err(|error| format!("failed to query terminal mouse tracking mode: {error}"))?;
        if tracking && !input.modifiers.shift {
            self.clear_selection()?;
            let button = if steps > 0 {
                MouseButton::Four
            } else {
                MouseButton::Five
            };
            let any_button_pressed = self.active_pointer.is_some();
            let mut bytes = Vec::new();
            for _ in 0..steps.unsigned_abs() {
                self.encode_mouse_event(
                    MouseAction::Press,
                    Some(button),
                    input.position,
                    input.modifiers,
                    any_button_pressed,
                    &mut bytes,
                )?;
            }
            return Ok(EmulatorAction {
                bytes,
                screen_changed: true,
            });
        }

        let alternate_screen = self
            .terminal
            .active_screen()
            .map_err(|error| format!("failed to query the active terminal screen: {error}"))?
            == Screen::Alternate;
        let alternate_scroll = self
            .terminal
            .mode(Mode::ALT_SCROLL)
            .map_err(|error| format!("failed to query alternate-scroll mode: {error}"))?;
        if alternate_screen && alternate_scroll {
            self.clear_selection()?;
            let key = KeyInput {
                code: if steps > 0 {
                    KeyCode::ArrowUp
                } else {
                    KeyCode::ArrowDown
                },
                text: None,
                modifiers: InputModifiers::default(),
            };
            let mut bytes = Vec::new();
            for _ in 0..steps.unsigned_abs() {
                self.encode_key(&key, &mut bytes)?;
            }
            return Ok(EmulatorAction {
                bytes,
                screen_changed: true,
            });
        }

        self.terminal
            .scroll_viewport(ScrollViewport::Delta(-(steps as isize)));
        Ok(EmulatorAction::screen_changed())
    }

    pub(crate) fn paste(&mut self, text: String) -> Result<EmulatorAction, String> {
        self.clear_selection()?;
        let bracketed = self
            .terminal
            .mode(Mode::BRACKETED_PASTE)
            .map_err(|error| format!("failed to query bracketed-paste mode: {error}"))?;
        let mut source = text.into_bytes();
        let mut bytes = vec![0; source.len()];

        let written = loop {
            match paste::encode(&mut source, bracketed, &mut bytes) {
                Ok(written) => break written,
                Err(Error::OutOfSpace { required }) => {
                    let grown = required.max(bytes.len().saturating_add(16));
                    bytes.resize(grown, 0);
                }
                Err(error) => return Err(format!("failed to encode terminal paste: {error}")),
            }
        };
        bytes.truncate(written);
        self.terminal.scroll_viewport(ScrollViewport::Bottom);

        Ok(EmulatorAction {
            bytes,
            screen_changed: true,
        })
    }

    pub(crate) fn selection_text(&self) -> Result<Option<String>, String> {
        let options = FormatOptions::new()
            .with_emit_format(Format::Plain)
            .with_unwrap(true)
            .with_trim(true);
        let Some(bytes) = self
            .terminal
            .format_selection_alloc(None, options)
            .map_err(|error| format!("failed to format terminal selection: {error}"))?
        else {
            return Ok(None);
        };

        String::from_utf8(bytes.as_ref().to_vec())
            .map(Some)
            .map_err(|error| {
                format!(
                    "terminal selection contained invalid UTF-8 at byte {}",
                    error.utf8_error().valid_up_to()
                )
            })
    }

    fn pointer_press(&mut self, input: PointerInput) -> Result<EmulatorAction, String> {
        if self.active_pointer.is_some() {
            return Ok(EmulatorAction::none());
        }

        let tracking = self
            .terminal
            .is_mouse_tracking()
            .map_err(|error| format!("failed to query terminal mouse tracking mode: {error}"))?;
        let Some(button) = input.button else {
            return Ok(EmulatorAction::none());
        };
        let route = match button {
            PointerButton::Left => {
                if input.modifiers.shift || !tracking {
                    PointerRoute::Selection
                } else {
                    PointerRoute::Application
                }
            }
            PointerButton::Middle | PointerButton::Right if tracking => PointerRoute::Application,
            PointerButton::Middle | PointerButton::Right => {
                self.active_pointer = None;
                return Ok(EmulatorAction::none());
            }
        };
        self.active_pointer = Some(ActivePointer { button, route });

        match route {
            PointerRoute::Application => {
                self.clear_selection()?;
                let mut bytes = Vec::new();
                self.encode_mouse_event(
                    MouseAction::Press,
                    Some(mouse_button(button)),
                    input.position,
                    input.modifiers,
                    true,
                    &mut bytes,
                )?;
                Ok(EmulatorAction {
                    bytes,
                    screen_changed: true,
                })
            }
            PointerRoute::Selection => {
                self.selection_press(input.position)?;
                Ok(EmulatorAction::screen_changed())
            }
        }
    }

    fn pointer_motion(&mut self, input: PointerInput) -> Result<EmulatorAction, String> {
        match self.active_pointer {
            Some(ActivePointer {
                button,
                route: PointerRoute::Application,
            }) => {
                let mut bytes = Vec::new();
                self.encode_mouse_event(
                    MouseAction::Motion,
                    Some(mouse_button(button)),
                    input.position,
                    input.modifiers,
                    true,
                    &mut bytes,
                )?;
                Ok(EmulatorAction::bytes(bytes))
            }
            Some(ActivePointer {
                route: PointerRoute::Selection,
                ..
            }) => {
                self.selection_drag(input.position)?;
                Ok(EmulatorAction::screen_changed())
            }
            None => {
                let tracking = self.terminal.is_mouse_tracking().map_err(|error| {
                    format!("failed to query terminal mouse tracking mode: {error}")
                })?;
                if !tracking || input.modifiers.shift {
                    return Ok(EmulatorAction::none());
                }

                let mut bytes = Vec::new();
                self.encode_mouse_event(
                    MouseAction::Motion,
                    input.button.map(mouse_button),
                    input.position,
                    input.modifiers,
                    input.button.is_some(),
                    &mut bytes,
                )?;
                Ok(EmulatorAction::bytes(bytes))
            }
        }
    }

    fn pointer_release(&mut self, input: PointerInput) -> Result<EmulatorAction, String> {
        let Some(active) = self.active_pointer else {
            return Ok(EmulatorAction::none());
        };
        if input.button != Some(active.button) {
            return Ok(EmulatorAction::none());
        }
        self.active_pointer = None;

        match active.route {
            PointerRoute::Application => {
                let mut bytes = Vec::new();
                self.encode_mouse_event(
                    MouseAction::Release,
                    Some(mouse_button(active.button)),
                    input.position,
                    input.modifiers,
                    false,
                    &mut bytes,
                )?;
                Ok(EmulatorAction::bytes(bytes))
            }
            PointerRoute::Selection => {
                self.selection_release(input.position)?;
                Ok(EmulatorAction::none())
            }
        }
    }

    fn encode_key(&mut self, input: &KeyInput, bytes: &mut Vec<u8>) -> Result<(), String> {
        let (text, unshifted) = match input.code {
            KeyCode::Character(character) => {
                if character.is_control() {
                    return Err("terminal character keys must be printable".to_owned());
                }
                let text = input.text.clone().unwrap_or_else(|| character.to_string());
                if text.chars().any(char::is_control) {
                    return Err("terminal key text must not contain control characters".to_owned());
                }
                (Some(text), unshifted_character(character))
            }
            _ => (None, '\0'),
        };

        self.key_encoder
            .set_options_from_terminal(&self.terminal)
            .set_macos_option_as_alt(OptionAsAlt::True);
        self.key_event
            .set_action(KeyAction::Press)
            .set_key(ghostty_key(input.code))
            .set_mods(key_modifiers(input.modifiers))
            .set_consumed_mods(Mods::empty())
            .set_composing(false)
            .set_utf8(text)
            .set_unshifted_codepoint(unshifted);
        self.key_encoder
            .encode_to_vec(&self.key_event, bytes)
            .map_err(|error| format!("failed to encode terminal key input: {error}"))
    }

    fn encode_mouse_event(
        &mut self,
        action: MouseAction,
        button: Option<MouseButton>,
        position: SurfacePosition,
        modifiers: InputModifiers,
        any_button_pressed: bool,
        bytes: &mut Vec<u8>,
    ) -> Result<(), String> {
        let modes = self.mouse_mode_state()?;
        if self.cached_mouse_modes != Some(modes) {
            self.mouse_encoder.set_options_from_terminal(&self.terminal);
            self.cached_mouse_modes = Some(modes);
        }

        let size = self.mouse_encoder_size();
        if self.cached_mouse_size != Some(size) {
            self.mouse_encoder.set_size(size);
            self.cached_mouse_size = Some(size);
        }
        self.mouse_encoder
            .set_any_button_pressed(any_button_pressed);
        self.mouse_event
            .set_action(action)
            .set_button(button)
            .set_mods(mouse_modifiers(modifiers))
            .set_position(MousePosition {
                x: position.x,
                y: position.y,
            });
        self.mouse_encoder
            .encode_to_vec(&self.mouse_event, bytes)
            .map_err(|error| format!("failed to encode terminal mouse event: {error}"))
    }

    fn clear_selection(&mut self) -> Result<(), String> {
        self.terminal
            .set_selection(None)
            .map_err(|error| format!("failed to clear terminal selection: {error}"))?;
        self.selection_gesture.reset(&self.terminal);
        Ok(())
    }

    fn selection_press(&mut self, position: SurfacePosition) -> Result<(), String> {
        let point = self.viewport_point(position);
        let grid_ref = self
            .terminal
            .grid_ref(Point::Viewport(point))
            .map_err(|error| format!("failed to resolve selection press position: {error}"))?;
        let selection = self
            .selection_press
            .set_position(f64::from(position.x), f64::from(position.y))
            .and_then(|event| event.set_repeat_distance(REPEAT_CLICK_DISTANCE_PX))
            .and_then(|event| event.set_time(self.gesture_epoch.elapsed()))
            .and_then(|event| event.set_repeat_interval(REPEAT_CLICK_INTERVAL))
            .and_then(|event| event.apply(&mut self.selection_gesture, &self.terminal, grid_ref))
            .map_err(|error| format!("failed to apply terminal selection press: {error}"))?;
        self.terminal
            .set_selection(selection.as_ref())
            .map_err(|error| format!("failed to install terminal selection: {error}"))?;
        Ok(())
    }

    fn selection_drag(&mut self, position: SurfacePosition) -> Result<(), String> {
        let point = self.viewport_point(position);
        let geometry = self.selection_geometry();
        let grid_ref = self
            .terminal
            .grid_ref(Point::Viewport(point))
            .map_err(|error| format!("failed to resolve selection drag position: {error}"))?;
        let selection = self
            .selection_drag
            .set_position(f64::from(position.x), f64::from(position.y))
            .and_then(|event| event.set_rectangle(false))
            .and_then(|event| {
                event.apply(
                    &mut self.selection_gesture,
                    &self.terminal,
                    grid_ref,
                    geometry,
                )
            })
            .map_err(|error| format!("failed to apply terminal selection drag: {error}"))?;
        self.terminal
            .set_selection(selection.as_ref())
            .map_err(|error| format!("failed to install terminal selection: {error}"))?;
        Ok(())
    }

    fn selection_release(&mut self, position: SurfacePosition) -> Result<(), String> {
        let point = self.viewport_point(position);
        let grid_ref = self
            .terminal
            .grid_ref(Point::Viewport(point))
            .map_err(|error| format!("failed to resolve selection release position: {error}"))?;
        self.selection_release
            .apply(&mut self.selection_gesture, &self.terminal, Some(grid_ref))
            .map_err(|error| format!("failed to apply terminal selection release: {error}"))
    }

    fn viewport_point(&self, position: SurfacePosition) -> PointCoordinate {
        let cell_width = self.cell_width_px.max(1) as f32;
        let cell_height = self.cell_height_px.max(1) as f32;
        let x = (position.x / cell_width)
            .floor()
            .clamp(0.0, f32::from(self.cols.saturating_sub(1))) as u16;
        let y = (position.y / cell_height)
            .floor()
            .clamp(0.0, f32::from(self.rows_count.saturating_sub(1))) as u32;
        PointCoordinate { x, y }
    }

    fn mouse_mode_state(&self) -> Result<MouseModeState, String> {
        let mode = |mode| {
            self.terminal
                .mode(mode)
                .map_err(|error| format!("failed to query terminal mouse encoder mode: {error}"))
        };
        Ok(MouseModeState {
            x10: mode(Mode::X10_MOUSE)?,
            normal: mode(Mode::NORMAL_MOUSE)?,
            button: mode(Mode::BUTTON_MOUSE)?,
            any: mode(Mode::ANY_MOUSE)?,
            utf8: mode(Mode::UTF8_MOUSE)?,
            sgr: mode(Mode::SGR_MOUSE)?,
            urxvt: mode(Mode::URXVT_MOUSE)?,
            sgr_pixels: mode(Mode::SGR_PIXELS_MOUSE)?,
        })
    }

    fn mouse_encoder_size(&self) -> MouseEncoderSize {
        MouseEncoderSize {
            screen_width: u32::from(self.cols).saturating_mul(self.cell_width_px),
            screen_height: u32::from(self.rows_count).saturating_mul(self.cell_height_px),
            cell_width: self.cell_width_px.max(1),
            cell_height: self.cell_height_px.max(1),
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        }
    }

    fn selection_geometry(&self) -> SelectionGeometry {
        SelectionGeometry {
            columns: u32::from(self.cols),
            cell_width: self.cell_width_px.max(1),
            padding_left: 0,
            screen_height: u32::from(self.rows_count).saturating_mul(self.cell_height_px.max(1)),
        }
    }

    pub(crate) fn snapshot(&mut self) -> Result<Option<Arc<ScreenSnapshot>>, Error> {
        let snapshot = self.render_state.update(&self.terminal)?;
        let dirty = snapshot.dirty()?;
        let rows = snapshot.rows()?;
        let cols = snapshot.cols()?;
        let colors = snapshot.colors()?;
        let cursor = if snapshot.cursor_visible()? {
            snapshot.cursor_viewport()?
        } else {
            None
        };
        let scrollbar = self.terminal.scrollbar()?;
        let scrollbar = ScrollbarSnapshot {
            total_rows: scrollbar.total,
            offset_rows: scrollbar.offset,
            visible_rows: scrollbar.len,
        };

        let default_foreground: Color = colors.foreground.into();
        let default_background: Color = colors.background.into();
        let cursor_position = cursor.as_ref().map(|cursor| (cursor.x, cursor.y));
        let rebuild_all = matches!(dirty, Dirty::Full)
            || self.row_cache.len() != usize::from(rows)
            || self.cached_cols != cols
            || self.cached_foreground != Some(default_foreground)
            || self.cached_background != Some(default_background);
        let cursor_changed = self.cached_cursor != cursor_position;
        let scrollbar_changed = self.cached_scrollbar != Some(scrollbar);

        if matches!(dirty, Dirty::Clean) && !rebuild_all && !cursor_changed && !scrollbar_changed {
            return Ok(None);
        }

        let mut rendered_rows = if rebuild_all {
            Vec::with_capacity(usize::from(rows))
        } else {
            self.row_cache.clone()
        };
        let mut row_index = 0_u16;
        {
            let mut row_iteration = self.rows.update(&snapshot)?;

            while let Some(row) = row_iteration.next() {
                let cursor_row_changed = cursor_changed
                    && (self.cached_cursor.is_some_and(|(_, y)| y == row_index)
                        || cursor_position.is_some_and(|(_, y)| y == row_index));
                let rebuild_row = rebuild_all || row.dirty()? || cursor_row_changed;

                if rebuild_row {
                    let selection = row.selection()?;
                    let mut rendered_cells = Vec::with_capacity(usize::from(cols));
                    let mut column_index = 0_u16;
                    let mut cell_iteration = self.cells.update(row)?;

                    while let Some(cell) = cell_iteration.next() {
                        let style = cell.style()?;
                        let mut foreground = cell
                            .fg_color()?
                            .map(Color::from)
                            .unwrap_or(default_foreground);
                        let mut background = cell
                            .bg_color()?
                            .map(Color::from)
                            .unwrap_or(default_background);

                        if style.inverse {
                            mem::swap(&mut foreground, &mut background);
                        }

                        let raw_cell = cell.raw_cell()?;
                        let spacer_tail = matches!(raw_cell.wide()?, CellWide::SpacerTail);
                        let text = if style.invisible || spacer_tail {
                            " ".to_owned()
                        } else {
                            let graphemes = cell.graphemes()?;
                            if graphemes.is_empty() {
                                " ".to_owned()
                            } else {
                                graphemes.into_iter().collect()
                            }
                        };

                        let is_cursor = cursor.as_ref().is_some_and(|cursor| {
                            cursor.x == column_index && cursor.y == row_index
                        });

                        rendered_cells.push(CellSnapshot {
                            text,
                            foreground,
                            background,
                            bold: style.bold,
                            italic: style.italic,
                            cursor: is_cursor,
                            selected: selection.is_some_and(|range| {
                                column_index >= range.start_x && column_index <= range.end_x
                            }),
                            spacer_tail,
                        });
                        column_index = column_index.saturating_add(1);
                    }

                    let rendered_row = Arc::<[CellSnapshot]>::from(rendered_cells);
                    if rebuild_all {
                        rendered_rows.push(rendered_row);
                    } else {
                        rendered_rows[usize::from(row_index)] = rendered_row;
                    }
                }

                row.set_dirty(false)?;
                row_index = row_index.saturating_add(1);
            }
        }
        snapshot.set_dirty(Dirty::Clean)?;

        self.row_cache = rendered_rows;
        self.cached_cols = cols;
        self.cached_foreground = Some(default_foreground);
        self.cached_background = Some(default_background);
        self.cached_cursor = cursor_position;
        self.cached_scrollbar = Some(scrollbar);

        Ok(Some(Arc::new(ScreenSnapshot {
            rows: Arc::from(self.row_cache.clone()),
            background: default_background,
            scrollbar,
        })))
    }
}

impl Drop for TerminalEmulator {
    fn drop(&mut self) {
        self.selection_gesture.reset(&self.terminal);
    }
}

fn ghostty_key(code: KeyCode) -> GhosttyKey {
    match code {
        KeyCode::Character(character) => ghostty_character_key(unshifted_character(character)),
        KeyCode::Enter => GhosttyKey::Enter,
        KeyCode::Backspace => GhosttyKey::Backspace,
        KeyCode::Tab => GhosttyKey::Tab,
        KeyCode::Escape => GhosttyKey::Escape,
        KeyCode::ArrowUp => GhosttyKey::ArrowUp,
        KeyCode::ArrowDown => GhosttyKey::ArrowDown,
        KeyCode::ArrowLeft => GhosttyKey::ArrowLeft,
        KeyCode::ArrowRight => GhosttyKey::ArrowRight,
        KeyCode::Home => GhosttyKey::Home,
        KeyCode::End => GhosttyKey::End,
        KeyCode::PageUp => GhosttyKey::PageUp,
        KeyCode::PageDown => GhosttyKey::PageDown,
        KeyCode::Insert => GhosttyKey::Insert,
        KeyCode::Delete => GhosttyKey::Delete,
    }
}

fn ghostty_character_key(character: char) -> GhosttyKey {
    match character {
        '`' => GhosttyKey::Backquote,
        '\\' => GhosttyKey::Backslash,
        '[' => GhosttyKey::BracketLeft,
        ']' => GhosttyKey::BracketRight,
        ',' => GhosttyKey::Comma,
        '0' => GhosttyKey::Digit0,
        '1' => GhosttyKey::Digit1,
        '2' => GhosttyKey::Digit2,
        '3' => GhosttyKey::Digit3,
        '4' => GhosttyKey::Digit4,
        '5' => GhosttyKey::Digit5,
        '6' => GhosttyKey::Digit6,
        '7' => GhosttyKey::Digit7,
        '8' => GhosttyKey::Digit8,
        '9' => GhosttyKey::Digit9,
        '=' => GhosttyKey::Equal,
        'a' | 'A' => GhosttyKey::A,
        'b' | 'B' => GhosttyKey::B,
        'c' | 'C' => GhosttyKey::C,
        'd' | 'D' => GhosttyKey::D,
        'e' | 'E' => GhosttyKey::E,
        'f' | 'F' => GhosttyKey::F,
        'g' | 'G' => GhosttyKey::G,
        'h' | 'H' => GhosttyKey::H,
        'i' | 'I' => GhosttyKey::I,
        'j' | 'J' => GhosttyKey::J,
        'k' | 'K' => GhosttyKey::K,
        'l' | 'L' => GhosttyKey::L,
        'm' | 'M' => GhosttyKey::M,
        'n' | 'N' => GhosttyKey::N,
        'o' | 'O' => GhosttyKey::O,
        'p' | 'P' => GhosttyKey::P,
        'q' | 'Q' => GhosttyKey::Q,
        'r' | 'R' => GhosttyKey::R,
        's' | 'S' => GhosttyKey::S,
        't' | 'T' => GhosttyKey::T,
        'u' | 'U' => GhosttyKey::U,
        'v' | 'V' => GhosttyKey::V,
        'w' | 'W' => GhosttyKey::W,
        'x' | 'X' => GhosttyKey::X,
        'y' | 'Y' => GhosttyKey::Y,
        'z' | 'Z' => GhosttyKey::Z,
        '-' => GhosttyKey::Minus,
        '.' => GhosttyKey::Period,
        '\'' => GhosttyKey::Quote,
        ';' => GhosttyKey::Semicolon,
        '/' => GhosttyKey::Slash,
        ' ' => GhosttyKey::Space,
        _ => GhosttyKey::Unidentified,
    }
}

fn unshifted_character(character: char) -> char {
    match character {
        '~' => '`',
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        character if character.is_ascii_uppercase() => character.to_ascii_lowercase(),
        character => character,
    }
}

fn key_modifiers(modifiers: InputModifiers) -> Mods {
    let mut result = Mods::empty();
    result.set(Mods::SHIFT, modifiers.shift);
    result.set(Mods::ALT, modifiers.alt);
    result.set(Mods::CTRL, modifiers.control);
    result.set(Mods::SUPER, modifiers.platform);
    result
}

fn mouse_button(button: PointerButton) -> MouseButton {
    match button {
        PointerButton::Left => MouseButton::Left,
        PointerButton::Middle => MouseButton::Middle,
        PointerButton::Right => MouseButton::Right,
    }
}

fn mouse_modifiers(modifiers: InputModifiers) -> Mods {
    let mut result = Mods::empty();
    result.set(Mods::SHIFT, modifiers.shift);
    result.set(Mods::ALT, modifiers.alt);
    result.set(Mods::CTRL, modifiers.control);
    result.set(Mods::SUPER, modifiers.platform);
    result
}

const ANSI_NORMAL_INDICES: [PaletteIndex; 8] = [
    PaletteIndex::BLACK,
    PaletteIndex::RED,
    PaletteIndex::GREEN,
    PaletteIndex::YELLOW,
    PaletteIndex::BLUE,
    PaletteIndex::MAGENTA,
    PaletteIndex::CYAN,
    PaletteIndex::WHITE,
];

const ANSI_BRIGHT_INDICES: [PaletteIndex; 8] = [
    PaletteIndex::BRIGHT_BLACK,
    PaletteIndex::BRIGHT_RED,
    PaletteIndex::BRIGHT_GREEN,
    PaletteIndex::BRIGHT_YELLOW,
    PaletteIndex::BRIGHT_BLUE,
    PaletteIndex::BRIGHT_MAGENTA,
    PaletteIndex::BRIGHT_CYAN,
    PaletteIndex::BRIGHT_WHITE,
];

fn apply_theme(terminal: &mut Terminal<'static, 'static>) -> Result<(), libghostty_vt::Error> {
    let theme = ACTIVE_THEME;
    terminal
        .set_default_fg_color(Some(ghostty_color(theme.terminal_foreground)))?
        .set_default_bg_color(Some(ghostty_color(theme.terminal_background)))?
        .set_default_cursor_color(Some(ghostty_color(theme.terminal_foreground)))?;

    let mut palette = terminal.default_color_palette()?;
    for (index, color) in ANSI_NORMAL_INDICES
        .into_iter()
        .zip(theme.terminal_normal())
        .chain(ANSI_BRIGHT_INDICES.into_iter().zip(theme.terminal_bright()))
    {
        palette.set(index, ghostty_color(color));
    }
    terminal.set_default_color_palette(Some(palette))?;

    Ok(())
}

fn ghostty_color(color: Color) -> RgbColor {
    RgbColor {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emulator(cols: u16, rows: u16) -> TerminalEmulator {
        TerminalEmulator::new(cols, rows, 10, 20).unwrap()
    }

    fn pointer(
        phase: PointerPhase,
        button: Option<PointerButton>,
        x: f32,
        y: f32,
        shift: bool,
    ) -> PointerInput {
        PointerInput {
            phase,
            button,
            position: SurfacePosition { x, y },
            modifiers: InputModifiers {
                shift,
                ..InputModifiers::default()
            },
        }
    }

    fn wheel(steps: i32, shift: bool) -> WheelInput {
        WheelInput {
            steps,
            position: SurfacePosition { x: 1.0, y: 1.0 },
            modifiers: InputModifiers {
                shift,
                ..InputModifiers::default()
            },
        }
    }

    fn key(code: KeyCode, modifiers: InputModifiers) -> KeyInput {
        KeyInput {
            code,
            text: None,
            modifiers,
        }
    }

    fn select_first_five(emulator: &mut TerminalEmulator, shift: bool) {
        emulator
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                2.0,
                10.0,
                shift,
            ))
            .unwrap();
        emulator
            .pointer(pointer(PointerPhase::Motion, None, 48.0, 10.0, false))
            .unwrap();
        emulator
            .pointer(pointer(
                PointerPhase::Release,
                Some(PointerButton::Left),
                48.0,
                10.0,
                false,
            ))
            .unwrap();
        assert_eq!(emulator.selection_text().unwrap(), Some("hello".to_owned()));
    }

    fn row_text(snapshot: &ScreenSnapshot, row: usize) -> String {
        snapshot.rows[row]
            .iter()
            .map(|cell| cell.text.as_str())
            .collect()
    }

    #[test]
    fn vt_sequences_update_the_screen_without_leaking_escape_bytes() {
        let mut emulator = emulator(12, 3);
        emulator.feed(b"hello\r\n\x1b[31mred\x1b[0m");

        let snapshot = emulator.snapshot().unwrap().unwrap();
        let first_row = snapshot.rows[0]
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>();
        let second_row = snapshot.rows[1]
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>();

        assert!(first_row.starts_with("hello"));
        assert!(second_row.starts_with("red"));
        assert!(!first_row.contains('\x1b'));
        assert_eq!(
            snapshot.rows[1][0].foreground,
            ACTIVE_THEME.terminal_normal()[1]
        );
    }

    #[test]
    fn resize_changes_the_visible_grid() {
        let mut emulator = emulator(10, 2);
        emulator.resize(20, 4, 8, 18).unwrap();

        let snapshot = emulator.snapshot().unwrap().unwrap();
        assert_eq!(snapshot.rows.len(), 4);
        assert!(snapshot.rows.iter().all(|row| row.len() == 20));
    }

    #[test]
    fn resize_emits_in_band_size_responses() {
        let mut emulator = emulator(10, 2);
        emulator.feed(b"\x1b[?2048h");
        assert!(emulator.take_pty_responses().is_empty());

        emulator.resize(20, 4, 8, 18).unwrap();
        assert_eq!(emulator.take_pty_responses(), b"\x1b[48;4;20;72;160t");
    }

    #[test]
    fn key_encoding_tracks_cursor_mode_and_modifiers() {
        let mut emulator = emulator(10, 2);
        assert_eq!(
            emulator
                .key(key(KeyCode::ArrowUp, InputModifiers::default()))
                .unwrap()
                .bytes,
            b"\x1b[A"
        );

        emulator.feed(b"\x1b[?1h");
        assert_eq!(
            emulator
                .key(key(KeyCode::ArrowUp, InputModifiers::default()))
                .unwrap()
                .bytes,
            b"\x1bOA"
        );

        let printable = KeyInput {
            code: KeyCode::Character('é'),
            text: Some("é".to_owned()),
            modifiers: InputModifiers::default(),
        };
        assert_eq!(emulator.key(printable).unwrap().bytes, "é".as_bytes());

        let control_c = KeyInput {
            code: KeyCode::Character('c'),
            text: Some("c".to_owned()),
            modifiers: InputModifiers {
                control: true,
                ..InputModifiers::default()
            },
        };
        assert_eq!(emulator.key(control_c).unwrap().bytes, b"\x03");

        let alt_x = KeyInput {
            code: KeyCode::Character('x'),
            text: Some("x".to_owned()),
            modifiers: InputModifiers {
                alt: true,
                ..InputModifiers::default()
            },
        };
        assert_eq!(emulator.key(alt_x).unwrap().bytes, b"\x1bx");
    }

    #[test]
    fn named_keys_use_ghostty_key_encoding() {
        let mut emulator = emulator(10, 2);
        let cases: &[(KeyCode, InputModifiers, &[u8])] = &[
            (KeyCode::Enter, InputModifiers::default(), b"\r"),
            (KeyCode::Backspace, InputModifiers::default(), b"\x7f"),
            (KeyCode::Tab, InputModifiers::default(), b"\t"),
            (
                KeyCode::Tab,
                InputModifiers {
                    shift: true,
                    ..InputModifiers::default()
                },
                b"\x1b[Z",
            ),
            (KeyCode::Escape, InputModifiers::default(), b"\x1b"),
            (KeyCode::ArrowDown, InputModifiers::default(), b"\x1b[B"),
            (KeyCode::ArrowLeft, InputModifiers::default(), b"\x1b[D"),
            (KeyCode::ArrowRight, InputModifiers::default(), b"\x1b[C"),
            (KeyCode::Home, InputModifiers::default(), b"\x1b[H"),
            (KeyCode::End, InputModifiers::default(), b"\x1b[F"),
            (KeyCode::PageUp, InputModifiers::default(), b"\x1b[5~"),
            (KeyCode::PageDown, InputModifiers::default(), b"\x1b[6~"),
            (KeyCode::Insert, InputModifiers::default(), b"\x1b[2~"),
            (KeyCode::Delete, InputModifiers::default(), b"\x1b[3~"),
        ];

        for (code, modifiers, expected) in cases {
            assert_eq!(
                emulator.key(key(*code, *modifiers)).unwrap().bytes,
                *expected,
                "unexpected encoding for {code:?}"
            );
        }
    }

    #[test]
    fn clean_screens_do_not_publish_another_snapshot() {
        let mut emulator = emulator(10, 2);

        assert!(emulator.snapshot().unwrap().is_some());
        assert!(emulator.snapshot().unwrap().is_none());
    }

    #[test]
    fn unchanged_rows_reuse_their_cell_storage() {
        let mut emulator = emulator(10, 3);
        let first = emulator.snapshot().unwrap().unwrap();

        emulator.feed(b"x");
        let second = emulator.snapshot().unwrap().unwrap();

        assert!(!Arc::ptr_eq(&first.rows[0], &second.rows[0]));
        assert!(Arc::ptr_eq(&first.rows[1], &second.rows[1]));
        assert!(Arc::ptr_eq(&first.rows[2], &second.rows[2]));
    }

    #[test]
    fn selection_is_rendered_and_formats_as_plain_text() {
        let mut emulator = emulator(12, 3);
        emulator.feed(b"hello world");
        _ = emulator.snapshot().unwrap();
        assert_eq!(emulator.selection_text().unwrap(), None);

        emulator
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                2.0,
                10.0,
                false,
            ))
            .unwrap();
        emulator
            .pointer(pointer(PointerPhase::Motion, None, 48.0, 10.0, false))
            .unwrap();
        emulator
            .pointer(pointer(
                PointerPhase::Release,
                Some(PointerButton::Left),
                48.0,
                10.0,
                false,
            ))
            .unwrap();

        let snapshot = emulator.snapshot().unwrap().unwrap();
        assert!(snapshot.rows[0][..5].iter().all(|cell| cell.selected));
        assert!(!snapshot.rows[0][5].selected);
        assert_eq!(emulator.selection_text().unwrap(), Some("hello".to_owned()));
    }

    #[test]
    fn scrollback_wheel_changes_visible_rows() {
        let mut emulator = emulator(10, 2);
        emulator.feed(b"one\r\ntwo\r\nthree");
        let bottom = emulator.snapshot().unwrap().unwrap();
        assert!(row_text(&bottom, 0).starts_with("two"));
        assert!(row_text(&bottom, 1).starts_with("three"));
        assert_eq!(
            bottom
                .scrollbar
                .offset_rows
                .saturating_add(bottom.scrollbar.visible_rows),
            bottom.scrollbar.total_rows
        );

        let action = emulator.wheel(wheel(1, false)).unwrap();
        assert!(action.bytes.is_empty());
        assert!(action.screen_changed);

        let scrolled = emulator.snapshot().unwrap().unwrap();
        assert!(row_text(&scrolled, 0).starts_with("one"));
        assert!(row_text(&scrolled, 1).starts_with("two"));
        assert!(
            scrolled
                .scrollbar
                .offset_rows
                .saturating_add(scrolled.scrollbar.visible_rows)
                < scrolled.scrollbar.total_rows
        );
        assert!(scrolled.scrollbar.offset_rows < bottom.scrollbar.offset_rows);

        let action = emulator.scroll_to(bottom.scrollbar.offset_rows);
        assert!(action.bytes.is_empty());
        assert!(action.screen_changed);
        let restored = emulator.snapshot().unwrap().unwrap();
        assert!(row_text(&restored, 0).starts_with("two"));
        assert!(row_text(&restored, 1).starts_with("three"));
    }

    #[test]
    fn tracked_and_alternate_screen_wheel_events_encode_input() {
        let mut tracked = emulator(10, 2);
        tracked.feed(b"\x1b[?1000h\x1b[?1006h");
        let reported = tracked.wheel(wheel(2, false)).unwrap();
        assert_eq!(reported.bytes, b"\x1b[<64;1;1M\x1b[<64;1;1M");
        assert!(reported.screen_changed);

        let mut alternate = emulator(10, 2);
        alternate.feed(b"\x1b[?1049h\x1b[?1007h");
        assert_eq!(
            alternate.wheel(wheel(2, false)).unwrap().bytes,
            b"\x1b[A\x1b[A"
        );
        alternate.feed(b"\x1b[?1h");
        assert_eq!(alternate.wheel(wheel(-1, false)).unwrap().bytes, b"\x1bOB");
    }

    #[test]
    fn cell_mouse_motion_is_deduplicated_without_losing_encoder_state() {
        let mut emulator = emulator(10, 2);
        emulator.feed(b"\x1b[?1003h\x1b[?1006h");

        let first = emulator
            .pointer(pointer(PointerPhase::Motion, None, 1.0, 1.0, false))
            .unwrap();
        let same_cell = emulator
            .pointer(pointer(PointerPhase::Motion, None, 9.0, 19.0, false))
            .unwrap();
        let next_cell = emulator
            .pointer(pointer(PointerPhase::Motion, None, 11.0, 1.0, false))
            .unwrap();

        assert_eq!(first.bytes, b"\x1b[<35;1;1M");
        assert!(same_cell.bytes.is_empty());
        assert_eq!(next_cell.bytes, b"\x1b[<35;2;1M");
    }

    #[test]
    fn paste_encoding_tracks_bracketed_paste_mode() {
        let mut emulator = emulator(10, 2);
        let plain = emulator.paste("one\ntwo".to_owned()).unwrap();
        assert_eq!(plain.bytes, b"one\rtwo");

        emulator.feed(b"\x1b[?2004h");
        let bracketed = emulator.paste("one\ntwo".to_owned()).unwrap();
        assert_eq!(bracketed.bytes, b"\x1b[200~one\ntwo\x1b[201~");
    }

    #[test]
    fn mouse_tracking_reports_bytes_but_shift_overrides_with_selection() {
        let mut emulator = emulator(12, 3);
        emulator.feed(b"hello world\x1b[?1000h\x1b[?1006h");
        _ = emulator.snapshot().unwrap();

        let reported = emulator
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                2.0,
                10.0,
                false,
            ))
            .unwrap();
        assert!(!reported.bytes.is_empty());
        let released = emulator
            .pointer(pointer(
                PointerPhase::Release,
                Some(PointerButton::Left),
                2.0,
                10.0,
                true,
            ))
            .unwrap();
        assert!(!released.bytes.is_empty());

        let selected_press = emulator
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                2.0,
                10.0,
                true,
            ))
            .unwrap();
        let selected_drag = emulator
            .pointer(pointer(PointerPhase::Motion, None, 48.0, 10.0, false))
            .unwrap();
        assert!(selected_press.bytes.is_empty());
        assert!(selected_drag.bytes.is_empty());
        assert!(selected_drag.screen_changed);

        let snapshot = emulator.snapshot().unwrap().unwrap();
        assert!(snapshot.rows[0][..5].iter().all(|cell| cell.selected));
    }

    #[test]
    fn application_mouse_routes_clear_selection_but_hover_does_not() {
        let mut pressed = emulator(12, 3);
        pressed.feed(b"hello world");
        select_first_five(&mut pressed, false);
        pressed.feed(b"\x1b[?1000h\x1b[?1006h");
        let action = pressed
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                2.0,
                10.0,
                false,
            ))
            .unwrap();
        assert!(action.screen_changed);
        assert_eq!(pressed.selection_text().unwrap(), None);

        let mut tracked_wheel = emulator(12, 3);
        tracked_wheel.feed(b"hello world\x1b[?1000h\x1b[?1006h");
        select_first_five(&mut tracked_wheel, true);
        let action = tracked_wheel.wheel(wheel(1, false)).unwrap();
        assert!(action.screen_changed);
        assert_eq!(tracked_wheel.selection_text().unwrap(), None);

        let mut alternate_wheel = emulator(12, 3);
        alternate_wheel.feed(b"\x1b[?1049hhello world\x1b[?1007h");
        select_first_five(&mut alternate_wheel, false);
        let action = alternate_wheel.wheel(wheel(1, false)).unwrap();
        assert!(action.screen_changed);
        assert_eq!(alternate_wheel.selection_text().unwrap(), None);

        let mut hover = emulator(12, 3);
        hover.feed(b"hello world");
        select_first_five(&mut hover, false);
        hover.feed(b"\x1b[?1003h\x1b[?1006h");
        let action = hover
            .pointer(pointer(PointerPhase::Motion, None, 11.0, 1.0, false))
            .unwrap();
        assert!(!action.screen_changed);
        assert_eq!(hover.selection_text().unwrap(), Some("hello".to_owned()));
    }

    #[test]
    fn additional_presses_and_mismatched_releases_do_not_replace_active_route() {
        let mut emulator = emulator(10, 2);
        emulator.feed(b"\x1b[?1002h\x1b[?1006h");

        let first = emulator
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                1.0,
                1.0,
                false,
            ))
            .unwrap();
        let additional = emulator
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Right),
                1.0,
                1.0,
                false,
            ))
            .unwrap();
        let mismatched_release = emulator
            .pointer(pointer(
                PointerPhase::Release,
                Some(PointerButton::Right),
                1.0,
                1.0,
                false,
            ))
            .unwrap();
        let motion = emulator
            .pointer(pointer(
                PointerPhase::Motion,
                Some(PointerButton::Left),
                11.0,
                1.0,
                false,
            ))
            .unwrap();
        let release = emulator
            .pointer(pointer(
                PointerPhase::Release,
                Some(PointerButton::Left),
                11.0,
                1.0,
                false,
            ))
            .unwrap();

        assert_eq!(first.bytes, b"\x1b[<0;1;1M");
        assert!(additional.bytes.is_empty());
        assert!(mismatched_release.bytes.is_empty());
        assert_eq!(motion.bytes, b"\x1b[<32;2;1M");
        assert_eq!(release.bytes, b"\x1b[<0;2;1m");
    }

    #[test]
    fn auxiliary_buttons_and_hover_only_report_when_tracking_allows() {
        let mut emulator = emulator(10, 2);
        let ignored = emulator
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Middle),
                1.0,
                1.0,
                false,
            ))
            .unwrap();
        assert!(ignored.bytes.is_empty());

        emulator.feed(b"\x1b[?1003h\x1b[?1006h");
        let reported = emulator
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Right),
                1.0,
                1.0,
                true,
            ))
            .unwrap();
        assert!(!reported.bytes.is_empty());
        _ = emulator
            .pointer(pointer(
                PointerPhase::Release,
                Some(PointerButton::Right),
                1.0,
                1.0,
                true,
            ))
            .unwrap();

        let hover = emulator
            .pointer(pointer(PointerPhase::Motion, None, 11.0, 1.0, false))
            .unwrap();
        assert!(!hover.bytes.is_empty());
        let shifted_hover = emulator
            .pointer(pointer(PointerPhase::Motion, None, 21.0, 1.0, true))
            .unwrap();
        assert!(shifted_hover.bytes.is_empty());
    }
}
