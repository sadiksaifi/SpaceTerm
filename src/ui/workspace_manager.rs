use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    Action, AnyElement, App, Context, DispatchPhase, DragMoveEvent, Empty, Entity, EntityId,
    FocusHandle, MouseButton, MouseExitEvent, Pixels, PromptButton, PromptLevel, Render,
    ScrollHandle, ScrollWheelEvent, SharedString, Subscription, WeakEntity, Window, canvas, div,
    point, px, rgba,
};
use gpui_symbols::{Icon, RenderingMode, SymbolWeight};
use spaceterm_ui::{
    Button, ButtonShape, ButtonSize, ButtonVariant, CommandPalette, CommandPaletteAccessory,
    CommandPaletteCloseReason, CommandPaletteEvent, CommandPaletteHint, CommandPaletteItem,
    CommandPaletteLifecycleEvent, ContextMenu, IconButton, MenuCloseReason, MenuEntry,
    MenuLifecycleEvent, MenuSize, MiddleTruncatedText, OverlayScrollbar, OverlayScrollbarEvent,
    ScrollMetrics, TextInput, TextInputEvent, TextInputStyle,
};

use super::button_theme;
use super::terminal_focus::TerminalFocusBlocker;
use super::{
    ActivateWindow1, ActivateWindow2, ActivateWindow3, ActivateWindow4, ActivateWindow5,
    ActivateWindow6, ActivateWindow7, ActivateWindow8, ActivateWindow9, ActivateWorkspace1,
    ActivateWorkspace2, ActivateWorkspace3, ActivateWorkspace4, ActivateWorkspace5,
    ActivateWorkspace6, ActivateWorkspace7, ActivateWorkspace8, ActivateWorkspace9, ClosePane,
    CloseTerminalFind, CloseWindow, CloseWorkspace, CopySelection, CreateWindow, CreateWorkspace,
    FindNext, FindPrevious, FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp,
    OpenLocalProject, OpenTerminalFind, SearchWorkspaces, SplitDown, SplitRight,
    TERMINAL_KEY_CONTEXT, TOP_CHROME_HEIGHT, TogglePaneZoom, ToggleSidebar, ToggleSidebarFocus,
    WORKSPACE_SIDEBAR_DEFAULT_WIDTH, WORKSPACE_SIDEBAR_MINIMUM_WIDTH, WindowManager,
    WindowManagerEvent, handle_top_chrome_mouse_down,
};
use crate::domain::{
    CloseWorkspaceOutcome, DirectoryAuthority, FinalWindowCloseOutcome,
    ValidatedWorkspaceDirectory, WorkspaceCollection, WorkspaceDirectoryAvailability,
    WorkspaceDirectoryIdentity, WorkspaceError, WorkspaceId, WorkspaceKind,
};
use crate::platform::local_project_picker::{LocalProjectPicker, NativeLocalProjectPicker};
use crate::platform::workspace_directory::validate_workspace_directory;
use crate::terminal::{
    NativeServiceOrigin, NativeServiceStatus, SelectionCopy, TerminalSessionFactory,
    WorkspaceTerminalSessionFactory,
};
use crate::theme::{ACTIVE_THEME, Color};

const SIDEBAR_TOGGLE_INSET: f32 = 4.0;
const SIDEBAR_ROW_HEIGHT: f32 = 58.0;
// Vertical breathing room above and below the header's 28px `ButtonSize::Regular` actions. The
// header height is derived from it so the two cannot drift apart.
const SIDEBAR_HEADER_ACTION_PADDING: f32 = 6.0;
const SIDEBAR_HEADER_HEIGHT: f32 = 28.0 + SIDEBAR_HEADER_ACTION_PADDING * 2.0;
const SIDEBAR_HEADER_TRAILING_PADDING: f32 = SIDEBAR_TOGGLE_INSET;
// The header actions carry their own horizontal padding inside a 28px control box, which already
// leaves roughly 15px between the two glyphs. Any additional gap reads as a gulf at this size.
const SIDEBAR_HEADER_ACTION_GAP: f32 = 0.0;
const SIDEBAR_ROW_HORIZONTAL_PADDING: f32 = 12.0;
const SIDEBAR_ROW_ICON_SIZE: f32 = 14.0;
const SIDEBAR_NAME_TEXT_SIZE: f32 = 13.0;
const SIDEBAR_DETAIL_TEXT_SIZE: f32 = 11.0;
const NEW_WORKSPACE_BUTTON_HEIGHT: f32 = 40.0;
const CHROME_DIVIDER_SIZE: f32 = 1.0;
const SIDEBAR_RESIZE_HIT_SIZE: f32 = 8.0;
const SIDEBAR_MAXIMUM_WIDTH: f32 = 420.0;
const TERMINAL_CONTENT_MINIMUM_WIDTH: f32 = 240.0;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspaceMenuCommand {
    NewWindow,
    Rename,
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WorkspaceMenuState {
    workspace_id: WorkspaceId,
}

struct WorkspaceRenameState {
    workspace_id: WorkspaceId,
    input: Entity<TextInput>,
    focus_handle: FocusHandle,
}

struct WorkspaceSidebarTooltip {
    text: SharedString,
}

struct WorkspaceRowViewModel {
    workspace_id: WorkspaceId,
    name: SharedString,
    path: SharedString,
    tooltip: SharedString,
    local_project: bool,
    available: bool,
    window_count: usize,
    pane_count: usize,
    active: bool,
}

impl Render for WorkspaceSidebarTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(480.0))
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(5.0))
            .bg(gpui_color(ACTIVE_THEME.elevated_surface_background))
            .text_size(px(11.0))
            .text_color(gpui_color(ACTIVE_THEME.text))
            .child(self.text.clone())
    }
}

#[derive(Clone, Copy)]
struct DraggedWorkspaceSidebar;

pub(crate) struct WorkspaceManager {
    workspaces: WorkspaceCollection<Entity<WindowManager>>,
    session_factory: Rc<dyn TerminalSessionFactory>,
    default_workspace_root: PathBuf,
    default_workspace_identity: WorkspaceDirectoryIdentity,
    local_project_picker: Rc<dyn LocalProjectPicker>,
    local_project_picker_open: bool,
    sidebar_visible: bool,
    sidebar_width: Pixels,
    workspace_list_scroll_handle: ScrollHandle,
    scrollbar: Entity<OverlayScrollbar<f32>>,
    sidebar_focus: FocusHandle,
    workspace_search: Entity<CommandPalette<WorkspaceId>>,
    workspace_search_open: bool,
    workspace_search_open_request: Option<u64>,
    workspace_search_request_generation: u64,
    workspace_search_activation_pending: bool,
    _workspace_search_subscription: Subscription,
    workspace_menu: Option<WorkspaceMenuState>,
    rename: Option<WorkspaceRenameState>,
    top_chrome_interaction: bool,
    top_chrome_move_requested: bool,
    pending_final_window_closes: BTreeSet<WorkspaceId>,
}

impl WorkspaceManager {
    pub(crate) fn new(
        session_factory: Rc<dyn TerminalSessionFactory>,
        default_workspace_root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_local_project_picker(
            session_factory,
            default_workspace_root,
            Rc::new(NativeLocalProjectPicker),
            window,
            cx,
        )
    }

    fn new_with_local_project_picker(
        session_factory: Rc<dyn TerminalSessionFactory>,
        default_workspace_root: PathBuf,
        local_project_picker: Rc<dyn LocalProjectPicker>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (default_directory, initial_directory_error) =
            initial_workspace_directory(default_workspace_root.clone());
        let default_workspace_identity = default_directory.identity();
        let initial_workspace_identity = default_directory.identity();
        let mut workspaces = WorkspaceCollection::new_ad_hoc(
            default_directory,
            DirectoryAuthority::initial(),
            |workspace_id, workspace_root| {
                Self::create_window_manager(
                    workspace_id,
                    WorkspaceTerminalSessionFactory::new(
                        Rc::clone(&session_factory),
                        workspace_root.to_path_buf(),
                    )
                    .with_directory_identity(initial_workspace_identity),
                    true,
                    px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
                    window,
                    cx,
                )
            },
        );
        if let Some(reason) = initial_directory_error {
            let _ = workspaces.set_directory_unavailable(workspaces.active_workspace_id(), reason);
        }
        let scrollbar = cx.new(|_| OverlayScrollbar::<f32>::new("workspace-scrollbar"));
        cx.subscribe_in(
            &scrollbar,
            window,
            |manager, _, event: &OverlayScrollbarEvent<f32>, window, cx| match event {
                OverlayScrollbarEvent::InteractionStarted => {
                    manager.sidebar_focus.focus(window);
                    manager.sync_terminal_focus_blocker(window, cx);
                }
                OverlayScrollbarEvent::OffsetRequested(offset) => {
                    let current_offset = manager.workspace_list_scroll_handle.offset();
                    manager
                        .workspace_list_scroll_handle
                        .set_offset(point(current_offset.x, px(-*offset)));
                    cx.notify();
                }
            },
        )
        .detach();
        cx.observe_window_activation(window, |manager, window, cx| {
            if !window.is_window_active() {
                manager.cancel_workspace_search_open_request(window, cx);
                manager.set_top_chrome_interaction(false, window, cx);
            }
        })
        .detach();
        let workspace_search = cx.new(|cx| {
            let mut palette = CommandPalette::new("Search Workspaces", Vec::new(), window, cx);
            palette.set_no_results_text("No matching Workspaces", cx);
            palette.set_hints(
                vec![
                    CommandPaletteHint::new("Open", "\u{21b5}"),
                    CommandPaletteHint::new("Dismiss", "esc"),
                ],
                cx,
            );
            palette
        });
        let workspace_search_subscription = cx.subscribe_in(
            &workspace_search,
            window,
            |manager, _, event: &CommandPaletteEvent<WorkspaceId>, window, cx| {
                manager.handle_workspace_search_event(event, window, cx);
            },
        );

        Self {
            workspaces,
            session_factory,
            default_workspace_root,
            default_workspace_identity,
            local_project_picker,
            local_project_picker_open: false,
            sidebar_visible: true,
            sidebar_width: px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            workspace_list_scroll_handle: ScrollHandle::new(),
            scrollbar,
            sidebar_focus: cx.focus_handle(),
            workspace_search,
            workspace_search_open: false,
            workspace_search_open_request: None,
            workspace_search_request_generation: 0,
            workspace_search_activation_pending: false,
            _workspace_search_subscription: workspace_search_subscription,
            workspace_menu: None,
            rename: None,
            top_chrome_interaction: false,
            top_chrome_move_requested: false,
            pending_final_window_closes: BTreeSet::new(),
        }
    }

    fn create_window_manager(
        workspace_id: WorkspaceId,
        session_factory: WorkspaceTerminalSessionFactory,
        sidebar_visible: bool,
        sidebar_width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<WindowManager> {
        let manager = cx.new(|cx| {
            let mut manager = WindowManager::new(session_factory, window, cx);
            manager.set_sidebar_layout(sidebar_visible, sidebar_width, cx);
            manager
        });
        cx.subscribe_in(
            &manager,
            window,
            move |workspace_manager, _, event: &WindowManagerEvent, window, cx| match event {
                WindowManagerEvent::FinalWindowCloseRequested { .. } => {
                    if workspace_manager
                        .pending_final_window_closes
                        .insert(workspace_id)
                    {
                        cx.defer_in(window, move |workspace_manager, window, cx| {
                            workspace_manager.close_workspace_for_final_window(
                                workspace_id,
                                window,
                                cx,
                            );
                        });
                    }
                }
                WindowManagerEvent::PresentationChanged => cx.notify(),
                WindowManagerEvent::ReportedWorkingDirectoryChanged {
                    window_id,
                    pane_id,
                    path,
                } => workspace_manager.handle_directory_report(
                    workspace_id,
                    DirectoryAuthority::new(*window_id, *pane_id),
                    path,
                    cx,
                ),
                WindowManagerEvent::PaneClosed {
                    window_id,
                    pane_id,
                    promoted_pane_id,
                    promoted_directory,
                } => workspace_manager.handle_authority_promotion(
                    workspace_id,
                    DirectoryAuthority::new(*window_id, *pane_id),
                    DirectoryAuthority::new(*window_id, *promoted_pane_id),
                    promoted_directory.as_deref(),
                    cx,
                ),
                WindowManagerEvent::WindowClosed {
                    window_id,
                    promoted_window_id,
                    promoted_pane_id,
                    promoted_directory,
                } => workspace_manager.handle_window_authority_promotion(
                    workspace_id,
                    *window_id,
                    DirectoryAuthority::new(*promoted_window_id, *promoted_pane_id),
                    promoted_directory.as_deref(),
                    cx,
                ),
                WindowManagerEvent::DirectoryAvailable { identity } => {
                    let _ = workspace_manager
                        .workspaces
                        .set_directory_available(workspace_id, *identity);
                    cx.notify();
                }
                WindowManagerEvent::DirectoryUnavailable { reason } => {
                    let _ = workspace_manager
                        .workspaces
                        .set_directory_unavailable(workspace_id, reason.clone());
                    cx.notify();
                }
            },
        )
        .detach();
        manager
    }

    fn handle_directory_report(
        &mut self,
        workspace_id: WorkspaceId,
        authority: DirectoryAuthority,
        path: &std::path::Path,
        cx: &mut Context<Self>,
    ) {
        let directory = match validate_workspace_directory(path) {
            Ok(directory) => directory,
            Err(error) => {
                if self
                    .workspaces
                    .mark_directory_authority_unavailable(
                        workspace_id,
                        authority,
                        error.to_string(),
                    )
                    .unwrap_or(false)
                {
                    cx.notify();
                }
                return;
            }
        };
        let changed = self
            .workspaces
            .update_directory_authority_report(workspace_id, authority, directory)
            .unwrap_or(false);
        if !changed {
            return;
        }
        let Some(workspace) = self.workspaces.workspace(workspace_id) else {
            return;
        };
        let manager = workspace.payload().clone();
        let path = workspace.working_directory().to_path_buf();
        let identity = workspace.directory_identity();
        manager.update(cx, |manager, cx| {
            manager.set_workspace_directory(&path, identity, cx);
        });
        cx.notify();
    }

    fn handle_authority_promotion(
        &mut self,
        workspace_id: WorkspaceId,
        removed_authority: DirectoryAuthority,
        promoted_authority: DirectoryAuthority,
        reported_directory: Option<&std::path::Path>,
        cx: &mut Context<Self>,
    ) {
        let (directory, invalid_reason) = match reported_directory {
            Some(path) => match validate_workspace_directory(path) {
                Ok(directory) => (Some(directory), None),
                Err(error) => (None, Some(error.to_string())),
            },
            None => (None, None),
        };
        let promoted = self
            .workspaces
            .promote_directory_authority(
                workspace_id,
                removed_authority,
                promoted_authority,
                directory,
            )
            .unwrap_or(false);
        if !promoted {
            return;
        }
        if let Some(reason) = invalid_reason {
            let _ = self.workspaces.mark_directory_authority_unavailable(
                workspace_id,
                promoted_authority,
                reason,
            );
        }
        let Some(workspace) = self.workspaces.workspace(workspace_id) else {
            return;
        };
        let manager = workspace.payload().clone();
        let path = workspace.working_directory().to_path_buf();
        let identity = workspace.directory_identity();
        manager.update(cx, |manager, cx| {
            manager.set_workspace_directory(&path, identity, cx)
        });
        cx.notify();
    }

    fn handle_window_authority_promotion(
        &mut self,
        workspace_id: WorkspaceId,
        removed_window_id: crate::domain::WindowId,
        promoted_authority: DirectoryAuthority,
        reported_directory: Option<&std::path::Path>,
        cx: &mut Context<Self>,
    ) {
        let (directory, invalid_reason) = match reported_directory {
            Some(path) => match validate_workspace_directory(path) {
                Ok(directory) => (Some(directory), None),
                Err(error) => (None, Some(error.to_string())),
            },
            None => (None, None),
        };
        let promoted = self
            .workspaces
            .promote_directory_authority_for_window(
                workspace_id,
                removed_window_id,
                promoted_authority,
                directory,
            )
            .unwrap_or(false);
        if !promoted {
            return;
        }
        if let Some(reason) = invalid_reason {
            let _ = self.workspaces.mark_directory_authority_unavailable(
                workspace_id,
                promoted_authority,
                reason,
            );
        }
        let Some(workspace) = self.workspaces.workspace(workspace_id) else {
            return;
        };
        let manager = workspace.payload().clone();
        let path = workspace.working_directory().to_path_buf();
        let identity = workspace.directory_identity();
        manager.update(cx, |manager, cx| {
            manager.set_workspace_directory(&path, identity, cx)
        });
        cx.notify();
    }

    fn report_workspace_error(operation: &str, error: WorkspaceError) {
        eprintln!("failed to {operation} Workspace: {error}");
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        let manager = self.workspaces.active_workspace().payload().clone();
        manager.update(cx, |manager, cx| {
            manager.set_parent_focus_blocker(None, cx);
            manager.focus(window, cx);
        });
    }

    pub(crate) fn native_service_status(
        &self,
        window: &Window,
        cx: &mut App,
    ) -> NativeServiceStatus {
        let workspace_id = self.workspaces.active_workspace_id();
        let blocker = self.terminal_focus_blocker(window);
        self.workspaces
            .active_workspace()
            .payload()
            .update(cx, |manager, cx| {
                manager.set_parent_focus_blocker(blocker, cx);
                manager.native_service_status(workspace_id, window, cx)
            })
    }

    pub(crate) fn native_service_selection(
        &self,
        origin: NativeServiceOrigin,
        window: &Window,
        cx: &mut App,
    ) -> Option<SelectionCopy> {
        if self.workspaces.active_workspace_id() != origin.workspace_id() {
            return None;
        }
        self.workspaces
            .workspace(origin.workspace_id())?
            .payload()
            .update(cx, |manager, cx| {
                manager.native_service_selection(origin, window, cx)
            })
    }

    pub(crate) fn insert_native_service_text(
        &self,
        origin: NativeServiceOrigin,
        text: String,
        window: &Window,
        cx: &mut App,
    ) -> bool {
        if self.workspaces.active_workspace_id() != origin.workspace_id() {
            return false;
        }
        let Some(workspace) = self.workspaces.workspace(origin.workspace_id()) else {
            return false;
        };
        workspace.payload().update(cx, |manager, cx| {
            manager.insert_native_service_text(origin, text, window, cx)
        })
    }

    fn terminal_focus_blocker(&self, window: &Window) -> Option<TerminalFocusBlocker> {
        self.local_project_picker_open
            .then_some(TerminalFocusBlocker::Modal)
            .or((self.workspace_search_open
                || self.workspace_search_open_request.is_some()
                || self.workspace_search_activation_pending)
                .then_some(TerminalFocusBlocker::CommandPalette))
            .or(self
                .top_chrome_interaction
                .then_some(TerminalFocusBlocker::TopChrome)
                .or(self
                    .rename
                    .as_ref()
                    .map(|_| TerminalFocusBlocker::RenameField))
                .or(self
                    .workspace_menu
                    .map(|_| TerminalFocusBlocker::ContextMenu))
                .or(self
                    .sidebar_focus
                    .is_focused(window)
                    .then_some(TerminalFocusBlocker::Sidebar)))
    }

    fn workspace_search_items(&self, cx: &App) -> Vec<CommandPaletteItem<WorkspaceId>> {
        self.workspaces
            .iter()
            .map(|workspace| {
                let (window_count, pane_count) = workspace.payload().read(cx).aggregate_counts(cx);
                let path =
                    compact_home_path(workspace.working_directory(), &self.default_workspace_root);
                let available = matches!(
                    workspace.availability(),
                    WorkspaceDirectoryAvailability::Available
                );
                let local_project = matches!(workspace.kind(), WorkspaceKind::LocalProject { .. });
                let icon_color = gpui_color(if available {
                    ACTIVE_THEME.icon
                } else {
                    ACTIVE_THEME.warning
                });
                CommandPaletteItem::new(workspace.id(), workspace.name().to_owned())
                    .description(path)
                    .leading_icon(move |_| {
                        Icon::new(if local_project { "folder" } else { "terminal" })
                            .size(px(SIDEBAR_ROW_ICON_SIZE))
                            .color(icon_color)
                            .into_any_element()
                    })
                    .trailing(CommandPaletteAccessory::Text(
                        format!("{window_count}W · {pane_count}P").into(),
                    ))
                    .debug_selector(format!("workspace-search-result-{}", workspace.id().get()))
            })
            .collect()
    }

    fn open_workspace_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The palette replaces sidebar transients before it captures responder ownership. Opening
        // is deferred outside the WorkspaceManager lock because palette events synchronously call
        // back into this manager.
        self.rename = None;
        self.workspace_menu = None;
        self.workspace_search_request_generation =
            self.workspace_search_request_generation.wrapping_add(1);
        let request_generation = self.workspace_search_request_generation;
        self.workspace_search_open_request = Some(request_generation);
        self.workspace_search_activation_pending = false;
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();

        let manager = cx.entity().downgrade();
        window.defer(cx, move |window, cx| {
            let request = manager
                .update(cx, |manager, cx| {
                    (manager.workspace_search_open_request == Some(request_generation)).then(|| {
                        (
                            manager.workspace_search.clone(),
                            manager.workspace_search_items(cx),
                        )
                    })
                })
                .ok()
                .flatten();
            let Some((palette, items)) = request else {
                return;
            };
            let (opened, is_open) = palette.update(cx, |palette, cx| {
                palette.set_items(items, cx);
                let opened = palette.open(window, cx);
                (opened, palette.is_open())
            });
            if !opened {
                let _ = manager.update(cx, |manager, cx| {
                    if manager.workspace_search_open_request == Some(request_generation) {
                        manager.workspace_search_open_request = None;
                        manager.workspace_search_open = is_open;
                        manager.sync_terminal_focus_blocker(window, cx);
                        cx.notify();
                    }
                });
            }
        });
    }

    fn cancel_workspace_search_open_request(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.workspace_search_open_request.take().is_some() {
            self.sync_terminal_focus_blocker(window, cx);
            cx.notify();
        }
    }

    fn handle_workspace_search_event(
        &mut self,
        event: &CommandPaletteEvent<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Opened) => {
                self.workspace_search_open = true;
                self.workspace_search_open_request = None;
                self.workspace_search_activation_pending = false;
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            }
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Closed(reason)) => {
                self.workspace_search_open = false;
                self.workspace_search_open_request = None;
                self.workspace_search_activation_pending =
                    *reason == CommandPaletteCloseReason::Activated;
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            }
            CommandPaletteEvent::Activated(activation) => {
                self.workspace_search_open_request = None;
                self.workspace_search_activation_pending = false;
                if !self.activate_workspace(*activation.item_id(), window, cx) {
                    self.sync_terminal_focus_blocker(window, cx);
                    cx.notify();
                }
            }
            // The Workspace palette installs no search-line controls or actions menu.
            CommandPaletteEvent::QueryChanged(_)
            | CommandPaletteEvent::HeaderAction(_)
            | CommandPaletteEvent::MenuAction(_) => {}
        }
    }

    fn rename_is_focused(&self, window: &Window) -> bool {
        self.rename
            .as_ref()
            .is_some_and(|rename| rename.focus_handle.is_focused(window))
    }

    fn sync_terminal_focus_blocker(&self, window: &Window, cx: &mut Context<Self>) {
        let blocker = self.terminal_focus_blocker(window);
        self.workspaces
            .active_workspace()
            .payload()
            .update(cx, |manager, cx| {
                manager.set_parent_focus_blocker(blocker, cx);
            });
    }

    fn set_top_chrome_interaction(
        &mut self,
        blocked: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.top_chrome_interaction == blocked {
            return;
        }
        self.top_chrome_interaction = blocked;
        self.top_chrome_move_requested = false;
        self.sync_terminal_focus_blocker(window, cx);
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

    fn set_sidebar_layout(&mut self, visible: bool, width: Pixels, cx: &mut Context<Self>) {
        if self.sidebar_visible == visible && self.sidebar_width == width {
            return;
        }
        self.sidebar_visible = visible;
        self.sidebar_width = width;
        if !visible {
            self.scrollbar
                .update(cx, |scrollbar, cx| scrollbar.reset(cx));
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
                self.sidebar_focus.is_focused(window) || self.rename_is_focused(window);
            self.rename = None;
            self.set_sidebar_layout(false, minimum_width, cx);
            if was_sidebar_focused {
                self.focus(window, cx);
            }
            self.sync_terminal_focus_blocker(window, cx);
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

    fn scrollbar_metrics(&self) -> Option<ScrollMetrics<f32>> {
        let track_height_px = f32::from(self.workspace_list_scroll_handle.bounds().size.height);
        let maximum_offset_px = f32::from(self.workspace_list_scroll_handle.max_offset().height);
        let offset_px = -f32::from(self.workspace_list_scroll_handle.offset().y);
        ScrollMetrics::for_pixels(0.0, track_height_px, maximum_offset_px, offset_px)
    }

    fn sync_scrollbar(&self, cx: &mut Context<Self>) {
        let metrics = self.scrollbar_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.sync(metrics, cx));
    }

    fn reveal_scrollbar(&self, cx: &mut Context<Self>) {
        let metrics = self.scrollbar_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.reveal(metrics, cx));
    }

    fn on_workspace_list_scroll_wheel(
        &mut self,
        _: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reveal_scrollbar(cx);
    }

    fn create_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous_manager = self.workspaces.active_workspace().payload().clone();
        let session_factory = Rc::clone(&self.session_factory);
        let sidebar_visible = self.sidebar_visible;
        let sidebar_width = self.sidebar_width;
        let (directory, unavailable_reason) = self.default_workspace_directory();
        let directory_identity = directory.identity();
        let result = self.workspaces.create_ad_hoc_workspace(
            directory,
            DirectoryAuthority::initial(),
            |workspace_id, workspace_root| {
                Self::create_window_manager(
                    workspace_id,
                    WorkspaceTerminalSessionFactory::new(
                        session_factory,
                        workspace_root.to_path_buf(),
                    )
                    .with_directory_identity(directory_identity),
                    sidebar_visible,
                    sidebar_width,
                    window,
                    cx,
                )
            },
        );
        let workspace_id = match result {
            Ok(workspace_id) => workspace_id,
            Err(error) => {
                Self::report_workspace_error("create", error);
                return;
            }
        };
        if let Some(reason) = unavailable_reason {
            let _ = self
                .workspaces
                .set_directory_unavailable(workspace_id, reason);
        }
        let Some(next_manager) = self
            .workspaces
            .workspace(workspace_id)
            .map(|workspace| workspace.payload().clone())
        else {
            unreachable!("a newly created Workspace must remain owned by its collection")
        };

        previous_manager.update(cx, |manager, cx| manager.deactivate(cx));
        next_manager.update(cx, |manager, cx| manager.activate(window, cx));
        self.rename = None;
        self.sync_terminal_focus_blocker(window, cx);
        self.scroll_active_workspace_into_view();
        cx.notify();
    }

    fn open_local_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.local_project_picker_open {
            return;
        }
        self.rename = None;
        self.local_project_picker_open = true;
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();

        let selection = self.local_project_picker.pick(cx);
        cx.spawn_in(window, async move |manager, cx| {
            let result = selection.await;
            let _ = manager.update_in(cx, |manager, window, cx| {
                manager.local_project_picker_open = false;
                match result {
                    Ok(Some(path)) => manager.open_selected_local_project(path, window, cx),
                    Ok(None) => {
                        manager.sync_terminal_focus_blocker(window, cx);
                        cx.notify();
                    }
                    Err(error) => {
                        manager.show_directory_warning(
                            "Local Project could not be opened",
                            &error,
                            window,
                            cx,
                        );
                        manager.sync_terminal_focus_blocker(window, cx);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn open_selected_local_project(
        &mut self,
        selected_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let directory = match validate_workspace_directory(&selected_path) {
            Ok(directory) => directory,
            Err(error) => {
                self.show_directory_warning(
                    "Local Project directory is unavailable",
                    &format!("Restore {} and try again. {error}", selected_path.display()),
                    window,
                    cx,
                );
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
                return;
            }
        };

        if let Some(workspace_id) = self
            .workspaces
            .local_project_workspace(directory.identity())
        {
            self.revalidate_local_project(workspace_id);
            self.activate_workspace(workspace_id, window, cx);
            return;
        }

        let previous_manager = self.workspaces.active_workspace().payload().clone();
        let session_factory = Rc::clone(&self.session_factory);
        let sidebar_visible = self.sidebar_visible;
        let sidebar_width = self.sidebar_width;
        let project_root_identity = directory.identity();
        let result = self.workspaces.create_local_project_workspace(
            directory,
            |workspace_id, project_root| {
                Self::create_window_manager(
                    workspace_id,
                    WorkspaceTerminalSessionFactory::new(
                        session_factory,
                        project_root.to_path_buf(),
                    )
                    .with_directory_identity(project_root_identity),
                    sidebar_visible,
                    sidebar_width,
                    window,
                    cx,
                )
            },
        );
        let workspace_id = match result {
            Ok(workspace_id) => workspace_id,
            Err(error) => {
                Self::report_workspace_error("open Local Project", error);
                return;
            }
        };
        let Some(next_manager) = self
            .workspaces
            .workspace(workspace_id)
            .map(|workspace| workspace.payload().clone())
        else {
            unreachable!("a newly opened Local Project must remain owned by its collection")
        };
        previous_manager.update(cx, |manager, cx| manager.deactivate(cx));
        next_manager.update(cx, |manager, cx| manager.activate(window, cx));
        self.sync_terminal_focus_blocker(window, cx);
        self.scroll_active_workspace_into_view();
        cx.notify();
    }

    fn revalidate_local_project(&mut self, workspace_id: WorkspaceId) {
        let Some(workspace) = self.workspaces.workspace(workspace_id) else {
            return;
        };
        let path = workspace.working_directory().to_path_buf();
        let expected_identity = workspace.directory_identity();
        match validate_workspace_directory(&path) {
            Ok(directory) if directory.identity() == expected_identity => {
                let _ = self
                    .workspaces
                    .set_directory_available(workspace_id, expected_identity);
            }
            Ok(_) => {
                let _ = self.workspaces.set_directory_unavailable(
                    workspace_id,
                    format!(
                        "{} no longer identifies the selected Project Root",
                        path.display()
                    ),
                );
            }
            Err(error) => {
                let _ = self
                    .workspaces
                    .set_directory_unavailable(workspace_id, error.to_string());
            }
        }
    }

    fn show_directory_warning(
        &self,
        message: &str,
        detail: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        drop(window.prompt(
            PromptLevel::Warning,
            message,
            Some(detail),
            &[PromptButton::ok("OK")],
            cx,
        ));
    }

    fn default_workspace_directory(&self) -> (ValidatedWorkspaceDirectory, Option<String>) {
        match validate_workspace_directory(&self.default_workspace_root) {
            Ok(directory) => (directory, None),
            Err(error) => (
                ValidatedWorkspaceDirectory::new(
                    self.default_workspace_root.clone(),
                    self.default_workspace_identity,
                ),
                Some(error.to_string()),
            ),
        }
    }

    fn activate_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if matches!(
            self.workspaces
                .workspace(workspace_id)
                .map(|workspace| workspace.kind()),
            Some(WorkspaceKind::LocalProject { .. })
        ) {
            self.revalidate_local_project(workspace_id);
        }
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
            self.sidebar_focus.is_focused(window) || self.rename_is_focused(window);
        if previous_workspace_id != workspace_id {
            previous_manager.update(cx, |manager, cx| manager.deactivate(cx));
            self.rename = None;
        }
        if preserve_sidebar_focus {
            next_manager.update(cx, |manager, cx| manager.activate_without_focus(cx));
        } else {
            next_manager.update(cx, |manager, cx| manager.activate(window, cx));
        }
        self.sync_terminal_focus_blocker(window, cx);
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
        let session_factory = Rc::clone(&self.session_factory);
        let sidebar_visible = self.sidebar_visible;
        let sidebar_width = self.sidebar_width;
        let (replacement, unavailable_reason) = self.default_workspace_directory();
        let replacement_identity = replacement.identity();
        let outcome = self.workspaces.close_workspace_with_ad_hoc_replacement(
            workspace_id,
            replacement,
            DirectoryAuthority::initial(),
            |replacement_workspace_id, workspace_root| {
                Self::create_window_manager(
                    replacement_workspace_id,
                    WorkspaceTerminalSessionFactory::new(
                        session_factory,
                        workspace_root.to_path_buf(),
                    )
                    .with_directory_identity(replacement_identity),
                    sidebar_visible,
                    sidebar_width,
                    window,
                    cx,
                )
            },
        );
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                Self::report_workspace_error("close", error);
                return;
            }
        };
        if matches!(
            outcome,
            CloseWorkspaceOutcome::FinalWorkspaceReplaced { .. }
        ) && let Some(reason) = unavailable_reason
        {
            let replacement_id = self.workspaces.active_workspace_id();
            let _ = self
                .workspaces
                .set_directory_unavailable(replacement_id, reason);
        }

        let closed_manager = match outcome {
            CloseWorkspaceOutcome::WorkspaceClosed { payload, .. }
            | CloseWorkspaceOutcome::FinalWorkspaceReplaced { payload, .. } => payload,
        };
        closed_manager.update(cx, |manager, cx| manager.close_all(cx));

        if was_active {
            let active_manager = self.workspaces.active_workspace().payload().clone();
            if self.sidebar_focus.is_focused(window) || self.rename_is_focused(window) {
                active_manager.update(cx, |manager, cx| manager.activate_without_focus(cx));
            } else {
                active_manager.update(cx, |manager, cx| manager.activate(window, cx));
            }
        }
        if self
            .rename
            .as_ref()
            .is_some_and(|rename| rename.workspace_id == workspace_id)
        {
            self.rename = None;
        }
        self.sync_terminal_focus_blocker(window, cx);
        self.scroll_active_workspace_into_view();
        cx.notify();
    }

    fn close_workspace_for_final_window(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_active = self.workspaces.active_workspace_id() == workspace_id;
        let outcome = match self
            .workspaces
            .close_workspace_for_final_window(workspace_id)
        {
            Ok(outcome) => outcome,
            Err(error) => {
                self.pending_final_window_closes.remove(&workspace_id);
                Self::report_workspace_error("close for final Window", error);
                return;
            }
        };

        match outcome {
            FinalWindowCloseOutcome::WorkspaceClosed {
                closed_workspace_id,
                active_workspace_id,
                payload,
            } => {
                debug_assert_eq!(closed_workspace_id, workspace_id);
                payload.update(cx, |manager, cx| manager.close_all(cx));

                if was_active {
                    let active_manager = self.workspaces.active_workspace().payload().clone();
                    if self.sidebar_focus.is_focused(window) || self.rename_is_focused(window) {
                        active_manager.update(cx, |manager, cx| manager.activate_without_focus(cx));
                    } else {
                        active_manager.update(cx, |manager, cx| manager.activate(window, cx));
                    }
                }
                debug_assert_eq!(active_workspace_id, self.workspaces.active_workspace_id());
                self.pending_final_window_closes.remove(&workspace_id);
                if self
                    .rename
                    .as_ref()
                    .is_some_and(|rename| rename.workspace_id == workspace_id)
                {
                    self.rename = None;
                }
                self.sync_terminal_focus_blocker(window, cx);
                self.scroll_active_workspace_into_view();
                cx.notify();
            }
            FinalWindowCloseOutcome::CloseOperatingSystemWindow {
                workspace_id: final_workspace_id,
            } => {
                debug_assert_eq!(final_workspace_id, workspace_id);
                let manager = self.workspaces.active_workspace().payload().clone();
                manager.update(cx, |manager, cx| manager.close_all(cx));
                self.pending_final_window_closes.remove(&workspace_id);
                window.remove_window();
            }
        }
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let was_sidebar_focused =
            self.sidebar_focus.is_focused(window) || self.rename_is_focused(window);
        let sidebar_visible = !self.sidebar_visible;
        self.rename = None;
        self.set_sidebar_layout(sidebar_visible, self.sidebar_width, cx);
        if !sidebar_visible && was_sidebar_focused {
            self.focus(window, cx);
        }
        self.sync_terminal_focus_blocker(window, cx);
    }

    fn toggle_sidebar_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_focus.is_focused(window) || self.rename_is_focused(window) {
            self.rename = None;
            self.focus(window, cx);
            self.sync_terminal_focus_blocker(window, cx);
            cx.notify();
            return;
        }

        if !self.sidebar_visible {
            self.set_sidebar_layout(true, self.sidebar_width, cx);
            cx.defer_in(window, |manager, window, cx| {
                manager.sidebar_focus.focus(window);
                manager.sync_terminal_focus_blocker(window, cx);
            });
            return;
        }

        self.sidebar_focus.focus(window);
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    fn request_workspace_menu(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.sidebar_focus.focus(window);
        self.rename = None;
        self.sync_terminal_focus_blocker(window, cx);
        let activated = self.activate_workspace(workspace_id, window, cx);
        if !activated {
            self.sync_terminal_focus_blocker(window, cx);
            cx.notify();
        }
        activated
    }

    fn handle_workspace_menu_lifecycle(
        &mut self,
        workspace_id: WorkspaceId,
        event: MenuLifecycleEvent,
        cx: &mut Context<Self>,
    ) {
        let blocker = match event {
            MenuLifecycleEvent::Opened => {
                self.workspace_menu = Some(WorkspaceMenuState { workspace_id });
                Some(TerminalFocusBlocker::ContextMenu)
            }
            MenuLifecycleEvent::Closed(reason)
                if self
                    .workspace_menu
                    .is_some_and(|menu| menu.workspace_id == workspace_id) =>
            {
                self.workspace_menu = None;
                Some(if reason == MenuCloseReason::Activated {
                    TerminalFocusBlocker::ContextMenu
                } else {
                    TerminalFocusBlocker::Sidebar
                })
            }
            MenuLifecycleEvent::Closed(_) => return,
        };
        self.workspaces
            .active_workspace()
            .payload()
            .update(cx, |manager, cx| {
                manager.set_parent_focus_blocker(blocker, cx);
            });
        cx.notify();
    }

    fn perform_workspace_menu_command(
        &mut self,
        workspace_id: WorkspaceId,
        command: WorkspaceMenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            WorkspaceMenuCommand::NewWindow => {
                if let Some(workspace) = self.workspaces.workspace(workspace_id) {
                    workspace
                        .payload()
                        .update(cx, |manager, cx| manager.create_window(window, cx));
                }
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            }
            WorkspaceMenuCommand::Rename => {
                let Some(workspace) = self.workspaces.workspace(workspace_id) else {
                    self.sync_terminal_focus_blocker(window, cx);
                    return;
                };
                let input = cx.new(|cx| {
                    TextInput::new(
                        workspace.name(),
                        TextInputStyle::new(
                            gpui_color(ACTIVE_THEME.text).into(),
                            gpui_color(ACTIVE_THEME.text_placeholder).into(),
                            gpui_color(ACTIVE_THEME.players[0].selection).into(),
                            gpui_color(ACTIVE_THEME.players[0].cursor).into(),
                        ),
                        window,
                        cx,
                    )
                });
                let input_id = input.entity_id();
                cx.subscribe_in(
                    &input,
                    window,
                    move |_manager, _, event: &TextInputEvent, window, cx| match event {
                        TextInputEvent::Submitted(value) => {
                            let value = value.clone();
                            cx.defer_in(window, move |manager, window, cx| {
                                manager.finish_rename(input_id, Some(value), true, window, cx);
                            });
                        }
                        TextInputEvent::Cancelled => {
                            cx.defer_in(window, move |manager, window, cx| {
                                manager.finish_rename(input_id, None, true, window, cx);
                            });
                        }
                        TextInputEvent::Blurred(value) => {
                            let value = value.clone();
                            cx.defer_in(window, move |manager, window, cx| {
                                manager.finish_rename(input_id, Some(value), false, window, cx);
                            });
                        }
                        TextInputEvent::Changed(_) => {}
                    },
                )
                .detach();
                self.rename = Some(WorkspaceRenameState {
                    workspace_id,
                    focus_handle: input.read(cx).focus_handle(),
                    input,
                });
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
                cx.defer_in(window, |manager, window, cx| {
                    let Some(rename) = &manager.rename else {
                        return;
                    };
                    let input = rename.input.clone();
                    let focus_handle = rename.focus_handle.clone();
                    input.update(cx, |input, cx| input.select_all(cx));
                    focus_handle.focus(window);
                    manager.sync_terminal_focus_blocker(window, cx);
                });
            }
            WorkspaceMenuCommand::Close => self.close_workspace(workspace_id, window, cx),
        }
    }

    fn finish_rename(
        &mut self,
        input_id: EntityId,
        value: Option<String>,
        restore_sidebar_focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rename) = self.rename.as_ref() else {
            return;
        };
        if rename.input.entity_id() != input_id {
            return;
        }
        let workspace_id = rename.workspace_id;
        if let Some(value) = value
            && let Err(error) = self.workspaces.rename_workspace(workspace_id, value)
        {
            Self::report_workspace_error("rename", error);
        }
        self.rename = None;
        if restore_sidebar_focus {
            self.sidebar_focus.focus(window);
        }
        self.sync_terminal_focus_blocker(window, cx);
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

    fn on_search_workspaces(
        &mut self,
        _: &SearchWorkspaces,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace_search.read(cx).is_open() {
            self.workspace_search
                .update(cx, |palette, cx| palette.open(window, cx));
        } else {
            self.open_workspace_search(window, cx);
        }
    }

    fn on_open_local_project(
        &mut self,
        _: &OpenLocalProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_local_project(window, cx);
    }

    fn on_close_workspace(
        &mut self,
        _: &CloseWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace_id = self.workspaces.active_workspace_id();
        self.close_workspace(workspace_id, window, cx);
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
        let chrome_down_manager = manager.clone();
        let chrome_capture_manager = manager.clone();
        let chrome_move_manager = manager.clone();
        let chrome_up_manager = manager.clone();
        let chrome_out_manager = manager.clone();
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
            .capture_any_mouse_down(move |event, window, cx| {
                if event.button == MouseButton::Left && event.click_count == 1 {
                    let _ = chrome_capture_manager.update(cx, |manager, cx| {
                        manager.set_top_chrome_interaction(true, window, cx);
                    });
                }
            })
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                handle_top_chrome_mouse_down(event, window, cx, |blocked, window, cx| {
                    let _ = chrome_down_manager.update(cx, |manager, cx| {
                        manager.set_top_chrome_interaction(blocked, window, cx);
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
            .on_mouse_up_out(MouseButton::Left, move |_, window, cx| {
                let _ = chrome_up_manager.update(cx, |manager, cx| {
                    manager.set_top_chrome_interaction(false, window, cx);
                });
            })
            .on_mouse_up(MouseButton::Left, move |_, window, cx| {
                let _ = chrome_out_manager.update(cx, |manager, cx| {
                    manager.set_top_chrome_interaction(false, window, cx);
                });
            })
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
                    .absolute()
                    .top(px(SIDEBAR_TOGGLE_INSET))
                    .right(px(SIDEBAR_TOGGLE_INSET))
                    .child(
                        IconButton::new("toggle-sidebar-button", "Toggle Sidebar", |foreground| {
                            Icon::new("sidebar.left")
                                .size(px(14.0))
                                .color(foreground)
                                .into_any_element()
                        })
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Regular)
                        .debug_selector("toggle-sidebar-button")
                        .tooltip(|_, cx| button_theme::tooltip("Toggle Sidebar", cx))
                        .on_activate(move |_, window, cx| {
                            let _ = toggle_manager.update(cx, |manager, cx| {
                                manager.toggle_sidebar(window, cx);
                            });
                        }),
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
        row: WorkspaceRowViewModel,
        manager: WeakEntity<Self>,
    ) -> AnyElement {
        let WorkspaceRowViewModel {
            workspace_id,
            name,
            path,
            tooltip,
            local_project,
            available,
            window_count,
            pane_count,
            active,
        } = row;
        let click_manager = manager.clone();
        let accessibility_name = format!("Workspace actions for {name}");
        let rename = self
            .rename
            .as_ref()
            .filter(|rename| rename.workspace_id == workspace_id);
        let first_line = if let Some(rename) = rename {
            let input = rename.input.clone();
            let focus_handle = rename.focus_handle.clone();
            div()
                .id(("workspace-rename-input", workspace_id.get()))
                .debug_selector(move || format!("workspace-rename-input-{}", workspace_id.get()))
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
                .text_size(px(SIDEBAR_NAME_TEXT_SIZE))
                .text_color(gpui_color(ACTIVE_THEME.text))
                .on_click(move |_, window, cx| {
                    focus_handle.focus(window);
                    cx.stop_propagation();
                })
                .child(input)
                .into_any_element()
        } else {
            div()
                .w_full()
                .truncate()
                .text_size(px(SIDEBAR_NAME_TEXT_SIZE))
                .text_color(gpui_color(if active {
                    ACTIVE_THEME.text_accent
                } else {
                    ACTIVE_THEME.text
                }))
                .child(name)
                .into_any_element()
        };

        let maximum_path_characters = ((f32::from(self.sidebar_width) - 64.0) / 6.0)
            .floor()
            .max(8.0) as usize;
        let tooltip_text = tooltip;

        let row = div()
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
            .block_mouse_except_scroll()
            .when(active, |row| {
                row.bg(gpui_color(ACTIVE_THEME.element_selected))
            })
            .hover(|row| row.bg(gpui_color(ACTIVE_THEME.ghost_element_selected)))
            .tooltip(move |_, cx| {
                cx.new(|_| WorkspaceSidebarTooltip {
                    text: tooltip_text.clone(),
                })
                .into()
            })
            .on_click(move |_, window, cx| {
                let _ = click_manager.update(cx, |manager, cx| {
                    manager.sidebar_focus.focus(window);
                    manager.activate_workspace(workspace_id, window, cx);
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
                    .child(
                        div()
                            .relative()
                            .child(
                                Icon::new(if local_project { "folder" } else { "terminal" })
                                    .size(px(SIDEBAR_ROW_ICON_SIZE))
                                    .color(gpui_color(if available {
                                        if active {
                                            ACTIVE_THEME.icon_accent
                                        } else {
                                            ACTIVE_THEME.icon
                                        }
                                    } else {
                                        ACTIVE_THEME.warning
                                    })),
                            )
                            .when(!available, |icon| {
                                icon.child(
                                    div().absolute().right(px(-5.0)).bottom(px(-4.0)).child(
                                        Icon::new("exclamationmark.triangle.fill")
                                            .size(px(8.0))
                                            .color(gpui_color(ACTIVE_THEME.warning)),
                                    ),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().min_w_0().flex_1().child(first_line))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(SIDEBAR_DETAIL_TEXT_SIZE))
                                    .text_color(gpui_color(ACTIVE_THEME.text_muted))
                                    .child(format!("{window_count}W · {pane_count}P")),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .overflow_hidden()
                            .text_size(px(SIDEBAR_DETAIL_TEXT_SIZE))
                            .text_color(gpui_color(if available {
                                ACTIVE_THEME.text_muted
                            } else {
                                ACTIVE_THEME.warning
                            }))
                            .child(MiddleTruncatedText::new(path, maximum_path_characters)),
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
            .into_any_element();

        let open_manager = manager.clone();
        let lifecycle_manager = manager.clone();
        let activate_manager = manager;
        div()
            .id(("workspace-menu", workspace_id.get()))
            .debug_selector(move || format!("workspace-menu-{}", workspace_id.get()))
            .w_full()
            .flex_shrink_0()
            .child(
                ContextMenu::new(
                    ("workspace-menu-controls", workspace_id.get()),
                    accessibility_name,
                    row,
                    workspace_menu_entries(),
                )
                .size(MenuSize::Small)
                .debug_selector(format!("workspace-menu-controls-{}", workspace_id.get()))
                .on_open_request(move |_, window, cx| {
                    open_manager
                        .update(cx, |manager, cx| {
                            manager.request_workspace_menu(workspace_id, window, cx)
                        })
                        .unwrap_or(false)
                })
                .on_lifecycle(move |event, cx| {
                    let _ = lifecycle_manager.update(cx, |manager, cx| {
                        manager.handle_workspace_menu_lifecycle(workspace_id, *event, cx);
                    });
                })
                .on_activate(move |activation, window, cx| {
                    let command = *activation.action();
                    let _ = activate_manager.update(cx, |manager, cx| {
                        manager.perform_workspace_menu_command(workspace_id, command, window, cx);
                    });
                }),
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
            let (window_count, pane_count) = workspace.payload().read(cx).aggregate_counts(cx);
            let path =
                compact_home_path(workspace.working_directory(), &self.default_workspace_root);
            let (available, tooltip) = match workspace.availability() {
                WorkspaceDirectoryAvailability::Available => {
                    (true, workspace.working_directory().display().to_string())
                }
                WorkspaceDirectoryAvailability::Unavailable { reason } => (
                    false,
                    format!("{}: {reason}", workspace.working_directory().display()),
                ),
            };
            rows = rows.child(self.render_workspace_row(
                WorkspaceRowViewModel {
                    workspace_id: workspace.id(),
                    name: workspace.name().to_owned().into(),
                    path: path.into(),
                    tooltip: tooltip.into(),
                    local_project: matches!(workspace.kind(), WorkspaceKind::LocalProject { .. }),
                    available,
                    window_count,
                    pane_count,
                    active: workspace.id() == active_workspace_id,
                },
                manager.clone(),
            ));
        }

        let scrollbar = self.scrollbar.clone();
        let create_manager = manager.clone();
        let search_manager = manager.clone();
        let picker_manager = manager.clone();
        let header = div()
            .id("workspace-sidebar-header")
            .debug_selector(|| "workspace-sidebar-header".to_owned())
            .w_full()
            .h(px(SIDEBAR_HEADER_HEIGHT))
            .flex_shrink_0()
            .pl(px(SIDEBAR_ROW_HORIZONTAL_PADDING))
            .pr(px(SIDEBAR_HEADER_TRAILING_PADDING))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SIDEBAR_HEADER_ACTION_GAP))
            .border_b(px(CHROME_DIVIDER_SIZE))
            .border_color(gpui_color(ACTIVE_THEME.border))
            .child(
                div()
                    .id("workspace-sidebar-header-title")
                    .debug_selector(|| "workspace-sidebar-header-title".to_owned())
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(px(SIDEBAR_NAME_TEXT_SIZE))
                    .text_color(gpui_color(ACTIVE_THEME.text_muted))
                    .child("Workspaces"),
            )
            .child(
                IconButton::new(
                    "search-workspaces-button",
                    "Search Workspaces",
                    |foreground| {
                        Icon::new("magnifyingglass")
                            .size(px(13.0))
                            .weight(SymbolWeight::Medium)
                            .rendering_mode(RenderingMode::Monochrome)
                            .color(foreground)
                            .into_any_element()
                    },
                )
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Regular)
                .debug_selector("search-workspaces-button")
                .tooltip(|_, cx| button_theme::tooltip("Search Workspaces", cx))
                .on_activate(move |_, window, cx| {
                    let _ = search_manager.update(cx, |manager, cx| {
                        manager.open_workspace_search(window, cx);
                    });
                }),
            )
            .child(
                IconButton::new(
                    "open-local-project-button",
                    "Open Local Project",
                    |foreground| {
                        Icon::new("folder.badge.plus")
                            .size(px(15.0))
                            .weight(SymbolWeight::Medium)
                            .rendering_mode(RenderingMode::Monochrome)
                            .color(foreground)
                            .into_any_element()
                    },
                )
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Regular)
                .debug_selector("open-local-project-button")
                .tooltip(|_, cx| button_theme::tooltip("Open Local Project…", cx))
                .on_activate(move |_, window, cx| {
                    let _ = picker_manager.update(cx, |manager, cx| {
                        manager.open_local_project(window, cx);
                    });
                }),
            );
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
            .child(header)
            .child(rows)
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(NEW_WORKSPACE_BUTTON_HEIGHT))
                    .flex_shrink_0()
                    .child(
                        Button::new("create-workspace-button", "New Workspace")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Large)
                            .shape(ButtonShape::Square)
                            .full_width(true)
                            .debug_selector("create-workspace-button")
                            .leading(|_| {
                                Icon::new("plus")
                                    .size(px(13.0))
                                    .color(gpui_color(ACTIVE_THEME.icon))
                                    .into_any_element()
                            })
                            .trailing(|_| {
                                div()
                                    .text_size(px(10.0))
                                    .text_color(gpui_color(ACTIVE_THEME.icon))
                                    .child("⌘N")
                                    .into_any_element()
                            })
                            .on_activate(move |_, window, cx| {
                                let _ = create_manager.update(cx, |manager, cx| {
                                    manager.create_workspace(window, cx);
                                });
                            }),
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
            .child(scrollbar)
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
}

impl Render for WorkspaceManager {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        debug_assert!(self.workspaces.len() > 0);
        self.sync_terminal_focus_blocker(window, cx);
        let manager = cx.entity().downgrade();
        let release_manager = manager.clone();
        let exit_manager = manager.clone();
        let resize_manager = manager.clone();
        let active_window_manager = self.workspaces.active_workspace().payload().clone();
        if self.sidebar_visible {
            self.sync_scrollbar(cx);
        }
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
            .capture_any_mouse_up(move |event, window, cx| {
                if event.button == MouseButton::Left {
                    let _ = release_manager.update(cx, |manager, cx| {
                        manager.set_top_chrome_interaction(false, window, cx);
                    });
                }
            })
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        window.on_mouse_event(move |_: &MouseExitEvent, phase, window, cx| {
                            if phase == DispatchPhase::Bubble {
                                let _ = exit_manager.update(cx, |manager, cx| {
                                    manager.set_top_chrome_interaction(false, window, cx);
                                });
                            }
                        });
                    },
                )
                .absolute()
                .inset_0(),
            )
            .on_drag_move::<DraggedWorkspaceSidebar>(move |event: &DragMoveEvent<_>, window, cx| {
                let pointer_x = event.event.position.x - event.bounds.origin.x;
                let _ = resize_manager.update(cx, |manager, cx| {
                    manager.resize_sidebar(pointer_x, window, cx);
                });
            })
            .on_action(cx.listener(Self::on_create_workspace))
            .on_action(cx.listener(Self::on_search_workspaces))
            .on_action(cx.listener(Self::on_open_local_project))
            .on_action(cx.listener(Self::on_close_workspace))
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
            .on_action(cx.listener(Self::forward_active_terminal_action::<CopySelection>))
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
            .on_action(cx.listener(Self::forward_active_terminal_action::<OpenTerminalFind>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<FindNext>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<FindPrevious>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<CloseTerminalFind>))
            .child(active_window_manager)
            .child(self.render_top_left_chrome(manager.clone()))
            .when(self.sidebar_visible, |root| {
                root.child(self.render_sidebar(manager, cx))
            })
            .child(self.workspace_search.clone())
    }
}

fn workspace_menu_entries() -> Vec<MenuEntry<WorkspaceMenuCommand>> {
    vec![
        MenuEntry::action("New Window", WorkspaceMenuCommand::NewWindow)
            .shortcut("⌘T")
            .icon(|foreground| {
                Icon::new("plus.rectangle.on.rectangle")
                    .weight(SymbolWeight::Regular)
                    .size(px(14.0))
                    .color(foreground)
                    .into_any_element()
            })
            .debug_selector("workspace-menu-row-new-window"),
        MenuEntry::action("Rename Workspace", WorkspaceMenuCommand::Rename)
            .icon(|foreground| {
                Icon::new("pencil")
                    .weight(SymbolWeight::Regular)
                    .size(px(14.0))
                    .color(foreground)
                    .into_any_element()
            })
            .debug_selector("workspace-menu-row-rename"),
        MenuEntry::separator(),
        MenuEntry::action("Close Workspace", WorkspaceMenuCommand::Close)
            .destructive(true)
            .icon(|foreground| {
                Icon::new("xmark")
                    .weight(SymbolWeight::Regular)
                    .size(px(14.0))
                    .color(foreground)
                    .into_any_element()
            })
            .debug_selector("workspace-menu-row-close"),
    ]
}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
}

fn compact_home_path(path: &std::path::Path, home: &std::path::Path) -> String {
    if path == home {
        return "~".to_owned();
    }
    path.strip_prefix(home)
        .ok()
        .map(|relative| format!("~/{}", relative.display()))
        .unwrap_or_else(|| path.display().to_string())
}

fn initial_workspace_directory(path: PathBuf) -> (ValidatedWorkspaceDirectory, Option<String>) {
    #[cfg(test)]
    {
        (
            ValidatedWorkspaceDirectory::new(path, WorkspaceDirectoryIdentity::new(0, 0)),
            None,
        )
    }
    #[cfg(not(test))]
    {
        match validate_workspace_directory(&path) {
            Ok(directory) => (directory, None),
            Err(error) => (
                ValidatedWorkspaceDirectory::new(path, WorkspaceDirectoryIdentity::new(0, 0)),
                Some(error.to_string()),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    use gpui::{
        Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, VisualTestContext,
        point,
    };

    use super::*;
    use crate::platform::local_project_picker::ScriptedLocalProjectPicker;
    use crate::terminal::testing::{
        RecordedSessionCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
    };
    use crate::terminal::{SessionEvent, SessionExit};

    fn workspace_manager(
        cx: &mut TestAppContext,
    ) -> (
        Entity<WorkspaceManager>,
        TestTerminalSessionRecords,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
        let (manager, cx) = cx.add_window_view(|window, cx| {
            WorkspaceManager::new(session_factory, PathBuf::from("/Users/test"), window, cx)
        });
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.focus(window, cx));
        });
        cx.run_until_parked();
        (manager, records, cx)
    }

    fn workspace_manager_with_picker(
        selections: impl IntoIterator<Item = Result<Option<PathBuf>, String>>,
        cx: &mut TestAppContext,
    ) -> (
        Entity<WorkspaceManager>,
        TestTerminalSessionRecords,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
        let picker: Rc<dyn LocalProjectPicker> =
            Rc::new(ScriptedLocalProjectPicker::new(selections));
        let (manager, cx) = cx.add_window_view(|window, cx| {
            WorkspaceManager::new_with_local_project_picker(
                session_factory,
                PathBuf::from("/Users/test"),
                picker,
                window,
                cx,
            )
        });
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.focus(window, cx));
        });
        cx.run_until_parked();
        (manager, records, cx)
    }

    fn temporary_directory(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("spaceterm-workspace-manager-{name}-{nonce}"))
    }

    #[gpui::test]
    fn cancelled_local_project_picker_should_leave_hierarchy_unchanged(cx: &mut TestAppContext) {
        let (manager, records, cx) = workspace_manager_with_picker([Ok(None)], cx);

        click("open-local-project-button", cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| manager.workspaces.len()),
            1
        );
        assert_eq!(records.starts().len(), 1);
    }

    #[gpui::test]
    fn equivalent_local_project_selections_should_preserve_the_first_path_and_deduplicate(
        cx: &mut TestAppContext,
    ) {
        let root = temporary_directory("project");
        let project = root.join("selected-project");
        let equivalent = root.join("equivalent-project");
        fs::create_dir_all(&project).unwrap();
        symlink(&project, &equivalent).unwrap();
        let selections = [Ok(Some(project.clone())), Ok(Some(equivalent))];
        let (manager, records, cx) = workspace_manager_with_picker(selections, cx);

        click("open-local-project-button", cx);
        click("open-local-project-button", cx);
        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            let workspace = manager.workspaces.active_workspace();
            (
                manager.workspaces.len(),
                workspace.working_directory().to_path_buf(),
                workspace.kind().clone(),
            )
        });
        assert_eq!((state.0, state.1), (2, project.clone()));
        assert!(matches!(state.2, WorkspaceKind::LocalProject { .. }));
        assert!(
            records.starts()[1..]
                .iter()
                .all(|start| start.working_directory == project)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn unavailable_local_project_should_block_children_and_recover_when_restored(
        cx: &mut TestAppContext,
    ) {
        let root = temporary_directory("availability");
        let project = root.join("project");
        let parked = root.join("parked");
        fs::create_dir_all(&project).unwrap();
        let (manager, records, cx) = workspace_manager_with_picker([Ok(Some(project.clone()))], cx);
        click("open-local-project-button", cx);
        assert_eq!(records.starts().len(), 2);

        fs::rename(&project, &parked).unwrap();
        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();
        assert_eq!(records.starts().len(), 2);
        assert!(!manager.read_with(cx, |manager, _| {
            manager
                .workspaces
                .active_workspace()
                .availability()
                .is_available()
        }));

        fs::rename(&parked, &project).unwrap();
        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();
        assert_eq!(records.starts().len(), 3);
        assert!(manager.read_with(cx, |manager, _| {
            manager
                .workspaces
                .active_workspace()
                .availability()
                .is_available()
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn unusable_local_project_selection_should_not_create_a_workspace(cx: &mut TestAppContext) {
        let missing = temporary_directory("missing");
        let (manager, records, cx) = workspace_manager_with_picker([Ok(Some(missing))], cx);

        click("open-local-project-button", cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| manager.workspaces.len()),
            1
        );
        assert_eq!(records.starts().len(), 1);
    }

    #[gpui::test]
    fn workspace_search_should_open_and_block_terminal_input_focus(cx: &mut TestAppContext) {
        let (manager, _, cx) = workspace_manager(cx);

        click("search-workspaces-button", cx);

        let state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.workspace_search_open,
                manager.terminal_focus_blocker(window),
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
            (true, Some(TerminalFocusBlocker::CommandPalette), false)
        );
        assert!(cx.debug_bounds("command-palette-panel").is_some());
    }

    #[gpui::test]
    fn repeated_workspace_search_requests_should_open_only_the_latest_deferred_request(
        cx: &mut TestAppContext,
    ) {
        let (manager, _, cx) = workspace_manager(cx);

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.open_workspace_search(window, cx);
                manager.open_workspace_search(window, cx);
            });
        });
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.workspace_search_open,
                manager.workspace_search_open_request,
                manager.workspace_search.read(cx).generation().value(),
            )
        });
        assert_eq!(state, (true, None, 1));
    }

    #[gpui::test]
    fn deactivation_before_deferred_workspace_search_open_should_clear_the_blocker(
        cx: &mut TestAppContext,
    ) {
        let (manager, _, cx) = workspace_manager(cx);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.open_workspace_search(window, cx);
            });
        });
        cx.deactivate_window();
        cx.run_until_parked();

        let state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.workspace_search_open,
                manager.workspace_search_open_request,
                manager.workspace_search.read(cx).is_open(),
                manager.terminal_focus_blocker(window),
            )
        });
        assert_eq!(state, (false, None, false, None));
    }

    #[gpui::test]
    fn workspace_search_should_replace_an_open_workspace_context_menu(cx: &mut TestAppContext) {
        let (manager, _, cx) = workspace_manager(cx);
        right_click("workspace-row-1-active", cx);
        assert!(cx.debug_bounds("menu-panel-0").is_some());

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.open_workspace_search(window, cx);
            });
        });
        cx.run_until_parked();

        assert!(cx.debug_bounds("menu-panel-0").is_none());
        assert!(cx.debug_bounds("command-palette-panel").is_some());
        assert!(manager.read_with(cx, |manager, _| manager.workspace_menu.is_none()));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        let restored = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.sidebar_focus.is_focused(window),
                manager.terminal_focus_blocker(window),
            )
        });
        assert_eq!(
            restored,
            (true, Some(TerminalFocusBlocker::Sidebar)),
            "closing the replacement palette must not restore the invisible menu focus owner"
        );
    }

    #[gpui::test]
    fn command_p_should_open_workspace_search(cx: &mut TestAppContext) {
        let (manager, _, cx) = workspace_manager(cx);

        cx.simulate_keystrokes("cmd-p");
        cx.run_until_parked();

        assert!(
            manager.read_with(cx, |manager, cx| manager
                .workspace_search
                .read(cx)
                .is_open()),
            "cmd-p did not open Workspace search"
        );
        assert!(cx.debug_bounds("command-palette-panel").is_some());

        // A second press while open must not reopen and clear the query.
        cx.simulate_keystrokes("a");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-p");
        cx.run_until_parked();

        assert_eq!(
            manager.read_with(cx, |manager, cx| manager
                .workspace_search
                .read(cx)
                .query()
                .to_owned()),
            "a",
            "a repeated cmd-p reopened the palette and discarded the query"
        );
    }

    #[gpui::test]
    #[gpui::test]
    fn workspace_search_rows_should_carry_the_sidebar_path_and_counts(cx: &mut TestAppContext) {
        let (manager, _, cx) = workspace_manager(cx);
        click("search-workspaces-button", cx);

        let items = manager.read_with(cx, |manager, cx| manager.workspace_search_items(cx));
        let active = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
        let item = items
            .iter()
            .find(|item| *item.id() == active)
            .expect("the Active Workspace was missing from the search items");

        assert!(
            item.description_text().is_some_and(|path| !path.is_empty()),
            "a Workspace search row carried no path: {:?}",
            item.description_text()
        );
        let selector: &'static str =
            Box::leak(format!("workspace-search-result-{}", active.get()).into_boxed_str());
        assert!(
            cx.debug_bounds(selector).is_some(),
            "the Workspace search row was not rendered"
        );
    }

    #[gpui::test]
    fn workspace_search_should_filter_workspace_names_case_insensitively(cx: &mut TestAppContext) {
        let (manager, _, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n cmd-n");
        cx.run_until_parked();
        manager.update(cx, |manager, cx| {
            manager
                .workspaces
                .rename_workspace(WorkspaceId::new(1), "ALPHA WORKSPACE".to_owned())
                .unwrap();
            manager
                .workspaces
                .rename_workspace(WorkspaceId::new(2), "Beta Workspace".to_owned())
                .unwrap();
            manager
                .workspaces
                .rename_workspace(WorkspaceId::new(3), "Gamma Workspace".to_owned())
                .unwrap();
            cx.notify();
        });

        click("search-workspaces-button", cx);
        let palette = manager.read_with(cx, |manager, _| manager.workspace_search.clone());
        assert_eq!(
            palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
            Some(WorkspaceId::new(1)),
            "the first Workspace should be selected for an empty query"
        );

        cx.simulate_keystrokes("a l p h a");
        cx.run_until_parked();

        assert_eq!(
            palette.read_with(cx, |palette, _| {
                (
                    palette.query().to_owned(),
                    palette.selected_item_id().copied(),
                )
            }),
            ("alpha".to_owned(), Some(WorkspaceId::new(1)))
        );
        assert!(cx.debug_bounds("workspace-search-result-1").is_some());
    }

    #[gpui::test]
    fn workspace_search_should_show_no_match_state_without_matching_workspace_paths(
        cx: &mut TestAppContext,
    ) {
        let (manager, _, cx) = workspace_manager(cx);

        click("search-workspaces-button", cx);
        cx.simulate_keystrokes("u s e r s");
        cx.run_until_parked();

        assert!(cx.debug_bounds("command-palette-no-results").is_some());
        let palette = manager.read_with(cx, |manager, _| manager.workspace_search.clone());
        assert_eq!(
            palette.read_with(cx, |palette, _| palette.selected_item_id().copied()),
            None
        );
    }

    #[gpui::test]
    fn workspace_search_escape_should_restore_terminal_focus(cx: &mut TestAppContext) {
        let (manager, _, cx) = workspace_manager(cx);
        let terminal_was_focused = cx.update(|window, cx| {
            manager
                .read(cx)
                .workspaces
                .active_workspace()
                .payload()
                .read(cx)
                .focused_terminal_is_focused(window, cx)
        });

        click("search-workspaces-button", cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        let restored = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.workspace_search_open,
                manager.terminal_focus_blocker(window),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        });
        assert_eq!(
            (terminal_was_focused, restored),
            (true, (false, None, true))
        );
        assert!(cx.debug_bounds("command-palette-panel").is_none());
    }

    #[gpui::test]
    fn workspace_search_selection_should_activate_the_matching_workspace(cx: &mut TestAppContext) {
        let (manager, _, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        manager.update(cx, |manager, cx| {
            manager
                .workspaces
                .rename_workspace(WorkspaceId::new(1), "Alpha Workspace".to_owned())
                .unwrap();
            manager
                .workspaces
                .rename_workspace(WorkspaceId::new(2), "Beta Workspace".to_owned())
                .unwrap();
            cx.notify();
        });

        click("search-workspaces-button", cx);
        cx.simulate_keystrokes("a l p h a enter");
        cx.run_until_parked();

        let state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.workspaces.active_workspace_id(),
                manager.workspace_search_open,
                manager.terminal_focus_blocker(window),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        });
        assert_eq!(state, (WorkspaceId::new(1), false, None, true));
    }

    #[gpui::test]
    fn sidebar_header_should_align_its_title_and_actions_with_the_surrounding_chrome(
        cx: &mut TestAppContext,
    ) {
        let (_manager, _, cx) = workspace_manager(cx);

        let header = cx
            .debug_bounds("workspace-sidebar-header")
            .expect("the Workspace sidebar header was not rendered");
        let title = cx
            .debug_bounds("workspace-sidebar-header-title")
            .expect("the Workspace sidebar header title was not rendered");
        let search = cx
            .debug_bounds("search-workspaces-button")
            .expect("the Search Workspaces button was not rendered");
        let picker = cx
            .debug_bounds("open-local-project-button")
            .expect("the Open Local Project button was not rendered");
        let active_row = cx
            .debug_bounds("workspace-row-1-active")
            .expect("the Active Workspace row was not rendered");
        let toggle = cx
            .debug_bounds("toggle-sidebar-button")
            .expect("the sidebar toggle was not rendered");

        assert_eq!(header.size.height, px(SIDEBAR_HEADER_HEIGHT));
        // Read the action height back from the painted button so a change to the shared button
        // theme cannot silently eat the header's vertical breathing room.
        assert_eq!(
            header.size.height,
            picker.size.height + px(SIDEBAR_HEADER_ACTION_PADDING * 2.0),
            "the header no longer leaves even breathing room above and below its actions"
        );
        assert_eq!(
            title.origin.x,
            active_row.origin.x + px(SIDEBAR_ROW_HORIZONTAL_PADDING)
        );
        assert!(
            title.origin.x + title.size.width <= search.origin.x,
            "the header title overlapped the Search Workspaces button: {title:?} {search:?}"
        );
        assert!(
            search.origin.x + search.size.width <= picker.origin.x,
            "the header actions were out of order: {search:?} {picker:?}"
        );
        assert_eq!(
            picker.origin.x + picker.size.width,
            toggle.origin.x + toggle.size.width,
            "the header actions were not flush with the sidebar toggle above them"
        );
        assert!(
            header.origin.y + header.size.height <= active_row.origin.y,
            "the header overlapped the first Workspace row: {header:?} {active_row:?}"
        );
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
    fn workspace_top_chrome_should_restore_after_release_outside_its_hitbox(
        cx: &mut TestAppContext,
    ) {
        let (_manager, records, cx) = workspace_manager(cx);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let command_count = records.commands().len();
        let chrome = cx
            .debug_bounds("workspace-top-chrome")
            .expect("Workspace top chrome must be rendered")
            .center();
        let outside = cx
            .debug_bounds("window-manager-content")
            .expect("Window content must be rendered")
            .center();

        cx.simulate_mouse_down(chrome, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        let focus_edges = records
            .commands()
            .into_iter()
            .skip(command_count)
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Focus(focused) => Some((call.session_id, focused)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(focus_edges, [(1, false), (1, true)]);
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
    fn sidebar_buttons_should_toggle_sidebar_and_create_workspace(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);

        click("toggle-sidebar-button", cx);
        assert!(!manager.read_with(cx, |manager, _| manager.sidebar_visible));

        cx.simulate_keystrokes("cmd-b");
        cx.run_until_parked();
        click("create-workspace-button", cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| {
                (
                    manager.workspaces.len(),
                    manager.workspaces.active_workspace_id(),
                )
            }),
            (2, WorkspaceId::new(2))
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
            thumb.size.width > px(0.0)
                && thumb.size.height > px(0.0)
                && thumb.size.height < list.size.height,
            "the revealed Workspace scrollbar had unexpected bounds: {thumb:?}"
        );
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
            manager.reveal_scrollbar(cx);
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
            manager.workspace_list_scroll_handle.offset().y
        });
        assert!(
            state < px(0.0),
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
    fn command_n_should_create_and_activate_a_default_root_workspace(cx: &mut TestAppContext) {
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
                records.dropped_session_ids(),
                records
                    .starts()
                    .into_iter()
                    .map(|start| start.working_directory)
                    .collect::<Vec<_>>(),
            )
        });
        assert_eq!(
            state,
            (
                2,
                WorkspaceId::new(2),
                PathBuf::from("/Users/test"),
                Vec::new(),
                vec![PathBuf::from("/Users/test"), PathBuf::from("/Users/test")],
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
            (WorkspaceId::new(1), false, Some(WorkspaceId::new(1)), false)
        );
        assert!(cx.debug_bounds("menu-panel-0").is_some());

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        let dismissed = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.workspace_menu,
                manager.sidebar_focus.is_focused(window),
                manager.terminal_focus_blocker(window),
            )
        });
        assert_eq!(dismissed, (None, true, Some(TerminalFocusBlocker::Sidebar)));
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
    fn command_w_from_sidebar_should_close_the_globally_final_operating_system_window(
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
                records.dropped_session_ids(),
                records.session_count(),
            )
        });
        assert_eq!(state, (1, WorkspaceId::new(1), vec![1], 1));
        assert!(cx.windows().is_empty());
    }

    #[gpui::test]
    fn duplicate_final_window_close_requests_should_schedule_one_operating_system_window_close(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let window_manager = manager.read_with(cx, |manager, _| {
            manager.workspaces.active_workspace().payload().clone()
        });

        window_manager.update(cx, |_, cx| {
            cx.emit(WindowManagerEvent::FinalWindowCloseRequested {
                final_window_id: crate::domain::WindowId::new(1),
            });
            cx.emit(WindowManagerEvent::FinalWindowCloseRequested {
                final_window_id: crate::domain::WindowId::new(1),
            });
        });
        cx.run_until_parked();

        assert_eq!(records.dropped_session_ids(), vec![1]);
        assert!(cx.windows().is_empty());
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
                records.dropped_session_ids(),
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
        cx.simulate_keystrokes("cmd-a D e v enter");
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
                manager.rename_is_focused(window),
                manager.sidebar_focus.is_focused(window),
                manager.rename.is_some(),
            )
        });
        cx.simulate_keystrokes("cmd-a D e v enter");
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
    fn blurring_inline_rename_should_commit_the_edited_name(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-rename", cx);
        cx.simulate_keystrokes("cmd-a D e v");

        let sidebar_focus = manager.read_with(cx, |manager, _| manager.sidebar_focus.clone());
        cx.update(|window, _| sidebar_focus.focus(window));
        cx.run_until_parked();

        let rename_state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.active_workspace().name().to_owned(),
                manager.rename.is_none(),
            )
        });
        assert_eq!(rename_state, ("Dev".to_owned(), true));
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
                records.dropped_session_ids(),
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
                "Default".to_owned(),
                "zsh",
                "zsh · 2 Windows",
                Vec::new(),
            )
        );
    }

    #[gpui::test]
    fn explicitly_closing_the_final_workspace_should_replace_it_and_keep_the_window_open(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);

        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-close", cx);

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                records.dropped_session_ids(),
                records.session_count(),
            )
        });
        assert_eq!(state, (1, WorkspaceId::new(2), vec![1], 2));
        assert_eq!(cx.windows().len(), 1);
    }

    #[gpui::test]
    fn inactive_shell_exit_should_close_its_workspace_without_stealing_activation(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let inactive_sender = records
            .event_sender(1)
            .expect("the initial Workspace terminal session must have started");
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();

        inactive_sender
            .try_send(SessionEvent::Exited(SessionExit::Success))
            .expect("the inactive shell exit must be delivered");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (2, WorkspaceId::new(3), vec![1]));
    }
}
