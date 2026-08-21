use std::ops::Range;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    GlobalElementId, Hsla, InspectorElementId, IntoElement, KeyBinding, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Render, ShapedLine,
    SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill,
    point, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation as _;

const KEY_CONTEXT: &str = "SpaceTermTextInput";
const CARET_BLINK_INTERVAL: Duration = Duration::from_millis(530);
const CARET_WIDTH: Pixels = px(1.0);
const SCROLL_PADDING: Pixels = px(2.0);
const HISTORY_LIMIT: usize = 128;
const HISTORY_BYTE_LIMIT: usize = 256 * 1024;

actions!(
    spaceterm_text_input,
    [
        Backspace,
        DeleteForward,
        DeleteToBeginning,
        DeleteToEnd,
        DeletePreviousWord,
        DeleteNextWord,
        MoveLeft,
        MoveRight,
        MoveToBeginning,
        MoveToEnd,
        MoveToPreviousWord,
        MoveToNextWord,
        SelectLeft,
        SelectRight,
        SelectToBeginning,
        SelectToEnd,
        SelectToPreviousWord,
        SelectToNextWord,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        Submit,
        Cancel,
        FocusNext,
        FocusPrevious,
        ShowCharacterPalette,
    ]
);

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some(KEY_CONTEXT)),
        KeyBinding::new("delete", DeleteForward, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-backspace", DeleteToBeginning, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-delete", DeleteToEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-backspace", DeletePreviousWord, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-delete", DeleteNextWord, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-h", Backspace, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-d", DeleteForward, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-k", DeleteToEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("left", MoveLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("right", MoveRight, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-b", MoveLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-f", MoveRight, Some(KEY_CONTEXT)),
        KeyBinding::new("home", MoveToBeginning, Some(KEY_CONTEXT)),
        KeyBinding::new("end", MoveToEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-left", MoveToBeginning, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-right", MoveToEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-a", MoveToBeginning, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-e", MoveToEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-left", MoveToPreviousWord, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-right", MoveToNextWord, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-left", SelectLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-right", SelectRight, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-home", SelectToBeginning, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-end", SelectToEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-left", SelectToBeginning, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-right", SelectToEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-shift-a", SelectToBeginning, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-shift-e", SelectToEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-shift-left", SelectToPreviousWord, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-shift-right", SelectToNextWord, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-a", SelectAll, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-c", Copy, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-x", Cut, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-v", Paste, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-z", Undo, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-z", Redo, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", Submit, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", Cancel, Some(KEY_CONTEXT)),
        KeyBinding::new("tab", FocusNext, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-tab", FocusPrevious, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, Some(KEY_CONTEXT)),
    ]);
}

/// Paint colors for an unstyled text input editor surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextInputStyle {
    text: Hsla,
    placeholder: Hsla,
    selection: Hsla,
    caret: Hsla,
}

impl TextInputStyle {
    /// Creates editor paint colors supplied by the application theme.
    pub fn new(text: Hsla, placeholder: Hsla, selection: Hsla, caret: Hsla) -> Self {
        Self {
            text,
            placeholder,
            selection,
            caret,
        }
    }
}

/// Events emitted by a [`TextInput`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TextInputEvent {
    /// The editable value changed.
    Changed(String),
    /// Return was pressed with the current value.
    Submitted(String),
    /// Escape was pressed.
    Cancelled,
    /// The input lost focus with the current value.
    Blurred(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Selection {
    range: Range<usize>,
    reversed: bool,
}

impl Selection {
    fn caret(offset: usize) -> Self {
        Self {
            range: offset..offset,
            reversed: false,
        }
    }

    fn cursor(&self) -> usize {
        if self.reversed {
            self.range.start
        } else {
            self.range.end
        }
    }

    fn anchor(&self) -> usize {
        if self.reversed {
            self.range.end
        } else {
            self.range.start
        }
    }

    fn is_empty(&self) -> bool {
        self.range.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Snapshot {
    text: String,
    selection: Selection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EditKind {
    Insert,
    Backspace,
    DeleteForward,
    Atomic,
}

#[derive(Debug, Default)]
struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    group: Option<EditKind>,
}

impl History {
    fn record(&mut self, snapshot: Snapshot, kind: EditKind) {
        let grouped = kind != EditKind::Atomic && self.group == Some(kind);
        if !grouped {
            if self.undo.len() == HISTORY_LIMIT {
                self.undo.remove(0);
            }
            self.undo.push(snapshot);
            while self.undo.len() > 1
                && self
                    .undo
                    .iter()
                    .map(|snapshot| snapshot.text.len())
                    .sum::<usize>()
                    > HISTORY_BYTE_LIMIT
            {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.group = Some(kind);
    }

    fn break_group(&mut self) {
        self.group = None;
    }
}

#[derive(Debug)]
struct TextBuffer {
    text: String,
    selection: Selection,
    marked: Option<Range<usize>>,
    history: History,
}

impl TextBuffer {
    fn new(text: String) -> Self {
        let end = text.len();
        Self {
            text,
            selection: Selection::caret(end),
            marked: None,
            history: History::default(),
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            selection: self.selection.clone(),
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.text = snapshot.text;
        self.selection = snapshot.selection;
        self.marked = None;
        self.history.break_group();
    }

    fn move_to(&mut self, offset: usize) {
        self.selection = Selection::caret(clamp_boundary(&self.text, offset));
        self.marked = None;
        self.history.break_group();
    }

    fn select_to(&mut self, offset: usize) {
        let anchor = self.selection.anchor();
        let head = clamp_boundary(&self.text, offset);
        self.selection = Selection {
            range: anchor.min(head)..anchor.max(head),
            reversed: head < anchor,
        };
        self.marked = None;
        self.history.break_group();
    }

    fn select_all(&mut self) {
        self.selection = Selection {
            range: 0..self.text.len(),
            reversed: false,
        };
        self.marked = None;
        self.history.break_group();
    }

    fn move_left(&mut self, extend: bool) {
        if !extend && !self.selection.is_empty() {
            self.move_to(self.selection.range.start);
            return;
        }
        let offset = previous_grapheme_boundary(&self.text, self.selection.cursor());
        if extend {
            self.select_to(offset);
        } else {
            self.move_to(offset);
        }
    }

    fn move_right(&mut self, extend: bool) {
        if !extend && !self.selection.is_empty() {
            self.move_to(self.selection.range.end);
            return;
        }
        let offset = next_grapheme_boundary(&self.text, self.selection.cursor());
        if extend {
            self.select_to(offset);
        } else {
            self.move_to(offset);
        }
    }

    fn move_to_beginning(&mut self, extend: bool) {
        if extend {
            self.select_to(0);
        } else {
            self.move_to(0);
        }
    }

    fn move_to_end(&mut self, extend: bool) {
        if extend {
            self.select_to(self.text.len());
        } else {
            self.move_to(self.text.len());
        }
    }

    fn move_to_previous_word(&mut self, extend: bool) {
        let offset = previous_word_start(&self.text, self.selection.cursor());
        if extend {
            self.select_to(offset);
        } else {
            self.move_to(offset);
        }
    }

    fn move_to_next_word(&mut self, extend: bool) {
        let offset = next_word_end(&self.text, self.selection.cursor());
        if extend {
            self.select_to(offset);
        } else {
            self.move_to(offset);
        }
    }

    fn replace(&mut self, range: Range<usize>, replacement: &str, kind: EditKind) -> bool {
        let range = normalized_byte_range(&self.text, range);
        let replacement = single_line_text(replacement);
        if self.text[range.clone()] == replacement {
            self.selection = Selection::caret(range.start + replacement.len());
            self.marked = None;
            return false;
        }
        self.history.record(self.snapshot(), kind);
        let cursor = range.start + replacement.len();
        self.text.replace_range(range, &replacement);
        self.selection = Selection::caret(cursor);
        self.marked = None;
        true
    }

    fn replace_without_history(&mut self, range: Range<usize>, replacement: &str) -> bool {
        let range = normalized_byte_range(&self.text, range);
        let replacement = single_line_text(replacement);
        if self.text[range.clone()] == replacement {
            self.selection = Selection::caret(range.start + replacement.len());
            self.marked = None;
            return false;
        }
        let cursor = range.start + replacement.len();
        self.text.replace_range(range, &replacement);
        self.selection = Selection::caret(cursor);
        self.marked = None;
        self.history.break_group();
        true
    }

    fn replace_selection(&mut self, replacement: &str, kind: EditKind) -> bool {
        let range = self
            .marked
            .clone()
            .unwrap_or_else(|| self.selection.range.clone());
        self.replace(range, replacement, kind)
    }

    fn delete_backward(&mut self) -> bool {
        if !self.selection.is_empty() || self.marked.is_some() {
            return self.replace_selection("", EditKind::Atomic);
        }
        let cursor = self.selection.cursor();
        let start = previous_grapheme_boundary(&self.text, cursor);
        self.replace(start..cursor, "", EditKind::Backspace)
    }

    fn delete_forward(&mut self) -> bool {
        if !self.selection.is_empty() || self.marked.is_some() {
            return self.replace_selection("", EditKind::Atomic);
        }
        let cursor = self.selection.cursor();
        let end = next_grapheme_boundary(&self.text, cursor);
        self.replace(cursor..end, "", EditKind::DeleteForward)
    }

    fn delete_to_beginning(&mut self) -> bool {
        if !self.selection.is_empty() || self.marked.is_some() {
            return self.replace_selection("", EditKind::Atomic);
        }
        self.replace(0..self.selection.cursor(), "", EditKind::Atomic)
    }

    fn delete_to_end(&mut self) -> bool {
        if !self.selection.is_empty() || self.marked.is_some() {
            return self.replace_selection("", EditKind::Atomic);
        }
        self.replace(
            self.selection.cursor()..self.text.len(),
            "",
            EditKind::Atomic,
        )
    }

    fn delete_previous_word(&mut self) -> bool {
        if !self.selection.is_empty() || self.marked.is_some() {
            return self.replace_selection("", EditKind::Atomic);
        }
        let cursor = self.selection.cursor();
        self.replace(
            previous_word_start(&self.text, cursor)..cursor,
            "",
            EditKind::Atomic,
        )
    }

    fn delete_next_word(&mut self) -> bool {
        if !self.selection.is_empty() || self.marked.is_some() {
            return self.replace_selection("", EditKind::Atomic);
        }
        let cursor = self.selection.cursor();
        self.replace(
            cursor..next_word_end(&self.text, cursor),
            "",
            EditKind::Atomic,
        )
    }

    fn selected_text(&self) -> Option<&str> {
        (!self.selection.is_empty()).then(|| &self.text[self.selection.range.clone()])
    }

    fn undo(&mut self) -> bool {
        let Some(snapshot) = self.history.undo.pop() else {
            return false;
        };
        self.history.redo.push(self.snapshot());
        self.restore(snapshot);
        true
    }

    fn redo(&mut self) -> bool {
        let Some(snapshot) = self.history.redo.pop() else {
            return false;
        };
        self.history.undo.push(self.snapshot());
        self.restore(snapshot);
        true
    }
}

/// A reusable unstyled single-line GPUI text input.
pub struct TextInput {
    buffer: TextBuffer,
    placeholder: SharedString,
    style: TextInputStyle,
    focus_handle: FocusHandle,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    scroll: Pixels,
    selecting: bool,
    focused: bool,
    caret_visible: bool,
    blinking: bool,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl TextInput {
    /// Creates a single-line editor with application-owned paint colors.
    pub fn new(
        value: impl Into<String>,
        style: TextInputStyle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle().tab_stop(true);
        cx.on_focus(&focus_handle, window, Self::on_focus).detach();
        cx.on_blur(&focus_handle, window, Self::on_blur).detach();
        Self {
            buffer: TextBuffer::new(single_line_text(&value.into())),
            placeholder: SharedString::default(),
            style,
            focus_handle,
            last_layout: None,
            last_bounds: None,
            scroll: px(0.0),
            selecting: false,
            focused: false,
            caret_visible: true,
            blinking: false,
        }
    }

    /// Sets the placeholder displayed when the value is empty.
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Returns the current value.
    pub fn value(&self) -> &str {
        &self.buffer.text
    }

    /// Replaces the value without emitting a change event.
    pub fn set_value(&mut self, value: impl Into<String>, cx: &mut Context<Self>) {
        self.buffer = TextBuffer::new(single_line_text(&value.into()));
        self.scroll = px(0.0);
        self.wake_caret(cx);
    }

    /// Selects the complete value.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.buffer.select_all();
        self.wake_caret(cx);
    }

    /// Returns the input's focus handle.
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    fn emit_change(&mut self, changed: bool, cx: &mut Context<Self>) {
        if changed {
            cx.emit(TextInputEvent::Changed(self.buffer.text.clone()));
        }
        self.wake_caret(cx);
    }

    fn wake_caret(&mut self, cx: &mut Context<Self>) {
        self.caret_visible = true;
        cx.notify();
    }

    fn on_focus(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.focused = true;
        self.caret_visible = true;
        if !self.blinking {
            self.blinking = true;
            Self::run_caret_blink(cx);
        }
        cx.notify();
    }

    fn on_blur(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.focused = false;
        self.caret_visible = true;
        self.buffer.marked = None;
        self.buffer.history.break_group();
        cx.emit(TextInputEvent::Blurred(self.buffer.text.clone()));
        cx.notify();
    }

    fn run_caret_blink(cx: &mut Context<Self>) {
        cx.spawn(async move |input, cx| {
            loop {
                cx.background_executor().timer(CARET_BLINK_INTERVAL).await;
                let keep_running = input
                    .update(cx, |input, cx| {
                        if !input.focused {
                            input.blinking = false;
                            input.caret_visible = true;
                            cx.notify();
                            return false;
                        }
                        input.caret_visible = !input.caret_visible;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        })
        .detach();
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        let changed = self.buffer.delete_backward();
        self.emit_change(changed, cx);
    }

    fn delete_forward(&mut self, _: &DeleteForward, _: &mut Window, cx: &mut Context<Self>) {
        let changed = self.buffer.delete_forward();
        self.emit_change(changed, cx);
    }

    fn delete_to_beginning(
        &mut self,
        _: &DeleteToBeginning,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self.buffer.delete_to_beginning();
        self.emit_change(changed, cx);
    }

    fn delete_to_end(&mut self, _: &DeleteToEnd, _: &mut Window, cx: &mut Context<Self>) {
        let changed = self.buffer.delete_to_end();
        self.emit_change(changed, cx);
    }

    fn delete_previous_word(
        &mut self,
        _: &DeletePreviousWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self.buffer.delete_previous_word();
        self.emit_change(changed, cx);
    }

    fn delete_next_word(&mut self, _: &DeleteNextWord, _: &mut Window, cx: &mut Context<Self>) {
        let changed = self.buffer.delete_next_word();
        self.emit_change(changed, cx);
    }

    fn move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_left(false);
        self.wake_caret(cx);
    }

    fn move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_right(false);
        self.wake_caret(cx);
    }

    fn move_to_beginning(&mut self, _: &MoveToBeginning, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_to_beginning(false);
        self.wake_caret(cx);
    }

    fn move_to_end(&mut self, _: &MoveToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_to_end(false);
        self.wake_caret(cx);
    }

    fn move_to_previous_word(
        &mut self,
        _: &MoveToPreviousWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.move_to_previous_word(false);
        self.wake_caret(cx);
    }

    fn move_to_next_word(&mut self, _: &MoveToNextWord, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_to_next_word(false);
        self.wake_caret(cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_left(true);
        self.wake_caret(cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_right(true);
        self.wake_caret(cx);
    }

    fn select_to_beginning(
        &mut self,
        _: &SelectToBeginning,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.move_to_beginning(true);
        self.wake_caret(cx);
    }

    fn select_to_end(&mut self, _: &SelectToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_to_end(true);
        self.wake_caret(cx);
    }

    fn select_to_previous_word(
        &mut self,
        _: &SelectToPreviousWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.move_to_previous_word(true);
        self.wake_caret(cx);
    }

    fn select_to_next_word(
        &mut self,
        _: &SelectToNextWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.move_to_next_word(true);
        self.wake_caret(cx);
    }

    fn on_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_all(cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.buffer.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        }
        cx.stop_propagation();
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.buffer.selected_text().map(ToOwned::to_owned) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            let changed = self.buffer.replace_selection("", EditKind::Atomic);
            self.emit_change(changed, cx);
        }
        cx.stop_propagation();
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let changed = self.buffer.replace_selection(&text, EditKind::Atomic);
            self.emit_change(changed, cx);
        }
        cx.stop_propagation();
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.undo() {
            cx.emit(TextInputEvent::Changed(self.buffer.text.clone()));
        }
        self.wake_caret(cx);
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.redo() {
            cx.emit(TextInputEvent::Changed(self.buffer.text.clone()));
        }
        self.wake_caret(cx);
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.history.break_group();
        cx.emit(TextInputEvent::Submitted(self.buffer.text.clone()));
        cx.stop_propagation();
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.history.break_group();
        cx.emit(TextInputEvent::Cancelled);
        cx.stop_propagation();
    }

    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_next();
        cx.stop_propagation();
    }

    fn focus_previous(&mut self, _: &FocusPrevious, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_prev();
        cx.stop_propagation();
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.show_character_palette();
        cx.stop_propagation();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window);
        let offset = self.index_for_mouse_position(event.position);
        match event.click_count {
            1 if event.modifiers.shift => self.buffer.select_to(offset),
            1 => self.buffer.move_to(offset),
            2 => {
                let range = word_range_at(&self.buffer.text, offset);
                self.buffer.selection = Selection {
                    range,
                    reversed: false,
                };
            }
            _ => self.buffer.select_all(),
        }
        self.selecting = true;
        self.wake_caret(cx);
        cx.stop_propagation();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.buffer
                .select_to(self.index_for_mouse_position(event.position));
            self.wake_caret(cx);
            cx.stop_propagation();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.selecting = false;
            cx.stop_propagation();
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.buffer.text.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(line)) = (self.last_bounds, self.last_layout.as_ref()) else {
            return self.buffer.selection.cursor();
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.buffer.text.len();
        }
        line.closest_index_for_x(position.x - bounds.left() + self.scroll)
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = utf16_range_to_bytes(&self.buffer.text, range_utf16);
        adjusted_range.replace(byte_range_to_utf16(&self.buffer.text, range.clone()));
        Some(self.buffer.text[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: byte_range_to_utf16(&self.buffer.text, self.buffer.selection.range.clone()),
            reversed: self.buffer.selection.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer
            .marked
            .clone()
            .map(|range| byte_range_to_utf16(&self.buffer.text, range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.buffer.marked = None;
        self.buffer.history.break_group();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let had_marked_text = self.buffer.marked.is_some();
        let range = range_utf16
            .map(|range| utf16_range_to_bytes(&self.buffer.text, range))
            .or_else(|| self.buffer.marked.clone())
            .unwrap_or_else(|| self.buffer.selection.range.clone());
        let kind = if !self.buffer.selection.is_empty() {
            EditKind::Atomic
        } else {
            EditKind::Insert
        };
        let changed = if had_marked_text {
            self.buffer.replace_without_history(range, text)
        } else {
            self.buffer.replace(range, text, kind)
        };
        if changed && !had_marked_text {
            self.buffer.history.group = Some(EditKind::Insert);
        }
        self.emit_change(changed, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let replacement = single_line_text(text);
        let range = range_utf16
            .map(|range| utf16_range_to_bytes(&self.buffer.text, range))
            .or_else(|| self.buffer.marked.clone())
            .unwrap_or_else(|| self.buffer.selection.range.clone());
        let starts_composition = self.buffer.marked.is_none();
        let before = starts_composition.then(|| self.buffer.snapshot());
        let range = normalized_byte_range(&self.buffer.text, range);
        let start = range.start;
        let old = self.buffer.text[range.clone()].to_owned();
        self.buffer.text.replace_range(range, &replacement);
        if let Some(snapshot) = before
            && old != replacement
        {
            self.buffer.history.record(snapshot, EditKind::Atomic);
        }
        let end = start + replacement.len();
        self.buffer.marked = (!replacement.is_empty()).then_some(start..end);
        self.buffer.selection = selected_utf16.map_or_else(
            || Selection::caret(end),
            |selected| {
                let relative = utf16_range_to_bytes(&replacement, selected);
                Selection {
                    range: start + relative.start..start + relative.end,
                    reversed: false,
                }
            },
        );
        if old != replacement {
            cx.emit(TextInputEvent::Changed(self.buffer.text.clone()));
        }
        self.wake_caret(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        let range = utf16_range_to_bytes(&self.buffer.text, range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + layout.x_for_index(range.start) - self.scroll,
                bounds.top(),
            ),
            point(
                bounds.left() + layout.x_for_index(range.end) - self.scroll,
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let layout = self.last_layout.as_ref()?;
        let index = layout.closest_index_for_x(point.x - bounds.left() + self.scroll);
        Some(byte_offset_to_utf16(&self.buffer.text, index))
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        div()
            .size_full()
            .min_w_0()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete_forward))
            .on_action(cx.listener(Self::delete_to_beginning))
            .on_action(cx.listener(Self::delete_to_end))
            .on_action(cx.listener(Self::delete_previous_word))
            .on_action(cx.listener(Self::delete_next_word))
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::move_to_beginning))
            .on_action(cx.listener(Self::move_to_end))
            .on_action(cx.listener(Self::move_to_previous_word))
            .on_action(cx.listener(Self::move_to_next_word))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_to_beginning))
            .on_action(cx.listener(Self::select_to_end))
            .on_action(cx.listener(Self::select_to_previous_word))
            .on_action(cx.listener(Self::select_to_next_word))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_previous))
            .on_action(cx.listener(Self::show_character_palette))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .child(TextElement { input: entity })
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct TextPrepaint {
    line: Option<ShapedLine>,
    caret: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    scroll: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = TextPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> TextPrepaint {
        let input = self.input.read(cx);
        let focused = input.focus_handle.is_focused(window) && window.is_window_active();
        let empty = input.buffer.text.is_empty();
        let display = if empty {
            input.placeholder.clone()
        } else {
            input.buffer.text.clone().into()
        };
        let color = if empty {
            input.style.placeholder
        } else {
            input.style.text
        };
        let base_run = TextRun {
            len: display.len(),
            font: window.text_style().font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = marked_text_runs(&display, input.buffer.marked.clone(), base_run);
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display, font_size, &runs, None);

        let caret_x = if empty {
            px(0.0)
        } else {
            line.x_for_index(input.buffer.selection.cursor())
        };
        let mut scroll = input
            .scroll
            .min((line.width - bounds.size.width + SCROLL_PADDING).max(px(0.0)));
        if caret_x - scroll > bounds.size.width - SCROLL_PADDING {
            scroll = caret_x - bounds.size.width + SCROLL_PADDING;
        }
        if caret_x - scroll < px(0.0) {
            scroll = caret_x;
        }
        scroll = scroll.max(px(0.0));

        let (caret, selection) = if focused && !input.buffer.selection.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(input.buffer.selection.range.start)
                                - scroll,
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(input.buffer.selection.range.end)
                                - scroll,
                            bounds.bottom(),
                        ),
                    ),
                    input.style.selection,
                )),
            )
        } else if focused && input.caret_visible {
            (
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + caret_x - scroll, bounds.top()),
                        size(CARET_WIDTH, bounds.size.height),
                    ),
                    input.style.caret,
                )),
                None,
            )
        } else {
            (None, None)
        };

        TextPrepaint {
            line: Some(line),
            caret,
            selection,
            scroll,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut TextPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );

        let line = prepaint.line.take().unwrap_or_default();
        let origin = point(bounds.origin.x - prepaint.scroll, bounds.origin.y);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(selection) = prepaint.selection.take() {
                window.paint_quad(selection);
            }
            _ = line.paint(origin, window.line_height(), window, cx);
            if let Some(caret) = prepaint.caret.take() {
                window.paint_quad(caret);
            }
        });

        self.input.update(cx, |input, _| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
            input.scroll = prepaint.scroll;
        });
    }
}

fn marked_text_runs(display: &str, marked: Option<Range<usize>>, base: TextRun) -> Vec<TextRun> {
    let Some(marked) = marked else {
        return vec![base];
    };
    let start = clamp_boundary(display, marked.start);
    let end = clamp_boundary(display, marked.end.max(start));
    [
        TextRun {
            len: start,
            ..base.clone()
        },
        TextRun {
            len: end - start,
            underline: Some(UnderlineStyle {
                color: Some(base.color),
                thickness: px(1.0),
                wavy: false,
            }),
            ..base.clone()
        },
        TextRun {
            len: display.len() - end,
            ..base
        },
    ]
    .into_iter()
    .filter(|run| run.len > 0)
    .collect()
}

fn single_line_text(text: &str) -> String {
    let mut flattened = String::with_capacity(text.len());
    let mut previous_was_carriage_return = false;
    for character in text.chars() {
        match character {
            '\r' => {
                flattened.push(' ');
                previous_was_carriage_return = true;
            }
            '\n' if previous_was_carriage_return => {
                previous_was_carriage_return = false;
            }
            '\n' => flattened.push(' '),
            _ if character.is_control() => {
                previous_was_carriage_return = false;
            }
            _ => {
                flattened.push(character);
                previous_was_carriage_return = false;
            }
        }
    }
    flattened
}

fn previous_grapheme_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .rev()
        .find_map(|(index, _)| (index < offset).then_some(index))
        .unwrap_or(0)
}

fn next_grapheme_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .find_map(|(index, _)| (index > offset).then_some(index))
        .unwrap_or(text.len())
}

fn previous_word_start(text: &str, offset: usize) -> usize {
    let offset = clamp_boundary(text, offset);
    text[..offset]
        .split_word_bound_indices()
        .rfind(|(_, word)| !word.trim_start().is_empty())
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_word_end(text: &str, offset: usize) -> usize {
    let offset = clamp_boundary(text, offset);
    text[offset..]
        .split_word_bound_indices()
        .find(|(_, word)| !word.trim_start().is_empty())
        .map(|(index, word)| offset + index + word.len())
        .unwrap_or(text.len())
}

fn word_range_at(text: &str, offset: usize) -> Range<usize> {
    if text.is_empty() {
        return 0..0;
    }
    let probe = clamp_boundary(text, offset.min(text.len().saturating_sub(1)));
    text.split_word_bound_indices()
        .find_map(|(start, word)| {
            let end = start + word.len();
            (probe >= start && probe < end).then_some(start..end)
        })
        .unwrap_or(text.len()..text.len())
}

fn clamp_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn normalized_byte_range(text: &str, range: Range<usize>) -> Range<usize> {
    let start = clamp_boundary(text, range.start);
    let end = clamp_boundary(text, range.end.max(start));
    start..end.max(start)
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
    text[..clamp_boundary(text, offset)].encode_utf16().count()
}

fn byte_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    byte_offset_to_utf16(text, range.start)..byte_offset_to_utf16(text, range.end)
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext, hsla};

    use super::*;

    struct FocusTestRoot {
        input: Entity<TextInput>,
        other_focus: FocusHandle,
    }

    impl Render for FocusTestRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(self.input.clone())
                .child(div().track_focus(&self.other_focus).child("Other"))
        }
    }

    fn text_input<'a>(
        cx: &'a mut TestAppContext,
        value: &'static str,
    ) -> (Entity<TextInput>, &'a mut VisualTestContext) {
        cx.update(super::init);
        let (input, cx) = cx.add_window_view(move |window, cx| {
            TextInput::new(
                value,
                TextInputStyle::new(
                    hsla(0.0, 0.0, 0.9, 1.0),
                    hsla(0.0, 0.0, 0.5, 1.0),
                    hsla(0.6, 0.5, 0.5, 0.4),
                    hsla(0.0, 0.0, 0.9, 1.0),
                ),
                window,
                cx,
            )
        });
        cx.update(|window, cx| {
            window.activate_window();
            input.read(cx).focus_handle().focus(window);
        });
        cx.run_until_parked();
        (input, cx)
    }

    #[test]
    fn grapheme_deletion_should_remove_complete_clusters() {
        let mut buffer = TextBuffer::new("e\u{301}👩‍💻".to_owned());

        buffer.delete_backward();
        buffer.delete_backward();

        assert_eq!(buffer.text, "");
    }

    #[test]
    fn option_delete_should_remove_the_previous_word() {
        let mut buffer = TextBuffer::new("Workspace Name".to_owned());

        buffer.delete_previous_word();

        assert_eq!(buffer.text, "Workspace ");
    }

    #[test]
    fn command_delete_should_remove_text_to_the_beginning() {
        let mut buffer = TextBuffer::new("Workspace Name".to_owned());

        buffer.delete_to_beginning();

        assert_eq!(buffer.text, "");
    }

    #[test]
    fn selection_replacement_should_be_one_undo_step() {
        let mut buffer = TextBuffer::new("Workspace Name".to_owned());
        buffer.select_all();
        buffer.replace_selection("Dev", EditKind::Atomic);

        buffer.undo();

        assert_eq!(buffer.text, "Workspace Name");
    }

    #[test]
    fn utf16_ranges_should_not_split_surrogate_pairs() {
        let text = "A😀B";

        let bytes = utf16_range_to_bytes(text, 1..3);

        assert_eq!(&text[bytes], "😀");
    }

    #[test]
    fn pasted_lines_should_flatten_into_one_line() {
        let mut buffer = TextBuffer::new(String::new());

        buffer.replace_selection("one\r\ntwo\rthree", EditKind::Atomic);

        assert_eq!(buffer.text, "one two three");
    }

    #[gpui::test]
    fn command_a_should_select_all_for_replacement(cx: &mut TestAppContext) {
        let (input, cx) = text_input(cx, "Workspace Name");

        cx.simulate_keystrokes("cmd-a D e v");
        cx.run_until_parked();

        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            "Dev"
        );
    }

    #[gpui::test]
    fn typed_replacement_should_undo_as_one_edit(cx: &mut TestAppContext) {
        let (input, cx) = text_input(cx, "Workspace Name");

        cx.simulate_keystrokes("cmd-a D e v cmd-z");
        cx.run_until_parked();

        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            "Workspace Name"
        );
    }

    #[gpui::test]
    fn command_delete_should_delete_to_the_beginning(cx: &mut TestAppContext) {
        let (input, cx) = text_input(cx, "Workspace Name");

        cx.simulate_keystrokes("cmd-backspace");
        cx.run_until_parked();

        assert_eq!(input.read_with(cx, |input, _| input.value().to_owned()), "");
    }

    #[gpui::test]
    fn option_delete_should_delete_the_previous_word(cx: &mut TestAppContext) {
        let (input, cx) = text_input(cx, "Workspace Name");

        cx.simulate_keystrokes("alt-backspace");
        cx.run_until_parked();

        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            "Workspace "
        );
    }

    #[gpui::test]
    fn control_a_and_e_should_move_to_input_edges(cx: &mut TestAppContext) {
        let (input, cx) = text_input(cx, "Workspace");

        cx.simulate_keystrokes("ctrl-a A ctrl-e Z");
        cx.run_until_parked();

        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            "AWorkspaceZ"
        );
    }

    #[gpui::test]
    fn control_editing_bindings_should_follow_macos(cx: &mut TestAppContext) {
        let (input, cx) = text_input(cx, "abc");

        cx.simulate_keystrokes("ctrl-b ctrl-k ctrl-a ctrl-d ctrl-e ctrl-h");
        cx.run_until_parked();

        assert_eq!(input.read_with(cx, |input, _| input.value().to_owned()), "");
    }

    #[gpui::test]
    fn focus_callbacks_should_track_blur(cx: &mut TestAppContext) {
        cx.update(super::init);
        let mut input = None;
        let mut other_focus = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                TextInput::new(
                    "Workspace",
                    TextInputStyle::new(
                        hsla(0.0, 0.0, 0.9, 1.0),
                        hsla(0.0, 0.0, 0.5, 1.0),
                        hsla(0.6, 0.5, 0.5, 0.4),
                        hsla(0.0, 0.0, 0.9, 1.0),
                    ),
                    window,
                    cx,
                )
            });
            let other = cx.focus_handle();
            input = Some(entity.clone());
            other_focus = Some(other.clone());
            FocusTestRoot {
                input: entity,
                other_focus: other,
            }
        });
        let input = input.unwrap_or_else(|| panic!("test input was not created"));
        let other_focus = other_focus.unwrap_or_else(|| panic!("test focus was not created"));

        cx.update(|window, cx| {
            window.activate_window();
            input.read(cx).focus_handle().focus(window);
            window.refresh();
        });
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| input.focused));

        cx.update(|window, _| {
            other_focus.focus(window);
            window.refresh();
        });
        cx.run_until_parked();

        assert!(!input.read_with(cx, |input, _| input.focused));
    }
}
