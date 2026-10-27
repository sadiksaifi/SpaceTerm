//! Applies screen updates to the rows the desktop renders.
//!
//! Rows an update does not replace keep their `Arc`, including rows a scroll moves, so the
//! renderer finds them by pointer and prepares only the rows that changed.
use super::*;
use crate::runtime::HyperlinkTarget;
use spaceterm_server::pane::graphics;
use spaceterm_server::protocol::screen::{
    Colors, Cursor, DEFAULT_STYLE, Geometry, History, Modes, Page, Row, ScreenUpdate, Style, flags,
};
use std::collections::HashMap;

/// Why an update could not be applied. Either way the decoder is empty afterwards, so the next
/// request asks for a keyframe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UpdateError {
    /// The update applies to a frame this decoder does not hold.
    UnknownBase,
    /// The update contradicts itself or the frame it applies to.
    Malformed,
}

/// One channel's decoder. It holds the last applied frame and the channel's style and link
/// tables.
#[derive(Default)]
pub(super) struct ScreenDecoder {
    frame: Option<u64>,
    styles: HashMap<u16, CellSnapshot>,
    links: HashMap<u16, Option<Arc<HyperlinkTarget>>>,
    screen: Option<DecodedScreen>,
}

/// The server's screen as of the decoder's last frame.
#[derive(Clone)]
pub(super) struct DecodedScreen {
    pub(super) geometry: Geometry,
    pub(super) modes: Modes,
    pub(super) colors: Colors,
    pub(super) cursor: Cursor,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the Scrollback cache reads it when scrollback merges"
        )
    )]
    pub(super) history: History,
    pub(super) reading: Option<pane::reading::Snapshot>,
    pub(super) graphics: Option<Result<graphics::Snapshot, graphics::Error>>,
    pub(super) generation: u64,
    pub(super) accessibility_revision: u64,
    pub(super) rows: Arc<[RowSnapshot]>,
    pub(super) wrapped: Arc<[bool]>,
    text_blinking: bool,
    blank: RowSnapshot,
}

impl ScreenDecoder {
    /// The last applied frame. `None` asks the server for a keyframe.
    pub(super) fn frame(&self) -> Option<u64> {
        self.frame
    }

    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Applies frame `frame`, which the server encoded against `base`. A file link becomes a
    /// target only through `local_files`.
    pub(super) fn apply(
        &mut self,
        frame: u64,
        base: Option<u64>,
        update: ScreenUpdate,
        local_files: Option<&crate::platform::local_filesystem::LocalFilesystemAuthority>,
    ) -> Result<&DecodedScreen, UpdateError> {
        match self.decode(frame, base, update, local_files) {
            Ok(()) => self.screen.as_ref().ok_or(UpdateError::Malformed),
            Err(error) => {
                self.reset();
                Err(error)
            }
        }
    }

    fn decode(
        &mut self,
        frame: u64,
        base: Option<u64>,
        update: ScreenUpdate,
        local_files: Option<&crate::platform::local_filesystem::LocalFilesystemAuthority>,
    ) -> Result<(), UpdateError> {
        update.validate().map_err(|_| UpdateError::Malformed)?;
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
            self.links.insert(
                definition.id,
                super::screen::decode_hyperlink(&definition.link, local_files),
            );
        }
        let geometry = required(update.geometry, previous.as_ref().map(|p| p.geometry))?;
        let modes = required(update.modes, previous.as_ref().map(|p| p.modes))?;
        let colors = required(update.colors, previous.as_ref().map(|p| p.colors.clone()))?;
        let columns = usize::from(geometry.columns);
        let row_count = usize::from(geometry.rows);
        // A new grid size starts from blank rows.
        let kept = previous.as_ref().filter(|previous| {
            (previous.geometry.columns, previous.geometry.rows) == (geometry.columns, geometry.rows)
        });
        let blank = kept.map_or_else(
            || Arc::from(vec![blank_cell(); columns]),
            |kept| Arc::clone(&kept.blank),
        );
        let (mut rows, mut wrapped) = match kept {
            Some(kept) => (kept.rows.to_vec(), kept.wrapped.to_vec()),
            None => (vec![Arc::clone(&blank); row_count], vec![false; row_count]),
        };
        if let Some(shift) = update.shift {
            shift
                .apply(&mut rows, || Arc::clone(&blank))
                .ok_or(UpdateError::Malformed)?;
            shift
                .apply(&mut wrapped, || false)
                .ok_or(UpdateError::Malformed)?;
        }
        for row in &update.rows {
            let index = usize::from(row.index);
            if index >= row_count {
                return Err(UpdateError::Malformed);
            }
            rows[index] = self.row(row, columns)?;
            wrapped[index] = row.wrapped;
        }
        if let Some(position) = update.cursor.position
            && (position.row >= geometry.rows || position.column >= geometry.columns)
        {
            return Err(UpdateError::Malformed);
        }
        let reading = match update.reading {
            Some(reading) => Some(reading),
            None => previous
                .as_ref()
                .and_then(|previous| previous.reading.clone()),
        };
        if let Some(reading) = &reading
            && (reading.visible_rows != row_count as u64
                || reading.offset_rows > reading.total_rows.saturating_sub(reading.visible_rows))
        {
            return Err(UpdateError::Malformed);
        }
        let graphics = match update.graphics {
            Some(graphics) => Some(graphics),
            None => previous
                .as_ref()
                .and_then(|previous| previous.graphics.clone()),
        };
        let (rows, wrapped, text_blinking) =
            match kept.filter(|_| update.shift.is_none() && update.rows.is_empty()) {
                // Nothing moved or changed, so the renderer sees the same row list.
                Some(kept) => (
                    Arc::clone(&kept.rows),
                    Arc::clone(&kept.wrapped),
                    kept.text_blinking,
                ),
                None => {
                    let text_blinking = rows.iter().any(|row| row.iter().any(|cell| cell.blinking));
                    (Arc::from(rows), Arc::from(wrapped), text_blinking)
                }
            };
        self.screen = Some(DecodedScreen {
            geometry,
            modes,
            colors,
            cursor: update.cursor,
            history: update.history,
            reading,
            graphics,
            generation: update.generation,
            accessibility_revision: update.accessibility_revision,
            rows,
            wrapped,
            text_blinking,
            blank,
        });
        self.frame = Some(frame);
        Ok(())
    }

    fn row(&self, row: &Row, columns: usize) -> Result<RowSnapshot, UpdateError> {
        decode_row(row, columns, &self.styles, &self.links)
    }
}

/// Decodes a Scrollback page against its own tables. Each row has `columns` cells.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the Scrollback cache decodes pages when scrollback merges"
    )
)]
pub(super) fn decode_page(
    page: &Page,
    columns: u16,
    local_files: Option<&crate::platform::local_filesystem::LocalFilesystemAuthority>,
) -> Result<Vec<(RowSnapshot, bool)>, UpdateError> {
    page.validate().map_err(|_| UpdateError::Malformed)?;
    let styles = page
        .styles
        .iter()
        .map(|definition| (definition.id, template(&definition.style)))
        .collect();
    let links = page
        .links
        .iter()
        .map(|definition| {
            (
                definition.id,
                super::screen::decode_hyperlink(&definition.link, local_files),
            )
        })
        .collect();
    page.rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            if usize::from(row.index) != index {
                return Err(UpdateError::Malformed);
            }
            Ok((
                decode_row(row, usize::from(columns), &styles, &links)?,
                row.wrapped,
            ))
        })
        .collect()
}

/// A keyframe's value, or the base frame's when the update leaves it out.
fn required<T>(value: Option<T>, previous: Option<T>) -> Result<T, UpdateError> {
    value.or(previous).ok_or(UpdateError::Malformed)
}

fn decode_row(
    row: &Row,
    columns: usize,
    styles: &HashMap<u16, CellSnapshot>,
    links: &HashMap<u16, Option<Arc<HyperlinkTarget>>>,
) -> Result<RowSnapshot, UpdateError> {
    let default = blank_cell();
    let mut cells = Vec::with_capacity(columns);
    for run in &row.runs {
        let template = match run.style {
            DEFAULT_STYLE => &default,
            id => styles.get(&id).ok_or(UpdateError::Malformed)?,
        };
        let link = match run.link {
            None => None,
            Some(id) => links.get(&id).ok_or(UpdateError::Malformed)?.clone(),
        };
        let ranges = run.cells().ok_or(UpdateError::Malformed)?;
        if cells.len() + ranges.len() > columns {
            return Err(UpdateError::Malformed);
        }
        for range in ranges {
            let spacer_tail = range.is_empty();
            cells.push(CellSnapshot {
                text: if spacer_tail {
                    " ".to_owned()
                } else {
                    run.text[range].to_owned()
                },
                spacer_tail,
                hyperlink: link.clone(),
                ..template.clone()
            });
        }
    }
    cells.resize(columns, default);
    Ok(Arc::from(cells))
}

fn blank_cell() -> CellSnapshot {
    template(&Style::DEFAULT)
}

/// A cell in `style` whose text and link the run supplies.
fn template(style: &Style) -> CellSnapshot {
    let flag = |flag: u16| style.flags & flag != 0;
    CellSnapshot {
        text: " ".to_owned(),
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

fn color(color: pane::Color) -> TerminalColor {
    match color {
        pane::Color::Default => TerminalColor::Default,
        pane::Color::Palette(index) => TerminalColor::Palette(index),
        pane::Color::Rgb(color) => TerminalColor::Rgb(rgb(color)),
    }
}

fn rgb(color: [u8; 3]) -> crate::appearance::Color {
    crate::appearance::Color::from_rgb_components(color[0], color[1], color[2])
}

/// The text of each row of a polled keyframe, with a wide character's spacer as a space.
#[cfg(test)]
pub(super) fn keyframe_lines(
    frame: &spaceterm_server::protocol::interim::ScreenFrame,
) -> Vec<String> {
    ScreenDecoder::default()
        .apply(frame.frame, frame.base, frame.update.clone(), None)
        .map(DecodedScreen::lines)
        .unwrap_or_default()
}

impl DecodedScreen {
    #[cfg(test)]
    pub(super) fn lines(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| row.iter().map(|cell| cell.text.as_str()).collect())
            .collect()
    }

    /// The screen the renderer draws, with the client's own colors wherever the program did not
    /// override them. Graphics and Pane metadata are left to the caller.
    pub(super) fn snapshot(
        &self,
        generation: PresentationGeneration,
        appearance: &TerminalAppearanceUpdate,
    ) -> ScreenSnapshot {
        let configured = &appearance.appearance.colors;
        let mut screen = (*ScreenSnapshot::empty_with_appearance(
            crate::local_path::LocalPathSemantics::Posix,
            appearance,
        ))
        .clone();
        screen.generation = generation;
        if let Some((width, height)) = self.geometry.cell_pixels {
            screen.cell_pixels = (u32::from(width), u32::from(height));
        }
        if let Some(reading) = &self.reading {
            screen.viewport = ViewportSnapshot {
                offset_rows: reading.offset_rows,
                visible_rows: reading.visible_rows,
            };
            screen.scrollbar = ScrollbarSnapshot {
                total_rows: reading.total_rows,
                offset_rows: reading.offset_rows,
                visible_rows: reading.visible_rows,
            };
            screen.selection_present = reading.selection_present;
            screen.find = reading.find.as_ref().map(|find| {
                Arc::new(super::super::TerminalFindSnapshot {
                    generation: super::super::FindQueryGeneration::from_value(find.generation),
                    total_matches: find.total,
                    current_match: find.current,
                    visible_spans: find
                        .spans
                        .iter()
                        .map(|&(row, start_column, end_column, current)| {
                            super::super::FindHighlightSpan {
                                row,
                                start_column,
                                end_column,
                                current,
                            }
                        })
                        .collect(),
                })
            });
        }
        screen.mouse_tracking = self.modes.mouse_tracking;
        screen.alternate_scroll = self.modes.alternate_scroll;
        screen.size = ScreenSizeSnapshot {
            cols: self.geometry.columns,
            rows: self.geometry.rows,
        };
        screen.rows = Arc::clone(&self.rows);
        screen.row_soft_wrapped = Arc::clone(&self.wrapped);
        let colors = &self.colors;
        let source = |overridden| {
            if overridden {
                TerminalDefaultColorSource::ProgramOverride
            } else {
                TerminalDefaultColorSource::HostDefault
            }
        };
        screen.colors.foreground_source = source(colors.foreground_overridden);
        screen.colors.background_source = source(colors.background_overridden);
        screen.colors.cursor_source = source(colors.cursor_overridden);
        screen.colors.foreground = if colors.foreground_overridden {
            rgb(colors.foreground)
        } else {
            configured.foreground
        };
        screen.colors.background = if colors.background_overridden {
            rgb(colors.background)
        } else {
            configured.background
        };
        let overridden =
            |index: usize| colors.palette_overrides[index / 64] & (1 << (index % 64)) != 0;
        let mut palette = [crate::appearance::Color::BLACK; 256];
        for (index, (destination, source)) in palette.iter_mut().zip(&colors.palette).enumerate() {
            *destination = match index {
                0..8 if !overridden(index) => configured.normal[index],
                8..16 if !overridden(index) => configured.bright[index - 8],
                _ => rgb(*source),
            };
        }
        screen.colors.palette = Arc::new(palette);
        screen.colors.palette_overrides = Arc::new(std::array::from_fn(overridden));
        screen.colors.reversed = self.modes.reversed;
        screen.background = screen.colors.effective_background();
        screen.active_screen = if self.modes.alternate_screen {
            ActiveScreenSnapshot::Alternate
        } else {
            ActiveScreenSnapshot::Primary
        };
        screen.cursor = CursorSnapshot {
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
            color: if colors.cursor_overridden {
                rgb(colors.cursor)
            } else {
                configured.cursor
            },
            text_color: if colors.cursor_overridden {
                rgb(colors.cursor_text)
            } else {
                configured.cursor_text.unwrap_or(screen.background)
            },
        };
        screen.text_blinking = self.text_blinking;
        screen
    }
}
