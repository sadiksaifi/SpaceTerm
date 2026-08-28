use std::{
    collections::HashMap,
    fmt,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, App, BorrowAppContext as _, Bounds, Corner, ElementId, Entity, FocusHandle, Global,
    HitboxBehavior, InteractiveElement as _, IntoElement, KeyBinding, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, RenderOnce,
    Rgba, SharedString, StatefulInteractiveElement as _, Styled as _, Task, WeakEntity,
    WeakFocusHandle, Window, WindowId, actions, anchored, canvas, deferred, div, point,
    prelude::FluentBuilder as _, px, size,
};

const KEY_CONTEXT: &str = "SpaceTermMenu";
const TYPEAHEAD_RESET: Duration = Duration::from_millis(700);
const SUBMENU_OPEN_DELAY: Duration = Duration::from_millis(100);
const SUBMENU_CLOSE_GRACE: Duration = Duration::from_millis(150);
const OVERLAY_PRIORITY: usize = 1;

actions!(
    spaceterm_menu,
    [
        MoveUp, MoveDown, MoveHome, MoveEnd, MoveLeft, MoveRight, Activate, Dismiss
    ]
);

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", MoveUp, Some(KEY_CONTEXT)),
        KeyBinding::new("down", MoveDown, Some(KEY_CONTEXT)),
        KeyBinding::new("home", MoveHome, Some(KEY_CONTEXT)),
        KeyBinding::new("end", MoveEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("left", MoveLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("right", MoveRight, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", Activate, Some(KEY_CONTEXT)),
        KeyBinding::new("space", Activate, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT)),
    ]);
    if !cx.has_global::<MenuCoordinator>() {
        cx.set_global(MenuCoordinator::default());
    }
}

/// The input path that selected a menu entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuActivationSource {
    /// A primary pointer release selected the entry.
    Pointer,
    /// Keyboard navigation selected the entry.
    Keyboard,
}

/// A typed semantic menu selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MenuActivation<A> {
    /// An ordinary action was selected.
    Action {
        /// Caller-owned action identity.
        action: A,
        /// Input path that selected the action.
        source: MenuActivationSource,
    },
    /// A checkbox was selected, proposing the opposite checked state.
    Checkbox {
        /// Caller-owned action identity.
        action: A,
        /// Proposed checked state.
        checked: bool,
        /// Input path that selected the action.
        source: MenuActivationSource,
    },
    /// An option in a radio group was selected.
    Radio {
        /// Caller-owned action identity.
        action: A,
        /// Zero-based index within the semantic radio group.
        index: usize,
        /// Input path that selected the action.
        source: MenuActivationSource,
    },
}

impl<A> MenuActivation<A> {
    /// Returns the caller-owned action identity.
    pub fn action(&self) -> &A {
        match self {
            Self::Action { action, .. }
            | Self::Checkbox { action, .. }
            | Self::Radio { action, .. } => action,
        }
    }

    /// Returns the input path that selected the action.
    pub fn source(&self) -> MenuActivationSource {
        match self {
            Self::Action { source, .. }
            | Self::Checkbox { source, .. }
            | Self::Radio { source, .. } => *source,
        }
    }

    /// Consumes the event and returns the caller-owned action identity.
    pub fn into_action(self) -> A {
        match self {
            Self::Action { action, .. }
            | Self::Checkbox { action, .. }
            | Self::Radio { action, .. } => action,
        }
    }
}

/// Why an open menu chain closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuCloseReason {
    /// An action or value was selected.
    Activated,
    /// Escape dismissed the chain.
    Escape,
    /// A pointer press outside the complete chain dismissed it.
    Outside,
    /// The trigger toggled its open chain closed.
    Trigger,
    /// The operating-system window deactivated.
    Deactivated,
    /// Another menu chain in the same window replaced this chain.
    Replaced,
    /// The control disappeared while open.
    TargetDisappeared,
    /// Synchronization made the control unavailable while open.
    Disabled,
}

/// One exact menu-chain lifecycle transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuLifecycleEvent {
    /// The chain became open.
    Opened,
    /// The chain became closed for the supplied reason.
    Closed(MenuCloseReason),
}

/// A proposed context-menu opening at a window-relative pointer position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContextMenuOpenRequest {
    position: Point<Pixels>,
}

impl ContextMenuOpenRequest {
    /// Returns the window-relative pointer position requested by the context gesture.
    pub fn position(self) -> Point<Pixels> {
        self.position
    }
}

/// A typed picker value change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerChange<T> {
    value: T,
    source: MenuActivationSource,
}

impl<T> PickerChange<T> {
    /// Returns the selected value.
    pub fn value(&self) -> &T {
        &self.value
    }

    /// Returns the input path that selected the value.
    pub fn source(&self) -> MenuActivationSource {
        self.source
    }

    /// Consumes the event and returns the selected value.
    pub fn into_value(self) -> T {
        self.value
    }
}

/// Standard bounded menu widths.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MenuSize {
    /// Narrow menus, conventionally 208 logical pixels.
    Small,
    /// Regular menus, conventionally 220 logical pixels.
    #[default]
    Regular,
    /// Wide menus, conventionally 248 logical pixels.
    Wide,
}

/// Preferred side of a root menu relative to its trigger.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MenuPlacement {
    /// Place below the trigger.
    #[default]
    Bottom,
    /// Place above the trigger.
    Top,
    /// Place to the left of the trigger.
    Left,
    /// Place to the right of the trigger.
    Right,
}

/// Cross-axis alignment between a root menu and its trigger.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MenuAlignment {
    /// Align leading edges.
    #[default]
    Start,
    /// Align centers.
    Center,
    /// Align trailing edges.
    End,
}

/// Narrow placement policy for a root menu.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MenuPlacementConfig {
    placement: MenuPlacement,
    alignment: MenuAlignment,
    offset: Pixels,
    viewport_margin: Pixels,
}

impl MenuPlacementConfig {
    /// Creates placement with a four-pixel trigger offset and twelve-pixel viewport margin.
    pub fn new(placement: MenuPlacement, alignment: MenuAlignment) -> Self {
        Self {
            placement,
            alignment,
            offset: px(4.0),
            viewport_margin: px(12.0),
        }
    }

    /// Sets the gap between trigger and menu.
    pub fn offset(mut self, offset: Pixels) -> Self {
        self.offset = offset.max(px(0.0));
        self
    }

    /// Sets the minimum distance from the viewport edge.
    pub fn viewport_margin(mut self, margin: Pixels) -> Self {
        self.viewport_margin = margin.max(px(0.0));
        self
    }
}

impl Default for MenuPlacementConfig {
    fn default() -> Self {
        Self::new(MenuPlacement::default(), MenuAlignment::default())
    }
}

/// Bounded paint values shared by menus, context menus, and pickers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MenuPaint {
    background: Rgba,
    border: Rgba,
    foreground: Rgba,
    muted: Rgba,
    disabled: Rgba,
    selected_background: Rgba,
    selected_foreground: Rgba,
    destructive: Rgba,
    separator: Rgba,
    trigger_background: Rgba,
    trigger_hover_background: Rgba,
    trigger_border: Rgba,
    focus_border: Rgba,
}

impl MenuPaint {
    /// Creates menu paint. Trigger paint initially reuses the menu surface and border.
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
        destructive: Rgba,
        separator: Rgba,
    ) -> Self {
        Self {
            background,
            border,
            foreground,
            muted,
            disabled,
            selected_background,
            selected_foreground,
            destructive,
            separator,
            trigger_background: background,
            trigger_hover_background: selected_background,
            trigger_border: border,
            focus_border: selected_background,
        }
    }

    /// Sets the trigger's normal, hovered, and border colors.
    pub fn trigger(mut self, background: Rgba, hovered: Rgba, border: Rgba) -> Self {
        self.trigger_background = background;
        self.trigger_hover_background = hovered;
        self.trigger_border = border;
        self
    }

    /// Sets the keyboard focus border color used by menu and picker triggers.
    pub fn focus_border(mut self, color: Rgba) -> Self {
        self.focus_border = color;
        self
    }
}

/// Metrics for one standard menu density.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MenuMetrics {
    panel_width: Pixels,
    row_height: Pixels,
    section_height: Pixels,
    separator_height: Pixels,
    trigger_height: Pixels,
    horizontal_padding: Pixels,
    indicator_width: Pixels,
    gap: Pixels,
    corner_radius: Pixels,
    border_width: Pixels,
    font_size: Pixels,
    shortcut_font_size: Pixels,
    panel_padding: Pixels,
    submenu_gap: Pixels,
}

impl MenuMetrics {
    /// Creates metrics with compact desktop defaults around the supplied panel width and row height.
    pub fn new(panel_width: Pixels, row_height: Pixels) -> Self {
        Self {
            panel_width,
            row_height,
            section_height: px(22.0),
            separator_height: px(9.0),
            trigger_height: row_height,
            horizontal_padding: px(8.0),
            indicator_width: px(14.0),
            gap: px(6.0),
            corner_radius: px(6.0),
            border_width: px(1.0),
            font_size: px(12.0),
            shortcut_font_size: px(11.0),
            panel_padding: px(4.0),
            submenu_gap: px(2.0),
        }
    }

    /// Sets the menu trigger height.
    pub fn trigger_height(mut self, height: Pixels) -> Self {
        self.trigger_height = height;
        self
    }

    /// Sets horizontal row and trigger padding.
    pub fn horizontal_padding(mut self, padding: Pixels) -> Self {
        self.horizontal_padding = padding;
        self
    }

    /// Sets the leading indicator column width.
    pub fn indicator_width(mut self, width: Pixels) -> Self {
        self.indicator_width = width;
        self
    }

    /// Sets spacing between row columns.
    pub fn gap(mut self, gap: Pixels) -> Self {
        self.gap = gap;
        self
    }

    /// Sets panel and trigger corner radius.
    pub fn corner_radius(mut self, radius: Pixels) -> Self {
        self.corner_radius = radius;
        self
    }

    /// Sets stable panel and trigger border width.
    pub fn border_width(mut self, width: Pixels) -> Self {
        self.border_width = width;
        self
    }

    /// Sets menu label and shortcut font sizes.
    pub fn font_sizes(mut self, label: Pixels, shortcut: Pixels) -> Self {
        self.font_size = label;
        self.shortcut_font_size = shortcut;
        self
    }

    /// Sets vertical panel padding and the horizontal submenu gap.
    pub fn panel_spacing(mut self, padding: Pixels, submenu_gap: Pixels) -> Self {
        self.panel_padding = padding;
        self.submenu_gap = submenu_gap;
        self
    }
}

/// Complete metric catalog for menu densities.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MenuSizes {
    small: MenuMetrics,
    regular: MenuMetrics,
    wide: MenuMetrics,
}

impl MenuSizes {
    /// Creates the complete three-width menu metric catalog.
    pub fn new(small: MenuMetrics, regular: MenuMetrics, wide: MenuMetrics) -> Self {
        Self {
            small,
            regular,
            wide,
        }
    }

    fn resolve(self, size: MenuSize) -> MenuMetrics {
        match size {
            MenuSize::Small => self.small,
            MenuSize::Regular => self.regular,
            MenuSize::Wide => self.wide,
        }
    }
}

/// Application-owned menu colors and bounded metrics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MenuTheme {
    paint: MenuPaint,
    sizes: MenuSizes,
}

impl MenuTheme {
    /// Creates a complete theme for the menu family.
    pub fn new(paint: MenuPaint, sizes: MenuSizes) -> Self {
        Self { paint, sizes }
    }

    fn resolve(self, size: MenuSize) -> MenuStyle {
        MenuStyle {
            paint: self.paint,
            metrics: self.sizes.resolve(size),
        }
    }
}

impl Global for MenuTheme {}

#[derive(Clone, Copy)]
struct MenuStyle {
    paint: MenuPaint,
    metrics: MenuMetrics,
}

type IconBuilder = Rc<dyn Fn(Rgba) -> AnyElement>;
type InternalActivation = Rc<dyn Fn(MenuActivationSource, &mut Window, &mut App)>;
type PickerChangeHandler<T> = Rc<dyn Fn(&PickerChange<T>, &mut Window, &mut App)>;
type MenuLifecycleHandler = Rc<dyn Fn(&MenuLifecycleEvent, &mut App)>;
type ContextOpenHandler = Rc<dyn Fn(&ContextMenuOpenRequest, &mut Window, &mut App) -> bool>;

/// One semantic entry in a menu tree.
#[derive(Clone)]
pub struct MenuEntry<A> {
    kind: MenuEntryKind<A>,
}

#[derive(Clone)]
enum MenuEntryKind<A> {
    Item(MenuItem<A>),
    Separator,
    Section {
        label: Option<SharedString>,
        entries: Vec<MenuEntry<A>>,
    },
    Submenu {
        item: MenuItem<()>,
        entries: Vec<MenuEntry<A>>,
    },
}

#[derive(Clone)]
struct MenuItem<A> {
    label: SharedString,
    action: A,
    disabled: bool,
    destructive: bool,
    shortcut: Option<SharedString>,
    icon: Option<IconBuilder>,
    mark: EntryMark,
    debug_selector: Option<String>,
}

/// One configurable option in a semantic radio group.
pub struct MenuRadioOption<A> {
    item: MenuItem<A>,
}

impl<A> MenuRadioOption<A> {
    /// Creates an enabled radio option.
    pub fn new(label: impl Into<SharedString>, action: A) -> Self {
        Self {
            item: MenuItem {
                label: label.into(),
                action,
                disabled: false,
                destructive: false,
                shortcut: None,
                icon: None,
                mark: EntryMark::None,
                debug_selector: None,
            },
        }
    }

    /// Controls whether navigation and activation may reach this option.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.item.disabled = disabled;
        self
    }

    /// Marks the option as destructive.
    pub fn destructive(mut self, destructive: bool) -> Self {
        self.item.destructive = destructive;
        self
    }

    /// Adds a display-only shortcut hint.
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.item.shortcut = Some(shortcut.into());
        self
    }

    /// Adds a leading icon built with the resolved row foreground color.
    ///
    /// The selected radio mark replaces the icon within the shared leading slot.
    pub fn icon(mut self, build: impl Fn(Rgba) -> AnyElement + 'static) -> Self {
        self.item.icon = Some(Rc::new(build));
        self
    }

    /// Adds a stable selector used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.item.debug_selector = Some(selector.into());
        self
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum EntryMark {
    #[default]
    None,
    Checkbox(bool),
    Radio {
        selected: bool,
        index: usize,
    },
}

impl<A> MenuEntry<A> {
    /// Creates an ordinary action entry. The label is its logical accessibility name.
    pub fn action(label: impl Into<SharedString>, action: A) -> Self {
        Self::item(label.into(), action, EntryMark::None)
    }

    /// Creates a checkbox action entry.
    pub fn checkbox(label: impl Into<SharedString>, checked: bool, action: A) -> Self {
        Self::item(label.into(), action, EntryMark::Checkbox(checked))
    }

    fn item(label: SharedString, action: A, mark: EntryMark) -> Self {
        Self {
            kind: MenuEntryKind::Item(MenuItem {
                label,
                action,
                disabled: false,
                destructive: false,
                shortcut: None,
                icon: None,
                mark,
                debug_selector: None,
            }),
        }
    }

    /// Creates a separator.
    pub fn separator() -> Self {
        Self {
            kind: MenuEntryKind::Separator,
        }
    }

    /// Creates a labeled section group rendered in the current panel.
    pub fn section(label: impl Into<SharedString>, entries: Vec<Self>) -> Self {
        Self {
            kind: MenuEntryKind::Section {
                label: Some(label.into()),
                entries,
            },
        }
    }

    /// Creates an unlabeled semantic section group rendered in the current panel.
    pub fn group(entries: Vec<Self>) -> Self {
        Self {
            kind: MenuEntryKind::Section {
                label: None,
                entries,
            },
        }
    }

    /// Creates a radio group with at most one selected option.
    ///
    /// Selection state remains caller-owned. A nonempty group falls back to its first option when
    /// the selected index is absent or out of range.
    pub fn radio_group(selected: Option<usize>, options: Vec<MenuRadioOption<A>>) -> Self {
        let normalized = (!options.is_empty())
            .then(|| selected.filter(|index| *index < options.len()).unwrap_or(0));
        Self::group(
            options
                .into_iter()
                .enumerate()
                .map(|(index, mut option)| {
                    option.item.mark = EntryMark::Radio {
                        selected: normalized == Some(index),
                        index,
                    };
                    Self {
                        kind: MenuEntryKind::Item(option.item),
                    }
                })
                .collect(),
        )
    }

    /// Creates a submenu entry.
    pub fn submenu(label: impl Into<SharedString>, entries: Vec<Self>) -> Self {
        Self {
            kind: MenuEntryKind::Submenu {
                item: MenuItem {
                    label: label.into(),
                    action: (),
                    disabled: false,
                    destructive: false,
                    shortcut: None,
                    icon: None,
                    mark: EntryMark::None,
                    debug_selector: None,
                },
                entries,
            },
        }
    }

    /// Controls whether an action or submenu can be reached or activated.
    pub fn disabled(mut self, disabled: bool) -> Self {
        match &mut self.kind {
            MenuEntryKind::Item(item) => item.disabled = disabled,
            MenuEntryKind::Submenu { item, .. } => item.disabled = disabled,
            MenuEntryKind::Separator | MenuEntryKind::Section { .. } => {}
        }
        self
    }

    /// Marks an action or submenu as destructive.
    pub fn destructive(mut self, destructive: bool) -> Self {
        match &mut self.kind {
            MenuEntryKind::Item(item) => item.destructive = destructive,
            MenuEntryKind::Submenu { item, .. } => item.destructive = destructive,
            MenuEntryKind::Separator | MenuEntryKind::Section { .. } => {}
        }
        self
    }

    /// Adds a display-only shortcut hint.
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        match &mut self.kind {
            MenuEntryKind::Item(item) => item.shortcut = Some(shortcut.into()),
            MenuEntryKind::Submenu { item, .. } => item.shortcut = Some(shortcut.into()),
            MenuEntryKind::Separator | MenuEntryKind::Section { .. } => {}
        }
        self
    }

    /// Adds a leading icon built with the resolved row foreground color.
    ///
    /// A selected checkbox or radio mark replaces the icon within the shared leading slot.
    pub fn icon(mut self, build: impl Fn(Rgba) -> AnyElement + 'static) -> Self {
        match &mut self.kind {
            MenuEntryKind::Item(item) => item.icon = Some(Rc::new(build)),
            MenuEntryKind::Submenu { item, .. } => item.icon = Some(Rc::new(build)),
            MenuEntryKind::Separator | MenuEntryKind::Section { .. } => {}
        }
        self
    }

    /// Adds a stable selector used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        match &mut self.kind {
            MenuEntryKind::Item(item) => item.debug_selector = Some(selector.into()),
            MenuEntryKind::Submenu { item, .. } => item.debug_selector = Some(selector.into()),
            MenuEntryKind::Separator | MenuEntryKind::Section { .. } => {}
        }
        self
    }

    /// Returns this entry's presentation when it is one ordinary action.
    ///
    /// A caller that would otherwise wrap a lone entry in a disclosure uses this to offer the
    /// action directly instead. Separators, sections, and submenus report nothing.
    pub(crate) fn plain_action(&self) -> Option<PlainMenuAction<'_, A>> {
        match &self.kind {
            MenuEntryKind::Item(item)
                if !item.destructive
                    && item.shortcut.is_none()
                    && item.icon.is_none()
                    && item.mark == EntryMark::None =>
            {
                Some(PlainMenuAction {
                    label: &item.label,
                    action: &item.action,
                    disabled: item.disabled,
                    debug_selector: item.debug_selector.as_deref(),
                })
            }
            MenuEntryKind::Item(_)
            | MenuEntryKind::Separator
            | MenuEntryKind::Section { .. }
            | MenuEntryKind::Submenu { .. } => None,
        }
    }
}

/// One ordinary menu action a caller may present outside a menu.
pub(crate) struct PlainMenuAction<'entry, A> {
    pub(crate) label: &'entry SharedString,
    pub(crate) action: &'entry A,
    pub(crate) disabled: bool,
    pub(crate) debug_selector: Option<&'entry str>,
}

/// A button-triggered action menu.
#[derive(IntoElement)]
pub struct Menu<A: Clone + 'static> {
    core: MenuControl<A>,
    label: SharedString,
    leading_icon: Option<IconBuilder>,
    icon_trigger: bool,
}

impl<A: Clone + 'static> Menu<A> {
    /// Creates a regular menu. The trigger label is its logical accessibility name.
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        entries: Vec<MenuEntry<A>>,
    ) -> Self {
        let label = label.into();
        Self {
            core: MenuControl::new(id.into(), label.clone(), entries, TriggerKind::Menu),
            label,
            leading_icon: None,
            icon_trigger: false,
        }
    }

    /// Adds a leading trigger icon built with the resolved foreground color.
    pub fn leading_icon(mut self, build: impl Fn(Rgba) -> AnyElement + 'static) -> Self {
        self.leading_icon = Some(Rc::new(build));
        self
    }

    /// Uses a compact icon-only trigger while retaining the label as its logical name.
    pub fn icon_trigger(mut self, build: impl Fn(Rgba) -> AnyElement + 'static) -> Self {
        self.leading_icon = Some(Rc::new(build));
        self.icon_trigger = true;
        self.core.icon_trigger = true;
        self
    }

    control_builders!(A, MenuActivation<A>, on_activate);
}

impl<A: Clone + 'static> RenderOnce for Menu<A> {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let style = cx.global::<MenuTheme>().resolve(self.core.size);
        let foreground = style.paint.foreground;
        let content = div()
            .flex()
            .items_center()
            .gap(style.metrics.gap)
            .when_some(self.leading_icon, |content, icon| {
                content.child(icon(foreground))
            })
            .when(!self.icon_trigger, |content| {
                content
                    .child(self.label)
                    .child(div().ml_auto().text_color(style.paint.muted).child("▾"))
            })
            .into_any_element();
        self.core.render(content, window, cx)
    }
}

/// A secondary-click menu attached to arbitrary trigger content.
#[derive(IntoElement)]
pub struct ContextMenu<T: IntoElement + 'static, A: Clone + 'static> {
    core: MenuControl<A>,
    child: T,
}

impl<T: IntoElement + 'static, A: Clone + 'static> ContextMenu<T, A> {
    /// Creates a context menu with a mandatory logical accessibility name for its trigger.
    pub fn new(
        id: impl Into<ElementId>,
        accessibility_name: impl Into<SharedString>,
        child: T,
        entries: Vec<MenuEntry<A>>,
    ) -> Self {
        Self {
            core: MenuControl::new(
                id.into(),
                accessibility_name.into(),
                entries,
                TriggerKind::Context,
            ),
            child,
        }
    }

    control_builders!(A, MenuActivation<A>, on_activate);

    /// Handles a proposed context opening before menu state or focus changes.
    ///
    /// Returning `false` rejects the request and leaves the gesture available to the underlay.
    pub fn on_open_request(
        mut self,
        handler: impl Fn(&ContextMenuOpenRequest, &mut Window, &mut App) -> bool + 'static,
    ) -> Self {
        self.core.on_context_open = Some(Rc::new(handler));
        self
    }

    /// Makes the context-menu decorator fill the width allocated by its parent.
    ///
    /// Use this for children such as editors whose percentage width requires a definite
    /// containing block. Intrinsically sized context-menu targets retain their natural width by
    /// default.
    pub fn fill_parent_width(mut self) -> Self {
        self.core.fill_parent_width = true;
        self
    }
}

impl<T: IntoElement + 'static, A: Clone + 'static> RenderOnce for ContextMenu<T, A> {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.core.render(self.child.into_any_element(), window, cx)
    }
}

/// One typed option in a [`Picker`].
pub struct PickerOption<T> {
    value: T,
    label: SharedString,
    disabled: bool,
    icon: Option<IconBuilder>,
    debug_selector: Option<String>,
}

impl<T> PickerOption<T> {
    /// Creates an enabled option. Its label is also its logical accessibility name.
    pub fn new(value: T, label: impl Into<SharedString>) -> Self {
        Self {
            value,
            label: label.into(),
            disabled: false,
            icon: None,
            debug_selector: None,
        }
    }

    /// Controls whether navigation and activation may reach this option.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Adds a leading icon built with the resolved row foreground color.
    ///
    /// The selected option mark replaces the icon within the shared leading slot.
    pub fn icon(mut self, build: impl Fn(Rgba) -> AnyElement + 'static) -> Self {
        self.icon = Some(Rc::new(build));
        self
    }

    /// Adds a stable selector used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }
}

/// Error returned when a picker cannot represent exactly one current option.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PickerBuildError {
    /// A picker requires at least one option.
    EmptyOptions,
}

impl fmt::Display for PickerBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyOptions => formatter.write_str("a Picker requires at least one option"),
        }
    }
}

impl std::error::Error for PickerBuildError {}

/// A button-triggered single-value picker with typed current value and options.
#[derive(IntoElement)]
pub struct Picker<T: Clone + PartialEq + 'static> {
    core: MenuControl<T>,
    selected_label: SharedString,
    leading_icon: Option<IconBuilder>,
    on_change: Option<PickerChangeHandler<T>>,
}

impl<T: Clone + PartialEq + 'static> Picker<T> {
    /// Creates a picker that represents exactly one option as current.
    ///
    /// The first option matching `current` is selected. If none matches, the first option is the
    /// bounded fallback so malformed caller state never produces a multi-selected picker.
    ///
    /// # Errors
    ///
    /// Returns [`PickerBuildError::EmptyOptions`] when no option is supplied.
    pub fn new(
        id: impl Into<ElementId>,
        accessibility_name: impl Into<SharedString>,
        current: T,
        options: Vec<PickerOption<T>>,
    ) -> Result<Self, PickerBuildError> {
        if options.is_empty() {
            return Err(PickerBuildError::EmptyOptions);
        }
        let selected = options
            .iter()
            .position(|option| option.value == current)
            .or_else(|| (!options.is_empty()).then_some(0));
        let selected_label = selected
            .and_then(|index| options.get(index))
            .map_or_else(SharedString::default, |option| option.label.clone());
        let entries = options
            .into_iter()
            .enumerate()
            .map(|(index, option)| {
                let mut entry = MenuEntry::item(
                    option.label,
                    option.value,
                    EntryMark::Radio {
                        selected: selected == Some(index),
                        index,
                    },
                )
                .disabled(option.disabled);
                if let Some(icon) = option.icon {
                    entry = entry.icon(move |color| icon(color));
                }
                if let Some(selector) = option.debug_selector {
                    entry = entry.debug_selector(selector);
                }
                entry
            })
            .collect();
        Ok(Self {
            core: MenuControl::new(
                id.into(),
                accessibility_name.into(),
                entries,
                TriggerKind::Picker,
            ),
            selected_label,
            leading_icon: None,
            on_change: None,
        })
    }

    /// Adds a leading trigger icon built with the resolved foreground color.
    pub fn leading_icon(mut self, build: impl Fn(Rgba) -> AnyElement + 'static) -> Self {
        self.leading_icon = Some(Rc::new(build));
        self
    }

    /// Handles a typed value change.
    pub fn on_change(
        mut self,
        handler: impl Fn(&PickerChange<T>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }

    lifecycle_builders!();
}

impl<T: Clone + PartialEq + 'static> RenderOnce for Picker<T> {
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let on_change = self.on_change.take();
        self.core.on_activate = on_change.map(|handler| {
            Rc::new(
                move |activation: &MenuActivation<T>, window: &mut Window, cx: &mut App| {
                    handler(
                        &PickerChange {
                            value: activation.action().clone(),
                            source: activation.source(),
                        },
                        window,
                        cx,
                    );
                },
            ) as Rc<dyn Fn(&MenuActivation<T>, &mut Window, &mut App)>
        });
        let style = cx.global::<MenuTheme>().resolve(self.core.size);
        let foreground = style.paint.foreground;
        let content = div()
            .flex()
            .items_center()
            .gap(style.metrics.gap)
            .when_some(self.leading_icon, |content, icon| {
                content.child(icon(foreground))
            })
            .child(div().flex_grow().child(self.selected_label))
            .child(div().text_color(style.paint.muted).child("▾"))
            .into_any_element();
        self.core.render(content, window, cx)
    }
}

macro_rules! lifecycle_builders {
    () => {
        /// Handles exact open and closed lifecycle transitions.
        pub fn on_lifecycle(
            mut self,
            handler: impl Fn(&MenuLifecycleEvent, &mut App) + 'static,
        ) -> Self {
            self.core.on_lifecycle = Some(Rc::new(handler));
            self
        }

        /// Selects a standard bounded menu width.
        pub fn size(mut self, size: MenuSize) -> Self {
            self.core.size = size;
            self
        }

        /// Selects root menu placement.
        pub fn placement(mut self, placement: MenuPlacementConfig) -> Self {
            self.core.placement = placement;
            self
        }

        /// Controls whether the control can open.
        pub fn disabled(mut self, disabled: bool) -> Self {
            self.core.disabled = disabled;
            self
        }

        /// Adds a stable selector used by GPUI interaction tests.
        pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
            self.core.debug_selector = Some(selector.into());
            self
        }
    };
}
use lifecycle_builders;

macro_rules! control_builders {
    ($action:ident, $event:ty, $handler:ident) => {
        /// Handles a typed menu selection.
        pub fn $handler(
            mut self,
            handler: impl Fn(&$event, &mut Window, &mut App) + 'static,
        ) -> Self {
            self.core.on_activate = Some(Rc::new(handler));
            self
        }

        lifecycle_builders!();
    };
}
use control_builders;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TriggerKind {
    Menu,
    Context,
    Picker,
}

type TypedActivationHandler<A> = Rc<dyn Fn(&MenuActivation<A>, &mut Window, &mut App)>;

struct MenuControl<A> {
    id: ElementId,
    accessibility_name: SharedString,
    entries: Vec<MenuEntry<A>>,
    kind: TriggerKind,
    size: MenuSize,
    placement: MenuPlacementConfig,
    disabled: bool,
    icon_trigger: bool,
    fill_parent_width: bool,
    debug_selector: Option<String>,
    on_activate: Option<TypedActivationHandler<A>>,
    on_lifecycle: Option<MenuLifecycleHandler>,
    on_context_open: Option<ContextOpenHandler>,
}

impl<A> MenuControl<A> {
    fn new(
        id: ElementId,
        accessibility_name: SharedString,
        entries: Vec<MenuEntry<A>>,
        kind: TriggerKind,
    ) -> Self {
        Self {
            id,
            accessibility_name,
            entries,
            kind,
            size: MenuSize::default(),
            placement: MenuPlacementConfig::default(),
            disabled: false,
            icon_trigger: false,
            fill_parent_width: false,
            debug_selector: None,
            on_activate: None,
            on_lifecycle: None,
            on_context_open: None,
        }
    }
}

impl<A: Clone + 'static> MenuControl<A> {
    fn render(self, content: AnyElement, window: &mut Window, cx: &mut App) -> AnyElement {
        let style = cx.global::<MenuTheme>().resolve(self.size);
        let handler = self.on_activate;
        let lifecycle = self.on_lifecycle;
        let context_open = self.on_context_open;
        let enabled = !self.disabled && handler.is_some() && has_selectable(&self.entries);
        let entries = convert_entries(self.entries, &move |action: A, mark| {
            let handler = handler.clone();
            Rc::new(move |source, window: &mut Window, cx: &mut App| {
                if let Some(handler) = &handler {
                    let activation = match mark {
                        EntryMark::None => MenuActivation::Action {
                            action: action.clone(),
                            source,
                        },
                        EntryMark::Checkbox(checked) => MenuActivation::Checkbox {
                            action: action.clone(),
                            checked: !checked,
                            source,
                        },
                        EntryMark::Radio { index, .. } => MenuActivation::Radio {
                            action: action.clone(),
                            index,
                            source,
                        },
                    };
                    handler(&activation, window, cx);
                }
            }) as InternalActivation
        });
        let state = window.use_keyed_state(self.id.clone(), cx, MenuState::new);
        let closed_reservation = state.update(cx, |state, cx| {
            let should_close = state.open && !enabled;
            state.synchronize(
                entries,
                style,
                self.placement,
                enabled,
                self.kind != TriggerKind::Context,
                self.kind == TriggerKind::Context,
                lifecycle.clone(),
            );
            should_close
                .then(|| state.dismiss(MenuCloseReason::Disabled, true, Some(window), cx))
                .flatten()
        });
        if let Some(reservation) = closed_reservation {
            release_window(reservation, window.window_handle().window_id(), cx);
        }

        let (open, focus_handle) = {
            let state = state.read(cx);
            (state.open, state.focus_handle.clone())
        };
        let focused = focus_handle.is_focused(window);
        let trigger_bounds_state = state.downgrade();
        let trigger_kind = self.kind;
        let trigger_state = state.downgrade();
        let trigger_tracker = canvas(
            move |bounds, window, _| {
                let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                (bounds, hitbox)
            },
            move |_, (bounds, hitbox), window, cx| {
                let _ =
                    trigger_bounds_state.update(cx, |state, _| state.trigger_bounds = Some(bounds));
                let hitbox = hitbox.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.capture() || !hitbox.is_hovered(window) || !enabled {
                        return;
                    }
                    let context_gesture = event.button == MouseButton::Right
                        || (event.button == MouseButton::Left
                            && event.modifiers.control
                            && !event.modifiers.alt
                            && !event.modifiers.platform);
                    let ordinary_gesture =
                        event.button == MouseButton::Left && !event.modifiers.control;
                    let accepts = match trigger_kind {
                        TriggerKind::Context => context_gesture,
                        TriggerKind::Menu | TriggerKind::Picker => ordinary_gesture,
                    };
                    if !accepts {
                        return;
                    }
                    let requesting_open = !trigger_state
                        .read_with(cx, |state, _| state.open)
                        .unwrap_or(false);
                    if trigger_kind == TriggerKind::Context && requesting_open {
                        let request = ContextMenuOpenRequest {
                            position: event.position,
                        };
                        if context_open
                            .as_ref()
                            .is_some_and(|handler| !handler(&request, window, cx))
                        {
                            return;
                        }
                    }
                    window.prevent_default();
                    toggle_menu(
                        &trigger_state,
                        if trigger_kind == TriggerKind::Context {
                            Some(event.position)
                        } else {
                            None
                        },
                        window,
                        cx,
                    );
                    if requesting_open {
                        let _ = trigger_state.update(cx, |state, _| {
                            state.begin_pointer_tracking(event.button);
                        });
                    }
                    cx.stop_propagation();
                });
            },
        )
        .absolute()
        .inset_0();

        let key_state = state.downgrade();
        let fill_parent_width = self.fill_parent_width;
        let debug_selector = self.debug_selector;
        let accessibility_name = self.accessibility_name;
        let mut trigger = div()
            .id(self.id)
            .debug_selector(move || {
                debug_selector.unwrap_or_else(|| accessibility_name.to_string())
            })
            .relative()
            .cursor_default()
            .when(fill_parent_width, |trigger| trigger.w_full())
            .when(!open && self.kind != TriggerKind::Context, |trigger| {
                trigger.track_focus(&focus_handle)
            })
            .child(content)
            .child(trigger_tracker)
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                if trigger_kind == TriggerKind::Context
                    || !enabled
                    || event.keystroke.modifiers.modified()
                {
                    return;
                }
                if matches!(event.keystroke.key.as_str(), "space" | "enter" | "down") {
                    window.prevent_default();
                    open_menu(&key_state, None, OpenDirection::First, window, cx);
                    cx.stop_propagation();
                }
            });

        if self.kind != TriggerKind::Context {
            let paint = style.paint;
            trigger = trigger
                .flex()
                .items_center()
                .h(style.metrics.trigger_height)
                .when(self.icon_trigger, |trigger| {
                    trigger
                        .w(style.metrics.trigger_height)
                        .justify_center()
                        .px(px(0.0))
                })
                .when(!self.icon_trigger, |trigger| {
                    trigger
                        .min_w(style.metrics.panel_width)
                        .px(style.metrics.horizontal_padding)
                })
                .rounded(style.metrics.corner_radius)
                .border(style.metrics.border_width)
                .border_color(if focused {
                    paint.focus_border
                } else {
                    paint.trigger_border
                })
                .bg(if open {
                    paint.trigger_hover_background
                } else {
                    paint.trigger_background
                })
                .text_color(if enabled {
                    paint.foreground
                } else {
                    paint.disabled
                })
                .text_size(style.metrics.font_size)
                .when(enabled && !open, |trigger| {
                    trigger.hover(move |style| style.bg(paint.trigger_hover_background))
                });
        }

        div()
            .relative()
            .when(fill_parent_width, |root| root.w_full())
            .child(trigger)
            .when(open, |root| root.child(render_overlay(state, window, cx)))
            .into_any_element()
    }
}

fn has_selectable<A>(entries: &[MenuEntry<A>]) -> bool {
    entries.iter().any(|entry| match &entry.kind {
        MenuEntryKind::Item(item) => !item.disabled,
        MenuEntryKind::Submenu { item, entries } => !item.disabled && has_selectable(entries),
        MenuEntryKind::Section { entries, .. } => has_selectable(entries),
        MenuEntryKind::Separator => false,
    })
}

#[derive(Clone)]
struct InternalEntry {
    kind: InternalEntryKind,
}

#[derive(Clone)]
enum InternalEntryKind {
    Item {
        label: SharedString,
        disabled: bool,
        destructive: bool,
        shortcut: Option<SharedString>,
        icon: Option<IconBuilder>,
        mark: EntryMark,
        debug_selector: Option<String>,
        activate: InternalActivation,
    },
    Separator,
    Heading(SharedString),
    Submenu {
        label: SharedString,
        disabled: bool,
        destructive: bool,
        shortcut: Option<SharedString>,
        icon: Option<IconBuilder>,
        debug_selector: Option<String>,
        entries: Vec<InternalEntry>,
    },
}

impl InternalEntry {
    fn selectable(&self) -> bool {
        match &self.kind {
            InternalEntryKind::Item { disabled, .. }
            | InternalEntryKind::Submenu { disabled, .. } => !disabled,
            InternalEntryKind::Separator | InternalEntryKind::Heading(_) => false,
        }
    }

    fn label(&self) -> Option<&str> {
        match &self.kind {
            InternalEntryKind::Item { label, .. } | InternalEntryKind::Submenu { label, .. } => {
                Some(label.as_ref())
            }
            InternalEntryKind::Separator | InternalEntryKind::Heading(_) => None,
        }
    }

    fn submenu(&self) -> Option<&[InternalEntry]> {
        match &self.kind {
            InternalEntryKind::Submenu {
                entries,
                disabled: false,
                ..
            } => Some(entries),
            _ => None,
        }
    }
}

fn convert_entries<A: Clone + 'static>(
    entries: Vec<MenuEntry<A>>,
    make_activation: &impl Fn(A, EntryMark) -> InternalActivation,
) -> Vec<InternalEntry> {
    let mut converted = Vec::new();
    for entry in entries {
        match entry.kind {
            MenuEntryKind::Item(item) => converted.push(InternalEntry {
                kind: InternalEntryKind::Item {
                    label: item.label,
                    disabled: item.disabled,
                    destructive: item.destructive,
                    shortcut: item.shortcut,
                    icon: item.icon,
                    mark: item.mark,
                    debug_selector: item.debug_selector,
                    activate: make_activation(item.action, item.mark),
                },
            }),
            MenuEntryKind::Separator => converted.push(InternalEntry {
                kind: InternalEntryKind::Separator,
            }),
            MenuEntryKind::Section { label, entries } => {
                if let Some(label) = label {
                    converted.push(InternalEntry {
                        kind: InternalEntryKind::Heading(label),
                    });
                }
                converted.extend(convert_entries(entries, make_activation));
            }
            MenuEntryKind::Submenu { item, entries } => {
                let disabled = item.disabled || !has_selectable(&entries);
                converted.push(InternalEntry {
                    kind: InternalEntryKind::Submenu {
                        label: item.label,
                        disabled,
                        destructive: item.destructive,
                        shortcut: item.shortcut,
                        icon: item.icon,
                        debug_selector: item.debug_selector,
                        entries: convert_entries(entries, make_activation),
                    },
                });
            }
        }
    }
    converted
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MenuReservation(u64);

struct MenuOwnership {
    owner: WeakEntity<MenuState>,
    reservation: MenuReservation,
    modal_parent: Option<crate::modal::ModalParentToken>,
}

#[derive(Default)]
struct MenuCoordinator {
    owners: HashMap<WindowId, MenuOwnership>,
    next_reservation: u64,
}
impl Global for MenuCoordinator {}

fn reserve_window(
    owner: &Entity<MenuState>,
    window: &mut Window,
    cx: &mut App,
) -> Option<(MenuReservation, Option<WeakEntity<MenuState>>)> {
    let window_id = window.window_handle().window_id();
    let current_modal = crate::modal::current_modal_parent(window, cx);
    let modal_parent = crate::modal::focused_modal_parent(window, cx);
    if current_modal.is_some() && modal_parent != current_modal {
        return None;
    }
    let weak = owner.downgrade();
    let reservation = cx.update_global::<MenuCoordinator, _>(|coordinator, _| {
        coordinator.next_reservation = coordinator.next_reservation.wrapping_add(1);
        let reservation = MenuReservation(coordinator.next_reservation);
        let previous = coordinator
            .owners
            .insert(
                window_id,
                MenuOwnership {
                    owner: weak.clone(),
                    reservation,
                    modal_parent,
                },
            )
            .map(|ownership| ownership.owner)
            .filter(|previous| previous != &weak);
        (reservation, previous)
    });
    crate::tooltip::set_window_tooltip_suppression(
        window_id,
        crate::tooltip::TooltipSuppression::Menu,
        true,
        cx,
    );
    Some(reservation)
}

fn release_window(reservation: MenuReservation, window_id: WindowId, cx: &mut App) {
    let menu_remains = cx.update_global::<MenuCoordinator, _>(|coordinator, _| {
        if coordinator
            .owners
            .get(&window_id)
            .is_some_and(|ownership| ownership.reservation == reservation)
        {
            coordinator.owners.remove(&window_id);
        }
        coordinator
            .owners
            .retain(|_, ownership| ownership.owner.upgrade().is_some());
        coordinator.owners.contains_key(&window_id)
    });
    if !menu_remains {
        crate::tooltip::set_window_tooltip_suppression(
            window_id,
            crate::tooltip::TooltipSuppression::Menu,
            false,
            cx,
        );
        crate::command_palette::retry_window_command_palette_modal_resume(window_id, cx);
    }
}

struct MenuReplacement {
    lifecycle: Option<MenuLifecycleHandler>,
    restore_focus: Option<WeakFocusHandle>,
}

struct MenuDismissal {
    lifecycle: Option<MenuLifecycleHandler>,
    open_generation: u64,
    reservation: Option<MenuReservation>,
}

impl MenuDismissal {
    fn finish(self, reason: MenuCloseReason, cx: &mut App) {
        if let Some(handler) = self.lifecycle {
            handler(&MenuLifecycleEvent::Closed(reason), cx);
        }
    }
}

pub(crate) struct MenuReplacementFocus(pub(crate) Option<WeakFocusHandle>);

/// Returns whether this Operating-System Window currently owns an open menu.
///
/// A transient owner such as the Command Palette stays open while one of its own menus holds
/// focus. The coordinator reserves the window before the menu takes focus, so this answer is
/// already correct when the displaced owner observes its blur.
pub fn window_menu_is_open(window: &Window, cx: &App) -> bool {
    if !cx.has_global::<MenuCoordinator>() {
        return false;
    }
    let window_id = window.window_handle().window_id();
    cx.global::<MenuCoordinator>()
        .owners
        .get(&window_id)
        .and_then(|ownership| ownership.owner.upgrade())
        .is_some_and(|owner| owner.read(cx).open)
}

pub(crate) fn window_menu_is_owned_by_current_modal(window: &Window, cx: &App) -> bool {
    let Some(modal_parent) = crate::modal::current_modal_parent(window, cx) else {
        return false;
    };
    cx.has_global::<MenuCoordinator>()
        && cx
            .global::<MenuCoordinator>()
            .owners
            .get(&window.window_handle().window_id())
            .is_some_and(|ownership| {
                ownership.modal_parent == Some(modal_parent)
                    && ownership
                        .owner
                        .upgrade()
                        .is_some_and(|owner| owner.read(cx).open)
            })
}

pub(crate) fn dismiss_menu_owned_by_modal_parent(
    modal_parent: crate::modal::ModalParentToken,
    cx: &mut App,
) -> Option<WeakFocusHandle> {
    if !cx.has_global::<MenuCoordinator>() {
        return None;
    }
    let (owner, reservation) = cx
        .global::<MenuCoordinator>()
        .owners
        .get(&modal_parent.window_id)
        .filter(|ownership| ownership.modal_parent == Some(modal_parent))
        .map(|ownership| (ownership.owner.clone(), ownership.reservation))?;
    let retired_focus = owner
        .read_with(cx, |state, _| state.focus_handle.downgrade())
        .ok()?;
    let replacement = owner
        .update(cx, |state, cx| state.replace_without_lifecycle(cx))
        .ok()
        .flatten();
    release_window(reservation, modal_parent.window_id, cx);
    let replacement = replacement?;
    if let Some(handler) = replacement.lifecycle {
        handler(&MenuLifecycleEvent::Closed(MenuCloseReason::Replaced), cx);
    }
    Some(retired_focus)
}

/// Dismisses the menu owned by this Operating-System Window and restores its displaced focus.
///
/// Returns `true` only when an open menu was dismissed. Application-owned transients use this
/// before capturing focus so the menu cannot remain above or restore focus through the new owner.
pub fn dismiss_active_menu(window: &mut Window, cx: &mut App) -> bool {
    let Some(MenuReplacementFocus(restore_focus)) = dismiss_active_menu_for_replacement(window, cx)
    else {
        return false;
    };
    if let Some(focus) = restore_focus.and_then(|focus| focus.upgrade()) {
        focus.focus(window);
    }
    true
}

pub(crate) fn dismiss_active_menu_for_replacement(
    window: &Window,
    cx: &mut App,
) -> Option<MenuReplacementFocus> {
    if !cx.has_global::<MenuCoordinator>() {
        return None;
    }
    let window_id = window.window_handle().window_id();
    let (owner, reservation) = cx
        .global::<MenuCoordinator>()
        .owners
        .get(&window_id)
        .map(|ownership| (ownership.owner.clone(), ownership.reservation))?;
    let replacement = owner
        .update(cx, |state, cx| state.replace_without_lifecycle(cx))
        .ok()
        .flatten();
    release_window(reservation, window_id, cx);
    let replacement = replacement?;
    if let Some(handler) = replacement.lifecycle {
        handler(&MenuLifecycleEvent::Closed(MenuCloseReason::Replaced), cx);
    }
    Some(MenuReplacementFocus(replacement.restore_focus))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OpenDirection {
    First,
}

struct MenuState {
    focus_handle: FocusHandle,
    entries: Vec<InternalEntry>,
    style: MenuStyle,
    placement: MenuPlacementConfig,
    enabled: bool,
    restore_to_trigger: bool,
    freeze_entries_while_open: bool,
    awaiting_context_snapshot: bool,
    open: bool,
    open_generation: u64,
    reservation: Option<MenuReservation>,
    trigger_bounds: Option<Bounds<Pixels>>,
    context_anchor: Option<Point<Pixels>>,
    restore_focus: Option<WeakFocusHandle>,
    active_path: Vec<usize>,
    highlighted: Vec<Option<usize>>,
    typeahead: String,
    last_typeahead: Option<Instant>,
    lifecycle: Option<MenuLifecycleHandler>,
    window_id: Option<WindowId>,
    submenu_generation: u64,
    submenu_task: Option<Task<()>>,
    pointer_button: Option<MouseButton>,
    pointer_press: Option<(usize, usize)>,
}

impl MenuState {
    fn new(window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        cx.on_blur(&focus_handle, window, |state, window, cx| {
            if state.open {
                let window_id = window.window_handle().window_id();
                if let Some(reservation) = state.dismiss(MenuCloseReason::Outside, false, None, cx)
                {
                    release_window(reservation, window_id, cx);
                }
            }
        })
        .detach();
        cx.observe_window_activation(window, |state, window, cx| {
            if state.open && !window.is_window_active() {
                let window_id = window.window_handle().window_id();
                if let Some(reservation) =
                    state.dismiss(MenuCloseReason::Deactivated, false, None, cx)
                {
                    release_window(reservation, window_id, cx);
                }
            }
        })
        .detach();
        cx.on_release(|state, cx| {
            let reservation = state.reservation.take();
            if state.open {
                state.open = false;
                state.emit_lifecycle(
                    MenuLifecycleEvent::Closed(MenuCloseReason::TargetDisappeared),
                    cx,
                );
            }
            if let (Some(window_id), Some(reservation)) = (state.window_id.take(), reservation) {
                release_window(reservation, window_id, cx);
            }
        })
        .detach();
        Self {
            focus_handle,
            entries: Vec::new(),
            style: MenuStyle {
                paint: MenuPaint::new(
                    Rgba::default(),
                    Rgba::default(),
                    Rgba::default(),
                    Rgba::default(),
                    Rgba::default(),
                    Rgba::default(),
                    Rgba::default(),
                    Rgba::default(),
                    Rgba::default(),
                ),
                metrics: MenuMetrics::new(px(0.0), px(0.0)),
            },
            placement: MenuPlacementConfig::default(),
            enabled: false,
            restore_to_trigger: false,
            freeze_entries_while_open: false,
            awaiting_context_snapshot: false,
            open: false,
            open_generation: 0,
            reservation: None,
            trigger_bounds: None,
            context_anchor: None,
            restore_focus: None,
            active_path: Vec::new(),
            highlighted: vec![None],
            typeahead: String::new(),
            last_typeahead: None,
            lifecycle: None,
            window_id: None,
            submenu_generation: 0,
            submenu_task: None,
            pointer_button: None,
            pointer_press: None,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the private synchronization boundary receives one complete control snapshot"
    )]
    fn synchronize(
        &mut self,
        entries: Vec<InternalEntry>,
        style: MenuStyle,
        placement: MenuPlacementConfig,
        enabled: bool,
        trigger_focusable: bool,
        freeze_entries_while_open: bool,
        lifecycle: Option<MenuLifecycleHandler>,
    ) {
        if !self.open || !freeze_entries_while_open || self.awaiting_context_snapshot {
            self.entries = entries;
            self.awaiting_context_snapshot = false;
        }
        self.style = style;
        self.placement = placement;
        self.enabled = enabled;
        self.restore_to_trigger = trigger_focusable;
        self.freeze_entries_while_open = freeze_entries_while_open;
        self.lifecycle = lifecycle;
        self.focus_handle = self
            .focus_handle
            .clone()
            .tab_stop(enabled && trigger_focusable);
        self.repair_path();
    }

    fn repair_path(&mut self) {
        let mut entries = self.entries.as_slice();
        let mut valid = 0;
        for index in &self.active_path {
            let Some(next) = entries.get(*index).and_then(InternalEntry::submenu) else {
                break;
            };
            entries = next;
            valid += 1;
        }
        self.active_path.truncate(valid);
        self.highlighted.truncate(valid + 1);
        while self.highlighted.len() < valid + 1 {
            self.highlighted.push(None);
        }
        for depth in 0..self.highlighted.len() {
            let entries = self.entries_at(depth);
            if self.highlighted[depth]
                .is_some_and(|index| !entries.get(index).is_some_and(InternalEntry::selectable))
            {
                self.highlighted[depth] = first_selectable(entries);
            }
        }
    }

    fn entries_at(&self, depth: usize) -> &[InternalEntry] {
        let mut entries = self.entries.as_slice();
        for index in self.active_path.iter().take(depth) {
            let Some(next) = entries.get(*index).and_then(InternalEntry::submenu) else {
                return &[];
            };
            entries = next;
        }
        entries
    }

    fn open(
        &mut self,
        context_anchor: Option<Point<Pixels>>,
        direction: OpenDirection,
        reservation: Option<MenuReservation>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if self.open || !self.enabled || (context_anchor.is_none() && self.trigger_bounds.is_none())
        {
            return false;
        }
        self.restore_focus = if self.restore_to_trigger {
            Some(self.focus_handle.downgrade())
        } else {
            window.focused(cx).map(|handle| handle.downgrade())
        };
        self.window_id = Some(window.window_handle().window_id());
        self.context_anchor = context_anchor;
        self.open = true;
        self.open_generation = self.open_generation.wrapping_add(1);
        self.reservation = reservation;
        self.awaiting_context_snapshot = self.freeze_entries_while_open;
        self.active_path.clear();
        self.highlighted.clear();
        self.highlighted.push(match direction {
            OpenDirection::First => initial_selectable(&self.entries),
        });
        self.typeahead.clear();
        self.last_typeahead = None;
        self.invalidate_submenu_task();
        self.pointer_button = None;
        self.pointer_press = None;
        self.focus_handle.focus(window);
        self.emit_lifecycle(MenuLifecycleEvent::Opened, cx);
        cx.notify();
        true
    }

    fn dismiss(
        &mut self,
        reason: MenuCloseReason,
        restore: bool,
        window: Option<&mut Window>,
        cx: &mut gpui::Context<Self>,
    ) -> Option<MenuReservation> {
        let dismissal = self.begin_dismiss(restore, window, cx)?;
        let reservation = dismissal.reservation;
        dismissal.finish(reason, cx);
        reservation
    }

    fn begin_dismiss(
        &mut self,
        restore: bool,
        window: Option<&mut Window>,
        cx: &mut gpui::Context<Self>,
    ) -> Option<MenuDismissal> {
        if !self.open {
            return None;
        }
        self.open = false;
        self.awaiting_context_snapshot = false;
        let reservation = self.reservation.take();
        self.window_id = None;
        self.context_anchor = None;
        self.active_path.clear();
        self.highlighted.clear();
        self.highlighted.push(None);
        self.typeahead.clear();
        self.last_typeahead = None;
        self.invalidate_submenu_task();
        self.pointer_button = None;
        self.pointer_press = None;
        if restore {
            if let (Some(window), Some(focus)) = (
                window,
                self.restore_focus.take().and_then(|focus| focus.upgrade()),
            ) {
                focus.focus(window);
            }
        } else {
            self.restore_focus = None;
        }
        cx.notify();
        Some(MenuDismissal {
            lifecycle: self.lifecycle.clone(),
            open_generation: self.open_generation,
            reservation,
        })
    }

    fn replace_without_lifecycle(
        &mut self,
        cx: &mut gpui::Context<Self>,
    ) -> Option<MenuReplacement> {
        if !self.open {
            return None;
        }
        self.open = false;
        self.awaiting_context_snapshot = false;
        self.reservation = None;
        self.window_id = None;
        self.context_anchor = None;
        self.active_path.clear();
        self.highlighted.clear();
        self.highlighted.push(None);
        self.typeahead.clear();
        self.last_typeahead = None;
        self.invalidate_submenu_task();
        self.pointer_button = None;
        self.pointer_press = None;
        let restore_focus = self.restore_focus.take();
        cx.notify();
        Some(MenuReplacement {
            lifecycle: self.lifecycle.clone(),
            restore_focus,
        })
    }

    fn emit_lifecycle(&self, event: MenuLifecycleEvent, cx: &mut App) {
        if let Some(handler) = &self.lifecycle {
            handler(&event, cx);
        }
    }

    fn invalidate_submenu_task(&mut self) {
        self.submenu_generation = self.submenu_generation.wrapping_add(1);
        self.submenu_task.take();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut gpui::Context<Self>) {
        self.invalidate_submenu_task();
        let depth = self.highlighted.len().saturating_sub(1);
        let entries = self.entries_at(depth);
        let next = adjacent_selectable(entries, self.highlighted[depth], delta);
        if self.highlighted[depth] != next {
            self.highlighted[depth] = next;
            self.active_path.truncate(depth);
            cx.notify();
        }
    }

    fn move_edge(&mut self, first: bool, cx: &mut gpui::Context<Self>) {
        self.invalidate_submenu_task();
        let depth = self.highlighted.len().saturating_sub(1);
        let next = if first {
            first_selectable(self.entries_at(depth))
        } else {
            last_selectable(self.entries_at(depth))
        };
        if self.highlighted[depth] != next {
            self.highlighted[depth] = next;
            self.active_path.truncate(depth);
            cx.notify();
        }
    }

    fn open_submenu(&mut self, cx: &mut gpui::Context<Self>) {
        self.invalidate_submenu_task();
        let depth = self.highlighted.len().saturating_sub(1);
        let Some(index) = self.highlighted[depth] else {
            return;
        };
        self.open_submenu_at(depth, index, cx);
    }

    fn open_submenu_at(&mut self, depth: usize, index: usize, cx: &mut gpui::Context<Self>) {
        let first = self
            .entries_at(depth)
            .get(index)
            .and_then(InternalEntry::submenu)
            .and_then(initial_selectable);
        if first.is_none() {
            return;
        }
        self.active_path.truncate(depth);
        self.active_path.push(index);
        self.highlighted.truncate(depth + 1);
        self.highlighted.push(first);
        cx.notify();
    }

    fn close_submenu(&mut self, cx: &mut gpui::Context<Self>) {
        self.invalidate_submenu_task();
        if self.active_path.pop().is_some() {
            self.highlighted.pop();
            cx.notify();
        }
    }

    fn pointer_hover(
        &mut self,
        depth: usize,
        index: usize,
        hovered: bool,
        cx: &mut gpui::Context<Self>,
    ) {
        if !hovered {
            if depth > 0 {
                self.schedule_submenu_close(depth - 1, cx);
            } else if self.active_path.get(depth) == Some(&index) {
                self.schedule_submenu_close(depth, cx);
            }
            return;
        }
        if !self
            .entries_at(depth)
            .get(index)
            .is_some_and(InternalEntry::selectable)
        {
            return;
        }
        self.invalidate_submenu_task();
        self.highlighted.truncate(depth + 1);
        while self.highlighted.len() <= depth {
            self.highlighted.push(None);
        }
        self.highlighted[depth] = Some(index);
        let is_submenu = self
            .entries_at(depth)
            .get(index)
            .and_then(InternalEntry::submenu)
            .is_some();
        if is_submenu {
            if self.active_path.get(depth) != Some(&index) {
                self.schedule_submenu_open(depth, index, cx);
            }
        } else if self.active_path.len() > depth {
            self.schedule_submenu_close(depth, cx);
        }
        cx.notify();
    }

    fn schedule_submenu_open(&mut self, depth: usize, index: usize, cx: &mut gpui::Context<Self>) {
        self.invalidate_submenu_task();
        let generation = self.submenu_generation;
        self.submenu_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SUBMENU_OPEN_DELAY).await;
            let _ = this.update(cx, |state, cx| {
                if state.open && state.submenu_generation == generation {
                    state.open_submenu_at(depth, index, cx);
                }
            });
        }));
    }

    fn schedule_submenu_close(&mut self, depth: usize, cx: &mut gpui::Context<Self>) {
        self.invalidate_submenu_task();
        let generation = self.submenu_generation;
        self.submenu_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SUBMENU_CLOSE_GRACE).await;
            let _ = this.update(cx, |state, cx| {
                if state.open && state.submenu_generation == generation {
                    state.active_path.truncate(depth);
                    state.highlighted.truncate(depth + 1);
                    cx.notify();
                }
            });
        }));
    }

    fn begin_pointer_tracking(&mut self, button: MouseButton) {
        self.pointer_button = Some(button);
        self.pointer_press = None;
    }

    fn pointer_down(&mut self, button: MouseButton, depth: usize, index: usize) {
        self.begin_pointer_tracking(button);
        self.pointer_press = self
            .entries_at(depth)
            .get(index)
            .is_some_and(InternalEntry::selectable)
            .then_some((depth, index));
    }

    fn pointer_drag_over(
        &mut self,
        pressed_button: Option<MouseButton>,
        depth: usize,
        index: usize,
        hovered: bool,
    ) {
        if pressed_button != self.pointer_button {
            self.pointer_button = None;
            self.pointer_press = None;
        } else if hovered {
            self.pointer_press = self
                .entries_at(depth)
                .get(index)
                .is_some_and(InternalEntry::selectable)
                .then_some((depth, index));
        }
    }

    fn pointer_up(&mut self, button: MouseButton, depth: usize, index: usize) -> bool {
        let matched =
            self.pointer_button == Some(button) && self.pointer_press == Some((depth, index));
        self.pointer_button = None;
        self.pointer_press = None;
        matched
    }

    fn current_activation(&self) -> Option<InternalActivation> {
        let depth = self.highlighted.len().checked_sub(1)?;
        let index = self.highlighted[depth]?;
        self.activation_at(depth, index)
    }

    fn activation_at(&self, depth: usize, index: usize) -> Option<InternalActivation> {
        match &self.entries_at(depth).get(index)?.kind {
            InternalEntryKind::Item {
                disabled: false,
                activate,
                ..
            } => Some(activate.clone()),
            _ => None,
        }
    }

    fn typeahead(&mut self, text: &str, now: Instant, cx: &mut gpui::Context<Self>) {
        let Some((query, cycling)) =
            update_typeahead(&mut self.typeahead, &mut self.last_typeahead, text, now)
        else {
            return;
        };
        self.invalidate_submenu_task();
        let depth = self.highlighted.len().saturating_sub(1);
        let entries = self.entries_at(depth);
        if let Some(index) = find_typeahead_match(entries, self.highlighted[depth], &query, cycling)
        {
            self.highlighted[depth] = Some(index);
            self.active_path.truncate(depth);
            cx.notify();
        }
    }
}

fn initial_selectable(entries: &[InternalEntry]) -> Option<usize> {
    entries
        .iter()
        .position(|entry| {
            matches!(
                &entry.kind,
                InternalEntryKind::Item {
                    disabled: false,
                    mark: EntryMark::Radio { selected: true, .. },
                    ..
                }
            )
        })
        .or_else(|| first_selectable(entries))
}

fn first_selectable(entries: &[InternalEntry]) -> Option<usize> {
    entries.iter().position(InternalEntry::selectable)
}
fn last_selectable(entries: &[InternalEntry]) -> Option<usize> {
    entries.iter().rposition(InternalEntry::selectable)
}
fn adjacent_selectable(
    entries: &[InternalEntry],
    current: Option<usize>,
    delta: isize,
) -> Option<usize> {
    let selectable: Vec<_> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| entry.selectable().then_some(i))
        .collect();
    if selectable.is_empty() {
        return None;
    }
    let position =
        current.and_then(|current| selectable.iter().position(|index| *index == current));
    if delta >= 0 {
        Some(selectable[position.map_or(0, |position| (position + 1) % selectable.len())])
    } else {
        Some(
            selectable[position.map_or(selectable.len() - 1, |position| {
                position.checked_sub(1).unwrap_or(selectable.len() - 1)
            })],
        )
    }
}

fn update_typeahead(
    buffer: &mut String,
    last_input: &mut Option<Instant>,
    text: &str,
    now: Instant,
) -> Option<(String, bool)> {
    if text.chars().count() != 1 || text.chars().any(char::is_control) {
        return None;
    }
    if last_input.is_none_or(|last| now.saturating_duration_since(last) > TYPEAHEAD_RESET) {
        buffer.clear();
    }
    *last_input = Some(now);
    let character: String = text.chars().flat_map(char::to_lowercase).collect();
    let repeated = !buffer.is_empty()
        && buffer
            .chars()
            .all(|existing| character.starts_with(existing));
    if repeated {
        buffer.clear();
        buffer.push_str(&character);
        Some((buffer.clone(), true))
    } else {
        buffer.push_str(&character);
        Some((buffer.clone(), false))
    }
}

fn find_typeahead_match(
    entries: &[InternalEntry],
    current: Option<usize>,
    query: &str,
    cycling: bool,
) -> Option<usize> {
    if entries.is_empty() {
        return None;
    }
    let start = if cycling {
        current.map_or(0, |index| index.saturating_add(1))
    } else {
        current.unwrap_or(0)
    };
    (0..entries.len())
        .map(|offset| (start + offset) % entries.len())
        .find(|index| {
            entries.get(*index).is_some_and(|entry| {
                entry.selectable()
                    && entry
                        .label()
                        .is_some_and(|label| label.to_lowercase().starts_with(query))
            })
        })
}

fn toggle_menu(
    state: &WeakEntity<MenuState>,
    anchor: Option<Point<Pixels>>,
    window: &mut Window,
    cx: &mut App,
) {
    let is_open = state.read_with(cx, |state, _| state.open).unwrap_or(false);
    if is_open {
        dismiss_menu(state, MenuCloseReason::Trigger, true, window, cx);
    } else {
        open_menu(state, anchor, OpenDirection::First, window, cx);
    }
}

fn open_menu(
    state: &WeakEntity<MenuState>,
    anchor: Option<Point<Pixels>>,
    direction: OpenDirection,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(entity) = state.upgrade() else {
        return;
    };
    let can_open = !entity.read(cx).open
        && entity.read(cx).enabled
        && (anchor.is_some() || entity.read(cx).trigger_bounds.is_some());
    if !can_open {
        return;
    }
    let Some((reservation, previous)) = reserve_window(&entity, window, cx) else {
        return;
    };
    let replacement = previous.and_then(|previous| {
        previous
            .update(cx, |state, cx| state.replace_without_lifecycle(cx))
            .ok()
            .flatten()
    });
    let opened = entity.update(cx, |state, cx| {
        let opened = state.open(anchor, direction, Some(reservation), window, cx);
        if opened && let Some(replacement) = &replacement {
            state.restore_focus = replacement.restore_focus.clone();
        }
        opened
    });
    if opened {
        if let Some(handler) = replacement.and_then(|replacement| replacement.lifecycle) {
            handler(&MenuLifecycleEvent::Closed(MenuCloseReason::Replaced), cx);
        }
    } else {
        release_window(reservation, window.window_handle().window_id(), cx);
    }
}

fn dismiss_menu(
    state: &WeakEntity<MenuState>,
    reason: MenuCloseReason,
    restore: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let window_id = window.window_handle().window_id();
    let reservation = state
        .update(cx, |menu, cx| {
            menu.dismiss(reason, restore, Some(window), cx)
        })
        .ok()
        .flatten();
    if let Some(reservation) = reservation {
        release_window(reservation, window_id, cx);
    }
}

fn activate_menu(
    state: &WeakEntity<MenuState>,
    activation: &InternalActivation,
    source: MenuActivationSource,
    window: &mut Window,
    cx: &mut App,
) {
    let window_id = window.window_handle().window_id();
    let dismissal = state
        .update(cx, |menu, cx| menu.begin_dismiss(true, Some(window), cx))
        .ok()
        .flatten();
    let Some(dismissal) = dismissal else {
        return;
    };
    activation(source, window, cx);
    if let Some(reservation) = dismissal.reservation {
        release_window(reservation, window_id, cx);
    }
    let superseded = state
        .read_with(cx, |menu, _| {
            menu.open_generation != dismissal.open_generation
        })
        .unwrap_or(false);
    if !superseded {
        dismissal.finish(MenuCloseReason::Activated, cx);
    }
}

fn render_overlay(state: Entity<MenuState>, window: &mut Window, cx: &mut App) -> AnyElement {
    let menu = state.read(cx);
    let viewport = window.viewport_size();
    let anchor = menu.context_anchor.map_or_else(
        || {
            menu.trigger_bounds
                .unwrap_or_else(|| Bounds::new(Point::default(), size(px(0.0), px(0.0))))
        },
        |point| Bounds::new(point, size(px(0.0), px(0.0))),
    );
    let root_size = constrain_panel_size(
        panel_size(&menu.entries, menu.style.metrics),
        viewport,
        menu.placement.viewport_margin,
    );
    let root_bounds = place_root(anchor, root_size, viewport, menu.placement);
    let mut panels = vec![(
        0usize,
        root_bounds,
        menu.entries.clone(),
        menu.highlighted.first().copied().flatten(),
    )];
    let mut parent_bounds = root_bounds;
    let mut parent_entries = menu.entries.as_slice();
    for (depth, index) in menu.active_path.iter().copied().enumerate() {
        let Some(entry) = parent_entries.get(index) else {
            break;
        };
        let Some(children) = entry.submenu() else {
            break;
        };
        let row_top = parent_bounds.top()
            + menu.style.metrics.panel_padding
            + entry_offset(parent_entries, index, menu.style.metrics);
        let child_size = constrain_panel_size(
            panel_size(children, menu.style.metrics),
            viewport,
            menu.placement.viewport_margin,
        );
        let child_bounds = place_submenu(
            parent_bounds,
            row_top,
            child_size,
            viewport,
            menu.placement.viewport_margin,
            menu.style.metrics.submenu_gap,
        );
        panels.push((
            depth + 1,
            child_bounds,
            children.to_vec(),
            menu.highlighted.get(depth + 1).copied().flatten(),
        ));
        parent_bounds = child_bounds;
        parent_entries = children;
    }
    let chain_bounds: Vec<_> = panels.iter().map(|(_, bounds, _, _)| *bounds).collect();
    let trigger_bounds = menu.trigger_bounds;
    let style = menu.style;

    let outside_state = state.downgrade();
    let outside_tracker = canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if !phase.capture() {
                    return;
                }
                let inside_panel = chain_bounds
                    .iter()
                    .any(|bounds| bounds.contains(&event.position));
                let inside_trigger =
                    trigger_bounds.is_some_and(|bounds| bounds.contains(&event.position));
                if !inside_panel && !inside_trigger {
                    window.prevent_default();
                    dismiss_menu(&outside_state, MenuCloseReason::Outside, true, window, cx);
                    cx.stop_propagation();
                }
            });
        },
    )
    .absolute()
    .inset_0();

    let mut overlay = div()
        .relative()
        .w(viewport.width)
        .h(viewport.height)
        .key_context(KEY_CONTEXT)
        .track_focus(&state.read(cx).focus_handle)
        .child(outside_tracker);
    for (depth, bounds, entries, highlighted) in panels {
        overlay = overlay.child(render_panel(
            state.downgrade(),
            depth,
            bounds,
            entries,
            highlighted,
            style,
        ));
    }

    let key_state = state.downgrade();
    let up_state = key_state.clone();
    let down_state = key_state.clone();
    let home_state = key_state.clone();
    let end_state = key_state.clone();
    let left_state = key_state.clone();
    let right_state = key_state.clone();
    let activate_state = key_state.clone();
    let dismiss_state = key_state.clone();
    overlay = overlay
        .on_action(move |_: &MoveUp, _, cx| {
            let _ = up_state.update(cx, |state, cx| state.move_selection(-1, cx));
            cx.stop_propagation();
        })
        .on_action(move |_: &MoveDown, _, cx| {
            let _ = down_state.update(cx, |state, cx| state.move_selection(1, cx));
            cx.stop_propagation();
        })
        .on_action(move |_: &MoveHome, _, cx| {
            let _ = home_state.update(cx, |state, cx| state.move_edge(true, cx));
            cx.stop_propagation();
        })
        .on_action(move |_: &MoveEnd, _, cx| {
            let _ = end_state.update(cx, |state, cx| state.move_edge(false, cx));
            cx.stop_propagation();
        })
        .on_action(move |_: &MoveLeft, _, cx| {
            let _ = left_state.update(cx, |state, cx| state.close_submenu(cx));
            cx.stop_propagation();
        })
        .on_action(move |_: &MoveRight, _, cx| {
            let _ = right_state.update(cx, |state, cx| state.open_submenu(cx));
            cx.stop_propagation();
        })
        .on_action(move |_: &Activate, window, cx| {
            let activation = activate_state
                .read_with(cx, |state, _| state.current_activation())
                .ok()
                .flatten();
            if let Some(activation) = activation {
                activate_menu(
                    &activate_state,
                    &activation,
                    MenuActivationSource::Keyboard,
                    window,
                    cx,
                );
            } else {
                let _ = activate_state.update(cx, |state, cx| state.open_submenu(cx));
            }
            cx.stop_propagation();
        })
        .on_action(move |_: &Dismiss, window, cx| {
            dismiss_menu(&dismiss_state, MenuCloseReason::Escape, true, window, cx);
            cx.stop_propagation();
        })
        .on_key_down(move |event: &KeyDownEvent, _, cx| {
            if event.keystroke.modifiers.control
                || event.keystroke.modifiers.alt
                || event.keystroke.modifiers.platform
                || event.keystroke.modifiers.function
            {
                return;
            }
            if let Some(text) = event.keystroke.key_char.as_deref().or_else(|| {
                (event.keystroke.key.chars().count() == 1).then_some(event.keystroke.key.as_str())
            }) {
                let _ = key_state.update(cx, |state, cx| state.typeahead(text, Instant::now(), cx));
                cx.stop_propagation();
            }
        });

    deferred(
        anchored()
            .anchor(Corner::TopLeft)
            .position(point(px(0.0), px(0.0)))
            .snap_to_window()
            .child(overlay),
    )
    .with_priority(OVERLAY_PRIORITY)
    .into_any_element()
}

fn render_panel(
    state: WeakEntity<MenuState>,
    depth: usize,
    bounds: Bounds<Pixels>,
    entries: Vec<InternalEntry>,
    highlighted: Option<usize>,
    style: MenuStyle,
) -> AnyElement {
    let panel_selector: SharedString = format!("menu-panel-{depth}").into();
    let panel_debug_selector = panel_selector.clone();
    let panel = div()
        .id(panel_selector)
        .debug_selector(move || panel_debug_selector.to_string())
        .absolute()
        .left(bounds.left())
        .top(bounds.top())
        .w(bounds.size.width)
        .h(bounds.size.height)
        .overflow_hidden()
        .rounded(style.metrics.corner_radius)
        .shadow_md()
        .border(style.metrics.border_width)
        .border_color(style.paint.border)
        .bg(style.paint.background)
        .text_size(style.metrics.font_size)
        .block_mouse_except_scroll()
        .cursor_default();
    let mut content = div()
        .id(("menu-panel-scroll", depth))
        .absolute()
        .top(style.metrics.panel_padding)
        .bottom(style.metrics.panel_padding)
        .left_0()
        .right_0()
        .overflow_y_scroll();

    for (index, entry) in entries.into_iter().enumerate() {
        match entry.kind {
            InternalEntryKind::Separator => {
                content = content.child(
                    div()
                        .h(style.metrics.separator_height)
                        .mx(style.metrics.panel_padding)
                        .flex()
                        .items_center()
                        .child(div().h(px(1.0)).w_full().bg(style.paint.separator)),
                );
            }
            InternalEntryKind::Heading(label) => {
                content = content.child(
                    div()
                        .h(style.metrics.section_height)
                        .pl(content_leading_inset(style.metrics))
                        .pr(style.metrics.horizontal_padding + style.metrics.panel_padding)
                        .flex()
                        .items_center()
                        .text_color(style.paint.muted)
                        .text_size(style.metrics.shortcut_font_size)
                        .child(label),
                );
            }
            InternalEntryKind::Item {
                label,
                disabled,
                destructive,
                shortcut,
                icon,
                mark,
                debug_selector,
                activate,
            } => {
                content = content.child(render_row(
                    state.clone(),
                    depth,
                    index,
                    label,
                    disabled,
                    destructive,
                    shortcut,
                    icon,
                    mark,
                    debug_selector,
                    false,
                    Some(activate),
                    highlighted == Some(index),
                    style,
                ));
            }
            InternalEntryKind::Submenu {
                label,
                disabled,
                destructive,
                shortcut,
                icon,
                debug_selector,
                ..
            } => {
                content = content.child(render_row(
                    state.clone(),
                    depth,
                    index,
                    label,
                    disabled,
                    destructive,
                    shortcut,
                    icon,
                    EntryMark::None,
                    debug_selector,
                    true,
                    None,
                    highlighted == Some(index),
                    style,
                ));
            }
        }
    }
    panel.child(content).into_any_element()
}

#[expect(
    clippy::too_many_arguments,
    reason = "the private renderer receives one normalized semantic row snapshot"
)]
fn render_row(
    state: WeakEntity<MenuState>,
    depth: usize,
    index: usize,
    label: SharedString,
    disabled: bool,
    destructive: bool,
    shortcut: Option<SharedString>,
    icon: Option<IconBuilder>,
    mark: EntryMark,
    debug_selector: Option<String>,
    submenu: bool,
    activation: Option<InternalActivation>,
    highlighted: bool,
    style: MenuStyle,
) -> AnyElement {
    let foreground = row_foreground(style.paint, disabled, destructive, highlighted);
    let secondary_foreground =
        row_secondary_foreground(style.paint, disabled, destructive, highlighted);
    let hover_state = state.clone();
    let pointer_state = state;
    let logical_name = label.clone();
    let mut row = div()
        .id(index)
        .debug_selector(move || debug_selector.unwrap_or_else(|| logical_name.to_string()))
        .relative()
        .h(style.metrics.row_height)
        .mx(style.metrics.panel_padding)
        .px(style.metrics.horizontal_padding)
        .flex()
        .items_center()
        .gap(style.metrics.gap)
        .rounded(row_corner_radius(style.metrics))
        .text_color(foreground)
        .cursor_default()
        .when(highlighted, |row| row.bg(style.paint.selected_background))
        .when(!disabled, |row| {
            row.on_hover(move |hovered, _, cx| {
                let _ = hover_state.update(cx, |state, cx| {
                    state.pointer_hover(depth, index, *hovered, cx)
                });
            })
        });
    let mut leading = div()
        .w(style.metrics.indicator_width)
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center();
    if let Some(indicator) = mark_indicator(mark) {
        leading = leading.child(indicator);
    } else if let Some(icon) = icon {
        leading = leading.child(icon(foreground));
    }
    row = row
        .child(leading)
        .child(div().min_w_0().flex_grow().truncate().child(label))
        .when_some(shortcut, |row, shortcut| {
            row.child(
                div()
                    .text_size(style.metrics.shortcut_font_size)
                    .text_color(secondary_foreground)
                    .child(shortcut),
            )
        })
        .when(submenu, |row| {
            row.child(div().text_color(secondary_foreground).child("›"))
        });
    if !disabled {
        let down_state = pointer_state.clone();
        let move_state = pointer_state.clone();
        let up_state = pointer_state;
        let pointer_tracker = canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_, hitbox, window, _| {
                let down_hitbox = hitbox.clone();
                let move_hitbox = hitbox.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.capture()
                        || event.button != MouseButton::Left
                        || !down_hitbox.is_hovered(window)
                    {
                        return;
                    }
                    window.prevent_default();
                    let _ = down_state.update(cx, |state, _| {
                        state.pointer_down(event.button, depth, index)
                    });
                    cx.stop_propagation();
                });
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase.capture() {
                        let hovered = move_hitbox.is_hovered(window);
                        let _ = move_state.update(cx, |state, _| {
                            state.pointer_drag_over(event.pressed_button, depth, index, hovered);
                        });
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if !phase.capture() || !hitbox.is_hovered(window) {
                        return;
                    }
                    let matched = up_state
                        .update(cx, |state, _| state.pointer_up(event.button, depth, index))
                        .unwrap_or(false);
                    if !matched {
                        return;
                    }
                    window.prevent_default();
                    if let Some(activation) = &activation {
                        activate_menu(
                            &up_state,
                            activation,
                            MenuActivationSource::Pointer,
                            window,
                            cx,
                        );
                    } else if submenu {
                        let _ = up_state.update(cx, |state, cx| {
                            state.invalidate_submenu_task();
                            state.open_submenu_at(depth, index, cx);
                        });
                    }
                    cx.stop_propagation();
                });
            },
        )
        .absolute()
        .inset_0();
        row = row.child(pointer_tracker);
    }
    row.into_any_element()
}

fn mark_indicator(mark: EntryMark) -> Option<&'static str> {
    match mark {
        EntryMark::None => None,
        EntryMark::Checkbox(true) => Some("✓"),
        EntryMark::Checkbox(false) => None,
        EntryMark::Radio { selected: true, .. } => Some("●"),
        EntryMark::Radio {
            selected: false, ..
        } => None,
    }
}

fn row_foreground(paint: MenuPaint, disabled: bool, destructive: bool, highlighted: bool) -> Rgba {
    if disabled {
        paint.disabled
    } else if destructive {
        paint.destructive
    } else if highlighted {
        paint.selected_foreground
    } else {
        paint.foreground
    }
}

fn row_secondary_foreground(
    paint: MenuPaint,
    disabled: bool,
    destructive: bool,
    highlighted: bool,
) -> Rgba {
    if disabled {
        paint.disabled
    } else if destructive {
        paint.destructive
    } else if highlighted {
        paint.selected_foreground
    } else {
        paint.muted
    }
}

fn row_corner_radius(metrics: MenuMetrics) -> Pixels {
    // Keep the inset highlight concentric with the outer panel instead of reusing the outer radius.
    (metrics.corner_radius - metrics.panel_padding).max(px(0.0))
}

fn content_leading_inset(metrics: MenuMetrics) -> Pixels {
    metrics.panel_padding + metrics.horizontal_padding + metrics.indicator_width + metrics.gap
}

fn panel_size(entries: &[InternalEntry], metrics: MenuMetrics) -> gpui::Size<Pixels> {
    let content_height = entries.iter().fold(px(0.0), |height, entry| {
        height
            + match entry.kind {
                InternalEntryKind::Separator => metrics.separator_height,
                InternalEntryKind::Heading(_) => metrics.section_height,
                InternalEntryKind::Item { .. } | InternalEntryKind::Submenu { .. } => {
                    metrics.row_height
                }
            }
    });
    size(
        metrics.panel_width,
        content_height + (metrics.panel_padding + metrics.border_width) * 2.0,
    )
}

fn constrain_panel_size(
    panel: gpui::Size<Pixels>,
    viewport: gpui::Size<Pixels>,
    margin: Pixels,
) -> gpui::Size<Pixels> {
    let available_width = (viewport.width - margin * 2.0).max(px(1.0));
    let available_height = (viewport.height - margin * 2.0).max(px(1.0));
    size(
        panel.width.min(available_width),
        panel.height.min(available_height),
    )
}

fn entry_offset(entries: &[InternalEntry], index: usize, metrics: MenuMetrics) -> Pixels {
    entries.iter().take(index).fold(px(0.0), |height, entry| {
        height
            + match entry.kind {
                InternalEntryKind::Separator => metrics.separator_height,
                InternalEntryKind::Heading(_) => metrics.section_height,
                InternalEntryKind::Item { .. } | InternalEntryKind::Submenu { .. } => {
                    metrics.row_height
                }
            }
    })
}

fn place_root(
    anchor: Bounds<Pixels>,
    panel: gpui::Size<Pixels>,
    viewport: gpui::Size<Pixels>,
    config: MenuPlacementConfig,
) -> Bounds<Pixels> {
    let margin = config.viewport_margin;
    let max_x = viewport.width - margin;
    let max_y = viewport.height - margin;
    let clamp_x = |x: Pixels| x.max(margin).min((max_x - panel.width).max(margin));
    let clamp_y = |y: Pixels| y.max(margin).min((max_y - panel.height).max(margin));
    let aligned_x = match config.alignment {
        MenuAlignment::Start => anchor.left(),
        MenuAlignment::Center => anchor.center().x - panel.width / 2.0,
        MenuAlignment::End => anchor.right() - panel.width,
    };
    let aligned_y = match config.alignment {
        MenuAlignment::Start => anchor.top(),
        MenuAlignment::Center => anchor.center().y - panel.height / 2.0,
        MenuAlignment::End => anchor.bottom() - panel.height,
    };
    let (preferred, alternate, vertical) = match config.placement {
        MenuPlacement::Bottom => (
            anchor.bottom() + config.offset,
            anchor.top() - config.offset - panel.height,
            true,
        ),
        MenuPlacement::Top => (
            anchor.top() - config.offset - panel.height,
            anchor.bottom() + config.offset,
            true,
        ),
        MenuPlacement::Left => (
            anchor.left() - config.offset - panel.width,
            anchor.right() + config.offset,
            false,
        ),
        MenuPlacement::Right => (
            anchor.right() + config.offset,
            anchor.left() - config.offset - panel.width,
            false,
        ),
    };
    if vertical {
        let fits = |y: Pixels| y >= margin && y + panel.height <= max_y;
        let y = if fits(preferred) {
            preferred
        } else if fits(alternate) {
            alternate
        } else {
            clamp_y(preferred)
        };
        Bounds::new(point(clamp_x(aligned_x), y), panel)
    } else {
        let fits = |x: Pixels| x >= margin && x + panel.width <= max_x;
        let x = if fits(preferred) {
            preferred
        } else if fits(alternate) {
            alternate
        } else {
            clamp_x(preferred)
        };
        Bounds::new(point(x, clamp_y(aligned_y)), panel)
    }
}

fn place_submenu(
    parent: Bounds<Pixels>,
    row_top: Pixels,
    panel: gpui::Size<Pixels>,
    viewport: gpui::Size<Pixels>,
    margin: Pixels,
    gap: Pixels,
) -> Bounds<Pixels> {
    let right = parent.right() + gap;
    let left = parent.left() - gap - panel.width;
    let limit_right = viewport.width - margin;
    let x = if right + panel.width <= limit_right {
        right
    } else if left >= margin {
        left
    } else {
        right
            .max(margin)
            .min((limit_right - panel.width).max(margin))
    };
    let limit_bottom = viewport.height - margin;
    let y = row_top
        .max(margin)
        .min((limit_bottom - panel.height).max(margin));
    Bounds::new(point(x, y), panel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        AppContext as _, Context, Entity, Modifiers, Render, TestAppContext, VisualTestContext,
        rgba,
    };
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    fn test_theme() -> MenuTheme {
        let paint = MenuPaint::new(
            rgba(0x202020ff),
            rgba(0x404040ff),
            rgba(0xffffffff),
            rgba(0xaaaaaaff),
            rgba(0x777777ff),
            rgba(0x336699ff),
            rgba(0xffffffff),
            rgba(0xff5555ff),
            rgba(0x555555ff),
        );
        let metrics = MenuMetrics::new(px(160.0), px(28.0));
        MenuTheme::new(paint, MenuSizes::new(metrics, metrics, metrics))
    }

    fn inert(label: &str, disabled: bool) -> InternalEntry {
        InternalEntry {
            kind: InternalEntryKind::Item {
                label: label.to_owned().into(),
                disabled,
                destructive: false,
                shortcut: None,
                icon: None,
                mark: EntryMark::None,
                debug_selector: None,
                activate: Rc::new(|_, _, _| {}),
            },
        }
    }

    #[test]
    fn navigation_should_wrap_and_skip_nonselectable_entries() {
        let entries = vec![
            inert("Disabled", true),
            InternalEntry {
                kind: InternalEntryKind::Separator,
            },
            inert("One", false),
            inert("Two", false),
        ];
        assert_eq!(first_selectable(&entries), Some(2));
        assert_eq!(adjacent_selectable(&entries, Some(3), 1), Some(2));
        assert_eq!(adjacent_selectable(&entries, Some(2), -1), Some(3));
    }

    #[test]
    fn default_placement_margin_should_preserve_the_panel_shadow() {
        assert_eq!(MenuPlacementConfig::default().viewport_margin, px(12.0));
    }

    #[test]
    fn root_placement_should_flip_above_when_below_overflows() {
        let anchor = Bounds::new(point(px(20.0), px(170.0)), size(px(40.0), px(20.0)));
        let placed = place_root(
            anchor,
            size(px(100.0), px(80.0)),
            size(px(240.0), px(200.0)),
            MenuPlacementConfig::default(),
        );
        assert_eq!(placed.origin, point(px(20.0), px(86.0)));
    }

    #[test]
    fn oversized_panel_should_be_constrained_inside_the_viewport_margin() {
        let constrained = constrain_panel_size(
            size(px(300.0), px(400.0)),
            size(px(200.0), px(100.0)),
            px(8.0),
        );

        assert_eq!(constrained, size(px(184.0), px(84.0)));
    }

    #[test]
    fn typeahead_should_cycle_repeated_characters_and_extend_mixed_prefixes() {
        let start = Instant::now();
        let mut buffer = String::new();
        let mut last = None;

        assert_eq!(
            update_typeahead(&mut buffer, &mut last, "a", start),
            Some(("a".to_owned(), false))
        );
        assert_eq!(
            update_typeahead(
                &mut buffer,
                &mut last,
                "a",
                start + Duration::from_millis(10)
            ),
            Some(("a".to_owned(), true))
        );
        assert_eq!(
            update_typeahead(
                &mut buffer,
                &mut last,
                "l",
                start + Duration::from_millis(20)
            ),
            Some(("al".to_owned(), false))
        );
        assert_eq!(
            update_typeahead(
                &mut buffer,
                &mut last,
                "b",
                start + TYPEAHEAD_RESET + Duration::from_millis(21),
            ),
            Some(("b".to_owned(), false))
        );
    }

    #[test]
    fn typeahead_match_should_wrap_and_skip_disabled_entries() {
        let entries = vec![
            inert("Alpha", true),
            inert("Alpine", false),
            inert("Beta", false),
            inert("Atlas", false),
        ];

        assert_eq!(find_typeahead_match(&entries, Some(3), "a", true), Some(1));
        assert_eq!(
            find_typeahead_match(&entries, Some(1), "at", false),
            Some(3)
        );
    }

    #[test]
    fn radio_group_should_normalize_nonempty_selection_to_exactly_one() {
        let group = MenuEntry::radio_group(
            Some(99),
            vec![
                MenuRadioOption::new("One", 1),
                MenuRadioOption::new("Two", 2),
            ],
        );
        let MenuEntryKind::Section { entries, .. } = group.kind else {
            panic!("radio group did not create a section");
        };
        let selected = entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.kind,
                    MenuEntryKind::Item(MenuItem {
                        mark: EntryMark::Radio { selected: true, .. },
                        ..
                    })
                )
            })
            .count();

        assert_eq!(selected, 1);
    }

    #[test]
    fn root_placement_should_support_horizontal_sides_and_center_alignment() {
        let anchor = Bounds::new(point(px(100.0), px(80.0)), size(px(40.0), px(40.0)));
        let viewport = size(px(400.0), px(260.0));
        let panel = size(px(80.0), px(60.0));
        let right = place_root(
            anchor,
            panel,
            viewport,
            MenuPlacementConfig::new(MenuPlacement::Right, MenuAlignment::Center),
        );
        let left = place_root(
            anchor,
            panel,
            viewport,
            MenuPlacementConfig::new(MenuPlacement::Left, MenuAlignment::End),
        );
        let top = place_root(
            anchor,
            panel,
            viewport,
            MenuPlacementConfig::new(MenuPlacement::Top, MenuAlignment::End),
        );
        let bottom = place_root(
            anchor,
            panel,
            viewport,
            MenuPlacementConfig::new(MenuPlacement::Bottom, MenuAlignment::Center),
        );

        assert_eq!(right.origin, point(px(144.0), px(70.0)));
        assert_eq!(left.origin, point(px(16.0), px(60.0)));
        assert_eq!(top.origin, point(px(60.0), px(16.0)));
        assert_eq!(bottom.origin, point(px(80.0), px(124.0)));
    }

    #[test]
    fn root_placement_should_flip_a_horizontal_side_before_clamping() {
        let anchor = Bounds::new(point(px(350.0), px(40.0)), size(px(30.0), px(30.0)));
        let placed = place_root(
            anchor,
            size(px(100.0), px(50.0)),
            size(px(400.0), px(200.0)),
            MenuPlacementConfig::new(MenuPlacement::Right, MenuAlignment::Start),
        );

        assert_eq!(placed.origin.x, px(246.0));
    }

    #[test]
    fn size_catalog_should_preserve_three_bounded_widths() {
        let paint = test_theme().paint;
        let sizes = MenuSizes::new(
            MenuMetrics::new(px(208.0), px(28.0)),
            MenuMetrics::new(px(220.0), px(28.0)),
            MenuMetrics::new(px(248.0), px(28.0)),
        );
        let theme = MenuTheme::new(paint, sizes);

        assert_eq!(
            theme.resolve(MenuSize::Small).metrics.panel_width,
            px(208.0)
        );
        assert_eq!(
            theme.resolve(MenuSize::Regular).metrics.panel_width,
            px(220.0)
        );
        assert_eq!(theme.resolve(MenuSize::Wide).metrics.panel_width, px(248.0));
    }

    #[test]
    fn rows_should_use_one_leading_slot_and_leave_unselected_icons_visible() {
        assert_eq!(mark_indicator(EntryMark::None), None);
        assert_eq!(mark_indicator(EntryMark::Checkbox(false)), None);
        assert_eq!(mark_indicator(EntryMark::Checkbox(true)), Some("✓"));
        assert_eq!(
            mark_indicator(EntryMark::Radio {
                selected: false,
                index: 0,
            }),
            None
        );
        assert_eq!(
            mark_indicator(EntryMark::Radio {
                selected: true,
                index: 0,
            }),
            Some("●")
        );
    }

    #[test]
    fn highlighted_destructive_rows_should_preserve_danger_color() {
        let paint = test_theme().paint;

        assert_eq!(row_foreground(paint, false, true, true), paint.destructive);
        assert_eq!(
            row_secondary_foreground(paint, false, true, true),
            paint.destructive
        );
        assert_eq!(
            row_secondary_foreground(paint, false, false, true),
            paint.selected_foreground
        );
    }

    #[test]
    fn row_and_separator_geometry_should_follow_the_panel_content_grid() {
        let metrics = MenuMetrics::new(px(196.0), px(26.0))
            .horizontal_padding(px(6.0))
            .indicator_width(px(16.0))
            .gap(px(6.0))
            .corner_radius(px(8.0))
            .panel_spacing(px(3.0), px(2.0));

        let entries = vec![
            inert("New Window", false),
            inert("Rename Workspace", false),
            InternalEntry {
                kind: InternalEntryKind::Separator,
            },
            inert("Close Workspace", false),
        ];

        assert_eq!(row_corner_radius(metrics), px(5.0));
        assert_eq!(content_leading_inset(metrics), px(31.0));
        assert_eq!(metrics.panel_padding, px(3.0));
        assert_eq!(panel_size(&entries, metrics), size(px(196.0), px(95.0)));
    }

    #[test]
    fn picker_should_normalize_duplicate_current_values_to_one_selected_option() {
        let picker = Picker::new(
            "picker",
            "Picker",
            2,
            vec![
                PickerOption::new(2, "First Two"),
                PickerOption::new(2, "Second Two"),
            ],
        )
        .unwrap_or_else(|error| panic!("valid picker rejected: {error}"));
        let selected = picker
            .core
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    &entry.kind,
                    MenuEntryKind::Item(MenuItem {
                        mark: EntryMark::Radio { selected: true, .. },
                        ..
                    })
                )
            })
            .count();

        assert_eq!((picker.selected_label.as_ref(), selected), ("First Two", 1));
    }

    #[test]
    fn picker_should_reject_an_empty_option_list() {
        let result = Picker::new("picker", "Picker", 1_u8, Vec::new());

        assert!(matches!(result, Err(PickerBuildError::EmptyOptions)));
    }

    #[test]
    fn submenu_placement_should_flip_to_the_left() {
        let parent = Bounds::new(point(px(140.0), px(20.0)), size(px(100.0), px(80.0)));
        let placed = place_submenu(
            parent,
            px(30.0),
            size(px(100.0), px(60.0)),
            size(px(260.0), px(160.0)),
            px(8.0),
            px(2.0),
        );
        assert_eq!(placed.origin.x, px(38.0));
    }

    struct TestRoot {
        events: Rc<RefCell<Vec<MenuActivation<&'static str>>>>,
        other_focus: FocusHandle,
    }
    impl Render for TestRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let events = self.events.clone();
            div()
                .size_full()
                .child(div().track_focus(&self.other_focus).child("Other"))
                .child(
                    Menu::new(
                        "test-menu",
                        "Actions",
                        vec![
                            MenuEntry::action("Disabled", "disabled")
                                .disabled(true)
                                .debug_selector("disabled-entry"),
                            MenuEntry::action("Open", "open").debug_selector("open-entry"),
                        ],
                    )
                    .debug_selector("menu-trigger")
                    .on_activate(move |event, _, _| events.borrow_mut().push(event.clone())),
                )
        }
    }

    type TestMenuWindow<'a> = (
        Entity<TestRoot>,
        Rc<RefCell<Vec<MenuActivation<&'static str>>>>,
        &'a mut VisualTestContext,
    );

    fn menu_window(cx: &mut TestAppContext) -> TestMenuWindow<'_> {
        cx.update(super::init);
        cx.set_global(test_theme());
        let events = Rc::new(RefCell::new(Vec::new()));
        let root_events = events.clone();
        let (root, cx) = cx.add_window_view(move |_, cx| TestRoot {
            events: root_events,
            other_focus: cx.focus_handle().tab_stop(true),
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (root, events, cx)
    }

    #[gpui::test]
    fn pointer_should_open_and_activate_an_enabled_entry(cx: &mut TestAppContext) {
        let (_, events, cx) = menu_window(cx);
        let trigger = cx
            .debug_bounds("menu-trigger")
            .unwrap_or_else(|| panic!("trigger not painted"));
        cx.simulate_click(trigger.center(), Modifiers::none());
        cx.run_until_parked();
        let entry = cx
            .debug_bounds("open-entry")
            .unwrap_or_else(|| panic!("entry not painted"));
        cx.simulate_click(entry.center(), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            events.borrow().as_slice(),
            [MenuActivation::Action {
                action: "open",
                source: MenuActivationSource::Pointer
            }]
        );
    }

    #[gpui::test]
    fn open_menu_should_suppress_tooltips_until_the_menu_closes(cx: &mut TestAppContext) {
        let (_, _, cx) = menu_window(cx);
        let trigger = cx
            .debug_bounds("menu-trigger")
            .unwrap_or_else(|| panic!("trigger not painted"));

        cx.simulate_click(trigger.center(), Modifiers::none());
        cx.run_until_parked();
        assert!(cx.update(|window, cx| { crate::tooltip::window_tooltips_suppressed(window, cx) }));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(
            !cx.update(|window, cx| { crate::tooltip::window_tooltips_suppressed(window, cx) })
        );
    }

    #[gpui::test]
    fn trigger_press_drag_release_should_activate_the_traversed_entry(cx: &mut TestAppContext) {
        let (_, events, cx) = menu_window(cx);
        let trigger = cx
            .debug_bounds("menu-trigger")
            .unwrap_or_else(|| panic!("trigger not painted"));
        cx.simulate_mouse_down(trigger.center(), MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let entry = cx
            .debug_bounds("open-entry")
            .unwrap_or_else(|| panic!("entry not painted"));

        cx.simulate_mouse_move(entry.center(), Some(MouseButton::Left), Modifiers::none());
        cx.simulate_mouse_up(entry.center(), MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            events.borrow().as_slice(),
            [MenuActivation::Action {
                action: "open",
                source: MenuActivationSource::Pointer,
            }]
        );
    }

    #[gpui::test]
    fn isolated_pointer_release_should_not_activate_a_row(cx: &mut TestAppContext) {
        let (_, events, cx) = menu_window(cx);
        let trigger = cx
            .debug_bounds("menu-trigger")
            .unwrap_or_else(|| panic!("trigger not painted"));
        cx.simulate_click(trigger.center(), Modifiers::none());
        cx.run_until_parked();
        let entry = cx
            .debug_bounds("open-entry")
            .unwrap_or_else(|| panic!("entry not painted"));

        cx.simulate_mouse_up(entry.center(), MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        assert!(events.borrow().is_empty());
    }

    #[gpui::test]
    fn keyboard_should_open_and_skip_disabled_entries(cx: &mut TestAppContext) {
        let (_, events, cx) = menu_window(cx);
        cx.update(|window, _| {
            window.focus_next();
            window.focus_next();
        });
        cx.simulate_keystrokes("space");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            events.borrow().as_slice(),
            [MenuActivation::Action {
                action: "open",
                source: MenuActivationSource::Keyboard
            }]
        );
    }

    struct SemanticRoot {
        events: Rc<RefCell<Vec<MenuActivation<&'static str>>>>,
    }

    impl Render for SemanticRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let events = self.events.clone();
            Menu::new(
                "semantic-menu",
                "Semantic",
                vec![
                    MenuEntry::checkbox("Flag", false, "flag").debug_selector("checkbox-entry"),
                    MenuEntry::radio_group(
                        Some(99),
                        vec![
                            MenuRadioOption::new("One", "one"),
                            MenuRadioOption::new("Two", "two"),
                        ],
                    ),
                ],
            )
            .debug_selector("semantic-trigger")
            .on_activate(move |event, _, _| events.borrow_mut().push(event.clone()))
        }
    }

    #[gpui::test]
    fn checkbox_and_radio_should_emit_typed_proposals(cx: &mut TestAppContext) {
        cx.update(super::init);
        cx.set_global(test_theme());
        let events = Rc::new(RefCell::new(Vec::new()));
        let root_events = events.clone();
        let (_, cx) = cx.add_window_view(move |_, _| SemanticRoot {
            events: root_events,
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();

        let trigger = cx
            .debug_bounds("semantic-trigger")
            .unwrap_or_else(|| panic!("semantic trigger not painted"));
        cx.simulate_click(trigger.center(), Modifiers::none());
        cx.run_until_parked();
        let checkbox = cx
            .debug_bounds("checkbox-entry")
            .unwrap_or_else(|| panic!("checkbox not painted"));
        cx.simulate_click(checkbox.center(), Modifiers::none());
        cx.run_until_parked();

        cx.simulate_click(trigger.center(), Modifiers::none());
        cx.run_until_parked();
        let radio = cx
            .debug_bounds("One")
            .unwrap_or_else(|| panic!("radio option not painted"));
        cx.simulate_click(radio.center(), Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            events.borrow().as_slice(),
            [
                MenuActivation::Checkbox {
                    action: "flag",
                    checked: true,
                    source: MenuActivationSource::Pointer,
                },
                MenuActivation::Radio {
                    action: "one",
                    index: 0,
                    source: MenuActivationSource::Pointer,
                },
            ]
        );
    }

    struct PickerRoot {
        changes: Rc<RefCell<Vec<PickerChange<u8>>>>,
    }

    impl Render for PickerRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let changes = self.changes.clone();
            Picker::new(
                "test-picker",
                "Choice",
                1,
                vec![
                    PickerOption::new(1, "One")
                        .disabled(true)
                        .debug_selector("disabled-option"),
                    PickerOption::new(2, "Two").debug_selector("enabled-option"),
                ],
            )
            .unwrap_or_else(|error| panic!("valid picker rejected: {error}"))
            .debug_selector("picker-trigger")
            .on_change(move |change, _, _| changes.borrow_mut().push(change.clone()))
        }
    }

    #[gpui::test]
    fn picker_should_represent_current_value_and_skip_disabled_options(cx: &mut TestAppContext) {
        cx.update(super::init);
        cx.set_global(test_theme());
        let changes = Rc::new(RefCell::new(Vec::new()));
        let root_changes = changes.clone();
        let (_, cx) = cx.add_window_view(move |_, _| PickerRoot {
            changes: root_changes,
        });
        cx.update(|window, _| {
            window.activate_window();
            window.focus_next();
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("space");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert_eq!(
            changes.borrow().as_slice(),
            [PickerChange {
                value: 2,
                source: MenuActivationSource::Keyboard,
            }]
        );
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ActivationOrderEvent {
        Lifecycle(MenuLifecycleEvent),
        Activation,
    }

    struct ActivationOrderRoot {
        events: Rc<RefCell<Vec<ActivationOrderEvent>>>,
        callback_had_focus: Rc<Cell<bool>>,
    }

    impl Render for ActivationOrderRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let activation_events = self.events.clone();
            let lifecycle_events = self.events.clone();
            let callback_had_focus = self.callback_had_focus.clone();
            Menu::new(
                "activation-order-menu",
                "Activation order",
                vec![MenuEntry::action("Run", ())],
            )
            .debug_selector("activation-order-trigger")
            .on_activate(move |_, window, cx| {
                callback_had_focus.set(window.focused(cx).is_some());
                activation_events
                    .borrow_mut()
                    .push(ActivationOrderEvent::Activation);
            })
            .on_lifecycle(move |event, _| {
                lifecycle_events
                    .borrow_mut()
                    .push(ActivationOrderEvent::Lifecycle(*event));
            })
        }
    }

    #[gpui::test]
    fn activation_callback_should_run_after_internal_focus_restore_and_before_final_close(
        cx: &mut TestAppContext,
    ) {
        cx.update(super::init);
        cx.set_global(test_theme());
        let events = Rc::new(RefCell::new(Vec::new()));
        let callback_had_focus = Rc::new(Cell::new(false));
        let root_events = events.clone();
        let root_callback_had_focus = callback_had_focus.clone();
        let (_, cx) = cx.add_window_view(move |_, _| ActivationOrderRoot {
            events: root_events,
            callback_had_focus: root_callback_had_focus,
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();

        let trigger = cx
            .debug_bounds("activation-order-trigger")
            .unwrap_or_else(|| panic!("activation-order trigger not painted"));
        cx.simulate_click(trigger.center(), Modifiers::none());
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(callback_had_focus.get());
        assert_eq!(
            events.borrow().as_slice(),
            [
                ActivationOrderEvent::Lifecycle(MenuLifecycleEvent::Opened),
                ActivationOrderEvent::Activation,
                ActivationOrderEvent::Lifecycle(MenuLifecycleEvent::Closed(
                    MenuCloseReason::Activated,
                )),
            ]
        );
    }

    struct LifecycleRoot {
        lifecycle: Rc<RefCell<Vec<MenuLifecycleEvent>>>,
        underlay_presses: Rc<Cell<usize>>,
        other_focus: FocusHandle,
        show_menu: bool,
    }

    impl Render for LifecycleRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let lifecycle = self.lifecycle.clone();
            let underlay_presses = self.underlay_presses.clone();
            div()
                .id("lifecycle-root")
                .debug_selector(|| "lifecycle-root".into())
                .relative()
                .size_full()
                .on_mouse_down(MouseButton::Left, move |_, _, _| {
                    underlay_presses.set(underlay_presses.get() + 1);
                })
                .child(
                    div()
                        .debug_selector(|| "other-focus".into())
                        .track_focus(&self.other_focus)
                        .child("Other"),
                )
                .when(self.show_menu, |root| {
                    root.child(
                        Menu::new(
                            "lifecycle-menu",
                            "Lifecycle",
                            vec![MenuEntry::action("Run", ())],
                        )
                        .debug_selector("lifecycle-trigger")
                        .on_activate(|_, _, _| {})
                        .on_lifecycle(move |event, _| lifecycle.borrow_mut().push(*event)),
                    )
                })
        }
    }

    type LifecycleWindow<'a> = (
        Entity<LifecycleRoot>,
        Rc<RefCell<Vec<MenuLifecycleEvent>>>,
        Rc<Cell<usize>>,
        &'a mut VisualTestContext,
    );

    fn lifecycle_window(cx: &mut TestAppContext) -> LifecycleWindow<'_> {
        cx.update(super::init);
        cx.set_global(test_theme());
        let lifecycle = Rc::new(RefCell::new(Vec::new()));
        let underlay = Rc::new(Cell::new(0));
        let root_lifecycle = lifecycle.clone();
        let root_underlay = underlay.clone();
        let (root, cx) = cx.add_window_view(move |_, cx| LifecycleRoot {
            lifecycle: root_lifecycle,
            underlay_presses: root_underlay,
            other_focus: cx.focus_handle(),
            show_menu: true,
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (root, lifecycle, underlay, cx)
    }

    #[gpui::test]
    fn outside_dismissal_should_restore_focus_and_not_leak_to_underlay(cx: &mut TestAppContext) {
        let (root, lifecycle, underlay, cx) = lifecycle_window(cx);
        let focus = root.read_with(cx, |root, _| root.other_focus.clone());
        cx.update(|window, _| focus.focus(window));
        let trigger = cx
            .debug_bounds("lifecycle-trigger")
            .unwrap_or_else(|| panic!("lifecycle trigger not painted"));
        let root_bounds = cx
            .debug_bounds("lifecycle-root")
            .unwrap_or_else(|| panic!("lifecycle root not painted"));
        cx.simulate_click(trigger.center(), Modifiers::none());
        cx.run_until_parked();

        let outside = point(
            root_bounds.right() - px(4.0),
            root_bounds.bottom() - px(4.0),
        );
        cx.simulate_mouse_down(outside, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            lifecycle.borrow().as_slice(),
            [
                MenuLifecycleEvent::Opened,
                MenuLifecycleEvent::Closed(MenuCloseReason::Outside),
            ]
        );
        assert_eq!(underlay.get(), 0);
        assert!(!cx.update(|window, _| focus.is_focused(window)));

        cx.simulate_keystrokes("space");
        cx.run_until_parked();
        assert_eq!(lifecycle.borrow().last(), Some(&MenuLifecycleEvent::Opened),);
    }

    #[gpui::test]
    fn explicit_replacement_should_close_the_window_menu_without_restoring_focus(
        cx: &mut TestAppContext,
    ) {
        let (root, lifecycle, _, cx) = lifecycle_window(cx);
        let focus = root.read_with(cx, |root, _| root.other_focus.clone());
        cx.update(|window, _| focus.focus(window));
        let trigger = cx
            .debug_bounds("lifecycle-trigger")
            .unwrap_or_else(|| panic!("lifecycle trigger not painted"));
        cx.simulate_click(trigger.center(), Modifiers::none());
        cx.run_until_parked();

        let dismissed = cx.update(|window, cx| dismiss_active_menu_for_replacement(window, cx));
        cx.run_until_parked();

        assert!(dismissed.is_some());
        assert!(!cx.update(|window, _| focus.is_focused(window)));
        assert_eq!(
            lifecycle.borrow().as_slice(),
            [
                MenuLifecycleEvent::Opened,
                MenuLifecycleEvent::Closed(MenuCloseReason::Replaced),
            ]
        );
    }

    #[gpui::test]
    fn escape_and_deactivation_should_emit_one_closed_transition_each(cx: &mut TestAppContext) {
        let (_, lifecycle, _, cx) = lifecycle_window(cx);
        cx.update(|window, _| window.focus_next());
        cx.simulate_keystrokes("space");
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.simulate_keystrokes("space");
        cx.run_until_parked();
        cx.deactivate_window();
        cx.run_until_parked();

        assert_eq!(
            lifecycle.borrow().as_slice(),
            [
                MenuLifecycleEvent::Opened,
                MenuLifecycleEvent::Closed(MenuCloseReason::Escape),
                MenuLifecycleEvent::Opened,
                MenuLifecycleEvent::Closed(MenuCloseReason::Deactivated),
            ]
        );
    }

    #[gpui::test]
    fn disappearing_open_target_should_emit_closed_once(cx: &mut TestAppContext) {
        let (root, lifecycle, _, cx) = lifecycle_window(cx);
        let trigger = cx
            .debug_bounds("lifecycle-trigger")
            .unwrap_or_else(|| panic!("lifecycle trigger not painted"));
        cx.simulate_click(trigger.center(), Modifiers::none());
        cx.run_until_parked();
        root.update(cx, |root, cx| {
            root.show_menu = false;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        assert_eq!(
            lifecycle.borrow().as_slice(),
            [
                MenuLifecycleEvent::Opened,
                MenuLifecycleEvent::Closed(MenuCloseReason::TargetDisappeared),
            ]
        );
    }

    struct StateHarness {
        first: Entity<MenuState>,
        second: Entity<MenuState>,
    }

    impl Render for StateHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    fn state_harness(cx: &mut TestAppContext) -> (Entity<StateHarness>, &mut VisualTestContext) {
        cx.update(super::init);
        cx.set_global(test_theme());
        let (root, cx) = cx.add_window_view(|window, cx| {
            let first = cx.new(|cx| MenuState::new(window, cx));
            let second = cx.new(|cx| MenuState::new(window, cx));
            StateHarness { first, second }
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (root, cx)
    }

    fn submenu_entries() -> Vec<InternalEntry> {
        vec![
            InternalEntry {
                kind: InternalEntryKind::Submenu {
                    label: "Parent".into(),
                    disabled: false,
                    destructive: false,
                    shortcut: None,
                    icon: None,
                    debug_selector: None,
                    entries: vec![inert("Child", false)],
                },
            },
            inert("Other", false),
        ]
    }

    fn prepare_state(
        state: &Entity<MenuState>,
        entries: Vec<InternalEntry>,
        lifecycle: Option<MenuLifecycleHandler>,
        cx: &mut VisualTestContext,
    ) {
        let style = test_theme().resolve(MenuSize::Regular);
        cx.update(|window, app| {
            state.update(app, |state, cx| {
                state.trigger_bounds = Some(Bounds::new(
                    point(px(20.0), px(20.0)),
                    size(px(80.0), px(30.0)),
                ));
                state.synchronize(
                    entries,
                    style,
                    MenuPlacementConfig::default(),
                    true,
                    true,
                    false,
                    lifecycle,
                );
                state.open(None, OpenDirection::First, None, window, cx);
            });
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn pointer_submenu_timing_should_delay_open_and_preserve_transit_grace(
        cx: &mut TestAppContext,
    ) {
        let (root, cx) = state_harness(cx);
        let state = root.read_with(cx, |root, _| root.first.clone());
        prepare_state(&state, submenu_entries(), None, cx);

        state.update(cx, |state, cx| state.pointer_hover(0, 0, true, cx));
        cx.executor()
            .advance_clock(SUBMENU_OPEN_DELAY - Duration::from_millis(1));
        cx.run_until_parked();
        assert!(state.read_with(cx, |state, _| state.active_path.is_empty()));
        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        assert_eq!(
            state.read_with(cx, |state, _| state.active_path.clone()),
            [0]
        );

        state.update(cx, |state, cx| {
            state.pointer_hover(0, 0, false, cx);
            state.pointer_hover(1, 0, true, cx);
        });
        cx.executor().advance_clock(SUBMENU_CLOSE_GRACE);
        cx.run_until_parked();
        assert_eq!(
            state.read_with(cx, |state, _| state.active_path.clone()),
            [0]
        );

        state.update(cx, |state, cx| state.pointer_hover(1, 0, false, cx));
        cx.executor()
            .advance_clock(SUBMENU_CLOSE_GRACE - Duration::from_millis(1));
        cx.run_until_parked();
        assert_eq!(
            state.read_with(cx, |state, _| state.active_path.clone()),
            [0]
        );
        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        assert!(state.read_with(cx, |state, _| state.active_path.is_empty()));
    }

    #[gpui::test]
    fn keyboard_submenu_should_be_immediate_and_stale_pointer_open_should_be_inert(
        cx: &mut TestAppContext,
    ) {
        let (root, cx) = state_harness(cx);
        let state = root.read_with(cx, |root, _| root.first.clone());
        prepare_state(&state, submenu_entries(), None, cx);

        state.update(cx, |state, cx| state.open_submenu(cx));
        assert_eq!(
            state.read_with(cx, |state, _| state.active_path.clone()),
            [0]
        );
        state.update(cx, |state, cx| state.close_submenu(cx));
        state.update(cx, |state, cx| {
            state.pointer_hover(0, 0, true, cx);
            state.pointer_hover(0, 1, true, cx);
        });
        cx.executor().advance_clock(SUBMENU_OPEN_DELAY);
        cx.run_until_parked();

        assert!(state.read_with(cx, |state, _| state.active_path.is_empty()));
    }

    #[gpui::test]
    fn context_entries_should_freeze_after_the_opening_target_snapshot(cx: &mut TestAppContext) {
        let (root, cx) = state_harness(cx);
        let state = root.read_with(cx, |root, _| root.first.clone());
        let style = test_theme().resolve(MenuSize::Regular);
        cx.update(|window, app| {
            state.update(app, |state, cx| {
                state.trigger_bounds = Some(Bounds::new(
                    point(px(20.0), px(20.0)),
                    size(px(80.0), px(30.0)),
                ));
                state.synchronize(
                    vec![inert("Before capture", false)],
                    style,
                    MenuPlacementConfig::default(),
                    true,
                    false,
                    true,
                    None,
                );
                state.open(
                    Some(point(px(24.0), px(24.0))),
                    OpenDirection::First,
                    None,
                    window,
                    cx,
                );
                state.synchronize(
                    vec![inert("Captured target A", false)],
                    style,
                    MenuPlacementConfig::default(),
                    true,
                    false,
                    true,
                    None,
                );
                state.synchronize(
                    vec![inert("Later target B", false)],
                    style,
                    MenuPlacementConfig::default(),
                    true,
                    false,
                    true,
                    None,
                );
            });
        });

        let label = state.read_with(cx, |state, _| {
            state
                .entries
                .first()
                .and_then(InternalEntry::label)
                .map(str::to_owned)
        });
        assert_eq!(label.as_deref(), Some("Captured target A"));
    }

    #[gpui::test]
    fn competing_menu_should_open_replacement_before_closing_previous_chain(
        cx: &mut TestAppContext,
    ) {
        let (root, cx) = state_harness(cx);
        let (first, second) =
            root.read_with(cx, |root, _| (root.first.clone(), root.second.clone()));
        let events = Rc::new(RefCell::new(Vec::new()));
        let first_events = events.clone();
        let second_events = events.clone();
        let style = test_theme().resolve(MenuSize::Regular);
        cx.update(|window, app| {
            first.update(app, |state, _| {
                state.trigger_bounds = Some(Bounds::new(
                    point(px(10.0), px(10.0)),
                    size(px(20.0), px(20.0)),
                ));
                state.synchronize(
                    vec![inert("First", false)],
                    style,
                    MenuPlacementConfig::default(),
                    true,
                    true,
                    false,
                    Some(Rc::new(move |event, _| {
                        first_events.borrow_mut().push((1, *event))
                    })),
                );
            });
            second.update(app, |state, _| {
                state.trigger_bounds = Some(Bounds::new(
                    point(px(40.0), px(10.0)),
                    size(px(20.0), px(20.0)),
                ));
                state.synchronize(
                    vec![inert("Second", false)],
                    style,
                    MenuPlacementConfig::default(),
                    true,
                    true,
                    false,
                    Some(Rc::new(move |event, _| {
                        second_events.borrow_mut().push((2, *event))
                    })),
                );
            });
            open_menu(&first.downgrade(), None, OpenDirection::First, window, app);
            open_menu(&second.downgrade(), None, OpenDirection::First, window, app);
        });

        assert_eq!(
            events.borrow().as_slice(),
            [
                (1, MenuLifecycleEvent::Opened),
                (2, MenuLifecycleEvent::Opened),
                (1, MenuLifecycleEvent::Closed(MenuCloseReason::Replaced)),
            ]
        );
    }

    #[gpui::test]
    fn same_menu_reopen_during_activation_should_keep_new_reservation_and_lifecycle(
        cx: &mut TestAppContext,
    ) {
        let (root, cx) = state_harness(cx);
        let state = root.read_with(cx, |root, _| root.first.clone());
        let weak_state = state.downgrade();
        let events = Rc::new(RefCell::new(Vec::new()));
        let lifecycle_events = events.clone();
        let style = test_theme().resolve(MenuSize::Regular);
        cx.update(|window, app| {
            state.update(app, |state, _| {
                state.trigger_bounds = Some(Bounds::new(
                    point(px(10.0), px(10.0)),
                    size(px(20.0), px(20.0)),
                ));
                state.synchronize(
                    vec![inert("Reopen", false)],
                    style,
                    MenuPlacementConfig::default(),
                    true,
                    true,
                    false,
                    Some(Rc::new(move |event, _| {
                        lifecycle_events
                            .borrow_mut()
                            .push(ActivationOrderEvent::Lifecycle(*event));
                    })),
                );
            });
            open_menu(&weak_state, None, OpenDirection::First, window, app);
        });

        let activation_events = events.clone();
        let reopening_state = weak_state.clone();
        let activation: InternalActivation = Rc::new(move |_, window, cx| {
            activation_events
                .borrow_mut()
                .push(ActivationOrderEvent::Activation);
            open_menu(&reopening_state, None, OpenDirection::First, window, cx);
        });
        cx.update(|window, app| {
            activate_menu(
                &weak_state,
                &activation,
                MenuActivationSource::Keyboard,
                window,
                app,
            );
        });

        let state_is_coherent = cx.update(|window, app| {
            let window_id = window.window_handle().window_id();
            let reservation = state.read(app).reservation;
            let owns_window = app
                .global::<MenuCoordinator>()
                .owners
                .get(&window_id)
                .is_some_and(|ownership| {
                    ownership.owner == weak_state && Some(ownership.reservation) == reservation
                });
            (
                state.read(app).open,
                owns_window,
                window_menu_is_open(window, app),
            )
        });
        assert_eq!(state_is_coherent, (true, true, true));
        assert_eq!(
            events.borrow().as_slice(),
            [
                ActivationOrderEvent::Lifecycle(MenuLifecycleEvent::Opened),
                ActivationOrderEvent::Activation,
                ActivationOrderEvent::Lifecycle(MenuLifecycleEvent::Opened),
            ]
        );
    }

    struct ContextRequestRoot {
        accept: Rc<Cell<bool>>,
        requests: Rc<RefCell<Vec<Point<Pixels>>>>,
    }

    impl Render for ContextRequestRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let accept = self.accept.clone();
            let requests = self.requests.clone();
            ContextMenu::new(
                "request-context",
                "Requested context",
                div()
                    .debug_selector(|| "request-target".into())
                    .w(px(80.0))
                    .h(px(40.0)),
                vec![MenuEntry::action("Inspect", ())],
            )
            .on_open_request(move |request, _, _| {
                requests.borrow_mut().push(request.position());
                accept.get()
            })
            .on_activate(|_, _, _| {})
        }
    }

    #[gpui::test]
    fn context_open_request_should_capture_position_and_allow_rejection(cx: &mut TestAppContext) {
        cx.update(super::init);
        cx.set_global(test_theme());
        let accept = Rc::new(Cell::new(false));
        let requests = Rc::new(RefCell::new(Vec::new()));
        let root_accept = accept.clone();
        let root_requests = requests.clone();
        let (_, cx) = cx.add_window_view(move |_, _| ContextRequestRoot {
            accept: root_accept,
            requests: root_requests,
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let target = cx
            .debug_bounds("request-target")
            .unwrap_or_else(|| panic!("request target not painted"));

        cx.simulate_mouse_down(target.center(), MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(target.center(), MouseButton::Right, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(requests.borrow().as_slice(), [target.center()]);
        assert!(cx.debug_bounds("Inspect").is_none());

        accept.set(true);
        cx.simulate_mouse_down(target.center(), MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(target.center(), MouseButton::Right, Modifiers::none());
        cx.run_until_parked();
        assert!(cx.debug_bounds("Inspect").is_some());
    }

    struct ContextRoot;

    impl Render for ContextRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ContextMenu::new(
                "context",
                "Context actions",
                div()
                    .debug_selector(|| "context-target".into())
                    .w(px(80.0))
                    .h(px(40.0)),
                vec![MenuEntry::action("Inspect", ())],
            )
            .on_activate(|_, _, _| {})
        }
    }

    struct ContextDragRoot {
        activations: Rc<Cell<usize>>,
    }

    impl Render for ContextDragRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let activations = self.activations.clone();
            ContextMenu::new(
                "drag-context",
                "Context actions",
                div()
                    .debug_selector(|| "drag-context-target".into())
                    .w(px(80.0))
                    .h(px(40.0)),
                vec![MenuEntry::action("Drag Inspect", ()).debug_selector("drag-context-entry")],
            )
            .on_activate(move |_, _, _| activations.set(activations.get() + 1))
        }
    }

    #[gpui::test]
    fn context_press_drag_release_should_activate_with_the_secondary_button(
        cx: &mut TestAppContext,
    ) {
        cx.update(super::init);
        cx.set_global(test_theme());
        let activations = Rc::new(Cell::new(0));
        let root_activations = activations.clone();
        let (_, cx) = cx.add_window_view(move |_, _| ContextDragRoot {
            activations: root_activations,
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let target = cx
            .debug_bounds("drag-context-target")
            .unwrap_or_else(|| panic!("context target not painted"));

        cx.simulate_mouse_down(target.center(), MouseButton::Right, Modifiers::none());
        cx.run_until_parked();
        let entry = cx
            .debug_bounds("drag-context-entry")
            .unwrap_or_else(|| panic!("context entry not painted"));
        cx.simulate_mouse_move(entry.center(), Some(MouseButton::Right), Modifiers::none());
        cx.simulate_mouse_up(entry.center(), MouseButton::Right, Modifiers::none());
        cx.run_until_parked();

        assert_eq!(activations.get(), 1);
    }

    #[gpui::test]
    fn context_menu_should_open_for_control_primary_click(cx: &mut TestAppContext) {
        cx.update(super::init);
        cx.set_global(test_theme());
        let (_, cx) = cx.add_window_view(|_, _| ContextRoot);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let target = cx
            .debug_bounds("context-target")
            .unwrap_or_else(|| panic!("target not painted"));
        let modifiers = Modifiers {
            control: true,
            ..Modifiers::none()
        };
        cx.simulate_click(target.center(), modifiers);
        cx.run_until_parked();
        assert!(cx.debug_bounds("Inspect").is_some());
    }
}
