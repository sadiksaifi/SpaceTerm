use super::pane_lifecycle::{PaneConstruction, RemoteHierarchyLifecycle};
use crate::domain::PinnedDirectory;
use crate::domain::remote_workspace::RemoteRestartBatch;
#[cfg(test)]
use std::cell::Cell;
use std::rc::Rc;

use thiserror::Error;

use super::chrome_icons::IconRole;
use super::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use super::selection_chip::{ChipPaint, ChipShape, SelectionChip};
use super::terminal_focus::{TabFocusOwners, TerminalFocusBlocker, TerminalFocusCoordinator};
use super::terminal_status::{StatusColors, StatusGlyph};
use super::{
    ActivateTab1, ActivateTab2, ActivateTab3, ActivateTab4, ActivateTab5, ActivateTab6,
    ActivateTab7, ActivateTab8, ActivateTab9, CloseTab, CreateTab, PaneHost, PaneHostEvent,
    PreparedPaneHostRemoteRestart, RemoteChildLaunchUnavailable, RemotePaneHostLifecycleError,
    TERMINAL_KEY_CONTEXT, TabIdentity, WORKSPACE_SIDEBAR_DEFAULT_WIDTH,
};
#[cfg(test)]
use super::{TOP_CHROME_HEIGHT, WORKSPACE_SIDEBAR_MINIMUM_WIDTH};

#[derive(Debug, Error)]
/// A typed rejection while coordinating Remote lifecycle across the Workspace's Tab hierarchy.
pub(crate) enum RemoteTabManagerLifecycleError {
    #[error(transparent)]
    Revalidation(#[from] RemoteChannelRevalidationError),
    #[error(transparent)]
    ChannelUnavailable(#[from] RemoteChannelUnavailable),
    #[error("remote restart preparation was superseded")]
    PreparationSuperseded,
    #[error("Tab {tab_id} cannot change remote session lifecycle: {source}")]
    Tab {
        tab_id: TabId,
        #[source]
        source: RemotePaneHostLifecycleError,
    },
    #[error("Tab {0} changed after remote restart preparation")]
    TabChanged(TabId),
}

/// Move-only restart reservations for every Pane across one unchanged Tab hierarchy.
///
/// No Tab or Pane is mutated until the complete token has been prepared and revalidated.
pub(crate) struct PreparedTabManagerRemoteRestart {
    session_factory: WorkspaceTerminalSessionFactory,
    tabs: RemoteRestartBatch<(TabId, Entity<PaneHost>, PreparedPaneHostRemoteRestart)>,
}
use crate::appearance::ChromeColors;
use crate::appearance::Color;
use crate::domain::{CloseTabOutcome, PaneId, TabCollection, TabError, TabId, WorkspaceId};
#[cfg(test)]
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::platform::window_movement::{
    OperatingSystemWindowDragError, OperatingSystemWindowDragPlatform,
};
use crate::terminal::{
    NativeServiceOrigin, NativeServiceStatus, PreparedWorkspaceTerminalLaunch,
    RemoteChannelRevalidationError, RemoteChannelUnavailable, WorkspaceTerminalSessionFactory,
};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Edges, Entity, EventEmitter, MouseButton, Pixels, Render,
    ScrollHandle, Task, Window, div, px, relative, rgba,
};
use spaceterm_ui::{
    Alert, AlertIntent, ButtonSize, ButtonTheme, ButtonVariant, CustomIconName, Icon, IconButton,
    IconName, ModalAction, ModalActionRole, ModalId, Tooltip, WindowDragRegion,
    WindowDragRegionEvent, WindowDragRegionResponse, WindowDragRegionStatus,
};

#[cfg(test)]
const TAB_BAR_HEIGHT: f32 = TOP_CHROME_HEIGHT;
const TAB_ITEM_WIDTH: f32 = 178.2;
const TAB_ITEM_MINIMUM_WIDTH: f32 = 159.3;
pub(super) const TAB_ITEM_MAXIMUM_WIDTH: f32 = 216.0;
/// The title starts as far inside the chip as a Settings navigation label does inside its own, and
/// Close keeps the same air to the chip's right edge as it keeps above and below.
const TAB_ITEM_LEFT_PADDING: f32 = 11.0;
const TAB_ITEM_RIGHT_PADDING: f32 = 7.0;
/// The geometry of the chip carrying one Tab's material, resolved from the Workspace frame.
///
/// A Tab keeps the full height of the title bar as its hit target and its hover region; only the
/// paint moves inward. The insets are what the eye actually measures:
///
/// - vertically the chip faces the window's top edge and, below the strip, the Pane's own surface,
///   so it carries a whole frame space on each side of the band beneath the window's edge;
/// - horizontally it faces another chip, so each side carries a share and the visible gap between
///   two Tabs is one frame space again.
///
/// The radius comes from the frame's one radius family, so a Tab, a selected sidebar row, and a
/// floating Pane read as the same shape at three sizes rather than as cousins.
fn tab_chip_shape(appearance: &super::appearance::ChromeAppearance, cx: &App) -> ChipShape {
    let frame = super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx);
    ChipShape {
        inset_leading: frame.strip_chip_leading_inset(),
        inset_trailing: frame.strip_chip_trailing_inset(),
        inset_y: frame.space(),
        radius: frame.chip_radius(),
    }
}

/// The leading Terminal glyph every Tab carries, and the air between it and the Tab's identity.
const TAB_ORIGIN_GAP: f32 = 6.0;
/// The air after the status glyph, and before the close control.
const TAB_TRAILING_GAP: f32 = 4.0;
/// How much of a Tab's identity the activity may claim when a place beside it needs a share.
///
/// The place yields all the room a narrowing Tab needs, so a short activity stays whole. A long
/// one is capped here instead, so a wordy title never pushes the directory leaf out of the Tab.
const TAB_ACTIVITY_MAXIMUM_SHARE: f32 = 0.6;
/// The Compact-density length of the quiet mark between two neighbouring inactive Tabs.
///
/// Inactive Tabs rest as text on the bar, so a short hairline is enough to say where one title
/// ends. The Active Tab's chip already has an edge, so no mark touches it. Like the chip insets,
/// the length is a density baseline: 18 points at Compact and 22.5 at Comfortable, so the mark
/// keeps its proportion to a Tab that grows with density.
const TAB_SEPARATOR_LENGTH: f32 = 18.0;
/// The mark's thickness: one logical point at every density, the same hairline as the chip rim.
///
/// Density lengthens the mark but never thickens it. A whole point covers at least one whole device
/// pixel at every supported display scale, so the mark stays thin on a 1x display without ever
/// dropping below a pixel on a fractional one, and a width derived from the scale would leave
/// layout rounding to decide which side of an edge it lands on.
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
                // ChromeAppearance has already prepared the common active-state roles for an
                // inactive window. Keep the legacy inactive-selected aliases out of rendering.
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
    ///
    /// The Active Tab uses an inset selection fill tuned to the title bar, with a stronger fill
    /// under the pointer. Built-in appearances omit decorative outlines; custom Tab edges remain
    /// supported.
    ///
    /// An inactive Tab paints its own fill rather than nothing at all, so a scheme that authors a
    /// distinct inactive Tab color still gets it. The built-in palette resolves that color to the
    /// title bar itself, which leaves an inactive Tab as text on the bar and the Active Tab as the
    /// one shape on it.
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

    fn control_style(
        &self,
        active: bool,
        ancestor_hovered: bool,
        colors: &ChromeColors,
        materials: crate::appearance::SurfaceMaterials,
        host: Color,
    ) -> spaceterm_ui::ButtonVariantStyle {
        let clear = gpui::rgba(0);
        let icon = if ancestor_hovered {
            if active {
                self.active_tab_hover_icon
            } else {
                self.hover_icon
            }
        } else if active {
            self.active_tab_icon
        } else {
            self.inactive_tab_icon
        };
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

pub(crate) struct TabManager {
    tabs: TabCollection<Entity<PaneHost>>,
    session_factory: WorkspaceTerminalSessionFactory,
    pane_construction: PaneConstruction,
    active: bool,
    sidebar_visible: bool,
    sidebar_width: Pixels,
    top_chrome_width: Pixels,
    parent_focus_blocker: Option<TerminalFocusBlocker>,
    tab_selector_pressed: Option<TabId>,
    hovered_tab: Option<TabId>,
    operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
    window_drag_status: WindowDragRegionStatus,
    tab_bar_scroll_handle: ScrollHandle,
    close_workspace_requested: bool,
    remote_lifecycle: RemoteHierarchyLifecycle,
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
        match Self::new_with_operating_system_window_drag_platform(
            session_factory,
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            window,
            cx,
        ) {
            Ok(manager) => manager,
            Err(error) => panic!("test TabManager channel preparation failed: {error}"),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with_operating_system_window_drag_platform(
        session_factory: WorkspaceTerminalSessionFactory,
        operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Self, RemoteChannelUnavailable> {
        let prepared_launch = session_factory.prepare_child_launch()?;
        Ok(Self::new_with_prepared_initial_launch(
            session_factory,
            prepared_launch,
            operating_system_window_drag_platform,
            PaneConstruction::testing(),
            window,
            cx,
        ))
    }

    pub(crate) fn new_with_prepared_initial_launch(
        session_factory: WorkspaceTerminalSessionFactory,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
        pane_construction: PaneConstruction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tabs = TabCollection::new(|tab_id| {
            Self::create_pane_host(
                tab_id,
                session_factory.clone(),
                prepared_launch,
                pane_construction.clone(),
                window,
                cx,
            )
        });
        Self {
            tabs,
            session_factory,
            pane_construction,
            active: true,
            sidebar_visible: true,
            sidebar_width: px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            top_chrome_width: px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            parent_focus_blocker: None,
            tab_selector_pressed: None,
            hovered_tab: None,
            operating_system_window_drag_platform,
            window_drag_status: WindowDragRegionStatus::new(),
            tab_bar_scroll_handle: ScrollHandle::new(),
            close_workspace_requested: false,
            remote_lifecycle: RemoteHierarchyLifecycle::default(),
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

    fn create_pane_host(
        tab_id: TabId,
        session_factory: WorkspaceTerminalSessionFactory,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        pane_construction: PaneConstruction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<PaneHost> {
        let pane_host = cx.new(|cx| {
            PaneHost::new_with_prepared_launch(
                tab_id,
                session_factory,
                prepared_launch,
                pane_construction,
                window,
                cx,
            )
        });
        debug_assert_eq!(pane_host.read(cx).tab_id(), tab_id);
        cx.subscribe_in(
            &pane_host,
            window,
            |manager, _, event: &PaneHostEvent, window, cx| match event {
                PaneHostEvent::UserClosePaneRequested { tab_id, pane_id } => {
                    cx.emit(TabManagerEvent::ClosePaneRequested {
                        tab_id: *tab_id,
                        pane_id: *pane_id,
                    });
                }
                PaneHostEvent::CloseTabRequested { tab_id } => {
                    manager.close_tab(*tab_id, window, cx);
                }
                PaneHostEvent::PresentationChanged { .. } => {
                    cx.emit(TabManagerEvent::PresentationChanged);
                    cx.notify();
                }
            },
        )
        .detach();
        cx.subscribe(
            &pane_host,
            |_, _, event: &RemoteChildLaunchUnavailable, cx| {
                cx.emit(*event);
            },
        )
        .detach();
        pane_host
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        if self.active {
            self.tabs
                .active_tab()
                .update(cx, |tab, cx| tab.focus(window, cx));
        }
    }

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
        self.tabs.active_tab().update(cx, |pane_host, cx| {
            pane_host.set_focus_branch(true, blocker, cx);
            pane_host.native_service_status(workspace_id, window, cx)
        })
    }

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

    pub(crate) fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_without_focus(cx);
        self.focus(window, cx);
    }

    pub(crate) fn activate_without_focus(&mut self, cx: &mut Context<Self>) {
        self.active = true;
        self.tabs
            .active_tab()
            .update(cx, |pane_host, cx| pane_host.activate_without_focus(cx));
        self.sync_terminal_focus_blocker(cx);
    }

    pub(crate) fn deactivate(&mut self, cx: &mut Context<Self>) {
        self.active = false;
        self.tabs
            .active_tab()
            .update(cx, |pane_host, cx| pane_host.deactivate(cx));
        self.tab_selector_pressed = None;
        self.sync_terminal_focus_blocker(cx);
    }

    pub(crate) fn close_all(&self, cx: &mut App) {
        for (_, pane_host) in self.tabs.iter() {
            pane_host.update(cx, |pane_host, cx| pane_host.close_all(cx));
        }
    }

    pub(crate) fn terminal_panes<'a>(
        &'a self,
        cx: &'a App,
    ) -> impl Iterator<Item = (TabId, PaneId, &'a super::terminal_pane::TerminalPane)> {
        self.tabs.iter().flat_map(move |(tab, host)| {
            host.read(cx)
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
        let Some(host) = self.tabs.tab(tab_id).cloned() else {
            return;
        };
        host.update(cx, |host, cx| {
            host.close_pane_authorized(pane_id, window, cx)
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

    /// Atomically disconnects every Tab and Pane for the authoritative connection generation.
    ///
    /// The complete hierarchy is prevalidated before mutation. Tab IDs, Pane layouts, active and
    /// focused identities, zoom, and final presentations remain intact while input and new Remote
    /// child launches are blocked.
    pub(crate) fn disconnect_remote(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> Result<(), RemoteTabManagerLifecycleError> {
        for (tab_id, pane_host) in self.tabs.iter() {
            pane_host
                .read(cx)
                .can_disconnect_remote(generation, cx)
                .map_err(|source| RemoteTabManagerLifecycleError::Tab { tab_id, source })?;
        }
        for (_, pane_host) in self.tabs.iter() {
            pane_host.update(cx, |pane_host, cx| {
                pane_host
                    .disconnect_remote(generation, cx)
                    .expect("prevalidated Tab disconnect must remain legal")
            });
        }
        self.remote_lifecycle.disconnect(generation);
        self.sync_terminal_focus_blocker(cx);
        cx.notify();
        Ok(())
    }

    /// Revalidates and reserves one fresh Remote channel for every preserved Pane.
    ///
    /// Reservation is asynchronous and completes before hierarchy mutation. Each channel requires
    /// its own current physical-identity grant. Cancellation, stale generation, directory change,
    /// or any reservation failure drops all prepared tokens and leaves the hierarchy disconnected.
    pub(crate) fn prepare_remote_restart(
        &mut self,
        session_factory: WorkspaceTerminalSessionFactory,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> Task<Result<PreparedTabManagerRemoteRestart, RemoteTabManagerLifecycleError>> {
        let child_launch_generation = self.remote_lifecycle.begin_child_launch();
        let sources: Vec<_> = self
            .tabs
            .iter()
            .flat_map(|(_, host)| {
                host.read(cx)
                    .terminal_panes(cx)
                    .map(|(_, terminal)| terminal.current_directory())
            })
            .collect();
        // Reconnecting restores existing terminals. The workspace pin only controls future ones.
        let mut restart_factory = session_factory.clone();
        restart_factory.set_pinned_directory(None);
        let factories: Result<Vec<_>, _> = sources
            .into_iter()
            .map(|source| restart_factory.for_source_directory(source))
            .collect();
        let factories = match factories {
            Ok(factories) => factories,
            Err(_) => {
                return Task::ready(Err(
                    RemoteChannelRevalidationError::DirectoryUnavailable.into()
                ));
            }
        };
        cx.spawn(async move |manager, cx| {
            let mut prepared_launches = Vec::with_capacity(factories.len());
            for factory in factories {
                let Some(revalidation) = factory.revalidate_remote_child_launch() else {
                    return Err(RemoteTabManagerLifecycleError::PreparationSuperseded);
                };
                revalidation.await?;
                prepared_launches.push(factory.prepare_child_launch()?);
            }
            manager
                .update(cx, |manager, cx| {
                    if !manager
                        .remote_lifecycle
                        .is_current_child_launch(child_launch_generation)
                    {
                        return Err(RemoteTabManagerLifecycleError::PreparationSuperseded);
                    }
                    manager.prepare_remote_restart_with_launches(
                        session_factory,
                        generation,
                        prepared_launches,
                        cx,
                    )
                })
                .map_err(|_| RemoteTabManagerLifecycleError::PreparationSuperseded)?
        })
    }

    fn prepare_remote_restart_with_launches(
        &self,
        session_factory: WorkspaceTerminalSessionFactory,
        generation: u64,
        prepared_launches: Vec<PreparedWorkspaceTerminalLaunch>,
        cx: &App,
    ) -> Result<PreparedTabManagerRemoteRestart, RemoteTabManagerLifecycleError> {
        let mut prepared_launches = prepared_launches.into_iter();
        let mut tabs = Vec::with_capacity(self.tabs.len());
        for (tab_id, pane_host) in self.tabs.iter() {
            let pane_count = pane_host.read(cx).pane_count();
            let launches: Vec<_> = prepared_launches.by_ref().take(pane_count).collect();
            if launches.len() != pane_count {
                return Err(RemoteTabManagerLifecycleError::TabChanged(tab_id));
            }
            let prepared = pane_host
                .read(cx)
                .prepare_remote_restart(session_factory.clone(), generation, launches, cx)
                .map_err(|source| RemoteTabManagerLifecycleError::Tab { tab_id, source })?;
            tabs.push((tab_id, pane_host.clone(), prepared));
        }
        if prepared_launches.next().is_some() {
            return Err(RemoteTabManagerLifecycleError::PreparationSuperseded);
        }
        Ok(PreparedTabManagerRemoteRestart {
            session_factory,
            tabs: RemoteRestartBatch::new(tabs),
        })
    }

    /// Commits a fully prepared Remote restart across the existing Tab hierarchy.
    ///
    /// The method revalidates all Tab and Pane identities before the first commit, then replaces
    /// Terminal Sessions in place. Post-commit session startup failures remain local to each Pane.
    pub(crate) fn commit_remote_restart(
        &mut self,
        prepared: PreparedTabManagerRemoteRestart,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<(), RemoteTabManagerLifecycleError> {
        let session_factory = prepared.session_factory;
        prepared.tabs.commit(
            self.tabs.len(),
            || RemoteTabManagerLifecycleError::TabChanged(self.tabs.active_tab_id()),
            cx,
            |(tab_id, pane_host, host_restart), cx| {
                let Some(current) = self.tabs.tab(*tab_id) else {
                    return Err(RemoteTabManagerLifecycleError::TabChanged(*tab_id));
                };
                if current.entity_id() != pane_host.entity_id() {
                    return Err(RemoteTabManagerLifecycleError::TabChanged(*tab_id));
                }
                pane_host
                    .read(cx)
                    .can_commit_remote_restart(host_restart, cx)
                    .map_err(|source| RemoteTabManagerLifecycleError::Tab {
                        tab_id: *tab_id,
                        source,
                    })?;
                Ok(())
            },
            |(tab_id, pane_host, host_restart), cx| {
                pane_host.update(cx, |pane_host, cx| {
                    pane_host
                        .commit_remote_restart(host_restart, session_factory.clone(), window, cx)
                        .unwrap_or_else(|error| {
                            panic!("prevalidated Tab {tab_id} restart commit failed: {error}")
                        })
                });
            },
        )?;
        self.session_factory = session_factory;
        self.remote_lifecycle.restarted();
        self.sync_terminal_focus_blocker(cx);
        cx.emit(TabManagerEvent::PresentationChanged);
        cx.notify();
        Ok(())
    }

    pub(crate) fn aggregate_counts(&self, cx: &App) -> (usize, usize) {
        let panes = self
            .tabs
            .iter()
            .map(|(_, pane_host)| pane_host.read(cx).pane_count())
            .sum();
        (self.tabs.len(), panes)
    }

    pub(crate) fn automatic_directory(&self, cx: &App) -> Option<crate::domain::CurrentDirectory> {
        self.tabs.root_tab().read(cx).automatic_directory(cx)
    }

    #[cfg(test)]
    pub(crate) fn active_terminal_identity(&self, cx: &App) -> (TabId, PaneId) {
        let tab_id = self.tabs.active_tab_id();
        let pane_id = self.tabs.active_tab().read(cx).focused_pane_id();
        (tab_id, pane_id)
    }

    pub(crate) fn set_pinned_directory(
        &mut self,
        directory: Option<PinnedDirectory>,
        cx: &mut Context<Self>,
    ) {
        self.session_factory.set_pinned_directory(directory.clone());
        for (_, pane_host) in self.tabs.iter() {
            pane_host.update(cx, |pane_host, _| {
                pane_host.set_pinned_directory(directory.clone())
            });
        }
    }

    pub(crate) fn set_sidebar_layout(
        &mut self,
        visible: bool,
        sidebar_width: Pixels,
        top_chrome_width: Pixels,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_visible != visible
            || self.sidebar_width != sidebar_width
            || self.top_chrome_width != top_chrome_width
        {
            self.sidebar_visible = visible;
            self.sidebar_width = sidebar_width;
            self.top_chrome_width = top_chrome_width;
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
        })
    }

    fn sync_terminal_focus_blocker(&self, cx: &mut Context<Self>) {
        let blocker = self.terminal_focus_blocker();
        let active_tab_id = self.tabs.active_tab_id();
        for (tab_id, pane_host) in self.tabs.iter() {
            let active = self.active && tab_id == active_tab_id;
            pane_host.update(cx, |pane_host, cx| {
                pane_host.set_focus_branch(active, blocker, cx);
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
                if let Err(error) = self
                    .operating_system_window_drag_platform
                    .interaction_started()
                {
                    Self::report_operating_system_window_drag_error("begin", error);
                }
                self.sync_terminal_focus_blocker(cx);
                WindowDragRegionResponse::Continue
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

    #[cfg(test)]
    pub(crate) fn active_pane_host(&self) -> Entity<PaneHost> {
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
        let active_tab_id = self.tabs.active_tab_id();
        if let Some(index) = self
            .tabs
            .iter()
            .position(|(tab_id, _)| tab_id == active_tab_id)
        {
            self.tab_bar_scroll_handle.scroll_to_item(index);
        }
    }

    pub(crate) fn create_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.remote_lifecycle.disconnected_generation().is_some() {
            cx.emit(RemoteChildLaunchUnavailable::ConnectionUnavailable);
            return;
        }
        self.tab_selector_pressed = None;
        self.sync_terminal_focus_blocker(cx);
        let session_factory = match self.session_factory.for_source_directory(
            self.tabs
                .active_tab()
                .read(cx)
                .current_directory(self.tabs.active_tab().read(cx).focused_pane_id(), cx),
        ) {
            Ok(factory) => factory,
            Err(error) => {
                let detail = format!(
                    "Cannot create a Tab because {error}. Restore the directory or change the pinned directory."
                );
                let manager = cx.weak_entity();
                let window_handle = window.window_handle();
                let _ = Alert::new(
                    ModalId::new("tab-starting-directory-unavailable"),
                    "Starting directory unavailable",
                    "Starting Directory Unavailable",
                    detail,
                    vec![ModalAction::new(
                        (),
                        "OK",
                        ModalActionRole::Cancel,
                        "tab-start-error-ok",
                    )],
                )
                .intent(AlertIntent::Warning)
                .present(window, cx, move |_, cx| {
                    let _ = window_handle.update(cx, |_, window, cx| {
                        let _ = manager.update(cx, |manager, cx| manager.focus(window, cx));
                    });
                });
                return;
            }
        };
        if let Some(revalidation) = session_factory.revalidate_remote_child_launch() {
            let child_launch_generation = self.remote_lifecycle.begin_child_launch();
            cx.spawn_in(window, async move |manager, cx| {
                let revalidation = revalidation.await;
                let _ = manager.update_in(cx, |manager, window, cx| {
                    if manager.remote_lifecycle.disconnected_generation().is_some() {
                        cx.emit(RemoteChildLaunchUnavailable::Cancelled);
                        return;
                    }
                    if !manager
                        .remote_lifecycle
                        .is_current_child_launch(child_launch_generation)
                    {
                        cx.emit(RemoteChildLaunchUnavailable::Stale);
                        return;
                    }
                    if let Err(error) = revalidation {
                        cx.emit(RemoteChildLaunchUnavailable::from(error));
                        return;
                    }
                    let prepared_launch = match session_factory.prepare_child_launch() {
                        Ok(prepared_launch) => prepared_launch,
                        Err(_) => {
                            cx.emit(RemoteChildLaunchUnavailable::ConnectionUnavailable);
                            return;
                        }
                    };
                    manager.create_tab_with_prepared_launch(prepared_launch, window, cx);
                });
            })
            .detach();
            return;
        }
        let prepared_launch = match session_factory.prepare_child_launch() {
            Ok(prepared_launch) => prepared_launch,
            Err(_) => {
                cx.emit(RemoteChildLaunchUnavailable::ConnectionUnavailable);
                return;
            }
        };
        self.create_tab_with_prepared_launch(prepared_launch, window, cx);
    }

    fn create_tab_with_prepared_launch(
        &mut self,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous_tab = self.tabs.active_tab().clone();
        let session_factory = self.session_factory.clone();
        let pane_construction = self.pane_construction.clone();
        let result = self.tabs.create_tab(|tab_id| {
            Self::create_pane_host(
                tab_id,
                session_factory,
                prepared_launch,
                pane_construction,
                window,
                cx,
            )
        });
        let tab_id = match result {
            Ok(tab_id) => tab_id,
            Err(error) => {
                Self::report_tab_error("create", error);
                return;
            }
        };
        let Some(pane_host) = self.tabs.tab(tab_id).cloned() else {
            unreachable!("a newly created Tab must remain owned by its collection")
        };

        previous_tab.update(cx, |pane_host, cx| pane_host.deactivate(cx));
        if self.active {
            pane_host.update(cx, |pane_host, cx| pane_host.activate(window, cx));
        } else {
            pane_host.update(cx, |pane_host, cx| pane_host.deactivate(cx));
        }
        self.sync_terminal_focus_blocker(cx);
        self.scroll_active_tab_into_view();
        cx.emit(TabManagerEvent::PresentationChanged);
        cx.notify();
    }

    fn activate_tab(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) -> bool {
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
            previous_tab.update(cx, |pane_host, cx| pane_host.deactivate(cx));
        }
        let blocker = self.terminal_focus_blocker();
        next_tab.update(cx, |pane_host, cx| {
            pane_host.set_focus_branch(self.active, blocker, cx);
        });
        if self.active {
            next_tab.update(cx, |pane_host, cx| pane_host.activate(window, cx));
        } else {
            next_tab.update(cx, |pane_host, cx| pane_host.deactivate(cx));
        }
        self.sync_terminal_focus_blocker(cx);
        self.scroll_active_tab_into_view();
        cx.emit(TabManagerEvent::PresentationChanged);
        cx.notify();
        true
    }

    fn activate_tab_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let tab_id = self.tabs.iter().nth(index).map(|(tab_id, _)| tab_id);
        if let Some(tab_id) = tab_id {
            self.activate_tab(tab_id, window, cx);
        }
    }

    fn close_tab(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
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
                payload.update(cx, |pane_host, cx| pane_host.close_all(cx));
                if was_active {
                    let active_tab = self.tabs.active_tab().clone();
                    if self.active {
                        active_tab.update(cx, |pane_host, cx| pane_host.activate(window, cx));
                    } else {
                        active_tab.update(cx, |pane_host, cx| pane_host.deactivate(cx));
                    }
                }
                self.tab_selector_pressed = None;
                self.sync_terminal_focus_blocker(cx);
                debug_assert_eq!(active_tab_id, self.tabs.active_tab_id());
                self.scroll_active_tab_into_view();
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
        if self.close_workspace_requested || self.tabs.tab(tab_id).is_none() {
            return;
        }
        cx.emit(TabManagerEvent::CloseTabRequested { tab_id });
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

    #[expect(
        clippy::too_many_arguments,
        reason = "one Tab render step needs its identity, presentation, owner, appearance, and host geometry"
    )]
    fn render_tab_item(
        &self,
        tab_id: TabId,
        identity: TabIdentity,
        active: bool,
        presentation: &TabChromePresentation,
        manager: gpui::WeakEntity<Self>,
        appearance: &super::appearance::ChromeAppearance,
        window: &Window,
        cx: &App,
    ) -> gpui::Stateful<gpui::Div> {
        let press_manager = manager.clone();
        let release_manager = manager.clone();
        let click_manager = manager.clone();
        let hover_manager = manager.clone();
        let close_manager = manager;
        let chip = presentation.tab_chip(active, appearance, cx);
        let foreground = presentation.tab_foreground(active);
        let hover_active = appearance.active && presentation.window_active;
        let ancestor_hovered = hover_active && self.hovered_tab == Some(tab_id);
        let control_style = presentation.close_control_style(
            active,
            ancestor_hovered,
            &appearance.colors,
            appearance.materials,
        );
        let hover_foreground = presentation.tab_hover_foreground(active);
        // Native activation can precede dispatch of an accepts-first-mouse event. Retain whether
        // this action was visible in the pre-activation frame so a hidden inactive-Tab control
        // cannot close its Tab through a stale hitbox. The selected Tab remains available.
        let close_action_available = active || appearance.active;
        let status = presentation.tab_status(active, ancestor_hovered, &appearance.colors);
        let status_host = presentation.tab_surface(active, ancestor_hovered);
        let close_clearance = cx
            .global::<ButtonTheme>()
            .icon_button_size(ButtonSize::Compact)
            + appearance.spacing(TAB_TRAILING_GAP);
        let close_icon_size = appearance.icons.metrics(IconRole::Control).glyph_size;
        #[cfg(test)]
        let rendered_active_close_icon = Rc::clone(&self.rendered_active_close_icon);
        #[cfg(test)]
        let rendered_inactive_close_icon = Rc::clone(&self.rendered_inactive_close_icon);
        let text_style = appearance.typography.style(TextRole::Navigation);
        let mut identity = identity;
        identity.glyph =
            super::pane_host::drawable_reported_glyph(identity.glyph.as_ref(), |glyph| {
                super::terminal_status::reported_glyph_is_drawable(
                    glyph,
                    &text_style.font,
                    text_style.size,
                    window,
                )
            });
        let tab_group = format!("tab-item-{}", tab_id.get());
        div()
            .id(("tab-item", tab_id.get()))
            .debug_selector(move || {
                format!(
                    "tab-item-{}-{}",
                    tab_id.get(),
                    if active { "active" } else { "inactive" }
                )
            })
            .relative()
            .group(tab_group.clone())
            .h_full()
            .flex_none()
            .w(appearance.spacing(TAB_ITEM_WIDTH))
            .min_w(appearance.spacing(TAB_ITEM_MINIMUM_WIDTH))
            .max_w(appearance.spacing(TAB_ITEM_MAXIMUM_WIDTH))
            .pl(appearance.spacing(TAB_ITEM_LEFT_PADDING))
            .pr(appearance.spacing(TAB_ITEM_RIGHT_PADDING))
            .flex()
            .items_center()
            .cursor_pointer()
            .block_mouse_except_scroll()
            .on_hover(move |hovered, _, cx| {
                let _ = hover_manager.update(cx, |manager, cx| {
                    if *hovered {
                        if manager.hovered_tab != Some(tab_id) {
                            manager.hovered_tab = Some(tab_id);
                            cx.notify();
                        }
                    } else if manager.hovered_tab == Some(tab_id) {
                        manager.hovered_tab = None;
                        cx.notify();
                    }
                });
            })
            .chrome_text(text_style)
            .text_color(gpui_color(foreground))
            // Content follows the chip's paired hover paint, preserving selected identity.
            .when(hover_active, |item| {
                item.hover(move |item| item.text_color(gpui_color(hover_foreground)))
            })
            .child(chip.render(format!("tab-item-{}-chip", tab_id.get()), &tab_group))
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
                    .when(!active, |button| {
                        button.opacity(0.0).when(hover_active, |button| {
                            button.group_hover(tab_group, |button| button.opacity(1.0))
                        })
                    })
                    .child(
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
                                Icon::new(IconName::X, close_icon_size, foreground)
                                    .into_any_element()
                            },
                        )
                        .variant(ButtonVariant::Ghost)
                        .disabled(!close_action_available)
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
                        }),
                    ),
            )
    }

    fn render_tab_bar(
        &self,
        presentation: &TabChromePresentation,
        manager: gpui::WeakEntity<Self>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let appearance = super::appearance::chrome(cx);
        let active_tab_id = self.tabs.active_tab_id();
        let background = presentation.background;
        let create_icon_size = appearance.icons.metrics(IconRole::Chrome).glyph_size;
        // A chip is inset inside its item, which would add to the gap the Workspace identity before
        // the strip already leaves. The strip pulls that inset back, so the visible distance from
        // the identity to the first Tab is one frame space and the first Tab's paint lines up with
        // the Pane beneath it.
        let leading_alignment =
            super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx)
                .chip_strip_leading_offset();
        let mut items = div()
            .id("tab-items")
            .debug_selector(|| "tab-items".to_owned())
            .h_full()
            .min_w_0()
            .ml(leading_alignment)
            .flex()
            .flex_row()
            .overflow_x_scroll()
            .track_scroll(&self.tab_bar_scroll_handle);
        let mut previous_inactive_tab = None;
        for (tab_id, pane_host) in self.tabs.iter() {
            let active = tab_id == active_tab_id;
            let leading_separator =
                previous_inactive_tab
                    .filter(|_| !active)
                    .map(|leading_tab_id| {
                        render_tab_separator(leading_tab_id, tab_id, presentation, appearance)
                    });
            previous_inactive_tab = (!active).then_some(tab_id);
            items = items.child(
                self.render_tab_item(
                    tab_id,
                    pane_host.read(cx).tab_identity(),
                    active,
                    presentation,
                    manager.clone(),
                    appearance,
                    window,
                    cx,
                )
                .children(leading_separator),
            );
        }

        let drag_manager = manager.clone();
        let create_manager = manager.clone();
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
            .child(items)
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
                        .debug_selector("create-tab-button")
                        .tooltip(
                            Tooltip::new("create-tab-tooltip", "Create Tab")
                                .keyboard_equivalent(create_tab_shortcut(
                                    crate::desktop_profile::DesktopPresentation::get(cx),
                                ))
                                .debug_selector("create-tab-tooltip"),
                        )
                        .on_activate(move |_, window, cx| {
                            let _ = create_manager.update(cx, |manager, cx| {
                                manager.create_tab(window, cx);
                            });
                        }),
                    ),
            );

        let drag_region = WindowDragRegion::new(
            "tab-bar-drag-region",
            "Move Operating-System Window from Tab chrome",
            content,
        )
        .status(self.window_drag_status.clone())
        .pointer_insets(Edges {
            left: super::resize_handle_theme::spacious_target_half_thickness(cx),
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
        #[cfg(test)]
        {
            self.rendered_window_active = window.is_window_active();
        }
        let manager = cx.entity().downgrade();
        let active_tab = self.tabs.active_tab().clone();
        let appearance = super::appearance::chrome(cx);
        let presentation = TabChromePresentation::resolve(
            window.is_window_active(),
            appearance.capabilities.show_borders,
            &appearance.colors,
        );
        let tab_bar = self.render_tab_bar(&presentation, manager.clone(), window, cx);
        let frame = super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx);
        let stage_surface = super::workspace_frame::base_surface(&appearance.colors);

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
                    .id("tab-manager-content")
                    .debug_selector(|| "tab-manager-content".to_owned())
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .when(self.sidebar_visible, |body| body.ml(self.sidebar_width))
                    // The content stage is base surface, and every gap it paints is measured to the
                    // next painted surface rather than counted in layout properties. It has no top
                    // edge: the chrome above already carries that space in its own height. Beside a
                    // sidebar it has no leading edge either, because the sidebar chip's own trailing
                    // margin is already that gap and two insets of one continuous surface would read
                    // as a gap of twice the size.
                    .border_color(gpui_color(
                        appearance.surface(crate::appearance::SurfaceRole::Base, stage_surface),
                    ))
                    .border_b(frame.window_edge_inset())
                    .border_r(frame.window_edge_inset())
                    .border_l(frame.stage_leading_inset(self.sidebar_visible))
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
                    .child(active_tab),
            )
    }
}

impl EventEmitter<TabManagerEvent> for TabManager {}
impl EventEmitter<RemoteChildLaunchUnavailable> for TabManager {}

/// The quiet mark at the boundary between two neighbouring inactive Tabs.
///
/// The trailing Tab carries the mark as paint just inside its own edge on the shared boundary, so
/// the row keeps one scroll child per Tab and both Tabs keep their spacing, hit targets, and hover
/// regions. Staying inside the Tab's bounds and on whole points leaves layout rounding nothing to
/// move, and the chip inset keeps hover paint clear of it.
///
/// The mark paints its own `tab_separator` role. `border` describes full-length structure and is
/// too close to the bar to show on a mark this short, and an outlined control's ring is a separate
/// decision a scheme must be able to retune without moving the Tab strip.
fn render_tab_separator(
    leading_tab_id: TabId,
    trailing_tab_id: TabId,
    presentation: &TabChromePresentation,
    appearance: &super::appearance::ChromeAppearance,
) -> AnyElement {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left_0()
        .w(px(TAB_SEPARATOR_WIDTH))
        .flex()
        .items_center()
        .child(
            div()
                .debug_selector(move || {
                    format!(
                        "tab-separator-{}-{}",
                        leading_tab_id.get(),
                        trailing_tab_id.get()
                    )
                })
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

/// One Tab's identity: `<status glyph> <activity> · <place>`.
///
/// The status glyph is the segment a Tab never gives up. Only the words after it narrow: the
/// activity first, then the place.
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
                    }
                    .render(),
                ),
        )
        .child(words)
        .into_any_element()
}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
}

fn title_bar_control_focus_ring(appearance: &super::appearance::ChromeAppearance) -> Color {
    appearance.title_bar_controls.colors.focus_ring
}

fn create_tab_shortcut(presentation: &crate::desktop_profile::DesktopPresentation) -> &'static str {
    presentation.shortcut(&crate::ui::CreateTab)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use gpui::{
        Modifiers, MouseDownEvent, MouseExitEvent, MouseUpEvent, ScrollDelta, ScrollWheelEvent,
        TestAppContext, TouchPhase, VisualTestContext, point,
    };

    use super::*;

    #[test]
    fn create_tab_tooltip_should_use_the_host_neutral_profile_shortcut() {
        assert_eq!(
            create_tab_shortcut(&crate::desktop_profile::testing_presentation()),
            "Primary+T"
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
            SchemeCatalog, SystemAppearance,
        };

        let mut resolved = SchemeCatalog::default()
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
        let appearance =
            prepared_opposing_title_bar(Color::rgb(0x000000), Color::rgb(0xffffff), true, false);
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
        let appearance =
            prepared_opposing_title_bar(Color::rgb(0xffffff), Color::rgb(0x000000), false, true);
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
    fn active_window_tab_chrome_should_preserve_the_existing_presentation() {
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
            tab_inactive_selected_background: Color::rgb(0x334455),
            tab_inactive_selected_border: Color::rgb(0x3a4b5c),
            tab_inactive_selected_foreground: Color::rgb(0xbbccdd),
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

    /// The Active Tab carries selected-row content on a material tuned for the title bar.
    ///
    /// The Workspace sidebar and Settings navigation need a fill that works on shell and raised
    /// surfaces. The Active Tab has one title-bar host, so it keeps the shared text hierarchy while
    /// using its own borderless fill and hover response.
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

    /// A separator is a short hairline, so it needs more contrast than a full-length divider to be
    /// seen at all, yet it must stay a step quieter than the titles it sits between. The mark rests
    /// on the title bar in a focused window and on the inactive title bar in an unfocused one, so
    /// both surfaces are held to the same band.
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

    /// A custom scheme tunes the Tab separator and outlined controls as two decisions.
    ///
    /// The scheme travels the production path, from a native color document through the catalog
    /// and prepared Chrome into both consumers, so an alias anywhere along it would tie a retuned
    /// outline ring to the Tab strip or the reverse.
    #[test]
    fn custom_scheme_should_tune_tab_separators_independently_of_outlined_controls() {
        use crate::appearance::{
            AppearanceGeneration, AppearanceMode, AppearancePreferences, AvailableFonts,
            SchemeCatalog, SchemeId, SystemAppearance, parse_color_document,
        };

        let prepare = |outline: &str, separator: &str| {
            let document = format!(
                r##"{{"schema_version":1,"schemes":[{{"kind":"chrome","id":"test.tab-separator","name":"Tab separator","appearance":"light","colors":{{"background":"#fbfbfc","text":"#1d1f23","outline_border":"{outline}","tab_separator":"{separator}"}}}}]}}"##
            );
            let document = parse_color_document(document.as_bytes()).unwrap();
            let catalog = SchemeCatalog::from_custom_schemes(&document.schemes).unwrap();
            let mut preferences = AppearancePreferences {
                mode: AppearanceMode::Light,
                ..Default::default()
            };
            preferences.chrome.schemes.light = SchemeId::new("test.tab-separator").unwrap();
            let resolved = catalog
                .resolve(
                    AppearanceGeneration::INITIAL,
                    &preferences,
                    SystemAppearance::unavailable(),
                    &AvailableFonts::default(),
                )
                .unwrap();
            super::super::appearance::ChromeAppearance::prepare(&resolved.chrome).colors
        };
        let separators = |colors: &ChromeColors| {
            [true, false].map(|window_active| {
                TabChromePresentation::resolve(window_active, false, colors).tab_separator
            })
        };

        let baseline = prepare("#c8cbd3", "#b9bcc4");
        assert_eq!(separators(&baseline), [Color::rgb(0xb9bcc4); 2]);

        let retuned_outline = prepare("#5a5e66", "#b9bcc4");
        assert_ne!(
            super::super::button_theme::theme(&retuned_outline),
            super::super::button_theme::theme(&baseline),
            "the outlined control should follow its own ring"
        );
        assert_eq!(
            separators(&retuned_outline),
            separators(&baseline),
            "retuning outlined controls should leave the Tab separator alone"
        );

        let retuned_separator = prepare("#c8cbd3", "#d4d6dc");
        assert_eq!(separators(&retuned_separator), [Color::rgb(0xd4d6dc); 2]);
        assert_eq!(
            super::super::button_theme::theme(&retuned_separator),
            super::super::button_theme::theme(&baseline),
            "retuning the Tab separator should leave outlined controls alone"
        );
    }

    /// The separator keeps one logical point at every supported display scale, which never
    /// rasterises below one whole device pixel: a single crisp pixel at 1x and more on denser
    /// displays, so the mark stays thin without disappearing.
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
            tab_inactive_selected_background: Color::rgb(0x556677),
            tab_active_hover_background: Color::rgb(0x667788),
            tab_hover_background: Color::rgb(0x778899),
            tab_active_icon: Color::rgb(0x112233),
            tab_inactive_selected_icon: Color::rgb(0x223344),
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
            SchemeCatalog, SurfaceRole, SystemAppearance,
        };

        let mut preferences = AppearancePreferences::default();
        preferences.background.transparency = 1.0;
        let resolved = SchemeCatalog::default()
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
        cx.run_until_parked();
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
            active.colors.tab_active_background = Color::rgb(0x000000);
            active.colors.tab_active_icon = Color::rgb(0xffffff);
            active.colors.tab_active_hover_background = Color::rgb(0xffffff);
            active.colors.tab_active_hover_foreground = Color::rgb(0x000000);
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
        cx.run_until_parked();
        assert_eq!(
            rendered.get(),
            parent_hover,
            "hovering the Active Tab title should repaint its rendered close glyph for the selected-hover fill"
        );

        cx.simulate_mouse_move(close, None, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            rendered.get(),
            direct_hover,
            "direct close hover should retain the IconButton's own foreground"
        );

        cx.simulate_mouse_move(title, None, Modifiers::none());
        cx.run_until_parked();
        cx.deactivate_window();
        cx.run_until_parked();
        let inactive_resting = rendered.get();
        cx.simulate_mouse_move(title, None, Modifiers::none());
        cx.run_until_parked();
        assert!(!manager.read_with(cx, |manager, _| manager.rendered_window_active));
        assert_eq!(
            rendered.get(),
            inactive_resting,
            "an inactive window should retain the resting selected-Tab close glyph"
        );

        cx.simulate_mouse_move(close, None, Modifiers::none());
        cx.run_until_parked();
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
        cx.run_until_parked();
        assert_eq!(rendered_inactive.get(), inactive_tab_resting);
        cx.simulate_mouse_move(inactive_close, None, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(rendered_inactive.get(), inactive_tab_resting);

        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        cx.simulate_mouse_move(inactive_title, None, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(rendered_inactive.get(), direct_hover);
        cx.simulate_mouse_move(inactive_close, None, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(rendered_inactive.get(), direct_hover);
    }
    use crate::domain::PaneId;
    use crate::domain::ZoomState;
    use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
    use crate::ssh::command::{SshCommandContext, ValidatedRemoteShellCommand};
    use crate::terminal::testing::{
        RecordedSessionCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
    };
    use crate::terminal::{
        RemoteChannelUnavailable, RemoteTerminalChannelProvider, SessionEvent, SessionExit,
        SessionFailure, TerminalSessionFactory,
    };
    use crate::ui::TogglePaneZoom;

    struct RemoteLaunchEventHarness {
        manager: Entity<TabManager>,
        events: Rc<RefCell<Vec<RemoteChildLaunchUnavailable>>>,
    }

    type PaneHierarchyIdentity = (
        TabId,
        gpui::EntityId,
        Vec<(PaneId, gpui::EntityId)>,
        String,
        PaneId,
        ZoomState,
    );
    type TabHierarchyIdentity = (TabId, Vec<PaneHierarchyIdentity>);

    impl Render for RemoteLaunchEventHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.manager.clone()
        }
    }

    struct SequencedRemoteChannelProvider {
        ready: AtomicBool,
        grant: AtomicBool,
        preparations: AtomicUsize,
        revalidations: AtomicUsize,
        fail_at: Mutex<Option<usize>>,
        revalidation_error: Mutex<Option<RemoteChannelRevalidationError>>,
        invalidate_grant_after_revalidation: AtomicBool,
        command_context: SshCommandContext,
    }

    impl SequencedRemoteChannelProvider {
        fn new(destination: crate::domain::SshDestination) -> Self {
            Self {
                ready: AtomicBool::new(true),
                grant: AtomicBool::new(true),
                preparations: AtomicUsize::new(0),
                revalidations: AtomicUsize::new(0),
                fail_at: Mutex::new(None),
                revalidation_error: Mutex::new(None),
                invalidate_grant_after_revalidation: AtomicBool::new(false),
                command_context: SshCommandContext::new(
                    crate::ssh::command::OpenSshExecutable::for_test(),
                    PathBuf::from("/private/config/spaceterm/ssh_config"),
                    destination,
                    PathBuf::from("/private/runtime/spaceterm/master.sock"),
                )
                .unwrap(),
            }
        }

        fn set_ready(&self, ready: bool) {
            self.ready.store(ready, Ordering::Release);
        }

        fn fail_at(&self, preparation: Option<usize>) {
            *self.fail_at.lock().unwrap() = preparation;
        }

        fn preparation_count(&self) -> usize {
            self.preparations.load(Ordering::Acquire)
        }

        fn revalidation_count(&self) -> usize {
            self.revalidations.load(Ordering::Acquire)
        }

        fn fail_revalidation_with(&self, error: Option<RemoteChannelRevalidationError>) {
            *self.revalidation_error.lock().unwrap() = error;
        }

        fn invalidate_next_grant(&self) {
            self.invalidate_grant_after_revalidation
                .store(true, Ordering::Release);
        }
    }

    impl RemoteTerminalChannelProvider for SequencedRemoteChannelProvider {
        fn is_ready(&self) -> bool {
            self.ready.load(Ordering::Acquire)
        }

        fn revalidate(
            &self,
            _directory: crate::domain::RemoteDirectory,
            _expected_identity: Option<crate::domain::RemoteDirectoryIdentity>,
        ) -> Task<Result<(), crate::terminal::RemoteChannelRevalidationError>> {
            self.revalidations.fetch_add(1, Ordering::AcqRel);
            self.grant.store(false, Ordering::Release);
            let result = self.revalidation_error.lock().unwrap().map_or(Ok(()), Err);
            if result.is_ok()
                && !self
                    .invalidate_grant_after_revalidation
                    .swap(false, Ordering::AcqRel)
            {
                self.grant.store(true, Ordering::Release);
            }
            Task::ready(result)
        }

        fn prepare(
            &self,
            _directory: &crate::domain::RemoteDirectory,
        ) -> Result<crate::ssh::command::PreparedSshPaneChannelCommand, RemoteChannelUnavailable>
        {
            if !self.grant.swap(false, Ordering::AcqRel) {
                return Err(RemoteChannelUnavailable);
            }
            let preparation = self.preparations.fetch_add(1, Ordering::AcqRel) + 1;
            if *self.fail_at.lock().unwrap() == Some(preparation) {
                return Err(RemoteChannelUnavailable);
            }
            Ok(self.command_context.prepare_pane_channel(
                ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
            ))
        }
    }

    fn remote_tab_manager_with_provider(
        cx: &mut TestAppContext,
        provider: Arc<SequencedRemoteChannelProvider>,
    ) -> (
        Entity<TabManager>,
        TestTerminalSessionRecords,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let session_factory =
            remote_session_factory_with_provider(records.clone(), destination, provider);
        let (manager, cx) =
            cx.add_window_view(|window, cx| TabManager::new(session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            manager.update(cx, |manager, cx| manager.focus(window, cx));
        });
        cx.run_until_parked();
        (manager, records, cx)
    }

    fn remote_tab_manager_with_provider_and_events(
        cx: &mut TestAppContext,
        provider: Arc<SequencedRemoteChannelProvider>,
    ) -> (
        Entity<TabManager>,
        TestTerminalSessionRecords,
        Rc<RefCell<Vec<RemoteChildLaunchUnavailable>>>,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let session_factory =
            remote_session_factory_with_provider(records.clone(), destination, provider);
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded_events = Rc::clone(&events);
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let manager = cx.new(|cx| TabManager::new(session_factory, window, cx));
            cx.subscribe(
                &manager,
                move |_, _, event: &RemoteChildLaunchUnavailable, _| {
                    recorded_events.borrow_mut().push(*event);
                },
            )
            .detach();
            RemoteLaunchEventHarness { manager, events }
        });
        let (manager, events) = harness.read_with(cx, |harness, _| {
            (harness.manager.clone(), Rc::clone(&harness.events))
        });
        cx.update(|window, cx| {
            window.activate_window();
            manager.update(cx, |manager, cx| manager.focus(window, cx));
        });
        cx.run_until_parked();
        (manager, records, events, cx)
    }

    fn hierarchy_identity(manager: &TabManager, cx: &App) -> TabHierarchyIdentity {
        (
            manager.tabs.active_tab_id(),
            manager
                .tabs
                .iter()
                .map(|(tab_id, pane_host)| {
                    let host = pane_host.read(cx);
                    (
                        tab_id,
                        pane_host.entity_id(),
                        host.pane_entity_ids(),
                        host.layout_signature(),
                        host.focused_pane_id(),
                        host.zoom_state(),
                    )
                })
                .collect(),
        )
    }

    fn prepare_remote_restart_for_test(
        manager: &Entity<TabManager>,
        session_factory: WorkspaceTerminalSessionFactory,
        generation: u64,
        cx: &mut VisualTestContext,
    ) -> Result<PreparedTabManagerRemoteRestart, RemoteTabManagerLifecycleError> {
        let task = manager.update(cx, |manager, cx| {
            manager.prepare_remote_restart(session_factory, generation, cx)
        });
        let result = Rc::new(RefCell::new(None));
        let task_result = Rc::clone(&result);
        cx.update(|_, cx| {
            cx.spawn(async move |_| {
                *task_result.borrow_mut() = Some(task.await);
            })
            .detach();
        });
        cx.run_until_parked();
        result
            .borrow_mut()
            .take()
            .expect("remote restart preparation task must finish")
    }

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

    fn remote_tab_manager(
        cx: &mut TestAppContext,
    ) -> (
        Entity<TabManager>,
        TestTerminalSessionRecords,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let command_context = Arc::new(
            SshCommandContext::new(
                crate::ssh::command::OpenSshExecutable::for_test(),
                PathBuf::from("/private/config/spaceterm/ssh_config"),
                destination.clone(),
                PathBuf::from("/private/runtime/spaceterm/master.sock"),
            )
            .unwrap(),
        );
        let session_factory = remote_session_factory_with_provider(
            records.clone(),
            destination,
            Arc::new(move || {
                Ok(command_context.prepare_pane_channel(
                    ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
                ))
            }),
        );
        let (manager, cx) =
            cx.add_window_view(|window, cx| TabManager::new(session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            manager.update(cx, |manager, cx| manager.focus(window, cx));
        });
        cx.run_until_parked();
        (manager, records, cx)
    }

    fn remote_session_factory_with_provider(
        records: TestTerminalSessionRecords,
        destination: crate::domain::SshDestination,
        provider: Arc<dyn RemoteTerminalChannelProvider>,
    ) -> WorkspaceTerminalSessionFactory {
        remote_session_factory_with_terminal_factory(
            Rc::new(TestTerminalSessionFactory::new(records)),
            destination,
            provider,
        )
    }

    fn remote_session_factory_with_terminal_factory(
        terminal_factory: Rc<dyn TerminalSessionFactory>,
        destination: crate::domain::SshDestination,
        provider: Arc<dyn RemoteTerminalChannelProvider>,
    ) -> WorkspaceTerminalSessionFactory {
        WorkspaceTerminalSessionFactory::new_remote(
            terminal_factory,
            crate::domain::ValidatedLocalDirectory::new(
                PathBuf::from("/missing/local/home-is-not-a-workspace"),
                crate::domain::LocalDirectoryIdentity::for_test(79083),
            ),
            crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                destination,
                crate::domain::RemoteDirectory::new("~/project".to_owned()).unwrap(),
            ),
            crate::domain::RemoteDirectoryIdentity::new("/home/tester/project".to_owned()).unwrap(),
            "project on remote".to_owned(),
            provider,
        )
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
            .unwrap()
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
    fn right_clicking_a_tab_should_leave_the_active_tab_and_terminal_focus_unchanged(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = tab_manager(cx);
        click("create-tab-button", cx);
        let commands_before = records.commands().len();
        right_click("tab-item-1-inactive", cx);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager.tabs.active_tab_id()),
            TabId::new(2)
        );
        assert!(cx.debug_bounds("tab-menu-button").is_none());
        assert!(cx.debug_bounds("menu-panel-0").is_none());
        assert!(cx.update(|window, cx| {
            manager
                .read(cx)
                .focused_terminal_has_input_focus(window, cx)
        }));
        assert_eq!(records.commands().len(), commands_before);
    }

    /// Tabs read as shapes resting inside the title bar rather than as a strip cut into it.
    ///
    /// The chip is what carries that reading, and it only works while it keeps air on every side:
    /// against its own item, against the chip beside it, and against the bar's lower edge, which
    /// meets the base surface without a seam. The item itself keeps the full height of the bar,
    /// because the inset is paint and must never shrink what a pointer can hit.
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

    /// Opens a fresh four-Tab row for each density and selected position and checks every
    /// boundary.
    ///
    /// Each position gets its own window because rendered debug bounds outlive the frame that drew
    /// them, so a mark that disappears could not otherwise be told apart from one still drawn.
    ///
    /// A boundary is marked only while both of its Tabs are inactive. The mark is a hairline one
    /// logical point wide at every density, whose length scales with density from its 18-point
    /// Compact baseline to 22.5 points at Comfortable. It is laid out on whole device pixels, painted entirely inside one of
    /// the two Tabs against their shared edge and clear of both chips, so neither layout rounding,
    /// an ancestor's clip, nor a neighbour's paint can take it away. It carries no hit target of its
    /// own. The shared edge is found from the rendered items rather than assumed to run left to
    /// right.
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
            for selected in 1..=4_u64 {
                let (manager, _records, cx) = open_tab_manager(cx);
                cx.update(|window, cx| {
                    manager.update(cx, |manager, cx| {
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

                for leading in 1..4_u64 {
                    let trailing = leading + 1;
                    let separator = cx.debug_bounds(leaked_selector(format!(
                        "tab-separator-{leading}-{trailing}"
                    )));
                    let leading_item = item(leading, cx);
                    let trailing_item = item(trailing, cx);
                    let shared_edge = if leading_item.right() == trailing_item.left() {
                        leading_item.right()
                    } else {
                        assert_eq!(
                            trailing_item.right(),
                            leading_item.left(),
                            "neighbouring Tabs {leading} and {trailing} should keep contiguous hit \
                         targets with Tab {selected} selected"
                        );
                        leading_item.left()
                    };

                    let touches_selected = leading == selected || trailing == selected;
                    match (separator, touches_selected) {
                        (Some(separator), false) => {
                            assert_eq!(
                                separator.size.width,
                                px(TAB_SEPARATOR_WIDTH),
                                "the separator should stay a one-point hairline at {density:?} \
                             density, got {separator:?}"
                            );
                            assert!(
                                on_device_pixels(separator.left())
                                    && on_device_pixels(separator.size.width),
                                "the separator should be laid out on whole device pixels at scale \
                             {scale_factor}, got {separator:?}"
                            );
                            assert!(
                                separator.size.height == expected_length
                                    && separator.size.height < leading_item.size.height,
                                "the separator should scale to {expected_length:?} at {density:?} \
                             density and stay shorter than the Tab, got {separator:?}"
                            );
                            assert!(
                                (separator.center().y - leading_item.center().y).abs() <= px(0.5),
                                "the separator should be centred on the bar, got {separator:?}"
                            );
                            assert!(
                                within(separator, leading_item) || within(separator, trailing_item),
                                "the separator should paint inside one of Tabs {leading} and \
                             {trailing} rather than across their edge, got {separator:?} between \
                             {leading_item:?} and {trailing_item:?}"
                            );
                            assert!(
                                separator.left() == shared_edge || separator.right() == shared_edge,
                                "the separator should rest against the shared edge of Tabs {leading} \
                             and {trailing}, got {separator:?} at {shared_edge:?}"
                            );
                            for tab in [leading, trailing] {
                                let chip = chip(tab, cx);
                                assert!(
                                    !overlaps(separator, chip),
                                    "the separator should stay clear of Tab {tab}'s chip, got \
                                 {separator:?} and {chip:?}"
                                );
                            }
                        }
                        (None, true) => {}
                        (Some(_), true) => panic!(
                            "no separator should touch selected Tab {selected} at boundary \
                         {leading}-{trailing}"
                        ),
                        (None, false) => panic!(
                            "inactive Tabs {leading} and {trailing} should be separated with Tab \
                         {selected} selected"
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
    fn remote_tab_creation_skips_local_validation_and_preserves_launch_context(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = remote_tab_manager(cx);

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.create_tab(window, cx));
        });
        cx.run_until_parked();

        assert_eq!(manager.read_with(cx, |manager, _| manager.tabs.len()), 2);
        assert_eq!(records.starts().len(), 2);
        assert!(records.starts().iter().all(|start| {
            start.remote_launch_plan().is_some_and(|plan| {
                plan.destination().as_str() == "tester@remote"
                    && plan.remote_directory().as_str() == "~/project"
            })
        }));
    }

    #[gpui::test]
    fn remote_tab_creation_should_leave_hierarchy_unchanged_when_channel_reservation_fails(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let records = TestTerminalSessionRecords::default();
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let command_context = Arc::new(
            SshCommandContext::new(
                crate::ssh::command::OpenSshExecutable::for_test(),
                PathBuf::from("/private/config/spaceterm/ssh_config"),
                destination.clone(),
                PathBuf::from("/private/runtime/spaceterm/master.sock"),
            )
            .unwrap(),
        );
        let preparations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = {
            let preparations = Arc::clone(&preparations);
            Arc::new(move || {
                if preparations.fetch_add(1, std::sync::atomic::Ordering::AcqRel) == 0 {
                    Ok(command_context.prepare_pane_channel(
                        ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
                    ))
                } else {
                    Err(RemoteChannelUnavailable)
                }
            })
        };
        let session_factory =
            remote_session_factory_with_provider(records.clone(), destination, provider);
        let (manager, cx) =
            cx.add_window_view(|window, cx| TabManager::new(session_factory, window, cx));
        cx.run_until_parked();

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.create_tab(window, cx));
        });
        cx.run_until_parked();

        assert_eq!(
            manager.read_with(cx, |manager, _| {
                (manager.tabs.len(), manager.tabs.active_tab_id())
            }),
            (1, TabId::new(1))
        );
        assert_eq!(records.starts().len(), 1);
        assert_eq!(preparations.load(std::sync::atomic::Ordering::Acquire), 2);
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
    /// fallback title, and is followed by the Current Directory leaf.
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
            metadata.directory.path = Arc::from("/tmp/api");
            metadata.command = Some(CommandMetadata {
                line: Arc::from("cargo test"),
                state: CommandState::Running,
            });
        });
        cx.run_until_parked();
        assert_eq!(tab_title(cx).as_ref(), "cargo test · api");

        report_metadata(&records, 1, 2, |metadata| {
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
            metadata.directory.path = Arc::from("/tmp/api");
        });
        cx.run_until_parked();
        assert_eq!(tab_title(cx).as_ref(), "Terminal · api");
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
            manager.tabs.active_tab().update(cx, |host, cx| {
                host.set_test_attention(PaneId::new(2), 1, cx);
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
        manager.update(cx, |manager, cx| {
            manager.tabs.active_tab().update(cx, |host, cx| {
                host.set_test_attention(PaneId::new(1), 1, cx);
            });
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("tab-status-1-attention").is_some());
        assert!(cx.debug_bounds("pane-status-1-attention").is_some());
    }

    /// A program that draws its own glyph gets that glyph in the Session's slot, not beside it.
    #[gpui::test]
    fn local_reported_glyph_should_take_the_session_glyph(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        assert_reported_glyph(&manager, &records, false, cx);
    }

    #[gpui::test]
    fn remote_reported_glyph_should_take_the_session_glyph(cx: &mut TestAppContext) {
        let (manager, records, cx) = remote_tab_manager(cx);
        assert_reported_glyph(&manager, &records, true, cx);
    }

    #[gpui::test]
    fn remote_disconnect_should_clear_cached_pane_and_tab_progress(cx: &mut TestAppContext) {
        use super::super::terminal_status::TerminalProgress;
        use crate::terminal::metadata::ProgressMetadata;

        let (manager, records, cx) = remote_tab_manager(cx);
        report_metadata(&records, 1, 1, |metadata| {
            metadata.progress = ProgressMetadata::Indeterminate;
        });
        cx.run_until_parked();

        manager
            .update(cx, |manager, cx| manager.disconnect_remote(1, cx))
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            manager.read_with(cx, |manager, cx| {
                let host = manager.tabs.active_tab().read(cx);
                (host.cached_focused_progress(), host.tab_identity().progress)
            }),
            (TerminalProgress::None, TerminalProgress::None)
        );
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
            .try_send(SessionEvent::Failed(SessionFailure::Runtime(
                "worker stopped".to_owned(),
            )))
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            manager.read_with(cx, |manager, cx| {
                let host = manager.tabs.active_tab().read(cx);
                (host.cached_focused_progress(), host.tab_identity().progress)
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
        // The Tab carries one glyph, in the slot the Session's own glyph would have taken.
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

        // A title without a glyph leaves the Session with its own.
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

    /// Local and Remote Sessions present every OSC 9;4 state the same way in the Tab and the Pane
    /// Caption, independently of the title, and removing the status leaves no mark.
    #[gpui::test]
    fn local_progress_should_present_each_state_in_tab_and_caption(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        assert_progress_states(&manager, &records, false, cx);
    }

    #[gpui::test]
    fn remote_progress_should_present_each_state_in_tab_and_caption(cx: &mut TestAppContext) {
        let (manager, records, cx) = remote_tab_manager(cx);
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
        let (manager, records, cx) = remote_tab_manager(cx);
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

    /// A hidden Tab receives no Screens, yet its item follows the Session's title and progress.
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
                .map(|(_, host)| host.read(cx).tab_identity())
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
    /// so every Tab stays identifiable and closable at the narrowest width.
    #[gpui::test]
    fn narrow_tabs_should_keep_status_glyph_and_close_reachable(cx: &mut TestAppContext) {
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
            let pane_host = first_tab.read(cx);
            (
                manager.read(cx).tabs.active_tab_id(),
                pane_host.focused_pane_id(),
                pane_host.focused_terminal_is_focused(window, cx),
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

        assert_eq!(platform.counts(), (1, 1, 1, 0));
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
                RecordedSessionCommand::Focus(focused) => Some((call.session_id, focused)),
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
                RecordedSessionCommand::Focus(focused) => Some((call.session_id, focused)),
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
            .expect("Tab 1 session must have started");

        first_sender
            .try_send(SessionEvent::Exited(SessionExit::Success))
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
            .expect("Tab 2 session must have started");
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.deactivate(cx));
            window.blur(cx);
        });

        active_sender
            .try_send(SessionEvent::Exited(SessionExit::Success))
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
            .expect("Tab 2 session must have started");

        active_sender
            .try_send(SessionEvent::Exited(SessionExit::Success))
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
    }

    #[gpui::test]
    fn remote_create_tab_should_revalidate_before_mutating_the_hierarchy(cx: &mut TestAppContext) {
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(SequencedRemoteChannelProvider::new(destination));
        let (manager, records, cx) = remote_tab_manager_with_provider(cx, Arc::clone(&provider));
        let before = manager.read_with(cx, hierarchy_identity);

        provider.fail_revalidation_with(Some(RemoteChannelRevalidationError::IdentityChanged));
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.create_tab(window, cx));
        });
        cx.run_until_parked();

        assert_eq!(manager.read_with(cx, hierarchy_identity), before);
        assert_eq!(records.starts().len(), 1);
        assert_eq!(provider.preparation_count(), 1);
        assert_eq!(provider.revalidation_count(), 1);

        provider.fail_revalidation_with(None);
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.create_tab(window, cx));
        });
        cx.run_until_parked();

        assert_eq!(manager.read_with(cx, |manager, _| manager.tabs.len()), 2);
        assert_eq!(records.starts().len(), 2);
        assert_eq!(provider.preparation_count(), 2);
        assert_eq!(provider.revalidation_count(), 2);
    }

    #[gpui::test]
    fn remote_create_tab_should_emit_each_typed_revalidation_failure_without_mutation(
        cx: &mut TestAppContext,
    ) {
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(SequencedRemoteChannelProvider::new(destination));
        let (manager, records, events, cx) =
            remote_tab_manager_with_provider_and_events(cx, Arc::clone(&provider));
        let before = manager.read_with(cx, hierarchy_identity);

        for error in [
            RemoteChannelRevalidationError::ConnectionUnavailable,
            RemoteChannelRevalidationError::DirectoryUnavailable,
            RemoteChannelRevalidationError::IdentityChanged,
        ] {
            provider.fail_revalidation_with(Some(error));
            cx.update(|window, cx| {
                manager.update(cx, |manager, cx| manager.create_tab(window, cx));
            });
            cx.run_until_parked();
        }

        assert_eq!(
            events.borrow().as_slice(),
            [
                RemoteChildLaunchUnavailable::ConnectionUnavailable,
                RemoteChildLaunchUnavailable::DirectoryUnavailable,
                RemoteChildLaunchUnavailable::IdentityChanged,
            ]
        );
        assert_eq!(manager.read_with(cx, hierarchy_identity), before);
        assert_eq!(records.starts().len(), 1);
        assert_eq!(provider.preparation_count(), 1);
    }

    #[gpui::test]
    fn remote_create_tab_should_report_consumed_grant_supersession_and_cancellation_once(
        cx: &mut TestAppContext,
    ) {
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(SequencedRemoteChannelProvider::new(destination));
        let (manager, records, events, cx) =
            remote_tab_manager_with_provider_and_events(cx, Arc::clone(&provider));
        let before = manager.read_with(cx, hierarchy_identity);

        provider.invalidate_next_grant();
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.create_tab(window, cx));
        });
        cx.run_until_parked();

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.create_tab(window, cx);
                manager.remote_lifecycle.begin_child_launch();
            });
        });
        cx.run_until_parked();

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.create_tab(window, cx);
                manager.disconnect_remote(1, cx).unwrap();
            });
        });
        cx.run_until_parked();

        assert_eq!(
            events.borrow().as_slice(),
            [
                RemoteChildLaunchUnavailable::ConnectionUnavailable,
                RemoteChildLaunchUnavailable::Stale,
                RemoteChildLaunchUnavailable::Cancelled,
            ]
        );
        assert_eq!(manager.read_with(cx, hierarchy_identity), before);
        assert_eq!(records.starts().len(), 1);
    }

    #[gpui::test]
    fn remote_split_failure_should_be_forwarded_once_by_tab_manager(cx: &mut TestAppContext) {
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(SequencedRemoteChannelProvider::new(destination));
        let (manager, records, events, cx) =
            remote_tab_manager_with_provider_and_events(cx, Arc::clone(&provider));
        let before = manager.read_with(cx, hierarchy_identity);
        provider.fail_revalidation_with(Some(RemoteChannelRevalidationError::IdentityChanged));

        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();

        assert_eq!(
            events.borrow().as_slice(),
            [RemoteChildLaunchUnavailable::IdentityChanged]
        );
        assert_eq!(manager.read_with(cx, hierarchy_identity), before);
        assert_eq!(records.starts().len(), 1);
    }

    #[gpui::test]
    fn remote_create_tab_should_not_mutate_when_generation_changes_after_revalidation(
        cx: &mut TestAppContext,
    ) {
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(SequencedRemoteChannelProvider::new(destination));
        let (manager, records, cx) = remote_tab_manager_with_provider(cx, Arc::clone(&provider));
        let before = manager.read_with(cx, hierarchy_identity);
        provider.invalidate_next_grant();

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.create_tab(window, cx));
        });
        cx.run_until_parked();

        assert_eq!(manager.read_with(cx, hierarchy_identity), before);
        assert_eq!(records.starts().len(), 1);
        assert_eq!(provider.preparation_count(), 1);
        assert_eq!(provider.revalidation_count(), 1);
    }

    #[gpui::test]
    fn remote_restart_reserves_all_channels_before_preserving_and_restarting_hierarchy(
        cx: &mut TestAppContext,
    ) {
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(SequencedRemoteChannelProvider::new(destination));
        let (manager, records, cx) = remote_tab_manager_with_provider(cx, Arc::clone(&provider));
        cx.simulate_keystrokes("cmd-d");
        cx.dispatch_action(TogglePaneZoom);
        click("create-tab-button", cx);
        cx.run_until_parked();
        assert_eq!(records.starts().len(), 3);

        manager
            .update(cx, |manager, cx| manager.disconnect_remote(4, cx))
            .unwrap();
        cx.simulate_keystrokes("cmd-d");
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.create_tab(window, cx));
        });
        cx.run_until_parked();
        assert_eq!(
            manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
            (2, 3)
        );
        let before = manager.read_with(cx, hierarchy_identity);

        provider.fail_revalidation_with(Some(RemoteChannelRevalidationError::IdentityChanged));
        let factory = manager.read_with(cx, |manager, _| manager.session_factory.clone());
        let identity_changed = prepare_remote_restart_for_test(&manager, factory, 5, cx);
        assert!(matches!(
            identity_changed,
            Err(RemoteTabManagerLifecycleError::Revalidation(
                RemoteChannelRevalidationError::IdentityChanged
            ))
        ));
        assert_eq!(records.starts().len(), 3);
        assert_eq!(manager.read_with(cx, hierarchy_identity), before);
        assert!(manager.read_with(cx, |manager, cx| {
            manager
                .tabs
                .iter()
                .all(|(_, host)| host.read(cx).remote_disconnected_generation() == Some(4))
        }));

        provider.fail_revalidation_with(None);
        provider.fail_at(Some(provider.preparation_count() + 2));
        let factory = manager.read_with(cx, |manager, _| manager.session_factory.clone());
        let failed = prepare_remote_restart_for_test(&manager, factory, 5, cx);
        assert!(matches!(
            failed,
            Err(RemoteTabManagerLifecycleError::ChannelUnavailable(_))
        ));
        assert_eq!(records.starts().len(), 3);
        let after_failed_prepare = manager.read_with(cx, hierarchy_identity);
        assert_eq!(after_failed_prepare, before);
        assert!(manager.read_with(cx, |manager, cx| {
            manager
                .tabs
                .iter()
                .all(|(_, host)| host.read(cx).remote_disconnected_generation() == Some(4))
        }));

        provider.fail_at(None);
        let factory = manager.read_with(cx, |manager, _| manager.session_factory.clone());
        let prepared = prepare_remote_restart_for_test(&manager, factory, 5, cx).unwrap();
        cx.update(|window, cx| {
            manager
                .update(cx, |manager, cx| {
                    manager.commit_remote_restart(prepared, window, cx)
                })
                .unwrap();
        });
        cx.run_until_parked();

        let after_commit = manager.read_with(cx, hierarchy_identity);
        assert_eq!(after_commit, before);
        assert_eq!(records.starts().len(), 6);
    }

    #[gpui::test]
    fn cancelled_remote_restart_preparation_should_not_reserve_or_mutate(cx: &mut TestAppContext) {
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(SequencedRemoteChannelProvider::new(destination));
        let (manager, records, cx) = remote_tab_manager_with_provider(cx, Arc::clone(&provider));
        manager
            .update(cx, |manager, cx| manager.disconnect_remote(4, cx))
            .unwrap();
        let before = manager.read_with(cx, hierarchy_identity);
        let preparation_count = provider.preparation_count();
        let revalidation_count = provider.revalidation_count();
        let factory = manager.read_with(cx, |manager, _| manager.session_factory.clone());

        let task = manager.update(cx, |manager, cx| {
            manager.prepare_remote_restart(factory, 5, cx)
        });
        drop(task);
        cx.run_until_parked();

        assert_eq!(manager.read_with(cx, hierarchy_identity), before);
        assert_eq!(records.starts().len(), 1);
        assert_eq!(provider.preparation_count(), preparation_count);
        assert_eq!(provider.revalidation_count(), revalidation_count);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .remote_lifecycle
                .disconnected_generation()),
            Some(4)
        );
    }

    #[gpui::test]
    fn known_remote_master_failure_keeps_pane_and_blocks_new_children(cx: &mut TestAppContext) {
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(SequencedRemoteChannelProvider::new(destination));
        let (manager, records, cx) = remote_tab_manager_with_provider(cx, Arc::clone(&provider));
        provider.set_ready(false);
        records
            .event_sender(1)
            .unwrap()
            .try_send(SessionEvent::Exited(SessionExit::ExitCode(255)))
            .unwrap();
        cx.run_until_parked();

        cx.simulate_keystrokes("cmd-d");
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.create_tab(window, cx));
        });
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, cx| {
            let host = manager.tabs.active_tab().read(cx);
            let remote_state = host.focused_terminal_remote_state(cx);
            (
                manager.tabs.len(),
                host.pane_count(),
                remote_state.0,
                remote_state.1,
            )
        });
        assert_eq!(state, (1, 1, true, true));
        assert!(records.dropped_session_ids().is_empty());
    }

    #[gpui::test]
    fn post_commit_start_failure_is_scoped_to_the_failed_remote_pane(cx: &mut TestAppContext) {
        let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
        let provider = Arc::new(SequencedRemoteChannelProvider::new(destination.clone()));
        let (manager, records, cx) = remote_tab_manager_with_provider(cx, Arc::clone(&provider));
        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();
        manager
            .update(cx, |manager, cx| manager.disconnect_remote(10, cx))
            .unwrap();
        let restart_factory = remote_session_factory_with_terminal_factory(
            Rc::new(
                TestTerminalSessionFactory::new(records.clone())
                    .with_start_failure_at(2, "injected second Pane startup failure"),
            ),
            destination,
            provider,
        );
        let prepared = prepare_remote_restart_for_test(&manager, restart_factory, 11, cx).unwrap();
        cx.update(|window, cx| {
            manager
                .update(cx, |manager, cx| {
                    manager.commit_remote_restart(prepared, window, cx)
                })
                .unwrap();
        });
        cx.run_until_parked();

        let states = manager.read_with(cx, |manager, cx| {
            manager
                .tabs
                .active_tab()
                .read(cx)
                .terminal_restart_states(cx)
        });
        assert_eq!(
            states,
            vec![
                (PaneId::new(1), true, None),
                (PaneId::new(2), false, Some("restart-remote-session")),
            ]
        );
        assert_eq!(records.starts().len(), 4);
        assert_eq!(
            manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
            (1, 2)
        );
    }

    #[gpui::test]
    fn healthy_remote_shell_exit_keeps_existing_hierarchy_close_behavior(cx: &mut TestAppContext) {
        let (manager, records, cx) = remote_tab_manager(cx);
        click("create-tab-button", cx);
        records
            .event_sender(2)
            .unwrap()
            .try_send(SessionEvent::Exited(SessionExit::Success))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            manager.read_with(cx, |manager, _| {
                (manager.tabs.len(), manager.tabs.active_tab_id())
            }),
            (1, TabId::new(1))
        );
    }

    #[gpui::test]
    fn closing_a_multi_pane_tab_should_close_every_owned_session_exactly_once(
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
            .try_send(SessionEvent::Screen(screen))
            .unwrap();
    }

    #[gpui::test]
    fn new_tabs_should_inherit_active_pane_with_pin_change_and_unpin_applied_to_all_hosts(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = remote_tab_manager(cx);
        report_current_directory(&records, 1, 1, "/srv/frontend", true);
        cx.run_until_parked();
        cx.update(|window, cx| manager.update(cx, |manager, cx| manager.create_tab(window, cx)));
        cx.run_until_parked();
        report_current_directory(&records, 2, 1, "/srv/backend", true);
        cx.run_until_parked();
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.create_tab(window, cx);
                manager.activate_tab(TabId::new(1), window, cx);
            })
        });
        cx.run_until_parked();
        for directory in ["/srv/pinned", "/srv/changed"] {
            cx.update(|window, cx| {
                manager.update(cx, |manager, cx| {
                    manager.set_pinned_directory(
                        Some(PinnedDirectory::Remote {
                            directory: crate::domain::RemoteDirectory::new(directory.into())
                                .unwrap(),
                            identity: crate::domain::RemoteDirectoryIdentity::new(directory.into())
                                .unwrap(),
                        }),
                        cx,
                    );
                    manager.create_tab(window, cx);
                })
            });
            cx.run_until_parked();
        }
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.set_pinned_directory(None, cx);
                manager.activate_tab(TabId::new(1), window, cx);
                manager.create_tab(window, cx);
            })
        });
        cx.run_until_parked();
        let directories: Vec<_> = records
            .starts()
            .iter()
            .map(|start| {
                start
                    .remote_launch_plan()
                    .unwrap()
                    .remote_directory()
                    .as_str()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            &directories[1..],
            [
                "/srv/frontend",
                "/srv/backend",
                "/srv/pinned",
                "/srv/changed",
                "/srv/frontend"
            ]
        );
        assert_eq!(records.dropped_session_ids(), Vec::<usize>::new());
    }

    #[gpui::test]
    fn reconnect_should_restore_pane_directories_and_keep_changed_pin_for_new_tabs(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = remote_tab_manager(cx);
        report_current_directory(&records, 1, 1, "/srv/first", true);
        cx.run_until_parked();
        cx.update(|window, cx| manager.update(cx, |manager, cx| manager.create_tab(window, cx)));
        cx.run_until_parked();
        report_current_directory(&records, 2, 1, "/srv/second", true);
        cx.run_until_parked();
        for directory in ["/srv/pinned", "/srv/changed"] {
            manager.update(cx, |manager, cx| {
                manager.set_pinned_directory(
                    Some(PinnedDirectory::Remote {
                        directory: crate::domain::RemoteDirectory::new(directory.into()).unwrap(),
                        identity: crate::domain::RemoteDirectoryIdentity::new(directory.into())
                            .unwrap(),
                    }),
                    cx,
                );
            });
        }
        assert_eq!(records.starts().len(), 2);
        manager
            .update(cx, |manager, cx| manager.disconnect_remote(4, cx))
            .unwrap();
        let factory = manager.read_with(cx, |manager, _| manager.session_factory.clone());
        let prepared = prepare_remote_restart_for_test(&manager, factory, 5, cx).unwrap();
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.commit_remote_restart(prepared, window, cx)
            })
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|window, cx| manager.update(cx, |manager, cx| manager.create_tab(window, cx)));
        cx.run_until_parked();
        let directories: Vec<_> = records
            .starts()
            .iter()
            .skip(2)
            .map(|start| {
                start
                    .remote_launch_plan()
                    .unwrap()
                    .remote_directory()
                    .as_str()
                    .to_owned()
            })
            .collect();
        assert_eq!(directories, ["/srv/first", "/srv/second", "/srv/changed"]);
    }
}
