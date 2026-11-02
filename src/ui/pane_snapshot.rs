//! A frozen, reduced picture of a Pane's terminal screen, which a Pane or Tab drag carries.
//!
//! The picture is taken once, when the drag starts, from the screen the Pane already holds. It
//! keeps the top rows and left columns that fit the drag's rectangle, with their text colors, and
//! never follows later output.

use std::ops::Range;

use gpui::{
    Font, HighlightStyle, IntoElement, ParentElement, Pixels, SharedString, Styled, StyledText, div,
};

use crate::appearance::Color;
use crate::terminal::{CellSnapshot, RowSnapshot, ScreenSnapshot};

use super::appearance::gpui_color;

/// The most rows and columns a snapshot keeps. The drag's rectangle shows no more than this.
const ROWS: usize = 18;
const COLUMNS: usize = 96;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PaneSnapshot {
    lines: Vec<SnapshotLine>,
    font: Font,
}

#[derive(Clone, Debug, PartialEq)]
struct SnapshotLine {
    text: SharedString,
    /// Byte ranges of `text` and the color each one paints in.
    runs: Vec<(Range<usize>, gpui::Rgba)>,
}

impl PaneSnapshot {
    /// The top-left of `screen`, drawn in `font`.
    pub(crate) fn capture(screen: &ScreenSnapshot, font: Font) -> Self {
        Self::from_rows(&screen.rows, font, |cell| {
            super::terminal_element::effective_colors(cell, &screen.colors).0
        })
    }

    fn from_rows(rows: &[RowSnapshot], font: Font, color: impl Fn(&CellSnapshot) -> Color) -> Self {
        let mut lines: Vec<SnapshotLine> = rows
            .iter()
            .take(ROWS)
            .map(|row| {
                let mut text = String::new();
                let mut runs: Vec<(Range<usize>, gpui::Rgba)> = Vec::new();
                for cell in row.iter().filter(|cell| !cell.spacer_tail).take(COLUMNS) {
                    let start = text.len();
                    if cell.invisible || cell.text.is_empty() {
                        text.push(' ');
                        continue;
                    }
                    text.push_str(&cell.text);
                    if cell.text.trim().is_empty() {
                        continue;
                    }
                    let paint = gpui_color(color(cell));
                    match runs.last_mut() {
                        Some((range, last)) if *last == paint => range.end = text.len(),
                        _ => runs.push((start..text.len(), paint)),
                    }
                }
                text.truncate(text.trim_end().len());
                SnapshotLine {
                    text: text.into(),
                    runs,
                }
            })
            .collect();
        while lines.last().is_some_and(|line| line.text.is_empty()) {
            lines.pop();
        }
        Self { lines, font }
    }

    /// The snapshot as lines of `size` text, one `line_height` apart, clipped by its container.
    pub(crate) fn render(&self, size: Pixels, line_height: Pixels) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .overflow_hidden()
            .whitespace_nowrap()
            .font(self.font.clone())
            .text_size(size)
            .line_height(line_height)
            .children(self.lines.iter().map(|line| {
                div()
                    .h(line_height)
                    .child(StyledText::new(line.text.clone()).with_highlights(
                        line.runs.iter().map(|(range, color)| {
                            (
                                range.clone(),
                                HighlightStyle {
                                    color: Some((*color).into()),
                                    ..HighlightStyle::default()
                                },
                            )
                        }),
                    ))
            }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::terminal::TerminalColor;

    fn cell(text: &str, color: u8) -> CellSnapshot {
        CellSnapshot {
            text: text.into(),
            foreground_source: TerminalColor::Palette(color),
            background_source: TerminalColor::Default,
            inverse: false,
            bold: false,
            faint: false,
            italic: false,
            blinking: false,
            invisible: false,
            underline: Default::default(),
            underline_source: TerminalColor::Default,
            strikethrough: false,
            overline: false,
            selected: false,
            spacer_tail: false,
            semantic_content: Default::default(),
            hyperlink: None,
        }
    }

    fn row(cells: Vec<CellSnapshot>) -> RowSnapshot {
        Arc::from(cells)
    }

    #[test]
    fn capture_should_join_same_colored_cells_and_trim_blank_ends() {
        let palette = |cell: &CellSnapshot| match cell.foreground_source {
            TerminalColor::Palette(index) => Color::rgb(u32::from(index)),
            _ => Color::BLACK,
        };
        let rows = [
            row(vec![
                cell("❯", 2),
                cell(" ", 7),
                cell("l", 7),
                cell("s", 7),
                cell(" ", 7),
                cell(" ", 7),
            ]),
            row(vec![cell(" ", 7), cell(" ", 7)]),
            row(vec![cell("a", 1), cell(" ", 1), cell("b", 1)]),
            row(vec![cell(" ", 7)]),
        ];

        let snapshot = PaneSnapshot::from_rows(&rows, gpui::font("Menlo"), palette);

        let lines: Vec<_> = snapshot
            .lines
            .iter()
            .map(|line| {
                (
                    line.text.to_string(),
                    line.runs
                        .iter()
                        .map(|(range, color)| (range.clone(), *color))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            lines,
            vec![
                (
                    "❯ ls".to_owned(),
                    vec![
                        (0.."❯".len(), gpui_color(Color::rgb(2))),
                        ("❯ ".len().."❯ ls".len(), gpui_color(Color::rgb(7))),
                    ]
                ),
                (String::new(), vec![]),
                ("a b".to_owned(), vec![(0..3, gpui_color(Color::rgb(1)))]),
            ],
            "colors run across spaces of the same color, and trailing blank rows go"
        );
    }
}
