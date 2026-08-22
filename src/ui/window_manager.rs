use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, DispatchPhase, Entity, EventEmitter, MouseButton, MouseDownEvent,
    MouseExitEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollHandle,
    SharedString, Window, canvas, div, px, rgba,
};
use gpui_symbols::{Icon, SymbolWeight};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::terminal_focus::TerminalFocusBlocker;
use super::workspace_manager::{WorkspaceDirectorySource, WorkspaceDirectoryUnavailable};
use super::{
    ActivateWindow1, ActivateWindow2, ActivateWindow3, ActivateWindow4, ActivateWindow5,
    ActivateWindow6, ActivateWindow7, ActivateWindow8, ActivateWindow9, CloseTarget, CloseWindow,
    CreateWindow, PANE_ACTION_MENU_HEIGHT, PANE_ACTION_MENU_WIDTH, PaneActionMenuCommand, PaneHost,
    PaneHostEvent, TERMINAL_KEY_CONTEXT, TOP_CHROME_HEIGHT, WORKSPACE_SIDEBAR_DEFAULT_WIDTH,
    handle_top_chrome_mouse_down, render_pane_action_menu,
};
use crate::domain::{
    CloseWindowOutcome, PaneId, SplitAxis, WindowCollection, WindowError, WindowId, WorkspaceId,
    ZoomState,
};
use crate::terminal::{
    NativeServiceOrigin, NativeServiceStatus, SelectionCopy, WorkspaceTerminalSessionFactory,
};
use crate::theme::{ACTIVE_THEME, Color};

const WINDOW_BAR_HEIGHT: f32 = TOP_CHROME_HEIGHT;
const WINDOW_BAR_DIVIDER_SIZE: f32 = 1.0;
const WINDOW_ITEM_WIDTH: f32 = 132.0;
const WINDOW_ITEM_MINIMUM_WIDTH: f32 = 84.0;
const WINDOW_ITEM_MAXIMUM_WIDTH: f32 = 160.0;
const WINDOW_ITEM_RIGHT_PADDING: f32 = 6.0;
const WINDOW_CLOSE_CONTROL_SIZE: f32 = 20.0;
const WINDOW_CLOSE_ICON_SIZE: f32 = 12.0;
const WINDOW_CONTROL_SIZE: f32 = 28.0;
const WINDOW_CONTROL_INSET: f32 = 4.0;
const WINDOW_MENU_BAR_OVERLAP: f32 = 8.0;
const WINDOW_MENU_TOP: f32 = WINDOW_BAR_HEIGHT - WINDOW_MENU_BAR_OVERLAP;

#[derive(Clone, Copy, Debug, PartialEq)]
struct WindowMenuState {
    window_id: WindowId,
    left: Option<Pixels>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WindowManagerEvent {
    FinalWindowCloseRequested {
        final_window_id: WindowId,
    },
    PresentationChanged,
    ChildCreationBlocked,
    PaneReportedDirectoryChanged {
        window_id: WindowId,
        pane_id: PaneId,
    },
    PaneClosed {
        window_id: WindowId,
        closed_pane_id: PaneId,
    },
    WindowClosed {
        window_id: WindowId,
    },
}

pub(crate) struct WindowManager {
    windows: WindowCollection<Entity<PaneHost>>,
    session_factory: WorkspaceTerminalSessionFactory,
    directory_gate: Rc<dyn WorkspaceDirectorySource>,
    active: bool,
    sidebar_visible: bool,
    sidebar_width: Pixels,
    window_menu: Option<WindowMenuState>,
    parent_focus_blocker: Option<TerminalFocusBlocker>,
    window_selector_pressed: Option<WindowId>,
    top_chrome_interaction: bool,
    top_chrome_move_requested: bool,
    window_bar_scroll_handle: ScrollHandle,
    close_workspace_requested: bool,
}

impl WindowManager {
    fn report_window_error(operation: &str, error: WindowError) {
        eprintln!("failed to {operation} Window: {error}");
    }

    pub(crate) fn new(
        session_factory: WorkspaceTerminalSessionFactory,
        directory_gate: Rc<dyn WorkspaceDirectorySource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let windows = WindowCollection::new(|window_id| {
            Self::create_pane_host(
                window_id,
                session_factory.clone(),
                directory_gate.clone(),
                window,
                cx,
            )
        });
        cx.observe_window_activation(window, |manager, window, cx| {
            if !window.is_window_active() {
                manager.set_top_chrome_interaction(false, cx);
            }
        })
        .detach();

        Self {
            windows,
            session_factory,
            directory_gate,
            active: true,
            sidebar_visible: true,
            sidebar_width: px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            window_menu: None,
            parent_focus_blocker: None,
            window_selector_pressed: None,
            top_chrome_interaction: false,
            top_chrome_move_requested: false,
            window_bar_scroll_handle: ScrollHandle::new(),
            close_workspace_requested: false,
        }
    }

    fn create_pane_host(
        window_id: WindowId,
        session_factory: WorkspaceTerminalSessionFactory,
        directory_gate: Rc<dyn WorkspaceDirectorySource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<PaneHost> {
        let pane_host = cx
            .new(|cx| PaneHost::new(window_id, session_factory, Some(directory_gate), window, cx));
        debug_assert_eq!(pane_host.read(cx).window_id(), window_id);
        cx.subscribe_in(
            &pane_host,
            window,
            |manager, _, event: &PaneHostEvent, window, cx| match event {
                PaneHostEvent::CloseWindowRequested { window_id } => {
                    manager.close_window(*window_id, window, cx);
                }
                PaneHostEvent::ChildCreationBlocked => {
                    cx.emit(WindowManagerEvent::ChildCreationBlocked);
                }
                PaneHostEvent::PresentationChanged { .. } => {
                    cx.emit(WindowManagerEvent::PresentationChanged);
                    cx.notify();
                }
                PaneHostEvent::PaneReportedDirectoryChanged { window_id, pane_id } => {
                    cx.emit(WindowManagerEvent::PaneReportedDirectoryChanged {
                        window_id: *window_id,
                        pane_id: *pane_id,
                    });
                }
                PaneHostEvent::PaneClosed {
                    window_id,
                    closed_pane_id,
                } => {
                    cx.emit(WindowManagerEvent::PaneClosed {
                        window_id: *window_id,
                        closed_pane_id: *closed_pane_id,
                    });
                }
            },
        )
        .detach();
        pane_host
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        if self.active {
            self.windows.active_window().read(cx).focus(window, cx);
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
        self.windows.active_window().update(cx, |pane_host, cx| {
            pane_host.set_focus_branch(true, blocker, cx);
            pane_host.native_service_status(workspace_id, window, cx)
        })
    }

    pub(crate) fn native_service_selection(
        &self,
        origin: NativeServiceOrigin,
        window: &Window,
        cx: &mut App,
    ) -> Option<SelectionCopy> {
        if !self.active || self.windows.active_window_id() != origin.window_id() {
            return None;
        }
        self.windows
            .window(origin.window_id())?
            .update(cx, |pane_host, cx| {
                pane_host.native_service_selection(origin, window, cx)
            })
    }

    pub(crate) fn insert_native_service_text(
        &self,
        origin: NativeServiceOrigin,
        text: String,
        window: &Window,
        cx: &mut App,
    ) -> bool {
        if !self.active || self.windows.active_window_id() != origin.window_id() {
            return false;
        }
        let Some(pane_host) = self.windows.window(origin.window_id()) else {
            return false;
        };
        pane_host.update(cx, |pane_host, cx| {
            pane_host.insert_native_service_text(origin, text, window, cx)
        })
    }

    pub(crate) fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_without_focus(cx);
        self.focus(window, cx);
    }

    pub(crate) fn activate_without_focus(&mut self, cx: &mut Context<Self>) {
        self.active = true;
        self.windows
            .active_window()
            .update(cx, |pane_host, cx| pane_host.activate_without_focus(cx));
        self.sync_terminal_focus_blocker(cx);
    }

    pub(crate) fn deactivate(&mut self, cx: &mut Context<Self>) {
        self.active = false;
        self.windows
            .active_window()
            .update(cx, |pane_host, cx| pane_host.deactivate(cx));
        self.window_menu = None;
        self.window_selector_pressed = None;
        self.top_chrome_interaction = false;
        self.top_chrome_move_requested = false;
        self.sync_terminal_focus_blocker(cx);
    }

    pub(crate) fn close_all(&self, cx: &mut App) {
        for (_, pane_host) in self.windows.iter() {
            pane_host.update(cx, |pane_host, cx| pane_host.close_all(cx));
        }
    }

    pub(crate) fn set_sidebar_layout(
        &mut self,
        visible: bool,
        width: Pixels,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_visible != visible || self.sidebar_width != width {
            self.sidebar_visible = visible;
            self.sidebar_width = width;
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
        self.parent_focus_blocker
            .or(self
                .top_chrome_interaction
                .then_some(TerminalFocusBlocker::TopChrome))
            .or(self
                .window_selector_pressed
                .map(|_| TerminalFocusBlocker::WindowSelector))
            .or(self.window_menu.map(|menu| {
                if menu.left.is_some() {
                    TerminalFocusBlocker::ContextMenu
                } else {
                    TerminalFocusBlocker::WindowMenu
                }
            }))
    }

    fn sync_terminal_focus_blocker(&self, cx: &mut Context<Self>) {
        let blocker = self.terminal_focus_blocker();
        let active_window_id = self.windows.active_window_id();
        for (window_id, pane_host) in self.windows.iter() {
            let active = self.active && window_id == active_window_id;
            pane_host.update(cx, |pane_host, cx| {
                pane_host.set_focus_branch(active, blocker, cx);
            });
        }
    }

    fn set_top_chrome_interaction(&mut self, blocked: bool, cx: &mut Context<Self>) {
        if self.top_chrome_interaction == blocked {
            return;
        }
        self.top_chrome_interaction = blocked;
        self.top_chrome_move_requested = false;
        self.sync_terminal_focus_blocker(cx);
        cx.notify();
    }

    fn continue_top_chrome_interaction(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.top_chrome_interaction || self.top_chrome_move_requested {
            return;
        }
        self.top_chrome_move_requested = true;
        window.start_window_move();
        cx.notify();
    }

    fn finish_top_chrome_interaction(&mut self, cx: &mut Context<Self>) {
        self.set_top_chrome_interaction(false, cx);
    }

    fn begin_window_selector(&mut self, window_id: WindowId, cx: &mut Context<Self>) {
        self.window_selector_pressed = Some(window_id);
        self.sync_terminal_focus_blocker(cx);
        cx.notify();
    }

    fn cancel_window_selector(&mut self, window_id: WindowId, cx: &mut Context<Self>) {
        if self.window_selector_pressed != Some(window_id) {
            return;
        }
        self.window_selector_pressed = None;
        self.sync_terminal_focus_blocker(cx);
        cx.notify();
    }

    fn commit_window_selector(
        &mut self,
        window_id: WindowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.window_selector_pressed != Some(window_id) {
            return;
        }
        let _ = self.activate_window(window_id, window, cx);
        self.window_selector_pressed = None;
        self.sync_terminal_focus_blocker(cx);
        cx.notify();
    }

    /// Aggregate Workspace-wide (Window, Pane) counts across every owned
    /// Window, in Window order.
    pub(crate) fn aggregate_counts(&self, cx: &App) -> (usize, usize) {
        (
            self.windows.len(),
            self.windows
                .iter()
                .map(|(_, pane_host)| pane_host.read(cx).pane_count())
                .sum(),
        )
    }

    pub(crate) fn ordered_window_ids(&self) -> Vec<WindowId> {
        self.windows
            .iter()
            .map(|(window_id, _)| window_id)
            .collect()
    }

    /// The (Window, Pane) pair that owns Directory Authority for a freshly
    /// materialized Workspace: the first Window's initial Pane.
    pub(crate) fn first_authority_pane(&self, cx: &App) -> Option<(WindowId, PaneId)> {
        let window_id = self.ordered_window_ids().first().copied()?;
        let pane_id = self.first_pane_in_layout_order(window_id, cx)?;
        Some((window_id, pane_id))
    }

    pub(crate) fn contains_window(&self, window_id: WindowId) -> bool {
        self.windows.window(window_id).is_some()
    }

    pub(crate) fn first_pane_in_layout_order(
        &self,
        window_id: WindowId,
        cx: &App,
    ) -> Option<PaneId> {
        self.windows
            .window(window_id)?
            .read(cx)
            .first_pane_in_layout_order()
    }

    pub(crate) fn pane_reported_directory(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
        cx: &App,
    ) -> Option<PathBuf> {
        self.windows
            .window(window_id)?
            .read(cx)
            .pane_reported_directory(pane_id)
            .map(Path::to_path_buf)
    }

    #[cfg(test)]
    pub(crate) fn focused_terminal_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.windows
            .active_window()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }

    #[cfg(test)]
    pub(crate) fn focused_terminal_has_input_focus(&self, window: &Window, cx: &App) -> bool {
        self.windows
            .active_window()
            .read(cx)
            .focused_terminal_has_input_focus(window, cx)
    }

    fn scroll_active_window_into_view(&self) {
        let active_window_id = self.windows.active_window_id();
        if let Some(index) = self
            .windows
            .iter()
            .position(|(window_id, _)| window_id == active_window_id)
        {
            self.window_bar_scroll_handle.scroll_to_item(index);
        }
    }

    pub(crate) fn create_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(WorkspaceDirectoryUnavailable) = self.directory_gate.resolve() {
            cx.emit(WindowManagerEvent::ChildCreationBlocked);
            cx.notify();
            return;
        }
        self.window_menu = None;
        self.window_selector_pressed = None;
        self.sync_terminal_focus_blocker(cx);
        let previous_window = self.windows.active_window().clone();
        let session_factory = self.session_factory.clone();
        let directory_gate = self.directory_gate.clone();
        let result = self.windows.create_window(|window_id| {
            Self::create_pane_host(window_id, session_factory, directory_gate, window, cx)
        });
        let window_id = match result {
            Ok(window_id) => window_id,
            Err(error) => {
                Self::report_window_error("create", error);
                return;
            }
        };
        let Some(pane_host) = self.windows.window(window_id).cloned() else {
            unreachable!("a newly created Window must remain owned by its collection")
        };

        previous_window.update(cx, |pane_host, cx| pane_host.deactivate(cx));
        if self.active {
            pane_host.update(cx, |pane_host, cx| pane_host.activate(window, cx));
        } else {
            pane_host.update(cx, |pane_host, cx| pane_host.deactivate(cx));
        }
        self.sync_terminal_focus_blocker(cx);
        self.scroll_active_window_into_view();
        cx.emit(WindowManagerEvent::PresentationChanged);
        cx.notify();
    }

    fn activate_window(
        &mut self,
        window_id: WindowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.window_menu = None;
        self.activate_window_preserving_menu(window_id, window, cx)
    }

    fn activate_window_preserving_menu(
        &mut self,
        window_id: WindowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(next_window) = self.windows.window(window_id).cloned() else {
            eprintln!("cannot activate unknown Window {window_id}");
            return false;
        };
        let previous_window_id = self.windows.active_window_id();
        let previous_window = self.windows.active_window().clone();
        if let Err(error) = self.windows.activate_window(window_id) {
            Self::report_window_error("activate", error);
            return false;
        }

        if previous_window_id != window_id {
            previous_window.update(cx, |pane_host, cx| pane_host.deactivate(cx));
        }
        let blocker = self.terminal_focus_blocker();
        next_window.update(cx, |pane_host, cx| {
            pane_host.set_focus_branch(self.active, blocker, cx);
        });
        if self.active {
            next_window.update(cx, |pane_host, cx| pane_host.activate(window, cx));
        } else {
            next_window.update(cx, |pane_host, cx| pane_host.deactivate(cx));
        }
        self.sync_terminal_focus_blocker(cx);
        self.scroll_active_window_into_view();
        cx.emit(WindowManagerEvent::PresentationChanged);
        cx.notify();
        true
    }

    fn activate_window_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let window_id = self
            .windows
            .iter()
            .nth(index)
            .map(|(window_id, _)| window_id);
        if let Some(window_id) = window_id {
            self.activate_window(window_id, window, cx);
        }
    }

    fn close_window(&mut self, window_id: WindowId, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_workspace_requested {
            return;
        }

        let was_active = self.windows.active_window_id() == window_id;
        match self.windows.close_window(window_id) {
            Ok(CloseWindowOutcome::WindowClosed {
                closed_window_id,
                payload,
                active_window_id,
            }) => {
                debug_assert_eq!(closed_window_id, window_id);
                payload.update(cx, |pane_host, cx| pane_host.close_all(cx));
                if was_active {
                    let active_window = self.windows.active_window().clone();
                    if self.active {
                        active_window.update(cx, |pane_host, cx| pane_host.activate(window, cx));
                    } else {
                        active_window.update(cx, |pane_host, cx| pane_host.deactivate(cx));
                    }
                }
                self.window_menu = None;
                self.window_selector_pressed = None;
                self.sync_terminal_focus_blocker(cx);
                debug_assert_eq!(active_window_id, self.windows.active_window_id());
                self.scroll_active_window_into_view();
                cx.emit(WindowManagerEvent::WindowClosed {
                    window_id: closed_window_id,
                });
                cx.emit(WindowManagerEvent::PresentationChanged);
                cx.notify();
            }
            Ok(CloseWindowOutcome::CloseWorkspace { final_window_id }) => {
                self.close_workspace_requested = true;
                self.window_menu = None;
                self.window_selector_pressed = None;
                cx.emit(WindowManagerEvent::FinalWindowCloseRequested { final_window_id });
            }
            Err(error) => {
                self.window_menu = None;
                self.window_selector_pressed = None;
                self.sync_terminal_focus_blocker(cx);
                Self::report_window_error("close", error);
            }
        }
    }

    fn open_window_menu(
        &mut self,
        window_id: WindowId,
        left: Option<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.window_menu = Some(WindowMenuState { window_id, left });
        self.sync_terminal_focus_blocker(cx);
        if !self.activate_window_preserving_menu(window_id, window, cx) {
            self.window_menu = None;
            self.sync_terminal_focus_blocker(cx);
            return;
        }
        cx.notify();
    }

    fn toggle_active_window_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let window_id = self.windows.active_window_id();
        if self
            .window_menu
            .is_some_and(|menu| menu.window_id == window_id && menu.left.is_none())
        {
            self.window_menu = None;
            self.sync_terminal_focus_blocker(cx);
            cx.notify();
            return;
        }
        self.open_window_menu(window_id, None, window, cx);
    }

    fn open_context_menu(
        &mut self,
        window_id: WindowId,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let content_width = window.bounds().size.width;
        let maximum_left = (content_width - px(PANE_ACTION_MENU_WIDTH + WINDOW_CONTROL_INSET))
            .max(px(WINDOW_CONTROL_INSET));
        let left = event
            .position
            .x
            .clamp(px(WINDOW_CONTROL_INSET), maximum_left);
        self.open_window_menu(window_id, Some(left), window, cx);
    }

    fn perform_menu_command(
        &mut self,
        command: PaneActionMenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.window_menu else {
            return;
        };
        let Some(pane_host) = self.windows.window(menu.window_id).cloned() else {
            self.window_menu = None;
            self.sync_terminal_focus_blocker(cx);
            return;
        };

        match command {
            PaneActionMenuCommand::SplitRight => pane_host.update(cx, |pane_host, cx| {
                pane_host.split_focused(SplitAxis::Horizontal, window, cx);
            }),
            PaneActionMenuCommand::SplitDown => pane_host.update(cx, |pane_host, cx| {
                pane_host.split_focused(SplitAxis::Vertical, window, cx);
            }),
            PaneActionMenuCommand::ToggleZoom => {
                pane_host.update(cx, |pane_host, cx| pane_host.toggle_zoom(window, cx));
            }
            PaneActionMenuCommand::Close => self.close_window(menu.window_id, window, cx),
        }
        if self.window_menu.take().is_some() {
            self.sync_terminal_focus_blocker(cx);
        }
        cx.notify();
    }

    fn dismiss_window_menu(&mut self, cx: &mut Context<Self>) {
        if self.window_menu.take().is_some() {
            self.sync_terminal_focus_blocker(cx);
            cx.notify();
        }
    }

    fn on_close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        self.close_window(self.windows.active_window_id(), window, cx);
    }

    fn on_create_window(&mut self, _: &CreateWindow, window: &mut Window, cx: &mut Context<Self>) {
        self.create_window(window, cx);
    }

    fn on_activate_window_1(
        &mut self,
        _: &ActivateWindow1,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(0, window, cx);
    }

    fn on_activate_window_2(
        &mut self,
        _: &ActivateWindow2,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(1, window, cx);
    }

    fn on_activate_window_3(
        &mut self,
        _: &ActivateWindow3,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(2, window, cx);
    }

    fn on_activate_window_4(
        &mut self,
        _: &ActivateWindow4,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(3, window, cx);
    }

    fn on_activate_window_5(
        &mut self,
        _: &ActivateWindow5,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(4, window, cx);
    }

    fn on_activate_window_6(
        &mut self,
        _: &ActivateWindow6,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(5, window, cx);
    }

    fn on_activate_window_7(
        &mut self,
        _: &ActivateWindow7,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(6, window, cx);
    }

    fn on_activate_window_8(
        &mut self,
        _: &ActivateWindow8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(7, window, cx);
    }

    fn on_activate_window_9(
        &mut self,
        _: &ActivateWindow9,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_window_at(8, window, cx);
    }

    fn render_window_item(
        &self,
        window_id: WindowId,
        title: SharedString,
        active: bool,
        manager: gpui::WeakEntity<Self>,
    ) -> AnyElement {
        let press_manager = manager.clone();
        let release_manager = manager.clone();
        let click_manager = manager.clone();
        let context_manager = manager.clone();
        let close_manager = manager;
        let window_group = format!("window-item-{}", window_id.get());
        div()
            .id(("window-item", window_id.get()))
            .debug_selector(move || {
                format!(
                    "window-item-{}-{}",
                    window_id.get(),
                    if active { "active" } else { "inactive" }
                )
            })
            .relative()
            .group(window_group.clone())
            .h_full()
            .flex_none()
            .w(px(WINDOW_ITEM_WIDTH))
            .min_w(px(WINDOW_ITEM_MINIMUM_WIDTH))
            .max_w(px(WINDOW_ITEM_MAXIMUM_WIDTH))
            .pl(px(12.0))
            .pr(px(WINDOW_ITEM_RIGHT_PADDING))
            .flex()
            .items_center()
            .cursor_pointer()
            .block_mouse_except_scroll()
            .bg(gpui_color(if active {
                ACTIVE_THEME.tab_active_background
            } else {
                ACTIVE_THEME.tab_inactive_background
            }))
            .text_size(px(12.0))
            .text_color(gpui_color(if active {
                ACTIVE_THEME.text_accent
            } else {
                ACTIVE_THEME.text_muted
            }))
            .hover(|item| item.bg(gpui_color(ACTIVE_THEME.ghost_element_selected)))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                let _ = press_manager.update(cx, |manager, cx| {
                    manager.begin_window_selector(window_id, cx);
                });
                cx.stop_propagation();
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                let _ = release_manager.update(cx, |manager, cx| {
                    manager.cancel_window_selector(window_id, cx);
                });
            })
            .on_click(move |_, window, cx| {
                let _ = click_manager.update(cx, |manager, cx| {
                    manager.commit_window_selector(window_id, window, cx);
                });
                cx.stop_propagation();
            })
            .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                let _ = context_manager.update(cx, |manager, cx| {
                    manager.open_context_menu(window_id, event, window, cx);
                });
                cx.stop_propagation();
            })
            .child(
                div()
                    .id(("window-title", window_id.get()))
                    .debug_selector(move || format!("window-title-{}", window_id.get()))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(title),
            )
            .child(
                div()
                    .id(("window-close-button", window_id.get()))
                    .debug_selector(move || format!("window-close-button-{}", window_id.get()))
                    .size(px(WINDOW_CLOSE_CONTROL_SIZE))
                    .ml(px(4.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .block_mouse_except_scroll()
                    .when(!active, |button| {
                        button
                            .opacity(0.0)
                            .group_hover(window_group, |button| button.opacity(1.0))
                    })
                    .hover(|button| {
                        button
                            .opacity(1.0)
                            .bg(gpui_color(ACTIVE_THEME.ghost_element_hover))
                    })
                    .on_click(move |_, window, cx| {
                        let _ = close_manager.update(cx, |manager, cx| {
                            manager.close_window(window_id, window, cx);
                        });
                        cx.stop_propagation();
                    })
                    .child(
                        Icon::new("xmark")
                            .weight(SymbolWeight::Medium)
                            .size(px(WINDOW_CLOSE_ICON_SIZE))
                            .color(gpui_color(ACTIVE_THEME.icon)),
                    ),
            )
            .child(
                div()
                    .id(("window-item-divider", window_id.get()))
                    .debug_selector(move || format!("window-item-{}-divider", window_id.get()))
                    .absolute()
                    .top_0()
                    .right_0()
                    .h_full()
                    .w(px(WINDOW_BAR_DIVIDER_SIZE))
                    .bg(gpui_color(ACTIVE_THEME.border)),
            )
            .child(
                div()
                    .id(("window-item-bottom-divider", window_id.get()))
                    .debug_selector(move || {
                        format!("window-item-{}-bottom-divider", window_id.get())
                    })
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .w_full()
                    .h(px(WINDOW_BAR_DIVIDER_SIZE))
                    .bg(gpui_color(ACTIVE_THEME.border)),
            )
            .when(active, |item| {
                item.child(
                    div()
                        .id(("window-item-underline", window_id.get()))
                        .debug_selector(move || {
                            format!("window-item-{}-underline", window_id.get())
                        })
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .h(px(WINDOW_BAR_DIVIDER_SIZE))
                        .bg(gpui_color(ACTIVE_THEME.panel_focused_border)),
                )
            })
            .into_any_element()
    }

    fn render_window_bar(&self, manager: gpui::WeakEntity<Self>, cx: &App) -> AnyElement {
        let active_window_id = self.windows.active_window_id();
        let mut items = div()
            .id("window-items")
            .debug_selector(|| "window-items".to_owned())
            .h_full()
            .min_w_0()
            .flex_1()
            .flex()
            .flex_row()
            .overflow_x_scroll()
            .track_scroll(&self.window_bar_scroll_handle)
            .occlude();
        for (window_id, pane_host) in self.windows.iter() {
            items = items.child(self.render_window_item(
                window_id,
                pane_host.read(cx).window_title(),
                window_id == active_window_id,
                manager.clone(),
            ));
        }

        let chrome_down_manager = manager.clone();
        let chrome_event_manager = manager.clone();
        let chrome_move_manager = manager.clone();
        let chrome_up_manager = manager.clone();
        let chrome_out_manager = manager.clone();
        let create_manager = manager.clone();
        let menu_manager = manager;
        div()
            .id("window-bar")
            .debug_selector(|| "window-bar".to_owned())
            .relative()
            .h(px(WINDOW_BAR_HEIGHT))
            .min_w_0()
            .flex_1()
            .flex_shrink_0()
            .flex()
            .flex_row()
            .items_center()
            .pr(px(WINDOW_CONTROL_SIZE + WINDOW_CONTROL_INSET * 2.0))
            .bg(gpui_color(ACTIVE_THEME.tab_bar_background))
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                handle_top_chrome_mouse_down(event, window, cx, |blocked, _, cx| {
                    let _ = chrome_down_manager.update(cx, |manager, cx| {
                        manager.set_top_chrome_interaction(blocked, cx);
                    });
                });
            })
            .on_mouse_move(move |event, window, cx| {
                if event.dragging() {
                    let _ = chrome_move_manager.update(cx, |manager, cx| {
                        manager.continue_top_chrome_interaction(window, cx);
                    });
                }
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                let _ = chrome_up_manager.update(cx, |manager, cx| {
                    manager.finish_top_chrome_interaction(cx);
                });
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                let _ = chrome_out_manager.update(cx, |manager, cx| {
                    manager.finish_top_chrome_interaction(cx);
                });
            })
            .child(
                canvas(
                    |_, _, _| (),
                    move |chrome_bounds, _, window, _| {
                        let down_manager = chrome_event_manager.clone();
                        window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                            if phase != DispatchPhase::Capture
                                || event.button != MouseButton::Left
                                || !chrome_bounds.contains(&event.position)
                            {
                                return;
                            }
                            let blocked = event.click_count == 1;
                            let _ = down_manager.update(cx, |manager, cx| {
                                manager.set_top_chrome_interaction(blocked, cx);
                            });
                        });

                        let move_manager = chrome_event_manager.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                            if phase == DispatchPhase::Capture && event.dragging() {
                                let _ = move_manager.update(cx, |manager, cx| {
                                    manager.continue_top_chrome_interaction(window, cx);
                                });
                            }
                        });

                        let up_manager = chrome_event_manager.clone();
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture && event.button == MouseButton::Left
                            {
                                let _ = up_manager.update(cx, |manager, cx| {
                                    manager.finish_top_chrome_interaction(cx);
                                });
                            }
                        });
                    },
                )
                .absolute()
                .inset_0(),
            )
            .child(
                div()
                    .id("window-bar-divider")
                    .debug_selector(|| "window-bar-divider".to_owned())
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .w_full()
                    .h(px(WINDOW_BAR_DIVIDER_SIZE))
                    .bg(gpui_color(ACTIVE_THEME.border)),
            )
            .child(items)
            .child(
                div()
                    .id("create-window-button")
                    .debug_selector(|| "create-window-button".to_owned())
                    .size(px(WINDOW_CONTROL_SIZE))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .occlude()
                    .hover(|button| button.bg(gpui_color(ACTIVE_THEME.ghost_element_selected)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        let _ = create_manager.update(cx, |manager, cx| {
                            manager.create_window(window, cx);
                        });
                        cx.stop_propagation();
                    })
                    .child(
                        Icon::new("plus")
                            .size(px(14.0))
                            .color(gpui_color(ACTIVE_THEME.icon)),
                    ),
            )
            .child(
                div()
                    .id("window-menu-button")
                    .debug_selector(|| "window-menu-button".to_owned())
                    .absolute()
                    .top(px(WINDOW_CONTROL_INSET))
                    .right(px(WINDOW_CONTROL_INSET))
                    .size(px(WINDOW_CONTROL_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .occlude()
                    .hover(|button| button.bg(gpui_color(ACTIVE_THEME.ghost_element_selected)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        let _ = menu_manager.update(cx, |manager, cx| {
                            manager.toggle_active_window_menu(window, cx);
                        });
                        cx.stop_propagation();
                    })
                    .child(
                        Icon::new("ellipsis")
                            .size(px(16.0))
                            .color(gpui_color(ACTIVE_THEME.icon)),
                    ),
            )
            .into_any_element()
    }

    fn render_window_menu(
        &self,
        menu: WindowMenuState,
        manager: gpui::WeakEntity<Self>,
        cx: &App,
    ) -> AnyElement {
        let Some((zoomed, zoom_enabled)) = self.windows.window(menu.window_id).map(|pane_host| {
            let pane_host = pane_host.read(cx);
            (
                matches!(pane_host.zoom_state(), ZoomState::Zoomed(_)),
                pane_host.pane_count() > 1,
            )
        }) else {
            return div().into_any_element();
        };
        let dismiss_manager = manager.clone();
        let menu_element = render_pane_action_menu(
            ("window-menu", menu.window_id.get()),
            zoomed,
            zoom_enabled,
            CloseTarget::Window,
            manager,
            |manager, command, window, cx| {
                manager.perform_menu_command(command, window, cx);
            },
        );
        let controls = div()
            .id(("window-menu-controls", menu.window_id.get()))
            .debug_selector(move || format!("window-menu-controls-{}", menu.window_id.get()))
            .absolute()
            .top(px(WINDOW_MENU_TOP))
            .w(px(PANE_ACTION_MENU_WIDTH))
            .h(px(PANE_ACTION_MENU_HEIGHT))
            .on_mouse_down_out(move |event, window, cx| {
                if window_menu_button_contains(event.position, window.bounds().size.width) {
                    return;
                }
                let _ = dismiss_manager.update(cx, |manager, cx| {
                    manager.dismiss_window_menu(cx);
                });
            })
            .child(menu_element);
        match menu.left {
            Some(left) => controls.left(left),
            None => controls.right(px(WINDOW_CONTROL_INSET)),
        }
        .into_any_element()
    }
}

impl Render for WindowManager {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        debug_assert!(self.windows.len() > 0);
        let manager = cx.entity().downgrade();
        let release_manager = manager.clone();
        let exit_manager = manager.clone();
        let active_window = self.windows.active_window().clone();
        let window_bar = self.render_window_bar(manager.clone(), cx);

        div()
            .id("window-manager")
            .debug_selector(|| "window-manager".to_owned())
            .key_context(TERMINAL_KEY_CONTEXT)
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(gpui_color(ACTIVE_THEME.terminal_background))
            .capture_any_mouse_up(move |event, _, cx| {
                if event.button == MouseButton::Left {
                    let _ = release_manager.update(cx, |manager, cx| {
                        manager.finish_top_chrome_interaction(cx);
                    });
                }
            })
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble {
                                let _ = exit_manager.update(cx, |manager, cx| {
                                    manager.finish_top_chrome_interaction(cx);
                                });
                            }
                        });
                    },
                )
                .absolute()
                .inset_0(),
            )
            .on_action(cx.listener(Self::on_create_window))
            .on_action(cx.listener(Self::on_activate_window_1))
            .on_action(cx.listener(Self::on_activate_window_2))
            .on_action(cx.listener(Self::on_activate_window_3))
            .on_action(cx.listener(Self::on_activate_window_4))
            .on_action(cx.listener(Self::on_activate_window_5))
            .on_action(cx.listener(Self::on_activate_window_6))
            .on_action(cx.listener(Self::on_activate_window_7))
            .on_action(cx.listener(Self::on_activate_window_8))
            .on_action(cx.listener(Self::on_activate_window_9))
            .on_action(cx.listener(Self::on_close_window))
            .child(
                div()
                    .h(px(TOP_CHROME_HEIGHT))
                    .w_full()
                    .flex_shrink_0()
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .id("window-manager-top-spacer")
                            .debug_selector(|| "window-manager-top-spacer".to_owned())
                            .w(self.sidebar_width)
                            .h_full()
                            .flex_shrink_0()
                            .bg(gpui_color(ACTIVE_THEME.tab_bar_background)),
                    )
                    .child(window_bar),
            )
            .child(
                div()
                    .id("window-manager-content")
                    .debug_selector(|| "window-manager-content".to_owned())
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .when(self.sidebar_visible, |body| body.ml(self.sidebar_width))
                    .child(active_window),
            )
            .when_some(self.window_menu, |root, menu| {
                root.child(self.render_window_menu(menu, manager, cx))
            })
    }
}

impl EventEmitter<WindowManagerEvent> for WindowManager {}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
}

fn window_menu_button_contains(position: Point<Pixels>, content_width: Pixels) -> bool {
    let left = content_width - px(WINDOW_CONTROL_INSET + WINDOW_CONTROL_SIZE);
    let right = content_width - px(WINDOW_CONTROL_INSET);
    let top = px(WINDOW_CONTROL_INSET);
    let bottom = top + px(WINDOW_CONTROL_SIZE);

    position.x >= left && position.x <= right && position.y >= top && position.y <= bottom
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::Arc;

    use gpui::{
        Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, VisualTestContext,
        point,
    };

    use super::*;
    use crate::domain::PaneId;
    use crate::terminal::testing::{
        RecordedSessionCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
    };
    use crate::terminal::{ScreenSnapshot, SessionEvent, SessionExit, TerminalSessionFactory};
    use crate::ui::workspace_manager::{
        DynamicWorkspaceDirectorySource, WorkspaceDirectorySource, WorkspaceDirectoryUnavailable,
    };
    use std::cell::RefCell;

    /// A directory source whose availability tests control directly.
    #[derive(Clone)]
    struct ToggleableDirectoryGate(Rc<RefCell<Option<PathBuf>>>);

    impl WorkspaceDirectorySource for ToggleableDirectoryGate {
        fn resolve(&self) -> Result<PathBuf, WorkspaceDirectoryUnavailable> {
            self.0.borrow().clone().ok_or(WorkspaceDirectoryUnavailable)
        }
    }

    fn window_manager(
        cx: &mut TestAppContext,
    ) -> (
        Entity<WindowManager>,
        TestTerminalSessionRecords,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new(
            session_factory,
            PathBuf::from("/tmp/spaceterm-window-manager-test"),
        );
        let directory_gate = DynamicWorkspaceDirectorySource::available(PathBuf::from(
            "/tmp/spaceterm-window-manager-test",
        ));
        let (manager, cx) = cx.add_window_view(|window, cx| {
            WindowManager::new(session_factory, Rc::new(directory_gate), window, cx)
        });
        cx.update(|window, cx| {
            window.activate_window();
            manager.update(cx, |manager, cx| manager.focus(window, cx));
        });
        cx.run_until_parked();
        (manager, records, cx)
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

    /// A screen snapshot whose Terminal Metadata reports `directory` as the
    /// Reported Working Directory.
    fn screen_with_reported_directory(directory: &Path) -> Arc<ScreenSnapshot> {
        use crate::terminal::metadata::{
            DirectoryMetadata, DirectoryProvenance, MetadataFreshness, ProgressMetadata,
            PromptZone, TerminalMetadataSnapshot, TitleMetadata, TitleProvenance,
        };
        let mut screen = Arc::unwrap_or_clone(ScreenSnapshot::from_test_parts(
            Arc::from([]),
            Default::default(),
            "",
        ));
        screen.metadata = Arc::new(TerminalMetadataSnapshot {
            revision: 0,
            freshness: MetadataFreshness::Live,
            title: TitleMetadata {
                value: Arc::from(""),
                provenance: TitleProvenance::Fallback,
            },
            directory: DirectoryMetadata {
                path: Arc::from(directory.to_string_lossy().as_ref()),
                provenance: DirectoryProvenance::Osc7,
            },
            prompt_zone: PromptZone::Unknown,
            command: None,
            progress: ProgressMetadata::None,
        });
        Arc::new(screen)
    }

    #[gpui::test]
    fn window_managers_should_forward_pane_reports_and_close_notifications(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        let forwarded = Rc::new(RefCell::new(Vec::new()));
        {
            let forwarded = Rc::clone(&forwarded);
            manager.update(cx, |_, cx| {
                cx.subscribe(&manager, move |_, _, event: &WindowManagerEvent, _| {
                    if matches!(
                        event,
                        WindowManagerEvent::PaneReportedDirectoryChanged { .. }
                            | WindowManagerEvent::PaneClosed { .. }
                            | WindowManagerEvent::WindowClosed { .. }
                    ) {
                        forwarded.borrow_mut().push(*event);
                    }
                })
                .detach();
            });
        }
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.create_window(window, cx));
        });
        cx.run_until_parked();

        records
            .event_sender(1)
            .expect("the initial Window session was not started")
            .try_send(SessionEvent::Screen(screen_with_reported_directory(
                Path::new("/tmp/spaceterm-forward-report"),
            )))
            .expect("the report must be delivered");
        cx.run_until_parked();

        records
            .event_sender(1)
            .expect("the initial Window session must remain started")
            .try_send(SessionEvent::Exited(SessionExit::Success))
            .expect("the exit must be delivered");
        cx.run_until_parked();

        assert_eq!(
            *forwarded.borrow(),
            vec![
                WindowManagerEvent::PaneReportedDirectoryChanged {
                    window_id: WindowId::new(1),
                    pane_id: PaneId::new(1),
                },
                WindowManagerEvent::PaneClosed {
                    window_id: WindowId::new(1),
                    closed_pane_id: PaneId::new(1),
                },
                WindowManagerEvent::WindowClosed {
                    window_id: WindowId::new(1),
                },
            ]
        );
    }

    #[gpui::test]
    fn window_bar_should_keep_dim_dividers_beneath_every_item_and_accent_the_active_window(
        cx: &mut TestAppContext,
    ) {
        let (_manager, _records, cx) = window_manager(cx);
        click("create-window-button", cx);

        let bar = cx
            .debug_bounds("window-bar")
            .expect("the Window bar was not rendered");
        let divider = cx
            .debug_bounds("window-bar-divider")
            .expect("the Window bar divider was not rendered");
        let underline = cx
            .debug_bounds("window-item-2-underline")
            .expect("the Active Window underline was not rendered");
        let inactive_item = cx
            .debug_bounds("window-item-1-inactive")
            .expect("the inactive Window item was not rendered");
        let active_item = cx
            .debug_bounds("window-item-2-active")
            .expect("the Active Window item was not rendered");
        let item_divider = cx
            .debug_bounds("window-item-1-divider")
            .expect("the Window item divider was not rendered");
        let inactive_bottom_divider = cx
            .debug_bounds("window-item-1-bottom-divider")
            .expect("the inactive Window bottom divider was not rendered");
        let active_bottom_divider = cx
            .debug_bounds("window-item-2-bottom-divider")
            .expect("the Active Window bottom divider was not rendered");

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
                px(WINDOW_BAR_HEIGHT),
                px(WINDOW_BAR_DIVIDER_SIZE),
                px(WINDOW_BAR_DIVIDER_SIZE),
                underline.origin.y + underline.size.height,
                px(WINDOW_BAR_DIVIDER_SIZE),
                inactive_item.size.height,
                inactive_item.origin.x + inactive_item.size.width,
                divider.origin.y,
                gpui::size(inactive_item.size.width, px(WINDOW_BAR_DIVIDER_SIZE)),
                divider.origin.y,
                gpui::size(active_item.size.width, px(WINDOW_BAR_DIVIDER_SIZE)),
            )
        );
    }

    #[gpui::test]
    fn window_bar_should_start_after_the_persistent_sidebar_chrome(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = window_manager(cx);
        let root = cx
            .debug_bounds("window-manager")
            .expect("the Window manager was not rendered");
        let spacer = cx
            .debug_bounds("window-manager-top-spacer")
            .expect("the persistent top-left spacer was not rendered");
        let bar = cx
            .debug_bounds("window-bar")
            .expect("the Window bar was not rendered");

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
    fn hiding_sidebar_should_expand_content_without_moving_the_window_bar(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);
        let root = cx
            .debug_bounds("window-manager")
            .expect("the Window manager was not rendered");
        let visible_content = cx
            .debug_bounds("window-manager-content")
            .expect("the Window content was not rendered");
        let visible_bar = cx
            .debug_bounds("window-bar")
            .expect("the Window bar was not rendered");

        manager.update(cx, |manager, cx| {
            manager.set_sidebar_layout(false, px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH), cx);
        });
        cx.run_until_parked();

        let hidden_content = cx
            .debug_bounds("window-manager-content")
            .expect("the expanded Window content was not rendered");
        let hidden_bar = cx
            .debug_bounds("window-bar")
            .expect("the Window bar was not rendered");
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
                root.origin.x + px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            )
        );
    }

    #[gpui::test]
    fn command_t_should_create_and_activate_a_new_window(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);

        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (2, WindowId::new(2), Vec::new()));
    }

    #[gpui::test]
    fn unavailable_workspace_directory_should_block_window_creation_and_emit_child_creation_blocked(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let session_factory = WorkspaceTerminalSessionFactory::new(
            session_factory,
            PathBuf::from("/tmp/spaceterm-window-manager-test"),
        );
        let directory_gate = ToggleableDirectoryGate(Rc::new(RefCell::new(Some(PathBuf::from(
            "/tmp/spaceterm-window-manager-test",
        )))));
        let (manager, cx) = cx.add_window_view(|window, cx| {
            WindowManager::new(session_factory, Rc::new(directory_gate.clone()), window, cx)
        });
        let blocked_events = Rc::new(Cell::new(0));
        let blocked_events_for_subscription = Rc::clone(&blocked_events);
        manager.update(cx, |_, cx| {
            cx.subscribe(&manager, move |_, _, event: &WindowManagerEvent, _| {
                if matches!(event, WindowManagerEvent::ChildCreationBlocked) {
                    blocked_events_for_subscription.update(|count| count + 1);
                }
            })
            .detach();
        });
        cx.run_until_parked();

        directory_gate.0.borrow_mut().take();
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.create_window(window, cx));
        });
        cx.run_until_parked();

        let blocked_state = manager.read_with(cx, |manager, _| {
            (manager.windows.len(), manager.windows.active_window_id())
        });
        assert_eq!(blocked_state, (1, WindowId::new(1)));
        assert_eq!(blocked_events.get(), 1);
        assert_eq!(records.starts().len(), 1);

        directory_gate
            .0
            .borrow_mut()
            .replace(PathBuf::from("/tmp/spaceterm-window-manager-test"));
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.create_window(window, cx));
        });
        cx.run_until_parked();

        let restored_state = manager.read_with(cx, |manager, _| {
            (manager.windows.len(), manager.windows.active_window_id())
        });
        assert_eq!(restored_state, (2, WindowId::new(2)));
        assert_eq!(blocked_events.get(), 1);
        assert_eq!(records.starts().len(), 2);
    }

    #[gpui::test]
    fn command_number_shortcuts_should_activate_windows_by_position(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);
        for _ in 1..9 {
            cx.simulate_keystrokes("cmd-t");
            cx.run_until_parked();
        }

        let mut active_window_ids = Vec::new();
        for shortcut in [
            "cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-5", "cmd-6", "cmd-7", "cmd-8", "cmd-9",
        ] {
            cx.simulate_keystrokes(shortcut);
            cx.run_until_parked();
            active_window_ids
                .push(manager.read_with(cx, |manager, _| manager.windows.active_window_id()));
        }

        assert_eq!(
            active_window_ids,
            (1..=9).map(WindowId::new).collect::<Vec<_>>()
        );
    }

    #[gpui::test]
    fn unavailable_command_number_shortcut_should_preserve_the_active_window(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = window_manager(cx);

        cx.simulate_keystrokes("cmd-9");
        cx.run_until_parked();

        let active_window_id =
            manager.read_with(cx, |manager, _| manager.windows.active_window_id());
        assert_eq!(active_window_id, WindowId::new(1));
    }

    #[gpui::test]
    fn create_button_should_create_and_activate_without_dropping_the_inactive_window(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        let first_entity_id =
            manager.read_with(cx, |manager, _| manager.windows.active_window().entity_id());

        click("create-window-button", cx);

        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                manager
                    .windows
                    .window(WindowId::new(1))
                    .map(Entity::entity_id),
                manager
                    .windows
                    .window(WindowId::new(1))
                    .is_some_and(|window| !window.read(cx).is_active()),
                manager.windows.active_window().read(cx).is_active(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(
            state,
            (
                2,
                WindowId::new(2),
                Some(first_entity_id),
                true,
                true,
                Vec::new(),
            )
        );
    }

    #[gpui::test]
    fn single_pane_window_title_should_follow_the_terminal_title(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        let sender = records
            .event_sender(1)
            .expect("the initial Window session must have started");

        sender
            .try_send(SessionEvent::Screen(ScreenSnapshot::from_test_parts(
                Arc::from([]),
                Default::default(),
                "Claude Code",
            )))
            .unwrap();
        cx.run_until_parked();

        let title = manager.read_with(cx, |manager, cx| {
            manager.windows.active_window().read(cx).window_title()
        });
        assert_eq!(title.as_ref(), "Claude Code");
    }

    #[gpui::test]
    fn split_window_title_should_show_the_count_and_restore_the_terminal_title_after_close(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        let sender = records
            .event_sender(1)
            .expect("the initial Window session must have started");
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
            manager.windows.active_window().read(cx).window_title()
        });
        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        let restored_title = manager.read_with(cx, |manager, cx| {
            manager.windows.active_window().read(cx).window_title()
        });

        assert_eq!(
            (split_title.as_ref(), restored_title.as_ref()),
            ("2 Panes", "Claude Code")
        );
    }

    #[gpui::test]
    fn hover_close_button_should_close_its_window_without_activating_it(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);

        click("window-close-button-1", cx);

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (1, WindowId::new(2), vec![1]));
    }

    #[gpui::test]
    fn active_window_close_button_should_close_the_active_window(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);

        click("window-close-button-2", cx);

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (1, WindowId::new(1), vec![2]));
    }

    #[gpui::test]
    fn window_close_button_should_use_a_compact_right_inset(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = window_manager(cx);
        let item = cx
            .debug_bounds("window-item-1-active")
            .expect("the Active Window item was not rendered");
        let close_button = cx
            .debug_bounds("window-close-button-1")
            .expect("the Active Window close button was not rendered");

        assert_eq!(
            (
                item.origin.x + item.size.width - (close_button.origin.x + close_button.size.width),
                close_button.size,
            ),
            (
                px(WINDOW_ITEM_RIGHT_PADDING),
                gpui::size(px(WINDOW_CLOSE_CONTROL_SIZE), px(WINDOW_CLOSE_CONTROL_SIZE)),
            )
        );
    }

    #[gpui::test]
    fn creating_windows_should_scroll_the_active_window_into_view(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);

        for _ in 0..20 {
            click("create-window-button", cx);
        }

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                manager.window_bar_scroll_handle.offset().x,
            )
        });
        assert_eq!((state.0, state.1), (21, WindowId::new(21)));
        assert!(
            state.2 < px(0.0),
            "the Window bar did not scroll; offset was {:?}",
            state.2
        );
    }

    #[gpui::test]
    fn window_items_should_scroll_horizontally_with_the_mouse_wheel(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);
        for _ in 0..12 {
            click("create-window-button", cx);
        }

        manager.read_with(cx, |manager, _| {
            manager
                .window_bar_scroll_handle
                .set_offset(point(px(0.0), px(0.0)));
        });
        manager.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let items = cx
            .debug_bounds("window-items")
            .expect("the Window item strip was not rendered");
        cx.simulate_event(ScrollWheelEvent {
            position: items.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-120.0))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();

        let offset =
            manager.read_with(cx, |manager, _| manager.window_bar_scroll_handle.offset().x);
        assert!(
            offset < px(0.0),
            "the Window strip did not scroll; offset was {offset:?}"
        );
    }

    #[gpui::test]
    fn activating_an_inactive_window_should_restore_its_focused_pane(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);
        cx.simulate_keystrokes("cmd-d");
        click("create-window-button", cx);

        click("window-item-1-inactive", cx);

        let first_window = manager.read_with(cx, |manager, _| {
            manager
                .windows
                .window(WindowId::new(1))
                .cloned()
                .expect("Window 1 must remain owned")
        });
        let state = cx.update(|window, cx| {
            let pane_host = first_window.read(cx);
            (
                manager.read(cx).windows.active_window_id(),
                pane_host.focused_pane_id(),
                pane_host.focused_terminal_is_focused(window, cx),
            )
        });
        assert_eq!(state, (WindowId::new(1), PaneId::new(2), true));
    }

    #[gpui::test]
    fn right_click_should_activate_and_target_the_clicked_window(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);
        click("create-window-button", cx);

        right_click("window-item-1-inactive", cx);

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.active_window_id(),
                manager
                    .window_menu
                    .map(|menu| (menu.window_id, menu.left.is_some())),
            )
        });
        assert_eq!(state, (WindowId::new(1), Some((WindowId::new(1), true))));
        let services_blocked = cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                !manager
                    .native_service_status(WorkspaceId::new(1), window, cx)
                    .capabilities
                    .return_text
            })
        });
        assert!(services_blocked);
    }

    #[gpui::test]
    fn inactive_window_context_menu_should_not_transiently_focus_its_terminal(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);
        let command_count = records.commands().len();

        right_click("window-item-1-inactive", cx);

        let state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.windows.active_window_id(),
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
            (state, focus_edges),
            ((WindowId::new(1), true, false), vec![(2, false)])
        );
    }

    #[gpui::test]
    fn top_ellipsis_should_target_the_active_window(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);
        click("create-window-button", cx);

        click("window-menu-button", cx);

        let menu = manager.read_with(cx, |manager, _| manager.window_menu);
        assert_eq!(
            menu.map(|menu| (menu.window_id, menu.left)),
            Some((WindowId::new(2), None))
        );
    }

    #[gpui::test]
    fn window_menu_blocks_services_and_invalidates_the_previous_focus_branch(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = window_manager(cx);
        let before = cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.native_service_status(WorkspaceId::new(1), window, cx)
            })
        });

        click("window-menu-button", cx);
        let blocked = cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.native_service_status(WorkspaceId::new(1), window, cx)
            })
        });
        click("window-menu-button", cx);
        let restored = cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.native_service_status(WorkspaceId::new(1), window, cx)
            })
        });

        assert!(before.capabilities.return_text);
        assert!(!blocked.capabilities.return_text);
        assert!(restored.capabilities.return_text);
        assert_ne!(before.origin, restored.origin);
    }

    #[gpui::test]
    fn top_ellipsis_should_toggle_its_open_menu_closed(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);
        click("window-menu-button", cx);

        click("window-menu-button", cx);

        let menu = manager.read_with(cx, |manager, _| manager.window_menu);
        assert_eq!(menu, None);
    }

    #[gpui::test]
    fn window_menu_should_block_and_restore_input_without_changing_focused_pane(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        let command_count = records.commands().len();
        let focused_pane_id = manager.read_with(cx, |manager, cx| {
            manager.windows.active_window().read(cx).focused_pane_id()
        });

        click("window-menu-button", cx);

        let menu_open = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.windows.active_window().read(cx).focused_pane_id(),
                manager.focused_terminal_is_focused(window, cx),
                manager.focused_terminal_has_input_focus(window, cx),
            )
        });
        assert_eq!(menu_open, (focused_pane_id, true, false));

        click("window-menu-button", cx);
        cx.simulate_keystrokes("a");

        let commands = records
            .commands()
            .into_iter()
            .skip(command_count)
            .map(|call| (call.session_id, call.command))
            .collect::<Vec<_>>();
        assert!(matches!(
            commands[0],
            (1, RecordedSessionCommand::Focus(false))
        ));
        assert!(matches!(
            commands[1],
            (1, RecordedSessionCommand::Focus(true))
        ));
        assert!(matches!(commands[2], (1, RecordedSessionCommand::Key(_))));
    }

    #[gpui::test]
    fn top_chrome_mouse_down_should_block_until_release_without_changing_focused_pane(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        let command_count = records.commands().len();
        let chrome = cx
            .debug_bounds("window-bar")
            .expect("top chrome must be rendered")
            .center();
        let focused_pane_id = manager.read_with(cx, |manager, cx| {
            manager.windows.active_window().read(cx).focused_pane_id()
        });

        cx.simulate_mouse_down(chrome, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let blocked = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.windows.active_window().read(cx).focused_pane_id(),
                manager.focused_terminal_is_focused(window, cx),
                manager.focused_terminal_has_input_focus(window, cx),
            )
        });
        assert_eq!(blocked, (focused_pane_id, true, false));

        cx.simulate_mouse_move(chrome, None, Modifiers::none());
        cx.run_until_parked();
        assert!(!cx.update(|window, cx| {
            manager
                .read(cx)
                .focused_terminal_has_input_focus(window, cx)
        }));

        let outside_chrome = cx
            .debug_bounds("window-manager-content")
            .expect("Window content must be rendered")
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
    fn window_selector_press_should_block_before_activation_and_restore_selected_terminal(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);
        let command_count = records.commands().len();
        let position = cx
            .debug_bounds("window-item-1-inactive")
            .expect("inactive Window selector must be rendered")
            .center();
        let focused_pane_id = manager.read_with(cx, |manager, cx| {
            manager.windows.active_window().read(cx).focused_pane_id()
        });

        cx.simulate_mouse_move(position, None, Modifiers::none());
        cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        let pressed = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.windows.active_window_id(),
                manager.windows.active_window().read(cx).focused_pane_id(),
                manager.focused_terminal_is_focused(window, cx),
                manager.focused_terminal_has_input_focus(window, cx),
            )
        });
        assert_eq!(pressed, (WindowId::new(2), focused_pane_id, true, false));

        cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        let selected = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.windows.active_window_id(),
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
            ((WindowId::new(1), true, true), vec![(2, false), (1, true)])
        );
    }

    #[gpui::test]
    fn window_menu_split_should_target_the_selected_window_without_terminal_pointer_input(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);
        right_click("window-item-1-inactive", cx);

        click("window-menu-row-split-right", cx);

        let pane_counts = manager.read_with(cx, |manager, cx| {
            (
                manager
                    .windows
                    .window(WindowId::new(1))
                    .expect("Window 1 must remain owned")
                    .read(cx)
                    .pane_count(),
                manager
                    .windows
                    .window(WindowId::new(2))
                    .expect("Window 2 must remain owned")
                    .read(cx)
                    .pane_count(),
                records.pointer_count(),
            )
        });
        assert_eq!(pane_counts, (2, 1, 0));
    }

    #[gpui::test]
    fn single_pane_window_menu_should_disable_zoom_without_dismissing_the_menu(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = window_manager(cx);
        click("window-menu-button", cx);

        click("window-menu-row-toggle-zoom", cx);

        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.window_menu.is_some(),
                manager.windows.active_window().read(cx).zoom_state(),
            )
        });
        assert_eq!(state, (true, ZoomState::Restored));
    }

    #[gpui::test]
    fn open_single_pane_window_menu_should_enable_zoom_after_split_shortcut(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = window_manager(cx);
        click("window-menu-button", cx);

        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();
        click("window-menu-row-toggle-zoom", cx);

        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.window_menu.is_none(),
                manager.windows.active_window().read(cx).zoom_state(),
            )
        });
        assert!(
            matches!(state, (true, ZoomState::Zoomed(_))),
            "the open Window menu did not use the target PaneHost's live zoom state: {state:?}"
        );
    }

    #[gpui::test]
    fn close_window_menu_should_focus_the_neighbor_and_drop_the_closed_session_once(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);
        click("window-menu-button", cx);

        click("window-menu-row-close-window", cx);

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (1, WindowId::new(1), vec![2]));
    }

    #[gpui::test]
    fn closing_an_inactive_window_should_preserve_the_active_window(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.close_window(WindowId::new(1), window, cx);
            });
        });
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.active_window_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (WindowId::new(2), vec![1]));
    }

    #[gpui::test]
    fn inactive_shell_exit_should_close_its_window_without_stealing_focus(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);
        let first_sender = records
            .event_sender(1)
            .expect("Window 1 session must have started");

        first_sender
            .try_send(SessionEvent::Exited(SessionExit::Success))
            .unwrap();
        cx.run_until_parked();

        let active_window =
            manager.read_with(cx, |manager, _| manager.windows.active_window().clone());
        let state = cx.update(|window, cx| {
            (
                manager.read(cx).windows.active_window_id(),
                active_window
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (WindowId::new(2), true, vec![1]));
    }

    #[gpui::test]
    fn inactive_workspace_active_window_exit_should_leave_its_fallback_deactivated_and_unfocused(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);
        let active_sender = records
            .event_sender(2)
            .expect("Window 2 session must have started");
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.deactivate(cx));
            window.blur();
        });

        active_sender
            .try_send(SessionEvent::Exited(SessionExit::Success))
            .unwrap();
        cx.run_until_parked();

        let fallback = manager.read_with(cx, |manager, _| manager.windows.active_window().clone());
        let state = cx.update(|window, cx| {
            (
                manager.read(cx).active,
                manager.read(cx).windows.len(),
                manager.read(cx).windows.active_window_id(),
                fallback.read(cx).is_active(),
                fallback.read(cx).focused_terminal_is_focused(window, cx),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (false, 1, WindowId::new(1), false, false, vec![2]));
    }

    #[gpui::test]
    fn active_shell_exit_should_close_its_window_and_focus_the_neighbor(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);
        let active_sender = records
            .event_sender(2)
            .expect("Window 2 session must have started");

        active_sender
            .try_send(SessionEvent::Exited(SessionExit::Success))
            .unwrap();
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (1, WindowId::new(1), vec![2]));
    }

    #[gpui::test]
    fn closing_a_multi_pane_window_should_close_every_owned_session_exactly_once(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        cx.simulate_keystrokes("cmd-d");
        click("create-window-button", cx);

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.close_window(WindowId::new(1), window, cx);
            });
        });
        cx.run_until_parked();

        let mut dropped = records.dropped_session_ids();
        dropped.sort_unstable();
        assert_eq!(dropped, vec![1, 2]);
    }

    #[gpui::test]
    fn command_w_should_close_only_the_focused_pane_when_the_window_is_split(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();

        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.windows.len(),
                manager.windows.active_window().read(cx).pane_count(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (1, 1, vec![2]));
    }

    #[gpui::test]
    fn command_w_should_close_the_active_window_when_its_last_pane_closes(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        click("create-window-button", cx);

        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (1, WindowId::new(1), vec![2]));
    }

    #[gpui::test]
    fn command_shift_w_should_close_every_pane_in_only_the_active_window(cx: &mut TestAppContext) {
        let (manager, records, cx) = window_manager(cx);
        cx.simulate_keystrokes("cmd-d");
        click("create-window-button", cx);
        click("window-item-1-inactive", cx);

        cx.simulate_keystrokes("cmd-shift-w");
        cx.run_until_parked();

        let mut dropped = records.dropped_session_ids();
        dropped.sort_unstable();
        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.windows.len(),
                manager.windows.active_window_id(),
                manager.windows.active_window().read(cx).pane_count(),
            )
        });
        assert_eq!(state, (1, WindowId::new(2), 1));
        assert_eq!(dropped, vec![1, 2]);
    }

    #[gpui::test]
    fn command_shift_w_should_request_owning_workspace_close_for_the_final_window(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = window_manager(cx);
        let close_requests = Rc::new(Cell::new(0));
        let close_requests_for_subscription = Rc::clone(&close_requests);
        manager.update(cx, |_, cx| {
            cx.subscribe(&manager, move |_, _, event: &WindowManagerEvent, _| {
                if matches!(event, WindowManagerEvent::FinalWindowCloseRequested { .. }) {
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

    #[gpui::test]
    fn window_menu_should_overlap_the_bar_and_clamp_inside_the_content_width(
        cx: &mut TestAppContext,
    ) {
        let (_manager, _records, cx) = window_manager(cx);
        right_click("window-item-1-active", cx);

        let bar = cx
            .debug_bounds("window-bar")
            .expect("the Window bar was not rendered");
        let menu = cx
            .debug_bounds("window-menu-1")
            .expect("the Window menu was not rendered");
        let item = cx
            .debug_bounds("window-item-1-active")
            .expect("the Window item was not rendered");
        let root = cx
            .debug_bounds("window-manager")
            .expect("the Window manager was not rendered");
        let local_pointer_x = item.center().x - root.origin.x;
        let maximum_left = (root.size.width - px(PANE_ACTION_MENU_WIDTH + WINDOW_CONTROL_INSET))
            .max(px(WINDOW_CONTROL_INSET));
        let expected_menu_x =
            root.origin.x + local_pointer_x.clamp(px(WINDOW_CONTROL_INSET), maximum_left);

        assert_eq!(
            (
                bar.origin.y + bar.size.height - menu.origin.y,
                menu.origin.x,
                menu.origin.x >= root.origin.x,
                menu.origin.x + menu.size.width <= root.origin.x + root.size.width,
            ),
            (px(WINDOW_MENU_BAR_OVERLAP), expected_menu_x, true, true)
        );
    }
}
