//! A controlled, searchable, single-value selector with an anchored popup.
//!
//! The Module owns provisional navigation and popup lifecycle. Callers own the committed value and
//! receive acceptance only after the popup has closed. [`TextInput`] owns editing, clipboard,
//! grapheme, and input-method behavior. GPUI 0.2.2 does not expose listbox roles or active-option
//! relationships for ordinary elements, so this Module retains those facts without claiming native
//! assistive-technology publication.

use std::{cell::RefCell, collections::HashMap, ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, AppContext as _, BorrowAppContext as _, Bounds, Corner, ElementId, Entity,
    FocusHandle, Global, HitboxBehavior, InteractiveElement as _, IntoElement, KeyBinding,
    KeyDownEvent, ListAlignment, ListOffset, ListState, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, RenderOnce, Rgba, SharedString, Size,
    StatefulInteractiveElement as _, Styled as _, StyledText, Subscription, WeakEntity,
    WeakFocusHandle, Window, WindowId, actions, anchored, canvas, div, list,
    prelude::FluentBuilder as _, px, size,
};

use crate::{
    FloatingRole, FloatingShell, Icon, IconName, TextInput, TextInputEvent,
    TextInputHomeEndBehavior, TextInputTabBehavior, TextInputVariant,
    anchored_placement::{
        AnchoredPlacementConfig, AnchoredTextDirection, constrain_anchored_size, place_anchored,
    },
    fuzzy::{FuzzyTarget, fuzzy_filter, highlight_ranges},
    tooltip::{Tooltip, TooltipTargetVisibility},
};

const KEY_CONTEXT: &str = "SpaceTermComboBox";
/// The open popup is an anchored popup, like the menus it sits beside.
const COMBO_BOX_ROLE: FloatingRole = FloatingRole::Popover;
const MENU_MINIMUM_WIDTH: f32 = 240.0;
const MENU_MAXIMUM_WIDTH: f32 = 420.0;
const MENU_SHORTCUT_GAP: f32 = 24.0;

actions!(
    spaceterm_combo_box,
    [
        MoveUp,
        MoveDown,
        MovePageUp,
        MovePageDown,
        MoveHome,
        MoveEnd,
        Accept,
        Dismiss
    ]
);

/// Platform-specific ComboBox key equivalents layered over the portable bindings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComboBoxKeybindingProfile {
    /// Conventional macOS Control-N and Control-P navigation. Selecting this profile is explicit
    /// and performs no operating-system detection.
    MacOs,
}

/// Installs the platform-specific key equivalents for `profile`.
///
/// Applications explicitly install the portable navigation, acceptance, and dismissal bindings
/// before calling this function. Both sets remain scoped to an open ComboBox.
pub fn install_combo_box_keybindings(cx: &mut App, profile: ComboBoxKeybindingProfile) {
    match profile {
        ComboBoxKeybindingProfile::MacOs => cx.bind_keys([
            KeyBinding::new("ctrl-p", MoveUp, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-n", MoveDown, Some(KEY_CONTEXT)),
        ]),
    }
}

/// Installs platform-neutral ComboBox navigation, acceptance, and dismissal bindings.
pub fn install_portable_combo_box_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", MoveUp, Some(KEY_CONTEXT)),
        KeyBinding::new("down", MoveDown, Some(KEY_CONTEXT)),
        KeyBinding::new("pageup", MovePageUp, Some(KEY_CONTEXT)),
        KeyBinding::new("pagedown", MovePageDown, Some(KEY_CONTEXT)),
        KeyBinding::new("home", MoveHome, Some(KEY_CONTEXT)),
        KeyBinding::new("end", MoveEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", Accept, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT)),
    ]);
}

pub(crate) fn init(cx: &mut App) {
    if !cx.has_global::<ComboBoxCoordinator>() {
        cx.set_global(ComboBoxCoordinator::default());
    }
}

/// The input path that accepted a ComboBox item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComboBoxActivationSource {
    /// Return accepted the provisional item.
    Keyboard,
    /// A primary pointer press and release on the same row accepted it.
    Pointer,
}

/// A typed ComboBox acceptance delivered after popup closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComboBoxAcceptance<I> {
    item_id: I,
    source: ComboBoxActivationSource,
}

impl<I> ComboBoxAcceptance<I> {
    /// Returns the caller-owned stable item identity.
    pub fn item_id(&self) -> &I {
        &self.item_id
    }

    /// Returns the input path that accepted the item.
    pub fn source(&self) -> ComboBoxActivationSource {
        self.source
    }

    /// Consumes the event and returns its caller-owned identity.
    pub fn into_item_id(self) -> I {
        self.item_id
    }
}

/// Why an open ComboBox popup closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComboBoxCloseReason {
    /// An enabled provisional item was accepted.
    Accepted,
    /// Escape dismissed the popup.
    Escape,
    /// A pointer press outside the trigger and popup dismissed it.
    Outside,
    /// Activating the already-open trigger toggled the popup closed.
    Trigger,
    /// Focus moved outside the control.
    FocusLost,
    /// Tab or Shift-Tab continued focus traversal.
    TabTraversal,
    /// Another ComboBox in the same Operating-System Window replaced it.
    Replaced,
    /// The trigger disappeared while its popup was open.
    TargetDisappeared,
    /// Synchronization disabled the open control.
    Disabled,
    /// The Operating-System Window deactivated.
    Deactivated,
}

/// One exact ComboBox lifecycle transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComboBoxLifecycleEvent {
    /// The popup became open.
    Opened,
    /// The popup became closed for the supplied reason.
    Closed(ComboBoxCloseReason),
}

/// Standardized semantic content at a row's trailing edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComboBoxAccessory {
    /// Secondary explanatory text.
    Text(SharedString),
    /// Compact status text such as `Unavailable`.
    Status(SharedString),
    /// A display-only keyboard equivalent.
    Shortcut(SharedString),
}

/// Application-owned copy used by the searchable popup.
///
/// Keeping these strings outside the Module lets products localize status and editor text without
/// replacing any ComboBox behavior.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComboBoxCopy {
    filter_name: SharedString,
    filter_placeholder: SharedString,
    busy_status: SharedString,
    empty_status: SharedString,
}

impl ComboBoxCopy {
    /// Creates the complete bounded copy catalog.
    pub fn new(
        filter_name: impl Into<SharedString>,
        filter_placeholder: impl Into<SharedString>,
        busy_status: impl Into<SharedString>,
        empty_status: impl Into<SharedString>,
    ) -> Self {
        Self {
            filter_name: filter_name.into(),
            filter_placeholder: filter_placeholder.into(),
            busy_status: busy_status.into(),
            empty_status: empty_status.into(),
        }
    }
}

impl Default for ComboBoxCopy {
    fn default() -> Self {
        Self::new(
            "Filter options",
            "Search",
            "Loading\u{2026}",
            "No matching options",
        )
    }
}

type IconBuilder = Rc<dyn Fn(Rgba, Pixels) -> AnyElement>;
type InputIconBuilder = Rc<dyn Fn(Pixels) -> AnyElement>;

/// One typed semantic ComboBox item.
///
/// Identities must remain stable. Later duplicate identities are discarded so provisional
/// selection and pointer ownership always refer to exactly one row.
#[derive(Clone)]
pub struct ComboBoxItem<I> {
    id: I,
    label: SharedString,
    description: Option<SharedString>,
    keywords: Vec<SharedString>,
    disabled: bool,
    leading_icon: Option<IconBuilder>,
    trailing: Option<ComboBoxAccessory>,
    shortcut: Option<SharedString>,
    debug_selector: Option<String>,
    #[cfg(feature = "appearance-exerciser")]
    preview_selected: bool,
}

impl<I> ComboBoxItem<I> {
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
            shortcut: None,
            debug_selector: None,
            #[cfg(feature = "appearance-exerciser")]
            preview_selected: false,
        }
    }

    /// Adds one line of secondary descriptive text.
    pub fn description(mut self, value: impl Into<SharedString>) -> Self {
        self.description = Some(value.into());
        self
    }

    /// Replaces non-presentational strings considered by search.
    pub fn keywords(mut self, values: impl IntoIterator<Item = impl Into<SharedString>>) -> Self {
        self.keywords = values.into_iter().map(Into::into).collect();
        self
    }

    /// Controls whether the item remains visible but is skipped by navigation and activation.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Paints this row as persistently selected in development acceptance fixtures.
    ///
    /// This does not make a disabled item eligible for navigation or activation.
    #[cfg(feature = "appearance-exerciser")]
    pub fn preview_selected(mut self, selected: bool) -> Self {
        self.preview_selected = selected;
        self
    }

    /// Adds a bounded leading icon built with the resolved row foreground color and live size.
    pub fn leading_icon(mut self, build: impl Fn(Rgba, Pixels) -> AnyElement + 'static) -> Self {
        self.leading_icon = Some(Rc::new(build));
        self
    }

    /// Adds standardized semantic content at the trailing edge.
    pub fn trailing(mut self, accessory: ComboBoxAccessory) -> Self {
        self.trailing = Some(accessory);
        self
    }

    /// Adds a display-only keyboard equivalent after any trailing accessory.
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Adds a stable selector used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    /// Returns the stable caller-owned identity.
    pub fn id(&self) -> &I {
        &self.id
    }

    /// Returns the primary label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the secondary description, when present.
    pub fn description_text(&self) -> Option<&str> {
        self.description.as_ref().map(AsRef::as_ref)
    }

    /// Returns the display-only keyboard equivalent, when present.
    pub fn shortcut_text(&self) -> Option<&str> {
        self.shortcut.as_ref().map(AsRef::as_ref)
    }

    /// Returns whether the item is visible but inert.
    pub const fn is_disabled(&self) -> bool {
        self.disabled
    }

    fn paints_selected(&self, provisional: bool) -> bool {
        #[cfg(feature = "appearance-exerciser")]
        if self.preview_selected {
            return true;
        }
        provisional
    }
}

/// Query-aware rows accepted through the ordinary typed ComboBox callback.
///
/// Providers receive the editor's exact text, including case and whitespace. Fallback rows are
/// never filtered and ordinary matches retain initial-selection precedence.
#[derive(Clone)]
pub struct ComboBoxFallback<I>(ComboBoxFallbackProvider<I>);

type FallbackRowsProvider<I> = Rc<dyn Fn(&str) -> Vec<ComboBoxItem<I>>>;

#[derive(Clone)]
enum ComboBoxFallbackProvider<I> {
    Pinned(FallbackRowsProvider<I>),
    NoMatches(FallbackRowsProvider<I>),
}

impl<I> ComboBoxFallback<I> {
    /// Pins one row after ordinary matches, including when the query is empty.
    pub fn new(provider: impl Fn(&str) -> ComboBoxItem<I> + 'static) -> Self {
        Self::pinned_rows(move |query| vec![provider(query)])
    }

    /// Pins rows in provider order after ordinary matches, including for an empty query.
    pub fn pinned_rows(provider: impl Fn(&str) -> Vec<ComboBoxItem<I>> + 'static) -> Self {
        Self(ComboBoxFallbackProvider::Pinned(Rc::new(provider)))
    }

    /// Shows rows only when a non-whitespace query has no ordinary matches.
    pub fn when_no_matches(provider: impl Fn(&str) -> Vec<ComboBoxItem<I>> + 'static) -> Self {
        Self(ComboBoxFallbackProvider::NoMatches(Rc::new(provider)))
    }

    fn items(&self, query: &str, ordinary_match_count: usize) -> Vec<ComboBoxItem<I>> {
        match &self.0 {
            ComboBoxFallbackProvider::Pinned(provider) => provider(query),
            ComboBoxFallbackProvider::NoMatches(provider)
                if ordinary_match_count == 0 && !query.trim().is_empty() =>
            {
                provider(query)
            }
            ComboBoxFallbackProvider::NoMatches(_) => Vec::new(),
        }
    }
}

/// A weak handle for opening or accepting one rendered ComboBox from an application action.
///
/// Attach the same handle on every render. It neither retains a removed control nor opens a
/// control in another Operating-System Window.
#[derive(Clone)]
pub struct ComboBoxHandle<I: Clone + Eq + 'static> {
    state: Rc<RefCell<Option<WeakEntity<ComboBoxState<I>>>>>,
}

impl<I: Clone + Eq + 'static> Default for ComboBoxHandle<I> {
    fn default() -> Self {
        Self {
            state: Rc::new(RefCell::new(None)),
        }
    }
}

impl<I: Clone + Eq + 'static> ComboBoxHandle<I> {
    /// Accepts the first enabled, visible item matching an application choice, independent of
    /// the highlighted row. Uses the ordinary keyboard acceptance and focus lifecycle.
    /// Returns false without changing selection if the popup or choice is unavailable.
    pub fn accept_matching(
        &self,
        matches: impl Fn(&I) -> bool,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let state = self.state.borrow().clone();
        state.is_some_and(|state| {
            state
                .update(cx, |state, cx| {
                    if state.window_id != window.window_handle().window_id()
                        || !state.open
                        || state.disabled
                        || state.busy
                        || !state.popup_focus.contains_focused(window, cx)
                        || crate::modal::current_modal_parent(window, cx)
                            != crate::modal::modal_parent_for_focus(
                                &state.trigger_focus,
                                window,
                                cx,
                            )
                        || crate::menu::window_menu_is_open(window, cx)
                    {
                        return false;
                    }
                    let item_id = state
                        .matches
                        .iter()
                        .filter_map(|index| state.presented_items.get(*index))
                        .find(|item| !item.disabled && matches(&item.id))
                        .map(|item| item.id.clone());
                    let Some(item_id) = item_id else {
                        return false;
                    };
                    state.provisional = Some(item_id);
                    state.accept(ComboBoxActivationSource::Keyboard, window, cx);
                    true
                })
                .unwrap_or(false)
        })
    }

    /// Opens the attached, enabled, rendered control using its ordinary popup lifecycle.
    /// Returns false if it is unavailable, already open, or blocked by a modal.
    pub fn open(&self, window: &mut Window, cx: &mut App) -> bool {
        let state = self.state.borrow().clone();
        state.is_some_and(|state| {
            state
                .update(cx, |state, cx| {
                    state.window_id == window.window_handle().window_id()
                        && state.open(None, false, window, cx)
                })
                .unwrap_or(false)
        })
    }
}

/// Application-owned ComboBox paint values.
///
/// The open popup's material, edge, internal divider, corners, and elevation come from the shared
/// anchored-popup surface. This catalog carries the trigger and the popup's content.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ComboBoxPaint {
    rows: Option<crate::ListRowPaints>,
    foreground: Rgba,
    trigger_icon_foreground: Rgba,
    trigger_icon_disabled: Rgba,
    muted: Rgba,
    disabled: Rgba,
    selected_background: Rgba,
    hover_background: Rgba,
    hover_foreground: Rgba,
    selected_foreground: Rgba,
    trigger_background: Rgba,
    trigger_hover_background: Rgba,
    trigger_pressed_background: Rgba,
    trigger_disabled_background: Rgba,
    trigger_border: Rgba,
    trigger_shadow: crate::ControlShadow,
    trigger_bottom_edge: Rgba,
    focus_border: Rgba,
}

impl ComboBoxPaint {
    /// Creates the complete bounded paint catalog.
    #[expect(
        clippy::too_many_arguments,
        reason = "the bounded paint catalog is one theme fact"
    )]
    pub fn new(
        foreground: Rgba,
        muted: Rgba,
        disabled: Rgba,
        selected_background: Rgba,
        selected_foreground: Rgba,
        trigger_background: Rgba,
        trigger_hover_background: Rgba,
        trigger_border: Rgba,
        focus_border: Rgba,
    ) -> Self {
        Self {
            rows: None,
            foreground,
            trigger_icon_foreground: foreground,
            trigger_icon_disabled: disabled,
            muted,
            disabled,
            selected_background,
            hover_background: selected_background,
            hover_foreground: selected_foreground,
            selected_foreground,
            trigger_background,
            trigger_hover_background,
            trigger_pressed_background: trigger_hover_background,
            trigger_disabled_background: trigger_background,
            trigger_border,
            trigger_shadow: crate::ControlShadow::none(),
            trigger_bottom_edge: Rgba::default(),
            focus_border,
        }
    }

    /// Installs complete semantic list row states.
    pub fn rows(mut self, rows: crate::ListRowPaints) -> Self {
        self.rows = Some(rows);
        self
    }

    /// Sets row hover independently of the provisional selection.
    pub fn hover_background(mut self, color: Rgba) -> Self {
        self.hover_background = color;
        self
    }

    /// Sets the foreground paired with the row hover background.
    pub fn hover_foreground(mut self, color: Rgba) -> Self {
        self.hover_foreground = color;
        self
    }

    /// Sets icon-only trigger colors independently of text and popup row foregrounds.
    pub fn trigger_icon_colors(mut self, normal: Rgba, disabled: Rgba) -> Self {
        self.trigger_icon_foreground = normal;
        self.trigger_icon_disabled = disabled;
        self
    }

    /// Sets the pressed and disabled fills for the trigger independently from hover.
    pub fn trigger_state_backgrounds(mut self, pressed: Rgba, disabled: Rgba) -> Self {
        self.trigger_pressed_background = pressed;
        self.trigger_disabled_background = disabled;
        self
    }

    /// Adds the application-owned raised treatment to an ordinary trigger.
    pub fn trigger_elevation(
        mut self,
        shadow: crate::ControlShadow,
        border: Option<Rgba>,
        bottom_edge: Rgba,
    ) -> Self {
        self.trigger_shadow = shadow;
        self.trigger_bottom_edge = bottom_edge;
        if let Some(border) = border {
            self.trigger_border = border;
        }
        self
    }

    fn trigger_background(self, enabled: bool, open: bool) -> Rgba {
        match (enabled, open) {
            (false, _) => self.trigger_disabled_background,
            (true, true) => self.trigger_pressed_background,
            (true, false) => self.trigger_background,
        }
    }

    fn trigger_shadow(self, enabled: bool, open: bool) -> crate::ControlShadow {
        if enabled && !open {
            self.trigger_shadow
        } else {
            crate::ControlShadow::none()
        }
    }

    fn trigger_bottom_edge(self, enabled: bool, _open: bool) -> Rgba {
        if enabled {
            self.trigger_bottom_edge
        } else {
            Rgba::default()
        }
    }

    fn trigger_leading_foreground(&self, icon_only: bool, enabled: bool) -> Rgba {
        match (icon_only, enabled) {
            (true, true) => self.trigger_icon_foreground,
            (true, false) => self.trigger_icon_disabled,
            (false, true) => self.foreground,
            (false, false) => self.disabled,
        }
    }
}

/// Bounded dimensions for every ComboBox instance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ComboBoxMetrics {
    panel_width: Pixels,
    maximum_height: Pixels,
    trigger_height: Pixels,
    icon_trigger_size: Pixels,
    input_height: Pixels,
    row_height: Pixels,
    described_row_height: Pixels,
    panel_padding: Pixels,
    horizontal_padding: Pixels,
    /// Always-reserved check or radio column.
    leading_width: Pixels,
    /// Identity-icon column, reserved for every row when any visible item has an icon.
    identity_icon_width: Pixels,
    state_icon_gap: Pixels,
    gap: Pixels,
    trigger_corner_radius: Pixels,
    corner_radius: Pixels,
    border_width: Pixels,
    label_size: Pixels,
    secondary_size: Pixels,
    label_line_height: Pixels,
    secondary_line_height: Pixels,
    icon_size: Pixels,
    trigger_icon_size: Pixels,
    icon_baseline_center: Pixels,
    group_separator_height: Pixels,
}

impl ComboBoxMetrics {
    /// Creates compact native defaults around a panel width and trigger height.
    pub fn new(panel_width: Pixels, trigger_height: Pixels) -> Self {
        let shell = crate::FloatingSurfaceTheme::default().shell(COMBO_BOX_ROLE);
        Self {
            panel_width,
            maximum_height: px(320.0),
            trigger_height,
            icon_trigger_size: px(28.0),
            input_height: px(34.0),
            row_height: px(30.0),
            described_row_height: px(46.0),
            panel_padding: shell.content_inset(),
            horizontal_padding: px(10.0),
            leading_width: px(16.0),
            identity_icon_width: px(18.0),
            state_icon_gap: px(4.0),
            gap: px(6.0),
            trigger_corner_radius: px(6.0),
            corner_radius: shell.corner_radius(),
            border_width: shell.hairline(),
            label_size: px(12.0),
            secondary_size: px(11.0),
            label_line_height: px(16.0),
            secondary_line_height: px(15.0),
            icon_size: px(12.0),
            trigger_icon_size: px(12.0),
            icon_baseline_center: px(4.0),
            group_separator_height: px(9.0),
        }
    }

    /// Sets the square target size used by icon-only triggers.
    pub fn icon_trigger_size(mut self, size: Pixels) -> Self {
        self.icon_trigger_size = size.max(px(0.0));
        self
    }

    /// Sets maximum panel, editor, plain-row, and described-row heights.
    pub fn geometry(
        mut self,
        maximum_height: Pixels,
        input_height: Pixels,
        row_height: Pixels,
        described_row_height: Pixels,
    ) -> Self {
        self.maximum_height = maximum_height;
        self.input_height = input_height;
        self.row_height = row_height;
        self.described_row_height = described_row_height;
        self
    }

    /// Sets content padding, leading-slot width, and column gap.
    pub fn spacing(
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

    /// Sets the state gutter, optional identity-icon gutter, and their two column gaps.
    pub fn row_gutters(
        mut self,
        state_width: Pixels,
        identity_icon_width: Pixels,
        state_icon_gap: Pixels,
        icon_text_gap: Pixels,
    ) -> Self {
        self.leading_width = state_width.max(px(0.0));
        self.identity_icon_width = identity_icon_width.max(px(0.0));
        self.state_icon_gap = state_icon_gap.max(px(0.0));
        self.gap = icon_text_gap.max(px(0.0));
        self
    }

    /// Sets the corner radius of the trigger that opens the popup.
    pub fn trigger_shape(mut self, corner_radius: Pixels) -> Self {
        self.trigger_corner_radius = corner_radius;
        self
    }

    /// Sets primary and secondary font sizes.
    pub fn font_sizes(mut self, label: Pixels, secondary: Pixels) -> Self {
        self.label_size = label;
        self.secondary_size = secondary;
        self
    }

    /// Sets the label and secondary line boxes and the chrome glyph size.
    pub fn text_geometry(
        mut self,
        label_line_height: Pixels,
        secondary_line_height: Pixels,
        icon_size: Pixels,
    ) -> Self {
        self.label_line_height = label_line_height;
        self.secondary_line_height = secondary_line_height;
        self.icon_size = icon_size;
        self
    }

    /// Sets the nominal glyph size for trigger-leading content and the disclosure chevron.
    pub fn trigger_icon_size(mut self, size: Pixels) -> Self {
        self.trigger_icon_size = size.max(px(0.0));
        self
    }

    /// Sets the center-above-baseline metric shared by row and labeled-trigger icons.
    pub fn icon_baseline_center(mut self, center: Pixels) -> Self {
        self.icon_baseline_center = center;
        self
    }

    fn scaled(self, text_scale: f32, spacing_scale: f32) -> Self {
        let width_scale = crate::appearance::normalized_scale(text_scale)
            .max(crate::appearance::normalized_scale(spacing_scale));
        let label_size = crate::appearance::scale_metric(self.label_size, text_scale);
        let icon_size = crate::appearance::scale_metric(self.icon_size, text_scale);
        let trigger_icon_size = crate::appearance::scale_metric(self.trigger_icon_size, text_scale);
        let density_step = (crate::appearance::normalized_scale(spacing_scale) - 1.0).max(0.0);
        Self {
            panel_width: self.panel_width * width_scale,
            maximum_height: crate::appearance::scale_metric(self.maximum_height, spacing_scale),
            trigger_height: crate::appearance::scale_line_box(
                self.trigger_height,
                self.label_size,
                text_scale,
                spacing_scale,
            ),
            icon_trigger_size: crate::appearance::scale_line_box(
                self.icon_trigger_size,
                self.icon_size,
                text_scale,
                spacing_scale,
            ),
            input_height: crate::appearance::scale_line_box(
                self.input_height,
                self.label_line_height,
                text_scale,
                spacing_scale,
            ),
            row_height: crate::appearance::scale_line_box(
                self.row_height,
                self.label_line_height,
                text_scale,
                spacing_scale,
            ),
            described_row_height: crate::appearance::scale_line_box(
                self.described_row_height,
                self.label_line_height + self.secondary_line_height,
                text_scale,
                spacing_scale,
            ),
            panel_padding: self.panel_padding,
            horizontal_padding: crate::appearance::scale_metric(
                self.horizontal_padding,
                spacing_scale,
            ),
            leading_width: (self.leading_width + px(8.0) * density_step).max(icon_size),
            identity_icon_width: (self.identity_icon_width + px(8.0) * density_step).max(icon_size),
            state_icon_gap: self.state_icon_gap,
            gap: self.gap,
            trigger_corner_radius: self.trigger_corner_radius,
            corner_radius: self.corner_radius,
            border_width: self.border_width,
            label_size,
            secondary_size: crate::appearance::scale_metric(self.secondary_size, text_scale),
            label_line_height: crate::appearance::scale_metric(self.label_line_height, text_scale),
            secondary_line_height: crate::appearance::scale_metric(
                self.secondary_line_height,
                text_scale,
            ),
            icon_size,
            trigger_icon_size,
            icon_baseline_center: crate::appearance::scale_metric(
                self.icon_baseline_center,
                text_scale,
            ),
            group_separator_height: crate::appearance::scale_metric(
                self.group_separator_height,
                spacing_scale,
            ),
        }
    }

    fn row_height(self, described: bool) -> Pixels {
        if described {
            self.described_row_height
        } else {
            self.row_height
        }
    }

    fn row_radius(self) -> Pixels {
        (self.corner_radius - self.panel_padding).max(px(0.0))
    }
}

/// Application-owned presentation installed once for every ComboBox.
///
/// The open popup's surface treatment belongs to the shared anchored-popup role, so a ComboBox and
/// a Menu opened beside it are the same object at the same elevation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ComboBoxTheme {
    paint: ComboBoxPaint,
    metrics: ComboBoxMetrics,
    shell: FloatingShell,
    focus_ring_width: Pixels,
}

impl ComboBoxTheme {
    /// Creates a complete ComboBox theme.
    pub fn new(paint: ComboBoxPaint, metrics: ComboBoxMetrics) -> Self {
        Self {
            paint,
            metrics,
            shell: crate::FloatingSurfaceTheme::default().shell(COMBO_BOX_ROLE),
            focus_ring_width: px(1.0),
        }
    }

    /// Sets the focus-ring width independently of the trigger border and radius.
    pub fn focus_ring_width(mut self, width: Pixels) -> Self {
        self.focus_ring_width = width.max(px(0.0));
        self
    }

    #[cfg(test)]
    pub(crate) fn resolved_focus_ring_width(self) -> Pixels {
        self.focus_ring_width
    }

    pub(crate) fn scaled_metrics(self, text_scale: f32, spacing_scale: f32) -> Self {
        Self {
            metrics: self.metrics.scaled(text_scale, spacing_scale),
            ..self
        }
    }

    /// Measures a custom trigger's outer width, including its themed borders.
    /// The caller's content width must include any caller-owned padding.
    pub fn custom_trigger_width(self, content_width: Pixels) -> Pixels {
        content_width + self.metrics.border_width * 2.0
    }

    /// Adds the shared ordinary-control elevation to non-custom triggers.
    pub fn ordinary_elevation(
        mut self,
        shadow: crate::ControlShadow,
        border: Option<Rgba>,
        bottom_edge: Rgba,
    ) -> Self {
        self.paint = self.paint.trigger_elevation(shadow, border, bottom_edge);
        self
    }
}

impl Global for ComboBoxTheme {}

/// Resolves the installed ComboBox theme against the shared anchored-popup surface.
fn combo_box_theme(cx: &App) -> ComboBoxTheme {
    let mut theme = *crate::control_theme_catalog(cx).map_or_else(
        || cx.global::<ComboBoxTheme>(),
        |catalog| &catalog.combo_box,
    );
    let shell = crate::floating_surface::shell(COMBO_BOX_ROLE, cx);
    theme.metrics.corner_radius = shell.corner_radius();
    theme.metrics.panel_padding = shell.content_inset();
    theme.metrics.border_width = shell.hairline();
    theme.shell = shell;
    theme
}

fn trigger_edges(paint: ComboBoxPaint, enabled: bool, focused: bool) -> (Rgba, Option<Rgba>) {
    (
        paint.trigger_border,
        (enabled && focused && paint.focus_border.a > 0.0).then_some(paint.focus_border),
    )
}

type AcceptanceHandler<I> = Rc<dyn Fn(&ComboBoxAcceptance<I>, &mut Window, &mut App)>;
type LifecycleHandler = Rc<dyn Fn(&ComboBoxLifecycleEvent, &mut App)>;

enum ComboBoxTrigger {
    Text,
    Icon,
    Custom(AnyElement),
}

/// A reusable controlled ComboBox with a searchable anchored popup.
///
/// The caller supplies `selected` on every render. Acceptance proposes a new identity but never
/// mutates caller state. Passing `None` creates an ephemeral chooser that returns to its prompt.
#[derive(IntoElement)]
pub struct ComboBox<I: Clone + Eq + 'static> {
    id: ElementId,
    accessibility_name: SharedString,
    selected: Option<I>,
    prompt: SharedString,
    items: Vec<ComboBoxItem<I>>,
    fallback: Option<ComboBoxFallback<I>>,
    handle: Option<ComboBoxHandle<I>>,
    copy: ComboBoxCopy,
    disabled: bool,
    busy: bool,
    placement: AnchoredPlacementConfig,
    panel_width: Option<Pixels>,
    menu_with_filter_header: bool,
    full_width: bool,
    hug: bool,
    trigger_leading: Option<IconBuilder>,
    input_leading: Option<InputIconBuilder>,
    trigger: ComboBoxTrigger,
    custom_trigger_content_height: Option<Pixels>,
    tooltip: Option<Tooltip>,
    debug_selector: Option<String>,
    on_accept: Option<AcceptanceHandler<I>>,
    on_lifecycle: Option<LifecycleHandler>,
}

impl<I: Clone + Eq + 'static> ComboBox<I> {
    /// Creates an enabled controlled selector.
    pub fn new(
        id: impl Into<ElementId>,
        accessibility_name: impl Into<SharedString>,
        selected: Option<I>,
        prompt: impl Into<SharedString>,
        items: Vec<ComboBoxItem<I>>,
    ) -> Self {
        Self {
            id: id.into(),
            accessibility_name: accessibility_name.into(),
            selected,
            prompt: prompt.into(),
            items,
            fallback: None,
            handle: None,
            copy: ComboBoxCopy::default(),
            disabled: false,
            busy: false,
            placement: AnchoredPlacementConfig::default(),
            panel_width: None,
            menu_with_filter_header: false,
            full_width: false,
            hug: false,
            trigger_leading: None,
            input_leading: None,
            trigger: ComboBoxTrigger::Text,
            custom_trigger_content_height: None,
            tooltip: None,
            debug_selector: None,
            on_accept: None,
            on_lifecycle: None,
        }
    }

    /// Installs query-aware fallback rows.
    pub fn fallback(mut self, fallback: ComboBoxFallback<I>) -> Self {
        self.fallback = Some(fallback);
        self
    }

    /// Attaches a weak handle for opening this control from an application action.
    pub fn handle(mut self, handle: ComboBoxHandle<I>) -> Self {
        self.handle = Some(handle);
        self
    }

    /// Controls whether the complete selector is inert.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Controls whether the popup presents a busy status and blocks acceptance.
    pub fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    /// Replaces the popup's localizable editor and status copy.
    pub fn copy(mut self, copy: ComboBoxCopy) -> Self {
        self.copy = copy;
        self
    }

    /// Selects the shared anchored placement policy.
    pub fn placement(mut self, placement: AnchoredPlacementConfig) -> Self {
        self.placement = placement;
        self
    }

    /// Sets the preferred popup width independently of the trigger width.
    ///
    /// The shared placement policy still constrains this width to the available viewport.
    pub fn panel_width(mut self, width: Pixels) -> Self {
        self.panel_width = Some(width.max(px(0.0)));
        self
    }

    /// Uses the menu-with-filter grammar: row-driven width, a 240-point minimum, a 420-point
    /// truncation cap, and a 24-point minimum gap before keyboard equivalents.
    pub fn menu_with_filter_header(mut self) -> Self {
        self.menu_with_filter_header = true;
        self
    }

    /// Resolves logical Start and End placement right-to-left.
    pub fn right_to_left(mut self, right_to_left: bool) -> Self {
        self.placement = self.placement.direction(if right_to_left {
            AnchoredTextDirection::RightToLeft
        } else {
            AnchoredTextDirection::LeftToRight
        });
        self
    }

    /// Makes a text or custom trigger fill the width allocated by its parent.
    ///
    /// Icon-only triggers retain their theme-owned square target size.
    pub fn full_width(mut self, full_width: bool) -> Self {
        self.full_width = full_width;
        self
    }

    /// Makes a text trigger take only the width its own value needs.
    ///
    /// A trigger otherwise reserves the popup's width, which leaves its value and its chevron at
    /// opposite ends of an empty bezel. A hugging trigger keeps them together, so a row of them
    /// reads as values rather than as fields. The popup keeps its own width either way.
    pub fn hug(mut self, hug: bool) -> Self {
        self.hug = hug;
        self
    }

    /// Adds decorative content before the popup filter editor using the resolved live icon size.
    pub fn input_leading(mut self, build: impl Fn(Pixels) -> AnyElement + 'static) -> Self {
        self.input_leading = Some(Rc::new(build));
        self
    }

    /// Adds optional leading trigger content using the resolved foreground color.
    pub fn leading(mut self, build: impl Fn(Rgba, Pixels) -> AnyElement + 'static) -> Self {
        self.trigger_leading = Some(Rc::new(build));
        self
    }

    /// Uses a compact icon-only trigger while retaining its logical accessibility name.
    ///
    /// The icon replaces the visible prompt, selected label, and disclosure chevron. Its square
    /// target size comes from [`ComboBoxMetrics::icon_trigger_size`].
    pub fn icon_trigger(mut self, build: impl Fn(Rgba, Pixels) -> AnyElement + 'static) -> Self {
        self.trigger_leading = Some(Rc::new(build));
        self.trigger = ComboBoxTrigger::Icon;
        self
    }

    /// Replaces the visible trigger content while retaining ComboBox interaction and focus.
    ///
    /// Content must be decorative, without nested controls. The trigger uses the compact icon
    /// target height and the content's intrinsic width, or its parent's width with `full_width`.
    /// Supply any desired padding and fill states inside the content; no wrapper fill, label, or
    /// chevron is added. The wrapper retains its focus border and all interaction semantics.
    pub fn custom_trigger(mut self, content: impl IntoElement) -> Self {
        self.trigger = ComboBoxTrigger::Custom(content.into_any_element());
        self
    }

    /// Sets the custom content's height, excluding the trigger's focus border.
    pub fn custom_trigger_content_height(mut self, height: Pixels) -> Self {
        self.custom_trigger_content_height = Some(height.max(px(0.0)));
        self
    }

    /// Adds help for the enabled, closed trigger without changing its layout or focus behavior.
    pub fn tooltip(mut self, tooltip: Tooltip) -> Self {
        self.tooltip = Some(tooltip);
        self
    }

    /// Adds a stable selector used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    /// Handles a typed acceptance after the popup has closed and released transient focus.
    pub fn on_accept(
        mut self,
        handler: impl Fn(&ComboBoxAcceptance<I>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_accept = Some(Rc::new(handler));
        self
    }

    /// Handles exact open and close lifecycle transitions.
    pub fn on_lifecycle(
        mut self,
        handler: impl Fn(&ComboBoxLifecycleEvent, &mut App) + 'static,
    ) -> Self {
        self.on_lifecycle = Some(Rc::new(handler));
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ComboBoxRegistration(u64);

type ReplaceComboBox = Rc<dyn Fn(&mut App) -> Option<ComboBoxReplacement>>;
type ComboBoxIsOpen = Rc<dyn Fn(&App) -> bool>;
type ClaimComboBoxMenu = Rc<dyn Fn(&mut App) -> bool>;

struct ErasedComboBoxRegistration {
    token: ComboBoxRegistration,
    modal_parent: Option<crate::modal::ModalParentToken>,
    popup_focus: WeakFocusHandle,
    replace: ReplaceComboBox,
    is_open: ComboBoxIsOpen,
    claim_menu: ClaimComboBoxMenu,
}

pub(crate) struct ComboBoxReplacement {
    lifecycle: Option<LifecycleHandler>,
    pub(crate) restore_focus: Option<WeakFocusHandle>,
}

impl ComboBoxReplacement {
    pub(crate) fn finish(self, cx: &mut App) {
        if let Some(handler) = self.lifecycle {
            handler(
                &ComboBoxLifecycleEvent::Closed(ComboBoxCloseReason::Replaced),
                cx,
            );
        }
    }
}

#[derive(Default)]
struct ComboBoxCoordinator {
    owners: HashMap<WindowId, ErasedComboBoxRegistration>,
    next_registration: u64,
}

impl Global for ComboBoxCoordinator {}

/// Returns whether a ComboBox popup currently owns this Operating-System Window.
pub fn window_combo_box_is_open(window: &Window, cx: &App) -> bool {
    cx.has_global::<ComboBoxCoordinator>()
        && cx
            .global::<ComboBoxCoordinator>()
            .owners
            .get(&window.window_handle().window_id())
            .is_some_and(|owner| (owner.is_open)(cx))
}

fn register_combo_box<I: Clone + Eq + 'static>(
    owner: WeakEntity<ComboBoxState<I>>,
    modal_parent: Option<crate::modal::ModalParentToken>,
    popup_focus: WeakFocusHandle,
    window: &Window,
    cx: &mut App,
) -> (ComboBoxRegistration, Option<WeakFocusHandle>) {
    let window_id = window.window_handle().window_id();
    let open_owner = owner.clone();
    let menu_owner = owner.clone();
    let replace = Rc::new(move |cx: &mut App| {
        owner
            .update(cx, |state, cx| state.replace_without_lifecycle(cx))
            .ok()
            .flatten()
    });
    let is_open = Rc::new(move |cx: &App| {
        open_owner
            .read_with(cx, |state, _| state.open)
            .unwrap_or(false)
    });
    let claim_menu = Rc::new(move |cx: &mut App| {
        menu_owner
            .update(cx, |state, cx| {
                let claimed = state.open
                    && (state.input_context_menu_open || state.input.read(cx).owns_context_menu())
                    && !state.input_context_menu_claimed;
                state.input_context_menu_claimed |= claimed;
                claimed
            })
            .unwrap_or(false)
    });
    let (registration, previous) = cx.update_global::<ComboBoxCoordinator, _>(|coordinator, _| {
        coordinator.next_registration = coordinator.next_registration.wrapping_add(1);
        let registration = ComboBoxRegistration(coordinator.next_registration);
        let previous = coordinator.owners.insert(
            window_id,
            ErasedComboBoxRegistration {
                token: registration,
                modal_parent,
                popup_focus,
                replace,
                is_open,
                claim_menu,
            },
        );
        (registration, previous.map(|owner| owner.replace))
    });
    let predecessor = previous.and_then(|previous| previous(cx));
    let restore_focus = predecessor
        .as_ref()
        .and_then(|replacement| replacement.restore_focus.clone());
    if let Some(replacement) = predecessor {
        cx.defer(move |cx| replacement.finish(cx));
    }
    (registration, restore_focus)
}

pub(crate) fn dismiss_active_combo_box_for_replacement(
    window: &Window,
    cx: &mut App,
) -> Option<ComboBoxReplacement> {
    if !cx.has_global::<ComboBoxCoordinator>() {
        return None;
    }
    let replace = cx
        .global::<ComboBoxCoordinator>()
        .owners
        .get(&window.window_handle().window_id())
        .map(|owner| owner.replace.clone());
    replace.and_then(|replace| replace(cx))
}

pub(crate) fn window_combo_box_is_owned_by_current_modal(window: &Window, cx: &App) -> bool {
    let Some(parent) = crate::modal::current_modal_parent(window, cx) else {
        return false;
    };
    cx.has_global::<ComboBoxCoordinator>()
        && cx
            .global::<ComboBoxCoordinator>()
            .owners
            .get(&parent.window_id)
            .is_some_and(|owner| owner.modal_parent == Some(parent) && (owner.is_open)(cx))
}

pub(crate) fn focused_combo_box_modal_parent(
    window: &Window,
    cx: &App,
) -> Option<crate::modal::ModalParentToken> {
    let parent = crate::modal::current_modal_parent(window, cx)?;
    if !cx.has_global::<ComboBoxCoordinator>() {
        return None;
    }
    let owner = cx
        .global::<ComboBoxCoordinator>()
        .owners
        .get(&parent.window_id)?;
    (owner.modal_parent == Some(parent)
        && owner
            .popup_focus
            .upgrade()
            .is_some_and(|focus| focus.contains_focused(window, cx)))
    .then_some(parent)
}

pub(crate) fn dismiss_combo_box_owned_by_modal_parent(
    parent: crate::modal::ModalParentToken,
    cx: &mut App,
) -> Option<WeakFocusHandle> {
    if !cx.has_global::<ComboBoxCoordinator>() {
        return None;
    }
    let owner = cx
        .global::<ComboBoxCoordinator>()
        .owners
        .get(&parent.window_id)?;
    if owner.modal_parent != Some(parent) {
        return None;
    }
    let retired_focus = owner.popup_focus.clone();
    let replace = owner.replace.clone();
    let replacement = replace(cx)?;
    cx.defer(move |cx| replacement.finish(cx));
    Some(retired_focus)
}

pub(crate) fn claim_window_combo_box_menu(window: &Window, cx: &mut App) -> bool {
    if !cx.has_global::<ComboBoxCoordinator>() {
        return false;
    }
    let claim = cx
        .global::<ComboBoxCoordinator>()
        .owners
        .get(&window.window_handle().window_id())
        .map(|owner| owner.claim_menu.clone());
    claim.is_some_and(|claim| claim(cx))
}

fn unregister_combo_box(window_id: WindowId, registration: ComboBoxRegistration, cx: &mut App) {
    if !cx.has_global::<ComboBoxCoordinator>() {
        return;
    }
    cx.update_global::<ComboBoxCoordinator, _>(|coordinator, _| {
        if coordinator
            .owners
            .get(&window_id)
            .is_some_and(|owner| owner.token == registration)
        {
            coordinator.owners.remove(&window_id);
        }
    });
}

#[derive(Clone)]
struct PointerPress<I> {
    id: I,
    generation: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ComboBoxHighlights {
    label: Vec<Range<usize>>,
    description: Vec<Range<usize>>,
}

#[derive(Clone, Copy)]
struct ComboBoxRowState {
    selected: bool,
    provisional: bool,
    hovered: bool,
}

struct ComboBoxState<I: Clone + Eq + 'static> {
    accessibility_name: SharedString,
    selected: Option<I>,
    prompt: SharedString,
    copy: ComboBoxCopy,
    items: Rc<[ComboBoxItem<I>]>,
    presented_items: Rc<[ComboBoxItem<I>]>,
    fallback: Option<ComboBoxFallback<I>>,
    ordinary_match_count: usize,
    matches: Rc<[usize]>,
    match_highlights: Rc<[ComboBoxHighlights]>,
    provisional: Option<I>,
    hovered_row: Option<I>,
    query: String,
    disabled: bool,
    busy: bool,
    open: bool,
    model_generation: u64,
    trigger_bounds: Option<BoundsPixels>,
    placement: AnchoredPlacementConfig,
    panel_width: Option<Pixels>,
    menu_with_filter_header: bool,
    trigger_focus: FocusHandle,
    popup_focus: FocusHandle,
    input: Entity<TextInput>,
    list: ListState,
    result_viewport_size: Option<Size<Pixels>>,
    pointer_press: Option<PointerPress<I>>,
    selection_reveal_pending: bool,
    input_context_menu_open: bool,
    input_context_menu_claimed: bool,
    registration: Option<ComboBoxRegistration>,
    window_id: WindowId,
    restore_focus: Option<WeakFocusHandle>,
    restore_on_activation: Option<(WeakFocusHandle, Option<crate::modal::ModalParentToken>)>,
    on_accept: Option<AcceptanceHandler<I>>,
    on_lifecycle: Option<LifecycleHandler>,
    _input_subscription: Subscription,
    _focus_subscription: Subscription,
}

type BoundsPixels = Bounds<Pixels>;

impl<I: Clone + Eq + 'static> ComboBoxState<I> {
    fn new(window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextInput::new("combo-box-input", "Filter options", "", window, cx)
                .placeholder("Search")
                .variant(TextInputVariant::Bare)
                .tab_behavior(TextInputTabBehavior::Propagate)
                .home_end_behavior(TextInputHomeEndBehavior::Propagate)
                .debug_selector("combo-box-input")
        });
        let input_subscription = cx.subscribe_in(
            &input,
            window,
            |state, input, event: &TextInputEvent, window, cx| match event {
                TextInputEvent::ValueChanged(_) => {
                    state.set_query(input.read(cx).value().to_owned(), cx);
                }
                TextInputEvent::Submitted => {
                    state.accept(ComboBoxActivationSource::Keyboard, window, cx);
                }
                TextInputEvent::Cancelled => {
                    state.close(ComboBoxCloseReason::Escape, true, Some(window), cx);
                }
                TextInputEvent::TabForwardRequested => {
                    if state.close(ComboBoxCloseReason::TabTraversal, false, Some(window), cx) {
                        state.trigger_focus.focus(window);
                        window.defer(cx, |window, _| window.focus_next());
                    }
                }
                TextInputEvent::TabBackwardRequested => {
                    if state.close(ComboBoxCloseReason::TabTraversal, false, Some(window), cx) {
                        state.trigger_focus.focus(window);
                        window.defer(cx, |window, _| window.focus_prev());
                    }
                }
                TextInputEvent::ContextMenuOpened => {
                    state.input_context_menu_open = true;
                    state.input_context_menu_claimed = false;
                }
                TextInputEvent::ContextMenuClosed => {
                    state.input_context_menu_open = false;
                    state.input_context_menu_claimed = false;
                }
                _ => {}
            },
        );
        let trigger_focus = cx.focus_handle();
        let popup_focus = cx.focus_handle();
        let focus_subscription = cx.on_focus_out(&popup_focus, window, |_, _, window, cx| {
            let state = cx.entity().downgrade();
            window.defer(cx, move |window, cx| {
                let _ = state.update(cx, |state, cx| {
                    if state.open
                        && !state.popup_focus.contains_focused(window, cx)
                        && !crate::menu::window_menu_is_open(window, cx)
                    {
                        state.close(ComboBoxCloseReason::FocusLost, false, Some(window), cx);
                    }
                });
            });
        });
        cx.observe_window_activation(window, |state, window, cx| {
            if state.open && !window.is_window_active() {
                state.restore_on_activation = state
                    .restore_focus
                    .clone()
                    .map(|focus| (focus, crate::modal::current_modal_parent(window, cx)));
                state.close(ComboBoxCloseReason::Deactivated, false, Some(window), cx);
            } else if !state.open
                && window.is_window_active()
                && let Some(focus) = state
                    .restore_on_activation
                    .take()
                    .filter(|(_, parent)| *parent == crate::modal::current_modal_parent(window, cx))
                    .and_then(|(focus, _)| focus.upgrade())
            {
                focus.focus(window);
            }
        })
        .detach();
        let window_id = window.window_handle().window_id();
        let release_window = window.window_handle();
        cx.on_release(move |state, cx| {
            if let Some(registration) = state.registration.take() {
                unregister_combo_box(window_id, registration, cx);
            }
            if state.open {
                state.open = false;
                if let Some(focus) = state.restore_focus.take().and_then(|focus| focus.upgrade()) {
                    cx.defer(move |cx| {
                        let _ = cx.update_window(release_window, |_, window, _| {
                            focus.focus(window);
                        });
                    });
                }
                state.emit_lifecycle(
                    ComboBoxLifecycleEvent::Closed(ComboBoxCloseReason::TargetDisappeared),
                    cx,
                );
            }
        })
        .detach();
        Self {
            accessibility_name: SharedString::default(),
            selected: None,
            prompt: SharedString::default(),
            copy: ComboBoxCopy::default(),
            items: Vec::new().into(),
            presented_items: Vec::new().into(),
            fallback: None,
            ordinary_match_count: 0,
            matches: Vec::new().into(),
            match_highlights: Vec::new().into(),
            provisional: None,
            hovered_row: None,
            query: String::new(),
            disabled: true,
            busy: false,
            open: false,
            model_generation: 0,
            trigger_bounds: None,
            placement: AnchoredPlacementConfig::default(),
            panel_width: None,
            menu_with_filter_header: false,
            trigger_focus,
            popup_focus,
            input,
            list: ListState::new(0, ListAlignment::Top, px(0.0)).measure_all(),
            result_viewport_size: None,
            pointer_press: None,
            selection_reveal_pending: false,
            input_context_menu_open: false,
            input_context_menu_claimed: false,
            registration: None,
            window_id,
            restore_focus: None,
            restore_on_activation: None,
            on_accept: None,
            on_lifecycle: None,
            _input_subscription: input_subscription,
            _focus_subscription: focus_subscription,
        }
    }

    fn emit_lifecycle(&self, event: ComboBoxLifecycleEvent, cx: &mut App) {
        let Some(handler) = self.on_lifecycle.clone() else {
            return;
        };
        cx.defer(move |cx| handler(&event, cx));
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the private synchronization seam receives one complete controlled snapshot"
    )]
    fn synchronize(
        &mut self,
        accessibility_name: SharedString,
        selected: Option<I>,
        prompt: SharedString,
        items: Vec<ComboBoxItem<I>>,
        fallback: Option<ComboBoxFallback<I>>,
        copy: ComboBoxCopy,
        disabled: bool,
        busy: bool,
        placement: AnchoredPlacementConfig,
        panel_width: Option<Pixels>,
        menu_with_filter_header: bool,
        on_accept: Option<AcceptanceHandler<I>>,
        on_lifecycle: Option<LifecycleHandler>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let items = unique_items(items);
        let model_changed = !same_model(&self.items, &items);
        self.items = items.into();
        self.fallback = fallback;
        let results_changed = self.recompute_matches(false);
        if model_changed || results_changed {
            self.model_generation = self.model_generation.wrapping_add(1);
            self.pointer_press = None;
        }
        self.accessibility_name = accessibility_name;
        self.selected = selected;
        self.prompt = prompt;
        if self.copy != copy {
            self.input.update(cx, |input, cx| {
                input.set_accessibility_name(copy.filter_name.clone(), cx);
                input.set_placeholder(copy.filter_placeholder.clone(), cx);
            });
            self.copy = copy;
        }
        self.disabled = disabled;
        self.busy = busy;
        self.placement = placement;
        self.panel_width = panel_width;
        self.menu_with_filter_header = menu_with_filter_header;
        self.on_accept = on_accept;
        self.on_lifecycle = on_lifecycle;
        self.trigger_focus = self.trigger_focus.clone().tab_stop(!disabled);
        self.repair_provisional();
        if self.open && disabled {
            self.close(ComboBoxCloseReason::Disabled, false, Some(window), cx);
            window.defer(cx, |window, _| window.focus_next());
        }
    }

    fn enabled_item(&self, id: &I) -> Option<&ComboBoxItem<I>> {
        self.presented_items
            .iter()
            .find(|item| item.id == *id && !item.disabled)
    }

    fn trigger_label(&self) -> SharedString {
        self.selected
            .as_ref()
            .and_then(|selected| self.enabled_item(selected))
            .map_or_else(|| self.prompt.clone(), |item| item.label.clone())
    }

    fn set_query(&mut self, query: String, cx: &mut gpui::Context<Self>) {
        if self.query == query {
            return;
        }
        self.query = query;
        self.model_generation = self.model_generation.wrapping_add(1);
        self.pointer_press = None;
        self.recompute_matches(true);
        self.repair_provisional();
        cx.notify();
    }

    fn recompute_matches(&mut self, reset_fallback_selection: bool) -> bool {
        let mut presented = self.items.to_vec();
        let item_matches = match_items(&self.items, &self.query);
        let mut matches = item_matches
            .iter()
            .map(|(item_index, _)| *item_index)
            .collect::<Vec<_>>();
        let mut match_highlights = item_matches
            .into_iter()
            .map(|(_, highlights)| highlights)
            .collect::<Vec<_>>();
        self.ordinary_match_count = matches.len();
        if let Some(fallback) = &self.fallback {
            for item in fallback.items(&self.query, self.ordinary_match_count) {
                if !presented.iter().any(|existing| existing.id == item.id) {
                    matches.push(presented.len());
                    match_highlights.push(ComboBoxHighlights::default());
                    presented.push(item);
                }
            }
        }
        let changed = !same_model(&self.presented_items, &presented)
            || self.matches.as_ref() != matches.as_slice()
            || self.match_highlights.as_ref() != match_highlights.as_slice();
        self.presented_items = presented.into();
        self.matches = matches.into();
        self.match_highlights = match_highlights.into();
        if reset_fallback_selection && self.ordinary_match_count > 0 {
            self.provisional = None;
        }
        if changed {
            self.list.reset(self.matches.len());
            self.selection_reveal_pending = true;
        }
        changed
    }

    fn repair_provisional(&mut self) {
        let remains = self.provisional.as_ref().is_some_and(|id| {
            self.matches.iter().any(|index| {
                self.presented_items
                    .get(*index)
                    .is_some_and(|item| item.id == *id && !item.disabled)
            })
        });
        if !remains {
            self.provisional = self
                .selected
                .as_ref()
                .filter(|id| {
                    self.matches.iter().any(|index| {
                        self.presented_items
                            .get(*index)
                            .is_some_and(|item| item.id == **id && !item.disabled)
                    })
                })
                .cloned()
                .or_else(|| self.first_enabled_match().map(|item| item.id.clone()));
            self.selection_reveal_pending = self.provisional.is_some();
            if let Some(position) = self.provisional_position() {
                self.list.scroll_to(ListOffset {
                    item_ix: position,
                    offset_in_item: px(0.0),
                });
            }
        }
    }

    fn first_enabled_match(&self) -> Option<&ComboBoxItem<I>> {
        self.matches
            .iter()
            .filter_map(|index| self.presented_items.get(*index))
            .find(|item| !item.disabled)
    }

    fn last_enabled_match(&self) -> Option<&ComboBoxItem<I>> {
        self.matches
            .iter()
            .rev()
            .filter_map(|index| self.presented_items.get(*index))
            .find(|item| !item.disabled)
    }

    fn open(
        &mut self,
        query: Option<String>,
        from_end: bool,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let modal_parent = crate::modal::modal_parent_for_focus(&self.trigger_focus, window, cx);
        if self.open
            || self.disabled
            || self.trigger_bounds.is_none()
            || crate::modal::current_modal_parent(window, cx) != modal_parent
        {
            return false;
        }
        let palette_predecessor =
            crate::command_palette::dismiss_active_command_palette_for_replacement(window, cx);
        let menu_predecessor = crate::menu::dismiss_active_menu_for_replacement(window, cx)
            .and_then(|replacement| replacement.0);
        self.open = true;
        self.hovered_row = None;
        self.pointer_press = None;
        self.result_viewport_size = None;
        if let Some(query) = query {
            self.input
                .update(cx, |input, cx| input.set_value(query, cx));
        } else if !self.query.is_empty() {
            self.input.update(cx, |input, cx| input.set_value("", cx));
        }
        self.query = self.input.read(cx).value().to_owned();
        self.recompute_matches(false);
        self.provisional = if from_end {
            self.last_enabled_match().map(|item| item.id.clone())
        } else {
            self.selected
                .as_ref()
                .and_then(|id| self.enabled_item(id))
                .filter(|item| {
                    self.matches
                        .iter()
                        .any(|index| self.presented_items[*index].id == item.id)
                })
                .map(|item| item.id.clone())
                .or_else(|| self.first_enabled_match().map(|item| item.id.clone()))
        };
        if let Some(position) = self.provisional_position() {
            self.list.scroll_to_reveal_item(position);
        }
        let (registration, combo_predecessor) = register_combo_box(
            cx.entity().downgrade(),
            modal_parent,
            self.popup_focus.downgrade(),
            window,
            cx,
        );
        self.registration = Some(registration);
        self.restore_focus = palette_predecessor
            .or(menu_predecessor)
            .or(combo_predecessor)
            .or_else(|| window.focused(cx).map(|focus| focus.downgrade()));
        self.input.read(cx).focus_handle().focus(window);
        self.emit_lifecycle(ComboBoxLifecycleEvent::Opened, cx);
        cx.notify();
        true
    }

    fn close(
        &mut self,
        reason: ComboBoxCloseReason,
        restore_focus: bool,
        window: Option<&mut Window>,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if !self.open {
            return false;
        }
        self.open = false;
        self.hovered_row = None;
        self.pointer_press = None;
        self.provisional = None;
        if let Some(registration) = self.registration.take() {
            unregister_combo_box(self.window_id, registration, cx);
        }
        let predecessor = self.restore_focus.take();
        if restore_focus
            && let (Some(window), Some(focus)) =
                (window, predecessor.and_then(|focus| focus.upgrade()))
        {
            focus.focus(window);
        }
        self.emit_lifecycle(ComboBoxLifecycleEvent::Closed(reason), cx);
        cx.notify();
        true
    }

    fn replace_without_lifecycle(
        &mut self,
        cx: &mut gpui::Context<Self>,
    ) -> Option<ComboBoxReplacement> {
        if !self.open {
            return None;
        }
        self.open = false;
        self.hovered_row = None;
        self.pointer_press = None;
        self.provisional = None;
        self.input_context_menu_open = false;
        self.input_context_menu_claimed = false;
        if let Some(registration) = self.registration.take() {
            unregister_combo_box(self.window_id, registration, cx);
        }
        let replacement = ComboBoxReplacement {
            lifecycle: self.on_lifecycle.clone(),
            restore_focus: self.restore_focus.take(),
        };
        cx.notify();
        Some(replacement)
    }

    fn accept(
        &mut self,
        source: ComboBoxActivationSource,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let Some(item_id) = self.provisional.clone() else {
            return;
        };
        if self.enabled_item(&item_id).is_none() {
            return;
        }
        let handler = self.on_accept.clone();
        if !self.close(ComboBoxCloseReason::Accepted, true, Some(window), cx) {
            return;
        }
        if let Some(handler) = handler {
            let window_handle = window.window_handle();
            cx.defer(move |cx| {
                let _ = cx.update_window(window_handle, |_, window, cx| {
                    handler(&ComboBoxAcceptance { item_id, source }, window, cx);
                });
            });
        }
    }

    fn enabled_positions(&self) -> Vec<usize> {
        self.matches
            .iter()
            .enumerate()
            .filter_map(|(position, index)| {
                self.presented_items
                    .get(*index)
                    .is_some_and(|item| !item.disabled)
                    .then_some(position)
            })
            .collect()
    }

    fn provisional_position(&self) -> Option<usize> {
        let provisional = self.provisional.as_ref()?;
        self.matches.iter().position(|index| {
            self.presented_items
                .get(*index)
                .is_some_and(|item| item.id == *provisional)
        })
    }

    fn move_by(&mut self, delta: isize, cx: &mut gpui::Context<Self>) {
        let enabled = self.enabled_positions();
        if enabled.is_empty() {
            self.provisional = None;
            return;
        }
        let current = self
            .provisional_position()
            .and_then(|position| enabled.iter().position(|candidate| *candidate == position));
        let next = match (current, delta.is_negative()) {
            (Some(current), false) => (current + delta.unsigned_abs()) % enabled.len(),
            (Some(current), true) => {
                (current + enabled.len() - delta.unsigned_abs() % enabled.len()) % enabled.len()
            }
            (None, false) => 0,
            (None, true) => enabled.len() - 1,
        };
        self.select_position(enabled[next], cx);
    }

    fn move_edge(&mut self, first: bool, cx: &mut gpui::Context<Self>) {
        let position = if first {
            self.enabled_positions().first().copied()
        } else {
            self.enabled_positions().last().copied()
        };
        if let Some(position) = position {
            self.select_position(position, cx);
        }
    }

    fn move_page(
        &mut self,
        direction: isize,
        metrics: ComboBoxMetrics,
        cx: &mut gpui::Context<Self>,
    ) {
        let enabled = self.enabled_positions();
        if enabled.is_empty() {
            return;
        }
        let current = self.provisional_position().unwrap_or_else(|| {
            if direction < 0 {
                *enabled.last().unwrap_or(&0)
            } else {
                *enabled.first().unwrap_or(&0)
            }
        });
        let viewport = self.list.viewport_bounds().size.height;
        let page = if viewport > px(0.0) {
            viewport
        } else {
            metrics.row_height * 8.0
        };
        let top = |position: usize| {
            self.matches
                .iter()
                .take(position)
                .filter_map(|index| self.presented_items.get(*index))
                .fold(px(0.0), |top, item| {
                    top + metrics.row_height(item.description.is_some())
                })
        };
        let current_top = top(current);
        let target = if direction < 0 {
            (current_top - page).max(px(0.0))
        } else {
            current_top + page
        };
        let next = if direction < 0 {
            enabled
                .iter()
                .copied()
                .take_while(|position| *position < current)
                .find(|position| top(*position) >= target)
                .or_else(|| enabled.iter().copied().take_while(|p| *p < current).last())
        } else {
            enabled
                .iter()
                .copied()
                .filter(|position| *position > current)
                .take_while(|position| top(*position) <= target)
                .last()
                .or_else(|| enabled.iter().copied().find(|p| *p > current))
        };
        self.select_position(next.unwrap_or(current), cx);
    }

    fn select_position(&mut self, position: usize, cx: &mut gpui::Context<Self>) {
        let next = self
            .matches
            .get(position)
            .and_then(|index| self.presented_items.get(*index))
            .filter(|item| !item.disabled)
            .map(|item| item.id.clone());
        if next.is_some() {
            self.provisional = next;
            self.list.scroll_to_reveal_item(position);
            cx.notify();
        }
    }

    fn hover(&mut self, id: &I, cx: &mut gpui::Context<Self>) {
        if self.provisional.as_ref() == Some(id) {
            return;
        }
        if let Some(position) = self.matches.iter().position(|index| {
            self.presented_items
                .get(*index)
                .is_some_and(|item| item.id == *id && !item.disabled)
        }) {
            self.select_position(position, cx);
        }
    }

    fn pointer_down(&mut self, id: I) {
        self.pointer_press = Some(PointerPress {
            id,
            generation: self.model_generation,
        });
    }

    fn pointer_up(&mut self, id: &I, inside: bool) -> bool {
        let matched = self
            .pointer_press
            .as_ref()
            .is_some_and(|press| press.id == *id && press.generation == self.model_generation);
        self.pointer_press = None;
        matched
            && inside
            && self.matches.iter().any(|index| {
                self.presented_items
                    .get(*index)
                    .is_some_and(|item| item.id == *id && !item.disabled)
            })
    }
}

impl<I: Clone + Eq + 'static> RenderOnce for ComboBox<I> {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, ComboBoxState::new);
        if let Some(handle) = self.handle {
            *handle.state.borrow_mut() = Some(state.downgrade());
        }
        state.update(cx, |state, cx| {
            state.synchronize(
                self.accessibility_name.clone(),
                self.selected,
                self.prompt,
                self.items,
                self.fallback,
                self.copy,
                self.disabled,
                self.busy,
                self.placement,
                self.panel_width,
                self.menu_with_filter_header,
                self.on_accept,
                self.on_lifecycle,
                window,
                cx,
            );
        });
        let reveal_selection = state.update(cx, |state, _| {
            std::mem::take(&mut state.selection_reveal_pending)
        });
        if reveal_selection {
            let reveal_state = state.downgrade();
            window.on_next_frame(move |window, _| {
                window.on_next_frame(move |window, cx| {
                    let _ = reveal_state.update(cx, |state, cx| {
                        if let Some(position) = state.provisional_position() {
                            state.list.scroll_to_reveal_item(position);
                            cx.notify();
                        }
                    });
                    window.refresh();
                });
                window.refresh();
            });
            window.refresh();
        }
        let theme = combo_box_theme(cx);
        let typography = crate::control_typography(cx);
        let font = typography.regular().clone();
        let snapshot = state.read(cx);
        let open = snapshot.open;
        let enabled = !snapshot.disabled;
        let label = snapshot.trigger_label();
        let focus = snapshot.trigger_focus.clone();
        let focused = focus.is_focused(window);

        let bounds_state = state.downgrade();
        let pointer_state = state.downgrade();
        let trigger_tracker = canvas(
            move |bounds, window, _| {
                let bounds = bounds.dilate(theme.metrics.border_width);
                let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                (bounds, hitbox)
            },
            move |_, (bounds, hitbox), window, cx| {
                let _ = bounds_state.update(cx, |state, cx| {
                    if state.trigger_bounds != Some(bounds) {
                        state.trigger_bounds = Some(bounds);
                        if state.open {
                            cx.notify();
                        }
                    }
                });
                let hitbox = hitbox.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.capture()
                        || event.button != MouseButton::Left
                        || event.modifiers.control
                        || !hitbox.is_hovered(window)
                        || !enabled
                    {
                        return;
                    }
                    window.prevent_default();
                    let _ = pointer_state.update(cx, |state, cx| {
                        if state.open {
                            state.close(ComboBoxCloseReason::Trigger, true, Some(window), cx);
                        } else {
                            state.open(None, false, window, cx);
                        }
                    });
                    cx.stop_propagation();
                });
            },
        )
        .absolute()
        .inset_0();
        let key_state = state.downgrade();
        let debug_selector = self.debug_selector;
        let focus_selector = debug_selector
            .as_ref()
            .map(|selector| format!("{selector}-keyboard-focus"))
            .unwrap_or_else(|| format!("{}-keyboard-focus", self.accessibility_name));
        let accessibility_name = self.accessibility_name;
        let paint = crate::floating_surface::hosted_combo_box_theme(cx)
            .map_or(theme.paint, |theme| theme.paint);
        let metrics = theme.metrics;
        let icon_offset = crate::icon::text_alignment_offset(
            typography.regular(),
            metrics.label_size,
            metrics.label_line_height,
            metrics.icon_baseline_center,
            window,
        );
        let (icon_trigger, custom_content) = match self.trigger {
            ComboBoxTrigger::Text => (false, None),
            ComboBoxTrigger::Icon => (true, None),
            ComboBoxTrigger::Custom(content) => (false, Some(content)),
        };
        let custom_trigger = custom_content.is_some();
        let text_trigger = !icon_trigger && !custom_trigger;
        let fill_parent = self.full_width && !icon_trigger;
        let hug = self.hug && !fill_parent;
        let trigger_background = paint.trigger_background(enabled, open);
        let trigger_shadow = paint.trigger_shadow(enabled, open);
        let trigger_bottom_edge = paint.trigger_bottom_edge(enabled, open);
        let (trigger_border, focus_ring) = trigger_edges(paint, enabled, focused);
        let trigger = div()
            .id(self.id)
            .debug_selector(move || {
                debug_selector.unwrap_or_else(|| accessibility_name.to_string())
            })
            .relative()
            .when(icon_trigger, |trigger| {
                trigger
                    .size(metrics.icon_trigger_size)
                    .flex_shrink_0()
                    .justify_center()
            })
            .when(custom_trigger, |trigger| {
                trigger
                    .h(self
                        .custom_trigger_content_height
                        .map_or(metrics.icon_trigger_size, |height| {
                            height + metrics.border_width * 2.0
                        }))
                    .min_w_0()
                    .when(fill_parent, |trigger| trigger.w_full())
            })
            .when(text_trigger, |trigger| {
                trigger
                    .h(metrics.trigger_height)
                    .when(fill_parent, |trigger| trigger.w_full())
                    .when(!fill_parent && !hug, |trigger| {
                        trigger.min_w(metrics.panel_width)
                    })
                    // A hugging trigger still stops where a reserving one would have, so one long
                    // value cannot crowd out the label naming it.
                    .when(hug, |trigger| trigger.max_w(metrics.panel_width))
                    .px(metrics.horizontal_padding)
                    .gap(metrics.gap)
            })
            .flex()
            .items_center()
            .rounded(metrics.trigger_corner_radius)
            .border(metrics.border_width)
            .border_color(trigger_border)
            .when(!custom_trigger, |trigger| {
                trigger
                    .bg(trigger_background)
                    .shadow(trigger_shadow.layers())
                    .shadow_outside_only()
                    .when(enabled && !open, |trigger| {
                        trigger
                            .hover(move |style| style.bg(paint.trigger_hover_background))
                            .active(move |style| {
                                style
                                    .bg(paint.trigger_pressed_background)
                                    .shadow(Vec::new())
                            })
                    })
            })
            .text_color(if enabled {
                paint.foreground
            } else {
                paint.disabled
            })
            .text_size(metrics.label_size)
            .line_height(metrics.label_line_height)
            .font(font)
            .cursor_default()
            .when(enabled, |trigger| trigger.track_focus(&focus))
            .children(
                self.trigger_leading
                    .filter(|_| !custom_trigger)
                    .map(|leading| {
                        let icon = leading(
                            paint.trigger_leading_foreground(icon_trigger, enabled),
                            metrics.trigger_icon_size,
                        );
                        if icon_trigger {
                            icon
                        } else {
                            div()
                                .relative()
                                .top(icon_offset)
                                .child(icon)
                                .into_any_element()
                        }
                    }),
            )
            .children(custom_content)
            .when(trigger_bottom_edge.a > 0.0 && !custom_trigger, |trigger| {
                trigger.child(
                    div()
                        .absolute()
                        .bottom(px(1.0))
                        .left(metrics.trigger_corner_radius)
                        .right(metrics.trigger_corner_radius)
                        .h(px(1.0))
                        .bg(trigger_bottom_edge),
                )
            })
            .when(text_trigger, |trigger| {
                trigger
                    .child(
                        div()
                            .debug_selector(|| "combo-box-trigger-label".to_owned())
                            .min_w_0()
                            .when(!hug, |value| value.flex_1())
                            .truncate()
                            .child(label),
                    )
                    .child(div().relative().top(icon_offset).child(Icon::new(
                        IconName::ChevronDown,
                        metrics.trigger_icon_size,
                        paint.muted,
                    )))
            })
            .when_some(focus_ring, |trigger, ring_color| {
                let gap = px(2.0);
                let position = gap + theme.focus_ring_width;
                trigger.child(
                    div()
                        .debug_selector(move || focus_selector.clone())
                        .absolute()
                        .top(-position)
                        .right(-position)
                        .bottom(-position)
                        .left(-position)
                        .rounded(metrics.trigger_corner_radius + gap)
                        .border(theme.focus_ring_width)
                        .border_color(ring_color),
                )
            })
            .child(trigger_tracker)
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                if !enabled || key_event_is_modified(event) {
                    return;
                }
                let key = event.keystroke.key.as_str();
                let (query, from_end) = match key {
                    "space" | "enter" | "down" => (None, false),
                    "up" => (None, true),
                    _ => {
                        let Some(text) = printable_text(event) else {
                            return;
                        };
                        (Some(text.to_owned()), false)
                    }
                };
                window.prevent_default();
                let _ = key_state.update(cx, |state, cx| {
                    state.open(query, from_end, window, cx);
                });
                cx.stop_propagation();
            });
        let trigger = if let Some(tooltip) = self.tooltip {
            tooltip
                .attach(trigger, TooltipTargetVisibility::Visible)
                .disabled(!enabled || open)
                .into_any_element()
        } else {
            trigger.into_any_element()
        };

        div()
            .relative()
            .min_w_0()
            .when(fill_parent, |root| root.w_full())
            .when(icon_trigger, |root| {
                root.size(metrics.icon_trigger_size).flex_shrink_0()
            })
            .child(trigger)
            .when(open, |root| {
                root.child(render_overlay(state, self.input_leading, window, cx))
            })
            .into_any_element()
    }
}

fn key_event_is_modified(event: &KeyDownEvent) -> bool {
    event.keystroke.modifiers.control
        || event.keystroke.modifiers.alt
        || event.keystroke.modifiers.platform
        || event.keystroke.modifiers.function
}

fn printable_text(event: &KeyDownEvent) -> Option<&str> {
    event.keystroke.key_char.as_deref().or_else(|| {
        (event.keystroke.key.chars().count() == 1).then_some(event.keystroke.key.as_str())
    })
}

fn unique_items<I: Clone + Eq>(items: Vec<ComboBoxItem<I>>) -> Vec<ComboBoxItem<I>> {
    let mut unique = Vec::with_capacity(items.len());
    for item in items {
        if !unique
            .iter()
            .any(|existing: &ComboBoxItem<I>| existing.id == item.id)
        {
            unique.push(item);
        }
    }
    unique
}

fn same_model<I: Eq>(current: &[ComboBoxItem<I>], next: &[ComboBoxItem<I>]) -> bool {
    current.len() == next.len()
        && current.iter().zip(next).all(|(current, next)| {
            current.id == next.id
                && current.label == next.label
                && current.description == next.description
                && current.keywords == next.keywords
                && current.disabled == next.disabled
                && current.trailing == next.trailing
                && current.shortcut == next.shortcut
        })
}

fn match_items<I>(items: &[ComboBoxItem<I>], query: &str) -> Vec<(usize, ComboBoxHighlights)> {
    fuzzy_filter(items, query, |item| {
        let target = FuzzyTarget::new(item.label.as_ref());
        let target = if let Some(description) = &item.description {
            target.field(description.as_ref())
        } else {
            target
        };
        item.keywords
            .iter()
            .fold(target, |target, keyword| target.field(keyword.as_ref()))
    })
    .into_iter()
    .map(|matched| {
        let item_index = matched.item_index();
        let item = &items[item_index];
        let description_field = usize::from(item.description.is_some());
        let description = item.description.as_ref().map_or_else(Vec::new, |text| {
            highlight_ranges(
                text.as_ref(),
                &matched.field_highlight_indices(description_field),
            )
        });
        (
            item_index,
            ComboBoxHighlights {
                label: highlight_ranges(item.label.as_ref(), &matched.field_highlight_indices(0)),
                description,
            },
        )
    })
    .collect()
}

fn render_overlay<I: Clone + Eq + 'static>(
    state: Entity<ComboBoxState<I>>,
    input_leading: Option<InputIconBuilder>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let typography = crate::control_typography(cx);
    let font = typography.regular().clone();
    let snapshot = state.read(cx);
    let collection_focused = snapshot.popup_focus.contains_focused(window, cx);
    let theme = combo_box_theme(cx);
    let Some(target) = snapshot.trigger_bounds else {
        return div().into_any_element();
    };
    let viewport = window.viewport_size();
    let content_height = if snapshot.busy || snapshot.matches.is_empty() {
        theme.metrics.row_height
    } else {
        snapshot
            .matches
            .iter()
            .enumerate()
            .fold(px(0.0), |height, (position, index)| {
                height
                    + snapshot
                        .presented_items
                        .get(*index)
                        .map_or(theme.metrics.row_height, |item| {
                            theme.metrics.row_height(item.description.is_some())
                        })
                    + if position == snapshot.ordinary_match_count
                        && snapshot.ordinary_match_count > 0
                        && snapshot.matches.len() > snapshot.ordinary_match_count
                    {
                        theme.metrics.group_separator_height
                    } else {
                        px(0.0)
                    }
            })
    };
    let panel_width = snapshot.panel_width.unwrap_or_else(|| {
        if snapshot.menu_with_filter_header {
            natural_menu_width(&snapshot.presented_items, theme, &typography, window)
        } else {
            theme.metrics.panel_width.max(target.size.width)
        }
    });
    let desired = size(
        panel_width,
        theme.metrics.input_height
            + theme.metrics.border_width
            + content_height
            + theme.metrics.panel_padding * 2.0
            + theme.metrics.border_width * 2.0,
    );
    let panel_size = constrain_anchored_size(
        size(
            desired.width,
            desired.height.min(theme.metrics.maximum_height),
        ),
        viewport,
        snapshot.placement.viewport_margin,
    );
    let bounds = place_anchored(target, panel_size, viewport, snapshot.placement);
    let popup_focus = snapshot.popup_focus.clone();
    let outside_state = state.downgrade();
    let outside = canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            let release_state = outside_state.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if !phase.capture()
                    || bounds.contains(&event.position)
                    || target.contains(&event.position)
                    || crate::menu::window_menu_is_open(window, cx)
                {
                    return;
                }
                window.prevent_default();
                let _ = outside_state.update(cx, |state, cx| {
                    state.close(ComboBoxCloseReason::Outside, true, Some(window), cx);
                });
                cx.stop_propagation();
            });
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase.capture()
                    && event.button == MouseButton::Left
                    && !bounds.contains(&event.position)
                {
                    let _ = release_state.update(cx, |state, _| state.pointer_press = None);
                }
            });
        },
    )
    .absolute()
    .inset_0();

    let input = state.read(cx).input.clone();
    let matches = Rc::clone(&state.read(cx).matches);
    let match_highlights = Rc::clone(&state.read(cx).match_highlights);
    let items = Rc::clone(&state.read(cx).presented_items);
    let identity_icons_reserved = items.iter().any(|item| item.leading_icon.is_some());
    let icon_offset = crate::icon::text_alignment_offset(
        typography.regular(),
        theme.metrics.label_size,
        theme.metrics.label_line_height,
        theme.metrics.icon_baseline_center,
        window,
    );
    let selected = state.read(cx).selected.clone();
    let provisional = state.read(cx).provisional.clone();
    let hovered_row = state.read(cx).hovered_row.clone();
    let ordinary_match_count = state.read(cx).ordinary_match_count;
    let menu_with_filter_header = state.read(cx).menu_with_filter_header;
    let busy = state.read(cx).busy;
    let copy = state.read(cx).copy.clone();
    let list_state = state.read(cx).list.clone();
    let rows_height = (bounds.size.height
        - theme.metrics.input_height
        - theme.metrics.border_width
        - theme.metrics.panel_padding * 2.0
        - theme.metrics.border_width * 2.0)
        .max(px(0.0));
    let row_owner = state.downgrade();
    let layout_owner = state.downgrade();
    let result_tracker = canvas(
        |_, _, _| (),
        move |bounds, _, window, cx| {
            let changed = layout_owner
                .update(cx, |state, _| {
                    state.result_viewport_size.replace(bounds.size) != Some(bounds.size)
                })
                .unwrap_or(false);
            if changed {
                // Reveal after the list has measured the new viewport. Geometry changes
                // request this once, so ordinary pointer scrolling stays independent.
                window.defer(cx, move |window, cx| {
                    let _ = layout_owner.update(cx, |state, cx| {
                        if state.open
                            && let Some(position) = state.provisional_position()
                        {
                            state.list.scroll_to_reveal_item(position);
                            cx.notify();
                        }
                    });
                    window.refresh();
                });
            }
        },
    )
    .absolute()
    .inset_0();
    let content = if busy {
        status_row(copy.busy_status, "combo-box-loading", theme).into_any_element()
    } else if matches.is_empty() {
        status_row(copy.empty_status, "combo-box-empty", theme).into_any_element()
    } else {
        list(list_state, move |position, _, _| {
            matches
                .get(position)
                .and_then(|index| items.get(*index))
                .map(|item| {
                    let highlights = match_highlights.get(position).cloned().unwrap_or_default();
                    let provisional = provisional.as_ref() == Some(&item.id);
                    let paints_selected = item.paints_selected(provisional);
                    render_row(
                        row_owner.clone(),
                        position,
                        item,
                        &highlights,
                        ComboBoxRowState {
                            selected: selected.as_ref() == Some(&item.id),
                            provisional: paints_selected,
                            hovered: hovered_row.as_ref() == Some(&item.id),
                        },
                        ComboBoxRowRenderContext {
                            identity_icons_reserved,
                            separator_before: position == ordinary_match_count
                                && ordinary_match_count > 0
                                && matches.len() > ordinary_match_count,
                            shortcut_gap: menu_with_filter_header.then_some(
                                (px(MENU_SHORTCUT_GAP) - theme.metrics.gap).max(px(0.0)),
                            ),
                            collection_focused,
                            theme,
                            label_font: typography.regular().clone(),
                            icon_offset,
                        },
                    )
                })
                .unwrap_or_else(|| div().into_any_element())
        })
        .h(rows_height)
        .w_full()
        .into_any_element()
    };
    let panel = div()
        .debug_selector(|| "combo-box-panel".to_owned())
        .absolute()
        .left(bounds.left())
        .top(bounds.top())
        .w(bounds.size.width)
        .h(bounds.size.height)
        .flex()
        .flex_col()
        .text_size(theme.metrics.label_size)
        .line_height(theme.metrics.label_line_height)
        .font(font)
        .block_mouse_except_scroll()
        .child(
            div()
                .debug_selector(|| "combo-box-input-row".to_owned())
                .h(theme.metrics.input_height)
                .flex_shrink_0()
                .px(theme.metrics.horizontal_padding)
                .flex()
                .items_center()
                .when_some(input_leading, |row, leading| {
                    row.gap(theme.metrics.gap).child(
                        div()
                            .debug_selector(|| "combo-box-input-leading".to_owned())
                            .w(theme.metrics.leading_width)
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(leading(theme.metrics.icon_size)),
                    )
                })
                .child(div().flex_1().min_w_0().child(input)),
        )
        .child(
            div()
                .w_full()
                .h(theme.shell.hairline())
                .flex_shrink_0()
                .bg(theme.shell.divider()),
        )
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .p(theme.metrics.panel_padding)
                .child(content)
                .child(result_tracker),
        );
    let panel = theme.shell.mount(panel);

    let up = state.downgrade();
    let down = state.downgrade();
    let page_up = state.downgrade();
    let page_down = state.downgrade();
    let home = state.downgrade();
    let end = state.downgrade();
    let accept = state.downgrade();
    let dismiss = state.downgrade();
    let overlay = div()
        .relative()
        .w(viewport.width)
        .h(viewport.height)
        .key_context(KEY_CONTEXT)
        .track_focus(&popup_focus)
        .child(outside)
        .child(panel)
        .child(crate::menu::combo_box_owned_overlay())
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
        .on_action(move |_: &MoveUp, _, cx| {
            let _ = up.update(cx, |state, cx| state.move_by(-1, cx));
            cx.stop_propagation();
        })
        .on_action(move |_: &MoveDown, _, cx| {
            let _ = down.update(cx, |state, cx| state.move_by(1, cx));
            cx.stop_propagation();
        })
        .on_action(move |_: &MovePageUp, _, cx| {
            let _ = page_up.update(cx, |state, cx| state.move_page(-1, theme.metrics, cx));
            cx.stop_propagation();
        })
        .on_action(move |_: &MovePageDown, _, cx| {
            let _ = page_down.update(cx, |state, cx| state.move_page(1, theme.metrics, cx));
            cx.stop_propagation();
        })
        .on_action(move |_: &MoveHome, _, cx| {
            let _ = home.update(cx, |state, cx| state.move_edge(true, cx));
            cx.stop_propagation();
        })
        .on_action(move |_: &MoveEnd, _, cx| {
            let _ = end.update(cx, |state, cx| state.move_edge(false, cx));
            cx.stop_propagation();
        })
        .on_action(move |_: &Accept, window, cx| {
            let _ = accept.update(cx, |state, cx| {
                state.accept(ComboBoxActivationSource::Keyboard, window, cx);
            });
            cx.stop_propagation();
        })
        .on_action(move |_: &Dismiss, window, cx| {
            let _ = dismiss.update(cx, |state, cx| {
                state.close(ComboBoxCloseReason::Escape, true, Some(window), cx);
            });
            cx.stop_propagation();
        });

    let overlay = anchored()
        .anchor(Corner::TopLeft)
        .position(gpui::point(px(0.0), px(0.0)))
        .snap_to_window()
        .child(overlay);
    // The ComboBox retains one deferred viewport root. A nested editor menu joins that root as a
    // later sibling, so both surfaces escape ancestor clipping without scheduling nested defers.
    crate::floating_surface::present(theme.shell.layer(false), overlay)
}

fn status_row(
    text: SharedString,
    selector: &'static str,
    theme: ComboBoxTheme,
) -> impl IntoElement {
    div()
        .debug_selector(move || selector.to_owned())
        .h(theme.metrics.row_height)
        .px(theme.metrics.horizontal_padding)
        .flex()
        .items_center()
        .text_size(theme.metrics.secondary_size)
        .line_height(theme.metrics.secondary_line_height)
        .text_color(theme.paint.muted)
        .child(text)
}

struct ComboBoxRowRenderContext {
    identity_icons_reserved: bool,
    separator_before: bool,
    shortcut_gap: Option<Pixels>,
    collection_focused: bool,
    theme: ComboBoxTheme,
    label_font: gpui::Font,
    icon_offset: Pixels,
}

fn render_row<I: Clone + Eq + 'static>(
    state: WeakEntity<ComboBoxState<I>>,
    position: usize,
    item: &ComboBoxItem<I>,
    highlights: &ComboBoxHighlights,
    state_paint: ComboBoxRowState,
    context: ComboBoxRowRenderContext,
) -> AnyElement {
    let ComboBoxRowRenderContext {
        identity_icons_reserved,
        separator_before,
        shortcut_gap,
        collection_focused,
        theme,
        label_font,
        icon_offset,
    } = context;
    let ComboBoxRowState {
        selected,
        provisional,
        hovered,
    } = state_paint;
    let row_paint = resolve_row_paint(
        theme.paint.rows,
        !item.disabled,
        provisional,
        hovered,
        collection_focused,
    );
    let foreground = if item.disabled {
        theme.paint.disabled
    } else if hovered {
        theme.paint.hover_foreground
    } else if provisional {
        theme.paint.selected_foreground
    } else {
        theme.paint.foreground
    };
    let secondary = if item.disabled {
        theme.paint.disabled
    } else if hovered {
        theme.paint.hover_foreground
    } else {
        theme.paint.muted
    };
    let foreground = row_paint.map_or(foreground, |paint| paint.foreground);
    let secondary = row_paint.map_or(secondary, |paint| paint.secondary);
    let icon_foreground = row_paint.map_or(foreground, |paint| paint.icon);
    let matched = row_paint.map_or(foreground, |paint| paint.matched);
    let id = item.id.clone();
    let logical_name = item.label.clone();
    let debug_selector = item.debug_selector.clone();
    let group_separator_selector = debug_selector
        .as_ref()
        .map_or_else(|| format!("combo-box-row-{position}"), Clone::clone);
    let hover_state = state.clone();
    let hover_tracking_state = state.clone();
    let mut row = div()
        .id(("combo-box-row", position))
        .debug_selector(move || debug_selector.unwrap_or_else(|| logical_name.to_string()))
        .relative()
        .w_full()
        .h(theme.metrics.row_height(item.description.is_some()))
        .px(theme.metrics.horizontal_padding)
        .flex()
        .items_center()
        .gap(theme.metrics.gap)
        .rounded(theme.metrics.row_radius())
        .text_color(foreground)
        .cursor_default()
        .when(provisional, |row| row.bg(theme.paint.selected_background))
        .when(hovered && !item.disabled, |row| {
            row.bg(theme.paint.hover_background)
        })
        .when_some(row_paint, |row, paint| {
            row.bg(paint.background)
                .border(theme.metrics.border_width)
                .border_color(paint.border)
        })
        .when(!item.disabled, |row| {
            let id = id.clone();
            let hover_id = id.clone();
            row.on_hover(move |hovered, _, cx| {
                let _ = hover_tracking_state.update(cx, |state, cx| {
                    if *hovered {
                        state.hovered_row = Some(hover_id.clone());
                    } else if state.hovered_row.as_ref() == Some(&hover_id) {
                        state.hovered_row = None;
                    } else {
                        return;
                    }
                    cx.notify();
                });
            })
            .on_mouse_move(move |_, _, cx| {
                let _ = hover_state.update(cx, |state, cx| state.hover(&id, cx));
            })
        });
    let mut state_gutter = div()
        .w(theme.metrics.leading_width)
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center();
    if selected {
        state_gutter = state_gutter
            .debug_selector(move || format!("combo-box-row-{position}-check"))
            .child(Icon::new(
                IconName::Check,
                theme.metrics.icon_size,
                icon_foreground,
            ));
    }
    let mut gutters = div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(theme.metrics.state_icon_gap)
        .relative()
        .top(icon_offset)
        .child(state_gutter);
    if identity_icons_reserved {
        let mut icon_gutter = div()
            .debug_selector(move || format!("combo-box-row-{position}-identity-icon"))
            .w(theme.metrics.identity_icon_width)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center();
        if let Some(icon) = &item.leading_icon {
            icon_gutter = icon_gutter.child(icon(icon_foreground, theme.metrics.icon_size));
        }
        gutters = gutters.child(icon_gutter);
    }
    row = row.child(gutters).child(
        div()
            .min_w_0()
            .flex_1()
            .flex()
            .flex_col()
            .justify_center()
            .child(
                div()
                    .debug_selector(move || format!("combo-box-row-{position}-label"))
                    .truncate()
                    .text_size(theme.metrics.label_size)
                    .line_height(theme.metrics.label_line_height)
                    .font(label_font)
                    .child(highlighted_text(
                        item.label.clone(),
                        &highlights.label,
                        matched,
                    )),
            )
            .when_some(item.description.clone(), |text, description| {
                text.child(
                    div()
                        .truncate()
                        .text_size(theme.metrics.secondary_size)
                        .line_height(theme.metrics.secondary_line_height)
                        .text_color(secondary)
                        .child(highlighted_text(
                            description,
                            &highlights.description,
                            matched,
                        )),
                )
            }),
    );
    if let Some(accessory) = item.trailing.clone() {
        let text = match accessory {
            ComboBoxAccessory::Text(text)
            | ComboBoxAccessory::Status(text)
            | ComboBoxAccessory::Shortcut(text) => text,
        };
        row = row.child(
            div()
                .debug_selector(move || format!("combo-box-row-{position}-accessory"))
                .flex_shrink_0()
                .text_size(theme.metrics.secondary_size)
                .line_height(theme.metrics.secondary_line_height)
                .text_color(secondary)
                .child(text),
        );
    }
    if let Some(shortcut) = item.shortcut.clone() {
        row = row.child(
            div()
                .debug_selector(move || format!("combo-box-row-{position}-shortcut"))
                .flex_shrink_0()
                .when_some(shortcut_gap, |shortcut, gap| shortcut.ml(gap))
                .text_size(theme.metrics.secondary_size)
                .line_height(theme.metrics.secondary_line_height)
                .text_color(secondary)
                .child(shortcut),
        );
    }
    if !item.disabled {
        let hover_state = state.clone();
        let down_state = state.clone();
        let up_state = state.clone();
        let move_state = state;
        let hover_id = item.id.clone();
        let down_id = item.id.clone();
        let up_id = item.id.clone();
        row = row.child(
            canvas(
                |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                move |_, hitbox, window, cx| {
                    // A model or layout change can replace the row under a stationary pointer.
                    let actually_hovered = hitbox.is_hovered(window);
                    if actually_hovered != hovered {
                        let hover_state = hover_state.clone();
                        let hover_id = hover_id.clone();
                        cx.defer(move |cx| {
                            let _ = hover_state.update(cx, |state, cx| {
                                if !state.open {
                                    return;
                                }
                                if actually_hovered {
                                    state.hovered_row = Some(hover_id);
                                } else if state.hovered_row.as_ref() == Some(&hover_id) {
                                    state.hovered_row = None;
                                } else {
                                    return;
                                }
                                cx.notify();
                            });
                        });
                    }
                    let down_hitbox = hitbox.clone();
                    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                        if !phase.capture()
                            || event.button != MouseButton::Left
                            || event.modifiers.control
                            || !down_hitbox.is_hovered(window)
                        {
                            return;
                        }
                        window.prevent_default();
                        let _ = down_state.update(cx, |state, _| {
                            state.pointer_down(down_id.clone());
                        });
                        cx.stop_propagation();
                    });
                    let up_hitbox = hitbox.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                        if !phase.capture()
                            || event.button != MouseButton::Left
                            || !up_hitbox.is_hovered(window)
                        {
                            return;
                        }
                        let accepted = up_state
                            .update(cx, |state, _| {
                                state.pointer_up(
                                    &up_id,
                                    !event.modifiers.control && up_hitbox.is_hovered(window),
                                )
                            })
                            .unwrap_or(false);
                        if accepted {
                            window.prevent_default();
                            let _ = up_state.update(cx, |state, cx| {
                                state.provisional = Some(up_id.clone());
                                state.accept(ComboBoxActivationSource::Pointer, window, cx);
                            });
                            cx.stop_propagation();
                        }
                    });
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase.capture() && event.pressed_button != Some(MouseButton::Left) {
                            let _ = move_state.update(cx, |state, _| state.pointer_press = None);
                        }
                    });
                },
            )
            .absolute()
            .inset_0(),
        );
    }
    if separator_before {
        div()
            .w_full()
            .h(theme.metrics.group_separator_height
                + theme.metrics.row_height(item.description.is_some()))
            .flex()
            .flex_col()
            .child(
                div()
                    .debug_selector(move || format!("{group_separator_selector}-group-separator"))
                    .h(theme.metrics.group_separator_height)
                    .mx(theme.metrics.panel_padding)
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .h(theme.shell.hairline())
                            .w_full()
                            .bg(theme.shell.divider()),
                    ),
            )
            .child(row)
            .into_any_element()
    } else {
        row.into_any_element()
    }
}

fn natural_menu_width<I>(
    items: &[ComboBoxItem<I>],
    theme: ComboBoxTheme,
    typography: &crate::ControlTypography,
    window: &Window,
) -> Pixels {
    let measure = |text: &SharedString, size: Pixels, font: &gpui::Font| {
        window
            .text_system()
            .shape_line(
                text.clone(),
                size,
                &[gpui::TextRun {
                    len: text.len(),
                    font: font.clone(),
                    color: gpui::rgba(0).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            )
            .width
    };
    let identity_icons_reserved = items.iter().any(|item| item.leading_icon.is_some());
    let leading = theme.metrics.leading_width
        + if identity_icons_reserved {
            theme.metrics.state_icon_gap + theme.metrics.identity_icon_width
        } else {
            px(0.0)
        };
    let fixed = theme.metrics.border_width * 2.0
        + theme.metrics.panel_padding * 2.0
        + theme.metrics.horizontal_padding * 2.0
        + leading
        + theme.metrics.gap;
    let widest = items.iter().fold(px(0.0), |widest, item| {
        let label = measure(&item.label, theme.metrics.label_size, typography.regular());
        let description = item.description.as_ref().map_or(px(0.0), |description| {
            measure(
                description,
                theme.metrics.secondary_size,
                typography.regular(),
            )
        });
        let text = label.max(description);
        let accessory = item.trailing.as_ref().map_or(px(0.0), |accessory| {
            let text = match accessory {
                ComboBoxAccessory::Text(text)
                | ComboBoxAccessory::Status(text)
                | ComboBoxAccessory::Shortcut(text) => text,
            };
            theme.metrics.gap + measure(text, theme.metrics.secondary_size, typography.regular())
        });
        let shortcut = item.shortcut.as_ref().map_or(px(0.0), |shortcut| {
            px(MENU_SHORTCUT_GAP)
                + measure(
                    shortcut,
                    theme.metrics.secondary_size,
                    typography.shortcut(),
                )
        });
        widest.max(fixed + text + accessory + shortcut)
    });
    widest
        .max(px(MENU_MINIMUM_WIDTH))
        .min(px(MENU_MAXIMUM_WIDTH))
}

fn highlighted_text(text: SharedString, ranges: &[Range<usize>], highlight: Rgba) -> AnyElement {
    StyledText::new(text)
        .with_highlights(
            ranges
                .iter()
                .cloned()
                .map(|range| (range, highlight.into())),
        )
        .into_any_element()
}

fn resolve_row_paint(
    rows: Option<crate::ListRowPaints>,
    enabled: bool,
    selected: bool,
    hovered: bool,
    collection_focused: bool,
) -> Option<crate::ListRowPaint> {
    rows.map(|rows| rows.resolve_for_collection(enabled, selected, hovered, collection_focused))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_paint(seed: u32) -> crate::ListRowPaint {
        let colors = [seed, seed + 1, seed + 2, seed + 3, seed + 4, seed + 5].map(gpui::rgba);
        crate::ListRowPaint::new(
            colors[0], colors[1], colors[2], colors[3], colors[4], colors[5],
        )
    }

    #[test]
    fn density_scales_combo_bounds_but_not_trigger_radius() {
        let paint = ComboBoxPaint::new(
            gpui::rgba(0),
            gpui::rgba(0),
            gpui::rgba(0),
            gpui::rgba(0),
            gpui::rgba(0),
            gpui::rgba(0),
            gpui::rgba(0),
            gpui::rgba(0),
            gpui::rgba(0),
        );
        let theme = ComboBoxTheme::new(
            paint,
            ComboBoxMetrics::new(px(240.0), px(28.0)).trigger_shape(px(6.0)),
        );
        let comfortable = theme.scaled_metrics(1.0, 1.25);

        assert!(comfortable.metrics.trigger_height > theme.metrics.trigger_height);
        assert_eq!(
            comfortable.metrics.trigger_corner_radius,
            theme.metrics.trigger_corner_radius
        );
    }

    #[test]
    fn renderer_should_reach_selected_hovered_row_paint() {
        let normal = row_paint(10);
        let hovered = row_paint(20);
        let selected = row_paint(30);
        let selected_hovered = row_paint(40);
        let disabled = row_paint(50);
        let rows = crate::ListRowPaints::new(normal, hovered, selected, selected_hovered, disabled);

        assert_eq!(
            resolve_row_paint(Some(rows), true, true, true, true),
            Some(selected_hovered)
        );
    }

    #[test]
    fn trigger_icon_colors_should_not_replace_text_or_selection_colors() {
        let text = gpui::rgba(0x111111ff);
        let icon = gpui::rgba(0xabcdef80);
        let disabled = gpui::rgba(0x123456ff);
        let paint = ComboBoxPaint::new(text, text, text, text, text, text, text, text, text)
            .trigger_icon_colors(icon, disabled);
        assert_eq!(paint.trigger_icon_foreground, icon);
        assert_eq!(paint.trigger_icon_disabled, disabled);
        assert_eq!(paint.foreground, text);
        assert_eq!(paint.disabled, text);
        assert_eq!(paint.selected_foreground, text);
        assert_eq!(paint.trigger_leading_foreground(false, true), text);
        assert_eq!(paint.trigger_leading_foreground(false, false), text);
        assert_eq!(paint.trigger_leading_foreground(true, true), icon);
        assert_eq!(paint.trigger_leading_foreground(true, false), disabled);
    }

    #[test]
    fn focused_trigger_keeps_its_ordinary_border_below_the_focus_ring() {
        let ordinary = gpui::rgba(0x112233ff);
        let focus = gpui::rgba(0x445566ff);
        let fill = gpui::rgba(0);
        let paint = ComboBoxPaint::new(fill, fill, fill, fill, fill, fill, fill, ordinary, focus);

        assert_eq!(trigger_edges(paint, true, false), (ordinary, None));
        assert_eq!(trigger_edges(paint, true, true), (ordinary, Some(focus)));
        assert_eq!(trigger_edges(paint, false, true), (ordinary, None));
        assert_eq!(
            trigger_edges(
                ComboBoxPaint::new(
                    fill,
                    fill,
                    fill,
                    fill,
                    fill,
                    fill,
                    fill,
                    ordinary,
                    gpui::rgba(0),
                ),
                true,
                true,
            ),
            (ordinary, None),
            "an inactive presentation must not submit a transparent focus primitive"
        );
    }

    #[test]
    fn ordinary_trigger_elevation_follows_enabled_and_open_state() {
        let normal = gpui::rgba(0x111111ff);
        let hovered = gpui::rgba(0x222222ff);
        let pressed = gpui::rgba(0x333333ff);
        let disabled = gpui::rgba(0x444444ff);
        let border = gpui::rgba(0x555555ff);
        let edge = gpui::rgba(0x666666ff);
        let shadow = crate::ControlShadow::single(crate::ControlShadowLayer::new(
            gpui::rgba(0x777777ff).into(),
            px(0.0),
            px(1.0),
            px(2.0),
            px(-1.0),
        ));
        let paint = ComboBoxPaint::new(
            normal, normal, disabled, normal, normal, normal, hovered, normal, normal,
        )
        .trigger_state_backgrounds(pressed, disabled)
        .trigger_elevation(shadow, Some(border), edge);

        assert_eq!(paint.trigger_background(true, false), normal);
        assert_eq!(paint.trigger_background(true, true), pressed);
        assert_eq!(paint.trigger_background(false, false), disabled);
        assert_eq!(paint.trigger_border, border);
        assert_eq!(paint.trigger_shadow(true, false), shadow);
        assert_eq!(
            paint.trigger_shadow(true, true),
            crate::ControlShadow::none()
        );
        assert_eq!(
            paint.trigger_shadow(false, false),
            crate::ControlShadow::none()
        );
        assert_eq!(paint.trigger_bottom_edge(true, false), edge);
        assert_eq!(paint.trigger_bottom_edge(true, true), edge);
        assert_eq!(paint.trigger_bottom_edge(false, false), Rgba::default());
    }

    #[test]
    fn row_hover_paint_should_not_replace_selection_paint() {
        let base = gpui::rgba(0x111111ff);
        let selected = gpui::rgba(0x222222ff);
        let hovered = gpui::rgba(0xabcdef80);
        let hover_foreground = gpui::rgba(0x123456ff);
        let paint = ComboBoxPaint::new(base, base, base, selected, base, base, base, base, base)
            .hover_background(hovered)
            .hover_foreground(hover_foreground);
        assert_eq!(paint.hover_background, hovered);
        assert_eq!(paint.hover_foreground, hover_foreground);
        assert_eq!(paint.selected_foreground, base);
        assert_eq!(paint.selected_background, selected);
        assert_eq!(paint.trigger_hover_background, base);
    }

    #[test]
    fn role_line_boxes_scale_independently_from_fixed_combo_extents() {
        let metrics = ComboBoxMetrics::new(px(240.0), px(28.0))
            .geometry(px(260.0), px(28.0), px(28.0), px(40.0))
            .text_geometry(px(18.0), px(15.0), px(14.0))
            .scaled(1.5, 1.0);

        assert_eq!(metrics.label_line_height, px(27.0));
        assert_eq!(metrics.secondary_line_height, px(22.5));
        assert_eq!(metrics.input_height, px(37.0));
        assert_eq!(metrics.row_height, px(37.0));
        assert_eq!(metrics.described_row_height, px(56.5));
    }

    #[test]
    fn filtering_should_match_unicode_case_without_slicing_text() {
        let items = vec![
            ComboBoxItem::new(1, "Ångström").keywords(["measurement"]),
            ComboBoxItem::new(2, "Remote over SSH"),
        ];

        let matches = match_items(&items, "ång");

        assert_eq!(matches[0].0, 0);
        assert_eq!(matches[0].1.label, vec![0..4]);
    }

    #[test]
    fn filtering_should_report_unicode_description_highlights() {
        let items = vec![ComboBoxItem::new(1, "Workspace").description("/project/Ångström")];

        let matches = match_items(&items, "ång");

        assert_eq!(matches[0].1.description, vec![9..13]);
    }

    #[test]
    fn filtering_should_require_every_token_across_semantic_fields() {
        let items = vec![
            ComboBoxItem::new(1, "This Mac")
                .description("Start at home")
                .keywords(["workspace"]),
        ];

        assert_eq!(match_items(&items, "mac home workspace")[0].0, 0);
    }

    #[test]
    fn filtering_should_rank_contiguous_matches() {
        let items = vec![
            ComboBoxItem::new(1, "random"),
            ComboBoxItem::new(2, "projects"),
        ];

        assert_eq!(match_items(&items, "ro")[0].0, 1);
    }

    #[test]
    fn filtering_should_preserve_item_order_for_an_empty_query() {
        let items = vec![ComboBoxItem::new(1, "Third"), ComboBoxItem::new(2, "First")];

        assert_eq!(
            match_items(&items, "")
                .into_iter()
                .map(|(index, _)| index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn duplicate_identities_should_keep_only_the_first_item() {
        let items = unique_items(vec![
            ComboBoxItem::new(1, "First"),
            ComboBoxItem::new(1, "Later"),
        ]);

        assert_eq!(items[0].label(), "First");
    }

    #[test]
    fn fallback_provider_should_preserve_exact_query_in_typed_identity() {
        let fallback =
            ComboBoxFallback::new(|query| ComboBoxItem::new(query.to_owned(), "Use exact query"));

        assert_eq!(fallback.items(" Mixed Case ", 0)[0].id(), " Mixed Case ");
    }

    #[test]
    fn no_match_provider_should_preserve_exact_query_in_each_typed_identity() {
        let fallback = ComboBoxFallback::when_no_matches(|query| {
            vec![
                ComboBoxItem::new((0, query.to_owned()), "Local Workspace"),
                ComboBoxItem::new((1, query.to_owned()), "Remote Workspace"),
            ]
        });
        let identities: Vec<_> = fallback
            .items(" Mixed Case ", 0)
            .into_iter()
            .map(|item| item.id)
            .collect();
        assert_eq!(
            identities,
            vec![
                (0, " Mixed Case ".to_owned()),
                (1, " Mixed Case ".to_owned())
            ]
        );
    }
}
