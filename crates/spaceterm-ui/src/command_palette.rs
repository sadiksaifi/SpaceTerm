use std::{ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, AppContext as _, Corner, Entity, EventEmitter, Global, HitboxBehavior,
    InteractiveElement as _, IntoElement, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Pixels, Render, Rgba, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, UniformListScrollHandle,
    WeakEntity, WeakFocusHandle, Window, actions, anchored, canvas, deferred, div,
    prelude::FluentBuilder as _, px, uniform_list,
};

use crate::{TextInput, TextInputEvent, TextInputStyle};

const KEY_CONTEXT: &str = "SpaceTermCommandPalette";
const OVERLAY_PRIORITY: usize = 2;

actions!(
    spaceterm_command_palette,
    [MoveUp, MoveDown, MoveHome, MoveEnd]
);

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", MoveUp, Some(KEY_CONTEXT)),
        KeyBinding::new("down", MoveDown, Some(KEY_CONTEXT)),
        KeyBinding::new("home", MoveHome, Some(KEY_CONTEXT)),
        KeyBinding::new("end", MoveEnd, Some(KEY_CONTEXT)),
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

/// One typed semantic command-palette item.
///
/// Item identities must remain stable. Later items with a duplicate identity are discarded so
/// selection and pointer ownership always refer to exactly one row.
#[derive(Clone)]
pub struct CommandPaletteItem<I> {
    id: I,
    label: SharedString,
    description: Option<SharedString>,
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
}

fn match_command_palette_items<I>(
    items: &[CommandPaletteItem<I>],
    query: &str,
) -> Vec<CommandPaletteMatch> {
    let tokens: Vec<Vec<char>> = query
        .split_whitespace()
        .map(lowercase_chars)
        .filter(|token| !token.is_empty())
        .collect();
    if tokens.is_empty() {
        return items
            .iter()
            .enumerate()
            .map(|(item_index, _)| CommandPaletteMatch {
                item_index,
                score: 0,
                label_highlights: Vec::new(),
            })
            .collect();
    }

    let mut matches = items
        .iter()
        .enumerate()
        .filter_map(|(item_index, item)| {
            match_item(item, &tokens).map(|(score, label_highlights)| CommandPaletteMatch {
                item_index,
                score,
                label_highlights,
            })
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
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

fn match_item<I>(
    item: &CommandPaletteItem<I>,
    tokens: &[Vec<char>],
) -> Option<(i64, Vec<Range<usize>>)> {
    let label_units = search_units(item.label.as_ref());
    let description_units = item.description.as_ref().map(|text| search_units(text));
    let keyword_units: Vec<_> = item
        .keywords
        .iter()
        .map(|keyword| search_units(keyword))
        .collect();
    let mut score = 0;
    let mut highlights = Vec::new();

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
        if field == 0 {
            highlights.extend(matched.ranges);
        }
    }

    highlights.sort_by_key(|range| (range.start, range.end));
    Some((score, merge_ranges(highlights)))
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
    foreground: Rgba,
    muted: Rgba,
    disabled: Rgba,
    selected_background: Rgba,
    selected_foreground: Rgba,
    match_foreground: Rgba,
    input_background: Rgba,
    input_border: Rgba,
    input_selection: Rgba,
    caret: Rgba,
}

impl CommandPalettePaint {
    /// Creates the complete bounded paint catalog.
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
        input_background: Rgba,
        input_border: Rgba,
        input_selection: Rgba,
        caret: Rgba,
    ) -> Self {
        Self {
            background,
            border,
            foreground,
            muted,
            disabled,
            selected_background,
            selected_foreground,
            match_foreground,
            input_background,
            input_border,
            input_selection,
            caret,
        }
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
    horizontal_padding: Pixels,
    leading_width: Pixels,
    gap: Pixels,
    corner_radius: Pixels,
    border_width: Pixels,
    label_size: Pixels,
    secondary_size: Pixels,
}

impl CommandPaletteMetrics {
    /// Creates compact native defaults around a panel width and row height.
    pub fn new(panel_width: Pixels, row_height: Pixels) -> Self {
        Self {
            panel_width,
            maximum_height: px(420.0),
            top_offset: px(64.0),
            viewport_margin: px(16.0),
            panel_padding: px(6.0),
            input_height: px(38.0),
            row_height,
            horizontal_padding: px(10.0),
            leading_width: px(18.0),
            gap: px(8.0),
            corner_radius: px(10.0),
            border_width: px(1.0),
            label_size: px(13.0),
            secondary_size: px(11.0),
        }
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

    /// Sets panel corner radius and stable border width.
    pub fn panel_shape(mut self, corner_radius: Pixels, border_width: Pixels) -> Self {
        self.corner_radius = corner_radius;
        self.border_width = border_width;
        self
    }

    /// Sets primary and secondary row font sizes.
    pub fn font_sizes(mut self, label: Pixels, secondary: Pixels) -> Self {
        self.label_size = label;
        self.secondary_size = secondary;
        self
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
    items: Vec<CommandPaletteItem<I>>,
    matches: Vec<CommandPaletteMatch>,
    selected: Option<I>,
    preferred: Option<I>,
    query: String,
    generation: CommandPaletteGeneration,
    loading: bool,
    open: bool,
    input: Entity<TextInput>,
    restore_focus: Option<WeakFocusHandle>,
    restore_on_activation: Option<WeakFocusHandle>,
    pointer_press: Option<I>,
    scroll_handle: UniformListScrollHandle,
    _input_subscription: Subscription,
}

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
        let paint = cx.global::<CommandPaletteTheme>().paint;
        let input_placeholder = placeholder.clone();
        let input = cx.new(|cx| {
            TextInput::new(
                "",
                TextInputStyle::new(
                    paint.foreground.into(),
                    paint.muted.into(),
                    paint.input_selection.into(),
                    paint.caret.into(),
                ),
                window,
                cx,
            )
            .placeholder(input_placeholder)
        });
        let input_subscription = cx.subscribe_in(
            &input,
            window,
            |palette, _, event: &TextInputEvent, window, cx| match event {
                TextInputEvent::Changed(query) => palette.update_query(query.clone(), cx),
                TextInputEvent::Submitted(_) => {
                    palette.activate_selected(CommandPaletteActivationSource::Keyboard, window, cx)
                }
                TextInputEvent::Cancelled => {
                    palette.close(CommandPaletteCloseReason::Escape, window, cx);
                }
                TextInputEvent::Blurred(_) if palette.open => {
                    palette.close(CommandPaletteCloseReason::FocusLost, window, cx);
                }
                TextInputEvent::Blurred(_) => {}
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
        let items = unique_items(items);
        let matches = match_command_palette_items(&items, "");
        let selected = first_enabled_id(&items, &matches);
        Self {
            no_results_text: "No matching items".into(),
            items,
            matches,
            selected,
            preferred: None,
            query: String::new(),
            generation: CommandPaletteGeneration::default(),
            loading: false,
            open: false,
            input,
            restore_focus: None,
            restore_on_activation: None,
            pointer_press: None,
            scroll_handle: UniformListScrollHandle::new(),
            _input_subscription: input_subscription,
        }
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

    /// Opens the palette, captures the exact prior focus owner, and focuses its editor.
    ///
    /// Returns `true` only for an actual closed-to-open transition.
    pub fn open(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> bool {
        if self.open {
            self.input.read(cx).focus_handle().focus(window);
            return false;
        }
        self.restore_on_activation = None;
        self.restore_focus = match crate::menu::dismiss_active_menu_for_replacement(window, cx) {
            Some(crate::menu::MenuReplacementFocus(focus)) => focus,
            None => window.focused(cx).map(|focus| focus.downgrade()),
        };
        self.open = true;
        self.pointer_press = None;
        self.selected = None;
        if !self.query.is_empty() {
            self.input.update(cx, |input, cx| input.set_value("", cx));
            self.query.clear();
            self.recompute_matches();
        }
        self.repair_selection();
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

    /// Closes an open palette before another transient takes focus.
    pub fn dismiss_for_replacement(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        self.close(CommandPaletteCloseReason::Replaced, window, cx)
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
        self.items = unique_items(items);
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
        let query = query.into();
        self.input
            .update(cx, |input, cx| input.set_value(query.clone(), cx));
        self.update_query(query, cx);
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
        self.matches = match_command_palette_items(&self.items, &self.query);
        self.repair_selection();
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

    fn move_selection(&mut self, delta: isize, cx: &mut gpui::Context<Self>) {
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

    fn move_edge(&mut self, first: bool, cx: &mut gpui::Context<Self>) {
        let enabled = self.enabled_match_positions();
        let position = if first {
            enabled.first().copied()
        } else {
            enabled.last().copied()
        };
        if let Some(position) = position {
            self.select_match_position(position, cx);
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
            self.scroll_handle
                .scroll_to_item(position, ScrollStrategy::Center);
            cx.notify();
        }
    }

    fn pointer_hover(&mut self, id: &I, cx: &mut gpui::Context<Self>) {
        let position = self.matches.iter().position(|matched| {
            self.items
                .get(matched.item_index)
                .is_some_and(|item| !item.disabled && item.id == *id)
        });
        if let Some(position) = position {
            self.select_match_position(position, cx);
        }
    }

    fn pointer_down(&mut self, id: I) {
        self.pointer_press = Some(id);
    }

    fn pointer_up(&mut self, id: &I, inside: bool) -> bool {
        if self.pointer_press.as_ref() != Some(id) {
            return false;
        }
        self.pointer_press = None;
        inside
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
        let enabled = self
            .items
            .iter()
            .any(|item| item.id == item_id && !item.disabled);
        if !enabled {
            return;
        }
        self.close(CommandPaletteCloseReason::Activated, window, cx);
        cx.emit(CommandPaletteEvent::Activated(CommandPaletteActivation {
            item_id,
            source,
        }));
    }

    fn close(
        &mut self,
        reason: CommandPaletteCloseReason,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if !self.open {
            return false;
        }
        self.open = false;
        self.loading = false;
        self.pointer_press = None;
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
        cx.emit(CommandPaletteEvent::Lifecycle(
            CommandPaletteLifecycleEvent::Closed(reason),
        ));
        cx.notify();
        true
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
        let viewport = window.viewport_size();
        let available_width = (viewport.width - theme.metrics.viewport_margin * 2.0).max(px(0.0));
        let panel_width = theme.metrics.panel_width.min(available_width);
        let left = ((viewport.width - panel_width) / 2.0).max(px(0.0));
        let top = theme
            .metrics
            .top_offset
            .min((viewport.height - theme.metrics.viewport_margin).max(px(0.0)));
        let result_rows = if self.loading || self.matches.is_empty() {
            1
        } else {
            self.matches.len()
        };
        let desired_height = theme.metrics.panel_padding * 3.0
            + theme.metrics.input_height
            + theme.metrics.row_height * result_rows;
        let available_height = (viewport.height - top - theme.metrics.viewport_margin).max(px(0.0));
        let panel_height = desired_height
            .min(theme.metrics.maximum_height)
            .min(available_height);

        let panel_bounds = gpui::Bounds::new(
            gpui::point(left, top),
            gpui::size(panel_width, panel_height),
        );
        let outside = self.render_outside_tracker(panel_bounds, cx);
        let panel = self.render_panel(panel_width, panel_height, theme, cx);
        let overlay = div()
            .relative()
            .w(viewport.width)
            .h(viewport.height)
            .key_context(KEY_CONTEXT)
            .child(outside)
            .child(div().absolute().left(left).top(top).child(panel))
            .on_action(cx.listener(|palette, _: &MoveUp, _, cx| {
                palette.move_selection(-1, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|palette, _: &MoveDown, _, cx| {
                palette.move_selection(1, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|palette, _: &MoveHome, _, cx| {
                palette.move_edge(true, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|palette, _: &MoveEnd, _, cx| {
                palette.move_edge(false, cx);
                cx.stop_propagation();
            }));

        deferred(
            anchored()
                .anchor(Corner::TopLeft)
                .position(gpui::point(px(0.0), px(0.0)))
                .snap_to_window()
                .child(overlay),
        )
        .with_priority(OVERLAY_PRIORITY)
        .into_any_element()
    }
}

impl<I: Clone + Eq + 'static> CommandPalette<I> {
    fn render_outside_tracker(
        &self,
        panel_bounds: gpui::Bounds<Pixels>,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let palette = cx.entity().downgrade();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.capture() || panel_bounds.contains(&event.position) {
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
        theme: CommandPaletteTheme,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let paint = theme.paint;
        let metrics = theme.metrics;
        let input = div()
            .h(metrics.input_height)
            .mx(metrics.panel_padding)
            .mt(metrics.panel_padding)
            .px(metrics.horizontal_padding)
            .flex()
            .items_center()
            .rounded((metrics.corner_radius - metrics.panel_padding).max(px(0.0)))
            .border(metrics.border_width)
            .border_color(paint.input_border)
            .bg(paint.input_background)
            .text_size(metrics.label_size)
            .child(self.input.clone());

        let content = if self.loading {
            status_row("Loading…", "command-palette-loading", metrics, paint).into_any_element()
        } else if self.matches.is_empty() {
            status_row(
                self.no_results_text.clone(),
                "command-palette-no-results",
                metrics,
                paint,
            )
            .into_any_element()
        } else {
            let items = Rc::new(self.items.clone());
            let matches = Rc::new(self.matches.clone());
            let selected = self.selected.clone();
            let palette = cx.entity().downgrade();
            uniform_list(
                "command-palette-results",
                matches.len(),
                move |range, _, _| {
                    range
                        .filter_map(|position| {
                            let matched = matches.get(position)?.clone();
                            let item = items.get(matched.item_index)?.clone();
                            Some(render_row(
                                palette.clone(),
                                position,
                                item.clone(),
                                matched.label_highlights,
                                selected.as_ref() == Some(&item.id),
                                theme,
                            ))
                        })
                        .collect::<Vec<_>>()
                },
            )
            .track_scroll(self.scroll_handle.clone())
            .h((height - metrics.input_height - metrics.panel_padding * 3.0).max(px(0.0)))
            .into_any_element()
        };

        div()
            .debug_selector(|| "command-palette-panel".to_owned())
            .w(width)
            .h(height)
            .overflow_hidden()
            .rounded(metrics.corner_radius)
            .shadow_lg()
            .border(metrics.border_width)
            .border_color(paint.border)
            .bg(paint.background)
            .block_mouse_except_scroll()
            .child(input)
            .child(div().h(metrics.panel_padding))
            .child(content)
            .into_any_element()
    }
}

fn status_row(
    text: impl Into<SharedString>,
    debug_selector: &'static str,
    metrics: CommandPaletteMetrics,
    paint: CommandPalettePaint,
) -> impl IntoElement {
    div()
        .debug_selector(move || debug_selector.to_owned())
        .h(metrics.row_height)
        .px(metrics.horizontal_padding + metrics.panel_padding)
        .flex()
        .items_center()
        .text_size(metrics.secondary_size)
        .text_color(paint.muted)
        .child(text.into())
}

fn render_row<I: Clone + Eq + 'static>(
    palette: WeakEntity<CommandPalette<I>>,
    position: usize,
    item: CommandPaletteItem<I>,
    highlights: Vec<Range<usize>>,
    selected: bool,
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
    } else if selected {
        paint.selected_foreground
    } else {
        paint.muted
    };
    let match_foreground = if selected {
        paint.selected_foreground
    } else {
        paint.match_foreground
    };
    let logical_name = item.label.clone();
    let debug_selector = item.debug_selector.clone();
    let id = item.id.clone();
    let hover_palette = palette.clone();
    let mut row = div()
        .id(("command-palette-row", position))
        .debug_selector(move || debug_selector.unwrap_or_else(|| logical_name.to_string()))
        .relative()
        .h(metrics.row_height)
        .mx(metrics.panel_padding)
        .px(metrics.horizontal_padding)
        .flex()
        .items_center()
        .gap(metrics.gap)
        .rounded((metrics.corner_radius - metrics.panel_padding).max(px(0.0)))
        .text_color(foreground)
        .cursor_default()
        .when(selected, |row| row.bg(paint.selected_background))
        .when(!item.disabled, |row| {
            let id = id.clone();
            row.on_hover(move |hovered, _, cx| {
                if *hovered {
                    let _ = hover_palette.update(cx, |palette, cx| palette.pointer_hover(&id, cx));
                }
            })
        });

    let mut leading = div()
        .w(metrics.leading_width)
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center();
    if let Some(icon) = item.leading_icon.clone() {
        leading = leading.child(icon(foreground));
    }
    let label = highlighted_label(
        item.label.clone(),
        &highlights,
        foreground,
        match_foreground,
    );
    let text = div()
        .min_w_0()
        .flex_grow()
        .flex()
        .flex_col()
        .justify_center()
        .child(label)
        .when_some(item.description.clone(), |text, description| {
            text.child(
                div()
                    .truncate()
                    .text_size(metrics.secondary_size)
                    .text_color(secondary)
                    .child(description),
            )
        });
    row = row.child(leading).child(text);
    if let Some(accessory) = item.trailing.clone() {
        row = row.child(render_accessory(
            accessory,
            secondary,
            paint.selected_background,
            metrics,
        ));
    }

    if !item.disabled {
        let down_palette = palette.clone();
        let up_palette = palette;
        let down_id = item.id.clone();
        let up_id = item.id;
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
                        let _ = down_palette.update(cx, |palette, _| palette.pointer_down(id));
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
    row.into_any_element()
}

fn highlighted_label(
    label: SharedString,
    ranges: &[Range<usize>],
    foreground: Rgba,
    highlight: Rgba,
) -> AnyElement {
    let text = label.as_ref();
    let mut content = div()
        .min_w_0()
        .flex()
        .items_center()
        .truncate()
        .text_color(foreground);
    let mut cursor = 0;
    for range in ranges {
        if range.start > cursor {
            content = content.child(text[cursor..range.start].to_owned());
        }
        content = content.child(
            div()
                .text_color(highlight)
                .child(text[range.clone()].to_owned()),
        );
        cursor = range.end;
    }
    if cursor < text.len() {
        content = content.child(text[cursor..].to_owned());
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
            .px(px(5.0))
            .py(px(2.0))
            .rounded(px(4.0))
            .bg(status_background)
            .text_size(metrics.secondary_size)
            .text_color(color)
            .child(text)
            .into_any_element(),
        CommandPaletteAccessory::Checkmark => div()
            .flex_shrink_0()
            .text_size(metrics.secondary_size)
            .text_color(color)
            .child("✓")
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{
        Context, Entity, FocusHandle, Modifiers, Render, TestAppContext, VisualTestContext, Window,
        point, rgba,
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
                rgba(0x141415ff),
                rgba(0x405065ff),
                rgba(0x6e94b266),
                rgba(0xcdcdcdff),
            ),
            CommandPaletteMetrics::new(px(420.0), px(40.0)).panel_geometry(px(260.0), px(24.0)),
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

    #[test]
    fn empty_query_should_preserve_provider_order() {
        let matches = match_command_palette_items(&items(), "");

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
            match_command_palette_items(&items, "directory")[0].item_index,
            0
        );
        assert_eq!(
            match_command_palette_items(&items, "remove")[0].item_index,
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
            match_command_palette_items(&items, "window")[0].item_index,
            1
        );
    }

    #[test]
    fn unicode_highlights_should_remain_valid_label_boundaries() {
        let items = vec![CommandPaletteItem::new(1, "Éclair 🔍")];
        let matches = match_command_palette_items(&items, "é🔍");
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

        assert_eq!(match_command_palette_items(&items, "open project").len(), 1);
    }

    struct TestRoot {
        palette: Entity<CommandPalette<u8>>,
        other_focus: FocusHandle,
        intruder_focus: FocusHandle,
        underlay_presses: Rc<RefCell<usize>>,
    }

    impl Render for TestRoot {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let underlay = self.underlay_presses.clone();
            div()
                .relative()
                .size_full()
                .on_mouse_down(MouseButton::Left, move |_, _, _| {
                    *underlay.borrow_mut() += 1;
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

    fn palette_window(cx: &mut TestAppContext) -> PaletteWindow<'_> {
        cx.set_global(test_theme());
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
    fn escape_should_close_and_restore_exact_prior_focus(cx: &mut TestAppContext) {
        let (root, palette, events, _, cx) = palette_window(cx);
        let prior = open_palette(&root, &palette, cx);

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(!palette.read_with(cx, |palette, _| palette.is_open()));
        assert!(cx.update(|window, _| prior.is_focused(window)));
        assert!(events.borrow().contains(&CommandPaletteEvent::Lifecycle(
            CommandPaletteLifecycleEvent::Closed(CommandPaletteCloseReason::Escape)
        )));
    }

    #[gpui::test]
    fn navigation_should_wrap_and_skip_disabled_items(cx: &mut TestAppContext) {
        let (root, palette, _, _, cx) = palette_window(cx);
        open_palette(&root, &palette, cx);

        cx.simulate_keystrokes("down down up");
        cx.run_until_parked();

        assert_eq!(
            palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
            Some(3)
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
    fn return_should_emit_typed_keyboard_activation(cx: &mut TestAppContext) {
        let (root, palette, events, _, cx) = palette_window(cx);
        let prior = open_palette(&root, &palette, cx);

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(events.borrow().contains(&CommandPaletteEvent::Activated(
            CommandPaletteActivation {
                item_id: 1,
                source: CommandPaletteActivationSource::Keyboard,
            }
        )));
        assert!(cx.update(|window, _| prior.is_focused(window)));
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
                palette.dismiss_for_replacement(window, cx);
            });
        });
        cx.run_until_parked();

        assert!(!cx.update(|window, _| prior.is_focused(window)));
        assert!(events.borrow().contains(&CommandPaletteEvent::Lifecycle(
            CommandPaletteLifecycleEvent::Closed(CommandPaletteCloseReason::Replaced)
        )));
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
