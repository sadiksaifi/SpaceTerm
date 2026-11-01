//! Applies screen updates to the rows the desktop renders.
//!
//! Rows an update does not replace keep their `Arc`, including rows a scroll moves, so the
//! renderer finds them by pointer and prepares only the rows that changed.
use super::*;
use crate::runtime::HyperlinkTarget;
use spaceterm_server::diagnostics::TerminalCounters;
use spaceterm_server::pane::graphics;
use spaceterm_server::protocol::screen::{
    Colors, Cursor, Geometry, History, Modes, Page, Row, ScreenUpdate, Style, StyleRef, flags,
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
    pub(super) history: History,
    pub(super) graphics: Option<Result<graphics::Snapshot, graphics::Error>>,
    pub(super) generation: u64,
    pub(super) accessibility_revision: u64,
    pub(super) rows: Arc<[RowSnapshot]>,
    pub(super) wrapped: Arc<[bool]>,
    /// Whether each row shows blinking text, kept up to date row by row.
    blinking: Vec<bool>,
    blank: RowSnapshot,
}

impl ScreenDecoder {
    /// The last applied frame.
    #[cfg(test)]
    pub(super) fn frame(&self) -> Option<u64> {
        self.frame
    }

    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Applies frame `frame`, which the server encoded against `base`. A file link becomes a
    /// target only through `local_files`.
    ///
    /// `update` must have passed [`ScreenUpdate::validate`]. The link's `Assembler` validates
    /// every message it receives, so the decoder does not repeat it.
    pub(super) fn apply(
        &mut self,
        frame: u64,
        base: Option<u64>,
        update: ScreenUpdate,
        local_files: Option<&crate::platform::local_filesystem::LocalFilesystemAuthority>,
    ) -> Result<&DecodedScreen, UpdateError> {
        match self.decode(frame, base, update, local_files) {
            Ok(()) => {
                TerminalCounters::process().record_frame_decoded();
                self.screen.as_ref().ok_or(UpdateError::Malformed)
            }
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
        let (mut rows, mut wrapped, mut blinking) = match kept {
            Some(kept) => (
                kept.rows.to_vec(),
                kept.wrapped.to_vec(),
                kept.blinking.clone(),
            ),
            None => (
                vec![Arc::clone(&blank); row_count],
                vec![false; row_count],
                vec![false; row_count],
            ),
        };
        if let Some(shift) = update.shift {
            shift
                .apply(&mut rows, || Arc::clone(&blank))
                .ok_or(UpdateError::Malformed)?;
            shift
                .apply(&mut wrapped, || false)
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
            let decoded = self.row(row, columns)?;
            blinking[index] = decoded
                .iter()
                .any(|cell| cell.blinking && !cell.invisible && !cell.spacer_tail);
            rows[index] = decoded;
            wrapped[index] = row.wrapped;
        }
        if let Some(position) = update.cursor.position
            && (position.row >= geometry.rows || position.column >= geometry.columns)
        {
            return Err(UpdateError::Malformed);
        }
        let graphics = match update.graphics {
            Some(graphics) => Some(graphics),
            None => previous
                .as_ref()
                .and_then(|previous| previous.graphics.clone()),
        };
        let (rows, wrapped) =
            match kept.filter(|_| update.shift.is_none() && update.rows.is_empty()) {
                // Nothing moved or changed, so the renderer sees the same row list.
                Some(kept) => (Arc::clone(&kept.rows), Arc::clone(&kept.wrapped)),
                None => (Arc::from(rows), Arc::from(wrapped)),
            };
        self.screen = Some(DecodedScreen {
            geometry,
            modes,
            colors,
            cursor: update.cursor,
            history: update.history,
            graphics,
            generation: update.generation,
            accessibility_revision: update.accessibility_revision,
            rows,
            wrapped,
            blinking,
            blank,
        });
        self.frame = Some(frame);
        Ok(())
    }

    fn row(&self, row: &Row, columns: usize) -> Result<RowSnapshot, UpdateError> {
        decode_row(row, columns, &self.styles, &self.links)
    }
}

/// Decodes a Scrollback page against its own tables. Each row has `columns` cells. `page` must
/// have passed [`Page::validate`], which the link runs on every reply.
pub(super) fn decode_page(
    page: &Page,
    columns: u16,
    local_files: Option<&crate::platform::local_filesystem::LocalFilesystemAuthority>,
) -> Result<Vec<(RowSnapshot, bool)>, UpdateError> {
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
                    " ".to_owned()
                } else {
                    run.text
                        .get(range)
                        .ok_or(UpdateError::Malformed)?
                        .to_owned()
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

/// The text of each row of a keyframe, with a wide character's spacer as a space.
#[cfg(test)]
pub(super) fn keyframe_lines(update: &ScreenUpdate) -> Vec<String> {
    ScreenDecoder::default()
        .apply(1, None, update.clone(), None)
        .map(DecodedScreen::lines)
        .unwrap_or_default()
}

/// The rows a channel bound to `lease` receives now from `host`, or none once the lease no
/// longer presents the Pane.
#[cfg(test)]
pub(super) fn observed_lines(
    host: &mut impl crate::runtime::HostCommands,
    lease: pane::Lease,
) -> Vec<String> {
    let crate::runtime::Observed::Live(observation) = host.observe([1; 32], &lease) else {
        return Vec::new();
    };
    observation
        .screen
        .and_then(|capture| {
            crate::runtime::screen_encoder::ScreenEncoder::default().encode(&capture, 1, None)
        })
        .map(|update| keyframe_lines(&update))
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
        // The history positions; the Scrollback view sets the viewport and scrollbar.
        screen.history = crate::terminal::emulator::HistorySnapshot {
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
        screen.text_blinking = self.blinking.contains(&true);
        screen
    }
}
