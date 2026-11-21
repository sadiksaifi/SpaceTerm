//! Applies terminal screen and Scrollback page updates to presentation rows.
//!
//! Unchanged rows keep their Arc, including rows that move in a scroll.
use crate::snapshot::{
    ActiveScreenSnapshot, CellSemanticSnapshot, CellSnapshot, CellText, ColorValue,
    CursorPositionSnapshot, CursorShapeSnapshot, CursorSnapshot, HistorySnapshot,
    PresentationGeneration, Rgb, RowSnapshot, ScreenSizeSnapshot, ScreenState, TerminalColor,
    TerminalUnderlineSnapshot,
};
use spaceterm_protocol::pane::{self, effects::Hyperlink, graphics};
use spaceterm_protocol::protocol::screen::{
    Colors, Cursor, Geometry, History, Modes, Page, Row, ScreenUpdate, Style, StyleRef, flags,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Why an update could not be applied. Either way the decoder is empty afterwards, so the next
/// request asks for a keyframe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateError {
    /// The update applies to a frame this decoder does not hold.
    UnknownBase,
    /// The update contradicts itself or the frame it applies to.
    Malformed,
}

/// One channel's decoder. It holds the last applied frame and the channel's style and link
/// tables.
pub struct ScreenDecoder<L = Hyperlink, C = Rgb> {
    frame: Option<u64>,
    styles: HashMap<u16, CellSnapshot<L, C>>,
    links: HashMap<u16, Option<Arc<L>>>,
    screen: Option<DecodedScreen<L, C>>,
    /// Where each row's cells are decoded, reused across rows.
    cells: Vec<CellSnapshot<L, C>>,
}

/// The server's screen as of the decoder's last frame.
#[derive(Clone)]
pub struct DecodedScreen<L = Hyperlink, C = Rgb> {
    pub geometry: Geometry,
    pub modes: Modes,
    pub colors: Colors,
    pub cursor: Cursor,
    pub history: History,
    pub graphics: Option<Result<graphics::Snapshot, graphics::Error>>,
    pub generation: u64,
    pub rows: Arc<[RowSnapshot<L, C>]>,
    pub wrapped: Arc<[bool]>,
    /// Whether each row shows blinking text, kept up to date row by row.
    blinking: Vec<bool>,
    blank: RowSnapshot<L, C>,
}

impl<L: Clone, C: ColorValue> ScreenDecoder<L, C> {
    /// The last applied frame.
    pub fn frame(&self) -> Option<u64> {
        self.frame
    }

    pub fn new() -> Self {
        Self {
            frame: None,
            styles: HashMap::new(),
            links: HashMap::new(),
            screen: None,
            cells: Vec::new(),
        }
    }

    /// Applies frame `frame`, which the server encoded against `base`. The client supplies the link values to retain.
    ///
    /// `update` must have passed [`ScreenUpdate::validate`]. The link's `Assembler` validates
    /// every message it receives, so the decoder does not repeat it.
    pub fn apply(
        &mut self,
        frame: u64,
        base: Option<u64>,
        update: ScreenUpdate,
        resolve_link: impl FnMut(&Hyperlink) -> Option<Arc<L>>,
    ) -> Result<&DecodedScreen<L, C>, UpdateError> {
        match self.decode(frame, base, update, resolve_link) {
            Ok(()) => self.screen.as_ref().ok_or(UpdateError::Malformed),
            Err(error) => {
                *self = Self::new();
                Err(error)
            }
        }
    }

    fn decode(
        &mut self,
        frame: u64,
        base: Option<u64>,
        update: ScreenUpdate,
        mut resolve_link: impl FnMut(&Hyperlink) -> Option<Arc<L>>,
    ) -> Result<(), UpdateError> {
        let previous = match base {
            None => {
                self.styles.clear();
                self.links.clear();
                None
            }
            Some(base) if self.frame == Some(base) && base < frame => self.screen.take(),
            Some(_) => return Err(UpdateError::UnknownBase),
        };
        for definition in &update.styles {
            self.styles
                .insert(definition.id, template(&definition.style));
        }
        for definition in &update.links {
            self.links
                .insert(definition.id, resolve_link(&definition.link));
        }
        let geometry = required(update.geometry, previous.as_ref().map(|p| p.geometry))?;
        let modes = required(update.modes, previous.as_ref().map(|p| p.modes))?;
        let columns = usize::from(geometry.columns);
        let row_count = usize::from(geometry.rows);
        // The base frame's values move into this frame, so a frame that leaves them out
        // allocates nothing for them.
        let (colors, graphics, kept) = match previous {
            Some(previous) => {
                // A new grid size starts from blank rows.
                let same_grid = (previous.geometry.columns, previous.geometry.rows)
                    == (geometry.columns, geometry.rows);
                (
                    update.colors.or(Some(previous.colors)),
                    update.graphics.or(previous.graphics),
                    same_grid.then_some(Grid {
                        rows: previous.rows,
                        wrapped: previous.wrapped,
                        blinking: previous.blinking,
                        blank: previous.blank,
                    }),
                )
            }
            None => (update.colors, update.graphics, None),
        };
        let colors = colors.ok_or(UpdateError::Malformed)?;
        let Grid {
            mut rows,
            mut wrapped,
            mut blinking,
            blank,
        } = kept.unwrap_or_else(|| Grid::blank(columns, row_count));
        // Rows and markers the update leaves alone keep their arrays. The first change copies
        // an array the renderer still holds, so its screen stays as it was.
        if let Some(shift) = update.shift {
            shift
                .apply(Arc::make_mut(&mut rows), || Arc::clone(&blank))
                .ok_or(UpdateError::Malformed)?;
            shift
                .apply(Arc::make_mut(&mut wrapped), || false)
                .ok_or(UpdateError::Malformed)?;
            shift
                .apply(&mut blinking, || false)
                .ok_or(UpdateError::Malformed)?;
        }
        for row in &update.rows {
            let index = usize::from(row.index);
            if index >= row_count {
                return Err(UpdateError::Malformed);
            }
            let decoded = decode_row(row, columns, &self.styles, &self.links, &mut self.cells)?;
            blinking[index] = decoded
                .iter()
                .any(|cell| cell.blinking && !cell.invisible && !cell.spacer_tail);
            Arc::make_mut(&mut rows)[index] = decoded;
            if wrapped[index] != row.wrapped {
                Arc::make_mut(&mut wrapped)[index] = row.wrapped;
            }
        }
        if let Some(position) = update.cursor.position
            && (position.row >= geometry.rows || position.column >= geometry.columns)
        {
            return Err(UpdateError::Malformed);
        }
        self.screen = Some(DecodedScreen {
            geometry,
            modes,
            colors,
            cursor: update.cursor,
            history: update.history,
            graphics,
            generation: update.generation,
            rows,
            wrapped,
            blinking,
            blank,
        });
        self.frame = Some(frame);
        Ok(())
    }
}

/// The rows of a frame and what the decoder tracks for each.
struct Grid<L, C> {
    rows: Arc<[RowSnapshot<L, C>]>,
    wrapped: Arc<[bool]>,
    blinking: Vec<bool>,
    /// The row that fills rows a scroll vacates.
    blank: RowSnapshot<L, C>,
}

impl<L: Clone, C: ColorValue> Grid<L, C> {
    fn blank(columns: usize, rows: usize) -> Self {
        let blank: RowSnapshot<L, C> = Arc::from(vec![blank_cell(); columns]);
        Self {
            rows: vec![Arc::clone(&blank); rows].into(),
            wrapped: vec![false; rows].into(),
            blinking: vec![false; rows],
            blank,
        }
    }
}

/// One decoded row and its soft-wrap marker.
pub type DecodedRow<L, C> = (RowSnapshot<L, C>, bool);

/// Decodes a Scrollback page against its own tables. Each row has `columns` cells. `page` must
/// have passed [`Page::validate`], which the link runs on every reply.
pub fn decode_page<L: Clone, C: ColorValue>(
    page: &Page,
    columns: u16,
    mut resolve_link: impl FnMut(&Hyperlink) -> Option<Arc<L>>,
) -> Result<Vec<DecodedRow<L, C>>, UpdateError> {
    let styles = page
        .styles
        .iter()
        .map(|definition| (definition.id, template(&definition.style)))
        .collect();
    let links = page
        .links
        .iter()
        .map(|definition| (definition.id, resolve_link(&definition.link)))
        .collect();
    let mut cells = Vec::new();
    page.rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            if usize::from(row.index) != index {
                return Err(UpdateError::Malformed);
            }
            Ok((
                decode_row(row, usize::from(columns), &styles, &links, &mut cells)?,
                row.wrapped,
            ))
        })
        .collect()
}

/// A keyframe's value, or the base frame's when the update leaves it out.
fn required<T>(value: Option<T>, previous: Option<T>) -> Result<T, UpdateError> {
    value.or(previous).ok_or(UpdateError::Malformed)
}

/// Decodes `row` through `cells`, which only holds cells while it runs.
fn decode_row<L: Clone, C: ColorValue>(
    row: &Row,
    columns: usize,
    styles: &HashMap<u16, CellSnapshot<L, C>>,
    links: &HashMap<u16, Option<Arc<L>>>,
    cells: &mut Vec<CellSnapshot<L, C>>,
) -> Result<RowSnapshot<L, C>, UpdateError> {
    let default = blank_cell();
    cells.clear();
    cells.reserve(columns);
    for run in &row.runs {
        let inline;
        let template = match run.style {
            StyleRef::Default => &default,
            StyleRef::Id(id) => styles.get(&id).ok_or(UpdateError::Malformed)?,
            StyleRef::Inline(style) => {
                inline = template(&style);
                &inline
            }
        };
        let link = match run.link {
            None => None,
            Some(id) => links.get(&id).ok_or(UpdateError::Malformed)?.clone(),
        };
        if cells.len() + run.width() > columns {
            return Err(UpdateError::Malformed);
        }
        for range in run.cells() {
            let spacer_tail = range.is_empty();
            cells.push(CellSnapshot {
                text: if spacer_tail {
                    CellText::new_inline(" ")
                } else {
                    CellText::new(run.text.get(range).ok_or(UpdateError::Malformed)?)
                },
                spacer_tail,
                hyperlink: link.clone(),
                ..template.clone()
            });
        }
    }
    cells.resize(columns, default);
    // The row's only allocation: the drain has an exact length.
    Ok(cells.drain(..).collect())
}

fn blank_cell<L, C: ColorValue>() -> CellSnapshot<L, C> {
    template(&Style::DEFAULT)
}

/// A cell in `style` whose text and link the run supplies.
fn template<L, C: ColorValue>(style: &Style) -> CellSnapshot<L, C> {
    let flag = |flag: u16| style.flags & flag != 0;
    CellSnapshot {
        text: CellText::new_inline(" "),
        foreground_source: color(style.foreground),
        background_source: color(style.background),
        inverse: flag(flags::INVERSE),
        bold: flag(flags::BOLD),
        faint: flag(flags::FAINT),
        italic: flag(flags::ITALIC),
        blinking: flag(flags::BLINKING),
        invisible: flag(flags::INVISIBLE),
        underline: match style.underline {
            1 => TerminalUnderlineSnapshot::Single,
            2 => TerminalUnderlineSnapshot::Double,
            3 => TerminalUnderlineSnapshot::Curly,
            4 => TerminalUnderlineSnapshot::Dotted,
            5 => TerminalUnderlineSnapshot::Dashed,
            _ => TerminalUnderlineSnapshot::None,
        },
        underline_source: color(style.underline_color),
        strikethrough: flag(flags::STRIKETHROUGH),
        overline: flag(flags::OVERLINE),
        selected: flag(flags::SELECTED),
        spacer_tail: false,
        semantic_content: CellSemanticSnapshot::Output,
        hyperlink: None,
    }
}

fn color<C: ColorValue>(color: pane::Color) -> TerminalColor<C> {
    match color {
        pane::Color::Default => TerminalColor::Default,
        pane::Color::Palette(index) => TerminalColor::Palette(index),
        pane::Color::Rgb(color) => TerminalColor::Rgb(C::from_rgb(color)),
    }
}

impl<L: Clone, C: ColorValue> Default for ScreenDecoder<L, C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<L, C> DecodedScreen<L, C> {
    /// Apply the decoded frame's rows and geometry to a client snapshot.
    pub fn apply_to_rows(
        &self,
        screen: &mut ScreenState<L, C>,
        generation: PresentationGeneration,
    ) {
        screen.generation = generation;
        screen.cell_pixels = self
            .geometry
            .cell_pixels
            .map_or((0, 0), |(width, height)| (u32::from(width), u32::from(height)));
        screen.history = HistorySnapshot {
            epoch: self.history.epoch,
            oldest: self.history.oldest,
            total: self.history.total,
        };
        screen.mouse_tracking = self.modes.mouse_tracking;
        screen.alternate_scroll = self.modes.alternate_scroll;
        screen.size = ScreenSizeSnapshot {
            cols: self.geometry.columns,
            rows: self.geometry.rows,
        };
        screen.rows = Arc::clone(&self.rows);
        screen.row_soft_wrapped = Arc::clone(&self.wrapped);
        screen.active_screen = if self.modes.alternate_screen {
            ActiveScreenSnapshot::Alternate
        } else {
            ActiveScreenSnapshot::Primary
        };
        screen.text_blinking = self.blinking.contains(&true);
    }

    /// The wire cursor with the client's resolved colors.
    pub fn cursor_snapshot(&self, color: C, text_color: C) -> CursorSnapshot<C> {
        CursorSnapshot {
            position: self.cursor.position.map(|position| CursorPositionSnapshot {
                column: position.column,
                row: position.row,
                width_cells: position.width,
            }),
            visible: self.cursor.visible,
            blinking: self.cursor.blinking,
            password_input: false,
            shape: match self.cursor.shape {
                1 => CursorShapeSnapshot::Bar,
                2 => CursorShapeSnapshot::Underline,
                3 => CursorShapeSnapshot::BlockHollow,
                _ => CursorShapeSnapshot::Block,
            },
            color,
            text_color,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_without_cell_pixels_clears_the_old_dimensions() {
        let mut decoder: ScreenDecoder = ScreenDecoder::new();
        let mut screen = ScreenState::default();
        let initial = ScreenUpdate {
            geometry: Some(Geometry {
                columns: 8,
                rows: 2,
                cell_pixels: Some((8, 16)),
            }),
            modes: Some(Modes::default()),
            colors: Some(Colors {
                foreground: [255; 3],
                background: [0; 3],
                cursor: [255; 3],
                cursor_text: [0; 3],
                palette: vec![[0; 3]; 256],
                foreground_overridden: false,
                background_overridden: false,
                cursor_overridden: false,
                palette_overrides: [0; 4],
            }),
            ..ScreenUpdate::default()
        };
        initial.validate().unwrap();
        decoder
            .apply(1, None, initial, |_| None)
            .unwrap()
            .apply_to_rows(&mut screen, PresentationGeneration::from_counter(1));
        assert_eq!(screen.cell_pixels, (8, 16));

        let resized = ScreenUpdate {
            geometry: Some(Geometry {
                columns: 10,
                rows: 2,
                cell_pixels: None,
            }),
            ..ScreenUpdate::default()
        };
        resized.validate().unwrap();
        decoder
            .apply(2, Some(1), resized, |_| None)
            .unwrap()
            .apply_to_rows(&mut screen, PresentationGeneration::from_counter(2));
        assert_eq!(screen.cell_pixels, (0, 0));
    }
}
