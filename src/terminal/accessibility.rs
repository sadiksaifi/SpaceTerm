use std::ops::Range;

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
pub(crate) struct TerminalAccessibilityModel {
    text: String,
    lines: Vec<Range<usize>>,
    cells: Vec<CellMapping>,
    visible_lines: Range<usize>,
    visible_range: Range<usize>,
    selection: Option<Range<usize>>,
    cursor: Range<usize>,
}

impl TerminalAccessibilityModel {
    pub(crate) fn from_screen(screen: &super::ScreenSnapshot) -> Self {
        let lines = screen
            .rows
            .iter()
            .map(|row| {
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
                AccessibilityLine::new(cells, false)
            })
            .collect::<Vec<_>>();
        let visible = 0..lines.len();
        let cursor = screen
            .cursor
            .position
            .map(|position| (usize::from(position.row), position.column));
        Self::new(lines, visible, cursor)
    }

    pub(crate) fn new(
        lines: Vec<AccessibilityLine>,
        visible_lines: Range<usize>,
        cursor: Option<(usize, u16)>,
    ) -> Self {
        let visible_lines =
            visible_lines.start.min(lines.len())..visible_lines.end.min(lines.len());
        let mut text = String::new();
        let mut line_ranges = Vec::with_capacity(lines.len());
        let mut mappings = Vec::new();
        let mut selection_start = None;
        let mut selection_end = None;
        let mut utf16_offset = 0;

        for (line_index, line) in lines.iter().enumerate() {
            let line_start = utf16_offset;
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

        Self {
            text,
            lines: line_ranges,
            cells: mappings,
            visible_lines,
            visible_range,
            selection: selection_start
                .zip(selection_end)
                .map(|(start, end)| start..end),
            cursor: cursor_offset..cursor_offset,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn visible_range(&self) -> Range<usize> {
        self.visible_range.clone()
    }

    pub(crate) fn selection_range(&self) -> Option<Range<usize>> {
        self.selection.clone()
    }

    pub(crate) fn cursor_range(&self) -> Range<usize> {
        self.cursor.clone()
    }

    pub(crate) fn range_for_line(&self, line: usize) -> Option<Range<usize>> {
        self.lines.get(line).cloned()
    }

    pub(crate) fn line_for_index(&self, index: usize) -> Option<usize> {
        (0..self.lines.len()).find(|line| {
            self.range_for_line(*line).is_some_and(|range| {
                range.contains(&index) || (index == range.end && range.is_empty())
            })
        })
    }

    pub(crate) fn text_for_range(&self, range: Range<usize>) -> Option<&str> {
        let start = utf16_to_byte(&self.text, range.start)?;
        let end = utf16_to_byte(&self.text, range.end)?;
        self.text.get(start..end)
    }

    pub(crate) fn bounds_for_range(
        &self,
        range: Range<usize>,
        geometry: AccessibilityGeometry,
    ) -> Option<(f32, f32, f32, f32)> {
        let first = self
            .cells
            .iter()
            .find(|cell| cell.utf16.start < range.end && cell.utf16.end > range.start)?;
        let last = self.cells.iter().rev().find(|cell| {
            cell.line == first.line && cell.utf16.start < range.end && cell.utf16.end > range.start
        })?;
        let visible_row = first.line.checked_sub(self.visible_lines.start)?;
        (first.line < self.visible_lines.end).then_some((
            geometry.origin_x + f32::from(first.columns.start) * geometry.cell_width,
            geometry.origin_y + visible_row as f32 * geometry.line_height,
            f32::from(last.columns.end - first.columns.start) * geometry.cell_width,
            geometry.line_height,
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
        let line = self.visible_lines.start
            + ((y - geometry.origin_y) / geometry.line_height).floor() as usize;
        let column = ((x - geometry.origin_x) / geometry.cell_width).floor() as u16;
        self.cells
            .iter()
            .find(|cell| cell.line == line && cell.columns.contains(&column))
            .map(|cell| cell.utf16.start)
    }
}

fn utf16_to_byte(text: &str, target: usize) -> Option<usize> {
    let mut utf16 = 0;
    for (byte, character) in text.char_indices() {
        if utf16 == target {
            return Some(byte);
        }
        utf16 += character.len_utf16();
        if utf16 > target {
            return None;
        }
    }
    (utf16 == target).then_some(text.len())
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
