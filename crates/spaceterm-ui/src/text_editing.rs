//! The editing core shared by SpaceTerm's text controls.
//!
//! A buffer owns its text, a grapheme-normalized selection, and bounded undo and redo history. The
//! helpers here convert between byte, grapheme, word, and UTF-16 positions. Nothing in this module
//! knows how text is laid out, so single-line and multi-line editors share one editing model.

use std::ops::Range;

use gpui::{ClipboardEntry, ClipboardItem};
use unicode_segmentation::UnicodeSegmentation as _;
use zeroize::Zeroizing;

/// The greatest value any text control retains.
pub(crate) const HARD_VALUE_LIMIT: usize = 1024 * 1024;
/// The greatest clipboard text any text control reads.
pub(crate) const CLIPBOARD_INSERTION_LIMIT: usize = 1024 * 1024;
pub(crate) const HISTORY_ENTRY_LIMIT: usize = 128;
pub(crate) const HISTORY_BYTE_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Selection {
    pub(crate) range: Range<usize>,
    pub(crate) reversed: bool,
}

impl Selection {
    pub(crate) fn caret(offset: usize) -> Self {
        Self {
            range: offset..offset,
            reversed: false,
        }
    }
    pub(crate) fn cursor(&self) -> usize {
        if self.reversed {
            self.range.start
        } else {
            self.range.end
        }
    }
    pub(crate) fn anchor(&self) -> usize {
        if self.reversed {
            self.range.end
        } else {
            self.range.start
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.range.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Snapshot {
    pub(crate) text: Zeroizing<String>,
    pub(crate) selection: Selection,
}
impl Snapshot {
    pub(crate) fn bytes(&self) -> usize {
        self.text.len()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EditKind {
    Insert,
    Backspace,
    DeleteForward,
    Atomic,
}

#[derive(Debug, Default)]
pub(crate) struct History {
    pub(crate) undo: Vec<Snapshot>,
    pub(crate) redo: Vec<Snapshot>,
    pub(crate) group: Option<EditKind>,
    pub(crate) retained_bytes: usize,
}

impl History {
    pub(crate) fn should_snapshot(&self, kind: EditKind) -> bool {
        kind == EditKind::Atomic || self.group != Some(kind)
    }

    pub(crate) fn record_snapshot(&mut self, snapshot: Option<Snapshot>, kind: EditKind) {
        self.clear_redo();
        if let Some(snapshot) = snapshot {
            self.retained_bytes += snapshot.bytes();
            self.undo.push(snapshot);
        }
        self.group = Some(kind);
        self.trim();
    }

    pub(crate) fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.group = None;
        self.retained_bytes = 0;
    }
    pub(crate) fn clear_redo(&mut self) {
        self.retained_bytes -= self.redo.iter().map(Snapshot::bytes).sum::<usize>();
        self.redo.clear();
    }
    pub(crate) fn break_group(&mut self) {
        self.group = None;
    }

    pub(crate) fn push_undo(&mut self, snapshot: Snapshot) {
        self.retained_bytes += snapshot.bytes();
        self.undo.push(snapshot);
        self.trim();
    }
    pub(crate) fn push_redo(&mut self, snapshot: Snapshot) {
        self.retained_bytes += snapshot.bytes();
        self.redo.push(snapshot);
        self.trim();
    }

    pub(crate) fn pop_undo(&mut self) -> Option<Snapshot> {
        let value = self.undo.pop()?;
        self.retained_bytes -= value.bytes();
        Some(value)
    }
    pub(crate) fn pop_redo(&mut self) -> Option<Snapshot> {
        let value = self.redo.pop()?;
        self.retained_bytes -= value.bytes();
        Some(value)
    }

    pub(crate) fn trim(&mut self) {
        while self.undo.len() + self.redo.len() > HISTORY_ENTRY_LIMIT
            || self.retained_bytes > HISTORY_BYTE_LIMIT
        {
            let removed = if self.undo.len() > 1 || self.redo.is_empty() {
                self.undo.remove(0)
            } else {
                self.redo.remove(0)
            };
            self.retained_bytes -= removed.bytes();
        }
    }
}

#[derive(Debug)]
pub(crate) struct TextBuffer {
    pub(crate) text: Zeroizing<String>,
    pub(crate) selection: Selection,
    pub(crate) history: History,
    pub(crate) history_enabled: bool,
}

impl TextBuffer {
    pub(crate) fn new(text: String) -> Self {
        let end = text.len();
        Self {
            text: Zeroizing::new(text),
            selection: Selection::caret(end),
            history: History::default(),
            history_enabled: true,
        }
    }
    pub(crate) fn set_history_enabled(&mut self, enabled: bool) {
        self.history_enabled = enabled;
        if !enabled {
            self.history.clear();
        }
    }
    pub(crate) fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            selection: self.selection.clone(),
        }
    }
    pub(crate) fn restore(&mut self, snapshot: Snapshot) {
        self.text = snapshot.text;
        self.selection = snapshot.selection;
        self.history.break_group();
    }
    pub(crate) fn move_to(&mut self, offset: usize) {
        self.selection = Selection::caret(clamp_grapheme_boundary(&self.text, offset, false));
        self.history.break_group();
    }
    pub(crate) fn select_from_anchor(&mut self, anchor: usize, offset: usize) {
        let anchor = clamp_grapheme_boundary(&self.text, anchor, false);
        let head = clamp_grapheme_boundary(&self.text, offset, false);
        self.selection = Selection {
            range: anchor.min(head)..anchor.max(head),
            reversed: head < anchor,
        };
        self.history.break_group();
    }
    pub(crate) fn select_to(&mut self, offset: usize) {
        let anchor = self.selection.anchor();
        self.select_from_anchor(anchor, offset);
    }
    pub(crate) fn select_all(&mut self) {
        self.selection = Selection {
            range: 0..self.text.len(),
            reversed: false,
        };
        self.history.break_group();
    }

    pub(crate) fn replace(
        &mut self,
        range: Range<usize>,
        replacement: String,
        kind: EditKind,
        limit: usize,
    ) -> bool {
        let replacement = Zeroizing::new(replacement);
        let range = normalize_byte_range(&self.text, range);
        let final_len = self.text.len() - range.len() + replacement.len();
        if final_len > limit || final_len > HARD_VALUE_LIMIT {
            return false;
        }
        if self.text[range.clone()] == *replacement {
            self.selection = Selection::caret(range.start + replacement.len());
            return false;
        }
        let snapshot =
            (self.history_enabled && self.history.should_snapshot(kind)).then(|| self.snapshot());
        let cursor = range.start + replacement.len();
        if self.history_enabled {
            self.text.replace_range(range, &replacement);
        } else {
            let mut updated = String::with_capacity(final_len);
            updated.push_str(&self.text[..range.start]);
            updated.push_str(&replacement);
            updated.push_str(&self.text[range.end..]);
            self.text = Zeroizing::new(updated);
        }
        self.selection = Selection::caret(cursor);
        if self.history_enabled {
            self.history.record_snapshot(snapshot, kind);
        } else {
            self.history.clear();
        }
        true
    }

    pub(crate) fn can_replace(
        &self,
        range: &Range<usize>,
        replacement_len: usize,
        limit: usize,
    ) -> bool {
        let range = normalize_byte_range(&self.text, range.clone());
        let final_len = self.text.len() - range.len() + replacement_len;
        final_len <= limit && final_len <= HARD_VALUE_LIMIT
    }

    pub(crate) fn replace_without_history(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        limit: usize,
    ) -> bool {
        let range = normalize_byte_range(&self.text, range);
        let final_len = self.text.len() - range.len() + replacement.len();
        if final_len > limit || final_len > HARD_VALUE_LIMIT {
            return false;
        }
        if self.text[range.clone()] == *replacement {
            self.selection = Selection::caret(range.start + replacement.len());
            return false;
        }
        let cursor = range.start + replacement.len();
        if self.history_enabled {
            self.text.replace_range(range, replacement);
        } else {
            let mut updated = String::with_capacity(final_len);
            updated.push_str(&self.text[..range.start]);
            updated.push_str(replacement);
            updated.push_str(&self.text[range.end..]);
            self.text = Zeroizing::new(updated);
        }
        self.selection = Selection::caret(cursor);
        self.history.break_group();
        true
    }

    pub(crate) fn move_left(&mut self, extend: bool) {
        if !extend && !self.selection.is_empty() {
            self.move_to(self.selection.range.start);
            return;
        }
        let offset = previous_grapheme_boundary(&self.text, self.selection.cursor());
        if extend {
            self.select_to(offset)
        } else {
            self.move_to(offset)
        }
    }
    pub(crate) fn move_right(&mut self, extend: bool) {
        if !extend && !self.selection.is_empty() {
            self.move_to(self.selection.range.end);
            return;
        }
        let offset = next_grapheme_boundary(&self.text, self.selection.cursor());
        if extend {
            self.select_to(offset)
        } else {
            self.move_to(offset)
        }
    }
    pub(crate) fn move_edge(&mut self, end: bool, extend: bool) {
        let offset = if end { self.text.len() } else { 0 };
        if extend {
            self.select_to(offset)
        } else {
            self.move_to(offset)
        }
    }
    pub(crate) fn move_word(&mut self, next: bool, extend: bool) {
        let offset = if next {
            next_word_end(&self.text, self.selection.cursor())
        } else {
            previous_word_start(&self.text, self.selection.cursor())
        };
        if extend {
            self.select_to(offset)
        } else {
            self.move_to(offset)
        }
    }

    pub(crate) fn delete_backward(&mut self, limit: usize) -> bool {
        let range = if self.selection.is_empty() {
            previous_grapheme_boundary(&self.text, self.selection.cursor())..self.selection.cursor()
        } else {
            self.selection.range.clone()
        };
        let kind = if self.selection.is_empty() {
            EditKind::Backspace
        } else {
            EditKind::Atomic
        };
        self.replace(range, String::new(), kind, limit)
    }
    pub(crate) fn delete_forward(&mut self, limit: usize) -> bool {
        let range = if self.selection.is_empty() {
            self.selection.cursor()..next_grapheme_boundary(&self.text, self.selection.cursor())
        } else {
            self.selection.range.clone()
        };
        let kind = if self.selection.is_empty() {
            EditKind::DeleteForward
        } else {
            EditKind::Atomic
        };
        self.replace(range, String::new(), kind, limit)
    }
    pub(crate) fn selected_text(&self) -> Option<&str> {
        (!self.selection.is_empty()).then(|| &self.text[self.selection.range.clone()])
    }

    pub(crate) fn transpose(&mut self, limit: usize) -> bool {
        if !self.selection.is_empty() {
            return false;
        }
        let cursor = self.selection.cursor();
        if cursor == 0 || self.text.is_empty() {
            return false;
        }
        let (left, split, right) = if cursor == self.text.len() {
            let split = previous_grapheme_boundary(&self.text, cursor);
            (previous_grapheme_boundary(&self.text, split), split, cursor)
        } else {
            (
                previous_grapheme_boundary(&self.text, cursor),
                cursor,
                next_grapheme_boundary(&self.text, cursor),
            )
        };
        if left == split || split == right {
            return false;
        }
        let replacement = format!("{}{}", &self.text[split..right], &self.text[left..split]);
        self.replace(left..right, replacement, EditKind::Atomic, limit)
    }

    pub(crate) fn undo(&mut self) -> bool {
        if !self.history_enabled {
            return false;
        }
        let Some(snapshot) = self.history.pop_undo() else {
            return false;
        };
        let current = self.snapshot();
        self.restore(snapshot);
        self.history.push_redo(current);
        true
    }
    pub(crate) fn redo(&mut self) -> bool {
        if !self.history_enabled {
            return false;
        }
        let Some(snapshot) = self.history.pop_redo() else {
            return false;
        };
        let current = self.snapshot();
        self.restore(snapshot);
        self.history.push_undo(current);
        true
    }
}

pub(crate) fn truncate_grapheme(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let end = text
        .grapheme_indices(true)
        .take_while(|(index, grapheme)| index + grapheme.len() <= limit)
        .map(|(index, grapheme)| index + grapheme.len())
        .last()
        .unwrap_or(0);
    &text[..end]
}
pub(crate) fn previous_grapheme_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .rev()
        .find_map(|(index, _)| (index < offset).then_some(index))
        .unwrap_or(0)
}
pub(crate) fn next_grapheme_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .find_map(|(index, _)| (index > offset).then_some(index))
        .unwrap_or(text.len())
}
pub(crate) fn previous_word_start(text: &str, offset: usize) -> usize {
    let offset = clamp_char_boundary(text, offset, false);
    text[..offset]
        .split_word_bound_indices()
        .rfind(|(_, word)| !word.trim_start().is_empty())
        .map(|(index, _)| index)
        .unwrap_or(0)
}
pub(crate) fn next_word_end(text: &str, offset: usize) -> usize {
    let offset = clamp_char_boundary(text, offset, false);
    text[offset..]
        .split_word_bound_indices()
        .find(|(_, word)| !word.trim_start().is_empty())
        .map(|(index, word)| offset + index + word.len())
        .unwrap_or(text.len())
}
pub(crate) fn bounded_clipboard_text(item: ClipboardItem) -> Option<String> {
    let mut text = None::<String>;
    for entry in item.into_entries() {
        let ClipboardEntry::String(string) = entry else {
            continue;
        };
        let part = string.into_text();
        if text
            .as_ref()
            .map_or(0, String::len)
            .saturating_add(part.len())
            > CLIPBOARD_INSERTION_LIMIT
        {
            return None;
        }
        if let Some(text) = &mut text {
            text.push_str(&part);
        } else {
            text = Some(part);
        }
    }
    text
}

pub(crate) fn word_range_at(text: &str, offset: usize) -> Range<usize> {
    if text.is_empty() {
        return 0..0;
    }
    let probe = clamp_char_boundary(text, offset.min(text.len().saturating_sub(1)), false);
    text.split_word_bound_indices()
        .find_map(|(start, word)| {
            let end = start + word.len();
            (probe >= start && probe < end).then_some(start..end)
        })
        .unwrap_or(text.len()..text.len())
}
pub(crate) fn clamp_char_boundary(text: &str, offset: usize, up: bool) -> usize {
    let mut offset = offset.min(text.len());
    if up {
        while offset < text.len() && !text.is_char_boundary(offset) {
            offset += 1;
        }
    } else {
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
    }
    offset
}
pub(crate) fn clamp_grapheme_boundary(text: &str, offset: usize, up: bool) -> usize {
    let offset = clamp_char_boundary(text, offset, up);
    if offset == 0
        || offset == text.len()
        || text
            .grapheme_indices(true)
            .any(|(index, _)| index == offset)
    {
        return offset;
    }
    if up {
        next_grapheme_boundary(text, offset)
    } else {
        previous_grapheme_boundary(text, offset + 1)
    }
}
pub(crate) fn normalize_byte_range(text: &str, range: Range<usize>) -> Range<usize> {
    if range.is_empty() {
        let caret = clamp_grapheme_boundary(text, range.start, false);
        return caret..caret;
    }
    let start = clamp_grapheme_boundary(text, range.start, false);
    let end = clamp_grapheme_boundary(text, range.end.max(range.start), true);
    start..end.max(start)
}

pub(crate) fn utf16_offset_to_byte(text: &str, offset: usize, up: bool) -> usize {
    let mut utf16 = 0;
    for (byte, ch) in text.char_indices() {
        if offset <= utf16 {
            return byte;
        }
        let next = utf16 + ch.len_utf16();
        if offset < next {
            return if up { byte + ch.len_utf8() } else { byte };
        }
        utf16 = next;
    }
    text.len()
}
pub(crate) fn utf16_collapsed_caret_to_byte(text: &str, offset: usize) -> usize {
    clamp_grapheme_boundary(text, utf16_offset_to_byte(text, offset, false), false)
}
pub(crate) fn utf16_query_range_to_bytes(text: &str, range: Range<usize>) -> Range<usize> {
    if range.is_empty() {
        let caret = utf16_collapsed_caret_to_byte(text, range.start);
        caret..caret
    } else {
        let start =
            clamp_grapheme_boundary(text, utf16_offset_to_byte(text, range.start, false), false);
        let end = clamp_grapheme_boundary(
            text,
            utf16_offset_to_byte(text, range.end.max(range.start), true),
            true,
        );
        start..end.max(start)
    }
}
pub(crate) fn utf16_replacement_range_to_bytes(text: &str, range: Range<usize>) -> Range<usize> {
    if range.is_empty() {
        let caret = utf16_collapsed_caret_to_byte(text, range.start);
        caret..caret
    } else {
        utf16_query_range_to_bytes(text, range)
    }
}
pub(crate) fn utf16_selection_range_to_bytes(text: &str, range: Range<usize>) -> Range<usize> {
    if range.is_empty() {
        let caret = utf16_collapsed_caret_to_byte(text, range.start);
        caret..caret
    } else {
        utf16_query_range_to_bytes(text, range)
    }
}
pub(crate) fn byte_offset_to_utf16(text: &str, offset: usize) -> usize {
    text[..clamp_char_boundary(text, offset, false)]
        .encode_utf16()
        .count()
}
pub(crate) fn byte_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    byte_offset_to_utf16(text, range.start)..byte_offset_to_utf16(text, range.end)
}
