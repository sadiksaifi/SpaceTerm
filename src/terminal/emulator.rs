use std::cell::RefCell;
use std::mem;
use std::rc::Rc;
use std::sync::Arc;

use libghostty_vt::render::{CellIterator, Dirty, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{PaletteIndex, RgbColor};
use libghostty_vt::{RenderState, Terminal, TerminalOptions};

use crate::theme::{ACTIVE_THEME, Color};

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
    pty_responses: Rc<RefCell<Vec<u8>>>,
    row_cache: Vec<RowSnapshot>,
    cached_cols: u16,
    cached_foreground: Option<Color>,
    cached_background: Option<Color>,
    cached_cursor: Option<(u16, u16)>,
}

impl TerminalEmulator {
    pub(crate) fn new(cols: u16, rows: u16) -> Result<Self, libghostty_vt::Error> {
        let pty_responses = Rc::new(RefCell::new(Vec::new()));
        let mut terminal: Terminal<'static, 'static> = Terminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback: 10_000,
        })?;

        apply_theme(&mut terminal)?;
        terminal.on_pty_write({
            let pty_responses = Rc::clone(&pty_responses);
            move |_, data| pty_responses.borrow_mut().extend_from_slice(data)
        })?;

        Ok(Self {
            terminal,
            render_state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            pty_responses,
            row_cache: Vec::new(),
            cached_cols: 0,
            cached_foreground: None,
            cached_background: None,
            cached_cursor: None,
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
    ) -> Result<(), libghostty_vt::Error> {
        self.terminal
            .resize(cols, rows, cell_width_px, cell_height_px)
    }

    pub(crate) fn take_pty_responses(&self) -> Vec<u8> {
        mem::take(&mut *self.pty_responses.borrow_mut())
    }

    pub(crate) fn snapshot(&mut self) -> Result<Option<Arc<ScreenSnapshot>>, libghostty_vt::Error> {
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

    #[test]
    fn vt_sequences_update_the_screen_without_leaking_escape_bytes() {
        let mut emulator = TerminalEmulator::new(12, 3).unwrap();
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
        let mut emulator = TerminalEmulator::new(10, 2).unwrap();
        emulator.resize(20, 4, 8, 18).unwrap();

        let snapshot = emulator.snapshot().unwrap().unwrap();
        assert_eq!(snapshot.rows.len(), 4);
        assert!(snapshot.rows.iter().all(|row| row.len() == 20));
    }

    #[test]
    fn clean_screens_do_not_publish_another_snapshot() {
        let mut emulator = TerminalEmulator::new(10, 2).unwrap();

        assert!(emulator.snapshot().unwrap().is_some());
        assert!(emulator.snapshot().unwrap().is_none());
    }

    #[test]
    fn unchanged_rows_reuse_their_cell_storage() {
        let mut emulator = TerminalEmulator::new(10, 3).unwrap();
        let first = emulator.snapshot().unwrap().unwrap();

        emulator.feed(b"x");
        let second = emulator.snapshot().unwrap().unwrap();

        assert!(!Arc::ptr_eq(&first.rows[0], &second.rows[0]));
        assert!(Arc::ptr_eq(&first.rows[1], &second.rows[1]));
        assert!(Arc::ptr_eq(&first.rows[2], &second.rows[2]));
    }
}
