//! A bounded, content-safe, multi-line GPUI text editor for editable source such as JSON.
//!
//! The editor shares its buffer, selection, and bounded undo history with [`crate::TextInput`].
//! It keeps line breaks, converts pasted carriage returns and Unicode line separators to `\n`,
//! and replaces a tab with the indent unit, so every line it lays out is one shaped line without
//! control characters. The default value limit is 256 KiB and the absolute limit is 1 MiB.
//!
//! The editor inherits its font from the surrounding text style, so a caller chooses the typeface
//! and size, and it sizes itself to a fixed number of visible rows. Rows beyond them scroll.
//! Lines do not wrap; a line wider than the viewport scrolls horizontally. Only the rows in view
//! are shaped. Read-only content stays focusable, selectable, and copyable.

use std::{collections::HashMap, ops::Range, time::Duration};

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, DispatchPhase, Element,
    ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, Font, GlobalElementId, Hsla, InspectorElementId, IntoElement, KeyBinding, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render,
    ScrollWheelEvent, ShapedLine, SharedString, Style, Subscription, Task, TextRun, UTF16Selection,
    Window, actions, div, fill, point, px, relative, size,
};
use zeroize::Zeroizing;

use crate::TextInputKeybindingProfile;
use crate::menu::{ContextMenu, MenuActivation, MenuEntry, MenuLifecycleEvent};
use crate::text_editing::*;
use crate::text_input::{
    Backspace, Copy, Cut, DeleteForward, DeleteNextWord, DeletePreviousWord, FocusNext,
    FocusPrevious, MoveLeft, MoveRight, MoveToBeginning, MoveToEnd, MoveToNextWord,
    MoveToPreviousWord, Paste, Redo, SelectAll, SelectLeft, SelectRight, SelectToBeginning,
    SelectToEnd, SelectToNextWord, SelectToPreviousWord, ShowCharacterPalette, TextInputPaint,
    Undo, marked_text_runs, recolored_text_runs,
};

const KEY_CONTEXT: &str = "SpaceTermTextArea";
const CARET_BLINK_INTERVAL: Duration = Duration::from_millis(530);
const DEFAULT_VALUE_LIMIT: usize = 256 * 1024;
/// The greatest value a text area retains, enough for a whole configuration document.
const AREA_VALUE_LIMIT: usize = 4 * 1024 * 1024;
const DEFAULT_ROWS: usize = 8;
/// What Tab inserts and Shift-Tab removes, and what Return carries to the next line.
const INDENT: &str = "  ";

actions!(
    spaceterm_text_area,
    [
        MoveUp,
        MoveDown,
        SelectUp,
        SelectDown,
        MoveToLineStart,
        MoveToLineEnd,
        SelectToLineStart,
        SelectToLineEnd,
        DeleteToLineStart,
        DeleteToLineEnd,
        PageUp,
        PageDown,
        SelectPageUp,
        SelectPageDown,
        Newline,
        Indent,
        Outdent,
    ]
);

/// Installs the current text-area bindings for `profile`.
///
/// The clipboard, undo, and selection commands are the same actions [`crate::TextInput`] handles,
/// so an application Edit menu reaches either editor.
pub fn install_text_area_keybindings(cx: &mut App, profile: TextInputKeybindingProfile) {
    let context = Some(KEY_CONTEXT);
    match profile {
        TextInputKeybindingProfile::MacOs => cx.bind_keys([
            KeyBinding::new("backspace", Backspace, context),
            KeyBinding::new("shift-backspace", Backspace, context),
            KeyBinding::new("delete", DeleteForward, context),
            KeyBinding::new("ctrl-h", Backspace, context),
            KeyBinding::new("ctrl-d", DeleteForward, context),
            KeyBinding::new("cmd-backspace", DeleteToLineStart, context),
            KeyBinding::new("cmd-delete", DeleteToLineEnd, context),
            KeyBinding::new("ctrl-k", DeleteToLineEnd, context),
            KeyBinding::new("alt-backspace", DeletePreviousWord, context),
            KeyBinding::new("alt-delete", DeleteNextWord, context),
            KeyBinding::new("left", MoveLeft, context),
            KeyBinding::new("right", MoveRight, context),
            KeyBinding::new("up", MoveUp, context),
            KeyBinding::new("down", MoveDown, context),
            KeyBinding::new("ctrl-b", MoveLeft, context),
            KeyBinding::new("ctrl-f", MoveRight, context),
            KeyBinding::new("ctrl-p", MoveUp, context),
            KeyBinding::new("ctrl-n", MoveDown, context),
            KeyBinding::new("alt-left", MoveToPreviousWord, context),
            KeyBinding::new("alt-right", MoveToNextWord, context),
            KeyBinding::new("cmd-left", MoveToLineStart, context),
            KeyBinding::new("cmd-right", MoveToLineEnd, context),
            KeyBinding::new("ctrl-a", MoveToLineStart, context),
            KeyBinding::new("ctrl-e", MoveToLineEnd, context),
            KeyBinding::new("home", MoveToLineStart, context),
            KeyBinding::new("end", MoveToLineEnd, context),
            KeyBinding::new("cmd-up", MoveToBeginning, context),
            KeyBinding::new("cmd-down", MoveToEnd, context),
            KeyBinding::new("pageup", PageUp, context),
            KeyBinding::new("pagedown", PageDown, context),
            KeyBinding::new("shift-left", SelectLeft, context),
            KeyBinding::new("shift-right", SelectRight, context),
            KeyBinding::new("shift-up", SelectUp, context),
            KeyBinding::new("shift-down", SelectDown, context),
            KeyBinding::new("alt-shift-left", SelectToPreviousWord, context),
            KeyBinding::new("alt-shift-right", SelectToNextWord, context),
            KeyBinding::new("cmd-shift-left", SelectToLineStart, context),
            KeyBinding::new("cmd-shift-right", SelectToLineEnd, context),
            KeyBinding::new("shift-home", SelectToLineStart, context),
            KeyBinding::new("shift-end", SelectToLineEnd, context),
            KeyBinding::new("cmd-shift-up", SelectToBeginning, context),
            KeyBinding::new("cmd-shift-down", SelectToEnd, context),
            KeyBinding::new("shift-pageup", SelectPageUp, context),
            KeyBinding::new("shift-pagedown", SelectPageDown, context),
            KeyBinding::new("cmd-a", SelectAll, context),
            KeyBinding::new("cmd-c", Copy, context),
            KeyBinding::new("cmd-x", Cut, context),
            KeyBinding::new("cmd-v", Paste, context),
            KeyBinding::new("cmd-z", Undo, context),
            KeyBinding::new("cmd-shift-z", Redo, context),
            KeyBinding::new("enter", Newline, context),
            KeyBinding::new("shift-enter", Newline, context),
            KeyBinding::new("tab", Indent, context),
            KeyBinding::new("shift-tab", Outdent, context),
            // Tab indents inside source, so the standard text-view chords leave the editor.
            KeyBinding::new("ctrl-tab", FocusNext, context),
            KeyBinding::new("ctrl-shift-tab", FocusPrevious, context),
            KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, context),
        ]),
    }
}

/// Content-safe editor events. None carries editor content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextAreaEvent {
    /// The value changed and reached this revision.
    ValueChanged {
        revision: u64,
    },
    FocusGained,
    FocusLost,
}

#[derive(Clone, Copy)]
enum TextAreaMenuAction {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

#[derive(Debug)]
struct Composition {
    original: Snapshot,
    marked_range: Range<usize>,
}

#[derive(Clone, Copy, Debug)]
struct PointerGesture {
    generation: u64,
    anchor: usize,
    latest_position: Point<Pixels>,
}

/// What the latest frame laid out: the viewport, the text geometry, and the rows shaped for it.
struct Layout {
    /// The whole element, gutter included.
    bounds: Bounds<Pixels>,
    /// Where column zero of every row starts before horizontal scrolling.
    text_left: Pixels,
    line_height: Pixels,
    font: Font,
    font_size: Pixels,
    color: Hsla,
    revision: u64,
    /// Rows shaped at `revision`, keyed by line index.
    shaped: HashMap<usize, ShapedLine>,
    /// The widest row shaped at `revision`, which bounds horizontal scrolling.
    widest: Pixels,
}

/// A reusable, bounded multi-line text editor.
pub struct TextArea {
    id: ElementId,
    accessibility_name: SharedString,
    debug_selector: SharedString,
    placeholder: SharedString,
    editable: bool,
    line_numbers: bool,
    rows: usize,
    input_length_limit: usize,
    focus_handle: FocusHandle,
    focused: bool,
    window_active: bool,
    buffer: TextBuffer,
    revision: u64,
    /// Byte ranges of every line, excluding its line break, at `revision`.
    lines: Vec<Range<usize>>,
    composition: Option<Composition>,
    /// The horizontal position vertical movement returns to, kept across short lines.
    goal_x: Option<Pixels>,
    scroll: Point<Pixels>,
    /// Keyboard movement and editing bring the caret into view on the next frame.
    reveal_caret: bool,
    layout: Option<Layout>,
    pointer_generation: u64,
    pointer_gesture: Option<PointerGesture>,
    /// A drag the autoscroll timer moved past; the next frame extends the selection to it.
    pending_drag_selection: Option<PointerGesture>,
    autoscroll_task: Option<Task<()>>,
    caret_generation: u64,
    caret_visible: bool,
    caret_task: Option<Task<()>>,
    context_menu_open: bool,
    paste_available: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TextAreaEvent> for TextArea {}

impl TextArea {
    /// Creates an editor with a normalized initial value, truncated at a grapheme boundary to the
    /// default 256 KiB limit. The caret starts at the beginning, where source is read from.
    pub fn new(
        id: impl Into<ElementId>,
        accessibility_name: impl Into<SharedString>,
        initial_value: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle().tab_stop(true);
        let subscriptions = vec![
            cx.on_focus(&focus_handle, window, Self::on_focus),
            cx.on_blur(&focus_handle, window, Self::on_blur),
            cx.observe_window_activation(window, Self::on_window_activation),
        ];
        let initial_value = Zeroizing::new(initial_value.into());
        let normalized = normalize_multiline(&initial_value);
        let value = truncate_grapheme(&normalized, DEFAULT_VALUE_LIMIT).to_owned();
        let accessibility_name = accessibility_name.into();
        let mut buffer = TextBuffer::new(value);
        buffer.move_to(0);
        let lines = line_ranges(&buffer.text);
        Self {
            id: id.into(),
            debug_selector: accessibility_name.clone(),
            accessibility_name,
            placeholder: SharedString::default(),
            editable: true,
            line_numbers: false,
            rows: DEFAULT_ROWS,
            input_length_limit: DEFAULT_VALUE_LIMIT,
            focus_handle,
            focused: false,
            window_active: window.is_window_active(),
            buffer,
            revision: 0,
            lines,
            composition: None,
            goal_x: None,
            scroll: Point::default(),
            reveal_caret: false,
            layout: None,
            pointer_generation: 0,
            pointer_gesture: None,
            pending_drag_selection: None,
            autoscroll_task: None,
            caret_generation: 0,
            caret_visible: true,
            caret_task: None,
            context_menu_open: false,
            paste_available: false,
            _subscriptions: subscriptions,
        }
    }

    /// Sets the placeholder shown when the value is empty.
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }
    /// Sets the initial editable state. Read-only content remains focusable, selectable, and
    /// copyable.
    pub fn editable(mut self, editable: bool) -> Self {
        self.editable = editable;
        self
    }
    /// Shows each row's one-based line number in a leading gutter.
    pub fn line_numbers(mut self, line_numbers: bool) -> Self {
        self.line_numbers = line_numbers;
        self
    }
    /// Sets how many rows the editor shows before it scrolls. At least one row is shown.
    pub fn rows(mut self, rows: usize) -> Self {
        self.rows = rows.max(1);
        self
    }
    /// Sets the optional product limit. `Some` is clamped to 4 MiB; `None` keeps only the hard
    /// 4 MiB limit. A current value above the resulting limit is truncated at a grapheme boundary.
    pub fn input_length_limit(mut self, limit: Option<usize>) -> Self {
        self.input_length_limit = limit.unwrap_or(AREA_VALUE_LIMIT).min(AREA_VALUE_LIMIT);
        let value = truncate_grapheme(&self.buffer.text, self.input_length_limit).to_owned();
        if value != *self.buffer.text {
            self.buffer = TextBuffer::new(value);
            self.buffer.move_to(0);
            self.lines = line_ranges(&self.buffer.text);
        }
        self
    }
    /// Overrides the stable debug selector, which otherwise uses the logical name.
    pub fn debug_selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.debug_selector = selector.into();
        self
    }

    /// Returns the current value.
    pub fn value(&self) -> &str {
        &self.buffer.text
    }
    /// Returns the monotonic content revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Returns whether the editor currently owns responder focus.
    pub fn is_focused(&self) -> bool {
        self.focused
    }
    /// Returns whether keyboard and pointer editing may change the value.
    pub fn is_editable(&self) -> bool {
        self.editable
    }
    /// Returns the focus handle used by a containing composite for explicit focus transfer.
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// Changes whether editing is allowed while preserving selection and copy behavior.
    pub fn set_editable(&mut self, editable: bool, cx: &mut Context<Self>) {
        if self.editable == editable {
            return;
        }
        if !editable {
            self.commit_composition();
            self.cancel_pointer_gesture();
        }
        self.editable = editable;
        self.restart_caret(cx);
    }

    /// Replaces the complete value after normalization.
    ///
    /// A value over the configured or hard limit is rejected whole. Any composition is dropped, the
    /// caret moves to the beginning, the view scrolls to the top, and undo and redo are cleared.
    /// The revision advances and `ValueChanged` is emitted only when the text changes. Returns
    /// whether it changed.
    pub fn set_value(&mut self, value: impl Into<String>, cx: &mut Context<Self>) -> bool {
        let value = Zeroizing::new(value.into());
        let normalized = normalize_multiline(&value);
        if normalized.len() > self.input_length_limit {
            return false;
        }
        self.composition = None;
        self.cancel_pointer_gesture();
        self.buffer.history.clear();
        let changed = *self.buffer.text != normalized;
        self.buffer.text = Zeroizing::new(normalized);
        self.buffer.move_to(0);
        self.goal_x = None;
        self.scroll = Point::default();
        if changed {
            self.advance_revision(cx);
        } else {
            self.restart_caret(cx);
        }
        changed
    }

    /// Selects the complete value.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.commit_composition();
        self.buffer.select_all();
        self.goal_x = None;
        self.restart_caret(cx);
    }

    /// Places the caret at a one-based line and column and scrolls it into view.
    ///
    /// Columns count characters, the way a JSON parser reports a position. A position past the
    /// end of its line or of the value lands on the nearest end.
    pub fn move_caret_to_position(&mut self, line: usize, column: usize, cx: &mut Context<Self>) {
        self.commit_composition();
        let index = line.saturating_sub(1).min(self.lines.len() - 1);
        let range = self.lines[index].clone();
        let offset = self.buffer.text[range.clone()]
            .char_indices()
            .nth(column.saturating_sub(1))
            .map_or(range.end, |(offset, _)| range.start + offset);
        self.buffer.move_to(offset);
        self.goal_x = None;
        self.reveal_caret = true;
        self.restart_caret(cx);
    }

    fn can_edit(&self) -> bool {
        self.editable
    }

    fn is_visually_active(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window) && window.is_window_active()
    }

    fn advance_revision(&mut self, cx: &mut Context<Self>) {
        self.revision = self.revision.saturating_add(1);
        self.lines = line_ranges(&self.buffer.text);
        cx.emit(TextAreaEvent::ValueChanged {
            revision: self.revision,
        });
        self.restart_caret(cx);
    }

    fn finish_edit(&mut self, changed: bool, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.reveal_caret = true;
        if changed {
            self.advance_revision(cx);
        } else {
            self.restart_caret(cx);
        }
    }

    fn on_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focused = true;
        self.window_active = window.is_window_active();
        cx.emit(TextAreaEvent::FocusGained);
        self.restart_caret(cx);
    }
    fn on_blur(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if !self.focused {
            return;
        }
        self.commit_composition();
        self.focused = false;
        self.cancel_pointer_gesture();
        self.caret_task = None;
        self.caret_visible = true;
        self.buffer.history.break_group();
        cx.emit(TextAreaEvent::FocusLost);
        cx.notify();
    }
    fn on_window_activation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window_active = window.is_window_active();
        if !self.window_active {
            self.cancel_pointer_gesture();
        }
        self.restart_caret(cx);
    }

    fn restart_caret(&mut self, cx: &mut Context<Self>) {
        self.caret_generation = self.caret_generation.wrapping_add(1);
        self.caret_visible = true;
        self.caret_task = None;
        if !(self.focused && self.window_active) {
            cx.notify();
            return;
        }
        let generation = self.caret_generation;
        self.caret_task = Some(cx.spawn(async move |area, cx| {
            loop {
                cx.background_executor().timer(CARET_BLINK_INTERVAL).await;
                let keep = area
                    .update(cx, |area, cx| {
                        if area.caret_generation != generation
                            || !area.focused
                            || !area.window_active
                        {
                            return false;
                        }
                        area.caret_visible = !area.caret_visible;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
        cx.notify();
    }

    /// Keeps the text an input method composed and makes it one undoable step.
    fn commit_composition(&mut self) -> bool {
        let Some(state) = self.composition.take() else {
            return false;
        };
        if state.original.text != self.buffer.text {
            self.buffer
                .history
                .record_snapshot(Some(state.original), EditKind::Atomic);
        } else {
            self.buffer.history.break_group();
        }
        true
    }

    /// The line holding `offset`. A line break belongs to the line it ends.
    fn line_of(&self, offset: usize) -> usize {
        self.lines
            .partition_point(|line| line.end < offset)
            .min(self.lines.len() - 1)
    }

    fn replace_selection(&mut self, text: &str, kind: EditKind, cx: &mut Context<Self>) {
        if !self.can_edit() {
            return;
        }
        self.commit_composition();
        let replacement = normalize_multiline(text);
        let changed = self.buffer.replace(
            self.buffer.selection.range.clone(),
            replacement,
            kind,
            self.input_length_limit,
        );
        self.finish_edit(changed, cx);
    }

    fn delete_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        if !self.can_edit() {
            return;
        }
        self.commit_composition();
        let changed = self.buffer.replace(
            range,
            String::new(),
            EditKind::Atomic,
            self.input_length_limit,
        );
        self.finish_edit(changed, cx);
    }

    /// The selection, or the range from the caret to `edge` when nothing is selected.
    fn selection_or(&self, edge: usize) -> Range<usize> {
        if self.buffer.selection.is_empty() {
            let caret = self.buffer.selection.cursor();
            caret.min(edge)..caret.max(edge)
        } else {
            self.buffer.selection.range.clone()
        }
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.can_edit() {
            self.commit_composition();
            let changed = self.buffer.delete_backward(self.input_length_limit);
            self.finish_edit(changed, cx);
        }
    }
    fn delete_forward(&mut self, _: &DeleteForward, _: &mut Window, cx: &mut Context<Self>) {
        if self.can_edit() {
            self.commit_composition();
            let changed = self.buffer.delete_forward(self.input_length_limit);
            self.finish_edit(changed, cx);
        }
    }
    fn delete_to_line_start(
        &mut self,
        _: &DeleteToLineStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let caret = self.buffer.selection.cursor();
        let start = self.lines[self.line_of(caret)].start;
        // At the start of a line the deletion joins it to the line above, as Backspace would.
        let edge = if start == caret {
            previous_grapheme_boundary(&self.buffer.text, caret)
        } else {
            start
        };
        self.delete_range(self.selection_or(edge), cx);
    }
    fn delete_to_line_end(&mut self, _: &DeleteToLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        let caret = self.buffer.selection.cursor();
        let end = self.lines[self.line_of(caret)].end;
        let edge = if end == caret {
            next_grapheme_boundary(&self.buffer.text, caret)
        } else {
            end
        };
        self.delete_range(self.selection_or(edge), cx);
    }
    fn delete_previous_word(
        &mut self,
        _: &DeletePreviousWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let edge = previous_word_start(&self.buffer.text, self.buffer.selection.cursor());
        self.delete_range(self.selection_or(edge), cx);
    }
    fn delete_next_word(&mut self, _: &DeleteNextWord, _: &mut Window, cx: &mut Context<Self>) {
        let edge = next_word_end(&self.buffer.text, self.buffer.selection.cursor());
        self.delete_range(self.selection_or(edge), cx);
    }

    /// Breaks the line and carries the current line's indentation onto the new one.
    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        if !self.can_edit() {
            return;
        }
        if self.commit_composition() {
            self.restart_caret(cx);
            return;
        }
        let start = self.lines[self.line_of(self.buffer.selection.range.start)].start;
        let indentation = leading_indentation(&self.buffer.text[start..]);
        let indentation = &indentation[..indentation
            .len()
            .min(self.buffer.selection.range.start - start)];
        let replacement = format!("\n{indentation}");
        self.replace_selection(&replacement, EditKind::Atomic, cx);
    }

    /// Inserts one indent unit, or indents every selected line.
    fn indent(&mut self, _: &Indent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.can_edit() {
            return;
        }
        if !self.selection_spans_lines() {
            self.replace_selection(INDENT, EditKind::Insert, cx);
            return;
        }
        self.edit_selected_lines(cx, |line| Some(format!("{INDENT}{line}")));
    }

    /// Removes up to one indent unit from the start of every line the selection touches.
    fn outdent(&mut self, _: &Outdent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.can_edit() {
            return;
        }
        self.edit_selected_lines(cx, |line| {
            let removable = leading_indentation(line).len().min(INDENT.len());
            (removable > 0).then(|| line[removable..].to_owned())
        });
    }

    fn selection_spans_lines(&self) -> bool {
        let range = &self.buffer.selection.range;
        self.line_of(range.start) != self.line_of(range.end)
    }

    /// Rewrites every line the selection touches as one undoable edit and selects the result.
    fn edit_selected_lines(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl Fn(&str) -> Option<String>,
    ) {
        self.commit_composition();
        let range = self.buffer.selection.range.clone();
        let first = self.line_of(range.start);
        let mut last = self.line_of(range.end);
        // A selection ending at the start of a line does not reach into that line.
        if last > first && self.lines[last].start == range.end {
            last -= 1;
        }
        let block = self.lines[first].start..self.lines[last].end;
        let mut changed_any = false;
        let rewritten = self.lines[first..=last]
            .iter()
            .map(|line| {
                let text = &self.buffer.text[line.clone()];
                edit(text)
                    .inspect(|_| changed_any = true)
                    .unwrap_or_else(|| text.to_owned())
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !changed_any {
            return;
        }
        let length = rewritten.len();
        let changed = self.buffer.replace(
            block.clone(),
            rewritten,
            EditKind::Atomic,
            self.input_length_limit,
        );
        if changed {
            self.buffer.selection = Selection {
                range: block.start..block.start + length,
                reversed: false,
            };
        }
        self.finish_edit(changed, cx);
    }

    fn move_horizontal(&mut self, right: bool, extend: bool, cx: &mut Context<Self>) {
        self.commit_composition();
        if right {
            self.buffer.move_right(extend);
        } else {
            self.buffer.move_left(extend);
        }
        self.goal_x = None;
        self.reveal_caret = true;
        self.restart_caret(cx);
    }
    fn move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_horizontal(false, false, cx);
    }
    fn move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_horizontal(true, false, cx);
    }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_horizontal(false, true, cx);
    }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_horizontal(true, true, cx);
    }

    /// Moves the caret, or extends the selection, to `offset`.
    fn move_caret(&mut self, offset: usize, extend: bool, cx: &mut Context<Self>) {
        self.commit_composition();
        if extend {
            self.buffer.select_to(offset);
        } else {
            self.buffer.move_to(offset);
        }
        self.reveal_caret = true;
        self.restart_caret(cx);
    }

    fn move_word(&mut self, next: bool, extend: bool, cx: &mut Context<Self>) {
        self.commit_composition();
        self.buffer.move_word(next, extend);
        self.goal_x = None;
        self.reveal_caret = true;
        self.restart_caret(cx);
    }
    fn move_to_previous_word(
        &mut self,
        _: &MoveToPreviousWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_word(false, false, cx);
    }
    fn move_to_next_word(&mut self, _: &MoveToNextWord, _: &mut Window, cx: &mut Context<Self>) {
        self.move_word(true, false, cx);
    }
    fn select_to_previous_word(
        &mut self,
        _: &SelectToPreviousWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_word(false, true, cx);
    }
    fn select_to_next_word(
        &mut self,
        _: &SelectToNextWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_word(true, true, cx);
    }

    fn move_line_edge(&mut self, end: bool, extend: bool, cx: &mut Context<Self>) {
        let line = &self.lines[self.line_of(self.buffer.selection.cursor())];
        let offset = if end { line.end } else { line.start };
        self.goal_x = None;
        self.move_caret(offset, extend, cx);
    }
    fn move_to_line_start(&mut self, _: &MoveToLineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_line_edge(false, false, cx);
    }
    fn move_to_line_end(&mut self, _: &MoveToLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_line_edge(true, false, cx);
    }
    fn select_to_line_start(
        &mut self,
        _: &SelectToLineStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_line_edge(false, true, cx);
    }
    fn select_to_line_end(&mut self, _: &SelectToLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_line_edge(true, true, cx);
    }

    fn move_document_edge(&mut self, end: bool, extend: bool, cx: &mut Context<Self>) {
        self.goal_x = None;
        let offset = if end { self.buffer.text.len() } else { 0 };
        self.move_caret(offset, extend, cx);
    }
    fn move_to_beginning(&mut self, _: &MoveToBeginning, _: &mut Window, cx: &mut Context<Self>) {
        self.move_document_edge(false, false, cx);
    }
    fn move_to_end(&mut self, _: &MoveToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_document_edge(true, false, cx);
    }
    fn select_to_beginning(
        &mut self,
        _: &SelectToBeginning,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_document_edge(false, true, cx);
    }
    fn select_to_end(&mut self, _: &SelectToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_document_edge(true, true, cx);
    }

    /// Moves by whole rows, keeping the horizontal position the movement started from.
    ///
    /// Without a selection to extend, a collapsing movement starts from the selection's own edge in
    /// its direction. Moving past the first or last row reaches that end of the value.
    fn move_vertically(
        &mut self,
        rows: isize,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_composition();
        let from = if extend || self.buffer.selection.is_empty() {
            self.buffer.selection.cursor()
        } else if rows < 0 {
            self.buffer.selection.range.start
        } else {
            self.buffer.selection.range.end
        };
        let line = self.line_of(from);
        let goal = match self.goal_x {
            Some(goal) => goal,
            None => self.x_for_offset(line, from, window),
        };
        let target = line as isize + rows;
        let offset = if target < 0 {
            0
        } else if target as usize >= self.lines.len() {
            self.buffer.text.len()
        } else {
            self.offset_for_x(target as usize, goal, window)
        };
        self.move_caret(offset, extend, cx);
        self.goal_x = Some(goal);
    }
    fn move_up(&mut self, _: &MoveUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(-1, false, window, cx);
    }
    fn move_down(&mut self, _: &MoveDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(1, false, window, cx);
    }
    fn select_up(&mut self, _: &SelectUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(-1, true, window, cx);
    }
    fn select_down(&mut self, _: &SelectDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(1, true, window, cx);
    }
    fn page_rows(&self) -> isize {
        self.rows.saturating_sub(1).max(1) as isize
    }
    fn page_up(&mut self, _: &PageUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(-self.page_rows(), false, window, cx);
    }
    fn page_down(&mut self, _: &PageDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(self.page_rows(), false, window, cx);
    }
    fn select_page_up(&mut self, _: &SelectPageUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(-self.page_rows(), true, window, cx);
    }
    fn select_page_down(
        &mut self,
        _: &SelectPageDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_vertically(self.page_rows(), true, window, cx);
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
        if self.can_edit()
            && let Some(text) = self.buffer.selected_text().map(ToOwned::to_owned)
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.delete_range(self.buffer.selection.range.clone(), cx);
        }
        cx.stop_propagation();
    }
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(bounded_clipboard_text)
            && self.can_accept_paste(&text)
        {
            self.replace_selection(&text, EditKind::Atomic, cx);
        }
        cx.stop_propagation();
    }
    fn can_accept_paste(&self, text: &str) -> bool {
        if !self.can_edit() || text.len() > CLIPBOARD_INSERTION_LIMIT {
            return false;
        }
        let replacement_len = normalize_multiline(text).len();
        (!self.buffer.selection.is_empty() || replacement_len > 0)
            && self.buffer.can_replace(
                &self.buffer.selection.range,
                replacement_len,
                self.input_length_limit,
            )
    }
    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.can_edit() {
            self.commit_composition();
            let changed = self.buffer.undo();
            self.finish_edit(changed, cx);
        }
        cx.stop_propagation();
    }
    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.can_edit() {
            self.commit_composition();
            let changed = self.buffer.redo();
            self.finish_edit(changed, cx);
        }
        cx.stop_propagation();
    }
    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_next(cx);
        cx.stop_propagation();
    }
    fn focus_previous(&mut self, _: &FocusPrevious, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_prev(cx);
        cx.stop_propagation();
    }
    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.can_edit() {
            window.show_character_palette();
            cx.stop_propagation();
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            return;
        }
        self.focus_handle.focus(window, cx);
        self.commit_composition();
        let offset = self.offset_for_position(event.position, window);
        match event.click_count {
            1 if event.modifiers.shift => self.buffer.select_to(offset),
            1 => self.buffer.move_to(offset),
            2 => {
                self.buffer.selection = Selection {
                    range: word_range_at(&self.buffer.text, offset),
                    reversed: false,
                }
            }
            _ => {
                // A triple click selects the line with its break, so a drag or copy takes whole
                // lines.
                let line = &self.lines[self.line_of(offset)];
                let end = next_grapheme_boundary(&self.buffer.text, line.end).max(line.end);
                self.buffer.selection = Selection {
                    range: line.start..end,
                    reversed: false,
                }
            }
        }
        self.goal_x = None;
        self.pointer_generation = self.pointer_generation.wrapping_add(1);
        self.pointer_gesture = Some(PointerGesture {
            generation: self.pointer_generation,
            anchor: self.buffer.selection.anchor(),
            latest_position: event.position,
        });
        self.restart_caret(cx);
        cx.stop_propagation();
    }

    fn on_global_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut gesture) = self.pointer_gesture else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.cancel_pointer_gesture();
            return;
        }
        gesture.latest_position = event.position;
        self.pointer_gesture = Some(gesture);
        if self.overflow(event.position) != Point::default() {
            self.start_autoscroll(gesture.generation, cx);
        } else {
            self.autoscroll_task = None;
            let offset = self.offset_for_position(event.position, window);
            self.buffer.select_from_anchor(gesture.anchor, offset);
            self.restart_caret(cx);
        }
        cx.stop_propagation();
    }

    fn on_global_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if event.button == MouseButton::Left && self.pointer_gesture.is_some() {
            self.cancel_pointer_gesture();
            cx.stop_propagation();
        }
    }

    fn cancel_pointer_gesture(&mut self) {
        self.pointer_generation = self.pointer_generation.wrapping_add(1);
        self.pointer_gesture = None;
        self.autoscroll_task = None;
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(layout) = &self.layout else {
            return;
        };
        let delta = event.delta.pixel_delta(layout.line_height);
        let before = self.scroll;
        self.scroll = self.clamp_scroll(point(self.scroll.x - delta.x, self.scroll.y - delta.y));
        if self.scroll != before {
            cx.notify();
            cx.stop_propagation();
        }
    }

    /// How far `position` lies outside the text viewport on each axis, or zero inside it.
    fn overflow(&self, position: Point<Pixels>) -> Point<Pixels> {
        let Some(layout) = &self.layout else {
            return Point::default();
        };
        let bounds = layout.bounds;
        let outside = |value: Pixels, low: Pixels, high: Pixels| {
            if value < low {
                value - low
            } else if value > high {
                value - high
            } else {
                px(0.0)
            }
        };
        point(
            outside(position.x, layout.text_left, bounds.right()),
            outside(position.y, bounds.top(), bounds.bottom()),
        )
    }

    fn start_autoscroll(&mut self, generation: u64, cx: &mut Context<Self>) {
        if self.autoscroll_task.is_some() {
            return;
        }
        let metrics = crate::floating_surface::hosted_text_input_theme(cx).metrics;
        self.autoscroll_task = Some(cx.spawn(async move |area, cx| {
            loop {
                cx.background_executor()
                    .timer(metrics.autoscroll_interval)
                    .await;
                let keep = area
                    .update(cx, |area, cx| {
                        let Some(gesture) = area.pointer_gesture else {
                            return false;
                        };
                        if gesture.generation != generation {
                            return false;
                        }
                        let overflow = area.overflow(gesture.latest_position);
                        if overflow == Point::default() {
                            return false;
                        }
                        let step = |value: Pixels| {
                            value.clamp(-metrics.autoscroll_max_step, metrics.autoscroll_max_step)
                        };
                        area.scroll = area.clamp_scroll(point(
                            area.scroll.x + step(overflow.x),
                            area.scroll.y + step(overflow.y),
                        ));
                        area.pending_drag_selection = Some(gesture);
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
            let _ = area.update(cx, |area, _| area.autoscroll_task = None);
        }));
    }

    /// The largest scroll offsets the latest layout allows.
    fn clamp_scroll(&self, scroll: Point<Pixels>) -> Point<Pixels> {
        let Some(layout) = &self.layout else {
            return Point::default();
        };
        let content_height = layout.line_height * self.lines.len() as f32;
        let viewport = layout.bounds.size;
        let text_width = layout.bounds.right() - layout.text_left;
        let max_y = (content_height - viewport.height).max(px(0.0));
        let max_x = (layout.widest - text_width).max(px(0.0));
        point(
            scroll.x.clamp(px(0.0), max_x),
            scroll.y.clamp(px(0.0), max_y),
        )
    }

    /// Shapes one row at the latest layout's font, reusing the row shaped for this revision.
    fn shaped_line(&mut self, index: usize, window: &mut Window) -> Option<ShapedLine> {
        let revision = self.revision;
        let range = self.lines.get(index)?.clone();
        let layout = self.layout.as_mut()?;
        if layout.revision != revision {
            layout.revision = revision;
            layout.shaped.clear();
            layout.widest = px(0.0);
        }
        if let Some(line) = layout.shaped.get(&index) {
            return Some(line.clone());
        }
        let text = SharedString::from(self.buffer.text[range].to_owned());
        let run = TextRun {
            len: text.len(),
            font: layout.font.clone(),
            color: layout.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window
            .text_system()
            .shape_line(text, layout.font_size, &[run], None);
        layout.widest = layout.widest.max(line.width);
        layout.shaped.insert(index, line.clone());
        Some(line)
    }

    /// The horizontal position of `offset` within its row, before scrolling.
    fn x_for_offset(&mut self, line: usize, offset: usize, window: &mut Window) -> Pixels {
        let start = self.lines[line].start;
        match self.shaped_line(line, window) {
            Some(shaped) => shaped.x_for_index(offset - start),
            // Before the first layout there is no geometry, so the column stands in for it.
            None => px(self.buffer.text[start..offset].chars().count() as f32),
        }
    }

    /// The offset in row `line` nearest to horizontal position `x`, before scrolling.
    fn offset_for_x(&mut self, line: usize, x: Pixels, window: &mut Window) -> usize {
        let range = self.lines[line].clone();
        let index = match self.shaped_line(line, window) {
            Some(shaped) => shaped.closest_index_for_x(x),
            None => self.buffer.text[range.clone()]
                .char_indices()
                .nth(f32::from(x).max(0.0) as usize)
                .map_or(range.len(), |(index, _)| index),
        };
        clamp_grapheme_boundary(
            &self.buffer.text,
            range.start + index.min(range.len()),
            false,
        )
    }

    fn offset_for_position(&mut self, position: Point<Pixels>, window: &mut Window) -> usize {
        let Some(layout) = &self.layout else {
            return self.buffer.selection.cursor();
        };
        let row = ((position.y - layout.bounds.top() + self.scroll.y) / layout.line_height)
            .floor()
            .max(0.0) as usize;
        if row >= self.lines.len() {
            return self.buffer.text.len();
        }
        let x = position.x - layout.text_left + self.scroll.x;
        self.offset_for_x(row, x, window)
    }

    /// Scrolls the least distance that shows the caret with the theme's padding beside it.
    fn scroll_caret_into_view(&mut self, padding: Pixels, window: &mut Window) {
        let caret = self.buffer.selection.cursor();
        let line = self.line_of(caret);
        let caret_x = self.x_for_offset(line, caret, window);
        let Some(layout) = &self.layout else {
            return;
        };
        let viewport = layout.bounds.size.height;
        let text_width = layout.bounds.right() - layout.text_left;
        let top = layout.line_height * line as f32;
        let bottom = top + layout.line_height;
        let mut scroll = self.scroll;
        if top < scroll.y {
            scroll.y = top;
        } else if bottom > scroll.y + viewport {
            scroll.y = bottom - viewport;
        }
        if caret_x < scroll.x {
            scroll.x = (caret_x - padding).max(px(0.0));
        } else if caret_x > scroll.x + text_width - padding {
            scroll.x = caret_x - text_width + padding;
        }
        self.scroll = point(scroll.x.max(px(0.0)), scroll.y.max(px(0.0)));
    }

    fn refresh_paste_availability(&mut self, cx: &mut Context<Self>) {
        self.paste_available = cx
            .read_from_clipboard()
            .and_then(bounded_clipboard_text)
            .is_some_and(|text| self.can_accept_paste(&text));
    }
}

impl Focusable for TextArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for TextArea {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = utf16_query_range_to_bytes(&self.buffer.text, range_utf16);
        adjusted.replace(byte_range_to_utf16(&self.buffer.text, range.clone()));
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
        self.composition
            .as_ref()
            .map(|state| byte_range_to_utf16(&self.buffer.text, state.marked_range.clone()))
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.commit_composition() {
            self.restart_caret(cx);
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_edit() {
            return;
        }
        let composing = self.composition.is_some();
        let range = range_utf16
            .map(|range| utf16_replacement_range_to_bytes(&self.buffer.text, range))
            .or_else(|| {
                self.composition
                    .as_ref()
                    .map(|state| state.marked_range.clone())
            })
            .unwrap_or_else(|| self.buffer.selection.range.clone());
        let replacement = normalize_multiline(text);
        let changed = if composing {
            self.buffer
                .replace_without_history(range, &replacement, self.input_length_limit)
        } else {
            self.buffer.replace(
                range,
                replacement,
                EditKind::Insert,
                self.input_length_limit,
            )
        };
        self.finish_edit(changed, cx);
        self.commit_composition();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_edit() {
            return;
        }
        let range = range_utf16
            .map(|range| utf16_replacement_range_to_bytes(&self.buffer.text, range))
            .or_else(|| {
                self.composition
                    .as_ref()
                    .map(|state| state.marked_range.clone())
            })
            .unwrap_or_else(|| self.buffer.selection.range.clone());
        let range = normalize_byte_range(&self.buffer.text, range);
        let marked = normalize_multiline(text);
        if !self
            .buffer
            .can_replace(&range, marked.len(), self.input_length_limit)
        {
            return;
        }
        if self.composition.is_none() {
            self.composition = Some(Composition {
                original: self.buffer.snapshot(),
                marked_range: range.clone(),
            });
        }
        let selected =
            selected_utf16.map(|selected| utf16_selection_range_to_bytes(&marked, selected));
        let marked_len = marked.len();
        let changed =
            self.buffer
                .replace_without_history(range.clone(), &marked, self.input_length_limit);
        if let Some(composition) = &mut self.composition {
            composition.marked_range = range.start..range.start + marked_len;
        }
        self.buffer.selection = selected.map_or_else(
            || Selection::caret(range.start + marked_len),
            |selected| Selection {
                range: range.start + selected.start..range.start + selected.end,
                reversed: false,
            },
        );
        self.finish_edit(changed, cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _: Bounds<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = utf16_query_range_to_bytes(&self.buffer.text, range_utf16);
        let line = self.line_of(range.start);
        let start_x = self.x_for_offset(line, range.start, window);
        let end = range.end.min(self.lines[line].end);
        let end_x = self.x_for_offset(line, end, window);
        let layout = self.layout.as_ref()?;
        let top = layout.bounds.top() + layout.line_height * line as f32 - self.scroll.y;
        let left = layout.text_left - self.scroll.x;
        Some(Bounds::from_corners(
            point(left + start_x, top),
            point(left + end_x, top + layout.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.layout.as_ref()?;
        let offset = self.offset_for_position(point, window);
        Some(byte_offset_to_utf16(&self.buffer.text, offset))
    }
}

impl Render for TextArea {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let has_selection = !self.buffer.selection.is_empty();
        let can_edit = self.can_edit();
        let entries = vec![
            MenuEntry::action("Undo", TextAreaMenuAction::Undo)
                .disabled(!can_edit || self.buffer.history.undo.is_empty()),
            MenuEntry::action("Redo", TextAreaMenuAction::Redo)
                .disabled(!can_edit || self.buffer.history.redo.is_empty()),
            MenuEntry::separator(),
            MenuEntry::action("Cut", TextAreaMenuAction::Cut).disabled(!can_edit || !has_selection),
            MenuEntry::action("Copy", TextAreaMenuAction::Copy).disabled(!has_selection),
            MenuEntry::action("Paste", TextAreaMenuAction::Paste).disabled(!self.paste_available),
            MenuEntry::action("Select All", TextAreaMenuAction::SelectAll)
                .disabled(self.buffer.text.is_empty()),
        ];
        let selector = self.debug_selector.clone();
        let editor = div()
            .id(self.id.clone())
            .debug_selector(move || selector.to_string())
            .w_full()
            .min_w_0()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete_forward))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::delete_to_line_end))
            .on_action(cx.listener(Self::delete_previous_word))
            .on_action(cx.listener(Self::delete_next_word))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::indent))
            .on_action(cx.listener(Self::outdent))
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::move_to_previous_word))
            .on_action(cx.listener(Self::move_to_next_word))
            .on_action(cx.listener(Self::select_to_previous_word))
            .on_action(cx.listener(Self::select_to_next_word))
            .on_action(cx.listener(Self::move_to_line_start))
            .on_action(cx.listener(Self::move_to_line_end))
            .on_action(cx.listener(Self::select_to_line_start))
            .on_action(cx.listener(Self::select_to_line_end))
            .on_action(cx.listener(Self::move_to_beginning))
            .on_action(cx.listener(Self::move_to_end))
            .on_action(cx.listener(Self::select_to_beginning))
            .on_action(cx.listener(Self::select_to_end))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::select_page_up))
            .on_action(cx.listener(Self::select_page_down))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_previous))
            .on_action(cx.listener(Self::show_character_palette))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .child(TextAreaElement {
                area: entity.clone(),
            });
        let menu_open = entity.downgrade();
        let menu_lifecycle = entity.downgrade();
        let menu_activate = entity.downgrade();
        ContextMenu::new(
            ("text-area-context-menu", entity.entity_id()),
            self.accessibility_name.clone(),
            editor,
            entries,
        )
        .fill_parent_width()
        .on_open_request(move |_, _, cx| {
            menu_open
                .update(cx, |area, cx| {
                    area.refresh_paste_availability(cx);
                    area.context_menu_open = true;
                    area.cancel_pointer_gesture();
                    cx.notify();
                    true
                })
                .unwrap_or(false)
        })
        .on_lifecycle(move |event, cx| {
            if matches!(event, MenuLifecycleEvent::Closed(_)) {
                let _ = menu_lifecycle.update(cx, |area, _| area.context_menu_open = false);
            }
        })
        .on_activate(
            move |activation: &MenuActivation<TextAreaMenuAction>, window, cx| {
                let action = *activation.action();
                let _ = menu_activate.update(cx, |area, cx| match action {
                    TextAreaMenuAction::Undo => area.undo(&Undo, window, cx),
                    TextAreaMenuAction::Redo => area.redo(&Redo, window, cx),
                    TextAreaMenuAction::Cut => area.cut(&Cut, window, cx),
                    TextAreaMenuAction::Copy => area.copy(&Copy, window, cx),
                    TextAreaMenuAction::Paste => area.paste(&Paste, window, cx),
                    TextAreaMenuAction::SelectAll => area.on_select_all(&SelectAll, window, cx),
                });
            },
        )
    }
}

struct TextAreaElement {
    area: Entity<TextArea>,
}

/// One row the frame paints.
struct PaintedRow {
    origin: Point<Pixels>,
    line: ShapedLine,
    /// The same row recolored for the part the selection covers, when that color differs.
    selected: Option<(ShapedLine, Bounds<Pixels>)>,
    number: Option<(Point<Pixels>, ShapedLine)>,
}

struct TextAreaPrepaint {
    rows: Vec<PaintedRow>,
    selections: Vec<Bounds<Pixels>>,
    caret: Option<Bounds<Pixels>>,
    placeholder: Option<ShapedLine>,
    paint: TextInputPaint,
    text_bounds: Bounds<Pixels>,
}

impl IntoElement for TextAreaElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextAreaElement {
    type RequestLayoutState = ();
    type PrepaintState = TextAreaPrepaint;
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
        let rows = self.area.read(cx).rows;
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = (window.line_height() * rows as f32).into();
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
    ) -> TextAreaPrepaint {
        self.area.update(cx, |area, cx| {
            let theme = *crate::floating_surface::hosted_text_input_theme(cx);
            let paint = theme.paint(crate::TextInputVariant::Standard);
            let text_style = window.text_style();
            let font = text_style.font();
            let font_size = text_style.font_size.to_pixels(window.rem_size());
            let line_height = window.line_height();
            let text_color: Hsla = paint.text.into();
            let muted: Hsla = paint.placeholder.into();
            let run = |len: usize, color: Hsla| TextRun {
                len,
                font: font.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            // The gutter holds the widest line number with one digit's room to spare on its
            // trailing side, so the numbers never touch the text.
            let digits = area.lines.len().to_string().len().max(2);
            let gutter = area.line_numbers.then(|| {
                let sample = SharedString::from("0".repeat(digits + 1));
                window
                    .text_system()
                    .shape_line(sample.clone(), font_size, &[run(sample.len(), muted)], None)
                    .width
            });
            let text_left = bounds.left() + gutter.unwrap_or_default();
            let revision = area.revision;
            let (shaped, widest) = match area.layout.take() {
                Some(layout)
                    if layout.revision == revision
                        && layout.font == font
                        && layout.font_size == font_size
                        && layout.color == text_color =>
                {
                    (layout.shaped, layout.widest)
                }
                _ => (HashMap::new(), px(0.0)),
            };
            area.layout = Some(Layout {
                bounds,
                text_left,
                line_height,
                font: font.clone(),
                font_size,
                color: text_color,
                revision,
                shaped,
                widest,
            });
            if let Some(gesture) = area.pending_drag_selection.take() {
                let offset = area.offset_for_position(gesture.latest_position, window);
                area.buffer.select_from_anchor(gesture.anchor, offset);
            }
            if std::mem::take(&mut area.reveal_caret) {
                area.scroll_caret_into_view(theme.metrics.scroll_padding, window);
            }
            let first = (area.scroll.y / line_height).floor().max(0.0) as usize;
            let visible = (bounds.size.height / line_height).ceil() as usize + 1;
            let last = (first + visible).min(area.lines.len());
            for index in first..last {
                area.shaped_line(index, window);
            }
            area.scroll = area.clamp_scroll(area.scroll);
            let scroll = area.scroll;
            let active = area.is_visually_active(window);
            let marked = active
                .then(|| {
                    area.composition
                        .as_ref()
                        .map(|state| state.marked_range.clone())
                })
                .flatten();
            let selection = area.buffer.selection.range.clone();
            let caret_offset = area.buffer.selection.cursor();
            let caret_line = area.line_of(caret_offset);
            let recolor = paint.selection_foreground != paint.text;
            let mut rows = Vec::with_capacity(last - first);
            let mut selections = Vec::new();
            let mut caret = None;
            for index in first..last {
                let range = area.lines[index].clone();
                let top = bounds.top() + line_height * index as f32 - scroll.y;
                let origin = point(text_left - scroll.x, top);
                // A composing row is reshaped with its marked text underlined.
                let line = match marked
                    .as_ref()
                    .filter(|marked| marked.start <= range.end && marked.end >= range.start)
                {
                    Some(marked) => {
                        let text = SharedString::from(area.buffer.text[range.clone()].to_owned());
                        let local = marked.start.max(range.start) - range.start
                            ..marked.end.min(range.end) - range.start;
                        let runs =
                            marked_text_runs(&text, Some(local), run(text.len(), text_color));
                        window
                            .text_system()
                            .shape_line(text, font_size, &runs, None)
                    }
                    None => area
                        .shaped_line(index, window)
                        .expect("visible rows are shaped"),
                };
                let mut selected = None;
                if active && !selection.is_empty() {
                    let start = selection.start.max(range.start);
                    let end = selection.end.min(range.end);
                    // A selection running past the end of a row covers its line break too, which
                    // shows as a narrow band after the last glyph.
                    let covers_break = selection.end > range.end && selection.start <= range.end;
                    if start <= end && (start < end || covers_break) {
                        let left = origin.x + line.x_for_index(start - range.start);
                        let mut right = origin.x + line.x_for_index(end - range.start);
                        if covers_break {
                            right += font_size / 2.0;
                        }
                        let band =
                            Bounds::from_corners(point(left, top), point(right, top + line_height));
                        selections.push(band);
                        if recolor && start < end {
                            let text =
                                SharedString::from(area.buffer.text[range.clone()].to_owned());
                            let runs = recolored_text_runs(
                                &[run(text.len(), text_color)],
                                paint.selection_foreground.into(),
                            );
                            selected = Some((
                                window
                                    .text_system()
                                    .shape_line(text, font_size, &runs, None),
                                band,
                            ));
                        }
                    }
                }
                if active && selection.is_empty() && area.caret_visible && index == caret_line {
                    let x = origin.x + line.x_for_index(caret_offset - range.start);
                    caret = Some(Bounds::new(
                        point(x, top),
                        size(theme.metrics.caret_width, line_height),
                    ));
                }
                let number = gutter.map(|gutter| {
                    let label = SharedString::from((index + 1).to_string());
                    let shaped = window.text_system().shape_line(
                        label.clone(),
                        font_size,
                        &[run(label.len(), muted)],
                        None,
                    );
                    let digit = gutter / (digits + 1) as f32;
                    (
                        point(bounds.left() + gutter - digit - shaped.width, top),
                        shaped,
                    )
                });
                rows.push(PaintedRow {
                    origin,
                    line,
                    selected,
                    number,
                });
            }
            let placeholder =
                (area.buffer.text.is_empty() && !area.placeholder.is_empty()).then(|| {
                    window.text_system().shape_line(
                        area.placeholder.clone(),
                        font_size,
                        &[run(area.placeholder.len(), muted)],
                        None,
                    )
                });
            TextAreaPrepaint {
                rows,
                selections,
                caret,
                placeholder,
                paint,
                text_bounds: Bounds::from_corners(
                    point(text_left, bounds.top()),
                    bounds.bottom_right(),
                ),
            }
        })
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut TextAreaPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.area.read(cx).focus_handle.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.area.clone()),
            cx,
        );
        let move_area = self.area.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                move_area.update(cx, |area, cx| area.on_global_mouse_move(event, window, cx));
            }
        });
        let up_area = self.area.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Capture {
                up_area.update(cx, |area, cx| area.on_global_mouse_up(event, cx));
            }
        });
        let line_height = window.line_height();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for row in &prepaint.rows {
                if let Some((origin, number)) = &row.number {
                    _ = number.paint(
                        *origin,
                        line_height,
                        gpui::TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
            }
            let text_bounds = prepaint.text_bounds;
            window.with_content_mask(
                Some(ContentMask {
                    bounds: text_bounds,
                }),
                |window| {
                    for band in &prepaint.selections {
                        window.paint_quad(fill(*band, prepaint.paint.selection));
                    }
                    if let Some(placeholder) = &prepaint.placeholder {
                        _ = placeholder.paint(
                            text_bounds.origin,
                            line_height,
                            gpui::TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                    for row in &prepaint.rows {
                        _ = row.line.paint(
                            row.origin,
                            line_height,
                            gpui::TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                        if let Some((selected, band)) = &row.selected {
                            window.with_content_mask(
                                Some(ContentMask { bounds: *band }),
                                |window| {
                                    _ = selected.paint(
                                        row.origin,
                                        line_height,
                                        gpui::TextAlign::Left,
                                        None,
                                        window,
                                        cx,
                                    );
                                },
                            );
                        }
                    }
                    if let Some(caret) = prepaint.caret {
                        window.paint_quad(fill(caret, prepaint.paint.caret));
                    }
                },
            );
        });
    }
}

/// Keeps line breaks, turns every other line separator into `\n` and a tab into the indent unit,
/// and replaces remaining control characters with spaces, so each line shapes as plain text.
fn normalize_multiline(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                result.push('\n');
            }
            '\n' | '\u{2028}' | '\u{2029}' => result.push('\n'),
            '\t' => result.push_str(INDENT),
            ch if ch.is_control() => result.push(' '),
            ch => result.push(ch),
        }
    }
    result
}

/// The byte range of every line, excluding its line break. An empty value has one empty line.
fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, _) in text.match_indices('\n') {
        lines.push(start..index);
        start = index + 1;
    }
    lines.push(start..text.len());
    lines
}

/// The spaces a line starts with.
fn leading_indentation(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches(' ').len()]
}

#[cfg(test)]
#[path = "text_area_tests.rs"]
mod tests;
