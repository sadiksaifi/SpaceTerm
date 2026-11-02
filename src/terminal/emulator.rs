use super::failure::{FailureReason, TerminalFailure};
use crate::platform::local_filesystem::{LocalFileEmissionRegistry, LocalFilesystemAuthority};
mod compression;
mod history;
pub(crate) use history::HistoryPage;
mod run;
pub use history::HistorySnapshot;
use std::cell::{Cell, RefCell};
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
use libghostty_vt::screen::{
    CellContentTag, CellSemanticContent, CellWide, Screen, TrackedGridRef,
};
use libghostty_vt::selection::FormatOptions;
use libghostty_vt::style::{PaletteIndex, RgbColor, StyleColor, Underline};
use libghostty_vt::terminal::{
    ColorScheme, HyperlinkResolution, Mode, Point, PointCoordinate, PointSpace, ProgressState,
    ScrollViewport, SemanticPromptAction,
};
use libghostty_vt::{Error, RenderState, Terminal, TerminalOptions};

use crate::appearance::{
    Appearance, AppearanceGeneration, Color, ResolvedTerminalAppearance, TerminalColors,
};
use crate::terminal::TerminalAppearanceUpdate;
use crate::terminal::TerminalFindSnapshot;
use crate::terminal::accessibility::{
    AccessibilityCell, AccessibilityCellRef, AccessibilityRowId, AccessibilityRowUpdate,
    AccessibilityScreen, AccessibilitySelectionRefs, AccessibilityUpdate,
    TerminalAccessibilityModel, TerminalAccessibilityState,
};
use crate::terminal::attention::AttentionEvent;
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
const MAX_WHEEL_STEPS: i32 = 100;
const MAX_SCROLLBACK_ROWS: usize = 10_000;
// Match Ghostty's application default in third_party/ghostty/src/config/Config.zig.
const MAX_SCROLLBACK_BYTES: usize = 50_000_000;
pub(crate) const MAX_SYNCHRONIZED_OUTPUT_DURATION: Duration = Duration::from_secs(1);
/// The longest a resize that cleared a redrawable prompt waits for the shell to draw it again.
pub(crate) const PROMPT_REDRAW_HOLD: Duration = Duration::from_millis(200);
/// How long shell output must pause before a redraw without a prompt-end mark counts as drawn.
pub(crate) const PROMPT_REDRAW_SETTLE: Duration = Duration::from_millis(30);

impl From<RgbColor> for Color {
    fn from(value: RgbColor) -> Self {
        Self::from_rgb_components(value.r, value.g, value.b)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CellSnapshot {
    pub text: String,
    pub foreground_source: TerminalColor,
    pub background_source: TerminalColor,
    pub inverse: bool,
    pub bold: bool,
    pub faint: bool,
    pub italic: bool,
    pub blinking: bool,
    pub invisible: bool,
    pub underline: TerminalUnderlineSnapshot,
    pub underline_source: TerminalColor,
    pub strikethrough: bool,
    pub overline: bool,
    pub selected: bool,
    pub spacer_tail: bool,
    pub semantic_content: CellSemanticSnapshot,
    pub hyperlink: Option<Arc<crate::terminal::HyperlinkTarget>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CellSemanticSnapshot {
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
pub enum TerminalUnderlineSnapshot {
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
pub enum TerminalColor {
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
pub struct TerminalColorsSnapshot {
    pub foreground: Color,
    pub background: Color,
    pub palette: Arc<[Color; 256]>,
    pub reversed: bool,
    pub foreground_source: TerminalDefaultColorSource,
    pub background_source: TerminalDefaultColorSource,
    pub cursor_source: TerminalDefaultColorSource,
    pub palette_overrides: Arc<[bool; 256]>,
    pub configured: Arc<TerminalColors>,
    pub bold_as_bright: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TerminalDefaultColorSource {
    #[default]
    HostDefault,
    ProgramOverride,
}

impl TerminalColorsSnapshot {
    fn configured(configured: &TerminalColors, bold_as_bright: bool) -> Self {
        let mut palette = [configured.foreground; 256];
        palette[..8].copy_from_slice(&configured.normal);
        palette[8..16].copy_from_slice(&configured.bright);
        Self {
            foreground: configured.foreground,
            background: configured.background,
            palette: Arc::new(palette),
            reversed: false,
            foreground_source: TerminalDefaultColorSource::HostDefault,
            background_source: TerminalDefaultColorSource::HostDefault,
            cursor_source: TerminalDefaultColorSource::HostDefault,
            palette_overrides: Arc::new([false; 256]),
            configured: Arc::new(configured.clone()),
            bold_as_bright,
        }
    }

    pub fn effective_background(&self) -> Color {
        if self.reversed {
            self.foreground
        } else {
            self.background
        }
    }
}

pub type RowSnapshot = Arc<[CellSnapshot]>;

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct PresentationGeneration(u64);

impl PresentationGeneration {
    pub(crate) fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    #[cfg(test)]
    pub const fn test(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ActiveScreenSnapshot {
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
pub struct ScreenSizeSnapshot {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ViewportSnapshot {
    pub offset_rows: u64,
    pub visible_rows: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CursorShapeSnapshot {
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
pub struct CursorPositionSnapshot {
    pub column: u16,
    pub row: u16,
    pub width_cells: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CursorSnapshot {
    pub position: Option<CursorPositionSnapshot>,
    pub visible: bool,
    pub blinking: bool,
    pub password_input: bool,
    pub shape: CursorShapeSnapshot,
    pub color: Color,
    pub text_color: Color,
}

impl Default for CursorSnapshot {
    fn default() -> Self {
        let configured = TerminalColors::default();
        Self {
            position: None,
            visible: false,
            blinking: false,
            password_input: false,
            shape: CursorShapeSnapshot::default(),
            color: configured.cursor,
            text_color: configured.cursor_text.unwrap_or(configured.background),
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
pub enum ContentDamageSnapshot {
    #[default]
    Clean,
    Rows(Arc<[u16]>),
    Full,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SnapshotDamage {
    pub content: ContentDamageSnapshot,
    pub cursor: ContentDamageSnapshot,
    pub title: bool,
    pub metadata: bool,
    pub scrollbar: bool,
    pub viewport: bool,
    pub active_screen: bool,
    pub resize: bool,
    pub mouse_tracking: bool,
    pub selection_presence: bool,
    pub search: bool,
    pub graphics_content: bool,
    pub graphics_geometry: bool,
    pub appearance: bool,
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
            appearance: true,
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
pub struct ScrollbarSnapshot {
    pub total_rows: u64,
    pub offset_rows: u64,
    pub visible_rows: u64,
}

#[derive(Clone, Debug)]
pub struct ScreenSnapshot {
    pub generation: PresentationGeneration,
    pub appearance_generation: AppearanceGeneration,
    pub terminal_appearance: Appearance,
    pub configured_colors: Arc<TerminalColors>,
    pub bold_as_bright: bool,
    pub rows: Arc<[RowSnapshot]>,
    /// Soft-wrap markers corresponding one-to-one with the published viewport rows.
    pub row_soft_wrapped: Arc<[bool]>,
    pub background: Color,
    pub colors: TerminalColorsSnapshot,
    pub size: ScreenSizeSnapshot,
    pub cell_pixels: (u32, u32),
    pub viewport: ViewportSnapshot,
    pub scrollbar: ScrollbarSnapshot,
    pub active_screen: ActiveScreenSnapshot,
    pub cursor: CursorSnapshot,
    pub text_blinking: bool,
    pub mouse_tracking: bool,
    pub alternate_scroll: bool,
    pub selection_present: bool,
    pub selection_revision: u64,
    pub title: Arc<str>,
    pub metadata: Arc<TerminalMetadataSnapshot>,
    pub find: Option<Arc<TerminalFindSnapshot>>,
    pub graphics: GraphicsSnapshot,
    pub damage: SnapshotDamage,
    pub history: HistorySnapshot,
    /// The highest client Input serial written to the PTY before this state was captured.
    /// Zero before any, and in screens a client builds.
    pub input_serial: u32,
}

impl PartialEq for ScreenSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.rows == other.rows
            && self.row_soft_wrapped == other.row_soft_wrapped
            && self.generation == other.generation
            && self.appearance_generation == other.appearance_generation
            && self.terminal_appearance == other.terminal_appearance
            && self.configured_colors == other.configured_colors
            && self.bold_as_bright == other.bold_as_bright
            && self.background == other.background
            && self.colors == other.colors
            && self.size == other.size
            && self.cell_pixels == other.cell_pixels
            && self.viewport == other.viewport
            && self.scrollbar == other.scrollbar
            && self.active_screen == other.active_screen
            && self.cursor == other.cursor
            && self.text_blinking == other.text_blinking
            && self.mouse_tracking == other.mouse_tracking
            && self.alternate_scroll == other.alternate_scroll
            && self.selection_present == other.selection_present
            && self.selection_revision == other.selection_revision
            && self.title == other.title
            && self.metadata == other.metadata
            && self.find == other.find
            && self.graphics == other.graphics
            && self.damage == other.damage
            && self.history == other.history
            && self.input_serial == other.input_serial
    }
}

impl Eq for ScreenSnapshot {}

impl ScreenSnapshot {
    pub fn empty_with_appearance(
        paths: crate::local_path::LocalPathSemantics,
        initial_appearance: &TerminalAppearanceUpdate,
    ) -> Arc<Self> {
        let appearance = &initial_appearance.appearance;
        let colors =
            TerminalColorsSnapshot::configured(&appearance.colors, appearance.bold_as_bright);
        Arc::new(Self {
            generation: PresentationGeneration::default(),
            appearance_generation: initial_appearance.generation,
            terminal_appearance: appearance.appearance,
            configured_colors: Arc::new(appearance.colors.clone()),
            bold_as_bright: appearance.bold_as_bright,
            rows: Arc::from([]),
            row_soft_wrapped: Arc::from([]),
            background: appearance.colors.background,
            colors,
            size: ScreenSizeSnapshot::default(),
            cell_pixels: (0, 0),
            viewport: ViewportSnapshot::default(),
            scrollbar: ScrollbarSnapshot::default(),
            active_screen: ActiveScreenSnapshot::default(),
            cursor: CursorSnapshot {
                color: appearance.colors.cursor,
                text_color: appearance
                    .colors
                    .cursor_text
                    .unwrap_or(appearance.colors.background),
                ..CursorSnapshot::default()
            },
            text_blinking: false,
            mouse_tracking: false,
            alternate_scroll: false,
            selection_present: false,
            selection_revision: 0,
            title: Arc::from(""),
            metadata: MetadataTracker::new(paths, "", "", Default::default(), Instant::now())
                .snapshot(),
            find: None,
            graphics: GraphicsSnapshot::default(),
            damage: SnapshotDamage::initial(),
            history: HistorySnapshot::default(),
            input_serial: 0,
        })
    }

    /// Recolors retained content for rendering without claiming that the worker applied a newer
    /// appearance generation. Program-owned defaults and palette entries remain unchanged.
    pub fn projected_for_renderer(
        screen: &Arc<Self>,
        appearance: &ResolvedTerminalAppearance,
    ) -> Arc<Self> {
        if screen.terminal_appearance == appearance.appearance
            && screen.configured_colors.as_ref() == &appearance.colors
            && screen.bold_as_bright == appearance.bold_as_bright
        {
            return Arc::clone(screen);
        }
        screen.reprojected(appearance)
    }

    fn reprojected(&self, appearance: &ResolvedTerminalAppearance) -> Arc<Self> {
        let mut colors = self.colors.clone();
        if colors.foreground_source == TerminalDefaultColorSource::HostDefault {
            colors.foreground = appearance.colors.foreground;
        }
        if colors.background_source == TerminalDefaultColorSource::HostDefault {
            colors.background = appearance.colors.background;
        }
        let mut palette = *colors.palette;
        for index in 0..8 {
            if !colors.palette_overrides[index] {
                palette[index] = appearance.colors.normal[index];
            }
            if !colors.palette_overrides[index + 8] {
                palette[index + 8] = appearance.colors.bright[index];
            }
        }
        colors.palette = Arc::new(palette);
        colors.configured = Arc::new(appearance.colors.clone());
        colors.bold_as_bright = appearance.bold_as_bright;

        let mut cursor = self.cursor;
        if colors.cursor_source == TerminalDefaultColorSource::HostDefault {
            cursor.color = appearance.colors.cursor;
            cursor.text_color = appearance
                .colors
                .cursor_text
                .unwrap_or_else(|| colors.effective_background());
        } else {
            cursor.text_color = colors.effective_background();
        }
        let background = colors.effective_background();
        Arc::new(Self {
            terminal_appearance: appearance.appearance,
            configured_colors: Arc::clone(&colors.configured),
            bold_as_bright: appearance.bold_as_bright,
            background,
            colors,
            cursor,
            ..self.clone()
        })
    }

    #[cfg(test)]
    pub fn empty(paths: crate::local_path::LocalPathSemantics) -> Arc<Self> {
        Self::empty_with_appearance(paths, &crate::terminal::test_terminal_appearance_update())
    }

    #[cfg(test)]
    pub fn from_test_parts(
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
            selection_revision: 0,
            scrollbar,
            title: title.into(),
            ..Self::empty_value()
        })
    }

    #[cfg(test)]
    pub fn from_test_parts_at(
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
            selection_revision: 0,
            scrollbar,
            title: title.into(),
            ..Self::empty_value()
        })
    }

    #[cfg(test)]
    fn empty_value() -> Self {
        let configured = TerminalColors::default();
        Self {
            generation: PresentationGeneration::default(),
            appearance_generation: AppearanceGeneration::INITIAL,
            terminal_appearance: Appearance::Dark,
            configured_colors: Arc::new(TerminalColors::default()),
            bold_as_bright: true,
            rows: Arc::from([]),
            row_soft_wrapped: Arc::from([]),
            background: configured.background,
            colors: TerminalColorsSnapshot::configured(&configured, true),
            size: ScreenSizeSnapshot::default(),
            cell_pixels: (0, 0),
            viewport: ViewportSnapshot::default(),
            scrollbar: ScrollbarSnapshot::default(),
            active_screen: ActiveScreenSnapshot::default(),
            cursor: CursorSnapshot::default(),
            text_blinking: false,
            mouse_tracking: false,
            alternate_scroll: false,
            selection_present: false,
            selection_revision: 0,
            title: Arc::from(""),
            metadata: MetadataTracker::new(
                crate::local_path::LocalPathSemantics::Posix,
                "",
                "",
                Default::default(),
                Instant::now(),
            )
            .snapshot(),
            find: None,
            graphics: GraphicsSnapshot::default(),
            damage: SnapshotDamage::initial(),
            history: HistorySnapshot::default(),
            input_serial: 0,
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

/// Waits for a prompt Ghostty cleared on resize to be drawn again.
#[derive(Debug, Default)]
struct PromptRedraw {
    /// When a resize cleared the prompt, while the shell has not drawn it again.
    cleared_at: Option<Instant>,
    /// When the shell last wrote output since then.
    last_output: Option<Instant>,
}

impl PromptRedraw {
    /// Follows one OSC 133 mark. The end of the prompt, or a command, ends the wait.
    fn observe(&mut self, mark: &str) {
        if matches!(mark.split(';').next(), Some("B" | "I" | "C" | "D")) {
            self.release();
        }
    }

    fn cleared(&mut self, now: Instant) {
        self.cleared_at = Some(now);
        self.last_output = None;
    }

    fn note_output(&mut self, now: Instant) {
        if self.cleared_at.is_some() {
            self.last_output = Some(now);
        }
    }

    fn waiting(&self) -> bool {
        self.cleared_at.is_some()
    }

    /// When the wait ends without a prompt-end mark: once the shell's output pauses, as a shell
    /// that marks no prompt end has finished its redraw by then, and at the latest after
    /// [`PROMPT_REDRAW_HOLD`], so a silent shell or continuous output cannot hold the screen.
    fn deadline(&self) -> Option<Instant> {
        let latest = self.cleared_at? + PROMPT_REDRAW_HOLD;
        Some(
            self.last_output
                .map_or(latest, |output| (output + PROMPT_REDRAW_SETTLE).min(latest)),
        )
    }

    /// Ends the wait, and reports whether there was one.
    fn release(&mut self) -> bool {
        self.last_output = None;
        self.cleared_at.take().is_some()
    }
}

/// The semantic store: Ghostty's row observer and the model built from what it extracted.
struct SemanticStore {
    observer: ghostty_accessibility::State,
    model: TerminalAccessibilityState,
    /// The content change count when the capture in progress took its first step.
    capture_began_at: Option<u64>,
    /// Content changed while the capture completed last was running.
    capture_outran: bool,
}

pub(crate) struct TerminalEmulator {
    local_file_emissions: Rc<RefCell<LocalFileEmissionRegistry>>,
    terminal: Terminal<'static, 'static>,
    /// Present from the first semantic capture step until the store is released.
    semantics: Option<SemanticStore>,
    accessibility_capture_needed: bool,
    /// Counts the content changes a semantic capture can observe.
    semantic_changes: u64,
    accessibility_generation: PresentationGeneration,
    render_state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    keyboard_protocol: KeyboardProtocolEncoder,
    mouse_encoder: MouseEncoder<'static>,
    mouse_event: MouseEvent<'static>,
    cached_mouse_modes: Option<MouseModeState>,
    cached_mouse_size: Option<MouseEncoderSize>,
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
    cached_geometry: Option<TerminalGeometry>,
    cached_colors: Option<TerminalColorsSnapshot>,
    cached_cursor: Option<CursorSnapshot>,
    cached_scrollbar: Option<ScrollbarSnapshot>,
    cached_active_screen: Option<ActiveScreenSnapshot>,
    cached_mouse_tracking: Option<bool>,
    cached_alternate_scroll: Option<bool>,
    cached_selection_present: Option<bool>,
    selection_revision: u64,
    selection_copy_generation: u64,
    selection_copy_cache: RefCell<Option<(SelectionCopyOptions, Option<SelectionCopy>)>>,
    cached_selection_revision: Option<u64>,
    cached_metadata_revision: Option<u64>,
    cached_appearance_generation: Option<AppearanceGeneration>,
    geometry: TerminalGeometry,
    active_pointer: Option<ActivePointer>,
    application_pointer_generation: Option<u64>,
    presentation_generation: PresentationGeneration,
    /// The client Input serial the next snapshot reports.
    input_serial: u32,
    synchronized_output_last_activity: Option<Instant>,
    prompt_redraw: PromptRedraw,
    history: history::HistoryTracker,
    /// Owner-marked markers a pending run follows, with where each was observed.
    run_observations: Vec<spaceterm_server::pane::run::Observation>,
    integration_expected: bool,
    applied_appearance: TerminalAppearanceUpdate,
    reported_appearance: Rc<Cell<Appearance>>,
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
    generation: PresentationGeneration,
    position: SurfacePosition,
    modifiers: InputModifiers,
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

/// A keyboard movement through the Scrollback viewport, as the Scroll Commands request it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(feature = "desktop")]
pub enum ScrollbackMovement {
    PageUp,
    PageDown,
    Top,
    Bottom,
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
    #[cfg(test)]
    pub(crate) fn new(geometry: TerminalGeometry) -> Result<Self, Error> {
        Self::new_with_appearance(geometry, crate::terminal::test_terminal_appearance_update())
    }

    #[cfg(test)]
    pub(crate) fn new_with_appearance(
        geometry: TerminalGeometry,
        initial_appearance: TerminalAppearanceUpdate,
    ) -> Result<Self, Error> {
        Self::new_with_metadata_and_appearance(
            geometry,
            "",
            "",
            None,
            identity::TERM_FALLBACK,
            Instant::now(),
            initial_appearance,
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
        Self::new_with_metadata_and_appearance(
            geometry,
            initial_directory,
            fallback_title,
            local_hostname,
            terminal_name,
            epoch,
            crate::terminal::test_terminal_appearance_update(),
        )
    }

    #[cfg(test)]
    fn new_with_metadata_and_appearance(
        geometry: TerminalGeometry,
        initial_directory: &str,
        fallback_title: &str,
        local_hostname: Option<&str>,
        terminal_name: &'static str,
        epoch: Instant,
        initial_appearance: TerminalAppearanceUpdate,
    ) -> Result<Self, Error> {
        Self::new_with_metadata_context_and_appearance(
            geometry,
            TerminalMetadataContext::local(
                crate::local_path::LocalPathSemantics::Posix,
                initial_directory,
                crate::terminal::metadata::LocalMachine::new(None, local_hostname, None),
            ),
            fallback_title,
            terminal_name,
            epoch,
            initial_appearance,
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
        Self::new_with_metadata_context_and_appearance(
            geometry,
            metadata_context,
            fallback_title,
            terminal_name,
            epoch,
            crate::terminal::test_terminal_appearance_update(),
        )
    }

    #[cfg(test)]
    fn new_with_metadata_context_and_appearance(
        geometry: TerminalGeometry,
        metadata_context: TerminalMetadataContext,
        fallback_title: &str,
        terminal_name: &'static str,
        epoch: Instant,
        initial_appearance: TerminalAppearanceUpdate,
    ) -> Result<Self, Error> {
        Self::new_with_local_filesystem(
            geometry,
            metadata_context,
            fallback_title,
            terminal_name,
            epoch,
            LocalFilesystemAuthority::testing(),
            initial_appearance,
        )
    }

    pub(crate) fn new_with_local_filesystem(
        geometry: TerminalGeometry,
        metadata_context: TerminalMetadataContext,
        fallback_title: &str,
        terminal_name: &'static str,
        epoch: Instant,
        local_filesystem: LocalFilesystemAuthority,
        initial_appearance: TerminalAppearanceUpdate,
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
            max_scrollback_bytes: MAX_SCROLLBACK_BYTES,
        })?;

        terminal
            .set_kitty_image_storage_limit(0)?
            .set_kitty_image_from_file_allowed(false)?
            .set_kitty_image_from_temp_file_allowed(false)?
            .set_kitty_image_from_shared_mem_allowed(false)?
            .set_apc_max_bytes_kitty(Some(APC_TRANSMISSION_LIMIT))?;

        apply_terminal_appearance_defaults_transactional(
            &mut terminal,
            &initial_appearance.appearance,
        )?;
        terminal.resize(grid.cols, grid.rows, cell.width, cell.height)?;
        terminal.on_pty_write({
            let pty_responses = Rc::clone(&pty_responses);
            move |_, data| {
                // SpaceTerm answers XTGETTCAP with its bounded capability profile.
                // Native replies would duplicate those frames or expose a different profile.
                if data.starts_with(b"\x1bP1+r") || data.starts_with(b"\x1bP0+r") {
                    return;
                }
                pty_responses.borrow_mut().extend_from_slice(data);
            }
        })?;
        // Image clients need the engine's integer cell geometry. Deriving cells
        // by dividing the fractional PTY pixel extent can underestimate their size
        // and make Unicode-placeholder rows wrap beyond the terminal grid.
        terminal.on_size(|terminal| terminal.size_report().ok())?;
        let reported_appearance = Rc::new(Cell::new(initial_appearance.appearance.appearance));
        terminal.on_color_scheme({
            let reported_appearance = Rc::clone(&reported_appearance);
            move |_| Some(color_scheme(reported_appearance.get()))
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
            semantics: None,
            accessibility_capture_needed: true,
            semantic_changes: 0,
            accessibility_generation: PresentationGeneration::default(),
            render_state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            keyboard_protocol: KeyboardProtocolEncoder::new()?,
            mouse_encoder,
            mouse_event: MouseEvent::new()?,
            cached_mouse_modes: None,
            cached_mouse_size: None,
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
            cached_geometry: None,
            cached_colors: None,
            cached_cursor: None,
            cached_scrollbar: None,
            cached_active_screen: None,
            cached_mouse_tracking: None,
            cached_alternate_scroll: None,
            cached_selection_present: None,
            selection_revision: 0,
            selection_copy_generation: 1,
            selection_copy_cache: RefCell::new(None),
            cached_selection_revision: None,
            cached_metadata_revision: None,
            cached_appearance_generation: None,
            geometry,
            active_pointer: None,
            application_pointer_generation: None,
            presentation_generation: PresentationGeneration::default(),
            input_serial: 0,
            synchronized_output_last_activity: None,
            prompt_redraw: PromptRedraw::default(),
            history: history::HistoryTracker::default(),
            run_observations: Vec::new(),
            integration_expected: false,
            applied_appearance: initial_appearance,
            reported_appearance,
        })
    }

    pub(crate) fn restrict_client_effects(&mut self) -> Result<(), Error> {
        self.terminal.set_glyph_protocol_enabled(false)?;
        self.terminal.on_device_attributes(|_| {
            let mut attributes = identity::device_attributes();
            attributes.primary = libghostty_vt::terminal::PrimaryDeviceAttributes::new(
                libghostty_vt::terminal::ConformanceLevel::VT220,
                &[libghostty_vt::terminal::DeviceAttributeFeature::ANSI_COLOR],
            );
            Some(attributes)
        })?;
        Ok(())
    }

    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        self.feed_at(bytes, Instant::now());
    }

    pub(crate) fn feed_at(&mut self, bytes: &[u8], now: Instant) {
        if !bytes.is_empty() {
            self.invalidate_selection_copy();
            self.note_semantic_change();
        }
        self.xtgettcap
            .feed(bytes, &mut self.pty_responses.borrow_mut());
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
                    self.metadata.set_reported_title(&title, now);
                }
                MetadataEvent::Directory(directory) => {
                    self.metadata.set_reported_directory(&directory);
                }
                MetadataEvent::SemanticPrompt(value) => {
                    self.prompt_redraw.observe(&value);
                    let was_running =
                        self.metadata
                            .snapshot()
                            .command
                            .as_ref()
                            .is_some_and(|command| {
                                command.state == crate::terminal::metadata::CommandState::Running
                            });
                    self.metadata.apply_semantic_prompt(&value, now);
                    // One read may contain several commands, or a completion followed by a new
                    // command. Publish each accepted transition before the next event replaces it.
                    if was_running
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
                }
                MetadataEvent::Progress { state, value } => {
                    self.metadata.apply_progress_report(state, value, now);
                }
            }
        }
        self.observe_run_markers();
        if !bytes.is_empty() {
            self.prompt_redraw.note_output(now);
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

    pub(crate) fn clear_screen_and_scrollback(&mut self) -> EmulatorAction {
        self.invalidate_selection_copy();
        self.note_semantic_change();
        let redraw_prompt = self.terminal.clear_screen(true);
        EmulatorAction {
            bytes: if redraw_prompt {
                vec![0x0c]
            } else {
                Vec::new()
            },
            screen_changed: true,
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

    /// When held output is presented even if its producer has not finished: a synchronized output
    /// transaction, or a prompt cleared by a resize that the shell has not drawn again.
    pub(crate) fn synchronized_output_deadline(&self) -> Option<Instant> {
        let synchronized = self
            .synchronized_output_last_activity
            .map(|last_activity| last_activity + MAX_SYNCHRONIZED_OUTPUT_DURATION);
        synchronized
            .into_iter()
            .chain(self.prompt_redraw.deadline())
            .min()
    }

    #[cfg(test)]
    pub(crate) fn presentation_generation(&self) -> PresentationGeneration {
        self.presentation_generation
    }

    pub(crate) fn expire_synchronized_output(&mut self, now: Instant) -> Result<bool, Error> {
        let prompt_released = self
            .prompt_redraw
            .deadline()
            .is_some_and(|deadline| now >= deadline)
            && self.prompt_redraw.release();
        let synchronized_released = if self
            .synchronized_output_last_activity
            .is_some_and(|activity| now >= activity + MAX_SYNCHRONIZED_OUTPUT_DURATION)
        {
            self.end_explicit_synchronized_output()?
        } else {
            false
        };
        Ok(prompt_released || synchronized_released)
    }

    pub(crate) fn end_synchronized_output(&mut self) -> Result<bool, Error> {
        let prompt_redraw = self.prompt_redraw.release();
        Ok(self.end_explicit_synchronized_output()? || prompt_redraw)
    }

    fn end_explicit_synchronized_output(&mut self) -> Result<bool, Error> {
        self.synchronized_output_last_activity = None;
        if !self.terminal.mode(Mode::SYNC_OUTPUT)? {
            return Ok(false);
        }
        self.terminal.set_mode(Mode::SYNC_OUTPUT, false)?;
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) fn resize(&mut self, geometry: TerminalGeometry) -> Result<(), Error> {
        self.resize_for_pty(geometry, true)
    }

    pub(crate) fn geometry(&self) -> TerminalGeometry {
        self.geometry
    }

    pub(crate) fn resize_for_pty(
        &mut self,
        geometry: TerminalGeometry,
        winsize_changed: bool,
    ) -> Result<(), Error> {
        self.resize_for_pty_at(geometry, winsize_changed, Instant::now())
    }

    pub(crate) fn resize_for_pty_at(
        &mut self,
        geometry: TerminalGeometry,
        winsize_changed: bool,
        now: Instant,
    ) -> Result<(), Error> {
        if self.application_pointer_generation.is_some() && geometry == self.geometry {
            return Ok(());
        }
        self.end_explicit_synchronized_output()?;
        self.invalidate_selection_copy();
        self.active_pointer = None;
        let grid = geometry.grid();
        let cell = geometry.backing_cell_size();
        if winsize_changed {
            self.capture_accessibility_if_needed()?;
            self.note_semantic_change();
            let clears_prompt = self.terminal.resize_with_prompt_redraw(
                grid.cols,
                grid.rows,
                cell.width,
                cell.height,
            )?;
            self.capture_accessibility_if_needed()?;
            if clears_prompt {
                self.prompt_redraw.cleared(now);
            }
        } else {
            debug_assert_eq!(grid, self.geometry.grid());
            debug_assert_eq!(cell, self.geometry.backing_cell_size());
        }
        self.geometry = geometry;
        Ok(())
    }

    pub(crate) fn take_pty_responses(&self) -> Vec<u8> {
        mem::take(&mut *self.pty_responses.borrow_mut())
    }

    pub(crate) fn apply_appearance(
        &mut self,
        update: TerminalAppearanceUpdate,
    ) -> Result<(), Error> {
        if update.generation <= self.applied_appearance.generation {
            return Ok(());
        }

        let appearance_changed =
            update.appearance.appearance != self.applied_appearance.appearance.appearance;
        let report = if appearance_changed && self.terminal.mode(Mode::COLOR_SCHEME_REPORT)? {
            let mut bytes = [0; 32];
            let length = color_scheme(update.appearance.appearance).encode_report(&mut bytes)?;
            Some(bytes[..length].to_vec())
        } else {
            None
        };

        apply_terminal_appearance_defaults_transactional(&mut self.terminal, &update.appearance)?;
        self.reported_appearance.set(update.appearance.appearance);
        self.applied_appearance = update;
        self.invalidate_selection_copy();
        if let Some(report) = report {
            self.pty_responses.borrow_mut().extend_from_slice(&report);
        }
        Ok(())
    }

    pub(crate) fn take_attention_events(&self) -> Vec<AttentionEvent> {
        mem::take(&mut *self.pending_attention.borrow_mut())
    }

    pub(crate) fn key(&mut self, input: KeyInput) -> Result<EmulatorAction, TerminalFailure> {
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

    pub(crate) fn focus_reporting_enabled(&self) -> Result<bool, TerminalFailure> {
        self.terminal.mode(Mode::FOCUS_EVENT).map_err(|error| {
            TerminalFailure::emulator_error("query-terminal-focus-reporting-mode", error)
        })
    }

    pub(crate) fn focus(&self, focused: bool) -> Result<EmulatorAction, TerminalFailure> {
        if !self.focus_reporting_enabled()? {
            return Ok(EmulatorAction::none());
        }

        let event = if focused {
            FocusEvent::Gained
        } else {
            FocusEvent::Lost
        };
        let mut buffer = [0_u8; 8];
        let written = event.encode(&mut buffer).map_err(|error| {
            TerminalFailure::emulator_error("encode-terminal-focus-event", error)
        })?;
        Ok(EmulatorAction::bytes(buffer[..written].to_vec()))
    }

    pub(crate) fn application_pointer(
        &mut self,
        input: PointerInput,
        snapshot: u64,
        current_snapshot: u64,
    ) -> Result<EmulatorAction, TerminalFailure> {
        let active = self.active_pointer;
        if active.is_none() {
            self.application_pointer_generation = None;
        }
        if active.is_some() && input.phase == PointerPhase::Press {
            return Ok(EmulatorAction::none());
        }
        let continuing = active.is_some() && input.phase != PointerPhase::Press;
        let eligible = if continuing {
            self.application_pointer_generation
                .is_some_and(|start| (start..=current_snapshot).contains(&snapshot))
        } else {
            snapshot != 0 && snapshot == current_snapshot
        };
        if !eligible {
            return if continuing {
                self.cancel_application_pointer()
            } else {
                Ok(EmulatorAction::none())
            };
        }
        if active.is_none()
            && (input.phase == PointerPhase::Release
                || (input.phase == PointerPhase::Motion && input.button.is_some()))
        {
            return Ok(EmulatorAction::none());
        }
        if input.phase == PointerPhase::Release
            && active.is_some_and(|active| input.button != Some(active.button))
        {
            return self.cancel_application_pointer();
        }
        if !self.terminal.is_mouse_tracking().map_err(|error| {
            TerminalFailure::emulator_error("query-terminal-mouse-tracking-mode", error)
        })? || shift_overrides_application_mouse(input.modifiers, input.shift_selection)
            || self.terminal.mode(Mode::SYNC_OUTPUT).map_err(|error| {
                TerminalFailure::emulator_error("query-synchronized-output-mode", error)
            })?
        {
            return self.cancel_application_pointer();
        }
        let action = self.pointer(input)?;
        if self.active_pointer.is_some() && input.phase == PointerPhase::Press {
            self.application_pointer_generation = Some(snapshot);
        } else if self.active_pointer.is_none() {
            self.application_pointer_generation = None;
        }
        Ok(action)
    }

    pub(crate) fn cancel_application_pointer(&mut self) -> Result<EmulatorAction, TerminalFailure> {
        if self.application_pointer_generation.is_none() {
            return Ok(EmulatorAction::none());
        }
        self.cancel_pointer_drag()
    }

    pub(crate) fn application_wheel(
        &mut self,
        input: WheelInput,
    ) -> Result<EmulatorAction, TerminalFailure> {
        let tracking = self.terminal.is_mouse_tracking().map_err(|error| {
            TerminalFailure::emulator_error("query-terminal-mouse-tracking-mode", error)
        })?;
        if tracking && shift_overrides_application_mouse(input.modifiers, input.shift_selection) {
            return Ok(EmulatorAction::none());
        }
        let alternate_scroll = self.terminal.active_screen().map_err(|error| {
            TerminalFailure::emulator_error("query-active-terminal-screen", error)
        })? == Screen::Alternate
            && self.terminal.mode(Mode::ALT_SCROLL).map_err(|error| {
                TerminalFailure::emulator_error("query-alternate-scroll-mode", error)
            })?;
        if !tracking && !alternate_scroll {
            return Ok(EmulatorAction::none());
        }
        self.wheel(input)
    }

    pub(crate) fn pointer(
        &mut self,
        input: PointerInput,
    ) -> Result<EmulatorAction, TerminalFailure> {
        if self.terminal.mode(Mode::SYNC_OUTPUT).map_err(|error| {
            TerminalFailure::emulator_error("query-synchronized-output-mode", error)
        })? || !self.accept_pointer_generation(input.generation)
        {
            self.active_pointer = None;
            return Ok(EmulatorAction::none());
        }
        match input.phase {
            PointerPhase::Press => self.pointer_press(input),
            PointerPhase::Motion => self.pointer_motion(input),
            PointerPhase::Release => self.pointer_release(input),
        }
    }

    pub(crate) fn wheel(&mut self, input: WheelInput) -> Result<EmulatorAction, TerminalFailure> {
        if input.generation != self.presentation_generation
            || self.terminal.mode(Mode::SYNC_OUTPUT).map_err(|error| {
                TerminalFailure::emulator_error("query-synchronized-output-mode", error)
            })?
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

        let tracking = self.terminal.is_mouse_tracking().map_err(|error| {
            TerminalFailure::emulator_error("query-terminal-mouse-tracking-mode", error)
        })?;
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

        let alternate_screen = self.terminal.active_screen().map_err(|error| {
            TerminalFailure::emulator_error("query-the-active-terminal-screen", error)
        })? == Screen::Alternate;
        let alternate_scroll = self.terminal.mode(Mode::ALT_SCROLL).map_err(|error| {
            TerminalFailure::emulator_error("query-alternate-scroll-mode", error)
        })?;
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

        // Without mouse tracking or alternate scroll the client scrolls its own view.
        Ok(EmulatorAction::none())
    }

    pub(crate) fn paste(&mut self, text: String) -> Result<EmulatorAction, TerminalFailure> {
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
                Err(error) => {
                    return Err(TerminalFailure::emulator_error(
                        "encode-terminal-paste",
                        error,
                    ));
                }
            }
        };
        bytes.truncate(written);
        self.terminal.scroll_viewport(ScrollViewport::Bottom);

        Ok(EmulatorAction {
            bytes,
            screen_changed: true,
        })
    }

    pub(crate) fn bracketed_paste_mode(&self) -> Result<bool, TerminalFailure> {
        self.terminal
            .mode(Mode::BRACKETED_PASTE)
            .map_err(|error| TerminalFailure::emulator_error("query-bracketed-paste-mode", error))
    }

    #[cfg(test)]
    pub(crate) fn selection_text(&self) -> Result<Option<String>, TerminalFailure> {
        self.format_selection(Format::Plain, SelectionCopyOptions::default())
    }

    /// Selects from `start` to `end`, each a visible `(line, column)`, through the accessible
    /// Selection a server client sends. Returns whether the Selection changed.
    #[cfg(test)]
    pub(crate) fn select_visible(&mut self, start: (usize, u16), end: (usize, u16)) -> bool {
        let model = (0..128)
            .find_map(
                |_| match self.accessibility_snapshot_for_current_presentation() {
                    Ok((model, false)) => Some(model),
                    _ => None,
                },
            )
            .flatten()
            .expect("bounded accessibility publication produces the current model");
        let reference = |(line, column): (usize, u16)| {
            model.row(line).map(|row| AccessibilityCellRef {
                row: row.id,
                row_revision: row.revision,
                column,
            })
        };
        let (Some(start), Some(end)) = (reference(start), reference(end)) else {
            return false;
        };
        self.set_accessibility_selection_refs(Some(AccessibilitySelectionRefs {
            start,
            end,
            rectangle: false,
        }))
        .unwrap_or(false)
    }

    fn invalidate_selection_copy(&mut self) {
        self.selection_copy_cache.get_mut().take();
        if self.selection_copy_generation != 0 {
            self.selection_copy_generation =
                self.selection_copy_generation.checked_add(1).unwrap_or(0);
        }
    }

    pub(crate) fn selection_copy(
        &self,
        options: SelectionCopyOptions,
    ) -> Result<Option<SelectionCopy>, TerminalFailure> {
        if let Some((saved_options, copy)) = self.selection_copy_cache.borrow().as_ref()
            && *saved_options == options
        {
            return Ok(copy.clone());
        }
        let copy = if let Some(plain_text) = self.format_selection(Format::Plain, options)? {
            let html = if options.include_html {
                self.format_selection(Format::Html, options)?
            } else {
                None
            };
            Some(SelectionCopy {
                plain_text: plain_text.into(),
                html: html.map(Into::into),
            })
        } else {
            None
        };
        if self.selection_copy_generation != 0 {
            *self.selection_copy_cache.borrow_mut() = Some((options, copy.clone()));
        }
        Ok(copy)
    }

    fn format_selection(
        &self,
        format: Format,
        options: SelectionCopyOptions,
    ) -> Result<Option<String>, TerminalFailure> {
        let options = FormatOptions::new()
            .with_emit_format(format)
            .with_unwrap(options.unwrap_soft_wraps)
            .with_trim(options.trailing_spaces == TrailingSpacePolicy::Trim);
        let Some(bytes) = self
            .terminal
            .format_selection_alloc(None, options)
            .map_err(|error| TerminalFailure::emulator_error("format-terminal-selection", error))?
        else {
            return Ok(None);
        };

        String::from_utf8(bytes.as_ref().to_vec())
            .map(Some)
            .map_err(|_| {
                TerminalFailure::emulator("decode-terminal-selection")
                    .with_reason(FailureReason::InvalidUtf8)
            })
    }

    fn accept_pointer_generation(&mut self, generation: PresentationGeneration) -> bool {
        let Some(active) = self.active_pointer.as_mut() else {
            return generation == self.presentation_generation;
        };
        if generation < active.generation || generation > self.presentation_generation {
            return false;
        }
        active.generation = generation;
        true
    }

    fn pointer_press(&mut self, input: PointerInput) -> Result<EmulatorAction, TerminalFailure> {
        if self.active_pointer.is_some() {
            return Ok(EmulatorAction::none());
        }

        let tracking = self.terminal.is_mouse_tracking().map_err(|error| {
            TerminalFailure::emulator_error("query-terminal-mouse-tracking-mode", error)
        })?;
        let Some(button) = input.button else {
            return Ok(EmulatorAction::none());
        };
        // Selection belongs to the client, so a program that does not track the mouse gets no
        // press, and the Shift override stays with the client too.
        if !tracking || shift_overrides_application_mouse(input.modifiers, input.shift_selection) {
            self.active_pointer = None;
            return Ok(EmulatorAction::none());
        }
        self.active_pointer = Some(ActivePointer {
            button,
            generation: input.generation,
            position: input.position,
            modifiers: input.modifiers,
        });
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

    fn pointer_motion(&mut self, input: PointerInput) -> Result<EmulatorAction, TerminalFailure> {
        if let Some(active) = self.active_pointer.as_mut() {
            active.position = input.position;
            active.modifiers = input.modifiers;
        }
        match self.active_pointer {
            Some(ActivePointer { button, .. }) => {
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
            None => {
                let tracking = self.terminal.is_mouse_tracking().map_err(|error| {
                    TerminalFailure::emulator_error("query-terminal-mouse-tracking-mode", error)
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

    fn pointer_release(&mut self, input: PointerInput) -> Result<EmulatorAction, TerminalFailure> {
        let Some(active) = self.active_pointer else {
            return Ok(EmulatorAction::none());
        };
        if input.button != Some(active.button) {
            return Ok(EmulatorAction::none());
        }
        self.active_pointer = None;
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

    pub(super) fn enhanced_keyboard_active(&self) -> bool {
        self.terminal
            .kitty_keyboard_flags()
            .is_ok_and(|flags| !flags.is_empty())
    }

    fn encode_key(&mut self, input: &KeyInput, bytes: &mut Vec<u8>) -> Result<(), TerminalFailure> {
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
    ) -> Result<(), TerminalFailure> {
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
            .map_err(|error| TerminalFailure::emulator_error("encode-terminal-mouse-event", error))
    }

    fn clear_selection(&mut self) -> Result<(), TerminalFailure> {
        self.invalidate_selection_copy();
        self.terminal
            .set_selection(None)
            .map_err(|error| TerminalFailure::emulator_error("clear-terminal-selection", error))?;
        Ok(())
    }

    /// Applies Selection endpoints named by a server client's accessible snapshot.
    ///
    /// Returns `false` without mutation when the endpoints disagree on their screen, synchronized
    /// output is active, or either endpoint no longer names a live row revision.
    pub(crate) fn set_accessibility_selection_refs(
        &mut self,
        selection: Option<AccessibilitySelectionRefs>,
    ) -> Result<bool, TerminalFailure> {
        if selection.is_some_and(|selection| {
            selection.start.row.screen != selection.end.row.screen
                || selection.start.row.screen_generation != selection.end.row.screen_generation
        }) {
            return Ok(false);
        }
        if self.terminal.mode(Mode::SYNC_OUTPUT).map_err(|error| {
            TerminalFailure::emulator_error("query-synchronized-output-mode", error)
        })? {
            return Ok(false);
        }
        self.apply_accessibility_selection(
            selection.map(|selection| (selection.start, selection.end)),
        )
    }

    fn apply_accessibility_selection(
        &mut self,
        selection: Option<(AccessibilityCellRef, AccessibilityCellRef)>,
    ) -> Result<bool, TerminalFailure> {
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
        // Without a store no client holds a snapshot, so every endpoint is stale.
        let Some(semantics) = self.semantics.as_mut() else {
            return Ok(false);
        };
        let changed = semantics
            .observer
            .set_selection(&self.terminal, selection)
            .map_err(|error| {
                TerminalFailure::emulator_error("set-terminal-accessibility-selection", error)
            })?;
        if changed {
            self.invalidate_selection_copy();
            self.active_pointer = None;
        }
        Ok(changed)
    }
    pub(crate) fn cancel_pointer_drag(&mut self) -> Result<EmulatorAction, TerminalFailure> {
        self.application_pointer_generation = None;
        let Some(active) = self.active_pointer.take() else {
            return Ok(EmulatorAction::none());
        };
        let mut bytes = Vec::new();
        self.encode_mouse_event(
            MouseAction::Release,
            Some(mouse_button(active.button)),
            active.position,
            active.modifiers,
            false,
            &mut bytes,
        )?;
        Ok(EmulatorAction::bytes(bytes))
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

    fn mouse_mode_state(&self) -> Result<MouseModeState, TerminalFailure> {
        let mode = |mode| {
            self.terminal.mode(mode).map_err(|error| {
                TerminalFailure::emulator_error("query-terminal-mouse-encoder-mode", error)
            })
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
    pub(super) fn accessibility_content_changed(&self) -> bool {
        self.accessibility_capture_needed
    }

    fn note_semantic_change(&mut self) {
        self.accessibility_capture_needed = true;
        self.semantic_changes = self.semantic_changes.wrapping_add(1);
    }

    /// Whether content changed while the capture that completed last was running, and clears
    /// it. A capture finishes against what it observed when it began, so those changes need the
    /// next capture.
    pub(crate) fn take_accessibility_capture_outran(&mut self) -> bool {
        self.semantics
            .as_mut()
            .is_some_and(|semantics| std::mem::take(&mut semantics.capture_outran))
    }

    /// Retains Ghostty's dirty rows for the semantic store before rendering clears them. Without
    /// a store there is nothing to retain: the first capture of a new store extracts every row.
    fn capture_accessibility_if_needed(&mut self) -> Result<(), Error> {
        if self.accessibility_capture_needed {
            if let Some(semantics) = self.semantics.as_mut() {
                semantics.observer.capture(&self.terminal)?;
            }
            self.accessibility_capture_needed = false;
        }
        Ok(())
    }

    pub(crate) fn accessibility_snapshot(
        &mut self,
        bind_next_presentation: bool,
    ) -> Result<(Option<Arc<TerminalAccessibilityModel>>, bool), TerminalFailure> {
        if self.terminal.mode(Mode::SYNC_OUTPUT).map_err(|error| {
            TerminalFailure::emulator_error("query-synchronized-output-mode", error)
        })? {
            return Ok((None, false));
        }
        if bind_next_presentation {
            self.accessibility_generation = self.presentation_generation.next();
        }
        let semantics = match &mut self.semantics {
            Some(semantics) => semantics,
            store => store.insert(SemanticStore {
                observer: ghostty_accessibility::State::new().map_err(|error| {
                    TerminalFailure::emulator_error("observe-retained-terminal-text", error)
                })?,
                model: TerminalAccessibilityState::default(),
                capture_began_at: None,
                capture_outran: false,
            }),
        };
        let began_at = *semantics
            .capture_began_at
            .get_or_insert(self.semantic_changes);
        let snapshot = semantics
            .observer
            .update(
                &self.terminal,
                ghostty_accessibility::UpdateOptions {
                    max_cells: 16_384,
                    max_rows: 256,
                },
            )
            .map_err(|error| {
                TerminalFailure::emulator_error("observe-retained-terminal-text", error)
            })?;
        self.accessibility_capture_needed = false;
        let more = snapshot.more;
        if !more {
            semantics.capture_began_at = None;
            semantics.capture_outran = began_at != self.semantic_changes;
        }
        let update = accessibility_update(snapshot)?;
        let model = semantics.model.apply(update, self.accessibility_generation);
        Ok((model, more))
    }

    /// Drops the semantic store, in Ghostty and here. The next snapshot creates a new one and
    /// extracts the whole terminal again.
    pub(crate) fn release_accessibility(&mut self) {
        self.semantics = None;
    }

    /// Whether the emulator holds a semantic store.
    #[cfg(test)]
    pub(crate) fn retains_semantics(&self) -> bool {
        self.semantics.is_some()
    }

    pub(crate) fn reset_accessibility_presentation(&mut self) {
        if let Some(semantics) = self.semantics.as_mut() {
            semantics.model.reset_presentation();
        }
    }

    pub(crate) fn accessibility_snapshot_for_current_presentation(
        &mut self,
    ) -> Result<(Option<Arc<TerminalAccessibilityModel>>, bool), TerminalFailure> {
        if self.terminal.mode(Mode::SYNC_OUTPUT).map_err(|error| {
            TerminalFailure::emulator_error("query-synchronized-output-mode", error)
        })? {
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
        if self.terminal.mode(Mode::SYNC_OUTPUT)? || self.prompt_redraw.waiting() {
            return Ok(None);
        }

        self.advance_graphics_animations(now)?;

        let metadata = self.metadata.snapshot();
        let title = Arc::clone(&metadata.title.value);
        let title_changed = title != self.title;
        // Rendering consumes Ghostty's dirty flags. Retain semantic invalidation
        // now; the worker still extracts text only when accessibility is due.
        // A semantic update may already have observed this content before rendering.
        self.capture_accessibility_if_needed()?;
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

        let palette_overrides = self.terminal.color_palette_overrides()?;
        let palette = colors.palette.map(Color::from);
        let palette_overrides =
            std::array::from_fn(|index| palette_overrides.is_set(PaletteIndex(index as u8)));
        let cached_colors = self.cached_colors.as_ref();
        let configured_colors = cached_colors
            .filter(|cached| {
                cached.configured.as_ref() == &self.applied_appearance.appearance.colors
            })
            .map(|cached| Arc::clone(&cached.configured))
            .unwrap_or_else(|| Arc::new(self.applied_appearance.appearance.colors.clone()));
        let terminal_colors = TerminalColorsSnapshot {
            foreground: colors.foreground.into(),
            background: colors.background.into(),
            palette: cached_colors
                .filter(|cached| cached.palette.as_ref() == &palette)
                .map(|cached| Arc::clone(&cached.palette))
                .unwrap_or_else(|| Arc::new(palette)),
            reversed: self.terminal.mode(Mode::REVERSE_COLORS)?,
            foreground_source: color_source(self.terminal.fg_color_overridden()?),
            background_source: color_source(self.terminal.bg_color_overridden()?),
            cursor_source: color_source(self.terminal.cursor_color_overridden()?),
            palette_overrides: cached_colors
                .filter(|cached| cached.palette_overrides.as_ref() == &palette_overrides)
                .map(|cached| Arc::clone(&cached.palette_overrides))
                .unwrap_or_else(|| Arc::new(palette_overrides)),
            configured: Arc::clone(&configured_colors),
            bold_as_bright: self.applied_appearance.appearance.bold_as_bright,
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
            text_color: if terminal_colors.cursor_source == TerminalDefaultColorSource::HostDefault
            {
                terminal_colors
                    .configured
                    .cursor_text
                    .unwrap_or_else(|| terminal_colors.effective_background())
            } else {
                terminal_colors.effective_background()
            },
        };
        let mut cursor = build_cursor(row_cache);
        let rebuild_all = matches!(dirty, Dirty::Full)
            || row_cache.len() != usize::from(rows)
            || self.cached_cols != cols
            || self.cached_colors.as_ref() != Some(&terminal_colors);
        let previous_scrollbar = self.cached_scrollbar;
        let alternate_scroll = self.terminal.mode(Mode::ALT_SCROLL)?;
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
                resize: self.cached_cols != cols
                    || self.cached_rows != rows
                    || self.cached_geometry != Some(self.geometry),
                mouse_tracking: self.cached_mouse_tracking != Some(mouse_tracking)
                    || self.cached_alternate_scroll != Some(alternate_scroll),
                selection_presence: self.cached_selection_present != Some(selection_present)
                    || self.cached_selection_revision != Some(self.selection_revision),
                graphics_content: graphics_content_changed,
                graphics_geometry: graphics_geometry_changed,
                appearance: self.cached_appearance_generation
                    != Some(self.applied_appearance.generation),
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
                    let mut rendered_cells: Vec<CellSnapshot> =
                        Vec::with_capacity(usize::from(cols));
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
                        let mut selected = selection.is_some_and(|range| {
                            column_index >= range.start_x && column_index <= range.end_x
                        });
                        if spacer_tail && let Some(head) = rendered_cells.last_mut() {
                            // Copying includes the complete wide character even when a
                            // gesture endpoint falls on its spacer. Paint the same range.
                            selected |= head.selected;
                            head.selected = selected;
                        }
                        let mut text = String::with_capacity(4);
                        if !spacer_tail {
                            cell.graphemes_utf8(&mut text)?;
                        }
                        if text.is_empty() {
                            text.push(' ');
                        }
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
                            selected,
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
        self.cached_geometry = Some(self.geometry);
        self.cached_colors = Some(terminal_colors.clone());
        self.cached_cursor = Some(cursor);
        self.cached_scrollbar = Some(scrollbar);
        self.cached_active_screen = Some(active_screen);
        self.cached_mouse_tracking = Some(mouse_tracking);
        self.cached_alternate_scroll = Some(alternate_scroll);
        self.cached_selection_present = Some(selection_present);
        self.cached_selection_revision = Some(self.selection_revision);
        self.cached_metadata_revision = Some(metadata.revision);
        self.cached_appearance_generation = Some(self.applied_appearance.generation);
        self.title = Arc::clone(&title);

        self.presentation_generation = self.presentation_generation.next();
        let text_blinking = rows_have_visible_blinking_text(row_cache);
        let history = self.history.update(&mut self.terminal)?;
        let screen = Arc::new(ScreenSnapshot {
            generation: self.presentation_generation,
            appearance_generation: self.applied_appearance.generation,
            terminal_appearance: self.applied_appearance.appearance.appearance,
            configured_colors,
            bold_as_bright: self.applied_appearance.appearance.bold_as_bright,
            rows: Arc::from(row_cache.clone()),
            row_soft_wrapped: Arc::from(row_soft_wrapped),
            background: terminal_colors.effective_background(),
            colors: terminal_colors,
            size,
            cell_pixels: (
                self.geometry.backing_cell_size().width,
                self.geometry.backing_cell_size().height,
            ),
            viewport,
            scrollbar,
            active_screen,
            cursor,
            text_blinking,
            mouse_tracking,
            alternate_scroll,
            selection_present,
            selection_revision: self.selection_revision,
            title,
            metadata,
            find: None,
            graphics,
            damage,
            history,
            input_serial: self.input_serial,
        });
        Ok(Some(screen))
    }

    /// Sets the client Input serial that snapshots report from now on. A new serial alone does
    /// not make a snapshot.
    pub(crate) fn set_input_serial(&mut self, serial: u32) {
        self.input_serial = serial;
    }

    pub(crate) fn mark_metadata_stale(&mut self) {
        self.metadata.mark_stale();
    }

    pub(crate) fn metadata_status_deadline(&self) -> Option<Instant> {
        self.metadata.status_deadline()
    }

    pub(crate) fn advance_metadata_status(&mut self, now: Instant) -> bool {
        self.metadata.advance_status(now)
    }

    pub(crate) fn metadata(&self) -> Arc<TerminalMetadataSnapshot> {
        self.metadata.snapshot()
    }
}

fn accessibility_update(
    snapshot: ghostty_accessibility::Snapshot,
) -> Result<AccessibilityUpdate, TerminalFailure> {
    let screen = match snapshot.screen {
        ghostty_accessibility::Screen::Primary => AccessibilityScreen::Primary,
        ghostty_accessibility::Screen::Alternate => AccessibilityScreen::Alternate,
    };
    let screen_generation = usize::try_from(snapshot.screen_generation).map_err(|_| {
        TerminalFailure::emulator("observe-accessibility-generation")
            .with_reason(FailureReason::CapacityExceeded)
    })?;
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

fn apply_terminal_appearance_defaults(
    terminal: &mut Terminal<'static, 'static>,
    appearance: &ResolvedTerminalAppearance,
) -> Result<(), Error> {
    terminal.set_default_fg_color(Some(ghostty_color(appearance.colors.foreground)))?;
    terminal.set_default_bg_color(Some(ghostty_color(appearance.colors.background)))?;
    terminal.set_default_cursor_color(Some(ghostty_color(appearance.colors.cursor)))?;
    let mut palette = terminal.default_color_palette()?;
    for (index, color) in ANSI_NORMAL_INDICES
        .into_iter()
        .zip(appearance.colors.normal)
        .chain(
            ANSI_BRIGHT_INDICES
                .into_iter()
                .zip(appearance.colors.bright),
        )
    {
        palette.set(index, ghostty_color(color));
    }
    terminal.set_default_color_palette(Some(palette))?;
    Ok(())
}

fn apply_terminal_appearance_defaults_transactional(
    terminal: &mut Terminal<'static, 'static>,
    appearance: &ResolvedTerminalAppearance,
) -> Result<(), Error> {
    let foreground = terminal.default_fg_color()?;
    let background = terminal.default_bg_color()?;
    let cursor = terminal.default_cursor_color()?;
    let palette = terminal.default_color_palette()?;

    let Err(apply_error) = apply_terminal_appearance_defaults(terminal, appearance) else {
        return Ok(());
    };

    let mut rollback_error = None;
    if let Err(error) = terminal.set_default_fg_color(foreground) {
        rollback_error = Some(error);
    }
    if let Err(error) = terminal.set_default_bg_color(background)
        && rollback_error.is_none()
    {
        rollback_error = Some(error);
    }
    if let Err(error) = terminal.set_default_cursor_color(cursor)
        && rollback_error.is_none()
    {
        rollback_error = Some(error);
    }
    if let Err(error) = terminal.set_default_color_palette(Some(palette))
        && rollback_error.is_none()
    {
        rollback_error = Some(error);
    }
    Err(rollback_error.unwrap_or(apply_error))
}

fn color_scheme(appearance: Appearance) -> ColorScheme {
    match appearance {
        Appearance::Light => ColorScheme::Light,
        Appearance::Dark => ColorScheme::Dark,
    }
}

fn color_source(overridden: bool) -> TerminalDefaultColorSource {
    if overridden {
        TerminalDefaultColorSource::ProgramOverride
    } else {
        TerminalDefaultColorSource::HostDefault
    }
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
