use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::pane_action_menu::{
    CloseTarget, PaneActionMenuCommand, pane_action_menu_entries, sf_symbol,
};
use super::terminal_focus::TerminalFocusBlocker;
use super::{
    ActivateWindow1, ActivateWindow2, ActivateWindow3, ActivateWindow4, ActivateWindow5,
    ActivateWindow6, ActivateWindow7, ActivateWindow8, ActivateWindow9, CloseWindow, CreateWindow,
    PaneHost, PaneHostEvent, TERMINAL_KEY_CONTEXT, TOP_CHROME_HEIGHT,
    WORKSPACE_SIDEBAR_DEFAULT_WIDTH,
};
use crate::domain::{
    CloseWindowOutcome, PaneId, SplitAxis, WindowCollection, WindowError, WindowId,
    WorkspaceDirectoryIdentity, WorkspaceId, ZoomState,
};
#[cfg(test)]
use crate::platform::macos_window_drag::MacosOperatingSystemWindowDragPlatform;
use crate::platform::macos_window_drag::{
    OperatingSystemWindowDragError, OperatingSystemWindowDragPlatform,
};
use crate::terminal::{
    NativeServiceOrigin, NativeServiceStatus, SelectionCopy, WorkspaceTerminalSessionFactory,
};
use crate::theme::{ACTIVE_THEME, Color};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, MouseButton, Pixels, PromptButton, PromptLevel,
    Render, ScrollHandle, SharedString, Window, div, px, rgba,
};
use gpui_symbols::{Icon, SymbolWeight};
use spaceterm_ui::{
    ButtonSize, ButtonVariant, ContextMenu, IconButton, Menu, MenuAlignment, MenuLifecycleEvent,
    MenuPlacement, MenuPlacementConfig, MenuSize, Tooltip, WindowDragRegion, WindowDragRegionEvent,
    WindowDragRegionResponse, WindowDragRegionStatus,
};

const WINDOW_BAR_HEIGHT: f32 = TOP_CHROME_HEIGHT;
const WINDOW_BAR_DIVIDER_SIZE: f32 = 1.0;
const WINDOW_ITEM_WIDTH: f32 = 132.0;
const WINDOW_ITEM_MINIMUM_WIDTH: f32 = 84.0;
const WINDOW_ITEM_MAXIMUM_WIDTH: f32 = 160.0;
const WINDOW_ITEM_RIGHT_PADDING: f32 = 6.0;
const WINDOW_CLOSE_ICON_SIZE: f32 = 12.0;
const WINDOW_CONTROL_SIZE: f32 = 28.0;
const WINDOW_CONTROL_INSET: f32 = 4.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowMenuInvocation {
    Explicit,
    Context,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WindowMenuState {
    window_id: WindowId,
    invocation: WindowMenuInvocation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WindowManagerEvent {
    FinalWindowCloseRequested {
        final_window_id: WindowId,
    },
    PresentationChanged,
    ReportedWorkingDirectoryChanged {
        window_id: WindowId,
        pane_id: PaneId,
        path: PathBuf,
    },
    PaneClosed {
        window_id: WindowId,
        pane_id: PaneId,
        promoted_pane_id: PaneId,
        promoted_directory: Option<PathBuf>,
    },
    WindowClosed {
        window_id: WindowId,
        promoted_window_id: WindowId,
        promoted_pane_id: PaneId,
        promoted_directory: Option<PathBuf>,
    },
    DirectoryAvailable {
        identity: WorkspaceDirectoryIdentity,
    },
    DirectoryUnavailable {
        reason: String,
    },
}

pub(crate) struct WindowManager {
    windows: WindowCollection<Entity<PaneHost>>,
    session_factory: WorkspaceTerminalSessionFactory,
    active: bool,
    sidebar_visible: bool,
    sidebar_width: Pixels,
    window_menu: Option<WindowMenuState>,
    parent_focus_blocker: Option<TerminalFocusBlocker>,
    window_selector_pressed: Option<WindowId>,
    operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
    window_drag_status: WindowDragRegionStatus,
    window_bar_scroll_handle: ScrollHandle,
    close_workspace_requested: bool,
}

impl WindowManager {
    fn report_window_error(operation: &str, error: WindowError) {
        eprintln!("failed to {operation} Window: {error}");
    }

    #[cfg(test)]
    pub(crate) fn new(
        session_factory: WorkspaceTerminalSessionFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_operating_system_window_drag_platform(
            session_factory,
            Rc::new(MacosOperatingSystemWindowDragPlatform::default()),
            window,
            cx,
        )
    }

    pub(crate) fn new_with_operating_system_window_drag_platform(
        session_factory: WorkspaceTerminalSessionFactory,
        operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let windows = WindowCollection::new(|window_id| {
            Self::create_pane_host(window_id, session_factory.clone(), window, cx)
        });
        Self {
            windows,
            session_factory,
            active: true,
            sidebar_visible: true,
            sidebar_width: px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            window_menu: None,
            parent_focus_blocker: None,
            window_selector_pressed: None,
            operating_system_window_drag_platform,
            window_drag_status: WindowDragRegionStatus::new(),
            window_bar_scroll_handle: ScrollHandle::new(),
            close_workspace_requested: false,
        }
    }

    fn create_pane_host(
        window_id: WindowId,
        session_factory: WorkspaceTerminalSessionFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<PaneHost> {
        let pane_host = cx.new(|cx| PaneHost::new(window_id, session_factory, window, cx));
        debug_assert_eq!(pane_host.read(cx).window_id(), window_id);
        cx.subscribe_in(
            &pane_host,
            window,
            |manager, _, event: &PaneHostEvent, window, cx| match event {
                PaneHostEvent::CloseWindowRequested { window_id } => {
                    manager.close_window(*window_id, window, cx);
                }
                PaneHostEvent::PresentationChanged { .. } => {
                    cx.emit(WindowManagerEvent::PresentationChanged);
                    cx.notify();
                }
                PaneHostEvent::ReportedWorkingDirectoryChanged {
                    window_id,
                    pane_id,
                    path,
                } => cx.emit(WindowManagerEvent::ReportedWorkingDirectoryChanged {
                    window_id: *window_id,
                    pane_id: *pane_id,
                    path: path.clone(),
                }),
                PaneHostEvent::PaneClosed {
                    window_id,
                    pane_id,
                    promoted_pane_id,
                    promoted_directory,
                } => cx.emit(WindowManagerEvent::PaneClosed {
                    window_id: *window_id,
                    pane_id: *pane_id,
                    promoted_pane_id: *promoted_pane_id,
                    promoted_directory: promoted_directory.clone(),
                }),
                PaneHostEvent::DirectoryAvailable { identity } => {
                    cx.emit(WindowManagerEvent::DirectoryAvailable {
                        identity: *identity,
                    });
                }
                PaneHostEvent::DirectoryUnavailable { reason } => {
                    cx.emit(WindowManagerEvent::DirectoryUnavailable {
                        reason: reason.clone(),
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
        self.sync_terminal_focus_blocker(cx);
    }

    pub(crate) fn close_all(&self, cx: &mut App) {
        for (_, pane_host) in self.windows.iter() {
            pane_host.update(cx, |pane_host, cx| pane_host.close_all(cx));
        }
    }

    pub(crate) fn aggregate_counts(&self, cx: &App) -> (usize, usize) {
        let panes = self
            .windows
            .iter()
            .map(|(_, pane_host)| pane_host.read(cx).pane_count())
            .sum();
        (self.windows.len(), panes)
    }

    pub(crate) fn set_workspace_directory(
        &mut self,
        path: &Path,
        identity: WorkspaceDirectoryIdentity,
        cx: &mut Context<Self>,
    ) {
        self.session_factory
            .set_working_directory(path.to_path_buf(), identity);
        for (_, pane_host) in self.windows.iter() {
            pane_host.update(cx, |pane_host, _| {
                pane_host.set_workspace_directory(path, identity)
            });
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
                .window_drag_status
                .is_active()
                .then_some(TerminalFocusBlocker::TopChrome))
            .or(self
                .window_selector_pressed
                .map(|_| TerminalFocusBlocker::WindowSelector))
            .or(self.window_menu.map(|menu| match menu.invocation {
                WindowMenuInvocation::Explicit => TerminalFocusBlocker::WindowMenu,
                WindowMenuInvocation::Context => TerminalFocusBlocker::ContextMenu,
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
                self.operating_system_window_drag_platform
                    .double_activation_requested(window);
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

    #[cfg(test)]
    pub(crate) fn sidebar_detail(&self, cx: &App) -> SharedString {
        let title = self.windows.active_window().read(cx).window_title();
        if self.windows.len() == 1 {
            return title;
        }
        format!("{title} · {} Windows", self.windows.len()).into()
    }

    #[cfg(test)]
    pub(crate) fn active_pane_host(&self) -> Entity<PaneHost> {
        self.windows.active_window().clone()
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
        self.window_menu = None;
        self.window_selector_pressed = None;
        self.sync_terminal_focus_blocker(cx);
        match self.session_factory.validate_working_directory() {
            Ok(directory) => cx.emit(WindowManagerEvent::DirectoryAvailable {
                identity: directory.identity(),
            }),
            Err(error) => {
                let reason = error.to_string();
                cx.emit(WindowManagerEvent::DirectoryUnavailable {
                    reason: reason.clone(),
                });
                let detail = format!(
                    "Cannot create a Window at {} because {reason}. Restore the directory or use another Workspace.",
                    self.session_factory.working_directory().display()
                );
                drop(window.prompt(
                    PromptLevel::Warning,
                    "Workspace Directory Unavailable",
                    Some(&detail),
                    &[PromptButton::ok("OK")],
                    cx,
                ));
                return;
            }
        }
        let previous_window = self.windows.active_window().clone();
        let session_factory = self.session_factory.clone();
        let result = self.windows.create_window(|window_id| {
            Self::create_pane_host(window_id, session_factory, window, cx)
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
                let Some((promoted_window_id, promoted_host)) = self.windows.iter().next() else {
                    unreachable!("closing one of multiple Windows must leave a promotion candidate")
                };
                let promoted_pane_id = promoted_host.read(cx).root_pane_id();
                let promoted_directory = promoted_host
                    .read(cx)
                    .reported_working_directory(promoted_pane_id, cx);
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
                cx.emit(WindowManagerEvent::PresentationChanged);
                cx.emit(WindowManagerEvent::WindowClosed {
                    window_id,
                    promoted_window_id,
                    promoted_pane_id,
                    promoted_directory,
                });
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

    fn prepare_context_menu(
        &mut self,
        window_id: WindowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.window_menu = Some(WindowMenuState {
            window_id,
            invocation: WindowMenuInvocation::Context,
        });
        self.sync_terminal_focus_blocker(cx);
        if !self.activate_window_preserving_menu(window_id, window, cx) {
            self.window_menu = None;
            self.sync_terminal_focus_blocker(cx);
            return false;
        }
        cx.notify();
        true
    }

    fn handle_menu_lifecycle(
        &mut self,
        window_id: WindowId,
        invocation: WindowMenuInvocation,
        event: MenuLifecycleEvent,
        cx: &mut Context<Self>,
    ) {
        let owner = WindowMenuState {
            window_id,
            invocation,
        };
        match event {
            MenuLifecycleEvent::Opened => self.window_menu = Some(owner),
            MenuLifecycleEvent::Closed(_) => {
                if self.window_menu != Some(owner) {
                    return;
                }
                self.window_menu = None;
            }
        }
        self.sync_terminal_focus_blocker(cx);
        cx.notify();
    }

    fn perform_menu_command(
        &mut self,
        command: PaneActionMenuCommand,
        window_id: WindowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let invocation = self
            .window_menu
            .filter(|menu| menu.window_id == window_id)
            .map_or(WindowMenuInvocation::Explicit, |menu| menu.invocation);
        self.window_menu = Some(WindowMenuState {
            window_id,
            invocation,
        });
        self.sync_terminal_focus_blocker(cx);

        let Some(pane_host) = self.windows.window(window_id).cloned() else {
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
            PaneActionMenuCommand::Close => self.close_window(window_id, window, cx),
        }
        if self.window_menu.take().is_some() {
            self.sync_terminal_focus_blocker(cx);
        }
        cx.notify();
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
        cx: &App,
    ) -> AnyElement {
        let press_manager = manager.clone();
        let release_manager = manager.clone();
        let click_manager = manager.clone();
        let context_open_manager = manager.clone();
        let context_activation_manager = manager.clone();
        let context_lifecycle_manager = manager.clone();
        let close_manager = manager;
        let window_group = format!("window-item-{}", window_id.get());
        let (zoomed, zoom_enabled) = self
            .windows
            .window(window_id)
            .map(|pane_host| {
                let pane_host = pane_host.read(cx);
                (
                    matches!(pane_host.zoom_state(), ZoomState::Zoomed(_)),
                    pane_host.pane_count() > 1,
                )
            })
            .unwrap_or((false, false));
        let item = div()
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
                    .ml(px(4.0))
                    .flex_shrink_0()
                    .when(!active, |button| {
                        button
                            .opacity(0.0)
                            .group_hover(window_group, |button| button.opacity(1.0))
                    })
                    .child(
                        IconButton::new(
                            ("window-close-button", window_id.get()),
                            "Close Window",
                            |foreground| {
                                Icon::new("xmark")
                                    .weight(SymbolWeight::Medium)
                                    .size(px(WINDOW_CLOSE_ICON_SIZE))
                                    .color(foreground)
                                    .into_any_element()
                            },
                        )
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Compact)
                        .debug_selector(format!("window-close-button-{}", window_id.get()))
                        .tooltip(
                            Tooltip::new(("window-close-tooltip", window_id.get()), "Close Window")
                                .debug_selector(format!(
                                    "window-close-tooltip-{}",
                                    window_id.get()
                                )),
                        )
                        .on_activate(move |_, window, cx| {
                            let _ = close_manager.update(cx, |manager, cx| {
                                manager.close_window(window_id, window, cx);
                            });
                        }),
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
            });

        ContextMenu::new(
            ("window-context-menu", window_id.get()),
            "Window Actions",
            div()
                .w(px(WINDOW_ITEM_WIDTH))
                .h(px(WINDOW_BAR_HEIGHT))
                .flex_none()
                .child(item),
            pane_action_menu_entries("window-menu", zoomed, zoom_enabled, CloseTarget::Window),
        )
        .size(MenuSize::Wide)
        .placement(
            MenuPlacementConfig::new(MenuPlacement::Bottom, MenuAlignment::Start).offset(px(0.0)),
        )
        .on_open_request(move |_, window, cx| {
            context_open_manager
                .update(cx, |manager, cx| {
                    manager.prepare_context_menu(window_id, window, cx)
                })
                .unwrap_or(false)
        })
        .on_activate(move |activation, window, cx| {
            let command = *activation.action();
            let _ = context_activation_manager.update(cx, |manager, cx| {
                manager.perform_menu_command(command, window_id, window, cx);
            });
        })
        .on_lifecycle(move |event, cx| {
            let event = *event;
            let _ = context_lifecycle_manager.update(cx, |manager, cx| {
                manager.handle_menu_lifecycle(window_id, WindowMenuInvocation::Context, event, cx);
            });
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
            .track_scroll(&self.window_bar_scroll_handle);
        for (window_id, pane_host) in self.windows.iter() {
            items = items.child(self.render_window_item(
                window_id,
                pane_host.read(cx).window_title(),
                window_id == active_window_id,
                manager.clone(),
                cx,
            ));
        }

        let drag_manager = manager.clone();
        let create_manager = manager.clone();
        let menu_activation_manager = manager.clone();
        let menu_lifecycle_manager = manager;
        let (zoomed, zoom_enabled) = self
            .windows
            .window(active_window_id)
            .map(|pane_host| {
                let pane_host = pane_host.read(cx);
                (
                    matches!(pane_host.zoom_state(), ZoomState::Zoomed(_)),
                    pane_host.pane_count() > 1,
                )
            })
            .unwrap_or((false, false));
        let content = div()
            .relative()
            .size_full()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .pr(px(WINDOW_CONTROL_SIZE + WINDOW_CONTROL_INSET * 2.0))
            .bg(gpui_color(ACTIVE_THEME.tab_bar_background))
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
                IconButton::new("create-window-button", "Create Window", |foreground| {
                    Icon::new("plus")
                        .size(px(14.0))
                        .color(foreground)
                        .into_any_element()
                })
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Regular)
                .debug_selector("create-window-button")
                .tooltip(
                    Tooltip::new("create-window-tooltip", "Create Window")
                        .keyboard_equivalent("⌘T")
                        .debug_selector("create-window-tooltip"),
                )
                .on_activate(move |_, window, cx| {
                    let _ = create_manager.update(cx, |manager, cx| {
                        manager.create_window(window, cx);
                    });
                }),
            )
            .child(
                div()
                    .absolute()
                    .top(px(WINDOW_CONTROL_INSET))
                    .right(px(WINDOW_CONTROL_INSET))
                    .child(
                        Menu::new(
                            "window-menu-button-control",
                            "Window Actions",
                            pane_action_menu_entries(
                                "window-menu",
                                zoomed,
                                zoom_enabled,
                                CloseTarget::Window,
                            ),
                        )
                        .icon_trigger(sf_symbol("ellipsis"))
                        .size(MenuSize::Wide)
                        .placement(
                            MenuPlacementConfig::new(MenuPlacement::Bottom, MenuAlignment::End)
                                .offset(px(0.0)),
                        )
                        .debug_selector("window-menu-button")
                        .on_activate(move |activation, window, cx| {
                            let command = *activation.action();
                            let _ = menu_activation_manager.update(cx, |manager, cx| {
                                manager.perform_menu_command(command, active_window_id, window, cx);
                            });
                        })
                        .on_lifecycle(move |event, cx| {
                            let event = *event;
                            let _ = menu_lifecycle_manager.update(cx, |manager, cx| {
                                manager.handle_menu_lifecycle(
                                    active_window_id,
                                    WindowMenuInvocation::Explicit,
                                    event,
                                    cx,
                                );
                            });
                        }),
                    ),
            );

        let drag_region = WindowDragRegion::new(
            "window-bar-drag-region",
            "Move Operating-System Window from Window chrome",
            content,
        )
        .status(self.window_drag_status.clone())
        .debug_selector("window-bar-drag-region")
        .on_event(move |event, window, cx| {
            let event = *event;
            drag_manager
                .update(cx, |manager, cx| {
                    manager.handle_operating_system_window_drag_event(event, window, cx)
                })
                .unwrap_or_default()
        });

        div()
            .id("window-bar")
            .debug_selector(|| "window-bar".to_owned())
            .relative()
            .h(px(WINDOW_BAR_HEIGHT))
            .min_w_0()
            .flex_1()
            .flex_shrink_0()
            .bg(gpui_color(ACTIVE_THEME.tab_bar_background))
            .child(drag_region)
            .into_any_element()
    }
}

impl Render for WindowManager {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        debug_assert!(self.windows.len() > 0);
        let manager = cx.entity().downgrade();
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
    }
}

impl EventEmitter<WindowManagerEvent> for WindowManager {}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
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
    use crate::domain::PaneId;
    use crate::platform::macos_window_drag::RecordingOperatingSystemWindowDragPlatform;
    use crate::terminal::testing::{
        RecordedSessionCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
    };
    use crate::terminal::{ScreenSnapshot, SessionEvent, SessionExit, TerminalSessionFactory};

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
        let (manager, cx) =
            cx.add_window_view(|window, cx| WindowManager::new(session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            manager.update(cx, |manager, cx| manager.focus(window, cx));
        });
        cx.run_until_parked();
        (manager, records, cx)
    }

    fn window_manager_with_operating_system_window_drag_platform(
        cx: &mut TestAppContext,
    ) -> (
        Entity<WindowManager>,
        Rc<RecordingOperatingSystemWindowDragPlatform>,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records));
        let session_factory = WorkspaceTerminalSessionFactory::new(
            session_factory,
            PathBuf::from("/tmp/spaceterm-window-manager-drag-test"),
        );
        let platform = Rc::new(RecordingOperatingSystemWindowDragPlatform::default());
        let injected_platform = Rc::clone(&platform);
        let (manager, cx) = cx.add_window_view(move |window, cx| {
            WindowManager::new_with_operating_system_window_drag_platform(
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
                gpui::size(px(20.0), px(20.0)),
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
                    .map(|menu| (menu.window_id, menu.invocation)),
            )
        });
        assert_eq!(
            state,
            (
                WindowId::new(1),
                Some((WindowId::new(1), WindowMenuInvocation::Context))
            )
        );
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
            ((WindowId::new(1), false, false), vec![(2, false)])
        );
    }

    #[gpui::test]
    fn top_ellipsis_should_target_the_active_window(cx: &mut TestAppContext) {
        let (manager, _records, cx) = window_manager(cx);
        click("create-window-button", cx);

        click("window-menu-button", cx);

        let menu = manager.read_with(cx, |manager, _| manager.window_menu);
        assert_eq!(
            menu,
            Some(WindowMenuState {
                window_id: WindowId::new(2),
                invocation: WindowMenuInvocation::Explicit,
            })
        );
    }

    #[gpui::test]
    fn window_menu_keeps_services_blocked_until_terminal_focus_is_restored(
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
        let trigger_focused = cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.native_service_status(WorkspaceId::new(1), window, cx)
            })
        });
        let pane_host = manager.read_with(cx, |manager, _| manager.windows.active_window().clone());
        cx.update(|window, cx| {
            pane_host.update(cx, |pane_host, cx| pane_host.focus(window, cx));
        });
        let restored = cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.native_service_status(WorkspaceId::new(1), window, cx)
            })
        });

        assert!(before.capabilities.return_text);
        assert!(!blocked.capabilities.return_text);
        assert!(!trigger_focused.capabilities.return_text);
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
    fn window_menu_outside_press_should_preempt_the_background_drag_region(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = window_manager(cx);
        click("window-menu-button", cx);
        let chrome = cx
            .debug_bounds("window-bar")
            .expect("Window chrome was not rendered")
            .center();

        cx.simulate_click(chrome, Modifiers::none());
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.window_menu,
                manager.window_drag_status.is_active(),
                manager.terminal_focus_blocker(),
            )
        });
        assert_eq!(state, (None, false, None));
    }

    #[gpui::test]
    fn window_menu_should_restore_its_trigger_without_changing_the_focused_pane(
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
        assert_eq!(menu_open, (focused_pane_id, false, false));

        click("window-menu-button", cx);
        cx.simulate_keystrokes("a");

        let trigger_commands = records
            .commands()
            .into_iter()
            .skip(command_count)
            .map(|call| (call.session_id, call.command))
            .collect::<Vec<_>>();
        assert!(matches!(
            trigger_commands.as_slice(),
            [(1, RecordedSessionCommand::Focus(false))]
        ));

        let pane_host = manager.read_with(cx, |manager, _| manager.windows.active_window().clone());
        cx.update(|window, cx| {
            pane_host.update(cx, |pane_host, cx| pane_host.focus(window, cx));
        });
        cx.simulate_keystrokes("a");
        let commands = records
            .commands()
            .into_iter()
            .skip(command_count)
            .map(|call| (call.session_id, call.command))
            .collect::<Vec<_>>();
        assert!(matches!(
            commands[1],
            (1, RecordedSessionCommand::Focus(true))
        ));
        assert!(matches!(commands[2], (1, RecordedSessionCommand::Key(_))));
    }

    #[gpui::test]
    fn window_chrome_should_forward_threshold_crossing_and_double_activation_to_platform_policy(
        cx: &mut TestAppContext,
    ) {
        let (_manager, platform, cx) =
            window_manager_with_operating_system_window_drag_platform(cx);
        let chrome = cx
            .debug_bounds("window-bar-drag-region")
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

        assert_eq!(platform.counts(), (1, 1, 1, 1));
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
    fn target_focus_change_should_dismiss_menu_and_refresh_zoom_when_reopened(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = window_manager(cx);
        click("window-menu-button", cx);

        let pane_host = manager.read_with(cx, |manager, _| manager.windows.active_window().clone());
        cx.update(|window, cx| {
            pane_host.update(cx, |pane_host, cx| {
                pane_host.split_focused(SplitAxis::Horizontal, window, cx);
            });
        });
        cx.run_until_parked();
        assert!(manager.read_with(cx, |manager, _| manager.window_menu.is_none()));

        click("window-menu-button", cx);
        click("window-menu-row-toggle-zoom", cx);

        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.window_menu.is_none(),
                manager.windows.active_window().read(cx).zoom_state(),
                manager.windows.active_window().read(cx).pane_count(),
            )
        });
        assert!(
            matches!(state, (true, ZoomState::Zoomed(_), 2)),
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
    fn window_context_menu_should_stay_inside_the_operating_system_window(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = window_manager(cx);
        right_click("window-item-1-active", cx);

        let row = cx
            .debug_bounds("window-menu-row-split-right")
            .expect("the Window menu was not rendered");
        let root = cx
            .debug_bounds("window-manager")
            .expect("the Window manager was not rendered");

        assert!(row.origin.x >= root.origin.x);
        assert!(row.origin.y >= root.origin.y);
        assert!(row.right() <= root.right());
        assert!(row.bottom() <= root.bottom());
    }
}
