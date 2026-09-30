//! Displayed Shortcut text whose key glyphs share one optical center.
use std::ops::Range;

use gpui::{App, IntoElement, Pixels, RenderOnce, SharedString, Window, div, prelude::*, px};

/// Shows a displayed Shortcut in the inherited text style.
///
/// A key glyph the shortcut font lacks comes from a fallback font with its own vertical design,
/// such as Return from Lucida Grande beside Command from SF. The label moves each such glyph so
/// its ink center meets the shortcut font's cap-height center, where the font's own key glyphs
/// sit. Glyphs the font provides keep their designed position, so a raised Control caret stays
/// raised.
#[derive(IntoElement)]
pub struct ShortcutLabel {
    text: SharedString,
}

impl ShortcutLabel {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self { text: text.into() }
    }
}

impl RenderOnce for ShortcutLabel {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let segments = optical_segments(&self.text, window);
        if segments.iter().all(|segment| segment.offset == px(0.0)) {
            return div().flex_none().child(self.text);
        }
        div()
            .flex()
            .flex_none()
            .children(segments.into_iter().map(|segment| {
                div()
                    .relative()
                    .top(segment.offset)
                    .child(self.text[segment.range].to_owned())
            }))
    }
}

/// A run of Shortcut text that paints `offset` below its designed position.
#[derive(Debug, PartialEq)]
struct Segment {
    range: Range<usize>,
    offset: Pixels,
}

fn optical_segments(text: &SharedString, window: &Window) -> Vec<Segment> {
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(&style.font());
    let cap_center = text_system.cap_height(font_id, font_size) / 2.0;
    let line = text_system.shape_line(text.clone(), font_size, &[style.to_run(text.len())], None);
    let mut offsets = Vec::new();
    for run in line.runs.iter().filter(|run| run.font_id != font_id) {
        for glyph in run.glyphs.iter().filter(|glyph| !glyph.is_emoji) {
            let Some(character) = text[glyph.index..].chars().next() else {
                continue;
            };
            let Ok(ink) = text_system.typographic_bounds(run.font_id, font_size, character) else {
                continue;
            };
            // Typographic bounds rise from the baseline, so a positive offset moves ink down.
            let ink_center = ink.origin.y + ink.size.height / 2.0;
            offsets.push((glyph.index, character.len_utf8(), ink_center - cap_center));
        }
    }
    segments(text.len(), offsets)
}

/// Splits `length` bytes of text into runs around each `(start, length, offset)` glyph, keeping
/// every other byte in an unmoved run.
fn segments(length: usize, mut offsets: Vec<(usize, usize, Pixels)>) -> Vec<Segment> {
    offsets.sort_by_key(|(start, _, _)| *start);
    let mut segments = Vec::new();
    let mut cursor = 0;
    for (start, glyph_length, offset) in offsets {
        if start < cursor {
            continue;
        }
        if start > cursor {
            segments.push(Segment {
                range: cursor..start,
                offset: px(0.0),
            });
        }
        cursor = start + glyph_length;
        segments.push(Segment {
            range: start..cursor,
            offset,
        });
    }
    if cursor < length {
        segments.push(Segment {
            range: cursor..length,
            offset: px(0.0),
        });
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shortcut_without_fallback_glyphs_should_stay_one_unmoved_run() {
        assert_eq!(
            segments(4, Vec::new()),
            vec![Segment {
                range: 0..4,
                offset: px(0.0),
            }]
        );
    }

    #[test]
    fn a_fallback_glyph_should_move_alone_between_unmoved_runs() {
        // "⇧⌘↩K": three three-byte key glyphs, then a letter.
        assert_eq!(
            segments(10, vec![(6, 3, px(-1.25))]),
            vec![
                Segment {
                    range: 0..6,
                    offset: px(0.0),
                },
                Segment {
                    range: 6..9,
                    offset: px(-1.25),
                },
                Segment {
                    range: 9..10,
                    offset: px(0.0),
                },
            ]
        );
    }
}
