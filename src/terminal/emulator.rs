use std::cell::RefCell;
use std::collections::HashMap;
use std::mem;
use std::path::PathBuf;
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
    APC_TRANSMISSION_LIMIT, GraphicsReservation, GraphicsSnapshot, GraphicsState,
    IMAGE_STORAGE_LIMIT, starts_apc,
};
use crate::terminal::hyperlink::{HyperlinkTarget, has_file_scheme};
use crate::terminal::identity::{self, XtGetTcapObserver};
use crate::terminal::key::{InputModifiers, KeyAction, KeyInput, OptionAsAltPolicy, PhysicalKey};
use crate::terminal::keyboard_protocol::KeyboardProtocolEncoder;
use crate::terminal::metadata::{
    MetadataTracker, TerminalMetadataContext, TerminalMetadataSnapshot,
};
use crate::terminal::selection::{SelectionCopy, SelectionCopyOptions, TrailingSpacePolicy};
#[cfg(test)]
use crate::terminal::session::WheelPhase;
use crate::terminal::session::{
    PointerButton, PointerInput, PointerPhase, ShiftSelectionPolicy, SurfacePosition, WheelInput,
};
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
    pub(crate) hyperlink: Option<crate::terminal::HyperlinkTarget>,
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

    pub(crate) const fn as_u64(self) -> u64 {
        self.0
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
    pub(crate) fn empty() -> Arc<Self> {
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
            metadata: MetadataTracker::new("", "", None, Instant::now()).snapshot(),
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
            metadata: MetadataTracker::new("", "", None, Instant::now()).snapshot(),
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

    fn screen_changed_if(changed: bool) -> Self {
        if changed {
            Self::screen_changed()
        } else {
            Self::none()
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
            TerminalMetadataContext::local(initial_directory, local_hostname),
            fallback_title,
            terminal_name,
            epoch,
        )
    }

    pub(crate) fn new_with_metadata_context(
        geometry: TerminalGeometry,
        metadata_context: TerminalMetadataContext,
        fallback_title: &str,
        terminal_name: &'static str,
        epoch: Instant,
    ) -> Result<Self, Error> {
        let grid = geometry.grid();
        let cell = geometry.backing_cell_size();
        let pty_responses = Rc::new(RefCell::new(Vec::new()));
        let pending_metadata = Rc::new(RefCell::new(Vec::new()));
        let local_file_capabilities = metadata_context.local_file_capabilities();
        let trusted_directory = Rc::new(RefCell::new(
            metadata_context
                .is_local()
                .then(|| PathBuf::from(metadata_context.initial_directory()))
                .filter(|directory| directory.is_absolute()),
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
            let local_hostname = metadata_context.local_hostname().map(ToOwned::to_owned);
            let local_context = metadata_context.is_local();
            move |terminal| {
                if let Ok(directory) = terminal.pwd() {
                    let reported = crate::terminal::metadata::parse_osc7_directory(
                        directory,
                        local_hostname.as_deref(),
                    );
                    *trusted_directory.borrow_mut() = if local_context {
                        reported.map(|metadata| PathBuf::from(metadata.path.as_ref()))
                    } else {
                        None
                    };
                    pending_metadata
                        .borrow_mut()
                        .push(MetadataEvent::Directory(Arc::from(directory)));
                }
            }
        })?;
        terminal.on_hyperlink_resolve({
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
                let Some(target) = HyperlinkTarget::osc8(
                    uri,
                    &directory,
                    local_hostname.as_deref(),
                    local_file_capabilities,
                ) else {
                    return HyperlinkResolution::Suppress;
                };
                let Some(userdata) = target.local_emission_metadata(local_file_capabilities) else {
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
        self.enable_graphics_for_apc(bytes);
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
        self.terminal.vt_write(bytes);
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

    fn enable_graphics_for_apc(&mut self, bytes: &[u8]) {
        if self.graphics_reservation.is_some() || !starts_apc(self.previous_feed_byte, bytes) {
            return;
        }
        let Some(reservation) = GraphicsReservation::try_acquire() else {
            return;
        };
        if self
            .terminal
            .set_kitty_image_storage_limit(IMAGE_STORAGE_LIMIT as u64)
            .is_ok()
        {
            self.graphics_reservation = Some(reservation);
        }
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
                Ok(EmulatorAction::none())
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

    pub(crate) fn snapshot(&mut self) -> Result<Option<Arc<ScreenSnapshot>>, Error> {
        if self.terminal.mode(Mode::SYNC_OUTPUT)? {
            return Ok(None);
        }

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
        let previous_graphics = match active_screen {
            ActiveScreenSnapshot::Primary => self.primary_graphics.published().clone(),
            ActiveScreenSnapshot::Alternate => self.alternate_graphics.published().clone(),
        };
        let graphics = if self.graphics_reservation.is_some() {
            match active_screen {
                ActiveScreenSnapshot::Primary => self.primary_graphics.snapshot(&self.terminal)?,
                ActiveScreenSnapshot::Alternate => {
                    self.alternate_graphics.snapshot(&self.terminal)?
                }
            }
        } else {
            GraphicsSnapshot::default()
        };
        let graphics_content_changed = previous_graphics.generation != graphics.generation;
        let graphics_geometry_changed = previous_graphics.placements != graphics.placements;
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
            HashMap::<String, Option<crate::terminal::HyperlinkTarget>>::new();
        let mut local_hyperlink_targets =
            HashMap::<Vec<u8>, Option<crate::terminal::HyperlinkTarget>>::new();
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
                                        let target = crate::terminal::HyperlinkTarget::url(uri);
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
                                    );
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
                        &rendered_cells
                            .iter()
                            .map(|cell| cell.text.clone())
                            .collect::<Vec<_>>(),
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
    use std::fs;

    use super::*;
    use crate::terminal::TerminalAccessibilityModel;
    use crate::terminal::geometry::{
        BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry,
    };
    use crate::terminal::metadata::{DirectoryProvenance, ProgressMetadata};

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

    fn emulator_with_terminal_name(
        cols: u16,
        rows: u16,
        terminal_name: &'static str,
    ) -> TerminalEmulator {
        TerminalEmulator::new_with_metadata(
            geometry(cols, rows, 10.0, 20.0),
            "",
            "",
            None,
            terminal_name,
            Instant::now(),
        )
        .unwrap()
    }

    #[test]
    fn terminal_find_matches_across_soft_wraps() {
        let mut emulator = emulator(3, 2);
        emulator.feed(b"abcdef");
        emulator.set_find_query(FindQueryGeneration::test(1), "cde".to_owned());

        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert_eq!(snapshot.find.as_ref().unwrap().total_matches, 1);
    }

    #[test]
    fn accessibility_snapshot_preserves_production_soft_wraps() {
        let mut emulator = emulator(3, 2);
        emulator.feed(b"abcdef");

        let snapshot = emulator.snapshot().unwrap().unwrap();
        let accessibility = TerminalAccessibilityModel::from_screen(&snapshot);

        assert_eq!(snapshot.row_soft_wrapped.as_ref(), &[true, false]);
        assert_eq!(accessibility.text(), "abcdef");
        assert_eq!(accessibility.range_for_line(0), Some(0..3));
        assert_eq!(accessibility.range_for_line(1), Some(3..6));
    }

    #[test]
    fn terminal_find_rejects_matches_across_hard_lines() {
        let mut emulator = emulator(4, 2);
        emulator.feed(b"abc\r\ndef");
        emulator.set_find_query(FindQueryGeneration::test(1), "cde".to_owned());

        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert_eq!(snapshot.find.as_ref().unwrap().total_matches, 0);
    }

    #[test]
    fn terminal_find_includes_primary_scrollback() {
        let mut emulator = emulator(8, 2);
        emulator.feed(b"needle\r\nsecond\r\nthird");
        emulator.set_find_query(FindQueryGeneration::test(1), "needle".to_owned());

        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert_eq!(snapshot.find.as_ref().unwrap().total_matches, 1);
    }

    #[test]
    fn terminal_find_highlights_the_complete_wide_grapheme() {
        let mut emulator = emulator(8, 2);
        emulator.feed("🙂".as_bytes());
        emulator.set_find_query(FindQueryGeneration::test(1), "🙂".to_owned());

        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert_eq!(
            snapshot.find.as_ref().unwrap().visible_spans.as_ref(),
            [crate::terminal::FindHighlightSpan {
                row: 0,
                start_column: 0,
                end_column: 1,
                current: false,
            }]
        );
    }

    #[test]
    fn terminal_find_query_only_snapshot_reuses_rows() {
        let mut emulator = emulator(8, 2);
        emulator.feed(b"needle");
        let first = emulator.snapshot().unwrap().unwrap();
        emulator.set_find_query(FindQueryGeneration::test(1), "needle".to_owned());

        let found = emulator.snapshot().unwrap().unwrap();

        assert!(found.damage.search);
        assert_eq!(found.damage.content, ContentDamageSnapshot::Clean);
        assert!(
            first
                .rows
                .iter()
                .zip(found.rows.iter())
                .all(|(first, second)| Arc::ptr_eq(first, second))
        );
    }

    #[test]
    fn terminal_find_navigation_rejects_stale_query_generation() {
        let mut emulator = emulator(8, 2);
        emulator.feed(b"needle");
        emulator.set_find_query(FindQueryGeneration::test(2), "needle".to_owned());
        let _ = emulator.snapshot().unwrap().unwrap();

        let action = emulator
            .navigate_find(FindQueryGeneration::test(1), FindDirection::Next)
            .unwrap();

        assert!(!action.screen_changed);
    }

    #[test]
    fn terminal_find_navigation_wraps_and_marks_the_current_result() {
        let mut emulator = emulator(12, 2);
        emulator.feed(b"one one");
        let generation = FindQueryGeneration::test(1);
        emulator.set_find_query(generation, "one".to_owned());
        let _ = emulator.snapshot().unwrap().unwrap();

        let _ = emulator
            .navigate_find(generation, FindDirection::Next)
            .unwrap();
        let first = emulator.snapshot().unwrap().unwrap();
        let _ = emulator
            .navigate_find(generation, FindDirection::Next)
            .unwrap();
        let second = emulator.snapshot().unwrap().unwrap();
        let _ = emulator
            .navigate_find(generation, FindDirection::Next)
            .unwrap();
        let wrapped = emulator.snapshot().unwrap().unwrap();

        assert_eq!(first.find.as_ref().unwrap().current_match, Some(1));
        assert_eq!(second.find.as_ref().unwrap().current_match, Some(2));
        assert_eq!(wrapped.find.as_ref().unwrap().current_match, Some(1));
    }

    #[test]
    fn terminal_find_initial_navigation_starts_in_the_current_viewport() {
        let mut emulator = emulator(16, 2);
        emulator.feed(b"needle old\r\nmiddle\r\nneedle new");
        let generation = FindQueryGeneration::test(1);
        emulator.set_find_query(generation, "needle".to_owned());
        let before = emulator.snapshot().unwrap().unwrap();
        assert!(before.scrollbar.offset_rows > 0);

        let _ = emulator
            .navigate_find(generation, FindDirection::Next)
            .unwrap();
        let selected = emulator.snapshot().unwrap().unwrap();

        assert_eq!(selected.find.as_ref().unwrap().current_match, Some(2));
        assert_eq!(selected.scrollbar.offset_rows, before.scrollbar.offset_rows);
    }

    #[test]
    fn terminal_find_navigation_scrolls_an_offscreen_match_completely_into_view() {
        let mut emulator = emulator(16, 2);
        emulator.feed(b"needle\r\nmiddle\r\nbottom");
        let generation = FindQueryGeneration::test(1);
        emulator.set_find_query(generation, "needle".to_owned());
        let _ = emulator.snapshot().unwrap().unwrap();

        let _ = emulator
            .navigate_find(generation, FindDirection::Next)
            .unwrap();
        let selected = emulator.snapshot().unwrap().unwrap();

        assert_eq!(selected.scrollbar.offset_rows, 0);
        assert_eq!(selected.find.as_ref().unwrap().current_match, Some(1));
    }

    #[test]
    fn terminal_find_preserves_current_result_across_output_and_reflow() {
        let mut emulator = emulator(8, 2);
        let generation = FindQueryGeneration::test(1);
        emulator.feed(b"needle");
        emulator.set_find_query(generation, "needle".to_owned());
        let _ = emulator.snapshot().unwrap().unwrap();
        let _ = emulator
            .navigate_find(generation, FindDirection::Next)
            .unwrap();
        let _ = emulator.snapshot().unwrap().unwrap();

        emulator.feed(b"\r\nmore");
        let after_output = emulator.snapshot().unwrap().unwrap();
        emulator.resize(geometry(4, 3, 10.0, 20.0)).unwrap();
        let after_reflow = emulator.snapshot().unwrap().unwrap();

        assert_eq!(after_output.find.as_ref().unwrap().current_match, Some(1));
        assert_eq!(after_reflow.find.as_ref().unwrap().current_match, Some(1));
    }

    #[test]
    fn terminal_find_drops_current_result_when_scrollback_prunes_it() {
        let mut emulator = emulator(8, 2);
        let generation = FindQueryGeneration::test(1);
        emulator.feed(b"needle");
        emulator.set_find_query(generation, "needle".to_owned());
        let _ = emulator.snapshot().unwrap().unwrap();
        let _ = emulator
            .navigate_find(generation, FindDirection::Next)
            .unwrap();
        let _ = emulator.snapshot().unwrap().unwrap();

        emulator.feed(format!("\r\n{}", "x\r\n".repeat(MAX_SCROLLBACK_ROWS * 2)).as_bytes());
        let pruned = emulator.snapshot().unwrap().unwrap();

        assert_eq!(pruned.find.as_ref().unwrap().total_matches, 0);
        assert_eq!(pruned.find.as_ref().unwrap().current_match, None);
    }

    #[test]
    fn terminal_find_active_screen_scope_restores_primary_results() {
        let mut emulator = emulator(12, 2);
        let generation = FindQueryGeneration::test(1);
        emulator.feed(b"primary");
        emulator.set_find_query(generation, "primary".to_owned());
        let primary = emulator.snapshot().unwrap().unwrap();
        emulator.feed(b"\x1b[?1049halternate");
        let alternate = emulator.snapshot().unwrap().unwrap();
        emulator.feed(b"\x1b[?1049l");
        let restored = emulator.snapshot().unwrap().unwrap();

        assert_eq!(primary.find.as_ref().unwrap().total_matches, 1);
        assert_eq!(alternate.find.as_ref().unwrap().total_matches, 0);
        assert_eq!(restored.find.as_ref().unwrap().total_matches, 1);
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

    #[test]
    fn conventional_mouse_protocol_mode_matrix_is_byte_exact() {
        let mut x10 = emulator(10, 3);
        x10.feed(b"\x1b[?9h");
        assert_eq!(
            x10.pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                11.0,
                21.0,
                false,
            ))
            .unwrap()
            .bytes,
            b"\x1b[M \"\""
        );

        let mut normal = emulator(10, 3);
        normal.feed(b"\x1b[?1000h");
        _ = normal
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                11.0,
                21.0,
                false,
            ))
            .unwrap();
        assert_eq!(
            normal
                .pointer(pointer(
                    PointerPhase::Release,
                    Some(PointerButton::Left),
                    11.0,
                    21.0,
                    false,
                ))
                .unwrap()
                .bytes,
            b"\x1b[M#\"\""
        );

        let mut button = emulator(10, 3);
        button.feed(b"\x1b[?1002h");
        _ = button
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                11.0,
                21.0,
                false,
            ))
            .unwrap();
        assert_eq!(
            button
                .pointer(pointer(
                    PointerPhase::Motion,
                    Some(PointerButton::Left),
                    21.0,
                    21.0,
                    false,
                ))
                .unwrap()
                .bytes,
            b"\x1b[M@#\""
        );

        let mut any_motion = emulator(10, 3);
        any_motion.feed(b"\x1b[?1003h");
        assert_eq!(
            any_motion
                .pointer(pointer(PointerPhase::Motion, None, 11.0, 21.0, false))
                .unwrap()
                .bytes,
            b"\x1b[MC\"\""
        );

        let mut utf8 = emulator(300, 3);
        utf8.feed(b"\x1b[?1000h\x1b[?1005h");
        assert_eq!(
            utf8.pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                2_591.0,
                21.0,
                false,
            ))
            .unwrap()
            .bytes,
            b"\x1b[M \xc4\xa4\""
        );

        let mut sgr = emulator(10, 3);
        sgr.feed(b"\x1b[?1000h\x1b[?1006h");
        assert_eq!(
            sgr.pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                11.0,
                21.0,
                false,
            ))
            .unwrap()
            .bytes,
            b"\x1b[<0;2;2M"
        );

        let mut urxvt = emulator(10, 3);
        urxvt.feed(b"\x1b[?1000h\x1b[?1015h");
        assert_eq!(
            urxvt
                .pointer(pointer(
                    PointerPhase::Press,
                    Some(PointerButton::Left),
                    11.0,
                    21.0,
                    false,
                ))
                .unwrap()
                .bytes,
            b"\x1b[32;2;2M"
        );

        let mut sgr_pixels = emulator(10, 3);
        sgr_pixels.feed(b"\x1b[?1000h\x1b[?1016h");
        assert_eq!(
            sgr_pixels
                .pointer(pointer(
                    PointerPhase::Press,
                    Some(PointerButton::Left),
                    11.0,
                    21.0,
                    false,
                ))
                .unwrap()
                .bytes,
            b"\x1b[<0;11;21M"
        );
    }

    #[test]
    fn offscreen_drag_preserves_button_and_modifiers_at_clamped_boundaries() {
        let mut emulator = emulator(10, 2);
        emulator.feed(b"\x1b[?1002h\x1b[?1006h");
        _ = emulator
            .pointer(pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                1.0,
                1.0,
                false,
            ))
            .unwrap();

        let mut drag = pointer(
            PointerPhase::Motion,
            Some(PointerButton::Left),
            -10.0,
            100.0,
            false,
        );
        drag.modifiers = InputModifiers {
            shift: true,
            alt: true,
            control: true,
            ..InputModifiers::default()
        };
        let dragged = emulator.pointer(drag).unwrap();

        let mut release = pointer(
            PointerPhase::Release,
            Some(PointerButton::Left),
            200.0,
            -10.0,
            false,
        );
        release.modifiers = drag.modifiers;
        let released = emulator.pointer(release).unwrap();

        assert_eq!(dragged.bytes, b"\x1b[<60;1;2M");
        assert_eq!(released.bytes, b"\x1b[<28;10;1m");
    }

    fn pointer(
        phase: PointerPhase,
        button: Option<PointerButton>,
        x: f32,
        y: f32,
        shift: bool,
    ) -> PointerInput {
        PointerInput {
            generation: PresentationGeneration::default(),
            phase,
            button,
            position: SurfacePosition { x, y },
            modifiers: InputModifiers {
                shift,
                ..InputModifiers::default()
            },
            shift_selection: ShiftSelectionPolicy::default(),
        }
    }

    fn wheel(steps: i32, shift: bool) -> WheelInput {
        WheelInput {
            generation: PresentationGeneration::default(),
            horizontal_steps: 0,
            vertical_steps: steps,
            phase: WheelPhase::GestureChanged,
            position: SurfacePosition { x: 1.0, y: 1.0 },
            modifiers: InputModifiers {
                shift,
                ..InputModifiers::default()
            },
            shift_selection: ShiftSelectionPolicy::default(),
        }
    }

    fn current_pointer(emulator: &TerminalEmulator, mut input: PointerInput) -> PointerInput {
        input.generation = emulator.presentation_generation;
        input
    }

    fn current_wheel(emulator: &TerminalEmulator, mut input: WheelInput) -> WheelInput {
        input.generation = emulator.presentation_generation;
        input
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
            option_as_alt: OptionAsAltPolicy::default(),
        }
    }

    fn text_key(
        physical_key: PhysicalKey,
        text: &str,
        unshifted_codepoint: char,
        action: KeyAction,
        modifiers: InputModifiers,
    ) -> KeyInput {
        KeyInput {
            action,
            physical_key,
            native_key_code: None,
            logical_key: text.to_owned(),
            text: Some(text.to_owned()),
            unshifted_codepoint: Some(unshifted_codepoint),
            modifiers,
            consumed_modifiers: InputModifiers::default(),
            option_as_alt: OptionAsAltPolicy::default(),
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

    fn select_all(emulator: &mut TerminalEmulator) {
        let selection = emulator.terminal.select_all().unwrap().unwrap();
        emulator.terminal.set_selection(Some(&selection)).unwrap();
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
            snapshot.rows[1][0].foreground_source,
            TerminalColor::Palette(1)
        );
    }

    #[test]
    fn snapshots_preserve_foreground_color_sources() {
        let mut emulator = emulator(8, 1);
        emulator.feed(b"d\x1b[31ma\x1b[38;5;200mi\x1b[38;2;1;2;3mr");

        let snapshot = emulator.snapshot().unwrap().unwrap();
        let sources = snapshot.rows[0][..4]
            .iter()
            .map(|cell| cell.foreground_source)
            .collect::<Vec<_>>();

        assert_eq!(
            sources,
            vec![
                TerminalColor::Default,
                TerminalColor::Palette(1),
                TerminalColor::Palette(200),
                TerminalColor::Rgb(Color::from_rgb_components(1, 2, 3)),
            ]
        );
    }

    #[test]
    fn snapshots_preserve_background_color_sources() {
        let mut emulator = emulator(8, 1);
        emulator.feed(b"d\x1b[41ma\x1b[48;5;200mi\x1b[48;2;4;5;6mr");

        let snapshot = emulator.snapshot().unwrap().unwrap();
        let sources = snapshot.rows[0][..4]
            .iter()
            .map(|cell| cell.background_source)
            .collect::<Vec<_>>();

        assert_eq!(
            sources,
            vec![
                TerminalColor::Default,
                TerminalColor::Palette(1),
                TerminalColor::Palette(200),
                TerminalColor::Rgb(Color::from_rgb_components(4, 5, 6)),
            ]
        );
    }

    #[test]
    fn snapshots_preserve_inverse_and_terminal_reverse_semantics() {
        let mut emulator = emulator(4, 1);
        emulator.feed(b"\x1b[7mx\x1b[27m\x1b[?5h");

        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert!(snapshot.rows[0][0].inverse);
        assert!(snapshot.colors.reversed);
        assert_eq!(snapshot.background, snapshot.colors.foreground);
        assert_eq!(
            snapshot.colors.palette[1],
            ACTIVE_THEME.terminal_normal()[1]
        );
        assert_eq!(
            snapshot.colors.palette[9],
            ACTIVE_THEME.terminal_bright()[1]
        );
    }

    #[test]
    fn snapshots_preserve_text_presentation_attributes_and_invisible_content() {
        let mut emulator = emulator(8, 1);
        emulator.feed(b"\x1b[1;2;3;5;7;8msecret\x1b[0m");

        let snapshot = emulator.snapshot().unwrap().unwrap();
        let cell = &snapshot.rows[0][0];

        assert_eq!(cell.text, "s");
        assert!(cell.bold);
        assert!(cell.faint);
        assert!(cell.italic);
        assert!(cell.blinking);
        assert!(cell.inverse);
        assert!(cell.invisible);
    }

    #[test]
    fn snapshot_reports_text_blink_demand_only_for_visible_content() {
        let mut visible = emulator(2, 1);
        visible.feed(b"\x1b[5mA");
        let visible = visible.snapshot().unwrap().unwrap();

        let mut invisible = emulator(2, 1);
        invisible.feed(b"\x1b[5;8mA");
        let invisible = invisible.snapshot().unwrap().unwrap();

        assert!(visible.text_blinking);
        assert!(!invisible.text_blinking);
    }

    #[test]
    fn snapshots_preserve_text_decorations_and_independent_underline_color() {
        let mut emulator = emulator(8, 1);
        emulator.feed(b"\x1b[58;5;200;4:1mS\x1b[4:2mD\x1b[4:3mC\x1b[4:4mO\x1b[4:5mH\x1b[9;53mX");

        let snapshot = emulator.snapshot().unwrap().unwrap();
        let cells = &snapshot.rows[0];

        assert_eq!(cells[0].underline, TerminalUnderlineSnapshot::Single);
        assert_eq!(cells[1].underline, TerminalUnderlineSnapshot::Double);
        assert_eq!(cells[2].underline, TerminalUnderlineSnapshot::Curly);
        assert_eq!(cells[3].underline, TerminalUnderlineSnapshot::Dotted);
        assert_eq!(cells[4].underline, TerminalUnderlineSnapshot::Dashed);
        assert_eq!(cells[0].underline_source, TerminalColor::Palette(200));
        assert!(cells[5].strikethrough);
        assert!(cells[5].overline);
    }

    #[test]
    fn erased_cells_preserve_explicit_background_sources() {
        let mut emulator = emulator(4, 1);
        emulator.feed(b"\x1b[41m\x1b[2K");

        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert!(
            snapshot.rows[0]
                .iter()
                .all(|cell| cell.background_source == TerminalColor::Palette(1))
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
                metadata: true,
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
    fn metadata_only_output_publishes_owned_provenance_without_rebuilding_rows() {
        let geometry = geometry(12, 3, 10.0, 20.0);
        let epoch = Instant::now();
        let mut emulator = TerminalEmulator::new_with_metadata(
            geometry,
            "/Users/me",
            "zsh",
            Some("mac.local"),
            identity::TERM_FALLBACK,
            epoch,
        )
        .unwrap();
        let first = emulator.snapshot().unwrap().unwrap();

        emulator.feed_at(
            b"\x1b]7;file://mac.local/Users/me/Project\x07\x1b]9;4;3\x07",
            epoch + Duration::from_secs(1),
        );
        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert_eq!(snapshot.title.as_ref(), "Project");
        assert_eq!(
            snapshot.metadata.directory.path.as_ref(),
            "/Users/me/Project"
        );
        assert_eq!(
            snapshot.metadata.directory.provenance,
            DirectoryProvenance::Osc7
        );
        assert_eq!(snapshot.metadata.progress, ProgressMetadata::Indeterminate);
        assert!(snapshot.damage.metadata);
        assert_eq!(snapshot.damage.content, ContentDamageSnapshot::Clean);
        assert!(
            first
                .rows
                .iter()
                .zip(snapshot.rows.iter())
                .all(|(first, second)| Arc::ptr_eq(first, second))
        );
    }

    #[test]
    fn semantic_prompt_zones_and_command_state_are_owned_by_the_snapshot() {
        let geometry = geometry(16, 2, 10.0, 20.0);
        let epoch = Instant::now();
        let mut emulator = TerminalEmulator::new_with_metadata(
            geometry,
            "/tmp",
            "zsh",
            None,
            identity::TERM_FALLBACK,
            epoch,
        )
        .unwrap();
        emulator.feed_at(
            b"\x1b]133;A\x07$ \x1b]133;B\x07echo\x1b]133;C;cmdline=echo\x07out",
            epoch + Duration::from_secs(1),
        );

        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert!(
            snapshot.rows[0][0..2]
                .iter()
                .all(|cell| cell.semantic_content == CellSemanticSnapshot::Prompt)
        );
        assert!(
            snapshot.rows[0][2..6]
                .iter()
                .all(|cell| cell.semantic_content == CellSemanticSnapshot::Input)
        );
        assert!(
            snapshot.rows[0][6..9]
                .iter()
                .all(|cell| cell.semantic_content == CellSemanticSnapshot::Output)
        );
        assert_eq!(
            snapshot.metadata.prompt_zone,
            crate::terminal::metadata::PromptZone::CommandOutput
        );
        assert_eq!(
            snapshot
                .metadata
                .command
                .as_ref()
                .map(|command| command.line.as_ref()),
            Some("echo")
        );
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
    fn grid_resize_reflows_logical_content_and_advances_the_presentation() {
        let mut emulator = emulator(8, 3);
        emulator.feed(b"abcdefghijkl");
        let before = emulator.snapshot().unwrap().unwrap();

        emulator.resize(geometry(5, 3, 8.0, 18.0)).unwrap();
        let after = emulator.snapshot().unwrap().unwrap();

        assert!(after.generation > before.generation);
        assert!(row_text(&after, 0).starts_with("abcde"));
        assert!(row_text(&after, 1).starts_with("fghij"));
        assert!(row_text(&after, 2).starts_with("kl"));
        assert!(after.damage.resize);
        assert_eq!(after.damage.content, ContentDamageSnapshot::Full);
    }

    #[test]
    fn grid_resize_preserves_selection_anchors_across_reflow() {
        let mut emulator = emulator(12, 3);
        emulator.feed(b"hello world");
        let _ = emulator.snapshot().unwrap().unwrap();
        let press = current_pointer(
            &emulator,
            pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                2.0,
                10.0,
                false,
            ),
        );
        emulator.pointer(press).unwrap();
        let drag = current_pointer(
            &emulator,
            pointer(PointerPhase::Motion, None, 48.0, 10.0, false),
        );
        emulator.pointer(drag).unwrap();
        let release = current_pointer(
            &emulator,
            pointer(
                PointerPhase::Release,
                Some(PointerButton::Left),
                48.0,
                10.0,
                false,
            ),
        );
        emulator.pointer(release).unwrap();
        assert_eq!(emulator.selection_text().unwrap(), Some("hello".to_owned()));

        emulator.resize(geometry(6, 3, 8.0, 18.0)).unwrap();
        let reflowed = emulator.snapshot().unwrap().unwrap();

        assert_eq!(emulator.selection_text().unwrap(), Some("hello".to_owned()));
        assert!(
            reflowed
                .rows
                .iter()
                .flat_map(|row| row.iter())
                .take(5)
                .all(|cell| cell.selected)
        );
    }

    #[test]
    fn selection_stays_anchored_when_output_moves_scrollback_pages() {
        let mut emulator = emulator(8, 2);
        emulator.feed(b"one\r\ntwo\r\nthree\r\nfour");
        _ = emulator.snapshot().unwrap().unwrap();
        emulator.scroll_to(0);
        let top = emulator.snapshot().unwrap().unwrap();
        let input = |phase, x| PointerInput {
            generation: top.generation,
            phase,
            button: (phase != PointerPhase::Motion).then_some(PointerButton::Left),
            position: SurfacePosition { x, y: 1.0 },
            modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::default(),
        };
        _ = emulator.pointer(input(PointerPhase::Press, 1.0)).unwrap();
        _ = emulator.pointer(input(PointerPhase::Motion, 28.0)).unwrap();
        _ = emulator
            .pointer(input(PointerPhase::Release, 28.0))
            .unwrap();
        assert_eq!(emulator.selection_text().unwrap().as_deref(), Some("one"));

        emulator.feed(b"\r\nfive");
        let moved = emulator.snapshot().unwrap().unwrap();

        assert!(row_text(&moved, 0).starts_with("one"));
        assert!(moved.rows[0][..3].iter().all(|cell| cell.selected));
        assert_eq!(emulator.selection_text().unwrap().as_deref(), Some("one"));
    }

    #[test]
    fn pixel_only_resize_updates_backing_geometry_without_a_grid_presentation() {
        let mut emulator = emulator(10, 2);
        let before = emulator.snapshot().unwrap().unwrap();

        emulator.resize(geometry(10, 2, 9.0, 20.0)).unwrap();

        assert!(emulator.snapshot().unwrap().is_none());
        assert_eq!(emulator.presentation_generation, before.generation);
        assert_eq!(
            emulator.mouse_encoder_size(),
            MouseEncoderSize {
                screen_width: 90,
                screen_height: 40,
                cell_width: 9,
                cell_height: 20,
                padding_top: 0,
                padding_bottom: 0,
                padding_right: 0,
                padding_left: 0,
            }
        );
    }

    #[test]
    fn resize_releases_synchronized_output_without_spurious_grid_damage() {
        let mut emulator = emulator(10, 2);
        let _ = emulator.snapshot().unwrap().unwrap();
        emulator.feed(b"\x1b[?2026hpending");
        assert!(emulator.snapshot().unwrap().is_none());

        emulator.resize(geometry(10, 2, 9.0, 20.0)).unwrap();
        let released = emulator.snapshot().unwrap().unwrap();

        assert!(row_text(&released, 0).starts_with("pending"));
        assert!(!released.damage.resize);
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
    fn alternate_screen_exit_restores_the_primary_viewport_and_cached_rows() {
        let mut emulator = emulator(10, 2);
        emulator.feed(b"one\r\ntwo\r\nthree");
        let _ = emulator.snapshot().unwrap().unwrap();
        let input = current_wheel(&emulator, wheel(1, false));
        let _ = emulator.wheel(input).unwrap();
        let primary = emulator.snapshot().unwrap().unwrap();
        assert!(row_text(&primary, 0).starts_with("one"));

        emulator.feed(b"\x1b[?1049halternate");
        let alternate = emulator.snapshot().unwrap().unwrap();
        assert_eq!(alternate.active_screen, ActiveScreenSnapshot::Alternate);
        assert_eq!(
            alternate.scrollbar.total_rows,
            alternate.scrollbar.visible_rows
        );

        emulator.feed(b"\x1b[?1049l");
        let restored = emulator.snapshot().unwrap().unwrap();
        assert_eq!(restored.active_screen, ActiveScreenSnapshot::Primary);
        assert_eq!(restored.viewport, primary.viewport);
        assert!(row_text(&restored, 0).starts_with("one"));
        assert!(
            primary
                .rows
                .iter()
                .zip(restored.rows.iter())
                .all(|(before, after)| Arc::ptr_eq(before, after))
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
        full_snapshot.generation = incremental_snapshot.generation;

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
    fn runtime_identity_replies_are_spaceterm_owned_and_capability_bounded() {
        let mut emulator = emulator_with_terminal_name(10, 2, identity::TERM_NAME);

        emulator.feed(b"\x1b[>q\x1b[c\x1b[>c\x1bP+q544E;436F\x1b\\");
        let replies = emulator.take_pty_responses();

        assert!(
            replies
                .windows(identity::XTVERSION.len())
                .any(|window| window == identity::XTVERSION.as_bytes())
        );
        assert!(
            replies
                .windows(b"\x1b[?62;22;52c".len())
                .any(|window| window == b"\x1b[?62;22;52c")
        );
        assert!(
            replies
                .windows(b"\x1b[>1;0;0c".len())
                .any(|window| window == b"\x1b[>1;0;0c")
        );
        assert!(replies.windows(7).any(|window| window == b"1+r544E"));
        assert!(
            replies
                .windows(b"787465726D2D73706163657465726D".len())
                .any(|window| window == b"787465726D2D73706163657465726D")
        );
        assert!(replies.windows(7).any(|window| window == b"1+r436F"));
        assert!(
            !String::from_utf8_lossy(&replies)
                .to_ascii_lowercase()
                .contains("ghostty")
        );
    }

    #[test]
    fn bell_and_command_completion_publish_typed_attention_once() {
        let epoch = Instant::now();
        let mut emulator = emulator(10, 2);

        emulator.feed_at(b"\x07\x1b]133;C;cmdline=true\x07", epoch);
        emulator.feed_at(b"\x1b]133;D;0\x07", epoch + Duration::from_secs(2));
        emulator.feed_at(b"\x1b]133;D;0\x07", epoch + Duration::from_secs(3));

        assert_eq!(
            emulator.take_attention_events(),
            vec![
                AttentionEvent::Bell,
                AttentionEvent::CommandFinished {
                    exit_status: Some(0),
                    duration: Duration::from_secs(2),
                },
            ]
        );
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
            option_as_alt: OptionAsAltPolicy::default(),
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
            option_as_alt: OptionAsAltPolicy::default(),
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
            option_as_alt: OptionAsAltPolicy::default(),
        };
        assert_eq!(emulator.key(alt_x).unwrap().bytes, b"\x1bx");
    }

    #[test]
    fn input_method_commits_emit_exact_utf8_independent_of_keyboard_protocol() {
        let mut emulator = emulator(10, 2);

        assert_eq!(
            emulator
                .key(KeyInput::input_method_commit("日本語"))
                .unwrap()
                .bytes,
            "日本語".as_bytes()
        );

        emulator.feed(b"\x1b[>11u");
        assert_eq!(
            emulator
                .key(KeyInput::input_method_commit("👩\u{200d}💻"))
                .unwrap()
                .bytes,
            "👩\u{200d}💻".as_bytes()
        );
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
            (
                PhysicalKey::ArrowRight,
                InputModifiers::default(),
                b"\x1b[C",
            ),
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
    fn application_keypad_mode_changes_the_next_numpad_key_immediately() {
        let mut emulator = emulator(10, 2);
        let numpad_one = || {
            text_key(
                PhysicalKey::Numpad1,
                "1",
                '1',
                KeyAction::Press,
                InputModifiers::default(),
            )
        };

        assert_eq!(emulator.key(numpad_one()).unwrap().bytes, b"1");
        emulator.feed(b"\x1b[?1035l");
        emulator.feed(b"\x1b[?66h");
        assert!(emulator.terminal.mode(Mode::KEYPAD_KEYS).unwrap());
        assert_eq!(emulator.key(numpad_one()).unwrap().bytes, b"\x1bOq");
        emulator.feed(b"\x1b[?66l");
        assert_eq!(emulator.key(numpad_one()).unwrap().bytes, b"1");
    }

    #[test]
    fn modify_other_keys_mode_changes_the_next_modified_text_key_immediately() {
        let mut emulator = emulator(10, 2);
        let alt_eight = || {
            text_key(
                PhysicalKey::Digit8,
                "8",
                '8',
                KeyAction::Press,
                InputModifiers {
                    alt: true,
                    ..InputModifiers::default()
                },
            )
        };

        assert_eq!(emulator.key(alt_eight()).unwrap().bytes, b"\x1b8");
        emulator.feed(b"\x1b[>4;2m");
        assert_eq!(emulator.key(alt_eight()).unwrap().bytes, b"\x1b[27;3;56~");
        emulator.feed(b"\x1b[>4;0m");
        assert_eq!(emulator.key(alt_eight()).unwrap().bytes, b"\x1b8");
    }

    #[test]
    fn fixterms_disambiguates_control_keys_that_overlap_legacy_bytes() {
        let mut emulator = emulator(10, 2);
        let cases = [
            (PhysicalKey::I, "i", 'i', false, b"\x1b[105;5u".as_slice()),
            (PhysicalKey::M, "m", 'm', false, b"\x1b[109;5u".as_slice()),
            (
                PhysicalKey::BracketLeft,
                "[",
                '[',
                false,
                b"\x1b[91;5u".as_slice(),
            ),
            (PhysicalKey::M, "M", 'm', true, b"\x1b[109;6u".as_slice()),
        ];

        for (physical_key, text, unshifted, shift, expected) in cases {
            let input = text_key(
                physical_key,
                text,
                unshifted,
                KeyAction::Press,
                InputModifiers {
                    shift,
                    control: true,
                    ..InputModifiers::default()
                },
            );
            assert_eq!(emulator.key(input).unwrap().bytes, expected);
        }
    }

    #[test]
    fn kitty_report_events_encodes_repeats_and_releases_but_legacy_does_not() {
        let mut emulator = emulator(10, 2);
        let action = |action| text_key(PhysicalKey::A, "a", 'a', action, InputModifiers::default());

        assert!(
            emulator
                .key(action(KeyAction::Release))
                .unwrap()
                .bytes
                .is_empty()
        );
        emulator.feed(b"\x1b[>11u");
        assert_eq!(
            emulator.key(action(KeyAction::Repeat)).unwrap().bytes,
            b"\x1b[97;1:2u"
        );
        assert_eq!(
            emulator.key(action(KeyAction::Release)).unwrap().bytes,
            b"\x1b[97;1:3u"
        );
        emulator.feed(b"\x1b[<u");
        assert!(
            emulator
                .key(action(KeyAction::Release))
                .unwrap()
                .bytes
                .is_empty()
        );
    }

    #[test]
    fn dec_backarrow_mode_changes_backspace_policy_immediately() {
        let mut emulator = emulator(10, 2);
        let backspace = || key(PhysicalKey::Backspace, InputModifiers::default());

        assert_eq!(emulator.key(backspace()).unwrap().bytes, b"\x7f");
        emulator.feed(b"\x1b[?67h");
        assert_eq!(emulator.key(backspace()).unwrap().bytes, b"\x08");
        emulator.feed(b"\x1b[?67l");
        assert_eq!(emulator.key(backspace()).unwrap().bytes, b"\x7f");
    }

    #[test]
    fn conventional_application_shortcuts_keep_legacy_compatibility_bytes() {
        let mut emulator = emulator(10, 2);
        let cases = [
            (PhysicalKey::C, "c", b"\x03".as_slice(), "shell interrupt"),
            (PhysicalKey::B, "b", b"\x02".as_slice(), "tmux prefix"),
            (PhysicalKey::R, "r", b"\x12".as_slice(), "fzf history"),
        ];

        for (physical_key, text, expected, fixture) in cases {
            let input = text_key(
                physical_key,
                text,
                text.chars().next().unwrap(),
                KeyAction::Press,
                InputModifiers {
                    control: true,
                    ..InputModifiers::default()
                },
            );
            assert_eq!(emulator.key(input).unwrap().bytes, expected, "{fixture}");
        }

        emulator.feed(b"\x1b[?1h");
        assert_eq!(
            emulator
                .key(key(PhysicalKey::ArrowUp, InputModifiers::default()))
                .unwrap()
                .bytes,
            b"\x1bOA",
            "Vim/Neovim application cursor"
        );
    }

    #[test]
    fn function_key_byte_tables_cover_legacy_and_extended_kitty_ranges() {
        let mut emulator = emulator(10, 2);
        let legacy_cases: &[(PhysicalKey, &[u8])] = &[
            (PhysicalKey::F1, b"\x1bOP"),
            (PhysicalKey::F2, b"\x1bOQ"),
            (PhysicalKey::F3, b"\x1bOR"),
            (PhysicalKey::F4, b"\x1bOS"),
            (PhysicalKey::F5, b"\x1b[15~"),
            (PhysicalKey::F6, b"\x1b[17~"),
            (PhysicalKey::F7, b"\x1b[18~"),
            (PhysicalKey::F8, b"\x1b[19~"),
            (PhysicalKey::F9, b"\x1b[20~"),
            (PhysicalKey::F10, b"\x1b[21~"),
            (PhysicalKey::F11, b"\x1b[23~"),
            (PhysicalKey::F12, b"\x1b[24~"),
        ];
        for (physical_key, expected) in legacy_cases {
            assert_eq!(
                emulator
                    .key(key(*physical_key, InputModifiers::default()))
                    .unwrap()
                    .bytes,
                *expected,
                "{physical_key:?}"
            );
        }

        let extended = [
            PhysicalKey::F13,
            PhysicalKey::F14,
            PhysicalKey::F15,
            PhysicalKey::F16,
            PhysicalKey::F17,
            PhysicalKey::F18,
            PhysicalKey::F19,
            PhysicalKey::F20,
            PhysicalKey::F21,
            PhysicalKey::F22,
            PhysicalKey::F23,
            PhysicalKey::F24,
            PhysicalKey::F25,
        ];
        for physical_key in extended {
            assert!(
                emulator
                    .key(key(physical_key, InputModifiers::default()))
                    .unwrap()
                    .bytes
                    .is_empty(),
                "legacy must not invent bytes for {physical_key:?}"
            );
        }

        emulator.feed(b"\x1b[>9u");
        for (offset, physical_key) in extended.into_iter().enumerate() {
            let expected = format!("\x1b[{}u", 57_376 + offset).into_bytes();
            assert_eq!(
                emulator
                    .key(key(physical_key, InputModifiers::default()))
                    .unwrap()
                    .bytes,
                expected,
                "{physical_key:?}"
            );
        }
    }

    #[test]
    fn option_as_alt_policy_matrix_respects_modifier_side() {
        let mut emulator = emulator(10, 2);
        let cases: &[(OptionAsAltPolicy, bool, &[u8])] = &[
            (OptionAsAltPolicy::None, false, b"["),
            (OptionAsAltPolicy::Both, false, b"\x1b["),
            (OptionAsAltPolicy::Left, false, b"\x1b["),
            (OptionAsAltPolicy::Right, false, b"["),
            (OptionAsAltPolicy::None, true, b"["),
            (OptionAsAltPolicy::Both, true, b"\x1b["),
            (OptionAsAltPolicy::Left, true, b"["),
            (OptionAsAltPolicy::Right, true, b"\x1b["),
        ];

        for (policy, alt_right, expected) in cases {
            let mut input = text_key(
                PhysicalKey::Digit8,
                "[",
                '8',
                KeyAction::Press,
                InputModifiers {
                    alt: true,
                    alt_right: *alt_right,
                    ..InputModifiers::default()
                },
            );
            let policy_applies = match policy {
                OptionAsAltPolicy::None => false,
                OptionAsAltPolicy::Both => true,
                OptionAsAltPolicy::Left => !alt_right,
                OptionAsAltPolicy::Right => *alt_right,
            };
            input.consumed_modifiers = InputModifiers {
                alt: !policy_applies,
                alt_right: *alt_right && !policy_applies,
                ..InputModifiers::default()
            };
            input.option_as_alt = *policy;
            assert_eq!(
                emulator.key(input).unwrap().bytes,
                *expected,
                "{policy:?}, right={alt_right}"
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
    fn synchronized_output_should_publish_only_the_completed_transaction() {
        let mut emulator = emulator(16, 2);
        let _ = emulator.snapshot().unwrap();

        emulator.feed(b"\x1b[?2026hpartial");
        assert!(emulator.snapshot().unwrap().is_none());

        emulator.feed(b" complete\x1b[?2026l");
        let completed = emulator.snapshot().unwrap().unwrap();
        assert!(row_text(&completed, 0).starts_with("partial complete"));
    }

    #[test]
    fn synchronized_output_deadline_should_release_a_stalled_transaction() {
        let mut emulator = emulator(16, 2);
        let _ = emulator.snapshot().unwrap();
        let started = Instant::now();
        emulator.feed_at(b"\x1b[?2026hstalled", started);
        assert!(emulator.snapshot().unwrap().is_none());

        assert!(
            !emulator
                .expire_synchronized_output(started + Duration::from_millis(999))
                .unwrap()
        );
        assert!(emulator.snapshot().unwrap().is_none());
        assert!(
            emulator
                .expire_synchronized_output(started + Duration::from_secs(1))
                .unwrap()
        );
        let released = emulator.snapshot().unwrap().unwrap();
        assert!(row_text(&released, 0).starts_with("stalled"));
    }

    #[test]
    fn synchronized_output_deadline_should_follow_the_last_output_activity() {
        let mut emulator = emulator(32, 2);
        let _ = emulator.snapshot().unwrap();
        let started = Instant::now();
        emulator.feed_at(b"\x1b[?2026hlong", started);
        assert!(emulator.snapshot().unwrap().is_none());

        let progressed = started + Duration::from_millis(900);
        emulator.feed_at(b" remote redraw", progressed);
        assert!(emulator.snapshot().unwrap().is_none());
        assert!(
            !emulator
                .expire_synchronized_output(started + Duration::from_secs(1))
                .unwrap(),
            "active synchronized output must not expose an intermediate grid"
        );

        assert!(
            emulator
                .expire_synchronized_output(progressed + Duration::from_secs(1))
                .unwrap(),
            "one second without output must still release a stalled producer"
        );
        let released = emulator.snapshot().unwrap().unwrap();
        assert!(row_text(&released, 0).starts_with("long remote redraw"));
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
        assert_eq!(second.damage, SnapshotDamage::cursor(0));
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
                    width_cells: 1,
                }),
                visible: true,
                blinking: false,
                password_input: false,
                shape: CursorShapeSnapshot::Bar,
                color: Color::rgb(0x11_22_33),
                text_color: snapshot.colors.background,
            }
        );
    }

    #[test]
    fn cursor_on_a_wide_tail_normalizes_to_the_full_grapheme() {
        let mut emulator = emulator(6, 2);
        emulator.feed("界\x1b[1D".as_bytes());

        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert_eq!(
            snapshot.cursor.position,
            Some(CursorPositionSnapshot {
                column: 0,
                row: 0,
                width_cells: 2,
            })
        );
    }

    #[test]
    fn cursor_snapshot_tracks_blink_requests_and_hidden_state() {
        let mut emulator = emulator(6, 2);
        emulator.feed(b"\x1b[5 q");
        let blinking = emulator.snapshot().unwrap().unwrap();
        assert_eq!(blinking.cursor.shape, CursorShapeSnapshot::Bar);
        assert!(blinking.cursor.blinking);
        assert!(blinking.cursor.visible);

        emulator.feed(b"\x1b[?25l");
        let hidden = emulator.snapshot().unwrap().unwrap();
        assert!(!hidden.cursor.visible);
    }

    #[test]
    fn cursor_movement_damages_only_the_old_and_new_rows() {
        let mut emulator = emulator(6, 3);
        let _ = emulator.snapshot().unwrap().unwrap();
        emulator.feed(b"\x1b[3;1H");

        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert_eq!(
            snapshot.damage.cursor,
            ContentDamageSnapshot::Rows(Arc::from([0, 2]))
        );
        assert_eq!(snapshot.damage.content, ContentDamageSnapshot::Clean);
    }

    #[test]
    fn selection_is_rendered_and_formats_as_plain_text() {
        let mut emulator = emulator(12, 3);
        emulator.feed(b"hello world");
        _ = emulator.snapshot().unwrap();
        assert_eq!(emulator.selection_text().unwrap(), None);

        emulator
            .pointer(current_pointer(
                &emulator,
                pointer(
                    PointerPhase::Press,
                    Some(PointerButton::Left),
                    2.0,
                    10.0,
                    false,
                ),
            ))
            .unwrap();
        emulator
            .pointer(current_pointer(
                &emulator,
                pointer(PointerPhase::Motion, None, 48.0, 10.0, false),
            ))
            .unwrap();
        emulator
            .pointer(current_pointer(
                &emulator,
                pointer(
                    PointerPhase::Release,
                    Some(PointerButton::Left),
                    48.0,
                    10.0,
                    false,
                ),
            ))
            .unwrap();

        let snapshot = emulator.snapshot().unwrap().unwrap();
        assert!(snapshot.rows[0][..5].iter().all(|cell| cell.selected));
        assert!(!snapshot.rows[0][5].selected);
        assert_eq!(emulator.selection_text().unwrap(), Some("hello".to_owned()));
    }

    #[test]
    fn snapshot_preserves_selection_presence_when_selected_content_is_offscreen() {
        let mut emulator = emulator(10, 2);
        emulator.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        let presented = emulator.snapshot().unwrap().unwrap();

        let mut pointer = current_pointer(
            &emulator,
            pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                2.0,
                21.0,
                false,
            ),
        );
        emulator.pointer(pointer).unwrap();
        pointer.phase = PointerPhase::Motion;
        pointer.button = None;
        pointer.position.x = 48.0;
        emulator.pointer(pointer).unwrap();
        pointer.phase = PointerPhase::Release;
        pointer.button = Some(PointerButton::Left);
        emulator.pointer(pointer).unwrap();
        let selected = emulator.snapshot().unwrap().unwrap();
        assert!(selected.selection_present);

        let action = emulator.scroll_to_at(0, selected.generation);
        assert!(action.screen_changed);
        let scrolled = emulator.snapshot().unwrap().unwrap();

        assert!(scrolled.selection_present);
        assert!(
            scrolled
                .rows
                .iter()
                .flat_map(|row| row.iter())
                .all(|cell| !cell.selected)
        );
        assert!(scrolled.generation > presented.generation);

        emulator.clear_selection().unwrap();
        let cleared = emulator.snapshot().unwrap().unwrap();
        assert!(!cleared.selection_present);
        assert!(cleared.damage.selection_presence);
    }

    #[test]
    fn snapshot_selection_presence_reflects_screen_switch_clearing() {
        let mut emulator = emulator(12, 2);
        emulator.feed(b"hello world");
        select_first_five(&mut emulator, false);

        let primary = emulator.snapshot().unwrap().unwrap();
        assert_eq!(primary.active_screen, ActiveScreenSnapshot::Primary);
        assert!(primary.selection_present);

        emulator.feed(b"\x1b[?1049h");
        let alternate = emulator.snapshot().unwrap().unwrap();
        assert_eq!(alternate.active_screen, ActiveScreenSnapshot::Alternate);
        assert!(!alternate.selection_present);
        assert!(
            alternate
                .rows
                .iter()
                .flat_map(|row| row.iter())
                .all(|cell| !cell.selected)
        );
        assert!(alternate.damage.selection_presence);

        emulator.feed(b"\x1b[?1049l");
        let restored = emulator.snapshot().unwrap().unwrap();
        assert_eq!(restored.active_screen, ActiveScreenSnapshot::Primary);
        assert!(!restored.selection_present);
        assert!(!restored.damage.selection_presence);
        assert!(
            restored
                .rows
                .iter()
                .flat_map(|row| row.iter())
                .all(|cell| !cell.selected)
        );
    }

    #[test]
    fn accessibility_selection_requires_the_current_semantic_snapshot() {
        let mut emulator = emulator(12, 3);
        emulator.feed(b"hello");
        let (model, more) = emulator.accessibility_snapshot(true).unwrap();
        assert!(more);
        assert!(model.is_none());
        let (model, more) = emulator.accessibility_snapshot(false).unwrap();
        assert!(!more);
        let model = model.unwrap();
        _ = emulator.snapshot().unwrap();
        let request = model.selection_request(1..4).unwrap();

        emulator
            .set_accessibility_selection(AccessibilitySelectionRequest {
                generation: PresentationGeneration::default(),
                ..request.clone()
            })
            .unwrap();
        assert_eq!(emulator.selection_text().unwrap(), None);

        emulator.set_accessibility_selection(request).unwrap();
        assert_eq!(emulator.selection_text().unwrap(), Some("ell".to_owned()));
    }

    #[test]
    fn accessibility_selection_rejects_synchronized_output_and_resets_pointer_invalidation() {
        let mut emulator = emulator(12, 3);
        emulator.feed(b"hello");
        let (model, more) = emulator.accessibility_snapshot(true).unwrap();
        assert!(more);
        assert!(model.is_none());
        let (model, more) = emulator.accessibility_snapshot(false).unwrap();
        assert!(!more);
        let model = model.unwrap();
        _ = emulator.snapshot().unwrap();
        let request = model.selection_request(1..4).unwrap();

        emulator.feed(b"\x1b[?2026hhidden");
        let action = emulator
            .set_accessibility_selection(request.clone())
            .unwrap();
        assert!(action.bytes.is_empty());
        assert!(!action.screen_changed);
        assert_eq!(emulator.selection_text().unwrap(), None);

        emulator.feed(b"\x1b[?2026l");
        let (model, more) = emulator.accessibility_snapshot(true).unwrap();
        assert!(!more);
        let model = model.unwrap();
        _ = emulator.snapshot().unwrap();
        emulator.pointer_mapping_invalidated = true;
        let request = model.selection_request(1..4).unwrap();
        emulator.set_accessibility_selection(request).unwrap();

        assert!(!emulator.pointer_mapping_invalidated);
    }

    #[test]
    fn modifier_key_transitions_should_preserve_selection_for_application_shortcuts() {
        let mut emulator = emulator(12, 3);
        emulator.feed(b"hello world");
        select_first_five(&mut emulator, false);
        let mut meta = key(
            PhysicalKey::MetaLeft,
            InputModifiers {
                platform: true,
                ..InputModifiers::default()
            },
        );

        emulator.key(meta.clone()).unwrap();
        meta.action = KeyAction::Release;
        meta.modifiers.platform = false;
        emulator.key(meta).unwrap();

        assert_eq!(emulator.selection_text().unwrap(), Some("hello".to_owned()));
    }

    #[test]
    fn non_modifier_key_input_should_clear_selection() {
        let mut emulator = emulator(12, 3);
        emulator.feed(b"hello world");
        select_first_five(&mut emulator, false);

        emulator
            .key(key(PhysicalKey::A, InputModifiers::default()))
            .unwrap();

        assert_eq!(emulator.selection_text().unwrap(), None);
    }

    #[test]
    fn non_modifier_key_release_preserves_selection() {
        let mut emulator = emulator(12, 3);
        emulator.feed(b"hello world");
        select_first_five(&mut emulator, false);
        let mut released = key(PhysicalKey::A, InputModifiers::default());
        released.action = KeyAction::Release;

        emulator.key(released).unwrap();

        assert_eq!(emulator.selection_text().unwrap(), Some("hello".to_owned()));
        emulator
            .key(key(PhysicalKey::A, InputModifiers::default()))
            .unwrap();
        assert_eq!(emulator.selection_text().unwrap(), None);
    }

    #[test]
    fn selection_copy_distinguishes_soft_wraps_and_hard_lines() {
        let mut emulator = emulator(5, 3);
        emulator.feed(b"abcdefgh\r\nxy");
        select_all(&mut emulator);

        let copy = emulator
            .selection_copy(SelectionCopyOptions::default())
            .unwrap()
            .unwrap();

        assert_eq!(copy.plain_text, "abcdefgh\nxy");
        let html = copy.html.unwrap();
        assert!(html.contains("abcdefgh"));
        assert!(html.contains("xy"));
        assert!(!html.contains("CellSnapshot"));
    }

    #[test]
    fn selection_copy_wide_cells_and_trailing_space_policy_are_deterministic() {
        let mut emulator = emulator(6, 2);
        emulator.feed("😀  \r\nx".as_bytes());
        select_all(&mut emulator);

        let trimmed = emulator
            .selection_copy(SelectionCopyOptions::default())
            .unwrap()
            .unwrap();
        let preserved = emulator
            .selection_copy(SelectionCopyOptions {
                trailing_spaces: TrailingSpacePolicy::Preserve,
                include_html: false,
                ..SelectionCopyOptions::default()
            })
            .unwrap()
            .unwrap();

        assert_eq!(trimmed.plain_text.matches('😀').count(), 1);
        assert_eq!(trimmed.plain_text, "😀\nx");
        assert!(preserved.plain_text.starts_with("😀  \n"));
        assert_eq!(preserved.plain_text.matches('😀').count(), 1);
        assert_eq!(preserved.html, None);
    }

    #[test]
    fn repeat_clicks_select_cells_words_and_lines_with_injected_time() {
        let mut emulator = emulator(12, 2);
        emulator.feed(b"alpha beta");

        for (time, expected) in [
            (Duration::ZERO, None),
            (Duration::from_millis(100), Some("beta")),
            (Duration::from_millis(200), Some("alpha beta")),
        ] {
            emulator.set_gesture_time_for_test(time);
            _ = emulator
                .pointer(pointer(
                    PointerPhase::Press,
                    Some(PointerButton::Left),
                    61.0,
                    1.0,
                    false,
                ))
                .unwrap();
            _ = emulator
                .pointer(pointer(
                    PointerPhase::Release,
                    Some(PointerButton::Left),
                    61.0,
                    1.0,
                    false,
                ))
                .unwrap();

            assert_eq!(emulator.selection_text().unwrap().as_deref(), expected);
        }
    }

    #[test]
    fn wide_tail_and_soft_wrapped_word_select_complete_graphemes() {
        for x in [11.0, 21.0] {
            let mut wide = emulator(8, 2);
            wide.feed("A😀B".as_bytes());
            for time in [Duration::ZERO, Duration::from_millis(100)] {
                wide.set_gesture_time_for_test(time);
                _ = wide
                    .pointer(pointer(
                        PointerPhase::Press,
                        Some(PointerButton::Left),
                        x,
                        1.0,
                        false,
                    ))
                    .unwrap();
                _ = wide
                    .pointer(pointer(
                        PointerPhase::Release,
                        Some(PointerButton::Left),
                        x,
                        1.0,
                        false,
                    ))
                    .unwrap();
            }
            assert_eq!(wide.selection_text().unwrap().as_deref(), Some("A😀"));
        }

        let mut wrapped = emulator(5, 2);
        wrapped.feed(b"abcdefgh");
        for time in [Duration::ZERO, Duration::from_millis(100)] {
            wrapped.set_gesture_time_for_test(time);
            _ = wrapped
                .pointer(pointer(
                    PointerPhase::Press,
                    Some(PointerButton::Left),
                    11.0,
                    21.0,
                    false,
                ))
                .unwrap();
            _ = wrapped
                .pointer(pointer(
                    PointerPhase::Release,
                    Some(PointerButton::Left),
                    11.0,
                    21.0,
                    false,
                ))
                .unwrap();
        }
        assert_eq!(
            wrapped.selection_text().unwrap().as_deref(),
            Some("abcdefgh")
        );
    }

    #[test]
    fn selection_autoscroll_rate_is_bounded_by_offscreen_depth() {
        assert_eq!(
            selection_autoscroll_interval_for_position(SurfacePosition { x: 1.0, y: 10.0 }, 40, 20,),
            None
        );
        assert_eq!(
            selection_autoscroll_interval_for_position(SurfacePosition { x: 1.0, y: -1.0 }, 40, 20,),
            Some(MAX_SELECTION_AUTOSCROLL_INTERVAL)
        );
        assert_eq!(
            selection_autoscroll_interval_for_position(
                SurfacePosition { x: 1.0, y: -200.0 },
                40,
                20,
            ),
            Some(MIN_SELECTION_AUTOSCROLL_INTERVAL)
        );
    }

    #[test]
    fn autoscroll_tick_moves_the_viewport_and_rejects_a_stale_generation() {
        let mut emulator = emulator(8, 2);
        emulator.feed(b"one\r\ntwo\r\nthree\r\nfour");
        let initial = emulator.snapshot().unwrap().unwrap();
        let press = current_pointer(
            &emulator,
            pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                1.0,
                21.0,
                false,
            ),
        );
        assert!(emulator.pointer(press).unwrap().screen_changed);
        let drag = current_pointer(
            &emulator,
            pointer(PointerPhase::Motion, None, 1.0, -41.0, false),
        );
        assert!(emulator.pointer(drag).unwrap().screen_changed);
        let dragged = emulator.snapshot().unwrap().unwrap();
        assert_eq!(
            emulator.selection_autoscroll_interval().unwrap(),
            Some(Duration::from_millis(100))
        );

        let action = emulator
            .selection_autoscroll_tick(dragged.generation)
            .unwrap();
        assert!(action.screen_changed);
        let scrolled = emulator.snapshot().unwrap().unwrap();
        assert!(scrolled.scrollbar.offset_rows < dragged.scrollbar.offset_rows);
        assert!(
            emulator
                .selection_autoscroll_tick(initial.generation)
                .unwrap()
                .bytes
                .is_empty()
        );
        assert_eq!(emulator.selection_autoscroll_interval().unwrap(), None);
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

        let input = current_wheel(&emulator, wheel(1, false));
        let action = emulator.wheel(input).unwrap();
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
        assert_eq!(scrolled.cursor.position, None);

        let action = emulator.scroll_to(bottom.scrollbar.offset_rows);
        assert!(action.bytes.is_empty());
        assert!(action.screen_changed);
        let restored = emulator.snapshot().unwrap().unwrap();
        assert!(row_text(&restored, 0).starts_with("two"));
        assert_eq!(restored.cursor.position.unwrap().row, 1);
        assert!(row_text(&restored, 1).starts_with("three"));
    }

    #[test]
    fn new_output_follows_the_bottom_only_from_a_following_viewport() {
        let mut following = emulator(10, 2);
        following.feed(b"one\r\ntwo\r\nthree");
        let _ = following.snapshot().unwrap().unwrap();
        following.feed(b"\r\nfour");
        let advanced = following.snapshot().unwrap().unwrap();
        assert!(row_text(&advanced, 0).starts_with("three"));
        assert!(row_text(&advanced, 1).starts_with("four"));
        assert_eq!(
            advanced.scrollbar.offset_rows + advanced.scrollbar.visible_rows,
            advanced.scrollbar.total_rows
        );

        let mut anchored = emulator(10, 2);
        anchored.feed(b"one\r\ntwo\r\nthree");
        let _ = anchored.snapshot().unwrap().unwrap();
        let input = current_wheel(&anchored, wheel(1, false));
        anchored.wheel(input).unwrap();
        let before = anchored.snapshot().unwrap().unwrap();
        assert!(row_text(&before, 0).starts_with("one"));
        anchored.feed(b"\r\nfour");
        let after = anchored.snapshot().unwrap().unwrap();
        assert!(row_text(&after, 0).starts_with("one"));
        assert!(row_text(&after, 1).starts_with("two"));
        assert!(
            after.scrollbar.offset_rows + after.scrollbar.visible_rows < after.scrollbar.total_rows
        );
    }

    #[test]
    fn erase_saved_lines_clears_scrollback_without_discarding_the_visible_screen() {
        let mut emulator = emulator(10, 2);
        emulator.feed(b"one\r\ntwo\r\nthree");
        let before = emulator.snapshot().unwrap().unwrap();
        assert!(before.scrollbar.total_rows > before.scrollbar.visible_rows);

        emulator.feed(b"\x1b[3J");
        let cleared = emulator.snapshot().unwrap().unwrap();

        assert!(row_text(&cleared, 0).starts_with("two"));
        assert!(row_text(&cleared, 1).starts_with("three"));
        assert_eq!(cleared.scrollbar.total_rows, cleared.scrollbar.visible_rows);
        assert_eq!(cleared.scrollbar.offset_rows, 0);
    }

    #[test]
    fn terminal_reset_returns_to_a_clean_primary_screen() {
        let mut emulator = emulator(10, 2);
        emulator.feed(b"primary\r\nscroll\r\nback\x1b[?1049halternate");
        let alternate = emulator.snapshot().unwrap().unwrap();
        assert_eq!(alternate.active_screen, ActiveScreenSnapshot::Alternate);

        emulator.feed(b"\x1bc");
        let reset = emulator.snapshot().unwrap().unwrap();

        assert_eq!(reset.active_screen, ActiveScreenSnapshot::Primary);
        assert_eq!(reset.scrollbar.total_rows, reset.scrollbar.visible_rows);
        assert!((0..reset.rows.len()).all(|row| row_text(&reset, row).trim().is_empty()));
        assert_eq!(reset.cursor.position.unwrap().row, 0);
        assert_eq!(reset.cursor.position.unwrap().column, 0);
    }

    #[test]
    fn primary_scrollback_is_bounded_to_the_configured_history() {
        let mut emulator = emulator(10, 2);
        let output = b"x\r\n".repeat(MAX_SCROLLBACK_ROWS + 100);
        emulator.feed(&output);

        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert!(
            snapshot.scrollbar.total_rows
                <= u64::try_from(MAX_SCROLLBACK_ROWS).unwrap() + snapshot.scrollbar.visible_rows
        );
        assert_eq!(
            snapshot.scrollbar.offset_rows + snapshot.scrollbar.visible_rows,
            snapshot.scrollbar.total_rows
        );
    }

    #[test]
    fn scrollback_rejects_a_viewport_request_from_an_older_presentation() {
        let mut emulator = emulator(10, 2);
        emulator.feed(b"one\r\ntwo\r\nthree");
        let stale = emulator.snapshot().unwrap().unwrap();
        emulator.resize(geometry(8, 2, 8.0, 18.0)).unwrap();
        let current = emulator.snapshot().unwrap().unwrap();
        assert!(current.generation > stale.generation);

        let action = emulator.scroll_to_at(0, stale.generation);
        assert!(!action.screen_changed);
        assert!(emulator.snapshot().unwrap().is_none());
    }

    #[test]
    fn selection_rejects_coordinates_from_an_older_presentation() {
        let mut emulator = emulator(10, 2);
        emulator.feed(b"hello world");
        let stale = emulator.snapshot().unwrap().unwrap();
        emulator.resize(geometry(8, 2, 8.0, 18.0)).unwrap();
        let _ = emulator.snapshot().unwrap().unwrap();

        let action = emulator
            .pointer(PointerInput {
                generation: stale.generation,
                phase: PointerPhase::Press,
                button: Some(PointerButton::Left),
                position: SurfacePosition { x: 2.0, y: 10.0 },
                modifiers: InputModifiers::default(),
                shift_selection: ShiftSelectionPolicy::default(),
            })
            .unwrap();

        assert!(!action.screen_changed);
        assert_eq!(emulator.selection_text().unwrap(), None);
    }

    #[test]
    fn selection_owned_presentations_do_not_stale_the_active_gesture() {
        let mut emulator = emulator(12, 2);
        emulator.feed(b"hello world");
        let presented = emulator.snapshot().unwrap().unwrap();
        let press = PointerInput {
            generation: presented.generation,
            phase: PointerPhase::Press,
            button: Some(PointerButton::Left),
            position: SurfacePosition { x: 1.0, y: 1.0 },
            modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::default(),
        };
        _ = emulator.pointer(press).unwrap();
        let mut drag = press;
        drag.phase = PointerPhase::Motion;
        drag.button = None;
        drag.position.x = 41.0;
        _ = emulator.pointer(drag).unwrap();
        let selected = emulator.snapshot().unwrap().unwrap();
        assert!(selected.generation > presented.generation);

        drag.position.x = 108.0;
        let extended = emulator.pointer(drag).unwrap();

        assert!(extended.screen_changed);
        assert_eq!(
            emulator.selection_text().unwrap().as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn selection_accepts_an_intermediate_selection_owned_presentation() {
        let mut emulator = emulator(12, 2);
        emulator.feed(b"hello world");
        let presented = emulator.snapshot().unwrap().unwrap();
        let mut pointer = PointerInput {
            generation: presented.generation,
            phase: PointerPhase::Press,
            button: Some(PointerButton::Left),
            position: SurfacePosition { x: 1.0, y: 1.0 },
            modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::default(),
        };
        _ = emulator.pointer(pointer).unwrap();

        pointer.phase = PointerPhase::Motion;
        pointer.button = None;
        pointer.position.x = 25.0;
        _ = emulator.pointer(pointer).unwrap();
        let intermediate = emulator.snapshot().unwrap().unwrap();

        pointer.position.x = 41.0;
        _ = emulator.pointer(pointer).unwrap();
        _ = emulator.snapshot().unwrap().unwrap();

        pointer.generation = intermediate.generation;
        pointer.position.x = 108.0;
        _ = emulator.pointer(pointer).unwrap();

        assert_eq!(
            emulator.selection_text().unwrap().as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn application_mouse_release_survives_output_generation_change() {
        let mut emulator = emulator(8, 2);
        emulator.feed(b"\x1b[?1000h\x1b[?1006h");
        let before = emulator.snapshot().unwrap().unwrap();
        let press = PointerInput {
            generation: before.generation,
            phase: PointerPhase::Press,
            button: Some(PointerButton::Left),
            position: SurfacePosition { x: 1.0, y: 1.0 },
            modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::default(),
        };
        _ = emulator.pointer(press).unwrap();
        emulator.feed(b"redraw");
        _ = emulator.snapshot().unwrap().unwrap();

        let release = emulator
            .pointer(PointerInput {
                phase: PointerPhase::Release,
                ..press
            })
            .unwrap();

        assert_eq!(release.bytes, b"\x1b[<0;1;1m");
    }

    #[test]
    fn output_mapping_changes_reject_active_gesture_coordinates() {
        let mut emulator = emulator(8, 2);
        emulator.feed(b"one\r\ntwo\r\nthree");
        let before = emulator.snapshot().unwrap().unwrap();
        let input = |phase, x| PointerInput {
            generation: before.generation,
            phase,
            button: (phase != PointerPhase::Motion).then_some(PointerButton::Left),
            position: SurfacePosition { x, y: 21.0 },
            modifiers: InputModifiers::default(),
            shift_selection: ShiftSelectionPolicy::default(),
        };
        _ = emulator.pointer(input(PointerPhase::Press, 1.0)).unwrap();
        _ = emulator.pointer(input(PointerPhase::Motion, 28.0)).unwrap();
        emulator.feed(b"\r\nfour");
        let after = emulator.snapshot().unwrap().unwrap();
        assert!(after.generation > before.generation);

        let stale = emulator.pointer(input(PointerPhase::Motion, 68.0)).unwrap();

        assert!(!stale.screen_changed);
        assert_eq!(emulator.selection_autoscroll_interval().unwrap(), None);
    }

    #[test]
    fn tracked_and_alternate_screen_wheel_events_encode_input() {
        let mut tracked = emulator(10, 2);
        tracked.feed(b"\x1b[?1000h\x1b[?1006h");
        let reported = tracked.wheel(wheel(2, false)).unwrap();
        assert_eq!(reported.bytes, b"\x1b[<64;1;1M\x1b[<64;1;1M");
        assert!(reported.screen_changed);
        let mut horizontal = wheel(0, false);
        horizontal.horizontal_steps = 1;
        assert_eq!(tracked.wheel(horizontal).unwrap().bytes, b"\x1b[<66;1;1M");

        let mut alternate = emulator(10, 2);
        alternate.feed(b"\x1b[?1049h\x1b[?1007h");
        assert_eq!(
            alternate.wheel(wheel(2, false)).unwrap().bytes,
            b"\x1b[A\x1b[A"
        );
        alternate.feed(b"\x1b[?1h");
        assert_eq!(alternate.wheel(wheel(-1, false)).unwrap().bytes, b"\x1bOB");
        let mut horizontal = wheel(0, false);
        horizontal.horizontal_steps = -2;
        let ignored = alternate.wheel(horizontal).unwrap();
        assert!(ignored.bytes.is_empty());
        assert!(!ignored.screen_changed);
    }

    #[test]
    fn osc8_identity_survives_wrapping_in_immutable_snapshots() {
        let mut emulator = emulator(5, 2);
        emulator.feed(b"\x1b]8;;https://example.test/path\x07abcdefgh\x1b]8;;\x07");

        let snapshot = emulator.snapshot().unwrap().unwrap();
        let identities = snapshot
            .rows
            .iter()
            .flat_map(|row| row.iter())
            .filter_map(|cell| cell.hyperlink.as_ref().map(|link| link.identity))
            .collect::<Vec<_>>();

        assert_eq!(identities.len(), 8);
        assert!(identities.iter().all(|identity| *identity == identities[0]));
    }

    #[test]
    fn osc8_relative_file_targets_bind_to_the_directory_at_emission() {
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-emulator-local-link-first-{}",
            std::process::id()
        ));
        let second_directory = std::env::temp_dir().join(format!(
            "spaceterm-emulator-local-link-second-{}",
            std::process::id()
        ));
        _ = fs::remove_dir_all(&directory);
        _ = fs::remove_dir_all(&second_directory);
        fs::create_dir_all(&directory).unwrap();
        fs::create_dir_all(&second_directory).unwrap();
        let file = directory.join("preview.txt");
        let second_file = second_directory.join("preview.txt");
        fs::write(&file, b"preview").unwrap();
        fs::write(&second_file, b"preview").unwrap();
        let mut emulator = TerminalEmulator::new_with_metadata(
            geometry(16, 2, 10.0, 20.0),
            directory.to_str().unwrap(),
            "zsh",
            Some("mac.local"),
            identity::TERM_FALLBACK,
            Instant::now(),
        )
        .unwrap();

        emulator.feed(b"\x1b]8;;file:prev");
        emulator.feed(b"iew.txt\x07first\x1b]8;;\x07 \x1b]7;FiLe://local");
        emulator.feed(
            format!(
                "host{}\x07\x1b]8;;file:preview.txt\x07second\x1b]8;;\x07",
                second_directory.to_str().unwrap()
            )
            .as_bytes(),
        );
        let replacement = directory.join("replacement.txt");
        fs::write(&replacement, b"replacement").unwrap();
        fs::rename(&replacement, &file).unwrap();
        let snapshot = emulator.snapshot().unwrap().unwrap();
        let values = snapshot.rows[0]
            .iter()
            .map(|cell| cell.hyperlink.as_ref().map(|link| link.value.as_str()))
            .collect::<Vec<_>>();

        assert!(
            values[..5]
                .iter()
                .all(|value| { *value == Some(file.canonicalize().unwrap().to_str().unwrap()) })
        );
        assert!(values[6..12].iter().all(|value| {
            *value == Some(second_file.canonicalize().unwrap().to_str().unwrap())
        }));
        let first_link = snapshot.rows[0][0].hyperlink.as_ref().unwrap();
        assert_eq!(
            first_link.value,
            file.canonicalize().unwrap().to_str().unwrap()
        );
        assert_eq!(
            first_link
                .activation_url(crate::terminal::metadata::TerminalLocalFileCapabilities::Enabled),
            None
        );
        fs::remove_dir_all(directory).unwrap();
        fs::remove_dir_all(second_directory).unwrap();
    }

    #[test]
    fn remote_metadata_context_should_never_resolve_file_links_as_local_paths() {
        let metadata_context = TerminalMetadataContext::Remote(
            crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                crate::domain::SshDestination::new("user@remote".to_owned()).unwrap(),
                crate::domain::RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap(),
            ),
        );
        let mut emulator = TerminalEmulator::new_with_metadata_context(
            geometry(16, 2, 10.0, 20.0),
            metadata_context,
            "project on remote",
            identity::TERM_FALLBACK,
            Instant::now(),
        )
        .unwrap();

        emulator.feed(
            b"\x1b]8;;file:preview.txt\x07remote\x1b]8;;\x07 \
              \x1b]8;;https://example.test\x07web\x1b]8;;\x07",
        );

        let snapshot = emulator.snapshot().unwrap().unwrap();
        assert!(
            snapshot.rows[0][..6]
                .iter()
                .all(|cell| cell.hyperlink.is_none())
        );
        assert!(snapshot.rows[0][7..10].iter().all(|cell| {
            cell.hyperlink
                .as_ref()
                .is_some_and(|link| link.kind == crate::terminal::hyperlink::HyperlinkKind::Url)
        }));
        assert!(!snapshot.metadata.context.is_local());
        assert_eq!(snapshot.metadata.directory.path.as_ref(), "~/project");
    }

    #[test]
    fn ground_utf8_containing_9d_is_printed_without_starting_c1_osc() {
        let mut emulator = emulator(24, 2);

        emulator.feed("before\u{075d}after".as_bytes());
        let snapshot = emulator.snapshot().unwrap().unwrap();
        let text = snapshot.rows[0]
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>();

        assert!(text.starts_with("before\u{075d}after"));
        assert!(snapshot.rows[0].iter().all(|cell| cell.hyperlink.is_none()));
    }

    #[test]
    fn raw_9d_in_ground_does_not_introduce_an_osc8_link() {
        let mut emulator = emulator(32, 2);

        emulator.feed(b"\x9d8;;file:/tmp/not-a-link\x07visible");
        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert!(
            snapshot
                .rows
                .iter()
                .flat_map(|row| row.iter())
                .all(|cell| cell.hyperlink.is_none())
        );
    }

    #[test]
    fn rejected_local_osc8_start_ends_a_previously_active_link() {
        let mut emulator = emulator(16, 2);

        emulator
            .feed(b"\x1b]8;;https://example.test\x07web\x1b]8;;file:missing\x07plain\x1b]8;;\x07");
        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert!(
            snapshot.rows[0][..3]
                .iter()
                .all(|cell| cell.hyperlink.is_some())
        );
        assert!(
            snapshot.rows[0][3..8]
                .iter()
                .all(|cell| cell.hyperlink.is_none())
        );
    }

    #[test]
    fn unsupported_terminal_uri_cannot_attach_resolver_only_local_metadata() {
        let mut emulator = emulator(16, 2);

        emulator.feed(b"\x1b]8;;unsupported:terminal-controlled-metadata\x07plain\x1b]8;;\x07");
        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert!(
            snapshot.rows[0][..5]
                .iter()
                .all(|cell| cell.hyperlink.is_none())
        );
    }

    #[test]
    fn c1_st_inside_osc_is_payload_in_the_pinned_terminal_stream() {
        let mut emulator = emulator(24, 2);

        emulator.feed(b"\x1b]8;;file:missing\x9cNOT_VISIBLE\x07visible");
        let snapshot = emulator.snapshot().unwrap().unwrap();
        let text = snapshot.rows[0]
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>();

        assert!(text.starts_with("visible"));
        assert!(!text.contains("NOT_VISIBLE"));
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
    fn bracketed_paste_encoder_neutralizes_embedded_closing_fences_and_controls() {
        let mut emulator = emulator(10, 2);
        emulator.feed(b"\x1b[?2004h");

        let action = emulator.paste("a\x1b[201~\x03b\n".to_owned()).unwrap();

        assert_eq!(action.bytes, b"\x1b[200~a [201~ b\n\x1b[201~");
        assert_eq!(
            action
                .bytes
                .windows(b"\x1b[201~".len())
                .filter(|window| *window == b"\x1b[201~")
                .count(),
            1
        );
    }

    #[test]
    fn focus_encoding_obeys_dec_1004_mode() {
        let mut emulator = emulator(10, 2);

        assert_eq!(emulator.focus(true).unwrap().bytes, b"");
        emulator.feed(b"\x1b[?1004h");
        assert_eq!(emulator.focus(true).unwrap().bytes, b"\x1b[I");
        assert_eq!(emulator.focus(false).unwrap().bytes, b"\x1b[O");
        emulator.feed(b"\x1b[?1004l");
        assert_eq!(emulator.focus(false).unwrap().bytes, b"");
    }

    #[test]
    fn mouse_tracking_reports_bytes_but_shift_overrides_with_selection() {
        let mut emulator = emulator(12, 3);
        emulator.feed(b"hello world\x1b[?1000h\x1b[?1006h");
        _ = emulator.snapshot().unwrap();

        let input = current_pointer(
            &emulator,
            pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                2.0,
                10.0,
                false,
            ),
        );
        let reported = emulator.pointer(input).unwrap();
        assert!(!reported.bytes.is_empty());
        let input = current_pointer(
            &emulator,
            pointer(
                PointerPhase::Release,
                Some(PointerButton::Left),
                2.0,
                10.0,
                true,
            ),
        );
        let released = emulator.pointer(input).unwrap();
        assert!(!released.bytes.is_empty());

        let input = current_pointer(
            &emulator,
            pointer(
                PointerPhase::Press,
                Some(PointerButton::Left),
                2.0,
                10.0,
                true,
            ),
        );
        let selected_press = emulator.pointer(input).unwrap();
        let input = current_pointer(
            &emulator,
            pointer(PointerPhase::Motion, None, 48.0, 10.0, false),
        );
        let selected_drag = emulator.pointer(input).unwrap();
        assert!(selected_press.bytes.is_empty());
        assert!(selected_drag.bytes.is_empty());
        assert!(selected_drag.screen_changed);

        let snapshot = emulator.snapshot().unwrap().unwrap();
        assert!(snapshot.rows[0][..5].iter().all(|cell| cell.selected));
    }

    #[test]
    fn shift_selection_override_is_policy_driven() {
        let mut emulator = emulator(10, 2);
        emulator.feed(b"\x1b[?1000h\x1b[?1006h");

        let mut input = pointer(
            PointerPhase::Press,
            Some(PointerButton::Left),
            1.0,
            1.0,
            true,
        );
        input.shift_selection = ShiftSelectionPolicy::ReportToApplication;

        let action = emulator.pointer(input).unwrap();

        assert_eq!(action.bytes, b"\x1b[<4;1;1M");
    }

    #[test]
    fn shift_policy_applies_to_hover_and_wheel_reporting() {
        let mut hover = emulator(10, 2);
        hover.feed(b"\x1b[?1003h\x1b[?1006h");
        let mut hover_input = pointer(PointerPhase::Motion, None, 11.0, 1.0, true);
        hover_input.shift_selection = ShiftSelectionPolicy::ReportToApplication;

        let hover_action = hover.pointer(hover_input).unwrap();

        assert_eq!(hover_action.bytes, b"\x1b[<39;2;1M");

        let mut wheel_emulator = emulator(10, 2);
        wheel_emulator.feed(b"\x1b[?1000h\x1b[?1006h");
        let mut wheel_input = wheel(1, true);
        wheel_input.shift_selection = ShiftSelectionPolicy::ReportToApplication;

        let wheel_action = wheel_emulator.wheel(wheel_input).unwrap();

        assert_eq!(wheel_action.bytes, b"\x1b[<68;1;1M");
    }

    #[test]
    fn mouse_tracking_changes_publish_pointer_routing_metadata() {
        let mut emulator = emulator(10, 2);
        let initial = emulator.snapshot().unwrap().unwrap();
        assert!(!initial.mouse_tracking);

        emulator.feed(b"\x1b[?1000h");
        let tracked = emulator.snapshot().unwrap().unwrap();

        assert!(tracked.mouse_tracking);
        assert!(tracked.damage.mouse_tracking);
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

    #[test]
    fn kitty_query_is_truthful_and_precedes_device_attributes() {
        let _guard = crate::terminal::graphics::test_lock();
        let mut emulator = emulator(8, 4);
        emulator.feed(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c");

        let replies = emulator.take_pty_responses();
        let graphics = replies
            .windows(b"\x1b_Gi=31;OK\x1b\\".len())
            .position(|window| window == b"\x1b_Gi=31;OK\x1b\\")
            .expect("a supported direct RGB query must receive an OK response");
        let attributes = replies
            .windows(b"\x1b[?62;22;52c".len())
            .position(|window| window == b"\x1b[?62;22;52c")
            .unwrap_or_else(|| panic!("device attributes missing from {replies:?}"));
        assert!(graphics < attributes);
    }

    #[test]
    fn kitty_rgb_transmission_publishes_owned_graphics_damage() {
        let _guard = crate::terminal::graphics::test_lock();
        let mut emulator = emulator(8, 4);
        let initial = emulator.snapshot().unwrap().unwrap();
        emulator.feed(b"\x1b_Ga=T,t=d,f=24,i=7,p=3,s=1,v=1,c=2,r=1;/wAA\x1b\\");

        let snapshot = emulator.snapshot().unwrap().unwrap();

        assert_eq!(snapshot.graphics.images.len(), 1);
        assert_eq!(snapshot.graphics.images[0].rgba.as_ref(), &[255, 0, 0, 255]);
        assert_eq!(snapshot.graphics.placements.len(), 1);
        assert_eq!(snapshot.graphics.placements[0].image.image_id, 7);
        assert_eq!(snapshot.graphics.placements[0].placement_id, 3);
        assert_eq!(snapshot.graphics.placements[0].destination_width, 20);
        assert_eq!(snapshot.graphics.placements[0].destination_height, 20);
        assert!(snapshot.damage.graphics_content);
        assert!(snapshot.damage.graphics_geometry);
        assert_eq!(initial.rows, snapshot.rows);

        assert!(emulator.snapshot().unwrap().is_none());
    }

    #[test]
    fn kitty_retransmission_replaces_content_and_deletion_releases_it() {
        let _guard = crate::terminal::graphics::test_lock();
        let mut emulator = emulator(8, 4);
        emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=9,p=4,s=1,v=1;AQIDBA==\x1b\\");
        let first = emulator.snapshot().unwrap().unwrap();
        let first_generation = first.graphics.images[0].key.generation;

        emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=9,p=4,s=1,v=1;BQYHCA==\x1b\\");
        let replaced = emulator.snapshot().unwrap().unwrap();
        assert_ne!(replaced.graphics.images[0].key.generation, first_generation);
        assert_eq!(replaced.graphics.images[0].rgba.as_ref(), &[5, 6, 7, 8]);

        emulator.feed(b"\x1b_Ga=d,d=i,i=9\x1b\\");
        let deleted = emulator.snapshot().unwrap().unwrap();
        assert!(deleted.graphics.images.is_empty());
        assert!(deleted.graphics.placements.is_empty());
        assert!(deleted.damage.graphics_content);
    }

    #[test]
    fn kitty_png_and_chunked_zlib_transmissions_decode_on_the_worker() {
        let _guard = crate::terminal::graphics::test_lock();
        let mut emulator = emulator(8, 4);
        emulator.feed(
            b"\x1b_Ga=T,t=d,f=100,i=20,p=1;iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==\x1b\\",
        );
        let png = emulator.snapshot().unwrap().unwrap();
        assert_eq!(
            png.graphics.images.len(),
            1,
            "PNG response: {:?}",
            emulator.take_pty_responses()
        );
        assert_eq!(png.graphics.images[0].rgba.as_ref(), &[255, 0, 0, 255]);

        emulator.feed(b"\x1b_Ga=T,t=d,f=32,o=z,i=21,p=2,s=1,v=1,m=1;eJz7z8Dw\x1b\\");
        assert!(emulator.snapshot().unwrap().is_none());
        emulator.feed(b"\x1b_Gm=0;HwAE/wH/\x1b\\");
        let zlib = emulator.snapshot().unwrap().unwrap();
        assert_eq!(zlib.graphics.images.len(), 2);
        assert_eq!(zlib.graphics.images[1].rgba.as_ref(), &[255, 0, 0, 255]);
    }

    #[test]
    fn kitty_later_display_resolves_crop_offsets_size_and_z() {
        let _guard = crate::terminal::graphics::test_lock();
        let mut emulator = emulator(8, 4);
        emulator.feed(b"\x1b_Ga=t,t=d,f=32,i=30,s=2,v=1;AQIDBAUGBwg=\x1b\\");
        let transmitted = emulator.snapshot().unwrap().unwrap();
        assert!(transmitted.graphics.images.is_empty());
        assert!(transmitted.damage.graphics_content);

        emulator
            .feed(b"\x1b_Ga=p,i=30,p=7,x=1,y=0,w=1,h=1,c=2,r=3,X=3,Y=4,C=1,z=-1073741825\x1b\\");
        let displayed = emulator.snapshot().unwrap().unwrap();
        let placement = &displayed.graphics.placements[0];
        assert_eq!(placement.placement_id, 7);
        assert_eq!(placement.source_x, 1);
        assert_eq!(placement.source_width, 1);
        assert_eq!(placement.cell_offset_x, 3);
        assert_eq!(placement.cell_offset_y, 4);
        assert_eq!(placement.destination_width, 20);
        assert_eq!(placement.destination_height, 60);
        assert_eq!(placement.z, -1_073_741_825);
        assert_eq!(displayed.cursor.position.unwrap().column, 0);
    }

    #[test]
    fn kitty_graphics_follow_screen_sync_scroll_and_resize_lifecycle() {
        let _guard = crate::terminal::graphics::test_lock();
        let mut emulator = emulator(8, 4);
        emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=40,p=1,s=1,v=1,C=1;AQIDBA==\x1b\\");
        let primary = emulator.snapshot().unwrap().unwrap();
        let primary_image = Arc::clone(&primary.graphics.images[0]);

        emulator.feed(b"\x1b[?1049h");
        let alternate = emulator.snapshot().unwrap().unwrap();
        assert!(alternate.graphics.placements.is_empty());
        emulator.feed(b"\x1b[?1049l");
        let restored = emulator.snapshot().unwrap().unwrap();
        assert!(Arc::ptr_eq(&primary_image, &restored.graphics.images[0]));

        emulator.feed(b"\x1b[?2026h\x1b_Ga=p,i=40,p=2,C=1\x1b\\");
        assert!(emulator.snapshot().unwrap().is_none());
        emulator.feed(b"\x1b[?2026l");
        let synchronized = emulator.snapshot().unwrap().unwrap();
        assert_eq!(synchronized.graphics.placements.len(), 2);

        emulator.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        let scrolled = emulator.snapshot().unwrap().unwrap();
        assert!(scrolled.damage.graphics_geometry);
        emulator.resize(geometry(10, 5, 10.0, 20.0)).unwrap();
        let resized = emulator.snapshot().unwrap().unwrap();
        assert!(resized.damage.graphics_geometry || resized.damage.resize);
    }

    #[test]
    fn kitty_unicode_placeholder_is_resolved_by_ghostty() {
        let _guard = crate::terminal::graphics::test_lock();
        let mut emulator = emulator(8, 4);
        emulator.feed(b"\x1b_Ga=t,t=d,f=32,i=1,s=1,v=1;AQIDBA==\x1b\\");
        emulator.feed(b"\x1b_Ga=p,i=1,U=1,c=1,r=1\x1b\\");
        emulator.feed("\x1b[38;5;1m\u{10eeee}\x1b[39m".as_bytes());

        let snapshot = emulator.snapshot().unwrap().unwrap();
        assert_eq!(snapshot.graphics.placements.len(), 1);
        let placement = &snapshot.graphics.placements[0];
        assert!(placement.unicode_placeholder);
        assert_eq!(placement.image.image_id, 1);
        assert_eq!(placement.viewport_col, 0);
        assert_eq!(placement.viewport_row, 0);
    }

    #[test]
    fn kitty_q_policy_and_unsupported_media_remain_safe() {
        let _guard = crate::terminal::graphics::test_lock();
        let mut emulator = emulator(8, 4);

        emulator.feed(b"\x1b_Ga=q,t=d,f=24,i=50,s=1,v=1,q=1;AAAA\x1b\\");
        assert!(emulator.take_pty_responses().is_empty());

        emulator.feed(b"\x1b_Ga=q,t=f,f=24,i=51,s=1,v=1;L2V0Yy9wYXNzd2Q=\x1b\\");
        let file_error = emulator.take_pty_responses();
        assert!(file_error.starts_with(b"\x1b_Gi=51;"));
        assert!(!file_error.windows(2).any(|window| window == b"OK"));

        emulator.feed(b"\x1b_Ga=f,i=50\x1b\\");
        let animation_error = emulator.take_pty_responses();
        assert!(!animation_error.windows(2).any(|window| window == b"OK"));

        emulator.feed(b"\x1b_Ga=q,t=d,f=32,i=52,s=8193,v=1,q=2;AAAA\x1b\\alive");
        assert!(emulator.take_pty_responses().is_empty());
        let snapshot = emulator.snapshot().unwrap().unwrap();
        assert!(snapshot.rows[0].iter().any(|cell| cell.text == "a"));
    }

    #[test]
    fn kitty_probe_stays_silent_when_application_budget_is_exhausted() {
        let _guard = crate::terminal::graphics::test_lock();
        let _first = GraphicsReservation::try_acquire().unwrap();
        let _second = GraphicsReservation::try_acquire().unwrap();
        let mut emulator = emulator(8, 4);

        emulator.feed(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");

        assert!(emulator.take_pty_responses().is_empty());
        assert!(
            emulator
                .snapshot()
                .unwrap()
                .unwrap()
                .graphics
                .images
                .is_empty()
        );
    }
}
