use super::pane_lifecycle::{PaneConstruction, RemoteHierarchyLifecycle};
use crate::domain::PinnedDirectory;
use crate::domain::remote_workspace::RemoteRestartBatch;
use std::rc::Rc;

use thiserror::Error;

use super::terminal_focus::{TabFocusOwners, TerminalFocusBlocker, TerminalFocusCoordinator};
use super::{
    ActivateTab1, ActivateTab2, ActivateTab3, ActivateTab4, ActivateTab5, ActivateTab6,
    ActivateTab7, ActivateTab8, ActivateTab9, CloseTab, CreateTab, PaneHost, PaneHostEvent,
    PreparedPaneHostRemoteRestart, RemoteChildLaunchUnavailable, RemotePaneHostLifecycleError,
    TERMINAL_KEY_CONTEXT, WORKSPACE_SIDEBAR_DEFAULT_WIDTH,
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
    AnyElement, App, Context, Edges, Entity, EventEmitter, MouseButton, Pixels, Render, Rgba,
    ScrollHandle, SharedString, Task, Window, div, px, rgba,
};
use spaceterm_ui::{
    Alert, AlertIntent, ButtonSize, ButtonVariant, CustomIconName, Icon, IconButton, IconName,
    ModalAction, ModalActionRole, ModalId, Tooltip, WindowDragRegion, WindowDragRegionEvent,
    WindowDragRegionResponse, WindowDragRegionStatus,
};

#[cfg(test)]
const TAB_BAR_HEIGHT: f32 = TOP_CHROME_HEIGHT;
const TAB_BAR_DIVIDER_SIZE: f32 = 1.0;
const TAB_ITEM_WIDTH: f32 = 132.0;
const TAB_ITEM_MINIMUM_WIDTH: f32 = 84.0;
const TAB_ITEM_MAXIMUM_WIDTH: f32 = 160.0;
const TAB_ITEM_RIGHT_PADDING: f32 = 6.0;
const TAB_CLOSE_ICON_SIZE: f32 = 12.0;

#[derive(Clone, Debug, Eq, PartialEq)]
struct TabChromePresentation {
    window_active: bool,
    background: Color,
    active_tab_background: Color,
    inactive_tab_background: Color,
    active_tab_foreground: Color,
    inactive_tab_foreground: Color,
    icon_foreground: Color,
    hover_background: Color,
    active_tab_underline: Color,
    divider: Color,
}

impl TabChromePresentation {
    fn resolve(window_active: bool, colors: &ChromeColors) -> Self {
        if window_active {
            Self {
                window_active,
                background: colors.title_bar_background,
                active_tab_background: colors.tab_active_background,
                inactive_tab_background: colors.tab_inactive_background,
                active_tab_foreground: colors.text_accent,
                inactive_tab_foreground: colors.text_muted,
                icon_foreground: colors.icon,
                hover_background: colors.ghost_element_hover,
                active_tab_underline: colors.navigation_selection,
                divider: colors.border,
            }
        } else {
            Self {
                window_active,
                background: colors.title_bar_inactive_background,
                active_tab_background: colors.title_bar_inactive_background,
                inactive_tab_background: colors.title_bar_inactive_background,
                active_tab_foreground: colors.text_muted,
                inactive_tab_foreground: colors.text_muted,
                icon_foreground: colors.text_muted,
                hover_background: colors.title_bar_inactive_background,
                active_tab_underline: colors.border,
                divider: colors.border,
            }
        }
    }

    fn tab_background(&self, active: bool) -> Color {
        if active {
            self.active_tab_background
        } else {
            self.inactive_tab_background
        }
    }

    fn tab_foreground(&self, active: bool) -> Color {
        if active {
            self.active_tab_foreground
        } else {
            self.inactive_tab_foreground
        }
    }

    fn resolved_icon_foreground(&self, button_state_foreground: Rgba) -> Rgba {
        if self.window_active {
            button_state_foreground
        } else {
            gpui_color(self.icon_foreground)
        }
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
    operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
    window_drag_status: WindowDragRegionStatus,
    tab_bar_scroll_handle: ScrollHandle,
    close_workspace_requested: bool,
    remote_lifecycle: RemoteHierarchyLifecycle,
    #[cfg(test)]
    rendered_window_active: bool,
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
            operating_system_window_drag_platform,
            window_drag_status: WindowDragRegionStatus::new(),
            tab_bar_scroll_handle: ScrollHandle::new(),
            close_workspace_requested: false,
            remote_lifecycle: RemoteHierarchyLifecycle::default(),
            #[cfg(test)]
            rendered_window_active: window.is_window_active(),
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
            self.tabs.active_tab().read(cx).focus(window, cx);
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

    fn render_tab_item(
        &self,
        tab_id: TabId,
        title: SharedString,
        active: bool,
        presentation: &TabChromePresentation,
        manager: gpui::WeakEntity<Self>,
        appearance: &super::appearance::ChromeAppearance,
    ) -> AnyElement {
        let press_manager = manager.clone();
        let release_manager = manager.clone();
        let click_manager = manager.clone();
        let close_manager = manager;
        let background = presentation.tab_background(active);
        let foreground = presentation.tab_foreground(active);
        let icon_presentation = presentation.clone();
        let hover_background = presentation.hover_background;
        let active_tab_underline = presentation.active_tab_underline;
        let divider = presentation.divider;
        let close_icon_size = appearance.spacing(TAB_CLOSE_ICON_SIZE);
        let tab_group = format!("tab-item-{}", tab_id.get());
        let item = div()
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
            .pl(appearance.spacing(12.0))
            .pr(appearance.spacing(TAB_ITEM_RIGHT_PADDING))
            .flex()
            .items_center()
            .cursor_pointer()
            .block_mouse_except_scroll()
            .bg(gpui_color(background))
            .font(appearance.emphasis.clone())
            .text_size(appearance.text_size(12.0))
            .text_color(gpui_color(foreground))
            .hover(move |item| item.bg(gpui_color(hover_background)))
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
            .child(
                div()
                    .id(("tab-title", tab_id.get()))
                    .debug_selector(move || format!("tab-title-{}", tab_id.get()))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(title),
            )
            .child(
                div()
                    .ml(appearance.spacing(4.0))
                    .flex_shrink_0()
                    .when(!active, |button| {
                        button
                            .opacity(0.0)
                            .group_hover(tab_group, |button| button.opacity(1.0))
                    })
                    .child(
                        IconButton::new(
                            ("tab-close-button", tab_id.get()),
                            "Close Tab",
                            move |foreground| {
                                Icon::new(
                                    IconName::X,
                                    close_icon_size,
                                    icon_presentation.resolved_icon_foreground(foreground),
                                )
                                .into_any_element()
                            },
                        )
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Compact)
                        .preserve_ancestor_hover()
                        .debug_selector(format!("tab-close-button-{}", tab_id.get()))
                        .tooltip(
                            Tooltip::new(("tab-close-tooltip", tab_id.get()), "Close Tab")
                                .debug_selector(format!("tab-close-tooltip-{}", tab_id.get())),
                        )
                        .on_activate(move |_, _, cx| {
                            let _ = close_manager.update(cx, |manager, cx| {
                                manager.request_close_tab(tab_id, cx);
                            });
                        }),
                    ),
            )
            .child(
                div()
                    .id(("tab-item-divider", tab_id.get()))
                    .debug_selector(move || format!("tab-item-{}-divider", tab_id.get()))
                    .absolute()
                    .top_0()
                    .right_0()
                    .h_full()
                    .w(px(TAB_BAR_DIVIDER_SIZE))
                    .bg(gpui_color(divider)),
            )
            .child(
                div()
                    .id(("tab-item-bottom-divider", tab_id.get()))
                    .debug_selector(move || format!("tab-item-{}-bottom-divider", tab_id.get()))
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .w_full()
                    .h(px(TAB_BAR_DIVIDER_SIZE))
                    .bg(gpui_color(divider)),
            )
            .when(active, |item| {
                item.child(
                    div()
                        .id(("tab-item-underline", tab_id.get()))
                        .debug_selector(move || format!("tab-item-{}-underline", tab_id.get()))
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .h(px(TAB_BAR_DIVIDER_SIZE))
                        .bg(gpui_color(active_tab_underline)),
                )
            });

        item.into_any_element()
    }

    fn render_tab_bar(
        &self,
        presentation: &TabChromePresentation,
        manager: gpui::WeakEntity<Self>,
        cx: &App,
    ) -> AnyElement {
        let appearance = super::appearance::chrome(cx);
        let active_tab_id = self.tabs.active_tab_id();
        let background = presentation.background;
        let create_icon_size = appearance.spacing(14.0);
        let mut items = div()
            .id("tab-items")
            .debug_selector(|| "tab-items".to_owned())
            .h_full()
            .min_w_0()
            .flex()
            .flex_row()
            .overflow_x_scroll()
            .track_scroll(&self.tab_bar_scroll_handle);
        for (tab_id, pane_host) in self.tabs.iter() {
            items = items.child(self.render_tab_item(
                tab_id,
                pane_host.read(cx).tab_title(),
                tab_id == active_tab_id,
                presentation,
                manager.clone(),
                appearance,
            ));
        }

        let drag_manager = manager.clone();
        let create_manager = manager.clone();
        let content = div()
            .relative()
            .size_full()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .bg(gpui_color(background))
            .child(
                div()
                    .id("tab-bar-divider")
                    .debug_selector(|| "tab-bar-divider".to_owned())
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .w_full()
                    .h(px(TAB_BAR_DIVIDER_SIZE))
                    .bg(gpui_color(presentation.divider)),
            )
            .child(items)
            .child(
                div()
                    .debug_selector(|| "create-tab-area".to_owned())
                    .size(appearance.top_height())
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        IconButton::new("create-tab-button", "Create Tab", move |foreground| {
                            Icon::custom(CustomIconName::Plus, create_icon_size, foreground)
                                .into_any_element()
                        })
                        .variant(ButtonVariant::Ghost)
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
            .h(appearance.top_height())
            .min_w_0()
            .flex_1()
            .flex_shrink_0()
            .bg(gpui_color(background))
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
        let presentation =
            TabChromePresentation::resolve(window.is_window_active(), &appearance.colors);
        let tab_bar = self.render_tab_bar(&presentation, manager.clone(), cx);

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
            .font(appearance.regular.clone())
            .bg(gpui_color(appearance.colors.background))
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
                    .h(appearance.top_height())
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
                            .flex_shrink_0()
                            .bg(gpui_color(presentation.background)),
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
                    .overflow_hidden()
                    .when(self.sidebar_visible, |body| body.ml(self.sidebar_width))
                    .child(active_tab),
            )
    }
}

impl EventEmitter<TabManagerEvent> for TabManager {}
impl EventEmitter<RemoteChildLaunchUnavailable> for TabManager {}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
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
        DivInspectorState, Hsla, Modifiers, MouseDownEvent, MouseExitEvent, MouseUpEvent,
        ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, VisualTestContext, point,
    };

    use super::*;

    #[test]
    fn create_tab_tooltip_should_use_the_host_neutral_profile_shortcut() {
        assert_eq!(
            create_tab_shortcut(&crate::desktop_profile::testing_presentation()),
            "Primary+T"
        );
    }

    #[test]
    fn active_window_tab_chrome_should_preserve_the_existing_presentation() {
        let colors = ChromeColors::default();
        let presentation = TabChromePresentation::resolve(true, &colors);

        assert_eq!(
            presentation,
            TabChromePresentation {
                window_active: true,
                background: colors.title_bar_background,
                active_tab_background: colors.tab_active_background,
                inactive_tab_background: colors.tab_inactive_background,
                active_tab_foreground: colors.text_accent,
                inactive_tab_foreground: colors.text_muted,
                icon_foreground: colors.icon,
                hover_background: colors.ghost_element_hover,
                active_tab_underline: colors.border_selected,
                divider: colors.border,
            }
        );
    }

    #[test]
    fn inactive_window_tab_chrome_should_be_one_muted_title_bar_band() {
        let colors = ChromeColors::default();
        let presentation = TabChromePresentation::resolve(false, &colors);

        assert_eq!(
            presentation,
            TabChromePresentation {
                window_active: false,
                background: colors.title_bar_inactive_background,
                active_tab_background: colors.title_bar_inactive_background,
                inactive_tab_background: colors.title_bar_inactive_background,
                active_tab_foreground: colors.text_muted,
                inactive_tab_foreground: colors.text_muted,
                icon_foreground: colors.text_muted,
                hover_background: colors.title_bar_inactive_background,
                active_tab_underline: colors.border,
                divider: colors.border,
            }
        );
    }

    #[test]
    fn tab_icon_foreground_should_follow_button_state_only_while_window_active() {
        let colors = ChromeColors::default();
        let button_state_foreground = rgba(0x1234_56ff);

        assert_eq!(
            TabChromePresentation::resolve(true, &colors)
                .resolved_icon_foreground(button_state_foreground),
            button_state_foreground
        );
        assert_eq!(
            TabChromePresentation::resolve(false, &colors)
                .resolved_icon_foreground(button_state_foreground),
            gpui_color(colors.text_muted)
        );
    }

    fn tab_icon_test_theme(
        normal: Rgba,
        hovered: Rgba,
        pressed: Rgba,
    ) -> spaceterm_ui::ButtonTheme {
        use spaceterm_ui::{
            ButtonMetrics, ButtonPaint, ButtonSizes, ButtonTheme, ButtonVariantStyle,
            ButtonVariants,
        };

        let paint = |icon_foreground| {
            ButtonPaint::new(rgba(0), rgba(0), rgba(0)).icon_foreground(icon_foreground)
        };
        let neutral =
            ButtonVariantStyle::new(paint(normal), paint(normal), paint(normal), paint(normal));
        let ghost =
            ButtonVariantStyle::new(paint(normal), paint(hovered), paint(pressed), paint(normal));
        let metrics = ButtonMetrics::new(px(28.0));
        ButtonTheme::new(
            ButtonVariants::new(neutral, neutral, neutral, ghost, neutral, neutral, neutral),
            ButtonSizes::new(metrics, metrics, metrics, metrics),
            normal,
        )
    }

    struct InspectedIconStyle {
        family: Option<SharedString>,
        color: Option<Hsla>,
        size: gpui::Size<Pixels>,
    }

    type InspectedGlyphStyles = Rc<RefCell<Vec<InspectedIconStyle>>>;

    fn inspect_icon_foreground(
        selector: &'static str,
        pressed: bool,
        observed: &InspectedGlyphStyles,
        cx: &mut VisualTestContext,
    ) -> Hsla {
        let mut position = cx
            .debug_bounds(selector)
            .expect("the tab IconButton was not rendered")
            .center();
        cx.simulate_mouse_move(position, None, Modifiers::none());
        if pressed {
            cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
        }
        cx.run_until_parked();

        cx.update(|window, cx| window.toggle_inspector(cx));
        cx.run_until_parked();
        position = cx
            .debug_bounds(selector)
            .expect("the tab IconButton was not rendered with the inspector open")
            .center();
        observed.borrow_mut().clear();
        cx.simulate_mouse_move(
            position,
            pressed.then_some(MouseButton::Left),
            Modifiers::none(),
        );
        cx.run_until_parked();

        let mut foreground = None;
        for _ in 0..16 {
            foreground = observed.borrow().iter().rev().find_map(|style| {
                let is_icon = if selector == "create-tab-button" {
                    style.family.is_none() && style.size == gpui::size(px(14.0), px(14.0))
                } else {
                    style
                        .family
                        .as_ref()
                        .is_some_and(|family| family.as_ref() == "lucide")
                };
                is_icon.then_some(style.color).flatten()
            });
            if foreground.is_some() {
                break;
            }
            cx.simulate_event(ScrollWheelEvent {
                position,
                delta: ScrollDelta::Pixels(point(px(0.0), px(36.0))),
                modifiers: Modifiers::none(),
                touch_phase: TouchPhase::Moved,
            });
            cx.run_until_parked();
        }

        cx.update(|window, cx| window.toggle_inspector(cx));
        cx.run_until_parked();
        if pressed {
            let outside = cx
                .debug_bounds("tab-manager-content")
                .expect("Tab content was not rendered")
                .center();
            cx.simulate_mouse_move(outside, Some(MouseButton::Left), Modifiers::none());
            cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::none());
            cx.run_until_parked();
        }

        foreground.expect("the runtime icon style was not inspectable")
    }

    #[gpui::test]
    fn tab_icon_glyphs_should_use_hovered_and_pressed_button_foregrounds(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = tab_manager(cx);
        let normal = rgba(0x11_22_33_ff);
        let hovered = rgba(0x22_cc_44_ff);
        let pressed = rgba(0x44_66_ee_ff);
        let observed = Rc::new(RefCell::new(Vec::new()));
        let observed_styles = Rc::clone(&observed);
        cx.update(|window, cx| {
            cx.set_global(tab_icon_test_theme(normal, hovered, pressed));
            cx.register_inspector_element(move |_, state: &DivInspectorState, _, _| {
                observed_styles.borrow_mut().push(InspectedIconStyle {
                    family: state
                        .base_style
                        .text
                        .as_ref()
                        .and_then(|text| text.font_family.clone()),
                    color: state.base_style.text.as_ref().and_then(|text| text.color),
                    size: state.bounds.size,
                });
                gpui::Empty
            });
            cx.set_inspector_renderer(Box::new(|inspector, window, cx| {
                div()
                    .children(inspector.render_inspector_states(window, cx))
                    .into_any_element()
            }));
            window.refresh();
        });
        cx.run_until_parked();

        for selector in ["create-tab-button", "tab-close-button-1"] {
            assert_eq!(
                inspect_icon_foreground(selector, false, &observed, cx),
                Hsla::from(hovered),
                "{selector} must paint its glyph with the hovered IconButton foreground"
            );
            assert_eq!(
                inspect_icon_foreground(selector, true, &observed, cx),
                Hsla::from(pressed),
                "{selector} must paint its glyph with the pressed IconButton foreground"
            );
        }

        cx.update(|window, cx| {
            cx.set_global(tab_icon_test_theme(normal, normal, normal));
            window.refresh();
        });
        cx.deactivate_window();
        cx.run_until_parked();
        assert_eq!(
            inspect_icon_foreground("create-tab-button", false, &observed, cx),
            Hsla::from(normal),
            "the new-tab SVG must retain the shared icon tint in an inactive window"
        );
    }
    use crate::domain::PaneId;
    use crate::domain::ZoomState;
    use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
    use crate::ssh::command::{SshCommandContext, ValidatedRemoteShellCommand};
    use crate::terminal::testing::{
        RecordedSessionCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
    };
    use crate::terminal::{
        RemoteChannelUnavailable, RemoteTerminalChannelProvider, ScreenSnapshot, SessionEvent,
        SessionExit, TerminalSessionFactory,
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
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(PathBuf::from(
                "/tmp/spaceterm-tab-manager-test",
            )),
        );
        let (manager, cx) =
            cx.add_window_view(|window, cx| TabManager::new(session_factory, window, cx));
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

    #[gpui::test]
    fn tab_bar_should_keep_dim_dividers_beneath_every_item_and_accent_the_active_tab(
        cx: &mut TestAppContext,
    ) {
        let (_manager, _records, cx) = tab_manager(cx);
        click("create-tab-button", cx);

        let bar = cx
            .debug_bounds("tab-bar")
            .expect("the Tab bar was not rendered");
        let divider = cx
            .debug_bounds("tab-bar-divider")
            .expect("the Tab bar divider was not rendered");
        let underline = cx
            .debug_bounds("tab-item-2-underline")
            .expect("the Active Tab underline was not rendered");
        let inactive_item = cx
            .debug_bounds("tab-item-1-inactive")
            .expect("the inactive Tab item was not rendered");
        let active_item = cx
            .debug_bounds("tab-item-2-active")
            .expect("the Active Tab item was not rendered");
        let item_divider = cx
            .debug_bounds("tab-item-1-divider")
            .expect("the Tab item divider was not rendered");
        let inactive_bottom_divider = cx
            .debug_bounds("tab-item-1-bottom-divider")
            .expect("the inactive Tab bottom divider was not rendered");
        let active_bottom_divider = cx
            .debug_bounds("tab-item-2-bottom-divider")
            .expect("the Active Tab bottom divider was not rendered");

        assert_eq!(
            (
                bar.size.height,
                divider.size.height,
                underline.size.height,
                divider.origin.y + divider.size.height,
                item_divider.size.width,
                item_divider.size.height,
                item_divider.origin.x + item_divider.size.width,
                inactive_bottom_divider.origin.y,
                inactive_bottom_divider.size,
                active_bottom_divider.origin.y,
                active_bottom_divider.size,
            ),
            (
                px(TAB_BAR_HEIGHT),
                px(TAB_BAR_DIVIDER_SIZE),
                px(TAB_BAR_DIVIDER_SIZE),
                underline.origin.y + underline.size.height,
                px(TAB_BAR_DIVIDER_SIZE),
                inactive_item.size.height,
                inactive_item.origin.x + inactive_item.size.width,
                divider.origin.y,
                gpui::size(inactive_item.size.width, px(TAB_BAR_DIVIDER_SIZE)),
                divider.origin.y,
                gpui::size(active_item.size.width, px(TAB_BAR_DIVIDER_SIZE)),
            )
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

        assert_eq!(
            (spacer.origin, spacer.size, bar.origin.x),
            (
                root.origin,
                gpui::size(px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH), px(TOP_CHROME_HEIGHT)),
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

    #[gpui::test]
    fn single_pane_tab_title_should_follow_the_terminal_title(cx: &mut TestAppContext) {
        let (manager, records, cx) = tab_manager(cx);
        let sender = records
            .event_sender(1)
            .expect("the initial Tab session must have started");

        sender
            .try_send(SessionEvent::Screen(ScreenSnapshot::from_test_parts(
                Arc::from([]),
                Default::default(),
                "Claude Code",
            )))
            .unwrap();
        cx.run_until_parked();

        let title = manager.read_with(cx, |manager, cx| {
            manager.tabs.active_tab().read(cx).tab_title()
        });
        assert_eq!(title.as_ref(), "Claude Code");
    }

    #[gpui::test]
    fn split_tab_title_should_show_the_count_and_restore_the_terminal_title_after_close(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = tab_manager(cx);
        let sender = records
            .event_sender(1)
            .expect("the initial Tab session must have started");
        sender
            .try_send(SessionEvent::Screen(ScreenSnapshot::from_test_parts(
                Arc::from([]),
                Default::default(),
                "Claude Code",
            )))
            .unwrap();
        cx.run_until_parked();

        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();
        let split_title = manager.read_with(cx, |manager, cx| {
            manager.tabs.active_tab().read(cx).tab_title()
        });
        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        let restored_title = manager.read_with(cx, |manager, cx| {
            manager.tabs.active_tab().read(cx).tab_title()
        });

        assert_eq!(
            (split_title.as_ref(), restored_title.as_ref()),
            ("Terminal · 2 Panes", "Claude Code")
        );
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

        assert_eq!(
            (
                item.origin.x + item.size.width - (close_button.origin.x + close_button.size.width),
                close_button.size,
            ),
            (px(TAB_ITEM_RIGHT_PADDING), gpui::size(px(20.0), px(20.0)),)
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
            assert_eq!(area.size, gpui::size(tab.size.height, tab.size.height));
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
                assert_eq!(area.size, gpui::size(bar.size.height, bar.size.height));
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
        assert_eq!(area.size, gpui::size(bar.size.height, bar.size.height));
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
            window.blur();
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
        let mut screen = crate::terminal::ScreenSnapshot::from_test_parts_at(
            Arc::from([]),
            crate::terminal::ScrollbarSnapshot::default(),
            "terminal",
            generation,
        );
        let metadata = Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata);
        metadata.directory.path = Arc::from(directory);
        if remote {
            metadata.context = crate::terminal::metadata::TerminalMetadataContext::Remote(
                crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                    crate::domain::SshDestination::new("tester@remote".into()).unwrap(),
                    crate::domain::RemoteDirectory::new(directory.into()).unwrap(),
                ),
            );
        }
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
