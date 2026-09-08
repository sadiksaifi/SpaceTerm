use crate::platform::local_filesystem::{LocalFileEmissionRegistry, LocalFilesystemAuthority};
use std::cell::RefCell;
use std::collections::HashMap;
use std::mem;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use libghostty_vt::accessibility as ghostty_accessibility;
use libghostty_vt::fmt::Format;
use libghostty_vt::focus::Event as FocusEvent;
use libghostty_vt::key::Mods;
use libghostty_vt::kitty::graphics::{RustPngDecoder, set_png_decoder};
use libghostty_vt::mouse::{
    Action as MouseAction, Button as MouseButton, Encoder as MouseEncoder,
    EncoderSize as MouseEncoderSize, Event as MouseEvent, Position as MousePosition,
};
use libghostty_vt::paste;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RowIterator};
use libghostty_vt::screen::{CellContentTag, CellSemanticContent, CellWide, Screen};
use libghostty_vt::selection::FormatOptions;
use libghostty_vt::selection::gesture::{
    Autoscroll, AutoscrollTickEvent, DragEvent, Geometry as SelectionGeometry, Gesture, PressEvent,
    ReleaseEvent,
};
use libghostty_vt::style::{PaletteIndex, RgbColor, StyleColor, Underline};
use libghostty_vt::terminal::{
    HyperlinkResolution, Mode, Point, PointCoordinate, ProgressState, ScrollViewport,
    SemanticPromptAction,
};
use libghostty_vt::{Error, RenderState, Terminal, TerminalOptions};

use crate::terminal::accessibility::{
    AccessibilityCell, AccessibilityCellRef, AccessibilityRowId, AccessibilityRowUpdate,
    AccessibilityScreen, AccessibilitySelectionRefs, AccessibilitySelectionRequest,
    AccessibilityUpdate, TerminalAccessibilityModel, TerminalAccessibilityState,
};
use crate::terminal::attention::AttentionEvent;
use crate::terminal::find::TerminalFindState;
use crate::terminal::geometry::{BackingPosition, TerminalGeometry};
use crate::terminal::graphics::{
    APC_TRANSMISSION_LIMIT, GraphicsBudgetWake, GraphicsReservation, GraphicsSnapshot,
    GraphicsState, starts_apc,
};
use crate::terminal::hyperlink::{HyperlinkTarget, has_file_scheme};
use crate::terminal::identity::{self, XtGetTcapObserver};
use crate::terminal::key::{InputModifiers, KeyAction, KeyInput, OptionAsAltPolicy, PhysicalKey};
use crate::terminal::keyboard_protocol::KeyboardProtocolEncoder;
use crate::terminal::metadata::{
    MetadataTracker, TerminalMetadataContext, TerminalMetadataSnapshot,
};
#[cfg(test)]
use crate::terminal::pointer_input::WheelPhase;
use crate::terminal::pointer_input::{
    PointerButton, PointerInput, PointerPhase, ShiftSelectionPolicy, SurfacePosition, WheelInput,
};
use crate::terminal::selection::{SelectionCopy, SelectionCopyOptions, TrailingSpacePolicy};
use crate::terminal::{FindDirection, FindQueryGeneration, TerminalFindSnapshot};
use crate::theme::{ACTIVE_THEME, Color};

const MAX_WHEEL_STEPS: i32 = 100;
const MAX_SCROLLBACK_ROWS: usize = 10_000;
pub(crate) const MAX_SYNCHRONIZED_OUTPUT_DURATION: Duration = Duration::from_secs(1);
const REPEAT_CLICK_DISTANCE_PX: f64 = 5.0;
const REPEAT_CLICK_INTERVAL: Duration = Duration::from_millis(500);
const MIN_SELECTION_AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_SELECTION_AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(150);

impl From<RgbColor> for Color {
    fn from(value: RgbColor) -> Self {
        Self::from_rgb_components(value.r, value.g, value.b)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CellSnapshot {
    pub(crate) text: String,
    pub(crate) foreground_source: TerminalColor,
    pub(crate) background_source: TerminalColor,
    pub(crate) inverse: bool,
    pub(crate) bold: bool,
    pub(crate) faint: bool,
    pub(crate) italic: bool,
    pub(crate) blinking: bool,
    pub(crate) invisible: bool,
    pub(crate) underline: TerminalUnderlineSnapshot,
    pub(crate) underline_source: TerminalColor,
    pub(crate) strikethrough: bool,
    pub(crate) overline: bool,
    pub(crate) selected: bool,
    pub(crate) spacer_tail: bool,
    pub(crate) semantic_content: CellSemanticSnapshot,
    pub(crate) hyperlink: Option<Arc<crate::terminal::HyperlinkTarget>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum CellSemanticSnapshot {
    #[default]
    Output,
    Input,
    Prompt,
}

impl From<CellSemanticContent> for CellSemanticSnapshot {
    fn from(value: CellSemanticContent) -> Self {
        match value {
            CellSemanticContent::Output => Self::Output,
            CellSemanticContent::Input => Self::Input,
            CellSemanticContent::Prompt => Self::Prompt,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TerminalUnderlineSnapshot {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl From<Underline> for TerminalUnderlineSnapshot {
    fn from(underline: Underline) -> Self {
        match underline {
            Underline::None => Self::None,
            Underline::Single => Self::Single,
            Underline::Double => Self::Double,
            Underline::Curly => Self::Curly,
            Underline::Dotted => Self::Dotted,
            Underline::Dashed => Self::Dashed,
            _ => Self::None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TerminalColor {
    #[default]
    Default,
    Palette(u8),
    Rgb(Color),
}

impl From<StyleColor> for TerminalColor {
    fn from(color: StyleColor) -> Self {
        match color {
            StyleColor::None => Self::Default,
            StyleColor::Palette(PaletteIndex(index)) => Self::Palette(index),
            StyleColor::Rgb(color) => Self::Rgb(color.into()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalColorsSnapshot {
    pub(crate) foreground: Color,
    pub(crate) background: Color,
    pub(crate) palette: Arc<[Color; 256]>,
    pub(crate) reversed: bool,
}

impl TerminalColorsSnapshot {
    fn themed() -> Self {
        let mut palette = [ACTIVE_THEME.terminal_foreground; 256];
        palette[..8].copy_from_slice(&ACTIVE_THEME.terminal_normal());
        palette[8..16].copy_from_slice(&ACTIVE_THEME.terminal_bright());
        Self {
            foreground: ACTIVE_THEME.terminal_foreground,
            background: ACTIVE_THEME.terminal_background,
            palette: Arc::new(palette),
            reversed: false,
        }
    }

    pub(crate) fn effective_background(&self) -> Color {
        if self.reversed {
            self.foreground
        } else {
            self.background
        }
    }
}

pub(crate) type RowSnapshot = Arc<[CellSnapshot]>;

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct PresentationGeneration(u64);

impl PresentationGeneration {
    fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    #[cfg(test)]
    pub(crate) const fn test(value: u64) -> Self {
        Self(value)
    }
}

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
    pub(crate) width_cells: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CursorSnapshot {
    pub(crate) position: Option<CursorPositionSnapshot>,
    pub(crate) visible: bool,
    pub(crate) blinking: bool,
    pub(crate) password_input: bool,
    pub(crate) shape: CursorShapeSnapshot,
    pub(crate) color: Color,
    pub(crate) text_color: Color,
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
            text_color: ACTIVE_THEME.terminal_background,
        }
    }
}

fn normalize_cursor_position(
    column: u16,
    row: u16,
    at_wide_tail: bool,
    rows: &[RowSnapshot],
) -> CursorPositionSnapshot {
    let column = if at_wide_tail {
        column.saturating_sub(1)
    } else {
        column
    };
    let width_cells = if rows
        .get(usize::from(row))
        .and_then(|row| row.get(usize::from(column).saturating_add(1)))
        .is_some_and(|cell| cell.spacer_tail)
    {
        2
    } else {
        1
    };
    CursorPositionSnapshot {
        column,
        row,
        width_cells,
    }
}

fn cursor_damage(
    previous: Option<&CursorSnapshot>,
    current: &CursorSnapshot,
) -> ContentDamageSnapshot {
    if previous == Some(current) {
        return ContentDamageSnapshot::Clean;
    }

    let mut rows = previous
        .filter(|cursor| cursor.visible)
        .and_then(|cursor| cursor.position)
        .map(|position| position.row)
        .into_iter()
        .chain(
            current
                .visible
                .then_some(current.position)
                .flatten()
                .map(|position| position.row),
        )
        .collect::<Vec<_>>();
    rows.sort_unstable();
    rows.dedup();
    if rows.is_empty() {
        ContentDamageSnapshot::Clean
    } else {
        ContentDamageSnapshot::Rows(Arc::from(rows))
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
    pub(crate) cursor: ContentDamageSnapshot,
    pub(crate) title: bool,
    pub(crate) metadata: bool,
    pub(crate) scrollbar: bool,
    pub(crate) viewport: bool,
    pub(crate) active_screen: bool,
    pub(crate) resize: bool,
    pub(crate) mouse_tracking: bool,
    pub(crate) selection_presence: bool,
    pub(crate) search: bool,
    pub(crate) graphics_content: bool,
    pub(crate) graphics_geometry: bool,
}

impl SnapshotDamage {
    fn initial() -> Self {
        Self {
            content: ContentDamageSnapshot::Full,
            cursor: ContentDamageSnapshot::Full,
            title: true,
            metadata: true,
            scrollbar: true,
            viewport: true,
            active_screen: true,
            resize: true,
            mouse_tracking: true,
            selection_presence: true,
            search: false,
            graphics_content: true,
            graphics_geometry: true,
        }
    }

    #[cfg(test)]
    fn cursor(row: u16) -> Self {
        Self {
            cursor: ContentDamageSnapshot::Rows(Arc::from([row])),
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
    pub(crate) generation: PresentationGeneration,
    pub(crate) rows: Arc<[RowSnapshot]>,
    /// Soft-wrap markers corresponding one-to-one with the published viewport rows.
    pub(crate) row_soft_wrapped: Arc<[bool]>,
    pub(crate) background: Color,
    pub(crate) colors: TerminalColorsSnapshot,
    pub(crate) size: ScreenSizeSnapshot,
    pub(crate) viewport: ViewportSnapshot,
    pub(crate) scrollbar: ScrollbarSnapshot,
    pub(crate) active_screen: ActiveScreenSnapshot,
    pub(crate) cursor: CursorSnapshot,
    pub(crate) text_blinking: bool,
    pub(crate) mouse_tracking: bool,
    pub(crate) selection_present: bool,
    pub(crate) title: Arc<str>,
    pub(crate) metadata: Arc<TerminalMetadataSnapshot>,
    pub(crate) find: Option<Arc<TerminalFindSnapshot>>,
    pub(crate) graphics: GraphicsSnapshot,
    pub(crate) damage: SnapshotDamage,
}

impl PartialEq for ScreenSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.rows == other.rows
            && self.row_soft_wrapped == other.row_soft_wrapped
            && self.generation == other.generation
            && self.background == other.background
            && self.colors == other.colors
            && self.size == other.size
            && self.viewport == other.viewport
            && self.scrollbar == other.scrollbar
            && self.active_screen == other.active_screen
            && self.cursor == other.cursor
            && self.text_blinking == other.text_blinking
            && self.mouse_tracking == other.mouse_tracking
            && self.selection_present == other.selection_present
            && self.title == other.title
            && self.metadata == other.metadata
            && self.find == other.find
            && self.graphics == other.graphics
            && self.damage == other.damage
    }
}

impl Eq for ScreenSnapshot {}

impl ScreenSnapshot {
    pub(crate) fn empty(paths: crate::local_path::LocalPathSemantics) -> Arc<Self> {
        Arc::new(Self {
            generation: PresentationGeneration::default(),
            rows: Arc::from([]),
            row_soft_wrapped: Arc::from([]),
            background: ACTIVE_THEME.terminal_background,
            colors: TerminalColorsSnapshot::themed(),
            size: ScreenSizeSnapshot::default(),
            viewport: ViewportSnapshot::default(),
            scrollbar: ScrollbarSnapshot::default(),
            active_screen: ActiveScreenSnapshot::default(),
            cursor: CursorSnapshot {
                color: ACTIVE_THEME.terminal_foreground,
                ..CursorSnapshot::default()
            },
            text_blinking: false,
            mouse_tracking: false,
            selection_present: false,
            title: Arc::from(""),
            metadata: MetadataTracker::new(paths, "", "", None, Instant::now()).snapshot(),
            find: None,
            graphics: GraphicsSnapshot::default(),
            damage: SnapshotDamage::initial(),
        })
    }

    #[cfg(test)]
    pub(crate) fn from_test_parts(
        rows: Arc<[RowSnapshot]>,
        scrollbar: ScrollbarSnapshot,
        title: impl Into<Arc<str>>,
    ) -> Arc<Self> {
        let text_blinking = rows_have_visible_blinking_text(&rows);
        let selection_present = rows_have_selection(&rows);
        Arc::new(Self {
            row_soft_wrapped: Arc::from(vec![false; rows.len()]),
            rows,
            text_blinking,
            selection_present,
            scrollbar,
            title: title.into(),
            ..Self::empty_value()
        })
    }

    #[cfg(test)]
    pub(crate) fn from_test_parts_at(
        rows: Arc<[RowSnapshot]>,
        scrollbar: ScrollbarSnapshot,
        title: impl Into<Arc<str>>,
        generation: u64,
    ) -> Arc<Self> {
        let text_blinking = rows_have_visible_blinking_text(&rows);
        let selection_present = rows_have_selection(&rows);
        Arc::new(Self {
            generation: PresentationGeneration(generation),
            row_soft_wrapped: Arc::from(vec![false; rows.len()]),
            rows,
            text_blinking,
            selection_present,
            scrollbar,
            title: title.into(),
            ..Self::empty_value()
        })
    }

    #[cfg(test)]
    fn empty_value() -> Self {
        Self {
            generation: PresentationGeneration::default(),
            rows: Arc::from([]),
            row_soft_wrapped: Arc::from([]),
            background: ACTIVE_THEME.terminal_background,
            colors: TerminalColorsSnapshot::themed(),
            size: ScreenSizeSnapshot::default(),
            viewport: ViewportSnapshot::default(),
            scrollbar: ScrollbarSnapshot::default(),
            active_screen: ActiveScreenSnapshot::default(),
            cursor: CursorSnapshot::default(),
            text_blinking: false,
            mouse_tracking: false,
            selection_present: false,
            title: Arc::from(""),
            metadata: MetadataTracker::new(
                crate::local_path::LocalPathSemantics::Posix,
                "",
                "",
                None,
                Instant::now(),
            )
            .snapshot(),
            find: None,
            graphics: GraphicsSnapshot::default(),
            damage: SnapshotDamage::initial(),
        }
    }
}

fn rows_have_visible_blinking_text(rows: &[RowSnapshot]) -> bool {
    rows.iter().any(|row| {
        row.iter()
            .any(|cell| cell.blinking && !cell.invisible && !cell.spacer_tail)
    })
}

#[cfg(test)]
fn rows_have_selection(rows: &[RowSnapshot]) -> bool {
    rows.iter().any(|row| row.iter().any(|cell| cell.selected))
}

pub(crate) struct TerminalEmulator {
    local_file_emissions: Rc<RefCell<LocalFileEmissionRegistry>>,
    terminal: Terminal<'static, 'static>,
    ghostty_accessibility: ghostty_accessibility::State,
    accessibility: TerminalAccessibilityState,
    accessibility_generation: PresentationGeneration,
    render_state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    keyboard_protocol: KeyboardProtocolEncoder,
    mouse_encoder: MouseEncoder<'static>,
    mouse_event: MouseEvent<'static>,
    cached_mouse_modes: Option<MouseModeState>,
    cached_mouse_size: Option<MouseEncoderSize>,
    selection_gesture: Gesture<'static>,
    selection_press: PressEvent<'static>,
    selection_drag: DragEvent<'static>,
    selection_release: ReleaseEvent<'static>,
    selection_autoscroll_tick: AutoscrollTickEvent<'static>,
    pty_responses: Rc<RefCell<Vec<u8>>>,
    pending_metadata: Rc<RefCell<Vec<MetadataEvent>>>,
    pending_attention: Rc<RefCell<Vec<AttentionEvent>>>,
    title: Arc<str>,
    metadata: MetadataTracker,
    local_file_capabilities: crate::terminal::metadata::TerminalLocalFileCapabilities,
    xtgettcap: XtGetTcapObserver,
    primary_row_cache: Vec<RowSnapshot>,
    alternate_row_cache: Vec<RowSnapshot>,
    primary_graphics: GraphicsState,
    alternate_graphics: GraphicsState,
    graphics_reservation: Option<GraphicsReservation>,
    graphics_failure: Option<Error>,
    graphics_budget_wake: Option<Arc<GraphicsBudgetWake>>,
    graphics_clock: Instant,
    graphics_animation_deadline: Option<Instant>,
    previous_feed_byte: Option<u8>,
    cached_cols: u16,
    cached_rows: u16,
    cached_colors: Option<TerminalColorsSnapshot>,
    cached_cursor: Option<CursorSnapshot>,
    cached_scrollbar: Option<ScrollbarSnapshot>,
    cached_active_screen: Option<ActiveScreenSnapshot>,
    cached_mouse_tracking: Option<bool>,
    cached_selection_present: Option<bool>,
    cached_metadata_revision: Option<u64>,
    geometry: TerminalGeometry,
    active_pointer: Option<ActivePointer>,
    selection_drag_position: Option<SurfacePosition>,
    pointer_mapping_invalidated: bool,
    gesture_clock: GestureClock,
    presentation_generation: PresentationGeneration,
    synchronized_output_last_activity: Option<Instant>,
    find: TerminalFindState,
}

enum MetadataEvent {
    Title(Arc<str>),
    Directory(Arc<str>),
    SemanticPrompt(String),
    Progress { state: u8, value: Option<u8> },
}

#[derive(Clone, Copy, Debug)]
struct ActivePointer {
    button: PointerButton,
    route: PointerRoute,
    generation: PresentationGeneration,
}

#[derive(Clone, Copy, Debug)]
enum PointerRoute {
    Application,
    Selection,
}

enum GestureClock {
    System(Instant),
    #[cfg(test)]
    Manual(Duration),
}

impl GestureClock {
    fn elapsed(&self) -> Duration {
        match self {
            Self::System(epoch) => epoch.elapsed(),
            #[cfg(test)]
            Self::Manual(elapsed) => *elapsed,
        }
    }
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
    pub(crate) selection_completed: bool,
}

impl EmulatorAction {
    fn bytes(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            screen_changed: false,
            selection_completed: false,
        }
    }

    pub(crate) fn screen_changed() -> Self {
        Self {
            bytes: Vec::new(),
            screen_changed: true,
            selection_completed: false,
        }
    }

    fn screen_changed_if(changed: bool) -> Self {
        if changed {
            Self::screen_changed()
        } else {
            Self::none()
        }
    }

    fn selection_completed() -> Self {
        Self {
            bytes: Vec::new(),
            screen_changed: false,
            selection_completed: true,
        }
    }

    fn none() -> Self {
        Self {
            bytes: Vec::new(),
            screen_changed: false,
            selection_completed: false,
        }
    }
}

impl TerminalEmulator {
    #[cfg(test)]
    pub(crate) fn new(geometry: TerminalGeometry) -> Result<Self, Error> {
        Self::new_with_metadata(
            geometry,
            "",
            "",
            None,
            identity::TERM_FALLBACK,
            Instant::now(),
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_metadata(
        geometry: TerminalGeometry,
        initial_directory: &str,
        fallback_title: &str,
        local_hostname: Option<&str>,
        terminal_name: &'static str,
        epoch: Instant,
    ) -> Result<Self, Error> {
        Self::new_with_metadata_context(
            geometry,
            TerminalMetadataContext::local(
                crate::local_path::LocalPathSemantics::Posix,
                initial_directory,
                local_hostname,
            ),
            fallback_title,
            terminal_name,
            epoch,
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_metadata_context(
        geometry: TerminalGeometry,
        metadata_context: TerminalMetadataContext,
        fallback_title: &str,
        terminal_name: &'static str,
        epoch: Instant,
    ) -> Result<Self, Error> {
        Self::new_with_local_filesystem(
            geometry,
            metadata_context,
            fallback_title,
            terminal_name,
            epoch,
            LocalFilesystemAuthority::testing(),
        )
    }

    pub(crate) fn new_with_local_filesystem(
        geometry: TerminalGeometry,
        metadata_context: TerminalMetadataContext,
        fallback_title: &str,
        terminal_name: &'static str,
        epoch: Instant,
        local_filesystem: LocalFilesystemAuthority,
    ) -> Result<Self, Error> {
        let grid = geometry.grid();
        let cell = geometry.backing_cell_size();
        let pty_responses = Rc::new(RefCell::new(Vec::new()));
        let pending_metadata = Rc::new(RefCell::new(Vec::new()));
        let local_file_capabilities = metadata_context.local_file_capabilities();
        let trusted_directory = Rc::new(RefCell::new(
            metadata_context.local_directory(metadata_context.initial_directory()),
        ));
        let pending_attention = Rc::new(RefCell::new(Vec::new()));
        set_png_decoder(Some(Box::new(RustPngDecoder::new())))?;
        let mut terminal: Terminal<'static, 'static> = Terminal::new(TerminalOptions {
            cols: grid.cols,
            rows: grid.rows,
            max_scrollback: MAX_SCROLLBACK_ROWS,
        })?;

        terminal
            .set_kitty_image_storage_limit(0)?
            .set_kitty_image_from_file_allowed(false)?
            .set_kitty_image_from_temp_file_allowed(false)?
            .set_kitty_image_from_shared_mem_allowed(false)?
            .set_apc_max_bytes_kitty(Some(APC_TRANSMISSION_LIMIT))?;

        apply_theme(&mut terminal)?;
        terminal.resize(grid.cols, grid.rows, cell.width, cell.height)?;
        terminal.on_pty_write({
            let pty_responses = Rc::clone(&pty_responses);
            move |_, data| pty_responses.borrow_mut().extend_from_slice(data)
        })?;
        // Image clients need the engine's integer cell geometry. Deriving cells
        // by dividing the fractional PTY pixel extent can underestimate their size
        // and make Unicode-placeholder rows wrap beyond the terminal grid.
        terminal.on_size(|terminal| terminal.size_report().ok())?;
        terminal.on_title_changed({
            let pending_metadata = Rc::clone(&pending_metadata);
            move |terminal| {
                if let Ok(title) = terminal.title() {
                    pending_metadata
                        .borrow_mut()
                        .push(MetadataEvent::Title(Arc::from(title)));
                }
            }
        })?;
        terminal.on_pwd_changed({
            let pending_metadata = Rc::clone(&pending_metadata);
            let trusted_directory = Rc::clone(&trusted_directory);
            let context = metadata_context.clone();
            move |terminal| {
                if let Ok(directory) = terminal.pwd() {
                    let reported =
                        crate::terminal::metadata::parse_osc7_directory(directory, &context);
                    *trusted_directory.borrow_mut() =
                        reported.and_then(|metadata| context.local_directory(&metadata.path));
                    pending_metadata
                        .borrow_mut()
                        .push(MetadataEvent::Directory(Arc::from(directory)));
                }
            }
        })?;
        let local_file_emissions = Rc::new(RefCell::new(LocalFileEmissionRegistry::default()));
        terminal.on_hyperlink_resolve({
            let local_file_emissions = Rc::clone(&local_file_emissions);
            let trusted_directory = Rc::clone(&trusted_directory);
            let local_hostname = metadata_context.local_hostname().map(ToOwned::to_owned);
            move |_, uri| {
                if !has_file_scheme(uri) {
                    return HyperlinkResolution::Passthrough;
                }
                let Some(directory) = trusted_directory.borrow().clone() else {
                    return HyperlinkResolution::Suppress;
                };
                let Ok(uri) = std::str::from_utf8(uri) else {
                    return HyperlinkResolution::Suppress;
                };
                local_file_emissions.borrow_mut().prepare_resolution();
                let Some(target) = HyperlinkTarget::resolve_osc8(
                    uri,
                    &directory,
                    local_hostname.as_deref(),
                    local_file_capabilities,
                    &local_filesystem,
                ) else {
                    return HyperlinkResolution::Suppress;
                };
                let Some(userdata) = target.local_emission_metadata(
                    local_file_capabilities,
                    &mut local_file_emissions.borrow_mut(),
                ) else {
                    return HyperlinkResolution::Suppress;
                };
                HyperlinkResolution::Replace {
                    uri: uri.as_bytes().to_vec(),
                    userdata,
                }
            }
        })?;
        terminal.on_semantic_prompt({
            let pending_metadata = Rc::clone(&pending_metadata);
            move |_, action, options| {
                let Ok(options) = std::str::from_utf8(options) else {
                    return;
                };
                let action = match action {
                    SemanticPromptAction::FreshLine => "L",
                    SemanticPromptAction::FreshLineNewPrompt => "A",
                    SemanticPromptAction::NewCommand => "N",
                    SemanticPromptAction::PromptStart => "P",
                    SemanticPromptAction::EndPromptStartInput => "B",
                    SemanticPromptAction::EndPromptStartInputTerminateEol => "I",
                    SemanticPromptAction::EndInputStartOutput => "C",
                    SemanticPromptAction::EndCommand => "D",
                };
                let value = if options.is_empty() {
                    action.to_owned()
                } else {
                    format!("{action};{options}")
                };
                pending_metadata
                    .borrow_mut()
                    .push(MetadataEvent::SemanticPrompt(value));
            }
        })?;
        terminal.on_progress_report({
            let pending_metadata = Rc::clone(&pending_metadata);
            move |_, state, value| {
                let state = match state {
                    ProgressState::Remove => 0,
                    ProgressState::Set => 1,
                    ProgressState::Error => 2,
                    ProgressState::Indeterminate => 3,
                    ProgressState::Pause => 4,
                };
                pending_metadata
                    .borrow_mut()
                    .push(MetadataEvent::Progress { state, value });
            }
        })?;
        terminal.on_xtversion(|_| Some(identity::XTVERSION))?;
        terminal.on_device_attributes(|_| Some(identity::device_attributes()))?;
        terminal.on_bell({
            let pending_attention = Rc::clone(&pending_attention);
            move |_| pending_attention.borrow_mut().push(AttentionEvent::Bell)
        })?;

        let metadata = MetadataTracker::new_with_context(metadata_context, fallback_title, epoch);
        let title = Arc::clone(&metadata.snapshot().title.value);

        let mut mouse_encoder = MouseEncoder::new()?;
        mouse_encoder.set_track_last_cell(true);

        Ok(Self {
            local_file_emissions,
            terminal,
            ghostty_accessibility: ghostty_accessibility::State::new()?,
            accessibility: TerminalAccessibilityState::default(),
            accessibility_generation: PresentationGeneration::default(),
            render_state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            keyboard_protocol: KeyboardProtocolEncoder::new()?,
            mouse_encoder,
            mouse_event: MouseEvent::new()?,
            cached_mouse_modes: None,
            cached_mouse_size: None,
            selection_gesture: Gesture::new()?,
            selection_press: PressEvent::new()?,
            selection_drag: DragEvent::new()?,
            selection_release: ReleaseEvent::new()?,
            selection_autoscroll_tick: AutoscrollTickEvent::new()?,
            pty_responses,
            pending_metadata,
            pending_attention,
            title,
            metadata,
            local_file_capabilities,
            xtgettcap: XtGetTcapObserver::new(terminal_name),
            primary_row_cache: Vec::new(),
            alternate_row_cache: Vec::new(),
            primary_graphics: GraphicsState::default(),
            alternate_graphics: GraphicsState::default(),
            graphics_reservation: None,
            graphics_failure: None,
            graphics_budget_wake: None,
            graphics_clock: Instant::now(),
            graphics_animation_deadline: None,
            previous_feed_byte: None,
            cached_cols: 0,
            cached_rows: 0,
            cached_colors: None,
            cached_cursor: None,
            cached_scrollbar: None,
            cached_active_screen: None,
            cached_mouse_tracking: None,
            cached_selection_present: None,
            cached_metadata_revision: None,
            geometry,
            active_pointer: None,
            selection_drag_position: None,
            pointer_mapping_invalidated: false,
            gesture_clock: GestureClock::System(Instant::now()),
            presentation_generation: PresentationGeneration::default(),
            synchronized_output_last_activity: None,
            find: TerminalFindState::default(),
        })
    }

    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        self.feed_at(bytes, Instant::now());
    }

    pub(crate) fn feed_at(&mut self, bytes: &[u8], now: Instant) {
        if !bytes.is_empty()
            && matches!(
                self.active_pointer,
                Some(ActivePointer {
                    route: PointerRoute::Selection,
                    ..
                })
            )
        {
            self.pointer_mapping_invalidated = true;
        }
        self.xtgettcap
            .feed(bytes, &mut self.pty_responses.borrow_mut());
        let command_was_finished =
            self.metadata
                .snapshot()
                .command
                .as_ref()
                .is_some_and(|command| {
                    matches!(
                        command.state,
                        crate::terminal::metadata::CommandState::Finished { .. }
                    )
                });
        let synchronized_before = self.terminal.mode(Mode::SYNC_OUTPUT).unwrap_or(false);
        if self.graphics_reservation.is_none() && starts_apc(self.previous_feed_byte, bytes) {
            self.graphics_reservation = Some(GraphicsReservation::default());
        }
        if let Some(reservation) = &mut self.graphics_reservation {
            if let Err(error) = reservation.write(&mut self.terminal, bytes) {
                self.graphics_failure = Some(error);
            }
        } else {
            self.terminal.vt_write(bytes);
        }
        for event in self.pending_metadata.borrow_mut().drain(..) {
            match event {
                MetadataEvent::Title(title) => {
                    self.metadata.set_reported_title(&title);
                }
                MetadataEvent::Directory(directory) => {
                    self.metadata.set_reported_directory(&directory);
                }
                MetadataEvent::SemanticPrompt(value) => {
                    self.metadata.apply_semantic_prompt(&value, now);
                }
                MetadataEvent::Progress { state, value } => {
                    self.metadata.apply_progress_report(state, value);
                }
            }
        }
        if !command_was_finished
            && let Some(command) = &self.metadata.snapshot().command
            && let crate::terminal::metadata::CommandState::Finished {
                exit_status,
                duration,
            } = command.state
        {
            self.pending_attention
                .borrow_mut()
                .push(AttentionEvent::CommandFinished {
                    exit_status,
                    duration,
                });
        }
        if !bytes.is_empty() {
            self.find.invalidate();
        }
        let synchronized_after = self.terminal.mode(Mode::SYNC_OUTPUT).unwrap_or(false);
        self.synchronized_output_last_activity = match (synchronized_before, synchronized_after) {
            (false, true) => Some(now),
            // The one-second safeguard is an inactivity deadline. A large remote redraw may
            // legitimately keep one synchronized transaction open for longer than a second while
            // output is still arriving; releasing it on total wall-clock duration exposes the
            // producer's intermediate grid.
            (true, true) if !bytes.is_empty() => Some(now),
            (true, true) => self.synchronized_output_last_activity.or(Some(now)),
            (_, false) => None,
        };
        if let Some(last) = bytes.last().copied() {
            self.previous_feed_byte = Some(last);
        }
    }

    pub(crate) fn graphics_animation_deadline(&self) -> Option<Instant> {
        self.graphics_animation_deadline
    }

    pub(crate) fn graphics_failure(&self) -> Option<Error> {
        self.graphics_failure
    }

    pub(crate) fn set_graphics_budget_wakeup(&mut self, notify: impl Fn() + Send + Sync + 'static) {
        self.graphics_budget_wake = Some(GraphicsBudgetWake::new(notify));
    }

    pub(crate) fn take_graphics_budget_wakeup(&self) -> bool {
        self.graphics_budget_wake
            .as_ref()
            .is_some_and(|wake| wake.take())
    }

    fn advance_graphics_animations(&mut self, now: Instant) -> Result<(), Error> {
        if self.graphics_reservation.is_none() {
            return Ok(());
        }
        let now_ms = u64::try_from(
            now.saturating_duration_since(self.graphics_clock)
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        self.graphics_animation_deadline = self
            .terminal
            .tick_kitty_animations(now_ms)?
            .and_then(|delay| now.checked_add(Duration::from_millis(delay.max(1))));
        Ok(())
    }

    pub(crate) fn synchronized_output_deadline(&self) -> Option<Instant> {
        self.synchronized_output_last_activity
            .map(|last_activity| last_activity + MAX_SYNCHRONIZED_OUTPUT_DURATION)
    }

    pub(crate) fn presentation_generation(&self) -> PresentationGeneration {
        self.presentation_generation
    }

    pub(crate) fn expire_synchronized_output(&mut self, now: Instant) -> Result<bool, Error> {
        if self
            .synchronized_output_deadline()
            .is_none_or(|deadline| now < deadline)
        {
            return Ok(false);
        }
        self.end_synchronized_output()
    }

    pub(crate) fn end_synchronized_output(&mut self) -> Result<bool, Error> {
        self.synchronized_output_last_activity = None;
        if !self.terminal.mode(Mode::SYNC_OUTPUT)? {
            return Ok(false);
        }
        self.terminal.set_mode(Mode::SYNC_OUTPUT, false)?;
        Ok(true)
    }

    pub(crate) fn resize(&mut self, geometry: TerminalGeometry) -> Result<(), Error> {
        self.end_synchronized_output()?;
        self.selection_gesture.reset(&self.terminal);
        self.active_pointer = None;
        self.selection_drag_position = None;
        self.pointer_mapping_invalidated = false;
        let grid = geometry.grid();
        let cell = geometry.backing_cell_size();
        self.terminal
            .resize(grid.cols, grid.rows, cell.width, cell.height)?;
        self.find.invalidate();
        self.geometry = geometry;
        Ok(())
    }

    pub(crate) fn take_pty_responses(&self) -> Vec<u8> {
        mem::take(&mut *self.pty_responses.borrow_mut())
    }

    pub(crate) fn take_attention_events(&self) -> Vec<AttentionEvent> {
        mem::take(&mut *self.pending_attention.borrow_mut())
    }

    pub(crate) fn key(&mut self, input: KeyInput) -> Result<EmulatorAction, String> {
        if !input.is_modifier_key() && matches!(input.action, KeyAction::Press | KeyAction::Repeat)
        {
            self.clear_selection()?;
            self.terminal.scroll_viewport(ScrollViewport::Bottom);
        }
        let mut bytes = Vec::new();
        self.encode_key(&input, &mut bytes)?;
        Ok(EmulatorAction {
            bytes,
            screen_changed: true,
            selection_completed: false,
        })
    }

    pub(crate) fn focus_reporting_enabled(&self) -> Result<bool, String> {
        self.terminal
            .mode(Mode::FOCUS_EVENT)
            .map_err(|error| format!("failed to query terminal focus reporting mode: {error}"))
    }

    pub(crate) fn set_find_query(
        &mut self,
        generation: FindQueryGeneration,
        query: String,
    ) -> EmulatorAction {
        self.find.set_query(generation, query);
        EmulatorAction::screen_changed()
    }

    pub(crate) fn end_find(&mut self, generation: FindQueryGeneration) -> EmulatorAction {
        self.find.end(generation);
        EmulatorAction::screen_changed()
    }

    pub(crate) fn navigate_find(
        &mut self,
        generation: FindQueryGeneration,
        direction: FindDirection,
    ) -> Result<EmulatorAction, String> {
        let cols = self.geometry.grid().cols;
        self.find
            .navigate(&mut self.terminal, cols, generation, direction)
            .map(EmulatorAction::screen_changed_if)
            .map_err(|error| format!("failed to navigate terminal Find results: {error}"))
    }

    pub(crate) fn focus(&self, focused: bool) -> Result<EmulatorAction, String> {
        if !self.focus_reporting_enabled()? {
            return Ok(EmulatorAction::none());
        }

        let event = if focused {
            FocusEvent::Gained
        } else {
            FocusEvent::Lost
        };
        let mut buffer = [0_u8; 8];
        let written = event
            .encode(&mut buffer)
            .map_err(|error| format!("failed to encode terminal focus event: {error}"))?;
        Ok(EmulatorAction::bytes(buffer[..written].to_vec()))
    }

    pub(crate) fn pointer(&mut self, input: PointerInput) -> Result<EmulatorAction, String> {
        if self
            .terminal
            .mode(Mode::SYNC_OUTPUT)
            .map_err(|error| format!("failed to query synchronized-output mode: {error}"))?
            || !self.accept_pointer_generation(input.generation)
        {
            self.selection_gesture.reset(&self.terminal);
            self.active_pointer = None;
            self.selection_drag_position = None;
            self.pointer_mapping_invalidated = false;
            return Ok(EmulatorAction::none());
        }
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

    pub(crate) fn scroll_to_at(
        &mut self,
        offset_rows: u64,
        generation: PresentationGeneration,
    ) -> EmulatorAction {
        if generation != self.presentation_generation {
            return EmulatorAction::none();
        }
        self.scroll_to(offset_rows)
    }

    pub(crate) fn wheel(&mut self, input: WheelInput) -> Result<EmulatorAction, String> {
        if input.generation != self.presentation_generation
            || self
                .terminal
                .mode(Mode::SYNC_OUTPUT)
                .map_err(|error| format!("failed to query synchronized-output mode: {error}"))?
        {
            return Ok(EmulatorAction::none());
        }
        let horizontal = input
            .horizontal_steps
            .clamp(-MAX_WHEEL_STEPS, MAX_WHEEL_STEPS);
        let vertical = input
            .vertical_steps
            .clamp(-MAX_WHEEL_STEPS, MAX_WHEEL_STEPS);
        if horizontal == 0 && vertical == 0 {
            return Ok(EmulatorAction::none());
        }

        let tracking = self
            .terminal
            .is_mouse_tracking()
            .map_err(|error| format!("failed to query terminal mouse tracking mode: {error}"))?;
        if tracking && !shift_overrides_application_mouse(input.modifiers, input.shift_selection) {
            self.clear_selection()?;
            let any_button_pressed = self.active_pointer.is_some();
            let mut bytes = Vec::new();
            for (steps, positive, negative) in [
                (horizontal, MouseButton::Six, MouseButton::Seven),
                (vertical, MouseButton::Four, MouseButton::Five),
            ] {
                let button = if steps > 0 { positive } else { negative };
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
            }
            return Ok(EmulatorAction {
                bytes,
                screen_changed: true,
                selection_completed: false,
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
        if alternate_screen && alternate_scroll && vertical != 0 {
            self.clear_selection()?;
            let key = KeyInput {
                action: KeyAction::Press,
                physical_key: if vertical > 0 {
                    PhysicalKey::ArrowUp
                } else {
                    PhysicalKey::ArrowDown
                },
                native_key_code: None,
                logical_key: if vertical > 0 { "up" } else { "down" }.to_owned(),
                text: None,
                unshifted_codepoint: None,
                modifiers: InputModifiers::default(),
                consumed_modifiers: InputModifiers::default(),
                option_as_alt: OptionAsAltPolicy::default(),
            };
            let mut bytes = Vec::new();
            for _ in 0..vertical.unsigned_abs() {
                self.encode_key(&key, &mut bytes)?;
            }
            return Ok(EmulatorAction {
                bytes,
                screen_changed: true,
                selection_completed: false,
            });
        }

        if vertical == 0 {
            return Ok(EmulatorAction::none());
        }

        self.terminal
            .scroll_viewport(ScrollViewport::Delta(-(vertical as isize)));
        Ok(EmulatorAction::screen_changed())
    }

    pub(crate) fn paste(&mut self, text: String) -> Result<EmulatorAction, String> {
        self.clear_selection()?;
        let bracketed = self.bracketed_paste_mode()?;
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
            selection_completed: false,
        })
    }

    pub(crate) fn bracketed_paste_mode(&self) -> Result<bool, String> {
        self.terminal
            .mode(Mode::BRACKETED_PASTE)
            .map_err(|error| format!("failed to query bracketed-paste mode: {error}"))
    }

    #[cfg(test)]
    pub(crate) fn selection_text(&self) -> Result<Option<String>, String> {
        self.format_selection(Format::Plain, SelectionCopyOptions::default())
    }

    pub(crate) fn selection_copy(
        &self,
        options: SelectionCopyOptions,
    ) -> Result<Option<SelectionCopy>, String> {
        let Some(plain_text) = self.format_selection(Format::Plain, options)? else {
            return Ok(None);
        };
        let html = if options.include_html {
            self.format_selection(Format::Html, options)?
        } else {
            None
        };
        Ok(Some(SelectionCopy { plain_text, html }))
    }

    fn format_selection(
        &self,
        format: Format,
        options: SelectionCopyOptions,
    ) -> Result<Option<String>, String> {
        let options = FormatOptions::new()
            .with_emit_format(format)
            .with_unwrap(options.unwrap_soft_wraps)
            .with_trim(options.trailing_spaces == TrailingSpacePolicy::Trim);
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
                    "formatted terminal selection contained invalid UTF-8 at byte {}",
                    error.utf8_error().valid_up_to()
                )
            })
    }

    fn accept_pointer_generation(&mut self, generation: PresentationGeneration) -> bool {
        let Some(active) = self.active_pointer.as_mut() else {
            return generation == self.presentation_generation;
        };
        if self.pointer_mapping_invalidated {
            if generation == self.presentation_generation && generation != active.generation {
                active.generation = generation;
                self.pointer_mapping_invalidated = false;
                return true;
            }
            return false;
        }
        if generation < active.generation || generation > self.presentation_generation {
            return false;
        }
        active.generation = generation;
        true
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
                if !tracking
                    || shift_overrides_application_mouse(input.modifiers, input.shift_selection)
                {
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
        self.active_pointer = Some(ActivePointer {
            button,
            route,
            generation: input.generation,
        });

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
                    selection_completed: false,
                })
            }
            PointerRoute::Selection => {
                self.selection_drag_position = Some(input.position);
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
                ..
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
                self.selection_drag_position = Some(input.position);
                self.selection_drag(input.position)?;
                Ok(EmulatorAction::screen_changed())
            }
            None => {
                let tracking = self.terminal.is_mouse_tracking().map_err(|error| {
                    format!("failed to query terminal mouse tracking mode: {error}")
                })?;
                if !tracking
                    || shift_overrides_application_mouse(input.modifiers, input.shift_selection)
                {
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
        self.selection_drag_position = None;

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
                Ok(EmulatorAction::selection_completed())
            }
        }
    }

    fn encode_key(&mut self, input: &KeyInput, bytes: &mut Vec<u8>) -> Result<(), String> {
        self.keyboard_protocol.encode(&self.terminal, input, bytes)
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
        self.selection_drag_position = None;
        Ok(())
    }

    pub(crate) fn set_accessibility_selection(
        &mut self,
        request: AccessibilitySelectionRequest,
    ) -> Result<EmulatorAction, String> {
        if request.generation != self.presentation_generation {
            return Ok(EmulatorAction::none());
        }
        if self
            .terminal
            .mode(Mode::SYNC_OUTPUT)
            .map_err(|error| format!("failed to query synchronized-output mode: {error}"))?
        {
            return Ok(EmulatorAction::none());
        }
        let Some(selection) = self.accessibility.resolve_selection(&request) else {
            return Ok(EmulatorAction::none());
        };
        let selection = selection.map(|(start, end)| {
            let convert = |reference: AccessibilityCellRef| ghostty_accessibility::CellRef {
                row: ghostty_accessibility::RowId {
                    screen: match reference.row.screen {
                        AccessibilityScreen::Primary => ghostty_accessibility::Screen::Primary,
                        AccessibilityScreen::Alternate => ghostty_accessibility::Screen::Alternate,
                    },
                    screen_generation: reference.row.screen_generation as u64,
                    node_serial: reference.row.node_serial,
                    page_row: reference.row.page_row,
                },
                row_revision: reference.row_revision,
                column: reference.column,
            };
            (convert(start), convert(end))
        });
        let changed = self
            .ghostty_accessibility
            .set_selection(&self.terminal, selection)
            .map_err(|error| format!("failed to set terminal accessibility selection: {error}"))?;
        if !changed {
            return Ok(EmulatorAction::none());
        }
        self.active_pointer = None;
        self.selection_gesture.reset(&self.terminal);
        self.selection_drag_position = None;
        self.pointer_mapping_invalidated = false;
        Ok(EmulatorAction::screen_changed())
    }

    fn selection_press(&mut self, position: SurfacePosition) -> Result<(), String> {
        let point = self.selection_viewport_point(position)?;
        let grid_ref = self
            .terminal
            .grid_ref(Point::Viewport(point))
            .map_err(|error| format!("failed to resolve selection press position: {error}"))?;
        let selection = self
            .selection_press
            .set_position(f64::from(position.x), f64::from(position.y))
            .and_then(|event| event.set_repeat_distance(REPEAT_CLICK_DISTANCE_PX))
            .and_then(|event| event.set_time(self.gesture_clock.elapsed()))
            .and_then(|event| event.set_repeat_interval(REPEAT_CLICK_INTERVAL))
            .and_then(|event| event.apply(&mut self.selection_gesture, &self.terminal, grid_ref))
            .map_err(|error| format!("failed to apply terminal selection press: {error}"))?;
        self.terminal
            .set_selection(selection.as_ref())
            .map_err(|error| format!("failed to install terminal selection: {error}"))?;
        Ok(())
    }

    fn selection_drag(&mut self, position: SurfacePosition) -> Result<(), String> {
        let point = self.selection_viewport_point(position)?;
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
        let point = self.selection_viewport_point(position)?;
        let grid_ref = self
            .terminal
            .grid_ref(Point::Viewport(point))
            .map_err(|error| format!("failed to resolve selection release position: {error}"))?;
        self.selection_release
            .apply(&mut self.selection_gesture, &self.terminal, Some(grid_ref))
            .map_err(|error| format!("failed to apply terminal selection release: {error}"))
    }

    pub(crate) fn selection_autoscroll_interval(&self) -> Result<Option<Duration>, String> {
        if !matches!(
            self.active_pointer,
            Some(ActivePointer {
                route: PointerRoute::Selection,
                ..
            })
        ) {
            return Ok(None);
        }
        let Some(position) = self.selection_drag_position else {
            return Ok(None);
        };
        let direction = self
            .selection_gesture
            .autoscroll(&self.terminal)
            .map_err(|error| format!("failed to query selection autoscroll: {error}"))?;
        if direction == Autoscroll::None {
            return Ok(None);
        }
        Ok(selection_autoscroll_interval_for_position(
            position,
            self.geometry.backing_grid_size().height,
            self.geometry.backing_cell_size().height,
        ))
    }

    pub(crate) fn selection_autoscroll_tick(
        &mut self,
        generation: PresentationGeneration,
    ) -> Result<EmulatorAction, String> {
        if generation != self.presentation_generation {
            self.selection_gesture.reset(&self.terminal);
            self.active_pointer = None;
            self.selection_drag_position = None;
            return Ok(EmulatorAction::none());
        }
        let Some(position) = self.selection_drag_position else {
            return Ok(EmulatorAction::none());
        };
        let direction = self
            .selection_gesture
            .autoscroll(&self.terminal)
            .map_err(|error| format!("failed to query selection autoscroll: {error}"))?;
        let delta = match direction {
            Autoscroll::Up => -1,
            Autoscroll::Down => 1,
            Autoscroll::None => return Ok(EmulatorAction::none()),
            _ => return Ok(EmulatorAction::none()),
        };
        self.terminal.scroll_viewport(ScrollViewport::Delta(delta));
        let viewport = self.selection_viewport_point(position)?;
        let geometry = self.selection_geometry();
        let selection = self
            .selection_autoscroll_tick
            .set_position(f64::from(position.x), f64::from(position.y))
            .and_then(|event| event.set_rectangle(false))
            .and_then(|event| {
                event.apply(
                    &mut self.selection_gesture,
                    &self.terminal,
                    viewport,
                    geometry,
                )
            })
            .map_err(|error| format!("failed to apply selection autoscroll tick: {error}"))?;
        self.terminal
            .set_selection(selection.as_ref())
            .map_err(|error| format!("failed to install autoscrolled selection: {error}"))?;
        Ok(EmulatorAction::screen_changed())
    }

    #[cfg(test)]
    fn set_gesture_time_for_test(&mut self, elapsed: Duration) {
        self.gesture_clock = GestureClock::Manual(elapsed);
    }

    fn selection_viewport_point(
        &self,
        position: SurfacePosition,
    ) -> Result<PointCoordinate, String> {
        let position = self
            .geometry
            .cell_at_backing_position(BackingPosition::new(position.x, position.y));
        let mut point = PointCoordinate {
            x: position.col,
            y: u32::from(position.row),
        };
        let grid_ref = self
            .terminal
            .grid_ref(Point::Viewport(point))
            .map_err(|error| format!("failed to resolve terminal selection cell: {error}"))?;
        if grid_ref
            .cell()
            .and_then(|cell| cell.wide())
            .map_err(|error| format!("failed to inspect terminal selection cell: {error}"))?
            == CellWide::SpacerTail
        {
            point.x = point.x.saturating_sub(1);
        }
        Ok(point)
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

    pub(crate) fn accessibility_snapshot(
        &mut self,
        bind_next_presentation: bool,
    ) -> Result<(Option<Arc<TerminalAccessibilityModel>>, bool), String> {
        if self
            .terminal
            .mode(Mode::SYNC_OUTPUT)
            .map_err(|error| format!("failed to query synchronized-output mode: {error}"))?
        {
            return Ok((None, false));
        }
        if bind_next_presentation {
            self.accessibility_generation = self.presentation_generation.next();
        }
        let snapshot = self
            .ghostty_accessibility
            .update(
                &self.terminal,
                ghostty_accessibility::UpdateOptions {
                    max_cells: 16_384,
                    max_rows: 256,
                },
            )
            .map_err(|error| format!("failed to observe retained terminal text: {error}"))?;
        let more = snapshot.more;
        let update = accessibility_update(snapshot)?;
        let model = self
            .accessibility
            .apply(update, self.accessibility_generation);
        Ok((model, more))
    }

    pub(crate) fn accessibility_snapshot_for_current_presentation(
        &mut self,
    ) -> Result<(Option<Arc<TerminalAccessibilityModel>>, bool), String> {
        if self
            .terminal
            .mode(Mode::SYNC_OUTPUT)
            .map_err(|error| format!("failed to query synchronized-output mode: {error}"))?
        {
            return Ok((None, false));
        }
        self.accessibility_generation = self.presentation_generation;
        self.accessibility_snapshot(false)
    }

    pub(crate) fn snapshot(&mut self) -> Result<Option<Arc<ScreenSnapshot>>, Error> {
        self.snapshot_at(Instant::now())
    }

    fn snapshot_at(&mut self, now: Instant) -> Result<Option<Arc<ScreenSnapshot>>, Error> {
        if let Some(error) = self.graphics_failure {
            return Err(error);
        }
        if self.terminal.mode(Mode::SYNC_OUTPUT)? {
            return Ok(None);
        }

        self.advance_graphics_animations(now)?;

        self.find
            .refresh(&self.terminal, self.geometry.grid().cols)?;
        let find_changed = self.find.is_changed();

        let metadata = self.metadata.snapshot();
        let title = Arc::clone(&metadata.title.value);
        let title_changed = title != self.title;
        let snapshot = self.render_state.update(&self.terminal)?;
        let dirty = snapshot.dirty()?;
        let rows = snapshot.rows()?;
        let cols = snapshot.cols()?;
        let colors = snapshot.colors()?;
        let cursor_position = snapshot
            .cursor_viewport()?
            .map(|cursor| (cursor.x, cursor.y, cursor.at_wide_tail));
        let cursor_visible = snapshot.cursor_visible()?;
        let cursor_blinking = snapshot.cursor_blinking()?;
        let cursor_password_input = snapshot.cursor_password_input()?;
        let cursor_shape: CursorShapeSnapshot = snapshot.cursor_visual_style()?.into();
        let cursor_color: Color = snapshot.cursor_color()?.unwrap_or(colors.foreground).into();
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
        if self.graphics_reservation.is_some() {
            let resident = self.terminal.kitty_image_storage_bytes()?;
            // Reset can destroy the inactive screen without presenting it again.
            // Release its cached pixels while existing UI snapshots retain their
            // own reservations, so cleared images cannot consume capacity forever.
            match active_screen {
                ActiveScreenSnapshot::Primary if resident[1] == 0 => {
                    self.alternate_graphics = GraphicsState::default();
                }
                ActiveScreenSnapshot::Alternate if resident[0] == 0 => {
                    self.primary_graphics = GraphicsState::default();
                }
                _ => {}
            }
        }
        let (previous_graphics_generation, previous_graphics_placements) = {
            let previous = match active_screen {
                ActiveScreenSnapshot::Primary => self.primary_graphics.published(),
                ActiveScreenSnapshot::Alternate => self.alternate_graphics.published(),
            };
            (previous.generation, Arc::clone(&previous.placements))
        };
        let graphics = if self.graphics_reservation.is_some() {
            match active_screen {
                ActiveScreenSnapshot::Primary => self
                    .primary_graphics
                    .snapshot(&self.terminal, self.graphics_budget_wake.as_ref())?,
                ActiveScreenSnapshot::Alternate => self
                    .alternate_graphics
                    .snapshot(&self.terminal, self.graphics_budget_wake.as_ref())?,
            }
        } else {
            GraphicsSnapshot::default()
        };
        let graphics_content_changed = previous_graphics_generation != graphics.generation;
        let graphics_geometry_changed = previous_graphics_placements != graphics.placements;
        let mouse_tracking = self.terminal.is_mouse_tracking()?;
        let selection_present = self.terminal.has_selection()?;
        let row_cache = match active_screen {
            ActiveScreenSnapshot::Primary => &self.primary_row_cache,
            ActiveScreenSnapshot::Alternate => &self.alternate_row_cache,
        };
        let other_row_cache = match active_screen {
            ActiveScreenSnapshot::Primary => &self.alternate_row_cache,
            ActiveScreenSnapshot::Alternate => &self.primary_row_cache,
        };

        let terminal_colors = TerminalColorsSnapshot {
            foreground: colors.foreground.into(),
            background: colors.background.into(),
            palette: Arc::new(colors.palette.map(Color::from)),
            reversed: self.terminal.mode(Mode::REVERSE_COLORS)?,
        };
        let build_cursor = |rows: &[RowSnapshot]| CursorSnapshot {
            position: cursor_position.map(|(column, row, at_wide_tail)| {
                normalize_cursor_position(column, row, at_wide_tail, rows)
            }),
            visible: cursor_visible,
            blinking: cursor_blinking,
            password_input: cursor_password_input,
            shape: cursor_shape,
            color: cursor_color,
            text_color: terminal_colors.effective_background(),
        };
        let mut cursor = build_cursor(row_cache);
        let rebuild_all = matches!(dirty, Dirty::Full)
            || row_cache.len() != usize::from(rows)
            || self.cached_cols != cols
            || self.cached_colors.as_ref() != Some(&terminal_colors);
        let previous_scrollbar = self.cached_scrollbar;
        let first_snapshot = self.cached_colors.is_none();
        let mut damage = if first_snapshot {
            SnapshotDamage::initial()
        } else {
            SnapshotDamage {
                cursor: cursor_damage(self.cached_cursor.as_ref(), &cursor),
                title: title_changed,
                metadata: self.cached_metadata_revision != Some(metadata.revision),
                scrollbar: previous_scrollbar
                    .is_some_and(|previous| previous.total_rows != scrollbar.total_rows),
                viewport: previous_scrollbar.is_some_and(|previous| {
                    previous.offset_rows != scrollbar.offset_rows
                        || previous.visible_rows != scrollbar.visible_rows
                }),
                active_screen: self.cached_active_screen != Some(active_screen),
                resize: self.cached_cols != cols || self.cached_rows != rows,
                mouse_tracking: self.cached_mouse_tracking != Some(mouse_tracking),
                selection_presence: self.cached_selection_present != Some(selection_present),
                search: find_changed,
                graphics_content: graphics_content_changed,
                graphics_geometry: graphics_geometry_changed,
                ..SnapshotDamage::default()
            }
        };

        if matches!(dirty, Dirty::Clean) && !rebuild_all && damage.is_clean() {
            return Ok(None);
        }

        let mut rendered_rows = if rebuild_all {
            Vec::with_capacity(usize::from(rows))
        } else {
            row_cache.clone()
        };
        let mut web_hyperlink_targets =
            HashMap::<String, Option<Arc<crate::terminal::HyperlinkTarget>>>::new();
        let mut local_hyperlink_targets =
            HashMap::<Vec<u8>, Option<Arc<crate::terminal::HyperlinkTarget>>>::new();
        let mut row_soft_wrapped = Vec::with_capacity(usize::from(rows));
        let mut dirty_rows = Vec::new();
        let mut row_index = 0_u16;
        {
            let mut row_iteration = self.rows.update(&snapshot)?;

            while let Some(row) = row_iteration.next() {
                row_soft_wrapped.push(row.raw_row()?.is_wrapped()?);
                let rebuild_row = rebuild_all || row.dirty()?;

                if rebuild_row {
                    let selection = row.selection()?;
                    let mut rendered_cells = Vec::with_capacity(usize::from(cols));
                    let mut hyperlink_uri = [0; crate::terminal::hyperlink::MAX_LINK_BYTES];
                    let mut hyperlink_userdata = [0; crate::terminal::hyperlink::MAX_LINK_BYTES];
                    let mut column_index = 0_u16;
                    let mut cell_iteration = self.cells.update(row)?;

                    while let Some(cell) = cell_iteration.next() {
                        let style = cell.style()?;
                        let raw_cell = cell.raw_cell()?;
                        let background_source = match raw_cell.content_tag()? {
                            CellContentTag::BgColorPalette => {
                                TerminalColor::Palette(raw_cell.bg_color_palette()?.0)
                            }
                            CellContentTag::BgColorRgb => {
                                TerminalColor::Rgb(raw_cell.bg_color_rgb()?.into())
                            }
                            _ => style.bg_color.into(),
                        };
                        let spacer_tail = matches!(raw_cell.wide()?, CellWide::SpacerTail);
                        let text = if spacer_tail {
                            " ".to_owned()
                        } else {
                            let graphemes = cell.graphemes()?;
                            if graphemes.is_empty() {
                                " ".to_owned()
                            } else {
                                graphemes.into_iter().collect()
                            }
                        };
                        let hyperlink = if raw_cell.has_hyperlink()? {
                            let reference =
                                self.terminal.grid_ref(Point::Viewport(PointCoordinate {
                                    x: column_index,
                                    y: u32::from(row_index),
                                }))?;
                            let uri_length = reference.hyperlink_uri(&mut hyperlink_uri).ok();
                            let userdata_length =
                                reference.hyperlink_userdata(&mut hyperlink_userdata).ok();
                            uri_length
                                .zip(userdata_length)
                                .and_then(|(uri_length, userdata_length)| {
                                    let uri = std::str::from_utf8(&hyperlink_uri[..uri_length]).ok()?;
                                    if userdata_length == 0 {
                                        if let Some(target) = web_hyperlink_targets.get(uri) {
                                            return target.clone();
                                        }
                                        let target = crate::terminal::HyperlinkTarget::url(uri)
                                            .map(Arc::new);
                                        web_hyperlink_targets
                                            .insert(uri.to_owned(), target.clone());
                                        return target;
                                    }
                                    if !has_file_scheme(uri.as_bytes()) {
                                        return None;
                                    }
                                    let userdata = &hyperlink_userdata[..userdata_length];
                                    if let Some(target) = local_hyperlink_targets.get(userdata) {
                                        return target.clone();
                                    }
                                    let target = crate::terminal::HyperlinkTarget::from_local_emission_metadata(
                                        userdata,
                                        self.local_file_capabilities,
                                        &self.local_file_emissions.borrow(),
                                    )
                                    .map(Arc::new);
                                    local_hyperlink_targets
                                        .insert(userdata.to_vec(), target.clone());
                                    target
                                })
                        } else {
                            None
                        };

                        rendered_cells.push(CellSnapshot {
                            text,
                            foreground_source: style.fg_color.into(),
                            background_source,
                            inverse: style.inverse,
                            bold: style.bold,
                            faint: style.faint,
                            italic: style.italic,
                            blinking: style.blink,
                            invisible: style.invisible,
                            underline: style.underline.into(),
                            underline_source: style.underline_color.into(),
                            strikethrough: style.strikethrough,
                            overline: style.overline,
                            selected: selection.is_some_and(|range| {
                                column_index >= range.start_x && column_index <= range.end_x
                            }),
                            spacer_tail,
                            semantic_content: raw_cell.semantic_content()?.into(),
                            hyperlink,
                        });
                        column_index = column_index.saturating_add(1);
                    }

                    let detected = crate::terminal::hyperlink::detect_url_cells(
                        rendered_cells.iter().map(|cell| cell.text.as_str()),
                    );
                    for (cell, detected) in rendered_cells.iter_mut().zip(detected) {
                        if cell.hyperlink.is_none() {
                            cell.hyperlink = detected;
                        }
                    }
                    let rendered_row = Arc::<[CellSnapshot]>::from(rendered_cells);
                    let previous_row = row_cache.get(usize::from(row_index));
                    let rendered_row = previous_row
                        .into_iter()
                        .chain(other_row_cache.get(usize::from(row_index)))
                        .find(|cached| cached.as_ref() == rendered_row.as_ref())
                        .cloned()
                        .unwrap_or(rendered_row);
                    if rebuild_all {
                        let row_changed = previous_row.map_or_else(
                            || {
                                other_row_cache
                                    .get(usize::from(row_index))
                                    .is_none_or(|cached| !Arc::ptr_eq(cached, &rendered_row))
                            },
                            |cached| !Arc::ptr_eq(cached, &rendered_row),
                        );
                        if row_changed {
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

        cursor = build_cursor(&rendered_rows);
        if !first_snapshot {
            damage.cursor = cursor_damage(self.cached_cursor.as_ref(), &cursor);
        }

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

        match active_screen {
            ActiveScreenSnapshot::Primary => self.primary_row_cache = rendered_rows,
            ActiveScreenSnapshot::Alternate => self.alternate_row_cache = rendered_rows,
        }
        let row_cache = match active_screen {
            ActiveScreenSnapshot::Primary => &self.primary_row_cache,
            ActiveScreenSnapshot::Alternate => &self.alternate_row_cache,
        };
        self.cached_cols = cols;
        self.cached_rows = rows;
        self.cached_colors = Some(terminal_colors.clone());
        self.cached_cursor = Some(cursor);
        self.cached_scrollbar = Some(scrollbar);
        self.cached_active_screen = Some(active_screen);
        self.cached_mouse_tracking = Some(mouse_tracking);
        self.cached_selection_present = Some(selection_present);
        self.cached_metadata_revision = Some(metadata.revision);
        self.title = Arc::clone(&title);

        self.presentation_generation = self.presentation_generation.next();
        let text_blinking = rows_have_visible_blinking_text(row_cache);
        let find = self
            .find
            .snapshot(cols, scrollbar.offset_rows, scrollbar.visible_rows);
        self.find.mark_published();
        Ok(Some(Arc::new(ScreenSnapshot {
            generation: self.presentation_generation,
            rows: Arc::from(row_cache.clone()),
            row_soft_wrapped: Arc::from(row_soft_wrapped),
            background: terminal_colors.effective_background(),
            colors: terminal_colors,
            size,
            viewport,
            scrollbar,
            active_screen,
            cursor,
            text_blinking,
            mouse_tracking,
            selection_present,
            title,
            metadata,
            find,
            graphics,
            damage,
        })))
    }

    pub(crate) fn mark_metadata_stale(&mut self) {
        self.metadata.mark_stale();
    }
}

impl Drop for TerminalEmulator {
    fn drop(&mut self) {
        self.selection_gesture.reset(&self.terminal);
    }
}

fn accessibility_update(
    snapshot: ghostty_accessibility::Snapshot,
) -> Result<AccessibilityUpdate, String> {
    let screen = match snapshot.screen {
        ghostty_accessibility::Screen::Primary => AccessibilityScreen::Primary,
        ghostty_accessibility::Screen::Alternate => AccessibilityScreen::Alternate,
    };
    let screen_generation = usize::try_from(snapshot.screen_generation)
        .map_err(|_| "terminal accessibility screen generation exceeded usize".to_owned())?;
    let row_id = |id: ghostty_accessibility::RowId| AccessibilityRowId {
        screen,
        screen_generation,
        node_serial: id.node_serial,
        page_row: id.page_row,
    };
    let cell_ref = |reference: ghostty_accessibility::CellRef| AccessibilityCellRef {
        row: row_id(reference.row),
        row_revision: reference.row_revision,
        column: reference.column,
    };
    Ok(AccessibilityUpdate {
        revision: snapshot.revision,
        screen,
        screen_generation,
        complete: snapshot.complete,
        more: snapshot.more,
        topology: snapshot
            .topology
            .map(|topology| topology.into_iter().map(row_id).collect()),
        visible_lines: snapshot.visible_lines,
        cursor: snapshot.cursor.map(cell_ref),
        selection: snapshot
            .selection
            .map(|selection| AccessibilitySelectionRefs {
                start: cell_ref(selection.start),
                end: cell_ref(selection.end),
                rectangle: selection.rectangle,
            }),
        changed_rows: snapshot
            .changed_rows
            .into_iter()
            .map(|row| AccessibilityRowUpdate {
                id: row_id(row.id),
                revision: row.revision,
                soft_wrapped: row.soft_wrapped,
                cells: row
                    .cells
                    .into_iter()
                    .map(|cell| {
                        AccessibilityCell::at_column(cell.text, cell.column, cell.width_cells)
                    })
                    .collect(),
            })
            .collect(),
    })
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

fn shift_overrides_application_mouse(
    modifiers: InputModifiers,
    policy: ShiftSelectionPolicy,
) -> bool {
    modifiers.shift && policy == ShiftSelectionPolicy::OverrideApplicationMouse
}

fn selection_autoscroll_interval_for_position(
    position: SurfacePosition,
    screen_height: u32,
    cell_height: u32,
) -> Option<Duration> {
    let overflow = if position.y < 0.0 {
        -position.y
    } else if position.y >= screen_height as f32 {
        position.y - screen_height as f32 + 1.0
    } else {
        return None;
    };
    let cell_height = cell_height.max(1) as f32;
    let depth = (overflow / cell_height).ceil().clamp(1.0, 6.0) as u32;
    let range = MAX_SELECTION_AUTOSCROLL_INTERVAL - MIN_SELECTION_AUTOSCROLL_INTERVAL;
    let step = range / 5;
    Some(MAX_SELECTION_AUTOSCROLL_INTERVAL - step * (depth - 1))
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
    let theme = &*ACTIVE_THEME;
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
#[path = "emulator/tests.rs"]
mod tests;
