use std::cell::RefCell;
use std::mem;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use libghostty_vt::fmt::Format;
use libghostty_vt::key::{
    Action as GhosttyKeyAction, Encoder as KeyEncoder, Event as KeyEvent, Mods, OptionAsAlt,
};
use libghostty_vt::mouse::{
    Action as MouseAction, Button as MouseButton, Encoder as MouseEncoder,
    EncoderSize as MouseEncoderSize, Event as MouseEvent, Position as MousePosition,
};
use libghostty_vt::paste;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RowIterator};
use libghostty_vt::screen::{CellWide, Screen};
use libghostty_vt::selection::FormatOptions;
use libghostty_vt::selection::gesture::{
    DragEvent, Geometry as SelectionGeometry, Gesture, PressEvent, ReleaseEvent,
};
use libghostty_vt::style::{PaletteIndex, RgbColor};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, ScrollViewport};
use libghostty_vt::{Error, RenderState, Terminal, TerminalOptions};

use crate::terminal::geometry::{BackingPosition, TerminalGeometry};
use crate::terminal::key::{InputModifiers, KeyAction, KeyInput, PhysicalKey};
use crate::terminal::session::{
    PointerButton, PointerInput, PointerPhase, SurfacePosition, WheelInput,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CellSnapshot {
    pub(crate) text: String,
    pub(crate) foreground: Color,
    pub(crate) background: Color,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) selected: bool,
    pub(crate) spacer_tail: bool,
}

pub(crate) type RowSnapshot = Arc<[CellSnapshot]>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ActiveScreenSnapshot {
    #[default]
    Primary,
    Alternate,
}

impl From<Screen> for ActiveScreenSnapshot {
    fn from(screen: Screen) -> Self {
        match screen {
            Screen::Primary => Self::Primary,
            Screen::Alternate => Self::Alternate,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ScreenSizeSnapshot {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ViewportSnapshot {
    pub(crate) offset_rows: u64,
    pub(crate) visible_rows: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum CursorShapeSnapshot {
    Bar,
    #[default]
    Block,
    Underline,
    BlockHollow,
}

impl From<CursorVisualStyle> for CursorShapeSnapshot {
    fn from(style: CursorVisualStyle) -> Self {
        match style {
            CursorVisualStyle::Bar => Self::Bar,
            CursorVisualStyle::Block => Self::Block,
            CursorVisualStyle::Underline => Self::Underline,
            CursorVisualStyle::BlockHollow => Self::BlockHollow,
            _ => Self::Block,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct CursorPositionSnapshot {
    pub(crate) column: u16,
    pub(crate) row: u16,
    pub(crate) at_wide_tail: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CursorSnapshot {
    pub(crate) position: Option<CursorPositionSnapshot>,
    pub(crate) visible: bool,
    pub(crate) blinking: bool,
    pub(crate) password_input: bool,
    pub(crate) shape: CursorShapeSnapshot,
    pub(crate) color: Color,
}

impl Default for CursorSnapshot {
    fn default() -> Self {
        Self {
            position: None,
            visible: false,
            blinking: false,
            password_input: false,
            shape: CursorShapeSnapshot::default(),
            color: ACTIVE_THEME.terminal_foreground,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum ContentDamageSnapshot {
    #[default]
    Clean,
    Rows(Arc<[u16]>),
    Full,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SnapshotDamage {
    pub(crate) content: ContentDamageSnapshot,
    pub(crate) cursor: bool,
    pub(crate) title: bool,
    pub(crate) scrollbar: bool,
    pub(crate) viewport: bool,
    pub(crate) active_screen: bool,
    pub(crate) resize: bool,
}

impl SnapshotDamage {
    fn initial() -> Self {
        Self {
            content: ContentDamageSnapshot::Full,
            cursor: true,
            title: true,
            scrollbar: true,
            viewport: true,
            active_screen: true,
            resize: true,
        }
    }

    #[cfg(test)]
    fn cursor() -> Self {
        Self {
            cursor: true,
            ..Self::default()
        }
    }

    fn is_clean(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ScrollbarSnapshot {
    pub(crate) total_rows: u64,
    pub(crate) offset_rows: u64,
    pub(crate) visible_rows: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct ScreenSnapshot {
    pub(crate) rows: Arc<[RowSnapshot]>,
    pub(crate) background: Color,
    pub(crate) size: ScreenSizeSnapshot,
    pub(crate) viewport: ViewportSnapshot,
    pub(crate) scrollbar: ScrollbarSnapshot,
    pub(crate) active_screen: ActiveScreenSnapshot,
    pub(crate) cursor: CursorSnapshot,
    pub(crate) title: Arc<str>,
    pub(crate) damage: SnapshotDamage,
}

impl PartialEq for ScreenSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.rows == other.rows
            && self.background == other.background
            && self.size == other.size
            && self.viewport == other.viewport
            && self.scrollbar == other.scrollbar
            && self.active_screen == other.active_screen
            && self.cursor == other.cursor
            && self.title == other.title
            && self.damage == other.damage
    }
}

impl Eq for ScreenSnapshot {}

impl ScreenSnapshot {
    pub(crate) fn empty() -> Arc<Self> {
        Arc::new(Self {
            rows: Arc::from([]),
            background: ACTIVE_THEME.terminal_background,
            size: ScreenSizeSnapshot::default(),
            viewport: ViewportSnapshot::default(),
            scrollbar: ScrollbarSnapshot::default(),
            active_screen: ActiveScreenSnapshot::default(),
            cursor: CursorSnapshot {
                color: ACTIVE_THEME.terminal_foreground,
                ..CursorSnapshot::default()
            },
            title: Arc::from(""),
            damage: SnapshotDamage::initial(),
        })
    }

    #[cfg(test)]
    pub(crate) fn from_test_parts(
        rows: Arc<[RowSnapshot]>,
        scrollbar: ScrollbarSnapshot,
        title: impl Into<Arc<str>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            rows,
            scrollbar,
            title: title.into(),
            ..Self::empty_value()
        })
    }

    #[cfg(test)]
    fn empty_value() -> Self {
        Self {
            rows: Arc::from([]),
            background: ACTIVE_THEME.terminal_background,
            size: ScreenSizeSnapshot::default(),
            viewport: ViewportSnapshot::default(),
            scrollbar: ScrollbarSnapshot::default(),
            active_screen: ActiveScreenSnapshot::default(),
            cursor: CursorSnapshot::default(),
            title: Arc::from(""),
            damage: SnapshotDamage::initial(),
        }
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
    pending_title: Rc<RefCell<Option<Arc<str>>>>,
    title: Arc<str>,
    row_cache: Vec<RowSnapshot>,
    cached_cols: u16,
    cached_foreground: Option<Color>,
    cached_background: Option<Color>,
    cached_cursor: Option<CursorSnapshot>,
    cached_scrollbar: Option<ScrollbarSnapshot>,
    cached_active_screen: Option<ActiveScreenSnapshot>,
    geometry: TerminalGeometry,
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
    pub(crate) fn new(geometry: TerminalGeometry) -> Result<Self, Error> {
        let grid = geometry.grid();
        let cell = geometry.backing_cell_size();
        let pty_responses = Rc::new(RefCell::new(Vec::new()));
        let pending_title = Rc::new(RefCell::new(None));
        let mut terminal: Terminal<'static, 'static> = Terminal::new(TerminalOptions {
            cols: grid.cols,
            rows: grid.rows,
            max_scrollback: 10_000,
        })?;

        apply_theme(&mut terminal)?;
        terminal.resize(grid.cols, grid.rows, cell.width, cell.height)?;
        terminal.on_pty_write({
            let pty_responses = Rc::clone(&pty_responses);
            move |_, data| pty_responses.borrow_mut().extend_from_slice(data)
        })?;
        terminal.on_title_changed({
            let pending_title = Rc::clone(&pending_title);
            move |terminal| {
                if let Ok(title) = terminal.title() {
                    *pending_title.borrow_mut() = Some(Arc::from(title));
                }
            }
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
            pending_title,
            title: Arc::from(""),
            row_cache: Vec::new(),
            cached_cols: 0,
            cached_foreground: None,
            cached_background: None,
            cached_cursor: None,
            cached_scrollbar: None,
            cached_active_screen: None,
            geometry,
            active_pointer: None,
            gesture_epoch: Instant::now(),
        })
    }

    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }

    pub(crate) fn resize(&mut self, geometry: TerminalGeometry) -> Result<(), Error> {
        let grid = geometry.grid();
        let cell = geometry.backing_cell_size();
        self.terminal
            .resize(grid.cols, grid.rows, cell.width, cell.height)?;
        self.geometry = geometry;
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
                action: KeyAction::Press,
                physical_key: if steps > 0 {
                    PhysicalKey::ArrowUp
                } else {
                    PhysicalKey::ArrowDown
                },
                native_key_code: None,
                logical_key: if steps > 0 { "up" } else { "down" }.to_owned(),
                text: None,
                unshifted_codepoint: None,
                modifiers: InputModifiers::default(),
                consumed_modifiers: InputModifiers::default(),
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
        input.validate().map_err(|error| error.to_string())?;
        if input
            .text
            .as_deref()
            .is_some_and(|text| text.chars().any(char::is_control))
        {
            return Err("terminal key text must not contain control characters".to_owned());
        }

        self.key_encoder
            .set_options_from_terminal(&self.terminal)
            .set_macos_option_as_alt(OptionAsAlt::True);
        self.key_event
            .set_action(match input.action {
                KeyAction::Press => GhosttyKeyAction::Press,
                KeyAction::Repeat => GhosttyKeyAction::Repeat,
                KeyAction::Release => GhosttyKeyAction::Release,
            })
            .set_key(input.physical_key)
            .set_mods(key_modifiers(input.modifiers))
            .set_consumed_mods(key_modifiers(input.consumed_modifiers))
            .set_composing(false)
            .set_utf8(input.text.clone())
            .set_unshifted_codepoint(input.unshifted_codepoint.unwrap_or('\0'));
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
        let encoded_position = if modes.sgr_pixels {
            position
        } else {
            self.cell_mouse_encoder_position(position)
        };
        self.mouse_event
            .set_action(action)
            .set_button(button)
            .set_mods(mouse_modifiers(modifiers))
            .set_position(MousePosition {
                x: encoded_position.x,
                y: encoded_position.y,
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
        let position = self
            .geometry
            .cell_at_backing_position(BackingPosition::new(position.x, position.y));
        PointCoordinate {
            x: position.col,
            y: u32::from(position.row),
        }
    }

    fn cell_mouse_encoder_position(&self, position: SurfacePosition) -> SurfacePosition {
        let cell = self
            .geometry
            .cell_at_backing_position(BackingPosition::new(position.x, position.y));
        let encoded_cell = self.geometry.backing_cell_size();
        SurfacePosition {
            x: f32::from(cell.col) * encoded_cell.width as f32,
            y: f32::from(cell.row) * encoded_cell.height as f32,
        }
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
        let cell = self.geometry.backing_cell_size();
        let backing = self.geometry.backing_grid_size();
        MouseEncoderSize {
            screen_width: backing.width,
            screen_height: backing.height,
            cell_width: cell.width,
            cell_height: cell.height,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        }
    }

    fn selection_geometry(&self) -> SelectionGeometry {
        let grid = self.geometry.grid();
        let cell = self.geometry.backing_cell_size();
        SelectionGeometry {
            columns: u32::from(grid.cols),
            cell_width: cell.width,
            padding_left: 0,
            screen_height: self.geometry.backing_grid_size().height,
        }
    }

    pub(crate) fn snapshot(&mut self) -> Result<Option<Arc<ScreenSnapshot>>, Error> {
        let pending_title = self.pending_title.borrow_mut().take();
        let title_changed = pending_title
            .as_ref()
            .is_some_and(|title| title.as_ref() != self.title.as_ref());
        if let Some(title) = pending_title {
            self.title = title;
        }
        let snapshot = self.render_state.update(&self.terminal)?;
        let dirty = snapshot.dirty()?;
        let rows = snapshot.rows()?;
        let cols = snapshot.cols()?;
        let colors = snapshot.colors()?;
        let cursor_position = snapshot
            .cursor_viewport()?
            .map(|cursor| CursorPositionSnapshot {
                column: cursor.x,
                row: cursor.y,
                at_wide_tail: cursor.at_wide_tail,
            });
        let cursor = CursorSnapshot {
            position: cursor_position,
            visible: snapshot.cursor_visible()?,
            blinking: snapshot.cursor_blinking()?,
            password_input: snapshot.cursor_password_input()?,
            shape: snapshot.cursor_visual_style()?.into(),
            color: snapshot.cursor_color()?.unwrap_or(colors.foreground).into(),
        };
        let scrollbar = self.terminal.scrollbar()?;
        let scrollbar = ScrollbarSnapshot {
            total_rows: scrollbar.total,
            offset_rows: scrollbar.offset,
            visible_rows: scrollbar.len,
        };
        let viewport = ViewportSnapshot {
            offset_rows: scrollbar.offset_rows,
            visible_rows: scrollbar.visible_rows,
        };
        let size = ScreenSizeSnapshot { cols, rows };
        let active_screen: ActiveScreenSnapshot = self.terminal.active_screen()?.into();

        let default_foreground: Color = colors.foreground.into();
        let default_background: Color = colors.background.into();
        let rebuild_all = matches!(dirty, Dirty::Full)
            || self.row_cache.len() != usize::from(rows)
            || self.cached_cols != cols
            || self.cached_foreground != Some(default_foreground)
            || self.cached_background != Some(default_background);
        let previous_scrollbar = self.cached_scrollbar;
        let first_snapshot = self.cached_foreground.is_none();
        let mut damage = if first_snapshot {
            SnapshotDamage::initial()
        } else {
            SnapshotDamage {
                cursor: self.cached_cursor != Some(cursor),
                title: title_changed,
                scrollbar: previous_scrollbar
                    .is_some_and(|previous| previous.total_rows != scrollbar.total_rows),
                viewport: previous_scrollbar.is_some_and(|previous| {
                    previous.offset_rows != scrollbar.offset_rows
                        || previous.visible_rows != scrollbar.visible_rows
                }),
                active_screen: self.cached_active_screen != Some(active_screen),
                resize: self.cached_cols != cols || self.row_cache.len() != usize::from(rows),
                ..SnapshotDamage::default()
            }
        };

        if matches!(dirty, Dirty::Clean) && !rebuild_all && damage.is_clean() {
            return Ok(None);
        }

        let mut rendered_rows = if rebuild_all {
            Vec::with_capacity(usize::from(rows))
        } else {
            self.row_cache.clone()
        };
        let mut dirty_rows = Vec::new();
        let mut row_index = 0_u16;
        {
            let mut row_iteration = self.rows.update(&snapshot)?;

            while let Some(row) = row_iteration.next() {
                let rebuild_row = rebuild_all || row.dirty()?;

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

                        rendered_cells.push(CellSnapshot {
                            text,
                            foreground,
                            background,
                            bold: style.bold,
                            italic: style.italic,
                            selected: selection.is_some_and(|range| {
                                column_index >= range.start_x && column_index <= range.end_x
                            }),
                            spacer_tail,
                        });
                        column_index = column_index.saturating_add(1);
                    }

                    let rendered_row = Arc::<[CellSnapshot]>::from(rendered_cells);
                    let cached_row = self.row_cache.get(usize::from(row_index));
                    let rendered_row = cached_row
                        .filter(|cached| cached.as_ref() == rendered_row.as_ref())
                        .cloned()
                        .unwrap_or(rendered_row);
                    if rebuild_all {
                        if cached_row.is_none_or(|cached| !Arc::ptr_eq(cached, &rendered_row)) {
                            dirty_rows.push(row_index);
                        }
                        rendered_rows.push(rendered_row);
                    } else if !Arc::ptr_eq(&rendered_rows[usize::from(row_index)], &rendered_row) {
                        rendered_rows[usize::from(row_index)] = rendered_row;
                        dirty_rows.push(row_index);
                    }
                }

                row.set_dirty(false)?;
                row_index = row_index.saturating_add(1);
            }
        }
        snapshot.set_dirty(Dirty::Clean)?;

        damage.content = if first_snapshot
            || damage.resize
            || (rebuild_all && dirty_rows.len() == usize::from(rows) && rows != 0)
        {
            ContentDamageSnapshot::Full
        } else if dirty_rows.is_empty() {
            ContentDamageSnapshot::Clean
        } else {
            ContentDamageSnapshot::Rows(Arc::from(dirty_rows))
        };

        self.row_cache = rendered_rows;
        self.cached_cols = cols;
        self.cached_foreground = Some(default_foreground);
        self.cached_background = Some(default_background);
        self.cached_cursor = Some(cursor);
        self.cached_scrollbar = Some(scrollbar);
        self.cached_active_screen = Some(active_screen);

        Ok(Some(Arc::new(ScreenSnapshot {
            rows: Arc::from(self.row_cache.clone()),
            background: default_background,
            size,
            viewport,
            scrollbar,
            active_screen,
            cursor,
            title: Arc::clone(&self.title),
            damage,
        })))
    }
}

impl Drop for TerminalEmulator {
    fn drop(&mut self) {
        self.selection_gesture.reset(&self.terminal);
    }
}

fn key_modifiers(modifiers: InputModifiers) -> Mods {
    let mut result = Mods::empty();
    result.set(Mods::SHIFT, modifiers.shift);
    result.set(Mods::ALT, modifiers.alt);
    result.set(Mods::CTRL, modifiers.control);
    result.set(Mods::SUPER, modifiers.platform);
    result.set(Mods::CAPS_LOCK, modifiers.caps_lock);
    result.set(Mods::NUM_LOCK, modifiers.num_lock);
    result.set(Mods::SHIFT_SIDE, modifiers.shift_right);
    result.set(Mods::ALT_SIDE, modifiers.alt_right);
    result.set(Mods::CTRL_SIDE, modifiers.control_right);
    result.set(Mods::SUPER_SIDE, modifiers.platform_right);
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
    use crate::terminal::geometry::{
        BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
    };

    fn geometry(cols: u16, rows: u16, cell_width: f32, cell_height: f32) -> TerminalGeometry {
        TerminalGeometry::from_grid(
            CellGridSize::new(cols, rows),
            LogicalCellSize::new(cell_width, cell_height),
            BackingScale::ONE,
        )
    }

    fn emulator(cols: u16, rows: u16) -> TerminalEmulator {
        TerminalEmulator::new(geometry(cols, rows, 10.0, 20.0)).unwrap()
    }

    #[test]
    fn pixel_mouse_coordinates_should_share_fractional_backing_geometry() {
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(10, 2),
            LogicalCellSize::new(7.5, 20.0),
            BackingScale::new(1.5).unwrap(),
        );
        let mut emulator = TerminalEmulator::new(geometry).unwrap();
        emulator.feed(b"\x1b[?1003h\x1b[?1016h");

        let action = emulator
            .pointer(pointer(PointerPhase::Motion, None, 5.625, 15.0, false))
            .unwrap();

        assert_eq!(
            (action.bytes, emulator.mouse_encoder_size()),
            (
                b"\x1b[<35;6;15M".to_vec(),
                MouseEncoderSize {
                    screen_width: 113,
                    screen_height: 60,
                    cell_width: 12,
                    cell_height: 30,
                    padding_top: 0,
                    padding_bottom: 0,
                    padding_right: 0,
                    padding_left: 0,
                },
            )
        );
    }

    #[test]
    fn cell_mouse_coordinates_should_not_drift_across_fractional_backing_cells() {
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(10, 2),
            LogicalCellSize::new(7.5, 20.0),
            BackingScale::new(1.5).unwrap(),
        );
        let mut emulator = TerminalEmulator::new(geometry).unwrap();
        emulator.feed(b"\x1b[?1003h\x1b[?1006h");

        let action = emulator
            .pointer(pointer(PointerPhase::Motion, None, 11.5, 1.0, false))
            .unwrap();

        assert_eq!(action.bytes, b"\x1b[<35;2;1M");
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

    fn key(physical_key: PhysicalKey, modifiers: InputModifiers) -> KeyInput {
        KeyInput {
            action: KeyAction::Press,
            physical_key,
            native_key_code: None,
            logical_key: format!("{physical_key:?}"),
            text: None,
            unshifted_codepoint: None,
            modifiers,
            consumed_modifiers: InputModifiers::default(),
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
    fn title_only_osc_sequence_publishes_a_screen_snapshot() {
        let mut emulator = emulator(12, 3);
        let first = emulator.snapshot().unwrap().unwrap();

        emulator.feed(b"\x1b]2;Claude Code\x07");
        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert_eq!(snapshot.title.as_ref(), "Claude Code");
        assert!(
            first
                .rows
                .iter()
                .zip(snapshot.rows.iter())
                .all(|(first, second)| Arc::ptr_eq(first, second))
        );
        assert_eq!(
            snapshot.damage,
            SnapshotDamage {
                title: true,
                ..SnapshotDamage::default()
            }
        );
    }

    #[test]
    fn latest_osc_title_replaces_the_previous_snapshot_title() {
        let mut emulator = emulator(12, 3);
        emulator.feed(b"\x1b]2;zsh\x07");
        let _ = emulator.snapshot().unwrap();

        emulator.feed(b"\x1b]2;cargo test\x07");
        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert_eq!(snapshot.title.as_ref(), "cargo test");
    }

    #[test]
    fn resize_changes_the_visible_grid() {
        let mut emulator = emulator(10, 2);
        let _ = emulator.snapshot().unwrap();
        emulator.resize(geometry(20, 4, 8.0, 18.0)).unwrap();

        let snapshot = emulator.snapshot().unwrap().unwrap();
        assert_eq!(
            (
                snapshot.size,
                snapshot.viewport.visible_rows,
                snapshot.rows.len(),
                snapshot.rows.iter().all(|row| row.len() == 20),
            ),
            (ScreenSizeSnapshot { cols: 20, rows: 4 }, 4, 4, true,)
        );
        assert!(snapshot.damage.resize);
        assert_eq!(snapshot.damage.content, ContentDamageSnapshot::Full);
        assert!(!snapshot.damage.title);
        assert!(!snapshot.damage.active_screen);
    }

    #[test]
    fn alternate_screen_transition_reports_only_affected_metadata_and_content() {
        let mut emulator = emulator(10, 2);
        let first = emulator.snapshot().unwrap().unwrap();

        emulator.feed(b"\x1b[?1049h");
        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert_eq!(snapshot.active_screen, ActiveScreenSnapshot::Alternate);
        assert!(
            first
                .rows
                .iter()
                .zip(snapshot.rows.iter())
                .all(|(first, second)| Arc::ptr_eq(first, second))
        );
        assert_eq!(
            snapshot.damage,
            SnapshotDamage {
                active_screen: true,
                ..SnapshotDamage::default()
            }
        );
    }

    #[test]
    fn incremental_snapshots_match_a_full_snapshot_of_the_same_terminal_state() {
        let chunks: [&[u8]; 3] = [
            b"one\r\ntwo",
            b"\x1b[31m red\x1b[0m",
            b"\x1b]2;incremental build\x07",
        ];
        let mut incremental = emulator(16, 3);
        let mut incremental_snapshot = None;
        for chunk in chunks {
            incremental.feed(chunk);
            incremental_snapshot = incremental.snapshot().unwrap();
        }

        let mut full = emulator(16, 3);
        full.feed(b"one\r\ntwo\x1b[31m red\x1b[0m\x1b]2;incremental build\x07");

        let mut incremental_snapshot = (*incremental_snapshot.unwrap()).clone();
        let mut full_snapshot = (*full.snapshot().unwrap().unwrap()).clone();
        incremental_snapshot.damage = SnapshotDamage::default();
        full_snapshot.damage = SnapshotDamage::default();

        assert_eq!(incremental_snapshot, full_snapshot);
    }

    #[test]
    fn resize_emits_in_band_size_responses() {
        let mut emulator = emulator(10, 2);
        emulator.feed(b"\x1b[?2048h");
        assert!(emulator.take_pty_responses().is_empty());

        emulator.resize(geometry(20, 4, 8.0, 18.0)).unwrap();
        assert_eq!(emulator.take_pty_responses(), b"\x1b[48;4;20;72;160t");
    }

    #[test]
    fn key_encoding_tracks_cursor_mode_and_modifiers() {
        let mut emulator = emulator(10, 2);
        assert_eq!(
            emulator
                .key(key(PhysicalKey::ArrowUp, InputModifiers::default()))
                .unwrap()
                .bytes,
            b"\x1b[A"
        );

        emulator.feed(b"\x1b[?1h");
        assert_eq!(
            emulator
                .key(key(PhysicalKey::ArrowUp, InputModifiers::default()))
                .unwrap()
                .bytes,
            b"\x1bOA"
        );

        let printable = KeyInput {
            action: KeyAction::Press,
            physical_key: PhysicalKey::E,
            native_key_code: None,
            logical_key: "é".to_owned(),
            text: Some("é".to_owned()),
            unshifted_codepoint: Some('e'),
            modifiers: InputModifiers::default(),
            consumed_modifiers: InputModifiers::default(),
        };
        assert_eq!(emulator.key(printable).unwrap().bytes, "é".as_bytes());

        let control_c = KeyInput {
            action: KeyAction::Press,
            physical_key: PhysicalKey::C,
            native_key_code: None,
            logical_key: "c".to_owned(),
            text: Some("c".to_owned()),
            unshifted_codepoint: Some('c'),
            modifiers: InputModifiers {
                control: true,
                ..InputModifiers::default()
            },
            consumed_modifiers: InputModifiers::default(),
        };
        assert_eq!(emulator.key(control_c).unwrap().bytes, b"\x03");

        let alt_x = KeyInput {
            action: KeyAction::Press,
            physical_key: PhysicalKey::X,
            native_key_code: None,
            logical_key: "x".to_owned(),
            text: Some("x".to_owned()),
            unshifted_codepoint: Some('x'),
            modifiers: InputModifiers {
                alt: true,
                ..InputModifiers::default()
            },
            consumed_modifiers: InputModifiers::default(),
        };
        assert_eq!(emulator.key(alt_x).unwrap().bytes, b"\x1bx");
    }

    #[test]
    fn named_keys_use_ghostty_key_encoding() {
        let mut emulator = emulator(10, 2);
        let cases: &[(PhysicalKey, InputModifiers, &[u8])] = &[
            (PhysicalKey::Enter, InputModifiers::default(), b"\r"),
            (PhysicalKey::Backspace, InputModifiers::default(), b"\x7f"),
            (PhysicalKey::Tab, InputModifiers::default(), b"\t"),
            (
                PhysicalKey::Tab,
                InputModifiers {
                    shift: true,
                    ..InputModifiers::default()
                },
                b"\x1b[Z",
            ),
            (PhysicalKey::Escape, InputModifiers::default(), b"\x1b"),
            (PhysicalKey::ArrowDown, InputModifiers::default(), b"\x1b[B"),
            (PhysicalKey::ArrowLeft, InputModifiers::default(), b"\x1b[D"),
            (PhysicalKey::ArrowRight, InputModifiers::default(), b"\x1b[C"),
            (PhysicalKey::Home, InputModifiers::default(), b"\x1b[H"),
            (PhysicalKey::End, InputModifiers::default(), b"\x1b[F"),
            (PhysicalKey::PageUp, InputModifiers::default(), b"\x1b[5~"),
            (PhysicalKey::PageDown, InputModifiers::default(), b"\x1b[6~"),
            (PhysicalKey::Insert, InputModifiers::default(), b"\x1b[2~"),
            (PhysicalKey::Delete, InputModifiers::default(), b"\x1b[3~"),
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
        assert!(Arc::ptr_eq(&first.title, &second.title));
    }

    #[test]
    fn row_dirty_update_reports_only_the_changed_row() {
        let mut emulator = emulator(10, 3);
        let first = emulator.snapshot().unwrap().unwrap();

        emulator.feed(b"x\x1b[1D");
        let second = emulator.snapshot().unwrap().unwrap();

        assert!(!Arc::ptr_eq(&first.rows[0], &second.rows[0]));
        assert!(Arc::ptr_eq(&first.rows[1], &second.rows[1]));
        assert!(Arc::ptr_eq(&first.rows[2], &second.rows[2]));
        assert_eq!(
            second.damage,
            SnapshotDamage {
                content: ContentDamageSnapshot::Rows(Arc::from([0])),
                ..SnapshotDamage::default()
            }
        );
    }

    #[test]
    fn cursor_only_changes_reuse_every_row_and_report_cursor_damage() {
        let mut emulator = emulator(10, 3);
        emulator.feed(b"abc");
        let first = emulator.snapshot().unwrap().unwrap();

        emulator.feed(b"\x1b[1D");
        let second = emulator.snapshot().unwrap().unwrap();

        assert!(
            first
                .rows
                .iter()
                .zip(second.rows.iter())
                .all(|(first, second)| Arc::ptr_eq(first, second))
        );
        assert_eq!(second.damage, SnapshotDamage::cursor());
    }

    #[test]
    fn cursor_snapshot_preserves_position_visibility_style_blink_and_color() {
        let mut emulator = emulator(10, 3);
        emulator.feed(b"\x1b[3;4H\x1b[6 q\x1b]12;#112233\x07");

        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert_eq!(
            snapshot.cursor,
            CursorSnapshot {
                position: Some(CursorPositionSnapshot {
                    column: 3,
                    row: 2,
                    at_wide_tail: false,
                }),
                visible: true,
                blinking: false,
                password_input: false,
                shape: CursorShapeSnapshot::Bar,
                color: Color::rgb(0x11_22_33),
            }
        );
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
        let _ = emulator.snapshot().unwrap();
        emulator.feed(b"one\r\ntwo\r\nthree");
        let bottom = emulator.snapshot().unwrap().unwrap();
        assert!(row_text(&bottom, 0).starts_with("two"));
        assert!(row_text(&bottom, 1).starts_with("three"));
        assert!(bottom.damage.scrollbar);
        assert!(!bottom.damage.title);
        assert!(!bottom.damage.active_screen);
        assert!(!bottom.damage.resize);
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
        assert!(scrolled.damage.viewport);
        assert!(!scrolled.damage.scrollbar);
        assert!(!scrolled.damage.title);

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

    #[test]
    fn screen_snapshots_are_owned_and_safe_to_share_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<ScreenSnapshot>();
    }
}
