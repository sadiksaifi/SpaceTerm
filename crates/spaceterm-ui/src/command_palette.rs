use std::{ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, AppContext as _, Corner, CursorStyle, Entity, EventEmitter, Global,
    HitboxBehavior, InteractiveElement as _, IntoElement, KeyBinding, ListAlignment, ListOffset,
    ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    Pixels, Render, Rgba, ScrollWheelEvent, SharedString, Styled as _, Subscription, WeakEntity,
    WeakFocusHandle, Window, actions, anchored, canvas, div, list, prelude::FluentBuilder as _, px,
};

use crate::{
    TextInput, TextInputEvent, TextInputTabBehavior, TextInputVariant,
    button::{Button, ButtonSize, ButtonVariant, IconButton},
    menu::{Menu, MenuActivation, MenuEntry, MenuSize},
    overlay_scrollbar::{OverlayScrollbar, OverlayScrollbarEvent, ScrollMetrics},
};

const KEY_CONTEXT: &str = "SpaceTermCommandPalette";

/// Every footer control shares one size so their labels sit on one baseline and one inset.
const FOOTER_CONTROL_SIZE: ButtonSize = ButtonSize::Small;

actions!(
    spaceterm_command_palette,
    [
        MoveUp,
        MoveDown,
        MovePageUp,
        MovePageDown,
        Activate,
        Confirm,
        Dismiss,
        FocusNext,
        FocusPrevious
    ]
);

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", MoveUp, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-p", MoveUp, Some(KEY_CONTEXT)),
        KeyBinding::new("down", MoveDown, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-n", MoveDown, Some(KEY_CONTEXT)),
        KeyBinding::new("pageup", MovePageUp, Some(KEY_CONTEXT)),
        KeyBinding::new("pagedown", MovePageDown, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-m", Activate, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-enter", Confirm, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-.", Dismiss, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-g", Dismiss, Some(KEY_CONTEXT)),
        KeyBinding::new("tab", FocusNext, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-tab", FocusPrevious, Some(KEY_CONTEXT)),
    ]);
}

/// The input path that activated a command-palette item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandPaletteActivationSource {
    /// Return activated the current item.
    Keyboard,
    /// A primary pointer press and release activated one row.
    Pointer,
}

/// A typed command-palette activation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandPaletteActivation<I> {
    item_id: I,
    source: CommandPaletteActivationSource,
}

impl<I> CommandPaletteActivation<I> {
    /// Returns the caller-owned item identity.
    pub fn item_id(&self) -> &I {
        &self.item_id
    }

    /// Returns the input path that activated the item.
    pub fn source(&self) -> CommandPaletteActivationSource {
        self.source
    }

    /// Consumes the activation and returns its caller-owned item identity.
    pub fn into_item_id(self) -> I {
        self.item_id
    }
}

/// Why an open command palette closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandPaletteCloseReason {
    /// An enabled item was activated.
    Activated,
    /// Escape dismissed the palette.
    Escape,
    /// A pointer press outside the panel dismissed the palette.
    Outside,
    /// Focus moved away from the palette editor.
    FocusLost,
    /// The operating-system window deactivated.
    Deactivated,
    /// The owner explicitly dismissed the palette.
    Programmatic,
    /// The owner replaced this palette with another transient UI owner.
    Replaced,
    /// The owner completed its operation and takes focus itself.
    Completed,
}

impl CommandPaletteCloseReason {
    const fn restores_focus(self) -> bool {
        matches!(
            self,
            Self::Activated | Self::Escape | Self::Outside | Self::Programmatic
        )
    }
}

/// One exact command-palette lifecycle transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandPaletteLifecycleEvent {
    /// The palette became open.
    Opened,
    /// The palette became closed for the supplied reason.
    Closed(CommandPaletteCloseReason),
}

/// The original focus owner transferred through a command-palette replacement chain.
pub struct CommandPaletteReplacementFocus {
    restore_focus: Option<WeakFocusHandle>,
}

/// Monotonic identity for the current command-palette query.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct CommandPaletteGeneration(u64);

impl CommandPaletteGeneration {
    /// Returns the opaque generation as a diagnostic integer.
    pub fn value(self) -> u64 {
        self.0
    }
}

/// A query snapshot that callers may use to feed asynchronous results back to the palette.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandPaletteQuery {
    text: String,
    generation: CommandPaletteGeneration,
}

impl CommandPaletteQuery {
    /// Returns the complete single-line query.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the generation that results must match.
    pub fn generation(&self) -> CommandPaletteGeneration {
        self.generation
    }
}

/// Typed events emitted by a [`CommandPalette`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandPaletteEvent<I> {
    /// The palette opened or closed.
    Lifecycle(CommandPaletteLifecycleEvent),
    /// An enabled semantic item was activated.
    Activated(CommandPaletteActivation<I>),
    /// The query changed or a refresh was explicitly requested.
    QueryChanged(CommandPaletteQuery),
    /// A search-line control was activated.
    HeaderAction(SharedString),
    /// A footer actions-menu entry was activated.
    MenuAction(SharedString),
    /// The footer confirm control was activated by pointer or by its keyboard equivalent.
    Confirmed,
}

/// A standardized trailing row accessory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandPaletteAccessory {
    /// Secondary explanatory text.
    Text(SharedString),
    /// A display-only keyboard shortcut.
    Shortcut(SharedString),
    /// A compact status label.
    Status(SharedString),
    /// A selected or completed checkmark.
    Checkmark,
}

type IconBuilder = Rc<dyn Fn(Rgba) -> AnyElement>;

/// One control rendered at the trailing edge of the command-palette search line.
///
/// The caller owns the icon and the identity it receives back through
/// [`CommandPaletteEvent::HeaderAction`]; the palette owns the control's size and paint.
#[derive(Clone)]
pub struct CommandPaletteAction {
    id: SharedString,
    accessibility_name: SharedString,
    icon: IconBuilder,
    disabled: bool,
    debug_selector: Option<String>,
}

impl CommandPaletteAction {
    /// Creates an enabled search-line control with a mandatory logical accessibility name.
    pub fn new(
        id: impl Into<SharedString>,
        accessibility_name: impl Into<SharedString>,
        icon: impl Fn(Rgba) -> AnyElement + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            accessibility_name: accessibility_name.into(),
            icon: Rc::new(icon),
            disabled: false,
            debug_selector: None,
        }
    }

    /// Controls whether the control can activate.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Adds a stable selector used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    /// Returns the caller-owned identity reported on activation.
    pub fn id(&self) -> &SharedString {
        &self.id
    }
}

/// One footer hint pairing a key presentation with the action it performs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandPaletteHint {
    label: SharedString,
    key: SharedString,
}

impl CommandPaletteHint {
    /// Creates a hint such as `Open` paired with `\u{23ce}`.
    pub fn new(label: impl Into<SharedString>, key: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            key: key.into(),
        }
    }
}

/// Who decides which items a query presents.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CommandPaletteMatching {
    /// The palette filters and ranks items with its own static semantic matcher.
    #[default]
    Semantic,
    /// The caller supplies exactly the items to present, already filtered and ordered.
    ///
    /// The palette presents every item in caller order and highlights nothing, because the query
    /// is not a substring of the labels it produces. Callers whose query is an address rather than
    /// a search term, such as a filesystem path, select this.
    Caller,
}

/// What activating an item does to the palette.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CommandPaletteActivationPolicy {
    /// Activation closes the palette, because the item completed the caller's operation.
    #[default]
    Close,
    /// Activation keeps the palette open, because the item narrows the caller's next results.
    ///
    /// Drill-down callers, such as a filesystem navigator whose rows descend a level, select this
    /// and answer the activation by publishing a new query and new items.
    Continue,
}

/// The single primary footer control and its keyboard equivalent.
///
/// It is primary by placement and by owning the confirm key, not by weight: the palette renders it
/// as low-emphasis text so the result list stays the loudest thing on the surface.
///
/// The caller owns the label, the enabled state, and the identity of the operation; the palette
/// owns the control's size, paint, placement, and shortcut presentation. Activating it, by pointer
/// or by the palette's confirm key, emits [`CommandPaletteEvent::Confirmed`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandPaletteConfirm {
    label: SharedString,
    shortcut: SharedString,
    disabled: bool,
    debug_selector: Option<String>,
}

impl CommandPaletteConfirm {
    /// Creates an enabled confirm control labelled for the operation it performs.
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            shortcut: "\u{2318}\u{21a9}".into(),
            disabled: false,
            debug_selector: None,
        }
    }

    /// Controls whether the control can activate. A disabled confirm ignores its keyboard
    /// equivalent as well as pointer activation.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Adds a stable selector used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    /// Returns the caller-owned label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns whether the control is disabled.
    pub const fn is_disabled(&self) -> bool {
        self.disabled
    }
}

/// One typed semantic command-palette item.
///
/// Item identities must remain stable. Later items with a duplicate identity are discarded so
/// selection and pointer ownership always refer to exactly one row.
#[derive(Clone)]
pub struct CommandPaletteItem<I> {
    id: I,
    label: SharedString,
    description: Option<SharedString>,
    section: Option<SharedString>,
    keywords: Vec<SharedString>,
    disabled: bool,
    leading_icon: Option<IconBuilder>,
    trailing: Option<CommandPaletteAccessory>,
    debug_selector: Option<String>,
}

impl<I> CommandPaletteItem<I> {
    /// Creates an enabled item. The label is also its logical accessibility name.
    pub fn new(id: I, label: impl Into<SharedString>) -> Self {
        Self {
            id,
            label: label.into(),
            description: None,
            section: None,
            keywords: Vec::new(),
            disabled: false,
            leading_icon: None,
            trailing: None,
            debug_selector: None,
        }
    }

    /// Adds one line of secondary descriptive text.
    pub fn description(mut self, value: impl Into<SharedString>) -> Self {
        self.description = Some(value.into());
        self
    }

    /// Groups the item under a heading shown when the section changes between adjacent rows.
    ///
    /// Items are grouped in provider order, so a caller that wants one heading per section must
    /// supply that section's items contiguously.
    pub fn section(mut self, value: impl Into<SharedString>) -> Self {
        self.section = Some(value.into());
        self
    }

    /// Replaces the non-presentational search keywords.
    pub fn keywords(mut self, values: impl IntoIterator<Item = impl Into<SharedString>>) -> Self {
        self.keywords = values.into_iter().map(Into::into).collect();
        self
    }

    /// Controls whether navigation and activation may reach this item.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Adds a bounded leading icon built with the resolved row foreground color.
    pub fn leading_icon(mut self, build: impl Fn(Rgba) -> AnyElement + 'static) -> Self {
        self.leading_icon = Some(Rc::new(build));
        self
    }

    /// Adds one standardized trailing accessory.
    pub fn trailing(mut self, accessory: CommandPaletteAccessory) -> Self {
        self.trailing = Some(accessory);
        self
    }

    /// Adds a stable selector used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    /// Returns the caller-owned identity.
    pub fn id(&self) -> &I {
        &self.id
    }

    /// Returns the primary label.
    pub fn label(&self) -> &str {
        self.label.as_ref()
    }

    /// Returns the optional description.
    pub fn description_text(&self) -> Option<&str> {
        self.description.as_ref().map(AsRef::as_ref)
    }

    /// Returns the optional grouping section.
    pub fn section_text(&self) -> Option<&str> {
        self.section.as_ref().map(AsRef::as_ref)
    }

    /// Returns whether navigation and activation skip this item.
    pub fn is_disabled(&self) -> bool {
        self.disabled
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommandPaletteMatch {
    item_index: usize,
    score: i64,
    label_highlights: Vec<Range<usize>>,
    description_highlights: Vec<Range<usize>>,
}

fn match_command_palette_items<I>(
    items: &[CommandPaletteItem<I>],
    query: &str,
    matching: CommandPaletteMatching,
) -> Vec<CommandPaletteMatch> {
    let tokens: Vec<Vec<char>> = match matching {
        CommandPaletteMatching::Caller => Vec::new(),
        CommandPaletteMatching::Semantic => query
            .split_whitespace()
            .map(lowercase_chars)
            .filter(|token| !token.is_empty())
            .collect(),
    };
    if tokens.is_empty() {
        return items
            .iter()
            .enumerate()
            .map(|(item_index, _)| CommandPaletteMatch {
                item_index,
                score: 0,
                label_highlights: Vec::new(),
                description_highlights: Vec::new(),
            })
            .collect();
    }

    let mut section_groups = vec![0usize; items.len()];
    for item_index in 1..items.len() {
        section_groups[item_index] = section_groups[item_index - 1]
            + usize::from(items[item_index].section != items[item_index - 1].section);
    }
    let mut matches = items
        .iter()
        .enumerate()
        .filter_map(|(item_index, item)| {
            match_item(item, &tokens).map(|(score, label_highlights, description_highlights)| {
                CommandPaletteMatch {
                    item_index,
                    score,
                    label_highlights,
                    description_highlights,
                }
            })
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        section_groups[left.item_index]
            .cmp(&section_groups[right.item_index])
            .then_with(|| right.score.cmp(&left.score))
            .then_with(|| left.item_index.cmp(&right.item_index))
    });
    matches
}

fn lowercase_chars(text: &str) -> Vec<char> {
    text.chars().flat_map(char::to_lowercase).collect()
}

#[derive(Clone)]
struct SearchUnit {
    character: char,
    source: Range<usize>,
}

fn search_units(text: &str) -> Vec<SearchUnit> {
    let mut units = Vec::new();
    for (start, character) in text.char_indices() {
        let source = start..start + character.len_utf8();
        units.extend(character.to_lowercase().map(|character| SearchUnit {
            character,
            source: source.clone(),
        }));
    }
    units
}

type ItemMatch = (i64, Vec<Range<usize>>, Vec<Range<usize>>);

fn match_item<I>(item: &CommandPaletteItem<I>, tokens: &[Vec<char>]) -> Option<ItemMatch> {
    let label_units = search_units(item.label.as_ref());
    let description_units = item.description.as_ref().map(|text| search_units(text));
    let keyword_units: Vec<_> = item
        .keywords
        .iter()
        .map(|keyword| search_units(keyword))
        .collect();
    let mut score = 0;
    let mut label_highlights = Vec::new();
    let mut description_highlights = Vec::new();

    for token in tokens {
        let mut best =
            fuzzy_match(&label_units, token).map(|matched| (matched.score + 20_000, 0, matched));
        if let Some(units) = &description_units
            && let Some(matched) = fuzzy_match(units, token)
        {
            let candidate = (matched.score + 400, 1, matched);
            if best.as_ref().is_none_or(|current| candidate.0 > current.0) {
                best = Some(candidate);
            }
        }
        for units in &keyword_units {
            if let Some(matched) = fuzzy_match(units, token) {
                let candidate = (matched.score + 200, 2, matched);
                if best.as_ref().is_none_or(|current| candidate.0 > current.0) {
                    best = Some(candidate);
                }
            }
        }
        let (token_score, field, matched) = best?;
        score += token_score;
        match field {
            0 => label_highlights.extend(matched.ranges),
            1 => description_highlights.extend(matched.ranges),
            _ => {}
        }
    }

    Some((
        score,
        merge_ranges(label_highlights),
        merge_ranges(description_highlights),
    ))
}

struct FuzzyMatch {
    score: i64,
    ranges: Vec<Range<usize>>,
}

fn fuzzy_match(target: &[SearchUnit], query: &[char]) -> Option<FuzzyMatch> {
    if query.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            ranges: Vec::new(),
        });
    }
    let mut best: Option<(i64, Vec<usize>)> = None;
    for start in target
        .iter()
        .enumerate()
        .filter_map(|(index, unit)| (unit.character == query[0]).then_some(index))
    {
        let mut indexes = vec![start];
        let mut cursor = start + 1;
        let mut complete = true;
        for query_character in &query[1..] {
            let Some(relative) = target[cursor..]
                .iter()
                .position(|unit| unit.character == *query_character)
            else {
                complete = false;
                break;
            };
            cursor += relative;
            indexes.push(cursor);
            cursor += 1;
        }
        if !complete {
            continue;
        }
        let end = indexes.last().copied().unwrap_or(start);
        let gaps = end + 1 - start - indexes.len();
        let contiguous_pairs = indexes
            .windows(2)
            .filter(|pair| pair[1] == pair[0] + 1)
            .count();
        let whole = indexes.len() == target.len() && start == 0;
        let prefix = start == 0;
        let rank = 1_000
            + i64::from(whole) * 8_000
            + i64::from(prefix) * 3_000
            + contiguous_pairs as i64 * 80
            - gaps as i64 * 25
            - start as i64 * 4;
        if best.as_ref().is_none_or(|current| rank > current.0) {
            best = Some((rank, indexes));
        }
    }
    let (score, indexes) = best?;
    let ranges = indexes
        .into_iter()
        .filter_map(|index| target.get(index).map(|unit| unit.source.clone()))
        .collect();
    Some(FuzzyMatch {
        score,
        ranges: merge_ranges(ranges),
    })
}

fn merge_ranges(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
            continue;
        }
        merged.push(range);
    }
    merged
}

/// Application-owned command-palette paint values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CommandPalettePaint {
    background: Rgba,
    border: Rgba,
    separator: Rgba,
    foreground: Rgba,
    muted: Rgba,
    disabled: Rgba,
    hover_background: Rgba,
    selected_background: Rgba,
    selected_foreground: Rgba,
    match_foreground: Rgba,
    section_foreground: Rgba,
    footer_foreground: Rgba,
    footer_key_foreground: Rgba,
}

impl CommandPalettePaint {
    /// Creates the core paint catalog.
    ///
    /// Separator, hover, section, and footer colors default to the closest core value so a caller
    /// only overrides what its theme distinguishes.
    #[expect(
        clippy::too_many_arguments,
        reason = "the bounded paint catalog is clearer than nested untyped color groups"
    )]
    pub fn new(
        background: Rgba,
        border: Rgba,
        foreground: Rgba,
        muted: Rgba,
        disabled: Rgba,
        selected_background: Rgba,
        selected_foreground: Rgba,
        match_foreground: Rgba,
    ) -> Self {
        Self {
            background,
            border,
            separator: border,
            foreground,
            muted,
            disabled,
            hover_background: selected_background,
            selected_background,
            selected_foreground,
            match_foreground,
            section_foreground: muted,
            footer_foreground: muted,
            footer_key_foreground: disabled,
        }
    }

    /// Sets the hairline color used under the editor, above the footer, and between sections.
    pub fn separator(mut self, color: Rgba) -> Self {
        self.separator = color;
        self
    }

    /// Sets the pointer-hover row background, which stays distinct from the selected background.
    pub fn hover_background(mut self, color: Rgba) -> Self {
        self.hover_background = color;
        self
    }

    /// Sets the section heading foreground.
    pub fn section_foreground(mut self, color: Rgba) -> Self {
        self.section_foreground = color;
        self
    }

    /// Sets the footer hint label and key foregrounds.
    pub fn footer(mut self, foreground: Rgba, key_foreground: Rgba) -> Self {
        self.footer_foreground = foreground;
        self.footer_key_foreground = key_foreground;
        self
    }
}

/// Native desktop dimensions for the command-palette panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CommandPaletteMetrics {
    panel_width: Pixels,
    maximum_height: Pixels,
    top_offset: Pixels,
    viewport_margin: Pixels,
    panel_padding: Pixels,
    input_height: Pixels,
    row_height: Pixels,
    single_line_row_height: Pixels,
    row_line_gap: Pixels,
    section_height: Pixels,
    separator_height: Pixels,
    footer_height: Pixels,
    footer_padding: Option<Pixels>,
    horizontal_padding: Pixels,
    leading_width: Pixels,
    gap: Pixels,
    corner_radius: Pixels,
    border_width: Pixels,
    input_size: Pixels,
    label_size: Pixels,
    secondary_size: Pixels,
    accessory_padding: Pixels,
    accessory_line_padding: Pixels,
    accessory_radius: Pixels,
}

impl CommandPaletteMetrics {
    /// Creates compact native defaults around a panel width and row height.
    pub fn new(panel_width: Pixels, row_height: Pixels) -> Self {
        Self {
            panel_width,
            maximum_height: px(480.0),
            top_offset: px(52.0),
            viewport_margin: px(16.0),
            panel_padding: px(4.0),
            input_height: px(42.0),
            row_height,
            single_line_row_height: row_height,
            row_line_gap: px(2.0),
            section_height: px(22.0),
            separator_height: px(9.0),
            footer_height: px(30.0),
            footer_padding: None,
            horizontal_padding: px(12.0),
            leading_width: px(18.0),
            gap: px(10.0),
            corner_radius: px(8.0),
            border_width: px(1.0),
            input_size: px(14.0),
            label_size: px(13.0),
            secondary_size: px(11.0),
            accessory_padding: px(5.0),
            accessory_line_padding: px(2.0),
            accessory_radius: px(4.0),
        }
    }

    /// Sets the footer's horizontal padding, overriding the panel's content inset.
    ///
    /// Footer controls are text with their own padding, so their boxes sitting on the content
    /// edges puts their labels inside those edges. A caller that wants the labels to line up with
    /// the editor and the rows sets a smaller footer padding. Defaults to the content inset, which
    /// aligns the control boxes instead.
    pub fn footer_padding(mut self, padding: Pixels) -> Self {
        self.footer_padding = Some(padding);
        self
    }

    /// Sets the height of a row that carries no description.
    ///
    /// A row height sized for a label above a description leaves a single-line row mostly empty,
    /// so callers whose items are all one line give that row its own height. Defaults to the
    /// ordinary row height, which keeps mixed result sets uniform.
    pub fn single_line_row_height(mut self, height: Pixels) -> Self {
        self.single_line_row_height = height;
        self
    }

    /// Sets the maximum panel height and top offset.
    pub fn panel_geometry(mut self, maximum_height: Pixels, top_offset: Pixels) -> Self {
        self.maximum_height = maximum_height;
        self.top_offset = top_offset;
        self
    }

    /// Sets the minimum panel distance from viewport edges.
    pub fn viewport_margin(mut self, margin: Pixels) -> Self {
        self.viewport_margin = margin;
        self
    }

    /// Sets panel padding and the editor height.
    pub fn panel_spacing(mut self, padding: Pixels, input_height: Pixels) -> Self {
        self.panel_padding = padding;
        self.input_height = input_height;
        self
    }

    /// Sets row padding, leading-slot width, and column gap.
    pub fn row_spacing(
        mut self,
        horizontal_padding: Pixels,
        leading_width: Pixels,
        gap: Pixels,
    ) -> Self {
        self.horizontal_padding = horizontal_padding;
        self.leading_width = leading_width;
        self.gap = gap;
        self
    }

    /// Sets the gap between a row's label line and its description line.
    pub fn row_line_gap(mut self, gap: Pixels) -> Self {
        self.row_line_gap = gap;
        self
    }

    /// Sets section heading and section separator heights.
    pub fn section_spacing(mut self, section_height: Pixels, separator_height: Pixels) -> Self {
        self.section_height = section_height;
        self.separator_height = separator_height;
        self
    }

    /// Sets the hint and actions footer height.
    pub fn footer_height(mut self, height: Pixels) -> Self {
        self.footer_height = height;
        self
    }

    /// Sets panel corner radius and stable border width.
    pub fn panel_shape(mut self, corner_radius: Pixels, border_width: Pixels) -> Self {
        self.corner_radius = corner_radius;
        self.border_width = border_width;
        self
    }

    /// Sets editor, primary, and secondary font sizes.
    pub fn font_sizes(mut self, input: Pixels, label: Pixels, secondary: Pixels) -> Self {
        self.input_size = input;
        self.label_size = label;
        self.secondary_size = secondary;
        self
    }

    /// Sets the padded status accessory shape.
    pub fn accessory_shape(
        mut self,
        padding: Pixels,
        line_padding: Pixels,
        radius: Pixels,
    ) -> Self {
        self.accessory_padding = padding;
        self.accessory_line_padding = line_padding;
        self.accessory_radius = radius;
        self
    }

    /// Returns the shared left edge of the editor, headings, status text, and row content.
    fn content_leading_inset(&self) -> Pixels {
        self.panel_padding + self.horizontal_padding
    }

    fn footer_inset(&self) -> Pixels {
        self.footer_padding
            .unwrap_or_else(|| self.content_leading_inset())
    }

    /// Returns the concentric radius for an inset row inside the outer panel.
    fn row_corner_radius(&self) -> Pixels {
        (self.corner_radius - self.panel_padding).max(px(0.0))
    }
}

/// Application-owned presentation installed once for every command palette.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CommandPaletteTheme {
    paint: CommandPalettePaint,
    metrics: CommandPaletteMetrics,
}

impl CommandPaletteTheme {
    /// Creates a complete command-palette theme.
    pub fn new(paint: CommandPalettePaint, metrics: CommandPaletteMetrics) -> Self {
        Self { paint, metrics }
    }
}

impl Global for CommandPaletteTheme {}

/// A reusable entity-backed command palette with typed semantic items.
///
/// Its [`TextInput`] supplies native editable-text semantics. GPUI 0.2.2 cannot yet publish
/// listbox and option roles for ordinary elements, so the API requires logical row labels and
/// keeps arbitrary row painting outside the accessibility seam.
pub struct CommandPalette<I: Clone + Eq + 'static> {
    no_results_text: SharedString,
    items: Rc<[CommandPaletteItem<I>]>,
    matches: Rc<[CommandPaletteMatch]>,
    presented_results: Rc<PresentedResults>,
    leading_reserved: bool,
    header_actions: Vec<CommandPaletteAction>,
    hints: Vec<CommandPaletteHint>,
    actions_menu: Vec<MenuEntry<SharedString>>,
    actions_menu_label: SharedString,
    confirm: Option<CommandPaletteConfirm>,
    matching: CommandPaletteMatching,
    activation: CommandPaletteActivationPolicy,
    selected: Option<I>,
    preferred: Option<I>,
    query: String,
    generation: CommandPaletteGeneration,
    loading: bool,
    open: bool,
    input: Entity<TextInput>,
    focus_scope: gpui::FocusHandle,
    scrollbar: Entity<OverlayScrollbar<f32>>,
    restore_focus: Option<WeakFocusHandle>,
    restore_on_activation: Option<WeakFocusHandle>,
    pointer_press: Option<I>,
    pointer_suppressed: bool,
    hover_suppressed: bool,
    pointer_anchor: gpui::Point<Pixels>,
    list: ListState,
    scrollbar_reveal_pending: bool,
    selection_reveal_pending: bool,
    _input_subscription: Subscription,
    _focus_subscription: Subscription,
    _scrollbar_subscription: Subscription,
}

mod presented_results {
    use gpui::{Pixels, SharedString, px};

    use super::{CommandPaletteItem, CommandPaletteMatch, CommandPaletteMetrics};

    /// One presented list row. Section headings and separators are derived, never caller-painted.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(super) enum PaletteRow {
        Section(SharedString),
        Separator,
        Item { position: usize, single_line: bool },
    }

    impl PaletteRow {
        pub(super) fn height(&self, metrics: CommandPaletteMetrics) -> Pixels {
            match self {
                Self::Section(_) => metrics.section_height,
                Self::Separator => metrics.separator_height,
                Self::Item {
                    single_line: true, ..
                } => metrics.single_line_row_height,
                Self::Item { .. } => metrics.row_height,
            }
        }

        pub(super) const fn item_position(&self) -> Option<usize> {
            match self {
                Self::Item { position, .. } => Some(*position),
                Self::Section(_) | Self::Separator => None,
            }
        }
    }

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub(super) struct PresentedResults {
        rows: Vec<PaletteRow>,
    }

    impl PresentedResults {
        pub(super) fn new<I>(
            items: &[CommandPaletteItem<I>],
            matches: &[CommandPaletteMatch],
        ) -> Self {
            let mut rows = Vec::with_capacity(matches.len());
            let mut current: Option<SharedString> = None;
            let mut started = false;
            for (position, matched) in matches.iter().enumerate() {
                let Some(item) = items.get(matched.item_index) else {
                    continue;
                };
                if !started || item.section != current {
                    if started {
                        rows.push(PaletteRow::Separator);
                    }
                    if let Some(section) = item.section.clone() {
                        rows.push(PaletteRow::Section(section));
                    }
                    current = item.section.clone();
                }
                started = true;
                rows.push(PaletteRow::Item {
                    position,
                    single_line: item.description.is_none(),
                });
            }
            Self { rows }
        }

        pub(super) fn len(&self) -> usize {
            self.rows.len()
        }

        #[cfg(test)]
        pub(super) fn rows(&self) -> &[PaletteRow] {
            &self.rows
        }

        pub(super) fn row(&self, index: usize) -> Option<&PaletteRow> {
            self.rows.get(index)
        }

        pub(super) fn total_height(&self, metrics: CommandPaletteMetrics) -> Pixels {
            self.rows
                .iter()
                .fold(px(0.0), |height, row| height + row.height(metrics))
        }

        pub(super) fn list_index_for_match(&self, position: usize) -> Option<usize> {
            self.rows
                .iter()
                .position(|row| row.item_position() == Some(position))
        }

        #[cfg(test)]
        pub(super) fn row_at_y(
            &self,
            content_y: Pixels,
            metrics: CommandPaletteMetrics,
        ) -> Option<(usize, &PaletteRow)> {
            if content_y < px(0.0) {
                return None;
            }
            let mut row_top = px(0.0);
            self.rows.iter().enumerate().find(|(_, row)| {
                let row_bottom = row_top + row.height(metrics);
                let contains = content_y >= row_top && content_y < row_bottom;
                row_top = row_bottom;
                contains
            })
        }

        #[cfg(test)]
        pub(super) fn item_at_y(
            &self,
            content_y: Pixels,
            metrics: CommandPaletteMetrics,
        ) -> Option<usize> {
            self.row_at_y(content_y, metrics)?.1.item_position()
        }

        pub(super) fn page_target(
            &self,
            current: Option<usize>,
            enabled: &[usize],
            viewport_height: Pixels,
            direction: isize,
            metrics: CommandPaletteMetrics,
        ) -> Option<usize> {
            let edge = if direction < 0 {
                *enabled.last()?
            } else {
                *enabled.first()?
            };
            let current = current
                .filter(|position| enabled.contains(position))
                .unwrap_or(edge);
            let current_enabled_index = enabled.iter().position(|position| *position == current)?;
            let current_top = self.item_top(current, metrics)?;
            let target_y = if direction < 0 {
                (current_top - viewport_height).max(px(0.0))
            } else {
                current_top + viewport_height.max(px(0.0))
            };

            if direction < 0 {
                let candidates = &enabled[..current_enabled_index];
                candidates
                    .iter()
                    .copied()
                    .find(|position| {
                        self.item_top(*position, metrics)
                            .is_some_and(|top| top >= target_y)
                    })
                    .or_else(|| candidates.last().copied())
                    .or(Some(current))
            } else {
                let candidates = &enabled[current_enabled_index + 1..];
                candidates
                    .iter()
                    .copied()
                    .take_while(|position| {
                        self.item_top(*position, metrics)
                            .is_some_and(|top| top <= target_y)
                    })
                    .last()
                    .or_else(|| candidates.first().copied())
                    .or(Some(current))
            }
        }

        fn item_top(&self, position: usize, metrics: CommandPaletteMetrics) -> Option<Pixels> {
            let mut top = px(0.0);
            for row in &self.rows {
                if row.item_position() == Some(position) {
                    return Some(top);
                }
                top += row.height(metrics);
            }
            None
        }
    }
}

use presented_results::{PaletteRow, PresentedResults};

impl<I: Clone + Eq + 'static> EventEmitter<CommandPaletteEvent<I>> for CommandPalette<I> {}

impl<I: Clone + Eq + 'static> CommandPalette<I> {
    /// Creates a closed palette with static items and a reusable [`TextInput`] editor.
    pub fn new(
        placeholder: impl Into<SharedString>,
        items: Vec<CommandPaletteItem<I>>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let placeholder = placeholder.into();
        let input_placeholder = placeholder.clone();
        let input = cx.new(|cx| {
            TextInput::new(
                "command-palette-input",
                "Command palette query",
                "",
                window,
                cx,
            )
            .placeholder(input_placeholder)
            .variant(TextInputVariant::Bare)
            .tab_behavior(TextInputTabBehavior::Propagate)
            .debug_selector("command-palette-input")
        });
        let input_subscription = cx.subscribe_in(
            &input,
            window,
            |palette, input, event: &TextInputEvent, window, cx| match event {
                TextInputEvent::ValueChanged(_) => {
                    palette.update_query(input.read(cx).value().to_owned(), cx);
                }
                TextInputEvent::Submitted => {
                    palette.activate_selected(CommandPaletteActivationSource::Keyboard, window, cx)
                }
                TextInputEvent::Cancelled => {
                    palette.close(CommandPaletteCloseReason::Escape, window, cx);
                }
                TextInputEvent::TabForwardRequested => {
                    palette.focus_next_control(window, cx);
                }
                TextInputEvent::TabBackwardRequested => {
                    palette.focus_previous_control(window, cx);
                }
                _ => {}
            },
        );
        let focus_scope = cx.focus_handle();
        let focus_subscription = cx.on_focus_out(&focus_scope, window, |palette, _, window, cx| {
            if palette.open && !crate::menu::window_menu_is_open(window, cx) {
                palette.close(CommandPaletteCloseReason::FocusLost, window, cx);
            }
        });
        let scrollbar = cx.new(|_| OverlayScrollbar::<f32>::new("command-palette-scrollbar"));
        let scrollbar_subscription = cx.subscribe_in(
            &scrollbar,
            window,
            |palette, _, event: &OverlayScrollbarEvent<f32>, _, cx| match event {
                OverlayScrollbarEvent::InteractionStarted => palette.list.scrollbar_drag_started(),
                OverlayScrollbarEvent::OffsetRequested(offset) => {
                    palette
                        .list
                        .set_offset_from_scrollbar(gpui::point(px(0.0), px(-*offset)));
                    cx.notify();
                }
            },
        );
        cx.observe_window_activation(window, |palette, window, cx| {
            if palette.open && !window.is_window_active() {
                palette.close(CommandPaletteCloseReason::Deactivated, window, cx);
            } else if !palette.open
                && window.is_window_active()
                && let Some(focus) = palette
                    .restore_on_activation
                    .take()
                    .and_then(|focus| focus.upgrade())
            {
                focus.focus(window);
            }
        })
        .detach();
        let window_id = window.window_handle().window_id();
        cx.on_release(move |_, cx| {
            crate::tooltip::set_window_tooltip_suppression(
                window_id,
                crate::tooltip::TooltipSuppression::CommandPalette,
                false,
                cx,
            );
        })
        .detach();
        let items: Rc<[CommandPaletteItem<I>]> = unique_items(items).into();
        let matches: Rc<[CommandPaletteMatch]> =
            match_command_palette_items(&items, "", CommandPaletteMatching::Semantic).into();
        let selected = first_enabled_id(&items, &matches);
        let presented_results = Rc::new(PresentedResults::new(&items, &matches));
        let leading_reserved = items.iter().any(|item| item.leading_icon.is_some());
        let list =
            ListState::new(presented_results.len(), ListAlignment::Top, px(0.0)).measure_all();
        let mut palette = Self {
            no_results_text: "No matching items".into(),
            items,
            matches,
            presented_results,
            leading_reserved,
            header_actions: Vec::new(),
            hints: Vec::new(),
            actions_menu: Vec::new(),
            actions_menu_label: "Actions\u{2026}".into(),
            confirm: None,
            matching: CommandPaletteMatching::Semantic,
            activation: CommandPaletteActivationPolicy::Close,
            selected,
            preferred: None,
            query: String::new(),
            generation: CommandPaletteGeneration::default(),
            loading: false,
            open: false,
            input,
            focus_scope,
            scrollbar,
            restore_focus: None,
            restore_on_activation: None,
            pointer_press: None,
            pointer_suppressed: false,
            hover_suppressed: false,
            pointer_anchor: gpui::point(px(0.0), px(0.0)),
            list,
            scrollbar_reveal_pending: false,
            selection_reveal_pending: false,
            _input_subscription: input_subscription,
            _focus_subscription: focus_subscription,
            _scrollbar_subscription: scrollbar_subscription,
        };
        palette.install_scroll_handler(cx);
        palette
    }

    fn install_scroll_handler(&mut self, cx: &mut gpui::Context<Self>) {
        let palette = cx.entity().downgrade();
        // GPUI dispatches this handler while it holds the list state's mutable borrow, so the
        // handler must not read that state. It only records the request; the next render reads
        // the scroll geometry and reveals the scrollbar.
        self.list.set_scroll_handler(move |_, _, cx| {
            let _ = palette.update(cx, |palette, cx| {
                palette.scrollbar_reveal_pending = true;
                cx.notify();
            });
        });
    }

    /// Sets the identity preferred when no stable enabled selection remains.
    pub fn set_preferred_item(&mut self, id: Option<I>, cx: &mut gpui::Context<Self>) {
        self.preferred = id;
        self.repair_selection();
        cx.notify();
    }

    /// Replaces the no-results message.
    pub fn set_no_results_text(
        &mut self,
        text: impl Into<SharedString>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.no_results_text = text.into();
        cx.notify();
    }

    /// Replaces the controls rendered at the trailing edge of the search line.
    ///
    /// Activating one emits [`CommandPaletteEvent::HeaderAction`] with the caller's identity.
    pub fn set_header_actions(
        &mut self,
        actions: Vec<CommandPaletteAction>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.header_actions = actions;
        cx.notify();
    }

    /// Selects who filters and orders items for the current query.
    ///
    /// Switching modes recomputes the presented results immediately and repairs selection by
    /// stable identity.
    pub fn set_matching(&mut self, matching: CommandPaletteMatching, cx: &mut gpui::Context<Self>) {
        if self.matching == matching {
            return;
        }
        self.matching = matching;
        self.recompute_matches();
        cx.notify();
    }

    /// Selects whether activating an item closes the palette or keeps it open.
    pub fn set_activation(
        &mut self,
        activation: CommandPaletteActivationPolicy,
        cx: &mut gpui::Context<Self>,
    ) {
        self.activation = activation;
        cx.notify();
    }

    /// Replaces the single primary footer control. `None` removes it.
    ///
    /// An installed confirm claims the palette's confirm key; without one that key is left for the
    /// surrounding application.
    pub fn set_confirm(
        &mut self,
        confirm: Option<CommandPaletteConfirm>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.confirm = confirm;
        cx.notify();
    }

    /// Replaces the footer hints. An empty list removes the footer unless an actions menu remains.
    pub fn set_hints(&mut self, hints: Vec<CommandPaletteHint>, cx: &mut gpui::Context<Self>) {
        self.hints = hints;
        cx.notify();
    }

    /// Replaces the footer actions menu. An empty list removes its trigger.
    ///
    /// Activating an entry emits [`CommandPaletteEvent::MenuAction`]. The palette stays open while
    /// the menu holds focus and closes only once the caller acts on the entry.
    pub fn set_actions_menu(
        &mut self,
        entries: Vec<MenuEntry<SharedString>>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.actions_menu = entries;
        cx.notify();
    }

    /// Replaces the footer actions-menu trigger label, which is also its accessibility name.
    pub fn set_actions_menu_label(
        &mut self,
        label: impl Into<SharedString>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.actions_menu_label = label.into();
        cx.notify();
    }

    /// Opens the palette, captures the exact prior focus owner, and focuses its editor.
    ///
    /// Returns `true` only for an actual closed-to-open transition.
    pub fn open(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> bool {
        self.open_with_replacement(None, window, cx)
    }

    /// Opens the palette as the next owner in a replacement chain.
    ///
    /// The transferred focus owner is restored when this palette later closes normally.
    pub fn open_replacing(
        &mut self,
        replacement: CommandPaletteReplacementFocus,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        self.open_with_replacement(Some(replacement), window, cx)
    }

    fn open_with_replacement(
        &mut self,
        replacement: Option<CommandPaletteReplacementFocus>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if self.open {
            self.input.read(cx).focus_handle().focus(window);
            return false;
        }
        self.restore_on_activation = None;
        let menu_replacement = crate::menu::dismiss_active_menu_for_replacement(window, cx);
        self.restore_focus = match replacement {
            Some(replacement) => replacement.restore_focus,
            None => match menu_replacement {
                Some(crate::menu::MenuReplacementFocus(focus)) => focus,
                None => window.focused(cx).map(|focus| focus.downgrade()),
            },
        };
        self.open = true;
        crate::tooltip::set_window_tooltip_suppression(
            window.window_handle().window_id(),
            crate::tooltip::TooltipSuppression::CommandPalette,
            true,
            cx,
        );
        self.pointer_press = None;
        self.pointer_suppressed = true;
        self.hover_suppressed = true;
        self.pointer_anchor = window.mouse_position();
        self.selected = None;
        if !self.query.is_empty() {
            self.input.update(cx, |input, cx| input.set_value("", cx));
            self.query.clear();
            self.recompute_matches();
        }
        self.repair_selection();
        self.reveal_selected();
        self.selection_reveal_pending = true;
        self.input.read(cx).focus_handle().focus(window);
        cx.emit(CommandPaletteEvent::Lifecycle(
            CommandPaletteLifecycleEvent::Opened,
        ));
        self.request_refresh(cx);
        cx.notify();
        true
    }

    /// Dismisses an open palette programmatically and restores its exact prior focus owner.
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> bool {
        self.close(CommandPaletteCloseReason::Programmatic, window, cx)
    }

    /// Closes an open palette whose owner has completed its operation and moved focus itself.
    ///
    /// Unlike [`Self::dismiss`] this discards the captured prior focus owner, so the palette does
    /// not pull focus back from whatever the completed operation focused.
    pub fn dismiss_without_restoring_focus(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        self.restore_focus = None;
        self.restore_on_activation = None;
        self.close(CommandPaletteCloseReason::Completed, window, cx)
    }

    /// Returns focus to the query editor of an open palette.
    pub fn focus_editor(&self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if self.open {
            self.input.read(cx).focus_handle().focus(window);
        }
    }

    /// Returns whether the query editor currently holds keyboard focus.
    pub fn editor_is_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.input.read(cx).focus_handle().is_focused(window)
    }

    /// Returns whether [`Self::set_query`] would preserve `query` byte-for-byte.
    ///
    /// Callers whose query is an address rather than a search term use this to reject values the
    /// single-line editor would normalize.
    pub fn can_set_query_exactly(&self, query: &str, cx: &gpui::App) -> bool {
        self.input.read(cx).can_set_value_exactly(query)
    }

    /// Closes an open palette before another transient takes focus.
    pub fn dismiss_for_replacement(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Option<CommandPaletteReplacementFocus> {
        let replacement = CommandPaletteReplacementFocus {
            restore_focus: self.restore_focus.clone(),
        };
        self.close(CommandPaletteCloseReason::Replaced, window, cx)
            .then_some(replacement)
    }

    /// Returns whether the transient overlay is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Returns the current editor query.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Returns the current query generation.
    pub fn generation(&self) -> CommandPaletteGeneration {
        self.generation
    }

    /// Returns the selected enabled item identity, if any.
    pub fn selected_item_id(&self) -> Option<&I> {
        self.selected.as_ref()
    }

    /// Replaces items immediately and preserves selection by stable identity when possible.
    pub fn set_items(&mut self, items: Vec<CommandPaletteItem<I>>, cx: &mut gpui::Context<Self>) {
        self.items = unique_items(items).into();
        self.leading_reserved = self.items.iter().any(|item| item.leading_icon.is_some());
        self.loading = false;
        self.recompute_matches();
        cx.notify();
    }

    /// Sets the current loading presentation without changing items or generation.
    pub fn set_loading(&mut self, loading: bool, cx: &mut gpui::Context<Self>) {
        if self.loading != loading {
            self.loading = loading;
            cx.notify();
        }
    }

    /// Requests a refresh for the current query and returns its new generation.
    pub fn refresh(&mut self, cx: &mut gpui::Context<Self>) -> CommandPaletteGeneration {
        self.loading = true;
        let generation = self.request_refresh(cx);
        cx.notify();
        generation
    }

    /// Applies items only when `generation` still describes the current query.
    ///
    /// Returns `false` without changing state for a stale asynchronous result.
    pub fn apply_items(
        &mut self,
        generation: CommandPaletteGeneration,
        items: Vec<CommandPaletteItem<I>>,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.set_items(items, cx);
        true
    }

    /// Sets loading only when `generation` still describes the current query.
    pub fn set_loading_for_generation(
        &mut self,
        generation: CommandPaletteGeneration,
        loading: bool,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.set_loading(loading, cx);
        true
    }

    /// Replaces the editor query and emits one generation-bearing query event.
    pub fn set_query(&mut self, query: impl Into<String>, cx: &mut gpui::Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value(query.into(), cx));
        let accepted_query = self.input.read(cx).value().to_owned();
        self.update_query(accepted_query, cx);
    }

    fn update_query(&mut self, query: String, cx: &mut gpui::Context<Self>) {
        if self.query == query {
            return;
        }
        self.query = query;
        self.loading = false;
        self.recompute_matches();
        self.request_refresh(cx);
        cx.notify();
    }

    fn request_refresh(&mut self, cx: &mut gpui::Context<Self>) -> CommandPaletteGeneration {
        self.generation.0 = self.generation.0.wrapping_add(1);
        let generation = self.generation;
        cx.emit(CommandPaletteEvent::QueryChanged(CommandPaletteQuery {
            text: self.query.clone(),
            generation,
        }));
        generation
    }

    fn recompute_matches(&mut self) {
        self.matches = match_command_palette_items(&self.items, &self.query, self.matching).into();
        self.presented_results = Rc::new(PresentedResults::new(&self.items, &self.matches));
        self.list.reset(self.presented_results.len());
        self.repair_selection();
        self.reveal_selected();
        self.selection_reveal_pending = true;
    }

    fn scrollbar_metrics(&self) -> Option<ScrollMetrics<f32>> {
        let track_height = f32::from(self.list.viewport_bounds().size.height);
        let maximum_offset = f32::from(self.list.max_offset_for_scrollbar().height);
        let offset = f32::from(-self.list.scroll_px_offset_for_scrollbar().y);
        ScrollMetrics::for_pixels(0.0, track_height, maximum_offset, offset)
    }

    fn sync_scrollbar(&self, cx: &mut gpui::Context<Self>) {
        let metrics = self.scrollbar_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.sync(metrics, cx));
    }

    fn reveal_scrollbar(&self, cx: &mut gpui::Context<Self>) {
        let metrics = self.scrollbar_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.reveal(metrics, cx));
    }

    fn repair_selection(&mut self) {
        let stable = self.selected.as_ref().is_some_and(|selected| {
            self.matches.iter().any(|matched| {
                self.items
                    .get(matched.item_index)
                    .is_some_and(|item| !item.disabled && item.id == *selected)
            })
        });
        if stable {
            return;
        }
        self.selected = self.preferred.as_ref().and_then(|preferred| {
            self.matches.iter().find_map(|matched| {
                self.items.get(matched.item_index).and_then(|item| {
                    (!item.disabled && item.id == *preferred).then(|| item.id.clone())
                })
            })
        });
        if self.selected.is_none() {
            self.selected = first_enabled_id(&self.items, &self.matches);
        }
    }

    fn move_selection(&mut self, delta: isize, window: &Window, cx: &mut gpui::Context<Self>) {
        self.suppress_pointer(window.mouse_position(), cx);
        let enabled = self.enabled_match_positions();
        if enabled.is_empty() {
            return;
        }
        let current = self.selected_match_position();
        let next = if delta >= 0 {
            current
                .and_then(|current| enabled.iter().position(|position| *position == current))
                .map_or(0, |position| (position + 1) % enabled.len())
        } else {
            current
                .and_then(|current| enabled.iter().position(|position| *position == current))
                .map_or(enabled.len() - 1, |position| {
                    position.checked_sub(1).unwrap_or(enabled.len() - 1)
                })
        };
        self.select_match_position(enabled[next], cx);
    }

    fn move_page(&mut self, direction: isize, window: &Window, cx: &mut gpui::Context<Self>) {
        self.suppress_pointer(window.mouse_position(), cx);
        let enabled = self.enabled_match_positions();
        if enabled.is_empty() {
            return;
        }
        let metrics = cx.global::<CommandPaletteTheme>().metrics;
        let next = self.presented_results.page_target(
            self.selected_match_position(),
            &enabled,
            self.list.viewport_bounds().size.height,
            direction,
            metrics,
        );
        if let Some(next) = next {
            self.select_match_position(next, cx);
        }
    }

    fn reveal_selected(&mut self) {
        let Some(position) = self.selected_match_position() else {
            return;
        };
        let Some(row) = self.presented_results.list_index_for_match(position) else {
            return;
        };
        let item_is_visible = self.list.bounds_for_item(row).is_some_and(|item_bounds| {
            let viewport = self.list.viewport_bounds();
            item_bounds.top() >= viewport.top() && item_bounds.bottom() <= viewport.bottom()
        });
        if !item_is_visible {
            self.list.scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: px(0.0),
            });
        }
    }

    fn enabled_match_positions(&self) -> Vec<usize> {
        self.matches
            .iter()
            .enumerate()
            .filter_map(|(position, matched)| {
                self.items
                    .get(matched.item_index)
                    .is_some_and(|item| !item.disabled)
                    .then_some(position)
            })
            .collect()
    }

    fn selected_match_position(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.matches.iter().position(|matched| {
            self.items
                .get(matched.item_index)
                .is_some_and(|item| item.id == *selected)
        })
    }

    fn select_match_position(&mut self, position: usize, cx: &mut gpui::Context<Self>) {
        let next = self
            .matches
            .get(position)
            .and_then(|matched| self.items.get(matched.item_index))
            .filter(|item| !item.disabled)
            .map(|item| item.id.clone());
        if next.is_some() && self.selected != next {
            self.selected = next;
            if let Some(row) = self.presented_results.list_index_for_match(position) {
                self.list.scroll_to_reveal_item(row);
            }
            cx.notify();
        }
    }

    fn suppress_pointer(&mut self, position: gpui::Point<Pixels>, cx: &mut gpui::Context<Self>) {
        self.pointer_anchor = position;
        if !self.pointer_suppressed || !self.hover_suppressed {
            self.pointer_suppressed = true;
            self.hover_suppressed = true;
            cx.notify();
        }
    }

    fn suppress_hover_for_scroll(
        &mut self,
        position: gpui::Point<Pixels>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.pointer_anchor = position;
        if self.pointer_suppressed || !self.hover_suppressed {
            self.pointer_suppressed = false;
            self.hover_suppressed = true;
            cx.notify();
        }
    }

    fn resume_pointer_interaction(&mut self, cx: &mut gpui::Context<Self>) {
        if self.pointer_suppressed || self.hover_suppressed {
            self.pointer_suppressed = false;
            self.hover_suppressed = false;
            cx.notify();
        }
    }

    fn pointer_moved(
        &mut self,
        position: gpui::Point<Pixels>,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if (self.pointer_suppressed || self.hover_suppressed) && position == self.pointer_anchor {
            return false;
        }
        self.resume_pointer_interaction(cx);
        true
    }

    fn pointer_hover(
        &mut self,
        id: &I,
        pointer_position: gpui::Point<Pixels>,
        cx: &mut gpui::Context<Self>,
    ) {
        if !self.pointer_moved(pointer_position, cx) {
            return;
        }
        let position = self.matches.iter().position(|matched| {
            self.items
                .get(matched.item_index)
                .is_some_and(|item| !item.disabled && item.id == *id)
        });
        if let Some(position) = position {
            self.select_match_position(position, cx);
        }
    }

    fn pointer_down(&mut self, id: I, cx: &mut gpui::Context<Self>) {
        self.resume_pointer_interaction(cx);
        self.pointer_press = Some(id);
    }

    fn pointer_up(&mut self, id: &I, inside: bool) -> bool {
        if self.pointer_press.as_ref() != Some(id) {
            return false;
        }
        self.pointer_press = None;
        inside
            && self.matches.iter().any(|matched| {
                self.items
                    .get(matched.item_index)
                    .is_some_and(|item| !item.disabled && item.id == *id)
            })
    }

    fn focus_next_control(&self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        window.focus_next();
        if !self.focus_scope.contains_focused(window, cx) {
            self.input.read(cx).focus_handle().focus(window);
        }
        cx.stop_propagation();
    }

    fn focus_previous_control(&self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        window.focus_prev();
        if self.focus_scope.contains_focused(window, cx) {
            cx.stop_propagation();
            return;
        }

        let input_focus = self.input.read(cx).focus_handle();
        input_focus.focus(window);
        let mut last_internal = input_focus;
        let maximum_steps = self.header_actions.len()
            + usize::from(!self.actions_menu.is_empty())
            + usize::from(self.confirm.is_some());
        for _ in 0..maximum_steps {
            window.focus_next();
            if !self.focus_scope.contains_focused(window, cx) {
                last_internal.focus(window);
                break;
            }
            if let Some(focused) = window.focused(cx) {
                last_internal = focused;
            }
        }
        cx.stop_propagation();
    }

    fn activate_selected(
        &mut self,
        source: CommandPaletteActivationSource,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.loading {
            return;
        }
        let Some(item_id) = self.selected.clone() else {
            return;
        };
        let enabled = self.matches.iter().any(|matched| {
            self.items
                .get(matched.item_index)
                .is_some_and(|item| item.id == item_id && !item.disabled)
        });
        if !enabled {
            return;
        }
        if self.activation == CommandPaletteActivationPolicy::Continue {
            cx.emit(CommandPaletteEvent::Activated(CommandPaletteActivation {
                item_id,
                source,
            }));
            return;
        }
        if !self.begin_close(CommandPaletteCloseReason::Activated, window, cx) {
            return;
        }
        cx.emit(CommandPaletteEvent::Activated(CommandPaletteActivation {
            item_id,
            source,
        }));
        self.finish_close(CommandPaletteCloseReason::Activated, cx);
    }

    fn close(
        &mut self,
        reason: CommandPaletteCloseReason,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if !self.begin_close(reason, window, cx) {
            return false;
        }
        self.finish_close(reason, cx);
        true
    }

    fn begin_close(
        &mut self,
        reason: CommandPaletteCloseReason,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if !self.open {
            return false;
        }
        self.open = false;
        crate::tooltip::set_window_tooltip_suppression(
            window.window_handle().window_id(),
            crate::tooltip::TooltipSuppression::CommandPalette,
            false,
            cx,
        );
        self.loading = false;
        self.generation.0 = self.generation.0.wrapping_add(1);
        self.pointer_press = None;
        self.pointer_suppressed = false;
        self.hover_suppressed = false;
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.reset(cx));
        let restore_focus = self.restore_focus.take();
        if reason == CommandPaletteCloseReason::Deactivated {
            self.restore_on_activation = restore_focus;
        } else {
            self.restore_on_activation = None;
            if reason.restores_focus()
                && let Some(focus) = restore_focus.and_then(|focus| focus.upgrade())
            {
                focus.focus(window);
            }
        }
        true
    }

    fn finish_close(&self, reason: CommandPaletteCloseReason, cx: &mut gpui::Context<Self>) {
        cx.emit(CommandPaletteEvent::Lifecycle(
            CommandPaletteLifecycleEvent::Closed(reason),
        ));
        cx.notify();
    }
}

fn unique_items<I: Clone + Eq>(items: Vec<CommandPaletteItem<I>>) -> Vec<CommandPaletteItem<I>> {
    let mut unique = Vec::with_capacity(items.len());
    for item in items {
        if !unique
            .iter()
            .any(|existing: &CommandPaletteItem<I>| existing.id == item.id)
        {
            unique.push(item);
        }
    }
    unique
}

fn first_enabled_id<I: Clone>(
    items: &[CommandPaletteItem<I>],
    matches: &[CommandPaletteMatch],
) -> Option<I> {
    matches.iter().find_map(|matched| {
        items
            .get(matched.item_index)
            .filter(|item| !item.disabled)
            .map(|item| item.id.clone())
    })
}

impl<I: Clone + Eq + 'static> Render for CommandPalette<I> {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        let theme = *cx.global::<CommandPaletteTheme>();
        let metrics = theme.metrics;
        if std::mem::take(&mut self.scrollbar_reveal_pending) {
            self.reveal_scrollbar(cx);
        } else {
            self.sync_scrollbar(cx);
        }
        if std::mem::take(&mut self.selection_reveal_pending) {
            let palette = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = palette.update(cx, |palette, cx| {
                    palette.reveal_selected();
                    cx.notify();
                });
            });
        }
        let viewport = window.viewport_size();
        let available_width = (viewport.width - metrics.viewport_margin * 2.0).max(px(0.0));
        let panel_width = metrics.panel_width.min(available_width);
        let left = ((viewport.width - panel_width) / 2.0).max(px(0.0));
        let top = metrics
            .top_offset
            .min((viewport.height - metrics.viewport_margin).max(px(0.0)));

        let footer = self.has_footer();
        let content_height = if self.loading || self.matches.is_empty() {
            metrics.row_height
        } else {
            self.presented_results.total_height(metrics)
        };
        let chrome_height = chrome_height(metrics, footer);
        let available_height = (viewport.height - top - metrics.viewport_margin).max(px(0.0));
        let panel_height = (chrome_height + content_height)
            .min(metrics.maximum_height)
            .min(available_height);
        let list_height = (panel_height - chrome_height).max(px(0.0));

        let panel_bounds = gpui::Bounds::new(
            gpui::point(left, top),
            gpui::size(panel_width, panel_height),
        );
        let outside = self.render_outside_tracker(panel_bounds, cx);
        let panel = self.render_panel(panel_width, panel_height, list_height, theme, cx);
        let overlay = div()
            .relative()
            .w(viewport.width)
            .h(viewport.height)
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_scope)
            .tab_group()
            .child(outside)
            .child(div().absolute().left(left).top(top).child(panel))
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .when(self.pointer_suppressed, |overlay| {
                overlay.child(
                    canvas(
                        |_, _, _| (),
                        |_, _, window, _| window.set_window_cursor_style(CursorStyle::None),
                    )
                    .absolute()
                    .inset_0(),
                )
            })
            .on_action(cx.listener(|palette, _: &MoveUp, window, cx| {
                palette.move_selection(-1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|palette, _: &MoveDown, window, cx| {
                palette.move_selection(1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|palette, _: &MovePageUp, window, cx| {
                palette.move_page(-1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|palette, _: &MovePageDown, window, cx| {
                palette.move_page(1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|palette, _: &Activate, window, cx| {
                palette.activate_selected(CommandPaletteActivationSource::Keyboard, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|palette, _: &Dismiss, window, cx| {
                palette.close(CommandPaletteCloseReason::Escape, window, cx);
                cx.stop_propagation();
            }))
            .when(self.confirm.is_some(), |overlay| {
                overlay.on_action(cx.listener(|palette, _: &Confirm, _, cx| {
                    palette.confirm_activated(cx);
                    cx.stop_propagation();
                }))
            })
            .on_action(cx.listener(|palette, _: &FocusNext, window, cx| {
                palette.focus_next_control(window, cx);
            }))
            .on_action(cx.listener(|palette, _: &FocusPrevious, window, cx| {
                palette.focus_previous_control(window, cx);
            }));

        // The palette is not itself deferred: GPUI collects deferred draws once per frame, so a
        // deferred palette could not host its own deferred footer menu. Its owner renders it last,
        // and the anchored full-window layer keeps it above the surrounding chrome.
        anchored()
            .anchor(Corner::TopLeft)
            .position(gpui::point(px(0.0), px(0.0)))
            .snap_to_window()
            .child(overlay)
            .into_any_element()
    }
}

impl<I: Clone + Eq + 'static> CommandPalette<I> {
    fn has_footer(&self) -> bool {
        !self.hints.is_empty() || !self.actions_menu.is_empty() || self.confirm.is_some()
    }

    fn confirm_activated(&mut self, cx: &mut gpui::Context<Self>) {
        if self.confirm.as_ref().is_none_or(|confirm| confirm.disabled) {
            return;
        }
        cx.emit(CommandPaletteEvent::<I>::Confirmed);
    }

    fn render_outside_tracker(
        &self,
        panel_bounds: gpui::Bounds<Pixels>,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let palette = cx.entity().downgrade();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                let move_palette = palette.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase.capture() {
                        let _ = move_palette
                            .update(cx, |palette, cx| palette.pointer_moved(event.position, cx));
                    }
                });
                let scroll_palette = palette.clone();
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, _, cx| {
                    if phase.capture() {
                        let _ = scroll_palette.update(cx, |palette, cx| {
                            palette.suppress_hover_for_scroll(event.position, cx);
                        });
                    }
                });
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.capture() {
                        return;
                    }
                    let _ = palette.update(cx, |palette, cx| {
                        palette.resume_pointer_interaction(cx);
                    });
                    if panel_bounds.contains(&event.position) {
                        return;
                    }
                    // A menu opened from this palette paints outside the panel and owns its own
                    // dismissal, so an outside press while it is open is not a palette dismissal.
                    if crate::menu::window_menu_is_open(window, cx) {
                        return;
                    }
                    window.prevent_default();
                    let _ = palette.update(cx, |palette, cx| {
                        palette.close(CommandPaletteCloseReason::Outside, window, cx)
                    });
                    cx.stop_propagation();
                });
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }

    fn render_panel(
        &self,
        width: Pixels,
        height: Pixels,
        list_height: Pixels,
        theme: CommandPaletteTheme,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let paint = theme.paint;
        let metrics = theme.metrics;
        let content = if self.loading {
            status_row("Loading\u{2026}", "command-palette-loading", metrics, paint)
                .into_any_element()
        } else if self.matches.is_empty() {
            status_row(
                self.no_results_text.clone(),
                "command-palette-no-results",
                metrics,
                paint,
            )
            .into_any_element()
        } else {
            self.render_results(list_height, theme, cx)
        };

        div()
            .debug_selector(|| "command-palette-panel".to_owned())
            .w(width)
            .h(height)
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(metrics.corner_radius)
            .shadow_lg()
            .border(metrics.border_width)
            .border_color(paint.border)
            .bg(paint.background)
            .block_mouse_except_scroll()
            .child(self.render_editor(theme, cx))
            .child(separator_line(metrics, paint))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .py(metrics.panel_padding)
                    .child(content),
            )
            .when(self.has_footer(), |panel| {
                panel
                    .child(separator_line(metrics, paint))
                    .child(self.render_footer(theme, cx))
            })
            .into_any_element()
    }

    /// Renders the borderless search line and its trailing controls as one continuous surface.
    fn render_editor(
        &self,
        theme: CommandPaletteTheme,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let metrics = theme.metrics;
        let palette = cx.entity().downgrade();
        div()
            .debug_selector(|| "command-palette-editor".to_owned())
            .w_full()
            .h(metrics.input_height)
            .flex_shrink_0()
            .pl(metrics.content_leading_inset())
            .pr(metrics.panel_padding)
            .flex()
            .flex_row()
            .items_center()
            .gap(metrics.gap)
            .text_size(metrics.input_size)
            .child(div().min_w_0().flex_1().child(self.input.clone()))
            .when(!self.header_actions.is_empty(), |editor| {
                editor.child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .flex_row()
                        .items_center()
                        .children(
                            self.header_actions
                                .iter()
                                .enumerate()
                                .map(|(index, action)| {
                                    render_header_action(palette.clone(), index, action)
                                }),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_results(
        &self,
        list_height: Pixels,
        theme: CommandPaletteTheme,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let items = Rc::clone(&self.items);
        let matches = Rc::clone(&self.matches);
        let presented_results = Rc::clone(&self.presented_results);
        let selected = self.selected.clone();
        let hover_suppressed = self.pointer_suppressed || self.hover_suppressed;
        let leading_reserved = self.leading_reserved;
        let palette = cx.entity().downgrade();
        div()
            .relative()
            .size_full()
            .child(
                list(self.list.clone(), move |index, _, _| {
                    let Some(row) = presented_results.row(index) else {
                        return div().into_any_element();
                    };
                    let row_height = row.height(theme.metrics);
                    match row {
                        PaletteRow::Section(label) => {
                            render_section(label.clone(), row_height, theme).into_any_element()
                        }
                        PaletteRow::Separator => {
                            render_row_separator(row_height, theme).into_any_element()
                        }
                        PaletteRow::Item { position, .. } => matches
                            .get(*position)
                            .and_then(|matched| {
                                items.get(matched.item_index).map(|item| {
                                    render_row(
                                        palette.clone(),
                                        *position,
                                        item,
                                        &matched.label_highlights,
                                        &matched.description_highlights,
                                        selected.as_ref() == Some(&item.id),
                                        hover_suppressed,
                                        leading_reserved,
                                        row_height,
                                        theme,
                                    )
                                })
                            })
                            .unwrap_or_else(|| div().into_any_element()),
                    }
                })
                .h(list_height)
                .w_full(),
            )
            .child(self.scrollbar.clone())
            .into_any_element()
    }

    /// Renders the footer's caller actions.
    ///
    /// A lone ordinary action is offered directly, because a disclosure that reveals exactly one
    /// choice is more chrome than the choice it hides. Anything else stays a menu.
    fn render_actions(&self, cx: &mut gpui::Context<Self>) -> Option<AnyElement> {
        if self.actions_menu.is_empty() {
            return None;
        }
        let palette = cx.entity().downgrade();
        if let [entry] = self.actions_menu.as_slice()
            && let Some(action) = entry.plain_action()
        {
            let emitted = action.action.clone();
            return Some(
                Button::new("command-palette-actions-single", action.label.clone())
                    .variant(ButtonVariant::Ghost)
                    .size(FOOTER_CONTROL_SIZE)
                    .disabled(action.disabled)
                    .tab_stop(true)
                    .when_some(action.debug_selector, |button, selector| {
                        button.debug_selector(selector)
                    })
                    .on_activate(move |_, _, cx| {
                        let action = emitted.clone();
                        let _ = palette.update(cx, |_, cx| {
                            cx.emit(CommandPaletteEvent::<I>::MenuAction(action));
                        });
                    })
                    .into_any_element(),
            );
        }
        Some(
            Menu::new(
                "command-palette-actions-menu",
                self.actions_menu_label.clone(),
                self.actions_menu.clone(),
            )
            .size(MenuSize::Regular)
            .debug_selector("command-palette-actions-menu")
            .on_activate(move |activation: &MenuActivation<SharedString>, _, cx| {
                let action = activation.action().clone();
                let _ = palette.update(cx, |_, cx| {
                    cx.emit(CommandPaletteEvent::<I>::MenuAction(action));
                });
            })
            .into_any_element(),
        )
    }

    fn render_footer(
        &self,
        theme: CommandPaletteTheme,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let paint = theme.paint;
        let metrics = theme.metrics;
        // Caller actions anchor the leading edge and the confirm anchors the trailing one, so an
        // escape hatch never reads as the peer of the primary action.
        div()
            .debug_selector(|| "command-palette-footer".to_owned())
            .w_full()
            .h(metrics.footer_height)
            .flex_shrink_0()
            .px(metrics.footer_inset())
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(metrics.gap)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(metrics.gap)
                    .children(self.render_actions(cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(metrics.gap)
                    .children(
                        self.hints
                            .iter()
                            .map(|hint| render_hint(hint, metrics, paint)),
                    )
                    .when_some(self.confirm.clone(), |trailing, confirm| {
                        let confirm_palette = cx.entity().downgrade();
                        let shortcut = confirm.shortcut.clone();
                        let shortcut_size = metrics.secondary_size;
                        trailing.child(
                            Button::new("command-palette-confirm", confirm.label.clone())
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::Small)
                                .disabled(confirm.disabled)
                                .tab_stop(true)
                                .when_some(confirm.debug_selector.clone(), |button, selector| {
                                    button.debug_selector(selector)
                                })
                                .trailing(move |foreground| {
                                    div()
                                        .text_size(shortcut_size)
                                        .text_color(foreground)
                                        .child(shortcut)
                                        .into_any_element()
                                })
                                .on_activate(move |_, _, cx| {
                                    let _ = confirm_palette
                                        .update(cx, |palette, cx| palette.confirm_activated(cx));
                                }),
                        )
                    }),
            )
            .into_any_element()
    }
}

fn chrome_height(metrics: CommandPaletteMetrics, footer: bool) -> Pixels {
    let footer_height = if footer {
        metrics.border_width + metrics.footer_height
    } else {
        px(0.0)
    };
    metrics.panel_padding * 2.0 + metrics.input_height + metrics.border_width * 3.0 + footer_height
}

fn separator_line(metrics: CommandPaletteMetrics, paint: CommandPalettePaint) -> impl IntoElement {
    div()
        .w_full()
        .h(metrics.border_width)
        .flex_shrink_0()
        .bg(paint.separator)
}

fn render_hint(
    hint: &CommandPaletteHint,
    metrics: CommandPaletteMetrics,
    paint: CommandPalettePaint,
) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(metrics.accessory_padding)
        .text_size(metrics.secondary_size)
        .child(
            div()
                .text_color(paint.footer_foreground)
                .child(hint.label.clone()),
        )
        .child(
            div()
                .text_color(paint.footer_key_foreground)
                .child(hint.key.clone()),
        )
}

/// Renders one search-line control as a ghost icon button from the installed button catalog.
fn render_header_action<I: Clone + Eq + 'static>(
    palette: WeakEntity<CommandPalette<I>>,
    index: usize,
    action: &CommandPaletteAction,
) -> AnyElement {
    let id = action.id.clone();
    let icon = action.icon.clone();
    let mut button = IconButton::new(
        ("command-palette-header-action", index),
        action.accessibility_name.clone(),
        move |foreground| icon(foreground),
    )
    .variant(ButtonVariant::Ghost)
    .size(ButtonSize::Compact)
    .disabled(action.disabled)
    .tab_stop(true)
    .on_activate(move |_, _, cx| {
        let id = id.clone();
        let _ = palette.update(cx, |_, cx| {
            cx.emit(CommandPaletteEvent::<I>::HeaderAction(id));
        });
    });
    if let Some(selector) = action.debug_selector.clone() {
        button = button.debug_selector(selector);
    }
    button.into_any_element()
}

fn render_section(
    label: SharedString,
    height: Pixels,
    theme: CommandPaletteTheme,
) -> impl IntoElement {
    let metrics = theme.metrics;
    div()
        .w_full()
        .h(height)
        .pl(metrics.content_leading_inset())
        .flex()
        .items_center()
        .text_size(metrics.secondary_size)
        .text_color(theme.paint.section_foreground)
        .child(label)
}

fn render_row_separator(height: Pixels, theme: CommandPaletteTheme) -> impl IntoElement {
    let metrics = theme.metrics;
    div()
        .w_full()
        .h(height)
        .px(metrics.panel_padding)
        .flex()
        .items_center()
        .child(
            div()
                .w_full()
                .h(metrics.border_width)
                .bg(theme.paint.separator),
        )
}

fn status_row(
    text: impl Into<SharedString>,
    debug_selector: &'static str,
    metrics: CommandPaletteMetrics,
    paint: CommandPalettePaint,
) -> impl IntoElement {
    div()
        .debug_selector(move || debug_selector.to_owned())
        .w_full()
        .h(metrics.row_height)
        .px(metrics.content_leading_inset())
        .flex()
        .items_center()
        .text_size(metrics.secondary_size)
        .text_color(paint.muted)
        .child(text.into())
}

#[expect(
    clippy::too_many_arguments,
    reason = "one row's complete presentation inputs are clearer than an intermediate struct"
)]
fn render_row<I: Clone + Eq + 'static>(
    palette: WeakEntity<CommandPalette<I>>,
    position: usize,
    item: &CommandPaletteItem<I>,
    label_highlights: &[Range<usize>],
    description_highlights: &[Range<usize>],
    selected: bool,
    hover_suppressed: bool,
    leading_reserved: bool,
    height: Pixels,
    theme: CommandPaletteTheme,
) -> AnyElement {
    let paint = theme.paint;
    let metrics = theme.metrics;
    let foreground = if item.disabled {
        paint.disabled
    } else if selected {
        paint.selected_foreground
    } else {
        paint.foreground
    };
    let secondary = if item.disabled {
        paint.disabled
    } else {
        paint.muted
    };
    let match_foreground = if item.disabled {
        paint.disabled
    } else {
        paint.match_foreground
    };
    let active_background = if hover_suppressed {
        paint.selected_background
    } else {
        paint.hover_background
    };
    let logical_name = item.label.clone();
    let debug_selector = item.debug_selector.clone();
    let id = item.id.clone();
    let hover_palette = palette.clone();
    let mut row = div()
        .id(("command-palette-row", position))
        .debug_selector(move || debug_selector.unwrap_or_else(|| logical_name.to_string()))
        .relative()
        .w_full()
        .h(height)
        .px(metrics.horizontal_padding)
        .flex()
        .items_center()
        .gap(metrics.gap)
        .rounded(metrics.row_corner_radius())
        .text_color(foreground)
        .cursor_default()
        .when(selected, |row| row.bg(active_background))
        .when(!item.disabled, |row| {
            let id = id.clone();
            row.on_mouse_move(move |event, _, cx| {
                let _ = hover_palette.update(cx, |palette, cx| {
                    palette.pointer_hover(&id, event.position, cx)
                });
            })
        });

    if leading_reserved {
        let mut leading = div()
            .w(metrics.leading_width)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center();
        if let Some(icon) = item.leading_icon.clone() {
            leading = leading.child(icon(foreground));
        }
        row = row.child(leading);
    }

    let label_line = div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(metrics.gap)
        .child(div().min_w_0().flex_1().child(highlighted_text(
            item.label.clone(),
            label_highlights,
            foreground,
            match_foreground,
            metrics.label_size,
        )))
        .when_some(item.trailing.clone(), |line, accessory| {
            line.child(render_accessory(
                accessory,
                secondary,
                paint.selected_background,
                metrics,
            ))
        });
    let text = div()
        .min_w_0()
        .flex_1()
        .flex()
        .flex_col()
        .justify_center()
        .gap(metrics.row_line_gap)
        .child(label_line)
        .when_some(item.description.clone(), |text, description| {
            text.child(
                div()
                    .w_full()
                    .min_w_0()
                    .overflow_hidden()
                    .child(highlighted_text(
                        description,
                        description_highlights,
                        secondary,
                        match_foreground,
                        metrics.secondary_size,
                    )),
            )
        });
    row = row.child(text);

    if !item.disabled {
        let down_palette = palette.clone();
        let up_palette = palette;
        let down_id = item.id.clone();
        let up_id = item.id.clone();
        row = row.child(
            canvas(
                |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                move |_, hitbox, window, _| {
                    let down_hitbox = hitbox.clone();
                    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                        if !phase.capture()
                            || event.button != MouseButton::Left
                            || !down_hitbox.is_hovered(window)
                        {
                            return;
                        }
                        window.prevent_default();
                        let id = down_id.clone();
                        let _ = down_palette.update(cx, |palette, cx| palette.pointer_down(id, cx));
                        cx.stop_propagation();
                    });
                    let up_hitbox = hitbox.clone();
                    let move_palette = up_palette.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                        if !phase.capture() || event.button != MouseButton::Left {
                            return;
                        }
                        let inside = up_hitbox.is_hovered(window);
                        let activate = up_palette
                            .update(cx, |palette, _| palette.pointer_up(&up_id, inside))
                            .unwrap_or(false);
                        if activate {
                            window.prevent_default();
                            let _ = up_palette.update(cx, |palette, cx| {
                                palette.selected = Some(up_id.clone());
                                palette.activate_selected(
                                    CommandPaletteActivationSource::Pointer,
                                    window,
                                    cx,
                                );
                            });
                            cx.stop_propagation();
                        }
                    });
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase.capture() && event.pressed_button != Some(MouseButton::Left) {
                            let _ = move_palette.update(cx, |palette, _| {
                                palette.pointer_press = None;
                            });
                        }
                    });
                },
            )
            .absolute()
            .inset_0(),
        );
    }
    div()
        .w_full()
        .px(metrics.panel_padding)
        .child(row)
        .into_any_element()
}

fn highlighted_text(
    text: SharedString,
    ranges: &[Range<usize>],
    foreground: Rgba,
    highlight: Rgba,
    size: Pixels,
) -> AnyElement {
    let source = text.as_ref();
    let mut content = div()
        .min_w_0()
        .flex()
        .items_center()
        .truncate()
        .text_size(size)
        .text_color(foreground);
    let mut cursor = 0;
    for range in ranges {
        if range.start > cursor {
            content = content.child(source[cursor..range.start].to_owned());
        }
        content = content.child(
            div()
                .text_color(highlight)
                .child(source[range.clone()].to_owned()),
        );
        cursor = range.end;
    }
    if cursor < source.len() {
        content = content.child(source[cursor..].to_owned());
    }
    content.into_any_element()
}

fn render_accessory(
    accessory: CommandPaletteAccessory,
    color: Rgba,
    status_background: Rgba,
    metrics: CommandPaletteMetrics,
) -> AnyElement {
    match accessory {
        CommandPaletteAccessory::Text(text) | CommandPaletteAccessory::Shortcut(text) => div()
            .flex_shrink_0()
            .text_size(metrics.secondary_size)
            .text_color(color)
            .child(text)
            .into_any_element(),
        CommandPaletteAccessory::Status(text) => div()
            .flex_shrink_0()
            .px(metrics.accessory_padding)
            .py(metrics.accessory_line_padding)
            .rounded(metrics.accessory_radius)
            .bg(status_background)
            .text_size(metrics.secondary_size)
            .text_color(color)
            .child(text)
            .into_any_element(),
        CommandPaletteAccessory::Checkmark => div()
            .flex_shrink_0()
            .text_size(metrics.secondary_size)
            .text_color(color)
            .child("\u{2713}")
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    use gpui::{
        Context, Entity, FocusHandle, Modifiers, Render, ScrollDelta, ScrollWheelEvent,
        TestAppContext, TouchPhase, VisualTestContext, Window, point, rgba,
    };

    use super::*;

    fn test_theme() -> CommandPaletteTheme {
        CommandPaletteTheme::new(
            CommandPalettePaint::new(
                rgba(0x141415ff),
                rgba(0x252530ff),
                rgba(0xcdcdcdff),
                rgba(0x878787ff),
                rgba(0x606079ff),
                rgba(0x252530ff),
                rgba(0xffffffff),
                rgba(0x7e98e8ff),
            )
            .separator(rgba(0x252530ff))
            .hover_background(rgba(0x1c1c24ff))
            .section_foreground(rgba(0x878787ff))
            .footer(rgba(0x878787ff), rgba(0x606079ff)),
            CommandPaletteMetrics::new(px(420.0), px(40.0))
                .single_line_row_height(px(28.0))
                .footer_padding(px(8.0))
                .panel_geometry(px(260.0), px(24.0)),
        )
    }

    fn items() -> Vec<CommandPaletteItem<u8>> {
        vec![
            CommandPaletteItem::new(1, "Open Workspace")
                .description("Choose a directory")
                .keywords(["project"])
                .debug_selector("row-open"),
            CommandPaletteItem::new(2, "Disabled Command")
                .disabled(true)
                .debug_selector("row-disabled"),
            CommandPaletteItem::new(3, "Close Window")
                .keywords(["remove"])
                .debug_selector("row-close"),
        ]
    }

    fn sectioned_results() -> (PresentedResults, CommandPaletteMetrics) {
        let items = vec![
            CommandPaletteItem::new(1, "Recent One").section("Recent"),
            CommandPaletteItem::new(2, "Recent Two").section("Recent"),
            CommandPaletteItem::new(3, "All One").section("All"),
            CommandPaletteItem::new(4, "All Two").section("All"),
            CommandPaletteItem::new(5, "All Three").section("All"),
        ];
        let matches = match_command_palette_items(&items, "", CommandPaletteMatching::Semantic);
        (
            PresentedResults::new(&items, &matches),
            CommandPaletteMetrics::new(px(420.0), px(40.0)),
        )
    }

    #[test]
    fn presented_results_should_own_section_order_and_match_mapping() {
        let (results, _) = sectioned_results();

        assert_eq!(
            (results.rows(), results.list_index_for_match(2)),
            (
                &[
                    PaletteRow::Section("Recent".into()),
                    PaletteRow::Item {
                        position: 0,
                        single_line: true
                    },
                    PaletteRow::Item {
                        position: 1,
                        single_line: true
                    },
                    PaletteRow::Separator,
                    PaletteRow::Section("All".into()),
                    PaletteRow::Item {
                        position: 2,
                        single_line: true
                    },
                    PaletteRow::Item {
                        position: 3,
                        single_line: true
                    },
                    PaletteRow::Item {
                        position: 4,
                        single_line: true
                    },
                ][..],
                Some(5),
            )
        );
    }

    #[test]
    fn presented_results_should_measure_and_hit_test_every_row_kind() {
        let (results, metrics) = sectioned_results();

        assert_eq!(
            (
                results.total_height(metrics),
                results.row_at_y(px(0.0), metrics).map(|(index, _)| index),
                results.item_at_y(px(22.0), metrics),
                results.item_at_y(px(105.0), metrics),
                results.item_at_y(px(133.0), metrics),
                results.row_at_y(px(253.0), metrics),
            ),
            (px(253.0), Some(0), Some(0), None, Some(2), None)
        );
    }

    #[test]
    fn page_target_should_include_section_and_separator_heights() {
        let (results, metrics) = sectioned_results();

        assert_eq!(
            results.page_target(Some(0), &[0, 1, 2, 3, 4], px(120.0), 1, metrics),
            Some(2)
        );
    }

    #[test]
    fn page_target_should_skip_disabled_matches_without_ignoring_their_height() {
        let (results, metrics) = sectioned_results();

        assert_eq!(
            results.page_target(Some(4), &[0, 2, 4], px(120.0), -1, metrics),
            Some(2)
        );
    }

    #[test]
    fn empty_query_should_preserve_provider_order() {
        let matches = match_command_palette_items(&items(), "", CommandPaletteMatching::Semantic);

        assert_eq!(
            matches
                .iter()
                .map(|matched| matched.item_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn duplicate_item_identity_should_keep_only_the_first_semantic_item() {
        let items = unique_items(vec![
            CommandPaletteItem::new(1, "First"),
            CommandPaletteItem::new(1, "Replacement"),
        ]);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label(), "First");
    }

    #[test]
    fn matcher_should_search_description_and_keywords() {
        let items = items();

        assert_eq!(
            match_command_palette_items(&items, "directory", CommandPaletteMatching::Semantic)[0]
                .item_index,
            0
        );
        assert_eq!(
            match_command_palette_items(&items, "remove", CommandPaletteMatching::Semantic)[0]
                .item_index,
            2
        );
    }

    #[test]
    fn direct_label_match_should_rank_above_metadata_match() {
        let items = vec![
            CommandPaletteItem::new(1, "Open").keywords(["window"]),
            CommandPaletteItem::new(2, "Window Settings"),
        ];

        assert_eq!(
            match_command_palette_items(&items, "window", CommandPaletteMatching::Semantic)[0]
                .item_index,
            1
        );
    }

    #[test]
    fn unicode_highlights_should_remain_valid_label_boundaries() {
        let items = vec![CommandPaletteItem::new(1, "Éclair 🔍")];
        let matches = match_command_palette_items(&items, "é🔍", CommandPaletteMatching::Semantic);
        let ranges = &matches[0].label_highlights;

        assert_eq!(ranges, &[0..2, 8..12]);
        assert!(ranges.iter().all(|range| {
            items[0].label().is_char_boundary(range.start)
                && items[0].label().is_char_boundary(range.end)
        }));
    }

    #[test]
    fn multiple_query_tokens_may_match_different_semantic_fields() {
        let items = vec![CommandPaletteItem::new(1, "Open Workspace").keywords(["project"])];

        assert_eq!(
            match_command_palette_items(&items, "open project", CommandPaletteMatching::Semantic)
                .len(),
            1
        );
    }

    struct CloneCountingId {
        value: u16,
        clones: Rc<Cell<usize>>,
    }

    impl Clone for CloneCountingId {
        fn clone(&self) -> Self {
            self.clones.set(self.clones.get() + 1);
            Self {
                value: self.value,
                clones: Rc::clone(&self.clones),
            }
        }
    }

    impl PartialEq for CloneCountingId {
        fn eq(&self, other: &Self) -> bool {
            self.value == other.value
        }
    }

    impl Eq for CloneCountingId {}

    struct CloneCountingRoot {
        palette: Entity<CommandPalette<CloneCountingId>>,
    }

    impl Render for CloneCountingRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.palette.clone())
        }
    }

    struct TestRoot {
        palette: Entity<CommandPalette<u8>>,
        other_focus: FocusHandle,
        intruder_focus: FocusHandle,
        underlay_presses: Rc<RefCell<usize>>,
    }

    impl Render for TestRoot {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let underlay_press = self.underlay_presses.clone();
            let underlay_scroll = self.underlay_presses.clone();
            div()
                .relative()
                .size_full()
                .on_mouse_down(MouseButton::Left, move |_, _, _| {
                    *underlay_press.borrow_mut() += 1;
                })
                .on_scroll_wheel(move |_, _, _| {
                    *underlay_scroll.borrow_mut() += 1;
                })
                .child(
                    div()
                        .debug_selector(|| "prior-focus".to_owned())
                        .track_focus(&self.other_focus)
                        .child("Prior"),
                )
                .child(div().track_focus(&self.intruder_focus).child("Intruder"))
                .child(self.palette.clone())
                .on_action(cx.listener(|_, _: &MoveDown, _, _| {}))
        }
    }

    type PaletteWindow<'a> = (
        Entity<TestRoot>,
        Entity<CommandPalette<u8>>,
        Rc<RefCell<Vec<CommandPaletteEvent<u8>>>>,
        Rc<RefCell<usize>>,
        &'a mut VisualTestContext,
    );

    fn install_control_themes(cx: &mut TestAppContext) {
        let variant = crate::button::ButtonVariantStyle::new(
            crate::button::ButtonPaint::new(rgba(0x00000000), rgba(0xcdcdcdff), rgba(0x00000000)),
            crate::button::ButtonPaint::new(rgba(0x252530ff), rgba(0xcdcdcdff), rgba(0x00000000)),
            crate::button::ButtonPaint::new(rgba(0x252530ff), rgba(0xcdcdcdff), rgba(0x00000000)),
            crate::button::ButtonPaint::new(rgba(0x141415ff), rgba(0x606079ff), rgba(0x00000000)),
        );
        let button_metrics = crate::button::ButtonMetrics::new(px(24.0));
        cx.set_global(crate::button::ButtonTheme::new(
            crate::button::ButtonVariants::new(
                variant, variant, variant, variant, variant, variant,
            ),
            crate::button::ButtonSizes::new(
                button_metrics,
                button_metrics,
                button_metrics,
                button_metrics,
            ),
            rgba(0x405065ff),
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
        let menu_metrics = crate::menu::MenuMetrics::new(px(160.0), px(26.0));
        cx.set_global(crate::menu::MenuTheme::new(
            menu_paint,
            crate::menu::MenuSizes::new(menu_metrics, menu_metrics, menu_metrics),
        ));
        cx.update(crate::menu::init);
        cx.set_global(crate::overlay_scrollbar::ScrollbarTheme::new(
            rgba(0x33373878),
            rgba(0x60607978),
            rgba(0xcdcdcdff),
        ));
        let input_paint = crate::text_input::TextInputPaint::new(
            rgba(0xcdcdcdff),
            rgba(0x878787ff),
            rgba(0x6e94b266),
            rgba(0xcdcdcdff),
            rgba(0x606079ff),
            rgba(0x606079ff),
        );
        cx.set_global(crate::text_input::TextInputTheme::new(
            crate::text_input::TextInputVariants::new(input_paint, input_paint),
            crate::text_input::TextInputMetrics::new(
                px(1.0),
                px(2.0),
                std::time::Duration::from_millis(16),
                px(20.0),
            ),
        ));
        cx.update(|cx| {
            crate::text_input::install_text_input_keybindings(
                cx,
                crate::text_input::TextInputKeybindingProfile::MacOs,
            );
        });
    }

    fn palette_window(cx: &mut TestAppContext) -> PaletteWindow<'_> {
        cx.set_global(test_theme());
        install_control_themes(cx);
        cx.update(crate::text_input::init);
        cx.update(super::init);
        let events = Rc::new(RefCell::new(Vec::new()));
        let underlay = Rc::new(RefCell::new(0));
        let root_events = events.clone();
        let root_underlay = underlay.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let palette = cx.new(|cx| CommandPalette::new("Search commands", items(), window, cx));
            cx.subscribe(&palette, move |_, _, event: &CommandPaletteEvent<u8>, _| {
                root_events.borrow_mut().push(event.clone());
            })
            .detach();
            TestRoot {
                palette,
                other_focus: cx.focus_handle().tab_stop(true),
                intruder_focus: cx.focus_handle().tab_stop(true),
                underlay_presses: root_underlay,
            }
        });
        let palette = root.read_with(cx, |root, _| root.palette.clone());
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (root, palette, events, underlay, cx)
    }

    fn open_palette(
        root: &Entity<TestRoot>,
        palette: &Entity<CommandPalette<u8>>,
        cx: &mut VisualTestContext,
    ) -> FocusHandle {
        let prior = root.read_with(cx, |root, _| root.other_focus.clone());
        cx.update(|window, _| prior.focus(window));
        cx.update(|window, cx| {
            palette.update(cx, |palette, cx| {
                palette.open(window, cx);
            });
        });
        cx.run_until_parked();
        prior
    }

    #[gpui::test]
    fn wheel_scrolling_the_results_should_not_reenter_the_list_state(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        // Overflow the panel so the list actually scrolls and notifies its scroll handler.
        palette.update(cx, |palette, cx| {
            palette.set_items(
                (0..64)
                    .map(|index| CommandPaletteItem::new(index, format!("Command {index}")))
                    .collect(),
                cx,
            );
        });
        cx.run_until_parked();

        let panel = cx
            .debug_bounds("command-palette-panel")
            .expect("the palette panel was not rendered");
        let selected_before_scroll =
            palette.read_with(cx, |palette, _| palette.selected_item_id().copied());
        cx.simulate_event(ScrollWheelEvent {
            position: panel.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-240.0))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();

        assert!(
            palette.read_with(cx, |palette, _| palette.is_open()),
            "wheel scrolling the results closed the palette"
        );
        assert!(
            cx.debug_bounds("command-palette-panel").is_some(),
            "the palette stopped rendering after a wheel scroll"
        );
        assert_eq!(
            palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
            selected_before_scroll,
            "wheel scrolling changed selection under a stationary pointer"
        );
        assert!(palette.read_with(cx, |palette, _| palette.hover_suppressed));
        assert!(!palette.read_with(cx, |palette, _| palette.pointer_suppressed));

        cx.simulate_mouse_move(panel.center(), None, Modifiers::none());
        cx.run_until_parked();
        assert!(palette.read_with(cx, |palette, _| palette.hover_suppressed));

        cx.simulate_mouse_move(
            panel.center() + point(px(1.0), px(0.0)),
            None,
            Modifiers::none(),
        );
        cx.run_until_parked();
        assert!(!palette.read_with(cx, |palette, _| palette.hover_suppressed));
    }

    #[gpui::test]
    fn wheel_scroll_should_not_clone_offscreen_items(cx: &mut TestAppContext) {
        cx.set_global(test_theme());
        install_control_themes(cx);
        cx.update(crate::text_input::init);
        cx.update(super::init);
        let offscreen_clones = Rc::new(Cell::new(0));
        let tracked_clones = Rc::clone(&offscreen_clones);
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let items = (0..64)
                .map(|value| {
                    let clones = if value == 63 {
                        Rc::clone(&tracked_clones)
                    } else {
                        Rc::new(Cell::new(0))
                    };
                    CommandPaletteItem::new(
                        CloneCountingId { value, clones },
                        format!("Command {value}"),
                    )
                })
                .collect();
            let palette = cx.new(|cx| CommandPalette::new("Search", items, window, cx));
            CloneCountingRoot { palette }
        });
        let palette = root.read_with(cx, |root, _| root.palette.clone());
        cx.update(|window, _| window.activate_window());
        cx.update(|window, cx| {
            palette.update(cx, |palette, cx| {
                palette.open(window, cx);
            });
        });
        cx.run_until_parked();
        offscreen_clones.set(0);

        let panel = cx
            .debug_bounds("command-palette-panel")
            .expect("the palette panel was not rendered");
        cx.simulate_event(ScrollWheelEvent {
            position: panel.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-24.0))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();

        assert_eq!(offscreen_clones.get(), 0);
    }

    #[gpui::test]
    fn exact_fit_panel_height_should_include_its_outer_borders(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        let metrics = test_theme().metrics;
        let content_height = palette.read_with(cx, |palette, _| {
            palette.presented_results.total_height(metrics)
        });
        let panel = cx
            .debug_bounds("command-palette-panel")
            .expect("the palette panel was not rendered");

        assert_eq!(
            panel.size.height,
            content_height
                + metrics.input_height
                + metrics.panel_padding * 2.0
                + metrics.border_width * 3.0,
        );
    }

    #[gpui::test]
    fn selected_row_highlight_should_span_the_panel_inset(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        let metrics = test_theme().metrics;
        let panel = cx
            .debug_bounds("command-palette-panel")
            .expect("the palette panel was not rendered");
        let row = cx
            .debug_bounds("row-open")
            .expect("the selected row was not rendered");

        assert_eq!(
            row.size.width,
            panel.size.width - metrics.panel_padding * 2.0 - metrics.border_width * 2.0,
            "the selected row did not span the panel inset: {row:?} in {panel:?}"
        );
    }

    #[gpui::test]
    fn editor_and_row_content_should_share_one_leading_edge(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        let metrics = test_theme().metrics;
        let editor = cx
            .debug_bounds("command-palette-editor")
            .expect("the palette editor was not rendered");
        let row = cx
            .debug_bounds("row-open")
            .expect("the first row was not rendered");

        assert_eq!(
            editor.left() + metrics.content_leading_inset(),
            row.left() + metrics.horizontal_padding,
            "the editor text and row content did not share a leading edge: {editor:?} {row:?}"
        );
    }

    #[test]
    fn caller_matching_should_present_every_item_in_caller_order() {
        let items = items();
        // A path-shaped query fuzzy-matches nothing here, so semantic matching would empty the list.
        let semantic =
            match_command_palette_items(&items, "~/Doc", CommandPaletteMatching::Semantic);
        assert!(
            semantic.is_empty(),
            "the semantic matcher unexpectedly matched a path query"
        );

        let caller = match_command_palette_items(&items, "~/Doc", CommandPaletteMatching::Caller);
        assert_eq!(
            caller
                .iter()
                .map(|matched| matched.item_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2],
            "caller matching did not present every item in caller order"
        );
        assert!(
            caller
                .iter()
                .all(|matched| matched.label_highlights.is_empty()
                    && matched.description_highlights.is_empty()),
            "caller matching produced highlights for a query that is not a search term"
        );
    }

    #[gpui::test]
    fn caller_matching_should_survive_a_query_that_matches_nothing(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        palette.update(cx, |palette, cx| {
            palette.set_matching(CommandPaletteMatching::Caller, cx);
            palette.set_query("~/Doc", cx);
        });
        cx.run_until_parked();

        assert!(
            cx.debug_bounds("row-open").is_some(),
            "caller matching filtered an item the caller supplied"
        );
    }

    #[gpui::test]
    fn a_lone_footer_action_should_be_offered_without_a_disclosure(cx: &mut TestAppContext) {
        let (root, palette, events, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        palette.update(cx, |palette, cx| {
            palette.set_actions_menu(
                vec![
                    MenuEntry::action("Choose with Finder\u{2026}", "finder".into())
                        .debug_selector("footer-finder"),
                ],
                cx,
            );
        });
        cx.run_until_parked();

        assert!(
            cx.debug_bounds("command-palette-actions-menu").is_none(),
            "one action still rendered a disclosure"
        );
        let button = cx
            .debug_bounds("footer-finder")
            .expect("the lone action was not offered directly");

        cx.simulate_click(button.center(), Modifiers::none());
        cx.run_until_parked();

        assert!(
            events
                .borrow()
                .contains(&CommandPaletteEvent::MenuAction("finder".into())),
            "activating the lone action did not report its caller identity"
        );
    }

    #[gpui::test]
    fn a_row_without_a_description_should_take_the_single_line_height(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        palette.update(cx, |palette, cx| {
            palette.set_items(
                vec![
                    CommandPaletteItem::new(1, "Projects/").debug_selector("row-single"),
                    CommandPaletteItem::new(2, "Open Workspace")
                        .description("Choose a directory")
                        .debug_selector("row-described"),
                ],
                cx,
            );
        });
        cx.run_until_parked();

        let metrics = test_theme().metrics;
        let single = cx
            .debug_bounds("row-single")
            .expect("the single-line row was not rendered");
        let described = cx
            .debug_bounds("row-described")
            .expect("the described row was not rendered");

        assert_eq!(single.size.height, metrics.single_line_row_height);
        assert_eq!(described.size.height, metrics.row_height);
    }

    #[gpui::test]
    fn footer_control_labels_should_share_the_content_edges(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        palette.update(cx, |palette, cx| {
            palette.set_actions_menu(
                vec![
                    MenuEntry::action("Choose with Finder\u{2026}", "finder".into())
                        .debug_selector("footer-finder"),
                ],
                cx,
            );
            palette.set_confirm(
                Some(CommandPaletteConfirm::new("Add").debug_selector("footer-confirm")),
                cx,
            );
        });
        cx.run_until_parked();

        let metrics = test_theme().metrics;
        // The padding install_control_themes gives a Small text button around its label.
        let label_inset = px(8.0);
        let row = cx
            .debug_bounds("row-open")
            .expect("the first row was not rendered");
        let action = cx
            .debug_bounds("footer-finder")
            .expect("the footer action was not rendered");
        let confirm = cx
            .debug_bounds("footer-confirm")
            .expect("the confirm was not rendered");

        // The controls hang outward by their own label padding, so their text, not their boxes,
        // lines up with the row content above.
        assert_eq!(
            action.left() + label_inset,
            row.left() + metrics.horizontal_padding,
            "the footer action's label did not share the content leading edge: {action:?} {row:?}"
        );
        assert_eq!(
            confirm.right() - label_inset,
            row.right() - metrics.horizontal_padding,
            "the confirm's label did not share the content trailing edge: {confirm:?} {row:?}"
        );
    }

    #[gpui::test]
    fn the_footer_should_anchor_actions_and_the_confirm_to_opposite_edges(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        palette.update(cx, |palette, cx| {
            palette.set_actions_menu(
                vec![
                    MenuEntry::action("Choose with Finder\u{2026}", "finder".into())
                        .debug_selector("footer-finder"),
                ],
                cx,
            );
            palette.set_confirm(
                Some(CommandPaletteConfirm::new("Add").debug_selector("footer-confirm")),
                cx,
            );
        });
        cx.run_until_parked();

        let footer = cx
            .debug_bounds("command-palette-footer")
            .expect("the footer was not rendered");
        let action = cx
            .debug_bounds("footer-finder")
            .expect("the footer action was not rendered");
        let confirm = cx
            .debug_bounds("footer-confirm")
            .expect("the confirm was not rendered");

        assert!(
            action.right() < confirm.left(),
            "the action and the confirm were not separated: {action:?} {confirm:?}"
        );
        let leading_gap = action.left() - footer.left();
        let trailing_gap = footer.right() - confirm.right();
        assert!(
            leading_gap < footer.size.width / 4.0 && trailing_gap < footer.size.width / 4.0,
            "the footer clustered its controls instead of anchoring both edges: \
             leading {leading_gap:?} trailing {trailing_gap:?} in {footer:?}"
        );
    }

    #[gpui::test]
    fn several_footer_actions_should_stay_behind_a_disclosure(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        palette.update(cx, |palette, cx| {
            palette.set_actions_menu(
                vec![
                    MenuEntry::action("Choose with Finder\u{2026}", "finder".into())
                        .debug_selector("footer-finder"),
                    MenuEntry::action("Retry", "retry".into()),
                ],
                cx,
            );
        });
        cx.run_until_parked();

        assert!(
            cx.debug_bounds("command-palette-actions-menu").is_some(),
            "several actions did not render a disclosure"
        );
        assert!(
            cx.debug_bounds("footer-finder").is_none(),
            "a menu entry rendered outside its closed menu"
        );
    }

    #[gpui::test]
    fn continuing_activation_should_report_the_item_without_closing(cx: &mut TestAppContext) {
        let (root, palette, events, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        palette.update(cx, |palette, cx| {
            palette.set_activation(CommandPaletteActivationPolicy::Continue, cx);
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(
            events.borrow().iter().any(|event| matches!(
                event,
                CommandPaletteEvent::Activated(activation) if *activation.item_id() == 1
            )),
            "a continuing activation did not report its item"
        );
        assert!(
            palette.read_with(cx, |palette, _| palette.is_open()),
            "a continuing activation closed the palette"
        );
        assert!(
            !events.borrow().iter().any(|event| matches!(
                event,
                CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Closed(_))
            )),
            "a continuing activation published a close lifecycle event"
        );
    }

    #[gpui::test]
    fn confirm_should_render_once_and_report_its_keyboard_equivalent(cx: &mut TestAppContext) {
        let (root, palette, events, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        assert!(
            cx.debug_bounds("command-palette-footer").is_none(),
            "a palette without a confirm rendered a footer"
        );

        palette.update(cx, |palette, cx| {
            palette.set_confirm(
                Some(CommandPaletteConfirm::new("Add").debug_selector("palette-confirm")),
                cx,
            );
        });
        cx.run_until_parked();

        let footer = cx
            .debug_bounds("command-palette-footer")
            .expect("an installed confirm did not render a footer");
        let confirm = cx
            .debug_bounds("palette-confirm")
            .expect("the confirm control was not rendered");
        assert!(
            footer.contains(&confirm.center()),
            "the confirm control rendered outside the footer: {footer:?} {confirm:?}"
        );

        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();

        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|event| **event == CommandPaletteEvent::Confirmed)
                .count(),
            1,
            "the confirm key did not emit exactly one confirmation"
        );
    }

    #[gpui::test]
    fn a_disabled_confirm_should_ignore_its_keyboard_equivalent(cx: &mut TestAppContext) {
        let (root, palette, events, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        palette.update(cx, |palette, cx| {
            palette.set_confirm(Some(CommandPaletteConfirm::new("Add").disabled(true)), cx);
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();

        assert!(
            !events.borrow().contains(&CommandPaletteEvent::Confirmed),
            "a disabled confirm reported a confirmation"
        );
    }

    #[gpui::test]
    fn the_confirm_key_should_stay_unclaimed_without_a_confirm(cx: &mut TestAppContext) {
        let (root, palette, events, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();

        assert!(
            !events.borrow().contains(&CommandPaletteEvent::Confirmed),
            "a palette without a confirm claimed the confirm key"
        );
        assert!(
            palette.read_with(cx, |palette, _| palette.is_open()),
            "the confirm key closed a palette that had no confirm"
        );
    }

    #[gpui::test]
    fn footer_should_render_only_with_hints_or_an_actions_menu(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        assert!(
            cx.debug_bounds("command-palette-footer").is_none(),
            "a palette without hints or an actions menu rendered a footer"
        );

        palette.update(cx, |palette, cx| {
            palette.set_hints(vec![CommandPaletteHint::new("Open", "\u{21b5}")], cx);
        });
        cx.run_until_parked();

        assert!(
            cx.debug_bounds("command-palette-footer").is_some(),
            "a palette with hints did not render a footer"
        );
    }

    #[gpui::test]
    fn header_action_press_should_emit_its_caller_identity(cx: &mut TestAppContext) {
        let (root, palette, events, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        palette.update(cx, |palette, cx| {
            palette.set_header_actions(
                vec![
                    CommandPaletteAction::new("toggle-ignored", "Toggle ignored", |_| {
                        div().into_any_element()
                    })
                    .debug_selector("header-toggle-ignored"),
                ],
                cx,
            );
        });
        cx.run_until_parked();

        let button = cx
            .debug_bounds("header-toggle-ignored")
            .expect("the search-line control was not rendered");
        cx.simulate_click(button.center(), Modifiers::default());
        cx.run_until_parked();

        assert!(
            events
                .borrow()
                .contains(&CommandPaletteEvent::HeaderAction("toggle-ignored".into())),
            "the search-line control did not emit its caller identity: {:?}",
            events.borrow()
        );
        assert!(
            palette.read_with(cx, |palette, _| palette.is_open()),
            "pressing a search-line control closed the palette"
        );
    }

    #[gpui::test]
    fn actions_menu_should_take_focus_without_closing_the_palette(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        palette.update(cx, |palette, cx| {
            palette.set_actions_menu(
                vec![
                    MenuEntry::action("Copy path", SharedString::from("copy-path")),
                    MenuEntry::action("Reveal", SharedString::from("reveal")),
                ],
                cx,
            );
        });
        cx.run_until_parked();

        let trigger = cx
            .debug_bounds("command-palette-actions-menu")
            .expect("the actions menu trigger was not rendered");
        cx.simulate_click(trigger.center(), Modifiers::default());
        cx.run_until_parked();

        assert!(
            palette.read_with(cx, |palette, _| palette.is_open()),
            "opening the actions menu closed the palette"
        );
    }

    #[gpui::test]
    fn actions_menu_external_focus_should_close_the_palette(cx: &mut TestAppContext) {
        let (root, palette, events, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);
        let intruder = root.read_with(cx, |root, _| root.intruder_focus.clone());
        cx.update(|window, cx| {
            root.update(cx, |_, cx| {
                let intruder = intruder.clone();
                cx.subscribe_in(
                    &palette,
                    window,
                    move |_, _, event: &CommandPaletteEvent<u8>, window, _| {
                        if matches!(event, CommandPaletteEvent::MenuAction(_)) {
                            intruder.focus(window);
                        }
                    },
                )
                .detach();
            });
            palette.update(cx, |palette, cx| {
                palette.set_actions_menu(
                    vec![
                        MenuEntry::action(
                            "Focus external control",
                            SharedString::from("focus-external"),
                        )
                        .debug_selector("command-palette-focus-external"),
                        MenuEntry::action("Second action", SharedString::from("second")),
                    ],
                    cx,
                );
            });
        });
        cx.run_until_parked();

        let trigger = cx
            .debug_bounds("command-palette-actions-menu")
            .expect("the actions menu trigger was not rendered");
        cx.simulate_click(trigger.center(), Modifiers::default());
        cx.run_until_parked();
        let entry = cx
            .debug_bounds("command-palette-focus-external")
            .expect("the actions menu entry was not rendered");
        cx.simulate_click(entry.center(), Modifiers::default());
        cx.run_until_parked();

        assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
        assert!(cx.update(|window, _| intruder.is_focused(window)));
        assert!(events.borrow().contains(&CommandPaletteEvent::Lifecycle(
            CommandPaletteLifecycleEvent::Closed(CommandPaletteCloseReason::FocusLost)
        )));
    }

    #[gpui::test]
    fn section_boundaries_should_emit_one_heading_each(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        palette.update(cx, |palette, cx| {
            palette.set_items(
                vec![
                    CommandPaletteItem::new(1, "Recent One").section("Recent"),
                    CommandPaletteItem::new(2, "Recent Two").section("Recent"),
                    CommandPaletteItem::new(3, "All One").section("All"),
                ],
                cx,
            );
        });
        cx.run_until_parked();

        let rows = palette.read_with(cx, |palette, _| palette.presented_results.rows().to_vec());
        assert_eq!(
            rows,
            vec![
                PaletteRow::Section("Recent".into()),
                PaletteRow::Item {
                    position: 0,
                    single_line: true
                },
                PaletteRow::Item {
                    position: 1,
                    single_line: true
                },
                PaletteRow::Separator,
                PaletteRow::Section("All".into()),
                PaletteRow::Item {
                    position: 2,
                    single_line: true
                },
            ]
        );
    }

    #[test]
    fn scored_queries_should_preserve_contiguous_section_order() {
        let items = vec![
            CommandPaletteItem::new(1, "Open Alpha Workspace").section("Recent"),
            CommandPaletteItem::new(2, "Another Alpha Workspace").section("Recent"),
            CommandPaletteItem::new(3, "Alpha").section("All"),
            CommandPaletteItem::new(4, "Alpha Command").section("All"),
        ];
        let matches =
            match_command_palette_items(&items, "alpha", CommandPaletteMatching::Semantic);

        assert_eq!(
            matches
                .iter()
                .map(|matched| items[matched.item_index].section_text().unwrap_or_default())
                .collect::<Vec<_>>(),
            vec!["Recent", "Recent", "All", "All"],
        );
    }

    #[test]
    fn description_matches_should_report_their_own_highlight_ranges() {
        let items = vec![CommandPaletteItem::new(1, "Open").description("Choose a directory")];
        let matches =
            match_command_palette_items(&items, "directory", CommandPaletteMatching::Semantic);

        assert!(matches[0].label_highlights.is_empty());
        assert_eq!(matches[0].description_highlights, vec![9..18]);
    }

    #[gpui::test]
    fn escape_should_close_and_restore_exact_prior_focus(cx: &mut TestAppContext) {
        let (root, palette, events, _, cx) = palette_window(cx);
        let prior = open_palette(&root, &palette, cx);
        assert!(cx.update(|window, cx| { crate::tooltip::window_tooltips_suppressed(window, cx) }));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
        assert!(
            !cx.update(|window, cx| { crate::tooltip::window_tooltips_suppressed(window, cx) })
        );
        assert!(cx.update(|window, _| prior.is_focused(window)));
        assert!(events.borrow().contains(&CommandPaletteEvent::Lifecycle(
            CommandPaletteLifecycleEvent::Closed(CommandPaletteCloseReason::Escape)
        )));
    }

    #[gpui::test]
    fn escape_should_close_after_focus_moves_to_a_header_action(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        palette.update(cx, |palette, cx| {
            palette.set_header_actions(
                vec![CommandPaletteAction::new("refresh", "Refresh", |_| {
                    div().into_any_element()
                })],
                cx,
            );
        });
        cx.run_until_parked();
        open_palette(&root, &palette, cx);

        cx.update(|window, _| window.focus_next());
        cx.run_until_parked();
        assert!(!cx.update(|window, cx| {
            palette
                .read(cx)
                .input
                .read(cx)
                .focus_handle()
                .is_focused(window)
        }));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
    }

    #[gpui::test]
    fn command_period_should_close_and_restore_exact_prior_focus(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        let prior = open_palette(&root, &palette, cx);

        cx.simulate_keystrokes("cmd-.");
        cx.run_until_parked();

        assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
        assert!(cx.update(|window, _| prior.is_focused(window)));
    }

    #[gpui::test]
    fn navigation_should_wrap_and_skip_disabled_items(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        cx.simulate_keystrokes("ctrl-n ctrl-n ctrl-p");
        cx.run_until_parked();

        assert_eq!(
            palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
            Some(3)
        );
    }

    #[gpui::test]
    fn pointer_hover_should_stay_suppressed_until_the_pointer_moves(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);
        let close_row = cx.debug_bounds("row-close").unwrap_or_default().center();

        assert!(palette.read_with(cx, |palette, _| palette.pointer_suppressed));

        cx.simulate_mouse_move(close_row, None, Modifiers::default());
        cx.simulate_keystrokes("down");
        cx.run_until_parked();

        assert_eq!(
            palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
            Some(1)
        );
        assert!(palette.read_with(cx, |palette, _| palette.pointer_suppressed));

        cx.simulate_mouse_move(close_row, None, Modifiers::default());
        cx.run_until_parked();

        assert_eq!(
            palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
            Some(1)
        );
        assert!(palette.read_with(cx, |palette, _| palette.pointer_suppressed));

        cx.simulate_mouse_move(
            close_row + gpui::point(px(1.0), px(0.0)),
            None,
            Modifiers::default(),
        );
        cx.run_until_parked();

        assert_eq!(
            palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
            Some(3)
        );
        assert!(!palette.read_with(cx, |palette, _| palette.pointer_suppressed));
    }

    #[gpui::test]
    fn page_navigation_should_move_by_the_visible_result_count(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);
        palette.update(cx, |palette, cx| {
            palette.set_items(
                (0u8..32)
                    .map(|id| CommandPaletteItem::new(id, format!("Command {id}")))
                    .collect(),
                cx,
            );
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("pagedown");
        cx.run_until_parked();

        assert!(
            palette
                .read_with(cx, |palette, _| palette.selected_item_id().copied())
                .is_some_and(|selected| selected > 0)
        );
    }

    #[gpui::test]
    fn keyboard_navigation_should_reveal_results_beyond_the_initial_viewport(
        cx: &mut TestAppContext,
    ) {
        let (root, palette, _, _, cx) = palette_window(cx);
        palette.update(cx, |palette, cx| {
            palette.set_items(
                (0u8..32)
                    .map(|id| {
                        CommandPaletteItem::new(id, format!("Command {id}"))
                            .debug_selector(format!("row-{id}"))
                    })
                    .collect(),
                cx,
            );
        });
        open_palette(&root, &palette, cx);

        cx.simulate_keystrokes(&["down"; 12].join(" "));
        cx.run_until_parked();

        assert_eq!(
            palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
            Some(12)
        );
        assert!(
            cx.debug_bounds("row-12").is_some(),
            "keyboard navigation selected an offscreen result without revealing it"
        );
    }

    #[gpui::test]
    fn tab_navigation_should_reach_header_controls_and_return_to_the_query(
        cx: &mut TestAppContext,
    ) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);
        palette.update(cx, |palette, cx| {
            palette.set_header_actions(
                vec![CommandPaletteAction::new("refresh", "Refresh", |_| {
                    div().into_any_element()
                })],
                cx,
            );
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        assert!(!cx.update(|window, cx| {
            palette
                .read(cx)
                .input
                .read(cx)
                .focus_handle()
                .is_focused(window)
        }));

        cx.simulate_keystrokes("shift-tab x");
        cx.run_until_parked();

        assert!(palette.read_with(cx, |palette, _| palette.is_open()));
        assert!(cx.update(|window, cx| {
            palette
                .read(cx)
                .input
                .read(cx)
                .focus_handle()
                .is_focused(window)
        }));
        assert_eq!(
            palette.read_with(cx, |palette, _| palette.query().to_owned()),
            "x"
        );
    }

    #[gpui::test]
    fn shift_tab_from_query_should_reach_a_confirm_only_footer(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        palette.update(cx, |palette, cx| {
            palette.set_confirm(Some(CommandPaletteConfirm::new("Add")), cx);
        });
        open_palette(&root, &palette, cx);

        cx.simulate_keystrokes("shift-tab");
        cx.run_until_parked();

        assert!(!cx.update(|window, cx| palette.read(cx).editor_is_focused(window, cx)));

        cx.simulate_keystrokes("tab x");
        cx.run_until_parked();

        assert_eq!(
            palette.read_with(cx, |palette, _| palette.query().to_owned()),
            "x"
        );
    }

    #[gpui::test]
    fn preferred_item_should_seed_each_open_transition(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        palette.update(cx, |palette, cx| {
            palette.set_preferred_item(Some(3), cx);
        });

        open_palette(&root, &palette, cx);

        assert_eq!(
            palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
            Some(3)
        );
    }

    #[gpui::test]
    fn opening_should_reveal_a_preferred_item_below_the_initial_viewport(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        palette.update(cx, |palette, cx| {
            palette.set_items(
                (0u8..32)
                    .map(|id| {
                        CommandPaletteItem::new(id, format!("Command {id}"))
                            .debug_selector(format!("row-{id}"))
                    })
                    .collect(),
                cx,
            );
            palette.set_preferred_item(Some(31), cx);
        });

        open_palette(&root, &palette, cx);

        assert!(cx.debug_bounds("row-31").is_some());
    }

    #[gpui::test]
    fn loading_state_should_not_activate_a_hidden_stale_selection(cx: &mut TestAppContext) {
        let (root, palette, events, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);
        palette.update(cx, |palette, cx| palette.set_loading(true, cx));

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(palette.read_with(cx, |palette, _| palette.is_open()));
        assert!(
            !events
                .borrow()
                .iter()
                .any(|event| matches!(event, CommandPaletteEvent::Activated(_)))
        );
    }

    #[gpui::test]
    fn activation_should_restore_focus_then_emit_activation_before_final_close(
        cx: &mut TestAppContext,
    ) {
        let (root, palette, events, _, cx) = palette_window(cx);
        let prior = open_palette(&root, &palette, cx);
        events.borrow_mut().clear();

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(cx.update(|window, _| prior.is_focused(window)));
        assert_eq!(
            events.borrow().as_slice(),
            [
                CommandPaletteEvent::Activated(CommandPaletteActivation {
                    item_id: 1,
                    source: CommandPaletteActivationSource::Keyboard,
                }),
                CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Closed(
                    CommandPaletteCloseReason::Activated,
                )),
            ]
        );
    }

    #[gpui::test]
    fn pointer_press_and_release_must_belong_to_the_same_row(cx: &mut TestAppContext) {
        let (root, palette, events, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);
        let first = cx.debug_bounds("row-open").unwrap_or_default().center();
        let second = cx.debug_bounds("row-close").unwrap_or_default().center();

        cx.simulate_mouse_down(first, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(second, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(second, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        assert!(palette.read_with(cx, |palette, _| palette.is_open()));
        assert!(
            !events
                .borrow()
                .iter()
                .any(|event| matches!(event, CommandPaletteEvent::Activated(_)))
        );
    }

    #[gpui::test]
    fn programmatic_query_uses_the_inputs_normalized_bounded_value(cx: &mut TestAppContext) {
        let (_, palette, events, _, cx) = palette_window(cx);
        palette.update(cx, |palette, cx| palette.set_query("open\r\nwindow", cx));
        assert_eq!(
            palette.read_with(cx, |palette, _| palette.query().to_owned()),
            "open window"
        );
        events.borrow_mut().clear();

        palette.update(cx, |palette, cx| {
            palette.set_query("x".repeat(65 * 1024), cx)
        });

        assert_eq!(
            palette.read_with(cx, |palette, _| palette.query().to_owned()),
            "open window"
        );
        assert!(events.borrow().is_empty());
    }

    #[gpui::test]
    fn pointer_release_should_not_activate_a_row_removed_by_a_query_change(
        cx: &mut TestAppContext,
    ) {
        let (root, palette, events, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);
        cx.update(|window, cx| {
            palette.update(cx, |palette, cx| {
                palette.pointer_down(3, cx);
                palette.set_query("open", cx);
                if palette.pointer_up(&3, true) {
                    palette.selected = Some(3);
                    palette.activate_selected(CommandPaletteActivationSource::Pointer, window, cx);
                }
            });
        });

        assert!(palette.read_with(cx, |palette, _| palette.is_open()));
        assert!(
            !events
                .borrow()
                .iter()
                .any(|event| matches!(event, CommandPaletteEvent::Activated(_)))
        );
    }

    #[gpui::test]
    fn pointer_click_should_emit_typed_pointer_activation_for_any_visible_row(
        cx: &mut TestAppContext,
    ) {
        let (root, _, events, _, cx) = palette_window(cx);
        let palette = root.read_with(cx, |root, _| root.palette.clone());
        open_palette(&root, &palette, cx);
        let last = cx.debug_bounds("row-close").unwrap_or_default().center();

        cx.simulate_click(last, Modifiers::default());
        cx.run_until_parked();

        assert!(events.borrow().contains(&CommandPaletteEvent::Activated(
            CommandPaletteActivation {
                item_id: 3,
                source: CommandPaletteActivationSource::Pointer,
            }
        )));
    }

    #[gpui::test]
    fn focus_loss_should_dismiss_without_stealing_focus_from_the_new_owner(
        cx: &mut TestAppContext,
    ) {
        let (root, palette, events, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);
        let intruder = root.read_with(cx, |root, _| root.intruder_focus.clone());

        cx.update(|window, _| intruder.focus(window));
        cx.run_until_parked();

        assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
        assert!(cx.update(|window, _| intruder.is_focused(window)));
        assert!(events.borrow().contains(&CommandPaletteEvent::Lifecycle(
            CommandPaletteLifecycleEvent::Closed(CommandPaletteCloseReason::FocusLost)
        )));
    }

    #[gpui::test]
    fn replacement_should_close_without_restoring_the_displaced_owner(cx: &mut TestAppContext) {
        let (root, palette, events, _, cx) = palette_window(cx);
        let prior = open_palette(&root, &palette, cx);

        cx.update(|window, cx| {
            palette.update(cx, |palette, cx| {
                let _ = palette.dismiss_for_replacement(window, cx);
            });
        });
        cx.run_until_parked();

        assert!(!cx.update(|window, _| prior.is_focused(window)));
        assert!(events.borrow().contains(&CommandPaletteEvent::Lifecycle(
            CommandPaletteLifecycleEvent::Closed(CommandPaletteCloseReason::Replaced)
        )));
    }

    #[gpui::test]
    fn replacement_chain_should_restore_the_original_focus_owner(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        let prior = open_palette(&root, &palette, cx);

        let replacement = cx
            .update(|window, cx| {
                palette.update(cx, |palette, cx| {
                    palette.dismiss_for_replacement(window, cx)
                })
            })
            .expect("an open palette should transfer its restoration focus");
        cx.update(|window, cx| {
            palette.update(cx, |palette, cx| {
                palette.open_replacing(replacement, window, cx);
                palette.dismiss(window, cx);
            });
        });
        cx.run_until_parked();

        assert!(cx.update(|window, _| prior.is_focused(window)));
    }

    #[gpui::test]
    fn outside_press_should_close_without_reaching_underlay(cx: &mut TestAppContext) {
        let (root, palette, _, underlay, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);
        let panel = cx.debug_bounds("command-palette-panel").unwrap_or_default();
        let outside = point(panel.left() - px(8.0), panel.bottom() + px(8.0));

        cx.simulate_click(outside, Modifiers::default());
        cx.run_until_parked();

        assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
        assert_eq!(*underlay.borrow(), 0);
    }

    #[gpui::test]
    fn palette_wheel_events_should_not_reach_the_underlay(cx: &mut TestAppContext) {
        let (root, palette, _, underlay, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);
        let panel = cx.debug_bounds("command-palette-panel").unwrap_or_default();
        let outside = point(panel.left() - px(8.0), panel.bottom() + px(8.0));

        cx.simulate_event(ScrollWheelEvent {
            position: outside,
            delta: ScrollDelta::Pixels(point(px(0.0), px(-20.0))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();
        assert_eq!(*underlay.borrow(), 0);

        palette.update(cx, |palette, cx| palette.set_loading(true, cx));
        cx.run_until_parked();
        let status = cx
            .debug_bounds("command-palette-loading")
            .unwrap_or_default();
        cx.simulate_event(ScrollWheelEvent {
            position: status.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-20.0))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();

        assert_eq!(*underlay.borrow(), 0);
    }

    #[gpui::test]
    fn stable_selection_should_survive_query_and_item_refresh(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);
        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        palette.update(cx, |palette, cx| {
            palette.set_query("window", cx);
            palette.set_items(items(), cx);
        });

        assert_eq!(
            palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
            Some(3)
        );
    }

    #[gpui::test]
    fn stale_generation_results_should_be_ignored(cx: &mut TestAppContext) {
        let (_, palette, _, _, cx) = palette_window(cx);
        let first = palette.update(cx, |palette, cx| palette.refresh(cx));
        let second = palette.update(cx, |palette, cx| palette.refresh(cx));

        let applied = palette.update(cx, |palette, cx| {
            palette.apply_items(first, vec![CommandPaletteItem::new(9, "Stale")], cx)
        });

        assert!(!applied);
        assert_eq!(
            palette.read_with(cx, |palette, _| palette.generation()),
            second
        );
    }

    #[gpui::test]
    fn closed_palette_should_reject_results_from_the_dismissed_generation(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);
        let dismissed_generation = palette.read_with(cx, |palette, _| palette.generation());
        cx.update(|window, cx| {
            palette.update(cx, |palette, cx| {
                palette.dismiss(window, cx);
            });
        });

        let applied = palette.update(cx, |palette, cx| {
            palette.apply_items(
                dismissed_generation,
                vec![CommandPaletteItem::new(9, "Stale")],
                cx,
            )
        });

        assert!(!applied);
    }

    #[gpui::test]
    fn deactivation_should_dismiss_without_restoring_prior_focus(cx: &mut TestAppContext) {
        let (root, palette, events, _, cx) = palette_window(cx);
        let prior = open_palette(&root, &palette, cx);

        cx.deactivate_window();
        cx.run_until_parked();

        assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
        assert!(!cx.update(|window, _| prior.is_focused(window)));
        assert!(events.borrow().contains(&CommandPaletteEvent::Lifecycle(
            CommandPaletteLifecycleEvent::Closed(CommandPaletteCloseReason::Deactivated)
        )));

        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();

        assert!(cx.update(|window, _| prior.is_focused(window)));
    }
}
