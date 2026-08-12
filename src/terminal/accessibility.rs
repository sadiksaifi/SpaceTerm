use std::ops::Range;
use std::sync::Arc;

use super::PresentationGeneration;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AccessibilityCell {
    text: String,
    width_cells: u16,
    selected: bool,
}

impl AccessibilityCell {
    pub(crate) fn new(text: impl Into<String>, width_cells: u16, selected: bool) -> Self {
        Self {
            text: text.into(),
            width_cells: width_cells.max(1),
            selected,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AccessibilityLine {
    cells: Vec<AccessibilityCell>,
    soft_wrapped: bool,
}

impl AccessibilityLine {
    pub(crate) fn new(cells: Vec<AccessibilityCell>, soft_wrapped: bool) -> Self {
        Self {
            cells,
            soft_wrapped,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AccessibilityGeometry {
    origin_x: f32,
    origin_y: f32,
    cell_width: f32,
    line_height: f32,
}

impl AccessibilityGeometry {
    pub(crate) fn new(
        origin_x: f32,
        origin_y: f32,
        cell_width: f32,
        line_height: f32,
    ) -> Option<Self> {
        (origin_x.is_finite()
            && origin_y.is_finite()
            && cell_width.is_finite()
            && cell_width > 0.0
            && line_height.is_finite()
            && line_height > 0.0)
            .then_some(Self {
                origin_x,
                origin_y,
                cell_width,
                line_height,
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AccessibilityNotification {
    Value,
    Selection,
    Focus,
}

impl AccessibilityNotification {
    pub(crate) fn coalesce(notifications: &[Self]) -> Vec<Self> {
        [Self::Value, Self::Selection, Self::Focus]
            .into_iter()
            .filter(|candidate| notifications.contains(candidate))
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CellMapping {
    line: usize,
    columns: Range<u16>,
    utf16: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TerminalAccessibilityData {
    generation: PresentationGeneration,
    text: String,
    utf16_bytes: Vec<usize>,
    lines: Vec<Range<usize>>,
    line_cells: Vec<Range<usize>>,
    cells: Vec<CellMapping>,
    visible_lines: Range<usize>,
    visible_range: Range<usize>,
    selection: Option<Range<usize>>,
    cursor: Range<usize>,
    len_utf16: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AccessibilityCellPosition {
    pub(crate) row: u16,
    pub(crate) column: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AccessibilitySelectionRequest {
    pub(crate) generation: PresentationGeneration,
    pub(crate) endpoints: Option<(AccessibilityCellPosition, AccessibilityCellPosition)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalAccessibilityModel {
    data: Arc<TerminalAccessibilityData>,
}

impl TerminalAccessibilityModel {
    pub(crate) fn from_screen(screen: &super::ScreenSnapshot) -> Self {
        // ScreenSnapshot intentionally publishes only immutable viewport rows. Retained
        // Scrollback needs a separate bounded, incremental worker snapshot; walking the entire
        // retained grid for each presentation would make accessibility output-sized work.
        let lines = screen
            .rows
            .iter()
            .enumerate()
            .map(|(row_index, row)| {
                let mut cells = Vec::with_capacity(row.len());
                for (index, cell) in row.iter().enumerate() {
                    if cell.spacer_tail {
                        continue;
                    }
                    let width = if row.get(index + 1).is_some_and(|next| next.spacer_tail) {
                        2
                    } else {
                        1
                    };
                    cells.push(AccessibilityCell::new(
                        cell.text.clone(),
                        width,
                        cell.selected,
                    ));
                }
                AccessibilityLine::new(
                    cells,
                    screen
                        .row_soft_wrapped
                        .get(row_index)
                        .copied()
                        .unwrap_or(false),
                )
            })
            .collect::<Vec<_>>();
        let visible = 0..lines.len();
        let cursor = screen
            .cursor
            .position
            .map(|position| (usize::from(position.row), position.column));
        Self::new_with_generation(lines, visible, cursor, screen.generation)
    }

    #[cfg(test)]
    pub(crate) fn new(
        lines: Vec<AccessibilityLine>,
        visible_lines: Range<usize>,
        cursor: Option<(usize, u16)>,
    ) -> Self {
        Self::new_with_generation(
            lines,
            visible_lines,
            cursor,
            PresentationGeneration::default(),
        )
    }

    fn new_with_generation(
        lines: Vec<AccessibilityLine>,
        visible_lines: Range<usize>,
        cursor: Option<(usize, u16)>,
        generation: PresentationGeneration,
    ) -> Self {
        let visible_lines =
            visible_lines.start.min(lines.len())..visible_lines.end.min(lines.len());
        let mut text = String::new();
        let mut line_ranges = Vec::with_capacity(lines.len());
        let mut line_cells = Vec::with_capacity(lines.len());
        let mut mappings = Vec::new();
        let mut selection_start = None;
        let mut selection_end = None;
        let mut utf16_offset = 0;

        for (line_index, line) in lines.iter().enumerate() {
            let line_start = utf16_offset;
            let line_cell_start = mappings.len();
            let mut column = 0_u16;
            for cell in &line.cells {
                let cell_start = utf16_offset;
                text.push_str(&cell.text);
                utf16_offset += cell.text.encode_utf16().count();
                let cell_end = utf16_offset;
                let column_end = column.saturating_add(cell.width_cells);
                mappings.push(CellMapping {
                    line: line_index,
                    columns: column..column_end,
                    utf16: cell_start..cell_end,
                });
                if cell.selected {
                    selection_start.get_or_insert(cell_start);
                    selection_end = Some(cell_end);
                }
                column = column_end;
            }
            line_cells.push(line_cell_start..mappings.len());
            if !line.soft_wrapped && line_index + 1 < lines.len() {
                text.push('\n');
                utf16_offset += 1;
            }
            line_ranges.push(line_start..utf16_offset);
        }

        let visible_range = if visible_lines.is_empty() {
            0..0
        } else {
            line_ranges[visible_lines.start].start..line_ranges[visible_lines.end - 1].end
        };
        let cursor_offset = cursor
            .and_then(|(line, column)| {
                mappings
                    .iter()
                    .find(|mapping| {
                        mapping.line == line
                            && (mapping.columns.contains(&column) || mapping.columns.end == column)
                    })
                    .map(|mapping| {
                        if mapping.columns.end == column {
                            mapping.utf16.end
                        } else {
                            mapping.utf16.start
                        }
                    })
                    .or_else(|| line_ranges.get(line).map(|range| range.end))
            })
            .unwrap_or(visible_range.start);
        let utf16_bytes = utf16_byte_index(&text, utf16_offset);

        Self {
            data: Arc::new(TerminalAccessibilityData {
                generation,
                text,
                utf16_bytes,
                lines: line_ranges,
                line_cells,
                cells: mappings,
                visible_lines,
                visible_range,
                selection: selection_start
                    .zip(selection_end)
                    .map(|(start, end)| start..end),
                cursor: cursor_offset..cursor_offset,
                len_utf16: utf16_offset,
            }),
        }
    }

    pub(crate) fn shares_snapshot(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data, &other.data)
    }

    pub(crate) fn text(&self) -> &str {
        &self.data.text
    }

    pub(crate) fn len_utf16(&self) -> usize {
        self.data.len_utf16
    }

    pub(crate) fn visible_range(&self) -> Range<usize> {
        self.data.visible_range.clone()
    }

    pub(crate) fn selection_range(&self) -> Option<Range<usize>> {
        self.data.selection.clone()
    }

    pub(crate) fn cursor_range(&self) -> Range<usize> {
        self.data.cursor.clone()
    }

    pub(crate) fn selected_or_cursor_range(&self) -> Range<usize> {
        self.data
            .selection
            .clone()
            .unwrap_or_else(|| self.data.cursor.clone())
    }

    pub(crate) fn range_for_line(&self, line: usize) -> Option<Range<usize>> {
        self.data.lines.get(line).cloned()
    }

    pub(crate) fn line_for_index(&self, index: usize) -> Option<usize> {
        let after_line = self
            .data
            .lines
            .partition_point(|range| range.start <= index);
        let line = after_line.checked_sub(1)?;
        self.data
            .lines
            .get(line)
            .filter(|range| range.contains(&index) || (index == range.end && range.is_empty()))
            .map(|_| line)
            .or_else(|| {
                (index == self.len_utf16())
                    .then(|| self.data.lines.len().checked_sub(1))
                    .flatten()
            })
    }

    pub(crate) fn range_for_index(&self, index: usize) -> Option<Range<usize>> {
        let cell = self
            .data
            .cells
            .partition_point(|cell| cell.utf16.end <= index);
        self.data
            .cells
            .get(cell)
            .filter(|cell| cell.utf16.contains(&index))
            .map(|cell| cell.utf16.clone())
            .or_else(|| (index == self.len_utf16()).then_some(index..index))
            .or_else(|| (index < self.len_utf16()).then_some(index..index + 1))
    }

    pub(crate) fn text_for_range(&self, range: Range<usize>) -> Option<&str> {
        let start = self.byte_for_utf16(range.start)?;
        let end = self.byte_for_utf16(range.end)?;
        self.data.text.get(start..end)
    }

    fn byte_for_utf16(&self, index: usize) -> Option<usize> {
        self.data
            .utf16_bytes
            .get(index)
            .copied()
            .filter(|byte| *byte != usize::MAX)
    }

    pub(crate) fn selection_request(
        &self,
        range: Range<usize>,
    ) -> Option<AccessibilitySelectionRequest> {
        if range.start > range.end
            || range.end > self.len_utf16()
            || self.byte_for_utf16(range.start).is_none()
            || self.byte_for_utf16(range.end).is_none()
        {
            return None;
        }
        if range.is_empty() {
            return Some(AccessibilitySelectionRequest {
                generation: self.data.generation,
                endpoints: None,
            });
        }

        let first = self
            .data
            .cells
            .partition_point(|cell| cell.utf16.end <= range.start);
        let after_last = self
            .data
            .cells
            .partition_point(|cell| cell.utf16.start < range.end);
        let start = self.data.cells.get(first)?;
        let end = self.data.cells.get(after_last.checked_sub(1)?)?;
        (first < after_last).then_some(AccessibilitySelectionRequest {
            generation: self.data.generation,
            endpoints: Some((
                AccessibilityCellPosition {
                    row: u16::try_from(start.line).ok()?,
                    column: start.columns.start,
                },
                AccessibilityCellPosition {
                    row: u16::try_from(end.line).ok()?,
                    column: end.columns.start,
                },
            )),
        })
    }

    pub(crate) fn bounds_for_range(
        &self,
        range: Range<usize>,
        geometry: AccessibilityGeometry,
    ) -> Option<(f32, f32, f32, f32)> {
        if range.start > range.end
            || range.start < self.data.visible_range.start
            || range.end > self.data.visible_range.end
        {
            return None;
        }
        if range.is_empty() {
            let line = self.line_for_index(range.start)?;
            let visible_row = line.checked_sub(self.data.visible_lines.start)?;
            (line < self.data.visible_lines.end).then_some(())?;
            let column = self
                .data
                .line_cells
                .get(line)
                .and_then(|range| self.data.cells.get(range.clone()))
                .into_iter()
                .flatten()
                .find_map(|cell| {
                    if cell.utf16.contains(&range.start) {
                        Some(cell.columns.start)
                    } else if cell.utf16.end == range.start {
                        Some(cell.columns.end)
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            return Some((
                geometry.origin_x + f32::from(column) * geometry.cell_width,
                geometry.origin_y + visible_row as f32 * geometry.line_height,
                0.0,
                geometry.line_height,
            ));
        }
        let first_cell = self
            .data
            .cells
            .partition_point(|cell| cell.utf16.end <= range.start);
        let after_last = self
            .data
            .cells
            .partition_point(|cell| cell.utf16.start < range.end);
        let mut cells = self.data.cells[first_cell..after_last]
            .iter()
            .filter(|cell| {
                cell.utf16.start < range.end
                    && cell.utf16.end > range.start
                    && cell.line >= self.data.visible_lines.start
                    && cell.line < self.data.visible_lines.end
            });
        let Some(first) = cells.next() else {
            return self.bounds_for_range(range.start..range.start, geometry);
        };
        let mut first_line = first.line;
        let mut last_line = first.line;
        let mut first_column = first.columns.start;
        let mut last_column = first.columns.end;
        for cell in cells {
            first_line = first_line.min(cell.line);
            last_line = last_line.max(cell.line);
            first_column = first_column.min(cell.columns.start);
            last_column = last_column.max(cell.columns.end);
        }
        let visible_row = first_line - self.data.visible_lines.start;
        Some((
            geometry.origin_x + f32::from(first_column) * geometry.cell_width,
            geometry.origin_y + visible_row as f32 * geometry.line_height,
            f32::from(last_column - first_column) * geometry.cell_width,
            (last_line - first_line + 1) as f32 * geometry.line_height,
        ))
    }

    pub(crate) fn index_for_point(
        &self,
        x: f32,
        y: f32,
        geometry: AccessibilityGeometry,
    ) -> Option<usize> {
        if x < geometry.origin_x || y < geometry.origin_y {
            return None;
        }
        let line = self.data.visible_lines.start
            + ((y - geometry.origin_y) / geometry.line_height).floor() as usize;
        let column = ((x - geometry.origin_x) / geometry.cell_width).floor() as u16;
        let cells = self
            .data
            .line_cells
            .get(line)
            .and_then(|range| self.data.cells.get(range.clone()))?;
        let cell = cells.partition_point(|cell| cell.columns.end <= column);
        cells
            .get(cell)
            .filter(|cell| cell.columns.contains(&column))
            .map(|cell| cell.utf16.start)
    }

    pub(crate) fn range_for_point(
        &self,
        x: f32,
        y: f32,
        geometry: AccessibilityGeometry,
    ) -> Option<Range<usize>> {
        let index = self.index_for_point(x, y, geometry)?;
        self.range_for_index(index)
    }
}

fn utf16_byte_index(text: &str, len_utf16: usize) -> Vec<usize> {
    let mut index = vec![usize::MAX; len_utf16.saturating_add(1)];
    let mut utf16 = 0_usize;
    index[0] = 0;
    for (byte, character) in text.char_indices() {
        index[utf16] = byte;
        utf16 += character.len_utf16();
        index[utf16] = byte + character.len_utf8();
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(text: &str, width_cells: u16, selected: bool) -> AccessibilityCell {
        AccessibilityCell::new(text, width_cells, selected)
    }

    #[test]
    fn utf16_ranges_preserve_wide_combining_emoji_and_soft_wraps() {
        let model = TerminalAccessibilityModel::new(
            vec![
                AccessibilityLine::new(vec![cell("A\u{301}", 1, false), cell("😀", 2, true)], true),
                AccessibilityLine::new(vec![cell("界", 2, true), cell("x", 1, false)], false),
                AccessibilityLine::new(vec![cell("scrollback", 10, false)], false),
            ],
            1..3,
            Some((1, 2)),
        );

        assert_eq!(model.text(), "A\u{301}😀界x\nscrollback");
        assert_eq!(model.range_for_line(0), Some(0..4));
        assert_eq!(model.range_for_line(1), Some(4..7));
        assert_eq!(model.selection_range(), Some(2..5));
        assert_eq!(model.cursor_range(), 5..5);
        assert_eq!(model.visible_range(), 4..17);
    }

    #[test]
    fn line_and_range_conversion_are_stable_at_utf16_boundaries() {
        let model = TerminalAccessibilityModel::new(
            vec![
                AccessibilityLine::new(vec![cell("😀a", 2, false)], false),
                AccessibilityLine::new(vec![cell("b", 1, false)], false),
            ],
            0..2,
            None,
        );
        assert_eq!(model.line_for_index(0), Some(0));
        assert_eq!(model.line_for_index(2), Some(0));
        assert_eq!(model.line_for_index(4), Some(1));
        assert_eq!(model.range_for_index(3), Some(3..4));
        assert_eq!(model.text_for_range(1..2), None);
        assert_eq!(model.text_for_range(0..2), Some("😀"));
    }

    #[test]
    fn bounds_and_hit_testing_share_logical_cell_geometry() {
        let model = TerminalAccessibilityModel::new(
            vec![AccessibilityLine::new(
                vec![cell("a", 1, false), cell("😀", 2, false)],
                false,
            )],
            0..1,
            None,
        );
        let geometry = AccessibilityGeometry::new(4.0, 8.0, 10.0, 20.0).unwrap();
        assert_eq!(
            model.bounds_for_range(1..3, geometry),
            Some((14.0, 8.0, 20.0, 20.0))
        );
        assert_eq!(model.index_for_point(24.0, 18.0, geometry), Some(1));
        assert_eq!(model.range_for_point(24.0, 18.0, geometry), Some(1..3));
        assert_eq!(
            model.bounds_for_range(3..3, geometry),
            Some((34.0, 8.0, 0.0, 20.0))
        );
    }

    #[test]
    fn range_bounds_cover_visible_rows_and_reject_retained_offscreen_text() {
        let model = TerminalAccessibilityModel::new(
            vec![
                AccessibilityLine::new(vec![cell("abc", 3, false)], false),
                AccessibilityLine::new(vec![cell("d", 1, false)], false),
            ],
            0..2,
            None,
        );
        let geometry = AccessibilityGeometry::new(0.0, 0.0, 10.0, 20.0).unwrap();
        assert_eq!(
            model.bounds_for_range(0..5, geometry),
            Some((0.0, 0.0, 30.0, 40.0))
        );

        let retained = TerminalAccessibilityModel::new(
            vec![
                AccessibilityLine::new(vec![cell("old", 3, false)], false),
                AccessibilityLine::new(vec![cell("visible", 7, false)], false),
            ],
            1..2,
            None,
        );
        assert_eq!(retained.bounds_for_range(0..3, geometry), None);
    }

    #[test]
    fn cursor_bounds_exist_on_an_empty_line() {
        let model = TerminalAccessibilityModel::new(
            vec![
                AccessibilityLine::new(Vec::new(), false),
                AccessibilityLine::new(Vec::new(), false),
            ],
            0..2,
            Some((1, 0)),
        );
        let geometry = AccessibilityGeometry::new(4.0, 8.0, 10.0, 20.0).unwrap();

        assert_eq!(model.cursor_range(), 1..1);
        assert_eq!(
            model.bounds_for_range(1..1, geometry),
            Some((4.0, 28.0, 0.0, 20.0))
        );
    }

    #[test]
    fn selection_request_binds_utf16_cells_to_its_presentation_generation() {
        let model = TerminalAccessibilityModel::new_with_generation(
            vec![AccessibilityLine::new(
                vec![
                    cell("A", 1, false),
                    cell("😀", 2, false),
                    cell("B", 1, false),
                ],
                false,
            )],
            0..1,
            None,
            PresentationGeneration::test(7),
        );

        assert_eq!(
            model.selection_request(1..3),
            Some(AccessibilitySelectionRequest {
                generation: PresentationGeneration::test(7),
                endpoints: Some((
                    AccessibilityCellPosition { row: 0, column: 1 },
                    AccessibilityCellPosition { row: 0, column: 1 },
                )),
            })
        );
    }

    #[test]
    fn accessibility_model_clone_reuses_the_immutable_index() {
        let model = TerminalAccessibilityModel::new(
            vec![AccessibilityLine::new(vec![cell("text", 4, false)], false)],
            0..1,
            None,
        );

        assert!(model.shares_snapshot(&model.clone()));
    }

    #[test]
    fn indexed_line_lookup_handles_consecutive_empty_lines() {
        let model = TerminalAccessibilityModel::new(
            vec![
                AccessibilityLine::new(Vec::new(), false),
                AccessibilityLine::new(Vec::new(), false),
                AccessibilityLine::new(vec![cell("x", 1, false)], false),
            ],
            0..3,
            None,
        );

        assert_eq!(model.line_for_index(1), Some(1));
    }

    #[test]
    fn notifications_are_typed_and_coalesced() {
        assert_eq!(
            AccessibilityNotification::coalesce(&[
                AccessibilityNotification::Value,
                AccessibilityNotification::Selection,
                AccessibilityNotification::Value,
                AccessibilityNotification::Focus,
            ]),
            vec![
                AccessibilityNotification::Value,
                AccessibilityNotification::Selection,
                AccessibilityNotification::Focus,
            ]
        );
    }
}
