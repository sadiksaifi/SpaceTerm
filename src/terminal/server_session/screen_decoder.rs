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
pub(super) struct ScreenDecoder {
    frame: Option<u64>,
    styles: HashMap<u16, CellSnapshot>,
    links: HashMap<u16, Option<Arc<HyperlinkTarget>>>,
    screen: Option<DecodedScreen>,
    /// Counts each decoded frame.
    counters: Arc<TerminalCounters>,
    /// Where each row's cells are decoded, reused across rows.
    cells: Vec<CellSnapshot>,
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

    pub(super) fn new(counters: Arc<TerminalCounters>) -> Self {
        Self {
            frame: None,
            styles: HashMap::new(),
            links: HashMap::new(),
            screen: None,
            counters,
            cells: Vec::new(),
        }
    }

    pub(super) fn reset(&mut self) {
        *self = Self::new(Arc::clone(&self.counters));
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
                self.counters.record_frame_decoded();
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
struct Grid {
    rows: Arc<[RowSnapshot]>,
    wrapped: Arc<[bool]>,
    blinking: Vec<bool>,
    /// The row that fills rows a scroll vacates.
    blank: RowSnapshot,
}

impl Grid {
    fn blank(columns: usize, rows: usize) -> Self {
        let blank: RowSnapshot = Arc::from(vec![blank_cell(); columns]);
        Self {
            rows: vec![Arc::clone(&blank); rows].into(),
            wrapped: vec![false; rows].into(),
            blinking: vec![false; rows],
            blank,
        }
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
fn decode_row(
    row: &Row,
    columns: usize,
    styles: &HashMap<u16, CellSnapshot>,
    links: &HashMap<u16, Option<Arc<HyperlinkTarget>>>,
    cells: &mut Vec<CellSnapshot>,
) -> Result<RowSnapshot, UpdateError> {
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

fn blank_cell() -> CellSnapshot {
    template(&Style::DEFAULT)
}

/// A cell in `style` whose text and link the run supplies.
fn template(style: &Style) -> CellSnapshot {
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
    ScreenDecoder::new(Arc::default())
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
}

/// Turns decoded frames into the screens the renderer draws, with the client's own colors
/// wherever the program did not override them. It keeps the client's default screen and the
/// projected palette until their inputs change, so consecutive screens share them.
#[derive(Default)]
pub(super) struct Presenter {
    /// Where the Pane's metadata comes from, when it is not this machine.
    metadata: Option<super::super::metadata::TerminalMetadataContext>,
    defaults: Option<Defaults>,
    palette: Option<Palette>,
}

/// The screen every snapshot starts from, for one appearance.
struct Defaults {
    appearance: TerminalAppearanceUpdate,
    screen: ScreenSnapshot,
}

/// The palette the renderer draws, projected from the program's palette and an appearance.
struct Palette {
    program: Vec<[u8; 3]>,
    overrides: [u64; 4],
    appearance: Arc<crate::appearance::ResolvedTerminalAppearance>,
    colors: Arc<[crate::appearance::Color; 256]>,
    overridden: Arc<[bool; 256]>,
}

impl Presenter {
    pub(super) fn new(
        remote_metadata: Option<&super::super::metadata::RemoteTerminalMetadataContext>,
    ) -> Self {
        Self {
            metadata: remote_metadata.map(|context| {
                super::super::metadata::TerminalMetadataContext::Remote(context.clone())
            }),
            ..Self::default()
        }
    }

    /// The screen the renderer draws for `decoded`. Graphics are left to the caller.
    pub(super) fn snapshot(
        &mut self,
        decoded: &DecodedScreen,
        generation: PresentationGeneration,
        appearance: &TerminalAppearanceUpdate,
    ) -> ScreenSnapshot {
        let configured = &appearance.appearance.colors;
        let mut screen = self.defaults(appearance).clone();
        let (palette, palette_overrides) = self.palette(&decoded.colors, appearance);
        screen.colors.palette = palette;
        screen.colors.palette_overrides = palette_overrides;
        decoded.present(&mut screen, generation, configured);
        screen
    }

    fn defaults(&mut self, appearance: &TerminalAppearanceUpdate) -> &ScreenSnapshot {
        let current = self.defaults.as_ref().is_some_and(|defaults| {
            defaults.appearance.generation == appearance.generation
                && Arc::ptr_eq(&defaults.appearance.appearance, &appearance.appearance)
        });
        if !current {
            let mut screen = (*ScreenSnapshot::empty_with_appearance(
                crate::local_path::LocalPathSemantics::Posix,
                appearance,
            ))
            .clone();
            if let Some(context) = &self.metadata {
                Arc::make_mut(&mut screen.metadata).context = context.clone();
            }
            self.defaults = Some(Defaults {
                appearance: appearance.clone(),
                screen,
            });
        }
        &self
            .defaults
            .as_ref()
            .expect("the defaults were just set")
            .screen
    }

    fn palette(
        &mut self,
        colors: &Colors,
        appearance: &TerminalAppearanceUpdate,
    ) -> (Arc<[crate::appearance::Color; 256]>, Arc<[bool; 256]>) {
        let current = self.palette.as_ref().filter(|palette| {
            palette.overrides == colors.palette_overrides
                && Arc::ptr_eq(&palette.appearance, &appearance.appearance)
                && palette.program == colors.palette
        });
        if let Some(palette) = current {
            return (Arc::clone(&palette.colors), Arc::clone(&palette.overridden));
        }
        let configured = &appearance.appearance.colors;
        let overridden =
            |index: usize| colors.palette_overrides[index / 64] & (1 << (index % 64)) != 0;
        let mut projected = [crate::appearance::Color::BLACK; 256];
        for (index, (destination, source)) in projected.iter_mut().zip(&colors.palette).enumerate()
        {
            *destination = match index {
                0..8 if !overridden(index) => configured.normal[index],
                8..16 if !overridden(index) => configured.bright[index - 8],
                _ => rgb(*source),
            };
        }
        let palette = Palette {
            program: colors.palette.clone(),
            overrides: colors.palette_overrides,
            appearance: Arc::clone(&appearance.appearance),
            colors: Arc::new(projected),
            overridden: Arc::new(std::array::from_fn(overridden)),
        };
        let shared = (Arc::clone(&palette.colors), Arc::clone(&palette.overridden));
        self.palette = Some(palette);
        shared
    }
}

impl DecodedScreen {
    /// Writes this frame's state over the client's default `screen`.
    fn present(
        &self,
        screen: &mut ScreenSnapshot,
        generation: PresentationGeneration,
        configured: &crate::appearance::TerminalColors,
    ) {
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
    }
}
