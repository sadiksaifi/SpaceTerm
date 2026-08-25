//! A bounded, content-safe, single-line GPUI text editor.
//!
//! Values are always normalized to one line. The default value limit is 64 KiB and the absolute
//! value limit is 1 MiB. Clipboard insertion is limited to 1 MiB before the configured value
//! limit is applied. Undo and redo retain at most 128 snapshots and 1 MiB of text in total. The
//! application kill ring retains at most 64 KiB. All limits are byte limits and every truncation
//! performed during construction or kill-ring capture ends at a complete grapheme boundary.

use std::{ops::Range, time::Duration};

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, DispatchPhase, Element,
    ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, Font, Global, GlobalElementId, InspectorElementId, IntoElement, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    Render, Rgba, ShapedLine, SharedString, Style, Subscription, Task, TextRun, UTF16Selection,
    UnderlineStyle, Window, actions, div, fill, point, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::menu::{ContextMenu, MenuActivation, MenuEntry, MenuLifecycleEvent};

const KEY_CONTEXT: &str = "SpaceTermTextInput";
const CARET_BLINK_INTERVAL: Duration = Duration::from_millis(530);
const HARD_VALUE_LIMIT: usize = 1024 * 1024;
const DEFAULT_VALUE_LIMIT: usize = 64 * 1024;
const CLIPBOARD_INSERTION_LIMIT: usize = 1024 * 1024;
const KILL_RING_LIMIT: usize = 64 * 1024;
const HISTORY_ENTRY_LIMIT: usize = 128;
const HISTORY_BYTE_LIMIT: usize = 1024 * 1024;

#[derive(Default)]
struct TextKillRing(String);

impl Global for TextKillRing {}

actions!(
    spaceterm_text_input,
    [
        Backspace,
        DeleteForward,
        DeleteToBeginning,
        DeleteToEnd,
        DeletePreviousWord,
        DeleteNextWord,
        KillToBeginning,
        KillToEnd,
        KillPreviousWord,
        Yank,
        Transpose,
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

/// Installs the bounded application kill ring.
///
/// Key behavior is intentionally not installed here. Applications select it explicitly with
/// [`install_text_input_keybindings`].
pub(crate) fn init(cx: &mut App) {
    if !cx.has_global::<TextKillRing>() {
        cx.set_global(TextKillRing::default());
    }
}

/// A platform-neutral name for a complete text-input keybinding set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextInputKeybindingProfile {
    /// Conventional macOS editing aliases. Selecting this profile is explicit and performs no
    /// operating-system detection.
    MacOs,
}

/// Installs the current bindings for `profile`.
pub fn install_text_input_keybindings(cx: &mut App, profile: TextInputKeybindingProfile) {
    match profile {
        TextInputKeybindingProfile::MacOs => cx.bind_keys([
            KeyBinding::new("backspace", Backspace, Some(KEY_CONTEXT)),
            KeyBinding::new("delete", DeleteForward, Some(KEY_CONTEXT)),
            KeyBinding::new("cmd-backspace", DeleteToBeginning, Some(KEY_CONTEXT)),
            KeyBinding::new("cmd-delete", DeleteToEnd, Some(KEY_CONTEXT)),
            KeyBinding::new("alt-backspace", DeletePreviousWord, Some(KEY_CONTEXT)),
            KeyBinding::new("alt-delete", DeleteNextWord, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-h", Backspace, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-d", DeleteForward, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-u", KillToBeginning, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-k", KillToEnd, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-w", KillPreviousWord, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-y", Yank, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-t", Transpose, Some(KEY_CONTEXT)),
            KeyBinding::new("left", MoveLeft, Some(KEY_CONTEXT)),
            KeyBinding::new("right", MoveRight, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-b", MoveLeft, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-f", MoveRight, Some(KEY_CONTEXT)),
            KeyBinding::new("home", MoveToBeginning, Some(KEY_CONTEXT)),
            KeyBinding::new("end", MoveToEnd, Some(KEY_CONTEXT)),
            KeyBinding::new("cmd-left", MoveToBeginning, Some(KEY_CONTEXT)),
            KeyBinding::new("cmd-up", MoveToBeginning, Some(KEY_CONTEXT)),
            KeyBinding::new("cmd-right", MoveToEnd, Some(KEY_CONTEXT)),
            KeyBinding::new("cmd-down", MoveToEnd, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-a", MoveToBeginning, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-e", MoveToEnd, Some(KEY_CONTEXT)),
            KeyBinding::new("alt-left", MoveToPreviousWord, Some(KEY_CONTEXT)),
            KeyBinding::new("alt-right", MoveToNextWord, Some(KEY_CONTEXT)),
            KeyBinding::new("shift-left", SelectLeft, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-shift-b", SelectLeft, Some(KEY_CONTEXT)),
            KeyBinding::new("shift-right", SelectRight, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-shift-f", SelectRight, Some(KEY_CONTEXT)),
            KeyBinding::new("shift-home", SelectToBeginning, Some(KEY_CONTEXT)),
            KeyBinding::new("shift-end", SelectToEnd, Some(KEY_CONTEXT)),
            KeyBinding::new("cmd-shift-left", SelectToBeginning, Some(KEY_CONTEXT)),
            KeyBinding::new("cmd-shift-up", SelectToBeginning, Some(KEY_CONTEXT)),
            KeyBinding::new("cmd-shift-right", SelectToEnd, Some(KEY_CONTEXT)),
            KeyBinding::new("cmd-shift-down", SelectToEnd, Some(KEY_CONTEXT)),
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
        ]),
    }
}

/// A bounded visual treatment for a text input.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextInputVariant {
    /// Ordinary text-editor presentation.
    #[default]
    Standard,
    /// Presentation for a parent-owned continuous surface.
    Bare,
}

/// Text-input colors supplied by the application theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextInputPaint {
    text: Rgba,
    placeholder: Rgba,
    selection: Rgba,
    caret: Rgba,
    disabled_text: Rgba,
    disabled_placeholder: Rgba,
}

impl TextInputPaint {
    /// Creates paint for enabled text, enabled placeholder, selection, caret, disabled text, and
    /// disabled placeholder, in that order.
    pub fn new(
        text: Rgba,
        placeholder: Rgba,
        selection: Rgba,
        caret: Rgba,
        disabled_text: Rgba,
        disabled_placeholder: Rgba,
    ) -> Self {
        Self {
            text,
            placeholder,
            selection,
            caret,
            disabled_text,
            disabled_placeholder,
        }
    }
}

/// Complete paint catalog for the bounded variants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextInputVariants {
    standard: TextInputPaint,
    bare: TextInputPaint,
}

impl TextInputVariants {
    /// Creates the Standard and Bare paint catalog.
    pub fn new(standard: TextInputPaint, bare: TextInputPaint) -> Self {
        Self { standard, bare }
    }

    fn paint(self, variant: TextInputVariant) -> TextInputPaint {
        match variant {
            TextInputVariant::Standard => self.standard,
            TextInputVariant::Bare => self.bare,
        }
    }
}

/// Bounded geometry and deterministic autoscroll timing for text inputs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextInputMetrics {
    caret_width: Pixels,
    scroll_padding: Pixels,
    autoscroll_interval: Duration,
    autoscroll_max_step: Pixels,
}

impl TextInputMetrics {
    /// Creates metrics. Values are clamped to safe ranges: caret width 1 to 8 px, scroll padding
    /// 0 to 64 px, interval 8 to 100 ms, and maximum autoscroll step 1 to 64 px.
    pub fn new(
        caret_width: Pixels,
        scroll_padding: Pixels,
        autoscroll_interval: Duration,
        autoscroll_max_step: Pixels,
    ) -> Self {
        Self {
            caret_width: caret_width.clamp(px(1.0), px(8.0)),
            scroll_padding: scroll_padding.clamp(px(0.0), px(64.0)),
            autoscroll_interval: autoscroll_interval
                .clamp(Duration::from_millis(8), Duration::from_millis(100)),
            autoscroll_max_step: autoscroll_max_step.clamp(px(1.0), px(64.0)),
        }
    }
}

/// Application-global presentation for every [`TextInput`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextInputTheme {
    variants: TextInputVariants,
    metrics: TextInputMetrics,
}

impl TextInputTheme {
    /// Creates a complete text-input theme.
    pub fn new(variants: TextInputVariants, metrics: TextInputMetrics) -> Self {
        Self { variants, metrics }
    }
}

impl Global for TextInputTheme {}

/// How Tab and Shift-Tab behave while a [`TextInput`] owns focus.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextInputTabBehavior {
    /// Move through the Operating-System Window's normal tab order.
    #[default]
    MoveFocus,
    /// Emit a typed traversal request for a containing composite control.
    Propagate,
}

/// The source of one content change. It never contains editor content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextInputChangeSource {
    Keyboard,
    InputMethodComposition,
    Paste,
    Cut,
    Undo,
    Redo,
    Programmatic,
}

/// Content-safe metadata for a value change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextInputValueChanged {
    revision: u64,
    source: TextInputChangeSource,
}

impl TextInputValueChanged {
    /// Returns the revision after the change.
    pub fn revision(self) -> u64 {
        self.revision
    }
    /// Returns the operation that produced the change.
    pub fn source(self) -> TextInputChangeSource {
        self.source
    }
}

/// Public selection state. Offsets are UTF-8 byte offsets at grapheme boundaries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextInputSelection {
    range: Range<usize>,
    reversed: bool,
}

impl TextInputSelection {
    /// Returns the normalized selected byte range.
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }
    /// Returns whether the active end precedes the anchor.
    pub fn is_reversed(&self) -> bool {
        self.reversed
    }
    /// Returns the active insertion end.
    pub fn caret(&self) -> usize {
        if self.reversed {
            self.range.start
        } else {
            self.range.end
        }
    }
    /// Returns the fixed selection end.
    pub fn anchor(&self) -> usize {
        if self.reversed {
            self.range.end
        } else {
            self.range.start
        }
    }
    /// Returns whether no text is selected.
    pub fn is_empty(&self) -> bool {
        self.range.is_empty()
    }
}

/// Public input-method composition state. The marked range exists even when it is empty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextInputComposition {
    marked_range: Range<usize>,
    selection: TextInputSelection,
}

impl TextInputComposition {
    /// Returns the marked UTF-8 byte range.
    pub fn marked_range(&self) -> Range<usize> {
        self.marked_range.clone()
    }
    /// Returns the selection within the current value.
    pub fn selection(&self) -> &TextInputSelection {
        &self.selection
    }
}

/// Content-safe editor events.
///
/// For a composition update, `CompositionStarted` precedes its first `ValueChanged`. A commit emits
/// the final `ValueChanged`, if any, before `CompositionCommitted`. Cancellation restoration emits
/// its `ValueChanged` before `CompositionCancelled`. Movement, pointer editing, ordinary editing,
/// undo, redo, blur, and submit first commit an active composition. Escape cancels a composition
/// and consumes that Escape; only a later Escape emits `Cancelled`. Focus events reflect the native
/// focus callback immediately, including focus transferred to any context menu. With
/// [`TextInputTabBehavior::Propagate`], Tab or Shift-Tab emits its traversal request synchronously
/// and is consumed; the containing composite is responsible for moving focus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextInputEvent {
    ValueChanged(TextInputValueChanged),
    Submitted,
    Cancelled,
    FocusGained,
    FocusLost,
    CompositionStarted,
    CompositionCommitted,
    CompositionCancelled,
    ContextMenuOpened,
    ContextMenuClosed,
    /// Tab requested forward traversal delegated to a containing composite.
    TabForwardRequested,
    /// Shift-Tab requested backward traversal delegated to a containing composite.
    TabBackwardRequested,
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
    fn public(&self) -> TextInputSelection {
        TextInputSelection {
            range: self.range.clone(),
            reversed: self.reversed,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Snapshot {
    text: String,
    selection: Selection,
}
impl Snapshot {
    fn bytes(&self) -> usize {
        self.text.len()
    }
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
    retained_bytes: usize,
}

impl History {
    fn should_snapshot(&self, kind: EditKind) -> bool {
        kind == EditKind::Atomic || self.group != Some(kind)
    }

    fn record_snapshot(&mut self, snapshot: Option<Snapshot>, kind: EditKind) {
        self.clear_redo();
        if let Some(snapshot) = snapshot {
            self.retained_bytes += snapshot.bytes();
            self.undo.push(snapshot);
        }
        self.group = Some(kind);
        self.trim();
    }

    fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.group = None;
        self.retained_bytes = 0;
    }
    fn clear_redo(&mut self) {
        self.retained_bytes -= self.redo.iter().map(Snapshot::bytes).sum::<usize>();
        self.redo.clear();
    }
    fn break_group(&mut self) {
        self.group = None;
    }

    fn push_undo(&mut self, snapshot: Snapshot) {
        self.retained_bytes += snapshot.bytes();
        self.undo.push(snapshot);
        self.trim();
    }
    fn push_redo(&mut self, snapshot: Snapshot) {
        self.retained_bytes += snapshot.bytes();
        self.redo.push(snapshot);
        self.trim();
    }

    fn pop_undo(&mut self) -> Option<Snapshot> {
        let value = self.undo.pop()?;
        self.retained_bytes -= value.bytes();
        Some(value)
    }
    fn pop_redo(&mut self) -> Option<Snapshot> {
        let value = self.redo.pop()?;
        self.retained_bytes -= value.bytes();
        Some(value)
    }

    fn trim(&mut self) {
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
struct TextBuffer {
    text: String,
    selection: Selection,
    history: History,
}

impl TextBuffer {
    fn new(text: String) -> Self {
        let end = text.len();
        Self {
            text,
            selection: Selection::caret(end),
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
        self.history.break_group();
    }
    fn move_to(&mut self, offset: usize) {
        self.selection = Selection::caret(clamp_grapheme_boundary(&self.text, offset, false));
        self.history.break_group();
    }
    fn select_from_anchor(&mut self, anchor: usize, offset: usize) {
        let anchor = clamp_grapheme_boundary(&self.text, anchor, false);
        let head = clamp_grapheme_boundary(&self.text, offset, false);
        self.selection = Selection {
            range: anchor.min(head)..anchor.max(head),
            reversed: head < anchor,
        };
        self.history.break_group();
    }
    fn select_to(&mut self, offset: usize) {
        let anchor = self.selection.anchor();
        self.select_from_anchor(anchor, offset);
    }
    fn select_all(&mut self) {
        self.selection = Selection {
            range: 0..self.text.len(),
            reversed: false,
        };
        self.history.break_group();
    }

    fn replace(
        &mut self,
        range: Range<usize>,
        replacement: String,
        kind: EditKind,
        limit: usize,
    ) -> bool {
        let range = normalize_byte_range(&self.text, range);
        let final_len = self.text.len() - range.len() + replacement.len();
        if final_len > limit || final_len > HARD_VALUE_LIMIT {
            return false;
        }
        if self.text[range.clone()] == replacement {
            self.selection = Selection::caret(range.start + replacement.len());
            return false;
        }
        let snapshot = self.history.should_snapshot(kind).then(|| self.snapshot());
        let cursor = range.start + replacement.len();
        self.text.replace_range(range, &replacement);
        self.selection = Selection::caret(cursor);
        self.history.record_snapshot(snapshot, kind);
        true
    }

    fn can_replace(&self, range: &Range<usize>, replacement_len: usize, limit: usize) -> bool {
        let range = normalize_byte_range(&self.text, range.clone());
        let final_len = self.text.len() - range.len() + replacement_len;
        final_len <= limit && final_len <= HARD_VALUE_LIMIT
    }

    fn replace_without_history(
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
        self.text.replace_range(range, replacement);
        self.selection = Selection::caret(cursor);
        self.history.break_group();
        true
    }

    fn move_left(&mut self, extend: bool) {
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
    fn move_right(&mut self, extend: bool) {
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
    fn move_edge(&mut self, end: bool, extend: bool) {
        let offset = if end { self.text.len() } else { 0 };
        if extend {
            self.select_to(offset)
        } else {
            self.move_to(offset)
        }
    }
    fn move_word(&mut self, next: bool, extend: bool) {
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

    fn delete_backward(&mut self, limit: usize) -> bool {
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
    fn delete_forward(&mut self, limit: usize) -> bool {
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
    fn selected_text(&self) -> Option<&str> {
        (!self.selection.is_empty()).then(|| &self.text[self.selection.range.clone()])
    }

    fn transpose(&mut self, limit: usize) -> bool {
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

    fn undo(&mut self) -> bool {
        let Some(snapshot) = self.history.pop_undo() else {
            return false;
        };
        let current = self.snapshot();
        self.restore(snapshot);
        self.history.push_redo(current);
        true
    }
    fn redo(&mut self) -> bool {
        let Some(snapshot) = self.history.pop_redo() else {
            return false;
        };
        let current = self.snapshot();
        self.restore(snapshot);
        self.history.push_undo(current);
        true
    }
}

#[derive(Debug)]
struct CompositionState {
    original: Snapshot,
    marked_range: Range<usize>,
}

#[derive(Clone, Copy, Debug)]
struct PointerGesture {
    generation: u64,
    anchor: usize,
    latest_position: Point<Pixels>,
}

#[derive(Clone, Debug, PartialEq)]
struct ShapeKey {
    revision: u64,
    marked_range: Option<Range<usize>>,
    placeholder: SharedString,
    empty: bool,
    enabled: bool,
    variant: TextInputVariant,
    font: Font,
    font_size: Pixels,
    bounds: Bounds<Pixels>,
    scale: f32,
    paint: TextInputPaint,
}

#[derive(Clone, Debug)]
struct GeometryCache {
    key: ShapeKey,
    line: ShapedLine,
}

#[derive(Clone, Copy)]
enum TextInputMenuAction {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

/// A reusable, bounded single-line text editor.
pub struct TextInput {
    id: ElementId,
    accessibility_name: SharedString,
    debug_selector: SharedString,
    placeholder: SharedString,
    variant: TextInputVariant,
    enabled: bool,
    editable: bool,
    tab_stop: bool,
    tab_behavior: TextInputTabBehavior,
    input_length_limit: usize,
    emit_programmatic_changes: bool,
    focus_handle: FocusHandle,
    focused: bool,
    window_active: bool,
    buffer: TextBuffer,
    initial_value_source: Option<String>,
    revision: u64,
    composition: Option<CompositionState>,
    geometry: Option<GeometryCache>,
    last_bounds: Option<Bounds<Pixels>>,
    scroll: Pixels,
    pointer_generation: u64,
    pointer_gesture: Option<PointerGesture>,
    autoscroll_generation: Option<u64>,
    autoscroll_task: Option<Task<()>>,
    caret_generation: u64,
    caret_visible: bool,
    caret_task: Option<Task<()>>,
    context_menu_open: bool,
    #[cfg(test)]
    shape_count: usize,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl TextInput {
    /// Creates an editor. The initial value is normalized and grapheme-safely truncated to the
    /// safe default 64 KiB limit. It begins at revision zero with a collapsed selection at the end.
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
        let normalized = normalize_single_line(&initial_value.into());
        let normalized = truncate_grapheme(&normalized, HARD_VALUE_LIMIT).to_owned();
        let default_value = truncate_grapheme(&normalized, DEFAULT_VALUE_LIMIT).to_owned();
        let initial_value_source = (default_value != normalized).then_some(normalized);
        let accessibility_name = accessibility_name.into();
        Self {
            id: id.into(),
            debug_selector: accessibility_name.clone(),
            accessibility_name,
            placeholder: SharedString::default(),
            variant: TextInputVariant::default(),
            enabled: true,
            editable: true,
            tab_stop: true,
            tab_behavior: TextInputTabBehavior::default(),
            input_length_limit: DEFAULT_VALUE_LIMIT,
            emit_programmatic_changes: false,
            focus_handle,
            focused: false,
            window_active: window.is_window_active(),
            buffer: TextBuffer::new(default_value),
            initial_value_source,
            revision: 0,
            composition: None,
            geometry: None,
            last_bounds: None,
            scroll: px(0.0),
            pointer_generation: 0,
            pointer_gesture: None,
            autoscroll_generation: None,
            autoscroll_task: None,
            caret_generation: 0,
            caret_visible: true,
            caret_task: None,
            context_menu_open: false,
            #[cfg(test)]
            shape_count: 0,
            _subscriptions: subscriptions,
        }
    }

    /// Sets the placeholder shown when the value is empty.
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self.geometry = None;
        self
    }
    /// Selects one bounded treatment from the installed theme.
    pub fn variant(mut self, variant: TextInputVariant) -> Self {
        self.variant = variant;
        self.geometry = None;
        self
    }
    /// Sets the initial enabled state.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self.focus_handle = self.focus_handle.clone().tab_stop(enabled && self.tab_stop);
        if !enabled {
            self.cancel_pointer_gesture();
            self.caret_task = None;
        }
        self
    }
    /// Sets the initial editable state. Read-only inputs remain selectable and copyable.
    pub fn editable(mut self, editable: bool) -> Self {
        self.editable = editable;
        if !editable {
            self.cancel_pointer_gesture();
        }
        self
    }
    /// Controls whether normal keyboard traversal may stop on the enabled input.
    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.tab_stop = tab_stop;
        self.focus_handle = self.focus_handle.clone().tab_stop(self.enabled && tab_stop);
        self
    }
    /// Selects whether the input or its containing composite handles Tab traversal.
    pub fn tab_behavior(mut self, behavior: TextInputTabBehavior) -> Self {
        self.tab_behavior = behavior;
        self
    }
    /// Sets the optional product limit. `Some` is clamped to 1 MiB; `None` removes the safe default
    /// while retaining the hard 1 MiB limit. An initial value above the resulting limit is
    /// grapheme-safely truncated before the entity can emit events.
    pub fn input_length_limit(mut self, limit: Option<usize>) -> Self {
        self.input_length_limit = limit.unwrap_or(HARD_VALUE_LIMIT).min(HARD_VALUE_LIMIT);
        let source = self
            .initial_value_source
            .as_deref()
            .unwrap_or(&self.buffer.text);
        let value = truncate_grapheme(source, self.input_length_limit).to_owned();
        if value != self.buffer.text {
            self.buffer = TextBuffer::new(value);
        }
        self
    }
    /// Configures whether [`Self::set_value`] emits a programmatic value-change event.
    pub fn emit_programmatic_changes(mut self, emit: bool) -> Self {
        self.emit_programmatic_changes = emit;
        self
    }
    /// Overrides the stable debug selector, which otherwise uses the logical name.
    pub fn debug_selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.debug_selector = selector.into();
        self
    }

    /// Returns the current normalized value.
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
    /// Returns the directional grapheme-normalized selection.
    pub fn selection(&self) -> TextInputSelection {
        self.buffer.selection.public()
    }
    /// Returns the active input-method composition, including an empty marked range.
    pub fn composition(&self) -> Option<TextInputComposition> {
        self.composition.as_ref().map(|state| TextInputComposition {
            marked_range: state.marked_range.clone(),
            selection: self.buffer.selection.public(),
        })
    }
    /// Returns the focus handle used by a containing composite for explicit focus transfer.
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// Changes the enabled state and cancels any owned pointer gesture.
    pub fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.enabled == enabled {
            return;
        }
        if !enabled {
            self.commit_composition(cx);
            self.cancel_pointer_gesture();
            self.caret_task = None;
            self.caret_visible = true;
        }
        self.enabled = enabled;
        self.focus_handle = self.focus_handle.clone().tab_stop(enabled && self.tab_stop);
        self.geometry = None;
        self.restart_caret(cx);
    }

    /// Changes whether user editing is allowed while preserving selection and copy behavior.
    pub fn set_editable(&mut self, editable: bool, cx: &mut Context<Self>) {
        if self.editable == editable {
            return;
        }
        if !editable {
            self.commit_composition(cx);
            self.cancel_pointer_gesture();
        }
        self.editable = editable;
        cx.notify();
    }

    /// Replaces the complete value after single-line normalization.
    ///
    /// The operation atomically rejects a normalized value over the configured or hard limit. An
    /// active composition is superseded and emits `CompositionCancelled` before any programmatic
    /// change event. Selection collapses at the end and undo and redo are cleared even when the
    /// normalized text is unchanged. Revision advances only when text changes. A `ValueChanged`
    /// event with `Programmatic` is emitted only when configured and only after that revision
    /// advances. The return value reports whether text changed.
    pub fn set_value(&mut self, value: impl Into<String>, cx: &mut Context<Self>) -> bool {
        let normalized = normalize_single_line(&value.into());
        if normalized.len() > self.input_length_limit || normalized.len() > HARD_VALUE_LIMIT {
            return false;
        }
        if self.composition.take().is_some() {
            cx.emit(TextInputEvent::CompositionCancelled);
        }
        self.buffer.history.clear();
        let changed = self.buffer.text != normalized;
        self.buffer.text = normalized;
        self.buffer.selection = Selection::caret(self.buffer.text.len());
        self.cancel_pointer_gesture();
        if changed {
            self.advance_revision(
                TextInputChangeSource::Programmatic,
                self.emit_programmatic_changes,
                cx,
            );
        } else {
            self.geometry = None;
            self.restart_caret(cx);
        }
        changed
    }

    /// Selects the complete value after committing active composition.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.commit_composition(cx);
        self.buffer.select_all();
        self.restart_caret(cx);
    }

    fn can_edit(&self) -> bool {
        self.enabled && self.editable
    }

    fn can_accept_paste(&self, text: &str) -> bool {
        if !self.can_edit() || text.len() > CLIPBOARD_INSERTION_LIMIT {
            return false;
        }
        let replacement_len = normalized_single_line_len(text);
        (!self.buffer.selection.is_empty() || replacement_len > 0)
            && self.buffer.can_replace(
                &self.buffer.selection.range,
                replacement_len,
                self.input_length_limit,
            )
    }
    fn advance_revision(
        &mut self,
        source: TextInputChangeSource,
        emit: bool,
        cx: &mut Context<Self>,
    ) {
        self.revision = self.revision.saturating_add(1);
        self.geometry = None;
        if emit {
            cx.emit(TextInputEvent::ValueChanged(TextInputValueChanged {
                revision: self.revision,
                source,
            }));
        }
        self.restart_caret(cx);
    }
    fn finish_edit(
        &mut self,
        changed: bool,
        source: TextInputChangeSource,
        cx: &mut Context<Self>,
    ) {
        if changed {
            self.advance_revision(source, true, cx);
        } else {
            self.restart_caret(cx);
        }
    }

    fn on_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.enabled {
            window.focus_next();
            return;
        }
        self.focused = true;
        self.window_active = window.is_window_active();
        self.caret_visible = true;
        cx.emit(TextInputEvent::FocusGained);
        self.restart_caret(cx);
    }
    fn on_blur(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if !self.focused {
            return;
        }
        self.commit_composition(cx);
        self.focused = false;
        self.cancel_pointer_gesture();
        self.caret_task = None;
        self.caret_visible = true;
        self.buffer.history.break_group();
        cx.emit(TextInputEvent::FocusLost);
        cx.notify();
    }
    fn on_window_activation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window_active = window.is_window_active();
        if self.window_active {
            self.caret_visible = true;
            self.restart_caret(cx);
        } else {
            self.cancel_pointer_gesture();
            self.caret_task = None;
            self.caret_visible = true;
            cx.notify();
        }
    }

    fn restart_caret(&mut self, cx: &mut Context<Self>) {
        self.caret_generation = self.caret_generation.wrapping_add(1);
        self.caret_visible = true;
        self.caret_task = None;
        if !(self.enabled && self.focused && self.window_active) {
            cx.notify();
            return;
        }
        let generation = self.caret_generation;
        self.caret_task = Some(cx.spawn(async move |input, cx| {
            loop {
                cx.background_executor().timer(CARET_BLINK_INTERVAL).await;
                let keep = input
                    .update(cx, |input, cx| {
                        if input.caret_generation != generation
                            || !input.enabled
                            || !input.focused
                            || !input.window_active
                        {
                            return false;
                        }
                        input.caret_visible = !input.caret_visible;
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

    fn commit_composition(&mut self, cx: &mut Context<Self>) -> bool {
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
        cx.emit(TextInputEvent::CompositionCommitted);
        self.geometry = None;
        true
    }
    fn cancel_composition(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(state) = self.composition.take() else {
            return false;
        };
        let changed = self.buffer.text != state.original.text;
        self.buffer.restore(state.original);
        if changed {
            self.advance_revision(TextInputChangeSource::InputMethodComposition, true, cx);
        }
        cx.emit(TextInputEvent::CompositionCancelled);
        self.geometry = None;
        self.restart_caret(cx);
        true
    }

    fn replace_selection_normalized(
        &mut self,
        text: &str,
        kind: EditKind,
        source: TextInputChangeSource,
        cx: &mut Context<Self>,
    ) {
        if !self.can_edit() {
            return;
        }
        self.commit_composition(cx);
        let replacement = normalize_single_line(text);
        let changed = self.buffer.replace(
            self.buffer.selection.range.clone(),
            replacement,
            kind,
            self.input_length_limit,
        );
        self.finish_edit(changed, source, cx);
    }

    fn delete_range(
        &mut self,
        range: Range<usize>,
        source: TextInputChangeSource,
        cx: &mut Context<Self>,
    ) {
        if !self.can_edit() {
            return;
        }
        self.commit_composition(cx);
        let changed = self.buffer.replace(
            range,
            String::new(),
            EditKind::Atomic,
            self.input_length_limit,
        );
        self.finish_edit(changed, source, cx);
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.can_edit() {
            self.commit_composition(cx);
            let changed = self.buffer.delete_backward(self.input_length_limit);
            self.finish_edit(changed, TextInputChangeSource::Keyboard, cx);
        }
    }
    fn delete_forward(&mut self, _: &DeleteForward, _: &mut Window, cx: &mut Context<Self>) {
        if self.can_edit() {
            self.commit_composition(cx);
            let changed = self.buffer.delete_forward(self.input_length_limit);
            self.finish_edit(changed, TextInputChangeSource::Keyboard, cx);
        }
    }
    fn delete_to_beginning(
        &mut self,
        _: &DeleteToBeginning,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = if self.buffer.selection.is_empty() {
            0..self.buffer.selection.cursor()
        } else {
            self.buffer.selection.range.clone()
        };
        self.delete_range(range, TextInputChangeSource::Keyboard, cx);
    }
    fn delete_to_end(&mut self, _: &DeleteToEnd, _: &mut Window, cx: &mut Context<Self>) {
        let range = if self.buffer.selection.is_empty() {
            self.buffer.selection.cursor()..self.buffer.text.len()
        } else {
            self.buffer.selection.range.clone()
        };
        self.delete_range(range, TextInputChangeSource::Keyboard, cx);
    }
    fn delete_previous_word(
        &mut self,
        _: &DeletePreviousWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cursor = self.buffer.selection.cursor();
        let range = if self.buffer.selection.is_empty() {
            previous_word_start(&self.buffer.text, cursor)..cursor
        } else {
            self.buffer.selection.range.clone()
        };
        self.delete_range(range, TextInputChangeSource::Keyboard, cx);
    }
    fn delete_next_word(&mut self, _: &DeleteNextWord, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.buffer.selection.cursor();
        let range = if self.buffer.selection.is_empty() {
            cursor..next_word_end(&self.buffer.text, cursor)
        } else {
            self.buffer.selection.range.clone()
        };
        self.delete_range(range, TextInputChangeSource::Keyboard, cx);
    }

    fn kill(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        if !self.can_edit() {
            return;
        }
        self.commit_composition(cx);
        let range = normalize_byte_range(&self.buffer.text, range);
        if range.is_empty() {
            self.restart_caret(cx);
            return;
        }
        let killed =
            truncate_grapheme(&self.buffer.text[range.clone()], KILL_RING_LIMIT).to_owned();
        cx.global_mut::<TextKillRing>().0 = killed;
        let changed = self.buffer.replace(
            range,
            String::new(),
            EditKind::Atomic,
            self.input_length_limit,
        );
        self.finish_edit(changed, TextInputChangeSource::Keyboard, cx);
    }
    fn kill_to_beginning(&mut self, _: &KillToBeginning, _: &mut Window, cx: &mut Context<Self>) {
        let range = if self.buffer.selection.is_empty() {
            0..self.buffer.selection.cursor()
        } else {
            self.buffer.selection.range.clone()
        };
        self.kill(range, cx);
    }
    fn kill_to_end(&mut self, _: &KillToEnd, _: &mut Window, cx: &mut Context<Self>) {
        let range = if self.buffer.selection.is_empty() {
            self.buffer.selection.cursor()..self.buffer.text.len()
        } else {
            self.buffer.selection.range.clone()
        };
        self.kill(range, cx);
    }
    fn kill_previous_word(&mut self, _: &KillPreviousWord, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.buffer.selection.cursor();
        let range = if self.buffer.selection.is_empty() {
            previous_word_start(&self.buffer.text, cursor)..cursor
        } else {
            self.buffer.selection.range.clone()
        };
        self.kill(range, cx);
    }
    fn yank(&mut self, _: &Yank, _: &mut Window, cx: &mut Context<Self>) {
        if !self.can_edit() {
            return;
        }
        let killed = cx.global::<TextKillRing>().0.clone();
        if !killed.is_empty() {
            self.replace_selection_normalized(
                &killed,
                EditKind::Atomic,
                TextInputChangeSource::Keyboard,
                cx,
            );
        }
    }
    fn transpose(&mut self, _: &Transpose, _: &mut Window, cx: &mut Context<Self>) {
        if self.can_edit() {
            self.commit_composition(cx);
            let changed = self.buffer.transpose(self.input_length_limit);
            self.finish_edit(changed, TextInputChangeSource::Keyboard, cx);
        }
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
    fn move_horizontal(&mut self, right: bool, extend: bool, cx: &mut Context<Self>) {
        if !self.enabled {
            return;
        }
        self.commit_composition(cx);
        if right {
            self.buffer.move_right(extend)
        } else {
            self.buffer.move_left(extend)
        };
        self.restart_caret(cx);
    }
    fn move_to_beginning(&mut self, _: &MoveToBeginning, _: &mut Window, cx: &mut Context<Self>) {
        self.move_edge(false, false, cx);
    }
    fn move_to_end(&mut self, _: &MoveToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_edge(true, false, cx);
    }
    fn select_to_beginning(
        &mut self,
        _: &SelectToBeginning,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_edge(false, true, cx);
    }
    fn select_to_end(&mut self, _: &SelectToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_edge(true, true, cx);
    }
    fn move_edge(&mut self, end: bool, extend: bool, cx: &mut Context<Self>) {
        if !self.enabled {
            return;
        }
        self.commit_composition(cx);
        self.buffer.move_edge(end, extend);
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
    fn move_word(&mut self, next: bool, extend: bool, cx: &mut Context<Self>) {
        if !self.enabled {
            return;
        }
        self.commit_composition(cx);
        self.buffer.move_word(next, extend);
        self.restart_caret(cx);
    }
    fn on_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        if self.enabled {
            self.select_all(cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if self.enabled
            && let Some(text) = self.buffer.selected_text()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        }
        cx.stop_propagation();
    }
    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if self.can_edit() {
            self.commit_composition(cx);
            if let Some(text) = self.buffer.selected_text().map(ToOwned::to_owned) {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                let changed = self.buffer.replace(
                    self.buffer.selection.range.clone(),
                    String::new(),
                    EditKind::Atomic,
                    self.input_length_limit,
                );
                self.finish_edit(changed, TextInputChangeSource::Cut, cx);
            }
        }
        cx.stop_propagation();
    }
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
            && self.can_accept_paste(&text)
        {
            self.replace_selection_normalized(
                &text,
                EditKind::Atomic,
                TextInputChangeSource::Paste,
                cx,
            );
        }
        cx.stop_propagation();
    }
    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.can_edit() {
            self.commit_composition(cx);
            if self.buffer.undo() {
                self.advance_revision(TextInputChangeSource::Undo, true, cx);
            } else {
                self.restart_caret(cx);
            }
        }
    }
    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.can_edit() {
            self.commit_composition(cx);
            if self.buffer.redo() {
                self.advance_revision(TextInputChangeSource::Redo, true, cx);
            } else {
                self.restart_caret(cx);
            }
        }
    }
    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        if self.enabled {
            self.commit_composition(cx);
            self.buffer.history.break_group();
            cx.emit(TextInputEvent::Submitted);
            cx.stop_propagation();
        }
    }
    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if self.enabled {
            if !self.cancel_composition(cx) {
                self.buffer.history.break_group();
                cx.emit(TextInputEvent::Cancelled);
            }
            cx.stop_propagation();
        }
    }
    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        match self.tab_behavior {
            TextInputTabBehavior::MoveFocus => window.focus_next(),
            TextInputTabBehavior::Propagate => cx.emit(TextInputEvent::TabForwardRequested),
        }
        cx.stop_propagation();
    }
    fn focus_previous(&mut self, _: &FocusPrevious, window: &mut Window, cx: &mut Context<Self>) {
        match self.tab_behavior {
            TextInputTabBehavior::MoveFocus => window.focus_prev(),
            TextInputTabBehavior::Propagate => cx.emit(TextInputEvent::TabBackwardRequested),
        }
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
        if !self.enabled || event.button != MouseButton::Left {
            return;
        }
        self.focus_handle.focus(window);
        let offset = self.index_for_mouse_position(event.position);
        self.commit_composition(cx);
        match event.click_count {
            1 if event.modifiers.shift => self.buffer.select_to(offset),
            1 => self.buffer.move_to(offset),
            2 => {
                self.buffer.selection = Selection {
                    range: word_range_at(&self.buffer.text, offset),
                    reversed: false,
                }
            }
            _ => self.buffer.select_all(),
        }
        self.pointer_generation = self.pointer_generation.wrapping_add(1);
        self.pointer_gesture = Some(PointerGesture {
            generation: self.pointer_generation,
            anchor: self.buffer.selection.anchor(),
            latest_position: event.position,
        });
        self.restart_caret(cx);
        cx.stop_propagation();
    }

    fn on_global_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(mut gesture) = self.pointer_gesture else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.cancel_pointer_gesture();
            return;
        }
        gesture.latest_position = event.position;
        self.pointer_gesture = Some(gesture);
        if self.pointer_is_outside_horizontally(event.position) {
            self.start_autoscroll(gesture.generation, cx);
        } else {
            self.autoscroll_generation = None;
            self.autoscroll_task = None;
            let offset = self.index_for_mouse_position(event.position);
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
        self.autoscroll_generation = None;
        self.autoscroll_task = None;
    }
    fn pointer_is_outside_horizontally(&self, point: Point<Pixels>) -> bool {
        self.last_bounds
            .is_some_and(|bounds| point.x < bounds.left() || point.x > bounds.right())
    }

    fn start_autoscroll(&mut self, generation: u64, cx: &mut Context<Self>) {
        if self.autoscroll_task.is_some() {
            return;
        }
        let metrics = cx.global::<TextInputTheme>().metrics;
        self.autoscroll_generation = Some(generation);
        self.autoscroll_task = Some(cx.spawn(async move |input, cx| {
            loop {
                cx.background_executor()
                    .timer(metrics.autoscroll_interval)
                    .await;
                let keep = input
                    .update(cx, |input, cx| {
                        let Some(gesture) = input.pointer_gesture else {
                            return false;
                        };
                        if gesture.generation != generation || !input.enabled {
                            return false;
                        }
                        let Some(bounds) = input.last_bounds else {
                            return false;
                        };
                        let overflow = if gesture.latest_position.x < bounds.left() {
                            gesture.latest_position.x - bounds.left()
                        } else if gesture.latest_position.x > bounds.right() {
                            gesture.latest_position.x - bounds.right()
                        } else {
                            px(0.0)
                        };
                        if overflow == px(0.0) {
                            return false;
                        }
                        let step = overflow
                            .clamp(-metrics.autoscroll_max_step, metrics.autoscroll_max_step);
                        let max_scroll = input.geometry.as_ref().map_or(px(0.0), |geometry| {
                            (geometry.line.width - bounds.size.width + metrics.scroll_padding)
                                .max(px(0.0))
                        });
                        input.scroll = (input.scroll + step).clamp(px(0.0), max_scroll);
                        let edge_position = point(
                            if overflow < px(0.0) {
                                bounds.left()
                            } else {
                                bounds.right()
                            },
                            gesture.latest_position.y,
                        );
                        let offset = input.index_for_mouse_position(edge_position);
                        input.buffer.select_from_anchor(gesture.anchor, offset);
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
            let _ = input.update(cx, |input, _| {
                if input.autoscroll_generation == Some(generation) {
                    input.autoscroll_generation = None;
                    input.autoscroll_task = None;
                }
            });
        }));
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.buffer.text.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(geometry)) = (self.last_bounds, self.geometry.as_ref()) else {
            return self.buffer.selection.cursor();
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.buffer.text.len();
        }
        clamp_grapheme_boundary(
            &self.buffer.text,
            geometry
                .line
                .closest_index_for_x(position.x - bounds.left() + self.scroll),
            false,
        )
    }

    fn rebuild_geometry(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> ShapedLine {
        let theme = *cx.global::<TextInputTheme>();
        let paint = theme.variants.paint(self.variant);
        let empty = self.buffer.text.is_empty();
        let display: SharedString = if empty {
            self.placeholder.clone()
        } else {
            self.buffer.text.clone().into()
        };
        let color: gpui::Hsla = if self.enabled {
            if empty { paint.placeholder } else { paint.text }
        } else if empty {
            paint.disabled_placeholder
        } else {
            paint.disabled_text
        }
        .into();
        let text_style = window.text_style();
        let font = text_style.font();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let marked_range = (!empty)
            .then(|| {
                self.composition
                    .as_ref()
                    .map(|state| state.marked_range.clone())
            })
            .flatten();
        let key = ShapeKey {
            revision: self.revision,
            marked_range: marked_range.clone(),
            placeholder: self.placeholder.clone(),
            empty,
            enabled: self.enabled,
            variant: self.variant,
            font: font.clone(),
            font_size,
            bounds,
            scale: window.scale_factor(),
            paint,
        };
        if let Some(cache) = &self.geometry
            && cache.key == key
        {
            return cache.line.clone();
        }
        let base = TextRun {
            len: display.len(),
            font,
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = marked_text_runs(&display, marked_range, base);
        #[cfg(test)]
        {
            self.shape_count += 1;
        }
        let line = window
            .text_system()
            .shape_line(display, font_size, &runs, None);
        self.geometry = Some(GeometryCache {
            key,
            line: line.clone(),
        });
        line
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
        if self.commit_composition(cx) {
            self.restart_caret(cx);
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_edit() {
            return;
        }
        let normalized = normalize_single_line(text);
        let had_composition = self.composition.is_some();
        let range = range_utf16
            .map(|range| utf16_replacement_range_to_bytes(&self.buffer.text, range))
            .or_else(|| {
                self.composition
                    .as_ref()
                    .map(|state| state.marked_range.clone())
            })
            .unwrap_or_else(|| self.buffer.selection.range.clone());
        if !self
            .buffer
            .can_replace(&range, normalized.len(), self.input_length_limit)
        {
            return;
        }
        let changed = if had_composition {
            self.buffer
                .replace_without_history(range, &normalized, self.input_length_limit)
        } else {
            self.buffer
                .replace(range, normalized, EditKind::Insert, self.input_length_limit)
        };
        if changed {
            self.advance_revision(
                if had_composition {
                    TextInputChangeSource::InputMethodComposition
                } else {
                    TextInputChangeSource::Keyboard
                },
                true,
                cx,
            );
        }
        if had_composition {
            self.commit_composition(cx);
        }
        self.rebuild_geometry(self.last_bounds.unwrap_or_default(), window, cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_edit() {
            return;
        }
        let replacement = normalize_single_line(text);
        let range = range_utf16
            .map(|range| utf16_replacement_range_to_bytes(&self.buffer.text, range))
            .or_else(|| {
                self.composition
                    .as_ref()
                    .map(|state| state.marked_range.clone())
            })
            .unwrap_or_else(|| self.buffer.selection.range.clone());
        if !self
            .buffer
            .can_replace(&range, replacement.len(), self.input_length_limit)
        {
            return;
        }
        if self.composition.is_none() {
            self.composition = Some(CompositionState {
                original: self.buffer.snapshot(),
                marked_range: range.clone(),
            });
            cx.emit(TextInputEvent::CompositionStarted);
        }
        let start = range.start;
        let changed =
            self.buffer
                .replace_without_history(range, &replacement, self.input_length_limit);
        if let Some(composition) = &mut self.composition {
            composition.marked_range = start..start + replacement.len();
        }
        self.buffer.selection = selected_utf16.map_or_else(
            || Selection::caret(start + replacement.len()),
            |selected| {
                let relative = utf16_selection_range_to_bytes(&replacement, selected);
                Selection {
                    range: start + relative.start..start + relative.end,
                    reversed: false,
                }
            },
        );
        if changed {
            self.advance_revision(TextInputChangeSource::InputMethodComposition, true, cx);
        }
        self.rebuild_geometry(self.last_bounds.unwrap_or_default(), window, cx);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.rebuild_geometry(bounds, window, cx);
        let range = utf16_query_range_to_bytes(&self.buffer.text, range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + line.x_for_index(range.start) - self.scroll,
                bounds.top(),
            ),
            point(
                bounds.left() + line.x_for_index(range.end) - self.scroll,
                bounds.bottom(),
            ),
        ))
    }
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line = self.rebuild_geometry(bounds, window, cx);
        let index = clamp_grapheme_boundary(
            &self.buffer.text,
            line.closest_index_for_x(point.x - bounds.left() + self.scroll),
            false,
        );
        Some(byte_offset_to_utf16(&self.buffer.text, index))
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.initial_value_source = None;
        let entity = cx.entity();
        let has_selection = !self.buffer.selection.is_empty();
        let can_edit = self.can_edit();
        let can_paste = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .is_some_and(|text| self.can_accept_paste(&text));
        let entries = vec![
            MenuEntry::action("Undo", TextInputMenuAction::Undo)
                .disabled(!can_edit || self.buffer.history.undo.is_empty()),
            MenuEntry::action("Redo", TextInputMenuAction::Redo)
                .disabled(!can_edit || self.buffer.history.redo.is_empty()),
            MenuEntry::separator(),
            MenuEntry::action("Cut", TextInputMenuAction::Cut)
                .disabled(!can_edit || !has_selection),
            MenuEntry::action("Copy", TextInputMenuAction::Copy)
                .disabled(!self.enabled || !has_selection),
            MenuEntry::action("Paste", TextInputMenuAction::Paste).disabled(!can_paste),
            MenuEntry::action("Select All", TextInputMenuAction::SelectAll)
                .disabled(!self.enabled || self.buffer.text.is_empty()),
        ];
        let selector = self.debug_selector.clone();
        let editor = div()
            .id(self.id.clone())
            .debug_selector(move || selector.to_string())
            .size_full()
            .min_w_0()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .cursor(if self.enabled {
                CursorStyle::IBeam
            } else {
                CursorStyle::Arrow
            })
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete_forward))
            .on_action(cx.listener(Self::delete_to_beginning))
            .on_action(cx.listener(Self::delete_to_end))
            .on_action(cx.listener(Self::delete_previous_word))
            .on_action(cx.listener(Self::delete_next_word))
            .on_action(cx.listener(Self::kill_to_beginning))
            .on_action(cx.listener(Self::kill_to_end))
            .on_action(cx.listener(Self::kill_previous_word))
            .on_action(cx.listener(Self::yank))
            .on_action(cx.listener(Self::transpose))
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
            .child(TextElement {
                input: entity.clone(),
            });
        let name = self.accessibility_name.clone();
        let menu_open = entity.downgrade();
        let menu_lifecycle = entity.downgrade();
        let menu_activate = entity.downgrade();
        ContextMenu::new(
            ("text-input-context-menu", entity.entity_id()),
            name,
            editor,
            entries,
        )
        .disabled(!self.enabled)
        .on_open_request(move |_, _, cx| {
            menu_open
                .update(cx, |input, cx| {
                    if !input.enabled {
                        return false;
                    }
                    input.context_menu_open = true;
                    input.cancel_pointer_gesture();
                    cx.emit(TextInputEvent::ContextMenuOpened);
                    true
                })
                .unwrap_or(false)
        })
        .on_lifecycle(move |event, cx| {
            if matches!(event, MenuLifecycleEvent::Closed(_)) {
                let _ = menu_lifecycle.update(cx, |input, cx| {
                    if input.context_menu_open {
                        input.context_menu_open = false;
                        cx.emit(TextInputEvent::ContextMenuClosed);
                    }
                });
            }
        })
        .on_activate(
            move |activation: &MenuActivation<TextInputMenuAction>, window, cx| {
                let action = *activation.action();
                let _ = menu_activate.update(cx, |input, cx| match action {
                    TextInputMenuAction::Undo => input.undo(&Undo, window, cx),
                    TextInputMenuAction::Redo => input.redo(&Redo, window, cx),
                    TextInputMenuAction::Cut => input.cut(&Cut, window, cx),
                    TextInputMenuAction::Copy => input.copy(&Copy, window, cx),
                    TextInputMenuAction::Paste => input.paste(&Paste, window, cx),
                    TextInputMenuAction::SelectAll => input.on_select_all(&SelectAll, window, cx),
                });
            },
        )
    }
}

struct TextElement {
    input: Entity<TextInput>,
}
struct TextPrepaint {
    line: ShapedLine,
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
        self.input.update(cx, |input, cx| {
            let theme = *cx.global::<TextInputTheme>();
            let paint = theme.variants.paint(input.variant);
            let line = input.rebuild_geometry(bounds, window, cx);
            let empty = input.buffer.text.is_empty();
            let caret_x = if empty {
                px(0.0)
            } else {
                line.x_for_index(input.buffer.selection.cursor())
            };
            let mut scroll = input
                .scroll
                .min((line.width - bounds.size.width + theme.metrics.scroll_padding).max(px(0.0)));
            if caret_x - scroll > bounds.size.width - theme.metrics.scroll_padding {
                scroll = caret_x - bounds.size.width + theme.metrics.scroll_padding;
            }
            if caret_x - scroll < px(0.0) {
                scroll = caret_x;
            }
            scroll = scroll.max(px(0.0));
            let active = input.enabled && input.focused && input.window_active;
            let (caret, selection) = if active && !input.buffer.selection.is_empty() {
                (
                    None,
                    Some(fill(
                        Bounds::from_corners(
                            point(
                                bounds.left()
                                    + line.x_for_index(input.buffer.selection.range.start)
                                    - scroll,
                                bounds.top(),
                            ),
                            point(
                                bounds.left() + line.x_for_index(input.buffer.selection.range.end)
                                    - scroll,
                                bounds.bottom(),
                            ),
                        ),
                        paint.selection,
                    )),
                )
            } else if active && input.caret_visible {
                (
                    Some(fill(
                        Bounds::new(
                            point(bounds.left() + caret_x - scroll, bounds.top()),
                            size(theme.metrics.caret_width, bounds.size.height),
                        ),
                        paint.caret,
                    )),
                    None,
                )
            } else {
                (None, None)
            };
            TextPrepaint {
                line,
                caret,
                selection,
                scroll,
            }
        })
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
        let focus = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let move_input = self.input.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                move_input.update(cx, |input, cx| input.on_global_mouse_move(event, cx));
            }
        });
        let up_input = self.input.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Capture {
                up_input.update(cx, |input, cx| input.on_global_mouse_up(event, cx));
            }
        });
        let origin = point(bounds.origin.x - prepaint.scroll, bounds.origin.y);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(selection) = prepaint.selection.take() {
                window.paint_quad(selection);
            }
            _ = prepaint
                .line
                .paint(origin, window.line_height(), window, cx);
            if let Some(caret) = prepaint.caret.take() {
                window.paint_quad(caret);
            }
        });
        self.input.update(cx, |input, _| {
            input.last_bounds = Some(bounds);
            input.scroll = prepaint.scroll;
        });
    }
}

fn marked_text_runs(display: &str, marked: Option<Range<usize>>, base: TextRun) -> Vec<TextRun> {
    let Some(marked) = marked else {
        return vec![base];
    };
    let start = clamp_char_boundary(display, marked.start, false);
    let end = clamp_char_boundary(display, marked.end.max(start), true);
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

fn visit_normalized_single_line(text: &str, mut visit: impl FnMut(char)) {
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                visit(' ');
            }
            '\n' | '\t' | '\u{2028}' | '\u{2029}' => visit(' '),
            ch if ch.is_control() => visit(' '),
            ch => visit(ch),
        }
    }
}

fn normalize_single_line(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    visit_normalized_single_line(text, |ch| result.push(ch));
    result
}

fn normalized_single_line_len(text: &str) -> usize {
    let mut len = 0;
    visit_normalized_single_line(text, |ch| len += ch.len_utf8());
    len
}

fn truncate_grapheme(text: &str, limit: usize) -> &str {
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
    let offset = clamp_char_boundary(text, offset, false);
    text[..offset]
        .split_word_bound_indices()
        .rfind(|(_, word)| !word.trim_start().is_empty())
        .map(|(index, _)| index)
        .unwrap_or(0)
}
fn next_word_end(text: &str, offset: usize) -> usize {
    let offset = clamp_char_boundary(text, offset, false);
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
    let probe = clamp_char_boundary(text, offset.min(text.len().saturating_sub(1)), false);
    text.split_word_bound_indices()
        .find_map(|(start, word)| {
            let end = start + word.len();
            (probe >= start && probe < end).then_some(start..end)
        })
        .unwrap_or(text.len()..text.len())
}
fn clamp_char_boundary(text: &str, offset: usize, up: bool) -> usize {
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
fn clamp_grapheme_boundary(text: &str, offset: usize, up: bool) -> usize {
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
fn normalize_byte_range(text: &str, range: Range<usize>) -> Range<usize> {
    if range.is_empty() {
        let caret = clamp_grapheme_boundary(text, range.start, false);
        return caret..caret;
    }
    let start = clamp_grapheme_boundary(text, range.start, false);
    let end = clamp_grapheme_boundary(text, range.end.max(range.start), true);
    start..end.max(start)
}

fn utf16_offset_to_byte(text: &str, offset: usize, up: bool) -> usize {
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
fn utf16_collapsed_caret_to_byte(text: &str, offset: usize) -> usize {
    clamp_grapheme_boundary(text, utf16_offset_to_byte(text, offset, false), false)
}
fn utf16_query_range_to_bytes(text: &str, range: Range<usize>) -> Range<usize> {
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
fn utf16_replacement_range_to_bytes(text: &str, range: Range<usize>) -> Range<usize> {
    if range.is_empty() {
        let caret = utf16_collapsed_caret_to_byte(text, range.start);
        caret..caret
    } else {
        utf16_query_range_to_bytes(text, range)
    }
}
fn utf16_selection_range_to_bytes(text: &str, range: Range<usize>) -> Range<usize> {
    if range.is_empty() {
        let caret = utf16_collapsed_caret_to_byte(text, range.start);
        caret..caret
    } else {
        utf16_query_range_to_bytes(text, range)
    }
}
fn byte_offset_to_utf16(text: &str, offset: usize) -> usize {
    text[..clamp_char_boundary(text, offset, false)]
        .encode_utf16()
        .count()
}
fn byte_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    byte_offset_to_utf16(text, range.start)..byte_offset_to_utf16(text, range.end)
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext, rgba};

    struct EventRoot {
        input: Entity<TextInput>,
        other_focus: FocusHandle,
        unrelated_menu: bool,
    }

    impl Render for EventRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(div().h(px(32.0)).child(self.input.clone()))
                .child(div().track_focus(&self.other_focus).child("Other"))
                .when(self.unrelated_menu, |root| {
                    root.child(
                        crate::menu::Menu::new(
                            "unrelated-menu",
                            "Unrelated menu",
                            vec![MenuEntry::action("Action", ())],
                        )
                        .debug_selector("unrelated-menu-target")
                        .on_activate(|_, _, _| {}),
                    )
                })
        }
    }
    fn install_theme(cx: &mut TestAppContext) {
        let paint = TextInputPaint::new(
            rgba(0xffffffff),
            rgba(0x888888ff),
            rgba(0x3355aaff),
            rgba(0xffffffff),
            rgba(0x777777ff),
            rgba(0x555555ff),
        );
        cx.set_global(TextInputTheme::new(
            TextInputVariants::new(paint, paint),
            TextInputMetrics::new(px(1.0), px(2.0), Duration::from_millis(16), px(20.0)),
        ));
        let menu_paint = crate::menu::MenuPaint::new(
            rgba(0x141415ff),
            rgba(0x252530ff),
            rgba(0xcdcdcdff),
            rgba(0x878787ff),
            rgba(0x606079ff),
            rgba(0x252530ff),
            rgba(0xcdcdcdff),
            rgba(0xd8647eff),
            rgba(0x252530ff),
        );
        let metrics = crate::menu::MenuMetrics::new(px(160.0), px(26.0));
        cx.set_global(crate::menu::MenuTheme::new(
            menu_paint,
            crate::menu::MenuSizes::new(metrics, metrics, metrics),
        ));
        cx.update(super::init);
        cx.update(crate::menu::init);
        cx.update(|cx| install_text_input_keybindings(cx, TextInputKeybindingProfile::MacOs));
    }

    fn input<'a>(
        cx: &'a mut TestAppContext,
        value: &'static str,
    ) -> (Entity<TextInput>, &'a mut VisualTestContext) {
        install_theme(cx);
        let (input, cx) = cx.add_window_view(move |window, cx| {
            TextInput::new("test-input", "Test input", value, window, cx)
                .debug_selector("test-input")
        });
        cx.update(|window, cx| {
            window.activate_window();
            input.read(cx).focus_handle().focus(window);
        });
        cx.run_until_parked();
        (input, cx)
    }

    fn input_with_events<'a>(
        cx: &'a mut TestAppContext,
        value: &'static str,
        unrelated_menu: bool,
    ) -> (
        Entity<TextInput>,
        FocusHandle,
        Rc<RefCell<Vec<TextInputEvent>>>,
        &'a mut VisualTestContext,
    ) {
        install_theme(cx);
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded_events = events.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let input = cx.new(|cx| {
                TextInput::new("test-input", "Test input", value, window, cx)
                    .debug_selector("test-input")
            });
            cx.subscribe(&input, move |_, _, event: &TextInputEvent, _| {
                recorded_events.borrow_mut().push(*event);
            })
            .detach();
            EventRoot {
                input,
                other_focus: cx.focus_handle(),
                unrelated_menu,
            }
        });
        let (input, other_focus) =
            root.read_with(cx, |root, _| (root.input.clone(), root.other_focus.clone()));
        cx.update(|window, cx| {
            window.activate_window();
            input.read(cx).focus_handle().focus(window);
        });
        cx.run_until_parked();
        events.borrow_mut().clear();
        (input, other_focus, events, cx)
    }

    fn mark_text(input: &Entity<TextInput>, cx: &mut VisualTestContext, text: &str) {
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, text, None, window, cx);
            });
        });
    }

    #[gpui::test]
    fn configured_larger_limit_preserves_initial_value_above_default_limit(
        cx: &mut TestAppContext,
    ) {
        install_theme(cx);
        let value = "é".repeat(DEFAULT_VALUE_LIMIT / 2 + 1024);
        let expected = value.clone();
        let (input, cx) = cx.add_window_view(move |window, cx| {
            TextInput::new("test-input", "Test input", value, window, cx)
                .input_length_limit(Some(128 * 1024))
        });
        cx.run_until_parked();

        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            expected
        );
    }

    #[gpui::test]
    fn removing_default_limit_preserves_initial_value_above_default_limit(cx: &mut TestAppContext) {
        install_theme(cx);
        let value = "é".repeat(DEFAULT_VALUE_LIMIT / 2 + 1024);
        let expected = value.clone();
        let (input, cx) = cx.add_window_view(move |window, cx| {
            TextInput::new("test-input", "Test input", value, window, cx).input_length_limit(None)
        });
        cx.run_until_parked();

        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            expected
        );
    }

    #[test]
    fn normalization_preserves_word_boundaries_for_every_line_separator() {
        let source = "a\r\nb\rc\nd\te\u{0007}f\u{2028}g\u{2029}h";
        assert_eq!(normalize_single_line(source), "a b c d e f g h");
        assert_eq!(
            normalized_single_line_len(source),
            normalize_single_line(source).len()
        );
    }
    #[test]
    fn surrogate_collapsed_range_stays_empty() {
        let text = "A😀B";
        let range = utf16_replacement_range_to_bytes(text, 2..2);
        assert!(range.is_empty());
        assert_eq!(range.start, 1);
    }
    #[test]
    fn surrogate_nonempty_range_expands_scalar() {
        let text = "A😀B";
        let range = utf16_replacement_range_to_bytes(text, 2..3);
        assert_eq!(&text[range], "😀");
    }
    #[test]
    fn grapheme_editing_is_atomic() {
        let mut buffer = TextBuffer::new("e\u{301}👩‍💻".into());
        assert!(buffer.delete_backward(HARD_VALUE_LIMIT));
        assert!(buffer.delete_backward(HARD_VALUE_LIMIT));
        assert_eq!(buffer.text, "");
    }
    #[test]
    fn transposition_swaps_complete_graphemes() {
        let mut buffer = TextBuffer::new("e\u{301}👩‍💻".into());
        assert!(buffer.transpose(HARD_VALUE_LIMIT));
        assert_eq!(buffer.text, "👩‍💻e\u{301}");
    }
    #[test]
    fn oversized_edit_is_atomic() {
        let mut buffer = TextBuffer::new("ok".into());
        let before = buffer.snapshot();
        assert!(!buffer.replace(0..2, "x".repeat(10), EditKind::Atomic, 4));
        assert_eq!(buffer.snapshot(), before);
    }
    #[test]
    fn history_is_bounded_across_undo_and_redo() {
        let mut buffer = TextBuffer::new(String::new());
        for _ in 0..240 {
            buffer.history.break_group();
            let end = buffer.text.len();
            assert!(buffer.replace(
                end..end,
                "x".repeat(4096),
                EditKind::Atomic,
                HARD_VALUE_LIMIT
            ));
        }
        for _ in 0..80 {
            buffer.undo();
        }
        assert!(buffer.history.undo.len() + buffer.history.redo.len() <= HISTORY_ENTRY_LIMIT);
        assert!(buffer.history.retained_bytes <= HISTORY_BYTE_LIMIT);
    }
    #[test]
    fn kill_capture_ends_at_grapheme_boundary() {
        let text = format!("{}👩‍💻", "x".repeat(KILL_RING_LIMIT - 1));
        let killed = truncate_grapheme(&text, KILL_RING_LIMIT);
        assert!(!killed.ends_with('\u{200d}'));
        assert!(killed.len() <= KILL_RING_LIMIT);
    }

    #[gpui::test]
    fn set_value_semantics_and_safe_event(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "old");
        input.update(cx, |input, cx| {
            input.emit_programmatic_changes = true;
            assert!(input.set_value("new\r\nvalue", cx));
            assert!(!input.set_value("new value", cx));
        });
        assert_eq!(
            input.read_with(cx, |input, _| (
                input.value().to_owned(),
                input.revision(),
                input.selection()
            )),
            (
                "new value".into(),
                1,
                TextInputSelection {
                    range: 9..9,
                    reversed: false
                }
            )
        );
        assert_eq!(input.read_with(cx, |input, _| input.revision()), 1);
    }

    #[gpui::test]
    fn composition_empty_state_cancels_before_cancel_event(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "abc");
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "", None, window, cx)
            })
        });
        assert!(input.read_with(cx, |input, _| input.composition().is_some()));
        cx.simulate_keystrokes("escape escape");
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| input.composition().is_none()));
        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            "abc"
        );
    }

    #[gpui::test]
    fn read_only_and_disabled_reject_mutation(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "abc");
        input.update(cx, |input, _| input.editable = false);
        cx.simulate_keystrokes("backspace");
        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            "abc"
        );
        input.update(cx, |input, _| {
            input.editable = true;
            input.enabled = false;
        });
        cx.simulate_keystrokes("backspace");
        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            "abc"
        );
    }

    #[gpui::test]
    fn pointer_lost_button_and_deactivation_cancel_owned_gesture(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "abcdef");
        input.update(cx, |input, cx| {
            input.pointer_generation = 10;
            input.pointer_gesture = Some(PointerGesture {
                generation: 10,
                anchor: 0,
                latest_position: point(px(0.0), px(0.0)),
            });
            input.on_global_mouse_move(
                &MouseMoveEvent {
                    position: point(px(0.0), px(0.0)),
                    pressed_button: None,
                    modifiers: Default::default(),
                },
                cx,
            );
            assert!(input.pointer_gesture.is_none());
            input.pointer_gesture = Some(PointerGesture {
                generation: 11,
                anchor: 0,
                latest_position: point(px(0.0), px(0.0)),
            });
        });
        cx.deactivate_window();
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| input.pointer_gesture.is_none()));
    }

    #[gpui::test]
    fn caret_does_not_schedule_while_unfocused_or_inactive(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "abc");
        cx.deactivate_window();
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| input.caret_task.is_none()));
        input.update(cx, |input, cx| {
            input.focused = false;
            input.window_active = true;
            input.restart_caret(cx);
            assert!(input.caret_task.is_none());
        });
    }

    #[test]
    fn grapheme_movement_and_replacement_never_split_clusters() {
        let mut buffer = TextBuffer::new("Ae\u{301}👩‍💻B".into());
        buffer.move_to(1);
        buffer.move_right(false);
        assert_eq!(buffer.selection.cursor(), "Ae\u{301}".len());
        buffer.select_to(2);
        assert_eq!(&buffer.text[buffer.selection.range.clone()], "e\u{301}");
        assert!(buffer.replace(
            buffer.selection.range.clone(),
            "X".into(),
            EditKind::Atomic,
            HARD_VALUE_LIMIT,
        ));
        assert_eq!(buffer.text, "AX👩‍💻B");
    }

    #[test]
    fn forward_delete_removes_one_complete_grapheme() {
        let mut buffer = TextBuffer::new("e\u{301}👩‍💻".into());
        buffer.move_to(0);
        assert!(buffer.delete_forward(HARD_VALUE_LIMIT));
        assert_eq!(buffer.text, "👩‍💻");
    }

    #[test]
    fn configured_limit_rejects_replacement_without_selection_or_history_changes() {
        let mut buffer = TextBuffer::new("abcd".into());
        buffer.select_from_anchor(1, 3);
        let before = buffer.snapshot();
        assert!(!buffer.replace(1..3, "wxyz".into(), EditKind::Atomic, 5));
        assert_eq!(buffer.snapshot(), before);
        assert!(buffer.history.undo.is_empty());
    }

    #[gpui::test]
    fn macos_command_option_and_control_bindings_drive_editing(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "Workspace Name");
        cx.simulate_keystrokes("alt-backspace ctrl-a X ctrl-e ctrl-h cmd-backspace");
        cx.run_until_parked();
        assert_eq!(input.read_with(cx, |input, _| input.value().to_owned()), "");
    }

    #[gpui::test]
    fn macos_select_all_replacement_is_one_undo_edit(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "Workspace Name");
        cx.simulate_keystrokes("cmd-a D e v cmd-z");
        cx.run_until_parked();
        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            "Workspace Name"
        );
    }

    #[gpui::test]
    fn macos_kill_and_yank_share_the_bounded_application_ring(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "abc");
        cx.simulate_keystrokes("ctrl-b ctrl-k ctrl-y");
        cx.run_until_parked();
        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            "abc"
        );
    }

    #[gpui::test]
    fn paste_availability_matches_clipboard_and_final_value_bounds(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "safe");
        input.update(cx, |input, _| {
            input.input_length_limit = 5;
            assert!(input.can_accept_paste("x"));
            assert!(!input.can_accept_paste("xy"));
            assert!(!input.can_accept_paste(""));
            assert!(!input.can_accept_paste(&"x".repeat(CLIPBOARD_INSERTION_LIMIT + 1)));
            input.buffer.select_all();
            assert!(input.can_accept_paste("xy"));
            input.editable = false;
            assert!(!input.can_accept_paste("x"));
        });
    }

    #[gpui::test]
    fn clipboard_over_hard_limit_is_rejected_atomically(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "safe");
        input.update(cx, |input, cx| input.select_all(cx));
        cx.write_to_clipboard(ClipboardItem::new_string(
            "x".repeat(CLIPBOARD_INSERTION_LIMIT + 1),
        ));
        cx.update(|window, cx| input.update(cx, |input, cx| input.paste(&Paste, window, cx)));
        assert_eq!(
            input.read_with(cx, |input, _| (
                input.value().to_owned(),
                input.selection(),
                input.revision(),
                input.buffer.history.undo.len(),
            )),
            (
                "safe".into(),
                TextInputSelection {
                    range: 0..4,
                    reversed: false,
                },
                0,
                0,
            )
        );
    }

    #[gpui::test]
    fn oversized_input_method_update_is_rejected_without_starting_composition(
        cx: &mut TestAppContext,
    ) {
        let (input, cx) = input(cx, "safe");
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.input_length_limit = HARD_VALUE_LIMIT;
                input.replace_and_mark_text_in_range(
                    None,
                    &"x".repeat(HARD_VALUE_LIMIT + 1),
                    None,
                    window,
                    cx,
                );
            });
        });
        assert_eq!(
            input.read_with(cx, |input, _| (
                input.value().to_owned(),
                input.revision(),
                input.composition().is_none(),
                input.buffer.history.undo.len(),
            )),
            ("safe".into(), 0, true, 0)
        );
    }

    #[gpui::test]
    fn kill_ring_capture_is_bounded_while_the_whole_selection_is_deleted(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "seed");
        let value = format!("{}👩‍💻tail", "x".repeat(KILL_RING_LIMIT));
        input.update(cx, |input, cx| {
            input.input_length_limit = HARD_VALUE_LIMIT;
            assert!(input.set_value(value, cx));
            input.select_all(cx);
        });
        cx.update(|window, cx| {
            input.update(cx, |input, cx| input.kill_to_end(&KillToEnd, window, cx));
        });
        assert_eq!(input.read_with(cx, |input, _| input.value().to_owned()), "");
        let killed = cx.update(|_, cx| cx.global::<TextKillRing>().0.clone());
        assert!(killed.len() <= KILL_RING_LIMIT);
        assert!(killed.is_char_boundary(killed.len()));
    }

    #[gpui::test]
    fn native_unmark_commits_and_immediately_restarts_caret_presentation(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "abc");
        mark_text(&input, cx, "日");
        let generation = input.read_with(cx, |input, _| input.caret_generation);

        cx.update(|window, cx| {
            input.update(cx, |input, cx| input.unmark_text(window, cx));
        });

        assert!(input.read_with(cx, |input, _| {
            input.composition().is_none()
                && input.caret_visible
                && input.caret_generation > generation
        }));
    }

    #[gpui::test]
    fn composition_movement_commits_one_undoable_edit_before_moving(cx: &mut TestAppContext) {
        let (input, _, events, cx) = input_with_events(cx, "abc", false);
        mark_text(&input, cx, "日");
        cx.update(|window, cx| {
            input.update(cx, |input, cx| input.move_left(&MoveLeft, window, cx));
        });
        assert_eq!(
            events.borrow().as_slice(),
            &[
                TextInputEvent::CompositionStarted,
                TextInputEvent::ValueChanged(TextInputValueChanged {
                    revision: 1,
                    source: TextInputChangeSource::InputMethodComposition,
                }),
                TextInputEvent::CompositionCommitted,
            ]
        );
        cx.update(|window, cx| input.update(cx, |input, cx| input.undo(&Undo, window, cx)));
        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            "abc"
        );
    }

    #[gpui::test]
    fn composition_undo_commits_then_restores_with_exact_revision_order(cx: &mut TestAppContext) {
        let (input, _, events, cx) = input_with_events(cx, "abc", false);
        mark_text(&input, cx, "日本");
        cx.update(|window, cx| input.update(cx, |input, cx| input.undo(&Undo, window, cx)));
        assert_eq!(
            events.borrow().as_slice(),
            &[
                TextInputEvent::CompositionStarted,
                TextInputEvent::ValueChanged(TextInputValueChanged {
                    revision: 1,
                    source: TextInputChangeSource::InputMethodComposition,
                }),
                TextInputEvent::CompositionCommitted,
                TextInputEvent::ValueChanged(TextInputValueChanged {
                    revision: 2,
                    source: TextInputChangeSource::Undo,
                }),
            ]
        );
        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            "abc"
        );
    }

    #[gpui::test]
    fn composition_redo_and_submit_interrupt_composition(cx: &mut TestAppContext) {
        let (input, _, events, cx) = input_with_events(cx, "abc", false);
        mark_text(&input, cx, "日");
        cx.update(|window, cx| input.update(cx, |input, cx| input.redo(&Redo, window, cx)));
        assert!(input.read_with(cx, |input, _| input.composition().is_none()));
        mark_text(&input, cx, "本");
        cx.update(|window, cx| input.update(cx, |input, cx| input.submit(&Submit, window, cx)));
        assert!(input.read_with(cx, |input, _| input.composition().is_none()));
        assert!(events.borrow().ends_with(&[
            TextInputEvent::CompositionStarted,
            TextInputEvent::ValueChanged(TextInputValueChanged {
                revision: 2,
                source: TextInputChangeSource::InputMethodComposition,
            }),
            TextInputEvent::CompositionCommitted,
            TextInputEvent::Submitted,
        ]));
    }

    #[gpui::test]
    fn empty_marked_text_continues_and_cancel_restores_original_state(cx: &mut TestAppContext) {
        let (input, _, events, cx) = input_with_events(cx, "abc", false);
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "", Some(0..0), window, cx);
                assert!(input.composition().is_some());
                input.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, cx);
                input.cancel(&Cancel, window, cx);
            });
        });
        assert_eq!(
            input.read_with(cx, |input, _| (
                input.value().to_owned(),
                input.composition().is_none(),
                input.revision(),
            )),
            ("abc".into(), true, 2)
        );
        assert_eq!(
            events.borrow().as_slice(),
            &[
                TextInputEvent::CompositionStarted,
                TextInputEvent::ValueChanged(TextInputValueChanged {
                    revision: 1,
                    source: TextInputChangeSource::InputMethodComposition,
                }),
                TextInputEvent::ValueChanged(TextInputValueChanged {
                    revision: 2,
                    source: TextInputChangeSource::InputMethodComposition,
                }),
                TextInputEvent::CompositionCancelled,
            ]
        );
    }

    #[gpui::test]
    fn multi_update_composition_commits_as_exactly_one_undo_edit(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "Workspace");
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "日", None, window, cx);
                input.replace_and_mark_text_in_range(None, "日本", None, window, cx);
                input.replace_text_in_range(None, "日本", window, cx);
                assert_eq!(input.buffer.history.undo.len(), 1);
            });
        });
        cx.simulate_keystrokes("cmd-z");
        cx.run_until_parked();
        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            "Workspace"
        );
    }

    #[gpui::test]
    fn pointer_press_commits_composition_before_starting_selection(cx: &mut TestAppContext) {
        let (input, _, events, cx) = input_with_events(cx, "abcdef", false);
        mark_text(&input, cx, "日");
        let click_position = input.read_with(cx, |input, _| {
            let bounds = input.last_bounds.expect("input should be painted");
            let geometry = input
                .geometry
                .as_ref()
                .expect("marked text should have current geometry");
            point(
                bounds.left() + geometry.line.x_for_index(1) - input.scroll,
                bounds.center().y,
            )
        });
        cx.simulate_mouse_down(click_position, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(click_position, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            input.read_with(cx, |input, _| (
                input.composition().is_none(),
                input.selection().range(),
            )),
            (true, 1..1),
            "composition commit must preserve the click's hit-tested position"
        );
        assert!(
            events
                .borrow()
                .contains(&TextInputEvent::CompositionCommitted)
        );
    }

    #[gpui::test]
    fn real_pointer_drag_selects_inside_and_releases_outside(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "abcdefghijklmnopqrstuvwxyz");
        let bounds = cx
            .debug_bounds("test-input")
            .expect("input should be painted");
        let start = point(bounds.left() + px(4.0), bounds.center().y);
        let outside = point(bounds.right() + px(40.0), bounds.center().y);
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(bounds.center(), MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| {
            !input.selection().is_empty()
                && input.pointer_gesture.is_none()
                && input.autoscroll_task.is_none()
        }));
    }

    #[gpui::test]
    fn real_pointer_drag_autoscroll_progresses_on_each_deadline(cx: &mut TestAppContext) {
        let (input, cx) = input(
            cx,
            concat!(
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            ),
        );
        input.update(cx, |input, cx| {
            input.buffer.move_to(0);
            input.scroll = px(0.0);
            input.restart_caret(cx);
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let bounds = cx
            .debug_bounds("test-input")
            .expect("input should be painted");
        let start = point(bounds.left() + px(2.0), bounds.center().y);
        let outside = point(bounds.right() + px(50.0), bounds.center().y);
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let before_deadline = input.read_with(cx, |input, _| (input.scroll, input.selection()));
        cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let pending_deadline = input.read_with(cx, |input, _| (input.scroll, input.selection()));
        assert_eq!(
            pending_deadline, before_deadline,
            "outside drag must wait for the first bounded autoscroll deadline"
        );
        cx.executor().advance_clock(Duration::from_millis(16));
        cx.run_until_parked();
        let first = input.read_with(cx, |input, _| input.scroll);
        cx.executor().advance_clock(Duration::from_millis(16));
        cx.run_until_parked();
        let second = input.read_with(cx, |input, _| input.scroll);
        cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::none());
        assert!(first > px(0.0), "first autoscroll deadline must advance");
        assert!(
            second > first,
            "later autoscroll deadline must progress: first={first:?}, second={second:?}"
        );
    }

    #[gpui::test]
    fn stale_autoscroll_generation_is_inert_and_releases_its_task_slot(cx: &mut TestAppContext) {
        let (input, cx) = input(
            cx,
            concat!(
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            ),
        );
        let bounds = cx
            .debug_bounds("test-input")
            .expect("input should be painted");
        input.update(cx, |input, cx| {
            input.buffer.move_to(0);
            input.scroll = px(0.0);
            input.pointer_gesture = Some(PointerGesture {
                generation: 7,
                anchor: 0,
                latest_position: point(bounds.right() + px(50.0), bounds.center().y),
            });
            input.start_autoscroll(7, cx);
        });
        cx.run_until_parked();
        input.update(cx, |input, _| {
            input.pointer_gesture = Some(PointerGesture {
                generation: 8,
                anchor: 0,
                latest_position: bounds.center(),
            });
        });
        cx.executor().advance_clock(Duration::from_millis(16));
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| {
            input.scroll == px(0.0)
                && input
                    .pointer_gesture
                    .is_some_and(|gesture| gesture.generation == 8)
                && input.autoscroll_generation.is_none()
                && input.autoscroll_task.is_none()
        }));
    }

    #[gpui::test]
    fn lost_button_blur_disable_and_stale_generation_cancel_drag(cx: &mut TestAppContext) {
        let (input, other_focus, _, cx) = input_with_events(cx, "abcdef", false);
        let bounds = cx
            .debug_bounds("test-input")
            .expect("input should be painted");
        cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
        assert!(input.read_with(cx, |input, _| input.pointer_gesture.is_none()));

        cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::none());
        cx.update(|window, _| other_focus.focus(window));
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| input.pointer_gesture.is_none()));

        input.update(cx, |input, cx| {
            input.focused = true;
            input.pointer_generation = 40;
            input.pointer_gesture = Some(PointerGesture {
                generation: 39,
                anchor: 0,
                latest_position: bounds.center(),
            });
            input.set_enabled(false, cx);
            assert!(input.pointer_gesture.is_none());
            assert!(input.pointer_generation > 40);
        });
    }

    #[gpui::test]
    fn synchronous_marked_update_refreshes_range_and_character_geometry(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "abc");
        let bounds = cx
            .debug_bounds("test-input")
            .expect("input should be painted");
        let (marked_bounds, character) = cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "日本", None, window, cx);
                let marked = input
                    .bounds_for_range(3..5, bounds, window, cx)
                    .expect("marked text bounds should exist");
                let character = input
                    .character_index_for_point(marked.bottom_right(), window, cx)
                    .expect("character geometry should exist");
                (marked, character)
            })
        });
        assert!(marked_bounds.size.width > px(0.0));
        assert_eq!(character, 5);
    }

    #[gpui::test]
    fn caret_restart_waits_a_full_interval_and_stale_generation_is_inert(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "abc");
        cx.executor().advance_clock(CARET_BLINK_INTERVAL / 2);
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| input.caret_visible));
        let stale_generation = input.read_with(cx, |input, _| input.caret_generation);
        input.update(cx, |input, cx| input.restart_caret(cx));
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| input.caret_generation != stale_generation));
        cx.executor()
            .advance_clock(CARET_BLINK_INTERVAL - Duration::from_millis(1));
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| input.caret_visible));
        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        assert!(!input.read_with(cx, |input, _| input.caret_visible));
    }

    #[gpui::test]
    fn caret_reactivation_is_visible_and_schedules_only_when_active(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "abc");
        cx.executor().advance_clock(CARET_BLINK_INTERVAL);
        cx.run_until_parked();
        assert!(!input.read_with(cx, |input, _| input.caret_visible));
        cx.deactivate_window();
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| input.caret_visible
            && input.caret_task.is_none()));
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| input.caret_visible
            && input.caret_task.is_some()));
    }

    #[gpui::test]
    fn caret_phase_changes_reuse_stable_shaping(cx: &mut TestAppContext) {
        let (input, cx) = input(cx, "stable shaping");
        let before = input.read_with(cx, |input, _| input.shape_count);
        cx.executor().advance_clock(CARET_BLINK_INTERVAL);
        cx.run_until_parked();
        let after = input.read_with(cx, |input, _| input.shape_count);
        assert_eq!(after, before);
    }

    #[gpui::test]
    fn runtime_editable_and_enabled_transitions_preserve_selection_and_gate_edits(
        cx: &mut TestAppContext,
    ) {
        let (input, cx) = input(cx, "abc");
        input.update(cx, |input, cx| {
            input.select_all(cx);
            input.set_editable(false, cx);
        });
        cx.simulate_keystrokes("backspace");
        assert_eq!(
            input.read_with(cx, |input, _| input.selection().range()),
            0..3
        );
        input.update(cx, |input, cx| {
            input.set_editable(true, cx);
            input.set_enabled(false, cx);
        });
        cx.simulate_keystrokes("backspace");
        assert_eq!(
            input.read_with(cx, |input, _| input.value().to_owned()),
            "abc"
        );
        input.update(cx, |input, cx| input.set_enabled(true, cx));
        cx.simulate_keystrokes("backspace");
        assert_eq!(input.read_with(cx, |input, _| input.value().to_owned()), "");
    }

    #[gpui::test]
    fn set_value_cancels_composition_clears_history_and_emits_in_order(cx: &mut TestAppContext) {
        let (input, _, events, cx) = input_with_events(cx, "abc", false);
        mark_text(&input, cx, "日");
        events.borrow_mut().clear();
        input.update(cx, |input, cx| {
            input.emit_programmatic_changes = true;
            assert!(input.set_value("new\r\nvalue", cx));
        });
        assert_eq!(
            events.borrow().as_slice(),
            &[
                TextInputEvent::CompositionCancelled,
                TextInputEvent::ValueChanged(TextInputValueChanged {
                    revision: 2,
                    source: TextInputChangeSource::Programmatic,
                }),
            ]
        );
        assert_eq!(
            input.read_with(cx, |input, _| (
                input.value().to_owned(),
                input.selection().range(),
                input.composition().is_none(),
                input.buffer.history.undo.len(),
                input.buffer.history.redo.len(),
            )),
            ("new value".into(), 9..9, true, 0, 0)
        );
    }

    #[gpui::test]
    fn propagated_tab_requests_are_typed_and_keep_focus_with_the_input(cx: &mut TestAppContext) {
        let (input, _, events, cx) = input_with_events(cx, "abc", false);
        input.update(cx, |input, _| {
            input.tab_behavior = TextInputTabBehavior::Propagate;
        });

        cx.simulate_keystrokes("tab shift-tab");
        cx.run_until_parked();

        assert_eq!(
            events.borrow().as_slice(),
            &[
                TextInputEvent::TabForwardRequested,
                TextInputEvent::TabBackwardRequested,
            ]
        );
        assert!(input.read_with(cx, |input, _| input.is_focused()));
    }

    #[gpui::test]
    fn owned_context_menu_reports_truthful_focus_and_lifecycle_continuity(cx: &mut TestAppContext) {
        let (input, _, events, cx) = input_with_events(cx, "abc", false);
        let bounds = cx
            .debug_bounds("test-input")
            .expect("input should be painted");
        cx.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
        cx.run_until_parked();
        assert!(cx.update(|window, cx| crate::menu::window_menu_is_open(window, cx)));
        assert!(!input.read_with(cx, |input, _| input.is_focused()));
        assert_eq!(
            &events.borrow()[..2],
            &[TextInputEvent::ContextMenuOpened, TextInputEvent::FocusLost,]
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(events.borrow().contains(&TextInputEvent::ContextMenuClosed));
        assert!(cx.update(|window, cx| input.read(cx).focus_handle().is_focused(window)));
        assert!(input.read_with(cx, |input, _| input.is_focused()));
    }

    #[gpui::test]
    fn unrelated_menu_causes_focus_lost_without_owned_context_events(cx: &mut TestAppContext) {
        let (input, _, events, cx) = input_with_events(cx, "abc", true);
        let menu = cx
            .debug_bounds("unrelated-menu-target")
            .expect("unrelated menu should be painted");
        cx.simulate_click(menu.center(), Modifiers::none());
        cx.run_until_parked();
        assert!(!input.read_with(cx, |input, _| input.is_focused()));
        assert!(events.borrow().contains(&TextInputEvent::FocusLost));
        assert!(!events.borrow().iter().any(|event| matches!(
            event,
            TextInputEvent::ContextMenuOpened | TextInputEvent::ContextMenuClosed
        )));
    }
}
