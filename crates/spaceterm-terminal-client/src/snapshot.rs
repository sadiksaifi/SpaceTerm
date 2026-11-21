//! Portable cells, rows and screen presentation state.
use crate::find::TerminalFindSnapshot;
use spaceterm_protocol::pane::effects::Hyperlink;
use std::sync::Arc;

/// An opaque color carried by SpaceTerm Protocol.
pub type Rgb = [u8; 3];

/// Converts between wire RGB and the client's color representation.
pub trait ColorValue: Copy + Eq {
    fn from_rgb(rgb: Rgb) -> Self;
    fn rgb(self) -> Rgb;

    fn default_cursor_colors() -> (Self, Self) {
        let color = Self::from_rgb([0; 3]);
        (color, color)
    }
}

impl ColorValue for Rgb {
    fn from_rgb(rgb: Rgb) -> Self {
        rgb
    }

    fn rgb(self) -> Rgb {
        self
    }
}

/// A cell's grapheme cluster. Clusters of up to 23 bytes are stored inline; longer ones share
/// one heap allocation among clones.
pub type CellText = smol_str::SmolStr;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CellSnapshot<L = Hyperlink, C = Rgb> {
    pub text: CellText,
    pub foreground_source: TerminalColor<C>,
    pub background_source: TerminalColor<C>,
    pub inverse: bool,
    pub bold: bool,
    pub faint: bool,
    pub italic: bool,
    pub blinking: bool,
    pub invisible: bool,
    pub underline: TerminalUnderlineSnapshot,
    pub underline_source: TerminalColor<C>,
    pub strikethrough: bool,
    pub overline: bool,
    pub selected: bool,
    pub spacer_tail: bool,
    pub semantic_content: CellSemanticSnapshot,
    pub hyperlink: Option<Arc<L>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CellSemanticSnapshot {
    #[default]
    Output,
    Input,
    Prompt,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TerminalColor<C = Rgb> {
    #[default]
    Default,
    Palette(u8),
    Rgb(C),
}

pub type RowSnapshot<L = Hyperlink, C = Rgb> = Arc<[CellSnapshot<L, C>]>;

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct PresentationGeneration(u64);

impl PresentationGeneration {
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    pub const fn from_counter(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ActiveScreenSnapshot {
    #[default]
    Primary,
    Alternate,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CursorPositionSnapshot {
    pub column: u16,
    pub row: u16,
    pub width_cells: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CursorSnapshot<C = Rgb> {
    pub position: Option<CursorPositionSnapshot>,
    pub visible: bool,
    pub blinking: bool,
    pub password_input: bool,
    pub shape: CursorShapeSnapshot,
    pub color: C,
    pub text_color: C,
}

impl<C: ColorValue> Default for CursorSnapshot<C> {
    fn default() -> Self {
        let (color, text_color) = C::default_cursor_colors();
        Self {
            position: None,
            visible: false,
            blinking: false,
            password_input: false,
            shape: CursorShapeSnapshot::default(),
            color,
            text_color,
        }
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
    pub fn initial() -> Self {
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

    pub fn is_clean(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScrollbarSnapshot {
    pub total_rows: u64,
    pub offset_rows: u64,
    pub visible_rows: u64,
}

/// The primary screen's Scrollback. Rows `oldest..total` are retained, and visible row `r` is
/// position `total + r`. Positions of one epoch never name different content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HistorySnapshot {
    pub epoch: u64,
    pub oldest: u64,
    pub total: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScrollbackMovement {
    PageUp,
    PageDown,
    Top,
    Bottom,
}

/// The frame data shared by live and scrolled views.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScreenState<L = Hyperlink, C = Rgb> {
    pub generation: PresentationGeneration,
    pub rows: Arc<[RowSnapshot<L, C>]>,
    /// Soft-wrap markers corresponding one-to-one with the published viewport rows.
    pub row_soft_wrapped: Arc<[bool]>,
    pub size: ScreenSizeSnapshot,
    pub cell_pixels: (u32, u32),
    pub viewport: ViewportSnapshot,
    pub scrollbar: ScrollbarSnapshot,
    pub active_screen: ActiveScreenSnapshot,
    pub text_blinking: bool,
    pub mouse_tracking: bool,
    pub alternate_scroll: bool,
    pub selection_present: bool,
    pub selection_revision: u64,
    pub find: Option<Arc<TerminalFindSnapshot>>,
    pub damage: SnapshotDamage,
    pub history: HistorySnapshot,
}

impl<L, C> Default for ScreenState<L, C> {
    fn default() -> Self {
        Self {
            generation: PresentationGeneration::default(),
            rows: Arc::from([]),
            row_soft_wrapped: Arc::from([]),
            size: ScreenSizeSnapshot::default(),
            cell_pixels: (0, 0),
            viewport: ViewportSnapshot::default(),
            scrollbar: ScrollbarSnapshot::default(),
            active_screen: ActiveScreenSnapshot::default(),
            text_blinking: false,
            mouse_tracking: false,
            alternate_scroll: false,
            selection_present: false,
            selection_revision: 0,
            find: None,
            damage: SnapshotDamage::initial(),
            history: HistorySnapshot::default(),
        }
    }
}

/// A frame presented or retained by a client. A host can use its own snapshot to carry
/// additional data through view updates without copying its rows.
pub trait Snapshot: Clone {
    type Link: Clone + Eq;
    type Color: ColorValue;

    fn screen(&self) -> &ScreenState<Self::Link, Self::Color>;
    fn screen_mut(&mut self) -> &mut ScreenState<Self::Link, Self::Color>;
    fn cursor(&self) -> &CursorSnapshot<Self::Color>;
    fn cursor_mut(&mut self) -> &mut CursorSnapshot<Self::Color>;
    fn palette(&self) -> &[Self::Color; 256];
}

/// Screen state for clients that use the protocol's link and color values.
#[derive(Clone, Debug)]
pub struct ScreenSnapshot<L = Hyperlink, C = Rgb> {
    pub state: ScreenState<L, C>,
    pub cursor: CursorSnapshot<C>,
    pub palette: Arc<[C; 256]>,
}

impl<L, C: ColorValue> Default for ScreenSnapshot<L, C> {
    fn default() -> Self {
        Self {
            state: ScreenState::default(),
            cursor: CursorSnapshot::default(),
            palette: Arc::new([C::from_rgb([0; 3]); 256]),
        }
    }
}

impl<L: Clone + Eq, C: ColorValue> Snapshot for ScreenSnapshot<L, C> {
    type Link = L;
    type Color = C;

    fn screen(&self) -> &ScreenState<L, C> {
        &self.state
    }

    fn screen_mut(&mut self) -> &mut ScreenState<L, C> {
        &mut self.state
    }

    fn cursor(&self) -> &CursorSnapshot<C> {
        &self.cursor
    }

    fn cursor_mut(&mut self) -> &mut CursorSnapshot<C> {
        &mut self.cursor
    }

    fn palette(&self) -> &[C; 256] {
        &self.palette
    }
}
