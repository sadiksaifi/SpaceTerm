use crate::appearance::{
    Appearance, AppearanceGeneration, AppearanceUpdate, Color, TerminalAppearance, TerminalColors,
};
use crate::find::TerminalFindSnapshot;
use crate::graphics::GraphicsSnapshot;
use crate::metadata::{MetadataTracker, TerminalMetadataSnapshot};
#[cfg(feature = "engine")]
use libghostty_vt::render::CursorVisualStyle;
#[cfg(feature = "engine")]
use libghostty_vt::screen::{CellSemanticContent, Screen};
#[cfg(feature = "engine")]
use libghostty_vt::style::{PaletteIndex, RgbColor, StyleColor, Underline};
use std::sync::Arc;
use std::time::Instant;
#[cfg(feature = "engine")]
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
    pub hyperlink: Option<Arc<crate::HyperlinkTarget>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CellSemanticSnapshot {
    #[default]
    Output,
    Input,
    Prompt,
}

#[cfg(feature = "engine")]
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

#[cfg(feature = "engine")]
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

#[cfg(feature = "engine")]
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
    /// Advances the local presentation generation without wrapping.
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    #[cfg(any(test, feature = "test-support"))]
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

#[cfg(feature = "engine")]
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

#[cfg(feature = "engine")]
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

#[cfg(feature = "engine")]
pub(crate) fn normalize_cursor_position(
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

#[cfg(feature = "engine")]
pub(crate) fn cursor_damage(
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
    pub(crate) fn initial() -> Self {
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

    #[cfg(all(test, feature = "engine"))]
    pub(crate) fn cursor(row: u16) -> Self {
        Self {
            cursor: ContentDamageSnapshot::Rows(Arc::from([row])),
            ..Self::default()
        }
    }

    #[cfg(feature = "engine")]
    pub(crate) fn is_clean(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScrollbarSnapshot {
    pub total_rows: u64,
    pub offset_rows: u64,
    pub visible_rows: u64,
}

/// A local presentation, including selection, Find, and retained local file authority.
/// This is not a wire message and must not be serialized as authoritative server content.
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
    pub viewport: ViewportSnapshot,
    pub scrollbar: ScrollbarSnapshot,
    pub active_screen: ActiveScreenSnapshot,
    pub cursor: CursorSnapshot,
    pub text_blinking: bool,
    pub mouse_tracking: bool,
    pub selection_present: bool,
    pub title: Arc<str>,
    pub metadata: Arc<TerminalMetadataSnapshot>,
    pub find: Option<Arc<TerminalFindSnapshot>>,
    pub graphics: GraphicsSnapshot,
    pub damage: SnapshotDamage,
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
    pub fn empty_with_appearance(
        paths: crate::local_path::LocalPathSemantics,
        initial_appearance: &impl AppearanceUpdate,
    ) -> Arc<Self> {
        let initial_appearance = initial_appearance.terminal_update();
        let appearance = &initial_appearance.appearance;
        let colors = TerminalColorsSnapshot::configured(
            appearance.terminal_colors(),
            appearance.terminal_bold_as_bright(),
        );
        Arc::new(Self {
            generation: PresentationGeneration::default(),
            appearance_generation: initial_appearance.generation,
            terminal_appearance: appearance.terminal_appearance(),
            configured_colors: Arc::new(appearance.terminal_colors().clone()),
            bold_as_bright: appearance.terminal_bold_as_bright(),
            rows: Arc::from([]),
            row_soft_wrapped: Arc::from([]),
            background: appearance.terminal_colors().background,
            colors,
            size: ScreenSizeSnapshot::default(),
            viewport: ViewportSnapshot::default(),
            scrollbar: ScrollbarSnapshot::default(),
            active_screen: ActiveScreenSnapshot::default(),
            cursor: CursorSnapshot {
                color: appearance.terminal_colors().cursor,
                text_color: appearance
                    .terminal_colors()
                    .cursor_text
                    .unwrap_or(appearance.terminal_colors().background),
                ..CursorSnapshot::default()
            },
            text_blinking: false,
            mouse_tracking: false,
            selection_present: false,
            title: Arc::from(""),
            metadata: MetadataTracker::new(paths, "", "", Default::default(), Instant::now())
                .snapshot(),
            find: None,
            graphics: GraphicsSnapshot::default(),
            damage: SnapshotDamage::initial(),
        })
    }

    /// Recolors retained content for rendering without claiming that the worker applied a newer
    /// appearance generation. Program-owned defaults and palette entries remain unchanged.
    pub fn projected_for_renderer(
        screen: &Arc<Self>,
        appearance: &impl TerminalAppearance,
    ) -> Arc<Self> {
        if screen.terminal_appearance == appearance.terminal_appearance()
            && screen.configured_colors.as_ref() == appearance.terminal_colors()
            && screen.bold_as_bright == appearance.terminal_bold_as_bright()
        {
            return Arc::clone(screen);
        }
        screen.reprojected(appearance)
    }

    fn reprojected(&self, appearance: &impl TerminalAppearance) -> Arc<Self> {
        let mut colors = self.colors.clone();
        if colors.foreground_source == TerminalDefaultColorSource::HostDefault {
            colors.foreground = appearance.terminal_colors().foreground;
        }
        if colors.background_source == TerminalDefaultColorSource::HostDefault {
            colors.background = appearance.terminal_colors().background;
        }
        let mut palette = *colors.palette;
        for index in 0..8 {
            if !colors.palette_overrides[index] {
                palette[index] = appearance.terminal_colors().normal[index];
            }
            if !colors.palette_overrides[index + 8] {
                palette[index + 8] = appearance.terminal_colors().bright[index];
            }
        }
        colors.palette = Arc::new(palette);
        colors.configured = Arc::new(appearance.terminal_colors().clone());
        colors.bold_as_bright = appearance.terminal_bold_as_bright();

        let mut cursor = self.cursor;
        if colors.cursor_source == TerminalDefaultColorSource::HostDefault {
            cursor.color = appearance.terminal_colors().cursor;
            cursor.text_color = appearance
                .terminal_colors()
                .cursor_text
                .unwrap_or_else(|| colors.effective_background());
        } else {
            cursor.text_color = colors.effective_background();
        }
        let background = colors.effective_background();
        Arc::new(Self {
            terminal_appearance: appearance.terminal_appearance(),
            configured_colors: Arc::clone(&colors.configured),
            bold_as_bright: appearance.terminal_bold_as_bright(),
            background,
            colors,
            cursor,
            ..self.clone()
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn empty(paths: crate::local_path::LocalPathSemantics) -> Arc<Self> {
        Self::empty_with_appearance(paths, &crate::test_terminal_appearance_update())
    }

    #[cfg(any(test, feature = "test-support"))]
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
            scrollbar,
            title: title.into(),
            ..Self::empty_value()
        })
    }

    #[cfg(any(test, feature = "test-support"))]
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
            scrollbar,
            title: title.into(),
            ..Self::empty_value()
        })
    }

    #[cfg(any(test, feature = "test-support"))]
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
                Default::default(),
                Instant::now(),
            )
            .snapshot(),
            find: None,
            graphics: GraphicsSnapshot::default(),
            damage: SnapshotDamage::initial(),
        }
    }
}

#[cfg(any(feature = "engine", test, feature = "test-support"))]
pub(crate) fn rows_have_visible_blinking_text(rows: &[RowSnapshot]) -> bool {
    rows.iter().any(|row| {
        row.iter()
            .any(|cell| cell.blinking && !cell.invisible && !cell.spacer_tail)
    })
}

#[cfg(any(test, feature = "test-support"))]
fn rows_have_selection(rows: &[RowSnapshot]) -> bool {
    rows.iter().any(|row| row.iter().any(|cell| cell.selected))
}
