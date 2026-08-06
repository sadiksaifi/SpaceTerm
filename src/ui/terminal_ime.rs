use std::ops::Range;

use libghostty_vt::unicode::grapheme_width;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct TerminalIme {
    marked: Option<MarkedText>,
    pending_commit: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MarkedText {
    text: String,
    selected_utf16: Range<usize>,
}

impl TerminalIme {
    pub(super) fn marked_text(&self) -> Option<&str> {
        self.marked.as_ref().map(|marked| marked.text.as_str())
    }

    pub(super) fn marked_range(&self) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|marked| 0..marked.text.encode_utf16().count())
    }

    pub(super) fn selected_range(&self) -> Range<usize> {
        self.marked
            .as_ref()
            .map_or(0..0, |marked| marked.selected_utf16.clone())
    }

    pub(super) fn text_for_utf16_range(
        &self,
        range_utf16: Range<usize>,
    ) -> Option<(String, Range<usize>)> {
        let marked = self.marked.as_ref()?;
        let bytes = utf16_range_to_bytes(&marked.text, range_utf16);
        let adjusted = byte_offset_to_utf16(&marked.text, bytes.start)
            ..byte_offset_to_utf16(&marked.text, bytes.end);
        Some((marked.text[bytes].to_owned(), adjusted))
    }

    pub(super) fn replace_and_mark(
        &mut self,
        replacement_utf16: Option<Range<usize>>,
        new_text: &str,
        selected_utf16: Option<Range<usize>>,
    ) {
        let existing = self
            .marked
            .take()
            .map_or_else(String::new, |marked| marked.text);
        let replacement_utf16 =
            replacement_utf16.unwrap_or_else(|| 0..existing.encode_utf16().count());
        let replacement = utf16_range_to_bytes(&existing, replacement_utf16);
        let replacement_start_utf16 = byte_offset_to_utf16(&existing, replacement.start);
        let new_text_utf16_len = new_text.encode_utf16().count();
        let relative_selection = normalized_utf16_range(
            new_text,
            selected_utf16.unwrap_or(new_text_utf16_len..new_text_utf16_len),
        );
        let mut text = existing;
        text.replace_range(replacement, new_text);

        if text.is_empty() {
            self.marked = None;
            return;
        }

        let selected_utf16 = normalized_utf16_range(
            &text,
            replacement_start_utf16 + relative_selection.start
                ..replacement_start_utf16 + relative_selection.end,
        );
        self.marked = Some(MarkedText {
            text,
            selected_utf16,
        });
    }

    pub(super) fn cancel(&mut self) {
        self.marked = None;
        self.pending_commit = None;
    }

    pub(super) fn commit(&mut self, text: &str) {
        self.marked = None;
        self.pending_commit = (!text.is_empty()).then(|| text.to_owned());
    }

    pub(super) fn take_commit(&mut self) -> Option<String> {
        self.pending_commit.take()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PreeditLayout {
    pub(super) clusters: Vec<PreeditCluster>,
    pub(super) caret: PreeditPosition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PreeditCluster {
    pub(super) text: String,
    pub(super) row: usize,
    pub(super) column: usize,
    pub(super) width: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PreeditPosition {
    pub(super) row: usize,
    pub(super) column: usize,
}

pub(super) fn layout_preedit(
    text: &str,
    start_row: usize,
    start_column: usize,
    columns: usize,
    caret_utf16: usize,
) -> PreeditLayout {
    let columns = columns.max(1);
    let mut row = start_row + start_column / columns;
    let mut column = start_column % columns;
    let mut clusters = Vec::new();
    let mut caret = (caret_utf16 == 0).then_some(PreeditPosition { row, column });
    let characters = text.chars().collect::<Vec<_>>();
    let mut character_index = 0;
    let mut utf16_index = 0;

    while character_index < characters.len() {
        let (consumed, width) = grapheme_width(&characters[character_index..]);
        let consumed = consumed.max(1).min(characters.len() - character_index);
        let cluster_characters = &characters[character_index..character_index + consumed];
        let cluster_text = cluster_characters.iter().collect::<String>();
        let cluster_utf16 = cluster_text.encode_utf16().count();
        let width = width.min(u8::try_from(columns).unwrap_or(u8::MAX));

        if width > 0 && column + usize::from(width) > columns {
            row += 1;
            column = 0;
        }
        if caret.is_none() && caret_utf16 <= utf16_index {
            caret = Some(PreeditPosition { row, column });
        }

        clusters.push(PreeditCluster {
            text: cluster_text,
            row,
            column,
            width,
        });

        column += usize::from(width);
        if column == columns {
            row += 1;
            column = 0;
        }
        utf16_index += cluster_utf16;
        if caret.is_none() && caret_utf16 <= utf16_index {
            caret = Some(PreeditPosition { row, column });
        }
        character_index += consumed;
    }

    PreeditLayout {
        clusters,
        caret: caret.unwrap_or(PreeditPosition { row, column }),
    }
}

fn normalized_utf16_range(text: &str, range: Range<usize>) -> Range<usize> {
    let bytes = utf16_range_to_bytes(text, range);
    byte_offset_to_utf16(text, bytes.start)..byte_offset_to_utf16(text, bytes.end)
}

fn utf16_range_to_bytes(text: &str, range: Range<usize>) -> Range<usize> {
    let start = utf16_offset_to_byte(text, range.start, false);
    if range.is_empty() {
        return start..start;
    }
    let end = utf16_offset_to_byte(text, range.end.max(range.start), true);
    start.min(end)..end
}

fn utf16_offset_to_byte(text: &str, offset: usize, round_up: bool) -> usize {
    let mut utf16_index = 0;
    for (byte_index, character) in text.char_indices() {
        if offset <= utf16_index {
            return byte_index;
        }
        let next_utf16 = utf16_index + character.len_utf16();
        if offset < next_utf16 {
            return if round_up {
                byte_index + character.len_utf8()
            } else {
                byte_index
            };
        }
        utf16_index = next_utf16;
    }
    text.len()
}

fn byte_offset_to_utf16(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].encode_utf16().count()
}

#[cfg(test)]
pub(crate) fn conformance_ime_observation() -> String {
    let mut ime = TerminalIme::default();
    ime.replace_and_mark(None, "A😀B", Some(1..3));
    ime.replace_and_mark(Some(1..3), "界", Some(1..1));
    let marked = ime.marked_text().unwrap_or_default().to_owned();
    let selection = ime.selected_range();
    let layout = layout_preedit(&marked, 0, 3, 4, selection.end);
    ime.commit(&marked);
    format!(
        "marked={marked} selection={selection:?} clusters={:?} caret={:?} commit={:?}",
        layout.clusters,
        layout.caret,
        ime.take_commit()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marked_text_updates_are_local_until_commit() {
        let mut ime = TerminalIme::default();

        ime.replace_and_mark(None, "にほん", Some(2..3));

        assert_eq!(ime.marked_text(), Some("にほん"));
        assert_eq!(ime.marked_range(), Some(0..3));
        assert_eq!(ime.selected_range(), 2..3);
        assert!(ime.take_commit().is_none());
    }

    #[test]
    fn marked_replacement_ranges_use_utf16_and_preserve_scalar_boundaries() {
        let mut ime = TerminalIme::default();
        ime.replace_and_mark(None, "A😀B", Some(3..3));

        ime.replace_and_mark(Some(1..3), "界", Some(1..1));

        assert_eq!(ime.marked_text(), Some("A界B"));
        assert_eq!(ime.selected_range(), 2..2);
    }

    #[test]
    fn replacement_selection_is_relative_to_inserted_marked_text() {
        let mut ime = TerminalIme::default();
        ime.replace_and_mark(None, "ab", Some(2..2));

        ime.replace_and_mark(Some(1..2), "界x", Some(1..1));

        assert_eq!(ime.marked_text(), Some("a界x"));
        assert_eq!(ime.selected_range(), 2..2);
    }

    #[test]
    fn collapsed_utf16_ranges_never_split_surrogate_pairs() {
        let mut ime = TerminalIme::default();
        ime.replace_and_mark(None, "A😀B", Some(2..2));

        assert_eq!(ime.marked_text(), Some("A😀B"));
        assert_eq!(ime.selected_range(), 1..1);
    }

    #[test]
    fn marked_substrings_report_adjusted_utf16_ranges() {
        let mut ime = TerminalIme::default();
        ime.replace_and_mark(None, "A😀B", Some(3..3));

        assert_eq!(ime.text_for_utf16_range(2..2), Some(("".to_owned(), 1..1)));
        assert_eq!(
            ime.text_for_utf16_range(1..3),
            Some(("😀".to_owned(), 1..3))
        );
    }

    #[test]
    fn cancel_discards_preedit_without_a_commit() {
        let mut ime = TerminalIme::default();
        ime.replace_and_mark(None, "한", Some(1..1));

        ime.cancel();

        assert_eq!(ime.marked_text(), None);
        assert!(ime.take_commit().is_none());
    }

    #[test]
    fn commit_clears_preedit_and_is_taken_exactly_once() {
        let mut ime = TerminalIme::default();
        ime.replace_and_mark(None, "中文", Some(2..2));

        ime.commit("中文");

        assert_eq!(ime.marked_text(), None);
        assert_eq!(ime.take_commit().as_deref(), Some("中文"));
        assert!(ime.take_commit().is_none());
    }

    #[test]
    fn wide_preedit_clusters_wrap_before_the_right_edge() {
        let layout = layout_preedit("a界b", 0, 3, 5, 3);

        assert_eq!(
            layout
                .clusters
                .iter()
                .map(|cluster| (
                    cluster.text.as_str(),
                    cluster.row,
                    cluster.column,
                    cluster.width
                ))
                .collect::<Vec<_>>(),
            vec![("a", 0, 3, 1), ("界", 1, 0, 2), ("b", 1, 2, 1)]
        );
        assert_eq!((layout.caret.row, layout.caret.column), (1, 3));
    }

    #[test]
    fn combining_and_emoji_sequences_are_laid_out_as_complete_clusters() {
        let layout = layout_preedit("e\u{301}👩\u{200d}💻", 2, 1, 8, 7);

        assert_eq!(layout.clusters.len(), 2);
        assert_eq!(layout.clusters[0].text, "e\u{301}");
        assert_eq!(layout.clusters[0].width, 1);
        assert_eq!(layout.clusters[1].text, "👩\u{200d}💻");
        assert_eq!(layout.clusters[1].width, 2);
        assert_eq!((layout.caret.row, layout.caret.column), (2, 4));
    }
}
