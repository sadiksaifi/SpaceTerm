use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Action, AnyElement, App, Bounds, Context, DispatchPhase, DragMoveEvent, Empty, Entity,
    FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Render, ScrollHandle, ScrollWheelEvent, SharedString, Task, WeakEntity, Window, canvas, div,
    point, px, rgba,
};
use gpui_symbols::{Icon, SymbolWeight};

use super::{
    ActivateWindow1, ActivateWindow2, ActivateWindow3, ActivateWindow4, ActivateWindow5,
    ActivateWindow6, ActivateWindow7, ActivateWindow8, ActivateWindow9, ActivateWorkspace1,
    ActivateWorkspace2, ActivateWorkspace3, ActivateWorkspace4, ActivateWorkspace5,
    ActivateWorkspace6, ActivateWorkspace7, ActivateWorkspace8, ActivateWorkspace9, ClosePane,
    CloseWindow, CreateWindow, CreateWorkspace, FocusPaneDown, FocusPaneLeft, FocusPaneRight,
    FocusPaneUp, SplitDown, SplitRight, TERMINAL_KEY_CONTEXT, TOP_CHROME_HEIGHT, TogglePaneZoom,
    ToggleSidebar, ToggleSidebarFocus, WORKSPACE_SIDEBAR_DEFAULT_WIDTH,
    WORKSPACE_SIDEBAR_MINIMUM_WIDTH, WindowManager, WindowManagerEvent,
    handle_top_chrome_mouse_down,
};
use crate::domain::{
    CloseWorkspaceOutcome, NewWorkspace, WorkspaceCollection, WorkspaceError, WorkspaceId,
};
use crate::terminal::TerminalSessionFactory;
use crate::theme::{ACTIVE_THEME, Color};

const SIDEBAR_TOGGLE_INSET: f32 = 4.0;
const SIDEBAR_TOGGLE_SIZE: f32 = 28.0;
const SIDEBAR_ROW_HEIGHT: f32 = 58.0;
const SIDEBAR_ROW_HORIZONTAL_PADDING: f32 = 12.0;
const SIDEBAR_ROW_ICON_SIZE: f32 = 14.0;
const SIDEBAR_NAME_TEXT_SIZE: f32 = 13.0;
const SIDEBAR_DETAIL_TEXT_SIZE: f32 = 11.0;
const NEW_WORKSPACE_BUTTON_HEIGHT: f32 = 40.0;
const CHROME_DIVIDER_SIZE: f32 = 1.0;
const SIDEBAR_RESIZE_HIT_SIZE: f32 = 8.0;
const SIDEBAR_MAXIMUM_WIDTH: f32 = 420.0;
const TERMINAL_CONTENT_MINIMUM_WIDTH: f32 = 240.0;
const SIDEBAR_SCROLLBAR_WIDTH: f32 = 5.0;
const SIDEBAR_SCROLLBAR_HORIZONTAL_HITBOX_PADDING: f32 = 4.0;
const SIDEBAR_SCROLLBAR_HITBOX_WIDTH: f32 =
    SIDEBAR_SCROLLBAR_WIDTH + SIDEBAR_SCROLLBAR_HORIZONTAL_HITBOX_PADDING * 2.0;
const SIDEBAR_SCROLLBAR_RIGHT_INSET: f32 = 4.0;
const SIDEBAR_SCROLLBAR_MINIMUM_THUMB_HEIGHT: f32 = 24.0;
const SIDEBAR_SCROLLBAR_HIDE_DELAY: Duration = Duration::from_secs(2);
const WORKSPACE_MENU_WIDTH: f32 = 208.0;
const WORKSPACE_MENU_ROW_HEIGHT: f32 = 28.0;
const WORKSPACE_MENU_SEPARATOR_SIZE: f32 = 1.0;
const WORKSPACE_MENU_BORDER_SIZE: f32 = 1.0;
const WORKSPACE_MENU_HEIGHT: f32 = WORKSPACE_MENU_ROW_HEIGHT * 3.0
    + WORKSPACE_MENU_SEPARATOR_SIZE
    + WORKSPACE_MENU_BORDER_SIZE * 2.0;
const WORKSPACE_MENU_INSET: f32 = 4.0;
const WORKSPACE_MENU_CORNER_RADIUS: f32 = 8.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspaceMenuCommand {
    NewWindow,
    Rename,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WorkspaceMenuState {
    workspace_id: WorkspaceId,
    top: Pixels,
}

#[derive(Debug, Eq, PartialEq)]
struct WorkspaceRenameState {
    workspace_id: WorkspaceId,
    draft: String,
    replace_on_first_input: bool,
}

#[derive(Clone, Copy)]
struct DraggedWorkspaceSidebar;

#[derive(Clone, Copy, Debug, PartialEq)]
struct WorkspaceScrollbarGeometry {
    top_px: f32,
    height_px: f32,
    track_height_px: f32,
}

#[derive(Clone, Copy, Debug)]
struct WorkspaceScrollbarDrag {
    grab_ratio: f32,
    track_top_px: f32,
    track_height_px: f32,
}

pub(crate) struct WorkspaceManager {
    workspaces: WorkspaceCollection<Entity<WindowManager>>,
    session_factory: Rc<dyn TerminalSessionFactory>,
    home_directory: PathBuf,
    next_workspace_id: u64,
    sidebar_visible: bool,
    sidebar_width: Pixels,
    workspace_list_scroll_handle: ScrollHandle,
    workspace_scrollbar_visible: bool,
    workspace_scrollbar_hovered: bool,
    workspace_scrollbar_drag: Option<WorkspaceScrollbarDrag>,
    workspace_scrollbar_visibility_generation: u64,
    sidebar_focus: FocusHandle,
    rename_focus: FocusHandle,
    workspace_menu: Option<WorkspaceMenuState>,
    rename: Option<WorkspaceRenameState>,
    _workspace_scrollbar_hide_task: Option<Task<()>>,
}

impl WorkspaceManager {
    pub(crate) fn new(
        session_factory: Rc<dyn TerminalSessionFactory>,
        home_directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_workspace_id = WorkspaceId::new(1);
        let initial_window_manager = Self::create_window_manager(
            initial_workspace_id,
            Rc::clone(&session_factory),
            home_directory.clone(),
            true,
            px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            window,
            cx,
        );

        Self {
            workspaces: WorkspaceCollection::new(
                initial_workspace_id,
                default_workspace_name(1),
                home_directory.clone(),
                initial_window_manager,
            ),
            session_factory,
            home_directory,
            next_workspace_id: 2,
            sidebar_visible: true,
            sidebar_width: px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            workspace_list_scroll_handle: ScrollHandle::new(),
            workspace_scrollbar_visible: false,
            workspace_scrollbar_hovered: false,
            workspace_scrollbar_drag: None,
            workspace_scrollbar_visibility_generation: 0,
            sidebar_focus: cx.focus_handle(),
            rename_focus: cx.focus_handle(),
            workspace_menu: None,
            rename: None,
            _workspace_scrollbar_hide_task: None,
        }
    }

    fn create_window_manager(
        workspace_id: WorkspaceId,
        session_factory: Rc<dyn TerminalSessionFactory>,
        workspace_root: PathBuf,
        sidebar_visible: bool,
        sidebar_width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<WindowManager> {
        let manager = cx.new(|cx| {
            let mut manager = WindowManager::new(session_factory, workspace_root, window, cx);
            manager.set_sidebar_layout(sidebar_visible, sidebar_width, cx);
            manager
        });
        cx.subscribe_in(
            &manager,
            window,
            move |_workspace_manager, _, event: &WindowManagerEvent, window, cx| match event {
                WindowManagerEvent::CloseWorkspaceRequested => {
                    cx.defer_in(window, move |workspace_manager, window, cx| {
                        workspace_manager.close_workspace(workspace_id, window, cx);
                    });
                }
                WindowManagerEvent::PresentationChanged => cx.notify(),
            },
        )
        .detach();
        manager
    }

    fn report_workspace_error(operation: &str, error: WorkspaceError) {
        eprintln!("failed to {operation} Workspace: {error}");
    }

    fn allocate_workspace_id(&mut self) -> Option<WorkspaceId> {
        let workspace_id = WorkspaceId::new(self.next_workspace_id);
        self.next_workspace_id = self.next_workspace_id.checked_add(1)?;
        Some(workspace_id)
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        let manager = self.workspaces.active_workspace().payload().clone();
        manager.update(cx, |manager, cx| manager.focus(window, cx));
    }

    fn set_sidebar_layout(&mut self, visible: bool, width: Pixels, cx: &mut Context<Self>) {
        if self.sidebar_visible == visible && self.sidebar_width == width {
            return;
        }
        self.sidebar_visible = visible;
        self.sidebar_width = width;
        if !visible {
            self.workspace_scrollbar_visible = false;
            self.workspace_scrollbar_hovered = false;
            self.workspace_scrollbar_drag = None;
            self._workspace_scrollbar_hide_task.take();
        }
        for workspace in self.workspaces.iter() {
            workspace.payload().update(cx, |manager, cx| {
                manager.set_sidebar_layout(visible, width, cx);
            });
        }
        cx.notify();
    }

    fn resize_sidebar(&mut self, pointer_x: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let minimum_width = px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH);
        if pointer_x < minimum_width {
            let was_sidebar_focused =
                self.sidebar_focus.is_focused(window) || self.rename_focus.is_focused(window);
            self.workspace_menu = None;
            self.rename = None;
            self.set_sidebar_layout(false, minimum_width, cx);
            if was_sidebar_focused {
                self.focus(window, cx);
            }
            return;
        }

        let maximum_width = (window.bounds().size.width - px(TERMINAL_CONTENT_MINIMUM_WIDTH))
            .min(px(SIDEBAR_MAXIMUM_WIDTH))
            .max(minimum_width);
        self.set_sidebar_layout(true, pointer_x.clamp(minimum_width, maximum_width), cx);
    }

    fn scroll_active_workspace_into_view(&self) {
        let active_workspace_id = self.workspaces.active_workspace_id();
        if let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id() == active_workspace_id)
        {
            self.workspace_list_scroll_handle.scroll_to_item(index);
        }
    }

    fn current_workspace_scrollbar_geometry(&self) -> Option<WorkspaceScrollbarGeometry> {
        let track_height_px = f32::from(self.workspace_list_scroll_handle.bounds().size.height);
        let maximum_offset_px = f32::from(self.workspace_list_scroll_handle.max_offset().height);
        let offset_px = -f32::from(self.workspace_list_scroll_handle.offset().y);
        workspace_scrollbar_geometry(track_height_px, maximum_offset_px, offset_px)
    }

    fn reveal_workspace_scrollbar(&mut self, cx: &mut Context<Self>) {
        self.workspace_scrollbar_visible = true;
        self.workspace_scrollbar_visibility_generation = self
            .workspace_scrollbar_visibility_generation
            .wrapping_add(1);
        let generation = self.workspace_scrollbar_visibility_generation;
        self._workspace_scrollbar_hide_task.take();

        if self.workspace_scrollbar_drag.is_none() && !self.workspace_scrollbar_hovered {
            self._workspace_scrollbar_hide_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(SIDEBAR_SCROLLBAR_HIDE_DELAY)
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.workspace_scrollbar_visibility_generation == generation
                        && this.workspace_scrollbar_drag.is_none()
                        && !this.workspace_scrollbar_hovered
                    {
                        this.workspace_scrollbar_visible = false;
                        cx.notify();
                    }
                });
            }));
        }
        cx.notify();
    }

    fn on_workspace_list_scroll_wheel(
        &mut self,
        _: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace_list_scroll_handle.max_offset().height > px(0.0) {
            self.reveal_workspace_scrollbar(cx);
        }
    }

    fn set_workspace_scrollbar_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if self.workspace_scrollbar_hovered == hovered {
            return;
        }

        self.workspace_scrollbar_hovered = hovered;
        self.workspace_scrollbar_visibility_generation = self
            .workspace_scrollbar_visibility_generation
            .wrapping_add(1);
        self._workspace_scrollbar_hide_task.take();
        if hovered {
            self.workspace_scrollbar_visible = true;
            cx.notify();
        } else {
            self.reveal_workspace_scrollbar(cx);
        }
    }

    fn begin_workspace_scrollbar_drag(
        &mut self,
        pointer_y: Pixels,
        thumb_bounds: Bounds<Pixels>,
        geometry: WorkspaceScrollbarGeometry,
        cx: &mut Context<Self>,
    ) {
        self.workspace_scrollbar_visibility_generation = self
            .workspace_scrollbar_visibility_generation
            .wrapping_add(1);
        self._workspace_scrollbar_hide_task.take();
        self.workspace_scrollbar_visible = true;
        self.workspace_scrollbar_drag = Some(WorkspaceScrollbarDrag {
            grab_ratio: (f32::from(pointer_y - thumb_bounds.origin.y) / geometry.height_px)
                .clamp(0.0, 1.0),
            track_top_px: f32::from(thumb_bounds.origin.y) - geometry.top_px,
            track_height_px: geometry.track_height_px,
        });
        cx.notify();
    }

    fn move_workspace_scrollbar_drag(&mut self, pointer_y: Pixels, cx: &mut Context<Self>) -> bool {
        let Some(drag) = self.workspace_scrollbar_drag else {
            return false;
        };
        let pointer_in_track_px = f32::from(pointer_y) - drag.track_top_px;
        let maximum_offset_px = f32::from(self.workspace_list_scroll_handle.max_offset().height);
        let Some(offset_px) = workspace_scrollbar_offset_for_pointer(
            maximum_offset_px,
            drag.track_height_px,
            pointer_in_track_px,
            drag.grab_ratio,
        ) else {
            return true;
        };
        let current_offset = self.workspace_list_scroll_handle.offset();
        let next_offset = point(current_offset.x, px(-offset_px));
        if current_offset == next_offset {
            return true;
        }

        self.workspace_list_scroll_handle.set_offset(next_offset);
        cx.notify();
        true
    }

    fn finish_workspace_scrollbar_drag(&mut self, cx: &mut Context<Self>) -> bool {
        if self.workspace_scrollbar_drag.take().is_some() {
            self.reveal_workspace_scrollbar(cx);
            true
        } else {
            false
        }
    }

    fn create_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace_id) = self.allocate_workspace_id() else {
            eprintln!("cannot create Workspace because the Workspace ID space is exhausted");
            return;
        };
        let workspace_name = next_default_workspace_name(&self.workspaces, None);
        let previous_manager = self.workspaces.active_workspace().payload().clone();
        let next_manager = Self::create_window_manager(
            workspace_id,
            Rc::clone(&self.session_factory),
            self.home_directory.clone(),
            self.sidebar_visible,
            self.sidebar_width,
            window,
            cx,
        );
        let result = self.workspaces.create_workspace(
            workspace_id,
            workspace_name,
            self.home_directory.clone(),
            || next_manager.clone(),
        );
        if let Err(error) = result {
            next_manager.update(cx, |manager, cx| manager.close_all(cx));
            Self::report_workspace_error("create", error);
            return;
        }

        previous_manager.update(cx, |manager, cx| manager.deactivate(cx));
        next_manager.update(cx, |manager, cx| manager.activate(window, cx));
        self.workspace_menu = None;
        self.rename = None;
        self.scroll_active_workspace_into_view();
        cx.notify();
    }

    fn activate_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(next_manager) = self
            .workspaces
            .workspace(workspace_id)
            .map(|workspace| workspace.payload().clone())
        else {
            eprintln!("cannot activate unknown Workspace {workspace_id}");
            return false;
        };
        let previous_workspace_id = self.workspaces.active_workspace_id();
        let previous_manager = self.workspaces.active_workspace().payload().clone();
        if let Err(error) = self.workspaces.activate_workspace(workspace_id) {
            Self::report_workspace_error("activate", error);
            return false;
        }

        let preserve_sidebar_focus =
            self.sidebar_focus.is_focused(window) || self.rename_focus.is_focused(window);
        if previous_workspace_id != workspace_id {
            previous_manager.update(cx, |manager, cx| manager.deactivate(cx));
            self.rename = None;
        }
        if preserve_sidebar_focus {
            next_manager.update(cx, |manager, cx| manager.activate_without_focus(cx));
        } else {
            next_manager.update(cx, |manager, cx| manager.activate(window, cx));
        }
        self.workspace_menu = None;
        self.scroll_active_workspace_into_view();
        cx.notify();
        true
    }

    fn activate_workspace_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let workspace_id = self
            .workspaces
            .iter()
            .nth(index)
            .map(|workspace| workspace.id());
        if let Some(workspace_id) = workspace_id {
            self.activate_workspace(workspace_id, window, cx);
        }
    }

    fn close_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_active = self.workspaces.active_workspace_id() == workspace_id;
        let is_final = self.workspaces.len() == 1;
        let replacement_workspace_id = if is_final {
            let Some(workspace_id) = self.allocate_workspace_id() else {
                eprintln!("cannot close Workspace because the Workspace ID space is exhausted");
                return;
            };
            workspace_id
        } else {
            WorkspaceId::new(0)
        };
        let mut replacement = is_final.then(|| {
            let name = next_default_workspace_name(&self.workspaces, Some(workspace_id));
            let manager = Self::create_window_manager(
                replacement_workspace_id,
                Rc::clone(&self.session_factory),
                self.home_directory.clone(),
                self.sidebar_visible,
                self.sidebar_width,
                window,
                cx,
            );
            (name, self.home_directory.clone(), manager)
        });

        let outcome =
            self.workspaces
                .close_workspace(workspace_id, replacement_workspace_id, || {
                    let Some((name, working_directory, manager)) = replacement.take() else {
                        unreachable!("the final Workspace replacement is prepared before closing")
                    };
                    NewWorkspace::new(name, working_directory, manager)
                });
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                if let Some((_, _, manager)) = replacement {
                    manager.update(cx, |manager, cx| manager.close_all(cx));
                }
                Self::report_workspace_error("close", error);
                return;
            }
        };

        let closed_manager = match outcome {
            CloseWorkspaceOutcome::WorkspaceClosed { payload, .. }
            | CloseWorkspaceOutcome::FinalWorkspaceReplaced { payload, .. } => payload,
        };
        closed_manager.update(cx, |manager, cx| manager.close_all(cx));

        if was_active {
            let active_manager = self.workspaces.active_workspace().payload().clone();
            if self.sidebar_focus.is_focused(window) || self.rename_focus.is_focused(window) {
                active_manager.update(cx, |manager, cx| manager.activate_without_focus(cx));
            } else {
                active_manager.update(cx, |manager, cx| manager.activate(window, cx));
            }
        }
        self.workspace_menu = None;
        if self
            .rename
            .as_ref()
            .is_some_and(|rename| rename.workspace_id == workspace_id)
        {
            self.rename = None;
        }
        self.scroll_active_workspace_into_view();
        cx.notify();
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let was_sidebar_focused =
            self.sidebar_focus.is_focused(window) || self.rename_focus.is_focused(window);
        let sidebar_visible = !self.sidebar_visible;
        self.workspace_menu = None;
        self.rename = None;
        self.set_sidebar_layout(sidebar_visible, self.sidebar_width, cx);
        if !sidebar_visible && was_sidebar_focused {
            self.focus(window, cx);
        }
    }

    fn toggle_sidebar_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_focus.is_focused(window) || self.rename_focus.is_focused(window) {
            self.rename = None;
            self.focus(window, cx);
            cx.notify();
            return;
        }

        if !self.sidebar_visible {
            self.set_sidebar_layout(true, self.sidebar_width, cx);
            cx.defer_in(window, |manager, window, _| {
                manager.sidebar_focus.focus(window);
            });
            return;
        }

        self.sidebar_focus.focus(window);
        cx.notify();
    }

    fn open_workspace_menu(
        &mut self,
        workspace_id: WorkspaceId,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_focus.focus(window);
        if !self.activate_workspace(workspace_id, window, cx) {
            return;
        }
        let maximum_top = (window.bounds().size.height
            - px(WORKSPACE_MENU_HEIGHT + WORKSPACE_MENU_INSET))
        .max(px(TOP_CHROME_HEIGHT + WORKSPACE_MENU_INSET));
        self.workspace_menu = Some(WorkspaceMenuState {
            workspace_id,
            top: event
                .position
                .y
                .clamp(px(TOP_CHROME_HEIGHT + WORKSPACE_MENU_INSET), maximum_top),
        });
        self.rename = None;
        cx.notify();
    }

    fn perform_workspace_menu_command(
        &mut self,
        command: WorkspaceMenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.workspace_menu.take() else {
            return;
        };
        match command {
            WorkspaceMenuCommand::NewWindow => {
                if self.activate_workspace(menu.workspace_id, window, cx)
                    && let Some(workspace) = self.workspaces.workspace(menu.workspace_id)
                {
                    workspace
                        .payload()
                        .update(cx, |manager, cx| manager.create_window(window, cx));
                }
            }
            WorkspaceMenuCommand::Rename => {
                let Some(workspace) = self.workspaces.workspace(menu.workspace_id) else {
                    return;
                };
                self.rename = Some(WorkspaceRenameState {
                    workspace_id: menu.workspace_id,
                    draft: workspace.name().to_owned(),
                    replace_on_first_input: true,
                });
                cx.notify();
                cx.defer_in(window, |manager, window, _| {
                    manager.rename_focus.focus(window);
                });
            }
            WorkspaceMenuCommand::Close => self.close_workspace(menu.workspace_id, window, cx),
        }
    }

    fn on_rename_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.rename_focus.is_focused(window) {
            return;
        }
        let Some(rename) = self.rename.as_mut() else {
            return;
        };
        match event.keystroke.key.as_str() {
            "enter" => {
                let name = rename.draft.trim();
                if !name.is_empty() {
                    let workspace_id = rename.workspace_id;
                    if let Err(error) = self
                        .workspaces
                        .rename_workspace(workspace_id, name.to_owned())
                    {
                        Self::report_workspace_error("rename", error);
                    }
                }
                self.rename = None;
                self.sidebar_focus.focus(window);
            }
            "escape" => {
                self.rename = None;
                self.sidebar_focus.focus(window);
            }
            "backspace" => {
                if rename.replace_on_first_input {
                    rename.draft.clear();
                    rename.replace_on_first_input = false;
                } else {
                    rename.draft.pop();
                }
            }
            _ if !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform =>
            {
                if let Some(text) = &event.keystroke.key_char {
                    if rename.replace_on_first_input {
                        rename.draft.clear();
                        rename.replace_on_first_input = false;
                    }
                    rename.draft.extend(text.chars().filter(|character| {
                        !character.is_control() && *character != '\n' && *character != '\r'
                    }));
                }
            }
            _ => {}
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn on_create_workspace(
        &mut self,
        _: &CreateWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.create_workspace(window, cx);
    }

    fn on_activate_workspace_1(
        &mut self,
        _: &ActivateWorkspace1,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_workspace_at(0, window, cx);
    }

    fn on_activate_workspace_2(
        &mut self,
        _: &ActivateWorkspace2,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_workspace_at(1, window, cx);
    }

    fn on_activate_workspace_3(
        &mut self,
        _: &ActivateWorkspace3,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_workspace_at(2, window, cx);
    }

    fn on_activate_workspace_4(
        &mut self,
        _: &ActivateWorkspace4,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_workspace_at(3, window, cx);
    }

    fn on_activate_workspace_5(
        &mut self,
        _: &ActivateWorkspace5,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_workspace_at(4, window, cx);
    }

    fn on_activate_workspace_6(
        &mut self,
        _: &ActivateWorkspace6,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_workspace_at(5, window, cx);
    }

    fn on_activate_workspace_7(
        &mut self,
        _: &ActivateWorkspace7,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_workspace_at(6, window, cx);
    }

    fn on_activate_workspace_8(
        &mut self,
        _: &ActivateWorkspace8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_workspace_at(7, window, cx);
    }

    fn on_activate_workspace_9(
        &mut self,
        _: &ActivateWorkspace9,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_workspace_at(8, window, cx);
    }

    fn on_toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_sidebar(window, cx);
    }

    fn on_toggle_sidebar_focus(
        &mut self,
        _: &ToggleSidebarFocus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_sidebar_focus(window, cx);
    }

    fn forward_active_terminal_action<A: Action>(
        &mut self,
        action: &A,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus(window, cx);
        window.dispatch_action(action.boxed_clone(), cx);
    }

    fn render_top_left_chrome(&self, manager: WeakEntity<Self>) -> AnyElement {
        let toggle_manager = manager;
        div()
            .id("workspace-top-chrome")
            .debug_selector(|| "workspace-top-chrome".to_owned())
            .absolute()
            .top_0()
            .left_0()
            .w(self.sidebar_width)
            .h(px(TOP_CHROME_HEIGHT))
            .bg(gpui_color(ACTIVE_THEME.tab_bar_background))
            .occlude()
            .on_mouse_down(MouseButton::Left, handle_top_chrome_mouse_down)
            .child(
                div()
                    .id("workspace-top-chrome-right-divider")
                    .debug_selector(|| "workspace-top-chrome-right-divider".to_owned())
                    .absolute()
                    .top_0()
                    .right_0()
                    .h_full()
                    .w(px(CHROME_DIVIDER_SIZE))
                    .bg(gpui_color(ACTIVE_THEME.border)),
            )
            .child(
                div()
                    .id("workspace-top-chrome-bottom-divider")
                    .debug_selector(|| "workspace-top-chrome-bottom-divider".to_owned())
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .w_full()
                    .h(px(CHROME_DIVIDER_SIZE))
                    .bg(gpui_color(ACTIVE_THEME.border)),
            )
            .child(
                div()
                    .id("toggle-sidebar-button")
                    .debug_selector(|| "toggle-sidebar-button".to_owned())
                    .absolute()
                    .top(px(SIDEBAR_TOGGLE_INSET))
                    .right(px(SIDEBAR_TOGGLE_INSET))
                    .size(px(SIDEBAR_TOGGLE_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .occlude()
                    .hover(|button| button.bg(gpui_color(ACTIVE_THEME.ghost_element_selected)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        let _ = toggle_manager.update(cx, |manager, cx| {
                            manager.toggle_sidebar(window, cx);
                        });
                        cx.stop_propagation();
                    })
                    .child(
                        Icon::new("sidebar.left")
                            .size(px(14.0))
                            .color(gpui_color(ACTIVE_THEME.icon)),
                    ),
            )
            .child(
                div()
                    .id("workspace-top-chrome-resize-handle")
                    .debug_selector(|| "workspace-top-chrome-resize-handle".to_owned())
                    .absolute()
                    .top_0()
                    .right(px(-(SIDEBAR_RESIZE_HIT_SIZE - CHROME_DIVIDER_SIZE) / 2.0))
                    .h_full()
                    .w(px(SIDEBAR_RESIZE_HIT_SIZE))
                    .block_mouse_except_scroll()
                    .cursor_col_resize()
                    .on_drag(DraggedWorkspaceSidebar, |_, _, _, cx| cx.new(|_| Empty)),
            )
            .into_any_element()
    }

    fn render_workspace_row(
        &self,
        workspace_id: WorkspaceId,
        name: SharedString,
        detail: SharedString,
        active: bool,
        manager: WeakEntity<Self>,
    ) -> AnyElement {
        let click_manager = manager.clone();
        let rename_manager = manager.clone();
        let context_manager = manager;
        let renaming = self
            .rename
            .as_ref()
            .is_some_and(|rename| rename.workspace_id == workspace_id);
        let first_line = if renaming {
            let draft = self
                .rename
                .as_ref()
                .map(|rename| rename.draft.clone())
                .unwrap_or_default();
            div()
                .id(("workspace-rename-input", workspace_id.get()))
                .debug_selector(move || format!("workspace-rename-input-{}", workspace_id.get()))
                .track_focus(&self.rename_focus)
                .h(px(22.0))
                .w_full()
                .px(px(5.0))
                .flex()
                .items_center()
                .overflow_hidden()
                .rounded(px(4.0))
                .border(px(1.0))
                .border_color(gpui_color(ACTIVE_THEME.panel_focused_border))
                .bg(gpui_color(ACTIVE_THEME.element_background))
                .text_color(gpui_color(ACTIVE_THEME.text))
                .on_click(move |_, window, cx| {
                    let _ = rename_manager.update(cx, |manager, _| {
                        manager.rename_focus.focus(window);
                    });
                    cx.stop_propagation();
                })
                .on_key_down(|_, _, _| {})
                .child(format!("{draft}│"))
                .into_any_element()
        } else {
            div()
                .w_full()
                .truncate()
                .text_size(px(SIDEBAR_NAME_TEXT_SIZE))
                .text_color(gpui_color(if active {
                    ACTIVE_THEME.tab_active_foreground
                } else {
                    ACTIVE_THEME.text
                }))
                .child(name)
                .into_any_element()
        };

        div()
            .id(("workspace-row", workspace_id.get()))
            .debug_selector(move || {
                format!(
                    "workspace-row-{}-{}",
                    workspace_id.get(),
                    if active { "active" } else { "inactive" }
                )
            })
            .relative()
            .w_full()
            .h(px(SIDEBAR_ROW_HEIGHT))
            .flex_shrink_0()
            .px(px(SIDEBAR_ROW_HORIZONTAL_PADDING))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .cursor_pointer()
            .block_mouse_except_scroll()
            .when(active, |row| {
                row.bg(gpui_color(ACTIVE_THEME.element_selected))
            })
            .hover(|row| row.bg(gpui_color(ACTIVE_THEME.ghost_element_selected)))
            .on_click(move |_, window, cx| {
                let _ = click_manager.update(cx, |manager, cx| {
                    manager.sidebar_focus.focus(window);
                    manager.activate_workspace(workspace_id, window, cx);
                });
                cx.stop_propagation();
            })
            .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                let _ = context_manager.update(cx, |manager, cx| {
                    manager.open_workspace_menu(workspace_id, event, window, cx);
                });
                cx.stop_propagation();
            })
            .child(
                div()
                    .w(px(18.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new("terminal").size(px(SIDEBAR_ROW_ICON_SIZE)).color(
                        gpui_color(if active {
                            ACTIVE_THEME.icon_accent
                        } else {
                            ACTIVE_THEME.icon
                        }),
                    )),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(first_line)
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_size(px(SIDEBAR_DETAIL_TEXT_SIZE))
                            .text_color(gpui_color(ACTIVE_THEME.text_muted))
                            .child(detail),
                    ),
            )
            .child(
                div()
                    .id(("workspace-row-divider", workspace_id.get()))
                    .debug_selector(move || format!("workspace-row-divider-{}", workspace_id.get()))
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .w_full()
                    .h(px(CHROME_DIVIDER_SIZE))
                    .bg(gpui_color(ACTIVE_THEME.border)),
            )
            .into_any_element()
    }

    fn render_sidebar(&self, manager: WeakEntity<Self>, cx: &App) -> AnyElement {
        let scroll_manager = manager.clone();
        let mut rows = div()
            .id("workspace-list")
            .debug_selector(|| "workspace-list".to_owned())
            .w_full()
            .min_h_0()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .track_scroll(&self.workspace_list_scroll_handle)
            .on_scroll_wheel(move |event, window, cx| {
                let _ = scroll_manager.update(cx, |manager, cx| {
                    manager.on_workspace_list_scroll_wheel(event, window, cx);
                });
            })
            .occlude();
        let active_workspace_id = self.workspaces.active_workspace_id();
        for workspace in self.workspaces.iter() {
            rows = rows.child(self.render_workspace_row(
                workspace.id(),
                workspace.name().to_owned().into(),
                workspace.payload().read(cx).sidebar_detail(cx),
                workspace.id() == active_workspace_id,
                manager.clone(),
            ));
        }

        let scrollbar_dragging = self.workspace_scrollbar_drag.is_some();
        let scrollbar = self
            .workspace_scrollbar_visible
            .then(|| self.current_workspace_scrollbar_geometry())
            .flatten();
        let create_manager = manager.clone();
        div()
            .id("workspace-sidebar")
            .debug_selector(|| "workspace-sidebar".to_owned())
            .absolute()
            .top(px(TOP_CHROME_HEIGHT))
            .bottom_0()
            .left_0()
            .w(self.sidebar_width)
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .track_focus(&self.sidebar_focus)
            .bg(gpui_color(ACTIVE_THEME.panel_background))
            .occlude()
            .child(rows)
            .child(
                div()
                    .id("create-workspace-button")
                    .debug_selector(|| "create-workspace-button".to_owned())
                    .relative()
                    .w_full()
                    .h(px(NEW_WORKSPACE_BUTTON_HEIGHT))
                    .flex_shrink_0()
                    .px(px(SIDEBAR_ROW_HORIZONTAL_PADDING))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .text_color(gpui_color(ACTIVE_THEME.text_muted))
                    .hover(|button| {
                        button
                            .bg(gpui_color(ACTIVE_THEME.ghost_element_selected))
                            .text_color(gpui_color(ACTIVE_THEME.text))
                    })
                    .on_click(move |_, window, cx| {
                        let _ = create_manager.update(cx, |manager, cx| {
                            manager.create_workspace(window, cx);
                        });
                        cx.stop_propagation();
                    })
                    .child(
                        Icon::new("plus")
                            .size(px(13.0))
                            .color(gpui_color(ACTIVE_THEME.icon)),
                    )
                    .child("New Workspace")
                    .child(div().flex_grow())
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(gpui_color(ACTIVE_THEME.icon))
                            .child("⌘N"),
                    )
                    .child(
                        div()
                            .id("create-workspace-button-top-divider")
                            .debug_selector(|| "create-workspace-button-top-divider".to_owned())
                            .absolute()
                            .top_0()
                            .left_0()
                            .w_full()
                            .h(px(CHROME_DIVIDER_SIZE))
                            .bg(gpui_color(ACTIVE_THEME.border)),
                    ),
            )
            .when_some(scrollbar, move |sidebar, scrollbar| {
                let hover_manager = manager.clone();
                let down_manager = manager.clone();
                let move_manager = manager.clone();
                let up_manager = manager.clone();
                let thumb_color = if scrollbar_dragging {
                    ACTIVE_THEME.icon
                } else {
                    ACTIVE_THEME.scrollbar_thumb_background
                };
                let thumb_hover_color = if scrollbar_dragging {
                    ACTIVE_THEME.icon
                } else {
                    ACTIVE_THEME.icon_accent
                };
                sidebar.child(
                    div()
                        .group("workspace-scrollbar-thumb")
                        .id("workspace-scrollbar-thumb-hitbox")
                        .debug_selector(|| "workspace-scrollbar-thumb-hitbox".to_owned())
                        .absolute()
                        .top(px(scrollbar.top_px))
                        .right_0()
                        .w(px(SIDEBAR_SCROLLBAR_HITBOX_WIDTH))
                        .h(px(scrollbar.height_px))
                        .block_mouse_except_scroll()
                        .cursor_default()
                        .on_hover(move |hovered, _, cx| {
                            let _ = hover_manager.update(cx, |manager, cx| {
                                manager.set_workspace_scrollbar_hovered(*hovered, cx);
                            });
                        })
                        .child(
                            div()
                                .id("workspace-scrollbar-thumb")
                                .debug_selector(|| "workspace-scrollbar-thumb".to_owned())
                                .absolute()
                                .right(px(SIDEBAR_SCROLLBAR_RIGHT_INSET))
                                .w(px(SIDEBAR_SCROLLBAR_WIDTH))
                                .h_full()
                                .rounded(px(SIDEBAR_SCROLLBAR_WIDTH / 2.0))
                                .bg(gpui_color(thumb_color))
                                .group_hover("workspace-scrollbar-thumb", move |thumb| {
                                    thumb.bg(gpui_color(thumb_hover_color))
                                }),
                        )
                        .child(
                            canvas(
                                |_, _, _| (),
                                move |thumb_bounds, _, window, _| {
                                    window.on_mouse_event(
                                        move |event: &MouseDownEvent, phase, window, cx| {
                                            if phase != DispatchPhase::Bubble
                                                || event.button != MouseButton::Left
                                                || !thumb_bounds.contains(&event.position)
                                            {
                                                return;
                                            }
                                            let _ = down_manager.update(cx, |manager, cx| {
                                                manager.sidebar_focus.focus(window);
                                                manager.begin_workspace_scrollbar_drag(
                                                    event.position.y,
                                                    thumb_bounds,
                                                    scrollbar,
                                                    cx,
                                                );
                                            });
                                            cx.stop_propagation();
                                        },
                                    );

                                    window.on_mouse_event(
                                        move |event: &MouseMoveEvent, phase, _, cx| {
                                            if phase != DispatchPhase::Bubble || !event.dragging() {
                                                return;
                                            }
                                            let handled = move_manager
                                                .update(cx, |manager, cx| {
                                                    manager.move_workspace_scrollbar_drag(
                                                        event.position.y,
                                                        cx,
                                                    )
                                                })
                                                .unwrap_or(false);
                                            if handled {
                                                cx.stop_propagation();
                                            }
                                        },
                                    );

                                    window.on_mouse_event(
                                        move |event: &MouseUpEvent, phase, _, cx| {
                                            if phase != DispatchPhase::Bubble
                                                || event.button != MouseButton::Left
                                            {
                                                return;
                                            }
                                            let handled = up_manager
                                                .update(cx, |manager, cx| {
                                                    manager.finish_workspace_scrollbar_drag(cx)
                                                })
                                                .unwrap_or(false);
                                            if handled {
                                                cx.stop_propagation();
                                            }
                                        },
                                    );
                                },
                            )
                            .size_full(),
                        ),
                )
            })
            .child(
                div()
                    .id("workspace-sidebar-right-divider")
                    .debug_selector(|| "workspace-sidebar-right-divider".to_owned())
                    .absolute()
                    .top_0()
                    .right_0()
                    .h_full()
                    .w(px(CHROME_DIVIDER_SIZE))
                    .bg(gpui_color(ACTIVE_THEME.border)),
            )
            .child(
                div()
                    .id("workspace-sidebar-resize-handle")
                    .debug_selector(|| "workspace-sidebar-resize-handle".to_owned())
                    .absolute()
                    .top_0()
                    .right(px(-(SIDEBAR_RESIZE_HIT_SIZE - CHROME_DIVIDER_SIZE) / 2.0))
                    .h_full()
                    .w(px(SIDEBAR_RESIZE_HIT_SIZE))
                    .block_mouse_except_scroll()
                    .cursor_col_resize()
                    .on_drag(DraggedWorkspaceSidebar, |_, _, _, cx| cx.new(|_| Empty)),
            )
            .into_any_element()
    }

    fn render_workspace_menu(
        &self,
        menu: WorkspaceMenuState,
        manager: WeakEntity<Self>,
    ) -> AnyElement {
        let dismiss_manager = manager.clone();
        div()
            .id(("workspace-menu-controls", menu.workspace_id.get()))
            .debug_selector(move || format!("workspace-menu-controls-{}", menu.workspace_id.get()))
            .absolute()
            .top(menu.top)
            .left(px(WORKSPACE_MENU_INSET))
            .w(px(WORKSPACE_MENU_WIDTH))
            .h(px(WORKSPACE_MENU_HEIGHT))
            .on_mouse_down_out(move |_, _, cx| {
                let _ = dismiss_manager.update(cx, |manager, cx| {
                    if manager.workspace_menu.take().is_some() {
                        cx.notify();
                    }
                });
            })
            .child(render_workspace_menu_content(menu.workspace_id, manager))
            .into_any_element()
    }
}

impl Render for WorkspaceManager {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        debug_assert!(self.workspaces.len() > 0);
        let manager = cx.entity().downgrade();
        let resize_manager = manager.clone();
        let active_window_manager = self.workspaces.active_workspace().payload().clone();
        div()
            .id("workspace-manager")
            .debug_selector(|| "workspace-manager".to_owned())
            .key_context(TERMINAL_KEY_CONTEXT)
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(gpui_color(ACTIVE_THEME.terminal_background))
            .on_drag_move::<DraggedWorkspaceSidebar>(move |event: &DragMoveEvent<_>, window, cx| {
                let pointer_x = event.event.position.x - event.bounds.origin.x;
                let _ = resize_manager.update(cx, |manager, cx| {
                    manager.resize_sidebar(pointer_x, window, cx);
                });
            })
            .on_action(cx.listener(Self::on_create_workspace))
            .on_action(cx.listener(Self::on_activate_workspace_1))
            .on_action(cx.listener(Self::on_activate_workspace_2))
            .on_action(cx.listener(Self::on_activate_workspace_3))
            .on_action(cx.listener(Self::on_activate_workspace_4))
            .on_action(cx.listener(Self::on_activate_workspace_5))
            .on_action(cx.listener(Self::on_activate_workspace_6))
            .on_action(cx.listener(Self::on_activate_workspace_7))
            .on_action(cx.listener(Self::on_activate_workspace_8))
            .on_action(cx.listener(Self::on_activate_workspace_9))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_sidebar_focus))
            .on_action(cx.listener(Self::forward_active_terminal_action::<CreateWindow>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateWindow1>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateWindow2>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateWindow3>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateWindow4>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateWindow5>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateWindow6>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateWindow7>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateWindow8>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateWindow9>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ClosePane>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<CloseWindow>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<SplitRight>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<SplitDown>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<FocusPaneLeft>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<FocusPaneRight>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<FocusPaneUp>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<FocusPaneDown>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<TogglePaneZoom>))
            .on_key_down(cx.listener(Self::on_rename_key_down))
            .child(active_window_manager)
            .child(self.render_top_left_chrome(manager.clone()))
            .when(self.sidebar_visible, |root| {
                root.child(self.render_sidebar(manager.clone(), cx))
            })
            .when_some(self.workspace_menu, |root, menu| {
                root.child(self.render_workspace_menu(menu, manager))
            })
    }
}

fn render_workspace_menu_content(
    workspace_id: WorkspaceId,
    manager: WeakEntity<WorkspaceManager>,
) -> AnyElement {
    let mut menu = div()
        .id(("workspace-menu", workspace_id.get()))
        .debug_selector(move || format!("workspace-menu-{}", workspace_id.get()))
        .w(px(WORKSPACE_MENU_WIDTH))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(WORKSPACE_MENU_CORNER_RADIUS))
        .border(px(WORKSPACE_MENU_BORDER_SIZE))
        .border_color(gpui_color(ACTIVE_THEME.border))
        .bg(gpui_color(ACTIVE_THEME.elevated_surface_background))
        .occlude();
    for command in [
        WorkspaceMenuCommand::NewWindow,
        WorkspaceMenuCommand::Rename,
        WorkspaceMenuCommand::Close,
    ] {
        if command == WorkspaceMenuCommand::Close {
            menu = menu.child(
                div()
                    .h(px(WORKSPACE_MENU_SEPARATOR_SIZE))
                    .bg(gpui_color(ACTIVE_THEME.border)),
            );
        }
        menu = menu.child(render_workspace_menu_row(command, manager.clone()));
    }
    menu.into_any_element()
}

fn render_workspace_menu_row(
    command: WorkspaceMenuCommand,
    manager: WeakEntity<WorkspaceManager>,
) -> AnyElement {
    let (name, icon, label, shortcut, destructive) = match command {
        WorkspaceMenuCommand::NewWindow => (
            "new-window",
            "plus.rectangle.on.rectangle",
            "New Window",
            "⌘T",
            false,
        ),
        WorkspaceMenuCommand::Rename => ("rename", "pencil", "Rename Workspace", "", false),
        WorkspaceMenuCommand::Close => ("close", "xmark", "Close Workspace", "", true),
    };
    let foreground = if destructive {
        ACTIVE_THEME.error
    } else {
        ACTIVE_THEME.text
    };
    div()
        .id(command as usize)
        .debug_selector(move || format!("workspace-menu-row-{name}"))
        .h(px(WORKSPACE_MENU_ROW_HEIGHT))
        .px(px(6.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .cursor_pointer()
        .text_size(px(13.0))
        .text_color(gpui_color(foreground))
        .hover(|row| row.bg(gpui_color(ACTIVE_THEME.element_hover)))
        .on_click(move |_, window, cx| {
            let _ = manager.update(cx, |manager, cx| {
                manager.perform_workspace_menu_command(command, window, cx);
            });
            cx.stop_propagation();
        })
        .child(
            div()
                .w(px(18.0))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(icon)
                        .weight(SymbolWeight::Regular)
                        .size(px(14.0))
                        .color(gpui_color(if destructive {
                            ACTIVE_THEME.error
                        } else {
                            ACTIVE_THEME.icon
                        })),
                ),
        )
        .child(label)
        .child(div().flex_grow())
        .child(
            div()
                .text_size(px(10.0))
                .text_color(gpui_color(ACTIVE_THEME.icon))
                .child(shortcut),
        )
        .into_any_element()
}

fn workspace_scrollbar_geometry(
    track_height_px: f32,
    maximum_offset_px: f32,
    offset_px: f32,
) -> Option<WorkspaceScrollbarGeometry> {
    if track_height_px <= 0.0 || maximum_offset_px <= 0.0 {
        return None;
    }

    let track_height = f64::from(track_height_px);
    let maximum_offset = f64::from(maximum_offset_px);
    let content_height = track_height + maximum_offset;
    let minimum_height = f64::from(SIDEBAR_SCROLLBAR_MINIMUM_THUMB_HEIGHT.min(track_height_px));
    let thumb_height =
        (track_height / content_height * track_height).clamp(minimum_height, track_height);
    let progress = f64::from(offset_px.clamp(0.0, maximum_offset_px)) / maximum_offset;

    Some(WorkspaceScrollbarGeometry {
        top_px: ((track_height - thumb_height) * progress) as f32,
        height_px: thumb_height as f32,
        track_height_px,
    })
}

fn workspace_scrollbar_offset_for_pointer(
    maximum_offset_px: f32,
    track_height_px: f32,
    pointer_in_track_px: f32,
    grab_ratio: f32,
) -> Option<f32> {
    let geometry = workspace_scrollbar_geometry(track_height_px, maximum_offset_px, 0.0)?;
    let movable_height = (track_height_px - geometry.height_px).max(0.0);
    if movable_height == 0.0 {
        return Some(0.0);
    }

    let thumb_top = (pointer_in_track_px - grab_ratio.clamp(0.0, 1.0) * geometry.height_px)
        .clamp(0.0, movable_height);
    Some(thumb_top / movable_height * maximum_offset_px)
}

fn default_workspace_name(workspace_number: usize) -> String {
    format!("Workspace {workspace_number}")
}

fn next_default_workspace_name<T>(
    workspaces: &WorkspaceCollection<T>,
    excluded_workspace_id: Option<WorkspaceId>,
) -> String {
    for workspace_number in 1..=workspaces.len().saturating_add(1) {
        let candidate = default_workspace_name(workspace_number);
        let is_available = workspaces.iter().all(|workspace| {
            Some(workspace.id()) == excluded_workspace_id || workspace.name() != candidate
        });
        if is_available {
            return candidate;
        }
    }

    unreachable!("one of len + 1 default Workspace names must be available")
}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::path::Path;

    use gpui::{
        Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, VisualTestContext,
        point,
    };

    use super::*;
    use crate::terminal::{
        GridSize, KeyInput, PointerInput, SessionError, SessionEvent, StartedTerminalSession,
        TerminalSessionHandle, WheelInput,
    };

    #[derive(Clone)]
    struct SessionRecords {
        event_senders: Rc<RefCell<Vec<async_channel::Sender<SessionEvent>>>>,
        dropped_session_ids: Rc<RefCell<Vec<usize>>>,
    }

    struct WorkspaceSessionFactory {
        records: SessionRecords,
        next_session_id: Cell<usize>,
    }

    struct WorkspaceSessionHandle {
        session_id: usize,
        dropped_session_ids: Rc<RefCell<Vec<usize>>>,
    }

    impl TerminalSessionFactory for WorkspaceSessionFactory {
        fn start(
            &self,
            _size: GridSize,
            _working_directory: &Path,
        ) -> Result<StartedTerminalSession, SessionError> {
            let session_id = self.next_session_id.get();
            self.next_session_id.set(session_id + 1);
            let (sender, events) = async_channel::unbounded();
            self.records.event_senders.borrow_mut().push(sender);
            Ok(StartedTerminalSession {
                handle: Box::new(WorkspaceSessionHandle {
                    session_id,
                    dropped_session_ids: Rc::clone(&self.records.dropped_session_ids),
                }),
                events,
            })
        }

        fn fallback_title(&self) -> String {
            "zsh".to_owned()
        }
    }

    impl Drop for WorkspaceSessionHandle {
        fn drop(&mut self) {
            self.dropped_session_ids.borrow_mut().push(self.session_id);
        }
    }

    impl TerminalSessionHandle for WorkspaceSessionHandle {
        fn key(&self, _input: KeyInput) {}

        fn resize(&self, _size: GridSize) {}

        fn pointer(&self, _input: PointerInput) {}

        fn wheel(&self, _input: WheelInput) {}

        fn scroll_to(&self, _offset_rows: u64) {}

        fn paste(&self, _text: String) {}

        fn request_selection_text(
            &self,
        ) -> async_channel::Receiver<Result<Option<String>, String>> {
            let (sender, receiver) = async_channel::bounded(1);
            let _ = sender.try_send(Ok(None));
            receiver
        }
    }

    fn workspace_manager(
        cx: &mut TestAppContext,
    ) -> (
        Entity<WorkspaceManager>,
        SessionRecords,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init);
        let records = SessionRecords {
            event_senders: Rc::new(RefCell::new(Vec::new())),
            dropped_session_ids: Rc::new(RefCell::new(Vec::new())),
        };
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(WorkspaceSessionFactory {
            records: records.clone(),
            next_session_id: Cell::new(1),
        });
        let (manager, cx) = cx.add_window_view(|window, cx| {
            WorkspaceManager::new(session_factory, PathBuf::from("/Users/test"), window, cx)
        });
        cx.update(|window, cx| {
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

    fn drag_to(selector: &'static str, destination_x: Pixels, cx: &mut VisualTestContext) {
        let start = cx
            .debug_bounds(selector)
            .map(|bounds| bounds.center())
            .unwrap_or_else(|| panic!("{selector} was not rendered"));
        let destination = point(destination_x, start.y);
        let drag_start = if destination_x >= start.x {
            point(start.x + px(12.0), start.y)
        } else {
            point(start.x - px(12.0), start.y)
        };
        cx.simulate_mouse_move(start, None, Modifiers::none());
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(drag_start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(destination, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(destination, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn sidebar_should_render_edge_to_edge_below_fixed_top_chrome(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = workspace_manager(cx);

        let chrome = cx
            .debug_bounds("workspace-top-chrome")
            .expect("the fixed top-left chrome was not rendered");
        let sidebar = cx
            .debug_bounds("workspace-sidebar")
            .expect("the Workspace sidebar was not rendered");
        let active_row = cx
            .debug_bounds("workspace-row-1-active")
            .expect("the Active Workspace row was not rendered");
        let sidebar_divider = cx
            .debug_bounds("workspace-sidebar-right-divider")
            .expect("the sidebar divider overlay was not rendered");
        let top_chrome_divider = cx
            .debug_bounds("workspace-top-chrome-right-divider")
            .expect("the fixed top-chrome divider was not rendered");
        let content = cx
            .debug_bounds("window-manager-content")
            .expect("the active Window content was not rendered");

        assert_eq!(
            (
                chrome.size,
                sidebar.origin.x,
                sidebar.origin.y,
                sidebar.size.width,
                active_row.origin.x,
                active_row.size.width,
                sidebar_divider.origin.x + sidebar_divider.size.width,
                sidebar_divider.origin.y,
                sidebar_divider.size,
                top_chrome_divider.origin.x + top_chrome_divider.size.width,
                top_chrome_divider.size.height,
                content.origin.x,
            ),
            (
                gpui::size(px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH), px(TOP_CHROME_HEIGHT)),
                px(0.0),
                px(TOP_CHROME_HEIGHT),
                px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
                px(0.0),
                px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
                sidebar.origin.x + sidebar.size.width,
                sidebar.origin.y,
                gpui::size(px(CHROME_DIVIDER_SIZE), sidebar.size.height),
                chrome.origin.x + chrome.size.width,
                chrome.size.height,
                px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            )
        );
    }

    #[gpui::test]
    fn dragging_sidebar_divider_should_resize_sidebar_chrome_and_content(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);
        let root = cx
            .debug_bounds("workspace-manager")
            .expect("the Workspace manager was not rendered");
        let resized_width = px(300.0);

        drag_to(
            "workspace-sidebar-resize-handle",
            root.origin.x + resized_width,
            cx,
        );

        let layout = manager.read_with(cx, |manager, _| {
            (manager.sidebar_visible, manager.sidebar_width)
        });
        let chrome = cx
            .debug_bounds("workspace-top-chrome")
            .expect("the persistent top-left chrome was not rendered");
        let sidebar = cx
            .debug_bounds("workspace-sidebar")
            .expect("the resized Workspace sidebar was not rendered");
        let content = cx
            .debug_bounds("window-manager-content")
            .expect("the active Window content was not rendered");
        let window_bar = cx
            .debug_bounds("window-bar")
            .expect("the Window bar was not rendered");
        assert_eq!(
            (
                layout,
                chrome.size.width,
                sidebar.size.width,
                content.origin.x,
                window_bar.origin.x,
            ),
            (
                (true, resized_width),
                resized_width,
                resized_width,
                root.origin.x + resized_width,
                root.origin.x + resized_width,
            )
        );
    }

    #[gpui::test]
    fn dragging_sidebar_below_minimum_should_collapse_it_at_the_minimum_width(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = workspace_manager(cx);
        let root = cx
            .debug_bounds("workspace-manager")
            .expect("the Workspace manager was not rendered");

        drag_to(
            "workspace-sidebar-resize-handle",
            root.origin.x + px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH - 20.0),
            cx,
        );

        let layout = manager.read_with(cx, |manager, _| {
            (manager.sidebar_visible, manager.sidebar_width)
        });
        let chrome = cx
            .debug_bounds("workspace-top-chrome")
            .expect("the persistent top-left chrome was not rendered");
        let content = cx
            .debug_bounds("window-manager-content")
            .expect("the active Window content was not rendered");
        let window_bar = cx
            .debug_bounds("window-bar")
            .expect("the Window bar was not rendered");
        assert_eq!(
            (
                layout,
                chrome.size.width,
                content.origin.x,
                window_bar.origin.x,
            ),
            (
                (false, px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH)),
                px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH),
                root.origin.x,
                root.origin.x + px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH),
            )
        );
    }

    #[gpui::test]
    fn every_workspace_row_should_end_with_a_full_width_divider(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();

        let first_row = cx
            .debug_bounds("workspace-row-1-inactive")
            .expect("the inactive Workspace row was not rendered");
        let first_divider = cx
            .debug_bounds("workspace-row-divider-1")
            .expect("the first Workspace divider was not rendered");
        let second_row = cx
            .debug_bounds("workspace-row-2-active")
            .expect("the Active Workspace row was not rendered");
        let second_divider = cx
            .debug_bounds("workspace-row-divider-2")
            .expect("the second Workspace divider was not rendered");

        assert_eq!(
            (first_divider, second_divider),
            (
                gpui::bounds(
                    point(
                        first_row.origin.x,
                        first_row.origin.y + first_row.size.height - px(1.0)
                    ),
                    gpui::size(first_row.size.width, px(CHROME_DIVIDER_SIZE)),
                ),
                gpui::bounds(
                    point(
                        second_row.origin.x,
                        second_row.origin.y + second_row.size.height - px(1.0),
                    ),
                    gpui::size(second_row.size.width, px(CHROME_DIVIDER_SIZE)),
                ),
            )
        );
    }

    #[gpui::test]
    fn new_workspace_button_should_start_with_a_full_width_divider(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = workspace_manager(cx);
        let button = cx
            .debug_bounds("create-workspace-button")
            .expect("the New Workspace button was not rendered");
        let divider = cx
            .debug_bounds("create-workspace-button-top-divider")
            .expect("the New Workspace button divider was not rendered");

        assert_eq!(
            divider,
            gpui::bounds(
                button.origin,
                gpui::size(button.size.width, px(CHROME_DIVIDER_SIZE)),
            )
        );
    }

    #[test]
    fn workspace_scrollbar_geometry_should_be_proportional_across_the_track() {
        let top = workspace_scrollbar_geometry(200.0, 800.0, 0.0).unwrap();
        let middle = workspace_scrollbar_geometry(200.0, 800.0, 400.0).unwrap();
        let bottom = workspace_scrollbar_geometry(200.0, 800.0, 800.0).unwrap();

        assert_eq!(
            (top, middle, bottom),
            (
                WorkspaceScrollbarGeometry {
                    top_px: 0.0,
                    height_px: 40.0,
                    track_height_px: 200.0,
                },
                WorkspaceScrollbarGeometry {
                    top_px: 80.0,
                    height_px: 40.0,
                    track_height_px: 200.0,
                },
                WorkspaceScrollbarGeometry {
                    top_px: 160.0,
                    height_px: 40.0,
                    track_height_px: 200.0,
                },
            )
        );
    }

    #[test]
    fn workspace_scrollbar_drag_should_map_to_the_full_scroll_range() {
        let top = workspace_scrollbar_offset_for_pointer(800.0, 200.0, 20.0, 0.5);
        let middle = workspace_scrollbar_offset_for_pointer(800.0, 200.0, 100.0, 0.5);
        let bottom = workspace_scrollbar_offset_for_pointer(800.0, 200.0, 180.0, 0.5);

        assert_eq!((top, middle, bottom), (Some(0.0), Some(400.0), Some(800.0)));
    }

    #[gpui::test]
    fn workspace_list_should_scroll_vertically_with_the_mouse_wheel(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);
        for _ in 0..24 {
            cx.simulate_keystrokes("cmd-n");
        }

        manager.read_with(cx, |manager, _| {
            manager
                .workspace_list_scroll_handle
                .set_offset(point(px(0.0), px(0.0)));
        });
        manager.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let list = cx
            .debug_bounds("workspace-list")
            .expect("the Workspace list was not rendered");
        cx.simulate_event(ScrollWheelEvent {
            position: list.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-120.0))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();

        let offset = manager.read_with(cx, |manager, _| {
            manager.workspace_list_scroll_handle.offset().y
        });
        assert!(
            offset < px(0.0),
            "the Workspace list did not scroll; offset was {offset:?}"
        );
    }

    #[gpui::test]
    fn workspace_scrollbar_should_reveal_when_the_list_scrolls(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);
        for _ in 0..24 {
            cx.simulate_keystrokes("cmd-n");
        }
        manager.read_with(cx, |manager, _| {
            manager
                .workspace_list_scroll_handle
                .set_offset(point(px(0.0), px(0.0)));
        });
        manager.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();

        let list = cx
            .debug_bounds("workspace-list")
            .expect("the Workspace list was not rendered");
        cx.simulate_event(ScrollWheelEvent {
            position: list.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-120.0))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();

        let thumb = cx
            .debug_bounds("workspace-scrollbar-thumb")
            .expect("the Workspace scrollbar thumb was not rendered");
        assert!(
            manager.read_with(cx, |manager, _| manager.workspace_scrollbar_visible)
                && thumb.size.width == px(SIDEBAR_SCROLLBAR_WIDTH)
                && thumb.size.height >= px(SIDEBAR_SCROLLBAR_MINIMUM_THUMB_HEIGHT)
                && thumb.size.height < list.size.height,
            "the revealed Workspace scrollbar had unexpected bounds: {thumb:?}"
        );
    }

    #[gpui::test]
    fn workspace_scrollbar_should_remain_visible_on_hover_then_hide_after_leaving(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = workspace_manager(cx);
        for _ in 0..24 {
            cx.simulate_keystrokes("cmd-n");
        }
        manager.update(cx, |manager, cx| {
            manager.reveal_workspace_scrollbar(cx);
        });
        cx.run_until_parked();

        let thumb = cx
            .debug_bounds("workspace-scrollbar-thumb-hitbox")
            .expect("the Workspace scrollbar hitbox was not rendered");
        let list = cx
            .debug_bounds("workspace-list")
            .expect("the Workspace list was not rendered");
        cx.simulate_mouse_move(thumb.center(), None, Modifiers::none());
        cx.executor()
            .advance_clock(SIDEBAR_SCROLLBAR_HIDE_DELAY + Duration::from_millis(1));
        cx.run_until_parked();
        let visible_while_hovered = manager.read_with(cx, |manager, _| {
            manager.workspace_scrollbar_visible && manager.workspace_scrollbar_hovered
        });

        cx.simulate_mouse_move(
            point(list.origin.x + px(20.0), list.center().y),
            None,
            Modifiers::none(),
        );
        cx.executor()
            .advance_clock(SIDEBAR_SCROLLBAR_HIDE_DELAY + Duration::from_millis(1));
        cx.run_until_parked();
        let hidden_after_leaving = manager.read_with(cx, |manager, _| {
            !manager.workspace_scrollbar_visible && !manager.workspace_scrollbar_hovered
        });

        assert!(visible_while_hovered && hidden_after_leaving);
    }

    #[gpui::test]
    fn workspace_scrollbar_thumb_should_drag_the_list(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);
        for _ in 0..24 {
            cx.simulate_keystrokes("cmd-n");
        }
        manager.update(cx, |manager, cx| {
            manager
                .workspace_list_scroll_handle
                .set_offset(point(px(0.0), px(0.0)));
            manager.reveal_workspace_scrollbar(cx);
        });
        cx.run_until_parked();

        let thumb = cx
            .debug_bounds("workspace-scrollbar-thumb-hitbox")
            .expect("the Workspace scrollbar hitbox was not rendered");
        let list = cx
            .debug_bounds("workspace-list")
            .expect("the Workspace list was not rendered");
        let start = thumb.center();
        let destination = point(start.x, list.bottom() - thumb.size.height / 2.0);
        cx.simulate_mouse_move(start, None, Modifiers::none());
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            point(start.x, start.y + px(12.0)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_move(destination, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(destination, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspace_list_scroll_handle.offset().y,
                manager.workspace_scrollbar_drag.is_none(),
            )
        });
        assert!(
            state.0 < px(0.0) && state.1,
            "the Workspace list did not finish a scrollbar drag: {state:?}"
        );
    }

    #[gpui::test]
    fn creating_workspaces_should_scroll_the_active_workspace_into_view(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);
        for _ in 0..24 {
            cx.simulate_keystrokes("cmd-n");
        }

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                manager.workspace_list_scroll_handle.offset().y,
            )
        });
        assert!(
            state.0 == 25 && state.1 == WorkspaceId::new(25) && state.2 < px(0.0),
            "the Active Workspace was not revealed; state was {state:?}"
        );
    }

    #[gpui::test]
    fn overflowing_workspace_list_should_not_cover_the_new_workspace_button(
        cx: &mut TestAppContext,
    ) {
        let (_manager, _records, cx) = workspace_manager(cx);
        for _ in 0..24 {
            cx.simulate_keystrokes("cmd-n");
        }

        let sidebar = cx
            .debug_bounds("workspace-sidebar")
            .expect("the Workspace sidebar was not rendered");
        let list = cx
            .debug_bounds("workspace-list")
            .expect("the overflowing Workspace list was not rendered");
        let button = cx
            .debug_bounds("create-workspace-button")
            .expect("the New Workspace button was not rendered");
        assert_eq!(
            (
                list.origin.y + list.size.height,
                button.origin.y + button.size.height,
            ),
            (button.origin.y, sidebar.origin.y + sidebar.size.height)
        );
    }

    #[gpui::test]
    fn command_b_should_hide_only_the_sidebar_body_and_expand_terminal_content(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = workspace_manager(cx);
        let expanded_chrome = cx
            .debug_bounds("workspace-top-chrome")
            .expect("the fixed top-left chrome was not rendered");

        cx.simulate_keystrokes("cmd-b");
        cx.run_until_parked();

        let hidden_state = manager.read_with(cx, |manager, _| manager.sidebar_visible);
        let collapsed_chrome = cx
            .debug_bounds("workspace-top-chrome")
            .expect("the fixed top-left chrome must remain rendered");
        let content = cx
            .debug_bounds("window-manager-content")
            .expect("the active Window content was not rendered");
        assert_eq!(
            (
                hidden_state,
                expanded_chrome,
                collapsed_chrome,
                content.origin.x,
            ),
            (false, expanded_chrome, expanded_chrome, px(0.0))
        );
    }

    #[gpui::test]
    fn command_shift_e_should_toggle_focus_and_reveal_a_hidden_sidebar(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);

        cx.simulate_keystrokes("cmd-shift-e");
        cx.run_until_parked();
        let sidebar_focused =
            cx.update(|window, cx| manager.read(cx).sidebar_focus.is_focused(window));

        cx.simulate_keystrokes("cmd-b");
        cx.run_until_parked();
        let hidden_state = cx.update(|window, cx| {
            let workspace_manager = manager.read(cx);
            (
                workspace_manager.sidebar_visible,
                workspace_manager.sidebar_focus.is_focused(window),
                workspace_manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        });

        cx.simulate_keystrokes("cmd-shift-e");
        cx.run_until_parked();
        let revealed_state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.sidebar_visible,
                manager.sidebar_focus.is_focused(window),
            )
        });

        assert_eq!(
            (sidebar_focused, hidden_state, revealed_state),
            (true, (false, false, true), (true, true))
        );
    }

    #[gpui::test]
    fn command_n_should_create_and_activate_a_home_directory_workspace(cx: &mut TestAppContext) {
        let (manager, records, cx) = workspace_manager(cx);

        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                manager
                    .workspaces
                    .active_workspace()
                    .working_directory()
                    .to_path_buf(),
                records.dropped_session_ids.borrow().clone(),
            )
        });
        assert_eq!(
            state,
            (
                2,
                WorkspaceId::new(2),
                PathBuf::from("/Users/test"),
                Vec::new(),
            )
        );
    }

    #[gpui::test]
    fn control_number_should_activate_workspaces_by_position(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n cmd-n");
        cx.run_until_parked();

        cx.simulate_keystrokes("ctrl-1");
        cx.run_until_parked();
        let first_state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.workspaces.active_workspace_id(),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        });

        cx.simulate_keystrokes("cmd-shift-e ctrl-2");
        cx.run_until_parked();
        let second_state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.workspaces.active_workspace_id(),
                manager.sidebar_focus.is_focused(window),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        });

        cx.simulate_keystrokes("ctrl-9");
        cx.run_until_parked();
        let unavailable_state =
            manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());

        assert_eq!(first_state, (WorkspaceId::new(1), true));
        assert_eq!(second_state, (WorkspaceId::new(2), true, false));
        assert_eq!(unavailable_state, WorkspaceId::new(2));
    }

    #[gpui::test]
    fn clicking_an_inactive_workspace_should_retain_sidebar_focus(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();

        click("workspace-row-1-inactive", cx);

        let state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.workspaces.active_workspace_id(),
                manager.sidebar_focus.is_focused(window),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        });
        assert_eq!(state, (WorkspaceId::new(1), true, false));
    }

    #[gpui::test]
    fn right_clicking_an_inactive_workspace_should_keep_menu_focus_off_the_terminal(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();

        right_click("workspace-row-1-inactive", cx);

        let state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.workspaces.active_workspace_id(),
                manager.sidebar_focus.is_focused(window),
                manager.workspace_menu.map(|menu| menu.workspace_id),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        });
        assert_eq!(
            state,
            (WorkspaceId::new(1), true, Some(WorkspaceId::new(1)), false)
        );
        assert!(cx.debug_bounds("workspace-menu-1").is_some());
    }

    #[gpui::test]
    fn window_shortcuts_should_create_and_activate_windows_while_sidebar_is_focused(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = workspace_manager(cx);

        cx.simulate_keystrokes("cmd-shift-e");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();

        let created_state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.sidebar_focus.is_focused(window),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .sidebar_detail(cx),
            )
        });

        cx.simulate_keystrokes("cmd-shift-e");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-1");
        cx.run_until_parked();

        assert_eq!(
            (created_state.0, created_state.1.as_ref()),
            (false, "zsh · 2 Windows")
        );
        assert!(cx.debug_bounds("window-item-1-active").is_some());
        assert!(cx.debug_bounds("window-item-2-inactive").is_some());
    }

    #[gpui::test]
    fn command_w_from_sidebar_should_close_the_final_pane_and_replace_its_workspace(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);

        cx.simulate_keystrokes("cmd-shift-e");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                manager.workspaces.active_workspace().name().to_owned(),
                records.dropped_session_ids.borrow().clone(),
                records.event_senders.borrow().len(),
            )
        });
        assert_eq!(
            state,
            (1, WorkspaceId::new(2), "Workspace 1".to_owned(), vec![1], 2,)
        );
    }

    #[gpui::test]
    fn repeated_command_w_should_keep_the_replacement_workspace_number_stable(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-shift-e");
        cx.run_until_parked();

        for _ in 0..3 {
            cx.simulate_keystrokes("cmd-w");
            cx.run_until_parked();
        }

        let name = manager.read_with(cx, |manager, _| {
            manager.workspaces.active_workspace().name().to_owned()
        });
        assert_eq!(name, "Workspace 1");
    }

    #[gpui::test]
    fn pane_shortcuts_should_operate_on_the_active_window_while_sidebar_is_focused(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);

        cx.simulate_keystrokes("cmd-shift-e");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();

        let state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.sidebar_focus.is_focused(window),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .sidebar_detail(cx),
                records.dropped_session_ids.borrow().clone(),
            )
        });
        assert_eq!(
            (state.0, state.1.as_ref(), state.2),
            (false, "2 Panes", Vec::new())
        );
    }

    #[gpui::test]
    fn workspace_context_menu_should_target_new_window_and_rename_commands(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = workspace_manager(cx);

        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-new-window", cx);
        let workspace_detail = manager.read_with(cx, |manager, cx| {
            manager
                .workspaces
                .active_workspace()
                .payload()
                .read(cx)
                .sidebar_detail(cx)
        });

        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-rename", cx);
        cx.simulate_keystrokes("D e v enter");
        cx.run_until_parked();
        let name = manager.read_with(cx, |manager, _| {
            manager.workspaces.active_workspace().name().to_owned()
        });

        assert_eq!(
            (workspace_detail.as_ref(), name),
            ("zsh · 2 Windows", "Dev".to_owned())
        );
    }

    #[gpui::test]
    fn clicking_the_active_inline_rename_should_keep_it_editable(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);
        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-rename", cx);

        click("workspace-rename-input-1", cx);
        let focus_state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.rename_focus.is_focused(window),
                manager.sidebar_focus.is_focused(window),
                manager.rename.is_some(),
            )
        });
        cx.simulate_keystrokes("D e v enter");
        cx.run_until_parked();

        let rename_state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.active_workspace_id(),
                manager.workspaces.active_workspace().name().to_owned(),
                manager.rename.is_none(),
            )
        });
        assert_eq!(focus_state, (true, false, true));
        assert_eq!(rename_state, (WorkspaceId::new(1), "Dev".to_owned(), true));
    }

    #[gpui::test]
    fn activating_another_workspace_should_cancel_the_previous_inline_rename(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        click("workspace-row-1-inactive", cx);
        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-rename", cx);

        assert!(manager.read_with(cx, |manager, _| manager.rename.is_some()));
        click("workspace-row-2-inactive", cx);
        cx.simulate_keystrokes("x enter");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, cx| {
            let first = manager
                .workspaces
                .workspace(WorkspaceId::new(1))
                .expect("Workspace 1 must remain owned");
            let second = manager
                .workspaces
                .workspace(WorkspaceId::new(2))
                .expect("Workspace 2 must remain owned");
            (
                manager.workspaces.active_workspace_id(),
                manager.rename.is_none(),
                first.name().to_owned(),
                first.payload().read(cx).sidebar_detail(cx),
                second.payload().read(cx).sidebar_detail(cx),
                records.dropped_session_ids.borrow().clone(),
            )
        });
        assert_eq!(
            (
                state.0,
                state.1,
                state.2,
                state.3.as_ref(),
                state.4.as_ref(),
                state.5,
            ),
            (
                WorkspaceId::new(2),
                true,
                "Workspace 1".to_owned(),
                "zsh",
                "zsh · 2 Windows",
                Vec::new(),
            )
        );
    }

    #[gpui::test]
    fn closing_the_final_workspace_should_replace_it_and_cleanup_its_pty_once(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);

        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-close", cx);

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                records.dropped_session_ids.borrow().clone(),
            )
        });
        assert_eq!(state, (1, WorkspaceId::new(2), vec![1]));
    }

    #[gpui::test]
    fn inactive_shell_exit_should_close_its_workspace_without_stealing_activation(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let inactive_sender = records
            .event_senders
            .borrow()
            .first()
            .cloned()
            .expect("the initial Workspace terminal session must have started");
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();

        inactive_sender
            .try_send(SessionEvent::Exited("Shell exited".to_owned()))
            .expect("the inactive shell exit must be delivered");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                records.dropped_session_ids.borrow().clone(),
                manager.next_workspace_id,
            )
        });
        assert_eq!(state, (1, WorkspaceId::new(2), vec![1], 3));
    }
}
