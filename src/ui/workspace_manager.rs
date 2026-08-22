use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Action, AnyElement, App, Context, DispatchPhase, DragMoveEvent, Empty, Entity, EntityId,
    FocusHandle, MouseButton, MouseDownEvent, MouseExitEvent, Pixels, Render, ScrollHandle,
    ScrollWheelEvent, SharedString, WeakEntity, Window, canvas, div, point, px, rgba,
};
use gpui_symbols::{Icon, SymbolWeight};
use spaceterm_ui::{TextInput, TextInputEvent, TextInputStyle};
use thiserror::Error;

use super::overlay_scrollbar::{OverlayScrollbar, OverlayScrollbarEvent, ScrollMetrics};
use super::terminal_focus::TerminalFocusBlocker;
use super::{
    ActivateWindow1, ActivateWindow2, ActivateWindow3, ActivateWindow4, ActivateWindow5,
    ActivateWindow6, ActivateWindow7, ActivateWindow8, ActivateWindow9, ActivateWorkspace1,
    ActivateWorkspace2, ActivateWorkspace3, ActivateWorkspace4, ActivateWorkspace5,
    ActivateWorkspace6, ActivateWorkspace7, ActivateWorkspace8, ActivateWorkspace9, ClosePane,
    CloseTerminalFind, CloseWindow, CloseWorkspace, CopySelection, CreateWindow, CreateWorkspace,
    ExportTerminalDiagnostics, FindNext, FindPrevious, FocusPaneDown, FocusPaneLeft,
    FocusPaneRight, FocusPaneUp, OpenLocalProject, OpenTerminalFind, SplitDown, SplitRight,
    TERMINAL_KEY_CONTEXT, TOP_CHROME_HEIGHT, TogglePaneZoom, ToggleSidebar, ToggleSidebarFocus,
    WORKSPACE_SIDEBAR_DEFAULT_WIDTH, WORKSPACE_SIDEBAR_MINIMUM_WIDTH, WindowManager,
    WindowManagerEvent, handle_top_chrome_mouse_down,
};
use crate::domain::{
    CloseWorkspaceOutcome, DirectoryValidity, FinalWindowCloseOutcome, OpenLocalProjectOutcome,
    PaneId, WindowId, WorkspaceCollection, WorkspaceError, WorkspaceId, WorkspaceKind,
    WorkspaceLaunch,
};
use crate::platform::macos_directory_picker::{LocalProjectPicker, MacosDirectoryPicker};
use crate::platform::{canonical_identity, is_valid_local_directory};
use crate::terminal::{
    NativeServiceOrigin, NativeServiceStatus, SelectionCopy, TerminalSessionFactory,
    WorkspaceTerminalSessionFactory,
};
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
const WORKSPACE_MENU_WIDTH: f32 = 208.0;
const WORKSPACE_MENU_ROW_HEIGHT: f32 = 28.0;
const WORKSPACE_MENU_SEPARATOR_SIZE: f32 = 1.0;
const WORKSPACE_MENU_BORDER_SIZE: f32 = 1.0;
const WORKSPACE_MENU_HEIGHT: f32 = WORKSPACE_MENU_ROW_HEIGHT * 3.0
    + WORKSPACE_MENU_SEPARATOR_SIZE
    + WORKSPACE_MENU_BORDER_SIZE * 2.0;
const WORKSPACE_MENU_INSET: f32 = 4.0;
const WORKSPACE_MENU_CORNER_RADIUS: f32 = 8.0;
const CREATION_BLOCK_NOTICE: &str = "Cannot create: Workspace directory is unavailable";
const CREATION_BLOCK_NOTICE_LIFETIME: Duration = Duration::from_secs(4);
const CREATION_BLOCK_NOTICE_VERTICAL_PADDING: f32 = 6.0;
const OPEN_LOCAL_PROJECT_PROMPT: &str = "Open Local Project";
const UNUSABLE_PROJECT_NOTICE: &str = "That folder can't be opened as a project";
const SEARCH_PLACEHOLDER: &str = "Search Workspaces";
const SIDEBAR_HEADER_VERTICAL_PADDING: f32 = 8.0;
const SIDEBAR_SEARCH_HEIGHT: f32 = 26.0;
const SIDEBAR_COUNTS_TEXT_SIZE: f32 = 10.0;
/// Deterministic character budget for the secondary Workspace Directory line;
/// longer displays engage middle truncation.
const SECONDARY_PATH_CHARACTER_BUDGET: usize = 48;
const MIDDLE_TRUNCATION_ELLIPSIS: char = '…';

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("the Workspace directory is unavailable")]
pub(crate) struct WorkspaceDirectoryUnavailable;

/// Source of the exact directory the next Terminal Session must start in.
pub(crate) trait WorkspaceDirectorySource {
    fn resolve(&self) -> Result<PathBuf, WorkspaceDirectoryUnavailable>;
}

#[derive(Clone)]
struct WorkspaceDirectoryState {
    available: bool,
    directory: PathBuf,
}

/// Live view of one Workspace's launch directory shared with its Window
/// Manager and Terminal Session factory. The owning WorkspaceManager keeps it
/// synchronized with the collection so Terminal Session creation can resolve
/// without a GPUI context.
#[derive(Clone)]
pub(crate) struct DynamicWorkspaceDirectorySource {
    state: Rc<RefCell<WorkspaceDirectoryState>>,
}

impl DynamicWorkspaceDirectorySource {
    pub(crate) fn available(directory: PathBuf) -> Self {
        Self {
            state: Rc::new(RefCell::new(WorkspaceDirectoryState {
                available: true,
                directory,
            })),
        }
    }

    fn set_validity(&self, validity: DirectoryValidity) {
        self.state.borrow_mut().available = validity == DirectoryValidity::Valid;
    }

    /// Mirrors an authoritative directory change from the domain collection.
    fn set_directory(&self, directory: PathBuf) {
        self.state.borrow_mut().directory = directory;
    }
}

impl WorkspaceDirectorySource for DynamicWorkspaceDirectorySource {
    fn resolve(&self) -> Result<PathBuf, WorkspaceDirectoryUnavailable> {
        let state = self.state.borrow();
        if !state.available {
            return Err(WorkspaceDirectoryUnavailable);
        }
        Ok(state.directory.clone())
    }
}

fn validity_of_path(directory: &Path) -> DirectoryValidity {
    if is_valid_local_directory(directory) {
        DirectoryValidity::Valid
    } else {
        DirectoryValidity::Invalid
    }
}

struct WorkspaceRowPresentation {
    workspace_id: WorkspaceId,
    kind: WorkspaceKind,
    name: SharedString,
    counts: SharedString,
    path: SharedString,
    path_truncated: bool,
    directory_available: bool,
    active: bool,
}

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

struct WorkspaceRenameState {
    workspace_id: WorkspaceId,
    input: Entity<TextInput>,
    focus_handle: FocusHandle,
}

#[derive(Clone, Copy)]
struct DraggedWorkspaceSidebar;

pub(crate) struct WorkspaceManager {
    workspaces: WorkspaceCollection<Entity<WindowManager>>,
    session_factory: Rc<dyn TerminalSessionFactory>,
    default_workspace_root: PathBuf,
    directory_gates: BTreeMap<WorkspaceId, DynamicWorkspaceDirectorySource>,
    /// Directory Authority per Ad Hoc Workspace: the (Window, Pane) pair whose
    /// Reported Working Directory updates the Workspace-owned directory.
    authority: BTreeMap<WorkspaceId, (WindowId, PaneId)>,
    creation_block_notice: Option<String>,
    creation_block_generation: u64,
    project_picker: Box<dyn LocalProjectPicker>,
    /// Exactly one native Local Project selection may be in flight.
    project_picker_open: bool,
    sidebar_visible: bool,
    sidebar_width: Pixels,
    workspace_list_scroll_handle: ScrollHandle,
    scrollbar: Entity<OverlayScrollbar<f32>>,
    sidebar_focus: FocusHandle,
    search: Entity<TextInput>,
    search_focus: FocusHandle,
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
        Self::new_with_project_picker(
            session_factory,
            default_workspace_root,
            Box::new(MacosDirectoryPicker),
            window,
            cx,
        )
    }

    pub(crate) fn new_with_project_picker(
        session_factory: Rc<dyn TerminalSessionFactory>,
        default_workspace_root: PathBuf,
        project_picker: Box<dyn LocalProjectPicker>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut directory_gates = BTreeMap::new();
        let workspaces = WorkspaceCollection::new(
            default_workspace_root.clone(),
            |workspace_id, workspace_root| {
                let directory_gate =
                    DynamicWorkspaceDirectorySource::available(workspace_root.to_path_buf());
                directory_gates.insert(workspace_id, directory_gate.clone());
                let factory_directory_gate = directory_gate.clone();
                Self::create_window_manager(
                    workspace_id,
                    WorkspaceTerminalSessionFactory::dynamic(
                        Rc::clone(&session_factory),
                        move || factory_directory_gate.resolve().ok(),
                    ),
                    Rc::new(directory_gate),
                    true,
                    px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
                    window,
                    cx,
                )
            },
        );
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
                manager.set_top_chrome_interaction(false, window, cx);
            }
        })
        .detach();

        let search = cx.new(|cx| {
            TextInput::new(
                "",
                TextInputStyle::new(
                    gpui_color(ACTIVE_THEME.text).into(),
                    gpui_color(ACTIVE_THEME.text_placeholder).into(),
                    gpui_color(ACTIVE_THEME.players[0].selection).into(),
                    gpui_color(ACTIVE_THEME.players[0].cursor).into(),
                ),
                window,
                cx,
            )
            .placeholder(SEARCH_PLACEHOLDER)
        });
        let search_focus = search.read(cx).focus_handle();
        // Gaining search focus cancels any active inline rename, mirroring how
        // other sidebar interactions dismiss rename editing.
        cx.on_focus(&search_focus, window, |manager, window, cx| {
            manager.rename = None;
            manager.sync_terminal_focus_blocker(window, cx);
            cx.notify();
        })
        .detach();
        cx.subscribe_in(
            &search,
            window,
            |_, _, event: &TextInputEvent, _, cx| match event {
                TextInputEvent::Changed(_) => {
                    // TODO(issue): filter the Workspace rows by this query once
                    // Workspace search ships; typing is accepted but inert today.
                    cx.notify();
                }
                TextInputEvent::Submitted(_)
                | TextInputEvent::Cancelled
                | TextInputEvent::Blurred(_) => {}
            },
        )
        .detach();

        let mut workspace_manager = Self {
            workspaces,
            session_factory,
            default_workspace_root,
            directory_gates,
            authority: BTreeMap::new(),
            creation_block_notice: None,
            creation_block_generation: 0,
            project_picker,
            project_picker_open: false,
            sidebar_visible: true,
            sidebar_width: px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            workspace_list_scroll_handle: ScrollHandle::new(),
            scrollbar,
            sidebar_focus: cx.focus_handle(),
            search,
            search_focus,
            workspace_menu: None,
            rename: None,
            top_chrome_interaction: false,
            top_chrome_move_requested: false,
            pending_final_window_closes: BTreeSet::new(),
        };
        let initial_workspace_id = workspace_manager.workspaces.active_workspace_id();
        workspace_manager.initialize_workspace_authority(initial_workspace_id, cx);
        workspace_manager
    }

    fn create_window_manager(
        workspace_id: WorkspaceId,
        session_factory: WorkspaceTerminalSessionFactory,
        directory_gate: Rc<dyn WorkspaceDirectorySource>,
        sidebar_visible: bool,
        sidebar_width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<WindowManager> {
        let manager = cx.new(|cx| {
            let mut manager = WindowManager::new(session_factory, directory_gate, window, cx);
            manager.set_sidebar_layout(sidebar_visible, sidebar_width, cx);
            manager
        });
        cx.subscribe_in(
            &manager,
            window,
            move |workspace_manager, _, event: &WindowManagerEvent, window, cx| match event {
                WindowManagerEvent::ChildCreationBlocked => {
                    workspace_manager.show_creation_block_notice(cx);
                }
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
                WindowManagerEvent::PaneReportedDirectoryChanged { window_id, pane_id } => {
                    workspace_manager.handle_reported_directory_changed(
                        workspace_id,
                        *window_id,
                        *pane_id,
                        cx,
                    );
                }
                WindowManagerEvent::PaneClosed {
                    window_id,
                    closed_pane_id,
                } => {
                    workspace_manager.recompute_authority_after_close(
                        workspace_id,
                        *window_id,
                        Some(*closed_pane_id),
                        cx,
                    );
                }
                WindowManagerEvent::WindowClosed { window_id } => {
                    workspace_manager.recompute_authority_after_close(
                        workspace_id,
                        *window_id,
                        None,
                        cx,
                    );
                }
            },
        )
        .detach();
        manager
    }

    fn report_workspace_error(operation: &str, error: WorkspaceError) {
        eprintln!("failed to {operation} Workspace: {error}");
    }

    fn show_creation_block_notice(&mut self, cx: &mut Context<Self>) {
        self.show_transient_notice(CREATION_BLOCK_NOTICE.to_owned(), cx);
    }

    fn show_unusable_project_notice(&mut self, cx: &mut Context<Self>) {
        self.show_transient_notice(UNUSABLE_PROJECT_NOTICE.to_owned(), cx);
    }

    fn show_transient_notice(&mut self, notice: String, cx: &mut Context<Self>) {
        self.creation_block_generation += 1;
        self.creation_block_notice = Some(notice);
        cx.notify();
        let generation = self.creation_block_generation;
        cx.spawn(async move |manager, cx| {
            cx.background_executor()
                .timer(CREATION_BLOCK_NOTICE_LIFETIME)
                .await;
            manager
                .update(cx, |manager, cx| {
                    if manager.creation_block_generation == generation {
                        manager.creation_block_notice = None;
                        cx.notify();
                    }
                })
                .ok();
        })
        .detach();
    }

    /// Records the initial Directory Authority (first Window's first Pane) for
    /// a freshly materialized Ad Hoc Workspace. Missing transient state is
    /// tolerated: lookups treat a missing entry as "no authority".
    fn initialize_workspace_authority(&mut self, workspace_id: WorkspaceId, cx: &App) {
        if self.workspaces.kind(workspace_id) != Ok(WorkspaceKind::AdHoc) {
            return;
        }
        let Some(manager) = self
            .workspaces
            .workspace(workspace_id)
            .map(|workspace| workspace.payload().clone())
        else {
            return;
        };
        if let Some(authority) =
            manager.read_with(cx, |manager, app| manager.first_authority_pane(app))
        {
            self.authority.insert(workspace_id, authority);
        }
    }

    /// Adopts an authoritative Pane's Reported Working Directory into its
    /// Workspace regardless of Active state.
    fn handle_reported_directory_changed(
        &mut self,
        workspace_id: WorkspaceId,
        window_id: WindowId,
        pane_id: PaneId,
        cx: &mut Context<Self>,
    ) {
        if self.workspaces.kind(workspace_id) != Ok(WorkspaceKind::AdHoc)
            || self.authority.get(&workspace_id) != Some(&(window_id, pane_id))
        {
            return;
        }
        let Some(workspace) = self.workspaces.workspace(workspace_id) else {
            return;
        };
        let manager = workspace.payload().clone();
        let Some(directory) = manager.read_with(cx, |manager, app| {
            manager.pane_reported_directory(window_id, pane_id, app)
        }) else {
            return;
        };
        let validity = validity_of_path(&directory);
        match self
            .workspaces
            .adopt_reported_directory(workspace_id, &directory, validity)
        {
            Ok(true) => {
                if let Some(gate) = self.directory_gates.get(&workspace_id) {
                    gate.set_directory(directory.clone());
                    gate.set_validity(DirectoryValidity::Valid);
                }
                cx.notify();
            }
            Ok(false) => {}
            Err(error) => {
                Self::report_workspace_error("adopt Reported Working Directory", error);
            }
        }
    }

    /// Repromotes Directory Authority after the authoritative Window or Pane
    /// closed, adopting the promoted Pane's Reported Working Directory when it
    /// is valid. Safe to invoke from both close paths; only the first matching
    /// call has an effect.
    fn recompute_authority_after_close(
        &mut self,
        workspace_id: WorkspaceId,
        closed_window: WindowId,
        closed_pane: Option<PaneId>,
        cx: &mut Context<Self>,
    ) {
        let matches_authority = self
            .authority
            .get(&workspace_id)
            .is_some_and(|&(window, pane)| {
                window == closed_window && closed_pane.is_none_or(|closed| closed == pane)
            });
        if !matches_authority {
            return;
        }
        let Some(workspace) = self.workspaces.workspace(workspace_id) else {
            self.authority.remove(&workspace_id);
            return;
        };
        let manager = workspace.payload().clone();
        let (successor, promoted_directory) = manager.read_with(cx, |manager, app| {
            let successor: Option<(WindowId, PaneId)> = if manager.contains_window(closed_window) {
                // Final-Pane close escalates without removing the Pane from its
                // Pane Layout, so skip it here and let the WindowClosed report
                // promote across Windows.
                manager
                    .first_pane_in_layout_order(closed_window, app)
                    .map(|pane| (closed_window, pane))
                    .filter(|&(_, pane)| Some(pane) != closed_pane)
            } else {
                manager.first_authority_pane(app)
            };
            let directory = successor
                .and_then(|(window, pane)| manager.pane_reported_directory(window, pane, app));
            (successor, directory)
        });

        match successor {
            Some(successor) => {
                let promoted = promoted_directory
                    .as_deref()
                    .map(|directory| (directory, validity_of_path(directory)));
                if let Err(error) = self
                    .workspaces
                    .promote_authority_directory(workspace_id, promoted)
                {
                    Self::report_workspace_error("promote Directory Authority", error);
                }
                if let Some((directory, DirectoryValidity::Valid)) = promoted
                    && let Some(gate) = self.directory_gates.get(&workspace_id)
                {
                    gate.set_directory(directory.to_path_buf());
                }
                self.authority.insert(workspace_id, successor);
            }
            None => {
                if !manager.read_with(cx, |manager, _| manager.contains_window(closed_window)) {
                    self.authority.remove(&workspace_id);
                }
            }
        }
        cx.notify();
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
        self.top_chrome_interaction
            .then_some(TerminalFocusBlocker::TopChrome)
            .or(self
                .rename
                .as_ref()
                .map(|_| TerminalFocusBlocker::RenameField))
            .or(self
                .search_focus
                .is_focused(window)
                .then_some(TerminalFocusBlocker::SearchField))
            .or(self
                .workspace_menu
                .map(|_| TerminalFocusBlocker::ContextMenu))
            .or(self
                .sidebar_focus
                .is_focused(window)
                .then_some(TerminalFocusBlocker::Sidebar))
    }

    fn rename_is_focused(&self, window: &Window) -> bool {
        self.rename
            .as_ref()
            .is_some_and(|rename| rename.focus_handle.is_focused(window))
    }

    fn search_is_focused(&self, window: &Window) -> bool {
        self.search_focus.is_focused(window)
    }

    /// Focuses the Workspace search input, cancelling any active inline
    /// rename first so two sidebar editors never compete for input.
    fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cancelled_rename = self.rename.take().is_some();
        self.search_focus.focus(window);
        self.sync_terminal_focus_blocker(window, cx);
        if cancelled_rename {
            cx.notify();
        }
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
            let was_sidebar_focused = self.sidebar_focus.is_focused(window)
                || self.rename_is_focused(window)
                || self.search_is_focused(window);
            self.workspace_menu = None;
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
        let mut directory_gates = std::mem::take(&mut self.directory_gates);
        let result = self.workspaces.create_workspace(
            WorkspaceLaunch::AdHoc {
                home: self.default_workspace_root.clone(),
            },
            |workspace_id, workspace_root| {
                let directory_gate =
                    DynamicWorkspaceDirectorySource::available(workspace_root.to_path_buf());
                directory_gates.insert(workspace_id, directory_gate.clone());
                let factory_directory_gate = directory_gate.clone();
                Self::create_window_manager(
                    workspace_id,
                    WorkspaceTerminalSessionFactory::dynamic(
                        Rc::clone(&session_factory),
                        move || factory_directory_gate.resolve().ok(),
                    ),
                    Rc::new(directory_gate),
                    sidebar_visible,
                    sidebar_width,
                    window,
                    cx,
                )
            },
        );
        self.directory_gates = directory_gates;
        let workspace_id = match result {
            Ok(workspace_id) => workspace_id,
            Err(error) => {
                Self::report_workspace_error("create", error);
                return;
            }
        };
        let Some(next_manager) = self
            .workspaces
            .workspace(workspace_id)
            .map(|workspace| workspace.payload().clone())
        else {
            unreachable!("a newly created Workspace must remain owned by its collection")
        };
        self.initialize_workspace_authority(workspace_id, cx);
        self.present_activated_workspace(previous_manager, next_manager, window, cx);
    }

    /// Deactivates the previously Active Workspace, presents `next_manager`,
    /// and refreshes the sidebar around the new Active Workspace.
    fn present_activated_workspace(
        &mut self,
        previous_manager: Entity<WindowManager>,
        next_manager: Entity<WindowManager>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        previous_manager.update(cx, |manager, cx| manager.deactivate(cx));
        next_manager.update(cx, |manager, cx| manager.activate(window, cx));
        self.workspace_menu = None;
        self.rename = None;
        self.sync_terminal_focus_blocker(window, cx);
        self.scroll_active_workspace_into_view();
        cx.notify();
    }

    /// Runs one native Local Project selection and applies the chosen
    /// directory through the domain. Cancelling or an unusable selection
    /// leaves the hierarchy unchanged.
    pub(crate) fn open_local_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_picker_open {
            return;
        }
        self.project_picker_open = true;
        let selection = self
            .project_picker
            .pick_local_project_directory(OPEN_LOCAL_PROJECT_PROMPT.into(), cx);
        cx.spawn_in(window, async move |workspace_manager, cx| {
            let Some(selected_path) = selection.await else {
                workspace_manager
                    .update(cx, |workspace_manager, cx| {
                        workspace_manager.project_picker_open = false;
                        cx.notify();
                    })
                    .ok();
                return;
            };
            workspace_manager
                .update_in(cx, |workspace_manager, window, cx| {
                    workspace_manager.open_selected_local_project(selected_path, window, cx);
                })
                .ok();
        })
        .detach();
    }

    fn on_open_local_project(
        &mut self,
        _: &OpenLocalProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_local_project(window, cx);
    }

    fn open_selected_local_project(
        &mut self,
        selected_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.project_picker_open = false;
        let Some(identity) =
            canonical_identity(&selected_path).filter(|_| is_valid_local_directory(&selected_path))
        else {
            self.show_unusable_project_notice(cx);
            return;
        };

        let previous_manager = self.workspaces.active_workspace().payload().clone();
        let session_factory = Rc::clone(&self.session_factory);
        let sidebar_visible = self.sidebar_visible;
        let sidebar_width = self.sidebar_width;
        let mut directory_gates = std::mem::take(&mut self.directory_gates);
        let result = self.workspaces.open_local_project(
            selected_path,
            identity,
            canonical_identity,
            |workspace_id, project_root| {
                let directory_gate =
                    DynamicWorkspaceDirectorySource::available(project_root.to_path_buf());
                directory_gates.insert(workspace_id, directory_gate.clone());
                let factory_directory_gate = directory_gate.clone();
                Self::create_window_manager(
                    workspace_id,
                    WorkspaceTerminalSessionFactory::dynamic(
                        Rc::clone(&session_factory),
                        move || factory_directory_gate.resolve().ok(),
                    ),
                    Rc::new(directory_gate),
                    sidebar_visible,
                    sidebar_width,
                    window,
                    cx,
                )
            },
        );
        self.directory_gates = directory_gates;

        match result {
            Ok(OpenLocalProjectOutcome::Created { workspace_id, .. }) => {
                let next_manager = self
                    .workspaces
                    .workspace(workspace_id)
                    .map(|workspace| workspace.payload().clone())
                    .unwrap_or_else(|| {
                        unreachable!(
                            "a newly created Workspace must remain owned by its collection"
                        )
                    });
                self.initialize_workspace_authority(workspace_id, cx);
                self.present_activated_workspace(previous_manager, next_manager, window, cx);
            }
            Ok(OpenLocalProjectOutcome::ActivatedExisting { .. }) => {
                self.scroll_active_workspace_into_view();
                cx.notify();
            }
            Err(error) => Self::report_workspace_error("open Local Project", error),
        }
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
        if let Some(directory) = self
            .workspaces
            .workspace_directory(workspace_id)
            .ok()
            .map(Path::to_path_buf)
        {
            let validity = validity_of_path(&directory);
            let _ = self.workspaces.revalidate_directory(workspace_id, validity);
            if let Some(directory_gate) = self.directory_gates.get(&workspace_id) {
                directory_gate.set_validity(validity);
            }
        }
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
        self.workspace_menu = None;
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
        let mut directory_gates = std::mem::take(&mut self.directory_gates);
        let outcome = self.workspaces.close_workspace(
            workspace_id,
            WorkspaceLaunch::AdHoc {
                home: self.default_workspace_root.clone(),
            },
            |replacement_workspace_id, workspace_root| {
                let directory_gate =
                    DynamicWorkspaceDirectorySource::available(workspace_root.to_path_buf());
                directory_gates.insert(replacement_workspace_id, directory_gate.clone());
                let factory_directory_gate = directory_gate.clone();
                Self::create_window_manager(
                    replacement_workspace_id,
                    WorkspaceTerminalSessionFactory::dynamic(
                        Rc::clone(&session_factory),
                        move || factory_directory_gate.resolve().ok(),
                    ),
                    Rc::new(directory_gate),
                    sidebar_visible,
                    sidebar_width,
                    window,
                    cx,
                )
            },
        );
        self.directory_gates = directory_gates;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                Self::report_workspace_error("close", error);
                return;
            }
        };

        let closed_manager = match outcome {
            CloseWorkspaceOutcome::WorkspaceClosed {
                closed_workspace_id,
                payload,
                ..
            } => {
                self.authority.remove(&closed_workspace_id);
                self.directory_gates.remove(&closed_workspace_id);
                payload
            }
            CloseWorkspaceOutcome::FinalWorkspaceReplaced {
                closed_workspace_id,
                replacement_workspace_id,
                payload,
            } => {
                self.authority.remove(&closed_workspace_id);
                self.directory_gates.remove(&closed_workspace_id);
                self.initialize_workspace_authority(replacement_workspace_id, cx);
                payload
            }
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
        self.workspace_menu = None;
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
                self.authority.remove(&closed_workspace_id);
                self.directory_gates.remove(&closed_workspace_id);
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
                self.workspace_menu = None;
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
        let was_sidebar_focused = self.sidebar_focus.is_focused(window)
            || self.rename_is_focused(window)
            || self.search_is_focused(window);
        let sidebar_visible = !self.sidebar_visible;
        self.workspace_menu = None;
        self.rename = None;
        self.set_sidebar_layout(sidebar_visible, self.sidebar_width, cx);
        if !sidebar_visible && was_sidebar_focused {
            self.focus(window, cx);
        }
        self.sync_terminal_focus_blocker(window, cx);
    }

    fn toggle_sidebar_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_focus.is_focused(window)
            || self.rename_is_focused(window)
            || self.search_is_focused(window)
        {
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

    fn open_workspace_menu(
        &mut self,
        workspace_id: WorkspaceId,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_focus.focus(window);
        self.sync_terminal_focus_blocker(window, cx);
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
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    fn perform_workspace_menu_command(
        &mut self,
        command: WorkspaceMenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.workspace_menu else {
            return;
        };
        match command {
            WorkspaceMenuCommand::NewWindow => {
                if let Some(workspace) = self.workspaces.workspace(menu.workspace_id) {
                    workspace
                        .payload()
                        .update(cx, |manager, cx| manager.create_window(window, cx));
                }
                self.workspace_menu = None;
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            }
            WorkspaceMenuCommand::Rename => {
                if self.workspaces.workspace(menu.workspace_id).is_none() {
                    self.workspace_menu = None;
                    self.sync_terminal_focus_blocker(window, cx);
                    return;
                };
                let input = cx.new(|cx| {
                    TextInput::new(
                        self.workspaces
                            .display_name(menu.workspace_id, &self.default_workspace_root)
                            .unwrap_or_default()
                            .as_str(),
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
                    workspace_id: menu.workspace_id,
                    focus_handle: input.read(cx).focus_handle(),
                    input,
                });
                self.workspace_menu = None;
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
            WorkspaceMenuCommand::Close => self.close_workspace(menu.workspace_id, window, cx),
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
        if let Some(value) = value {
            let name = value.trim();
            let custom_name = (!name.is_empty()).then(|| name.to_owned());
            if let Err(error) = self.workspaces.set_custom_name(workspace_id, custom_name) {
                Self::report_workspace_error("rename", error);
            }
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

    fn on_close_workspace_action(
        &mut self,
        _: &CloseWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active_workspace_id = self.workspaces.active_workspace_id();
        self.close_workspace(active_workspace_id, window, cx);
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
        row: WorkspaceRowPresentation,
        manager: WeakEntity<Self>,
    ) -> AnyElement {
        let WorkspaceRowPresentation {
            workspace_id,
            kind,
            name,
            counts,
            path,
            path_truncated,
            directory_available,
            active,
        } = row;
        let click_manager = manager.clone();
        let context_manager = manager;
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
                .flex_1()
                .min_w_0()
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
                .flex_1()
                .min_w_0()
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

        let (kind_icon_name, kind_label) = match kind {
            WorkspaceKind::AdHoc => ("terminal", "adhoc"),
            WorkspaceKind::LocalProject => ("folder", "project"),
        };
        let icon_color = if !directory_available {
            ACTIVE_THEME.warning
        } else if active {
            ACTIVE_THEME.icon_accent
        } else {
            ACTIVE_THEME.icon
        };
        let kind_icon_selector = format!(
            "workspace-kind-icon-{}-{kind_label}{}",
            workspace_id.get(),
            if directory_available {
                ""
            } else {
                "-unavailable"
            }
        );
        let counts_selector = format!("workspace-row-counts-{}-{counts}", workspace_id.get());
        let path_selector = format!(
            "workspace-row-path-{}{}",
            workspace_id.get(),
            if path_truncated { "-truncated" } else { "" }
        );

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
                    .id(("workspace-kind-icon", workspace_id.get()))
                    .debug_selector(move || kind_icon_selector.clone())
                    .w(px(18.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(kind_icon_name)
                            .size(px(SIDEBAR_ROW_ICON_SIZE))
                            .color(gpui_color(icon_color)),
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
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .child(first_line)
                            .child(
                                div()
                                    .id(("workspace-row-counts", workspace_id.get()))
                                    .debug_selector(move || counts_selector.clone())
                                    .flex_shrink_0()
                                    .text_size(px(SIDEBAR_COUNTS_TEXT_SIZE))
                                    .text_color(gpui_color(ACTIVE_THEME.text_muted))
                                    .child(counts),
                            ),
                    )
                    .child(
                        div()
                            .id(("workspace-row-path", workspace_id.get()))
                            .debug_selector(move || path_selector.clone())
                            .w_full()
                            .truncate()
                            .text_size(px(SIDEBAR_DETAIL_TEXT_SIZE))
                            .text_color(gpui_color(ACTIVE_THEME.text_muted))
                            .child(path),
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

    /// Single-row header above the Workspace list: a persistent search input
    /// and the Open Local Project entry point.
    fn render_sidebar_header(&self, manager: WeakEntity<Self>) -> AnyElement {
        let open_manager = manager.clone();
        let search_manager = manager;
        div()
            .id("workspace-sidebar-header")
            .debug_selector(|| "workspace-sidebar-header".to_owned())
            .w_full()
            .flex_shrink_0()
            .px(px(SIDEBAR_ROW_HORIZONTAL_PADDING))
            .py(px(SIDEBAR_HEADER_VERTICAL_PADDING))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .id("workspace-search-input")
                    .debug_selector(|| "workspace-search-input".to_owned())
                    .min_w_0()
                    .flex_1()
                    .h(px(SIDEBAR_SEARCH_HEIGHT))
                    .px(px(5.0))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .rounded(px(4.0))
                    .border(px(1.0))
                    .border_color(gpui_color(ACTIVE_THEME.border))
                    .bg(gpui_color(ACTIVE_THEME.element_background))
                    .text_size(px(SIDEBAR_NAME_TEXT_SIZE))
                    .text_color(gpui_color(ACTIVE_THEME.text))
                    .capture_any_mouse_down(move |_, window, cx| {
                        let _ = search_manager.update(cx, |manager, cx| {
                            manager.focus_search(window, cx);
                        });
                    })
                    .child(self.search.clone()),
            )
            .child(
                div()
                    .id("open-local-project-button")
                    .debug_selector(|| "open-local-project-button".to_owned())
                    .size(px(SIDEBAR_SEARCH_HEIGHT))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .hover(|button| button.bg(gpui_color(ACTIVE_THEME.ghost_element_selected)))
                    .on_click(move |_, window, cx| {
                        let _ = open_manager.update(cx, |manager, cx| {
                            manager.open_local_project(window, cx);
                        });
                        cx.stop_propagation();
                    })
                    .child(
                        Icon::new("folder.badge.plus")
                            .size(px(13.0))
                            .color(gpui_color(ACTIVE_THEME.icon)),
                    ),
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
            let workspace_id = workspace.id();
            let kind = self
                .workspaces
                .kind(workspace_id)
                .unwrap_or(WorkspaceKind::AdHoc);
            let display_name = self
                .workspaces
                .display_name(workspace_id, &self.default_workspace_root)
                .unwrap_or_default();
            let directory = self
                .workspaces
                .workspace_directory(workspace_id)
                .map(Path::to_path_buf)
                .unwrap_or_default();
            let directory_available = self
                .workspaces
                .directory_available(workspace_id)
                .unwrap_or(true);
            let (path, path_truncated) =
                secondary_path_line(&directory, &self.default_workspace_root);
            let (windows, panes) = workspace.payload().read(cx).aggregate_counts(cx);
            rows = rows.child(self.render_workspace_row(
                WorkspaceRowPresentation {
                    workspace_id,
                    kind,
                    name: display_name.into(),
                    counts: format_workspace_counts(windows, panes).into(),
                    path: path.into(),
                    path_truncated,
                    directory_available,
                    active: workspace_id == active_workspace_id,
                },
                manager.clone(),
            ));
        }

        let scrollbar = self.scrollbar.clone();
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
            .child(self.render_sidebar_header(manager.clone()))
            .child(rows)
            .when_some(self.creation_block_notice.clone(), |sidebar, notice| {
                sidebar.child(render_creation_block_notice(&notice))
            })
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
            .on_mouse_down_out(move |_, window, cx| {
                let _ = dismiss_manager.update(cx, |manager, cx| {
                    if manager.workspace_menu.take().is_some() {
                        manager.sync_terminal_focus_blocker(window, cx);
                        cx.notify();
                    }
                });
            })
            .child(render_workspace_menu_content(menu.workspace_id, manager))
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
            .on_action(cx.listener(Self::on_open_local_project))
            .on_action(cx.listener(Self::on_close_workspace_action))
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
            .on_action(
                cx.listener(Self::forward_active_terminal_action::<ExportTerminalDiagnostics>),
            )
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

fn render_creation_block_notice(notice: &str) -> AnyElement {
    div()
        .id("creation-block-notice")
        .debug_selector(|| "creation-block-notice".to_owned())
        .w_full()
        .flex_shrink_0()
        .px(px(SIDEBAR_ROW_HORIZONTAL_PADDING))
        .py(px(CREATION_BLOCK_NOTICE_VERTICAL_PADDING))
        .bg(gpui_color(ACTIVE_THEME.element_background))
        .text_size(px(SIDEBAR_DETAIL_TEXT_SIZE))
        .text_color(gpui_color(ACTIVE_THEME.error))
        .truncate()
        .child(notice.to_owned())
        .into_any_element()
}

fn gpui_color(color: Color) -> gpui::Rgba {
    rgba(color.rgba_hex())
}

/// Formats the aggregate Workspace-wide Window and Pane counts exactly as
/// presented on a Workspace row's primary line.
fn format_workspace_counts(windows: usize, panes: usize) -> String {
    format!("{windows}W · {panes}P")
}

/// Compacts `directory` for presentation under `home`, turning `$HOME/foo`
/// into `~/foo` and leaving any other absolute path untouched. The prefix
/// comparison is component-wise, so sibling directories never compact.
fn home_compacted_display(directory: &Path, home: &Path) -> String {
    match strip_home_prefix(directory, home) {
        Some(remainder) if remainder.as_os_str().is_empty() => "~".to_owned(),
        Some(remainder) => format!("~/{}", remainder.display()),
        None => directory.display().to_string(),
    }
}

/// Returns the portion of `directory` below `home` when `directory` lies
/// under `home`, and `None` otherwise.
fn strip_home_prefix<'a>(directory: &'a Path, home: &Path) -> Option<&'a Path> {
    let mut remaining_components = directory.components();
    let mut home_components = home.components();
    loop {
        match home_components.next() {
            Some(expected) => match remaining_components.next() {
                Some(actual) if actual == expected => continue,
                _ => return None,
            },
            None => return Some(remaining_components.as_path()),
        }
    }
}

/// Middle-truncates `text` to at most `budget` characters, preserving its
/// head and tail around one ellipsis. Returns the display text together with
/// whether truncation engaged; shorter text passes through unchanged.
fn middle_truncated(text: &str, budget: usize) -> (String, bool) {
    let total = text.chars().count();
    if total <= budget {
        return (text.to_owned(), false);
    }
    let kept = budget.saturating_sub(1);
    let head = kept - kept / 2;
    let tail = kept / 2;
    let mut truncated = String::with_capacity(budget);
    truncated.extend(text.chars().take(head));
    truncated.push(MIDDLE_TRUNCATION_ELLIPSIS);
    truncated.extend(text.chars().skip(total - tail));
    (truncated, true)
}

/// Builds the secondary Workspace Directory line: HOME-compacted, then
/// middle-truncated to the deterministic character budget when it cannot fit.
fn secondary_path_line(directory: &Path, home: &Path) -> (String, bool) {
    middle_truncated(
        &home_compacted_display(directory, home),
        SECONDARY_PATH_CHARACTER_BUDGET,
    )
}

#[cfg(test)]
mod tests {
    use gpui::{
        Modifiers, ScrollDelta, ScrollWheelEvent, Task, TestAppContext, TouchPhase,
        VisualTestContext, point,
    };

    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::terminal::testing::{
        RecordedSessionCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
    };
    use crate::terminal::{ScreenSnapshot, SessionEvent, SessionExit};

    /// A real directory so activation-time revalidation observes availability.
    fn harness_workspace_root() -> PathBuf {
        static ROOT: OnceLock<PathBuf> = OnceLock::new();
        ROOT.get_or_init(|| {
            let root = std::env::temp_dir().join(format!(
                "spaceterm-workspace-manager-{}",
                std::process::id()
            ));
            std::fs::create_dir_all(&root).expect("create the harness Workspace root");
            root
        })
        .clone()
    }

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
            WorkspaceManager::new(session_factory, harness_workspace_root(), window, cx)
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
                manager.workspaces.active_workspace().id(),
                manager
                    .workspaces
                    .kind(WorkspaceId::new(2))
                    .expect("the created Workspace must remain owned"),
                manager
                    .workspaces
                    .workspace_directory(WorkspaceId::new(2))
                    .expect("the created Workspace must remain owned")
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
                WorkspaceId::new(2),
                crate::domain::WorkspaceKind::AdHoc,
                harness_workspace_root(),
                Vec::new(),
                vec![harness_workspace_root(), harness_workspace_root()],
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
                    .aggregate_counts(cx),
            )
        });

        cx.simulate_keystrokes("cmd-shift-e");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-1");
        cx.run_until_parked();

        assert_eq!(created_state, (false, (2, 2)));
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
                    .aggregate_counts(cx),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (false, (1, 2), Vec::new()));
    }

    #[gpui::test]
    fn workspace_context_menu_should_target_new_window_and_rename_commands(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = workspace_manager(cx);

        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-new-window", cx);
        let workspace_counts = manager.read_with(cx, |manager, cx| {
            manager
                .workspaces
                .active_workspace()
                .payload()
                .read(cx)
                .aggregate_counts(cx)
        });

        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-rename", cx);
        cx.simulate_keystrokes("cmd-a D e v enter");
        cx.run_until_parked();
        let name = manager.read_with(cx, |manager, _| {
            manager
                .workspaces
                .display_name(WorkspaceId::new(1), &manager.default_workspace_root)
                .expect("the renamed Workspace must remain owned")
        });

        assert_eq!((workspace_counts, name), ((2, 2), "Dev".to_owned()));
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
                manager
                    .workspaces
                    .display_name(WorkspaceId::new(1), &manager.default_workspace_root)
                    .expect("the renamed Workspace must remain owned"),
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
                manager
                    .workspaces
                    .display_name(WorkspaceId::new(1), &manager.default_workspace_root)
                    .expect("the renamed Workspace must remain owned"),
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
                manager
                    .workspaces
                    .display_name(WorkspaceId::new(1), &manager.default_workspace_root)
                    .expect("Workspace 1 must remain owned"),
                first.payload().read(cx).aggregate_counts(cx),
                second.payload().read(cx).aggregate_counts(cx),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(
            state,
            (
                WorkspaceId::new(2),
                true,
                "Default".to_owned(),
                (1, 1),
                (2, 2),
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
                manager
                    .workspaces
                    .kind(WorkspaceId::new(2))
                    .expect("the replacement Workspace must remain owned"),
                manager
                    .workspaces
                    .workspace_directory(WorkspaceId::new(2))
                    .expect("the replacement Workspace must remain owned")
                    .to_path_buf(),
                manager
                    .workspaces
                    .display_name(WorkspaceId::new(2), &manager.default_workspace_root)
                    .expect("the replacement Workspace must remain owned"),
                records.dropped_session_ids(),
                records.session_count(),
            )
        });
        assert_eq!(
            state,
            (
                1,
                WorkspaceId::new(2),
                crate::domain::WorkspaceKind::AdHoc,
                harness_workspace_root(),
                "Default".to_owned(),
                vec![1],
                2,
            )
        );
        assert_eq!(cx.windows().len(), 1);
    }

    #[gpui::test]
    fn closing_the_active_workspace_via_menu_action_should_work_while_the_sidebar_is_focused(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-shift-e");
        cx.run_until_parked();
        let sidebar_was_focused =
            cx.update(|window, cx| manager.read(cx).sidebar_focus.is_focused(window));
        assert!(sidebar_was_focused);

        cx.dispatch_action(CloseWorkspace);
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                manager
                    .workspaces
                    .kind(WorkspaceId::new(1))
                    .expect("the remaining Workspace must stay owned"),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(
            state,
            (
                1,
                WorkspaceId::new(1),
                crate::domain::WorkspaceKind::AdHoc,
                vec![2],
            )
        );
    }

    #[gpui::test]
    fn closing_the_final_workspace_via_menu_action_should_replace_it_while_rename_is_active(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-rename", cx);
        assert!(manager.read_with(cx, |manager, _| manager.rename.is_some()));

        cx.dispatch_action(CloseWorkspace);
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                manager
                    .workspaces
                    .kind(WorkspaceId::new(2))
                    .expect("the replacement Workspace must remain owned"),
                manager
                    .workspaces
                    .display_name(WorkspaceId::new(2), &manager.default_workspace_root)
                    .expect("the replacement Workspace must remain owned"),
                manager.rename.is_none(),
                records.dropped_session_ids(),
                records.session_count(),
            )
        });
        assert_eq!(
            state,
            (
                1,
                WorkspaceId::new(2),
                crate::domain::WorkspaceKind::AdHoc,
                "Default".to_owned(),
                true,
                vec![1],
                2,
            )
        );
        assert_eq!(cx.windows().len(), 1);
    }

    #[gpui::test]
    fn closing_the_active_workspace_via_menu_action_should_work_while_search_is_focused(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        let search_focus = manager.read_with(cx, |manager, _| manager.search_focus.clone());
        cx.update(|window, _| search_focus.focus(window));
        cx.run_until_parked();

        cx.dispatch_action(CloseWorkspace);
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                records.dropped_session_ids(),
            )
        });
        assert_eq!(state, (1, WorkspaceId::new(1), vec![2]));
    }

    #[gpui::test]
    fn exporting_terminal_diagnostics_should_reach_the_terminal_while_search_is_focused(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = workspace_manager(cx);
        let search_focus = manager.read_with(cx, |manager, _| manager.search_focus.clone());
        cx.update(|window, _| search_focus.focus(window));
        cx.run_until_parked();

        cx.dispatch_action(ExportTerminalDiagnostics);
        cx.run_until_parked();

        assert!(
            cx.did_prompt_for_new_path(),
            "Export Terminal Diagnostics must be consumed by the active Pane's save panel"
        );
        cx.simulate_new_path_selection(|_| None);
        cx.run_until_parked();
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

    static TEMP_PROJECT_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_project_root(label: &str) -> PathBuf {
        let count = TEMP_PROJECT_COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "spaceterm-workspace-{label}-{}-{count}",
            std::process::id()
        ))
    }

    /// Adds one Local Project Workspace over `project_root` with the same
    /// production wiring the Create Workspace flow uses.
    fn add_project_workspace(
        manager: &Entity<WorkspaceManager>,
        project_root: PathBuf,
        cx: &mut VisualTestContext,
    ) -> WorkspaceId {
        manager.update_in(cx, |manager, window, cx| {
            let directory_gate = DynamicWorkspaceDirectorySource::available(project_root.clone());
            let factory_directory_gate = directory_gate.clone();
            let session_factory = WorkspaceTerminalSessionFactory::dynamic(
                Rc::clone(&manager.session_factory),
                move || factory_directory_gate.resolve().ok(),
            );
            let sidebar_visible = manager.sidebar_visible;
            let sidebar_width = manager.sidebar_width;
            let created = manager
                .workspaces
                .create_workspace(
                    crate::domain::WorkspaceLaunch::LocalProject {
                        project_root: project_root.clone(),
                    },
                    |workspace_id, _| {
                        WorkspaceManager::create_window_manager(
                            workspace_id,
                            session_factory,
                            Rc::new(directory_gate.clone()),
                            sidebar_visible,
                            sidebar_width,
                            window,
                            cx,
                        )
                    },
                )
                .expect("the Project Workspace must be created");
            manager.directory_gates.insert(created, directory_gate);
            cx.notify();
            created
        })
    }

    #[gpui::test]
    fn unavailable_directory_should_block_window_creation_and_present_the_creation_block_notice(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let project_root = unique_project_root("blocked-create");
        std::fs::create_dir_all(&project_root).expect("create the Project root");
        let project_workspace_id = add_project_workspace(&manager, project_root.clone(), cx);
        assert_eq!(project_workspace_id, WorkspaceId::new(2));
        cx.run_until_parked();
        let baseline_starts = records.starts().len();

        std::fs::remove_dir_all(&project_root).expect("remove the Project root");
        click("workspace-row-2-active", cx);

        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.workspaces.active_workspace_id(),
                manager.workspaces.len(),
                manager.creation_block_notice.is_some(),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .aggregate_counts(cx),
            )
        });
        assert_eq!(
            state,
            (WorkspaceId::new(2), 2, true, (1, 1),),
            "the blocked Window must not be created and the notice must be presented"
        );
        assert!(cx.debug_bounds("creation-block-notice").is_some());
        assert_eq!(
            records.starts().len(),
            baseline_starts,
            "no Terminal Session may start while the directory is unavailable"
        );
    }

    #[gpui::test]
    fn unavailable_directory_should_block_splitting_and_present_the_creation_block_notice(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let project_root = unique_project_root("blocked-split");
        std::fs::create_dir_all(&project_root).expect("create the Project root");
        add_project_workspace(&manager, project_root.clone(), cx);
        cx.run_until_parked();
        std::fs::remove_dir_all(&project_root).expect("remove the Project root");
        click("workspace-row-2-active", cx);
        let baseline_starts = records.starts().len();

        cx.simulate_keystrokes("cmd-shift-e");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();

        let detail = manager.read_with(cx, |manager, cx| {
            (
                manager.creation_block_notice.is_some(),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .aggregate_counts(cx),
            )
        });
        assert_eq!(
            detail,
            (true, (1, 1)),
            "the blocked Split must not mutate the Pane Layout and must present the notice"
        );
        assert!(cx.debug_bounds("creation-block-notice").is_some());
        assert_eq!(records.starts().len(), baseline_starts);
    }

    #[gpui::test]
    fn existing_panes_should_keep_their_sessions_while_new_children_are_blocked(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let first_session_sender = records
            .event_sender(1)
            .expect("the initial Workspace session must have started");
        let project_root = unique_project_root("existing-panes");
        std::fs::create_dir_all(&project_root).expect("create the Project root");
        add_project_workspace(&manager, project_root.clone(), cx);
        cx.run_until_parked();
        std::fs::remove_dir_all(&project_root).expect("remove the Project root");
        click("workspace-row-2-active", cx);
        let baseline_starts = records.starts().len();

        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();
        first_session_sender
            .try_send(SessionEvent::Screen(ScreenSnapshot::from_test_parts(
                Arc::from([]),
                Default::default(),
                "Claude Code",
            )))
            .expect("the existing session must keep accepting events");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, cx| {
            (
                manager.workspaces.workspace(WorkspaceId::new(1)).is_some(),
                manager
                    .workspaces
                    .workspace(WorkspaceId::new(1))
                    .map(|workspace| workspace.payload().read(cx).aggregate_counts(cx)),
            )
        });
        assert!(state.0, "Workspace 1 must remain owned");
        assert_eq!(
            state.1,
            Some((1, 1)),
            "the existing Pane session must keep running while creation is blocked"
        );
        assert_eq!(
            records.starts().len(),
            baseline_starts,
            "blocked creation must not start additional sessions"
        );
    }

    #[gpui::test]
    fn restoring_the_directory_should_reenable_child_creation_on_activation(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let project_root = unique_project_root("restore");
        std::fs::create_dir_all(&project_root).expect("create the Project root");
        add_project_workspace(&manager, project_root.clone(), cx);
        cx.run_until_parked();
        std::fs::remove_dir_all(&project_root).expect("remove the Project root");
        click("workspace-row-2-active", cx);
        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();
        let blocked_counts = manager.read_with(cx, |manager, cx| {
            manager
                .workspaces
                .active_workspace()
                .payload()
                .read(cx)
                .aggregate_counts(cx)
        });
        assert_eq!(blocked_counts, (1, 1));

        std::fs::create_dir_all(&project_root).expect("restore the Project root");
        click("workspace-row-1-inactive", cx);
        click("workspace-row-2-inactive", cx);
        cx.run_until_parked();

        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();

        let restored_counts = manager.read_with(cx, |manager, cx| {
            manager
                .workspaces
                .active_workspace()
                .payload()
                .read(cx)
                .aggregate_counts(cx)
        });
        assert_eq!(
            restored_counts,
            (2, 2),
            "revalidation on activation must reenable Window creation"
        );
        assert!(records.dropped_session_ids().is_empty());
    }

    static AUTHORITY_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A real on-disk directory so Reported Working Directory reports validate
    /// against the filesystem oracle.
    fn valid_authority_directory(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "spaceterm-authority-{label}-{}-{}",
            std::process::id(),
            AUTHORITY_DIR_COUNTER.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::create_dir_all(&path).expect("create the authority directory");
        path
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

    fn send_reported_directory(
        records: &TestTerminalSessionRecords,
        session_id: usize,
        directory: &Path,
    ) {
        records
            .event_sender(session_id)
            .unwrap_or_else(|| panic!("session {session_id} must have started"))
            .try_send(SessionEvent::Screen(screen_with_reported_directory(
                directory,
            )))
            .expect("the Reported Working Directory snapshot must be delivered");
    }

    fn send_exit(records: &TestTerminalSessionRecords, session_id: usize) {
        records
            .event_sender(session_id)
            .unwrap_or_else(|| panic!("session {session_id} must have started"))
            .try_send(SessionEvent::Exited(SessionExit::Success))
            .expect("the exit must be delivered");
    }

    fn workspace_directory_state(
        manager: &Entity<WorkspaceManager>,
        workspace_id: WorkspaceId,
        cx: &mut VisualTestContext,
    ) -> (PathBuf, bool) {
        manager.read_with(cx, |manager, _| {
            (
                manager
                    .workspaces
                    .workspace_directory(workspace_id)
                    .expect("the Workspace must remain owned")
                    .to_path_buf(),
                manager
                    .workspaces
                    .directory_available(workspace_id)
                    .expect("the Workspace must remain owned"),
            )
        })
    }

    fn authority_of(
        manager: &Entity<WorkspaceManager>,
        workspace_id: WorkspaceId,
        cx: &mut VisualTestContext,
    ) -> Option<(WindowId, PaneId)> {
        manager.read_with(cx, |manager, _| {
            manager.authority.get(&workspace_id).copied()
        })
    }

    fn two_workspaces(cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
    }

    #[gpui::test]
    fn authoritative_reports_should_update_inactive_workspace_directories(cx: &mut TestAppContext) {
        let (manager, records, cx) = workspace_manager(cx);
        two_workspaces(cx);
        let reported = valid_authority_directory("inactive");

        send_reported_directory(&records, 2, &reported);
        cx.run_until_parked();

        assert_eq!(
            (
                workspace_directory_state(&manager, WorkspaceId::new(2), cx),
                workspace_directory_state(&manager, WorkspaceId::new(3), cx),
                authority_of(&manager, WorkspaceId::new(2), cx),
            ),
            (
                (reported.clone(), true),
                (harness_workspace_root(), true),
                Some((WindowId::new(1), PaneId::new(1))),
            )
        );
    }

    #[gpui::test]
    fn non_authoritative_pane_reports_should_not_change_the_workspace(cx: &mut TestAppContext) {
        let (manager, records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-shift-e");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();
        let stray = valid_authority_directory("stray");

        send_reported_directory(&records, 3, &stray);
        cx.run_until_parked();

        assert_eq!(
            (
                workspace_directory_state(&manager, WorkspaceId::new(2), cx),
                authority_of(&manager, WorkspaceId::new(2), cx),
            ),
            (
                (harness_workspace_root(), true),
                Some((WindowId::new(1), PaneId::new(1)))
            ),
        );
    }

    #[gpui::test]
    fn splitting_should_not_move_authority_away_from_the_original_pane(cx: &mut TestAppContext) {
        let (manager, records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-shift-e");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();
        let original = valid_authority_directory("original");

        send_reported_directory(&records, 2, &original);
        cx.run_until_parked();

        assert_eq!(
            (
                workspace_directory_state(&manager, WorkspaceId::new(2), cx),
                authority_of(&manager, WorkspaceId::new(2), cx),
            ),
            ((original, true), Some((WindowId::new(1), PaneId::new(1)))),
        );
    }

    #[gpui::test]
    fn closing_the_authoritative_pane_should_promote_and_adopt_the_first_remaining_pane(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-shift-e");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();
        let root_report = valid_authority_directory("promote-root");
        let split_report = valid_authority_directory("promote-split");

        send_reported_directory(&records, 2, &root_report);
        cx.run_until_parked();
        assert_eq!(
            workspace_directory_state(&manager, WorkspaceId::new(2), cx),
            (root_report.clone(), true)
        );
        send_reported_directory(&records, 3, &split_report);
        cx.run_until_parked();

        send_exit(&records, 2);
        cx.run_until_parked();

        assert_eq!(records.dropped_session_ids(), vec![2]);
        assert_eq!(
            (
                workspace_directory_state(&manager, WorkspaceId::new(2), cx),
                authority_of(&manager, WorkspaceId::new(2), cx),
            ),
            (
                (split_report, true),
                Some((WindowId::new(1), PaneId::new(2)))
            ),
        );
    }

    #[gpui::test]
    fn promoting_a_pane_without_a_valid_directory_should_retain_the_previous_directory(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-shift-e");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();
        let adopted = valid_authority_directory("retain-adopted");
        let vanished = valid_authority_directory("retain-vanished");

        send_reported_directory(&records, 2, &adopted);
        cx.run_until_parked();
        assert_eq!(
            workspace_directory_state(&manager, WorkspaceId::new(2), cx),
            (adopted.clone(), true)
        );

        send_reported_directory(&records, 3, &vanished);
        std::fs::remove_dir_all(&vanished).expect("invalidate the promoted directory");
        send_exit(&records, 2);
        cx.run_until_parked();

        assert_eq!(
            (
                workspace_directory_state(&manager, WorkspaceId::new(2), cx),
                authority_of(&manager, WorkspaceId::new(2), cx),
            ),
            ((adopted, true), Some((WindowId::new(1), PaneId::new(2)))),
        );
    }

    #[gpui::test]
    fn invalid_reports_should_leave_the_previous_directory_available(cx: &mut TestAppContext) {
        let (manager, records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        let adopted = valid_authority_directory("invalid-adopted");
        let ghost = valid_authority_directory("invalid-ghost");

        send_reported_directory(&records, 2, &adopted);
        cx.run_until_parked();
        assert_eq!(
            workspace_directory_state(&manager, WorkspaceId::new(2), cx),
            (adopted.clone(), true)
        );

        send_reported_directory(&records, 2, &ghost);
        std::fs::remove_dir_all(&ghost).expect("invalidate the report before it is handled");
        cx.run_until_parked();

        assert_eq!(
            (
                workspace_directory_state(&manager, WorkspaceId::new(2), cx),
                authority_of(&manager, WorkspaceId::new(2), cx),
            ),
            ((adopted, true), Some((WindowId::new(1), PaneId::new(1)))),
        );
    }

    #[gpui::test]
    fn closing_the_authoritative_window_should_promote_across_windows_and_adopt(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();
        let survivor = valid_authority_directory("window-promote");

        send_reported_directory(&records, 3, &survivor);
        cx.run_until_parked();

        send_exit(&records, 2);
        cx.run_until_parked();

        assert_eq!(records.dropped_session_ids(), vec![2]);
        assert_eq!(
            (
                workspace_directory_state(&manager, WorkspaceId::new(2), cx),
                authority_of(&manager, WorkspaceId::new(2), cx),
            ),
            ((survivor, true), Some((WindowId::new(2), PaneId::new(1)))),
        );
    }

    #[gpui::test]
    fn local_project_workspaces_should_ignore_reported_directories(cx: &mut TestAppContext) {
        let (manager, records, cx) = workspace_manager(cx);
        let project_root = unique_project_root("ignore-reports");
        std::fs::create_dir_all(&project_root).expect("create the Project root");
        add_project_workspace(&manager, project_root.clone(), cx);
        cx.run_until_parked();
        let drifted = valid_authority_directory("project-drift");

        send_reported_directory(&records, 2, &drifted);
        cx.run_until_parked();

        assert_eq!(
            (
                workspace_directory_state(&manager, WorkspaceId::new(2), cx),
                authority_of(&manager, WorkspaceId::new(2), cx),
            ),
            ((project_root, true), None),
        );
    }

    #[gpui::test]
    fn authoritative_adoption_should_mirror_into_new_window_session_starts(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        two_workspaces(cx);
        let mirrored = valid_authority_directory("gate-mirror");

        send_reported_directory(&records, 2, &mirrored);
        cx.run_until_parked();
        assert_eq!(
            workspace_directory_state(&manager, WorkspaceId::new(2), cx),
            (mirrored.clone(), true)
        );

        click("workspace-row-2-inactive", cx);
        cx.run_until_parked();
        let baseline_starts = records.starts().len();
        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();

        let starts = records.starts();
        assert_eq!(starts.len(), baseline_starts + 1);
        assert_eq!(
            starts.last().map(|start| start.working_directory.clone()),
            Some(mirrored)
        );
    }

    #[gpui::test]
    fn final_pane_close_escalation_should_promote_exactly_once_across_windows(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();
        let survivor = valid_authority_directory("escalation");

        send_reported_directory(&records, 3, &survivor);
        cx.run_until_parked();

        send_exit(&records, 2);
        cx.run_until_parked();

        let counts = manager.read_with(cx, |manager, app| {
            manager
                .workspaces
                .workspace(WorkspaceId::new(2))
                .expect("the escalated Workspace must survive its Window close")
                .payload()
                .read(app)
                .aggregate_counts(app)
        });
        assert_eq!(
            (
                counts,
                records.dropped_session_ids(),
                workspace_directory_state(&manager, WorkspaceId::new(2), cx),
                authority_of(&manager, WorkspaceId::new(2), cx),
            ),
            (
                (1, 1),
                vec![2],
                (survivor, true),
                Some((WindowId::new(2), PaneId::new(1))),
            )
        );
    }

    /// A Local Project picker whose selections are scripted per test. Each
    /// invocation consumes one queued selection immediately.
    struct ScriptedProjectPicker {
        invocations: Rc<Cell<usize>>,
        selections: Rc<RefCell<VecDeque<Option<PathBuf>>>>,
    }

    impl LocalProjectPicker for ScriptedProjectPicker {
        fn pick_local_project_directory(
            &self,
            _: SharedString,
            _: &mut App,
        ) -> Task<Option<PathBuf>> {
            self.invocations.set(self.invocations.get() + 1);
            let selection = self
                .selections
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| panic!("unexpected Local Project picker invocation"));
            Task::ready(selection)
        }
    }

    fn workspace_manager_with_picker(
        selections: Vec<Option<PathBuf>>,
        cx: &mut TestAppContext,
    ) -> (
        Entity<WorkspaceManager>,
        Rc<Cell<usize>>,
        TestTerminalSessionRecords,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
        let invocations = Rc::new(Cell::new(0));
        let picker = ScriptedProjectPicker {
            invocations: Rc::clone(&invocations),
            selections: Rc::new(RefCell::new(selections.into_iter().collect())),
        };
        let (manager, cx) = cx.add_window_view(|window, cx| {
            WorkspaceManager::new_with_project_picker(
                session_factory,
                harness_workspace_root(),
                Box::new(picker),
                window,
                cx,
            )
        });
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.focus(window, cx));
        });
        cx.run_until_parked();
        (manager, invocations, records, cx)
    }

    #[gpui::test]
    fn opening_a_local_project_should_create_and_activate_it_at_the_selected_root(
        cx: &mut TestAppContext,
    ) {
        let project_root = unique_project_root("open-create").join("nested/site");
        std::fs::create_dir_all(&project_root).expect("create the nested Project root");
        let (manager, invocations, records, cx) =
            workspace_manager_with_picker(vec![Some(project_root.clone())], cx);

        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                manager.workspaces.kind(WorkspaceId::new(2)).unwrap(),
                manager
                    .workspaces
                    .workspace_directory(WorkspaceId::new(2))
                    .unwrap()
                    .to_path_buf(),
                manager
                    .workspaces
                    .project_root(WorkspaceId::new(2))
                    .unwrap()
                    .map(Path::to_path_buf),
                manager
                    .workspaces
                    .display_name(WorkspaceId::new(2), &manager.default_workspace_root)
                    .unwrap(),
            )
        });
        assert_eq!(
            state,
            (
                2,
                WorkspaceId::new(2),
                WorkspaceKind::LocalProject,
                project_root.clone(),
                Some(project_root.clone()),
                "site".to_owned(),
            ),
            "the selected path must be preserved exactly as the immutable Project Root"
        );
        assert_eq!(invocations.get(), 1);
        assert_eq!(
            records
                .starts()
                .last()
                .expect("the Project session must start")
                .working_directory,
            project_root
        );
        assert!(
            records.commands().iter().any(|call| call.session_id == 1
                && matches!(call.command, RecordedSessionCommand::Focus(false))),
            "the previous Workspace must be deactivated"
        );
    }

    #[gpui::test]
    fn reopening_the_same_directory_through_an_alias_should_activate_the_existing_project(
        cx: &mut TestAppContext,
    ) {
        let original = unique_project_root("dup-original");
        std::fs::create_dir_all(&original).expect("create the Project root");
        let alias_parent = unique_project_root("dup-alias");
        std::fs::create_dir_all(&alias_parent).expect("create the alias parent");
        let alias = alias_parent.join("link");
        std::os::unix::fs::symlink(&original, &alias).expect("create the alias symlink");

        let case_variant = PathBuf::from(original.to_string_lossy().to_uppercase());
        let original_basename = original
            .file_name()
            .expect("the Project root must have a file name")
            .to_string_lossy()
            .into_owned();
        let mut reopen_paths = vec![alias];
        if canonical_identity(&case_variant) == canonical_identity(&original) {
            reopen_paths.push(case_variant);
        }
        let mut selections = vec![Some(original.clone())];
        selections.extend(reopen_paths.iter().map(|path| Some(path.clone())));

        let (manager, invocations, records, cx) = workspace_manager_with_picker(selections, cx);

        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();
        for _ in 0..reopen_paths.len() {
            click("workspace-row-1-inactive", cx);
            cx.simulate_keystrokes("cmd-o");
            cx.run_until_parked();
        }

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                manager.workspaces.kind(WorkspaceId::new(2)).unwrap(),
                manager
                    .workspaces
                    .project_root(WorkspaceId::new(2))
                    .unwrap()
                    .map(Path::to_path_buf),
                manager
                    .workspaces
                    .display_name(WorkspaceId::new(2), &manager.default_workspace_root)
                    .unwrap(),
                records.starts().len(),
            )
        });
        assert_eq!(
            state,
            (
                2,
                WorkspaceId::new(2),
                WorkspaceKind::LocalProject,
                Some(original),
                original_basename,
                2,
            ),
            "an alias or case-variant selection must activate the existing \
             Workspace with its originally selected display path",
        );
        assert_eq!(invocations.get(), 1 + reopen_paths.len());
    }

    #[gpui::test]
    fn opening_a_project_at_the_harness_home_should_coexist_with_the_ad_hoc_workspace(
        cx: &mut TestAppContext,
    ) {
        let home = harness_workspace_root();
        let (manager, _, _, cx) = workspace_manager_with_picker(vec![Some(home.clone())], cx);

        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.kind(WorkspaceId::new(1)).unwrap(),
                manager
                    .workspaces
                    .workspace_directory(WorkspaceId::new(1))
                    .unwrap()
                    .to_path_buf(),
                manager.workspaces.kind(WorkspaceId::new(2)).unwrap(),
                manager
                    .workspaces
                    .project_root(WorkspaceId::new(2))
                    .unwrap()
                    .map(Path::to_path_buf),
                manager.workspaces.active_workspace_id(),
            )
        });
        assert_eq!(
            state,
            (
                2,
                WorkspaceKind::AdHoc,
                home.clone(),
                WorkspaceKind::LocalProject,
                Some(home),
                WorkspaceId::new(2),
            ),
            "an Ad Hoc Workspace at the same directory must remain distinct from \
             the opened Local Project",
        );
    }

    #[gpui::test]
    fn cancelling_the_picker_should_leave_the_hierarchy_unchanged(cx: &mut TestAppContext) {
        let (manager, invocations, records, cx) = workspace_manager_with_picker(vec![None], cx);
        let before = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
            )
        });
        let starts_before = records.starts().len();
        let sessions_before = records.session_count();

        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();

        let after = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
            )
        });
        assert_eq!(after, before);
        assert_eq!(
            (
                records.starts().len(),
                records.session_count(),
                invocations.get()
            ),
            (starts_before, sessions_before, 1)
        );
    }

    #[gpui::test]
    fn unusable_selections_should_leave_the_hierarchy_unchanged_and_present_a_notice(
        cx: &mut TestAppContext,
    ) {
        let missing = unique_project_root("missing-selection");
        let file_selection = unique_project_root("file-selection");
        std::fs::write(&file_selection, b"payload").expect("create the selection file");
        let (manager, invocations, records, cx) =
            workspace_manager_with_picker(vec![Some(missing), Some(file_selection)], cx);

        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();

        let after_first = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                manager.creation_block_notice.clone(),
                records.starts().len(),
            )
        });
        assert_eq!(
            after_first,
            (
                1,
                WorkspaceId::new(1),
                Some(UNUSABLE_PROJECT_NOTICE.to_owned()),
                1,
            ),
            "a nonexistent directory must not open a Workspace"
        );

        manager.update_in(cx, |manager, window, cx| {
            manager.open_local_project(window, cx);
        });
        cx.run_until_parked();

        let after_second = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                manager.creation_block_notice.is_some(),
                records.starts().len(),
            )
        });
        assert_eq!(after_second, (1, WorkspaceId::new(1), true, 1));
        assert!(cx.debug_bounds("creation-block-notice").is_some());
        assert_eq!(invocations.get(), 2);
    }

    #[gpui::test]
    fn cmd_o_should_reach_the_handler_and_reentry_should_be_blocked_while_the_picker_is_open(
        cx: &mut TestAppContext,
    ) {
        let first = unique_project_root("guard-first");
        std::fs::create_dir_all(&first).expect("create the first Project root");
        let second = unique_project_root("guard-second");
        std::fs::create_dir_all(&second).expect("create the second Project root");
        let (manager, invocations, _records, cx) =
            workspace_manager_with_picker(vec![Some(first), Some(second)], cx);

        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();

        // Two immediate reentries while one flow is already running: only the
        // first may reach the injected picker.
        manager.update_in(cx, |manager, window, cx| {
            manager.open_local_project(window, cx);
            manager.open_local_project(window, cx);
        });

        assert_eq!(invocations.get(), 2);
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
            )
        });
        assert_eq!(
            state,
            (3, WorkspaceId::new(3)),
            "exactly one of the two immediate invocations must proceed"
        );
    }

    #[gpui::test]
    fn deleting_a_project_directory_should_mark_it_unavailable_on_reactivation(
        cx: &mut TestAppContext,
    ) {
        let project_root = unique_project_root("revalidate-open");
        std::fs::create_dir_all(&project_root).expect("create the Project root");
        let (manager, _, _, cx) = workspace_manager_with_picker(vec![Some(project_root)], cx);

        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();

        let project_root = manager.read_with(cx, |manager, _| {
            manager
                .workspaces
                .project_root(WorkspaceId::new(2))
                .expect("the Project must remain owned")
                .expect("the Project must have a root")
                .to_path_buf()
        });
        std::fs::remove_dir_all(&project_root).expect("delete the Project root");
        click("workspace-row-1-inactive", cx);
        click("workspace-row-2-inactive", cx);

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                manager.workspaces.kind(WorkspaceId::new(2)).unwrap(),
                manager
                    .workspaces
                    .directory_available(WorkspaceId::new(2))
                    .unwrap(),
                manager
                    .workspaces
                    .directory_available(WorkspaceId::new(1))
                    .unwrap(),
                manager.directory_gates[&WorkspaceId::new(2)]
                    .resolve()
                    .is_err(),
            )
        });
        assert_eq!(
            state,
            (
                2,
                WorkspaceId::new(2),
                WorkspaceKind::LocalProject,
                false,
                true,
                true,
            ),
            "reactivation must mark the deleted Project Root unavailable in the \
             collection and its gate",
        );
    }

    #[gpui::test]
    fn workspace_rows_should_render_kind_icons_per_workspace_kind(cx: &mut TestAppContext) {
        let project_root = unique_project_root("kind-icons");
        std::fs::create_dir_all(&project_root).expect("create the Project root");
        let (_manager, _invocations, _records, cx) =
            workspace_manager_with_picker(vec![Some(project_root)], cx);

        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();

        let adhoc_icon = cx
            .debug_bounds("workspace-kind-icon-1-adhoc")
            .expect("the Ad Hoc row must render its kind icon");
        let project_icon = cx
            .debug_bounds("workspace-kind-icon-2-project")
            .expect("the Local Project row must render its kind icon");
        assert!(adhoc_icon.size.width > px(0.0) && project_icon.size.width > px(0.0));
        assert!(cx.debug_bounds("workspace-kind-icon-1-project").is_none());
        assert!(cx.debug_bounds("workspace-kind-icon-2-adhoc").is_none());
    }

    #[gpui::test]
    fn workspace_rows_should_track_aggregate_window_and_pane_counts(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);
        let active_counts = |manager: &Entity<WorkspaceManager>, cx: &mut VisualTestContext| {
            manager.read_with(cx, |manager, app| {
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(app)
                    .aggregate_counts(app)
            })
        };
        assert!(
            cx.debug_bounds("workspace-row-counts-1-1W · 1P").is_some(),
            "a fresh Workspace must present one Window and one Pane"
        );

        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();
        assert!(cx.debug_bounds("workspace-row-counts-1-2W · 2P").is_some());
        assert_eq!(active_counts(&manager, cx), (2, 2));

        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();
        assert!(cx.debug_bounds("workspace-row-counts-1-2W · 3P").is_some());
        assert_eq!(active_counts(&manager, cx), (2, 3));

        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        assert_eq!(active_counts(&manager, cx), (2, 2));

        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        assert_eq!(active_counts(&manager, cx), (1, 1));
    }

    #[gpui::test]
    fn workspace_rows_should_present_secondary_directory_lines_without_truncation(
        cx: &mut TestAppContext,
    ) {
        let outside = PathBuf::from("/tmp/spaceterm-short-project");
        std::fs::create_dir_all(&outside).expect("create the short Project root");
        let (manager, _invocations, _records, cx) =
            workspace_manager_with_picker(vec![Some(outside.clone())], cx);

        // The Ad Hoc Workspace sits at the harness HOME and compacts to "~".
        let home_line = manager.read_with(cx, |manager, _| {
            let directory = manager
                .workspaces
                .workspace_directory(WorkspaceId::new(1))
                .expect("Workspace 1 must remain owned");
            secondary_path_line(directory, &manager.default_workspace_root)
        });
        assert_eq!(home_line, ("~".to_owned(), false));
        assert!(cx.debug_bounds("workspace-row-path-1").is_some());
        assert!(cx.debug_bounds("workspace-row-path-1-truncated").is_none());

        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();

        // A Local Project outside HOME keeps its full absolute path.
        let outside_state = manager.read_with(cx, |manager, _| {
            (
                manager
                    .workspaces
                    .workspace_directory(WorkspaceId::new(2))
                    .expect("Workspace 2 must remain owned")
                    .to_path_buf(),
                manager.workspaces.active_workspace_id(),
            )
        });
        assert_eq!(outside_state.0, outside);
        assert_eq!(outside_state.1, WorkspaceId::new(2));
        assert!(cx.debug_bounds("workspace-row-path-2").is_some());
        assert!(cx.debug_bounds("workspace-row-path-2-truncated").is_none());
    }

    #[gpui::test]
    fn deleting_a_project_directory_should_warn_on_the_kind_icon(cx: &mut TestAppContext) {
        let project_root = unique_project_root("icon-warning");
        std::fs::create_dir_all(&project_root).expect("create the Project root");
        let (manager, _, _, cx) = workspace_manager_with_picker(vec![Some(project_root)], cx);

        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();
        assert!(cx.debug_bounds("workspace-kind-icon-2-project").is_some());
        assert!(
            cx.debug_bounds("workspace-kind-icon-2-project-unavailable")
                .is_none()
        );

        let project_root = manager.read_with(cx, |manager, _| {
            manager
                .workspaces
                .project_root(WorkspaceId::new(2))
                .expect("the Project must remain owned")
                .expect("the Project must have a root")
                .to_path_buf()
        });
        std::fs::remove_dir_all(&project_root).expect("delete the Project root");
        click("workspace-row-1-inactive", cx);
        click("workspace-row-2-inactive", cx);
        cx.run_until_parked();

        let availability = manager.read_with(cx, |manager, _| {
            (
                manager
                    .workspaces
                    .directory_available(WorkspaceId::new(2))
                    .expect("Workspace 2 must remain owned"),
                manager
                    .workspaces
                    .directory_available(WorkspaceId::new(1))
                    .expect("Workspace 1 must remain owned"),
            )
        });
        assert_eq!(availability, (false, true));
        assert!(
            cx.debug_bounds("workspace-kind-icon-2-project-unavailable")
                .is_some(),
            "an unavailable Workspace Directory must carry the warning treatment"
        );
    }

    #[gpui::test]
    fn custom_names_should_render_as_the_primary_label(cx: &mut TestAppContext) {
        let (manager, records, cx) = workspace_manager(cx);
        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-rename", cx);
        cx.simulate_keystrokes("cmd-a D e v enter");
        cx.run_until_parked();

        // Directory changes underneath must not disturb the frozen name.
        let renamed_directory = valid_authority_directory("custom-name");
        send_reported_directory(&records, 1, &renamed_directory);
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager
                    .workspaces
                    .display_name(WorkspaceId::new(1), &manager.default_workspace_root)
                    .expect("Workspace 1 must remain owned"),
                manager
                    .workspaces
                    .workspace_directory(WorkspaceId::new(1))
                    .expect("Workspace 1 must remain owned")
                    .to_path_buf(),
            )
        });
        assert_eq!(state, ("Dev".to_owned(), renamed_directory));
    }

    #[gpui::test]
    fn search_input_should_accept_text_without_filtering_or_reaching_the_terminal(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();

        let search_focus = manager.read_with(cx, |manager, _| manager.search_focus.clone());
        cx.update(|window, _| search_focus.focus(window));
        cx.run_until_parked();
        let command_count = records.commands().len();

        cx.simulate_keystrokes("w o r k");
        cx.run_until_parked();

        let state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.search.read(cx).value().to_owned(),
                manager.workspaces.len(),
                manager.search_is_focused(window),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_has_input_focus(window, cx),
            )
        });
        assert_eq!(
            state,
            ("work".to_owned(), 2, true, false,),
            "typing must be accepted without filtering rows or releasing Terminal Input Focus"
        );

        let terminal_keys = records
            .commands()
            .into_iter()
            .skip(command_count)
            .filter(|call| matches!(call.command, RecordedSessionCommand::Key(_)))
            .count();
        assert_eq!(
            terminal_keys, 0,
            "terminal input must stay blocked while search holds focus"
        );
        assert!(cx.debug_bounds("workspace-row-divider-1").is_some());
        assert!(cx.debug_bounds("workspace-row-divider-2").is_some());
        assert!(cx.debug_bounds("workspace-search-input").is_some());
    }

    #[gpui::test]
    fn focusing_the_search_input_should_cancel_an_active_inline_rename(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);
        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-rename", cx);
        assert!(manager.read_with(cx, |manager, _| manager.rename.is_some()));

        click("workspace-search-input", cx);
        cx.run_until_parked();

        let state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.search_is_focused(window),
                manager.rename.as_ref().map(|r| r.workspace_id.get()),
                manager.sidebar_focus.is_focused(window),
                manager.rename.is_none(),
            )
        });
        assert!(state.3);
    }
    #[gpui::test]
    fn folder_add_button_should_open_a_local_project_through_the_picker(cx: &mut TestAppContext) {
        let project_root = unique_project_root("header-button");
        std::fs::create_dir_all(&project_root).expect("create the Project root");
        let (manager, invocations, records, cx) =
            workspace_manager_with_picker(vec![Some(project_root.clone())], cx);

        click("open-local-project-button", cx);
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
                manager.workspaces.kind(WorkspaceId::new(2)).unwrap(),
                manager
                    .workspaces
                    .workspace_directory(WorkspaceId::new(2))
                    .unwrap()
                    .to_path_buf(),
                records
                    .starts()
                    .last()
                    .map(|start| start.working_directory.clone()),
            )
        });
        assert_eq!(
            state,
            (
                2,
                WorkspaceId::new(2),
                WorkspaceKind::LocalProject,
                project_root.clone(),
                Some(project_root),
            ),
            "the header button must run the existing Open Local Project flow"
        );
        assert_eq!(invocations.get(), 1);
        assert!(cx.debug_bounds("open-local-project-button").is_some());
    }

    #[test]
    fn home_compacted_display_should_compact_directories_under_home() {
        let home = Path::new("/Users/dev");
        assert_eq!(
            home_compacted_display(Path::new("/Users/dev"), home),
            "~",
            "HOME itself compacts to a bare tilde"
        );
        assert_eq!(
            home_compacted_display(Path::new("/Users/dev/projects/site"), home),
            "~/projects/site"
        );
    }

    #[test]
    fn home_compacted_display_should_keep_sibling_and_external_paths_absolute() {
        let home = Path::new("/Users/dev");
        assert_eq!(
            home_compacted_display(Path::new("/Users/devtools"), home),
            "/Users/devtools",
            "component-wise matching must not compact sibling names"
        );
        assert_eq!(
            home_compacted_display(Path::new("/opt/special"), home),
            "/opt/special"
        );
    }

    #[test]
    fn middle_truncation_should_preserve_head_and_tail_within_the_budget() {
        let text = "abcdefghij";
        let (truncated, engaged) = middle_truncated(text, 7);
        assert!(engaged);
        assert_eq!(truncated.chars().count(), 7);
        assert_eq!(truncated, "abc…hij");
    }

    #[test]
    fn middle_truncation_should_leave_short_text_untouched() {
        assert_eq!(
            middle_truncated("~/projects/site", SECONDARY_PATH_CHARACTER_BUDGET),
            ("~/projects/site".to_owned(), false)
        );
    }
}
