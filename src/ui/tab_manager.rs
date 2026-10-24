use super::pane_lifecycle::PaneConstruction;
use crate::ui::appearance::gpui_color;
#[cfg(test)]
use gpui::rgba;
#[cfg(test)]
use std::cell::Cell;
use std::rc::Rc;

use super::chrome_icons::IconRole;
use super::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use super::drag_and_drop::{
    DragPreview, DragSession, ItemMotion, ReorderableStrip, StripShape, drag_release_observer,
    lift_shadow,
};
use super::selection_chip::{ChipPaint, ChipShape, SelectionChip};
use super::terminal_focus::{TabFocusOwners, TerminalFocusBlocker, TerminalFocusCoordinator};
use super::terminal_status::{StatusColors, StatusGlyph};
use super::{
    ActivateTab1, ActivateTab2, ActivateTab3, ActivateTab4, ActivateTab5, ActivateTab6,
    ActivateTab7, ActivateTab8, ActivateTab9, CloseTab, CreateTab, MoveTabLeft, MoveTabRight,
    NextTab, PreviousTab, SplitDown, SplitRight, TERMINAL_KEY_CONTEXT, TabIdentity, TabView,
    TabViewEvent, WORKSPACE_SIDEBAR_DEFAULT_WIDTH,
};
#[cfg(test)]
use super::{TOP_CHROME_HEIGHT, WORKSPACE_SIDEBAR_MINIMUM_WIDTH};

use crate::appearance::ChromeColors;
use crate::appearance::Color;
use crate::domain::{
    CloseTabOutcome, PaneId, SplitAxis, TabCollection, TabError, TabId, TabStep, WorkspaceId,
    WorktreeId,
};
#[cfg(test)]
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::platform::window_movement::{
    OperatingSystemWindowDragError, OperatingSystemWindowDragPlatform, WindowMoveStart,
};
use crate::terminal::{NativeServiceOrigin, NativeServiceStatus, WorkspaceTerminalSessionFactory};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, Edges, Element, ElementId, Entity, EventEmitter,
    GlobalElementId, InspectorElementId, LayoutId, MouseButton, Pixels, Render, ScrollHandle,
    Window, accesskit, div, px, relative,
};
use spaceterm_ui::{
    Alert, AlertIntent, ButtonSize, ButtonTheme, ButtonVariant, ContextMenu, ContextMenuTarget,
    CustomIconName, HoverFade, Icon, IconButton, IconName, MenuEntry, MenuLifecycleEvent, MenuSize,
    ModalAction, ModalActionRole, ModalId, Tooltip, WindowDragRegion, WindowDragRegionEvent,
    WindowDragRegionResponse, WindowDragRegionStatus,
};

#[cfg(test)]
const TAB_BAR_HEIGHT: f32 = TOP_CHROME_HEIGHT;
const TAB_ITEM_WIDTH: f32 = 178.2;
const TAB_ITEM_MINIMUM_WIDTH: f32 = 159.3;
pub(super) const TAB_ITEM_MAXIMUM_WIDTH: f32 = 216.0;
/// The title starts as far inside the chip as a Settings navigation label does inside its own, and
/// Close uses the same inset at the chip's right, top, and bottom edges.
const TAB_ITEM_LEFT_PADDING: f32 = 11.0;
const TAB_ITEM_RIGHT_PADDING: f32 = 7.0;
/// The geometry of the chip carrying one Tab's material, resolved from the Workspace frame.
/// A Tab keeps the full title-bar height as its hit target; only the paint moves inward.
fn tab_chip_shape(appearance: &super::appearance::ChromeAppearance, cx: &App) -> ChipShape {
    let frame = super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx);
    ChipShape {
        inset_leading: frame.strip_chip_leading_inset(),
        inset_trailing: frame.strip_chip_trailing_inset(),
        inset_y: frame.space(),
        radius: frame.chip_radius(),
    }
}

/// The leading Terminal glyph every Tab carries, and its gap to the Tab's identity.
const TAB_ORIGIN_GAP: f32 = 6.0;
/// The gap between the status glyph and close control.
const TAB_TRAILING_GAP: f32 = 4.0;
/// The share of a Tab's identity the activity may claim, so a wordy title never pushes the
/// directory leaf out of the Tab.
const TAB_ACTIVITY_MAXIMUM_SHARE: f32 = 0.6;
/// The Compact-density length of the hairline at a Tab-strip boundary that no chip touches.
const TAB_SEPARATOR_LENGTH: f32 = 18.0;
/// One logical point at every density. It covers at least one device pixel at every supported
/// scale, where a scale-derived width would leave layout rounding to pick a side of the edge.
const TAB_SEPARATOR_WIDTH: f32 = 1.0;

/// The selected Tab material shared by the collapsed Workspace Switcher.
pub(super) fn active_tab_surface(
    appearance: &super::appearance::ChromeAppearance,
    window_active: bool,
) -> ChipPaint {
    let presentation = TabChromePresentation::resolve(
        window_active,
        appearance.capabilities.show_borders,
        &appearance.colors,
    );
    presentation.selected_chip_paint(appearance)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TabChromePresentation {
    window_active: bool,
    show_borders: bool,
    background: Color,
    active_tab_background: Color,
    active_tab_border: Color,
    active_tab_hover_background: Color,
    active_tab_hover_border: Color,
    active_tab_hover_foreground: Color,
    active_tab_hover_icon: Color,
    inactive_tab_background: Color,
    active_tab_foreground: Color,
    inactive_tab_foreground: Color,
    active_tab_icon: Color,
    inactive_tab_icon: Color,
    hover_background: Color,
    hover_foreground: Color,
    hover_icon: Color,
    tab_separator: Color,
}

impl TabChromePresentation {
    fn resolve(window_active: bool, show_borders: bool, colors: &ChromeColors) -> Self {
        if window_active {
            Self {
                window_active,
                show_borders,
                background: colors.title_bar_background,
                active_tab_background: colors.tab_active_background,
                active_tab_border: colors.tab_active_border,
                active_tab_hover_background: colors.tab_active_hover_background,
                active_tab_hover_border: colors.tab_active_border,
                active_tab_hover_foreground: colors.tab_active_hover_foreground,
                active_tab_hover_icon: colors.tab_active_hover_icon,
                inactive_tab_background: colors.tab_inactive_background,
                active_tab_foreground: colors.tab_active_foreground,
                inactive_tab_foreground: colors.tab_inactive_foreground,
                active_tab_icon: colors.tab_active_icon,
                inactive_tab_icon: colors.tab_inactive_icon,
                hover_background: colors.tab_hover_background,
                hover_foreground: colors.tab_hover_foreground,
                hover_icon: colors.tab_hover_icon,
                tab_separator: colors.tab_separator,
            }
        } else {
            Self {
                window_active,
                show_borders,
                background: colors.title_bar_inactive_background,
                // ChromeAppearance has already prepared the active-state roles for an inactive window.
                active_tab_background: colors.tab_active_background,
                active_tab_border: colors.tab_active_border,
                active_tab_hover_background: colors.tab_active_hover_background,
                active_tab_hover_border: colors.tab_active_border,
                active_tab_hover_foreground: colors.tab_active_hover_foreground,
                active_tab_hover_icon: colors.tab_active_hover_icon,
                inactive_tab_background: colors.title_bar_inactive_background,
                active_tab_foreground: colors.tab_active_foreground,
                inactive_tab_foreground: colors.tab_inactive_foreground,
                active_tab_icon: colors.tab_active_icon,
                inactive_tab_icon: colors.tab_inactive_icon,
                hover_background: colors.tab_hover_background,
                hover_foreground: colors.tab_hover_foreground,
                hover_icon: colors.tab_hover_icon,
                tab_separator: colors.tab_separator,
            }
        }
    }

    /// The material one Tab rests on, as an inset chip within the title-bar surface.
    /// An inactive Tab paints its own fill so a scheme's distinct inactive Tab color still shows.
    fn tab_chip(
        &self,
        active: bool,
        appearance: &super::appearance::ChromeAppearance,
        cx: &App,
    ) -> SelectionChip {
        let paint = self.tab_chip_paint(active);
        let paint = if active {
            self.selected_chip_paint(appearance)
        } else {
            paint.raised_on(appearance, self.background)
        };
        SelectionChip::new(tab_chip_shape(appearance, cx), paint)
    }

    fn selected_chip_paint(&self, appearance: &super::appearance::ChromeAppearance) -> ChipPaint {
        self.tab_chip_paint(true)
            .selected_on(appearance, self.background)
    }

    fn tab_chip_paint(&self, active: bool) -> ChipPaint {
        let mut paint = if active {
            ChipPaint {
                fill: Some(self.active_tab_background),
                rim: Some(self.active_tab_border),
                hover_fill: Some(self.active_tab_hover_background),
                hover_rim: Some(self.active_tab_hover_border),
            }
        } else {
            ChipPaint {
                fill: (self.inactive_tab_background != self.background)
                    .then_some(self.inactive_tab_background),
                rim: self.show_borders.then_some(self.active_tab_border),
                hover_fill: Some(self.hover_background),
                hover_rim: self.show_borders.then_some(self.active_tab_border),
            }
        };
        if !self.window_active {
            paint.hover_fill = None;
            paint.hover_rim = None;
        }
        paint
    }

    fn tab_foreground(&self, active: bool) -> Color {
        if active {
            self.active_tab_foreground
        } else {
            self.inactive_tab_foreground
        }
    }

    /// The text a Tab's title takes while the pointer is over its chip.
    fn tab_hover_foreground(&self, active: bool) -> Color {
        if active {
            self.active_tab_hover_foreground
        } else {
            self.hover_foreground
        }
    }

    /// Status glyph colors resolved for this Tab's current rest or hover surface.
    fn tab_status(
        &self,
        active: bool,
        hovered: bool,
        colors: &ChromeColors,
    ) -> crate::appearance::StatusPaint {
        colors.status(self.tab_surface(active, hovered))
    }

    fn tab_surface(&self, active: bool, hovered: bool) -> Color {
        match (active, hovered) {
            (true, true) => self.active_tab_hover_background,
            (true, false) => self.active_tab_background,
            (false, true) => self.hover_background,
            (false, false) => self.inactive_tab_background,
        }
        .source_over(self.background)
    }

    fn close_control_style(
        &self,
        active: bool,
        ancestor_hovered: bool,
        colors: &ChromeColors,
        materials: crate::appearance::SurfaceMaterials,
    ) -> spaceterm_ui::ButtonVariantStyle {
        let host = if active {
            self.active_tab_hover_background
        } else {
            self.hover_background
        };
        self.control_style(active, ancestor_hovered, colors, materials, host)
    }

    fn bar_control_style(
        &self,
        colors: &ChromeColors,
        materials: crate::appearance::SurfaceMaterials,
    ) -> spaceterm_ui::ButtonVariantStyle {
        self.control_style(false, false, colors, materials, self.background)
    }

    /// The close glyph's color while the close control itself is not under the pointer.
    fn close_icon(&self, active: bool, ancestor_hovered: bool) -> Color {
        match (active, ancestor_hovered) {
            (true, true) => self.active_tab_hover_icon,
            (false, true) => self.hover_icon,
            (true, false) => self.active_tab_icon,
            (false, false) => self.inactive_tab_icon,
        }
    }

    fn control_style(
        &self,
        active: bool,
        ancestor_hovered: bool,
        colors: &ChromeColors,
        materials: crate::appearance::SurfaceMaterials,
        host: Color,
    ) -> spaceterm_ui::ButtonVariantStyle {
        let clear = gpui::rgba(0);
        let icon = self.close_icon(active, ancestor_hovered);
        let normal = spaceterm_ui::ButtonPaint::new(clear, gpui_color(icon), clear);
        let hover_background = materials.paint(
            crate::appearance::SurfaceRole::Surface,
            host,
            self.hover_background,
        );
        let hover = spaceterm_ui::ButtonPaint::new(
            gpui_color(hover_background),
            gpui_color(self.hover_icon),
            clear,
        );
        let disabled =
            spaceterm_ui::ButtonPaint::new(clear, gpui_color(colors.text_disabled), clear);
        let hovered = if self.window_active { hover } else { normal };
        spaceterm_ui::ButtonVariantStyle::new(normal, hovered, hover, disabled)
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) enum TabManagerEvent {
    ClosePaneRequested { tab_id: TabId, pane_id: PaneId },
    CloseTabRequested { tab_id: TabId },
    FinalTabCloseRequested { final_tab_id: TabId },
    PresentationChanged,
}

impl std::fmt::Debug for TabManagerEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ClosePaneRequested { .. } => "TabManagerEvent::ClosePaneRequested",
            Self::CloseTabRequested { .. } => "TabManagerEvent::CloseTabRequested",
            Self::FinalTabCloseRequested { .. } => "TabManagerEvent::FinalTabCloseRequested",
            Self::PresentationChanged => "TabManagerEvent::PresentationChanged",
        })
    }
}

/// Whether a Tab face is the Tab in the Tab bar or its lifted copy following the pointer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TabItemRole {
    InBar,
    Lifted,
}

/// Where the pointer stands over one Tab.
#[derive(Clone, Copy)]
struct TabHover {
    /// How far the Tab has eased toward its hovered paint.
    level: f32,
    /// Whether the pointer is over the Tab now, which selects the glyph paints at once.
    under_pointer: bool,
}

/// A command in a Tab's context menu. Each one targets the Tab whose menu chose it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TabMenuCommand {
    NewTab,
    SplitRight,
    SplitDown,
    MoveLeft,
    MoveRight,
    Close,
}

/// The value a Tab drag carries, scoped to the Tab bar that owns the Tab.
struct DraggedTab {
    tab_id: TabId,
    owner: gpui::EntityId,
}

/// One shared server Workspace's Tabs, when a SpaceTerm Server owns the Workspace.
type SharedTabBinding = super::shared_workspaces::SharedTabs;

/// What a shared server Workspace's Tab strip offers. Creating a Tab, splitting a Pane, and
/// closing a Tab ask the server. They are unavailable offline or when the bounded connected
/// queue is full.
#[derive(Clone, Copy)]
struct SharedStripControls {
    edits: bool,
    edits_reason: &'static str,
}

pub(crate) struct TabManager {
    tabs: TabCollection<Entity<TabView>>,
    readiness: super::terminal_readiness::Readiness,
    session_factory: WorkspaceTerminalSessionFactory,
    pane_construction: PaneConstruction,
    active: bool,
    sidebar_visible: bool,
    /// How much of the sidebar the current frame shows, from 0 when hidden to 1 when shown.
    sidebar_reveal: f32,
    sidebar_width: Pixels,
    top_chrome_width: Pixels,
    trailing_accessory: Option<gpui::AnyView>,
    window_close_handler: Option<spaceterm_ui::WindowCloseHandler>,
    parent_focus_blocker: Option<TerminalFocusBlocker>,
    tab_selector_pressed: Option<TabId>,
    tab_menu: Option<TabId>,
    tab_reorder: ReorderableStrip<TabId>,
    operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
    window_drag_status: WindowDragRegionStatus,
    tab_bar_scroll_handle: ScrollHandle,
    close_workspace_requested: bool,
    /// The server Workspace this Tab strip presents, when a SpaceTerm Server owns it.
    shared: Option<SharedTabBinding>,
    parked_shared_tabs: std::collections::BTreeMap<
        TabId,
        crate::domain::tab_collection::ParkedTab<Entity<TabView>>,
    >,
    /// The Active Tab this client's just-submitted shared close ends, when the keyboard rested
    /// in this strip as the person asked for it. The next reconciliation takes it.
    shared_close_keyboard: Option<TabId>,
    #[cfg(test)]
    rendered_window_active: bool,
    #[cfg(test)]
    rendered_active_close_icon: Rc<Cell<gpui::Rgba>>,
    #[cfg(test)]
    rendered_inactive_close_icon: Rc<Cell<gpui::Rgba>>,
    #[cfg(test)]
    rendered_create_tab_icon: Rc<Cell<gpui::Rgba>>,
}

impl TabManager {
    fn report_tab_error(operation: &str, error: TabError) {
        eprintln!("failed to {operation} Tab: {error}");
    }

    #[cfg(test)]
    pub(crate) fn new(
        session_factory: WorkspaceTerminalSessionFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_operating_system_window_drag_platform(
            session_factory,
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            window,
            cx,
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_operating_system_window_drag_platform(
        session_factory: WorkspaceTerminalSessionFactory,
        operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_pane_construction(
            session_factory,
            operating_system_window_drag_platform,
            PaneConstruction::testing(),
            window,
            cx,
        )
    }

    pub(crate) fn with_pane_construction(
        session_factory: WorkspaceTerminalSessionFactory,
        operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
        pane_construction: PaneConstruction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tabs = TabCollection::new(|tab_id| {
            Self::create_tab_view(
                tab_id,
                session_factory.clone(),
                pane_construction.clone(),
                window,
                cx,
            )
        });
        Self {
            tabs,
            readiness: super::terminal_readiness::Readiness::for_window(window, cx),
            session_factory,
            pane_construction,
            active: true,
            sidebar_visible: true,
            sidebar_reveal: 1.0,
            sidebar_width: px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            top_chrome_width: px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            trailing_accessory: None,
            window_close_handler: None,
            parent_focus_blocker: None,
            tab_selector_pressed: None,
            tab_menu: None,
            tab_reorder: ReorderableStrip::new(gpui::Axis::Horizontal),
            operating_system_window_drag_platform,
            window_drag_status: WindowDragRegionStatus::new(),
            tab_bar_scroll_handle: ScrollHandle::new(),
            close_workspace_requested: false,
            shared: None,
            parked_shared_tabs: std::collections::BTreeMap::new(),
            shared_close_keyboard: None,
            #[cfg(test)]
            rendered_window_active: window.is_window_active(),
            #[cfg(test)]
            rendered_active_close_icon: Rc::new(Cell::new(rgba(0))),
            #[cfg(test)]
            rendered_inactive_close_icon: Rc::new(Cell::new(rgba(0))),
            #[cfg(test)]
            rendered_create_tab_icon: Rc::new(Cell::new(rgba(0))),
        }
    }

    fn create_tab_view(
        tab_id: TabId,
        session_factory: WorkspaceTerminalSessionFactory,
        pane_construction: PaneConstruction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TabView> {
        let view = cx.new(|cx| {
            TabView::with_pane_construction(tab_id, session_factory, pane_construction, window, cx)
        });
        debug_assert_eq!(view.read(cx).tab_id(), tab_id);
        cx.subscribe_in(
            &view,
            window,
            |manager, _, event: &TabViewEvent, window, cx| match event {
                TabViewEvent::UserClosePaneRequested { tab_id, pane_id } => {
                    cx.emit(TabManagerEvent::ClosePaneRequested {
                        tab_id: *tab_id,
                        pane_id: *pane_id,
                    });
                }
                TabViewEvent::CloseTabRequested { tab_id } => {
                    manager.close_tab(*tab_id, window, cx);
                }
                TabViewEvent::PresentationChanged { .. } => {
                    cx.emit(TabManagerEvent::PresentationChanged);
                    cx.notify();
                }
            },
        )
        .detach();
        view
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        if self.active {
            self.tabs
                .active_tab()
                .update(cx, |tab, cx| tab.focus(window, cx));
        }
    }

    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only a desktop Services Adapter queries Services state"
        )
    )]
    pub(crate) fn native_service_status(
        &self,
        workspace_id: WorkspaceId,
        window: &Window,
        cx: &mut App,
    ) -> NativeServiceStatus {
        if !self.active {
            return NativeServiceStatus::default();
        }
        let blocker = self.terminal_focus_blocker();
        self.tabs.active_tab().update(cx, |view, cx| {
            view.set_focus_branch(true, blocker, cx);
            view.native_service_status(workspace_id, window, cx)
        })
    }

    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only a desktop Services Adapter queries Services state"
        )
    )]
    pub(crate) fn native_service_target(
        &self,
        origin: NativeServiceOrigin,
        cx: &App,
    ) -> Option<Entity<super::TerminalPane>> {
        if !self.active || self.tabs.active_tab_id() != origin.tab_id() {
            return None;
        }
        self.tabs
            .tab(origin.tab_id())?
            .read(cx)
            .native_service_target(origin)
    }

    /// Opens this Workspace's Tabs for a person, which is an explicit opening of its Active Tab
    /// and so an explicit selection of that Tab's Focused Pane.
    ///
    /// Only that Pane is asked for: every other Pane keeps the grant it already has, or none.
    /// Restoring an Active Workspace while publishing membership, or while a person's focus stays
    /// in the sidebar, uses [`Self::activate_without_focus`], which asks the server for nothing.
    pub(crate) fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_without_focus(cx);
        self.tabs
            .active_tab()
            .update(cx, |view, cx| view.open_focused_pane(window, cx));
    }

    /// Opens this Workspace in place of one that went away. The keyboard moves into its Active
    /// Tab, but nobody chose the Workspace, so no Pane is selected and a restored Pane waits for
    /// its first use.
    pub(crate) fn activate_in_place(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_without_focus(cx);
        self.focus(window, cx);
    }

    pub(crate) fn activate_without_focus(&mut self, cx: &mut Context<Self>) {
        self.active = true;
        self.tabs
            .active_tab()
            .update(cx, |view, cx| view.activate_without_focus(cx));
        self.sync_terminal_focus_blocker(cx);
    }

    /// Where the Tab strip's Tabs and selection stand in the window, so the strip of the
    /// Workspace that takes this one's place can start from there.
    pub(crate) fn tab_strip_shape(&self, cx: &App) -> Option<StripShape> {
        self.tab_reorder.shape(&self.tab_bar_scroll_handle, cx)
    }

    /// Starts the Tab strip from `from`, the shape of the strip it takes the place of.
    pub(crate) fn arrive_from(&mut self, from: StripShape, cx: &mut Context<Self>) {
        self.tab_reorder.arrive(from, cx);
        cx.notify();
    }

    pub(crate) fn deactivate(&mut self, cx: &mut Context<Self>) {
        self.active = false;
        self.tabs
            .active_tab()
            .update(cx, |view, cx| view.deactivate(cx));
        self.tab_selector_pressed = None;
        self.sync_terminal_focus_blocker(cx);
    }

    /// Tells each Pane that the person stopped its server, as the server's route reported.
    pub(crate) fn server_stopped(&self, cx: &mut App) {
        for (_, view) in self.tabs.iter() {
            view.update(cx, |view, cx| view.server_stopped(cx));
        }
    }

    /// Reconnects each Pane the person's stop interrupted, once they started the server.
    pub(crate) fn server_started(&self, cx: &mut App) {
        for (_, view) in self.tabs.iter() {
            view.update(cx, |view, cx| view.server_started(cx));
        }
    }

    pub(crate) fn close_all(&self, cx: &mut App) {
        for (_, view) in self.tabs.iter() {
            view.update(cx, |view, cx| view.close_all(cx));
        }
        for parked in self.parked_shared_tabs.values() {
            parked.payload().update(cx, |view, cx| view.close_all(cx));
        }
    }

    pub(crate) fn terminal_panes<'a>(
        &'a self,
        cx: &'a App,
    ) -> impl Iterator<Item = (TabId, PaneId, &'a super::terminal_pane::TerminalPane)> {
        self.tabs.iter().flat_map(move |(tab, view)| {
            view.read(cx)
                .terminal_panes(cx)
                .map(move |(pane, terminal)| (tab, pane, terminal))
        })
    }

    pub(crate) fn close_pane_authorized(
        &mut self,
        tab_id: TabId,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.tabs.tab(tab_id).cloned() else {
            return;
        };
        view.update(cx, |view, cx| {
            view.close_pane_authorized(pane_id, window, cx)
        });
    }

    pub(crate) fn close_tab_authorized(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_tab(tab_id, window, cx);
    }

    #[cfg(test)]
    pub(crate) fn aggregate_counts(&self, cx: &App) -> (usize, usize) {
        let panes = self
            .tabs
            .iter()
            .map(|(_, view)| view.read(cx).pane_count())
            .sum();
        (self.tabs.len(), panes)
    }

    /// The Root Tab's Root Pane machine and Repository Source Directory.
    pub(crate) fn repository_source(
        &self,
        cx: &App,
    ) -> Option<(
        crate::repository_status::RepositoryMachine,
        crate::repository_status::scheduler::SourceDirectory,
    )> {
        self.tabs.root_tab().read(cx).repository_source(cx)
    }

    pub(crate) fn automatic_directory(&self, cx: &App) -> Option<crate::domain::CurrentDirectory> {
        self.tabs.root_tab().read(cx).automatic_directory(cx)
    }

    pub(crate) fn active_terminal_identity(&self, cx: &App) -> (TabId, PaneId) {
        let tab_id = self.tabs.active_tab_id();
        let pane_id = self.tabs.active_tab().read(cx).focused_pane_id();
        (tab_id, pane_id)
    }

    pub(crate) fn set_sidebar_layout(
        &mut self,
        visible: bool,
        sidebar_width: Pixels,
        top_chrome_width: Pixels,
        cx: &mut Context<Self>,
    ) {
        let reveal = if visible { 1.0 } else { 0.0 };
        if self.sidebar_visible != visible
            || self.sidebar_reveal != reveal
            || self.sidebar_width != sidebar_width
            || self.top_chrome_width != top_chrome_width
        {
            self.readiness.invalidate_layout();
            self.sidebar_visible = visible;
            self.sidebar_reveal = reveal;
            self.sidebar_width = sidebar_width;
            self.top_chrome_width = top_chrome_width;
            cx.notify();
        }
    }

    /// Where the stage's leading edge rests and where this frame draws it. The stage is laid out
    /// where it rests and drawn a whole number of device pixels from there, so the active Tab's
    /// layout, and the size of its Terminal Sessions, is the same in every frame.
    fn stage_leading_edge(&self, window: &Window, cx: &App) -> StageLeadingEdge {
        let frame = super::workspace_frame::WorkspaceFrame::for_appearance(
            super::appearance::chrome(cx),
            cx,
        );
        // The stage's leading margin and inset beside `reveal` of the sidebar.
        let leading_edge = |reveal: f32| {
            let hidden = frame.stage_leading_inset(false);
            let inset = hidden + (frame.stage_leading_inset(true) - hidden) * reveal;
            (self.sidebar_width * reveal, inset)
        };
        let (rest_margin, rest_inset) = leading_edge(if self.sidebar_visible { 1.0 } else { 0.0 });
        let scale = window.scale_factor();
        let snap = |length: Pixels| (length * scale).round() / scale;
        let (margin, inset) = leading_edge(self.sidebar_reveal);
        let inset = snap(inset);
        let offset = snap(rest_margin + rest_inset - margin - inset);
        StageLeadingEdge {
            rest_margin,
            rest_inset,
            margin: rest_margin + rest_inset - offset - inset,
            inset,
            offset,
        }
    }

    /// Presents `reveal` of the sidebar in the current frame while it shows or hides. The stage's
    /// leading edge follows the sidebar, and the active Tab keeps the width it has at rest, so its
    /// Terminal Sessions resize once per change rather than once per frame.
    pub(crate) fn present_sidebar(&mut self, reveal: f32, cx: &mut Context<Self>) {
        if self.sidebar_reveal != reveal {
            self.sidebar_reveal = reveal;
            cx.notify();
        }
    }

    /// Places the Operating-System Window's own control at the trailing end of the Tab bar.
    pub(crate) fn set_window_close_handler(&mut self, handler: spaceterm_ui::WindowCloseHandler) {
        self.window_close_handler = Some(handler);
    }

    pub(crate) fn set_trailing_accessory(
        &mut self,
        accessory: Option<gpui::AnyView>,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .trailing_accessory
            .as_ref()
            .map(gpui::AnyView::entity_id);
        if current != accessory.as_ref().map(gpui::AnyView::entity_id) {
            self.trailing_accessory = accessory;
            cx.notify();
        }
    }

    pub(crate) fn set_parent_focus_blocker(
        &mut self,
        blocker: Option<TerminalFocusBlocker>,
        cx: &mut Context<Self>,
    ) {
        if self.parent_focus_blocker == blocker {
            return;
        }
        self.parent_focus_blocker = blocker;
        self.sync_terminal_focus_blocker(cx);
        cx.notify();
    }

    fn terminal_focus_blocker(&self) -> Option<TerminalFocusBlocker> {
        TerminalFocusCoordinator::tab_blocker(TabFocusOwners {
            parent: self.parent_focus_blocker,
            window_drag: self.window_drag_status.is_active(),
            selector: self.tab_selector_pressed.is_some(),
            context_menu: self.tab_menu.is_some(),
        })
    }

    fn sync_terminal_focus_blocker(&self, cx: &mut Context<Self>) {
        let blocker = self.terminal_focus_blocker();
        let active_tab_id = self.tabs.active_tab_id();
        let display = self.visible_tab_ids();
        let workspace = cx.entity_id();
        for (index, (tab_id, view)) in self.tabs.iter().enumerate() {
            let order = display
                .iter()
                .position(|id| *id == tab_id)
                .unwrap_or(display.len() + index);
            let active = self.active && tab_id == active_tab_id;
            let identity = view.entity_id();
            view.update(cx, |view, cx| {
                view.readiness_navigation(workspace, identity, order, active, cx);
                view.set_focus_branch(active, blocker, cx);
            });
        }
    }

    fn handle_operating_system_window_drag_event(
        &mut self,
        event: WindowDragRegionEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> WindowDragRegionResponse {
        match event {
            WindowDragRegionEvent::InteractionStarted { .. } => {
                let response = match self
                    .operating_system_window_drag_platform
                    .interaction_started(window)
                {
                    Ok(WindowMoveStart::Started) => {
                        WindowDragRegionResponse::OperatingSystemWindowMoveStarted
                    }
                    Ok(WindowMoveStart::AwaitMovement) => WindowDragRegionResponse::Continue,
                    Err(error) => {
                        Self::report_operating_system_window_drag_error("begin", error);
                        WindowDragRegionResponse::Continue
                    }
                };
                self.sync_terminal_focus_blocker(cx);
                response
            }
            WindowDragRegionEvent::MoveRequested { .. } => {
                match self
                    .operating_system_window_drag_platform
                    .start_window_move(window)
                {
                    Ok(()) => WindowDragRegionResponse::OperatingSystemWindowMoveStarted,
                    Err(error) => {
                        Self::report_operating_system_window_drag_error("start", error);
                        WindowDragRegionResponse::Continue
                    }
                }
            }
            WindowDragRegionEvent::SecondaryActivationRequested { position } => {
                self.operating_system_window_drag_platform
                    .show_window_menu(window, position);
                WindowDragRegionResponse::Continue
            }
            WindowDragRegionEvent::MiddleActivationRequested => {
                window.titlebar_middle_click();
                WindowDragRegionResponse::Continue
            }
            WindowDragRegionEvent::DoubleActivationRequested => {
                window.titlebar_double_click();
                WindowDragRegionResponse::Continue
            }
            WindowDragRegionEvent::InteractionFinished { .. } => {
                self.operating_system_window_drag_platform
                    .interaction_finished();
                self.sync_terminal_focus_blocker(cx);
                WindowDragRegionResponse::Continue
            }
        }
    }

    fn report_operating_system_window_drag_error(
        operation: &str,
        error: OperatingSystemWindowDragError,
    ) {
        eprintln!("failed to {operation} Operating-System Window drag: {error}");
    }

    fn begin_tab_selector(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        self.tab_selector_pressed = Some(tab_id);
        self.sync_terminal_focus_blocker(cx);
        cx.notify();
    }

    fn cancel_tab_selector(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self.tab_selector_pressed != Some(tab_id) {
            return;
        }
        self.tab_selector_pressed = None;
        self.sync_terminal_focus_blocker(cx);
        cx.notify();
    }

    fn commit_tab_selector(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_selector_pressed != Some(tab_id) {
            return;
        }
        let _ = self.activate_tab(tab_id, window, cx);
        self.tab_selector_pressed = None;
        self.sync_terminal_focus_blocker(cx);
        cx.notify();
    }

    /// Lifts a Tab to move it in the Tab bar. A press that becomes a drag never selects its Tab.
    /// The starting motion already marks a slot, so a drag released on its first move still lands.
    fn begin_tab_drag(
        &mut self,
        tab_id: TabId,
        cursor_offset: gpui::Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> DragPreview {
        self.cancel_tab_selector(tab_id, cx);
        let size = self.tab_position(tab_id).and_then(|position| {
            self.tab_reorder
                .item_size(&self.tab_bar_scroll_handle, tab_id, position)
        });
        let session = DragSession::begin(cx, |manager: &mut Self, _, cx| {
            let ids = manager.visible_tab_ids();
            manager
                .tab_reorder
                .cancel(&manager.tab_bar_scroll_handle, &ids, cx);
            cx.notify();
        });
        let ids = self.visible_tab_ids();
        self.tab_reorder.begin(
            tab_id,
            None,
            session,
            &self.tab_bar_scroll_handle,
            &ids,
            window,
            cx,
        );
        self.drag_tab_to(tab_id, window.mouse_position(), cx);
        cx.notify();
        let manager = cx.entity().downgrade();
        let placement = manager.clone();
        DragPreview::new(window, cx, move |window, cx| {
            manager
                .upgrade()
                .map(|owner| {
                    owner
                        .read(cx)
                        .render_lifted_tab(tab_id, size, manager.clone(), window, cx)
                })
                .unwrap_or_else(|| div().into_any_element())
        })
        .placed(cursor_offset, move |cx| {
            placement
                .upgrade()
                .and_then(|owner| owner.read(cx).tab_reorder.lifted_origin())
        })
    }

    fn drag_tab_to(&mut self, tab_id: TabId, pointer: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        if self.tab_reorder.dragged() != Some(tab_id) {
            return;
        }
        let ids = self.visible_tab_ids();
        if self
            .tab_reorder
            .track(&self.tab_bar_scroll_handle, &ids, pointer, cx)
        {
            cx.notify();
        }
    }

    /// Lands the dragged Tab in the slot marked at the release point.
    fn finish_tab_drag(&mut self, pointer: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tab_reorder.dragged() else {
            return;
        };
        self.drag_tab_to(tab_id, pointer, cx);
        let ids = self.visible_tab_ids();
        let landing = self
            .tab_reorder
            .finish(&self.tab_bar_scroll_handle, &ids, cx);
        if let Some((tab_id, position)) = landing {
            self.move_tab(tab_id, position, cx);
        }
        cx.notify();
    }

    #[cfg(test)]
    pub(super) fn dragged_tab(&self) -> Option<TabId> {
        self.tab_reorder.dragged()
    }

    #[cfg(test)]
    pub(super) fn tab_ids(&self) -> Vec<TabId> {
        self.tabs.iter().map(|(tab_id, _)| tab_id).collect()
    }

    fn move_tab(&mut self, tab_id: TabId, position: usize, cx: &mut Context<Self>) {
        let moved = self.tabs.move_tab(tab_id, position);
        Self::present_tab_move(moved, cx);
    }

    fn step_tab(&mut self, tab_id: TabId, step: TabStep, cx: &mut Context<Self>) {
        let moved = self.tabs.step_tab(tab_id, step);
        Self::present_tab_move(moved, cx);
    }

    fn present_tab_move(moved: Result<bool, TabError>, cx: &mut Context<Self>) {
        match moved {
            Ok(true) => {
                cx.emit(TabManagerEvent::PresentationChanged);
                cx.notify();
            }
            Ok(false) => {}
            Err(error) => Self::report_tab_error("move", error),
        }
    }

    /// A Tab's position in the tab strip, which shows the Active Worktree's Tabs.
    fn tab_position(&self, tab_id: TabId) -> Option<usize> {
        self.visible_tabs()
            .position(|(candidate, _)| candidate == tab_id)
    }

    fn visible_tabs(&self) -> impl Iterator<Item = (TabId, &Entity<TabView>)> {
        self.tabs.tabs_in(self.tabs.active_scope())
    }

    fn visible_tab_ids(&self) -> Vec<TabId> {
        self.visible_tabs().map(|(tab_id, _)| tab_id).collect()
    }

    pub(crate) fn directory_recovery(
        &self,
        cx: &App,
    ) -> Option<Entity<super::server_pane::DirectoryRecovery>> {
        self.tabs.active_tab().read(cx).directory_recovery(cx)
    }

    fn visible_len(&self) -> usize {
        self.visible_tabs().count()
    }

    #[cfg(test)]
    pub(crate) fn active_tab_view(&self) -> Entity<TabView> {
        self.tabs.active_tab().clone()
    }

    #[cfg(test)]
    pub(crate) fn focused_terminal_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.tabs
            .active_tab()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }

    #[cfg(test)]
    pub(crate) fn focused_terminal_has_input_focus(&self, window: &Window, cx: &App) -> bool {
        self.tabs
            .active_tab()
            .read(cx)
            .focused_terminal_has_input_focus(window, cx)
    }

    fn scroll_active_tab_into_view(&self) {
        if let Some(index) = self.tab_position(self.tabs.active_tab_id()) {
            self.tab_bar_scroll_handle.scroll_to_item(index);
        }
    }

    pub(crate) fn create_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A shared Workspace's Tabs belong to the server. Creating one asks the server, and the
        // Tab appears here with the published membership rather than appearing locally first.
        if self.shared.is_some() {
            self.request_shared_tab(window, cx);
            return;
        }
        self.tab_selector_pressed = None;
        self.sync_terminal_focus_blocker(cx);
        let scope = self.tabs.active_scope();
        self.create_tab_in_scope(scope, window, cx);
    }

    fn create_tab_in_scope(
        &mut self,
        scope: Option<WorktreeId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous_tab = self.tabs.active_tab().clone();
        let session_factory = self.session_factory.clone();
        let pane_construction = self.pane_construction.clone();
        let result = self.tabs.create_tab_in(scope, |tab_id| {
            Self::create_tab_view(tab_id, session_factory, pane_construction, window, cx)
        });
        let tab_id = match result {
            Ok(tab_id) => tab_id,
            Err(error) => {
                Self::report_tab_error("create", error);
                return;
            }
        };
        let Some(view) = self.tabs.tab(tab_id).cloned() else {
            unreachable!("a newly created Tab must remain owned by its collection")
        };

        previous_tab.update(cx, |view, cx| view.deactivate(cx));
        if self.active {
            view.update(cx, |view, cx| view.activate(window, cx));
        } else {
            view.update(cx, |view, cx| view.deactivate(cx));
        }
        self.sync_terminal_focus_blocker(cx);
        self.scroll_active_tab_into_view();
        cx.emit(TabManagerEvent::PresentationChanged);
        cx.notify();
    }

    /// The Active Tab's Worktree.
    pub(crate) fn active_worktree(&self) -> Option<WorktreeId> {
        self.tabs.active_scope()
    }

    /// Whether any Tab sits outside every Worktree, as the Root Tab does after its Workspace
    /// leaves a repository.
    pub(crate) fn has_unscoped_tabs(&self) -> bool {
        self.tabs.tabs_in(None).next().is_some()
    }

    /// Shows the most recent Tab outside every Worktree when a Worktree's Tabs are shown.
    /// Returns whether the Active Tab changed. Showing the Tab moves focus to it only when
    /// `focus` is set.
    pub(crate) fn show_unscoped_tabs(
        &mut self,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.tabs.active_scope().is_none() {
            return false;
        }
        let Some(tab_id) = self
            .tabs
            .most_recent_tab_in(None)
            .or_else(|| self.tabs.tabs_in(None).map(|(tab_id, _)| tab_id).next())
        else {
            return false;
        };
        self.activate_tab_focusing(tab_id, focus, window, cx)
    }

    /// The Worktree a Tab belongs to.
    pub(crate) fn tab_worktree(&self, tab_id: TabId) -> Option<WorktreeId> {
        self.tabs.scope_of(tab_id).ok().flatten()
    }

    /// Whether any Tab is not among `tab_ids`, so closing them leaves a Tab.
    pub(crate) fn has_tabs_besides(&self, tab_ids: &[TabId]) -> bool {
        self.tabs
            .iter()
            .any(|(tab_id, _)| !tab_ids.contains(&tab_id))
    }

    /// The Tabs of a Worktree, in Tab order.
    pub(crate) fn worktree_tab_ids(&self, id: WorktreeId) -> Vec<TabId> {
        self.tabs
            .tabs_in(Some(id))
            .map(|(tab_id, _)| tab_id)
            .collect()
    }

    /// The number of Tabs each Worktree holds. Tabs outside any Worktree are not counted.
    pub(crate) fn worktree_tab_counts(&self) -> std::collections::BTreeMap<WorktreeId, usize> {
        let mut counts = std::collections::BTreeMap::new();
        for (tab_id, _) in self.tabs.iter() {
            if let Ok(Some(scope)) = self.tabs.scope_of(tab_id) {
                *counts.entry(scope).or_insert(0) += 1;
            }
        }
        counts
    }

    fn activate_tab(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.activate_tab_focusing(tab_id, true, window, cx)
    }

    fn activate_tab_focusing(
        &mut self,
        tab_id: TabId,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(next_tab) = self.tabs.tab(tab_id).cloned() else {
            eprintln!("cannot activate unknown Tab {tab_id}");
            return false;
        };
        let previous_tab_id = self.tabs.active_tab_id();
        let previous_tab = self.tabs.active_tab().clone();
        if let Err(error) = self.tabs.activate_tab(tab_id) {
            Self::report_tab_error("activate", error);
            return false;
        }

        if previous_tab_id != tab_id {
            previous_tab.update(cx, |view, cx| view.deactivate(cx));
        }
        let blocker = self.terminal_focus_blocker();
        next_tab.update(cx, |view, cx| {
            view.set_focus_branch(self.active, blocker, cx);
        });
        if self.active && focus {
            next_tab.update(cx, |view, cx| view.activate(window, cx));
        } else if self.active {
            next_tab.update(cx, |view, cx| view.activate_without_focus(cx));
        } else {
            next_tab.update(cx, |view, cx| view.deactivate(cx));
        }
        self.sync_terminal_focus_blocker(cx);
        self.scroll_active_tab_into_view();
        cx.emit(TabManagerEvent::PresentationChanged);
        cx.notify();
        true
    }

    fn activate_tab_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let tab_id = self.visible_tabs().nth(index).map(|(tab_id, _)| tab_id);
        if let Some(tab_id) = tab_id {
            self.activate_tab(tab_id, window, cx);
        }
    }

    fn step_active_tab(&mut self, step: TabStep, window: &mut Window, cx: &mut Context<Self>) {
        let tab_id = self.tabs.neighbor_of_active_tab(step);
        if tab_id != self.tabs.active_tab_id() {
            self.activate_tab(tab_id, window, cx);
        }
    }

    fn move_active_tab(&mut self, step: TabStep, cx: &mut Context<Self>) {
        if self.tabs.move_active_tab(step) {
            self.scroll_active_tab_into_view();
            cx.emit(TabManagerEvent::PresentationChanged);
            cx.notify();
        }
    }

    fn close_tab(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab_activating(tab_id, true, window, cx);
    }

    /// Closes a Tab. `activate_next` opens the Tab that takes its place for the person, which
    /// selects that Tab's Focused Pane; a close the server published passes `false`, because
    /// nobody asked for a Pane there.
    fn close_tab_activating(
        &mut self,
        tab_id: TabId,
        activate_next: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.close_workspace_requested {
            return;
        }

        let was_active = self.tabs.active_tab_id() == tab_id;
        match self.tabs.close_tab(tab_id) {
            Ok(CloseTabOutcome::TabClosed {
                closed_tab_id,
                payload,
                active_tab_id,
            }) => {
                debug_assert_eq!(closed_tab_id, tab_id);
                let tab_focused = payload.read(cx).tab_focus().contains_focused(window, cx);
                payload.update(cx, |view, cx| view.close_all(cx));
                if was_active {
                    let active_tab = self.tabs.active_tab().clone();
                    if self.active && activate_next {
                        active_tab.update(cx, |view, cx| view.activate(window, cx));
                    } else if self.active {
                        active_tab.update(cx, |view, cx| view.activate_without_focus(cx));
                    } else {
                        active_tab.update(cx, |view, cx| view.deactivate(cx));
                    }
                } else if tab_focused {
                    // Focus on the closed Tab or its Close Tab button leaves with it, so the
                    // Active Tab takes it.
                    self.focus(window, cx);
                }
                self.tab_selector_pressed = None;
                if self.tab_menu == Some(tab_id) {
                    self.tab_menu = None;
                }
                self.forget_shared_tab(tab_id);
                self.sync_terminal_focus_blocker(cx);
                debug_assert_eq!(active_tab_id, self.tabs.active_tab_id());
                // Closing another Tab leaves the bar where it was scrolled.
                if was_active {
                    self.scroll_active_tab_into_view();
                }
                cx.emit(TabManagerEvent::PresentationChanged);
                cx.notify();
            }
            Ok(CloseTabOutcome::CloseWorkspace { final_tab_id }) => {
                self.close_workspace_requested = true;
                self.tab_selector_pressed = None;
                cx.emit(TabManagerEvent::FinalTabCloseRequested { final_tab_id });
            }
            Err(error) => {
                self.tab_selector_pressed = None;
                self.sync_terminal_focus_blocker(cx);
                Self::report_tab_error("close", error);
            }
        }
    }

    fn request_close_tab(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        // A shared Tab's close goes up to the Workspace Window, which asks the server after Close
        // Confirmation. Nothing here closes it locally.
        if self.close_workspace_requested || self.tabs.tab(tab_id).is_none() {
            return;
        }
        cx.emit(TabManagerEvent::CloseTabRequested { tab_id });
    }

    /// Admits a context menu for a Tab without activating it. A Tab mid-drag or a Workspace
    /// already closing offers no menu.
    fn request_tab_menu(&mut self, tab_id: TabId, cx: &mut Context<Self>) -> bool {
        if self.close_workspace_requested
            || self.tab_reorder.dragged().is_some()
            || self.tabs.tab(tab_id).is_none()
        {
            return false;
        }
        self.cancel_tab_selector(tab_id, cx);
        true
    }

    fn handle_tab_menu_lifecycle(
        &mut self,
        tab_id: TabId,
        event: MenuLifecycleEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            MenuLifecycleEvent::Opened => self.tab_menu = Some(tab_id),
            MenuLifecycleEvent::Closed(_) if self.tab_menu == Some(tab_id) => self.tab_menu = None,
            MenuLifecycleEvent::Closed(_) => return,
        }
        self.sync_terminal_focus_blocker(cx);
        cx.notify();
    }

    fn perform_tab_menu_command(
        &mut self,
        tab_id: TabId,
        command: TabMenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            TabMenuCommand::NewTab => self.create_tab(window, cx),
            TabMenuCommand::SplitRight => self.split_tab(tab_id, SplitAxis::Horizontal, window, cx),
            TabMenuCommand::SplitDown => self.split_tab(tab_id, SplitAxis::Vertical, window, cx),
            TabMenuCommand::MoveLeft => self.step_tab(tab_id, TabStep::Previous, cx),
            TabMenuCommand::MoveRight => self.step_tab(tab_id, TabStep::Next, cx),
            TabMenuCommand::Close => self.request_close_tab(tab_id, cx),
        }
    }

    /// Splits a Tab's Focused Pane. The Tab becomes active first so the new Pane is visible.
    fn split_tab(
        &mut self,
        tab_id: TabId,
        axis: SplitAxis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A shared split needs no measured Pane, because the server decides the membership and
        // this client inserts the Pane beside its source in its own layout.
        if self.shared.is_some() {
            if self.activate_tab(tab_id, window, cx) {
                let view = self.tabs.active_tab().clone();
                view.update(cx, |view, cx| view.split_focused(axis, window, cx));
            }
            return;
        }
        let was_active = self.tabs.active_tab_id() == tab_id;
        if !self.activate_tab(tab_id, window, cx) {
            return;
        }
        let view = self.tabs.active_tab().clone();
        if was_active {
            view.update(cx, |view, cx| view.split_focused(axis, window, cx));
        } else {
            // A Tab that was inactive keeps Pane bounds from its last paint.
            view.update(cx, |view, cx| view.split_focused_after_layout(axis, cx));
        }
    }

    fn on_close_tab(&mut self, _: &CloseTab, _: &mut Window, cx: &mut Context<Self>) {
        self.request_close_tab(self.tabs.active_tab_id(), cx);
    }

    fn on_create_tab(&mut self, _: &CreateTab, window: &mut Window, cx: &mut Context<Self>) {
        self.create_tab(window, cx);
    }

    fn on_activate_tab_1(&mut self, _: &ActivateTab1, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_tab_at(0, window, cx);
    }

    fn on_activate_tab_2(&mut self, _: &ActivateTab2, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_tab_at(1, window, cx);
    }

    fn on_activate_tab_3(&mut self, _: &ActivateTab3, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_tab_at(2, window, cx);
    }

    fn on_activate_tab_4(&mut self, _: &ActivateTab4, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_tab_at(3, window, cx);
    }

    fn on_activate_tab_5(&mut self, _: &ActivateTab5, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_tab_at(4, window, cx);
    }

    fn on_activate_tab_6(&mut self, _: &ActivateTab6, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_tab_at(5, window, cx);
    }

    fn on_activate_tab_7(&mut self, _: &ActivateTab7, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_tab_at(6, window, cx);
    }

    fn on_activate_tab_8(&mut self, _: &ActivateTab8, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_tab_at(7, window, cx);
    }

    fn on_activate_tab_9(&mut self, _: &ActivateTab9, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_tab_at(8, window, cx);
    }

    fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.step_active_tab(TabStep::Next, window, cx);
    }

    fn on_previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        self.step_active_tab(TabStep::Previous, window, cx);
    }

    fn on_move_tab_right(&mut self, _: &MoveTabRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_active_tab(TabStep::Next, cx);
    }

    fn on_move_tab_left(&mut self, _: &MoveTabLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_active_tab(TabStep::Previous, cx);
    }

    /// The copy of a dragged Tab that follows the pointer, at the size its Tab was painted.
    fn render_lifted_tab(
        &self,
        tab_id: TabId,
        size: Option<gpui::Size<Pixels>>,
        manager: gpui::WeakEntity<Self>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let Some(view) = self.tabs.tab(tab_id) else {
            return div().into_any_element();
        };
        let appearance = super::appearance::chrome(cx);
        let presentation = TabChromePresentation::resolve(
            window.is_window_active(),
            appearance.capabilities.show_borders,
            &appearance.colors,
        );
        self.render_tab_item(
            tab_id,
            view.read(cx).tab_identity(),
            tab_id == self.tabs.active_tab_id(),
            TabItemRole::Lifted,
            TabHover {
                level: 1.0,
                under_pointer: true,
            },
            &presentation,
            self.shared_strip_controls(),
            manager,
            appearance,
            window,
            cx,
        )
        .h(size.map_or_else(
            || {
                super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx)
                    .top_chrome_height(appearance.top_height())
            },
            |size| size.height,
        ))
        .when_some(size, |item, size| item.w(size.width))
        .into_any_element()
    }

    /// One Tab in the Tab bar, or the lifted copy that follows the pointer during a drag.
    /// The copy claims no pointer input because it lies over every drop target.
    #[expect(
        clippy::too_many_arguments,
        reason = "one Tab render step needs its identity, role, hover, presentation, owner, appearance, and host geometry"
    )]
    fn render_tab_item(
        &self,
        tab_id: TabId,
        identity: TabIdentity,
        active: bool,
        role: TabItemRole,
        hover: TabHover,
        presentation: &TabChromePresentation,
        shared: Option<SharedStripControls>,
        manager: gpui::WeakEntity<Self>,
        appearance: &super::appearance::ChromeAppearance,
        window: &Window,
        cx: &App,
    ) -> gpui::Stateful<gpui::Div> {
        let lifted = role == TabItemRole::Lifted;
        let press_manager = manager.clone();
        let release_manager = manager.clone();
        let click_manager = manager.clone();
        let drag_manager = manager.clone();
        let owner = manager.entity_id();
        let close_manager = manager;
        let hover_active = appearance.active && presentation.window_active;
        let under_pointer = hover_active && hover.under_pointer;
        let hover = if hover_active { hover.level } else { 0.0 };
        let chip = presentation.tab_chip(active, appearance, cx);
        // Content follows the chip's paired hover paint, preserving selected identity.
        let foreground = presentation
            .tab_foreground(active)
            .fade(presentation.tab_hover_foreground(active), f64::from(hover));
        let control_style = presentation.close_control_style(
            active,
            under_pointer,
            &appearance.colors,
            appearance.materials,
        );
        // Native activation can precede dispatch of an accepts-first-mouse event. Retain whether
        // this action was visible in the pre-activation frame so a hidden inactive-Tab control
        // cannot close its Tab through a stale hitbox. The selected Tab remains available.
        // A shared Tab's close asks the server, so its control waits, and says why, while this
        // client cannot ask.
        let shared_close_reason = shared
            .filter(|shared| !shared.edits)
            .map(|shared| shared.edits_reason);
        let close_action_available = (active || appearance.active) && shared_close_reason.is_none();
        let status = presentation.tab_status(active, under_pointer, &appearance.colors);
        let status_host = presentation.tab_surface(active, under_pointer);
        let close_button_size = cx
            .global::<ButtonTheme>()
            .icon_button_size(ButtonSize::Compact);
        let close_clearance = close_button_size + appearance.spacing(TAB_TRAILING_GAP);
        let close_icon_size = appearance.icons.metrics(IconRole::Control).glyph_size;
        #[cfg(test)]
        let rendered_active_close_icon = Rc::clone(&self.rendered_active_close_icon);
        #[cfg(test)]
        let rendered_inactive_close_icon = Rc::clone(&self.rendered_inactive_close_icon);
        let text_style = appearance.typography.style(TextRole::Navigation);
        let mut identity = identity;
        identity.glyph =
            super::tab_view::drawable_reported_glyph(identity.glyph.as_ref(), |glyph| {
                super::terminal_status::reported_glyph_is_drawable(
                    glyph,
                    &text_style.font,
                    text_style.size,
                    window,
                )
            });
        let element_name = if lifted { "tab-preview" } else { "tab-item" };
        // The lifted copy shows its whole lift. A Tab in the bar slides aside, shows a faded face
        // where a release would land it, and shows what remains of its lift as it settles.
        let motion = if lifted {
            ItemMotion {
                lift: 1.0,
                ..ItemMotion::default()
            }
        } else {
            self.tab_reorder.motion(tab_id, cx)
        };
        let chip_selector = format!("{element_name}-{}-chip", tab_id.get());
        let chip = if motion.lifted {
            chip.landing(foreground)
        } else {
            chip.displaced(motion.selection_offset)
        };
        let chip = if motion.lift > 0.0 {
            chip.render_lifted(
                chip_selector,
                hover.max(motion.lift),
                lift_shadow(appearance, motion.lift),
            )
        } else {
            chip.render(chip_selector, hover)
        };
        let close_control = if lifted {
            // The close control's glyph at rest, without a control that could take the pointer.
            div()
                .size(close_button_size)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(
                    IconName::X,
                    close_icon_size,
                    gpui_color(presentation.close_icon(active, under_pointer)),
                ))
                .into_any_element()
        } else {
            IconButton::new(
                ("tab-close-button", tab_id.get()),
                "Close Tab",
                move |foreground| {
                    #[cfg(test)]
                    if active {
                        rendered_active_close_icon.set(foreground);
                    } else {
                        rendered_inactive_close_icon.set(foreground);
                    }
                    Icon::new(IconName::X, close_icon_size, foreground).into_any_element()
                },
            )
            .variant(ButtonVariant::Ghost)
            .disabled(!close_action_available)
            .when_some(shared_close_reason, |button, reason| {
                button.accessibility_description(reason)
            })
            .accept_first_mouse(active)
            .contextual_style(
                control_style,
                gpui_color(title_bar_control_focus_ring(appearance)),
            )
            .size(ButtonSize::Compact)
            .preserve_ancestor_hover()
            .debug_selector(format!("tab-close-button-{}", tab_id.get()))
            .tooltip(
                Tooltip::new(("tab-close-tooltip", tab_id.get()), "Close Tab")
                    .debug_selector(format!("tab-close-tooltip-{}", tab_id.get())),
            )
            .on_activate(move |_, _, cx| {
                if !close_action_available {
                    return;
                }
                let _ = close_manager.update(cx, |manager, cx| {
                    manager.request_close_tab(tab_id, cx);
                });
            })
            .into_any_element()
        };
        let face = div()
            .id((element_name, tab_id.get()))
            .debug_selector(move || {
                format!(
                    "{element_name}-{}-{}",
                    tab_id.get(),
                    if active { "active" } else { "inactive" }
                )
            })
            .relative()
            .left(motion.offset.x)
            .top(motion.offset.y)
            .opacity(motion.opacity())
            .h_full()
            .flex_none()
            .w(appearance.spacing(TAB_ITEM_WIDTH))
            .min_w(appearance.spacing(TAB_ITEM_MINIMUM_WIDTH))
            .max_w(appearance.spacing(TAB_ITEM_MAXIMUM_WIDTH))
            .pl(appearance.spacing(TAB_ITEM_LEFT_PADDING))
            .pr(appearance.spacing(TAB_ITEM_RIGHT_PADDING))
            .flex()
            .items_center()
            .chrome_text(text_style)
            .text_color(gpui_color(foreground))
            .child(chip)
            .child(render_tab_identity(
                tab_id,
                identity,
                status,
                status_host,
                close_clearance,
                appearance,
            ))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(appearance.spacing(TAB_ITEM_RIGHT_PADDING))
                    .flex()
                    .items_center()
                    // An inactive Tab shows its close control only under the pointer, where its
                    // lifted copy always is.
                    .when(!active, |button| button.opacity(hover))
                    .child(close_control),
            );
        if lifted {
            return face;
        }
        face.cursor_pointer()
            .block_mouse_except_scroll()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                let _ = press_manager.update(cx, |manager, cx| {
                    manager.begin_tab_selector(tab_id, cx);
                });
                cx.stop_propagation();
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                let _ = release_manager.update(cx, |manager, cx| {
                    manager.cancel_tab_selector(tab_id, cx);
                });
            })
            .on_click(move |_, window, cx| {
                let _ = click_manager.update(cx, |manager, cx| {
                    manager.commit_tab_selector(tab_id, window, cx);
                });
                cx.stop_propagation();
            })
            .on_drag(
                DraggedTab { tab_id, owner },
                move |_, cursor_offset, window, cx| {
                    let preview = drag_manager
                        .update(cx, |manager, cx| {
                            manager.begin_tab_drag(tab_id, cursor_offset, window, cx)
                        })
                        .unwrap_or_else(|_| DragPreview::empty());
                    cx.new(|_| preview)
                },
            )
    }

    /// Each Tab's hover, in Tab bar order.
    fn tab_hovers(&self, window: &mut Window, cx: &mut App) -> Vec<(HoverFade, TabHover)> {
        self.visible_tabs()
            .map(|(tab_id, _)| {
                let fade = HoverFade::new(("tab-hover", tab_id.get()), window, cx);
                let hover = TabHover {
                    level: fade.level(window, cx),
                    under_pointer: fade.is_hovered(cx),
                };
                (fade, hover)
            })
            .collect()
    }

    fn render_tab_bar(
        &self,
        presentation: &TabChromePresentation,
        hovers: Vec<(HoverFade, TabHover)>,
        manager: gpui::WeakEntity<Self>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let appearance = super::appearance::chrome(cx);
        let active_tab_id = self.tabs.active_tab_id();
        let shared_strip = self.shared_strip_controls();
        let desktop_presentation = crate::desktop_profile::DesktopPresentation::get(cx);
        let background = presentation.background;
        let create_icon_size = appearance.icons.metrics(IconRole::Chrome).glyph_size;
        // The strip pulls back the chip inset so the first Tab sits one frame space from the
        // identity.
        let leading_alignment =
            super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx)
                .chip_strip_leading_offset();
        let reorder_manager = manager.clone();
        let owner = manager.entity_id();
        let mut items = div()
            .id("tab-items")
            .role(accesskit::Role::TabList)
            .aria_label("Tabs")
            .debug_selector(|| "tab-items".to_owned())
            .h_full()
            .min_w_0()
            .flex()
            .flex_row()
            .overflow_x_scroll()
            .track_scroll(&self.tab_bar_scroll_handle)
            .on_drag_move::<DraggedTab>(move |event, _, cx| {
                let dragged = event.drag(cx);
                if dragged.owner != owner {
                    return;
                }
                let tab_id = dragged.tab_id;
                let pointer = event.event.position;
                let _ = reorder_manager.update(cx, |manager, cx| {
                    manager.drag_tab_to(tab_id, pointer, cx);
                });
            });
        let last_index = self.visible_len() - 1;
        // The strip's own ends carry the outer marks, so they stay put while Tabs slide or scroll.
        // Each hides only while the active Tab covers its end of the visible strip.
        let tab_count = self.visible_len();
        let active_position = self.tab_position(active_tab_id);
        let strip_marks = self
            .visible_tabs()
            .next()
            .filter(|_| self.sidebar_reveal > 0.0)
            .map(|(tab_id, _)| (TabBoundary::StripStart(tab_id), StripEdge::Start))
            .into_iter()
            .chain(
                self.visible_tabs()
                    .last()
                    .map(|(tab_id, _)| (TabBoundary::StripEnd(tab_id), StripEdge::End)),
            )
            .map(|(boundary, edge)| {
                let items = self.tab_bar_scroll_handle.clone();
                PaintWhen {
                    visible: Box::new(move || {
                        !active_position.is_some_and(|active| {
                            active_tab_covers(edge, &items, active, tab_count)
                        })
                    }),
                    child: render_tab_separator(boundary, presentation, appearance),
                }
            })
            .collect::<Vec<_>>();
        let mut previous_inactive_tab = None;
        for (index, ((tab_id, view), (fade, hover))) in self.visible_tabs().zip(hovers).enumerate()
        {
            let active = tab_id == active_tab_id;
            let separator = previous_inactive_tab
                .filter(|_| !active)
                .map(|leading_tab_id| {
                    render_tab_separator(
                        TabBoundary::Between(leading_tab_id, tab_id),
                        presentation,
                        appearance,
                    )
                });
            previous_inactive_tab = (!active).then_some(tab_id);
            let identity = view.read(cx).tab_identity();
            let tab_focus = view.read(cx).tab_focus().clone();
            let title = if identity.activity.is_empty() {
                "Terminal"
            } else {
                identity.activity.as_ref()
            };
            let accessibility_name = if identity.place.is_empty() {
                format!("Tab actions for {title}")
            } else {
                format!("Tab actions for {title} in {}", identity.place)
            };
            let press_manager = manager.clone();
            let mut target = ContextMenuTarget::new(accesskit::Role::Tab, title.to_owned())
                .selected(active)
                .on_press(move |window, cx| {
                    let _ = press_manager.update(cx, |manager, cx| {
                        manager.activate_tab(tab_id, window, cx);
                    });
                });
            if !identity.place.is_empty() {
                target = target.description(identity.place.clone());
            }
            let face = self
                .render_tab_item(
                    tab_id,
                    identity,
                    active,
                    TabItemRole::InBar,
                    hover,
                    presentation,
                    shared_strip,
                    manager.clone(),
                    appearance,
                    window,
                    cx,
                )
                .children(separator)
                .child(fade.tracker());
            let open_manager = manager.clone();
            let lifecycle_manager = manager.clone();
            let activate_manager = manager.clone();
            let grow_manager = manager.clone();
            // The slot grows a new Tab in and closes the gap a closed one leaves. As a flex row, it
            // leaves the Tab its own width while it is narrower.
            let slot = self.tab_reorder.motion(tab_id, cx).frame_slot(
                gpui::Axis::Horizontal,
                div().flex().flex_none().h_full(),
                move |size, cx| {
                    let _ = grow_manager.update(cx, |manager, _| {
                        manager.tab_reorder.measure(tab_id, size);
                    });
                },
            );
            items = items.child(
                slot.child(
                    ContextMenu::new(
                        ("tab-context-menu", tab_id.get()),
                        accessibility_name,
                        face,
                        tab_menu_entries(
                            active,
                            index == 0,
                            index == last_index,
                            shared_strip,
                            desktop_presentation,
                        ),
                    )
                    .size(MenuSize::Wide)
                    .fill_parent_height()
                    .preserve_trigger_cursor()
                    .keyboard_trigger(&tab_focus)
                    .target(target)
                    .debug_selector(format!("tab-context-menu-{}", tab_id.get()))
                    .on_open_request(move |_, _, cx| {
                        open_manager
                            .update(cx, |manager, cx| manager.request_tab_menu(tab_id, cx))
                            .unwrap_or(false)
                    })
                    .on_lifecycle(move |event, cx| {
                        let manager = lifecycle_manager.clone();
                        let event = *event;
                        // Menu lifecycle delivery can occur while its Window is borrowed.
                        cx.defer(move |cx| {
                            let _ = manager.update(cx, |manager, cx| {
                                manager.handle_tab_menu_lifecycle(tab_id, event, cx);
                            });
                        });
                    })
                    .on_activate(move |activation, window, cx| {
                        let command = *activation.action();
                        let _ = activate_manager.update(cx, |manager, cx| {
                            manager.perform_tab_menu_command(tab_id, command, window, cx);
                        });
                    }),
                ),
            );
        }
        let release_manager = manager.clone();
        let release_observer = drag_release_observer(move |window, cx| {
            let pointer = window.mouse_position();
            let _ = release_manager.update(cx, |manager, cx| {
                manager.finish_tab_drag(pointer, cx);
            });
        });

        let drag_manager = manager.clone();
        let create_manager = manager.clone();
        // A shared Workspace's New Tab asks the server, so it waits while this client is offline
        // or an earlier edit is unanswered.
        let create_tab_available = shared_strip.is_none_or(|shared| shared.edits);
        let create_tab_unavailable = shared_strip
            .filter(|shared| !shared.edits)
            .map(|shared| shared.edits_reason);
        #[cfg(test)]
        let rendered_create_tab_icon = Rc::clone(&self.rendered_create_tab_icon);
        // The window paints its own edge over the top of the bar, so the Tabs and the create
        // control centre in the band beneath it, on the line the top-left chrome shares.
        let content = div()
            .relative()
            .size_full()
            .min_w_0()
            .pt(
                super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx)
                    .window_edge(),
            )
            .flex()
            .flex_row()
            .items_center()
            .child(
                div()
                    .relative()
                    .h_full()
                    .min_w_0()
                    .ml(leading_alignment)
                    .flex()
                    .flex_row()
                    .child(items)
                    .children(strip_marks),
            )
            .child(release_observer)
            .child(
                div()
                    .debug_selector(|| "create-tab-area".to_owned())
                    .h_full()
                    .w(appearance.top_height())
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        IconButton::new("create-tab-button", "Create Tab", move |foreground| {
                            #[cfg(test)]
                            rendered_create_tab_icon.set(foreground);
                            Icon::custom(CustomIconName::Plus, create_icon_size, foreground)
                                .into_any_element()
                        })
                        .variant(ButtonVariant::Ghost)
                        .contextual_style(
                            presentation
                                .bar_control_style(&appearance.colors, appearance.materials),
                            gpui_color(title_bar_control_focus_ring(appearance)),
                        )
                        .size(ButtonSize::Regular)
                        .disabled(!create_tab_available)
                        .when_some(create_tab_unavailable, |button, reason| {
                            button.accessibility_description(reason)
                        })
                        .debug_selector("create-tab-button")
                        .tooltip(
                            Tooltip::new("create-tab-tooltip", "Create Tab")
                                .shortcut(
                                    create_tab_shortcut(
                                        crate::desktop_profile::DesktopPresentation::get(cx),
                                    )
                                    .unwrap_or_default(),
                                )
                                .debug_selector("create-tab-tooltip"),
                        )
                        .on_activate(move |_, window, cx| {
                            let _ = create_manager.update(cx, |manager, cx| {
                                manager.create_tab(window, cx);
                            });
                        }),
                    ),
            )
            .child(div().flex_1().min_w_0())
            .when_some(self.trailing_accessory.clone(), |content, accessory| {
                content.child(
                    div()
                        .debug_selector(|| "tab-bar-trailing-accessory".to_owned())
                        .flex_none()
                        .h_full()
                        .flex()
                        .items_center()
                        .child(accessory),
                )
            })
            .when_some(
                self.window_close_handler.clone().filter(|_| {
                    matches!(
                        window.window_decorations(),
                        gpui::Decorations::Client { .. }
                    ) && !window.is_fullscreen()
                }),
                |content, close| {
                    content.child(
                        div()
                            .flex_none()
                            .pr(px(spaceterm_ui::DesktopWindowStyle::current(cx)
                                .control_metrics()
                                .edge_margin))
                            .child(spaceterm_ui::ControlHost::TitleBar.mount(
                                spaceterm_ui::ClientWindowControls::new(close).surface_color(
                                    gpui_color(appearance.colors.title_bar_background),
                                ),
                            )),
                    )
                },
            );

        let drag_region = WindowDragRegion::new(
            "tab-bar-drag-region",
            "Move Operating-System Window from Tab chrome",
            content,
        )
        .middle_activation(matches!(
            window.window_decorations(),
            gpui::Decorations::Client { .. }
        ))
        .status(self.window_drag_status.clone())
        .pointer_insets(Edges {
            left: super::control_theme::resize_handle::spacious_target_half_thickness(cx),
            ..Edges::default()
        })
        .debug_selector("tab-bar-drag-region")
        .on_event(move |event, window, cx| {
            let event = *event;
            drag_manager
                .update(cx, |manager, cx| {
                    manager.handle_operating_system_window_drag_event(event, window, cx)
                })
                .unwrap_or_default()
        });

        div()
            .id("tab-bar")
            .debug_selector(|| "tab-bar".to_owned())
            .relative()
            .h(
                super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx)
                    .top_chrome_height(appearance.top_height()),
            )
            .min_w_0()
            .flex_1()
            .flex_shrink_0()
            .bg(gpui_color(
                appearance.surface(crate::appearance::SurfaceRole::Base, background),
            ))
            .child(drag_region)
            .into_any_element()
    }
}

impl Render for TabManager {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        debug_assert!(self.tabs.len() > 0);
        self.sync_terminal_focus_blocker(cx);
        let ids = self.visible_tab_ids();
        self.tab_reorder
            .refresh(&self.tab_bar_scroll_handle, &ids, window, cx);
        self.tab_reorder.reveal_while_entering(
            self.tabs.active_tab_id(),
            &self.tab_bar_scroll_handle,
            &ids,
        );
        self.tab_reorder
            .follow_selection(Some(self.tabs.active_tab_id()), window, cx);
        #[cfg(test)]
        {
            self.rendered_window_active = window.is_window_active();
        }
        let manager = cx.entity().downgrade();
        let active_tab = self.tabs.active_tab().clone();
        let edge = self.stage_leading_edge(window, cx);
        // The active Tab keeps its resting width, and its Panes stretch across the stage.
        active_tab.update(cx, |view, cx| view.present_stage_stretch(edge.offset, cx));
        let hovers = self.tab_hovers(window, cx);
        let appearance = super::appearance::chrome(cx);
        let presentation = TabChromePresentation::resolve(
            window.is_window_active(),
            appearance.capabilities.show_borders,
            &appearance.colors,
        );
        let tab_bar = self.render_tab_bar(&presentation, hovers, manager.clone(), window, cx);
        let frame = super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx);
        let stage_surface = super::workspace_frame::base_surface(&appearance.colors);
        let stage_color =
            gpui_color(appearance.surface(crate::appearance::SurfaceRole::Base, stage_surface));

        div()
            .id("tab-manager")
            .debug_selector(|| "tab-manager".to_owned())
            .key_context(TERMINAL_KEY_CONTEXT)
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font(
                appearance
                    .typography
                    .style(TextRole::Navigation)
                    .font
                    .clone(),
            )
            .on_action(cx.listener(Self::on_create_tab))
            .on_action(cx.listener(Self::on_activate_tab_1))
            .on_action(cx.listener(Self::on_activate_tab_2))
            .on_action(cx.listener(Self::on_activate_tab_3))
            .on_action(cx.listener(Self::on_activate_tab_4))
            .on_action(cx.listener(Self::on_activate_tab_5))
            .on_action(cx.listener(Self::on_activate_tab_6))
            .on_action(cx.listener(Self::on_activate_tab_7))
            .on_action(cx.listener(Self::on_activate_tab_8))
            .on_action(cx.listener(Self::on_activate_tab_9))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_previous_tab))
            .on_action(cx.listener(Self::on_move_tab_right))
            .on_action(cx.listener(Self::on_move_tab_left))
            .on_action(cx.listener(Self::on_close_tab))
            .child(
                div()
                    .h(frame.top_chrome_height(appearance.top_height()))
                    .w_full()
                    .flex_shrink_0()
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .id("tab-manager-top-spacer")
                            .debug_selector(|| "tab-manager-top-spacer".to_owned())
                            .w(self.top_chrome_width)
                            .h_full()
                            .flex_shrink_0(),
                    )
                    .child(tab_bar),
            )
            .child(
                div()
                    .on_children_prepainted({
                        let readiness = self.readiness.clone();
                        let active = self.active;
                        move |children, _, _| {
                            // The stage's available rectangle wins over an active Tab's minimum
                            // size, which can overflow it. Hidden targets validate their own minimum.
                            if active && let Some(stage) = children.last() {
                                readiness.measured(stage.size);
                            }
                        }
                    })
                    .id("tab-manager-content")
                    .debug_selector(|| "tab-manager-content".to_owned())
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .relative()
                    .when(edge.offset == px(0.0), |content| content.overflow_hidden())
                    .ml(edge.rest_margin)
                    // No top edge: the chrome above carries that space. No leading edge beside a
                    // sidebar: the sidebar chip's trailing margin is already that gap. A moving
                    // stage keeps these widths, so its layout holds, and paints its edges itself.
                    .border_color(if edge.offset == px(0.0) {
                        stage_color
                    } else {
                        gpui::rgba(0x0000_0000)
                    })
                    .border_b(frame.window_edge_inset())
                    .border_r(frame.window_edge_inset())
                    .border_l(edge.rest_inset)
                    .child(
                        div()
                            .debug_selector(move || {
                                format!(
                                    "tab-manager-stage-surface-{:08x}",
                                    stage_surface.rgba_hex()
                                )
                            })
                            .absolute()
                            .inset_0(),
                    )
                    // The stage as this frame draws it, from its leading edge to the window's
                    // edges. Like the stage at rest, it paints only its edges, so translucent
                    // Panes show the same backdrop while they move. Each corner is painted once.
                    .when(edge.offset != px(0.0), |content| {
                        let window_edge_inset = frame.window_edge_inset();
                        content.child(
                            div()
                                .debug_selector(|| "tab-manager-moving-stage".to_owned())
                                .absolute()
                                .top_0()
                                .left(edge.margin - edge.rest_margin - edge.rest_inset)
                                .right(-window_edge_inset)
                                .bottom(-window_edge_inset)
                                .child(
                                    div()
                                        .debug_selector(|| {
                                            "tab-manager-moving-stage-leading-edge".to_owned()
                                        })
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .bottom(window_edge_inset)
                                        .w(edge.inset)
                                        .bg(stage_color),
                                )
                                .child(
                                    div()
                                        .debug_selector(|| {
                                            "tab-manager-moving-stage-bottom-edge".to_owned()
                                        })
                                        .absolute()
                                        .left_0()
                                        .right_0()
                                        .bottom_0()
                                        .h(window_edge_inset)
                                        .bg(stage_color),
                                )
                                .child(
                                    div()
                                        .debug_selector(|| {
                                            "tab-manager-moving-stage-trailing-edge".to_owned()
                                        })
                                        .absolute()
                                        .top_0()
                                        .right_0()
                                        .bottom(window_edge_inset)
                                        .w(window_edge_inset)
                                        .bg(stage_color),
                                ),
                        )
                    })
                    .child(StageOffset {
                        offset: edge.offset,
                        child: div().size_full().child(active_tab).into_any_element(),
                    }),
            )
    }
}

impl EventEmitter<TabManagerEvent> for TabManager {}

/// A boundary of the Tab strip that a quiet mark can separate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TabBoundary {
    /// Between the strip's leading neighbour and its first Tab.
    StripStart(TabId),
    /// Between two neighbouring Tabs, in strip order.
    Between(TabId, TabId),
    /// Between the strip's last Tab and the Create Tab control.
    StripEnd(TabId),
}

impl TabBoundary {
    fn debug_selector(self) -> String {
        match self {
            Self::StripStart(tab_id) => format!("tab-separator-start-{}", tab_id.get()),
            Self::Between(leading, trailing) => {
                format!("tab-separator-{}-{}", leading.get(), trailing.get())
            }
            Self::StripEnd(tab_id) => format!("tab-separator-{}-end", tab_id.get()),
        }
    }
}

/// One end of the visible Tab strip.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StripEdge {
    Start,
    End,
}

/// Whether the active Tab, as scrolled this frame, covers one end of the visible Tab strip. Before
/// the strip is laid out, the first Tab covers its start and the last Tab its end.
fn active_tab_covers(edge: StripEdge, items: &ScrollHandle, active: usize, len: usize) -> bool {
    let Some(span) = items.bounds_for_item(active) else {
        return match edge {
            StripEdge::Start => active == 0,
            StripEdge::End => active + 1 == len,
        };
    };
    let offset = items.offset().x;
    let viewport = items.bounds();
    let edge = match edge {
        StripEdge::Start => viewport.left() + px(0.5),
        StripEdge::End => viewport.right() - px(0.5),
    };
    span.left() + offset < edge && edge < span.right() + offset
}

/// Where the stage's leading edge rests and where one frame draws it.
#[derive(Clone, Copy, Debug)]
struct StageLeadingEdge {
    /// The leading margin of the stage at rest.
    rest_margin: Pixels,
    /// The leading inset of the stage at rest.
    rest_inset: Pixels,
    /// The leading margin this frame draws.
    margin: Pixels,
    /// The leading inset this frame draws.
    inset: Pixels,
    /// How far this frame draws the stage's content leading of where it rests, in whole device
    /// pixels.
    offset: Pixels,
}

/// Draws its child `offset` leading of where it is laid out, clipped to the space from there to
/// its own trailing edge. The child's layout, and so the size of everything in it, does not
/// depend on the offset.
struct StageOffset {
    offset: Pixels,
    child: AnyElement,
}

impl StageOffset {
    fn content_mask(&self, bounds: Bounds<Pixels>) -> gpui::ContentMask<Pixels> {
        gpui::ContentMask {
            bounds: Bounds::from_corners(
                gpui::point(bounds.left() - self.offset, bounds.top()),
                bounds.bottom_right(),
            ),
        }
    }
}

impl IntoElement for StageOffset {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for StageOffset {
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
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let child = self.child.request_layout(window, cx);
        let style = gpui::Style {
            size: gpui::size(relative(1.0).into(), relative(1.0).into()),
            ..gpui::Style::default()
        };
        (window.request_layout(style, [child], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let mask = self.content_mask(bounds);
        window.with_content_mask(Some(mask), |window| {
            window.with_element_offset(gpui::point(-self.offset, px(0.0)), |window| {
                self.child.prepaint(window, cx)
            })
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let mask = self.content_mask(bounds);
        window.with_content_mask(Some(mask), |window| self.child.paint(window, cx));
    }
}

/// Paints its child only when `visible` holds at prepaint, after the elements before it have
/// prepainted. A scroll container settles its offset there, so the decision sees this frame's
/// scroll rather than the one render saw.
struct PaintWhen {
    visible: Box<dyn Fn() -> bool>,
    child: AnyElement,
}

impl IntoElement for PaintWhen {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PaintWhen {
    type RequestLayoutState = ();
    type PrepaintState = bool;

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
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let visible = (self.visible)();
        if visible {
            self.child.prepaint(window, cx);
        }
        visible
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        visible: &mut bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        if *visible {
            self.child.paint(window, cx);
        }
    }
}

/// The quiet mark at one boundary of the Tab strip whose Tabs are all inactive.
/// It paints inside its host, the trailing Tab or the strip itself, on whole points so layout
/// rounding cannot move it. It uses its own
/// `tab_separator` role because `border` is too close to the bar to show on a mark this short.
fn render_tab_separator(
    boundary: TabBoundary,
    presentation: &TabChromePresentation,
    appearance: &super::appearance::ChromeAppearance,
) -> AnyElement {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .map(|mark| match boundary {
            TabBoundary::StripStart(_) | TabBoundary::Between(..) => mark.left_0(),
            TabBoundary::StripEnd(_) => mark.right_0(),
        })
        .w(px(TAB_SEPARATOR_WIDTH))
        .flex()
        .items_center()
        .child(
            div()
                .debug_selector(move || boundary.debug_selector())
                .w_full()
                .h(appearance.spacing(TAB_SEPARATOR_LENGTH))
                .bg(gpui_color(
                    appearance
                        .materials
                        .edge(presentation.background, presentation.tab_separator),
                )),
        )
        .into_any_element()
}

/// One Tab's identity: `<status glyph> <activity> · <place>`. The status glyph never narrows.
fn render_tab_identity(
    tab_id: TabId,
    identity: TabIdentity,
    status: crate::appearance::StatusPaint,
    status_host: crate::appearance::Color,
    close_clearance: Pixels,
    appearance: &super::appearance::ChromeAppearance,
) -> AnyElement {
    let origin_location = if identity.remote { "remote" } else { "local" };
    let icon_size = appearance.icons.metrics(IconRole::Status).glyph_size;
    let has_place = !identity.place.is_empty();
    let mut words = div()
        .id(("tab-identity", tab_id.get()))
        .debug_selector(move || format!("tab-identity-{}", tab_id.get()))
        .flex_1()
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .overflow_hidden()
        .child(
            div()
                .debug_selector(move || format!("tab-activity-{}", tab_id.get()))
                .flex_shrink_0()
                .min_w_0()
                .when(has_place, |activity| {
                    activity.max_w(relative(TAB_ACTIVITY_MAXIMUM_SHARE))
                })
                .truncate()
                .child(if identity.activity.is_empty() {
                    gpui::SharedString::from("Terminal")
                } else {
                    identity.activity.clone()
                }),
        );
    if has_place {
        words = words
            .child(
                div()
                    .flex_shrink_0()
                    .mx(appearance.spacing(TAB_TRAILING_GAP))
                    .child("·"),
            )
            .child(
                div()
                    .debug_selector(move || format!("tab-place-{}", tab_id.get()))
                    .flex_shrink(1.0)
                    .min_w_0()
                    .truncate()
                    .child(identity.place.clone()),
            );
    }

    div()
        .id(("tab-title", tab_id.get()))
        .debug_selector(move || format!("tab-title-{}", tab_id.get()))
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .pr(close_clearance)
        .flex()
        .flex_row()
        .items_center()
        // Status belongs to the Tab rather than to any one segment of its name, so the terminal glyph
        // that leads the row carries it and survives every narrowing.
        .child(
            div()
                .debug_selector(move || format!("tab-origin-{}-{origin_location}", tab_id.get()))
                .flex_shrink_0()
                .mr(appearance.spacing(TAB_ORIGIN_GAP))
                .flex()
                .items_center()
                .child(
                    StatusGlyph {
                        icon: IconName::Terminal,
                        reported: identity.glyph,
                        size: icon_size,
                        progress: identity.progress,
                        attention: identity.attention,
                        id: ("tab-status", tab_id.get()).into(),
                        selector_prefix: format!("tab-status-{}", tab_id.get()),
                        colors: StatusColors {
                            host: gpui_color(status_host),
                            attention: gpui_color(status.attention),
                            busy: gpui_color(status.busy),
                            error: gpui_color(status.error),
                            paused: gpui_color(status.paused),
                        },
                        differentiate_without_color: appearance
                            .capabilities
                            .differentiate_without_color,
                    }
                    .render(),
                ),
        )
        .child(words)
        .into_any_element()
}

/// Every Tab command carries a symbol, so all labels share the icon column. A Shortcut acts on
/// the Active Tab, so only the Active Tab's menu advertises one.
/// Shared server Workspaces: how this Tab strip presents one server Workspace's Tabs and submits
/// their edits.
impl TabManager {
    /// Presents one server Workspace in this Tab strip. The Tab this strip already owns becomes
    /// the server's Root Tab, and every later Tab arrives with the published membership.
    pub(crate) fn adopt_shared(
        &mut self,
        mut shared: super::shared_workspaces::SharedTabs,
        root_tab: spaceterm_server::hierarchy::Id,
        root_pane: spaceterm_server::hierarchy::Id,
        root_state: std::sync::Arc<dyn crate::ui::server_pane::ServerPaneConnection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root_tab_id = self.tabs.root_tab_id();
        shared.register(root_tab_id, root_tab);
        let panes = super::shared_workspaces::SharedPanes::new(
            shared.edits(),
            shared.sessions().clone(),
            shared.workspace(),
            root_tab,
        );
        let view = self.tabs.root_tab().clone();
        view.update(cx, |view, cx| {
            view.adopt_shared(panes, root_pane, root_state, window, cx);
        });
        self.shared = Some(shared);
        cx.notify();
    }

    pub(crate) fn shared_arrangement(
        &self,
        cx: &App,
    ) -> Option<spaceterm_server::client_view::WorkspaceView> {
        use spaceterm_server::client_view::{TabView, WorkspaceView};
        let shared = self.shared.as_ref()?;
        let tabs = self
            .tabs
            .iter()
            .map(|(id, view)| {
                Some(TabView {
                    tab: shared.server_tab(id)?,
                    layout: view.read(cx).shared_layout()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(WorkspaceView {
            workspace: shared.workspace(),
            tabs,
        })
    }

    pub(crate) fn restore_shared_arrangement(
        &mut self,
        saved: &spaceterm_server::client_view::WorkspaceView,
        workspace: &spaceterm_server::hierarchy::Workspace,
        cx: &mut Context<Self>,
    ) {
        let saved = saved.reconcile(workspace);
        for (position, tab) in saved.tabs.iter().enumerate() {
            let Some(id) = self
                .shared
                .as_ref()
                .and_then(|shared| shared.local_tab(tab.tab))
            else {
                continue;
            };
            let _ = self.tabs.move_tab(id, position);
            if let Some(view) = self.tabs.tab(id) {
                view.update(cx, |view, cx| view.restore_shared_layout(&tab.layout, cx));
            }
        }
        cx.notify();
    }

    /// Adds the Tabs the server published that this strip does not present yet, then reconciles
    /// each Tab's Panes. A new Tab joins at the end of the strip and never takes the Active Tab.
    pub(crate) fn reconcile_shared(
        &mut self,
        workspace: &spaceterm_server::hierarchy::Workspace,
        removal: super::shared_workspaces::SharedRemoval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let close_keyboard = std::mem::take(&mut self.shared_close_keyboard);
        let Some(shared) = self.shared.as_mut() else {
            return false;
        };
        let applied_aliases = shared.aliases_to_apply();
        let edits = shared.edits();
        shared.canonicalize();
        // Every Tab the server published joins before any Tab it removed leaves, so one poll
        // carrying both never closes this Workspace through membership it has not applied yet.
        let mut changed = false;
        let restoring = self
            .parked_shared_tabs
            .keys()
            .copied()
            .filter(|id| {
                self.shared
                    .as_ref()
                    .and_then(|shared| shared.server_tab(*id))
                    .is_some_and(|server| workspace.tabs.iter().any(|tab| tab.id == server))
            })
            .collect::<Vec<_>>();
        for id in restoring {
            if let Some(parked) = self.parked_shared_tabs.remove(&id) {
                self.tabs.restore_parked_tab(parked);
                changed = true;
            }
        }
        for tab in &workspace.tabs {
            let tab_id = match self
                .shared
                .as_ref()
                .and_then(|shared| shared.local_tab(tab.id))
            {
                Some(tab_id) => tab_id,
                None => match self.create_shared_tab(&workspace.tabs, tab, window, cx) {
                    Some(tab_id) => {
                        changed = true;
                        tab_id
                    }
                    None => continue,
                },
            };
            let Some(view) = self.tabs.tab(tab_id).cloned() else {
                continue;
            };
            changed |= view.update(cx, |view, cx| view.reconcile_shared(tab, window, cx));
        }
        changed |= self.remove_absent_shared_tabs(workspace, removal, close_keyboard, window, cx);
        let retired = self
            .parked_shared_tabs
            .keys()
            .copied()
            .filter(|id| {
                self.shared
                    .as_ref()
                    .is_none_or(|shared| !shared.optimistically_closing(*id))
            })
            .collect::<Vec<_>>();
        for id in retired {
            if let Some(parked) = self.parked_shared_tabs.remove(&id) {
                self.tabs
                    .discard_parked_tab(parked)
                    .update(cx, |view, cx| view.close_all(cx));
                self.forget_shared_tab(id);
            }
        }
        // Every Tab view, including a parked one, must translate its containing identities
        // before their aliases retire. Pane aliases retire individually when binding succeeds.
        for parked in self.parked_shared_tabs.values() {
            parked
                .payload()
                .update(cx, |view, _| view.canonicalize_shared_ids());
        }
        for id in applied_aliases {
            edits.retire_alias(id);
        }
        if changed {
            cx.emit(TabManagerEvent::PresentationChanged);
            cx.notify();
        }
        changed
    }

    /// Records, right after this client submits a shared close, whether the keyboard rested in
    /// this strip when the person asked for it, read before any Close Confirmation took it.
    ///
    /// Only a close that ends every Pane of the Active Tab counts, so another client's removal of
    /// the Active Tab never borrows a close of some other Tab. The reconciliation that removes
    /// that Tab, whether the server's answer arrived yet or not, opens the Tab that takes its
    /// place, as a local close does. Every reconciliation clears the record, so a later
    /// publication or the close's own settlement selects nothing.
    pub(crate) fn prepare_shared_close_keyboard(&mut self, held_keyboard: bool, cx: &App) {
        let closes_active =
            held_keyboard && self.active && self.tabs.active_tab().read(cx).shared_closing();
        self.shared_close_keyboard = closes_active.then(|| self.tabs.active_tab_id());
    }

    /// Removes the Tabs the server no longer has.
    ///
    /// The server already removed them, so this needs no confirmation here. An inactive Tab's
    /// Panes may never have connected, so membership alone decides this rather than a Terminal
    /// Session reporting its own end.
    ///
    /// `close_keyboard` is [`Self::prepare_shared_close_keyboard`]'s record. When this
    /// reconciliation removes that Tab, the Tab that takes its place opens once, after every
    /// removal, so no Pane of a Tab closing alongside it is selected.
    fn remove_absent_shared_tabs(
        &mut self,
        workspace: &spaceterm_server::hierarchy::Workspace,
        removal: super::shared_workspaces::SharedRemoval,
        close_keyboard: Option<TabId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(shared) = self.shared.as_ref() else {
            return false;
        };
        let absent: Vec<TabId> = self
            .tabs
            .iter()
            .map(|(tab_id, _)| tab_id)
            .filter(|&tab_id| {
                shared
                    .server_tab(tab_id)
                    .is_some_and(|server| !workspace.tabs.iter().any(|tab| tab.id == server))
            })
            .collect();
        let opens_successor = close_keyboard.is_some_and(|closed| absent.contains(&closed));
        let mut removed = false;
        for tab_id in absent {
            let optimistic = self
                .shared
                .as_ref()
                .is_some_and(|shared| shared.optimistically_closing(tab_id));
            let own = removal == super::shared_workspaces::SharedRemoval::OwnClose;
            let held_keyboard = self
                .tabs
                .tab(tab_id)
                .is_some_and(|view| view.read(cx).holds_keyboard(window, cx));
            if optimistic {
                let was_active = self.tabs.active_tab_id() == tab_id;
                if let Ok(Some(parked)) = self.tabs.park_tab(tab_id) {
                    parked.payload().update(cx, |view, cx| view.deactivate(cx));
                    self.parked_shared_tabs.insert(tab_id, parked);
                    if was_active && self.active {
                        self.tabs
                            .active_tab()
                            .update(cx, |view, cx| view.activate_without_focus(cx));
                        if held_keyboard && !opens_successor {
                            self.focus(window, cx);
                        }
                    }
                    self.sync_terminal_focus_blocker(cx);
                }
            } else {
                // The server's answer can arrive before this reconciliation, so the closed Tab
                // leaves here without being parked; its successor still opens once below.
                let activate_next = own && held_keyboard && !opens_successor;
                self.close_tab_activating(tab_id, activate_next, window, cx);
            }
            // Another client's close asks the server for no Pane here, but the keyboard that was
            // in the Tab it removed still needs a place, so it rests in the Active Tab's Focused
            // Pane without selecting it. Typing anywhere else stays where it is.
            if !own && held_keyboard && !self.close_workspace_requested {
                self.focus(window, cx);
            }
            removed = true;
        }
        // The person's close opens the Tab that takes its place, which selects its Focused Pane
        // and so starts a retained shell there.
        if opens_successor && self.active && !self.close_workspace_requested {
            self.tabs
                .active_tab()
                .update(cx, |view, cx| view.open_focused_pane(window, cx));
        }
        removed
    }

    /// Activates the Tab or focuses the Pane one accepted creation of this client's own made.
    pub(crate) fn open_accepted_shared(
        &mut self,
        target: spaceterm_server::hierarchy::Id,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shared) = self.shared.as_ref() else {
            return;
        };
        if let Some(tab_id) = shared.local_tab(target) {
            self.activate_tab(tab_id, window, cx);
            return;
        }
        // A split's new Pane belongs to a Tab this strip already presents.
        let holder = self
            .tabs
            .iter()
            .find(|(_, view)| view.read(cx).holds_shared_pane(target))
            .map(|(tab_id, view)| (tab_id, view.clone()));
        if let Some((tab_id, view)) = holder {
            if !self.activate_tab(tab_id, window, cx) {
                return;
            }
            view.update(cx, |view, cx| {
                view.focus_shared_pane_explicitly(target, window, cx);
            });
        }
    }

    /// Presents one server Tab this client has not seen before. It appends to the strip and keeps
    /// the Tab a person is working in active, because another client's creation is membership and
    /// not a view change here.
    ///
    /// The Tab joins the Worktree of a presented Tab the server places in the same Worktree, so
    /// it appears in that Worktree's strip before the server's Git facts name the Worktree.
    fn create_shared_tab(
        &mut self,
        workspace_tabs: &[spaceterm_server::hierarchy::Tab],
        tab: &spaceterm_server::hierarchy::Tab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<TabId> {
        let shared = self.shared.as_ref()?;
        let workspace = shared.workspace();
        let edits = shared.edits();
        let sessions = shared.sessions().clone();
        let (session_factory, state) = sessions.open(tab.root_pane)?;
        let pane_construction = sessions.panes().clone();
        let active_tab_id = self.tabs.active_tab_id();
        let scope = self.presented_worktree(workspace_tabs, tab);
        let created = self.tabs.create_tab_in(scope, |tab_id| {
            Self::create_tab_view(tab_id, session_factory, pane_construction, window, cx)
        });
        let tab_id = match created {
            Ok(tab_id) => tab_id,
            Err(error) => {
                Self::report_tab_error("present a shared", error);
                return None;
            }
        };
        // Creating a Tab activates it. The Active Tab is this client's own view state, so it
        // returns to where a person left it. Restoring it is not a Pane selection, so it asks the
        // server for nothing: a publication must never start a retained shell.
        if let Err(error) = self.tabs.activate_tab(active_tab_id) {
            Self::report_tab_error("restore the Active", error);
        }
        let view = self.tabs.tab(tab_id)?.clone();
        let panes = super::shared_workspaces::SharedPanes::new(edits, sessions, workspace, tab.id);
        view.update(cx, |view, cx| {
            view.deactivate(cx);
            view.adopt_shared(panes, tab.root_pane, state, window, cx);
        });
        self.shared.as_mut()?.register(tab_id, tab.id);
        self.sync_terminal_focus_blocker(cx);
        Some(tab_id)
    }

    /// The Worktree this strip presents for Tabs the server places with `tab`, read from the
    /// presented Tabs among `workspace_tabs` that share its Worktree.
    fn presented_worktree(
        &self,
        workspace_tabs: &[spaceterm_server::hierarchy::Tab],
        tab: &spaceterm_server::hierarchy::Tab,
    ) -> Option<WorktreeId> {
        let shared = self.shared.as_ref()?;
        tab.worktree.as_ref()?;
        workspace_tabs
            .iter()
            .filter(|other| {
                other.id != tab.id
                    && other.worktree == tab.worktree
                    && other.worktree_repository == tab.worktree_repository
            })
            .find_map(|other| {
                self.tabs
                    .scope_of(shared.local_tab(other.id)?)
                    .ok()
                    .flatten()
            })
    }

    /// Asks the server for another Tab in this shared Workspace.
    fn request_shared_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use super::shared_workspaces::SharedSubmission;

        let Some(shared) = self.shared.as_ref() else {
            return;
        };
        self.tab_selector_pressed = None;
        let active = self.tabs.active_tab().read(cx);
        let Some(source) = active.shared_server_pane(active.focused_pane_id()) else {
            return;
        };
        let edits = shared.edits();
        if let SharedSubmission::Refused(reason) = shared.create_tab(source) {
            let detail = format!(
                "SpaceTerm did not create a Tab in this shared Workspace. {}",
                reason.explanation()
            );
            let _ = Alert::new(
                ModalId::new("shared-tab-unavailable"),
                "Shared change unavailable",
                "Shared Change Unavailable",
                detail,
                vec![ModalAction::new(
                    (),
                    "OK",
                    ModalActionRole::Cancel,
                    "shared-tab-unavailable-ok",
                )],
            )
            .intent(AlertIntent::Warning)
            .present(window, cx, |_, _| {});
        } else {
            let presentation = edits.take_presentation();
            let workspace = self
                .shared
                .as_ref()
                .and_then(|shared| shared.presentation_workspace());
            let target = presentation.map(|event| edits.canonical(event.target));
            if let Some(workspace) = workspace {
                self.reconcile_shared(
                    &workspace,
                    super::shared_workspaces::SharedRemoval::Published,
                    window,
                    cx,
                );
            }
            if let Some(target) = target {
                self.open_accepted_shared(target, window, cx);
            }
        }
        cx.notify();
    }

    fn forget_shared_tab(&mut self, tab_id: TabId) {
        if let Some(shared) = self.shared.as_mut() {
            shared.forget(tab_id);
        }
    }

    /// Whether the keyboard rests in one of this strip's Tabs or their Panes.
    pub(crate) fn holds_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.tabs
            .iter()
            .any(|(_, view)| view.read(cx).holds_keyboard(window, cx))
    }

    /// Tags each shared Tab with the Worktree the server says it belongs to. The server owns
    /// membership, so this client only mirrors it; the Tab strip and the Tab Commands then act on
    /// the Active Worktree's Tabs, as they do in a local Workspace.
    ///
    /// `scope` answers `None` for a Tab the server has not published yet. Such a Tab keeps the
    /// Worktree it was presented in, so a pending new Tab never leaves its Worktree's strip.
    pub(crate) fn apply_shared_worktrees(
        &mut self,
        scope: &dyn Fn(spaceterm_server::hierarchy::Id) -> Option<Option<WorktreeId>>,
        cx: &mut Context<Self>,
    ) {
        let Some(shared) = self.shared.as_ref() else {
            return;
        };
        let scopes: Vec<_> = self
            .tabs
            .iter()
            .filter_map(|(tab_id, _)| Some((tab_id, scope(shared.server_tab(tab_id)?)?)))
            .collect();
        // Only the scope changes. A shared Tab's launch policy never takes the server's root,
        // which names no directory this client may validate or start in.
        let mut changed = false;
        for (tab_id, scope) in scopes {
            if self.tabs.scope_of(tab_id) == Ok(scope) {
                continue;
            }
            if let Err(error) = self.tabs.set_tab_scope(tab_id, scope) {
                Self::report_tab_error("move", error);
                continue;
            }
            changed = true;
        }
        if changed {
            self.scroll_active_tab_into_view();
            cx.emit(TabManagerEvent::PresentationChanged);
            cx.notify();
        }
    }

    /// Shows a shared Worktree's most recent Tab, and reports whether it has one. It never opens a
    /// Tab: the server opens none in a chosen Worktree yet.
    pub(crate) fn show_shared_worktree(
        &mut self,
        id: WorktreeId,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab_id) = self.tabs.most_recent_tab_in(Some(id)) else {
            return false;
        };
        self.activate_tab_focusing(tab_id, focus, window, cx);
        true
    }

    /// The server Pane a Worktree row describes: the Focused Pane of the Worktree's most recently
    /// active Tab.
    pub(crate) fn shared_worktree_pane(
        &self,
        id: WorktreeId,
        cx: &App,
    ) -> Option<spaceterm_server::hierarchy::Id> {
        let view = self
            .tabs
            .tab(self.tabs.most_recent_tab_in(Some(id))?)?
            .read(cx);
        view.shared_server_pane(view.focused_pane_id())
    }

    /// Presents the Repository Status the server read for every shared Pane in this strip,
    /// including Tabs outside the Active Worktree.
    pub(crate) fn present_shared_repositories(
        &mut self,
        view: &dyn Fn(spaceterm_server::hierarchy::Id) -> crate::repository_status::RepositoryView,
        cx: &mut Context<Self>,
    ) {
        if self.shared.is_none() {
            return;
        }
        let tabs: Vec<_> = self.tabs.iter().map(|(_, tab)| tab.clone()).collect();
        for tab in tabs {
            tab.update(cx, |tab, cx| tab.present_shared_repositories(view, cx));
        }
    }

    /// Presents the Terminal Metadata the server published for every shared Pane in this strip,
    /// including Tabs outside the Active Worktree and Tabs this client never opened.
    pub(crate) fn present_shared_metadata<'p>(
        &mut self,
        published: &dyn Fn(
            spaceterm_server::hierarchy::Id,
        ) -> Option<(
            crate::ui::terminal_pane::ServerSessionRun,
            &'p spaceterm_server::pane::metadata::Snapshot,
        )>,
        cx: &mut Context<Self>,
    ) {
        if self.shared.is_none() {
            return;
        }
        let tabs: Vec<_> = self.tabs.iter().map(|(_, tab)| tab.clone()).collect();
        for tab in tabs {
            tab.update(cx, |tab, cx| tab.present_shared_metadata(published, cx));
        }
    }

    /// The server identity of one shared Tab this strip presents.
    pub(crate) fn shared_server_tab(
        &self,
        tab_id: TabId,
    ) -> Option<spaceterm_server::hierarchy::Id> {
        self.shared.as_ref()?.server_tab(tab_id)
    }

    /// The server identity of one shared Pane in one of this strip's Tabs.
    pub(crate) fn shared_server_pane(
        &self,
        tab_id: TabId,
        pane_id: PaneId,
        cx: &App,
    ) -> Option<spaceterm_server::hierarchy::Id> {
        self.tabs.tab(tab_id)?.read(cx).shared_server_pane(pane_id)
    }

    pub(crate) fn shared_pane_closes_directly(
        &self,
        server: spaceterm_server::hierarchy::Id,
        live_session: bool,
        cx: &App,
    ) -> bool {
        self.tabs
            .iter()
            .find_map(|(_, view)| {
                view.read(cx)
                    .shared_pane_closes_directly(server, live_session, cx)
            })
            .unwrap_or(false)
    }

    /// Delivers the Active Tab's Focused Pane exit, as its Terminal Session does when the server
    /// ends it.
    #[cfg(test)]
    pub(crate) fn exit_shared_pane_for_test(&mut self, cx: &mut Context<Self>) {
        self.tabs
            .active_tab()
            .clone()
            .update(cx, |view, cx| view.exit_focused_pane_for_test(cx));
    }

    fn shared_strip_controls(&self) -> Option<SharedStripControls> {
        let editability = self.shared.as_ref()?.editability();
        Some(SharedStripControls {
            edits: editability.available(),
            edits_reason: editability.explanation(),
        })
    }
}

fn tab_menu_entries(
    active: bool,
    first: bool,
    last: bool,
    shared: Option<SharedStripControls>,
    presentation: &crate::desktop_profile::DesktopPresentation,
) -> Vec<MenuEntry<TabMenuCommand>> {
    // A shared Workspace's creations and Tab close wait for the server. Each entry says so in
    // place of its label, so no menu offers a step that does nothing.
    let shared_edits = shared.is_none_or(|shared| shared.edits);
    let child_launch_available = shared_edits;
    let waiting = shared
        .filter(|shared| !shared.edits)
        .map(|shared| shared.edits_reason);
    let with_shortcut =
        |entry: MenuEntry<TabMenuCommand>, action: &dyn gpui::Action| match presentation
            .shortcut(action)
            .filter(|_| active)
        {
            Some(shortcut) => entry.shortcut(shortcut),
            None => entry,
        };
    vec![
        with_shortcut(
            MenuEntry::action(
                match waiting {
                    Some(_) => "New Tab Needs the Server",
                    None => "New Tab",
                },
                TabMenuCommand::NewTab,
            ),
            &CreateTab,
        )
        .icon(|foreground, size| {
            Icon::new(IconName::SquarePlus, size, foreground).into_any_element()
        })
        .disabled(!child_launch_available)
        .debug_selector("tab-menu-row-new-tab"),
        MenuEntry::separator(),
        with_shortcut(
            MenuEntry::action("Split Right", TabMenuCommand::SplitRight),
            &SplitRight,
        )
        .disabled(!child_launch_available)
        .icon(|foreground, size| Icon::new(IconName::Columns2, size, foreground).into_any_element())
        .debug_selector("tab-menu-row-split-right"),
        with_shortcut(
            MenuEntry::action("Split Down", TabMenuCommand::SplitDown),
            &SplitDown,
        )
        .disabled(!child_launch_available)
        .icon(|foreground, size| Icon::new(IconName::Rows2, size, foreground).into_any_element())
        .debug_selector("tab-menu-row-split-down"),
        MenuEntry::separator(),
        with_shortcut(
            MenuEntry::action("Move Tab Left", TabMenuCommand::MoveLeft),
            &MoveTabLeft,
        )
        .disabled(first)
        .icon(|foreground, size| {
            Icon::new(IconName::ArrowLeft, size, foreground).into_any_element()
        })
        .debug_selector("tab-menu-row-move-left"),
        with_shortcut(
            MenuEntry::action("Move Tab Right", TabMenuCommand::MoveRight),
            &MoveTabRight,
        )
        .disabled(last)
        .icon(|foreground, size| {
            Icon::new(IconName::ArrowRight, size, foreground).into_any_element()
        })
        .debug_selector("tab-menu-row-move-right"),
        MenuEntry::separator(),
        with_shortcut(
            MenuEntry::action(
                match waiting {
                    Some(_) => "Close Tab Needs the Server",
                    None => "Close Tab",
                },
                TabMenuCommand::Close,
            ),
            &CloseTab,
        )
        .destructive(shared_edits)
        .disabled(!shared_edits)
        .icon(|foreground, size| Icon::new(IconName::X, size, foreground).into_any_element())
        .debug_selector("tab-menu-row-close"),
    ]
}

fn title_bar_control_focus_ring(appearance: &super::appearance::ChromeAppearance) -> Color {
    appearance.title_bar_controls.colors.focus_ring
}

fn create_tab_shortcut(
    presentation: &crate::desktop_profile::DesktopPresentation,
) -> Option<gpui::SharedString> {
    presentation.shortcut(&crate::ui::CreateTab)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::Arc;

    use gpui::{
        Modifiers, MouseDownEvent, MouseExitEvent, MouseUpEvent, ScrollDelta, ScrollWheelEvent,
        TestAppContext, TouchPhase, VisualTestContext, point,
    };

    use super::*;

    #[test]
    fn create_tab_tooltip_should_use_the_host_neutral_profile_shortcut() {
        assert_eq!(
            create_tab_shortcut(&crate::desktop_profile::testing_presentation()),
            Some("Primary+T".into())
        );
    }

    fn prepared_opposing_title_bar(
        root: Color,
        title_bar: Color,
        increase_contrast: bool,
        show_borders: bool,
    ) -> super::super::appearance::ChromeAppearance {
        use crate::appearance::{
            AppearanceGeneration, AppearancePreferences, AvailableFonts, CompositionCapabilities,
            SystemAppearance, ThemeCatalog,
        };

        let mut resolved = ThemeCatalog::default()
            .resolve(
                AppearanceGeneration::INITIAL,
                &AppearancePreferences::default(),
                SystemAppearance::unavailable().with_composition(CompositionCapabilities {
                    increase_contrast,
                    show_borders,
                    ..CompositionCapabilities::new(true, true)
                }),
                &AvailableFonts::default(),
            )
            .unwrap();
        let chrome = Arc::make_mut(&mut resolved.chrome);
        chrome.colors.background = root;
        chrome.colors.title_bar_background = title_bar;
        chrome.colors.title_bar_inactive_background = title_bar;
        chrome.colors.tab_inactive_background = title_bar;
        chrome.colors.focus_ring = title_bar;
        chrome.colors.tab_active_border = title_bar;
        super::super::appearance::ChromeAppearance::prepare(&resolved.chrome)
    }

    #[test]
    fn title_bar_controls_use_focus_paint_prepared_for_their_actual_host() {
        let appearance = prepared_opposing_title_bar(Color::BLACK, Color::WHITE, true, false);
        let host = appearance.control_host_background(spaceterm_ui::ControlHost::TitleBar);
        let focus = title_bar_control_focus_ring(&appearance).source_over(host);
        assert!(
            focus.contrast_ratio(host) >= 4.5,
            "TitleBar focus {focus:?} must reach 4.5:1 on its final host {host:?}"
        );

        let presentation = TabChromePresentation::resolve(true, false, &appearance.colors);
        let selected = appearance
            .materials
            .paint(
                crate::appearance::SurfaceRole::Surface,
                presentation.background,
                presentation.active_tab_background,
            )
            .source_over(host);
        assert!(selected.contrast_ratio(host) >= 1.4);
    }

    #[test]
    fn show_borders_prepares_tab_edges_for_the_title_bar_host() {
        let appearance = prepared_opposing_title_bar(Color::WHITE, Color::BLACK, false, true);
        let presentation = TabChromePresentation::resolve(true, true, &appearance.colors);
        let host = appearance.control_host_background(spaceterm_ui::ControlHost::TitleBar);
        let chip = presentation
            .tab_chip_paint(false)
            .raised_on(&appearance, presentation.background);
        let background = chip.fill.unwrap_or(Color::rgba(0)).source_over(host);
        let edge = chip
            .rim
            .expect("Show Borders must retain an unselected Tab edge");
        let rendered_edge = edge.source_over(background);
        assert!(
            rendered_edge.contrast_ratio(background) >= 3.0,
            "unselected Tab edge {rendered_edge:?} must reach 3:1 on its final TitleBar background {background:?}"
        );
    }

    #[test]
    fn active_window_tab_chrome_should_resolve_authored_semantic_roles() {
        let colors = ChromeColors::default();
        let presentation = TabChromePresentation::resolve(true, false, &colors);

        assert_eq!(
            presentation,
            TabChromePresentation {
                window_active: true,
                show_borders: false,
                background: colors.title_bar_background,
                active_tab_background: colors.tab_active_background,
                active_tab_border: colors.tab_active_border,
                active_tab_hover_background: colors.tab_active_hover_background,
                active_tab_hover_border: colors.tab_active_border,
                active_tab_hover_foreground: colors.tab_active_hover_foreground,
                active_tab_hover_icon: colors.tab_active_hover_icon,
                inactive_tab_background: colors.tab_inactive_background,
                active_tab_foreground: colors.tab_active_foreground,
                inactive_tab_foreground: colors.tab_inactive_foreground,
                active_tab_icon: colors.tab_active_icon,
                inactive_tab_icon: colors.tab_inactive_icon,
                hover_background: colors.tab_hover_background,
                hover_foreground: colors.tab_hover_foreground,
                hover_icon: colors.tab_hover_icon,
                tab_separator: colors.tab_separator,
            }
        );
    }

    #[test]
    fn inactive_window_preserves_selected_tab_identity() {
        let colors = ChromeColors::default();
        let presentation = TabChromePresentation::resolve(false, false, &colors);

        assert_eq!(
            presentation,
            TabChromePresentation {
                window_active: false,
                show_borders: false,
                background: colors.title_bar_inactive_background,
                active_tab_background: colors.tab_active_background,
                active_tab_border: colors.tab_active_border,
                active_tab_hover_background: colors.tab_active_hover_background,
                active_tab_hover_border: colors.tab_active_border,
                active_tab_hover_foreground: colors.tab_active_hover_foreground,
                active_tab_hover_icon: colors.tab_active_hover_icon,
                inactive_tab_background: colors.title_bar_inactive_background,
                active_tab_foreground: colors.tab_active_foreground,
                inactive_tab_foreground: colors.tab_inactive_foreground,
                active_tab_icon: colors.tab_active_icon,
                inactive_tab_icon: colors.tab_inactive_icon,
                hover_background: colors.tab_hover_background,
                hover_foreground: colors.tab_hover_foreground,
                hover_icon: colors.tab_hover_icon,
                tab_separator: colors.tab_separator,
            }
        );
    }

    #[test]
    fn tab_hover_paints_only_when_the_window_is_active() {
        let colors = ChromeColors {
            tab_active_background: Color::rgb(0x112233),
            tab_active_border: Color::rgb(0x223344),
            tab_active_hover_background: Color::rgb(0x2a3b4c),
            tab_active_hover_foreground: Color::rgb(0xeef0ff),
            tab_active_foreground: Color::rgb(0xddeeff),
            tab_hover_background: Color::rgb(0x556677),
            tab_hover_foreground: Color::rgb(0x99aabb),
            tab_hover_icon: Color::rgb(0x778899),
            ..ChromeColors::default()
        };
        for window_active in [true, false] {
            let presentation = TabChromePresentation::resolve(window_active, false, &colors);
            let selected = presentation.tab_chip_paint(true);
            assert_eq!(
                (
                    selected.fill,
                    selected.rim,
                    selected.hover_fill,
                    selected.hover_rim
                ),
                (
                    Some(colors.tab_active_background),
                    Some(colors.tab_active_border),
                    window_active.then_some(colors.tab_active_hover_background),
                    window_active.then_some(colors.tab_active_border),
                ),
                "window_active={window_active}: hover paint follows window activity"
            );
            assert_eq!(
                presentation.tab_hover_foreground(true),
                colors.tab_active_hover_foreground
            );
            let inactive = presentation.tab_chip_paint(false);
            assert_eq!(
                (inactive.rim, inactive.hover_fill, inactive.hover_rim),
                (
                    None,
                    window_active.then_some(colors.tab_hover_background),
                    None
                ),
                "window_active={window_active}: only the Active Tab should carry a rim"
            );
            assert_eq!(
                presentation.tab_hover_foreground(false),
                colors.tab_hover_foreground
            );
            let close = presentation.close_control_style(
                true,
                true,
                &colors,
                crate::appearance::SurfaceMaterials::OPAQUE,
            );
            assert_eq!(
                close.hovered().background(),
                if window_active {
                    gpui_color(colors.tab_hover_background)
                } else {
                    gpui::rgba(0)
                },
                "window_active={window_active}: Close hover paint follows window activity"
            );
            assert_eq!(
                close.hovered().foreground(),
                if window_active {
                    gpui_color(colors.tab_hover_icon)
                } else {
                    gpui_color(colors.tab_active_hover_icon)
                }
            );
            assert_eq!(
                presentation
                    .close_control_style(
                        false,
                        true,
                        &colors,
                        crate::appearance::SurfaceMaterials::OPAQUE,
                    )
                    .normal()
                    .foreground(),
                gpui_color(colors.tab_hover_icon),
                "window_active={window_active}: the revealed inactive-Tab Close glyph should follow the Tab hover paint"
            );
        }
    }

    #[test]
    fn show_borders_outlines_unselected_tabs_without_restoring_inactive_hover() {
        let colors = ChromeColors {
            tab_active_border: Color::rgb(0x223344),
            tab_hover_background: Color::rgb(0x556677),
            ..ChromeColors::default()
        };

        let active = TabChromePresentation::resolve(true, true, &colors).tab_chip_paint(false);
        assert_eq!(active.rim, Some(colors.tab_active_border));
        assert_eq!(active.hover_rim, Some(colors.tab_active_border));

        let inactive = TabChromePresentation::resolve(false, true, &colors).tab_chip_paint(false);
        assert_eq!(inactive.rim, Some(colors.tab_active_border));
        assert_eq!(inactive.hover_fill, None);
        assert_eq!(inactive.hover_rim, None);
    }

    #[test]
    fn built_in_active_tab_should_use_its_authored_chip_material_and_edge() {
        use crate::appearance::{Appearance, builtin_chrome_base};

        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = builtin_chrome_base(appearance);
            let presentation = TabChromePresentation::resolve(true, false, &colors);
            let chip = presentation.tab_chip_paint(true);

            assert_eq!(
                (chip.fill, chip.rim, chip.hover_fill, chip.hover_rim),
                (
                    Some(colors.tab_active_background),
                    Some(colors.tab_active_border),
                    Some(colors.tab_active_hover_background),
                    Some(colors.tab_active_border),
                ),
                "{appearance:?} Active Tab chip should use its own authored states"
            );
            assert_eq!(
                (
                    presentation.tab_foreground(true),
                    presentation.tab_hover_foreground(true),
                ),
                (
                    colors.row_selected_foreground,
                    colors.row_selected_hover_foreground,
                ),
                "{appearance:?} Active Tab title should match a selected navigation label"
            );
            assert!(
                colors.tab_active_border.a > 0,
                "{appearance:?} Active Tab should carry the chip hairline"
            );
            assert_eq!(
                chip.hover_fill == chip.fill,
                appearance == Appearance::Light,
                "Light uses the prepared hover rim; Dark retains its hovered fill"
            );
            assert_eq!(
                chip.fill == Some(colors.row_selected_background),
                appearance == Appearance::Light,
                "Light should share its selected fill across Tab and row hosts"
            );
            let tab_step = i32::from(colors.tab_active_background.r)
                - i32::from(colors.title_bar_background.r);
            let row_step = i32::from(colors.navigation_selected_background.r)
                - i32::from(colors.panel_background.r);
            assert!(
                tab_step * row_step > 0,
                "{appearance:?} Active Tab should follow the navigation selection direction"
            );
        }
    }

    #[test]
    fn built_in_tab_separator_should_be_visible_but_quiet_on_both_title_bar_surfaces() {
        use crate::appearance::{Appearance, builtin_chrome_base};

        const MINIMUM_SEPARATOR_CONTRAST: f64 = 1.4;
        const MAXIMUM_SEPARATOR_CONTRAST: f64 = 3.0;

        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = builtin_chrome_base(appearance).opaque_presentation();
            for (window_active, surface) in [
                (true, colors.title_bar_background),
                (false, colors.title_bar_inactive_background),
            ] {
                let presentation = TabChromePresentation::resolve(window_active, false, &colors);
                let bar = presentation.background;
                assert_eq!(bar, surface);
                let separator = presentation.tab_separator.source_over(bar);
                let contrast = separator.contrast_ratio(bar);
                assert!(
                    (MINIMUM_SEPARATOR_CONTRAST..=MAXIMUM_SEPARATOR_CONTRAST).contains(&contrast),
                    "{appearance:?} window_active={window_active}: separator contrast {contrast:.2} \
                     should be visible but quiet"
                );
                assert!(
                    contrast
                        < presentation
                            .tab_foreground(false)
                            .source_over(bar)
                            .contrast_ratio(bar),
                    "{appearance:?} window_active={window_active}: separator should stay quieter \
                     than an inactive Tab title"
                );
            }
        }
    }

    #[test]
    fn tab_separator_width_should_cover_at_least_one_device_pixel_at_every_display_scale() {
        for scale in [1.0_f32, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0] {
            let device = TAB_SEPARATOR_WIDTH * scale;
            assert!(
                device.floor() >= 1.0,
                "a {TAB_SEPARATOR_WIDTH}-point separator covers {device} device pixels at {scale}x"
            );
        }
    }

    #[test]
    fn tab_close_control_should_share_parent_at_rest_and_keep_direct_interaction_states() {
        let colors = ChromeColors {
            tab_active_background: Color::rgb(0x445566),
            tab_active_hover_background: Color::rgb(0x667788),
            tab_hover_background: Color::rgb(0x778899),
            tab_active_icon: Color::rgb(0x112233),
            tab_hover_icon: Color::rgb(0x334455),
            ..ChromeColors::default()
        };
        let colors = colors.opaque_presentation();
        for window_active in [false, true] {
            let presentation = TabChromePresentation::resolve(window_active, false, &colors);
            let style = presentation.close_control_style(
                true,
                false,
                &colors,
                crate::appearance::SurfaceMaterials::OPAQUE,
            );
            assert_eq!(style.normal().background(), rgba(0));
            assert_eq!(
                style.hovered().background(),
                if window_active {
                    gpui_color(colors.tab_hover_background)
                } else {
                    rgba(0)
                }
            );
            assert_eq!(
                style.pressed().background(),
                gpui_color(colors.tab_hover_background)
            );
            assert_eq!(
                style.normal().foreground(),
                gpui_color(colors.tab_active_icon)
            );
            assert_eq!(
                style.hovered().foreground(),
                if window_active {
                    gpui_color(colors.tab_hover_icon)
                } else {
                    gpui_color(colors.tab_active_icon)
                }
            );
            assert_eq!(
                style.pressed().foreground(),
                gpui_color(colors.tab_hover_icon)
            );

            for active in [true, false] {
                let style = presentation.close_control_style(
                    active,
                    true,
                    &colors,
                    crate::appearance::SurfaceMaterials::OPAQUE,
                );
                assert_eq!(
                    style.normal().background(),
                    rgba(0),
                    "window_active={window_active} active={active}: the revealed Close control should leave the parent Tab surface continuous"
                );
                assert_eq!(
                    style.hovered().background(),
                    if window_active {
                        gpui_color(colors.tab_hover_background)
                    } else {
                        rgba(0)
                    },
                    "window_active={window_active} active={active}: direct hover should follow window activity"
                );
            }
        }
    }

    #[test]
    fn tab_contextual_controls_should_materialize_hover_against_their_actual_host() {
        use crate::appearance::{
            AppearanceGeneration, AppearancePreferences, AvailableFonts, CompositionCapabilities,
            SurfaceRole, SystemAppearance, ThemeCatalog,
        };

        let mut preferences = AppearancePreferences::default();
        preferences.window.opacity = 0.0;
        let resolved = ThemeCatalog::default()
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::unavailable()
                    .with_composition(CompositionCapabilities::new(true, true)),
                &AvailableFonts::default(),
            )
            .unwrap();
        let appearance = super::super::appearance::ChromeAppearance::prepare(&resolved.chrome);
        let presentation = TabChromePresentation::resolve(true, false, &appearance.colors);

        let close =
            presentation.close_control_style(true, true, &appearance.colors, appearance.materials);
        let expected_close = appearance.materials.paint(
            SurfaceRole::Surface,
            presentation.active_tab_hover_background,
            presentation.hover_background,
        );
        assert!(expected_close.a < 255);
        assert_eq!(close.hovered().background(), gpui_color(expected_close));

        let create = presentation.bar_control_style(&appearance.colors, appearance.materials);
        let expected_create = appearance.materials.paint(
            SurfaceRole::Surface,
            presentation.background,
            presentation.hover_background,
        );
        assert!(expected_create.a < 255);
        assert_eq!(create.hovered().background(), gpui_color(expected_create));
    }

    fn rendered_icon_foreground(
        selector: &'static str,
        pressed: bool,
        rendered: &Cell<gpui::Rgba>,
        cx: &mut VisualTestContext,
    ) -> gpui::Rgba {
        let position = cx
            .debug_bounds(selector)
            .expect("the tab IconButton was not rendered")
            .center();
        cx.simulate_mouse_move(position, None, Modifiers::none());
        if pressed {
            cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
        }
        crate::ui::settle_hover(cx);
        let foreground = rendered.get();
        if pressed {
            let outside = cx
                .debug_bounds("tab-manager-content")
                .expect("Tab content was not rendered")
                .center();
            cx.simulate_mouse_move(outside, Some(MouseButton::Left), Modifiers::none());
            cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::none());
            cx.run_until_parked();
        }
        foreground
    }

    #[gpui::test]
    fn tab_icon_glyphs_should_use_hovered_and_pressed_button_foregrounds(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        let normal = rgba(0x11_22_33_ff);
        let hovered = rgba(0x22_cc_44_ff);
        let create_icon = manager.read_with(cx, |manager, _| {
            Rc::clone(&manager.rendered_create_tab_icon)
        });
        let close_icon = manager.read_with(cx, |manager, _| {
            Rc::clone(&manager.rendered_active_close_icon)
        });
        cx.update(|window, cx| {
            let mut active = crate::ui::appearance::chrome(cx).clone();
            active.colors.tab_hover_icon = Color::rgb(0x22cc44);
            let mut inactive = active.clone();
            inactive.active = false;
            inactive.colors.tab_inactive_icon = Color::rgb(0x112233);
            cx.set_global(crate::ui::appearance::InstalledChrome {
                active: Arc::new(active),
                inactive: Arc::new(inactive),
            });
            window.refresh();
        });
        cx.run_until_parked();

        for (selector, rendered) in [
            ("create-tab-button", create_icon.as_ref()),
            ("tab-close-button-1", close_icon.as_ref()),
        ] {
            assert_eq!(
                rendered_icon_foreground(selector, false, rendered, cx),
                hovered,
                "{selector} must paint its glyph with the hovered IconButton foreground"
            );
            assert_eq!(
                rendered_icon_foreground(selector, true, rendered, cx),
                hovered,
                "{selector} must paint its glyph with the pressed IconButton foreground"
            );
        }

        cx.deactivate_window();
        cx.run_until_parked();
        assert_eq!(
            rendered_icon_foreground("create-tab-button", false, &create_icon, cx),
            normal,
            "the new-tab SVG must retain the shared icon tint in an inactive window"
        );
    }

    #[gpui::test]
    fn tab_close_glyph_should_suppress_hover_presentation_in_an_inactive_window(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = tab_manager(cx);
        let parent_hover = rgba(0x11_22_33_ff);
        let direct_hover = rgba(0x44_55_66_ff);
        let rendered = manager.read_with(cx, |manager, _| {
            Rc::clone(&manager.rendered_active_close_icon)
        });
        let rendered_inactive = manager.read_with(cx, |manager, _| {
            Rc::clone(&manager.rendered_inactive_close_icon)
        });
        cx.update(|window, cx| {
            let mut active = crate::ui::appearance::chrome(cx).clone();
            active.colors.tab_active_background = Color::BLACK;
            active.colors.tab_active_icon = Color::WHITE;
            active.colors.tab_active_hover_background = Color::WHITE;
            active.colors.tab_active_hover_foreground = Color::BLACK;
            active.colors.tab_active_hover_icon = Color::rgb(0x112233);
            active.colors.tab_hover_icon = Color::rgb(0x445566);
            let mut inactive = active.clone();
            inactive.active = false;
            inactive.colors.tab_active_icon = Color::rgb(0x778899);
            inactive.colors.tab_inactive_icon = Color::rgb(0x8899aa);
            cx.set_global(crate::ui::appearance::InstalledChrome {
                active: Arc::new(active),
                inactive: Arc::new(inactive),
            });
            window.refresh();
        });
        cx.run_until_parked();

        let title = cx
            .debug_bounds("tab-title-1")
            .expect("the Active Tab title was not rendered")
            .center();
        let close = cx
            .debug_bounds("tab-close-button-1")
            .expect("the Active Tab close button was not rendered")
            .center();

        cx.simulate_mouse_move(title, None, Modifiers::none());
        crate::ui::settle_hover(cx);
        assert_eq!(
            rendered.get(),
            parent_hover,
            "hovering the Active Tab title should repaint its rendered close glyph for the selected-hover fill"
        );

        cx.simulate_mouse_move(close, None, Modifiers::none());
        crate::ui::settle_hover(cx);
        assert_eq!(
            rendered.get(),
            direct_hover,
            "direct close hover should retain the IconButton's own foreground"
        );

        cx.simulate_mouse_move(title, None, Modifiers::none());
        crate::ui::settle_hover(cx);
        cx.deactivate_window();
        cx.run_until_parked();
        let inactive_resting = rendered.get();
        cx.simulate_mouse_move(title, None, Modifiers::none());
        crate::ui::settle_hover(cx);
        assert!(!manager.read_with(cx, |manager, _| manager.rendered_window_active));
        assert_eq!(
            rendered.get(),
            inactive_resting,
            "an inactive window should retain the resting selected-Tab close glyph"
        );

        cx.simulate_mouse_move(close, None, Modifiers::none());
        crate::ui::settle_hover(cx);
        assert_eq!(
            rendered.get(),
            inactive_resting,
            "direct hover must not repaint a control in an inactive window"
        );

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.create_tab(window, cx));
        });
        cx.run_until_parked();
        let inactive_title = cx
            .debug_bounds("tab-title-1")
            .expect("the inactive Tab title was not rendered")
            .center();
        let inactive_close = cx
            .debug_bounds("tab-close-button-1")
            .expect("the inactive Tab close button was not rendered")
            .center();

        let inactive_tab_resting = rendered_inactive.get();
        cx.simulate_mouse_move(inactive_title, None, Modifiers::none());
        crate::ui::settle_hover(cx);
        assert_eq!(rendered_inactive.get(), inactive_tab_resting);
        cx.simulate_mouse_move(inactive_close, None, Modifiers::none());
        crate::ui::settle_hover(cx);
        assert_eq!(rendered_inactive.get(), inactive_tab_resting);

        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        cx.simulate_mouse_move(inactive_title, None, Modifiers::none());
        crate::ui::settle_hover(cx);
        assert_eq!(rendered_inactive.get(), direct_hover);
        cx.simulate_mouse_move(inactive_close, None, Modifiers::none());
        crate::ui::settle_hover(cx);
        assert_eq!(rendered_inactive.get(), direct_hover);
    }
    use crate::domain::PaneId;
    use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
    use crate::terminal::testing::{
        RecordedCall, RecordedCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
    };
    use crate::terminal::{
        TerminalSessionEvent, TerminalSessionExit, TerminalSessionFactory, TerminalSessionFailure,
    };

    fn tab_manager(
        cx: &mut TestAppContext,
    ) -> (
        Entity<TabManager>,
        TestTerminalSessionRecords,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        open_tab_manager(cx)
    }

    struct TabManagerActivityHarness {
        manager: Entity<TabManager>,
    }

    impl Render for TabManagerActivityHarness {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let activity = crate::ui::appearance::window_activity(window);
            activity.mount(activity.with_scope(|| self.manager.clone()))
        }
    }

    fn open_tab_manager(
        cx: &mut TestAppContext,
    ) -> (
        Entity<TabManager>,
        TestTerminalSessionRecords,
        &mut VisualTestContext,
    ) {
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(PathBuf::from(
                "/tmp/spaceterm-tab-manager-test",
            )),
        );
        let (harness, cx) = cx.add_window_view(|window, cx| TabManagerActivityHarness {
            manager: cx.new(|cx| TabManager::new(session_factory, window, cx)),
        });
        let manager = harness.read_with(cx, |harness, _| harness.manager.clone());
        cx.update(|window, cx| {
            window.activate_window();
            manager.update(cx, |manager, cx| manager.focus(window, cx));
            manager.update(cx, |_, cx| {
                cx.subscribe_in(
                    &manager,
                    window,
                    |manager, _, event: &TabManagerEvent, window, cx| match event {
                        TabManagerEvent::ClosePaneRequested { tab_id, pane_id } => {
                            manager.close_pane_authorized(*tab_id, *pane_id, window, cx);
                        }
                        TabManagerEvent::CloseTabRequested { tab_id } => {
                            manager.close_tab_authorized(*tab_id, window, cx);
                        }
                        _ => {}
                    },
                )
                .detach();
            });
        });
        cx.run_until_parked();
        (manager, records, cx)
    }
    fn tab_manager_with_operating_system_window_drag_platform(
        cx: &mut TestAppContext,
    ) -> (
        Entity<TabManager>,
        Rc<RecordingOperatingSystemWindowDragPlatform>,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(PathBuf::from(
                "/tmp/spaceterm-tab-manager-drag-test",
            )),
        );
        let platform = Rc::new(RecordingOperatingSystemWindowDragPlatform::default());
        let injected_platform = Rc::clone(&platform);
        let (manager, cx) = cx.add_window_view(move |window, cx| {
            TabManager::new_with_operating_system_window_drag_platform(
                session_factory,
                injected_platform,
                window,
                cx,
            )
        });
        cx.update(|window, cx| {
            window.activate_window();
            manager.update(cx, |manager, cx| manager.focus(window, cx));
        });
        cx.run_until_parked();
        (manager, platform, cx)
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let position = cx
            .debug_bounds(selector)
            .map(|bounds| bounds.center())
            .unwrap_or_else(|| panic!("{selector} was not rendered"));
        cx.simulate_mouse_move(position, None, Modifiers::none());
        cx.simulate_click(position, Modifiers::none());
        cx.run_until_parked();
    }

    fn right_click(selector: &'static str, cx: &mut VisualTestContext) {
        let position = cx
            .debug_bounds(selector)
            .map(|bounds| bounds.center())
            .unwrap_or_else(|| panic!("{selector} was not rendered"));
        cx.simulate_mouse_move(position, None, Modifiers::none());
        cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn right_clicking_a_tab_should_open_its_menu_without_activating_it(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        let commands_before = records.commands().len();
        right_click("tab-item-1-inactive", cx);
        assert!(cx.debug_bounds("menu-panel-0").is_some());
        assert_eq!(
            manager.read_with(cx, |manager, _| manager.tabs.active_tab_id()),
            TabId::new(2)
        );
        assert_eq!(
            manager.read_with(cx, |manager, _| manager.terminal_focus_blocker()),
            Some(TerminalFocusBlocker::ContextMenu)
        );

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(cx.debug_bounds("menu-panel-0").is_none());
        assert_eq!(
            manager.read_with(cx, |manager, _| manager.tabs.active_tab_id()),
            TabId::new(2)
        );
        assert!(cx.update(|window, cx| {
            manager
                .read(cx)
                .focused_terminal_has_input_focus(window, cx)
        }));
        // Only the Active Tab's terminal hears the menu take focus and give it back.
        // Readiness can also ask inactive Panes to prepare screens without giving them input.
        let commands = records
            .commands()
            .into_iter()
            .skip(commands_before)
            .filter(|call| !matches!(call.command, RecordedCommand::SetPreparationDemand(_)))
            .collect::<Vec<_>>();
        assert_eq!(
            commands,
            [
                RecordedCall {
                    session_id: 2,
                    command: RecordedCommand::Focus(false),
                },
                RecordedCall {
                    session_id: 2,
                    command: RecordedCommand::Focus(true),
                },
            ]
        );
    }

    /// A Shortcut acts on the Active Tab, so only the Active Tab's menu may advertise one.
    #[gpui::test]
    fn tab_menu_should_show_shortcuts_only_for_the_active_tab(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        let rows = [
            "tab-menu-row-new-tab-shortcut",
            "tab-menu-row-split-right-shortcut",
            "tab-menu-row-split-down-shortcut",
            "tab-menu-row-move-left-shortcut",
            "tab-menu-row-move-right-shortcut",
            "tab-menu-row-close-shortcut",
        ];

        right_click("tab-item-2-active", cx);
        assert!(cx.debug_bounds("tab-menu-row-close-shortcut").is_some());
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        right_click("tab-item-1-inactive", cx);
        assert!(cx.debug_bounds("menu-panel-0").is_some());
        for row in rows {
            assert!(
                cx.debug_bounds(row).is_none(),
                "{row} must not advertise a Shortcut"
            );
        }
    }

    #[gpui::test]
    fn tab_menu_new_tab_should_create_and_activate_a_tab(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        right_click("tab-item-1-active", cx);
        click("tab-menu-row-new-tab", cx);

        assert_eq!(tab_order(&manager, cx), vec![1, 2]);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager.tabs.active_tab_id()),
            TabId::new(2)
        );
    }

    #[gpui::test]
    fn tab_menu_close_should_close_the_clicked_tab_not_the_active_tab(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        right_click("tab-item-1-inactive", cx);
        click("tab-menu-row-close", cx);

        assert_eq!(tab_order(&manager, cx), vec![2]);
        assert_eq!(
            manager.read_with(cx, |manager, _| (
                manager.tabs.active_tab_id(),
                manager.terminal_focus_blocker()
            )),
            (TabId::new(2), None)
        );
    }

    fn pane_counts(manager: &Entity<TabManager>, cx: &mut VisualTestContext) -> Vec<usize> {
        manager.read_with(cx, |manager, cx| {
            manager
                .tabs
                .iter()
                .map(|(_, view)| view.read(cx).pane_count())
                .collect()
        })
    }

    #[gpui::test]
    fn tab_menu_split_should_activate_the_clicked_tab_and_split_its_focused_pane(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        right_click("tab-item-1-inactive", cx);
        click("tab-menu-row-split-right", cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| manager.tabs.active_tab_id()),
            TabId::new(1)
        );
        assert_eq!(pane_counts(&manager, cx), vec![2, 1]);

        right_click("tab-item-2-inactive", cx);
        click("tab-menu-row-split-down", cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| manager.tabs.active_tab_id()),
            TabId::new(2)
        );
        assert_eq!(pane_counts(&manager, cx), vec![2, 2]);
        assert!(cx.update(|window, cx| {
            manager
                .read(cx)
                .focused_terminal_has_input_focus(window, cx)
        }));
    }

    #[gpui::test]
    fn tab_menu_move_should_reorder_only_the_clicked_tab_and_stop_at_the_ends(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);

        right_click("tab-item-1-inactive", cx);
        click("tab-menu-row-move-right", cx);
        assert_eq!(tab_order(&manager, cx), vec![2, 1, 3]);

        right_click("tab-item-1-inactive", cx);
        click("tab-menu-row-move-left", cx);
        assert_eq!(tab_order(&manager, cx), vec![1, 2, 3]);

        // The first Tab cannot move left, and the last Tab cannot move right.
        for (tab, row) in [
            ("tab-item-1-inactive", "tab-menu-row-move-left"),
            ("tab-item-3-active", "tab-menu-row-move-right"),
        ] {
            right_click(tab, cx);
            click(row, cx);
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
        }
        assert_eq!(tab_order(&manager, cx), vec![1, 2, 3]);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager.tabs.active_tab_id()),
            TabId::new(3)
        );
    }

    /// An inactive Tab keeps the Pane bounds from its last paint, which a resize makes stale.
    #[gpui::test]
    fn tab_menu_split_should_use_the_current_window_size_for_an_inactive_tab(
        cx: &mut TestAppContext,
    ) {
        let (manager, _, cx) = tab_manager(cx);
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.create_tab(window, cx));
        });
        cx.run_until_parked();
        cx.simulate_resize(gpui::size(px(900.0), px(500.0)));
        cx.run_until_parked();

        right_click("tab-item-1-inactive", cx);
        click("tab-menu-row-split-right", cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| manager.tabs.active_tab_id()),
            TabId::new(1)
        );
        assert_eq!(pane_counts(&manager, cx), vec![2, 1]);
    }

    #[gpui::test]
    fn a_secondary_click_during_a_tab_drag_should_open_no_menu(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        let first = cx.debug_bounds("tab-item-1-inactive").unwrap().center();
        let second = cx.debug_bounds("tab-item-2-active").unwrap().center();
        drag_tab(first, &[second], cx);

        cx.simulate_mouse_down(second, MouseButton::Right, Modifiers::none());
        cx.run_until_parked();

        assert!(cx.debug_bounds("menu-panel-0").is_none());
    }

    #[gpui::test]
    fn every_tab_should_float_as_an_inset_chip_without_a_bar_seam(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);

        let bar = cx
            .debug_bounds("tab-bar")
            .expect("the Tab bar was not rendered");
        let active_item = cx
            .debug_bounds("tab-item-2-active")
            .expect("the Active Tab item was not rendered");
        let inactive_chip = cx
            .debug_bounds("tab-item-1-chip")
            .expect("the inactive Tab chip was not rendered");
        let active_chip = cx
            .debug_bounds("tab-item-2-chip")
            .expect("the Active Tab chip was not rendered");

        // The bar carries the window's own edge above a band, and a Tab fills that band.
        let (bar_height, band_height) = cx.update(|_, cx| {
            let appearance = crate::ui::appearance::chrome(cx);
            let frame = crate::ui::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx);
            (
                frame.top_chrome_height(appearance.top_height()),
                frame.top_band_height(appearance.top_height()),
            )
        });
        assert_eq!(
            (active_item.size.height, bar.size.height),
            (band_height, bar_height),
            "a Tab should keep the full height of the bar's band as its hit target"
        );
        let (space, leading, trailing, window_edge) = cx.update(|_, cx| {
            let frame = crate::ui::workspace_frame::WorkspaceFrame::for_appearance(
                crate::ui::appearance::chrome(cx),
                cx,
            );
            (
                frame.space(),
                frame.strip_chip_leading_inset(),
                frame.strip_chip_trailing_inset(),
                frame.window_edge(),
            )
        });
        assert_eq!(
            active_chip,
            gpui::bounds(
                gpui::point(active_item.origin.x + leading, active_item.origin.y + space,),
                gpui::size(
                    active_item.size.width - leading - trailing,
                    active_item.size.height - space - space,
                ),
            ),
            "the Active Tab material should float inside its item"
        );
        // The visible distance between two Tabs, and between a Tab and the strip's own edges, is
        // the frame's one space. Above the chip, the window paints its own edge first.
        assert_eq!(
            (
                active_chip.left() - inactive_chip.right(),
                active_chip.top() - bar.top() - window_edge,
                bar.bottom() - active_chip.bottom(),
            ),
            (space, space, space),
            "every gap around a Tab should be one visible space"
        );
        assert!(
            active_chip.bottom() < bar.bottom(),
            "the Active Tab should clear the bar's lower edge, got {active_chip:?} against {bar:?}"
        );
        for stale in [
            "tab-bar-divider",
            "tab-item-1-divider",
            "tab-item-1-bottom-divider",
            "tab-item-2-underline",
        ] {
            assert!(
                cx.debug_bounds(stale).is_none(),
                "{stale} should no longer be drawn beside the chip"
            );
        }
    }

    /// The part of the Tab bar beneath the window's own edge, where every top control centres.
    fn top_band_height(cx: &mut VisualTestContext) -> Pixels {
        cx.update(|_, cx| {
            let appearance = crate::ui::appearance::chrome(cx);
            crate::ui::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx)
                .top_band_height(appearance.top_height())
        })
    }

    fn leaked_selector(selector: String) -> &'static str {
        Box::leak(selector.into_boxed_str())
    }

    /// Opens a fresh four-Tab row for each density, sidebar visibility, and selected position. Each
    /// position gets its own window because rendered debug bounds outlive the frame that drew them.
    fn assert_separators_mark_only_inactive_neighbours(
        cx: &mut TestAppContext,
        direction: spaceterm_ui::TextDirection,
    ) {
        use crate::appearance::ChromeDensity;

        cx.update(|cx| crate::ui::init_with_text_direction(cx, direction))
            .expect("UI initialization should succeed");
        let within = |inner: gpui::Bounds<Pixels>, outer: gpui::Bounds<Pixels>| {
            inner.left() >= outer.left()
                && inner.right() <= outer.right()
                && inner.top() >= outer.top()
                && inner.bottom() <= outer.bottom()
        };
        let overlaps = |a: gpui::Bounds<Pixels>, b: gpui::Bounds<Pixels>| {
            a.left() < b.right()
                && b.left() < a.right()
                && a.top() < b.bottom()
                && b.top() < a.bottom()
        };
        for density in [ChromeDensity::Compact, ChromeDensity::Comfortable] {
            let expected_length = match density {
                ChromeDensity::Compact => px(18.0),
                ChromeDensity::Comfortable => px(22.5),
            };
            cx.update(|cx| {
                let mut appearance = crate::ui::appearance::chrome(cx).clone();
                appearance.spacing_scale =
                    crate::ui::appearance::ChromeAppearance::density_spacing_scale(density);
                cx.set_global(crate::ui::appearance::InstalledChrome::single(Arc::new(
                    appearance,
                )));
            });
            let positions = [true, false].into_iter().flat_map(|sidebar_visible| {
                (1..=4_u64).map(move |selected| (sidebar_visible, selected))
            });
            for (sidebar_visible, selected) in positions {
                let (manager, _records, cx) = open_tab_manager(cx);
                cx.update(|window, cx| {
                    manager.update(cx, |manager, cx| {
                        let top_chrome_width = if sidebar_visible {
                            WORKSPACE_SIDEBAR_DEFAULT_WIDTH
                        } else {
                            WORKSPACE_SIDEBAR_MINIMUM_WIDTH
                        };
                        manager.set_sidebar_layout(
                            sidebar_visible,
                            px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
                            px(top_chrome_width),
                            cx,
                        );
                        for _ in 1..4 {
                            manager.create_tab(window, cx);
                        }
                        manager.activate_tab_at(selected as usize - 1, window, cx);
                    });
                });
                cx.run_until_parked();
                assert_eq!(
                    manager.read_with(cx, |manager, _| manager.tabs.active_tab_id()),
                    TabId::new(selected)
                );
                let scale_factor = cx.update(|window, _| window.scale_factor());
                let on_device_pixels = |value: Pixels| {
                    let device = f32::from(value) * scale_factor;
                    (device - device.round()).abs() < 1e-3
                };
                let item = |tab: u64, cx: &mut VisualTestContext| {
                    let state = if tab == selected {
                        "active"
                    } else {
                        "inactive"
                    };
                    cx.debug_bounds(leaked_selector(format!("tab-item-{tab}-{state}")))
                        .unwrap_or_else(|| panic!("Tab {tab} was not rendered"))
                };
                let chip = |tab: u64, cx: &mut VisualTestContext| {
                    cx.debug_bounds(leaked_selector(format!("tab-item-{tab}-chip")))
                        .unwrap_or_else(|| panic!("Tab {tab} chip was not rendered"))
                };

                let items = (1..=4_u64).map(|tab| item(tab, cx)).collect::<Vec<_>>();
                let item_bounds = |tab: u64| items[tab as usize - 1];
                let strip = cx
                    .debug_bounds("tab-items")
                    .expect("the Tab item strip was not rendered");
                let forward = item_bounds(1).right() == item_bounds(2).left();
                let leading_edge = |bounds: gpui::Bounds<Pixels>| {
                    if forward {
                        bounds.left()
                    } else {
                        bounds.right()
                    }
                };
                let trailing_edge = |bounds: gpui::Bounds<Pixels>| {
                    if forward {
                        bounds.right()
                    } else {
                        bounds.left()
                    }
                };
                let mut boundaries = vec![(
                    "tab-separator-start-1".to_owned(),
                    vec![1_u64],
                    leading_edge(item_bounds(1)),
                    sidebar_visible && selected != 1,
                )];
                for leading in 1..4_u64 {
                    let trailing = leading + 1;
                    let shared_edge = trailing_edge(item_bounds(leading));
                    assert_eq!(
                        shared_edge,
                        leading_edge(item_bounds(trailing)),
                        "neighbouring Tabs {leading} and {trailing} should keep contiguous hit \
                         targets with Tab {selected} selected"
                    );
                    boundaries.push((
                        format!("tab-separator-{leading}-{trailing}"),
                        vec![leading, trailing],
                        shared_edge,
                        leading != selected && trailing != selected,
                    ));
                }
                boundaries.push((
                    "tab-separator-4-end".to_owned(),
                    vec![4],
                    trailing_edge(item_bounds(4)),
                    selected != 4,
                ));

                for (selector, tabs, edge, expected) in boundaries {
                    let separator = cx.debug_bounds(leaked_selector(selector.clone()));
                    match (separator, expected) {
                        (Some(separator), true) => {
                            assert_eq!(
                                separator.size.width,
                                px(TAB_SEPARATOR_WIDTH),
                                "{selector} should stay a one-point hairline at {density:?} \
                                 density, got {separator:?}"
                            );
                            assert!(
                                on_device_pixels(separator.left())
                                    && on_device_pixels(separator.size.width),
                                "{selector} should be laid out on whole device pixels at scale \
                                 {scale_factor}, got {separator:?}"
                            );
                            let owner = item_bounds(tabs[0]);
                            assert!(
                                separator.size.height == expected_length
                                    && separator.size.height < owner.size.height,
                                "{selector} should scale to {expected_length:?} at {density:?} \
                                 density and stay shorter than the Tab, got {separator:?}"
                            );
                            assert!(
                                (separator.center().y - owner.center().y).abs() <= px(0.5),
                                "{selector} should be centred on the bar, got {separator:?}"
                            );
                            // A strip end belongs to the strip; a mark between Tabs belongs to a Tab.
                            assert!(
                                if tabs.len() == 1 {
                                    within(separator, strip)
                                } else {
                                    tabs.iter().any(|&tab| within(separator, item_bounds(tab)))
                                },
                                "{selector} should paint inside its strip or one of Tabs {tabs:?} \
                                 rather than across their edge, got {separator:?}"
                            );
                            assert!(
                                separator.left() == edge || separator.right() == edge,
                                "{selector} should rest against its boundary at {edge:?}, got \
                                 {separator:?}"
                            );
                            for &tab in &tabs {
                                let chip = chip(tab, cx);
                                assert!(
                                    !overlaps(separator, chip),
                                    "{selector} should stay clear of Tab {tab}'s chip, got \
                                     {separator:?} and {chip:?}"
                                );
                            }
                        }
                        (None, false) => {}
                        (Some(_), false) => panic!(
                            "{selector} should not be drawn with Tab {selected} selected and \
                             sidebar_visible={sidebar_visible}"
                        ),
                        (None, true) => panic!(
                            "{selector} should be drawn with Tab {selected} selected and \
                             sidebar_visible={sidebar_visible}"
                        ),
                    }
                }

                // The mark is paint only: a press on it lands on the Tab that contains it, and a press
                // just across the shared edge lands on the neighbour.
                let (leading, trailing, across) = match selected {
                    4 => (1, 2, false),
                    1 => (3, 4, true),
                    _ => continue,
                };
                let separator = cx
                    .debug_bounds(leaked_selector(format!(
                        "tab-separator-{leading}-{trailing}"
                    )))
                    .expect("the boundary between two inactive Tabs should be marked");
                let leading_item = item(leading, cx);
                let (owner, neighbour) = if within(separator, leading_item) {
                    (leading, trailing)
                } else {
                    (trailing, leading)
                };
                let position = if across {
                    let owner_item = item(owner, cx);
                    let x = if separator.center().x > owner_item.center().x {
                        separator.right() + px(1.0)
                    } else {
                        separator.left() - px(1.0)
                    };
                    point(x, separator.center().y)
                } else {
                    separator.center()
                };
                cx.simulate_mouse_move(position, None, Modifiers::none());
                cx.simulate_click(position, Modifiers::none());
                cx.run_until_parked();
                assert_eq!(
                    manager.read_with(cx, |manager, _| manager.tabs.active_tab_id()),
                    TabId::new(if across { neighbour } else { owner })
                );
            }
        }
    }

    #[gpui::test]
    fn separators_should_mark_only_boundaries_between_inactive_tabs(cx: &mut TestAppContext) {
        assert_separators_mark_only_inactive_neighbours(
            cx,
            spaceterm_ui::TextDirection::LeftToRight,
        );
    }

    #[gpui::test]
    fn separators_should_follow_tab_boundaries_under_right_to_left_text(cx: &mut TestAppContext) {
        assert_separators_mark_only_inactive_neighbours(
            cx,
            spaceterm_ui::TextDirection::RightToLeft,
        );
    }

    #[gpui::test]
    fn tabs_publish_a_tab_list_that_selects_on_press(cx: &mut TestAppContext) {
        use gpui::accesskit::Action;
        use spaceterm_ui::a11y_testing::{A11yTree, perform};

        let (manager, _records, cx) = tab_manager(cx);
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.create_tab(window, cx));
        });
        cx.run_until_parked();

        let tree = A11yTree::read(cx);
        let list = tree.node("Tabs");
        assert_eq!(list["aria"]["role"], "TabList");
        let tabs = tree.children(list);
        let tabs = tabs
            .iter()
            .filter(|node| node["aria"]["role"] == "Tab")
            .collect::<Vec<_>>();
        assert_eq!(tabs.len(), 2);
        assert!(tabs.iter().all(|tab| tab["aria"]["label"] == "Terminal"));
        assert_eq!(
            tabs.iter()
                .map(|tab| tab["aria"]["selected"] == true)
                .collect::<Vec<_>>(),
            [false, true]
        );

        perform(cx, tabs[0], Action::Click);
        let (first, active) = manager.read_with(cx, |manager, _| {
            let first = manager.tabs.iter().next().map(|(tab_id, _)| tab_id);
            (first, manager.tabs.active_tab_id())
        });
        assert_eq!(first, Some(active));
        let tree = A11yTree::read(cx);
        let selected = tree
            .with_role("Tab")
            .iter()
            .map(|tab| tab["aria"]["selected"] == true)
            .collect::<Vec<_>>();
        assert_eq!(selected, [true, false]);

        perform(cx, tree.with_role("Tab")[1], Action::ShowContextMenu);
        assert_eq!(A11yTree::read(cx).with_role("Menu").len(), 1);
    }

    /// Chooses an item of the open menu, which may share its name with a Tab bar control.
    fn perform_menu_item(cx: &mut VisualTestContext, label: &str) {
        use spaceterm_ui::a11y_testing::{A11yTree, perform};

        let tree = A11yTree::read(cx);
        let menu = tree.with_role("Menu")[0];
        let item = tree
            .children(menu)
            .into_iter()
            .find(|item| item["aria"]["label"] == label)
            .unwrap_or_else(|| panic!("the open menu has no {label:?} item"));
        perform(cx, item, gpui::accesskit::Action::Click);
    }

    #[gpui::test]
    fn a_focused_tab_opens_its_menu_from_the_keyboard_and_takes_focus_back(
        cx: &mut TestAppContext,
    ) {
        use gpui::accesskit::Action;
        use spaceterm_ui::a11y_testing::{A11yTree, node_id, perform, supports};

        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        let tree = A11yTree::read(cx);
        let first = tree.with_role("Tab")[0];
        assert!(supports(first, Action::Focus));
        let tab = node_id(first);
        perform(cx, first, Action::Focus);
        assert_eq!(A11yTree::read(cx).focused().map(node_id), Some(tab));

        cx.simulate_keystrokes("shift-f10");
        assert_eq!(A11yTree::read(cx).with_role("Menu").len(), 1);
        cx.simulate_keystrokes("escape");
        let tree = A11yTree::read(cx);
        assert!(tree.with_role("Menu").is_empty());
        assert_eq!(tree.focused().map(node_id), Some(tab));

        // Moving the Tab keeps focus on it in its new place.
        cx.simulate_keystrokes("shift-f10");
        perform_menu_item(cx, "Move Tab Right");
        assert_eq!(tab_order(&manager, cx), vec![2, 1]);
        let tree = A11yTree::read(cx);
        assert_eq!(tree.focused().map(node_id), Some(tab));
        assert_eq!(node_id(tree.with_role("Tab")[1]), tab);

        // New Tab moves focus to the new Tab's terminal on purpose.
        cx.simulate_keystrokes("shift-f10");
        perform_menu_item(cx, "New Tab");
        assert!(cx.update(|window, cx| {
            manager
                .read(cx)
                .focused_terminal_has_input_focus(window, cx)
        }));
    }

    #[gpui::test]
    fn closing_a_focused_inactive_tab_returns_focus_to_the_active_terminal(
        cx: &mut TestAppContext,
    ) {
        use gpui::accesskit::Action;
        use spaceterm_ui::a11y_testing::{A11yTree, perform};

        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        let tree = A11yTree::read(cx);
        perform(cx, tree.with_role("Tab")[0], Action::Focus);
        cx.simulate_keystrokes("shift-f10");
        perform_menu_item(cx, "Close Tab");

        assert_eq!(tab_order(&manager, cx), vec![2]);
        assert!(cx.update(|window, cx| {
            manager
                .read(cx)
                .focused_terminal_has_input_focus(window, cx)
        }));
    }

    #[gpui::test]
    fn tab_bar_should_follow_operating_system_window_activation(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        let active_before = manager.read_with(cx, |manager, _| manager.rendered_window_active);

        cx.deactivate_window();
        cx.run_until_parked();
        let inactive = manager.read_with(cx, |manager, _| manager.rendered_window_active);

        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let active_after = manager.read_with(cx, |manager, _| manager.rendered_window_active);

        assert_eq!((active_before, inactive, active_after), (true, false, true));
    }

    #[gpui::test]
    fn tab_bar_should_start_after_the_persistent_sidebar_chrome(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = tab_manager(cx);
        let root = cx
            .debug_bounds("tab-manager")
            .expect("the Tab manager was not rendered");
        let spacer = cx
            .debug_bounds("tab-manager-top-spacer")
            .expect("the persistent top-left spacer was not rendered");
        let bar = cx
            .debug_bounds("tab-bar")
            .expect("the Tab bar was not rendered");

        let chrome_height = cx.update(|_, cx| {
            let appearance = crate::ui::appearance::chrome(cx);
            crate::ui::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx)
                .top_chrome_height(appearance.top_height())
        });
        assert_eq!(
            (spacer.origin, spacer.size, bar.origin.x),
            (
                root.origin,
                gpui::size(px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH), chrome_height),
                root.origin.x + px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            )
        );
    }

    #[gpui::test]
    fn hiding_sidebar_should_expand_content_and_use_the_supplied_top_chrome_width(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = tab_manager(cx);
        let root = cx
            .debug_bounds("tab-manager")
            .expect("the Tab manager was not rendered");
        let visible_content = cx
            .debug_bounds("tab-manager-content")
            .expect("the Tab content was not rendered");
        let visible_bar = cx
            .debug_bounds("tab-bar")
            .expect("the Tab bar was not rendered");

        manager.update(cx, |manager, cx| {
            manager.set_sidebar_layout(
                false,
                px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
                px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH),
                cx,
            );
        });
        cx.run_until_parked();

        let hidden_content = cx
            .debug_bounds("tab-manager-content")
            .expect("the expanded Tab content was not rendered");
        let hidden_bar = cx
            .debug_bounds("tab-bar")
            .expect("the Tab bar was not rendered");
        assert_eq!(
            (
                visible_content.origin.x,
                hidden_content.origin.x,
                visible_bar.origin.x,
                hidden_bar.origin.x,
            ),
            (
                root.origin.x + px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
                root.origin.x,
                root.origin.x + px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
                root.origin.x + px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH),
            )
        );
    }

    #[gpui::test]
    fn command_t_should_create_and_activate_a_new_tab(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);

        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.tabs.len(),
                manager.tabs.active_tab_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (2, TabId::new(2), Vec::new()));
    }

    #[gpui::test]
    fn command_number_shortcuts_should_activate_tabs_by_position(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        for _ in 1..9 {
            cx.simulate_keystrokes("cmd-t");
            cx.run_until_parked();
        }

        let mut active_tab_ids = Vec::new();
        for shortcut in [
            "cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-5", "cmd-6", "cmd-7", "cmd-8", "cmd-9",
        ] {
            cx.simulate_keystrokes(shortcut);
            cx.run_until_parked();
            active_tab_ids.push(manager.read_with(cx, |manager, _| manager.tabs.active_tab_id()));
        }

        assert_eq!(active_tab_ids, (1..=9).map(TabId::new).collect::<Vec<_>>());
    }

    #[gpui::test]
    fn next_and_previous_tab_should_wrap_around_the_workspace(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        for _ in 1..3 {
            cx.simulate_keystrokes("cmd-t");
            cx.run_until_parked();
        }

        let mut active_tab_ids = Vec::new();
        for shortcut in ["cmd-}", "cmd-}", "cmd-{", "cmd-{", "cmd-{"] {
            cx.simulate_keystrokes(shortcut);
            cx.run_until_parked();
            active_tab_ids.push(manager.read_with(cx, |manager, _| manager.tabs.active_tab_id()));
        }

        assert_eq!(
            active_tab_ids,
            [1, 2, 1, 3, 2].map(TabId::new),
            "Next Tab and Previous Tab should step through the Tab order and wrap at both ends"
        );
    }

    #[gpui::test]
    fn command_key_profile_should_leave_control_tab_to_the_terminal(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        for _ in 1..3 {
            cx.simulate_keystrokes("cmd-t");
            cx.run_until_parked();
        }
        let active_tab_id = manager.read_with(cx, |manager, _| manager.tabs.active_tab_id());
        let key_inputs = |records: &TestTerminalSessionRecords| {
            records
                .commands()
                .into_iter()
                .filter(|call| matches!(call.command, RecordedCommand::Key(_)))
                .count()
        };
        let before = key_inputs(&records);

        let mut active_tab_ids = Vec::new();
        for shortcut in ["ctrl-tab", "ctrl-shift-tab"] {
            cx.simulate_keystrokes(shortcut);
            cx.run_until_parked();
            active_tab_ids.push(manager.read_with(cx, |manager, _| manager.tabs.active_tab_id()));
        }

        assert_eq!(
            active_tab_ids, [active_tab_id; 2],
            "Control-Tab is not a Tab navigation Shortcut in the Command-key profile"
        );
        assert_eq!(
            key_inputs(&records) - before,
            2,
            "Control-Tab and Control-Shift-Tab should reach the focused terminal session"
        );
    }

    #[gpui::test]
    fn move_tab_should_reorder_the_active_tab_and_stop_at_the_ends(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        for _ in 1..3 {
            cx.simulate_keystrokes("cmd-t");
            cx.run_until_parked();
        }
        let order =
            |cx: &mut VisualTestContext| manager.read_with(cx, |manager, _| manager.tab_ids());

        let mut orders = Vec::new();
        for right in [true, false, false, false] {
            if right {
                cx.dispatch_action(MoveTabRight);
            } else {
                cx.dispatch_action(MoveTabLeft);
            }
            cx.run_until_parked();
            orders.push(order(cx));
        }

        assert_eq!(
            orders,
            [[1, 2, 3], [1, 3, 2], [3, 1, 2], [3, 1, 2]]
                .map(|order| order.map(TabId::new).to_vec()),
        );
        assert_eq!(
            manager.read_with(cx, |manager, _| manager.tabs.active_tab_id()),
            TabId::new(3),
            "moving a Tab should keep it active"
        );
    }

    #[gpui::test]
    fn unavailable_command_number_shortcut_should_preserve_the_active_tab(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);

        cx.simulate_keystrokes("cmd-9");
        cx.run_until_parked();

        let active_tab_id = manager.read_with(cx, |manager, _| manager.tabs.active_tab_id());
        assert_eq!(active_tab_id, TabId::new(1));
    }

    #[gpui::test]
    fn create_button_should_create_and_activate_without_dropping_the_inactive_tab(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = tab_manager(cx);
        let first_entity_id =
            manager.read_with(cx, |manager, _| manager.tabs.active_tab().entity_id());

        click("create-tab-button", cx);

        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.tabs.len(),
                manager.tabs.active_tab_id(),
                manager.tabs.tab(TabId::new(1)).map(Entity::entity_id),
                manager
                    .tabs
                    .tab(TabId::new(1))
                    .is_some_and(|tab| !tab.read(cx).is_active()),
                manager.tabs.active_tab().read(cx).is_active(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(
            state,
            (
                2,
                TabId::new(2),
                Some(first_entity_id),
                true,
                true,
                Vec::new(),
            )
        );
    }

    /// The Tab's activity prefers an explicit Terminal title, then the running command, then the
    /// host shell name, and is followed by the Current Directory leaf.
    #[gpui::test]
    fn tab_title_should_present_the_terminal_title_or_running_command_then_the_directory_leaf(
        cx: &mut TestAppContext,
    ) {
        use crate::terminal::metadata::{CommandMetadata, CommandState, TitleProvenance};
        let (manager, records, cx) = tab_manager(cx);
        let tab_title = |cx: &mut VisualTestContext| {
            manager.read_with(cx, |manager, cx| {
                manager.tabs.active_tab().read(cx).tab_title()
            })
        };

        report_metadata(&records, 1, 1, |metadata| {
            metadata.shell_name = Arc::from("fish");
            metadata.directory.path = Arc::from("/tmp/api");
            metadata.command = Some(CommandMetadata {
                line: Arc::from("cargo test"),
                state: CommandState::Running,
            });
        });
        cx.run_until_parked();
        assert_eq!(tab_title(cx).as_ref(), "cargo test · api");

        report_metadata(&records, 1, 2, |metadata| {
            metadata.shell_name = Arc::from("fish");
            metadata.directory.path = Arc::from("/tmp/api");
            metadata.title.value = Arc::from("✳ Opaque ⠋ title");
            metadata.title.provenance = TitleProvenance::TerminalControl;
            metadata.command = Some(CommandMetadata {
                line: Arc::from("cargo test"),
                state: CommandState::Running,
            });
        });
        cx.run_until_parked();
        // The glyph the program draws at the front goes; the rest of its title stays opaque.
        assert_eq!(tab_title(cx).as_ref(), "Opaque ⠋ title · api");

        report_metadata(&records, 1, 3, |metadata| {
            metadata.shell_name = Arc::from("fish");
            metadata.directory.path = Arc::from("/tmp/api");
        });
        cx.run_until_parked();
        assert_eq!(tab_title(cx).as_ref(), "fish · api");
    }

    #[gpui::test]
    fn split_tab_title_should_follow_the_focused_pane_without_a_pane_count(
        cx: &mut TestAppContext,
    ) {
        use crate::terminal::metadata::TitleProvenance;
        let (manager, records, cx) = tab_manager(cx);
        let tab_title = |cx: &mut VisualTestContext| {
            manager.read_with(cx, |manager, cx| {
                manager.tabs.active_tab().read(cx).tab_title()
            })
        };
        // A reported directory would become the split's inherited, locally validated directory.
        report_metadata(&records, 1, 1, |metadata| {
            metadata.title.value = Arc::from("editor");
            metadata.title.provenance = TitleProvenance::TerminalControl;
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();
        report_metadata(&records, 2, 1, |metadata| {
            metadata.directory.path = Arc::from("/tmp/second");
        });
        cx.run_until_parked();
        let split_title = tab_title(cx);
        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        let restored_title = tab_title(cx);

        assert_eq!(
            (split_title.as_ref(), restored_title.as_ref()),
            ("Terminal · second", "editor")
        );
        assert!(cx.debug_bounds("tab-pane-count-1").is_none());
    }

    #[gpui::test]
    fn split_tab_should_refresh_rendered_identity_for_focused_pane_changes(
        cx: &mut TestAppContext,
    ) {
        let (_manager, records, cx) = tab_manager(cx);
        // A Pane's place is the directory its server reports, which the first Pane reports now.
        report_current_directory(&records, 1, 1, "/tmp/first", false);
        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();
        report_current_directory(&records, 2, 1, "/tmp/second-pane-with-a-long-name", false);
        cx.run_until_parked();
        let focused_second_before_caption_change = cx
            .debug_bounds("tab-place-1")
            .expect("the focused second Pane's initial place should render");

        report_current_directory(&records, 2, 2, "/tmp/x", false);
        cx.run_until_parked();
        let focused_second = cx
            .debug_bounds("tab-place-1")
            .expect("the focused second Pane's changed place should render");
        assert!(
            focused_second_before_caption_change.size.width > focused_second.size.width,
            "the rendered Tab should replace the focused Pane's changed place: \
             {focused_second_before_caption_change:?} {focused_second:?}"
        );

        cx.simulate_keystrokes("cmd-alt-left");
        cx.run_until_parked();
        let focused_first = cx
            .debug_bounds("tab-place-1")
            .expect("the focused first Pane's place should render");

        assert!(
            focused_first.size.width > focused_second.size.width,
            "the rendered Tab should replace the second Pane's short place after focus changes: \
             {focused_second:?} {focused_first:?}"
        );
    }

    #[gpui::test]
    fn tab_activity_without_a_place_should_use_the_full_identity_width(cx: &mut TestAppContext) {
        use crate::terminal::metadata::TitleProvenance;
        let (manager, records, cx) = tab_manager(cx);
        let activity = "long-activity-name-that-needs-the-full-tab-identity-width";
        report_metadata(&records, 1, 1, |metadata| {
            metadata.directory.path = Arc::from(format!("/tmp/{activity}"));
            metadata.title.value = Arc::from(activity);
            metadata.title.provenance = TitleProvenance::TerminalControl;
        });
        cx.run_until_parked();

        let place = manager.read_with(cx, |manager, cx| {
            manager
                .tabs
                .active_tab()
                .read(cx)
                .tab_identity()
                .place
                .clone()
        });
        let identity = cx.debug_bounds("tab-identity-1").unwrap();
        let activity = cx.debug_bounds("tab-activity-1").unwrap();
        assert!(place.is_empty(), "expected no place, got {place:?}");
        assert!(
            activity.size.width > identity.size.width * 0.9,
            "place-less activity should fill its identity: {activity:?} {identity:?}"
        );
    }

    /// A Tab reads `<status glyph> <activity> · <place>`, and neither the account, the Workspace
    /// name, nor a Pane count competes with those segments for room.
    #[gpui::test]
    fn tab_should_present_status_glyph_activity_and_place_in_order(cx: &mut TestAppContext) {
        use crate::terminal::metadata::{ProgressMetadata, TitleProvenance};
        let (manager, records, cx) = tab_manager(cx);
        report_metadata(&records, 1, 1, |metadata| {
            metadata.directory.path = Arc::from("/tmp/api");
            metadata.title.value = Arc::from("agent");
            metadata.title.provenance = TitleProvenance::TerminalControl;
            metadata.progress = ProgressMetadata::Normal(40);
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-alt-left");
        cx.run_until_parked();
        manager.update(cx, |manager, cx| {
            manager.tabs.active_tab().update(cx, |view, cx| {
                view.set_test_attention(PaneId::new(2), 1, cx);
            });
        });
        cx.run_until_parked();

        let attention = cx
            .debug_bounds("tab-status-1-attention")
            .expect("a Tab whose Pane has unread attention should blink its glyph");
        let progress = cx
            .debug_bounds("tab-status-1-normal")
            .expect("the Tab should present its reported progress");
        let origin = cx
            .debug_bounds("tab-origin-1-local")
            .expect("the Tab should carry its terminal glyph");
        let activity = cx.debug_bounds("tab-activity-1").unwrap();
        let place = cx.debug_bounds("tab-place-1").unwrap();
        let glyph = cx.update(|_, cx| {
            crate::ui::appearance::chrome(cx)
                .icons
                .metrics(IconRole::Status)
                .glyph_size
        });
        // Attention and progress are the terminal glyph itself, not separate marks beside it.
        for status in [attention, progress] {
            assert_eq!(status.size, gpui::size(glyph, glyph));
            assert!(
                status.left() >= origin.left() && status.right() <= origin.right(),
                "{status:?} should sit on the glyph {origin:?}"
            );
        }
        for (leading, trailing) in [(origin, activity), (activity, place)] {
            assert!(
                leading.right() <= trailing.left(),
                "{leading:?} should precede {trailing:?}"
            );
        }
        assert!(cx.debug_bounds("tab-pane-count-1").is_none());
        assert!(cx.debug_bounds("tab-account-1").is_none());

        // A single-Pane Tab carries the same mark as the Pane Caption under it.
        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        let remaining = manager.read_with(cx, |manager, cx| {
            manager.tabs.active_tab().read(cx).focused_pane_id()
        });
        manager.update(cx, |manager, cx| {
            manager.tabs.active_tab().update(cx, |view, cx| {
                view.set_test_attention(remaining, 1, cx);
            });
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("tab-status-1-attention").is_some());
        assert!(
            cx.debug_bounds(format!("pane-status-{}-attention", remaining.get()).leak())
                .is_some()
        );
    }

    /// A program's own glyph takes the Terminal Session's slot rather than sitting beside it.
    #[gpui::test]
    fn local_reported_glyph_should_take_the_terminal_session_glyph(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        assert_reported_glyph(&manager, &records, false, cx);
    }

    #[gpui::test]
    fn remote_reported_glyph_should_take_the_terminal_session_glyph(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        assert_reported_glyph(&manager, &records, true, cx);
    }

    #[gpui::test]
    fn fatal_failure_should_clear_cached_pane_and_tab_progress(cx: &mut TestAppContext) {
        use super::super::terminal_status::TerminalProgress;
        use crate::terminal::metadata::ProgressMetadata;

        let (manager, records, cx) = tab_manager(cx);
        report_metadata(&records, 1, 1, |metadata| {
            metadata.progress = ProgressMetadata::Normal(45);
        });
        cx.run_until_parked();
        records
            .event_sender(1)
            .unwrap()
            .try_send(TerminalSessionEvent::Failed(
                TerminalSessionFailure::Runtime(crate::terminal::TerminalFailure::emulator(
                    "session-runtime",
                )),
            ))
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            manager.read_with(cx, |manager, cx| {
                let view = manager.tabs.active_tab().read(cx);
                (view.cached_focused_progress(), view.tab_identity().progress)
            }),
            (TerminalProgress::None, TerminalProgress::None)
        );
    }

    fn assert_reported_glyph(
        manager: &Entity<TabManager>,
        records: &TestTerminalSessionRecords,
        remote: bool,
        cx: &mut VisualTestContext,
    ) {
        use crate::terminal::metadata::TitleProvenance;
        let identity = |cx: &mut VisualTestContext| {
            manager.read_with(cx, |manager, cx| {
                manager.tabs.active_tab().read(cx).tab_identity()
            })
        };
        let report = |records: &TestTerminalSessionRecords,
                      generation: u64,
                      title: &'static str,
                      cx: &mut VisualTestContext| {
            report_metadata(records, 1, generation, move |metadata| {
                metadata.directory.path = Arc::from("/srv/app");
                if remote {
                    metadata.context = remote_metadata_context("/srv/app");
                }
                metadata.title.value = Arc::from(title);
                metadata.title.provenance = TitleProvenance::TerminalControl;
            });
            cx.run_until_parked();
        };

        report(records, 1, "\u{2733} Claude Code", cx);
        let reported = identity(cx);
        assert_eq!(
            (
                reported.remote,
                reported.glyph.as_ref().map(|glyph| glyph.as_ref()),
                reported.activity.as_ref()
            ),
            (remote, Some("\u{2733}"), "Claude Code")
        );
        // The Tab carries one glyph, in the slot the Terminal Session's own glyph would have taken.
        let origin = if remote {
            "tab-origin-1-remote"
        } else {
            "tab-origin-1-local"
        };
        let glyph = cx.update(|_, cx| {
            crate::ui::appearance::chrome(cx)
                .icons
                .metrics(IconRole::Status)
                .glyph_size
        });
        assert_eq!(
            cx.debug_bounds(origin)
                .expect("the Tab lost its glyph")
                .size,
            gpui::size(glyph, glyph)
        );

        // A title without a glyph leaves the Terminal Session with its own.
        report(records, 2, "cargo test", cx);
        let plain = identity(cx);
        assert_eq!(
            (
                plain.glyph.as_ref().map(|glyph| glyph.as_ref()),
                plain.activity.as_ref()
            ),
            (None, "cargo test")
        );
    }

    /// Local and Remote Terminal Sessions present every OSC 9;4 state the same way in the Tab and
    /// the Pane Caption, independently of the title, and removing the status leaves no mark.
    #[gpui::test]
    fn local_progress_should_present_each_state_in_tab_and_caption(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        assert_progress_states(&manager, &records, false, cx);
    }

    #[gpui::test]
    fn remote_progress_should_present_each_state_in_tab_and_caption(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        assert_progress_states(&manager, &records, true, cx);
    }

    fn assert_progress_states(
        manager: &Entity<TabManager>,
        records: &TestTerminalSessionRecords,
        remote: bool,
        cx: &mut VisualTestContext,
    ) {
        use super::super::terminal_status::TerminalProgress;
        use crate::terminal::metadata::{ProgressMetadata, TitleProvenance};
        let states = [
            (
                ProgressMetadata::Normal(55),
                TerminalProgress::Normal(55),
                Some(["tab-status-1-normal", "pane-status-1-normal"]),
            ),
            (
                ProgressMetadata::Indeterminate,
                TerminalProgress::Indeterminate,
                Some(["tab-status-1-indeterminate", "pane-status-1-indeterminate"]),
            ),
            (
                ProgressMetadata::Error(10),
                TerminalProgress::Error(10),
                Some(["tab-status-1-error", "pane-status-1-error"]),
            ),
            (
                ProgressMetadata::Paused(20),
                TerminalProgress::Paused(20),
                Some(["tab-status-1-paused", "pane-status-1-paused"]),
            ),
            (ProgressMetadata::None, TerminalProgress::None, None),
        ];
        for (generation, (progress, presented, selectors)) in (1..).zip(states) {
            report_metadata(records, 1, generation, |metadata| {
                metadata.directory.path = Arc::from("/srv/app");
                if remote {
                    metadata.context = remote_metadata_context("/srv/app");
                }
                metadata.title.value = Arc::from("build");
                metadata.title.provenance = TitleProvenance::TerminalControl;
                metadata.progress = progress;
            });
            cx.run_until_parked();
            // Progress is presented in the glyph and never replaces the title.
            let identity = manager.read_with(cx, |manager, cx| {
                manager.tabs.active_tab().read(cx).tab_identity()
            });
            assert_eq!(
                (
                    identity.remote,
                    identity.progress,
                    identity.activity.as_ref()
                ),
                (remote, presented, "build")
            );
            // A rendered frame keeps no record of what it stopped drawing, so rendering is checked
            // for presence only; the typed identity above covers removal.
            for selector in selectors.into_iter().flatten() {
                assert!(
                    cx.debug_bounds(selector).is_some(),
                    "remote={remote} {selector} {progress:?}"
                );
            }
            if progress == ProgressMetadata::Indeterminate {
                for selector in [
                    "tab-status-1-progress-frame",
                    "pane-status-1-progress-frame",
                ] {
                    assert!(
                        cx.debug_bounds(selector).is_some(),
                        "remote={remote} {selector}"
                    );
                }
            }
        }
    }

    #[gpui::test]
    fn local_status_follows_reported_activity_and_command_lifetime(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        assert_reported_activity_lifecycle(&manager, &records, false, cx);
    }

    #[gpui::test]
    fn remote_status_follows_reported_activity_and_command_lifetime(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        assert_reported_activity_lifecycle(&manager, &records, true, cx);
    }

    fn assert_reported_activity_lifecycle(
        manager: &Entity<TabManager>,
        records: &TestTerminalSessionRecords,
        remote: bool,
        cx: &mut VisualTestContext,
    ) {
        use super::super::terminal_status::TerminalProgress;
        use crate::terminal::metadata::{MetadataTracker, TerminalMetadataContext};
        use std::time::{Duration, Instant};

        let epoch = Instant::now();
        let context = if remote {
            remote_metadata_context("/srv/app")
        } else {
            TerminalMetadataContext::local(
                crate::local_path::LocalPathSemantics::Posix,
                "/tmp/app",
                Default::default(),
            )
        };
        let mut tracker = MetadataTracker::new_with_context(context, "zsh", epoch);
        let publish = |tracker: &MetadataTracker, cx: &mut VisualTestContext| {
            records.report_metadata(1, |metadata| {
                let revision = metadata.revision;
                *metadata = (*tracker.snapshot()).clone();
                metadata.revision = revision;
            });
            cx.run_until_parked();
            manager.read_with(cx, |manager, cx| {
                manager.tabs.active_tab().read(cx).tab_identity()
            })
        };

        tracker.apply_semantic_prompt("C;cmdline=interactive", epoch);
        tracker.set_reported_title("✳ agent", epoch);
        tracker.advance_status(epoch + Duration::from_secs(60));
        assert_eq!(publish(&tracker, cx).progress, TerminalProgress::None);
        assert!(cx.debug_bounds("tab-status-1-progress-frame").is_none());
        assert!(cx.debug_bounds("pane-status-1-progress-frame").is_none());

        let started = epoch + Duration::from_secs(61);
        tracker.set_reported_title("◐ agent", started);
        let pending = publish(&tracker, cx);
        assert_eq!(
            pending.glyph.as_ref().map(|glyph| glyph.as_ref()),
            Some("✳")
        );
        assert_eq!(pending.progress, TerminalProgress::None);
        // Claude holds its first title frame for roughly a second. A title rename during that
        // interval must update the words without exposing the program's temporary loader.
        tracker.advance_status(started + Duration::from_millis(700));
        tracker.set_reported_title("◐ renamed", started + Duration::from_millis(800));
        let pending = publish(&tracker, cx);
        assert_eq!(pending.activity.as_ref(), "renamed");
        assert_eq!(
            pending.glyph.as_ref().map(|glyph| glyph.as_ref()),
            Some("✳")
        );
        assert_eq!(pending.progress, TerminalProgress::None);
        tracker.set_reported_title("◑ renamed", started + Duration::from_millis(1000));
        assert_eq!(
            publish(&tracker, cx).progress,
            TerminalProgress::TitleActivity
        );
        assert!(cx.debug_bounds("tab-status-1-progress-frame").is_some());
        assert!(cx.debug_bounds("pane-status-1-progress-frame").is_some());

        tracker.set_reported_title("π - agent", started + Duration::from_secs(1));
        let ready = publish(&tracker, cx);
        assert_eq!(ready.glyph.as_ref().map(|glyph| glyph.as_ref()), Some("π"));
        assert_eq!(ready.progress, TerminalProgress::None);
        assert!(cx.debug_bounds("tab-status-1-progress-frame").is_none());
        assert!(cx.debug_bounds("pane-status-1-progress-frame").is_none());

        tracker.apply_progress_report(3, None, started + Duration::from_secs(2));
        assert_eq!(
            publish(&tracker, cx).progress,
            TerminalProgress::Indeterminate
        );
        assert!(cx.debug_bounds("tab-status-1-progress-frame").is_some());
        assert!(cx.debug_bounds("pane-status-1-progress-frame").is_some());
        tracker.apply_progress_report(0, None, started + Duration::from_secs(3));
        assert_eq!(publish(&tracker, cx).progress, TerminalProgress::None);

        tracker.apply_semantic_prompt("D;0", started + Duration::from_secs(4));
        let finished = publish(&tracker, cx);
        assert_eq!(finished.glyph, None);
        assert_eq!(finished.progress, TerminalProgress::None);
        tracker.apply_semantic_prompt("C;cmdline=ls", started + Duration::from_secs(5));
        let next = publish(&tracker, cx);
        assert_eq!(next.activity.as_ref(), "ls");
        assert_eq!(next.glyph, None);
        assert_eq!(next.progress, TerminalProgress::None);
    }

    /// A hidden Tab gets no Screens, yet its item tracks the Terminal Session's title and progress.
    #[gpui::test]
    fn background_tab_should_follow_retained_title_and_progress(cx: &mut TestAppContext) {
        use super::super::terminal_status::TerminalProgress;
        use crate::terminal::metadata::{ProgressMetadata, TitleProvenance};
        let (manager, records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        records.report_metadata(1, |metadata| {
            metadata.title.value = Arc::from("agent");
            metadata.title.provenance = TitleProvenance::TerminalControl;
            metadata.progress = ProgressMetadata::Error(0);
        });
        cx.run_until_parked();

        let identity = manager.read_with(cx, |manager, cx| {
            manager
                .tabs
                .iter()
                .find(|(tab_id, _)| *tab_id == TabId::new(1))
                .map(|(_, view)| view.read(cx).tab_identity())
                .unwrap()
        });
        assert_eq!(
            (identity.activity.as_ref(), identity.progress),
            ("agent", TerminalProgress::Error(0))
        );
        assert!(cx.debug_bounds("tab-item-1-inactive").is_some());
        assert!(cx.debug_bounds("tab-status-1-error").is_some());
    }

    /// A narrow Tab gives up its words before it gives up its status glyph or its close control,
    /// keeping both controls inside the item bounds at the narrowest width.
    #[gpui::test]
    fn narrow_tabs_should_keep_status_glyph_and_close_within_item_bounds(cx: &mut TestAppContext) {
        use crate::terminal::metadata::{ProgressMetadata, TitleProvenance};
        let (_manager, records, cx) = tab_manager(cx);
        report_metadata(&records, 1, 1, |metadata| {
            metadata.title.value =
                Arc::from("a terminal title long enough to need truncating in a narrow Tab");
            metadata.title.provenance = TitleProvenance::TerminalControl;
            metadata.progress = ProgressMetadata::Indeterminate;
        });
        cx.run_until_parked();
        for _ in 0..6 {
            click("create-tab-button", cx);
        }
        cx.run_until_parked();

        let item = cx
            .debug_bounds("tab-item-1-inactive")
            .expect("the first Tab was not rendered");
        let origin = cx
            .debug_bounds("tab-origin-1-local")
            .expect("a narrowed Tab should keep its origin glyph");
        let progress = cx
            .debug_bounds("tab-status-1-indeterminate")
            .expect("a narrowed Tab should keep its status");
        let close = cx
            .debug_bounds("tab-close-button-1")
            .expect("a narrowed Tab should keep its close control");
        let minimum =
            cx.update(|_, cx| crate::ui::appearance::chrome(cx).spacing(TAB_ITEM_MINIMUM_WIDTH));

        assert!(
            item.size.width >= minimum,
            "got {item:?} against {minimum:?}"
        );
        for segment in [origin, progress, close] {
            assert!(
                segment.left() >= item.left() && segment.right() <= item.right(),
                "{segment:?} should stay inside its Tab {item:?}"
            );
        }
    }

    #[gpui::test]
    fn hover_close_button_should_close_its_tab_without_activating_it(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        click("create-tab-button", cx);

        click("tab-close-button-1", cx);

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.tabs.len(),
                manager.tabs.active_tab_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (1, TabId::new(2), vec![1]));
    }

    #[gpui::test]
    fn inactive_first_mouse_on_hidden_tab_close_does_not_close_the_tab(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        let close = cx
            .debug_bounds("tab-close-button-1")
            .expect("the inactive Tab close button was not rendered")
            .center();
        cx.deactivate_window();
        cx.run_until_parked();

        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: close,
            modifiers: Modifiers::none(),
            click_count: 1,
            first_mouse: true,
        });
        cx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: close,
            modifiers: Modifiers::none(),
            click_count: 1,
        });
        cx.run_until_parked();

        assert_eq!(
            manager.read_with(cx, |manager, _| manager.tabs.len()),
            2,
            "the activation click must not invoke an opacity-zero close action"
        );
        assert!(records.dropped_session_ids().is_empty());
    }

    #[gpui::test]
    fn inactive_first_mouse_can_close_the_visible_active_tab(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        let close = cx
            .debug_bounds("tab-close-button-2")
            .expect("the Active Tab close button was not rendered")
            .center();
        cx.deactivate_window();
        cx.run_until_parked();

        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: close,
            modifiers: Modifiers::none(),
            click_count: 1,
            first_mouse: true,
        });
        cx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: close,
            modifiers: Modifiers::none(),
            click_count: 1,
        });
        cx.run_until_parked();

        assert_eq!(
            manager.read_with(cx, |manager, _| manager.tabs.len()),
            1,
            "the visible active-Tab action remains available on the activation click"
        );
        assert_eq!(records.dropped_session_ids(), vec![2]);
    }

    #[gpui::test]
    fn active_tab_close_button_should_close_the_active_tab(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        click("create-tab-button", cx);

        click("tab-close-button-2", cx);

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.tabs.len(),
                manager.tabs.active_tab_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (1, TabId::new(1), vec![2]));
    }

    #[gpui::test]
    fn tab_close_button_should_use_a_compact_right_inset(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = tab_manager(cx);
        let item = cx
            .debug_bounds("tab-item-1-active")
            .expect("the Active Tab item was not rendered");
        let close_button = cx
            .debug_bounds("tab-close-button-1")
            .expect("the Active Tab close button was not rendered");
        let identity = cx
            .debug_bounds("tab-identity-1")
            .expect("the Active Tab identity was not rendered");

        assert_eq!(
            (
                item.origin.x + item.size.width - (close_button.origin.x + close_button.size.width),
                close_button.size,
            ),
            (px(TAB_ITEM_RIGHT_PADDING), gpui::size(px(20.0), px(20.0)),)
        );
        assert!(
            identity.right() + px(TAB_TRAILING_GAP) <= close_button.left(),
            "the visible Active Tab identity {identity:?} should stop before its floating Close control {close_button:?}"
        );
    }

    #[gpui::test]
    fn inactive_tab_identity_should_reserve_close_width_before_reveal(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);

        let item = cx
            .debug_bounds("tab-item-1-inactive")
            .expect("the inactive Tab item was not rendered");
        let title = cx
            .debug_bounds("tab-title-1")
            .expect("the inactive Tab title was not rendered");
        let close_button = cx
            .debug_bounds("tab-close-button-1")
            .expect("the inactive Tab close button was not rendered");

        assert_eq!(item.right() - title.right(), px(TAB_ITEM_RIGHT_PADDING));
        let resting_identity = cx
            .debug_bounds("tab-identity-1")
            .expect("the inactive Tab identity was not rendered");
        assert!(
            resting_identity.right() + px(TAB_TRAILING_GAP) <= close_button.left(),
            "the resting identity {resting_identity:?} should reserve the hidden Close slot {close_button:?}"
        );

        cx.simulate_mouse_move(item.center(), None, Modifiers::none());
        cx.run_until_parked();
        let hovered_identity = cx
            .debug_bounds("tab-identity-1")
            .expect("the hovered inactive Tab title was not rendered");
        assert_eq!(
            hovered_identity, resting_identity,
            "revealing Close must not change the identity's available width or truncation point"
        );
    }

    #[gpui::test]
    fn create_button_should_follow_fitting_tabs_and_move_back_after_closing(
        cx: &mut TestAppContext,
    ) {
        let (manager, _, cx) = tab_manager(cx);
        for (index, selector) in [
            "tab-item-1-active",
            "tab-item-2-active",
            "tab-item-3-active",
        ]
        .into_iter()
        .enumerate()
        {
            let tab = cx.debug_bounds(selector).unwrap();
            let button = cx.debug_bounds("create-tab-button").unwrap();
            let area = cx.debug_bounds("create-tab-area").unwrap();
            assert_eq!(area.left(), tab.right());
            assert_eq!(area.size, gpui::size(px(TAB_BAR_HEIGHT), tab.size.height));
            assert_eq!(button.center(), area.center());
            assert_eq!(button.center().y, tab.center().y);
            if index < 2 {
                click("create-tab-button", cx);
            }
        }
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.close_tab(TabId::new(3), window, cx);
            })
        });
        cx.run_until_parked();
        assert_eq!(
            cx.debug_bounds("create-tab-area").unwrap().left(),
            cx.debug_bounds("tab-item-2-active").unwrap().right(),
        );
    }

    #[gpui::test]
    fn create_button_should_stay_reachable_across_resize_and_sidebar_changes(
        cx: &mut TestAppContext,
    ) {
        let (manager, _, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);
        for width in [600.0, 1200.0] {
            cx.simulate_resize(gpui::size(px(width), px(600.0)));
            cx.run_until_parked();
            for visible in [false, true] {
                manager.update(cx, |manager, cx| {
                    manager.set_sidebar_layout(
                        visible,
                        px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
                        px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH),
                        cx,
                    )
                });
                cx.run_until_parked();
                let strip = cx.debug_bounds("tab-items").unwrap();
                let button = cx.debug_bounds("create-tab-button").unwrap();
                let area = cx.debug_bounds("create-tab-area").unwrap();
                let bar = cx.debug_bounds("tab-bar").unwrap();
                assert_eq!(area.left(), strip.right());
                assert!(area.right() <= bar.right());
                // The create control fills the band beneath the window's own edge.
                assert_eq!(
                    area.size,
                    gpui::size(px(TAB_BAR_HEIGHT), top_band_height(cx))
                );
                assert_eq!(area.bottom(), bar.bottom());
                assert_eq!(button.center(), area.center());
                assert_eq!(button.size, gpui::size(px(28.0), px(28.0)));
                if width == 1200.0 {
                    assert_eq!(
                        area.left(),
                        cx.debug_bounds("tab-item-3-active").unwrap().right()
                    );
                }
            }
        }
        click("create-tab-button", cx);
        assert_eq!(manager.read_with(cx, |manager, _| manager.tabs.len()), 4);
    }

    #[gpui::test]
    fn creating_tabs_should_scroll_the_active_tab_into_view(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);

        for _ in 0..20 {
            click("create-tab-button", cx);
        }

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.tabs.len(),
                manager.tabs.active_tab_id(),
                manager.tab_bar_scroll_handle.offset().x,
            )
        });
        assert_eq!((state.0, state.1), (21, TabId::new(21)));
        assert!(
            state.2 < px(0.0),
            "the Tab bar did not scroll; offset was {:?}",
            state.2
        );
        let strip = cx.debug_bounds("tab-items").unwrap();
        let button = cx.debug_bounds("create-tab-button").unwrap();
        let area = cx.debug_bounds("create-tab-area").unwrap();
        let bar = cx.debug_bounds("tab-bar").unwrap();
        assert_eq!(area.left(), strip.right());
        assert!(area.right() <= bar.right());
        // The create control fills the band beneath the window's own edge.
        assert_eq!(
            area.size,
            gpui::size(px(TAB_BAR_HEIGHT), top_band_height(cx))
        );
        assert_eq!(area.bottom(), bar.bottom());
        assert_eq!(button.center(), area.center());
        let active = cx.debug_bounds("tab-item-21-active").unwrap();
        assert!(active.left() >= strip.left());
        assert!(active.right() <= strip.right());
    }

    use crate::ui::drag_and_drop::{AnimatePresence, finish_slides};

    fn tab_order(manager: &Entity<TabManager>, cx: &mut VisualTestContext) -> Vec<u64> {
        manager.read_with(cx, |manager, _| {
            manager
                .tabs
                .iter()
                .map(|(tab_id, _)| tab_id.get())
                .collect()
        })
    }

    fn drag_tab(
        from: gpui::Point<Pixels>,
        path: &[gpui::Point<Pixels>],
        cx: &mut VisualTestContext,
    ) {
        cx.simulate_mouse_move(from, None, Modifiers::none());
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
        for position in path {
            cx.simulate_mouse_move(*position, Some(MouseButton::Left), Modifiers::none());
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn a_tab_held_still_while_it_grows_should_open_a_gap_as_wide_as_it_grows(
        cx: &mut TestAppContext,
    ) {
        let (_manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);
        finish_slides(cx);
        let first = cx.debug_bounds("tab-item-1-inactive").unwrap().origin;
        let rest = [
            "tab-item-2-inactive",
            "tab-item-3-inactive",
            "tab-item-4-active",
        ]
        .map(|selector| cx.debug_bounds(selector).unwrap().origin);
        click("tab-close-button-4", cx);
        finish_slides(cx);
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        click("create-tab-button", cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let grabbed = cx.debug_bounds("tab-item-5-active").unwrap().center();
        let held = first + point(px(4.0), px(10.0));

        drag_tab(grabbed, &[grabbed - point(px(8.0), px(0.0)), held], cx);
        for _ in 0..60 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(16));
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
        }

        assert_eq!(
            [
                "tab-item-1-inactive",
                "tab-item-2-inactive",
                "tab-item-3-inactive",
            ]
            .map(|selector| cx.debug_bounds(selector).map(|tab| tab.origin)),
            rest.map(Some),
            "Tabs must make room for the whole lifted Tab once it has grown, while the pointer \
             holds still"
        );
    }

    #[gpui::test]
    fn a_tab_dragged_while_it_grows_should_leave_every_tab_at_rest(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);
        finish_slides(cx);
        let rest = [1, 2, 3, 4].map(|tab| {
            cx.debug_bounds(if tab == 4 {
                "tab-item-4-active"
            } else {
                [
                    "",
                    "tab-item-1-inactive",
                    "tab-item-2-inactive",
                    "tab-item-3-inactive",
                ][tab]
            })
            .unwrap()
            .origin
        });
        click("tab-close-button-4", cx);
        finish_slides(cx);
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        click("create-tab-button", cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let grabbed = cx.debug_bounds("tab-item-5-active").unwrap().center();
        let release = rest[0] + point(px(4.0), px(10.0));

        drag_tab(grabbed, &[grabbed - point(px(8.0), px(0.0)), release], cx);
        finish_slides(cx);
        cx.simulate_mouse_up(release, MouseButton::Left, Modifiers::none());
        finish_slides(cx);

        assert_eq!(
            (
                tab_order(&manager, cx),
                [
                    "tab-item-5-active",
                    "tab-item-1-inactive",
                    "tab-item-2-inactive",
                    "tab-item-3-inactive",
                ]
                .map(|selector| cx.debug_bounds(selector).map(|tab| tab.origin)),
            ),
            (vec![5, 1, 2, 3], rest.map(Some)),
            "a Tab dropped after it finished growing must leave every Tab in its own place"
        );
    }

    #[gpui::test]
    fn switching_tabs_while_a_gap_closes_should_move_the_selection_from_where_it_was_drawn(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.activate_tab_at(1, window, cx));
        });
        finish_slides(cx);
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        click("tab-close-button-2", cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let drawn = cx.debug_bounds("tab-item-3-chip").unwrap().origin;

        click("tab-item-1-inactive", cx);
        let leaving = cx.debug_bounds("tab-item-1-chip").unwrap().origin;

        assert_eq!(
            leaving, drawn,
            "the selection must leave from where it was drawn while the closed Tab's gap closes"
        );
    }

    #[gpui::test]
    fn a_tab_dragged_while_it_grows_should_lift_at_its_full_size(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = tab_manager(cx);
        let full = cx.debug_bounds("tab-item-1-active").unwrap().size;
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        click("create-tab-button", cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let grabbed = cx.debug_bounds("tab-item-2-active").unwrap().center();

        drag_tab(grabbed, &[grabbed - point(px(8.0), px(0.0))], cx);
        let lifted = cx
            .debug_bounds("tab-preview-2-active")
            .map(|preview| preview.size);
        cx.simulate_mouse_up(
            grabbed - point(px(8.0), px(0.0)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.run_until_parked();

        assert_eq!(
            lifted,
            Some(full),
            "a Tab lifted while it grows must lift at its full size"
        );
    }

    #[gpui::test]
    fn a_tab_dropped_while_a_gap_closes_should_land_without_a_jump(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        for _ in 0..3 {
            click("create-tab-button", cx);
        }
        finish_slides(cx);
        let rest = [
            "tab-item-1-inactive",
            "tab-item-3-inactive",
            "tab-item-4-active",
        ]
        .map(|selector| cx.debug_bounds(selector).unwrap());
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.close_tab(TabId::new(2), window, cx);
            })
        });
        cx.run_until_parked();
        let grabbed = rest[0].center();
        let release = rest[2].center() + point(px(40.0), px(0.0));
        drag_tab(grabbed, &[grabbed + point(px(8.0), px(0.0)), release], cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let drawn = ["tab-item-3-inactive", "tab-item-4-active"]
            .map(|selector| cx.debug_bounds(selector).unwrap().origin);

        cx.simulate_mouse_up(release, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let dropped = ["tab-item-3-inactive", "tab-item-4-active"]
            .map(|selector| cx.debug_bounds(selector).unwrap().origin);

        assert_eq!(tab_order(&manager, cx), vec![3, 4, 1]);
        assert_eq!(
            dropped, drawn,
            "the Tabs a drop passes must stay where they were drawn as it lands"
        );
    }

    #[gpui::test]
    fn a_tab_drag_released_on_its_first_move_should_land(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);
        let first = cx.debug_bounds("tab-item-1-inactive").unwrap();
        let release =
            cx.debug_bounds("tab-item-3-active").unwrap().center() + point(px(4.0), px(0.0));

        drag_tab(first.center(), &[release], cx);
        cx.simulate_mouse_up(release, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        assert_eq!(tab_order(&manager, cx), vec![2, 3, 1]);
    }

    #[gpui::test]
    fn a_tab_released_beside_the_tab_bar_should_land_at_its_end(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);
        let strip = cx.debug_bounds("tab-items").unwrap();
        let first = cx.debug_bounds("tab-item-1-inactive").unwrap();
        let last = cx.debug_bounds("tab-item-3-active").unwrap();
        // Level with the Tabs, a little past the strip's trailing end.
        let beside = point(strip.right() + px(12.0), last.center().y);

        drag_tab(
            first.center(),
            &[first.center() + point(px(8.0), px(0.0)), beside],
            cx,
        );
        cx.simulate_mouse_up(beside, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        assert_eq!(tab_order(&manager, cx), vec![2, 3, 1]);
    }

    #[gpui::test]
    fn a_new_tab_should_grow_in_and_a_closed_one_should_close_its_gap(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = tab_manager(cx);
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        let create = |cx: &mut VisualTestContext| {
            cx.debug_bounds("create-tab-area")
                .expect("the Create Tab control was not rendered")
                .left()
        };
        let width = cx.debug_bounds("tab-item-1-active").unwrap().size.width;
        let before = create(cx);
        let selection = cx.debug_bounds("tab-item-1-chip").unwrap().origin;

        click("create-tab-button", cx);
        let entering = create(cx);
        let leaving = cx.debug_bounds("tab-item-2-chip").map(|chip| chip.origin);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(30));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let growing = create(cx);
        finish_slides(cx);
        let entered = create(cx);
        let landed = cx.debug_bounds("tab-item-2-chip").map(|chip| chip.origin);
        click("tab-close-button-2", cx);
        let closing = create(cx);
        finish_slides(cx);
        let closed = create(cx);

        assert_eq!(
            (entering, entered, closing, closed),
            (before, before + width, before + width, before),
            "a new Tab must grow from nothing, and a closed Tab's place must close from its full \
             width"
        );
        assert!(
            before < growing && growing < before + width,
            "a new Tab must be part way in shortly after it opens, got {growing:?}"
        );
        assert_eq!(
            (leaving, landed),
            (Some(selection), Some(selection + point(width, px(0.0)))),
            "the selection must move from the old Tab to the new one"
        );
    }

    #[gpui::test]
    fn switching_tabs_should_move_the_selection_from_the_old_tab(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        finish_slides(cx);
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        let first = cx.debug_bounds("tab-item-1-inactive").unwrap();
        let second = cx.debug_bounds("tab-item-2-active").unwrap();
        let selection = cx.debug_bounds("tab-item-2-chip").unwrap();
        let chip = |cx: &mut VisualTestContext| {
            cx.debug_bounds("tab-item-1-chip")
                .expect("the selected Tab's chip was not rendered")
                .origin
        };

        click("tab-item-1-inactive", cx);
        let leaving = chip(cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(30));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let moving = chip(cx);
        finish_slides(cx);
        let landed = chip(cx);

        let home = selection.origin + (first.origin - second.origin);
        assert_eq!(
            (leaving, landed),
            (selection.origin, home),
            "the selection must leave from the old Tab and land on the new one"
        );
        assert!(
            home.x < moving.x && moving.x < selection.origin.x,
            "the selection must be on its way shortly after the switch, got {moving:?}"
        );
    }

    #[gpui::test]
    fn closing_the_active_tab_should_hold_the_selection_where_the_next_tab_slides_in(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.activate_tab_at(1, window, cx));
        });
        finish_slides(cx);
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        let selection = cx.debug_bounds("tab-item-2-chip").unwrap();
        let third = cx.debug_bounds("tab-item-3-inactive").unwrap();

        click("tab-close-button-2", cx);
        let closing = (
            cx.debug_bounds("tab-item-3-active").map(|tab| tab.origin),
            cx.debug_bounds("tab-item-3-chip").map(|chip| chip.origin),
        );
        finish_slides(cx);
        let closed = cx.debug_bounds("tab-item-3-chip").map(|chip| chip.origin);

        assert_eq!(
            (closing, closed),
            (
                (Some(third.origin), Some(selection.origin)),
                Some(selection.origin)
            ),
            "the next Tab must slide in under the selection, which stays where it was"
        );
    }

    #[gpui::test]
    fn selecting_a_tab_while_a_closed_tab_closes_its_gap_should_leave_from_the_selection(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);
        finish_slides(cx);
        cx.update(|_, cx| cx.set_global(AnimatePresence));

        click("tab-close-button-2", cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(100));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let closing = cx.debug_bounds("tab-item-3-chip").unwrap().origin;
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.activate_tab_at(0, window, cx));
        });
        cx.run_until_parked();
        let leaving = cx.debug_bounds("tab-item-1-chip").unwrap().origin;

        assert!(
            (leaving.x - closing.x).abs() <= px(1.0),
            "the selection must leave from where it stood at {closing:?}, got {leaving:?}"
        );
    }

    #[gpui::test]
    fn closing_an_inactive_tab_should_keep_the_bar_where_it_was_scrolled(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        for _ in 0..13 {
            click("create-tab-button", cx);
        }
        finish_slides(cx);
        manager.update(cx, |manager, _| {
            manager
                .tab_bar_scroll_handle
                .set_offset(point(px(0.0), px(0.0)));
        });
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();

        click("tab-close-button-2", cx);
        finish_slides(cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| manager.tab_bar_scroll_handle.offset().x),
            px(0.0),
            "closing an inactive Tab must not scroll the bar back to the active one"
        );
    }

    #[gpui::test]
    fn closing_the_first_tab_of_a_bar_scrolled_to_its_end_should_slide_the_others_smoothly(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = tab_manager(cx);
        for _ in 0..13 {
            click("create-tab-button", cx);
        }
        finish_slides(cx);
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        let last = cx.debug_bounds("tab-item-14-active").unwrap().origin;
        let tab = |cx: &mut VisualTestContext| {
            cx.debug_bounds("tab-item-14-active")
                .expect("the active Tab was not rendered")
                .origin
        };
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                let first = manager
                    .tabs
                    .iter()
                    .next()
                    .map(|(tab_id, _)| tab_id)
                    .unwrap();
                manager.close_tab(first, window, cx);
            })
        });
        cx.run_until_parked();
        let closing = tab(cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let moving = tab(cx);
        finish_slides(cx);
        let closed = tab(cx);

        assert_eq!(
            (closing, closed),
            (last, last),
            "the active Tab at the end must hold its place while the first Tab's gap closes"
        );
        assert_eq!(
            moving, last,
            "the active Tab must not move while the gap closes"
        );
    }

    #[gpui::test]
    fn closing_the_last_tab_of_a_bar_scrolled_to_its_end_should_slide_the_others_under_the_selection(
        cx: &mut TestAppContext,
    ) {
        let (_manager, _records, cx) = tab_manager(cx);
        for _ in 0..13 {
            click("create-tab-button", cx);
        }
        finish_slides(cx);
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        let selection = cx.debug_bounds("tab-item-14-chip").unwrap().origin;
        let last = cx.debug_bounds("tab-item-14-active").unwrap().origin;
        let before = cx.debug_bounds("tab-item-13-inactive").unwrap().origin;
        let frame = |cx: &mut VisualTestContext| {
            (
                cx.debug_bounds("tab-item-13-active")
                    .expect("the newly active Tab was not rendered")
                    .origin,
                cx.debug_bounds("tab-item-13-chip")
                    .expect("the selection was not rendered")
                    .origin,
            )
        };

        click("tab-close-button-14", cx);
        let closing = frame(cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let (moving, held) = frame(cx);
        finish_slides(cx);
        let closed = frame(cx);

        assert_eq!(
            (closing, held, closed),
            ((before, selection), selection, (last, selection)),
            "the Tabs must slide over to fill the closed Tab's place while the selection stays put"
        );
        assert!(
            before.x < moving.x && moving.x < last.x,
            "the Tabs must be on their way shortly after the close, got {moving:?}"
        );
    }

    #[gpui::test]
    fn a_new_tab_in_an_overflowing_bar_should_show_whole_while_it_grows(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = tab_manager(cx);
        for _ in 0..12 {
            click("create-tab-button", cx);
        }
        finish_slides(cx);
        cx.update(|_, cx| cx.set_global(AnimatePresence));

        click("create-tab-button", cx);
        let mut frames = Vec::new();
        for _ in 0..4 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(30));
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
            frames.push(cx.debug_bounds("tab-item-14-active").unwrap());
        }
        let strip = cx.debug_bounds("tab-items").unwrap();

        for created in frames {
            assert!(
                strip.left() <= created.left() && created.right() <= strip.right(),
                "a growing Tab must show whole inside the strip {strip:?}, got {created:?}"
            );
        }
    }

    #[gpui::test]
    fn a_new_tab_in_an_overflowing_bar_should_keep_the_selection_at_the_bar_end(
        cx: &mut TestAppContext,
    ) {
        let (_manager, _records, cx) = tab_manager(cx);
        for _ in 0..12 {
            click("create-tab-button", cx);
        }
        finish_slides(cx);
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        let selection = cx.debug_bounds("tab-item-13-chip").unwrap().origin;

        click("create-tab-button", cx);
        let mut frames = vec![cx.debug_bounds("tab-item-14-chip").unwrap().origin];
        for _ in 0..4 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(30));
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
            frames.push(cx.debug_bounds("tab-item-14-chip").unwrap().origin);
        }

        assert_eq!(
            frames,
            vec![selection; 5],
            "a new Tab grows in where the old selection stood, so the selection must hold still"
        );
    }

    #[gpui::test]
    fn new_tabs_opened_in_quick_succession_should_keep_the_selection_at_the_bar_end(
        cx: &mut TestAppContext,
    ) {
        let (_manager, _records, cx) = tab_manager(cx);
        for _ in 0..12 {
            click("create-tab-button", cx);
        }
        finish_slides(cx);
        cx.update(|_, cx| cx.set_global(AnimatePresence));
        let selection = cx.debug_bounds("tab-item-13-chip").unwrap().origin;
        let next_frame = |cx: &mut VisualTestContext| {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(16));
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
        };

        let mut frames = Vec::new();
        for chip in [
            "tab-item-14-chip",
            "tab-item-15-chip",
            "tab-item-16-chip",
            "tab-item-17-chip",
        ] {
            // A Tab opens from the keyboard part way between two frames.
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(10));
            cx.dispatch_action(crate::ui::CreateTab);
            frames.push(cx.debug_bounds(chip).unwrap().origin.x);
            for _ in 0..2 {
                next_frame(cx);
                frames.push(cx.debug_bounds(chip).unwrap().origin.x);
            }
        }

        assert!(
            frames.iter().all(|x| (*x - selection.x).abs() <= px(1.0)),
            "a Tab opened while the one before it still grows must hold the selection still at \
             {selection:?}, got {frames:?}"
        );
    }

    #[gpui::test]
    fn selecting_a_tab_scrolled_out_of_view_should_leave_from_where_the_selection_stood(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = tab_manager(cx);
        for _ in 0..12 {
            click("create-tab-button", cx);
        }
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.activate_tab_at(0, window, cx));
        });
        finish_slides(cx);
        let strip = cx.debug_bounds("tab-items").unwrap();
        let stood = cx.debug_bounds("tab-item-1-chip").unwrap().origin.x;
        assert!(
            cx.debug_bounds("tab-item-13-inactive")
                .is_none_or(|tab| tab.left() >= strip.right()),
            "the last Tab is out of view"
        );
        cx.update(|_, cx| cx.set_global(AnimatePresence));

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.activate_tab_at(12, window, cx));
        });
        cx.run_until_parked();
        let mut frames = Vec::new();
        for _ in 0..3 {
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
            frames.push(cx.debug_bounds("tab-item-13-chip").unwrap().origin.x);
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(16));
        }

        assert!(
            (frames[0] - stood).abs() <= px(1.0)
                && frames
                    .iter()
                    .all(|x| strip.left() - px(1.0) <= *x && *x <= strip.right()),
            "the selection must leave from {stood:?} inside the strip {strip:?}, got {frames:?}"
        );
    }

    #[gpui::test]
    fn a_new_tab_in_an_overflowing_bar_should_end_fully_in_view(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = tab_manager(cx);
        for _ in 0..12 {
            click("create-tab-button", cx);
        }
        cx.update(|_, cx| cx.set_global(AnimatePresence));

        click("create-tab-button", cx);
        finish_slides(cx);
        let strip = cx.debug_bounds("tab-items").unwrap();
        let created = cx.debug_bounds("tab-item-14-active").unwrap();

        assert!(
            strip.left() <= created.left() && created.right() <= strip.right(),
            "a new active Tab must end inside the strip {strip:?}, got {created:?}"
        );
    }

    #[gpui::test]
    fn dragging_a_tab_should_open_a_gap_and_settle_there_on_release(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);
        let first = cx.debug_bounds("tab-item-1-inactive").unwrap();
        let second = cx.debug_bounds("tab-item-2-inactive").unwrap();
        let third = cx.debug_bounds("tab-item-3-active").unwrap();
        let lift = first.center() + point(px(8.0), px(0.0));
        // Past the last Tab and below the strip, where the lifted copy stays held at the strip's
        // end, level with the Tabs.
        let release = third.center() + point(px(4.0), px(10.0));

        drag_tab(first.center(), &[lift, release], cx);
        finish_slides(cx);
        let landing = cx.debug_bounds("tab-item-1-chip-landing").is_some();
        let during = (
            tab_order(&manager, cx),
            cx.debug_bounds("tab-item-1-inactive"),
            cx.debug_bounds("tab-item-2-inactive").map(|tab| tab.origin),
            cx.debug_bounds("tab-item-3-active").map(|tab| tab.origin),
            cx.debug_bounds("tab-preview-1-inactive"),
        );
        cx.simulate_mouse_up(release, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            during,
            (
                vec![1, 2, 3],
                Some(gpui::Bounds::new(third.origin, first.size)),
                Some(first.origin),
                Some(second.origin),
                Some(gpui::Bounds::new(third.origin, first.size)),
            ),
            "a dragged Tab must show its faded face where it lands, open a gap there, and hold its \
             lifted copy inside the strip"
        );
        assert!(
            landing,
            "a dragged Tab's faded face must edge its chip with a dashed line"
        );
        assert_eq!(
            (
                tab_order(&manager, cx),
                manager.read_with(cx, |manager, _| (
                    manager.tabs.active_tab_id(),
                    manager.tab_reorder.dragged(),
                    manager.tab_selector_pressed,
                )),
            ),
            (vec![2, 3, 1], (TabId::new(3), None, None))
        );
        assert!(cx.debug_bounds("drag-preview").is_none());
        // The moved Tab starts where its copy was released, already its new place.
        assert_eq!(
            cx.debug_bounds("tab-item-1-inactive").map(|tab| tab.origin),
            Some(third.origin)
        );
        finish_slides(cx);
        assert_eq!(
            [
                "tab-item-2-inactive",
                "tab-item-3-active",
                "tab-item-1-inactive"
            ]
            .map(|selector| cx.debug_bounds(selector).map(|tab| tab.origin)),
            [Some(first.origin), Some(second.origin), Some(third.origin)]
        );
    }

    #[gpui::test]
    fn a_tab_grabbed_again_while_it_settles_should_lift_from_where_it_is_drawn(
        cx: &mut TestAppContext,
    ) {
        let (_manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);
        let first = cx.debug_bounds("tab-item-1-inactive").unwrap();
        let second = cx.debug_bounds("tab-item-2-inactive").unwrap();
        // Past the second Tab's midpoint, short of the third's, so the Tab lands second and
        // settles 80pt back.
        let release = second.center() + point(px(80.0), px(0.0));
        drag_tab(
            first.center(),
            &[first.center() + point(px(8.0), px(0.0)), release],
            cx,
        );
        cx.simulate_mouse_up(release, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let settling = cx.debug_bounds("tab-item-1-inactive").unwrap();
        assert_ne!(settling.origin, second.origin, "the Tab is still settling");

        let moved = point(px(8.0), px(0.0));
        drag_tab(settling.center(), &[settling.center() + moved], cx);
        let copy = cx
            .debug_bounds("tab-preview-1-inactive")
            .map(|tab| tab.origin);
        cx.simulate_mouse_up(
            settling.center() + moved,
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.run_until_parked();

        assert_eq!(
            copy,
            Some(settling.origin + moved),
            "a Tab grabbed while it settles must lift from where it is drawn"
        );
    }

    #[gpui::test]
    fn the_gap_should_follow_the_lifted_tab_and_close_over_its_own_place(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);
        let first = cx.debug_bounds("tab-item-1-inactive").unwrap();
        let second = cx.debug_bounds("tab-item-2-inactive").unwrap();
        let third = cx.debug_bounds("tab-item-3-active").unwrap();
        let shifts = |cx: &mut VisualTestContext| {
            finish_slides(cx);
            [
                ("tab-item-1-inactive", first),
                ("tab-item-2-inactive", second),
            ]
            .map(|(selector, rest)| cx.debug_bounds(selector).unwrap().left() - rest.left())
        };
        let move_to = |position: gpui::Point<Pixels>, cx: &mut VisualTestContext| {
            cx.simulate_mouse_move(position, Some(MouseButton::Left), Modifiers::none());
            shifts(cx)
        };

        drag_tab(
            third.center(),
            &[third.center() - point(px(8.0), px(0.0))],
            cx,
        );
        let before_second = move_to(second.center() - point(px(4.0), px(0.0)), cx);
        let before_first = move_to(first.center() - point(px(4.0), px(0.0)), cx);
        let release = third.center() + point(px(4.0), px(0.0));
        let over_itself = move_to(release, cx);
        cx.simulate_mouse_up(release, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        let width = third.size.width;
        assert_eq!(
            (
                before_second,
                before_first,
                over_itself,
                tab_order(&manager, cx)
            ),
            (
                [px(0.0), width],
                [width, width],
                [px(0.0), px(0.0)],
                vec![1, 2, 3]
            )
        );
    }

    #[gpui::test]
    fn a_tab_dragged_far_below_the_tab_bar_should_stay_in_the_strip_and_land_there(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);
        let first = cx.debug_bounds("tab-item-1-inactive").unwrap();
        let second = cx.debug_bounds("tab-item-2-inactive").unwrap();
        let third = cx.debug_bounds("tab-item-3-active").unwrap();
        let below = point(second.center().x, third.bottom() + px(240.0));

        drag_tab(
            first.center(),
            &[first.center() + point(px(8.0), px(0.0)), below],
            cx,
        );
        finish_slides(cx);
        let preview = cx.debug_bounds("tab-preview-1-inactive");
        cx.simulate_mouse_up(below, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            (
                preview.map(|preview| preview.top()),
                tab_order(&manager, cx)
            ),
            (Some(second.top()), vec![2, 1, 3]),
            "the lifted copy must stay level with the Tabs, and a release must land the Tab where \
             its copy shows"
        );
    }

    #[gpui::test]
    fn escape_should_cancel_a_tab_drag_and_return_every_tab_to_its_place(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        click("create-tab-button", cx);
        let first = cx.debug_bounds("tab-item-1-inactive").unwrap();
        let third = cx.debug_bounds("tab-item-3-active").unwrap();
        let release = third.center() + point(px(4.0), px(0.0));

        drag_tab(
            first.center(),
            &[first.center() + point(px(8.0), px(0.0)), release],
            cx,
        );
        finish_slides(cx);
        let opened = cx.debug_bounds("tab-item-3-active").unwrap().origin != third.origin;
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        let cancelled = (
            manager.read_with(cx, |manager, _| manager.tab_reorder.dragged()),
            cx.debug_bounds("drag-preview").is_some(),
        );
        cx.simulate_mouse_up(release, MouseButton::Left, Modifiers::none());
        finish_slides(cx);

        assert_eq!(
            (
                opened,
                cancelled,
                tab_order(&manager, cx),
                ["tab-item-1-inactive", "tab-item-3-active"]
                    .map(|selector| cx.debug_bounds(selector).map(|tab| tab.origin)),
            ),
            (
                true,
                (None, false),
                vec![1, 2, 3],
                [Some(first.origin), Some(third.origin)]
            )
        );
    }

    #[gpui::test]
    fn a_tab_drag_should_end_on_a_release_of_any_button(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        let first = cx.debug_bounds("tab-item-1-inactive").unwrap();
        let second = cx.debug_bounds("tab-item-2-active").unwrap();
        let release = second.center() + point(px(4.0), px(0.0));

        drag_tab(
            first.center(),
            &[first.center() + point(px(8.0), px(0.0)), release],
            cx,
        );
        // AppKit reports a left release with Control held as a right release.
        cx.simulate_mouse_up(release, MouseButton::Right, Modifiers::control());
        cx.run_until_parked();

        assert_eq!(
            (
                tab_order(&manager, cx),
                manager.read_with(cx, |manager, _| manager.tab_reorder.dragged()),
            ),
            (vec![2, 1], None)
        );
        cx.debug_bounds("tab-item-1-inactive")
            .expect("the released Tab must render in its new place");
    }

    #[gpui::test]
    fn holding_a_tab_at_the_end_of_the_tab_bar_should_scroll_it_and_land_the_tab_last(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = tab_manager(cx);
        for _ in 0..12 {
            click("create-tab-button", cx);
        }
        manager.read_with(cx, |manager, _| {
            manager
                .tab_bar_scroll_handle
                .set_offset(point(px(0.0), px(0.0)));
        });
        manager.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let strip = cx.debug_bounds("tab-items").unwrap();
        let first = cx.debug_bounds("tab-item-1-inactive").unwrap();
        let held = point(strip.right() - px(4.0), first.center().y);
        let scroll = |cx: &mut VisualTestContext| {
            manager.read_with(cx, |manager, _| {
                let handle = &manager.tab_bar_scroll_handle;
                (handle.offset().x, -handle.max_offset().x)
            })
        };

        drag_tab(
            first.center(),
            &[first.center() + point(px(8.0), px(0.0)), held],
            cx,
        );
        // Held still, the strip keeps scrolling one frame after another until its end shows.
        for _ in 0..240 {
            let (offset, end) = scroll(cx);
            if offset <= end {
                break;
            }
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(16));
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
        }
        let (offset, end) = scroll(cx);
        cx.simulate_mouse_up(held, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        assert!(end < px(0.0), "twelve Tabs must overflow the Tab bar");
        assert_eq!(offset, end, "the Tab bar must scroll to its end");
        assert_eq!(tab_order(&manager, cx).last(), Some(&1));
    }

    #[gpui::test]
    fn the_strip_end_mark_should_stay_at_the_end_of_an_overflowing_tab_bar(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = tab_manager(cx);
        for _ in 0..12 {
            click("create-tab-button", cx);
        }
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.activate_tab_at(0, window, cx));
        });
        manager.read_with(cx, |manager, _| {
            manager
                .tab_bar_scroll_handle
                .set_offset(point(px(0.0), px(0.0)));
        });
        manager.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let last = *tab_order(&manager, cx).last().unwrap();
        let strip = cx.debug_bounds("tab-items").unwrap();
        let mark = cx
            .debug_bounds(leaked_selector(format!("tab-separator-{last}-end")))
            .expect("the last Tab is inactive, so the strip end should be marked");

        assert_eq!(
            mark.right(),
            strip.right(),
            "the mark should rest at the strip end while the last Tab is scrolled out of view"
        );
    }

    #[gpui::test]
    fn the_strip_end_marks_should_show_once_the_active_tab_scrolls_out_past_them(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = tab_manager(cx);
        for _ in 0..12 {
            click("create-tab-button", cx);
        }
        let activate = |position: usize, cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                manager.update(cx, |manager, cx| {
                    manager.set_sidebar_layout(
                        true,
                        px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
                        px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
                        cx,
                    );
                    manager.activate_tab_at(position, window, cx);
                });
            });
            cx.run_until_parked();
        };
        let wheel = |delta: f32, cx: &mut VisualTestContext| {
            let strip = cx.debug_bounds("tab-items").unwrap();
            cx.simulate_event(ScrollWheelEvent {
                position: strip.center(),
                delta: ScrollDelta::Pixels(point(px(0.0), px(delta))),
                modifiers: Modifiers::none(),
                touch_phase: TouchPhase::Moved,
            });
            cx.run_until_parked();
        };
        let order = tab_order(&manager, cx);
        let start = leaked_selector(format!("tab-separator-start-{}", order[0]));
        let end = leaked_selector(format!("tab-separator-{}-end", order[order.len() - 1]));

        activate(0, cx);
        assert!(
            cx.debug_bounds(start).is_none(),
            "the active first Tab at the strip start hides the start mark"
        );
        wheel(-10_000.0, cx);
        let strip = cx.debug_bounds("tab-items").unwrap();
        let mark = cx
            .debug_bounds(start)
            .expect("the active first Tab scrolled out past the start should show the start mark");
        assert_eq!(mark.left(), strip.left());

        activate(order.len() - 1, cx);
        assert!(
            cx.debug_bounds(end).is_none(),
            "the active last Tab scrolled into view at the strip end hides the end mark"
        );
        wheel(10_000.0, cx);
        let strip = cx.debug_bounds("tab-items").unwrap();
        let mark = cx
            .debug_bounds(end)
            .expect("the active last Tab scrolled out past the end should show the end mark");
        assert_eq!(mark.right(), strip.right());
    }

    #[gpui::test]
    fn tab_items_should_scroll_horizontally_with_the_mouse_wheel(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        for _ in 0..12 {
            click("create-tab-button", cx);
        }

        manager.read_with(cx, |manager, _| {
            manager
                .tab_bar_scroll_handle
                .set_offset(point(px(0.0), px(0.0)));
        });
        manager.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let items = cx
            .debug_bounds("tab-items")
            .expect("the Tab item strip was not rendered");
        cx.simulate_event(ScrollWheelEvent {
            position: items.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-120.0))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();

        let offset = manager.read_with(cx, |manager, _| manager.tab_bar_scroll_handle.offset().x);
        assert!(
            offset < px(0.0),
            "the Tab strip did not scroll; offset was {offset:?}"
        );
    }

    #[gpui::test]
    fn activating_an_inactive_tab_should_restore_its_focused_pane(cx: &mut TestAppContext) {
        let (manager, _records, cx) = tab_manager(cx);
        cx.simulate_keystrokes("cmd-d");
        click("create-tab-button", cx);

        click("tab-item-1-inactive", cx);

        let first_tab = manager.read_with(cx, |manager, _| {
            manager
                .tabs
                .tab(TabId::new(1))
                .cloned()
                .expect("Tab 1 must remain owned")
        });
        let state = cx.update(|window, cx| {
            let view = first_tab.read(cx);
            (
                manager.read(cx).tabs.active_tab_id(),
                view.focused_pane_id(),
                view.focused_terminal_is_focused(window, cx),
            )
        });
        assert_eq!(state, (TabId::new(1), PaneId::new(2), true));
    }

    #[gpui::test]
    fn tab_chrome_should_forward_threshold_crossing_and_double_activation_to_platform_policy(
        cx: &mut TestAppContext,
    ) {
        let (_manager, platform, cx) = tab_manager_with_operating_system_window_drag_platform(cx);
        let chrome = cx
            .debug_bounds("tab-bar-drag-region")
            .expect("Window drag region must be rendered")
            .center();

        cx.simulate_mouse_down(chrome, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            point(chrome.x + px(2.0), chrome.y),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_move(
            point(chrome.x + px(8.0), chrome.y),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_move(
            point(chrome.x + px(16.0), chrome.y),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_up(chrome, MouseButton::Left, Modifiers::none());
        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: chrome,
            modifiers: Modifiers::none(),
            click_count: 2,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: chrome,
            modifiers: Modifiers::none(),
            click_count: 2,
        });

        assert_eq!(platform.counts(), (1, 1, 1));
    }

    #[gpui::test]
    fn top_chrome_mouse_down_should_block_until_release_without_changing_focused_pane(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = tab_manager(cx);
        let command_count = records.commands().len();
        let chrome = cx
            .debug_bounds("tab-bar")
            .expect("top chrome must be rendered")
            .center();
        let focused_pane_id = manager.read_with(cx, |manager, cx| {
            manager.tabs.active_tab().read(cx).focused_pane_id()
        });

        cx.simulate_mouse_down(chrome, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let blocked = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.tabs.active_tab().read(cx).focused_pane_id(),
                manager.focused_terminal_is_focused(window, cx),
                manager.focused_terminal_has_input_focus(window, cx),
            )
        });
        assert_eq!(blocked, (focused_pane_id, true, false));

        let services_blocked = cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.native_service_status(WorkspaceId::new(1), window, cx)
            })
        });
        manager.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let rerender_blocked = cx.update(|window, cx| {
            !manager
                .read(cx)
                .focused_terminal_has_input_focus(window, cx)
        });
        assert!(!services_blocked.capabilities.return_text);
        assert!(rerender_blocked);

        cx.simulate_mouse_move(chrome, None, Modifiers::none());
        cx.run_until_parked();
        assert!(cx.update(|window, cx| {
            manager
                .read(cx)
                .focused_terminal_has_input_focus(window, cx)
        }));

        let outside_chrome = cx
            .debug_bounds("tab-manager-content")
            .expect("Tab content must be rendered")
            .center();
        cx.simulate_mouse_up(outside_chrome, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        cx.simulate_mouse_down(chrome, MouseButton::Left, Modifiers::none());
        cx.simulate_event(MouseExitEvent {
            position: chrome,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::none(),
        });
        let focus_edges = records
            .commands()
            .into_iter()
            .skip(command_count)
            .filter_map(|call| match call.command {
                RecordedCommand::Focus(focused) => Some((call.session_id, focused)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(focus_edges, [(1, false), (1, true), (1, false), (1, true)]);
    }

    #[gpui::test]
    fn tab_selector_press_should_block_before_activation_and_restore_selected_terminal(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        let command_count = records.commands().len();
        let position = cx
            .debug_bounds("tab-item-1-inactive")
            .expect("inactive Tab selector must be rendered")
            .center();
        let focused_pane_id = manager.read_with(cx, |manager, cx| {
            manager.tabs.active_tab().read(cx).focused_pane_id()
        });

        cx.simulate_mouse_move(position, None, Modifiers::none());
        cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        let pressed = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.tabs.active_tab_id(),
                manager.tabs.active_tab().read(cx).focused_pane_id(),
                manager.focused_terminal_is_focused(window, cx),
                manager.focused_terminal_has_input_focus(window, cx),
            )
        });
        assert_eq!(pressed, (TabId::new(2), focused_pane_id, true, false));

        cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        let selected = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.tabs.active_tab_id(),
                manager.focused_terminal_is_focused(window, cx),
                manager.focused_terminal_has_input_focus(window, cx),
            )
        });
        let focus_edges = records
            .commands()
            .into_iter()
            .skip(command_count)
            .filter_map(|call| match call.command {
                RecordedCommand::Focus(focused) => Some((call.session_id, focused)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            (selected, focus_edges),
            ((TabId::new(1), true, true), vec![(2, false), (1, true)])
        );
    }

    #[gpui::test]
    fn closing_an_inactive_tab_should_preserve_the_active_tab(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        click("create-tab-button", cx);

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.close_tab(TabId::new(1), window, cx);
            });
        });
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (manager.tabs.active_tab_id(), records.dropped_session_ids())
        });
        assert_eq!(state, (TabId::new(2), vec![1]));
    }

    #[gpui::test]
    fn inactive_shell_exit_should_close_its_tab_without_stealing_focus(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        let first_sender = records
            .event_sender(1)
            .expect("Tab 1 Terminal Session must have started");

        first_sender
            .try_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success))
            .unwrap();
        cx.run_until_parked();

        let active_tab = manager.read_with(cx, |manager, _| manager.tabs.active_tab().clone());
        let state = cx.update(|window, cx| {
            (
                manager.read(cx).tabs.active_tab_id(),
                active_tab.read(cx).focused_terminal_is_focused(window, cx),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (TabId::new(2), true, vec![1]));
    }

    #[gpui::test]
    fn inactive_workspace_active_tab_exit_should_leave_its_fallback_deactivated_and_unfocused(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        let active_sender = records
            .event_sender(2)
            .expect("Tab 2 Terminal Session must have started");
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.deactivate(cx));
            window.blur(cx);
        });

        active_sender
            .try_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success))
            .unwrap();
        cx.run_until_parked();

        let fallback = manager.read_with(cx, |manager, _| manager.tabs.active_tab().clone());
        let state = cx.update(|window, cx| {
            (
                manager.read(cx).active,
                manager.read(cx).tabs.len(),
                manager.read(cx).tabs.active_tab_id(),
                fallback.read(cx).is_active(),
                fallback.read(cx).focused_terminal_is_focused(window, cx),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (false, 1, TabId::new(1), false, false, vec![2]));
    }

    #[gpui::test]
    fn active_shell_exit_should_close_its_tab_and_focus_the_neighbor(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        let active_sender = records
            .event_sender(2)
            .expect("Tab 2 Terminal Session must have started");

        active_sender
            .try_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success))
            .unwrap();
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.tabs.len(),
                manager.tabs.active_tab_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (1, TabId::new(1), vec![2]));
        assert!(cx.update(|window, cx| {
            manager
                .read(cx)
                .focused_terminal_has_input_focus(window, cx)
        }));
    }

    #[gpui::test]
    fn closing_a_multi_pane_tab_should_close_every_owned_terminal_session_exactly_once(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = tab_manager(cx);
        cx.simulate_keystrokes("cmd-d");
        click("create-tab-button", cx);

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.close_tab(TabId::new(1), window, cx);
            });
        });
        cx.run_until_parked();

        let mut dropped = records.dropped_session_ids();
        dropped.sort_unstable();
        assert_eq!(dropped, vec![1, 2]);
    }

    #[gpui::test]
    fn command_w_should_close_only_the_focused_pane_when_the_tab_is_split(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();

        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.tabs.len(),
                manager.tabs.active_tab().read(cx).pane_count(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (1, 1, vec![2]));
    }

    #[gpui::test]
    fn command_w_should_close_the_active_tab_when_its_last_pane_closes(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        click("create-tab-button", cx);

        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.tabs.len(),
                manager.tabs.active_tab_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (1, TabId::new(1), vec![2]));
    }

    #[gpui::test]
    fn command_shift_w_should_close_every_pane_in_only_the_active_tab(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        cx.simulate_keystrokes("cmd-d");
        click("create-tab-button", cx);
        click("tab-item-1-inactive", cx);

        cx.simulate_keystrokes("cmd-shift-w");
        cx.run_until_parked();

        let mut dropped = records.dropped_session_ids();
        dropped.sort_unstable();
        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.tabs.len(),
                manager.tabs.active_tab_id(),
                manager.tabs.active_tab().read(cx).pane_count(),
            )
        });
        assert_eq!(state, (1, TabId::new(2), 1));
        assert_eq!(dropped, vec![1, 2]);
    }

    #[gpui::test]
    fn command_shift_w_should_request_owning_workspace_close_for_the_final_tab(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = tab_manager(cx);
        let close_requests = Rc::new(Cell::new(0));
        let close_requests_for_subscription = Rc::clone(&close_requests);
        manager.update(cx, |_, cx| {
            cx.subscribe(&manager, move |_, _, event: &TabManagerEvent, _| {
                if matches!(event, TabManagerEvent::FinalTabCloseRequested { .. }) {
                    close_requests_for_subscription.update(|count| count + 1);
                }
            })
            .detach();
        });

        cx.simulate_keystrokes("cmd-shift-w");
        cx.simulate_keystrokes("cmd-shift-w");
        cx.run_until_parked();

        assert_eq!(
            (close_requests.get(), records.dropped_session_ids()),
            (1, Vec::new())
        );
    }

    fn report_current_directory(
        records: &TestTerminalSessionRecords,
        session: usize,
        generation: u64,
        directory: &str,
        remote: bool,
    ) {
        report_metadata(records, session, generation, |metadata| {
            metadata.directory.path = Arc::from(directory);
            if remote {
                metadata.context = remote_metadata_context(directory);
            }
        });
    }

    fn remote_metadata_context(
        directory: &str,
    ) -> crate::terminal::metadata::TerminalMetadataContext {
        crate::terminal::metadata::TerminalMetadataContext::Remote(
            crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                crate::domain::SshDestination::new("tester@remote".into()).unwrap(),
                crate::domain::RemoteDirectory::new(directory.into()).unwrap(),
            ),
        )
    }

    /// Delivers one Screen whose sanitized Terminal Metadata the caller shapes.
    fn report_metadata(
        records: &TestTerminalSessionRecords,
        session: usize,
        generation: u64,
        change: impl FnOnce(&mut crate::terminal::metadata::TerminalMetadataSnapshot),
    ) {
        let mut screen = crate::terminal::ScreenSnapshot::from_test_parts_at(
            Arc::from([]),
            crate::terminal::ScrollbarSnapshot::default(),
            "terminal",
            generation,
        );
        change(Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata));
        records
            .event_sender(session)
            .unwrap()
            .try_send(TerminalSessionEvent::Screen(screen))
            .unwrap();
    }
}
