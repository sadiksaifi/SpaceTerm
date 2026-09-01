use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::native_remote_workspace_flow_backend::NativeRemoteWorkspaceFlowBackendFactory;
use super::new_workspace_panel::{NewWorkspacePanel, NewWorkspacePanelEvent, NewWorkspaceSource};
use super::remote_workspace_flow::{
    RemoteWorkspaceAliasPin, RemoteWorkspaceConnectContext, RemoteWorkspaceConnectedSession,
    RemoteWorkspaceConnectionProgress, RemoteWorkspaceFlow, RemoteWorkspaceFlowBackend,
    RemoteWorkspaceFlowBackendError, RemoteWorkspaceFlowBackendFactory,
    RemoteWorkspaceFlowCompletion, RemoteWorkspaceFlowCompletionHandle, RemoteWorkspaceFlowEvent,
};
use super::terminal_focus::TerminalFocusBlocker;
use super::window_manager::{
    PreparedWindowManagerRemoteRestart, RemoteWindowManagerLifecycleError,
};
use super::workspace_picker::{WorkspacePicker, WorkspacePickerEvent};
use super::workspace_search::{WorkspaceSearch, WorkspaceSearchEvent, WorkspaceSearchItem};
use super::{
    ActivateWindow1, ActivateWindow2, ActivateWindow3, ActivateWindow4, ActivateWindow5,
    ActivateWindow6, ActivateWindow7, ActivateWindow8, ActivateWindow9, ActivateWorkspace1,
    ActivateWorkspace2, ActivateWorkspace3, ActivateWorkspace4, ActivateWorkspace5,
    ActivateWorkspace6, ActivateWorkspace7, ActivateWorkspace8, ActivateWorkspace9, ClosePane,
    CloseTerminalFind, CloseWindow, CloseWorkspace, CopySelection, CreateScratchWorkspace,
    CreateWindow, FindNext, FindPrevious, FocusPaneDown, FocusPaneLeft, FocusPaneRight,
    FocusPaneUp, OpenLocalProject, OpenTerminalFind, RemoteChildLaunchUnavailable,
    SearchWorkspaces, ShowNewWorkspacePanel, SplitDown, SplitRight, TERMINAL_KEY_CONTEXT,
    TOP_CHROME_HEIGHT, TogglePaneZoom, ToggleSidebar, ToggleSidebarFocus,
    WORKSPACE_SIDEBAR_DEFAULT_WIDTH, WORKSPACE_SIDEBAR_MINIMUM_WIDTH, WindowManager,
    WindowManagerEvent,
};
use crate::domain::{
    CloseWorkspaceOutcome, CreateRemoteProjectOutcome, DirectoryAuthority, FinalWindowCloseOutcome,
    RemoteConnectionPhase, RemoteConnectionReduction, RemoteConnectionState,
    RemoteWorkspaceDirectory, RemoteWorkspaceKey, ValidatedWorkspaceDirectory, WorkspaceCollection,
    WorkspaceDirectoryAvailability, WorkspaceDirectoryIdentity, WorkspaceError, WorkspaceId,
    WorkspaceKind,
};
use crate::platform::finder_fallback::{FinderFallback, NativeFinderFallback};
use crate::platform::macos_system_settings::MacosSystemSettingsOpener;
use crate::platform::macos_window_drag::{
    MacosOperatingSystemWindowDragPlatform, OperatingSystemWindowDragError,
    OperatingSystemWindowDragPlatform,
};
use crate::platform::workspace_directory::validate_workspace_directory;
use crate::platform::workspace_picker_filesystem::NativeWorkspacePickerFilesystem;
use crate::ssh::live_connection::{ControlConnectionObserver, ControlConnectionTerminalState};
use crate::ssh::process::TransientSshErrorOutput;
use crate::terminal::metadata::RemoteTerminalMetadataContext;
use crate::terminal::{
    NativeServiceOrigin, NativeServiceStatus, PreparedWorkspaceTerminalLaunch, SelectionCopy,
    TerminalSessionFactory, WorkspaceTerminalSessionFactory,
};
use crate::theme::{ACTIVE_THEME, Color};
use gpui::prelude::*;
use gpui::{
    Action, AnyElement, App, Context, DispatchPhase, Entity, EntityId, FocusHandle, MouseButton,
    MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollHandle, ScrollWheelEvent, SharedString,
    Task, WeakEntity, Window, canvas, div, point, px, rgba,
};
use gpui_symbols::{Icon, RenderingMode, SymbolWeight};
use spaceterm_ui::{
    Alert, AlertIntent, Button, ButtonShape, ButtonSize, ButtonVariant, ContextMenu, IconButton,
    MenuEntry, MenuLifecycleEvent, MenuSize, MiddleTruncatedText, ModalAction, ModalActionRole,
    ModalId, ModalLayer, OverlayScrollbar, OverlayScrollbarEvent, ProgressCancelDecision,
    ProgressCancellation, ProgressDialog, ProgressDialogHandle, ProgressDialogOutcome,
    ProgressDialogUpdate, ProgressState, ResizeAxis, ResizeFinishReason, ResizeHandle,
    ResizeHandleEvent, ResizeInputSource, ScrollMetrics, TextInput, TextInputEvent,
    TextInputVariant, Tooltip, TooltipLayer, TooltipTargetVisibility, WindowDragRegion,
    WindowDragRegionEvent, WindowDragRegionResponse, WindowDragRegionStatus, window_modal_is_open,
};

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
const SIDEBAR_ROW_PIN_ICON_SIZE: f32 = 9.0;
/// Clearance for the native traffic lights that share the top-left chrome strip.
const TRAFFIC_LIGHT_CLEARANCE: f32 = 82.0;
const WORKSPACE_CHIP_ICON_SIZE: f32 = 11.0;
const WORKSPACE_CHIP_TEXT_SIZE: f32 = 11.0;
const SIDEBAR_NAME_TEXT_SIZE: f32 = 13.0;
const SIDEBAR_DETAIL_TEXT_SIZE: f32 = 11.0;
const NEW_WORKSPACE_BUTTON_HEIGHT: f32 = 40.0;
const CHROME_DIVIDER_SIZE: f32 = super::resize_handle_theme::VISIBLE_THICKNESS;
const SIDEBAR_MAXIMUM_WIDTH: f32 = 420.0;
const TERMINAL_CONTENT_MINIMUM_WIDTH: f32 = 240.0;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspaceMenuCommand {
    NewWindow,
    Rename,
    Reconnect,
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
    context_menu_open: bool,
}

struct WorkspaceRowViewModel {
    workspace_id: WorkspaceId,
    name: SharedString,
    path: SharedString,
    tooltip: SharedString,
    local_project: bool,
    remote_connection_phase: Option<RemoteConnectionPhase>,
    available: bool,
    window_count: usize,
    pane_count: usize,
    active: bool,
}

struct RemoteWorkspaceRuntime {
    generation: u64,
    session: Option<RemoteWorkspaceConnectedSession>,
    lifecycle: Option<ControlConnectionObserver>,
    alias_pin: Option<RemoteWorkspaceAliasPin>,
}

impl RemoteWorkspaceRuntime {
    fn new(
        generation: u64,
        session: RemoteWorkspaceConnectedSession,
        lifecycle: ControlConnectionObserver,
        alias_pin: Option<RemoteWorkspaceAliasPin>,
    ) -> Self {
        Self {
            generation,
            session: Some(session),
            lifecycle: Some(lifecycle),
            alias_pin,
        }
    }

    fn close(&mut self) {
        self.lifecycle.take();
        self.session.take();
        self.alias_pin.take();
    }
}

struct RemoteWorkspaceReconnectAttempt {
    workspace_id: WorkspaceId,
    generation: u64,
    cancelled: Arc<AtomicBool>,
    progress: ProgressDialogHandle,
    _work: Task<()>,
    _progress_updates: Task<()>,
}

struct PreparedRemoteWorkspaceReconnect {
    session: RemoteWorkspaceConnectedSession,
    lifecycle: ControlConnectionObserver,
    restart: PreparedWindowManagerRemoteRestart,
}

struct WindowManagerCreation {
    workspace_id: WorkspaceId,
    sidebar_visible: bool,
    sidebar_width: Pixels,
    operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
}

struct PendingRemoteProjectActivation {
    flow: Entity<RemoteWorkspaceFlow>,
    handle: RemoteWorkspaceFlowCompletionHandle,
    completion: RemoteWorkspaceFlowCompletion,
    terminal_factory: WorkspaceTerminalSessionFactory,
}

#[derive(Debug, Eq, PartialEq)]
enum RemoteWorkspaceReconnectFailure {
    Cancelled,
    ConnectionFailed {
        detail: Option<TransientSshErrorOutput>,
    },
    DirectoryUnavailable,
    IdentityChanged,
}

impl Drop for RemoteWorkspaceReconnectAttempt {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl Drop for RemoteWorkspaceRuntime {
    fn drop(&mut self) {
        self.close();
    }
}

pub(crate) struct WorkspaceManager {
    workspaces: WorkspaceCollection<Entity<WindowManager>>,
    session_factory: Rc<dyn TerminalSessionFactory>,
    default_workspace_root: PathBuf,
    default_workspace_identity: WorkspaceDirectoryIdentity,
    finder_fallback: Rc<dyn FinderFallback>,
    workspace_picker: Entity<WorkspacePicker>,
    sidebar_visible: bool,
    sidebar_width: Pixels,
    workspace_list_scroll_handle: ScrollHandle,
    scrollbar: Entity<OverlayScrollbar<f32>>,
    sidebar_focus: FocusHandle,
    workspace_search: Entity<WorkspaceSearch>,
    new_workspace_panel: Entity<NewWorkspacePanel>,
    remote_workspace_backend: Option<Arc<dyn RemoteWorkspaceFlowBackend>>,
    remote_workspace_flow: Option<Entity<RemoteWorkspaceFlow>>,
    remote_workspace_runtimes: BTreeMap<WorkspaceId, RemoteWorkspaceRuntime>,
    remote_workspace_activation_task: Option<Task<()>>,
    remote_workspace_reconnect: Option<RemoteWorkspaceReconnectAttempt>,
    #[cfg(test)]
    close_focused_pane_before_reconnect_commit: bool,
    picker_entered_from_panel: bool,
    workspace_menu: Option<WorkspaceMenuState>,
    rename: Option<WorkspaceRenameState>,
    sidebar_resize_interaction: bool,
    suppress_sidebar_pointer_until_release: bool,
    operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
    window_drag_status: WindowDragRegionStatus,
    pending_final_window_closes: BTreeSet<WorkspaceId>,
}

impl WorkspaceManager {
    pub(crate) fn new(
        session_factory: Rc<dyn TerminalSessionFactory>,
        default_workspace_root: PathBuf,
        remote_workspace_backend_factory: Arc<NativeRemoteWorkspaceFlowBackendFactory>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let remote_workspace_backend_factory: Arc<dyn RemoteWorkspaceFlowBackendFactory> =
            remote_workspace_backend_factory;
        Self::new_with_adapters(
            session_factory,
            default_workspace_root,
            Rc::new(NativeFinderFallback),
            Rc::new(MacosOperatingSystemWindowDragPlatform::default()),
            remote_workspace_backend_factory,
            window,
            cx,
        )
    }

    #[cfg(test)]
    fn new_with_remote_workspace_backend_factory(
        session_factory: Rc<dyn TerminalSessionFactory>,
        default_workspace_root: PathBuf,
        remote_workspace_backend_factory: Arc<dyn RemoteWorkspaceFlowBackendFactory>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_adapters(
            session_factory,
            default_workspace_root,
            Rc::new(NativeFinderFallback),
            Rc::new(MacosOperatingSystemWindowDragPlatform::default()),
            remote_workspace_backend_factory,
            window,
            cx,
        )
    }

    #[cfg(test)]
    fn new_with_finder_fallback(
        session_factory: Rc<dyn TerminalSessionFactory>,
        default_workspace_root: PathBuf,
        finder_fallback: Rc<dyn FinderFallback>,
        remote_workspace_backend_factory: Arc<dyn RemoteWorkspaceFlowBackendFactory>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_adapters(
            session_factory,
            default_workspace_root,
            finder_fallback,
            Rc::new(MacosOperatingSystemWindowDragPlatform::default()),
            remote_workspace_backend_factory,
            window,
            cx,
        )
    }

    #[cfg(test)]
    fn new_with_operating_system_window_drag_platform(
        session_factory: Rc<dyn TerminalSessionFactory>,
        default_workspace_root: PathBuf,
        operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
        remote_workspace_backend_factory: Arc<dyn RemoteWorkspaceFlowBackendFactory>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_adapters(
            session_factory,
            default_workspace_root,
            Rc::new(NativeFinderFallback),
            operating_system_window_drag_platform,
            remote_workspace_backend_factory,
            window,
            cx,
        )
    }

    fn new_with_adapters(
        session_factory: Rc<dyn TerminalSessionFactory>,
        default_workspace_root: PathBuf,
        finder_fallback: Rc<dyn FinderFallback>,
        operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
        remote_workspace_backend_factory: Arc<dyn RemoteWorkspaceFlowBackendFactory>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (default_directory, initial_directory_error) =
            initial_workspace_directory(default_workspace_root.clone());
        let default_workspace_identity = default_directory.identity();
        let initial_workspace_identity = default_directory.identity();
        let initial_window_drag_platform = Rc::clone(&operating_system_window_drag_platform);
        let mut workspaces = WorkspaceCollection::new_scratch(
            default_directory,
            DirectoryAuthority::initial(),
            |workspace_id, workspace_root| {
                Self::create_local_window_manager(
                    workspace_id,
                    WorkspaceTerminalSessionFactory::new_local(
                        Rc::clone(&session_factory),
                        ValidatedWorkspaceDirectory::new(
                            workspace_root.to_path_buf(),
                            initial_workspace_identity,
                        ),
                    ),
                    true,
                    px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
                    Rc::clone(&initial_window_drag_platform),
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
        let workspace_search = cx.new(|cx| WorkspaceSearch::new(window, cx));
        cx.subscribe_in(
            &workspace_search,
            window,
            |manager, _, event: &WorkspaceSearchEvent, window, cx| {
                manager.handle_workspace_search_event(event, window, cx);
            },
        )
        .detach();
        let workspace_picker_home =
            Self::workspace_picker_starting_directory(&default_workspace_root);
        let workspace_picker = cx.new(|cx| {
            WorkspacePicker::new(
                workspace_picker_home,
                Arc::new(NativeWorkspacePickerFilesystem),
                Rc::new(MacosSystemSettingsOpener::default()),
                window,
                cx,
            )
        });
        cx.subscribe_in(
            &workspace_picker,
            window,
            |manager, _, event: &WorkspacePickerEvent, window, cx| {
                manager.handle_workspace_picker_event(event, window, cx);
            },
        )
        .detach();
        let mut remote_unavailable_reason = remote_workspace_backend_factory.unavailable_reason();
        let remote_workspace_backend = if remote_unavailable_reason.is_some() {
            None
        } else {
            match remote_workspace_backend_factory.create(window, cx) {
                Ok(backend) => Some(backend),
                Err(error) => {
                    eprintln!("failed to initialize Remote Workspace backend: {error:?}");
                    remote_unavailable_reason = Some(
                        "Remote Workspace setup failed; restart SpaceTerm to retry".to_owned(),
                    );
                    None
                }
            }
        };
        let new_workspace_panel = cx.new(|cx| {
            NewWorkspacePanel::new_with_remote_unavailable_reason(
                remote_unavailable_reason,
                window,
                cx,
            )
        });
        cx.subscribe_in(
            &new_workspace_panel,
            window,
            |manager, _, event: &NewWorkspacePanelEvent, window, cx| {
                manager.handle_new_workspace_panel_event(event, window, cx);
            },
        )
        .detach();

        Self {
            workspaces,
            session_factory,
            default_workspace_root,
            default_workspace_identity,
            finder_fallback,
            workspace_picker,
            sidebar_visible: true,
            sidebar_width: px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            workspace_list_scroll_handle: ScrollHandle::new(),
            scrollbar,
            sidebar_focus: cx.focus_handle(),
            workspace_search,
            new_workspace_panel,
            remote_workspace_backend,
            remote_workspace_flow: None,
            remote_workspace_runtimes: BTreeMap::new(),
            remote_workspace_activation_task: None,
            remote_workspace_reconnect: None,
            #[cfg(test)]
            close_focused_pane_before_reconnect_commit: false,
            picker_entered_from_panel: false,
            workspace_menu: None,
            rename: None,
            sidebar_resize_interaction: false,
            suppress_sidebar_pointer_until_release: false,
            operating_system_window_drag_platform,
            window_drag_status: WindowDragRegionStatus::new(),
            pending_final_window_closes: BTreeSet::new(),
        }
    }

    fn create_local_window_manager(
        workspace_id: WorkspaceId,
        session_factory: WorkspaceTerminalSessionFactory,
        sidebar_visible: bool,
        sidebar_width: Pixels,
        operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<WindowManager> {
        match Self::try_create_window_manager(
            session_factory,
            WindowManagerCreation {
                workspace_id,
                sidebar_visible,
                sidebar_width,
                operating_system_window_drag_platform,
            },
            window,
            cx,
        ) {
            Ok(manager) => manager,
            Err(error) => unreachable!("Local initial launch preparation is infallible: {error}"),
        }
    }

    fn try_create_window_manager(
        session_factory: WorkspaceTerminalSessionFactory,
        creation: WindowManagerCreation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Entity<WindowManager>, crate::terminal::RemoteChannelUnavailable> {
        let prepared_launch = session_factory.prepare_child_launch()?;
        Ok(Self::create_window_manager_with_prepared_launch(
            session_factory,
            prepared_launch,
            creation,
            window,
            cx,
        ))
    }

    fn create_window_manager_with_prepared_launch(
        session_factory: WorkspaceTerminalSessionFactory,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        creation: WindowManagerCreation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<WindowManager> {
        let WindowManagerCreation {
            workspace_id,
            sidebar_visible,
            sidebar_width,
            operating_system_window_drag_platform,
        } = creation;
        let manager = cx.new(|cx| {
            let mut manager = WindowManager::new_with_prepared_initial_launch(
                session_factory,
                prepared_launch,
                operating_system_window_drag_platform,
                window,
                cx,
            );
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
                WindowManagerEvent::PresentationChanged => {
                    workspace_manager.refresh_workspace_search(cx);
                    cx.notify();
                }
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
                    workspace_manager.refresh_workspace_search(cx);
                    cx.notify();
                }
                WindowManagerEvent::DirectoryUnavailable { reason } => {
                    let _ = workspace_manager
                        .workspaces
                        .set_directory_unavailable(workspace_id, reason.clone());
                    workspace_manager.refresh_workspace_search(cx);
                    cx.notify();
                }
            },
        )
        .detach();
        cx.subscribe_in(
            &manager,
            window,
            move |workspace_manager, _, event: &RemoteChildLaunchUnavailable, window, cx| {
                workspace_manager.handle_remote_child_launch_unavailable(
                    workspace_id,
                    *event,
                    window,
                    cx,
                );
            },
        )
        .detach();
        manager
    }

    fn handle_remote_child_launch_unavailable(
        &mut self,
        workspace_id: WorkspaceId,
        event: RemoteChildLaunchUnavailable,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .workspaces
            .workspace(workspace_id)
            .is_none_or(|workspace| workspace.remote_workspace_key().is_none())
        {
            return;
        }
        let failure = match event {
            RemoteChildLaunchUnavailable::ConnectionUnavailable => {
                RemoteWorkspaceReconnectFailure::ConnectionFailed { detail: None }
            }
            RemoteChildLaunchUnavailable::DirectoryUnavailable => {
                RemoteWorkspaceReconnectFailure::DirectoryUnavailable
            }
            RemoteChildLaunchUnavailable::IdentityChanged => {
                RemoteWorkspaceReconnectFailure::IdentityChanged
            }
            RemoteChildLaunchUnavailable::Cancelled | RemoteChildLaunchUnavailable::Stale => {
                return;
            }
        };
        self.present_remote_workspace_reconnect_error(failure, window, cx);
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
                    self.refresh_workspace_search(cx);
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
        let (Some(path), Some(identity)) = (
            workspace.working_directory(),
            workspace.directory_identity(),
        ) else {
            unreachable!("a Scratch Workspace must own a local Workspace Directory")
        };
        let path = path.to_path_buf();
        manager.update(cx, |manager, cx| {
            manager.set_workspace_directory(&path, identity, cx);
        });
        self.refresh_workspace_search(cx);
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
        let (Some(path), Some(identity)) = (
            workspace.working_directory(),
            workspace.directory_identity(),
        ) else {
            unreachable!("a Scratch Workspace must own a local Workspace Directory")
        };
        let path = path.to_path_buf();
        manager.update(cx, |manager, cx| {
            manager.set_workspace_directory(&path, identity, cx)
        });
        self.refresh_workspace_search(cx);
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
        let (Some(path), Some(identity)) = (
            workspace.working_directory(),
            workspace.directory_identity(),
        ) else {
            unreachable!("a Scratch Workspace must own a local Workspace Directory")
        };
        let path = path.to_path_buf();
        manager.update(cx, |manager, cx| {
            manager.set_workspace_directory(&path, identity, cx)
        });
        self.refresh_workspace_search(cx);
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
        let blocker = self.terminal_focus_blocker(window, cx);
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

    fn terminal_focus_blocker(&self, window: &Window, cx: &App) -> Option<TerminalFocusBlocker> {
        window_modal_is_open(window, cx)
            .then_some(TerminalFocusBlocker::Modal)
            .or_else(|| self.non_modal_terminal_focus_blocker(window, cx))
    }

    fn non_modal_terminal_focus_blocker(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<TerminalFocusBlocker> {
        self.workspace_picker
            .read(cx)
            .blocks_terminal_input()
            .then_some(TerminalFocusBlocker::Modal)
            .or(self
                .remote_workspace_flow
                .as_ref()
                .is_some_and(|flow| flow.read(cx).blocks_terminal_input())
                .then_some(TerminalFocusBlocker::CommandPalette))
            .or(self
                .new_workspace_panel
                .read(cx)
                .blocks_terminal_input()
                .then_some(TerminalFocusBlocker::CommandPalette))
            .or(self
                .workspace_search
                .read(cx)
                .blocks_terminal_input()
                .then_some(TerminalFocusBlocker::CommandPalette))
            .or(self
                .window_drag_status
                .is_active()
                .then_some(TerminalFocusBlocker::TopChrome))
            .or(self
                .sidebar_resize_interaction
                .then_some(TerminalFocusBlocker::SidebarResize))
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
                .then_some(TerminalFocusBlocker::Sidebar))
    }

    fn workspace_search_items(&self, cx: &App) -> Vec<WorkspaceSearchItem> {
        self.workspaces
            .iter()
            .map(|workspace| {
                let (window_count, pane_count) = workspace.payload().read(cx).aggregate_counts(cx);
                WorkspaceSearchItem::new(
                    workspace.id(),
                    workspace.name().to_owned(),
                    workspace_directory_labels(
                        workspace.working_directory(),
                        workspace.remote_workspace_directory(),
                        &self.default_workspace_root,
                    )
                    .0,
                    matches!(workspace.kind(), WorkspaceKind::LocalProject { .. }),
                    matches!(
                        workspace.availability(),
                        WorkspaceDirectoryAvailability::Available
                    ),
                    window_count,
                    pane_count,
                )
            })
            .collect()
    }

    fn refresh_workspace_search(&self, cx: &mut Context<Self>) {
        if !self.workspace_search.read(cx).blocks_terminal_input() {
            return;
        }
        let items = self.workspace_search_items(cx);
        self.workspace_search
            .update(cx, |search, cx| search.refresh_items(items, cx));
    }

    fn open_workspace_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename_is_focused(window) {
            self.sidebar_focus.focus(window);
        }
        self.rename = None;
        self.workspace_menu = None;
        self.picker_entered_from_panel = false;
        self.new_workspace_panel
            .update(cx, |panel, cx| panel.dismiss(window, cx));
        self.workspace_picker
            .update(cx, |picker, cx| picker.dismiss(window, cx));
        let items = self.workspace_search_items(cx);
        self.workspace_search
            .update(cx, |search, cx| search.open(items, window, cx));
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    fn handle_workspace_search_event(
        &mut self,
        event: &WorkspaceSearchEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WorkspaceSearchEvent::StateChanged => {
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            }
            WorkspaceSearchEvent::WorkspaceSelected(workspace_id) => {
                if !self.activate_workspace(*workspace_id, window, cx) {
                    self.sync_terminal_focus_blocker(window, cx);
                    cx.notify();
                }
            }
        }
    }

    fn rename_is_focused(&self, window: &Window) -> bool {
        self.rename
            .as_ref()
            .is_some_and(|rename| rename.focus_handle.is_focused(window))
    }

    fn sync_terminal_focus_blocker(&self, window: &Window, cx: &mut Context<Self>) {
        let blocker = self.non_modal_terminal_focus_blocker(window, cx);
        self.workspaces
            .active_workspace()
            .payload()
            .update(cx, |manager, cx| {
                manager.set_parent_focus_blocker(blocker, cx);
            });
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
                self.sync_terminal_focus_blocker(window, cx);
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
                self.sync_terminal_focus_blocker(window, cx);
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

    fn resize_sidebar(
        &mut self,
        requested_width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let minimum_width = px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH);
        if requested_width < minimum_width {
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
        self.set_sidebar_layout(
            true,
            requested_width.clamp(minimum_width, maximum_width),
            cx,
        );
    }

    fn handle_sidebar_resize_event(
        &mut self,
        event: ResizeHandleEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ResizeHandleEvent::InteractionStarted { source, .. } => {
                self.sidebar_resize_interaction = true;
                if source == ResizeInputSource::Pointer {
                    self.suppress_sidebar_pointer_until_release = false;
                }
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            }
            ResizeHandleEvent::ResizeRequested {
                requested_value, ..
            } => self.resize_sidebar(px(requested_value), window, cx),
            ResizeHandleEvent::ResetRequested { source } => {
                self.resize_sidebar(px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH), window, cx);
                if source == ResizeInputSource::Pointer {
                    self.focus(window, cx);
                }
            }
            ResizeHandleEvent::InteractionFinished { source, reason, .. } => {
                if source == ResizeInputSource::Pointer {
                    self.suppress_sidebar_pointer_until_release = !matches!(
                        reason,
                        ResizeFinishReason::Completed | ResizeFinishReason::PointerButtonLost
                    );
                }
                if !self.sidebar_resize_interaction {
                    return;
                }
                self.sidebar_resize_interaction = false;
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
                if source == ResizeInputSource::Pointer {
                    self.focus(window, cx);
                }
            }
        }
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

    fn create_scratch_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous_manager = self.workspaces.active_workspace().payload().clone();
        let session_factory = Rc::clone(&self.session_factory);
        let window_drag_platform = Rc::clone(&self.operating_system_window_drag_platform);
        let sidebar_visible = self.sidebar_visible;
        let sidebar_width = self.sidebar_width;
        let (directory, unavailable_reason) = self.default_workspace_directory();
        let directory_identity = directory.identity();
        let result = self.workspaces.create_scratch_workspace(
            directory,
            DirectoryAuthority::initial(),
            |workspace_id, workspace_root| {
                Self::create_local_window_manager(
                    workspace_id,
                    WorkspaceTerminalSessionFactory::new_local(
                        session_factory,
                        ValidatedWorkspaceDirectory::new(
                            workspace_root.to_path_buf(),
                            directory_identity,
                        ),
                    ),
                    sidebar_visible,
                    sidebar_width,
                    window_drag_platform,
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
        self.refresh_workspace_search(cx);
        cx.notify();
    }

    fn workspace_picker_starting_directory(configured_home: &std::path::Path) -> PathBuf {
        configured_home.to_path_buf()
    }

    fn open_local_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace_picker.read(cx).is_open() {
            self.workspace_picker
                .update(cx, |picker, cx| picker.refocus_path(window, cx));
            return;
        }
        if spaceterm_ui::window_menu_is_open(window, cx) {
            let manager = cx.entity();
            window.defer(cx, move |window, cx| {
                spaceterm_ui::dismiss_active_menu(window, cx);
                manager.update(cx, |manager, cx| {
                    manager.present_workspace_picker(window, cx)
                });
            });
            return;
        }
        self.present_workspace_picker(window, cx);
    }

    fn present_workspace_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace_picker.read(cx).is_open() {
            self.workspace_picker
                .update(cx, |picker, cx| picker.refocus_path(window, cx));
            return;
        }
        if self.rename_is_focused(window) {
            self.sidebar_focus.focus(window);
        }
        self.rename = None;
        self.workspace_menu = None;
        self.workspace_search
            .update(cx, |search, cx| search.dismiss(window, cx));
        self.new_workspace_panel
            .update(cx, |panel, cx| panel.dismiss(window, cx));
        self.workspace_picker
            .update(cx, |picker, cx| picker.open(window, cx));
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    fn show_new_workspace_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_workspace_panel.read(cx).is_open() {
            return;
        }
        if spaceterm_ui::window_menu_is_open(window, cx) {
            let manager = cx.entity();
            window.defer(cx, move |window, cx| {
                spaceterm_ui::dismiss_active_menu(window, cx);
                manager.update(cx, |manager, cx| {
                    manager.present_new_workspace_panel(window, cx)
                });
            });
            return;
        }
        self.present_new_workspace_panel(window, cx);
    }

    fn present_new_workspace_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename_is_focused(window) {
            self.sidebar_focus.focus(window);
        }
        self.rename = None;
        self.workspace_menu = None;
        self.picker_entered_from_panel = false;
        self.workspace_search
            .update(cx, |search, cx| search.dismiss(window, cx));
        self.workspace_picker
            .update(cx, |picker, cx| picker.dismiss(window, cx));
        self.new_workspace_panel
            .update(cx, |panel, cx| panel.open(window, cx));
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    fn handle_new_workspace_panel_event(
        &mut self,
        event: &NewWorkspacePanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            NewWorkspacePanelEvent::StateChanged => {
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            }
            NewWorkspacePanelEvent::SourceSelected(source) => match source {
                NewWorkspaceSource::LocalProject => {
                    self.picker_entered_from_panel = true;
                    self.present_workspace_picker(window, cx);
                }
                NewWorkspaceSource::Scratch => self.create_scratch_workspace(window, cx),
                NewWorkspaceSource::RemoteProject => self.present_remote_workspace_flow(window, cx),
            },
        }
    }

    fn present_remote_workspace_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(backend) = self.remote_workspace_backend.as_ref().map(Arc::clone) else {
            self.sync_terminal_focus_blocker(window, cx);
            return;
        };
        let flow = self.remote_workspace_flow.get_or_insert_with(|| {
            let flow = cx.new(|cx| RemoteWorkspaceFlow::new(backend, window, cx));
            cx.subscribe_in(
                &flow,
                window,
                |manager, flow, event: &RemoteWorkspaceFlowEvent, window, cx| {
                    manager.handle_remote_workspace_flow_event(flow, event, window, cx);
                },
            )
            .detach();
            flow
        });
        let Some(replacement) = self
            .new_workspace_panel
            .update(cx, |panel, cx| panel.dismiss_for_replacement(window, cx))
        else {
            self.sync_terminal_focus_blocker(window, cx);
            return;
        };
        if !flow.update(cx, |flow, cx| flow.open_replacing(replacement, window, cx)) {
            self.remote_workspace_flow = None;
        }
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    fn handle_remote_workspace_flow_event(
        &mut self,
        source: &Entity<RemoteWorkspaceFlow>,
        event: &RemoteWorkspaceFlowEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .remote_workspace_flow
            .as_ref()
            .is_none_or(|current| current != source)
        {
            return;
        }
        match event {
            RemoteWorkspaceFlowEvent::StateChanged => {
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            }
            RemoteWorkspaceFlowEvent::Cancelled => {
                self.remote_workspace_activation_task.take();
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            }
            RemoteWorkspaceFlowEvent::Completed(handle) => {
                self.activate_remote_project(source, handle, window, cx);
            }
        }
    }

    fn activate_remote_project(
        &mut self,
        flow: &Entity<RemoteWorkspaceFlow>,
        handle: &RemoteWorkspaceFlowCompletionHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(completion) = handle.take() else {
            return;
        };
        let key = RemoteWorkspaceKey::new(
            completion.destination().clone(),
            completion.physical_directory().clone(),
        );
        if self.workspaces.remote_project_workspace(&key).is_some() {
            match self.try_activate_existing_remote_project(key, completion, window, cx) {
                Ok(()) => self.acknowledge_remote_project_activation(flow, handle, window, cx),
                Err(completion) => {
                    self.return_remote_project_activation(flow, handle, *completion, window, cx)
                }
            }
            return;
        }

        let terminal_factory = WorkspaceTerminalSessionFactory::new_remote(
            Rc::clone(&self.session_factory),
            ValidatedWorkspaceDirectory::new(
                self.default_workspace_root.clone(),
                self.default_workspace_identity,
            ),
            RemoteTerminalMetadataContext::new(
                completion.destination().clone(),
                completion.directory().clone(),
            ),
            remote_workspace_fallback_title(&key, completion.remote_home_identity()),
            completion.terminal_channels(),
        );
        let Some(revalidation) = terminal_factory.revalidate_remote_child_launch() else {
            unreachable!("a Remote Workspace Terminal factory must require revalidation")
        };
        let activation = PendingRemoteProjectActivation {
            flow: flow.clone(),
            handle: handle.clone(),
            completion,
            terminal_factory,
        };
        self.remote_workspace_activation_task =
            Some(cx.spawn_in(window, async move |manager, cx| {
                let revalidation = revalidation.await;
                let _ = manager.update_in(cx, |manager, window, cx| {
                    manager.finish_remote_project_activation(activation, revalidation, window, cx);
                });
            }));
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    fn finish_remote_project_activation(
        &mut self,
        activation: PendingRemoteProjectActivation,
        revalidation: Result<(), crate::terminal::RemoteChannelRevalidationError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let PendingRemoteProjectActivation {
            flow,
            handle,
            completion,
            terminal_factory,
        } = activation;
        let is_current = self.remote_workspace_flow.as_ref() == Some(&flow)
            && flow.read(cx).owns_activation(&handle);
        if !is_current {
            drop(completion);
            return;
        }
        if let Err(error) = revalidation {
            eprintln!("cannot create Remote Workspace because {error}");
            self.return_remote_project_activation(&flow, &handle, completion, window, cx);
            return;
        }
        let key = RemoteWorkspaceKey::new(
            completion.destination().clone(),
            completion.physical_directory().clone(),
        );
        if self.workspaces.remote_project_workspace(&key).is_some() {
            match self.try_activate_existing_remote_project(key, completion, window, cx) {
                Ok(()) => self.acknowledge_remote_project_activation(&flow, &handle, window, cx),
                Err(completion) => {
                    self.return_remote_project_activation(&flow, &handle, *completion, window, cx)
                }
            }
            return;
        }
        let prepared_launch = match terminal_factory.prepare_child_launch() {
            Ok(prepared_launch) => prepared_launch,
            Err(error) => {
                eprintln!("cannot create Remote Workspace because {error}");
                self.return_remote_project_activation(&flow, &handle, completion, window, cx);
                return;
            }
        };
        match self.try_create_remote_project(
            completion,
            terminal_factory,
            prepared_launch,
            window,
            cx,
        ) {
            Ok(()) => self.acknowledge_remote_project_activation(&flow, &handle, window, cx),
            Err(completion) => {
                self.return_remote_project_activation(&flow, &handle, *completion, window, cx)
            }
        }
    }

    fn try_activate_existing_remote_project(
        &mut self,
        key: RemoteWorkspaceKey,
        completion: RemoteWorkspaceFlowCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), Box<RemoteWorkspaceFlowCompletion>> {
        let previous_workspace_id = self.workspaces.active_workspace_id();
        let previous_manager = self.workspaces.active_workspace().payload().clone();
        let outcome = self.workspaces.create_remote_project_workspace(
            key,
            completion.directory().clone(),
            completion.remote_home_identity().clone(),
            RemoteConnectionState::connected(1),
            |_| unreachable!("deduplication must not create a replacement payload"),
        );
        let Ok(CreateRemoteProjectOutcome::ActivatedExisting { workspace_id }) = outcome else {
            return Err(Box::new(completion));
        };
        self.activate_remote_window_manager(
            previous_workspace_id,
            previous_manager,
            workspace_id,
            window,
            cx,
        );
        drop(completion);
        self.debug_assert_remote_runtime_invariants();
        Ok(())
    }

    fn try_create_remote_project(
        &mut self,
        completion: RemoteWorkspaceFlowCompletion,
        terminal_factory: WorkspaceTerminalSessionFactory,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), Box<RemoteWorkspaceFlowCompletion>> {
        let alias_pin = match completion.acquire_workspace_alias_pin() {
            Ok(alias_pin) => alias_pin,
            Err(_) => return Err(Box::new(completion)),
        };
        let key = RemoteWorkspaceKey::new(
            completion.destination().clone(),
            completion.physical_directory().clone(),
        );
        let previous_workspace_id = self.workspaces.active_workspace_id();
        let previous_manager = self.workspaces.active_workspace().payload().clone();
        let sidebar_visible = self.sidebar_visible;
        let sidebar_width = self.sidebar_width;
        let window_drag_platform = Rc::clone(&self.operating_system_window_drag_platform);
        let result = self.workspaces.create_remote_project_workspace(
            key,
            completion.directory().clone(),
            completion.remote_home_identity().clone(),
            RemoteConnectionState::connected(1),
            |workspace_id| {
                Self::create_window_manager_with_prepared_launch(
                    terminal_factory,
                    prepared_launch,
                    WindowManagerCreation {
                        workspace_id,
                        sidebar_visible,
                        sidebar_width,
                        operating_system_window_drag_platform: window_drag_platform,
                    },
                    window,
                    cx,
                )
            },
        );
        let workspace_id = match result {
            Ok(CreateRemoteProjectOutcome::Created { workspace_id }) => workspace_id,
            Ok(CreateRemoteProjectOutcome::ActivatedExisting { workspace_id }) => {
                drop(completion);
                self.activate_remote_window_manager(
                    previous_workspace_id,
                    previous_manager,
                    workspace_id,
                    window,
                    cx,
                );
                self.debug_assert_remote_runtime_invariants();
                return Ok(());
            }
            Err(_) => return Err(Box::new(completion)),
        };
        let (session, _, _, _, _, _, lifecycle) = completion.into_parts();
        let replaced = self.remote_workspace_runtimes.insert(
            workspace_id,
            RemoteWorkspaceRuntime::new(1, session, lifecycle, alias_pin),
        );
        debug_assert!(
            replaced.is_none(),
            "a new Remote Workspace owns one runtime"
        );
        self.activate_remote_window_manager(
            previous_workspace_id,
            previous_manager,
            workspace_id,
            window,
            cx,
        );
        self.observe_remote_workspace_runtime(workspace_id, 1, cx);
        self.debug_assert_remote_runtime_invariants();
        Ok(())
    }

    fn return_remote_project_activation(
        &mut self,
        flow: &Entity<RemoteWorkspaceFlow>,
        handle: &RemoteWorkspaceFlowCompletionHandle,
        completion: RemoteWorkspaceFlowCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = flow.update(cx, |flow, cx| {
            flow.activation_failed(handle, completion, window, cx)
        });
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    fn acknowledge_remote_project_activation(
        &mut self,
        flow: &Entity<RemoteWorkspaceFlow>,
        handle: &RemoteWorkspaceFlowCompletionHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let acknowledged =
            flow.update(cx, |flow, cx| flow.activation_succeeded(handle, window, cx));
        debug_assert!(
            acknowledged,
            "the active Remote Workspace completion must acknowledge"
        );
        if !acknowledged {
            return;
        }
        self.remote_workspace_flow = None;
        self.sync_terminal_focus_blocker(window, cx);
        self.scroll_active_workspace_into_view();
        self.refresh_workspace_search(cx);
        cx.notify();
    }

    fn activate_remote_window_manager(
        &self,
        previous_workspace_id: WorkspaceId,
        previous_manager: Entity<WindowManager>,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(next_manager) = self
            .workspaces
            .workspace(workspace_id)
            .map(|workspace| workspace.payload().clone())
        else {
            unreachable!("an activated Remote Workspace must remain in its collection")
        };
        if previous_workspace_id != workspace_id {
            previous_manager.update(cx, |manager, cx| manager.deactivate(cx));
        }
        next_manager.update(cx, |manager, cx| {
            manager.set_parent_focus_blocker(Some(TerminalFocusBlocker::CommandPalette), cx);
            manager.activate(window, cx);
        });
    }

    fn debug_assert_remote_runtime_invariants(&self) {
        debug_assert!(
            self.remote_workspace_runtimes
                .iter()
                .all(|(workspace_id, _)| {
                    self.workspaces
                        .workspace(*workspace_id)
                        .is_some_and(|workspace| {
                            matches!(workspace.kind(), WorkspaceKind::RemoteProject { .. })
                        })
                })
        );
        debug_assert!(self.workspaces.iter().all(|workspace| {
            !matches!(workspace.kind(), WorkspaceKind::RemoteProject { .. })
                || self.remote_workspace_runtimes.contains_key(&workspace.id())
        }));
    }

    fn observe_remote_workspace_runtime(
        &mut self,
        workspace_id: WorkspaceId,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(observer) = self
            .remote_workspace_runtimes
            .get_mut(&workspace_id)
            .filter(|runtime| runtime.generation == generation)
            .and_then(|runtime| runtime.lifecycle.take())
        else {
            return;
        };
        cx.spawn(async move |manager, cx| {
            let terminal = observer.terminal().await;
            let _ = manager.update(cx, |manager, cx| {
                manager.handle_remote_workspace_terminal(workspace_id, generation, terminal, cx);
            });
        })
        .detach();
    }

    fn handle_remote_workspace_terminal(
        &mut self,
        workspace_id: WorkspaceId,
        generation: u64,
        _terminal: ControlConnectionTerminalState,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self
            .workspaces
            .workspace(workspace_id)
            .and_then(|workspace| workspace.remote_connection_state())
        else {
            return;
        };
        if state != RemoteConnectionState::connected(generation)
            || self
                .remote_workspace_runtimes
                .get(&workspace_id)
                .is_none_or(|runtime| runtime.generation != generation)
        {
            return;
        }
        let Some(window_manager) = self
            .workspaces
            .workspace(workspace_id)
            .map(|workspace| workspace.payload().clone())
        else {
            return;
        };
        if let Err(error) =
            window_manager.update(cx, |manager, cx| manager.disconnect_remote(generation, cx))
        {
            eprintln!("cannot disconnect Remote Workspace terminals: {error}");
            return;
        }
        let next = RemoteConnectionState::disconnected(generation);
        if self
            .workspaces
            .reduce_remote_connection_state(workspace_id, next)
            != Ok(RemoteConnectionReduction::Applied)
        {
            return;
        }
        if let Some(runtime) = self.remote_workspace_runtimes.get_mut(&workspace_id) {
            runtime.session.take();
        }
        self.refresh_workspace_search(cx);
        cx.notify();
    }

    fn start_remote_workspace_reconnect(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.remote_workspace_reconnect.is_some() {
            return;
        }
        let Some(backend) = self.remote_workspace_backend.clone() else {
            return;
        };
        if self.workspaces.begin_remote_reconnect(workspace_id)
            != Ok(RemoteConnectionReduction::Applied)
        {
            return;
        }
        let Some(workspace) = self.workspaces.workspace(workspace_id) else {
            return;
        };
        let Some(key) = workspace.remote_workspace_key().cloned() else {
            return;
        };
        let Some(directory) = workspace.remote_workspace_directory().cloned() else {
            return;
        };
        let Some(state) = workspace.remote_connection_state() else {
            return;
        };
        let generation = state.generation();
        let destination = key.destination().clone();
        let expected_identity = key.physical_directory().clone();
        let window_manager = workspace.payload().clone();
        let local_root = ValidatedWorkspaceDirectory::new(
            self.default_workspace_root.clone(),
            self.default_workspace_identity,
        );
        let session_factory = Rc::clone(&self.session_factory);
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_manager = cx.weak_entity();
        let result_manager = cancel_manager.clone();
        let result_window = window.window_handle();
        let cancel_flag = Arc::clone(&cancelled);
        let dialog = ProgressDialog::new(
            ModalId::new("remote-workspace-reconnect-progress"),
            "Remote reconnection progress",
            "Reconnect Remote Workspace",
            RemoteWorkspaceConnectionProgress::CheckingCompatibility.status(),
            ProgressState::Indeterminate,
            ProgressCancellation::Cancellable(ModalAction::new(
                (),
                "Cancel",
                ModalActionRole::Cancel,
                "remote-workspace-reconnect-cancel",
            )),
        )
        .detail("Authentication prompts may appear in a secure macOS sheet.")
        .present(
            window,
            cx,
            move |_, _, _| {
                cancel_flag.store(true, Ordering::Release);
                ProgressCancelDecision::Allow
            },
            move |outcome, cx| {
                let _ = result_window.update(cx, |_, window, cx| {
                    let _ = result_manager.update(cx, |manager, cx| {
                        manager.finish_remote_workspace_reconnect_progress(
                            workspace_id,
                            generation,
                            outcome,
                            window,
                            cx,
                        );
                    });
                });
            },
        );
        let Ok(progress) = dialog else {
            let _ = self.workspaces.reduce_remote_connection_state(
                workspace_id,
                RemoteConnectionState::failed(generation),
            );
            cx.notify();
            return;
        };
        let (progress_sender, progress_receiver) = async_channel::bounded(8);
        let context = RemoteWorkspaceConnectContext::new(progress_sender, Arc::clone(&cancelled));
        let connection = backend.connect(destination.clone(), context);
        let progress_updates = cx.spawn_in(window, async move |manager, cx| {
            while let Ok(update) = progress_receiver.recv().await {
                let Ok(current) = manager.update_in(cx, |manager, window, cx| {
                    manager.apply_remote_workspace_reconnect_progress(
                        workspace_id,
                        generation,
                        update,
                        window,
                        cx,
                    )
                }) else {
                    break;
                };
                if !current {
                    break;
                }
            }
        });

        let work_cancelled = Arc::clone(&cancelled);
        let work = cx.spawn_in(window, async move |manager, cx| {
            let result = async {
                let mut session = connection.await.map_err(|error| {
                    if work_cancelled.load(Ordering::Acquire)
                        || matches!(
                            error,
                            RemoteWorkspaceFlowBackendError::AuthenticationCancelled
                        )
                    {
                        RemoteWorkspaceReconnectFailure::Cancelled
                    } else {
                        RemoteWorkspaceReconnectFailure::ConnectionFailed {
                            detail: error.into_connection_detail(),
                        }
                    }
                })?;
                if work_cancelled.load(Ordering::Acquire) {
                    return Err(RemoteWorkspaceReconnectFailure::Cancelled);
                }
                let provider = session.provider();
                let account = provider.discover_account().await.map_err(|_| {
                    RemoteWorkspaceReconnectFailure::ConnectionFailed { detail: None }
                })?;
                if work_cancelled.load(Ordering::Acquire) {
                    return Err(RemoteWorkspaceReconnectFailure::Cancelled);
                }
                let actual_identity = provider
                    .validate_physical_identity(directory.clone())
                    .await
                    .map_err(|_| RemoteWorkspaceReconnectFailure::DirectoryUnavailable)?;
                if actual_identity != expected_identity {
                    return Err(RemoteWorkspaceReconnectFailure::IdentityChanged);
                }
                let channels = session
                    .bind_terminal_channels_for_identity(
                        &directory,
                        &expected_identity,
                        account.login_shell(),
                    )
                    .map_err(|_| RemoteWorkspaceReconnectFailure::ConnectionFailed {
                        detail: None,
                    })?;
                let lifecycle = session
                    .take_lifecycle_observer()
                    .ok_or(RemoteWorkspaceReconnectFailure::ConnectionFailed { detail: None })?;
                let factory = WorkspaceTerminalSessionFactory::new_remote(
                    session_factory,
                    local_root,
                    RemoteTerminalMetadataContext::new(destination, directory),
                    remote_workspace_fallback_title(&key, account.home_identity()),
                    channels,
                );
                let restart = window_manager
                    .update(cx, |manager, cx| {
                        manager.prepare_remote_restart(factory, generation, cx)
                    })
                    .map_err(|_| RemoteWorkspaceReconnectFailure::Cancelled)?
                    .await
                    .map_err(classify_remote_workspace_restart_failure)?;
                if work_cancelled.load(Ordering::Acquire) {
                    return Err(RemoteWorkspaceReconnectFailure::Cancelled);
                }
                Ok(PreparedRemoteWorkspaceReconnect {
                    session,
                    lifecycle,
                    restart,
                })
            }
            .await;
            let _ = manager.update_in(cx, |manager, window, cx| {
                manager.finish_remote_workspace_reconnect(
                    workspace_id,
                    generation,
                    result,
                    window,
                    cx,
                );
            });
        });
        self.remote_workspace_reconnect = Some(RemoteWorkspaceReconnectAttempt {
            workspace_id,
            generation,
            cancelled,
            progress,
            _work: work,
            _progress_updates: progress_updates,
        });
        self.refresh_workspace_search(cx);
        cx.notify();
    }

    fn apply_remote_workspace_reconnect_progress(
        &mut self,
        workspace_id: WorkspaceId,
        generation: u64,
        update: RemoteWorkspaceConnectionProgress,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(attempt) = self.remote_workspace_reconnect.as_ref().filter(|attempt| {
            attempt.workspace_id == workspace_id && attempt.generation == generation
        }) else {
            return false;
        };
        let _ = attempt.progress.update(
            ProgressDialogUpdate::new().status(update.status()),
            window,
            cx,
        );
        true
    }

    fn finish_remote_workspace_reconnect(
        &mut self,
        workspace_id: WorkspaceId,
        generation: u64,
        result: Result<PreparedRemoteWorkspaceReconnect, RemoteWorkspaceReconnectFailure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_current = self
            .remote_workspace_reconnect
            .as_ref()
            .is_some_and(|attempt| {
                attempt.workspace_id == workspace_id && attempt.generation == generation
            })
            && self
                .workspaces
                .workspace(workspace_id)
                .and_then(|workspace| workspace.remote_connection_state())
                == Some(RemoteConnectionState::reconnecting(generation));
        if !is_current {
            return;
        }
        let attempt = self
            .remote_workspace_reconnect
            .take()
            .expect("a current reconnect attempt must remain owned");
        match result {
            Ok(prepared) => {
                let Some(window_manager) = self
                    .workspaces
                    .workspace(workspace_id)
                    .map(|workspace| workspace.payload().clone())
                else {
                    return;
                };
                #[cfg(test)]
                if self.close_focused_pane_before_reconnect_commit {
                    self.close_focused_pane_before_reconnect_commit = false;
                    window_manager
                        .read(cx)
                        .active_pane_host()
                        .update(cx, |host, cx| host.close_focused_for_test(window, cx));
                }
                if let Err(error) = window_manager.update(cx, |manager, cx| {
                    manager.commit_remote_restart(prepared.restart, window, cx)
                }) {
                    let failure = classify_remote_workspace_restart_failure(error);
                    let next = remote_workspace_reconnect_failure_state(&failure, generation);
                    let _ = self
                        .workspaces
                        .reduce_remote_connection_state(workspace_id, next);
                    let _ = attempt.progress.fail(window, cx);
                    self.present_remote_workspace_reconnect_error(failure, window, cx);
                    self.refresh_workspace_search(cx);
                    cx.notify();
                    return;
                }
                let alias_pin = self
                    .remote_workspace_runtimes
                    .get_mut(&workspace_id)
                    .and_then(|runtime| runtime.alias_pin.take());
                self.remote_workspace_runtimes.insert(
                    workspace_id,
                    RemoteWorkspaceRuntime::new(
                        generation,
                        prepared.session,
                        prepared.lifecycle,
                        alias_pin,
                    ),
                );
                let reduction = self.workspaces.reduce_remote_connection_state(
                    workspace_id,
                    RemoteConnectionState::connected(generation),
                );
                debug_assert_eq!(reduction, Ok(RemoteConnectionReduction::Applied));
                self.observe_remote_workspace_runtime(workspace_id, generation, cx);
                let _ = attempt.progress.complete(window, cx);
            }
            Err(failure) => {
                let next = remote_workspace_reconnect_failure_state(&failure, generation);
                let _ = self
                    .workspaces
                    .reduce_remote_connection_state(workspace_id, next);
                if matches!(failure, RemoteWorkspaceReconnectFailure::Cancelled) {
                    let _ = attempt.progress.dismiss(window, cx);
                } else {
                    let _ = attempt.progress.fail(window, cx);
                    self.present_remote_workspace_reconnect_error(failure, window, cx);
                }
            }
        }
        self.refresh_workspace_search(cx);
        cx.notify();
    }

    fn present_remote_workspace_reconnect_error(
        &mut self,
        failure: RemoteWorkspaceReconnectFailure,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((title, message)) = remote_workspace_reconnect_error_content(&failure) else {
            return;
        };
        let manager = cx.weak_entity();
        let window_handle = window.window_handle();
        let result = Alert::new(
            ModalId::new("remote-workspace-reconnect-error"),
            "Remote Workspace reconnection failed",
            title,
            message,
            vec![ModalAction::new(
                (),
                "OK",
                ModalActionRole::Cancel,
                "remote-workspace-reconnect-error-ok",
            )],
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, move |_, cx| {
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = manager.update(cx, |manager, cx| {
                    manager.sync_terminal_focus_blocker(window, cx);
                    manager.focus(window, cx);
                    cx.notify();
                });
            });
        });
        if result.is_ok() {
            self.sync_terminal_focus_blocker(window, cx);
            cx.notify();
        }
    }

    fn finish_remote_workspace_reconnect_progress(
        &mut self,
        workspace_id: WorkspaceId,
        generation: u64,
        outcome: ProgressDialogOutcome,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(
            outcome,
            ProgressDialogOutcome::Cancelled { .. }
                | ProgressDialogOutcome::DeadlineExpired
                | ProgressDialogOutcome::OwnerRemoved
                | ProgressDialogOutcome::ProgrammaticDismissal
                | ProgressDialogOutcome::Replaced
        ) {
            return;
        }
        let Some(attempt) = self.remote_workspace_reconnect.as_ref() else {
            return;
        };
        if attempt.workspace_id != workspace_id || attempt.generation != generation {
            return;
        }
        attempt.cancelled.store(true, Ordering::Release);
        self.remote_workspace_reconnect = None;
        let _ = self.workspaces.reduce_remote_connection_state(
            workspace_id,
            RemoteConnectionState::disconnected(generation),
        );
        self.refresh_workspace_search(cx);
        cx.notify();
    }

    fn close_remote_runtimes(&mut self) {
        if let Some(attempt) = self.remote_workspace_reconnect.take() {
            attempt.cancelled.store(true, Ordering::Release);
        }
        self.remote_workspace_activation_task.take();
        for runtime in self.remote_workspace_runtimes.values_mut() {
            runtime.close();
        }
    }

    fn handle_workspace_picker_event(
        &mut self,
        event: &WorkspacePickerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WorkspacePickerEvent::StateChanged => {
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            }
            WorkspacePickerEvent::Escaped => {
                if !self.picker_entered_from_panel {
                    return;
                }
                // Reopening is deferred so the picker finishes closing first; otherwise the two
                // palettes would contend for the responder and the panel would open unfocused.
                let manager = cx.entity();
                window.defer(cx, move |window, cx| {
                    manager.update(cx, |manager, cx| {
                        manager.present_new_workspace_panel(window, cx)
                    });
                });
            }
            WorkspacePickerEvent::FinderRequested => {
                let selection = self.finder_fallback.choose(cx);
                cx.spawn_in(window, async move |manager, cx| {
                    let result = selection.await;
                    let _ = manager.update_in(cx, |manager, window, cx| {
                        match result {
                            Ok(Some(path)) => manager.workspace_picker.update(cx, |picker, cx| {
                                picker.validate_finder_selection(path, window, cx)
                            }),
                            Ok(None) => manager
                                .workspace_picker
                                .update(cx, |picker, cx| picker.finder_cancelled(window, cx)),
                            Err(_) => manager
                                .workspace_picker
                                .update(cx, |picker, cx| picker.finder_failed(window, cx)),
                        };
                        manager.sync_terminal_focus_blocker(window, cx);
                        cx.notify();
                    });
                })
                .detach();
            }
            WorkspacePickerEvent::Confirmed(directory) => {
                self.picker_entered_from_panel = false;
                let activated =
                    self.activate_validated_local_project(directory.clone(), window, cx);
                let picker = self.workspace_picker.clone();
                window.defer(cx, move |window, cx| {
                    picker.update(cx, |picker, cx| {
                        if activated {
                            picker.complete_activation(window, cx);
                        } else {
                            picker.activation_failed(window, cx);
                        }
                    });
                });
            }
        }
    }

    fn activate_validated_local_project(
        &mut self,
        directory: ValidatedWorkspaceDirectory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(workspace_id) = self
            .workspaces
            .local_project_workspace(directory.identity())
        {
            return self.activate_workspace(workspace_id, window, cx);
        }

        let previous_manager = self.workspaces.active_workspace().payload().clone();
        let session_factory = Rc::clone(&self.session_factory);
        let window_drag_platform = Rc::clone(&self.operating_system_window_drag_platform);
        let sidebar_visible = self.sidebar_visible;
        let sidebar_width = self.sidebar_width;
        let project_root_identity = directory.identity();
        let result = self.workspaces.create_local_project_workspace(
            directory,
            |workspace_id, project_root| {
                Self::create_local_window_manager(
                    workspace_id,
                    WorkspaceTerminalSessionFactory::new_local(
                        session_factory,
                        ValidatedWorkspaceDirectory::new(
                            project_root.to_path_buf(),
                            project_root_identity,
                        ),
                    ),
                    sidebar_visible,
                    sidebar_width,
                    window_drag_platform,
                    window,
                    cx,
                )
            },
        );
        let workspace_id = match result {
            Ok(workspace_id) => workspace_id,
            Err(error) => {
                Self::report_workspace_error("open Local Project", error);
                return false;
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
        self.refresh_workspace_search(cx);
        cx.notify();
        true
    }

    fn revalidate_local_project(&mut self, workspace_id: WorkspaceId) {
        let Some(workspace) = self.workspaces.workspace(workspace_id) else {
            return;
        };
        let (Some(path), Some(expected_identity)) = (
            workspace.working_directory(),
            workspace.directory_identity(),
        ) else {
            unreachable!("a Local Project Workspace must own a local Workspace Directory")
        };
        let path = path.to_path_buf();
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
        self.refresh_workspace_search(cx);
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
        self.begin_remote_workspace_close(workspace_id, window, cx);
        let was_active = self.workspaces.active_workspace_id() == workspace_id;
        let session_factory = Rc::clone(&self.session_factory);
        let window_drag_platform = Rc::clone(&self.operating_system_window_drag_platform);
        let sidebar_visible = self.sidebar_visible;
        let sidebar_width = self.sidebar_width;
        let (replacement, unavailable_reason) = self.default_workspace_directory();
        let replacement_identity = replacement.identity();
        let outcome = self.workspaces.close_workspace_with_scratch_replacement(
            workspace_id,
            replacement,
            DirectoryAuthority::initial(),
            |replacement_workspace_id, workspace_root| {
                Self::create_local_window_manager(
                    replacement_workspace_id,
                    WorkspaceTerminalSessionFactory::new_local(
                        session_factory,
                        ValidatedWorkspaceDirectory::new(
                            workspace_root.to_path_buf(),
                            replacement_identity,
                        ),
                    ),
                    sidebar_visible,
                    sidebar_width,
                    window_drag_platform,
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
        self.remote_workspace_runtimes.remove(&workspace_id);
        self.debug_assert_remote_runtime_invariants();

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
        self.refresh_workspace_search(cx);
        cx.notify();
    }

    fn close_workspace_for_final_window(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_remote_workspace_close(workspace_id, window, cx);
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
                self.remote_workspace_runtimes.remove(&workspace_id);
                self.debug_assert_remote_runtime_invariants();

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
                self.refresh_workspace_search(cx);
                cx.notify();
            }
            FinalWindowCloseOutcome::CloseOperatingSystemWindow {
                workspace_id: final_workspace_id,
            } => {
                debug_assert_eq!(final_workspace_id, workspace_id);
                let manager = self.workspaces.active_workspace().payload().clone();
                manager.update(cx, |manager, cx| manager.close_all(cx));
                self.remote_workspace_runtimes.remove(&workspace_id);
                self.pending_final_window_closes.remove(&workspace_id);
                window.remove_window();
            }
        }
    }

    fn begin_remote_workspace_close(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .workspaces
            .workspace(workspace_id)
            .and_then(|workspace| workspace.remote_connection_state())
            .is_none()
        {
            return;
        }
        let _ = self.workspaces.begin_remote_close(workspace_id);
        let reconnect_matches = self
            .remote_workspace_reconnect
            .as_ref()
            .is_some_and(|attempt| attempt.workspace_id == workspace_id);
        if reconnect_matches {
            let attempt = self
                .remote_workspace_reconnect
                .take()
                .expect("matching reconnect attempt must remain owned");
            attempt.cancelled.store(true, Ordering::Release);
            let _ = attempt.progress.dismiss(window, cx);
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
            MenuLifecycleEvent::Closed(_)
                if self
                    .workspace_menu
                    .is_some_and(|menu| menu.workspace_id == workspace_id) =>
            {
                self.workspace_menu = None;
                Some(if self.rename.is_some() {
                    TerminalFocusBlocker::RenameField
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
                        "workspace-rename-input",
                        "Workspace name",
                        workspace.name(),
                        window,
                        cx,
                    )
                    .variant(TextInputVariant::Bare)
                    .debug_selector("workspace-rename-input")
                });
                let input_id = input.entity_id();
                cx.subscribe_in(
                    &input,
                    window,
                    move |manager, input, event: &TextInputEvent, window, cx| match event {
                        TextInputEvent::Submitted => {
                            let value = input.read(cx).value().to_owned();
                            cx.defer_in(window, move |manager, window, cx| {
                                manager.finish_rename(input_id, Some(value), true, window, cx);
                            });
                        }
                        TextInputEvent::Cancelled => {
                            cx.defer_in(window, move |manager, window, cx| {
                                manager.finish_rename(input_id, None, true, window, cx);
                            });
                        }
                        TextInputEvent::FocusLost => {
                            let menu_open = manager
                                .rename
                                .as_ref()
                                .filter(|rename| rename.input.entity_id() == input_id)
                                .is_some_and(|rename| rename.context_menu_open);
                            if !menu_open {
                                let value = input.read(cx).value().to_owned();
                                cx.defer_in(window, move |manager, window, cx| {
                                    manager.finish_rename(input_id, Some(value), false, window, cx);
                                });
                            }
                        }
                        TextInputEvent::ContextMenuOpened => {
                            if let Some(rename) = &mut manager.rename
                                && rename.input.entity_id() == input_id
                            {
                                rename.context_menu_open = true;
                            }
                        }
                        TextInputEvent::ContextMenuClosed => {
                            let should_finish = manager
                                .rename
                                .as_mut()
                                .filter(|rename| rename.input.entity_id() == input_id)
                                .is_some_and(|rename| {
                                    rename.context_menu_open = false;
                                    !rename.focus_handle.is_focused(window)
                                });
                            if should_finish {
                                let value = input.read(cx).value().to_owned();
                                cx.defer_in(window, move |manager, window, cx| {
                                    manager.finish_rename(input_id, Some(value), false, window, cx);
                                });
                            }
                        }
                        _ => {}
                    },
                )
                .detach();
                self.rename = Some(WorkspaceRenameState {
                    workspace_id,
                    focus_handle: input.read(cx).focus_handle(),
                    input,
                    context_menu_open: false,
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
            WorkspaceMenuCommand::Reconnect => {
                self.start_remote_workspace_reconnect(workspace_id, window, cx)
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
        self.refresh_workspace_search(cx);
        cx.notify();
    }

    fn on_create_scratch_workspace(
        &mut self,
        _: &CreateScratchWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.create_scratch_workspace(window, cx);
    }

    fn on_search_workspaces(
        &mut self,
        _: &SearchWorkspaces,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_workspace_search(window, cx);
    }

    fn on_open_local_project(
        &mut self,
        _: &OpenLocalProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_local_project(window, cx);
    }

    fn on_show_new_workspace_panel(
        &mut self,
        _: &ShowNewWorkspacePanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_new_workspace_panel(window, cx);
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

    /// The Active Workspace's identity, shown in the top-left chrome only while the sidebar is
    /// hidden.
    ///
    /// With the sidebar open its highlighted row already answers "which Workspace is this", so the
    /// chip would be duplicate chrome; with it closed nothing on screen does.
    fn render_workspace_chip(&self) -> AnyElement {
        let workspace = self.workspaces.active_workspace();
        let local_project = matches!(workspace.kind(), WorkspaceKind::LocalProject { .. });
        let workspace_icon = match workspace.kind() {
            WorkspaceKind::LocalProject { .. } => "folder",
            WorkspaceKind::RemoteProject { .. } => "globe",
            WorkspaceKind::Scratch { .. } => "terminal",
        };
        let available = workspace.availability().is_available();
        let remote_connection_phase = workspace
            .remote_connection_state()
            .map(RemoteConnectionState::phase);
        let remote_status = remote_connection_phase.and_then(remote_connection_status);
        let remote_color = remote_connection_phase.map(remote_connection_color);
        let name = workspace.name().to_owned();
        let (path, _) = workspace_directory_labels(
            workspace.working_directory(),
            workspace.remote_workspace_directory(),
            &self.default_workspace_root,
        );
        let foreground = gpui_color(if available {
            ACTIVE_THEME.text
        } else {
            ACTIVE_THEME.warning
        });
        let icon_color = gpui_color(if !available {
            ACTIVE_THEME.warning
        } else if local_project {
            ACTIVE_THEME.icon
        } else {
            ACTIVE_THEME.icon_muted
        });
        let tooltip_detail = remote_status
            .map(|status| format!("{path}: {status}"))
            .unwrap_or_else(|| path.clone());

        let chip = div()
            .id("workspace-chip")
            .debug_selector(|| "workspace-chip".to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(5.0))
            .min_w_0()
            .child(
                Icon::new(workspace_icon)
                    .size(px(WORKSPACE_CHIP_ICON_SIZE))
                    .color(remote_color.map(gpui_color).unwrap_or(icon_color)),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(WORKSPACE_CHIP_TEXT_SIZE))
                    .text_color(foreground)
                    .child(name.clone()),
            )
            .when(local_project, |chip| {
                chip.child(
                    Icon::new("pin.fill")
                        .size(px(SIDEBAR_ROW_PIN_ICON_SIZE))
                        .color(gpui_color(ACTIVE_THEME.icon_muted)),
                )
            })
            .when_some(remote_status, |chip, status| {
                chip.child(
                    div()
                        .id("workspace-chip-remote-status")
                        .debug_selector(|| "workspace-chip-remote-status".to_owned())
                        .flex_shrink_0()
                        .text_size(px(SIDEBAR_DETAIL_TEXT_SIZE))
                        .text_color(gpui_color(remote_color.unwrap_or(ACTIVE_THEME.text_muted)))
                        .child(status),
                )
            });

        Tooltip::new("workspace-chip-tooltip", name)
            .detail(tooltip_detail)
            .debug_selector("workspace-chip-tooltip")
            .attach(chip, TooltipTargetVisibility::Visible)
            .into_any_element()
    }

    fn render_top_left_chrome(&self, manager: WeakEntity<Self>) -> AnyElement {
        let drag_manager = manager.clone();
        let toggle_manager = manager;
        let content = div()
            .relative()
            .size_full()
            .when(!self.sidebar_visible, |chrome| {
                chrome.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(TRAFFIC_LIGHT_CLEARANCE))
                        .right(px(TRAFFIC_LIGHT_CLEARANCE / 2.0))
                        .flex()
                        .items_center()
                        .min_w_0()
                        .child(self.render_workspace_chip()),
                )
            })
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
                        .tooltip(
                            Tooltip::new("toggle-sidebar-tooltip", "Toggle Sidebar")
                                .debug_selector("toggle-sidebar-tooltip"),
                        )
                        .on_activate(move |_, window, cx| {
                            let _ = toggle_manager.update(cx, |manager, cx| {
                                manager.toggle_sidebar(window, cx);
                            });
                        }),
                    ),
            );
        let drag_region = WindowDragRegion::new(
            "workspace-top-chrome-drag-region",
            "Move Operating-System Window from Workspace chrome",
            content,
        )
        .status(self.window_drag_status.clone())
        .debug_selector("workspace-top-chrome-drag-region")
        .on_event(move |event, window, cx| {
            let event = *event;
            drag_manager
                .update(cx, |manager, cx| {
                    manager.handle_operating_system_window_drag_event(event, window, cx)
                })
                .unwrap_or_default()
        });

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
            .child(drag_region)
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
            remote_connection_phase,
            available,
            window_count,
            pane_count,
            active,
        } = row;
        let click_manager = manager.clone();
        let remote_status = remote_connection_phase.and_then(remote_connection_status);
        let remote_color = remote_connection_phase.map(remote_connection_color);
        let accessibility_name = remote_status.map_or_else(
            || format!("Workspace actions for {name}"),
            |status| format!("Workspace actions for {name}, connection {status}"),
        );
        let rename = self
            .rename
            .as_ref()
            .filter(|rename| rename.workspace_id == workspace_id);
        let renaming = rename.is_some();
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
        let tooltip_text = remote_status
            .map(|status| format!("{tooltip}: {status}"))
            .unwrap_or_else(|| tooltip.to_string());
        let tooltip_label = if remote_status.is_some() {
            "Remote Workspace connection"
        } else if available {
            "Workspace Directory"
        } else {
            "Workspace unavailable"
        };

        let row_content = div()
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
            .on_click(move |_, window, cx| {
                let _ = click_manager.update(cx, |manager, cx| {
                    if manager.activate_workspace(workspace_id, window, cx) {
                        manager.focus(window, cx);
                    }
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
                                Icon::new(if local_project {
                                    "folder"
                                } else if remote_connection_phase.is_some() {
                                    "globe"
                                } else {
                                    "terminal"
                                })
                                .size(px(SIDEBAR_ROW_ICON_SIZE))
                                .color(gpui_color(
                                    if let Some(color) = remote_color {
                                        color
                                    } else if available {
                                        if active {
                                            ACTIVE_THEME.icon_accent
                                        } else {
                                            ACTIVE_THEME.icon
                                        }
                                    } else {
                                        ACTIVE_THEME.warning
                                    },
                                )),
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
                            )
                            .when_some(remote_status, |line, status| {
                                line.child(
                                    div()
                                        .id(("workspace-row-remote-status", workspace_id.get()))
                                        .debug_selector(move || {
                                            format!(
                                                "workspace-row-remote-status-{}",
                                                workspace_id.get()
                                            )
                                        })
                                        .flex_shrink_0()
                                        .text_size(px(SIDEBAR_DETAIL_TEXT_SIZE))
                                        .text_color(gpui_color(
                                            remote_color.unwrap_or(ACTIVE_THEME.text_muted),
                                        ))
                                        .child(status),
                                )
                            }),
                    )
                    .child(
                        // The path carries the Workspace Kind: a Local Project is pinned to it,
                        // so it reads as settled text with a pin, while a Scratch Workspace's
                        // path follows its Directory Authority and stays muted. Watching one move
                        // once teaches the difference that no label explains as well.
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_size(px(SIDEBAR_DETAIL_TEXT_SIZE))
                                    .text_color(gpui_color(if !available {
                                        ACTIVE_THEME.warning
                                    } else if local_project {
                                        ACTIVE_THEME.text
                                    } else {
                                        ACTIVE_THEME.text_muted
                                    }))
                                    .child(MiddleTruncatedText::new(path, maximum_path_characters)),
                            )
                            .when(local_project, |line| {
                                line.child(
                                    div()
                                        .id(("workspace-row-pin", workspace_id.get()))
                                        .debug_selector(move || {
                                            format!("workspace-row-pin-{}", workspace_id.get())
                                        })
                                        .flex_shrink_0()
                                        .child(
                                            Icon::new("pin.fill")
                                                .size(px(SIDEBAR_ROW_PIN_ICON_SIZE))
                                                .color(gpui_color(if available {
                                                    ACTIVE_THEME.icon_muted
                                                } else {
                                                    ACTIVE_THEME.warning
                                                })),
                                        ),
                                )
                            }),
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
            );
        let row = Tooltip::new(("workspace-row-tooltip", workspace_id.get()), tooltip_label)
            .detail(tooltip_text)
            .debug_selector(format!("workspace-row-tooltip-{}", workspace_id.get()))
            .attach(row_content, TooltipTargetVisibility::Visible)
            .into_any_element();

        if renaming {
            return div()
                .id(("workspace-menu", workspace_id.get()))
                .debug_selector(move || format!("workspace-menu-{}", workspace_id.get()))
                .w_full()
                .flex_shrink_0()
                .child(row)
                .into_any_element();
        }

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
                    workspace_menu_entries(remote_connection_phase),
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
            let (path, directory_tooltip) = workspace_directory_labels(
                workspace.working_directory(),
                workspace.remote_workspace_directory(),
                &self.default_workspace_root,
            );
            let (available, tooltip) = match workspace.availability() {
                WorkspaceDirectoryAvailability::Available => (true, directory_tooltip),
                WorkspaceDirectoryAvailability::Unavailable { reason } => {
                    (false, format!("{directory_tooltip}: {reason}"))
                }
            };
            rows = rows.child(
                self.render_workspace_row(
                    WorkspaceRowViewModel {
                        workspace_id: workspace.id(),
                        name: workspace.name().to_owned().into(),
                        path: path.into(),
                        tooltip: tooltip.into(),
                        local_project: matches!(
                            workspace.kind(),
                            WorkspaceKind::LocalProject { .. }
                        ),
                        remote_connection_phase: workspace
                            .remote_connection_state()
                            .map(RemoteConnectionState::phase),
                        available,
                        window_count,
                        pane_count,
                        active: workspace.id() == active_workspace_id,
                    },
                    manager.clone(),
                ),
            );
        }

        let scrollbar = self.scrollbar.clone();
        let panel_manager = manager.clone();
        let search_manager = manager.clone();
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
                .tooltip(
                    Tooltip::new("search-workspaces-tooltip", "Search Workspaces")
                        .keyboard_equivalent("⌘P")
                        .debug_selector("search-workspaces-tooltip"),
                )
                .on_activate(move |_, window, cx| {
                    let _ = search_manager.update(cx, |manager, cx| {
                        manager.open_workspace_search(window, cx);
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
                        Button::new("new-workspace-button", "New Workspace")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Large)
                            .shape(ButtonShape::Square)
                            .full_width(true)
                            .debug_selector("new-workspace-button")
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
                                    .child("⌘O")
                                    .into_any_element()
                            })
                            .on_activate(move |_, window, cx| {
                                let _ = panel_manager.update(cx, |manager, cx| {
                                    manager.show_new_workspace_panel(window, cx);
                                });
                            }),
                    )
                    .child(
                        div()
                            .id("new-workspace-button-top-divider")
                            .debug_selector(|| "new-workspace-button-top-divider".to_owned())
                            .absolute()
                            .top_0()
                            .left_0()
                            .w_full()
                            .h(px(CHROME_DIVIDER_SIZE))
                            .bg(gpui_color(ACTIVE_THEME.border)),
                    ),
            )
            .child(scrollbar)
            .into_any_element()
    }

    fn render_sidebar_resize_handle(
        &self,
        selector: &'static str,
        top_chrome: bool,
        manager: WeakEntity<Self>,
    ) -> AnyElement {
        let current_width = f32::from(self.sidebar_width);
        let handle = ResizeHandle::new(
            selector,
            "Resize Workspace sidebar",
            ResizeAxis::Horizontal,
            current_width,
        )
        .tab_stop(true)
        .reset_on_double_click(true)
        .debug_selector(selector)
        .on_event(move |event, window, cx| {
            let event = *event;
            let _ = manager.update(cx, |manager, cx| {
                manager.handle_sidebar_resize_event(event, window, cx);
            });
        });
        let wrapper = div()
            .absolute()
            .left(self.sidebar_width - px(CHROME_DIVIDER_SIZE / 2.0))
            .w(px(CHROME_DIVIDER_SIZE));
        if top_chrome {
            wrapper
                .top_0()
                .h(px(TOP_CHROME_HEIGHT))
                .child(handle)
                .into_any_element()
        } else {
            wrapper
                .top(px(TOP_CHROME_HEIGHT))
                .bottom_0()
                .child(handle)
                .into_any_element()
        }
    }
}

impl Drop for WorkspaceManager {
    fn drop(&mut self) {
        self.close_remote_runtimes();
    }
}

impl Render for WorkspaceManager {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        debug_assert!(self.workspaces.len() > 0);
        self.sync_terminal_focus_blocker(window, cx);
        let manager = cx.entity().downgrade();
        let suppressed_move_manager = manager.clone();
        let suppressed_up_manager = manager.clone();
        let active_window_manager = self.workspaces.active_workspace().payload().clone();
        if self.sidebar_visible {
            self.sync_scrollbar(cx);
        }
        let content = div()
            .id("workspace-manager")
            .debug_selector(|| "workspace-manager".to_owned())
            .key_context(TERMINAL_KEY_CONTEXT)
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(gpui_color(ACTIVE_THEME.terminal_background))
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                            if phase != DispatchPhase::Capture {
                                return;
                            }
                            let suppressed = suppressed_move_manager
                                .update(cx, |manager, cx| {
                                    if !manager.suppress_sidebar_pointer_until_release {
                                        return false;
                                    }
                                    if event.pressed_button != Some(MouseButton::Left) {
                                        manager.suppress_sidebar_pointer_until_release = false;
                                        cx.notify();
                                    }
                                    true
                                })
                                .unwrap_or(false);
                            if suppressed {
                                window.prevent_default();
                                cx.stop_propagation();
                            }
                        });
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                            if phase != DispatchPhase::Capture || event.button != MouseButton::Left
                            {
                                return;
                            }
                            let suppressed = suppressed_up_manager
                                .update(cx, |manager, cx| {
                                    if !manager.suppress_sidebar_pointer_until_release {
                                        return false;
                                    }
                                    manager.suppress_sidebar_pointer_until_release = false;
                                    cx.notify();
                                    true
                                })
                                .unwrap_or(false);
                            if suppressed {
                                window.prevent_default();
                                cx.stop_propagation();
                            }
                        });
                    },
                )
                .absolute()
                .inset_0(),
            )
            .on_action(cx.listener(Self::on_create_scratch_workspace))
            .on_action(cx.listener(Self::on_search_workspaces))
            .on_action(cx.listener(Self::on_open_local_project))
            .on_action(cx.listener(Self::on_show_new_workspace_panel))
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
            .children(self.remote_workspace_flow.iter().cloned())
            .child(self.render_top_left_chrome(manager.clone()))
            .child(self.render_sidebar_resize_handle(
                "workspace-top-chrome-resize-handle",
                true,
                manager.clone(),
            ))
            .when(self.sidebar_visible, |root| {
                root.child(self.render_sidebar(manager.clone(), cx)).child(
                    self.render_sidebar_resize_handle(
                        "workspace-sidebar-resize-handle",
                        false,
                        manager,
                    ),
                )
            });
        let content = content
            .child(self.workspace_search.clone())
            .child(self.new_workspace_panel.clone())
            .child(self.workspace_picker.clone());
        ModalLayer::new(TooltipLayer::new(content))
    }
}

fn workspace_menu_entries(
    remote_connection_phase: Option<RemoteConnectionPhase>,
) -> Vec<MenuEntry<WorkspaceMenuCommand>> {
    let mut entries = vec![
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
    ];
    if let Some(phase) = remote_connection_phase {
        entries.push(
            MenuEntry::action("Reconnect", WorkspaceMenuCommand::Reconnect)
                .disabled(!matches!(
                    phase,
                    RemoteConnectionPhase::Disconnected | RemoteConnectionPhase::Failed
                ))
                .icon(|foreground| {
                    Icon::new("arrow.clockwise")
                        .weight(SymbolWeight::Regular)
                        .size(px(14.0))
                        .color(foreground)
                        .into_any_element()
                })
                .debug_selector("workspace-menu-row-reconnect"),
        );
    }
    entries.extend([
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
    ]);
    entries
}

fn remote_connection_status(phase: RemoteConnectionPhase) -> Option<&'static str> {
    match phase {
        RemoteConnectionPhase::Connected => Some("Connected"),
        RemoteConnectionPhase::Reconnecting => Some("Reconnecting"),
        RemoteConnectionPhase::Disconnected => Some("Disconnected"),
        RemoteConnectionPhase::Failed => Some("Connection failed"),
        RemoteConnectionPhase::Closing => Some("Closing"),
    }
}

fn classify_remote_workspace_restart_failure(
    error: RemoteWindowManagerLifecycleError,
) -> RemoteWorkspaceReconnectFailure {
    match error {
        RemoteWindowManagerLifecycleError::Revalidation(
            crate::terminal::RemoteChannelRevalidationError::DirectoryUnavailable,
        ) => RemoteWorkspaceReconnectFailure::DirectoryUnavailable,
        RemoteWindowManagerLifecycleError::Revalidation(
            crate::terminal::RemoteChannelRevalidationError::IdentityChanged,
        ) => RemoteWorkspaceReconnectFailure::IdentityChanged,
        _ => RemoteWorkspaceReconnectFailure::ConnectionFailed { detail: None },
    }
}

fn remote_workspace_reconnect_failure_state(
    failure: &RemoteWorkspaceReconnectFailure,
    generation: u64,
) -> RemoteConnectionState {
    match failure {
        RemoteWorkspaceReconnectFailure::ConnectionFailed { .. } => {
            RemoteConnectionState::failed(generation)
        }
        RemoteWorkspaceReconnectFailure::Cancelled
        | RemoteWorkspaceReconnectFailure::DirectoryUnavailable
        | RemoteWorkspaceReconnectFailure::IdentityChanged => {
            RemoteConnectionState::disconnected(generation)
        }
    }
}

fn remote_workspace_reconnect_error_content(
    failure: &RemoteWorkspaceReconnectFailure,
) -> Option<(&'static str, String)> {
    match failure {
        RemoteWorkspaceReconnectFailure::Cancelled => None,
        RemoteWorkspaceReconnectFailure::ConnectionFailed { detail } => Some((
            "Couldn’t Reconnect",
            detail.as_ref().map_or_else(
                || {
                    "SpaceTerm couldn’t restore the remote connection. Check the SSH host and try again."
                        .to_owned()
                },
                |detail| {
                    format!(
                        "SpaceTerm couldn’t restore the remote connection. OpenSSH reported:\n\n{}",
                        detail.as_str()
                    )
                },
            ),
        )),
        RemoteWorkspaceReconnectFailure::DirectoryUnavailable => Some((
            "Remote Directory Unavailable",
            "SpaceTerm can’t access the selected remote directory. Check its permissions or reopen the Remote Project."
                .to_owned(),
        )),
        RemoteWorkspaceReconnectFailure::IdentityChanged => Some((
            "Remote Directory Changed",
            "The selected remote path now resolves to a different directory. Reopen the Remote Project to review it."
                .to_owned(),
        )),
    }
}

fn remote_connection_color(phase: RemoteConnectionPhase) -> Color {
    match phase {
        RemoteConnectionPhase::Reconnecting => ACTIVE_THEME.info,
        RemoteConnectionPhase::Connected => ACTIVE_THEME.success,
        RemoteConnectionPhase::Disconnected | RemoteConnectionPhase::Closing => {
            ACTIVE_THEME.text_muted
        }
        RemoteConnectionPhase::Failed => ACTIVE_THEME.error,
    }
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

fn workspace_directory_labels(
    local_directory: Option<&std::path::Path>,
    remote_directory: Option<&RemoteWorkspaceDirectory>,
    local_home: &std::path::Path,
) -> (String, String) {
    match (local_directory, remote_directory) {
        (Some(directory), None) => (
            compact_home_path(directory, local_home),
            directory.display().to_string(),
        ),
        (None, Some(directory)) => {
            let directory = directory.as_str().to_owned();
            (directory.clone(), directory)
        }
        (Some(_), Some(_)) | (None, None) => {
            unreachable!("a Workspace must own exactly one local or remote directory")
        }
    }
}

fn remote_workspace_fallback_title(
    key: &RemoteWorkspaceKey,
    home_identity: &crate::domain::RemoteDirectoryIdentity,
) -> String {
    if key.physical_directory() == home_identity {
        return key.destination().as_str().to_owned();
    }
    let physical = key.physical_directory().as_str();
    let basename = if physical == "/" {
        "/"
    } else {
        physical
            .rsplit('/')
            .next()
            .filter(|component| !component.is_empty())
            .unwrap_or(physical)
    };
    format!("{basename} · {}", key.destination().as_str())
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
    use std::collections::VecDeque;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use gpui::{
        Modifiers, MouseDownEvent, MouseUpEvent, ScrollDelta, ScrollWheelEvent, TestAppContext,
        TouchPhase, VisualTestContext, point,
    };
    use spaceterm_ui::{
        Alert, Dialog, DialogCloseDecision, DialogInitialFocus, ModalAction, ModalActionRole,
        ModalId, ModalPresentationHandle, TextDirection,
    };

    use super::*;
    use crate::platform::finder_fallback::ScriptedFinderFallback;
    use crate::platform::macos_window_drag::RecordingOperatingSystemWindowDragPlatform;
    use crate::ssh::command::{SshCommandContext, ValidatedRemoteShellCommand};
    use crate::ssh::destination::SshHostAlias;
    use crate::ssh::host_config::HostDiscovery;
    use crate::ssh::managed_hosts::ManagedSshHost;
    use crate::terminal::testing::{
        RecordedSessionCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
    };
    use crate::terminal::{SessionEvent, SessionExit};
    use crate::ui::remote_workspace_flow::{
        RemoteWorkspaceAliasPin, RemoteWorkspaceAliasPinError, RemoteWorkspaceConnectContext,
        RemoteWorkspaceFlowBackendError, RemoteWorkspaceFlowStage, RemoteWorkspaceSessionOwner,
    };
    use crate::ui::remote_workspace_picker::{
        RemoteWorkspaceAccount, RemoteWorkspaceDirectoryListing, RemoteWorkspaceExactPathState,
        RemoteWorkspaceProvider, RemoteWorkspaceProviderError,
    };
    use crate::ui::ssh_host_form::ManagedHostFormBackendError;

    #[derive(Default)]
    struct TestRemoteWorkspaceFlowBackend {
        connections: Mutex<
            VecDeque<
                gpui::Task<
                    Result<RemoteWorkspaceConnectedSession, RemoteWorkspaceFlowBackendError>,
                >,
            >,
        >,
        connect_calls: AtomicUsize,
    }

    impl TestRemoteWorkspaceFlowBackend {
        fn with_connections(
            connections: impl IntoIterator<
                Item = gpui::Task<
                    Result<RemoteWorkspaceConnectedSession, RemoteWorkspaceFlowBackendError>,
                >,
            >,
        ) -> Arc<Self> {
            Arc::new(Self {
                connections: Mutex::new(connections.into_iter().collect()),
                connect_calls: AtomicUsize::new(0),
            })
        }
    }

    impl RemoteWorkspaceFlowBackend for TestRemoteWorkspaceFlowBackend {
        fn discover_hosts(&self) -> HostDiscovery {
            HostDiscovery::default()
        }

        fn host_in_active_use(&self, _: &SshHostAlias) -> bool {
            false
        }

        fn managed_host(&self, _: &SshHostAlias) -> Option<ManagedSshHost> {
            None
        }

        fn save_managed_host(
            &self,
            _: ManagedSshHost,
            _: Option<SshHostAlias>,
        ) -> gpui::Task<Result<(), ManagedHostFormBackendError>> {
            gpui::Task::ready(Ok(()))
        }

        fn delete_managed_host(
            &self,
            _: SshHostAlias,
        ) -> gpui::Task<Result<(), RemoteWorkspaceFlowBackendError>> {
            gpui::Task::ready(Ok(()))
        }

        fn connect(
            &self,
            _: crate::domain::SshDestination,
            _: RemoteWorkspaceConnectContext,
        ) -> gpui::Task<Result<RemoteWorkspaceConnectedSession, RemoteWorkspaceFlowBackendError>>
        {
            self.connect_calls.fetch_add(1, Ordering::AcqRel);
            self.connections
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| {
                    gpui::Task::ready(Err(RemoteWorkspaceFlowBackendError::ConnectionFailed))
                })
        }
    }

    struct TestRemoteWorkspaceFlowBackendFactory {
        backend: Arc<TestRemoteWorkspaceFlowBackend>,
    }

    impl RemoteWorkspaceFlowBackendFactory for TestRemoteWorkspaceFlowBackendFactory {
        fn create(
            &self,
            _: &Window,
            _: &mut App,
        ) -> Result<Arc<dyn RemoteWorkspaceFlowBackend>, RemoteWorkspaceFlowBackendError> {
            Ok(self.backend.clone())
        }
    }

    struct UnavailableTestRemoteWorkspaceFlowBackendFactory {
        create_calls: Arc<AtomicUsize>,
    }

    impl RemoteWorkspaceFlowBackendFactory for UnavailableTestRemoteWorkspaceFlowBackendFactory {
        fn unavailable_reason(&self) -> Option<String> {
            Some("OpenSSH 8.2 or later is required".to_owned())
        }

        fn create(
            &self,
            _: &Window,
            _: &mut App,
        ) -> Result<Arc<dyn RemoteWorkspaceFlowBackend>, RemoteWorkspaceFlowBackendError> {
            self.create_calls.fetch_add(1, Ordering::AcqRel);
            Err(RemoteWorkspaceFlowBackendError::ConnectionFailed)
        }
    }

    struct FailingTestRemoteWorkspaceFlowBackendFactory {
        create_calls: Arc<AtomicUsize>,
    }

    impl RemoteWorkspaceFlowBackendFactory for FailingTestRemoteWorkspaceFlowBackendFactory {
        fn create(
            &self,
            _: &Window,
            _: &mut App,
        ) -> Result<Arc<dyn RemoteWorkspaceFlowBackend>, RemoteWorkspaceFlowBackendError> {
            self.create_calls.fetch_add(1, Ordering::AcqRel);
            Err(RemoteWorkspaceFlowBackendError::ConnectionFailed)
        }
    }

    fn test_remote_backend_factory() -> Arc<dyn RemoteWorkspaceFlowBackendFactory> {
        Arc::new(TestRemoteWorkspaceFlowBackendFactory {
            backend: Arc::new(TestRemoteWorkspaceFlowBackend::default()),
        })
    }

    struct TestRemoteProvider {
        account_available: bool,
        identity: Result<crate::domain::RemoteDirectoryIdentity, RemoteWorkspaceProviderError>,
    }

    impl TestRemoteProvider {
        fn failing() -> Self {
            Self {
                account_available: false,
                identity: Err(RemoteWorkspaceProviderError::Other),
            }
        }

        fn connected(identity: crate::domain::RemoteDirectoryIdentity) -> Self {
            Self {
                account_available: true,
                identity: Ok(identity),
            }
        }

        fn directory_unavailable() -> Self {
            Self {
                account_available: true,
                identity: Err(RemoteWorkspaceProviderError::PermissionDenied),
            }
        }
    }

    impl RemoteWorkspaceProvider for TestRemoteProvider {
        fn discover_account(
            &self,
        ) -> gpui::Task<Result<RemoteWorkspaceAccount, RemoteWorkspaceProviderError>> {
            if !self.account_available {
                return gpui::Task::ready(Err(RemoteWorkspaceProviderError::Other));
            }
            gpui::Task::ready(
                RemoteWorkspaceAccount::new(
                    "tester".to_owned(),
                    crate::domain::RemoteDirectoryIdentity::new("/home/tester".to_owned()).unwrap(),
                    "/bin/zsh".to_owned(),
                )
                .map_err(|_| RemoteWorkspaceProviderError::InvalidResponse),
            )
        }

        fn list_directories(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> gpui::Task<Result<RemoteWorkspaceDirectoryListing, RemoteWorkspaceProviderError>>
        {
            gpui::Task::ready(Err(RemoteWorkspaceProviderError::Other))
        }

        fn probe_exact_path(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> gpui::Task<Result<RemoteWorkspaceExactPathState, RemoteWorkspaceProviderError>>
        {
            gpui::Task::ready(Err(RemoteWorkspaceProviderError::Other))
        }

        fn create_directory_recursively(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> gpui::Task<Result<(), RemoteWorkspaceProviderError>> {
            gpui::Task::ready(Err(RemoteWorkspaceProviderError::Other))
        }

        fn validate_physical_identity(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> gpui::Task<Result<crate::domain::RemoteDirectoryIdentity, RemoteWorkspaceProviderError>>
        {
            gpui::Task::ready(self.identity.clone())
        }
    }

    struct BlockingRemoteProvider {
        account:
            Mutex<Option<gpui::Task<Result<RemoteWorkspaceAccount, RemoteWorkspaceProviderError>>>>,
        identity: Mutex<
            Option<
                gpui::Task<
                    Result<crate::domain::RemoteDirectoryIdentity, RemoteWorkspaceProviderError>,
                >,
            >,
        >,
        account_calls: Arc<AtomicUsize>,
        identity_calls: Arc<AtomicUsize>,
    }

    impl RemoteWorkspaceProvider for BlockingRemoteProvider {
        fn discover_account(
            &self,
        ) -> gpui::Task<Result<RemoteWorkspaceAccount, RemoteWorkspaceProviderError>> {
            self.account_calls.fetch_add(1, Ordering::AcqRel);
            self.account
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| gpui::Task::ready(Err(RemoteWorkspaceProviderError::Other)))
        }

        fn list_directories(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> gpui::Task<Result<RemoteWorkspaceDirectoryListing, RemoteWorkspaceProviderError>>
        {
            gpui::Task::ready(Err(RemoteWorkspaceProviderError::Other))
        }

        fn probe_exact_path(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> gpui::Task<Result<RemoteWorkspaceExactPathState, RemoteWorkspaceProviderError>>
        {
            gpui::Task::ready(Err(RemoteWorkspaceProviderError::Other))
        }

        fn create_directory_recursively(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> gpui::Task<Result<(), RemoteWorkspaceProviderError>> {
            gpui::Task::ready(Err(RemoteWorkspaceProviderError::Other))
        }

        fn validate_physical_identity(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> gpui::Task<Result<crate::domain::RemoteDirectoryIdentity, RemoteWorkspaceProviderError>>
        {
            self.identity_calls.fetch_add(1, Ordering::AcqRel);
            self.identity
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| gpui::Task::ready(Err(RemoteWorkspaceProviderError::Other)))
        }
    }

    fn test_remote_account() -> RemoteWorkspaceAccount {
        RemoteWorkspaceAccount::new(
            "tester".to_owned(),
            crate::domain::RemoteDirectoryIdentity::new("/home/tester".to_owned()).unwrap(),
            "/bin/zsh".to_owned(),
        )
        .unwrap()
    }

    struct TestRemoteChannelProvider {
        preparations: Arc<AtomicUsize>,
        revalidations: Arc<AtomicUsize>,
        revalidation_tasks: Mutex<
            VecDeque<gpui::Task<Result<(), crate::terminal::RemoteChannelRevalidationError>>>,
        >,
        available: Arc<AtomicBool>,
        destination: crate::domain::SshDestination,
    }

    impl crate::terminal::RemoteTerminalChannelProvider for TestRemoteChannelProvider {
        fn is_ready(&self) -> bool {
            self.available.load(Ordering::Acquire)
        }

        fn revalidate(
            &self,
        ) -> gpui::Task<Result<(), crate::terminal::RemoteChannelRevalidationError>> {
            self.revalidations.fetch_add(1, Ordering::AcqRel);
            self.revalidation_tasks
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| gpui::Task::ready(Ok(())))
        }

        fn prepare(
            &self,
        ) -> Result<
            crate::ssh::command::PreparedSshPaneChannelCommand,
            crate::terminal::RemoteChannelUnavailable,
        > {
            self.preparations.fetch_add(1, Ordering::AcqRel);
            if !self.available.load(Ordering::Acquire) {
                return Err(crate::terminal::RemoteChannelUnavailable);
            }
            Ok(SshCommandContext::new(
                PathBuf::from("/private/config/spaceterm/ssh_config"),
                self.destination.clone(),
                PathBuf::from("/private/runtime/spaceterm/master.sock"),
            )
            .unwrap()
            .prepare_pane_channel(
                ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
            ))
        }
    }

    struct TestRemoteSessionOwner {
        closes: Arc<AtomicUsize>,
        channels: Arc<dyn crate::terminal::RemoteTerminalChannelProvider>,
        lifecycle: Option<crate::ssh::live_connection::ControlConnectionObserver>,
        _lifecycle_senders:
            Vec<async_channel::Sender<crate::ssh::live_connection::ControlConnectionTerminalState>>,
        alias: Option<crate::ssh::alias_usage::ActiveSshAliasLease>,
        alias_pin_error: bool,
    }

    impl RemoteWorkspaceSessionOwner for TestRemoteSessionOwner {
        fn acquire_workspace_alias_pin(
            &self,
        ) -> Result<Option<RemoteWorkspaceAliasPin>, RemoteWorkspaceAliasPinError> {
            if self.alias_pin_error {
                return Err(RemoteWorkspaceAliasPinError);
            }
            self.alias
                .as_ref()
                .map(|alias| {
                    alias
                        .try_duplicate()
                        .map(RemoteWorkspaceAliasPin::new)
                        .map_err(|_| RemoteWorkspaceAliasPinError)
                })
                .transpose()
        }

        fn bind_terminal_channels_for_identity(
            &self,
            _: &RemoteWorkspaceDirectory,
            _: &crate::domain::RemoteDirectoryIdentity,
            _: &crate::ssh::command::ValidatedRemoteLoginShell,
        ) -> Result<
            Arc<dyn crate::terminal::RemoteTerminalChannelProvider>,
            RemoteWorkspaceFlowBackendError,
        > {
            Ok(Arc::clone(&self.channels))
        }

        fn take_lifecycle_observer(
            &mut self,
        ) -> Option<crate::ssh::live_connection::ControlConnectionObserver> {
            self.lifecycle.take()
        }

        fn close(&mut self) {
            self.closes.fetch_add(1, Ordering::AcqRel);
            self.alias.take();
        }
    }

    fn remote_completion(
        destination: &str,
        directory: &str,
        physical: &str,
        available: bool,
    ) -> (
        RemoteWorkspaceFlowCompletion,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<AtomicBool>,
    ) {
        let (completion, closes, preparations, _, availability, _) =
            remote_completion_with_revalidation(
                destination,
                directory,
                physical,
                available,
                gpui::Task::ready(Ok(())),
            );
        (completion, closes, preparations, availability)
    }

    fn remote_completion_with_revalidation(
        destination: &str,
        directory: &str,
        physical: &str,
        available: bool,
        revalidation: gpui::Task<Result<(), crate::terminal::RemoteChannelRevalidationError>>,
    ) -> (
        RemoteWorkspaceFlowCompletion,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<AtomicBool>,
        async_channel::Sender<crate::ssh::live_connection::ControlConnectionTerminalState>,
    ) {
        let destination = crate::domain::SshDestination::new(destination.to_owned()).unwrap();
        let directory = RemoteWorkspaceDirectory::new(directory.to_owned()).unwrap();
        let physical = crate::domain::RemoteDirectoryIdentity::new(physical.to_owned()).unwrap();
        let home = crate::domain::RemoteDirectoryIdentity::new("/home/tester".to_owned()).unwrap();
        let preparations = Arc::new(AtomicUsize::new(0));
        let revalidations = Arc::new(AtomicUsize::new(0));
        let availability = Arc::new(AtomicBool::new(available));
        let channels: Arc<dyn crate::terminal::RemoteTerminalChannelProvider> =
            Arc::new(TestRemoteChannelProvider {
                preparations: Arc::clone(&preparations),
                revalidations: Arc::clone(&revalidations),
                revalidation_tasks: Mutex::new(VecDeque::from([revalidation])),
                available: Arc::clone(&availability),
                destination: destination.clone(),
            });
        let closes = Arc::new(AtomicUsize::new(0));
        let (runtime_lifecycle_sender, runtime_lifecycle) =
            crate::ssh::live_connection::ControlConnectionObserver::controlled();
        let (session_lifecycle_sender, session_lifecycle) =
            crate::ssh::live_connection::ControlConnectionObserver::controlled();
        let session = RemoteWorkspaceConnectedSession::new(
            Box::new(TestRemoteSessionOwner {
                closes: Arc::clone(&closes),
                channels: Arc::clone(&channels),
                lifecycle: Some(session_lifecycle),
                _lifecycle_senders: vec![
                    runtime_lifecycle_sender.clone(),
                    session_lifecycle_sender,
                ],
                alias: None,
                alias_pin_error: false,
            }),
            Arc::new(TestRemoteProvider::failing()),
        );
        let account =
            RemoteWorkspaceAccount::new("tester".to_owned(), home, "/bin/zsh".to_owned()).unwrap();
        (
            RemoteWorkspaceFlowCompletion::for_test(
                session,
                destination,
                directory,
                physical,
                account,
                channels,
                runtime_lifecycle,
            ),
            closes,
            preparations,
            revalidations,
            availability,
            runtime_lifecycle_sender,
        )
    }

    fn remote_completion_with_active_alias() -> (
        RemoteWorkspaceFlowCompletion,
        crate::ssh::alias_usage::ActiveSshAliasRegistry,
        SshHostAlias,
        Arc<AtomicUsize>,
        async_channel::Sender<crate::ssh::live_connection::ControlConnectionTerminalState>,
    ) {
        remote_completion_with_active_alias_pin_failure(false)
    }

    fn remote_completion_with_active_alias_pin_failure(
        alias_pin_error: bool,
    ) -> (
        RemoteWorkspaceFlowCompletion,
        crate::ssh::alias_usage::ActiveSshAliasRegistry,
        SshHostAlias,
        Arc<AtomicUsize>,
        async_channel::Sender<crate::ssh::live_connection::ControlConnectionTerminalState>,
    ) {
        let registry = crate::ssh::alias_usage::ActiveSshAliasRegistry::default();
        let alias = SshHostAlias::new("work".to_owned()).unwrap();
        let lease = registry.acquire(alias.clone()).unwrap();
        let destination = crate::domain::SshDestination::new("work".to_owned()).unwrap();
        let directory = RemoteWorkspaceDirectory::new("~/src".to_owned()).unwrap();
        let physical =
            crate::domain::RemoteDirectoryIdentity::new("/home/tester/src".to_owned()).unwrap();
        let home = crate::domain::RemoteDirectoryIdentity::new("/home/tester".to_owned()).unwrap();
        let preparations = Arc::new(AtomicUsize::new(0));
        let revalidations = Arc::new(AtomicUsize::new(0));
        let channels: Arc<dyn crate::terminal::RemoteTerminalChannelProvider> =
            Arc::new(TestRemoteChannelProvider {
                preparations,
                revalidations,
                revalidation_tasks: Mutex::new(VecDeque::from([gpui::Task::ready(Ok(()))])),
                available: Arc::new(AtomicBool::new(true)),
                destination: destination.clone(),
            });
        let closes = Arc::new(AtomicUsize::new(0));
        let (runtime_lifecycle_sender, runtime_lifecycle) =
            crate::ssh::live_connection::ControlConnectionObserver::controlled();
        let (session_lifecycle_sender, session_lifecycle) =
            crate::ssh::live_connection::ControlConnectionObserver::controlled();
        let session = RemoteWorkspaceConnectedSession::new(
            Box::new(TestRemoteSessionOwner {
                closes: Arc::clone(&closes),
                channels: Arc::clone(&channels),
                lifecycle: Some(session_lifecycle),
                _lifecycle_senders: vec![
                    runtime_lifecycle_sender.clone(),
                    session_lifecycle_sender,
                ],
                alias: Some(lease),
                alias_pin_error,
            }),
            Arc::new(TestRemoteProvider::failing()),
        );
        let account =
            RemoteWorkspaceAccount::new("tester".to_owned(), home, "/bin/zsh".to_owned()).unwrap();
        (
            RemoteWorkspaceFlowCompletion::for_test(
                session,
                destination,
                directory,
                physical,
                account,
                channels,
                runtime_lifecycle,
            ),
            registry,
            alias,
            closes,
            runtime_lifecycle_sender,
        )
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
            WorkspaceManager::new_with_remote_workspace_backend_factory(
                session_factory,
                PathBuf::from("/Users/test"),
                test_remote_backend_factory(),
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

    fn workspace_manager_with_remote_backend(
        backend: Arc<TestRemoteWorkspaceFlowBackend>,
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
        let factory: Arc<dyn RemoteWorkspaceFlowBackendFactory> =
            Arc::new(TestRemoteWorkspaceFlowBackendFactory { backend });
        let (manager, cx) = cx.add_window_view(move |window, cx| {
            WorkspaceManager::new_with_remote_workspace_backend_factory(
                session_factory,
                PathBuf::from("/Users/test"),
                factory,
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

    fn reconnect_session(
        destination: &str,
        physical: &str,
    ) -> (
        RemoteWorkspaceConnectedSession,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        async_channel::Sender<crate::ssh::live_connection::ControlConnectionTerminalState>,
    ) {
        let physical = crate::domain::RemoteDirectoryIdentity::new(physical.to_owned()).unwrap();
        reconnect_session_with_provider(
            destination,
            Arc::new(TestRemoteProvider::connected(physical)),
        )
    }

    fn reconnect_session_with_provider(
        destination: &str,
        provider: Arc<dyn RemoteWorkspaceProvider>,
    ) -> (
        RemoteWorkspaceConnectedSession,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        async_channel::Sender<crate::ssh::live_connection::ControlConnectionTerminalState>,
    ) {
        reconnect_session_with_provider_and_revalidation(destination, provider, VecDeque::new())
    }

    fn reconnect_session_with_provider_and_revalidation(
        destination: &str,
        provider: Arc<dyn RemoteWorkspaceProvider>,
        revalidation_tasks: VecDeque<
            gpui::Task<Result<(), crate::terminal::RemoteChannelRevalidationError>>,
        >,
    ) -> (
        RemoteWorkspaceConnectedSession,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        async_channel::Sender<crate::ssh::live_connection::ControlConnectionTerminalState>,
    ) {
        let destination = crate::domain::SshDestination::new(destination.to_owned()).unwrap();
        let preparations = Arc::new(AtomicUsize::new(0));
        let revalidations = Arc::new(AtomicUsize::new(0));
        let channels: Arc<dyn crate::terminal::RemoteTerminalChannelProvider> =
            Arc::new(TestRemoteChannelProvider {
                preparations: Arc::clone(&preparations),
                revalidations: Arc::clone(&revalidations),
                revalidation_tasks: Mutex::new(revalidation_tasks),
                available: Arc::new(AtomicBool::new(true)),
                destination,
            });
        let closes = Arc::new(AtomicUsize::new(0));
        let (lifecycle_sender, lifecycle) =
            crate::ssh::live_connection::ControlConnectionObserver::controlled();
        (
            RemoteWorkspaceConnectedSession::new(
                Box::new(TestRemoteSessionOwner {
                    closes: Arc::clone(&closes),
                    channels,
                    lifecycle: Some(lifecycle),
                    _lifecycle_senders: vec![lifecycle_sender.clone()],
                    alias: None,
                    alias_pin_error: false,
                }),
                provider,
            ),
            closes,
            preparations,
            revalidations,
            lifecycle_sender,
        )
    }

    fn workspace_manager_with_operating_system_window_drag_platform(
        cx: &mut TestAppContext,
    ) -> (
        Entity<WorkspaceManager>,
        Rc<RecordingOperatingSystemWindowDragPlatform>,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records).with_fallback_title("zsh"));
        let platform = Rc::new(RecordingOperatingSystemWindowDragPlatform::default());
        let injected_platform = Rc::clone(&platform);
        let (manager, cx) = cx.add_window_view(move |window, cx| {
            WorkspaceManager::new_with_operating_system_window_drag_platform(
                session_factory,
                PathBuf::from("/Users/test"),
                injected_platform,
                test_remote_backend_factory(),
                window,
                cx,
            )
        });
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.focus(window, cx));
        });
        cx.run_until_parked();
        (manager, platform, cx)
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
        let finder_fallback: Rc<dyn FinderFallback> =
            Rc::new(ScriptedFinderFallback::new(selections));
        let (manager, cx) = cx.add_window_view(|window, cx| {
            WorkspaceManager::new_with_finder_fallback(
                session_factory,
                PathBuf::from("/Users/test"),
                finder_fallback,
                test_remote_backend_factory(),
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

    fn test_alert(id: &'static str) -> Alert<&'static str> {
        Alert::new(
            ModalId::new(id),
            "Application integration alert",
            "Application Integration",
            "Confirm the modal integration behavior.",
            vec![
                ModalAction::new(
                    "acknowledge",
                    "OK",
                    ModalActionRole::Affirmative,
                    "acknowledge",
                )
                .default_action(true),
            ],
        )
    }

    fn present_test_alert(
        manager: &Entity<WorkspaceManager>,
        id: &'static str,
        cx: &mut VisualTestContext,
    ) -> ModalPresentationHandle {
        cx.update(|window, cx| {
            manager
                .update(cx, |_, cx| test_alert(id).present(window, cx, |_, _| {}))
                .expect("test alert should present")
        })
    }

    fn open_workspace_picker(cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("shift-cmd-o");
        cx.run_until_parked();
    }

    fn open_new_workspace_panel(cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();
    }

    fn open_remote_workspace_flow(
        manager: &Entity<WorkspaceManager>,
        cx: &mut VisualTestContext,
    ) -> Entity<RemoteWorkspaceFlow> {
        open_new_workspace_panel(cx);
        click("new-workspace-source-remote-project", cx);
        manager.read_with(cx, |manager, _| {
            manager
                .remote_workspace_flow
                .as_ref()
                .expect("Remote Project must create its flow")
                .clone()
        })
    }

    fn emit_remote_workspace_completion(
        flow: &Entity<RemoteWorkspaceFlow>,
        completion: RemoteWorkspaceFlowCompletion,
        cx: &mut VisualTestContext,
    ) {
        cx.update(|_, cx| {
            flow.update(cx, |flow, cx| flow.emit_completion_for_test(completion, cx));
        });
        cx.run_until_parked();
    }

    fn create_disconnected_remote_workspace(
        manager: &Entity<WorkspaceManager>,
        cx: &mut VisualTestContext,
    ) -> WorkspaceId {
        let flow = open_remote_workspace_flow(manager, cx);
        let (completion, _, _, _, _, lifecycle) = remote_completion_with_revalidation(
            "work",
            "~/src",
            "/home/tester/src",
            true,
            gpui::Task::ready(Ok(())),
        );
        emit_remote_workspace_completion(&flow, completion, cx);
        let workspace_id =
            manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
        lifecycle
            .try_send(ControlConnectionTerminalState::Closed)
            .unwrap();
        cx.run_until_parked();
        workspace_id
    }

    fn install_remote_completion_directly(
        manager: &Entity<WorkspaceManager>,
        completion: RemoteWorkspaceFlowCompletion,
        cx: &mut VisualTestContext,
    ) {
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                let key = RemoteWorkspaceKey::new(
                    completion.destination().clone(),
                    completion.physical_directory().clone(),
                );
                let terminal_factory = WorkspaceTerminalSessionFactory::new_remote(
                    Rc::clone(&manager.session_factory),
                    ValidatedWorkspaceDirectory::new(
                        manager.default_workspace_root.clone(),
                        manager.default_workspace_identity,
                    ),
                    RemoteTerminalMetadataContext::new(
                        completion.destination().clone(),
                        completion.directory().clone(),
                    ),
                    remote_workspace_fallback_title(&key, completion.remote_home_identity()),
                    completion.terminal_channels(),
                );
                let prepared = terminal_factory.prepare_child_launch().unwrap();
                manager
                    .try_create_remote_project(completion, terminal_factory, prepared, window, cx)
                    .unwrap_or_else(|_| panic!("direct Remote Workspace installation failed"));
            });
        });
        cx.run_until_parked();
    }

    fn choose_with_finder_fallback(cx: &mut VisualTestContext) {
        open_workspace_picker(cx);
        click("workspace-picker-finder", cx);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn application_rtl_locale_installation_should_mirror_production_modal_footer(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| crate::ui::init_with_text_direction(cx, TextDirection::RightToLeft));
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records).with_fallback_title("zsh"));
        let (manager, cx) = cx.add_window_view(|window, cx| {
            WorkspaceManager::new_with_remote_workspace_backend_factory(
                session_factory,
                PathBuf::from("/Users/test"),
                test_remote_backend_factory(),
                window,
                cx,
            )
        });
        let dialog = Dialog::new(
            ModalId::new("rtl-application-modal"),
            "RTL application modal",
            "RTL Application Modal",
            vec![
                ModalAction::new(
                    "save",
                    "Save",
                    ModalActionRole::Affirmative,
                    "rtl-application-save",
                )
                .default_action(true),
                ModalAction::new(
                    "help",
                    "Help",
                    ModalActionRole::Help,
                    "rtl-application-help",
                ),
                ModalAction::new(
                    "cancel",
                    "Cancel",
                    ModalActionRole::Cancel,
                    "rtl-application-cancel",
                ),
            ],
            DialogInitialFocus::Action("save"),
        )
        .description("Verify installed locale behavior.");
        let _completion = cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.focus(window, cx);
                dialog
                    .present(
                        window,
                        cx,
                        |_, _, _| DialogCloseDecision::Deny {
                            first_invalid: None,
                        },
                        |_, _| {},
                    )
                    .expect("application integration Dialog should present")
            })
        });
        cx.run_until_parked();

        let save = cx
            .debug_bounds("modal-action-rtl-application-save")
            .expect("Save should render");
        let cancel = cx
            .debug_bounds("modal-action-rtl-application-cancel")
            .expect("Cancel should render");
        let help = cx
            .debug_bounds("modal-action-rtl-application-help")
            .expect("Help should render");
        let policy_is_rtl = cx.update(|_, cx| {
            *cx.global::<spaceterm_ui::ModalDesktopPolicy>()
                == spaceterm_ui::ModalDesktopPolicy::mac_os()
                    .with_text_direction(TextDirection::RightToLeft)
        });

        assert!(
            policy_is_rtl && save.left() < cancel.left() && cancel.right() < help.left(),
            "RTL production bounds were save={save:?}, cancel={cancel:?}, help={help:?}"
        );
    }

    #[gpui::test]
    fn workspace_root_should_render_modal_outside_tooltip_content(cx: &mut TestAppContext) {
        let (manager, _, cx) = workspace_manager(cx);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let pane_host = manager.read_with(cx, |manager, cx| {
            manager
                .workspaces
                .active_workspace()
                .payload()
                .read(cx)
                .active_pane_host()
        });
        let focused_pane = pane_host.read_with(cx, |pane_host, _| pane_host.focused_pane_id());
        let focused_before = cx.update(|window, cx| {
            pane_host
                .read(cx)
                .focused_terminal_has_input_focus(window, cx)
        });

        let presentation = present_test_alert(&manager, "root-layer-alert", cx);
        cx.run_until_parked();
        let modal_state = cx.update(|window, cx| {
            let pane_host = pane_host.read(cx);
            (
                pane_host.focused_pane_id(),
                pane_host.focused_terminal_has_input_focus(window, cx),
            )
        });

        assert!(cx.debug_bounds("spaceterm-modal-root").is_some());
        assert_eq!(presentation.presentation_id().value(), 1);
        assert!(cx.debug_bounds("modal-surface-1").is_some());
        assert_eq!((focused_before, modal_state), (true, (focused_pane, false)));

        cx.update(|window, cx| {
            presentation
                .dismiss(window, cx)
                .expect("root integration modal should dismiss")
        });
        cx.run_until_parked();
        let restored = cx.update(|window, cx| {
            let pane_host = pane_host.read(cx);
            (
                pane_host.focused_pane_id(),
                pane_host.focused_terminal_has_input_focus(window, cx),
            )
        });

        assert_eq!(restored, (focused_pane, true));
    }

    #[gpui::test]
    fn workspace_search_reentry_should_not_steal_focus_from_an_active_modal(
        cx: &mut TestAppContext,
    ) {
        let (manager, _, cx) = workspace_manager(cx);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.open_workspace_search(window, cx));
        });
        cx.run_until_parked();
        let presentation = present_test_alert(&manager, "workspace-search-modal-priority", cx);
        cx.run_until_parked();

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.open_workspace_search(window, cx);
                manager.open_workspace_search(window, cx);
            });
        });
        cx.run_until_parked();
        let focus_contained = cx.update(|window, cx| {
            let manager = manager.read(cx);
            window_modal_is_open(window, cx)
                && !manager
                    .workspace_search
                    .read(cx)
                    .palette()
                    .read(cx)
                    .editor_is_focused(window, cx)
        });

        assert!(focus_contained);
        assert!(cx.debug_bounds("command-palette-panel").is_none());

        cx.update(|window, cx| {
            presentation
                .dismiss(window, cx)
                .expect("workspace-search modal should dismiss")
        });
        cx.run_until_parked();

        let resumed = cx.update(|window, cx| {
            let palette = manager.read(cx).workspace_search.read(cx).palette();
            (
                palette.read(cx).is_open(),
                palette.read(cx).editor_is_focused(window, cx),
                window_modal_is_open(window, cx),
                window.is_window_active(),
            )
        });
        assert_eq!(resumed, (true, true, false, true));
        assert!(cx.debug_bounds("command-palette-panel").is_some());
    }

    #[gpui::test]
    fn workspace_picker_reentry_should_not_steal_focus_from_an_active_modal(
        cx: &mut TestAppContext,
    ) {
        let (manager, _, cx) = workspace_manager(cx);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.present_workspace_picker(window, cx)
            });
        });
        cx.run_until_parked();
        let presentation = present_test_alert(&manager, "workspace-picker-modal-priority", cx);
        cx.run_until_parked();

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.open_local_project(window, cx);
                manager.open_local_project(window, cx);
            });
        });
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let focus_contained = cx.update(|window, cx| {
            let manager = manager.read(cx);
            window_modal_is_open(window, cx)
                && !manager
                    .workspace_picker
                    .read(cx)
                    .path_input_is_focused(window, cx)
        });

        assert!(focus_contained);
        assert!(cx.debug_bounds("modal-surface-1").is_some());

        cx.update(|window, cx| {
            presentation
                .dismiss(window, cx)
                .expect("workspace-picker modal should dismiss")
        });
        cx.run_until_parked();

        assert!(cx.debug_bounds("command-palette-panel").is_some());
        assert!(cx.update(|window, cx| {
            manager
                .read(cx)
                .workspace_picker
                .read(cx)
                .path_input_is_focused(window, cx)
        }));
    }

    #[gpui::test]
    fn queued_modals_should_preserve_focused_pane_and_restore_terminal_input_focus(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let pane_host = manager.read_with(cx, |manager, cx| {
            manager
                .workspaces
                .active_workspace()
                .payload()
                .read(cx)
                .active_pane_host()
        });
        let focused_pane = pane_host.read_with(cx, |pane_host, _| pane_host.focused_pane_id());
        let before = cx.update(|window, cx| {
            (
                pane_host.read(cx).focused_pane_id(),
                pane_host
                    .read(cx)
                    .focused_terminal_has_input_focus(window, cx),
            )
        });
        let command_count = records.commands().len();

        let first = present_test_alert(&manager, "queued-modal-first", cx);
        cx.run_until_parked();
        let active = cx.update(|window, cx| {
            (
                pane_host.read(cx).focused_pane_id(),
                pane_host
                    .read(cx)
                    .focused_terminal_has_input_focus(window, cx),
            )
        });
        let second = present_test_alert(&manager, "queued-modal-second", cx);
        cx.run_until_parked();
        let queued = cx.update(|window, cx| {
            (
                pane_host.read(cx).focused_pane_id(),
                pane_host
                    .read(cx)
                    .focused_terminal_has_input_focus(window, cx),
            )
        });

        cx.update(|window, cx| {
            first
                .dismiss(window, cx)
                .expect("first queued modal should dismiss")
        });
        cx.run_until_parked();
        let promoted = cx.update(|window, cx| {
            (
                pane_host.read(cx).focused_pane_id(),
                pane_host
                    .read(cx)
                    .focused_terminal_has_input_focus(window, cx),
            )
        });

        cx.update(|window, cx| {
            second
                .dismiss(window, cx)
                .expect("promoted modal should dismiss")
        });
        cx.run_until_parked();
        let restored = cx.update(|window, cx| {
            (
                pane_host.read(cx).focused_pane_id(),
                pane_host
                    .read(cx)
                    .focused_terminal_has_input_focus(window, cx),
            )
        });
        let focus_reports = records
            .commands()
            .into_iter()
            .skip(command_count)
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Focus(focused) => Some(focused),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            (before, active, queued, promoted, restored, focus_reports),
            (
                (focused_pane, true),
                (focused_pane, false),
                (focused_pane, false),
                (focused_pane, false),
                (focused_pane, true),
                vec![false, true],
            )
        );
    }

    #[gpui::test]
    fn simultaneous_modals_should_block_and_restore_terminal_input_focus_per_operating_system_window(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init);
        let first_records = TestTerminalSessionRecords::default();
        let second_records = TestTerminalSessionRecords::default();
        let first_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
            TestTerminalSessionFactory::new(first_records.clone()).with_fallback_title("zsh"),
        );
        let second_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
            TestTerminalSessionFactory::new(second_records.clone()).with_fallback_title("zsh"),
        );
        let first = cx.add_window(|window, cx| {
            WorkspaceManager::new_with_remote_workspace_backend_factory(
                first_factory,
                PathBuf::from("/Users/first"),
                test_remote_backend_factory(),
                window,
                cx,
            )
        });
        let second = cx.add_window(|window, cx| {
            WorkspaceManager::new_with_remote_workspace_backend_factory(
                second_factory,
                PathBuf::from("/Users/second"),
                test_remote_backend_factory(),
                window,
                cx,
            )
        });

        let first_pane_host = first
            .update(cx, |manager, window, cx| {
                window.activate_window();
                manager.focus(window, cx);
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .active_pane_host()
            })
            .expect("first Operating-System Window should remain available");
        cx.run_until_parked();
        let first_focused_pane =
            first_pane_host.read_with(cx, |pane_host, _| pane_host.focused_pane_id());
        let first_before = first
            .update(cx, |_, window, cx| {
                first_pane_host
                    .read(cx)
                    .focused_terminal_has_input_focus(window, cx)
            })
            .expect("first Operating-System Window should remain available");
        let first_command_count = first_records.commands().len();
        let first_presentation = first
            .update(cx, |_, window, cx| {
                test_alert("first-window-modal").present(window, cx, |_, _| {})
            })
            .expect("first Operating-System Window should remain available")
            .expect("first Operating-System Window should present its modal");
        cx.run_until_parked();

        let second_pane_host = second
            .update(cx, |manager, window, cx| {
                window.activate_window();
                manager.focus(window, cx);
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .active_pane_host()
            })
            .expect("second Operating-System Window should remain available");
        cx.run_until_parked();
        let second_focused_pane =
            second_pane_host.read_with(cx, |pane_host, _| pane_host.focused_pane_id());
        let second_before = second
            .update(cx, |_, window, cx| {
                second_pane_host
                    .read(cx)
                    .focused_terminal_has_input_focus(window, cx)
            })
            .expect("second Operating-System Window should remain available");
        let second_command_count = second_records.commands().len();
        let second_presentation = second
            .update(cx, |_, window, cx| {
                test_alert("second-window-modal").present(window, cx, |_, _| {})
            })
            .expect("second Operating-System Window should remain available")
            .expect("second Operating-System Window should present its modal");
        cx.run_until_parked();

        let first_blocked = first
            .update(cx, |_, window, cx| {
                let pane_host = first_pane_host.read(cx);
                (
                    pane_host.focused_pane_id(),
                    pane_host.focused_terminal_has_input_focus(window, cx),
                )
            })
            .expect("first Operating-System Window should remain available");
        let second_blocked = second
            .update(cx, |_, window, cx| {
                let pane_host = second_pane_host.read(cx);
                (
                    pane_host.focused_pane_id(),
                    pane_host.focused_terminal_has_input_focus(window, cx),
                )
            })
            .expect("second Operating-System Window should remain available");

        first
            .update(cx, |_, window, cx| {
                window.activate_window();
                first_presentation.dismiss(window, cx)
            })
            .expect("first Operating-System Window should remain available")
            .expect("first Operating-System Window modal should dismiss");
        cx.run_until_parked();
        let first_restored = first
            .update(cx, |_, window, cx| {
                let pane_host = first_pane_host.read(cx);
                (
                    pane_host.focused_pane_id(),
                    pane_host.focused_terminal_has_input_focus(window, cx),
                )
            })
            .expect("first Operating-System Window should remain available");
        let second_still_blocked = second
            .update(cx, |_, window, cx| {
                let pane_host = second_pane_host.read(cx);
                (
                    pane_host.focused_pane_id(),
                    pane_host.focused_terminal_has_input_focus(window, cx),
                )
            })
            .expect("second Operating-System Window should remain available");
        let first_focus_reports = first_records
            .commands()
            .into_iter()
            .skip(first_command_count)
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Focus(focused) => Some(focused),
                _ => None,
            })
            .collect::<Vec<_>>();
        let second_focus_reports_while_blocked = second_records
            .commands()
            .into_iter()
            .skip(second_command_count)
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Focus(focused) => Some(focused),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            (
                first_before,
                second_before,
                first_blocked,
                second_blocked,
                first_restored,
                second_still_blocked,
                first_focus_reports,
                second_focus_reports_while_blocked,
            ),
            (
                true,
                true,
                (first_focused_pane, false),
                (second_focused_pane, false),
                (first_focused_pane, true),
                (second_focused_pane, false),
                vec![false, true],
                vec![false],
            )
        );

        second
            .update(cx, |_, window, cx| {
                window.activate_window();
                second_presentation.dismiss(window, cx)
            })
            .expect("second Operating-System Window should remain available")
            .expect("second Operating-System Window modal should dismiss");
        cx.run_until_parked();
        let second_restored = second
            .update(cx, |_, window, cx| {
                let pane_host = second_pane_host.read(cx);
                (
                    pane_host.focused_pane_id(),
                    pane_host.focused_terminal_has_input_focus(window, cx),
                )
            })
            .expect("second Operating-System Window should remain available");
        let second_focus_reports = second_records
            .commands()
            .into_iter()
            .skip(second_command_count)
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Focus(focused) => Some(focused),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            (second_restored, second_focus_reports),
            ((second_focused_pane, true), vec![false, true])
        );
    }

    #[gpui::test]
    fn cancelled_finder_fallback_should_leave_hierarchy_unchanged(cx: &mut TestAppContext) {
        let (manager, records, cx) = workspace_manager_with_picker([Ok(None)], cx);

        choose_with_finder_fallback(cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| manager.workspaces.len()),
            1
        );
        assert_eq!(records.starts().len(), 1);
    }

    #[gpui::test]
    fn every_new_workspace_entry_point_should_present_the_panel_and_block_terminal_input(
        cx: &mut TestAppContext,
    ) {
        let (manager, _, cx) = workspace_manager(cx);

        click("new-workspace-button", cx);
        let sidebar_state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.new_workspace_panel.read(cx).is_open(),
                manager.terminal_focus_blocker(window, cx),
            )
        });
        open_new_workspace_panel(cx);
        let repeated_state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.new_workspace_panel.read(cx).is_open(),
                manager.terminal_focus_blocker(window, cx),
            )
        });

        assert_eq!(
            (sidebar_state, repeated_state),
            (
                (true, Some(TerminalFocusBlocker::CommandPalette)),
                (true, Some(TerminalFocusBlocker::CommandPalette)),
            )
        );
    }

    #[gpui::test]
    fn shift_cmd_o_should_present_the_workspace_picker_without_the_panel(cx: &mut TestAppContext) {
        let (manager, _, cx) = workspace_manager(cx);

        open_workspace_picker(cx);

        assert_eq!(
            cx.update(|window, cx| {
                let manager = manager.read(cx);
                (
                    manager.workspace_picker.read(cx).is_open(),
                    manager.new_workspace_panel.read(cx).is_open(),
                    manager.terminal_focus_blocker(window, cx),
                )
            }),
            (true, false, Some(TerminalFocusBlocker::Modal))
        );
    }

    #[gpui::test]
    fn choosing_local_project_should_replace_the_panel_with_the_workspace_picker(
        cx: &mut TestAppContext,
    ) {
        let (manager, _, cx) = workspace_manager(cx);

        open_new_workspace_panel(cx);
        click("new-workspace-source-local-project", cx);

        assert_eq!(
            cx.update(|window, cx| {
                let manager = manager.read(cx);
                (
                    manager.workspace_picker.read(cx).is_open(),
                    manager.new_workspace_panel.read(cx).is_open(),
                    manager.terminal_focus_blocker(window, cx),
                )
            }),
            (true, false, Some(TerminalFocusBlocker::Modal))
        );
    }

    #[gpui::test]
    fn choosing_remote_project_should_strictly_replace_the_panel_and_restore_focus_on_escape(
        cx: &mut TestAppContext,
    ) {
        let (manager, _, cx) = workspace_manager(cx);

        open_new_workspace_panel(cx);
        click("new-workspace-source-remote-project", cx);

        let opened = cx.update(|window, cx| {
            let manager = manager.read(cx);
            let flow = manager
                .remote_workspace_flow
                .as_ref()
                .expect("Remote Project must create its flow");
            (
                manager.new_workspace_panel.read(cx).is_open(),
                flow.read(cx).stage(),
                flow.read(cx).owns_first_responder(window, cx),
                manager.terminal_focus_blocker(window, cx),
            )
        });
        assert_eq!(
            opened,
            (
                false,
                RemoteWorkspaceFlowStage::HostSelection,
                true,
                Some(TerminalFocusBlocker::CommandPalette),
            )
        );

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        let escaped = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager
                    .remote_workspace_flow
                    .as_ref()
                    .expect("cancelled flow remains reusable")
                    .read(cx)
                    .stage(),
                manager.terminal_focus_blocker(window, cx),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        });
        assert_eq!(escaped, (RemoteWorkspaceFlowStage::Cancelled, None, true));

        open_new_workspace_panel(cx);
        click("new-workspace-source-remote-project", cx);
        assert_eq!(
            manager.read_with(cx, |manager, cx| {
                manager
                    .remote_workspace_flow
                    .as_ref()
                    .expect("cancelled flow should reopen")
                    .read(cx)
                    .stage()
            }),
            RemoteWorkspaceFlowStage::HostSelection
        );
    }

    #[gpui::test]
    fn unavailable_remote_source_should_stay_disabled_without_constructing_askpass_backend(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records).with_fallback_title("zsh"));
        let create_calls = Arc::new(AtomicUsize::new(0));
        let factory: Arc<dyn RemoteWorkspaceFlowBackendFactory> =
            Arc::new(UnavailableTestRemoteWorkspaceFlowBackendFactory {
                create_calls: Arc::clone(&create_calls),
            });
        let (manager, cx) = cx.add_window_view(move |window, cx| {
            WorkspaceManager::new_with_remote_workspace_backend_factory(
                session_factory,
                PathBuf::from("/Users/test"),
                factory,
                window,
                cx,
            )
        });
        cx.update(|window, cx| manager.update(cx, |manager, cx| manager.focus(window, cx)));
        cx.run_until_parked();

        open_new_workspace_panel(cx);
        click("new-workspace-source-remote-project", cx);
        cx.run_until_parked();

        assert_eq!(create_calls.load(Ordering::Acquire), 0);
        assert!(manager.read_with(cx, |manager, cx| {
            manager.new_workspace_panel.read(cx).is_open()
        }));
        assert!(manager.read_with(cx, |manager, _| manager.remote_workspace_flow.is_none()));
        assert_eq!(
            cx.update(|window, cx| manager.read(cx).terminal_focus_blocker(window, cx)),
            Some(TerminalFocusBlocker::CommandPalette)
        );
    }

    #[gpui::test]
    fn backend_construction_failure_should_disable_remote_instead_of_leaving_an_inert_source(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(TestTerminalSessionFactory::new(records).with_fallback_title("zsh"));
        let create_calls = Arc::new(AtomicUsize::new(0));
        let factory: Arc<dyn RemoteWorkspaceFlowBackendFactory> =
            Arc::new(FailingTestRemoteWorkspaceFlowBackendFactory {
                create_calls: Arc::clone(&create_calls),
            });
        let (manager, cx) = cx.add_window_view(move |window, cx| {
            WorkspaceManager::new_with_remote_workspace_backend_factory(
                session_factory,
                PathBuf::from("/Users/test"),
                factory,
                window,
                cx,
            )
        });
        cx.update(|window, cx| manager.update(cx, |manager, cx| manager.focus(window, cx)));
        cx.run_until_parked();

        open_new_workspace_panel(cx);
        click("new-workspace-source-remote-project", cx);
        cx.run_until_parked();

        assert_eq!(create_calls.load(Ordering::Acquire), 1);
        assert!(manager.read_with(cx, |manager, cx| {
            manager.new_workspace_panel.read(cx).is_open()
        }));
        assert!(manager.read_with(cx, |manager, _| manager.remote_workspace_flow.is_none()));
        assert_eq!(
            cx.update(|window, cx| manager.read(cx).terminal_focus_blocker(window, cx)),
            Some(TerminalFocusBlocker::CommandPalette)
        );
    }

    #[gpui::test]
    fn remote_completion_should_create_exact_metadata_launch_and_owned_runtime(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, closes, preparations, revalidations, _, _) =
            remote_completion_with_revalidation(
                "deploy@work",
                "~/src",
                "/home/tester/src",
                true,
                gpui::Task::ready(Ok(())),
            );

        emit_remote_workspace_completion(&flow, completion, cx);

        let (workspace_id, destination, directory, physical, runtime_count, has_alias_pin) =
            manager.read_with(cx, |manager, _| {
                let workspace = manager.workspaces.active_workspace();
                (
                    workspace.id(),
                    workspace
                        .remote_workspace_key()
                        .expect("active Workspace must be remote")
                        .destination()
                        .as_str()
                        .to_owned(),
                    workspace
                        .remote_workspace_directory()
                        .expect("Remote Workspace preserves typed spelling")
                        .as_str()
                        .to_owned(),
                    workspace
                        .remote_workspace_key()
                        .unwrap()
                        .physical_directory()
                        .as_str()
                        .to_owned(),
                    manager.remote_workspace_runtimes.len(),
                    manager
                        .remote_workspace_runtimes
                        .get(&workspace.id())
                        .is_some_and(|runtime| runtime.alias_pin.is_some()),
                )
            });
        let starts = records.starts();
        let remote = starts
            .last()
            .and_then(|start| start.remote_launch_plan())
            .expect("initial Remote Pane must use a remote launch plan");
        assert_eq!(
            (destination.as_str(), directory.as_str(), physical.as_str()),
            ("deploy@work", "~/src", "/home/tester/src")
        );
        assert_eq!(runtime_count, 1);
        assert!(
            !has_alias_pin,
            "a raw SSH destination must not own an alias pin"
        );
        assert_eq!(revalidations.load(Ordering::Acquire), 1);
        assert_eq!(preparations.load(Ordering::Acquire), 1);
        assert_eq!(closes.load(Ordering::Acquire), 0);
        assert_eq!(remote.destination().as_str(), "deploy@work");
        assert_eq!(remote.remote_directory().as_str(), "~/src");
        assert_eq!(remote.local_home().path(), PathBuf::from("/Users/test"));

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.close_workspace(workspace_id, window, cx)
            });
        });
        cx.run_until_parked();
        assert_eq!(closes.load(Ordering::Acquire), 1);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager.remote_workspace_runtimes.len()),
            0
        );
    }

    #[gpui::test]
    fn completed_flow_event_should_transfer_runtime_acknowledge_and_focus_remote_workspace(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, closes, preparations, _) =
            remote_completion("work", "~/src", "/home/tester/src", true);

        emit_remote_workspace_completion(&flow, completion, cx);

        let state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.workspaces.len(),
                manager.remote_workspace_flow.is_none(),
                manager.remote_workspace_runtimes.len(),
                manager.terminal_focus_blocker(window, cx),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        });
        assert_eq!(state, (2, true, 1, None, true));
        assert_eq!(records.starts().len(), 2);
        assert_eq!(preparations.load(Ordering::Acquire), 1);
        assert_eq!(closes.load(Ordering::Acquire), 0);
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::Completed
        );
    }

    #[gpui::test]
    fn closed_remote_control_connection_should_preserve_workspace_and_block_its_pane(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, closes, _, _, _, lifecycle) = remote_completion_with_revalidation(
            "work",
            "~/src",
            "/home/tester/src",
            true,
            gpui::Task::ready(Ok(())),
        );
        emit_remote_workspace_completion(&flow, completion, cx);
        let (workspace_id, window_manager, pane_host) = manager.read_with(cx, |manager, cx| {
            let workspace = manager.workspaces.active_workspace();
            let window_manager = workspace.payload().clone();
            let pane_host = window_manager.read(cx).active_pane_host();
            (workspace.id(), window_manager, pane_host)
        });
        let starts_before_disconnect = records.starts().len();

        lifecycle
            .try_send(ControlConnectionTerminalState::Closed)
            .unwrap();
        cx.run_until_parked();
        redraw(cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::disconnected(1))
        );
        assert_eq!(closes.load(Ordering::Acquire), 1);
        assert_eq!(records.starts().len(), starts_before_disconnect);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .payload()
                .entity_id()),
            window_manager.entity_id()
        );
        assert_eq!(
            window_manager.read_with(cx, |manager, _| manager.active_pane_host().entity_id()),
            pane_host.entity_id()
        );
        assert_eq!(
            pane_host.read_with(cx, |host, cx| (
                host.remote_disconnected_generation(),
                host.focused_terminal_remote_state(cx),
                host.pane_count(),
            )),
            (Some(1), (true, true), 1)
        );
        let status_selector: &'static str = Box::leak(
            format!("workspace-row-remote-status-{}", workspace_id.get()).into_boxed_str(),
        );
        assert!(cx.debug_bounds(status_selector).is_some());

        cx.simulate_keystrokes("cmd-b");
        redraw(cx);
        assert!(
            cx.debug_bounds("workspace-chip-remote-status").is_some(),
            "hidden-sidebar identity chip must retain remote connection status"
        );
        cx.simulate_keystrokes("cmd-b");
        redraw(cx);

        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();
        assert_eq!(records.starts().len(), starts_before_disconnect);
        assert_eq!(pane_host.read_with(cx, |host, _| host.pane_count()), 1);
    }

    #[gpui::test]
    fn workspace_menu_should_enable_its_single_reconnect_action_only_after_disconnect(
        cx: &mut TestAppContext,
    ) {
        let backend = Arc::new(TestRemoteWorkspaceFlowBackend::default());
        let (manager, _, cx) = workspace_manager_with_remote_backend(backend.clone(), cx);
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, _, _, _, _, lifecycle) = remote_completion_with_revalidation(
            "work",
            "~/src",
            "/home/tester/src",
            true,
            gpui::Task::ready(Ok(())),
        );
        emit_remote_workspace_completion(&flow, completion, cx);
        let workspace_id =
            manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
        let row_selector: &'static str =
            Box::leak(format!("workspace-row-{}-active", workspace_id.get()).into_boxed_str());
        redraw(cx);

        right_click(row_selector, cx);
        assert!(
            cx.debug_bounds("workspace-menu-row-reconnect").is_some(),
            "Remote Workspace menu must contain exactly one stable Reconnect row"
        );
        click("workspace-menu-row-reconnect", cx);
        assert_eq!(backend.connect_calls.load(Ordering::Acquire), 0);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::connected(1))
        );
        cx.simulate_keystrokes("escape");

        lifecycle
            .try_send(ControlConnectionTerminalState::Closed)
            .unwrap();
        cx.run_until_parked();
        redraw(cx);
        right_click(row_selector, cx);
        click("workspace-menu-row-reconnect", cx);

        assert_eq!(backend.connect_calls.load(Ordering::Acquire), 1);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::failed(2))
        );
    }

    #[gpui::test]
    fn reconnect_should_atomically_restart_the_same_workspace_window_and_pane(
        cx: &mut TestAppContext,
    ) {
        let (new_session, new_closes, new_preparations, new_revalidations, _new_lifecycle) =
            reconnect_session("work", "/home/tester/src");
        let backend =
            TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(new_session))]);
        let (manager, records, cx) = workspace_manager_with_remote_backend(backend.clone(), cx);
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, old_closes, _, _, _, old_lifecycle) = remote_completion_with_revalidation(
            "work",
            "~/src",
            "/home/tester/src",
            true,
            gpui::Task::ready(Ok(())),
        );
        emit_remote_workspace_completion(&flow, completion, cx);
        let (workspace_id, window_manager, pane_host, starts_before_reconnect) =
            manager.read_with(cx, |manager, cx| {
                let workspace = manager.workspaces.active_workspace();
                let window_manager = workspace.payload().clone();
                let pane_host = window_manager.read(cx).active_pane_host();
                (
                    workspace.id(),
                    window_manager,
                    pane_host,
                    records.starts().len(),
                )
            });

        old_lifecycle
            .try_send(ControlConnectionTerminalState::Closed)
            .unwrap();
        cx.run_until_parked();
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.start_remote_workspace_reconnect(workspace_id, window, cx)
            });
        });
        cx.run_until_parked();

        assert_eq!(backend.connect_calls.load(Ordering::Acquire), 1);
        assert_eq!(old_closes.load(Ordering::Acquire), 1);
        assert_eq!(new_closes.load(Ordering::Acquire), 0);
        assert_eq!(new_revalidations.load(Ordering::Acquire), 1);
        assert_eq!(new_preparations.load(Ordering::Acquire), 1);
        assert_eq!(records.starts().len(), starts_before_reconnect + 1);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::connected(2))
        );
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .payload()
                .entity_id()),
            window_manager.entity_id()
        );
        assert_eq!(
            window_manager.read_with(cx, |manager, _| manager.active_pane_host().entity_id()),
            pane_host.entity_id()
        );
        assert_eq!(
            pane_host.read_with(cx, |host, cx| (
                host.remote_disconnected_generation(),
                host.focused_terminal_remote_state(cx),
                host.pane_count(),
            )),
            (None, (false, true), 1)
        );

        manager.update(cx, |manager, cx| {
            manager.handle_remote_workspace_terminal(
                workspace_id,
                1,
                ControlConnectionTerminalState::Failed,
                cx,
            )
        });
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::connected(2))
        );

        assert_eq!(new_closes.load(Ordering::Acquire), 0);
    }

    #[gpui::test]
    fn reconnect_should_consume_a_terminal_state_published_before_observer_installation(
        cx: &mut TestAppContext,
    ) {
        let (new_session, new_closes, _, _, new_lifecycle) =
            reconnect_session("work", "/home/tester/src");
        new_lifecycle
            .try_send(ControlConnectionTerminalState::Failed)
            .unwrap();
        let backend =
            TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(new_session))]);
        let (manager, _, cx) = workspace_manager_with_remote_backend(backend, cx);
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, _, _, _, _, old_lifecycle) = remote_completion_with_revalidation(
            "work",
            "~/src",
            "/home/tester/src",
            true,
            gpui::Task::ready(Ok(())),
        );
        emit_remote_workspace_completion(&flow, completion, cx);
        let workspace_id =
            manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
        old_lifecycle
            .try_send(ControlConnectionTerminalState::Closed)
            .unwrap();
        cx.run_until_parked();

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.start_remote_workspace_reconnect(workspace_id, window, cx)
            });
        });
        cx.run_until_parked();

        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::disconnected(2))
        );
        assert_eq!(new_closes.load(Ordering::Acquire), 1);
    }

    #[gpui::test]
    fn reconnect_identity_change_should_keep_final_presentation_and_show_typed_alert(
        cx: &mut TestAppContext,
    ) {
        let (new_session, new_closes, new_preparations, new_revalidations, _) =
            reconnect_session("work", "/home/tester/replaced");
        let backend =
            TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(new_session))]);
        let (manager, records, cx) = workspace_manager_with_remote_backend(backend, cx);
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, _, _, _, _, old_lifecycle) = remote_completion_with_revalidation(
            "work",
            "~/src",
            "/home/tester/src",
            true,
            gpui::Task::ready(Ok(())),
        );
        emit_remote_workspace_completion(&flow, completion, cx);
        let (workspace_id, pane_host, starts) = manager.read_with(cx, |manager, cx| {
            let workspace = manager.workspaces.active_workspace();
            (
                workspace.id(),
                workspace.payload().read(cx).active_pane_host(),
                records.starts().len(),
            )
        });
        old_lifecycle
            .try_send(ControlConnectionTerminalState::Closed)
            .unwrap();
        cx.run_until_parked();

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.start_remote_workspace_reconnect(workspace_id, window, cx)
            });
        });
        cx.run_until_parked();
        redraw(cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::disconnected(2))
        );
        assert_eq!(new_closes.load(Ordering::Acquire), 1);
        assert_eq!(new_preparations.load(Ordering::Acquire), 0);
        assert_eq!(new_revalidations.load(Ordering::Acquire), 0);
        assert_eq!(records.starts().len(), starts);
        assert_eq!(
            pane_host.read_with(cx, |host, cx| host.focused_terminal_remote_state(cx)),
            (true, true)
        );
        assert!(
            cx.debug_bounds("modal-action-remote-workspace-reconnect-error-ok")
                .is_some()
        );
        assert_eq!(
            remote_workspace_reconnect_error_content(
                &RemoteWorkspaceReconnectFailure::IdentityChanged
            ),
            Some((
                "Remote Directory Changed",
                "The selected remote path now resolves to a different directory. Reopen the Remote Project to review it.".to_owned()
            ))
        );
    }

    #[gpui::test]
    fn reconnect_directory_unavailable_should_remain_disconnected_with_actionable_alert(
        cx: &mut TestAppContext,
    ) {
        let (new_session, new_closes, _, _, _) = reconnect_session_with_provider(
            "work",
            Arc::new(TestRemoteProvider::directory_unavailable()),
        );
        let backend =
            TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(new_session))]);
        let (manager, _, cx) = workspace_manager_with_remote_backend(backend, cx);
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, _, _, _, _, old_lifecycle) = remote_completion_with_revalidation(
            "work",
            "~/src",
            "/home/tester/src",
            true,
            gpui::Task::ready(Ok(())),
        );
        emit_remote_workspace_completion(&flow, completion, cx);
        let workspace_id =
            manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
        old_lifecycle
            .try_send(ControlConnectionTerminalState::Closed)
            .unwrap();
        cx.run_until_parked();

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.start_remote_workspace_reconnect(workspace_id, window, cx)
            });
        });
        cx.run_until_parked();
        redraw(cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::disconnected(2))
        );
        assert_eq!(new_closes.load(Ordering::Acquire), 1);
        assert!(
            cx.debug_bounds("modal-action-remote-workspace-reconnect-error-ok")
                .is_some()
        );
        assert_eq!(
            remote_workspace_reconnect_error_content(
                &RemoteWorkspaceReconnectFailure::DirectoryUnavailable
            ),
            Some((
                "Remote Directory Unavailable",
                "SpaceTerm can’t access the selected remote directory. Check its permissions or reopen the Remote Project.".to_owned()
            ))
        );
    }

    #[test]
    fn reconnect_connection_detail_should_reach_the_transient_alert_content() {
        let detail =
            TransientSshErrorOutput::from_untrusted_bytes(b"ssh: Permission denied (publickey).")
                .unwrap();
        let failure = RemoteWorkspaceReconnectFailure::ConnectionFailed {
            detail: Some(detail),
        };
        assert_eq!(
            format!("{failure:?}"),
            "ConnectionFailed { detail: Some(TransientSshErrorOutput(<redacted>)) }"
        );
        assert_eq!(
            remote_workspace_reconnect_error_content(&failure),
            Some((
                "Couldn’t Reconnect",
                "SpaceTerm couldn’t restore the remote connection. OpenSSH reported:\n\nssh: Permission denied (publickey)."
                    .to_owned(),
            ))
        );
    }

    #[gpui::test]
    fn hierarchy_change_between_restart_prepare_and_commit_should_fail_with_typed_alert(
        cx: &mut TestAppContext,
    ) {
        let (new_session, new_closes, _, _, _) = reconnect_session("work", "/home/tester/src");
        let backend =
            TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(new_session))]);
        let (manager, records, cx) = workspace_manager_with_remote_backend(backend, cx);
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, _, _, _, _, lifecycle) = remote_completion_with_revalidation(
            "work",
            "~/src",
            "/home/tester/src",
            true,
            gpui::Task::ready(Ok(())),
        );
        emit_remote_workspace_completion(&flow, completion, cx);
        redraw(cx);
        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();
        let (workspace_id, window_manager, starts) = manager.read_with(cx, |manager, _| {
            let workspace = manager.workspaces.active_workspace();
            (
                workspace.id(),
                workspace.payload().clone(),
                records.starts().len(),
            )
        });
        assert_eq!(
            window_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
            (1, 2)
        );

        lifecycle
            .try_send(ControlConnectionTerminalState::Closed)
            .unwrap();
        cx.run_until_parked();
        manager.update(cx, |manager, _| {
            manager.close_focused_pane_before_reconnect_commit = true;
        });
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.start_remote_workspace_reconnect(workspace_id, window, cx)
            });
        });
        cx.run_until_parked();
        redraw(cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::failed(2))
        );
        assert_eq!(new_closes.load(Ordering::Acquire), 1);
        assert_eq!(records.starts().len(), starts);
        assert_eq!(
            window_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
            (1, 1)
        );
        assert!(
            cx.debug_bounds("modal-action-remote-workspace-reconnect-error-ok")
                .is_some()
        );
    }

    #[gpui::test]
    fn reconnect_cancel_should_be_single_flight_and_close_late_success_without_resurrection(
        cx: &mut TestAppContext,
    ) {
        let (late_session, late_closes, _, _, _) = reconnect_session("work", "/home/tester/src");
        let (sender, receiver) = async_channel::bounded(1);
        let pending = cx.update(|cx| {
            cx.background_executor()
                .spawn(async move { receiver.recv().await.unwrap() })
        });
        let backend = TestRemoteWorkspaceFlowBackend::with_connections([pending]);
        let (manager, _, cx) = workspace_manager_with_remote_backend(backend.clone(), cx);
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, _, _, _, _, lifecycle) = remote_completion_with_revalidation(
            "work",
            "~/src",
            "/home/tester/src",
            true,
            gpui::Task::ready(Ok(())),
        );
        emit_remote_workspace_completion(&flow, completion, cx);
        let workspace_id =
            manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
        lifecycle
            .try_send(ControlConnectionTerminalState::Closed)
            .unwrap();
        cx.run_until_parked();

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.start_remote_workspace_reconnect(workspace_id, window, cx);
                manager.start_remote_workspace_reconnect(workspace_id, window, cx);
            });
        });
        cx.run_until_parked();
        redraw(cx);
        assert_eq!(backend.connect_calls.load(Ordering::Acquire), 1);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::reconnecting(2))
        );

        click("modal-action-remote-workspace-reconnect-cancel", cx);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::disconnected(2))
        );
        assert!(manager.read_with(cx, |manager, _| {
            manager.remote_workspace_reconnect.is_none()
        }));

        assert!(sender.try_send(Ok(late_session)).is_err());
        cx.run_until_parked();
        assert_eq!(late_closes.load(Ordering::Acquire), 1);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::disconnected(2))
        );
    }

    #[gpui::test]
    fn cancelling_while_account_discovery_is_blocked_should_close_connected_session_immediately(
        cx: &mut TestAppContext,
    ) {
        let (account_sender, account_receiver) = async_channel::bounded(1);
        let account_task = cx.update(|cx| {
            cx.background_executor()
                .spawn(async move { account_receiver.recv().await.unwrap() })
        });
        let account_calls = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn RemoteWorkspaceProvider> = Arc::new(BlockingRemoteProvider {
            account: Mutex::new(Some(account_task)),
            identity: Mutex::new(Some(gpui::Task::ready(Ok(
                crate::domain::RemoteDirectoryIdentity::new("/home/tester/src".to_owned()).unwrap(),
            )))),
            account_calls: Arc::clone(&account_calls),
            identity_calls: Arc::new(AtomicUsize::new(0)),
        });
        let (session, closes, _, _, _) = reconnect_session_with_provider("work", provider);
        let backend =
            TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(session))]);
        let (manager, _, cx) = workspace_manager_with_remote_backend(backend, cx);
        let workspace_id = create_disconnected_remote_workspace(&manager, cx);

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.start_remote_workspace_reconnect(workspace_id, window, cx)
            });
        });
        cx.run_until_parked();
        redraw(cx);
        assert_eq!(account_calls.load(Ordering::Acquire), 1);
        assert_eq!(closes.load(Ordering::Acquire), 0);

        click("modal-action-remote-workspace-reconnect-cancel", cx);

        assert_eq!(closes.load(Ordering::Acquire), 1);
        assert!(account_sender.is_closed());
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::disconnected(2))
        );
    }

    #[gpui::test]
    fn closing_workspace_while_identity_validation_is_blocked_should_close_session_immediately(
        cx: &mut TestAppContext,
    ) {
        let (identity_sender, identity_receiver) = async_channel::bounded(1);
        let identity_task = cx.update(|cx| {
            cx.background_executor()
                .spawn(async move { identity_receiver.recv().await.unwrap() })
        });
        let identity_calls = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn RemoteWorkspaceProvider> = Arc::new(BlockingRemoteProvider {
            account: Mutex::new(Some(gpui::Task::ready(Ok(test_remote_account())))),
            identity: Mutex::new(Some(identity_task)),
            account_calls: Arc::new(AtomicUsize::new(0)),
            identity_calls: Arc::clone(&identity_calls),
        });
        let (session, closes, _, _, _) = reconnect_session_with_provider("work", provider);
        let backend =
            TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(session))]);
        let (manager, _, cx) = workspace_manager_with_remote_backend(backend, cx);
        let workspace_id = create_disconnected_remote_workspace(&manager, cx);
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.start_remote_workspace_reconnect(workspace_id, window, cx)
            });
        });
        cx.run_until_parked();
        assert_eq!(identity_calls.load(Ordering::Acquire), 1);
        assert_eq!(closes.load(Ordering::Acquire), 0);

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.close_workspace(workspace_id, window, cx)
            });
        });
        cx.run_until_parked();

        assert_eq!(closes.load(Ordering::Acquire), 1);
        assert!(identity_sender.is_closed());
        assert!(manager.read_with(cx, |manager, _| {
            manager.workspaces.workspace(workspace_id).is_none()
        }));
    }

    #[gpui::test]
    fn application_cleanup_while_restart_preparation_is_blocked_should_abort_and_close_session(
        cx: &mut TestAppContext,
    ) {
        let (revalidation_sender, revalidation_receiver) = async_channel::bounded(1);
        let revalidation_task = cx.update(|cx| {
            cx.background_executor()
                .spawn(async move { revalidation_receiver.recv().await.unwrap() })
        });
        let provider: Arc<dyn RemoteWorkspaceProvider> = Arc::new(TestRemoteProvider::connected(
            crate::domain::RemoteDirectoryIdentity::new("/home/tester/src".to_owned()).unwrap(),
        ));
        let (session, closes, _, revalidations, _) =
            reconnect_session_with_provider_and_revalidation(
                "work",
                provider,
                VecDeque::from([revalidation_task]),
            );
        let backend =
            TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(session))]);
        let (manager, _, cx) = workspace_manager_with_remote_backend(backend, cx);
        let workspace_id = create_disconnected_remote_workspace(&manager, cx);
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.start_remote_workspace_reconnect(workspace_id, window, cx)
            });
        });
        cx.run_until_parked();
        assert_eq!(revalidations.load(Ordering::Acquire), 1);
        assert_eq!(closes.load(Ordering::Acquire), 0);

        manager.update(cx, |manager, _| manager.close_remote_runtimes());
        cx.run_until_parked();

        assert_eq!(closes.load(Ordering::Acquire), 1);
        assert!(revalidation_sender.is_closed());
        assert!(manager.read_with(cx, |manager, _| {
            manager.remote_workspace_reconnect.is_none()
        }));
        assert_ne!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::connected(2))
        );
    }

    #[gpui::test]
    fn authentication_cancelled_reconnect_should_return_to_disconnected_without_error_alert(
        cx: &mut TestAppContext,
    ) {
        let backend = TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Err(
            RemoteWorkspaceFlowBackendError::AuthenticationCancelled,
        ))]);
        let (manager, _, cx) = workspace_manager_with_remote_backend(backend, cx);
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, _, _, _, _, lifecycle) = remote_completion_with_revalidation(
            "work",
            "~/src",
            "/home/tester/src",
            true,
            gpui::Task::ready(Ok(())),
        );
        emit_remote_workspace_completion(&flow, completion, cx);
        let workspace_id =
            manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
        lifecycle
            .try_send(ControlConnectionTerminalState::Closed)
            .unwrap();
        cx.run_until_parked();

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.start_remote_workspace_reconnect(workspace_id, window, cx)
            });
        });
        cx.run_until_parked();
        redraw(cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::disconnected(2))
        );
        assert!(
            cx.debug_bounds("modal-action-remote-workspace-reconnect-error-ok")
                .is_none()
        );
    }

    #[gpui::test]
    fn bounded_connection_detail_should_survive_reconnect_into_the_failure_alert(
        cx: &mut TestAppContext,
    ) {
        let detail = crate::ssh::process::TransientSshErrorOutput::from_untrusted_bytes(
            b"ssh: Permission denied (publickey).",
        )
        .unwrap();
        let backend = TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Err(
            RemoteWorkspaceFlowBackendError::ConnectionFailedWithDetail(detail),
        ))]);
        let (manager, _, cx) = workspace_manager_with_remote_backend(backend, cx);
        let workspace_id = create_disconnected_remote_workspace(&manager, cx);

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.start_remote_workspace_reconnect(workspace_id, window, cx)
            });
        });
        cx.run_until_parked();
        redraw(cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::failed(2))
        );
        assert!(
            cx.debug_bounds("modal-action-remote-workspace-reconnect-error-ok")
                .is_some()
        );
    }

    #[gpui::test]
    fn closing_workspace_during_reconnect_should_close_late_session_and_prevent_resurrection(
        cx: &mut TestAppContext,
    ) {
        let (late_session, late_closes, _, _, _) = reconnect_session("work", "/home/tester/src");
        let (sender, receiver) = async_channel::bounded(1);
        let pending = cx.update(|cx| {
            cx.background_executor()
                .spawn(async move { receiver.recv().await.unwrap() })
        });
        let backend = TestRemoteWorkspaceFlowBackend::with_connections([pending]);
        let (manager, _, cx) = workspace_manager_with_remote_backend(backend, cx);
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, _, _, _, _, lifecycle) = remote_completion_with_revalidation(
            "work",
            "~/src",
            "/home/tester/src",
            true,
            gpui::Task::ready(Ok(())),
        );
        emit_remote_workspace_completion(&flow, completion, cx);
        let workspace_id =
            manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
        lifecycle
            .try_send(ControlConnectionTerminalState::Closed)
            .unwrap();
        cx.run_until_parked();
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.start_remote_workspace_reconnect(workspace_id, window, cx);
                manager.close_workspace(workspace_id, window, cx);
            });
        });
        cx.run_until_parked();
        assert!(manager.read_with(cx, |manager, _| {
            manager.workspaces.workspace(workspace_id).is_none()
        }));

        assert!(sender.try_send(Ok(late_session)).is_err());
        cx.run_until_parked();
        assert_eq!(late_closes.load(Ordering::Acquire), 1);
        assert!(manager.read_with(cx, |manager, _| {
            manager.workspaces.workspace(workspace_id).is_none()
        }));
    }

    #[gpui::test]
    fn remote_child_identity_failure_should_show_alert_without_changing_connection_or_focus(
        cx: &mut TestAppContext,
    ) {
        let (manager, _, cx) = workspace_manager(cx);
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, _, _, _) = remote_completion("work", "~/src", "/home/tester/src", true);
        emit_remote_workspace_completion(&flow, completion, cx);
        let (workspace_id, window_manager) = manager.read_with(cx, |manager, _| {
            let workspace = manager.workspaces.active_workspace();
            (workspace.id(), workspace.payload().clone())
        });

        window_manager.update(cx, |_, cx| {
            cx.emit(RemoteChildLaunchUnavailable::IdentityChanged)
        });
        cx.run_until_parked();
        redraw(cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::connected(1))
        );
        assert!(!cx.update(|window, cx| {
            window_manager
                .read(cx)
                .focused_terminal_has_input_focus(window, cx)
        }));
        assert!(
            cx.debug_bounds("modal-action-remote-workspace-reconnect-error-ok")
                .is_some()
        );
        click("modal-action-remote-workspace-reconnect-error-ok", cx);
        assert!(cx.update(|window, cx| {
            window_manager
                .read(cx)
                .focused_terminal_is_focused(window, cx)
        }));
    }

    #[gpui::test]
    fn failed_flow_activation_should_return_intact_connection_to_picker_retry_state(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, closes, preparations, _) =
            remote_completion("work", "~/src", "/home/tester/src", false);

        emit_remote_workspace_completion(&flow, completion, cx);

        let state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.workspaces.len(),
                manager.remote_workspace_runtimes.len(),
                manager.remote_workspace_flow.is_some(),
                flow.read(cx).stage(),
                manager.terminal_focus_blocker(window, cx),
            )
        });
        assert_eq!(
            state,
            (
                1,
                0,
                true,
                RemoteWorkspaceFlowStage::DirectorySelection,
                Some(TerminalFocusBlocker::CommandPalette),
            )
        );
        assert_eq!(records.starts().len(), 1);
        assert_eq!(preparations.load(Ordering::Acquire), 0);
        assert_eq!(closes.load(Ordering::Acquire), 0);
    }

    #[gpui::test]
    fn remote_revalidation_failures_should_return_intact_completion_without_mutation(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);

        for error in [
            crate::terminal::RemoteChannelRevalidationError::ConnectionUnavailable,
            crate::terminal::RemoteChannelRevalidationError::DirectoryUnavailable,
            crate::terminal::RemoteChannelRevalidationError::IdentityChanged,
        ] {
            let flow = open_remote_workspace_flow(&manager, cx);
            let (completion, closes, preparations, revalidations, _, _) =
                remote_completion_with_revalidation(
                    "work",
                    "~/src",
                    "/home/tester/src",
                    true,
                    gpui::Task::ready(Err(error)),
                );

            emit_remote_workspace_completion(&flow, completion, cx);

            assert_eq!(
                manager.read_with(cx, |manager, _| (
                    manager.workspaces.len(),
                    manager.remote_workspace_runtimes.len(),
                )),
                (1, 0)
            );
            assert_eq!(records.starts().len(), 1);
            assert_eq!(revalidations.load(Ordering::Acquire), 1);
            assert_eq!(preparations.load(Ordering::Acquire), 0);
            assert_eq!(closes.load(Ordering::Acquire), 0);
            assert_eq!(
                flow.read_with(cx, |flow, _| flow.stage()),
                RemoteWorkspaceFlowStage::DirectorySelection
            );

            cx.update(|window, cx| {
                flow.update(cx, |flow, cx| flow.cancel_for_test(window, cx));
            });
            cx.run_until_parked();
            assert_eq!(closes.load(Ordering::Acquire), 1);
        }
    }

    #[gpui::test]
    fn cancelled_initial_revalidation_should_close_immediately_without_late_resurrection(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let (sender, receiver) = async_channel::bounded(1);
        let delayed_revalidation = cx.update(|_, cx| {
            cx.background_executor()
                .spawn(async move { receiver.recv().await.unwrap() })
        });
        let (stale, stale_closes, stale_preparations, stale_revalidations, _, _) =
            remote_completion_with_revalidation(
                "work",
                "~/stale",
                "/home/tester/stale",
                true,
                delayed_revalidation,
            );
        let first_flow = open_remote_workspace_flow(&manager, cx);
        emit_remote_workspace_completion(&first_flow, stale, cx);
        assert_eq!(stale_revalidations.load(Ordering::Acquire), 1);

        cx.update(|window, cx| {
            first_flow.update(cx, |flow, cx| flow.cancel_for_test(window, cx));
        });
        cx.run_until_parked();
        assert_eq!(
            manager.read_with(cx, |manager, _| manager.workspaces.len()),
            1
        );
        assert_eq!(stale_preparations.load(Ordering::Acquire), 0);
        assert_eq!(stale_closes.load(Ordering::Acquire), 1);
        assert!(sender.try_send(Ok(())).is_err());

        let (current, current_closes, current_preparations, _) =
            remote_completion("work", "~/current", "/home/tester/current", true);
        let current_flow = open_remote_workspace_flow(&manager, cx);
        assert_eq!(current_flow, first_flow);
        emit_remote_workspace_completion(&current_flow, current, cx);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager.workspaces.len()),
            2
        );
        assert_eq!(current_preparations.load(Ordering::Acquire), 1);
        assert_eq!(current_closes.load(Ordering::Acquire), 0);

        assert_eq!(
            manager.read_with(cx, |manager, _| manager.workspaces.len()),
            2
        );
        assert_eq!(records.starts().len(), 2);
        assert_eq!(stale_preparations.load(Ordering::Acquire), 0);
        assert_eq!(current_closes.load(Ordering::Acquire), 0);
    }

    #[gpui::test]
    fn equivalent_remote_completion_should_activate_existing_and_close_redundant_session(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let (first, first_closes, first_preparations, _) =
            remote_completion("work", "~/src", "/home/tester/src", true);
        let (duplicate, duplicate_closes, duplicate_preparations, _) =
            remote_completion("work", "/home/tester/src", "/home/tester/src", true);

        let first_flow = open_remote_workspace_flow(&manager, cx);
        emit_remote_workspace_completion(&first_flow, first, cx);
        let duplicate_flow = open_remote_workspace_flow(&manager, cx);
        emit_remote_workspace_completion(&duplicate_flow, duplicate, cx);

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.len(),
                manager
                    .workspaces
                    .active_workspace()
                    .remote_workspace_directory()
                    .unwrap()
                    .as_str()
                    .to_owned(),
                manager.remote_workspace_runtimes.len(),
            )
        });
        assert_eq!(state, (2, "~/src".to_owned(), 1));
        assert_eq!(records.starts().len(), 2);
        assert_eq!(first_preparations.load(Ordering::Acquire), 1);
        assert_eq!(duplicate_preparations.load(Ordering::Acquire), 0);
        assert_eq!(first_closes.load(Ordering::Acquire), 0);
        assert_eq!(duplicate_closes.load(Ordering::Acquire), 1);
    }

    #[gpui::test]
    fn equivalent_workspace_created_during_revalidation_should_win_without_a_duplicate_launch(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let (sender, receiver) = async_channel::bounded(1);
        let pending_revalidation = cx.update(|_, cx| {
            cx.background_executor()
                .spawn(async move { receiver.recv().await.unwrap() })
        });
        let (pending, pending_closes, pending_preparations, pending_revalidations, _, _) =
            remote_completion_with_revalidation(
                "work",
                "~/src",
                "/home/tester/src",
                true,
                pending_revalidation,
            );
        let (winner, winner_closes, winner_preparations, _) =
            remote_completion("work", "/home/tester/src", "/home/tester/src", true);
        let flow = open_remote_workspace_flow(&manager, cx);
        emit_remote_workspace_completion(&flow, pending, cx);
        assert_eq!(pending_revalidations.load(Ordering::Acquire), 1);

        install_remote_completion_directly(&manager, winner, cx);
        assert_eq!(winner_preparations.load(Ordering::Acquire), 1);
        sender.try_send(Ok(())).unwrap();
        cx.run_until_parked();

        assert_eq!(
            manager.read_with(cx, |manager, _| (
                manager.workspaces.len(),
                manager.remote_workspace_runtimes.len(),
                manager
                    .workspaces
                    .active_workspace()
                    .remote_workspace_directory()
                    .map(|directory| directory.as_str().to_owned()),
            )),
            (2, 1, Some("/home/tester/src".to_owned()))
        );
        assert_eq!(records.starts().len(), 2);
        assert_eq!(pending_preparations.load(Ordering::Acquire), 0);
        assert_eq!(pending_closes.load(Ordering::Acquire), 1);
        assert_eq!(winner_closes.load(Ordering::Acquire), 0);
        assert!(manager.read_with(cx, |manager, _| manager.remote_workspace_flow.is_none()));
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::Completed
        );
    }

    #[gpui::test]
    fn remote_workspace_close_should_release_active_alias_after_runtime_cleanup(
        cx: &mut TestAppContext,
    ) {
        let (manager, _, cx) = workspace_manager(cx);
        let (completion, aliases, alias, closes, _) = remote_completion_with_active_alias();
        assert!(aliases.is_active(&alias));

        let flow = open_remote_workspace_flow(&manager, cx);
        emit_remote_workspace_completion(&flow, completion, cx);
        let workspace_id =
            manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
        assert!(aliases.is_active(&alias));
        assert_eq!(closes.load(Ordering::Acquire), 0);
        assert!(manager.read_with(cx, |manager, _| {
            manager
                .remote_workspace_runtimes
                .get(&workspace_id)
                .is_some_and(|runtime| runtime.alias_pin.is_some())
        }));

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.close_workspace(workspace_id, window, cx)
            });
        });
        cx.run_until_parked();

        assert_eq!(closes.load(Ordering::Acquire), 1);
        assert!(!aliases.is_active(&alias));
    }

    #[gpui::test]
    fn managed_alias_should_remain_immutable_after_master_loss_and_during_reconnect(
        cx: &mut TestAppContext,
    ) {
        let (connection_sender, connection_receiver) = async_channel::bounded(1);
        let connection = cx.update(|cx| {
            cx.background_executor()
                .spawn(async move { connection_receiver.recv().await.unwrap() })
        });
        let backend = TestRemoteWorkspaceFlowBackend::with_connections([connection]);
        let (manager, _, cx) = workspace_manager_with_remote_backend(backend, cx);
        let (completion, aliases, alias, closes, lifecycle) = remote_completion_with_active_alias();
        let flow = open_remote_workspace_flow(&manager, cx);
        emit_remote_workspace_completion(&flow, completion, cx);
        let workspace_id =
            manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());

        assert!(aliases.begin_mutation([alias.clone()]).is_err());
        lifecycle
            .try_send(ControlConnectionTerminalState::Closed)
            .unwrap();
        cx.run_until_parked();

        assert_eq!(closes.load(Ordering::Acquire), 1);
        assert!(aliases.is_active(&alias));
        assert!(aliases.begin_mutation([alias.clone()]).is_err());

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.start_remote_workspace_reconnect(workspace_id, window, cx)
            });
        });
        cx.run_until_parked();
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .workspace(workspace_id)
                .unwrap()
                .remote_connection_state()),
            Some(RemoteConnectionState::reconnecting(2))
        );
        assert!(aliases.begin_mutation([alias.clone()]).is_err());

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.close_workspace(workspace_id, window, cx)
            });
        });
        cx.run_until_parked();

        assert!(connection_sender.is_closed());
        assert!(!aliases.is_active(&alias));
        assert!(aliases.begin_mutation([alias]).is_ok());
    }

    #[gpui::test]
    fn failed_workspace_alias_pin_should_return_activation_without_leaking_authority(
        cx: &mut TestAppContext,
    ) {
        let (manager, _, cx) = workspace_manager(cx);
        let (completion, aliases, alias, closes, _) =
            remote_completion_with_active_alias_pin_failure(true);
        let flow = open_remote_workspace_flow(&manager, cx);

        emit_remote_workspace_completion(&flow, completion, cx);

        assert_eq!(
            manager.read_with(cx, |manager, _| (
                manager.workspaces.len(),
                manager.remote_workspace_runtimes.len(),
                manager.remote_workspace_flow.is_some(),
            )),
            (1, 0, true)
        );
        assert_eq!(closes.load(Ordering::Acquire), 0);
        assert!(aliases.is_active(&alias));
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::DirectorySelection
        );

        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| flow.cancel_for_test(window, cx));
        });
        cx.run_until_parked();

        assert_eq!(closes.load(Ordering::Acquire), 1);
        assert!(!aliases.is_active(&alias));
        assert!(aliases.begin_mutation([alias]).is_ok());
    }

    #[gpui::test]
    fn application_teardown_hook_should_close_remote_runtime_exactly_once(cx: &mut TestAppContext) {
        let (manager, _, cx) = workspace_manager(cx);
        let (completion, closes, _, _) =
            remote_completion("work", "~/src", "/home/tester/src", true);
        let flow = open_remote_workspace_flow(&manager, cx);
        emit_remote_workspace_completion(&flow, completion, cx);

        manager.update(cx, |manager, _| manager.close_remote_runtimes());
        manager.update(cx, |manager, _| manager.close_remote_runtimes());

        assert_eq!(closes.load(Ordering::Acquire), 1);
    }

    #[gpui::test]
    fn unavailable_initial_remote_channel_should_return_intact_completion_for_retry(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, closes, preparations, availability) =
            remote_completion("work", "~/src", "/home/tester/src", false);

        emit_remote_workspace_completion(&flow, completion, cx);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager.workspaces.len()),
            1
        );
        assert_eq!(records.starts().len(), 1);
        assert_eq!(preparations.load(Ordering::Acquire), 0);
        assert_eq!(closes.load(Ordering::Acquire), 0);

        availability.store(true, Ordering::Release);
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::DirectorySelection
        );
    }

    #[gpui::test]
    fn choosing_scratch_should_create_a_workspace_and_close_the_panel(cx: &mut TestAppContext) {
        let (manager, _, cx) = workspace_manager(cx);

        open_new_workspace_panel(cx);
        click("new-workspace-source-scratch", cx);

        assert_eq!(
            cx.update(|window, cx| {
                let manager = manager.read(cx);
                (
                    manager.workspaces.len(),
                    manager.new_workspace_panel.read(cx).is_open(),
                    manager.terminal_focus_blocker(window, cx),
                )
            }),
            (2, false, None)
        );
    }

    #[gpui::test]
    fn escape_should_step_back_from_the_picker_to_the_panel_that_opened_it(
        cx: &mut TestAppContext,
    ) {
        let (manager, _, cx) = workspace_manager(cx);

        open_new_workspace_panel(cx);
        click("new-workspace-source-local-project", cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert_eq!(
            cx.update(|window, cx| {
                let manager = manager.read(cx);
                (
                    manager.workspace_picker.read(cx).is_open(),
                    manager.new_workspace_panel.read(cx).is_open(),
                    manager.terminal_focus_blocker(window, cx),
                )
            }),
            (false, true, Some(TerminalFocusBlocker::CommandPalette))
        );
    }

    #[gpui::test]
    fn escape_should_close_a_picker_that_no_panel_opened(cx: &mut TestAppContext) {
        let (manager, _, cx) = workspace_manager(cx);

        open_workspace_picker(cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert_eq!(
            cx.update(|window, cx| {
                let manager = manager.read(cx);
                (
                    manager.workspace_picker.read(cx).is_open(),
                    manager.new_workspace_panel.read(cx).is_open(),
                    manager.terminal_focus_blocker(window, cx),
                )
            }),
            (false, false, None)
        );
    }

    #[gpui::test]
    fn workspace_picker_should_block_parent_shortcuts_and_keep_path_focus(cx: &mut TestAppContext) {
        let (manager, records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-n");
        open_workspace_picker(cx);
        let baseline = manager.read_with(cx, |manager, cx| {
            (
                manager.workspaces.len(),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .aggregate_counts(cx),
                records.starts().len(),
            )
        });

        cx.simulate_keystrokes("cmd-n");
        assert_eq!(
            manager.read_with(cx, |manager, _| manager.workspaces.len()),
            baseline.0
        );

        cx.simulate_keystrokes("cmd-p");
        let focus_state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.workspace_search.read(cx).blocks_terminal_input(),
                manager
                    .workspace_picker
                    .read(cx)
                    .path_input_is_focused(window, cx),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        });
        assert_eq!(focus_state, (false, true, false));

        cx.simulate_keystrokes("cmd-t");
        cx.simulate_keystrokes("cmd-w");
        let hierarchy = manager.read_with(cx, |manager, cx| {
            (
                manager.workspaces.len(),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .aggregate_counts(cx),
                records.starts().len(),
            )
        });
        assert_eq!(hierarchy, baseline);
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

        choose_with_finder_fallback(cx);
        choose_with_finder_fallback(cx);
        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-d");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            let workspace = manager.workspaces.active_workspace();
            (
                manager.workspaces.len(),
                workspace.working_directory().unwrap().to_path_buf(),
                workspace.kind().clone(),
            )
        });
        assert_eq!((state.0, state.1), (2, project.clone()));
        assert!(matches!(state.2, WorkspaceKind::LocalProject { .. }));
        assert!(records.starts()[1..].iter().all(|start| {
            start
                .local_working_directory()
                .is_some_and(|directory| directory.path() == project)
        }));
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
        choose_with_finder_fallback(cx);
        assert_eq!(records.starts().len(), 2);
        assert!(!manager.read_with(cx, |manager, cx| {
            manager.workspace_picker.read(cx).is_open()
        }));

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

        choose_with_finder_fallback(cx);

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
                manager.workspace_search.read(cx).blocks_terminal_input(),
                manager.terminal_focus_blocker(window, cx),
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
                manager.terminal_focus_blocker(window, cx),
            )
        });
        assert_eq!(
            restored,
            (true, Some(TerminalFocusBlocker::Sidebar)),
            "closing the replacement palette must not restore the invisible menu focus owner"
        );
    }

    #[gpui::test]
    fn workspace_search_from_inline_rename_should_restore_sidebar_focus(cx: &mut TestAppContext) {
        let (manager, _, cx) = workspace_manager(cx);
        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-rename", cx);
        assert!(cx.update(|window, cx| manager.read(cx).rename_is_focused(window)));

        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.open_workspace_search(window, cx);
            });
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        let restored = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.rename.is_none(),
                manager.sidebar_focus.is_focused(window),
                manager.terminal_focus_blocker(window, cx),
            )
        });
        assert_eq!(restored, (true, true, Some(TerminalFocusBlocker::Sidebar)));
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
                manager.workspace_search.read(cx).blocks_terminal_input(),
                manager.terminal_focus_blocker(window, cx),
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
    fn open_workspace_search_should_remove_a_workspace_after_its_final_session_exits(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let inactive_sender = records
            .event_sender(1)
            .expect("the initial Workspace terminal session must have started");
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        manager.update(cx, |manager, cx| {
            manager
                .workspaces
                .rename_workspace(WorkspaceId::new(1), "Alpha Workspace".to_owned())
                .expect("the inactive Workspace must remain owned");
            cx.notify();
        });

        click("search-workspaces-button", cx);
        cx.simulate_keystrokes("a l p h a");
        cx.run_until_parked();
        assert!(cx.debug_bounds("workspace-search-result-1").is_some());

        inactive_sender
            .try_send(SessionEvent::Exited(SessionExit::Success))
            .expect("the inactive shell exit must be delivered");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let state = manager.read_with(cx, |manager, _| {
            (
                manager.workspaces.workspace(WorkspaceId::new(1)).is_none(),
                manager.workspaces.active_workspace_id(),
            )
        });
        assert_eq!(state, (true, WorkspaceId::new(2)));
        assert!(cx.debug_bounds("command-palette-panel").is_some());
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
                manager.workspace_search.read(cx).blocks_terminal_input(),
                manager.terminal_focus_blocker(window, cx),
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
            search.size.height + px(SIDEBAR_HEADER_ACTION_PADDING * 2.0),
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
        assert_eq!(
            search.origin.x + search.size.width,
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

    fn redraw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
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
    fn workspace_chrome_should_forward_threshold_crossing_and_double_activation_to_platform_policy(
        cx: &mut TestAppContext,
    ) {
        let (_manager, platform, cx) =
            workspace_manager_with_operating_system_window_drag_platform(cx);
        let chrome = cx
            .debug_bounds("workspace-top-chrome-drag-region")
            .expect("Workspace drag region must be rendered")
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
    fn workspace_top_chrome_should_restore_after_release_outside_its_hitbox(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
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
        let services_blocked = cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.native_service_status(window, cx))
        });
        manager.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert!(!services_blocked.capabilities.return_text);
        assert!(cx.update(|window, cx| {
            !manager
                .read(cx)
                .workspaces
                .active_workspace()
                .payload()
                .read(cx)
                .focused_terminal_has_input_focus(window, cx)
        }));

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
            .debug_bounds("workspace-sidebar-resize-handle-divider")
            .expect("the shared sidebar divider was not rendered");
        let top_chrome_divider = cx
            .debug_bounds("workspace-top-chrome-resize-handle-divider")
            .expect("the shared fixed top-chrome divider was not rendered");
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
                sidebar_divider.center().x,
                sidebar_divider.origin.y,
                sidebar_divider.size,
                top_chrome_divider.center().x,
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
    fn shared_sidebar_handle_should_clamp_to_the_application_maximum(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);
        let root = cx
            .debug_bounds("workspace-manager")
            .expect("the Workspace manager was not rendered");
        let maximum = cx.update(|window, _| {
            (window.bounds().size.width - px(TERMINAL_CONTENT_MINIMUM_WIDTH))
                .min(px(SIDEBAR_MAXIMUM_WIDTH))
                .max(px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH))
        });

        drag_to(
            "workspace-sidebar-resize-handle",
            root.origin.x + px(10_000.0),
            cx,
        );

        assert_eq!(
            manager.read_with(cx, |manager, _| manager.sidebar_width),
            maximum
        );
    }

    #[gpui::test]
    fn sidebar_resize_interaction_should_block_then_restore_terminal_focus(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = workspace_manager(cx);
        let start = cx
            .debug_bounds("workspace-sidebar-resize-handle-hitbox")
            .expect("the shared sidebar handle was rendered")
            .center();

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        let active = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.sidebar_resize_interaction,
                manager.terminal_focus_blocker(window, cx),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        });
        assert_eq!(
            active,
            (true, Some(TerminalFocusBlocker::SidebarResize), false)
        );

        cx.simulate_mouse_up(start, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let finished = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.sidebar_resize_interaction,
                manager.terminal_focus_blocker(window, cx),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        });
        assert_eq!(finished, (false, None, true));
    }

    #[gpui::test]
    fn double_clicking_sidebar_handle_should_request_default_width(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.resize_sidebar(px(320.0), window, cx);
            });
        });
        cx.run_until_parked();
        let position = cx
            .debug_bounds("workspace-top-chrome-resize-handle-hitbox")
            .expect("the persistent shared sidebar handle was rendered")
            .center();

        cx.simulate_event(gpui::MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::none(),
            click_count: 2,
            first_mouse: false,
        });
        cx.simulate_event(gpui::MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::none(),
            click_count: 2,
        });
        cx.run_until_parked();

        let state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.sidebar_width,
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        });
        assert_eq!(state, (px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH), true));
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
    fn collapsed_sidebar_resize_should_not_leak_held_pointer_events_to_terminal_session(
        cx: &mut TestAppContext,
    ) {
        let (manager, records, cx) = workspace_manager(cx);
        let root = cx
            .debug_bounds("workspace-manager")
            .expect("the Workspace manager was rendered");
        let handle = cx
            .debug_bounds("workspace-sidebar-resize-handle-hitbox")
            .expect("the sidebar resize handle was rendered");
        let start = handle.center();
        let collapse = point(
            root.origin.x + px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH - 20.0),
            start.y,
        );

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            point(start.x - px(12.0), start.y),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_move(collapse, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let sidebar_visible = manager.read_with(cx, |manager, _| manager.sidebar_visible);
        assert!(
            !sidebar_visible,
            "the sidebar resize did not collapse the body"
        );
        let terminal = cx
            .debug_bounds("window-manager-content")
            .expect("the Terminal Session content was rendered")
            .center();
        cx.simulate_mouse_move(terminal, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(terminal, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        assert_eq!(records.pointer_count(), 0);
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
    fn sidebar_buttons_should_toggle_sidebar_and_present_the_new_workspace_panel(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = workspace_manager(cx);

        click("toggle-sidebar-button", cx);
        assert!(!manager.read_with(cx, |manager, _| manager.sidebar_visible));

        cx.simulate_keystrokes("cmd-b");
        cx.run_until_parked();
        click("new-workspace-button", cx);

        assert_eq!(
            manager.read_with(cx, |manager, cx| {
                (
                    manager.workspaces.len(),
                    manager.new_workspace_panel.read(cx).is_open(),
                )
            }),
            (1, true)
        );
    }

    #[gpui::test]
    fn only_a_pinned_workspace_row_should_carry_a_pin(cx: &mut TestAppContext) {
        let project = temporary_directory("pinned-project");
        fs::create_dir_all(&project).unwrap();
        let (manager, _, cx) = workspace_manager_with_picker([Ok(Some(project))], cx);

        choose_with_finder_fallback(cx);

        let (scratch_id, project_id) = manager.read_with(cx, |manager, _| {
            let mut scratch = None;
            let mut project = None;
            for workspace in manager.workspaces.iter() {
                match workspace.kind() {
                    WorkspaceKind::Scratch { .. } => scratch = Some(workspace.id().get()),
                    WorkspaceKind::LocalProject { .. } => project = Some(workspace.id().get()),
                    WorkspaceKind::RemoteProject { .. } => {}
                }
            }
            (
                scratch.expect("the initial Scratch Workspace must remain"),
                project.expect("the Local Project Workspace was not created"),
            )
        });

        assert!(
            cx.debug_bounds(format!("workspace-row-pin-{project_id}").leak())
                .is_some(),
            "the Local Project row lost the pin that says its directory never moves"
        );
        assert!(
            cx.debug_bounds(format!("workspace-row-pin-{scratch_id}").leak())
                .is_none(),
            "a Scratch Workspace must not claim a pinned directory"
        );
    }

    #[gpui::test]
    fn the_workspace_chip_should_appear_only_while_the_sidebar_is_hidden(cx: &mut TestAppContext) {
        let (manager, _, cx) = workspace_manager(cx);

        assert!(
            cx.debug_bounds("workspace-chip").is_none(),
            "the sidebar already answers which Workspace is active"
        );

        click("toggle-sidebar-button", cx);

        assert!(!manager.read_with(cx, |manager, _| manager.sidebar_visible));
        assert!(
            cx.debug_bounds("workspace-chip").is_some(),
            "nothing named the Active Workspace once the sidebar closed"
        );

        cx.simulate_keystrokes("cmd-b");
        redraw(cx);

        assert!(
            cx.debug_bounds("workspace-chip").is_none(),
            "the chip outlived the sidebar it stands in for"
        );
    }

    #[gpui::test]
    fn the_workspace_chip_should_follow_the_active_workspace(cx: &mut TestAppContext) {
        let (manager, _, cx) = workspace_manager(cx);

        click("toggle-sidebar-button", cx);
        cx.simulate_keystrokes("cmd-n");
        redraw(cx);

        let chip = cx
            .debug_bounds("workspace-chip")
            .expect("the chip was not rendered for the new Active Workspace");
        let toggle = cx
            .debug_bounds("toggle-sidebar-button")
            .expect("the sidebar toggle was not rendered");

        assert_eq!(
            manager.read_with(cx, |manager, _| manager.workspaces.len()),
            2
        );
        assert!(
            chip.origin.x + chip.size.width <= toggle.origin.x,
            "the chip overlapped the sidebar toggle: {chip:?} {toggle:?}"
        );
    }

    #[gpui::test]
    fn cmd_n_should_create_a_scratch_workspace_without_the_panel(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);

        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();

        assert_eq!(
            manager.read_with(cx, |manager, cx| {
                (
                    manager.workspaces.len(),
                    manager.workspaces.active_workspace_id(),
                    manager.new_workspace_panel.read(cx).is_open(),
                )
            }),
            (2, WorkspaceId::new(2), false)
        );
    }

    #[gpui::test]
    fn new_workspace_button_should_start_with_a_full_width_divider(cx: &mut TestAppContext) {
        let (_manager, _records, cx) = workspace_manager(cx);
        let button = cx
            .debug_bounds("new-workspace-button")
            .expect("the New Workspace button was not rendered");
        let divider = cx
            .debug_bounds("new-workspace-button-top-divider")
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
            .debug_bounds("new-workspace-button")
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
                    .unwrap()
                    .to_path_buf(),
                records.dropped_session_ids(),
                records
                    .starts()
                    .into_iter()
                    .map(|start| {
                        start
                            .local_working_directory()
                            .expect("Scratch Workspace starts must remain local")
                            .path()
                            .to_path_buf()
                    })
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
    fn clicking_an_inactive_workspace_should_restore_its_focused_pane(cx: &mut TestAppContext) {
        let (manager, _records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-d cmd-n");
        cx.run_until_parked();

        click("workspace-row-1-inactive", cx);

        let state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            let active_workspace = manager.workspaces.active_workspace().payload().read(cx);
            (
                manager.workspaces.active_workspace_id(),
                manager.sidebar_focus.is_focused(window),
                manager.terminal_focus_blocker(window, cx),
                active_workspace.sidebar_detail(cx),
                active_workspace.focused_terminal_is_focused(window, cx),
            )
        });
        assert_eq!(
            state,
            (
                WorkspaceId::new(1),
                false,
                None,
                SharedString::from("2 Panes"),
                true,
            )
        );
    }

    #[gpui::test]
    fn clicking_the_active_workspace_should_restore_terminal_focus_from_the_sidebar(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = workspace_manager(cx);
        cx.simulate_keystrokes("cmd-shift-e");
        cx.run_until_parked();

        click("workspace-row-1-active", cx);

        let state = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.sidebar_focus.is_focused(window),
                manager.terminal_focus_blocker(window, cx),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        });
        assert_eq!(state, (false, None, true));
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
                manager.terminal_focus_blocker(window, cx),
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

        let input_bounds = cx
            .debug_bounds("workspace-rename-input")
            .expect("the shared rename input should be rendered");
        assert!(
            input_bounds.size.width > px(0.0) && input_bounds.size.height > px(0.0),
            "the shared rename input collapsed inside its context-menu decorator: {input_bounds:?}"
        );
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
    fn dismissing_inline_rename_context_menu_should_preserve_editor_until_submission(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = workspace_manager(cx);
        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-rename", cx);
        cx.simulate_keystrokes("cmd-a D e v");
        cx.run_until_parked();
        assert_eq!(
            manager.read_with(cx, |manager, cx| manager
                .rename
                .as_ref()
                .expect("rename editor should remain active")
                .input
                .read(cx)
                .value()
                .to_owned()),
            "Dev"
        );
        right_click("workspace-rename-input", cx);
        assert!(manager.read_with(cx, |manager, _| manager.rename.is_some()));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        let state_before_submit = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.rename.is_some(),
                manager.rename_is_focused(window),
                manager.workspaces.active_workspace().name().to_owned(),
            )
        });
        assert_eq!(
            state_before_submit,
            (true, true, "Default".to_owned()),
            "dismissing the owned menu must not commit or destroy the editor"
        );

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let state_after_submit = manager.read_with(cx, |manager, _| {
            (
                manager.rename.is_none(),
                manager.workspaces.active_workspace().name().to_owned(),
            )
        });
        assert_eq!(state_after_submit, (true, "Dev".to_owned()));
    }

    #[gpui::test]
    fn activating_inline_rename_context_menu_should_preserve_editor_until_submission(
        cx: &mut TestAppContext,
    ) {
        let (manager, _records, cx) = workspace_manager(cx);
        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-rename", cx);
        cx.simulate_keystrokes("cmd-a D e v");
        cx.run_until_parked();
        assert_eq!(
            manager.read_with(cx, |manager, cx| manager
                .rename
                .as_ref()
                .expect("rename editor should remain active")
                .input
                .read(cx)
                .value()
                .to_owned()),
            "Dev"
        );
        right_click("workspace-rename-input", cx);
        cx.simulate_keystrokes("end enter");
        cx.run_until_parked();

        let state_before_submit = cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.rename.is_some(),
                manager.rename_is_focused(window),
                manager.workspaces.active_workspace().name().to_owned(),
            )
        });
        assert_eq!(
            state_before_submit,
            (true, true, "Default".to_owned()),
            "activating the owned menu must not commit or destroy the editor"
        );

        cx.simulate_keystrokes("O p s enter");
        cx.run_until_parked();
        let state_after_submit = manager.read_with(cx, |manager, _| {
            (
                manager.rename.is_none(),
                manager.workspaces.active_workspace().name().to_owned(),
            )
        });
        assert_eq!(state_after_submit, (true, "Ops".to_owned()));
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
