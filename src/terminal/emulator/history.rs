//! Absolute Scrollback positions, and Scrollback pages read from the engine.
//!
//! Ghostty counts history from its oldest retained row, which changes whenever it discards rows.
//! Clients cache rows by position, so positions here count every row that entered history in
//! one epoch instead. A tracked reference to the first visible row, re-anchored at every
//! snapshot, tells how many rows were discarded or erased above it since. Visible rows are never
//! discarded with history, so the anchor survives clearing the Scrollback. A new epoch starts when
//! the engine rewraps history, resets, or discards past the anchor, and when a taller screen pulls
//! history rows back onto it: those positions may name other rows once they scroll off again,
//! and a client that missed the frame with the lower total could keep the old rows.
use super::*;
use libghostty_vt::screen::GridRef;

/// The primary screen's Scrollback. Rows `oldest..total` are retained, and visible row `r` is
/// position `total + r`. Positions of one epoch never name different content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HistorySnapshot {
    pub epoch: u64,
    pub oldest: u64,
    pub total: u64,
}

pub(super) struct HistoryTracker {
    current: HistorySnapshot,
    columns: u16,
    rows: u16,
    /// The first visible row and its position.
    anchor: Option<(TrackedGridRef, u64)>,
}

impl Default for HistoryTracker {
    fn default() -> Self {
        Self {
            current: HistorySnapshot {
                epoch: 1,
                oldest: 0,
                total: 0,
            },
            columns: 0,
            rows: 0,
            anchor: None,
        }
    }
}

impl HistoryTracker {
    /// Measures the primary screen's history now. The alternate screen has none, so while it is
    /// active the primary screen's last measurement stands.
    pub(super) fn update(
        &mut self,
        terminal: &mut Terminal<'static, 'static>,
    ) -> Result<HistorySnapshot, Error> {
        if terminal.active_screen()? == Screen::Alternate {
            return Ok(self.current);
        }
        let retained = terminal.scrollback_rows()? as u64;
        let columns = terminal.cols()?;
        let rows = terminal.rows()?;
        let pulled_down = rows > self.rows && retained < self.current.total - self.current.oldest;
        self.rows = rows;
        let anchored = self
            .anchor
            .as_ref()
            .filter(|_| columns == self.columns && !pulled_down)
            .map(|(anchor, position)| {
                anchor
                    .point(PointSpace::Screen)
                    .map(|point| point.and_then(|point| position.checked_sub(u64::from(point.y))))
            })
            .transpose()?
            .flatten();
        let oldest = match anchored {
            Some(oldest) => oldest,
            None => {
                // The first measurement, or history that was rewrapped, reset or discarded past
                // the anchor.
                if self.anchor.is_some() {
                    self.current.epoch += 1;
                }
                self.columns = columns;
                0
            }
        };
        let total = oldest + retained;
        self.current.oldest = oldest;
        self.current.total = total;
        let point = Point::Screen(PointCoordinate {
            x: 0,
            y: u32::try_from(retained).unwrap_or(u32::MAX),
        });
        match &mut self.anchor {
            Some((anchor, position)) => {
                anchor.set(terminal, point)?;
                *position = total;
            }
            None => self.anchor = Some((terminal.track_grid_ref(point)?, total)),
        }
        Ok(self.current)
    }
}

/// History rows read for one `ScrollbackPage`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryPage {
    pub(crate) epoch: u64,
    /// The position of the first row. It is later than the request's start when the engine
    /// discarded the rows before it.
    pub(crate) start: u64,
    /// Each row's cells and whether it soft-wraps into the next.
    pub(crate) rows: Vec<(RowSnapshot, bool)>,
}

impl TerminalEmulator {
    /// Reads up to `rows` history rows from position `start` of `epoch`. Returns `None` when the
    /// epoch ended or the alternate screen hides the primary screen's history.
    pub(crate) fn scrollback_page(
        &mut self,
        epoch: u64,
        start: u64,
        rows: u16,
    ) -> Result<Option<HistoryPage>, TerminalFailure> {
        let error = |error| TerminalFailure::emulator_error("read-scrollback-page", error);
        if self.terminal.active_screen().map_err(error)? == Screen::Alternate {
            return Ok(None);
        }
        let history = self.history.update(&mut self.terminal).map_err(error)?;
        if history.epoch != epoch {
            return Ok(None);
        }
        let first = start.max(history.oldest);
        let end = start
            .saturating_add(u64::from(rows))
            .min(history.total)
            .max(first);
        let columns = self.terminal.cols().map_err(error)?;
        let mut links = Links::default();
        let rows = (first..end)
            .map(|position| {
                let y = u32::try_from(position - history.oldest).unwrap_or(u32::MAX);
                self.history_row(y, columns, &mut links).map_err(error)
            })
            .collect::<Result<_, _>>()?;
        Ok(Some(HistoryPage {
            epoch,
            start: first,
            rows,
        }))
    }

    fn history_row(
        &self,
        y: u32,
        columns: u16,
        links: &mut Links,
    ) -> Result<(RowSnapshot, bool), Error> {
        let wrapped = self
            .terminal
            .grid_ref(Point::Screen(PointCoordinate { x: 0, y }))?
            .row()?
            .is_wrapped()?;
        let mut graphemes = vec!['\0'; 8];
        let mut cells = Vec::with_capacity(usize::from(columns));
        for x in 0..columns {
            let reference = self
                .terminal
                .grid_ref(Point::Screen(PointCoordinate { x, y }))?;
            let cell = reference.cell()?;
            let style = reference.style()?;
            let spacer_tail = cell.wide()? == CellWide::SpacerTail;
            let mut text = String::new();
            if !spacer_tail && cell.has_text()? {
                let count = match reference.graphemes(&mut graphemes) {
                    Ok(count) => count,
                    Err(Error::OutOfSpace { required }) => {
                        graphemes.resize(required, '\0');
                        reference.graphemes(&mut graphemes)?
                    }
                    Err(error) => return Err(error),
                };
                text.extend(&graphemes[..count]);
            }
            if text.is_empty() {
                text.push(' ');
            }
            let background_source = match cell.content_tag()? {
                CellContentTag::BgColorPalette => {
                    TerminalColor::Palette(cell.bg_color_palette()?.0)
                }
                CellContentTag::BgColorRgb => TerminalColor::Rgb(cell.bg_color_rgb()?.into()),
                _ => style.bg_color.into(),
            };
            cells.push(CellSnapshot {
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
                selected: false,
                spacer_tail,
                semantic_content: cell.semantic_content()?.into(),
                hyperlink: if cell.has_hyperlink()? {
                    self.history_hyperlink(&reference, links)
                } else {
                    None
                },
            });
        }
        let detected = crate::terminal::hyperlink::detect_url_cells(
            cells.iter().map(|cell| cell.text.as_str()),
        );
        for (cell, detected) in cells.iter_mut().zip(detected) {
            if cell.hyperlink.is_none() {
                cell.hyperlink = detected;
            }
        }
        Ok((cells.into(), wrapped))
    }

    /// Resolves a cell's hyperlink as a snapshot does, sharing targets within one page.
    fn history_hyperlink(
        &self,
        reference: &GridRef<'_>,
        links: &mut Links,
    ) -> Option<Arc<crate::terminal::HyperlinkTarget>> {
        let mut uri = [0; crate::terminal::hyperlink::MAX_LINK_BYTES];
        let mut userdata = [0; crate::terminal::hyperlink::MAX_LINK_BYTES];
        let uri_length = reference.hyperlink_uri(&mut uri).ok()?;
        let userdata_length = reference.hyperlink_userdata(&mut userdata).ok()?;
        let uri = std::str::from_utf8(&uri[..uri_length]).ok()?;
        if userdata_length == 0 {
            return links
                .web
                .entry(uri.to_owned())
                .or_insert_with(|| crate::terminal::HyperlinkTarget::url(uri).map(Arc::new))
                .clone();
        }
        if !has_file_scheme(uri.as_bytes()) {
            return None;
        }
        let userdata = &userdata[..userdata_length];
        links
            .local
            .entry(userdata.to_vec())
            .or_insert_with(|| {
                crate::terminal::HyperlinkTarget::from_local_emission_metadata(
                    userdata,
                    self.local_file_capabilities,
                    &self.local_file_emissions.borrow(),
                )
                .map(Arc::new)
            })
            .clone()
    }
}

#[derive(Default)]
struct Links {
    web: HashMap<String, Option<Arc<crate::terminal::HyperlinkTarget>>>,
    local: HashMap<Vec<u8>, Option<Arc<crate::terminal::HyperlinkTarget>>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize};

    fn emulator(columns: u16, rows: u16) -> TerminalEmulator {
        TerminalEmulator::new(TerminalGeometry::from_grid(
            CellGridSize::new(columns, rows),
            LogicalCellSize::new(10.0, 20.0),
            BackingScale::ONE,
        ))
        .unwrap()
    }

    /// Feeds numbered lines, taking a snapshot after each batch as the worker does when it
    /// publishes.
    fn feed_lines(emulator: &mut TerminalEmulator, lines: std::ops::Range<u64>) -> HistorySnapshot {
        for batch in lines.collect::<Vec<_>>().chunks(500) {
            for line in batch {
                emulator.feed(format!("line {line}\r\n").as_bytes());
            }
            emulator.snapshot().unwrap();
        }
        emulator.snapshot().unwrap();
        emulator.history.current
    }

    fn text(row: &RowSnapshot) -> String {
        row.iter()
            .filter(|cell| !cell.spacer_tail)
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    #[test]
    fn positions_stay_absolute_while_the_engine_discards_history() {
        let mut emulator = emulator(20, 4);
        emulator.snapshot().unwrap();
        let history = feed_lines(&mut emulator, 0..12_000);
        assert_eq!(history.epoch, 1, "discarding old rows keeps the epoch");
        assert!(history.oldest > 0, "the engine discarded rows: {history:?}");
        assert_eq!(
            history.total,
            12_000 - 3,
            "three numbered lines are still on screen"
        );
        let page = emulator
            .scrollback_page(history.epoch, history.oldest - 5, 10)
            .unwrap()
            .unwrap();
        assert_eq!(page.start, history.oldest, "discarded rows are left out");
        assert_eq!(page.rows.len(), 5);
        for (offset, (row, wrapped)) in page.rows.iter().enumerate() {
            assert_eq!(
                text(row),
                format!("line {}", history.oldest + offset as u64)
            );
            assert!(!wrapped);
        }
        let newest = emulator
            .scrollback_page(history.epoch, history.total - 1, 500)
            .unwrap()
            .unwrap();
        assert_eq!(newest.rows.len(), 1, "a page ends at the screen");
        assert_eq!(
            text(&newest.rows[0].0),
            format!("line {}", history.total - 1)
        );
    }

    #[test]
    fn rewrapping_history_starts_a_new_epoch_and_clearing_it_discards_every_position() {
        let mut emulator = emulator(20, 4);
        emulator.snapshot().unwrap();
        let history = feed_lines(&mut emulator, 0..50);
        assert_eq!((history.epoch, history.oldest, history.total), (1, 0, 47));
        emulator
            .resize(TerminalGeometry::from_grid(
                CellGridSize::new(20, 6),
                LogicalCellSize::new(10.0, 20.0),
                BackingScale::ONE,
            ))
            .unwrap();
        emulator.snapshot().unwrap();
        let taller = emulator.history.current;
        assert_eq!(
            taller.epoch, 2,
            "history pulled onto a taller screen renumbers what scrolls off again"
        );
        emulator
            .resize(TerminalGeometry::from_grid(
                CellGridSize::new(20, 4),
                LogicalCellSize::new(10.0, 20.0),
                BackingScale::ONE,
            ))
            .unwrap();
        emulator.snapshot().unwrap();
        assert_eq!(
            emulator.history.current.epoch, 2,
            "a shorter screen keeps positions"
        );
        emulator
            .resize(TerminalGeometry::from_grid(
                CellGridSize::new(12, 6),
                LogicalCellSize::new(10.0, 20.0),
                BackingScale::ONE,
            ))
            .unwrap();
        emulator.snapshot().unwrap();
        assert_eq!(
            emulator.history.current.epoch, 3,
            "rewrapped rows name new content"
        );
        assert!(
            emulator.scrollback_page(2, 0, 10).unwrap().is_none(),
            "an ended epoch is refused"
        );
        let rewrapped = emulator.history.current;
        emulator.feed(b"\x1b[3J");
        emulator.snapshot().unwrap();
        let cleared = emulator.history.current;
        assert_eq!(cleared.epoch, rewrapped.epoch);
        assert_eq!(
            (cleared.oldest, cleared.total),
            (rewrapped.total, rewrapped.total),
            "erased positions are never reused"
        );
        let page = emulator
            .scrollback_page(cleared.epoch, 0, 10)
            .unwrap()
            .unwrap();
        assert!(page.rows.is_empty());
        emulator.feed(b"\x1bc");
        emulator.snapshot().unwrap();
        assert_eq!(emulator.history.current.epoch, 4, "a reset starts over");
    }

    #[test]
    fn pages_keep_wide_characters_and_soft_wraps_and_the_alternate_screen_hides_them() {
        let mut emulator = emulator(6, 2);
        emulator.snapshot().unwrap();
        emulator.feed("ab界cdef\r\nx\r\ny\r\nz".as_bytes());
        emulator.snapshot().unwrap();
        let history = emulator.history.current;
        let page = emulator
            .scrollback_page(history.epoch, history.oldest, 3)
            .unwrap()
            .unwrap();
        let (first, wrapped) = &page.rows[0];
        assert!(*wrapped);
        assert_eq!(first[2].text, "界");
        assert!(first[3].spacer_tail);
        assert_eq!(text(first), "ab界cd");
        assert_eq!(text(&page.rows[1].0), "ef");
        emulator.feed(b"\x1b[?1049h");
        emulator.snapshot().unwrap();
        assert_eq!(
            emulator.history.current, history,
            "the primary history stands"
        );
        assert!(
            emulator
                .scrollback_page(history.epoch, history.oldest, 3)
                .unwrap()
                .is_none()
        );
        emulator.feed(b"\x1b[?1049l");
        emulator.snapshot().unwrap();
        assert!(
            emulator
                .scrollback_page(history.epoch, history.oldest, 3)
                .unwrap()
                .is_some()
        );
    }
}
