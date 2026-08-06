use std::ops::Range;

use gpui::{
    App, Bounds, Element, ElementId, ElementInputHandler, Entity, FocusHandle, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Style, Window, relative,
};
use libghostty_vt::unicode::grapheme_width;

use super::terminal_pane::TerminalPane;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct FindEditor {
    text: String,
    selection: Range<usize>,
    selection_reversed: bool,
    marked: Option<Range<usize>>,
}

impl FindEditor {
    pub(super) fn text(&self) -> &str {
        &self.text
    }

    pub(super) fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    pub(super) fn selection_reversed(&self) -> bool {
        self.selection_reversed
    }

    pub(super) fn marked_range(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub(super) fn select_all(&mut self) {
        self.selection = 0..self.utf16_len();
        self.selection_reversed = false;
        self.marked = None;
    }

    pub(super) fn text_for_range(&self, range: Range<usize>) -> (String, Range<usize>) {
        let bytes = utf16_range_to_bytes(&self.text, range);
        let adjusted = byte_offset_to_utf16(&self.text, bytes.start)
            ..byte_offset_to_utf16(&self.text, bytes.end);
        (self.text[bytes].to_owned(), adjusted)
    }

    pub(super) fn replace(&mut self, range: Option<Range<usize>>, text: &str) {
        let replacement = range
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        let replacement = utf16_range_to_bytes(&self.text, replacement);
        let start_utf16 = byte_offset_to_utf16(&self.text, replacement.start);
        let text = single_line_text(text);
        let inserted_utf16 = text.encode_utf16().count();
        self.text.replace_range(replacement, &text);
        self.selection = start_utf16 + inserted_utf16..start_utf16 + inserted_utf16;
        self.selection_reversed = false;
        self.marked = None;
    }

    pub(super) fn replace_and_mark(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
    ) {
        let replacement = range
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        let replacement = utf16_range_to_bytes(&self.text, replacement);
        let start_utf16 = byte_offset_to_utf16(&self.text, replacement.start);
        let text = single_line_text(text);
        let inserted_utf16 = text.encode_utf16().count();
        self.text.replace_range(replacement, &text);
        let marked = start_utf16..start_utf16 + inserted_utf16;
        let selected = selected.unwrap_or(inserted_utf16..inserted_utf16);
        self.selection = (start_utf16 + selected.start.min(inserted_utf16))
            ..(start_utf16 + selected.end.min(inserted_utf16));
        self.selection_reversed = false;
        self.marked = (!marked.is_empty()).then_some(marked);
    }

    pub(super) fn unmark(&mut self) {
        self.marked = None;
    }

    pub(super) fn delete_backward(&mut self) -> bool {
        if !self.selection.is_empty() || self.marked.is_some() {
            self.replace(None, "");
            return true;
        }
        if self.selection.start == 0 {
            return false;
        }
        let start = previous_grapheme_boundary(&self.text, self.selection.start);
        self.replace(Some(start..self.selection.end), "");
        true
    }

    pub(super) fn delete_forward(&mut self) -> bool {
        if !self.selection.is_empty() || self.marked.is_some() {
            self.replace(None, "");
            return true;
        }
        if self.selection.end >= self.utf16_len() {
            return false;
        }
        let end = next_grapheme_boundary(&self.text, self.selection.end);
        self.replace(Some(self.selection.start..end), "");
        true
    }

    pub(super) fn move_left(&mut self, extend: bool) {
        if extend {
            let head = if self.selection_reversed {
                self.selection.start
            } else {
                self.selection.end
            };
            self.extend_selection(previous_grapheme_boundary(&self.text, head));
        } else {
            let next = if self.selection.is_empty() {
                previous_grapheme_boundary(&self.text, self.selection.start)
            } else {
                self.selection.start
            };
            self.selection = next..next;
            self.selection_reversed = false;
        }
        self.marked = None;
    }

    pub(super) fn move_right(&mut self, extend: bool) {
        if extend {
            let head = if self.selection_reversed {
                self.selection.start
            } else {
                self.selection.end
            };
            self.extend_selection(next_grapheme_boundary(&self.text, head));
        } else {
            let next = if self.selection.is_empty() {
                next_grapheme_boundary(&self.text, self.selection.end)
            } else {
                self.selection.end
            };
            self.selection = next..next;
            self.selection_reversed = false;
        }
        self.marked = None;
    }

    pub(super) fn move_to_start(&mut self, extend: bool) {
        if extend {
            self.extend_selection(0);
        } else {
            self.selection = 0..0;
            self.selection_reversed = false;
        }
        self.marked = None;
    }

    pub(super) fn move_to_end(&mut self, extend: bool) {
        let end = self.utf16_len();
        if extend {
            self.extend_selection(end);
        } else {
            self.selection = end..end;
            self.selection_reversed = false;
        }
        self.marked = None;
    }

    fn utf16_len(&self) -> usize {
        self.text.encode_utf16().count()
    }

    fn extend_selection(&mut self, head: usize) {
        let anchor = if self.selection_reversed {
            self.selection.end
        } else {
            self.selection.start
        };
        self.selection = anchor.min(head)..anchor.max(head);
        self.selection_reversed = head < anchor;
    }
}

pub(super) struct FindInputElement {
    focus_handle: FocusHandle,
    input: Entity<TerminalPane>,
}

impl FindInputElement {
    pub(super) fn new(focus_handle: FocusHandle, input: Entity<TerminalPane>) -> Self {
        Self {
            focus_handle,
            input,
        }
    }
}

impl IntoElement for FindInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for FindInputElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
    }
}

fn single_line_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .collect()
}

fn previous_grapheme_boundary(text: &str, offset: usize) -> usize {
    grapheme_utf16_boundaries(text)
        .into_iter()
        .take_while(|boundary| *boundary < offset)
        .last()
        .unwrap_or(0)
}

fn next_grapheme_boundary(text: &str, offset: usize) -> usize {
    grapheme_utf16_boundaries(text)
        .into_iter()
        .find(|boundary| *boundary > offset)
        .unwrap_or_else(|| text.encode_utf16().count())
}

fn grapheme_utf16_boundaries(text: &str) -> Vec<usize> {
    let characters = text.chars().collect::<Vec<_>>();
    let mut boundaries = vec![0];
    let mut character_index = 0;
    let mut utf16_index = 0;
    while character_index < characters.len() {
        let consumed = grapheme_width(&characters[character_index..])
            .0
            .max(1)
            .min(characters.len() - character_index);
        utf16_index += characters[character_index..character_index + consumed]
            .iter()
            .map(|character| character.len_utf16())
            .sum::<usize>();
        boundaries.push(utf16_index);
        character_index += consumed;
    }
    boundaries
}

fn utf16_range_to_bytes(text: &str, range: Range<usize>) -> Range<usize> {
    let start = utf16_offset_to_byte(text, range.start, false);
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
mod tests {
    use super::*;

    #[test]
    fn editor_replacement_ranges_are_utf16_safe() {
        let mut editor = FindEditor::default();
        editor.replace(None, "A😀B");

        editor.replace(Some(1..3), "界");

        assert_eq!(editor.text(), "A界B");
    }

    #[test]
    fn editor_strips_line_breaks_from_query() {
        let mut editor = FindEditor::default();

        editor.replace(None, "one\ntwo\rthree");

        assert_eq!(editor.text(), "onetwothree");
    }

    #[test]
    fn editor_deletes_complete_graphemes() {
        let mut editor = FindEditor::default();
        editor.replace(None, "e\u{301}👩‍💻");

        assert!(editor.delete_backward());
        assert_eq!(editor.text(), "e\u{301}");
        assert!(editor.delete_backward());
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn editor_reports_reversed_utf16_selection() {
        let mut editor = FindEditor::default();
        editor.replace(None, "A😀B");

        editor.move_left(true);

        assert_eq!(editor.selection(), 3..4);
        assert!(editor.selection_reversed());

        editor.move_right(true);

        assert_eq!(editor.selection(), 4..4);
        assert!(!editor.selection_reversed());
    }
}
