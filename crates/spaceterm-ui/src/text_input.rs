//! A bounded, content-safe, single-line GPUI text editor.
//!
//! Values are always normalized to one line. The default value limit is 64 KiB and the absolute
//! value limit is 1 MiB. Clipboard insertion is limited to 1 MiB before the configured value
//! limit is applied. Undo and redo retain at most 128 snapshots and 1 MiB of text in total. The
//! application kill ring retains at most 64 KiB. All limits are byte limits and every truncation
//! performed during construction or kill-ring capture ends at a complete grapheme boundary.
//! Obscured inputs use a 16 KiB limit, render one bullet per grapheme, and retain no clipboard,
//! kill-ring, undo, or redo content.

use std::{ops::Range, time::Duration};

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardEntry, ClipboardItem, ContentMask, Context, CursorStyle, DispatchPhase,
    Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, Font, Global, GlobalElementId, InspectorElementId, IntoElement, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    Render, Rgba, ShapedLine, SharedString, Style, Subscription, Task, TextRun, UTF16Selection,
    UnderlineStyle, Window, actions, div, fill, point, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation as _;
use zeroize::{Zeroize as _, Zeroizing};

use crate::{
    button::ModalControlScope,
    menu::{ContextMenu, MenuActivation, MenuEntry, MenuLifecycleEvent},
};

const KEY_CONTEXT: &str = "SpaceTermTextInput";
const CARET_BLINK_INTERVAL: Duration = Duration::from_millis(530);
const HARD_VALUE_LIMIT: usize = 1024 * 1024;
const DEFAULT_VALUE_LIMIT: usize = 64 * 1024;
const OBSCURED_VALUE_LIMIT: usize = 16 * 1024;
const CLIPBOARD_INSERTION_LIMIT: usize = 1024 * 1024;
const KILL_RING_LIMIT: usize = 64 * 1024;
const HISTORY_ENTRY_LIMIT: usize = 128;
const HISTORY_BYTE_LIMIT: usize = 1024 * 1024;
const OBSCURED_BULLET: char = '\u{2022}';
const OBSCURED_BULLET_BYTES: usize = OBSCURED_BULLET.len_utf8();

#[derive(Default)]
struct TextKillRing(Zeroizing<String>);

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
            KeyBinding::new(
                "home",
                MoveToBeginning,
                Some("SpaceTermTextInput && home_end == edit"),
            ),
            KeyBinding::new(
                "end",
                MoveToEnd,
                Some("SpaceTermTextInput && home_end == edit"),
            ),
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

/// How an input's value is presented and retained while editing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextInputContentMode {
    /// Present the value as ordinary text and enable the complete editing history.
    #[default]
    Plain,
    /// Present one bullet per grapheme and retain no clipboard or editing history.
    Obscured,
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

/// How Return behaves while a [`TextInput`] owns focus.
///
/// Active input-method composition always commits and consumes the first Return without emitting
/// [`TextInputEvent::Submitted`]. These variants apply only when no composition is active.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextInputReturnBehavior {
    /// Emit [`TextInputEvent::Submitted`] and consume Return.
    #[default]
    Consume,
    /// Emit [`TextInputEvent::Submitted`] and let a containing composite handle Return afterward.
    Propagate,
}

/// How Escape behaves while a [`TextInput`] owns focus.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextInputEscapeBehavior {
    /// Emit [`TextInputEvent::Cancelled`] and consume Escape.
    #[default]
    Consume,
    /// Emit [`TextInputEvent::Cancelled`] and let a containing composite handle Escape afterward.
    ///
    /// The Escape that cancels active input-method composition is always consumed. Propagation is
    /// possible only for a later Escape after composition has ended.
    Propagate,
}

/// How Tab and Shift-Tab behave while a [`TextInput`] owns focus.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextInputTabBehavior {
    /// Move through the Operating-System Window's normal tab order.
    #[default]
    MoveFocus,
    /// Emit a typed traversal request for a containing composite control.
    Propagate,
}

/// How unmodified Home and End behave while a [`TextInput`] owns focus.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextInputHomeEndBehavior {
    /// Move the editor caret to the beginning or end.
    #[default]
    MoveCaret,
    /// Leave the keys available to a containing composite control's key context.
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
    text: Zeroizing<String>,
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
    text: Zeroizing<String>,
    selection: Selection,
    history: History,
    history_enabled: bool,
}

impl TextBuffer {
    fn new(text: String) -> Self {
        let end = text.len();
        Self {
            text: Zeroizing::new(text),
            selection: Selection::caret(end),
            history: History::default(),
            history_enabled: true,
        }
    }
    fn set_history_enabled(&mut self, enabled: bool) {
        self.history_enabled = enabled;
        if !enabled {
            self.history.clear();
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
    fn redo(&mut self) -> bool {
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
    content_mode: TextInputContentMode,
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
    content_mode: TextInputContentMode,
    variant: TextInputVariant,
    enabled: bool,
    editable: bool,
    tab_stop: bool,
    tab_behavior: TextInputTabBehavior,
    home_end_behavior: TextInputHomeEndBehavior,
    context_menu_enabled: bool,
    return_behavior: TextInputReturnBehavior,
    escape_behavior: TextInputEscapeBehavior,
    input_length_limit: usize,
    emit_programmatic_changes: bool,
    focus_handle: FocusHandle,
    focused: bool,
    window_active: bool,
    buffer: TextBuffer,
    initial_value_source: Option<Zeroizing<String>>,
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
    paste_available: bool,
    #[cfg(test)]
    shape_count: usize,
    #[cfg(test)]
    value_shape_clone_count: usize,
    #[cfg(test)]
    clipboard_read_count: usize,
    #[cfg(test)]
    last_shaped_display: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl TextInput {
    pub(crate) const fn owns_context_menu(&self) -> bool {
        self.context_menu_open
    }

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
        let initial_value = Zeroizing::new(initial_value.into());
        let normalized = Zeroizing::new(normalize_single_line(&initial_value));
        let normalized =
            Zeroizing::new(truncate_grapheme(&normalized, HARD_VALUE_LIMIT).to_owned());
        let default_value = truncate_grapheme(&normalized, DEFAULT_VALUE_LIMIT).to_owned();
        let initial_value_source = (default_value != *normalized).then_some(normalized);
        let accessibility_name = accessibility_name.into();
        Self {
            id: id.into(),
            debug_selector: accessibility_name.clone(),
            accessibility_name,
            placeholder: SharedString::default(),
            content_mode: TextInputContentMode::default(),
            variant: TextInputVariant::default(),
            enabled: true,
            editable: true,
            tab_stop: true,
            tab_behavior: TextInputTabBehavior::default(),
            home_end_behavior: TextInputHomeEndBehavior::default(),
            context_menu_enabled: true,
            return_behavior: TextInputReturnBehavior::default(),
            escape_behavior: TextInputEscapeBehavior::default(),
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
            paste_available: false,
            #[cfg(test)]
            shape_count: 0,
            #[cfg(test)]
            value_shape_clone_count: 0,
            #[cfg(test)]
            clipboard_read_count: 0,
            #[cfg(test)]
            last_shaped_display: None,
            _subscriptions: subscriptions,
        }
    }

    /// Sets the placeholder shown when the value is empty.
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self.geometry = None;
        self
    }

    /// Replaces the logical accessibility name without changing focus or editor state.
    pub fn set_accessibility_name(
        &mut self,
        accessibility_name: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let accessibility_name = accessibility_name.into();
        if self.accessibility_name == accessibility_name {
            return;
        }
        self.accessibility_name = accessibility_name;
        cx.notify();
    }

    /// Replaces the placeholder without changing focus or editor state.
    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let placeholder = placeholder.into();
        if self.placeholder == placeholder {
            return;
        }
        self.placeholder = placeholder;
        self.geometry = None;
        cx.notify();
    }
    /// Selects ordinary or obscured content handling.
    ///
    /// Obscured values are capped at 16 KiB, rendered as one bullet per grapheme, and never enter
    /// the clipboard, kill ring, undo stack, redo stack, or native surrounding-text queries.
    pub fn content_mode(mut self, mode: TextInputContentMode) -> Self {
        self.content_mode = mode;
        self.buffer
            .set_history_enabled(mode == TextInputContentMode::Plain);
        if mode == TextInputContentMode::Obscured {
            self.input_length_limit = self.input_length_limit.min(OBSCURED_VALUE_LIMIT);
            let value = truncate_grapheme(&self.buffer.text, self.input_length_limit).to_owned();
            if value != *self.buffer.text {
                self.buffer = TextBuffer::new(value);
                self.buffer.set_history_enabled(false);
            }
            self.initial_value_source = None;
        }
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
    /// Selects whether unmodified Home and End edit the caret or delegate to a composite.
    pub fn home_end_behavior(mut self, behavior: TextInputHomeEndBehavior) -> Self {
        self.home_end_behavior = behavior;
        self
    }
    /// Controls whether secondary click presents the standard editor context menu.
    ///
    /// Composite controls rendered inside another deferred layer can disable the nested menu while
    /// retaining every keyboard editing and clipboard command.
    pub fn context_menu(mut self, enabled: bool) -> Self {
        self.context_menu_enabled = enabled;
        self
    }
    /// Selects whether a non-composition Return is consumed or may bubble after submission.
    pub fn return_behavior(mut self, behavior: TextInputReturnBehavior) -> Self {
        self.return_behavior = behavior;
        self
    }
    /// Selects whether a non-composition Escape is consumed or may bubble after cancellation.
    pub fn escape_behavior(mut self, behavior: TextInputEscapeBehavior) -> Self {
        self.escape_behavior = behavior;
        self
    }
    /// Sets the optional product limit. `Some` is clamped to 1 MiB; `None` removes the safe default
    /// while retaining the hard 1 MiB limit. An initial value above the resulting limit is
    /// grapheme-safely truncated before the entity can emit events.
    pub fn input_length_limit(mut self, limit: Option<usize>) -> Self {
        self.input_length_limit =
            limit
                .unwrap_or(HARD_VALUE_LIMIT)
                .min(HARD_VALUE_LIMIT)
                .min(match self.content_mode {
                    TextInputContentMode::Plain => HARD_VALUE_LIMIT,
                    TextInputContentMode::Obscured => OBSCURED_VALUE_LIMIT,
                });
        let source = self
            .initial_value_source
            .as_ref()
            .map_or(self.buffer.text.as_str(), |value| value.as_str());
        let value = truncate_grapheme(source, self.input_length_limit).to_owned();
        if value != *self.buffer.text {
            self.buffer = TextBuffer::new(value);
            self.buffer
                .set_history_enabled(self.content_mode == TextInputContentMode::Plain);
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
    /// Removes and returns the current value while clearing every retained editing state.
    ///
    /// The caller becomes the sole owner of the returned value. The input no longer retains the
    /// value through its buffer, composition, undo, redo, initial-value source, or shaped cache.
    pub fn take_value(&mut self, cx: &mut Context<Self>) -> String {
        let value = self.take_value_without_event();
        if !value.is_empty() {
            self.advance_revision(
                TextInputChangeSource::Programmatic,
                self.emit_programmatic_changes,
                cx,
            );
        } else {
            self.restart_caret(cx);
        }
        value
    }
    /// Clears the current value and every retained editing state.
    ///
    /// Returns whether the current value was nonempty.
    pub fn clear(&mut self, cx: &mut Context<Self>) -> bool {
        let mut value = self.take_value_without_event();
        let changed = !value.is_empty();
        value.zeroize();
        if changed {
            self.advance_revision(
                TextInputChangeSource::Programmatic,
                self.emit_programmatic_changes,
                cx,
            );
        } else {
            self.restart_caret(cx);
        }
        changed
    }
    /// Returns whether [`Self::set_value`] would preserve `value` byte-for-byte.
    pub fn can_set_value_exactly(&self, value: &str) -> bool {
        value.len() <= self.input_length_limit
            && value.len() <= HARD_VALUE_LIMIT
            && !value.chars().any(requires_single_line_normalization)
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
        let value = Zeroizing::new(value.into());
        let normalized = Zeroizing::new(normalize_single_line(&value));
        if normalized.len() > self.input_length_limit || normalized.len() > HARD_VALUE_LIMIT {
            return false;
        }
        if self.composition.take().is_some() {
            cx.emit(TextInputEvent::CompositionCancelled);
        }
        self.buffer.history.clear();
        let changed = self.buffer.text.as_str() != normalized.as_str();
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

    fn take_value_without_event(&mut self) -> String {
        self.composition = None;
        self.buffer.history.clear();
        self.buffer.selection = Selection::caret(0);
        self.initial_value_source = None;
        self.geometry = None;
        self.scroll = px(0.0);
        self.cancel_pointer_gesture();
        std::mem::take(&mut *self.buffer.text)
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

    fn display_offset_for_source(&self, source_offset: usize) -> usize {
        match self.content_mode {
            TextInputContentMode::Plain => source_offset.min(self.buffer.text.len()),
            TextInputContentMode::Obscured => {
                let source_offset =
                    clamp_grapheme_boundary(&self.buffer.text, source_offset, false);
                self.buffer.text[..source_offset].graphemes(true).count() * OBSCURED_BULLET_BYTES
            }
        }
    }

    fn source_offset_for_display(&self, display_offset: usize) -> usize {
        match self.content_mode {
            TextInputContentMode::Plain => {
                clamp_grapheme_boundary(&self.buffer.text, display_offset, false)
            }
            TextInputContentMode::Obscured => {
                let grapheme_index = display_offset / OBSCURED_BULLET_BYTES;
                self.buffer
                    .text
                    .grapheme_indices(true)
                    .nth(grapheme_index)
                    .map_or(self.buffer.text.len(), |(offset, _)| offset)
            }
        }
    }

    fn display_text(&self) -> SharedString {
        match self.content_mode {
            TextInputContentMode::Plain => self.buffer.text.as_str().to_owned().into(),
            TextInputContentMode::Obscured => {
                std::iter::repeat_n(OBSCURED_BULLET, self.buffer.text.graphemes(true).count())
                    .collect::<String>()
                    .into()
            }
        }
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
    fn refresh_paste_availability(&mut self, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.clipboard_read_count += 1;
        }
        self.paste_available = cx
            .read_from_clipboard()
            .and_then(bounded_clipboard_text)
            .is_some_and(|text| self.can_accept_paste(&text));
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
        if self.buffer.history_enabled && state.original.text != self.buffer.text {
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
        if self.content_mode == TextInputContentMode::Obscured {
            cx.stop_propagation();
            return;
        }
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
        cx.global_mut::<TextKillRing>().0 = Zeroizing::new(killed);
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
        if self.content_mode == TextInputContentMode::Obscured {
            cx.stop_propagation();
            return;
        }
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
        if self.content_mode == TextInputContentMode::Plain
            && self.enabled
            && let Some(text) = self.buffer.selected_text()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        }
        cx.stop_propagation();
    }
    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if self.content_mode == TextInputContentMode::Plain && self.can_edit() {
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
        if let Some(text) = cx.read_from_clipboard().and_then(bounded_clipboard_text)
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
        if self.content_mode == TextInputContentMode::Plain && self.can_edit() {
            self.commit_composition(cx);
            if self.buffer.undo() {
                self.advance_revision(TextInputChangeSource::Undo, true, cx);
            } else {
                self.restart_caret(cx);
            }
        }
    }
    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.content_mode == TextInputContentMode::Plain && self.can_edit() {
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
            if self.commit_composition(cx) {
                self.restart_caret(cx);
                cx.stop_propagation();
                return;
            }
            self.buffer.history.break_group();
            cx.emit(TextInputEvent::Submitted);
            match self.return_behavior {
                TextInputReturnBehavior::Consume => cx.stop_propagation(),
                TextInputReturnBehavior::Propagate => cx.propagate(),
            }
        }
    }
    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if self.enabled {
            if self.cancel_composition(cx) {
                cx.stop_propagation();
                return;
            }
            self.buffer.history.break_group();
            cx.emit(TextInputEvent::Cancelled);
            match self.escape_behavior {
                TextInputEscapeBehavior::Consume => cx.stop_propagation(),
                TextInputEscapeBehavior::Propagate => cx.propagate(),
            }
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

    fn reconcile_scroll(
        &mut self,
        line: &ShapedLine,
        bounds: Bounds<Pixels>,
        metrics: TextInputMetrics,
    ) -> Pixels {
        if bounds.size.width <= px(0.0) {
            self.scroll = px(0.0);
            return self.scroll;
        }
        let caret_x = if self.buffer.text.is_empty() {
            px(0.0)
        } else {
            line.x_for_index(self.display_offset_for_source(self.buffer.selection.cursor()))
        };
        let mut scroll = self
            .scroll
            .min((line.width - bounds.size.width + metrics.scroll_padding).max(px(0.0)));
        if caret_x - scroll > bounds.size.width - metrics.scroll_padding {
            scroll = caret_x - bounds.size.width + metrics.scroll_padding;
        }
        if caret_x - scroll < px(0.0) {
            scroll = caret_x;
        }
        self.scroll = scroll.max(px(0.0));
        self.scroll
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
        self.source_offset_for_display(
            geometry
                .line
                .closest_index_for_x(position.x - bounds.left() + self.scroll),
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
            content_mode: self.content_mode,
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
        let display: SharedString = if empty {
            self.placeholder.clone()
        } else {
            if self.content_mode == TextInputContentMode::Plain {
                #[cfg(test)]
                {
                    self.value_shape_clone_count += 1;
                }
            }
            self.display_text()
        };
        let marked_range = marked_range.map(|range| {
            self.display_offset_for_source(range.start)..self.display_offset_for_source(range.end)
        });
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
            self.last_shaped_display = Some(display.clone());
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
        if self.content_mode == TextInputContentMode::Obscured {
            *adjusted = None;
            return None;
        }
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
        let had_composition = self.composition.is_some();
        let range = range_utf16
            .map(|range| utf16_replacement_range_to_bytes(&self.buffer.text, range))
            .or_else(|| {
                self.composition
                    .as_ref()
                    .map(|state| state.marked_range.clone())
            })
            .unwrap_or_else(|| self.buffer.selection.range.clone());
        let normalized_len = normalized_single_line_len(text);
        if !self
            .buffer
            .can_replace(&range, normalized_len, self.input_length_limit)
        {
            if had_composition && self.commit_composition(cx) {
                self.rebuild_geometry(self.last_bounds.unwrap_or_default(), window, cx);
                self.restart_caret(cx);
            }
            return;
        }
        let normalized = normalize_single_line(text);
        let changed = if had_composition {
            self.buffer
                .replace_without_history(range, &normalized, self.input_length_limit)
        } else {
            self.buffer
                .replace(range, normalized, EditKind::Insert, self.input_length_limit)
        };
        self.finish_edit(
            changed,
            if had_composition {
                TextInputChangeSource::InputMethodComposition
            } else {
                TextInputChangeSource::Keyboard
            },
            cx,
        );
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
        let marked_range = range_utf16
            .map(|range| utf16_replacement_range_to_bytes(&self.buffer.text, range))
            .or_else(|| {
                self.composition
                    .as_ref()
                    .map(|state| state.marked_range.clone())
            })
            .unwrap_or_else(|| self.buffer.selection.range.clone());
        let range = normalize_byte_range(&self.buffer.text, marked_range.clone());
        let prefix = &self.buffer.text[range.start..marked_range.start];
        let suffix = &self.buffer.text[marked_range.end..range.end];
        let marked_replacement_len = normalized_single_line_len(text);
        let replacement_len = prefix.len() + marked_replacement_len + suffix.len();
        if !self
            .buffer
            .can_replace(&range, replacement_len, self.input_length_limit)
        {
            return;
        }
        let marked_replacement = normalize_single_line(text);
        let selected_range = selected_utf16
            .map(|selected| utf16_selection_range_to_bytes(&marked_replacement, selected));
        let marked_start = range.start + prefix.len();
        let replacement = if prefix.is_empty() && suffix.is_empty() {
            marked_replacement
        } else {
            let mut replacement = String::with_capacity(replacement_len);
            replacement.push_str(prefix);
            replacement.push_str(&marked_replacement);
            replacement.push_str(suffix);
            replacement
        };
        if self.composition.is_none() {
            self.composition = Some(CompositionState {
                original: self.buffer.snapshot(),
                marked_range: marked_range.clone(),
            });
            cx.emit(TextInputEvent::CompositionStarted);
        }
        let changed =
            self.buffer
                .replace_without_history(range, &replacement, self.input_length_limit);
        if let Some(composition) = &mut self.composition {
            composition.marked_range = marked_start..marked_start + marked_replacement_len;
        }
        self.buffer.selection = selected_range.map_or_else(
            || Selection::caret(marked_start + marked_replacement_len),
            |selected| Selection {
                range: marked_start + selected.start..marked_start + selected.end,
                reversed: false,
            },
        );
        self.finish_edit(changed, TextInputChangeSource::InputMethodComposition, cx);
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
        let scroll = self.reconcile_scroll(&line, bounds, cx.global::<TextInputTheme>().metrics);
        let range = utf16_query_range_to_bytes(&self.buffer.text, range_utf16);
        let display_range =
            self.display_offset_for_source(range.start)..self.display_offset_for_source(range.end);
        Some(Bounds::from_corners(
            point(
                bounds.left() + line.x_for_index(display_range.start) - scroll,
                bounds.top(),
            ),
            point(
                bounds.left() + line.x_for_index(display_range.end) - scroll,
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
        let scroll = self.reconcile_scroll(&line, bounds, cx.global::<TextInputTheme>().metrics);
        let index = self
            .source_offset_for_display(line.closest_index_for_x(point.x - bounds.left() + scroll));
        Some(byte_offset_to_utf16(&self.buffer.text, index))
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.initial_value_source = None;
        let entity = cx.entity();
        let has_selection = !self.buffer.selection.is_empty();
        let can_edit = self.can_edit();
        let can_paste = self.paste_available;
        let exposes_content = self.content_mode == TextInputContentMode::Plain;
        let entries = vec![
            MenuEntry::action("Undo", TextInputMenuAction::Undo)
                .disabled(!exposes_content || !can_edit || self.buffer.history.undo.is_empty()),
            MenuEntry::action("Redo", TextInputMenuAction::Redo)
                .disabled(!exposes_content || !can_edit || self.buffer.history.redo.is_empty()),
            MenuEntry::separator(),
            MenuEntry::action("Cut", TextInputMenuAction::Cut)
                .disabled(!exposes_content || !can_edit || !has_selection),
            MenuEntry::action("Copy", TextInputMenuAction::Copy)
                .disabled(!exposes_content || !self.enabled || !has_selection),
            MenuEntry::action("Paste", TextInputMenuAction::Paste).disabled(!can_paste),
            MenuEntry::action("Select All", TextInputMenuAction::SelectAll)
                .disabled(!self.enabled || self.buffer.text.is_empty()),
        ];
        let selector = self.debug_selector.clone();
        let key_context = match self.home_end_behavior {
            TextInputHomeEndBehavior::MoveCaret => "SpaceTermTextInput home_end = edit",
            TextInputHomeEndBehavior::Propagate => "SpaceTermTextInput home_end = propagate",
        };
        let focus_anchor = ModalControlScope::register_current_focus_anchor(&self.focus_handle);
        let editor = div()
            .id(self.id.clone())
            .debug_selector(move || selector.to_string())
            .size_full()
            .min_w_0()
            .key_context(key_context)
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
        let control = if self.context_menu_enabled {
            ContextMenu::new(
                ("text-input-context-menu", entity.entity_id()),
                name,
                editor,
                entries,
            )
            .fill_parent_width()
            .disabled(!self.enabled)
            .on_open_request(move |_, _, cx| {
                menu_open
                    .update(cx, |input, cx| {
                        if !input.enabled {
                            return false;
                        }
                        input.refresh_paste_availability(cx);
                        input.context_menu_open = true;
                        input.cancel_pointer_gesture();
                        cx.emit(TextInputEvent::ContextMenuOpened);
                        cx.notify();
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
                        TextInputMenuAction::SelectAll => {
                            input.on_select_all(&SelectAll, window, cx)
                        }
                    });
                },
            )
            .into_any_element()
        } else {
            editor.into_any_element()
        };
        if let Some(anchor) = focus_anchor {
            div()
                .id(("modal-text-input-focus-anchor", entity.entity_id()))
                .relative()
                .size_full()
                .flex()
                .items_center()
                .anchor_scroll(Some(anchor.scroll_anchor()))
                .child(control)
                .child(anchor.bounds_tracker(px(0.0)))
                .into_any_element()
        } else {
            control.into_any_element()
        }
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
                line.x_for_index(input.display_offset_for_source(input.buffer.selection.cursor()))
            };
            let scroll = input.reconcile_scroll(&line, bounds, theme.metrics);
            let active = input.enabled && input.focused && input.window_active;
            let (caret, selection) = if active && !input.buffer.selection.is_empty() {
                (
                    None,
                    Some(fill(
                        Bounds::from_corners(
                            point(
                                bounds.left()
                                    + line.x_for_index(input.display_offset_for_source(
                                        input.buffer.selection.range.start,
                                    ))
                                    - scroll,
                                bounds.top(),
                            ),
                            point(
                                bounds.left()
                                    + line.x_for_index(input.display_offset_for_source(
                                        input.buffer.selection.range.end,
                                    ))
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

fn requires_single_line_normalization(ch: char) -> bool {
    matches!(ch, '\r' | '\n' | '\t' | '\u{2028}' | '\u{2029}') || ch.is_control()
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
            ch if requires_single_line_normalization(ch) => visit(' '),
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
fn bounded_clipboard_text(item: ClipboardItem) -> Option<String> {
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
#[path = "text_input_tests.rs"]
mod tests;
