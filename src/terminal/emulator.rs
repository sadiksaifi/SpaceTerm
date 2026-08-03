use std::cell::RefCell;
use std::mem;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use libghostty_vt::fmt::Format;
use libghostty_vt::key::Mods;
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
    InputModifiers, PointerButton, PointerInput, PointerPhase, SurfacePosition, WheelInput,
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
    #[allow(
        dead_code,
        reason = "selection rendering is exposed before the UI interaction milestone"
    )]
    pub(crate) selected: bool,
    pub(crate) spacer_tail: bool,
}

pub(crate) type RowSnapshot = Arc<[CellSnapshot]>;

#[derive(Debug)]
pub(crate) struct ScreenSnapshot {
    pub(crate) rows: Arc<[RowSnapshot]>,
    pub(crate) background: Color,
}

impl ScreenSnapshot {
    pub(crate) fn empty() -> Arc<Self> {
        Arc::new(Self {
            rows: Arc::from([]),
            background: ACTIVE_THEME.terminal_background,
        })
    }
}

pub(crate) struct TerminalEmulator {
    terminal: Terminal<'static, 'static>,
    render_state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    mouse_encoder: MouseEncoder<'static>,
    mouse_event: MouseEvent<'static>,
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

    fn screen_changed() -> Self {
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

        Ok(Self {
            terminal,
            render_state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            mouse_encoder: MouseEncoder::new()?,
            mouse_event: MouseEvent::new()?,
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

    pub(crate) fn pointer(&mut self, input: PointerInput) -> Result<EmulatorAction, String> {
        match input.phase {
            PointerPhase::Press => self.pointer_press(input),
            PointerPhase::Motion => self.pointer_motion(input),
            PointerPhase::Release => self.pointer_release(input),
        }
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
            return Ok(EmulatorAction::bytes(bytes));
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
            let application_cursor = self
                .terminal
                .mode(Mode::DECCKM)
                .map_err(|error| format!("failed to query cursor-key mode: {error}"))?;
            let sequence: &[u8] = match (application_cursor, steps > 0) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1bOB",
                (false, true) => b"\x1b[A",
                (false, false) => b"\x1b[B",
            };
            let mut bytes = Vec::with_capacity(sequence.len() * steps.unsigned_abs() as usize);
            for _ in 0..steps.unsigned_abs() {
                bytes.extend_from_slice(sequence);
            }
            return Ok(EmulatorAction::bytes(bytes));
        }

        self.terminal
            .scroll_viewport(ScrollViewport::Delta(-(steps as isize)));
        Ok(EmulatorAction::screen_changed())
    }

    pub(crate) fn paste(&mut self, text: String) -> Result<EmulatorAction, String> {
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
        let tracking = self
            .terminal
            .is_mouse_tracking()
            .map_err(|error| format!("failed to query terminal mouse tracking mode: {error}"))?;
        let Some(button) = input.button else {
            self.active_pointer = None;
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
                let mut bytes = Vec::new();
                self.encode_mouse_event(
                    MouseAction::Press,
                    Some(mouse_button(button)),
                    input.position,
                    input.modifiers,
                    true,
                    &mut bytes,
                )?;
                Ok(EmulatorAction::bytes(bytes))
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
        let Some(active) = self.active_pointer.take() else {
            return Ok(EmulatorAction::none());
        };

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

    fn encode_mouse_event(
        &mut self,
        action: MouseAction,
        button: Option<MouseButton>,
        position: SurfacePosition,
        modifiers: InputModifiers,
        any_button_pressed: bool,
        bytes: &mut Vec<u8>,
    ) -> Result<(), String> {
        let size = self.mouse_encoder_size();
        self.mouse_encoder
            .set_options_from_terminal(&self.terminal)
            .set_size(size)
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

        let default_foreground: Color = colors.foreground.into();
        let default_background: Color = colors.background.into();
        let cursor_position = cursor.as_ref().map(|cursor| (cursor.x, cursor.y));
        let rebuild_all = matches!(dirty, Dirty::Full)
            || self.row_cache.len() != usize::from(rows)
            || self.cached_cols != cols
            || self.cached_foreground != Some(default_foreground)
            || self.cached_background != Some(default_background);
        let cursor_changed = self.cached_cursor != cursor_position;

        if matches!(dirty, Dirty::Clean) && !rebuild_all && !cursor_changed {
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

        Ok(Some(Arc::new(ScreenSnapshot {
            rows: Arc::from(self.row_cache.clone()),
            background: default_background,
        })))
    }
}

impl Drop for TerminalEmulator {
    fn drop(&mut self) {
        self.selection_gesture.reset(&self.terminal);
    }
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

        let action = emulator.wheel(wheel(1, false)).unwrap();
        assert!(action.bytes.is_empty());
        assert!(action.screen_changed);

        let scrolled = emulator.snapshot().unwrap().unwrap();
        assert!(row_text(&scrolled, 0).starts_with("one"));
        assert!(row_text(&scrolled, 1).starts_with("two"));
    }

    #[test]
    fn tracked_and_alternate_screen_wheel_events_encode_input() {
        let mut tracked = emulator(10, 2);
        tracked.feed(b"\x1b[?1000h\x1b[?1006h");
        let reported = tracked.wheel(wheel(2, false)).unwrap();
        assert_eq!(reported.bytes, b"\x1b[<64;1;1M\x1b[<64;1;1M");
        assert!(!reported.screen_changed);

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
