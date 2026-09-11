use super::pane_lifecycle::{PaneConstruction, PaneLifecycleDependencies};
use super::workspace_chrome::{
    ICON_SIZE as WORKSPACE_CHROME_ICON_SIZE, TOGGLE_SIZE, WorkspaceChromeIdentity,
    WorkspaceChromeLayout,
};
#[cfg(test)]
use super::workspace_sidebar::{
    SIDEBAR_MAXIMUM_WIDTH, SIDEBAR_ROW_HEIGHT, TERMINAL_CONTENT_MINIMUM_WIDTH,
};
use super::workspace_sidebar::{
    SIDEBAR_ROW_HORIZONTAL_PADDING, SidebarEvent, WorkspaceMenuCommand, WorkspaceRowViewModel,
    WorkspaceSidebar, remote_connection_status,
};
use crate::platform::terminal_accessibility::TerminalAccessibilityAdapterFactory;
#[cfg(test)]
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::ssh::remote_account::RemoteWorkspaceAccount;
use crate::terminal::native_services::NativeServiceAdapters;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::directory_picker::{DirectoryPicker, DirectoryPickerEvent};
use super::remote_directory_picker::{RemoteDirectoryPicker, RemoteDirectoryPickerEvent};
use super::remote_workspace_flow::{
    RemoteWorkspaceAliasPin, RemoteWorkspaceConnectContext, RemoteWorkspaceConnectedSession,
    RemoteWorkspaceConnectionProgress, RemoteWorkspaceFlow, RemoteWorkspaceFlowBackend,
    RemoteWorkspaceFlowBackendError, RemoteWorkspaceFlowBackendFactory,
    RemoteWorkspaceFlowCompletion, RemoteWorkspaceFlowCompletionHandle, RemoteWorkspaceFlowEvent,
};
use super::tab_manager::{PreparedTabManagerRemoteRestart, RemoteTabManagerLifecycleError};
use super::terminal_focus::{TerminalFocusBlocker, TerminalFocusCoordinator, WorkspaceFocusOwners};
use super::{
    ActivateTab1, ActivateTab2, ActivateTab3, ActivateTab4, ActivateTab5, ActivateTab6,
    ActivateTab7, ActivateTab8, ActivateTab9, ActivateWorkspace1, ActivateWorkspace2,
    ActivateWorkspace3, ActivateWorkspace4, ActivateWorkspace5, ActivateWorkspace6,
    ActivateWorkspace7, ActivateWorkspace8, ActivateWorkspace9, ClosePane, CloseTab,
    CloseTerminalFind, CloseWorkspace, CopySelection, CreateTab, FindNext, FindPrevious,
    FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, NewRemoteWorkspace, NewWorkspace,
    OpenTerminalFind, RemoteChildLaunchUnavailable, SplitDown, SplitRight, SwitchWorkspace,
    TERMINAL_KEY_CONTEXT, TabManager, TabManagerEvent, TogglePaneZoom, ToggleSidebar,
    ToggleSidebarFocus, WORKSPACE_SIDEBAR_DEFAULT_WIDTH,
};
use crate::appearance::Color;
use crate::close_confirmation::{CloseConfirmation, CloseHierarchy, CloseTarget};
#[cfg(test)]
use crate::directory_selection::GpuiDirectorySelection;
use crate::directory_selection::SystemDirectorySelection;
use crate::domain::{
    CloseWorkspaceOutcome, DirectoryAvailability, FinalTabCloseOutcome, LocalDirectoryIdentity,
    PinnedDirectory, RemoteConnectionReduction, RemoteConnectionState, RemoteDirectory,
    RemoteWorkspaceTarget, ValidatedLocalDirectory, WorkspaceCollection, WorkspaceError,
    WorkspaceId, WorkspaceLocation,
};
use crate::platform::local_filesystem::{LocalFilesystemAuthority, LocalFilesystemError};
use crate::platform::permission_recovery::PermissionRecoveryOpener;
use crate::platform::window_movement::{
    OperatingSystemWindowDragError, OperatingSystemWindowDragPlatform,
};
use crate::ssh::live_connection::{ControlConnectionObserver, ControlConnectionTerminalState};
use crate::ssh::process::TransientSshErrorOutput;
#[cfg(test)]
use crate::terminal::GpuiTerminalKeyInputAdapterFactory;
use crate::terminal::metadata::RemoteTerminalMetadataContext;
use crate::terminal::{
    NativeServiceOrigin, NativeServiceStatus, PreparedWorkspaceTerminalLaunch, SelectionCopy,
    TerminalKeyInputAdapterFactory, TerminalSessionFactory, WorkspaceTerminalSessionFactory,
};
use gpui::prelude::*;
use gpui::{
    Action, AnyElement, App, Context, Edges, Entity, Pixels, Render, Task, WeakEntity, Window, div,
    px, rgba,
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, AnchoredAlignment, AnchoredPlacement,
    AnchoredPlacementConfig, ButtonVariant, ComboBox, ComboBoxAccessory, ComboBoxCopy,
    ComboBoxFallback, ComboBoxHandle, ComboBoxItem, CustomIconName, Icon, IconButton, IconName,
    ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId, ModalLayer,
    ProgressCancelDecision, ProgressCancellation, ProgressDialog, ProgressDialogHandle,
    ProgressDialogOutcome, ProgressDialogUpdate, ProgressState, Tooltip, TooltipLayer,
    WindowDragRegion, WindowDragRegionEvent, WindowDragRegionResponse, WindowDragRegionStatus,
    window_combo_box_is_open, window_modal_is_open,
};

const CHROME_DIVIDER_SIZE: f32 = super::resize_handle_theme::VISIBLE_THICKNESS;

fn sidebar_toggle_presentation(sidebar_visible: bool) -> (IconName, &'static str) {
    if sidebar_visible {
        (IconName::PanelLeft, "Close Sidebar")
    } else {
        (IconName::PanelRight, "Open Sidebar")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloseConfirmationAction {
    Confirm,
    Cancel,
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
    progress: Option<ProgressDialogHandle>,
    progress_generation: u64,
    _work: Task<()>,
    _progress_updates: Task<()>,
}

#[derive(Clone, Copy)]
struct RemoteWorkspaceReconnectProgressIdentity {
    workspace_id: WorkspaceId,
    reconnect_generation: u64,
    presentation_generation: u64,
}

struct PreparedRemoteWorkspaceReconnect {
    session: RemoteWorkspaceConnectedSession,
    lifecycle: ControlConnectionObserver,
    restart: PreparedTabManagerRemoteRestart,
    remote_user: crate::domain::RemoteUser,
}

struct TabManagerCreation {
    workspace_id: WorkspaceId,
    sidebar_visible: bool,
    sidebar_width: Pixels,
    operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
    pane_construction: PaneConstruction,
}

#[derive(Clone)]
pub(crate) struct WorkspaceManagerAdapters {
    pub(crate) local_filesystem: LocalFilesystemAuthority,
    pub(crate) key_input: Rc<dyn TerminalKeyInputAdapterFactory>,
    pub(crate) accessibility: Rc<dyn TerminalAccessibilityAdapterFactory>,
    pub(crate) native_services: NativeServiceAdapters,
    pub(crate) lifecycle: PaneLifecycleDependencies,
    pub(crate) directory_selection: Rc<dyn SystemDirectorySelection>,
    pub(crate) window_drag: Rc<dyn OperatingSystemWindowDragPlatform>,
    pub(crate) remote_workspace: Arc<dyn RemoteWorkspaceFlowBackendFactory>,
    pub(crate) permission_recovery: Option<Rc<dyn PermissionRecoveryOpener>>,
}

struct PendingRemoteActivation {
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

/// Owns directory selection and its Workspace target.
struct WorkspaceTransientUi {
    picker: Entity<DirectoryPicker>,
    pin_target: Option<WorkspaceId>,
}

impl WorkspaceTransientUi {
    fn show_picker(&mut self, window: &mut Window, cx: &mut App) {
        self.picker.update(cx, |picker, cx| picker.open(window, cx));
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WorkspaceSwitcherChoice {
    Workspace(WorkspaceId),
    Local(String),
    Remote(String),
}

pub(crate) struct WorkspaceManager {
    transient: WorkspaceTransientUi,
    workspace_switcher: ComboBoxHandle<WorkspaceSwitcherChoice>,
    remote_workspace_name: Option<String>,
    sidebar: Entity<WorkspaceSidebar>,
    local_filesystem: LocalFilesystemAuthority,
    workspaces: WorkspaceCollection<Entity<TabManager>>,
    session_factory: Rc<dyn TerminalSessionFactory>,
    pane_construction: PaneConstruction,
    local_home_directory_path: PathBuf,
    local_home_identity: LocalDirectoryIdentity,
    directory_selection_fallback: Rc<dyn SystemDirectorySelection>,
    remote_workspace_backend: Option<Arc<dyn RemoteWorkspaceFlowBackend>>,
    remote_workspace_unavailable_reason: Option<String>,
    remote_workspace_flow: Option<Entity<RemoteWorkspaceFlow>>,
    remote_pin_picker: Option<Entity<RemoteDirectoryPicker>>,
    pin_operation: u64,
    remote_workspace_runtimes: BTreeMap<WorkspaceId, RemoteWorkspaceRuntime>,
    remote_workspace_activation_task: Option<Task<()>>,
    remote_workspace_focus_restore_pending: bool,
    remote_workspace_reconnect: Option<RemoteWorkspaceReconnectAttempt>,
    #[cfg(test)]
    close_focused_pane_before_reconnect_commit: bool,
    operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
    window_drag_status: WindowDragRegionStatus,
    pending_final_tab_closes: BTreeSet<WorkspaceId>,
    close_confirmation: CloseConfirmation,
}

impl WorkspaceManager {
    #[cfg(test)]
    fn new_with_remote_workspace_backend_factory(
        session_factory: Rc<dyn TerminalSessionFactory>,
        local_home_directory_path: PathBuf,
        remote_workspace_backend_factory: Arc<dyn RemoteWorkspaceFlowBackendFactory>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_adapters(
            session_factory,
            local_home_directory_path,
            WorkspaceManagerAdapters {
                local_filesystem: LocalFilesystemAuthority::testing(),
                key_input: Rc::new(GpuiTerminalKeyInputAdapterFactory::default()),
                accessibility: Rc::new(crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default()),
                native_services: crate::terminal::native_services::testing::adapters(),
                lifecycle: PaneLifecycleDependencies::testing(),
                directory_selection: Rc::new(GpuiDirectorySelection),
                permission_recovery: None,
                window_drag: Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
                remote_workspace: remote_workspace_backend_factory,
            },
            window,
            cx,
        )
    }

    #[cfg(test)]
    fn new_with_directory_selection_fallback(
        session_factory: Rc<dyn TerminalSessionFactory>,
        local_home_directory_path: PathBuf,
        directory_selection_fallback: Rc<dyn SystemDirectorySelection>,
        remote_workspace_backend_factory: Arc<dyn RemoteWorkspaceFlowBackendFactory>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_adapters(
            session_factory,
            local_home_directory_path,
            WorkspaceManagerAdapters {
                local_filesystem: LocalFilesystemAuthority::testing(),
                key_input: Rc::new(GpuiTerminalKeyInputAdapterFactory::default()),
                accessibility: Rc::new(crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default()),
                native_services: crate::terminal::native_services::testing::adapters(),
                lifecycle: PaneLifecycleDependencies::testing(),
                directory_selection: directory_selection_fallback,
                permission_recovery: None,
                window_drag: Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
                remote_workspace: remote_workspace_backend_factory,
            },
            window,
            cx,
        )
    }

    #[cfg(test)]
    fn new_with_operating_system_window_drag_platform(
        session_factory: Rc<dyn TerminalSessionFactory>,
        local_home_directory_path: PathBuf,
        operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
        remote_workspace_backend_factory: Arc<dyn RemoteWorkspaceFlowBackendFactory>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_adapters(
            session_factory,
            local_home_directory_path,
            WorkspaceManagerAdapters {
                local_filesystem: LocalFilesystemAuthority::testing(),
                key_input: Rc::new(GpuiTerminalKeyInputAdapterFactory::default()),
                accessibility: Rc::new(crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default()),
                native_services: crate::terminal::native_services::testing::adapters(),
                lifecycle: PaneLifecycleDependencies::testing(),
                directory_selection: Rc::new(GpuiDirectorySelection),
                permission_recovery: None,
                window_drag: operating_system_window_drag_platform,
                remote_workspace: remote_workspace_backend_factory,
            },
            window,
            cx,
        )
    }

    pub(crate) fn new_with_adapters(
        session_factory: Rc<dyn TerminalSessionFactory>,
        local_home_directory_path: PathBuf,
        adapters: WorkspaceManagerAdapters,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let WorkspaceManagerAdapters {
            local_filesystem,
            key_input: key_input_adapter_factory,
            accessibility: accessibility_adapter_factory,
            native_services: native_service_adapters,
            lifecycle: lifecycle_dependencies,
            directory_selection: directory_selection_fallback,
            permission_recovery,
            window_drag: operating_system_window_drag_platform,
            remote_workspace: remote_workspace_backend_factory,
        } = adapters;
        let pane_construction = PaneConstruction::new(
            key_input_adapter_factory,
            accessibility_adapter_factory,
            native_service_adapters,
            lifecycle_dependencies,
        );
        let (default_directory, initial_directory_error) =
            initial_home_directory(local_home_directory_path.clone(), &local_filesystem);
        let local_home_identity = default_directory.identity();
        let initial_workspace_identity = default_directory.identity();
        let initial_window_drag_platform = Rc::clone(&operating_system_window_drag_platform);
        let mut workspaces =
            WorkspaceCollection::new_local(default_directory, |workspace_id, home_directory| {
                Self::create_local_tab_manager(
                    WorkspaceTerminalSessionFactory::new_local_with_authority(
                        Rc::clone(&session_factory),
                        ValidatedLocalDirectory::new(
                            home_directory.to_path_buf(),
                            initial_workspace_identity,
                        ),
                        local_filesystem.clone(),
                    ),
                    TabManagerCreation {
                        workspace_id,
                        sidebar_visible: true,
                        sidebar_width: px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
                        operating_system_window_drag_platform: Rc::clone(
                            &initial_window_drag_platform,
                        ),
                        pane_construction: pane_construction.clone(),
                    },
                    window,
                    cx,
                )
            });
        if let Some(reason) = initial_directory_error {
            let _ = workspaces.set_directory_unavailable(workspaces.active_workspace_id(), reason);
        }
        let sidebar = cx.new(|cx| WorkspaceSidebar::new(window, cx));
        cx.subscribe_in(
            &sidebar,
            window,
            |manager, _, event: &SidebarEvent, window, cx| {
                manager.handle_sidebar_event(event.clone(), window, cx);
            },
        )
        .detach();
        let directory_picker_home =
            Self::directory_picker_starting_directory(&local_home_directory_path);
        let directory_picker = cx.new(|cx| {
            DirectoryPicker::new(
                directory_picker_home,
                Arc::new(local_filesystem.clone()),
                permission_recovery,
                window,
                cx,
            )
        });
        cx.subscribe_in(
            &directory_picker,
            window,
            |manager, _, event: &DirectoryPickerEvent, window, cx| {
                manager.handle_directory_picker_event(event, window, cx);
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
        cx.observe_window_activation(window, |manager, window, cx| {
            manager.restore_remote_workspace_focus_after_activation(window, cx);
        })
        .detach();

        Self {
            transient: WorkspaceTransientUi {
                picker: directory_picker,
                pin_target: None,
            },
            workspace_switcher: ComboBoxHandle::default(),
            remote_workspace_name: None,
            sidebar,
            workspaces,
            local_filesystem,
            session_factory,
            pane_construction,
            local_home_directory_path,
            local_home_identity,
            directory_selection_fallback,
            remote_workspace_backend,
            remote_workspace_unavailable_reason: remote_unavailable_reason,
            remote_workspace_flow: None,
            remote_pin_picker: None,
            pin_operation: 0,
            remote_workspace_runtimes: BTreeMap::new(),
            remote_workspace_activation_task: None,
            remote_workspace_focus_restore_pending: false,
            remote_workspace_reconnect: None,
            #[cfg(test)]
            close_focused_pane_before_reconnect_commit: false,
            operating_system_window_drag_platform,
            window_drag_status: WindowDragRegionStatus::new(),
            pending_final_tab_closes: BTreeSet::new(),
            close_confirmation: CloseConfirmation::default(),
        }
    }

    fn create_local_tab_manager(
        session_factory: WorkspaceTerminalSessionFactory,
        creation: TabManagerCreation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TabManager> {
        match Self::try_create_tab_manager(session_factory, creation, window, cx) {
            Ok(manager) => manager,
            Err(error) => unreachable!("Local initial launch preparation is infallible: {error}"),
        }
    }

    fn try_create_tab_manager(
        session_factory: WorkspaceTerminalSessionFactory,
        creation: TabManagerCreation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Entity<TabManager>, crate::terminal::RemoteChannelUnavailable> {
        let prepared_launch = session_factory.prepare_child_launch()?;
        Ok(Self::create_tab_manager_with_prepared_launch(
            session_factory,
            prepared_launch,
            creation,
            window,
            cx,
        ))
    }

    fn create_tab_manager_with_prepared_launch(
        session_factory: WorkspaceTerminalSessionFactory,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        creation: TabManagerCreation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TabManager> {
        let TabManagerCreation {
            workspace_id,
            sidebar_visible,
            sidebar_width,
            operating_system_window_drag_platform,
            pane_construction,
        } = creation;
        let manager = cx.new(|cx| {
            let mut manager = TabManager::new_with_prepared_initial_launch(
                session_factory,
                prepared_launch,
                operating_system_window_drag_platform,
                pane_construction,
                window,
                cx,
            );
            manager.set_sidebar_layout(sidebar_visible, sidebar_width, sidebar_width, cx);
            manager
        });
        cx.subscribe_in(
            &manager,
            window,
            move |workspace_manager, tab_manager, event: &TabManagerEvent, window, cx| match event {
                TabManagerEvent::ClosePaneRequested { tab_id, pane_id } => {
                    workspace_manager.request_close(
                        CloseTarget::Pane {
                            workspace_id,
                            tab_id: *tab_id,
                            pane_id: *pane_id,
                        },
                        window,
                        cx,
                    );
                }
                TabManagerEvent::CloseTabRequested { tab_id } => {
                    workspace_manager.request_close(
                        CloseTarget::Tab {
                            workspace_id,
                            tab_id: *tab_id,
                        },
                        window,
                        cx,
                    );
                }
                TabManagerEvent::FinalTabCloseRequested { .. } => {
                    if workspace_manager
                        .pending_final_tab_closes
                        .insert(workspace_id)
                    {
                        cx.defer_in(window, move |workspace_manager, window, cx| {
                            workspace_manager.close_workspace_for_final_tab(
                                workspace_id,
                                window,
                                cx,
                            );
                        });
                    }
                }
                TabManagerEvent::PresentationChanged => {
                    if let Some(directory) = tab_manager.read(cx).automatic_directory(cx) {
                        let _ = workspace_manager
                            .workspaces
                            .update_automatic_directory(workspace_id, directory);
                    }
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

    fn native_service_target(
        &self,
        origin: NativeServiceOrigin,
        cx: &App,
    ) -> Option<Entity<super::TerminalPane>> {
        if self.workspaces.active_workspace_id() != origin.workspace_id() {
            return None;
        }
        self.workspaces
            .workspace(origin.workspace_id())?
            .payload()
            .read(cx)
            .native_service_target(origin, cx)
    }

    pub(crate) fn native_service_selection(
        &self,
        origin: NativeServiceOrigin,
        window: &Window,
        cx: &mut App,
    ) -> Option<SelectionCopy> {
        self.native_service_target(origin, cx)?
            .update(cx, |terminal, cx| {
                terminal.native_service_selection(origin, window, cx)
            })
    }

    pub(crate) fn insert_native_service_text(
        &self,
        origin: NativeServiceOrigin,
        text: String,
        window: &Window,
        cx: &mut App,
    ) -> bool {
        self.native_service_target(origin, cx)
            .is_some_and(|terminal| {
                terminal.update(cx, |terminal, cx| {
                    terminal.insert_native_service_text(origin, text, window, cx)
                })
            })
    }

    fn terminal_focus_blocker(&self, window: &Window, cx: &App) -> Option<TerminalFocusBlocker> {
        TerminalFocusCoordinator::modal_blocker(
            self.non_modal_terminal_focus_blocker(window, cx),
            window_modal_is_open(window, cx),
        )
    }

    fn non_modal_terminal_focus_blocker(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<TerminalFocusBlocker> {
        TerminalFocusCoordinator::workspace_blocker(WorkspaceFocusOwners {
            picker: self.transient.picker.read(cx).blocks_terminal_input()
                || self
                    .remote_pin_picker
                    .as_ref()
                    .is_some_and(|picker| picker.read(cx).blocks_terminal_input()),
            remote_flow: self
                .remote_workspace_flow
                .as_ref()
                .is_some_and(|flow| flow.read(cx).blocks_terminal_input()),
            switcher: window_combo_box_is_open(window, cx),
            window_drag: self.window_drag_status.is_active(),
            sidebar_resize: self.sidebar.read(cx).is_resizing(),
            rename: self.sidebar.read(cx).is_renaming(),
            context_menu: self.sidebar.read(cx).menu_target().is_some(),
            sidebar: self.sidebar.read(cx).is_focused(window),
        })
    }

    fn workspace_switcher_items(&self, cx: &App) -> Vec<ComboBoxItem<WorkspaceSwitcherChoice>> {
        let presentation = crate::desktop_profile::DesktopPresentation::get(cx);
        self.workspaces
            .iter()
            .enumerate()
            .map(|(index, workspace)| {
                let active = workspace.id() == self.workspaces.active_workspace_id();
                let icon = if active {
                    IconName::Check
                } else {
                    match workspace.location() {
                        WorkspaceLocation::Local => IconName::Terminal,
                        WorkspaceLocation::Remote { .. } => IconName::Globe,
                    }
                };
                let (path, directory_identity) = directory_labels(
                    workspace.location(),
                    workspace.local_display_directory(),
                    workspace.remote_display_directory(),
                    &self.local_home_directory_path,
                );
                let item = ComboBoxItem::new(
                    WorkspaceSwitcherChoice::Workspace(workspace.id()),
                    workspace.name().to_owned(),
                )
                .keywords([path, directory_identity])
                .leading_icon(move |foreground| {
                    div()
                        .when(active, |icon| {
                            icon.debug_selector(|| "workspace-switcher-active-marker".to_owned())
                        })
                        .child(Icon::new(icon, px(14.0), foreground))
                        .into_any_element()
                })
                .debug_selector(format!(
                    "workspace-switcher-result-{}",
                    workspace.id().get()
                ));
                match workspace_activation_shortcut(index, presentation) {
                    Some(shortcut) => item.shortcut(shortcut),
                    None => item,
                }
            })
            .collect()
    }

    fn open_workspace_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window_modal_is_open(window, cx) {
            return;
        }
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.dismiss_editing(window, cx);
        });
        self.workspace_switcher.open(window, cx);
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
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

    fn restore_remote_workspace_focus_after_activation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !window.is_window_active()
            || !std::mem::take(&mut self.remote_workspace_focus_restore_pending)
        {
            return;
        }
        if self.terminal_focus_blocker(window, cx).is_some() {
            return;
        }
        self.focus(window, cx);
        cx.notify();
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
                window.titlebar_double_click();
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

    fn synchronize_tab_manager_layout(
        &self,
        workspace_id: WorkspaceId,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspaces.workspace(workspace_id) else {
            return;
        };
        let chrome = WorkspaceChromeLayout::resolve(
            self.sidebar.read(cx).layout(),
            workspace.name(),
            workspace.pinned_directory().is_some(),
            window,
            cx,
        );
        workspace.payload().update(cx, |manager, cx| {
            manager.set_sidebar_layout(
                self.sidebar.read(cx).layout().visible,
                self.sidebar.read(cx).layout().width,
                chrome.width,
                cx,
            );
        });
    }

    fn synchronize_tab_manager_layouts(&self, window: &Window, cx: &mut Context<Self>) {
        for workspace_id in self.workspaces.iter().map(|workspace| workspace.id()) {
            self.synchronize_tab_manager_layout(workspace_id, window, cx);
        }
    }

    #[cfg(test)]
    fn set_sidebar_layout(
        &mut self,
        visible: bool,
        width: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_layout(visible, width, window, cx);
        });
        self.synchronize_tab_manager_layouts(window, cx);
        cx.notify();
    }

    #[cfg(test)]
    fn resize_sidebar(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.resize(width, window, cx));
    }

    fn scroll_active_workspace_into_view(&self, cx: &mut App) {
        let id = self.workspaces.active_workspace_id();
        if let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id() == id)
        {
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.reveal_row(index, cx));
        }
    }

    fn create_local_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.create_named_local_workspace(None, window, cx);
    }

    fn create_named_local_workspace(
        &mut self,
        name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous_manager = self.workspaces.active_workspace().payload().clone();
        let local_filesystem = self.local_filesystem.clone();
        let session_factory = Rc::clone(&self.session_factory);
        let pane_construction = self.pane_construction.clone();
        let window_drag_platform = Rc::clone(&self.operating_system_window_drag_platform);
        let sidebar_visible = self.sidebar.read(cx).layout().visible;
        let sidebar_width = self.sidebar.read(cx).layout().width;
        let directory = match self.local_home_directory() {
            Ok(directory) => directory,
            Err(_) => {
                Self::show_home_directory_unavailable(window, cx);
                return;
            }
        };
        let directory_identity = directory.identity();
        let result =
            self.workspaces
                .create_local_workspace(directory, |workspace_id, home_directory| {
                    Self::create_local_tab_manager(
                        WorkspaceTerminalSessionFactory::new_local_with_authority(
                            session_factory,
                            ValidatedLocalDirectory::new(
                                home_directory.to_path_buf(),
                                directory_identity,
                            ),
                            local_filesystem.clone(),
                        ),
                        TabManagerCreation {
                            workspace_id,
                            sidebar_visible,
                            sidebar_width,
                            operating_system_window_drag_platform: window_drag_platform,
                            pane_construction,
                        },
                        window,
                        cx,
                    )
                });
        let workspace_id = match result {
            Ok(workspace_id) => workspace_id,
            Err(error) => {
                Self::report_workspace_error("create", error);
                return;
            }
        };
        if let Some(name) = name
            && let Err(error) = self
                .workspaces
                .name_workspace_for_creation(workspace_id, name)
        {
            Self::report_workspace_error("rename", error);
        }
        let Some(next_manager) = self
            .workspaces
            .workspace(workspace_id)
            .map(|workspace| workspace.payload().clone())
        else {
            unreachable!("a newly created Workspace must remain owned by its collection")
        };

        self.synchronize_tab_manager_layout(workspace_id, window, cx);
        previous_manager.update(cx, |manager, cx| manager.deactivate(cx));
        next_manager.update(cx, |manager, cx| manager.activate(window, cx));
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.cancel_rename(cx);
        });
        self.sync_terminal_focus_blocker(window, cx);
        self.scroll_active_workspace_into_view(cx);

        cx.notify();
    }

    fn directory_picker_starting_directory(configured_home: &std::path::Path) -> PathBuf {
        configured_home.to_path_buf()
    }

    fn apply_validated_local_pin(
        &mut self,
        workspace_id: WorkspaceId,
        directory: ValidatedLocalDirectory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Ok(directory) = self.local_filesystem.revalidate_directory(&directory) else {
            return false;
        };
        self.apply_directory_pin(
            workspace_id,
            Some(PinnedDirectory::Local(directory)),
            window,
            cx,
        )
    }

    fn apply_directory_pin(
        &mut self,
        workspace_id: WorkspaceId,
        pin: Option<PinnedDirectory>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self
            .workspaces
            .set_pinned_directory(workspace_id, pin.clone())
            .is_err()
        {
            return false;
        }
        self.pin_operation = self.pin_operation.wrapping_add(1);
        self.transient.pin_target = None;
        if let Some(workspace) = self.workspaces.workspace(workspace_id) {
            workspace
                .payload()
                .update(cx, |manager, cx| manager.set_pinned_directory(pin, cx));
        }
        self.synchronize_tab_manager_layouts(window, cx);

        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
        true
    }

    fn show_pin_error(window: &mut Window, cx: &mut Context<Self>) {
        let _ = Alert::new(
            ModalId::new("workspace-pin-directory-unavailable"),
            "Directory unavailable",
            "Directory Unavailable",
            "The directory could not be pinned. Check that it exists and is accessible, then try again.",
            vec![ModalAction::new((), "OK", ModalActionRole::Cancel, "workspace-pin-error-ok")],
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, |_, _| {});
    }

    fn open_remote_pin_picker(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(runtime) = self.remote_workspace_runtimes.get(&workspace_id) else {
            return;
        };
        let Some(session) = runtime.session.as_ref() else {
            Self::show_pin_error(window, cx);
            return;
        };
        let generation = runtime.generation;
        let operation = self.pin_operation;
        let provider = session.provider();
        let picker = cx.new(|cx| RemoteDirectoryPicker::new(provider, window, cx));
        cx.subscribe_in(
            &picker,
            window,
            move |manager, picker, event: &RemoteDirectoryPickerEvent, window, cx| {
                if manager.remote_pin_picker.as_ref() != Some(picker) {
                    return;
                }
                match event {
                    RemoteDirectoryPickerEvent::Confirmed(selection) => {
                        let current = manager.pin_operation == operation
                            && manager
                                .remote_workspace_runtimes
                                .get(&workspace_id)
                                .is_some_and(|runtime| {
                                    runtime.generation == generation && runtime.session.is_some()
                                });
                        let applied = current
                            && manager.apply_directory_pin(
                                workspace_id,
                                Some(PinnedDirectory::Remote {
                                    directory: selection.directory().clone(),
                                    identity: selection.physical_directory().clone(),
                                }),
                                window,
                                cx,
                            );
                        let picker = picker.clone();
                        let owner = cx.entity();
                        window.defer(cx, move |window, cx| {
                            picker.update(cx, |picker, cx| {
                                if applied {
                                    picker.complete_activation(window, cx);
                                } else {
                                    picker.activation_failed(window, cx);
                                }
                            });
                            if applied {
                                owner.update(cx, |manager, cx| {
                                    manager.remote_pin_picker = None;
                                    manager.sync_terminal_focus_blocker(window, cx);
                                    manager.focus(window, cx);
                                    cx.notify();
                                });
                            }
                        });
                    }
                    RemoteDirectoryPickerEvent::Dismissed => {
                        manager.remote_pin_picker = None;
                        manager.transient.pin_target = None;
                    }
                    RemoteDirectoryPickerEvent::StateChanged => {}
                }
                manager.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            },
        )
        .detach();
        self.remote_pin_picker = Some(picker.clone());
        self.transient
            .picker
            .update(cx, |picker, cx| picker.dismiss(window, cx));
        picker.update(cx, |picker, cx| {
            picker.open(window, cx);
        });
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    fn open_pin_directory_picker(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if spaceterm_ui::window_menu_is_open(window, cx) {
            let manager = cx.entity();
            window.defer(cx, move |window, cx| {
                spaceterm_ui::dismiss_active_menu(window, cx);
                manager.update(cx, |manager, cx| {
                    manager.open_pin_directory_picker(workspace_id, window, cx)
                });
            });
            return;
        }

        self.transient.pin_target = Some(workspace_id);
        self.pin_operation = self.pin_operation.wrapping_add(1);
        if self
            .workspaces
            .workspace(workspace_id)
            .is_some_and(|workspace| {
                matches!(workspace.location(), WorkspaceLocation::Remote { .. })
            })
        {
            self.open_remote_pin_picker(workspace_id, window, cx);
            return;
        }

        if self.transient.picker.read(cx).is_open() {
            self.transient
                .picker
                .update(cx, |picker, cx| picker.refocus_path(window, cx));
            return;
        }
        if spaceterm_ui::window_menu_is_open(window, cx) {
            let manager = cx.entity();
            window.defer(cx, move |window, cx| {
                spaceterm_ui::dismiss_active_menu(window, cx);
                manager.update(cx, |manager, cx| {
                    manager.present_directory_picker(window, cx)
                });
            });
            return;
        }
        self.present_directory_picker(window, cx);
    }

    fn present_directory_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.transient.picker.read(cx).is_open() {
            self.transient
                .picker
                .update(cx, |picker, cx| picker.refocus_path(window, cx));
            return;
        }
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.dismiss_editing(window, cx);
        });
        self.transient.show_picker(window, cx);
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    fn present_remote_workspace_flow(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.remote_workspace_backend.as_ref().map(Arc::clone) else {
            self.sync_terminal_focus_blocker(window, cx);
            return;
        };
        if self.remote_workspace_flow.is_some() {
            return;
        }
        self.remote_workspace_name = Some(name);
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
        if !flow.update(cx, |flow, cx| flow.open(window, cx)) {
            self.remote_workspace_flow = None;
            self.remote_workspace_name = None;
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
                self.remote_workspace_focus_restore_pending = !window.is_window_active();
                self.remote_workspace_flow = None;
                self.remote_workspace_name = None;
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            }
            RemoteWorkspaceFlowEvent::Completed(handle) => {
                self.activate_remote_workspace(source, handle, window, cx);
            }
        }
    }

    fn activate_remote_workspace(
        &mut self,
        flow: &Entity<RemoteWorkspaceFlow>,
        handle: &RemoteWorkspaceFlowCompletionHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(completion) = handle.take() else {
            return;
        };
        let terminal_factory = WorkspaceTerminalSessionFactory::new_remote(
            Rc::clone(&self.session_factory),
            ValidatedLocalDirectory::new(
                self.local_home_directory_path.clone(),
                self.local_home_identity.clone(),
            ),
            RemoteTerminalMetadataContext::new(
                completion.destination().clone(),
                completion.initial_directory().clone(),
            )
            .with_machine(remote_machine(completion.account())),
            completion.physical_directory().clone(),
            // A Remote Pane falls back to its login shell, exactly as a Local Pane does. The
            // The sidebar's machine label identifies the destination independently of its name.
            completion.account().login_shell().name().to_owned(),
            completion.terminal_channels(),
        );
        let Some(revalidation) = terminal_factory.revalidate_remote_child_launch() else {
            unreachable!("a Remote Workspace Terminal factory must require revalidation")
        };
        let activation = PendingRemoteActivation {
            flow: flow.clone(),
            handle: handle.clone(),
            completion,
            terminal_factory,
        };
        self.remote_workspace_activation_task =
            Some(cx.spawn_in(window, async move |manager, cx| {
                let revalidation = revalidation.await;
                let _ = manager.update_in(cx, |manager, window, cx| {
                    manager.finish_remote_workspace_activation(
                        activation,
                        revalidation,
                        window,
                        cx,
                    );
                });
            }));
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    fn finish_remote_workspace_activation(
        &mut self,
        activation: PendingRemoteActivation,
        revalidation: Result<(), crate::terminal::RemoteChannelRevalidationError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let PendingRemoteActivation {
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
            self.return_remote_workspace_activation(&flow, &handle, completion, window, cx);
            return;
        }
        let prepared_launch = match terminal_factory.prepare_child_launch() {
            Ok(prepared_launch) => prepared_launch,
            Err(error) => {
                eprintln!("cannot create Remote Workspace because {error}");
                self.return_remote_workspace_activation(&flow, &handle, completion, window, cx);
                return;
            }
        };
        match self.try_create_remote_workspace(
            completion,
            terminal_factory,
            prepared_launch,
            window,
            cx,
        ) {
            Ok(()) => self.acknowledge_remote_workspace_activation(&flow, &handle, window, cx),
            Err(completion) => {
                self.return_remote_workspace_activation(&flow, &handle, *completion, window, cx)
            }
        }
    }

    fn try_create_remote_workspace(
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
        let key = RemoteWorkspaceTarget::new(
            completion.destination().clone(),
            completion.physical_directory().clone(),
        );
        let previous_workspace_id = self.workspaces.active_workspace_id();
        let previous_manager = self.workspaces.active_workspace().payload().clone();
        let sidebar_visible = self.sidebar.read(cx).layout().visible;
        let sidebar_width = self.sidebar.read(cx).layout().width;
        let window_drag_platform = Rc::clone(&self.operating_system_window_drag_platform);
        let pane_construction = self.pane_construction.clone();
        let result = self.workspaces.create_remote_workspace(
            key,
            completion.account().remote_user().clone(),
            completion.initial_directory().clone(),
            completion.remote_home_identity().clone(),
            RemoteConnectionState::connected(1),
            |workspace_id| {
                Self::create_tab_manager_with_prepared_launch(
                    terminal_factory,
                    prepared_launch,
                    TabManagerCreation {
                        workspace_id,
                        sidebar_visible,
                        sidebar_width,
                        operating_system_window_drag_platform: window_drag_platform,
                        pane_construction,
                    },
                    window,
                    cx,
                )
            },
        );
        let workspace_id = match result {
            Ok(workspace_id) => workspace_id,
            Err(_) => return Err(Box::new(completion)),
        };
        if let Some(name) = self.remote_workspace_name.take()
            && let Err(error) = self
                .workspaces
                .name_workspace_for_creation(workspace_id, name)
        {
            Self::report_workspace_error("rename", error);
        }
        let (session, _, _, _, _, _, lifecycle) = completion.into_parts();
        let replaced = self.remote_workspace_runtimes.insert(
            workspace_id,
            RemoteWorkspaceRuntime::new(1, session, lifecycle, alias_pin),
        );
        debug_assert!(
            replaced.is_none(),
            "a new Remote Workspace owns one runtime"
        );
        self.activate_remote_tab_manager(
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

    fn return_remote_workspace_activation(
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

    fn acknowledge_remote_workspace_activation(
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
        self.remote_workspace_name = None;
        self.sync_terminal_focus_blocker(window, cx);
        self.scroll_active_workspace_into_view(cx);

        cx.notify();
    }

    fn activate_remote_tab_manager(
        &self,
        previous_workspace_id: WorkspaceId,
        previous_manager: Entity<TabManager>,
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
        self.synchronize_tab_manager_layout(workspace_id, window, cx);
        if previous_workspace_id != workspace_id {
            previous_manager.update(cx, |manager, cx| manager.deactivate(cx));
        }
        let blocker = self.non_modal_terminal_focus_blocker(window, cx);
        next_manager.update(cx, |manager, cx| {
            manager.set_parent_focus_blocker(blocker, cx);
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
                            matches!(workspace.location(), WorkspaceLocation::Remote { .. })
                        })
                })
        );
        debug_assert!(self.workspaces.iter().all(|workspace| {
            !matches!(workspace.location(), WorkspaceLocation::Remote { .. })
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
        let Some(tab_manager) = self
            .workspaces
            .workspace(workspace_id)
            .map(|workspace| workspace.payload().clone())
        else {
            return;
        };
        if let Err(error) =
            tab_manager.update(cx, |manager, cx| manager.disconnect_remote(generation, cx))
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
        let Some(directory) = workspace.remote_starting_directory().cloned() else {
            return;
        };
        let Some(state) = workspace.remote_connection_state() else {
            return;
        };
        let generation = state.generation();
        let destination = key.destination().clone();
        let pinned_directory = workspace.pinned_directory().cloned();
        let expected_identity = key.physical_directory().clone();
        let tab_manager = workspace.payload().clone();
        let local_root = ValidatedLocalDirectory::new(
            self.local_home_directory_path.clone(),
            self.local_home_identity.clone(),
        );
        let session_factory = Rc::clone(&self.session_factory);
        let cancelled = Arc::new(AtomicBool::new(false));
        let Some(progress) = self.present_remote_workspace_reconnect_progress(
            RemoteWorkspaceReconnectProgressIdentity {
                workspace_id,
                reconnect_generation: generation,
                presentation_generation: 1,
            },
            RemoteWorkspaceConnectionProgress::CheckingCompatibility,
            Arc::clone(&cancelled),
            window,
            cx,
        ) else {
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
                    .bind_terminal_channels(account.login_shell())
                    .map_err(|_| RemoteWorkspaceReconnectFailure::ConnectionFailed {
                        detail: None,
                    })?;
                let lifecycle = session
                    .take_lifecycle_observer()
                    .ok_or(RemoteWorkspaceReconnectFailure::ConnectionFailed { detail: None })?;
                let mut factory = WorkspaceTerminalSessionFactory::new_remote(
                    session_factory,
                    local_root,
                    RemoteTerminalMetadataContext::new(destination, directory)
                        .with_machine(remote_machine(&account)),
                    expected_identity.clone(),
                    account.login_shell().name().to_owned(),
                    channels,
                );
                factory.set_pinned_directory(pinned_directory);
                let restart = tab_manager
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
                    remote_user: account.remote_user().clone(),
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
            progress: Some(progress),
            progress_generation: 1,
            _work: work,
            _progress_updates: progress_updates,
        });

        cx.notify();
    }

    fn present_remote_workspace_reconnect_progress(
        &self,
        identity: RemoteWorkspaceReconnectProgressIdentity,
        progress: RemoteWorkspaceConnectionProgress,
        cancelled: Arc<AtomicBool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<ProgressDialogHandle> {
        let result_manager = cx.weak_entity();
        let result_window = window.window_handle();
        let cancel_flag = cancelled;
        ProgressDialog::new(
            ModalId::new("remote-workspace-reconnect-progress"),
            "Remote reconnection progress",
            "Reconnect Remote Workspace",
            progress.status(),
            ProgressState::Indeterminate,
            ProgressCancellation::Cancellable(ModalAction::new(
                (),
                "Cancel",
                ModalActionRole::Cancel,
                "remote-workspace-reconnect-cancel",
            )),
        )
        .detail("Authentication prompts open in a SpaceTerm dialog.")
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
                            identity, outcome, window, cx,
                        );
                    });
                });
            },
        )
        .ok()
    }

    fn apply_remote_workspace_reconnect_progress(
        &mut self,
        workspace_id: WorkspaceId,
        generation: u64,
        update: RemoteWorkspaceConnectionProgress,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let is_current = self
            .remote_workspace_reconnect
            .as_ref()
            .is_some_and(|attempt| {
                attempt.workspace_id == workspace_id && attempt.generation == generation
            });
        if !is_current {
            return false;
        }
        if update == RemoteWorkspaceConnectionProgress::Authenticating {
            let progress = self
                .remote_workspace_reconnect
                .as_mut()
                .and_then(|attempt| {
                    let progress = attempt.progress.take();
                    if progress.is_some() {
                        attempt.progress_generation = attempt.progress_generation.wrapping_add(1);
                    }
                    progress
                });
            if let Some(progress) = progress {
                let _ = progress.dismiss(window, cx);
            }
            return true;
        }
        if let Some(progress) = self
            .remote_workspace_reconnect
            .as_ref()
            .and_then(|attempt| attempt.progress.as_ref())
        {
            let _ = progress.update(
                ProgressDialogUpdate::new().status(update.status()),
                window,
                cx,
            );
            return true;
        }
        let attempt = self
            .remote_workspace_reconnect
            .as_ref()
            .expect("the current reconnect attempt must remain owned");
        let cancelled = Arc::clone(&attempt.cancelled);
        let progress_generation = attempt.progress_generation;
        let Some(progress) = self.present_remote_workspace_reconnect_progress(
            RemoteWorkspaceReconnectProgressIdentity {
                workspace_id,
                reconnect_generation: generation,
                presentation_generation: progress_generation,
            },
            update,
            Arc::clone(&cancelled),
            window,
            cx,
        ) else {
            cancelled.store(true, Ordering::Release);
            return false;
        };
        if let Some(attempt) = self.remote_workspace_reconnect.as_mut().filter(|attempt| {
            attempt.workspace_id == workspace_id && attempt.generation == generation
        }) {
            attempt.progress = Some(progress);
            return true;
        }
        let _ = progress.dismiss(window, cx);
        false
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
                let Some(tab_manager) = self
                    .workspaces
                    .workspace(workspace_id)
                    .map(|workspace| workspace.payload().clone())
                else {
                    return;
                };
                #[cfg(test)]
                if self.close_focused_pane_before_reconnect_commit {
                    self.close_focused_pane_before_reconnect_commit = false;
                    tab_manager
                        .read(cx)
                        .active_pane_host()
                        .update(cx, |host, cx| host.close_focused_for_test(window, cx));
                }
                if let Err(error) = tab_manager.update(cx, |manager, cx| {
                    manager.commit_remote_restart(prepared.restart, window, cx)
                }) {
                    let failure = classify_remote_workspace_restart_failure(error);
                    let next = remote_workspace_reconnect_failure_state(&failure, generation);
                    let _ = self
                        .workspaces
                        .reduce_remote_connection_state(workspace_id, next);
                    if let Some(progress) = &attempt.progress {
                        let _ = progress.fail(window, cx);
                    }
                    self.present_remote_workspace_reconnect_error(failure, window, cx);

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
                if let Err(error) = self
                    .workspaces
                    .set_remote_user(workspace_id, prepared.remote_user)
                {
                    Self::report_workspace_error("update remote account", error);
                }
                self.observe_remote_workspace_runtime(workspace_id, generation, cx);
                if let Some(progress) = &attempt.progress {
                    let _ = progress.complete(window, cx);
                }
            }
            Err(failure) => {
                let next = remote_workspace_reconnect_failure_state(&failure, generation);
                let _ = self
                    .workspaces
                    .reduce_remote_connection_state(workspace_id, next);
                if matches!(failure, RemoteWorkspaceReconnectFailure::Cancelled) {
                    if let Some(progress) = &attempt.progress {
                        let _ = progress.dismiss(window, cx);
                    }
                } else {
                    if let Some(progress) = &attempt.progress {
                        let _ = progress.fail(window, cx);
                    }
                    self.present_remote_workspace_reconnect_error(failure, window, cx);
                }
            }
        }

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
        identity: RemoteWorkspaceReconnectProgressIdentity,
        outcome: ProgressDialogOutcome,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(attempt) = self.remote_workspace_reconnect.as_ref() else {
            return;
        };
        if attempt.workspace_id != identity.workspace_id
            || attempt.generation != identity.reconnect_generation
            || attempt.progress_generation != identity.presentation_generation
        {
            return;
        }
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
        attempt.cancelled.store(true, Ordering::Release);
        self.remote_workspace_reconnect = None;
        let _ = self.workspaces.reduce_remote_connection_state(
            identity.workspace_id,
            RemoteConnectionState::disconnected(identity.reconnect_generation),
        );

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

    fn handle_directory_picker_event(
        &mut self,
        event: &DirectoryPickerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            DirectoryPickerEvent::StateChanged => {
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            }
            DirectoryPickerEvent::Escaped => {
                self.transient.pin_target = None;
            }
            DirectoryPickerEvent::DirectorySelectionRequested => {
                let identity = self
                    .transient
                    .picker
                    .read(cx)
                    .directory_selection_request_identity();
                let selection = self.directory_selection_fallback.choose(cx);
                cx.spawn_in(window, async move |manager, cx| {
                    let result = selection.await;
                    let _ = manager.update_in(cx, |manager, window, cx| {
                        manager.transient.picker.update(cx, |picker, cx| {
                            picker
                                .complete_directory_selection_request(identity, result, window, cx);
                        });
                        manager.sync_terminal_focus_blocker(window, cx);
                        cx.notify();
                    });
                })
                .detach();
            }
            DirectoryPickerEvent::Confirmed(directory) => {
                let activated = self.transient.pin_target.is_some_and(|workspace_id| {
                    self.apply_validated_local_pin(workspace_id, directory.clone(), window, cx)
                });
                if activated {
                    self.transient.pin_target = None;
                }
                let picker = self.transient.picker.clone();
                let owner = cx.entity();
                window.defer(cx, move |window, cx| {
                    picker.update(cx, |picker, cx| {
                        if activated {
                            picker.complete_activation(window, cx);
                        } else {
                            picker.activation_failed(window, cx);
                        }
                    });
                    if activated {
                        owner.update(cx, |manager, cx| {
                            manager.sync_terminal_focus_blocker(window, cx);
                            manager.focus(window, cx);
                            cx.notify();
                        });
                    }
                });
            }
        }
    }

    fn local_home_directory(&self) -> Result<ValidatedLocalDirectory, LocalFilesystemError> {
        self.local_filesystem
            .validate_directory(&self.local_home_directory_path)
    }

    fn show_home_directory_unavailable(window: &mut Window, cx: &mut Context<Self>) {
        let _ = Alert::new(
            ModalId::new("workspace-home-directory-unavailable"),
            "Home directory unavailable",
            "Home Directory Unavailable",
            "A workspace could not be created. Check that your home directory exists and is accessible, then try again.",
            vec![ModalAction::new((), "OK", ModalActionRole::Cancel, "workspace-home-error-ok")],
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, |_, _| {});
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

        self.synchronize_tab_manager_layout(workspace_id, window, cx);
        let preserve_sidebar_focus = self.sidebar.read(cx).is_focused(window)
            || self.sidebar.read(cx).rename_is_focused(window)
            || self.sidebar.read(cx).menu_target().is_some();
        if previous_workspace_id != workspace_id {
            previous_manager.update(cx, |manager, cx| manager.deactivate(cx));
            self.sidebar.update(cx, |sidebar, cx| {
                sidebar.cancel_rename(cx);
            });
        }
        if preserve_sidebar_focus {
            next_manager.update(cx, |manager, cx| manager.activate_without_focus(cx));
        } else {
            next_manager.update(cx, |manager, cx| manager.activate(window, cx));
        }
        self.sync_terminal_focus_blocker(window, cx);
        self.scroll_active_workspace_into_view(cx);

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

    fn close_target_requires_confirmation(&self, target: CloseTarget, cx: &App) -> Option<bool> {
        self.close_hierarchy(cx).requires_confirmation(target)
    }

    fn close_hierarchy(&self, cx: &App) -> CloseHierarchy {
        let mut hierarchy = CloseHierarchy::default();
        for workspace in self.workspaces.iter() {
            for (tab, pane, terminal) in workspace.payload().read(cx).terminal_panes(cx) {
                hierarchy.insert(workspace.id(), tab, pane, terminal.close_facts());
            }
        }
        hierarchy
    }

    fn request_close(&mut self, target: CloseTarget, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_confirmation.pending().is_some() {
            return;
        }
        let Some(requires_confirmation) = self.close_target_requires_confirmation(target, cx)
        else {
            return;
        };
        if !requires_confirmation {
            self.commit_close_target(target, window, cx);
            return;
        }
        self.present_close_confirmation(target, window, cx);
    }

    fn present_close_confirmation(
        &mut self,
        target: CloseTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(window_handle) = window.window_handle().downcast::<WorkspaceManager>() else {
            return;
        };
        let Some(generation) = self.close_confirmation.begin(target) else {
            return;
        };

        let scope = target.scope();
        let count = self.close_hierarchy(cx).affected_pane_count(target);
        let noun = if count == 1 { "Pane" } else { "Panes" };
        let result = Alert::new(
            ModalId::new("close-confirmation"),
            scope.title(),
            scope.title(),
            format!("Close {count} {noun}? Running commands in these Panes will stop."),
            vec![
                ModalAction::new(
                    CloseConfirmationAction::Confirm,
                    scope.destructive_label(),
                    ModalActionRole::Affirmative,
                    "close-confirmation-confirm",
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    CloseConfirmationAction::Cancel,
                    "Cancel",
                    ModalActionRole::Cancel,
                    "close-confirmation-cancel",
                ),
            ],
        )
        .intent(AlertIntent::Critical)
        .present(window, cx, move |outcome, cx| {
            let confirmed = matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: CloseConfirmationAction::Confirm,
                    ..
                }
            );
            let _ = window_handle.update(cx, |manager, window, cx| {
                manager.settle_close_confirmation(generation, target, confirmed, window, cx);
            });
        });

        if let Err(error) = result {
            self.close_confirmation.presentation_failed(generation);
            eprintln!("failed to present close confirmation: {error}");
        }
    }

    fn settle_close_confirmation(
        &mut self,
        generation: u64,
        target: CloseTarget,
        confirmed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hierarchy = self.close_hierarchy(cx);
        if let Some(target) = self
            .close_confirmation
            .settle(generation, target, confirmed, &hierarchy)
        {
            self.commit_close_target(target, window, cx);
        }
    }

    fn commit_close_target(
        &mut self,
        target: CloseTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            CloseTarget::Pane {
                workspace_id,
                tab_id,
                pane_id,
            } => {
                let Some(manager) = self
                    .workspaces
                    .workspace(workspace_id)
                    .map(|workspace| workspace.payload().clone())
                else {
                    return;
                };
                manager.update(cx, |manager, cx| {
                    manager.close_pane_authorized(tab_id, pane_id, window, cx)
                });
            }
            CloseTarget::Tab {
                workspace_id,
                tab_id,
            } => {
                let Some(manager) = self
                    .workspaces
                    .workspace(workspace_id)
                    .map(|workspace| workspace.payload().clone())
                else {
                    return;
                };
                manager.update(cx, |manager, cx| {
                    manager.close_tab_authorized(tab_id, window, cx)
                });
            }
            CloseTarget::Workspace(workspace_id) => {
                if self.workspaces.workspace(workspace_id).is_some() {
                    self.close_workspace(workspace_id, window, cx);
                }
            }
            CloseTarget::Window => window.remove_window(),
            CloseTarget::Application => cx.quit(),
        }
    }

    pub(crate) fn should_close_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.close_confirmation.pending().is_some() {
            return false;
        }
        if self
            .close_target_requires_confirmation(CloseTarget::Window, cx)
            .is_some_and(|requires| !requires)
        {
            return true;
        }
        self.present_close_confirmation(CloseTarget::Window, window, cx);
        false
    }

    pub(crate) fn request_application_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request_close(CloseTarget::Application, window, cx);
    }

    pub(crate) fn blocks_unconfirmed_application_quit(&self, cx: &App) -> bool {
        self.close_confirmation.pending().is_some()
            || self.close_target_requires_confirmation(CloseTarget::Application, cx) == Some(true)
    }

    fn close_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let replacement = if self.workspaces.len() == 1 {
            match self.local_home_directory() {
                Ok(directory) => directory,
                Err(_) => {
                    Self::show_home_directory_unavailable(window, cx);
                    return;
                }
            }
        } else {
            ValidatedLocalDirectory::new(
                self.local_home_directory_path.clone(),
                self.local_home_identity.clone(),
            )
        };
        self.begin_remote_workspace_close(workspace_id, window, cx);
        let was_active = self.workspaces.active_workspace_id() == workspace_id;
        let local_filesystem = self.local_filesystem.clone();
        let session_factory = Rc::clone(&self.session_factory);
        let pane_construction = self.pane_construction.clone();
        let window_drag_platform = Rc::clone(&self.operating_system_window_drag_platform);
        let sidebar_visible = self.sidebar.read(cx).layout().visible;
        let sidebar_width = self.sidebar.read(cx).layout().width;
        let replacement_identity = replacement.identity();
        let outcome = self.workspaces.close_workspace_with_local_replacement(
            workspace_id,
            replacement,
            |replacement_workspace_id, home_directory| {
                Self::create_local_tab_manager(
                    WorkspaceTerminalSessionFactory::new_local_with_authority(
                        session_factory,
                        ValidatedLocalDirectory::new(
                            home_directory.to_path_buf(),
                            replacement_identity,
                        ),
                        local_filesystem.clone(),
                    ),
                    TabManagerCreation {
                        workspace_id: replacement_workspace_id,
                        sidebar_visible,
                        sidebar_width,
                        operating_system_window_drag_platform: window_drag_platform,
                        pane_construction,
                    },
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
        let closed_manager = match outcome {
            CloseWorkspaceOutcome::WorkspaceClosed { payload, .. }
            | CloseWorkspaceOutcome::FinalWorkspaceReplaced { payload, .. } => payload,
        };
        closed_manager.update(cx, |manager, cx| manager.close_all(cx));
        self.remote_workspace_runtimes.remove(&workspace_id);
        self.debug_assert_remote_runtime_invariants();
        self.synchronize_tab_manager_layout(self.workspaces.active_workspace_id(), window, cx);

        if was_active {
            let active_manager = self.workspaces.active_workspace().payload().clone();
            if self.sidebar.read(cx).is_focused(window)
                || self.sidebar.read(cx).rename_is_focused(window)
            {
                active_manager.update(cx, |manager, cx| manager.activate_without_focus(cx));
            } else {
                active_manager.update(cx, |manager, cx| manager.activate(window, cx));
            }
        }
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.cancel_rename_for(workspace_id, cx)
        });
        self.sync_terminal_focus_blocker(window, cx);
        self.scroll_active_workspace_into_view(cx);

        cx.notify();
    }

    fn close_workspace_for_final_tab(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_remote_workspace_close(workspace_id, window, cx);
        let was_active = self.workspaces.active_workspace_id() == workspace_id;
        let outcome = match self.workspaces.close_workspace_for_final_tab(workspace_id) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.pending_final_tab_closes.remove(&workspace_id);
                Self::report_workspace_error("close for final Tab", error);
                return;
            }
        };

        match outcome {
            FinalTabCloseOutcome::WorkspaceClosed {
                closed_workspace_id,
                active_workspace_id,
                payload,
            } => {
                debug_assert_eq!(closed_workspace_id, workspace_id);
                payload.update(cx, |manager, cx| manager.close_all(cx));
                self.remote_workspace_runtimes.remove(&workspace_id);
                self.debug_assert_remote_runtime_invariants();
                self.synchronize_tab_manager_layout(
                    self.workspaces.active_workspace_id(),
                    window,
                    cx,
                );

                if was_active {
                    let active_manager = self.workspaces.active_workspace().payload().clone();
                    if self.sidebar.read(cx).is_focused(window)
                        || self.sidebar.read(cx).rename_is_focused(window)
                    {
                        active_manager.update(cx, |manager, cx| manager.activate_without_focus(cx));
                    } else {
                        active_manager.update(cx, |manager, cx| manager.activate(window, cx));
                    }
                }
                debug_assert_eq!(active_workspace_id, self.workspaces.active_workspace_id());
                self.pending_final_tab_closes.remove(&workspace_id);
                self.sidebar.update(cx, |sidebar, cx| {
                    sidebar.cancel_rename_for(workspace_id, cx)
                });
                self.sync_terminal_focus_blocker(window, cx);
                self.scroll_active_workspace_into_view(cx);

                cx.notify();
            }
            FinalTabCloseOutcome::CloseOperatingSystemWindow {
                workspace_id: final_workspace_id,
            } => {
                debug_assert_eq!(final_workspace_id, workspace_id);
                let manager = self.workspaces.active_workspace().payload().clone();
                manager.update(cx, |manager, cx| manager.close_all(cx));
                self.remote_workspace_runtimes.remove(&workspace_id);
                self.pending_final_tab_closes.remove(&workspace_id);
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
        if self.transient.pin_target == Some(workspace_id) {
            self.pin_operation = self.pin_operation.wrapping_add(1);
            self.transient.pin_target = None;
            self.transient
                .picker
                .update(cx, |picker, cx| picker.dismiss(window, cx));
            if let Some(picker) = self.remote_pin_picker.take() {
                picker.update(cx, |picker, cx| picker.cancel(window, cx));
            }
        }

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
            if let Some(progress) = &attempt.progress {
                let _ = progress.dismiss(window, cx);
            }
        }
    }

    fn handle_sidebar_event(
        &mut self,
        event: SidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SidebarEvent::Activate {
                workspace_id,
                focus_pane,
            } => {
                if self.activate_workspace(workspace_id, window, cx) && focus_pane {
                    self.focus(window, cx);
                }
            }
            SidebarEvent::Command {
                workspace_id,
                command,
            } => self.perform_workspace_menu_command(workspace_id, command, window, cx),
            SidebarEvent::Rename { workspace_id, name } => {
                if let Err(error) = self.workspaces.rename_workspace(workspace_id, name) {
                    Self::report_workspace_error("rename", error);
                }
                self.synchronize_tab_manager_layouts(window, cx);
            }
            SidebarEvent::NewLocalWorkspace => self.create_local_workspace(window, cx),
            SidebarEvent::NewRemoteWorkspace => {
                self.present_remote_workspace_flow(String::new(), window, cx)
            }
            SidebarEvent::LayoutChanged => self.synchronize_tab_manager_layouts(window, cx),
            SidebarEvent::FocusPane => self.focus(window, cx),
            SidebarEvent::FocusChanged => {}
        }
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.toggle(window, cx));
    }
    fn toggle_sidebar_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.toggle_focus(window, cx));
    }
    fn perform_workspace_menu_command(
        &mut self,
        workspace_id: WorkspaceId,
        command: WorkspaceMenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            WorkspaceMenuCommand::PinDirectory => {
                self.open_pin_directory_picker(workspace_id, window, cx)
            }
            WorkspaceMenuCommand::UnpinDirectory => {
                self.apply_directory_pin(workspace_id, None, window, cx);
            }
            WorkspaceMenuCommand::NewTab => {
                if let Some(workspace) = self.workspaces.workspace(workspace_id) {
                    workspace
                        .payload()
                        .update(cx, |manager, cx| manager.create_tab(window, cx));
                }
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            }
            WorkspaceMenuCommand::Reconnect => {
                self.start_remote_workspace_reconnect(workspace_id, window, cx)
            }
            WorkspaceMenuCommand::Close => {
                self.request_close(CloseTarget::Workspace(workspace_id), window, cx)
            }
        }
    }

    fn on_switch_workspace(
        &mut self,
        _: &SwitchWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_workspace_switcher(window, cx);
    }

    fn on_new_workspace(&mut self, _: &NewWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        if window_combo_box_is_open(window, cx) {
            self.workspace_switcher.accept_matching(
                |choice| matches!(choice, WorkspaceSwitcherChoice::Local(_)),
                window,
                cx,
            );
            return;
        }
        self.create_local_workspace(window, cx);
    }

    fn on_new_remote_workspace(
        &mut self,
        _: &NewRemoteWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window_combo_box_is_open(window, cx) {
            self.workspace_switcher.accept_matching(
                |choice| matches!(choice, WorkspaceSwitcherChoice::Remote(_)),
                window,
                cx,
            );
            return;
        }
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.dismiss_editing(window, cx);
        });
        self.present_remote_workspace_flow(String::new(), window, cx);
    }

    fn on_close_workspace(
        &mut self,
        _: &CloseWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace_id = self.workspaces.active_workspace_id();
        self.request_close(CloseTarget::Workspace(workspace_id), window, cx);
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
    fn workspace_chrome_identity(&self, cx: &App) -> (WorkspaceChromeIdentity, Tooltip) {
        let appearance = super::appearance::chrome(cx);
        let workspace = self.workspaces.active_workspace();
        let available = workspace.availability().is_available();
        let remote_connection_phase = workspace
            .remote_connection_state()
            .map(RemoteConnectionState::phase);
        let remote_status = remote_connection_phase.and_then(remote_connection_status);
        let name = workspace.name().to_owned();
        let (path, _) = directory_labels(
            workspace.location(),
            workspace.local_display_directory(),
            workspace.remote_display_directory(),
            &self.local_home_directory_path,
        );
        let foreground = gpui_color(if available {
            appearance.colors.text
        } else {
            appearance.colors.warning
        });
        let icon_color = gpui_color(if !available {
            appearance.colors.warning
        } else {
            appearance.colors.icon
        });
        let tooltip_detail = remote_status
            .map(|status| format!("{path}: {status}"))
            .unwrap_or_else(|| path.clone());

        (
            WorkspaceChromeIdentity {
                name: name.clone(),
                pinned: workspace.pinned_directory().is_some(),
                foreground,
                pin_color: icon_color,
            },
            Tooltip::new("workspace-switcher-tooltip", "Switch Workspace")
                .detail(format!("{name}\n{tooltip_detail}"))
                .debug_selector("workspace-switcher-tooltip"),
        )
    }

    fn render_top_left_chrome(
        &self,
        layout: WorkspaceChromeLayout,
        manager: WeakEntity<Self>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let appearance = super::appearance::chrome(cx);
        let chrome_icon_size = appearance.spacing(WORKSPACE_CHROME_ICON_SIZE);
        let fallback_icon_size = appearance.spacing(16.0);
        let placeholder_color = gpui_color(appearance.colors.text_placeholder);
        let sidebar_visible = self.sidebar.read(cx).layout().visible;
        let (toggle_icon, toggle_label) =
            sidebar_toggle_presentation(self.sidebar.read(cx).layout().visible);
        let drag_manager = manager.clone();
        let toggle_manager = manager.clone();
        let combo_lifecycle_manager = manager.clone();
        let combo_lifecycle_window = window.window_handle();
        let presentation = crate::desktop_profile::DesktopPresentation::get(cx);
        let remote_unavailable_reason = self.remote_workspace_unavailable_reason.clone();
        let new_workspace_shortcut = presentation.shortcut(&NewWorkspace);
        let new_remote_workspace_shortcut = presentation.shortcut(&NewRemoteWorkspace);
        let chooser = ComboBox::new(
            "workspace-switcher",
            "Switch Workspace",
            Some(WorkspaceSwitcherChoice::Workspace(
                self.workspaces.active_workspace_id(),
            )),
            "Switch Workspace",
            self.workspace_switcher_items(cx),
        )
        .handle(self.workspace_switcher.clone())
        .input_leading(move || {
            Icon::custom(
                CustomIconName::FilterCircle,
                chrome_icon_size,
                placeholder_color,
            )
            .into_any_element()
        })
        .copy(ComboBoxCopy::new(
            "Workspace name",
            "Filter or create...",
            "No Workspaces",
            "No matching Workspaces",
        ))
        .fallback(ComboBoxFallback::pinned_rows(move |query| {
            let name = query.trim().to_owned();
            let local = ComboBoxItem::new(
                WorkspaceSwitcherChoice::Local(name.clone()),
                "Local Workspace",
            )
            .leading_icon(move |foreground| {
                Icon::custom(
                    CustomIconName::RectangleStackBadgePlus,
                    fallback_icon_size,
                    foreground,
                )
                .into_any_element()
            })
            .shortcut(new_workspace_shortcut)
            .debug_selector("workspace-switcher-create-local");
            let mut remote =
                ComboBoxItem::new(WorkspaceSwitcherChoice::Remote(name), "Remote Workspace")
                    .shortcut(new_remote_workspace_shortcut)
                    .leading_icon(move |foreground| {
                        Icon::custom(CustomIconName::GlobePlus, fallback_icon_size, foreground)
                            .into_any_element()
                    })
                    .debug_selector("workspace-switcher-create-remote");
            if let Some(reason) = &remote_unavailable_reason {
                remote = remote
                    .disabled(true)
                    .description(reason.clone())
                    .trailing(ComboBoxAccessory::Status("Unavailable".into()));
            }
            vec![local, remote]
        }))
        .icon_trigger(move |foreground| {
            Icon::custom(CustomIconName::RectangleStack, chrome_icon_size, foreground)
                .into_any_element()
        })
        .placement(AnchoredPlacementConfig::new(
            AnchoredPlacement::Bottom,
            AnchoredAlignment::End,
        ))
        .panel_width(
            self.sidebar.read(cx).layout().width
                - appearance.spacing(SIDEBAR_ROW_HORIZONTAL_PADDING * 2.0),
        )
        .debug_selector("workspace-switcher")
        .tooltip(
            Tooltip::new("workspace-switcher-tooltip", "Switch Workspace")
                .debug_selector("workspace-switcher-tooltip")
                .keyboard_equivalent(presentation.shortcut(&SwitchWorkspace)),
        )
        .when(!sidebar_visible, |chooser| {
            let (identity, tooltip) = self.workspace_chrome_identity(cx);
            chooser
                .custom_trigger(identity.render(gpui_color(appearance.colors.text), appearance))
                .full_width(true)
                .tooltip(tooltip.keyboard_equivalent(presentation.shortcut(&SwitchWorkspace)))
        })
        .on_lifecycle(move |_, cx| {
            let manager = combo_lifecycle_manager.clone();
            cx.defer(move |cx| {
                let _ = cx.update_window(combo_lifecycle_window, |_, window, cx| {
                    let _ = manager.update(cx, |manager, cx| {
                        manager.sync_terminal_focus_blocker(window, cx);
                        cx.notify();
                    });
                });
            });
        })
        .on_accept(move |acceptance, window, cx| {
            let _ = manager.update(cx, |manager, cx| {
                match acceptance.item_id().clone() {
                    WorkspaceSwitcherChoice::Workspace(id) => {
                        manager.activate_workspace(id, window, cx);
                    }
                    WorkspaceSwitcherChoice::Local(name) => {
                        manager.create_named_local_workspace(Some(name), window, cx)
                    }
                    WorkspaceSwitcherChoice::Remote(name) => {
                        manager.present_remote_workspace_flow(name, window, cx)
                    }
                }
                manager.sync_terminal_focus_blocker(window, cx);
            });
        });
        let content = div()
            .relative()
            .size_full()
            .child(
                div()
                    .id("workspace-top-chrome-bottom-divider")
                    .debug_selector(|| "workspace-top-chrome-bottom-divider".to_owned())
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .w_full()
                    .h(appearance.spacing(CHROME_DIVIDER_SIZE))
                    .bg(gpui_color(appearance.colors.border)),
            )
            .child(
                layout.render_controls(
                    IconButton::new("toggle-sidebar-button", toggle_label, move |foreground| {
                        Icon::new(toggle_icon, chrome_icon_size, foreground).into_any_element()
                    })
                    .variant(ButtonVariant::Ghost)
                    .size(TOGGLE_SIZE)
                    .debug_selector("toggle-sidebar-button")
                    .tooltip(
                        Tooltip::new("toggle-sidebar-tooltip", toggle_label)
                            .debug_selector("toggle-sidebar-tooltip"),
                    )
                    .on_activate(move |_, window, cx| {
                        let _ = toggle_manager.update(cx, |manager, cx| {
                            manager.toggle_sidebar(window, cx);
                        });
                    }),
                    chooser,
                    cx,
                ),
            );
        let drag_region = WindowDragRegion::new(
            "workspace-top-chrome-drag-region",
            "Move Operating-System Window from Workspace chrome",
            content,
        )
        .status(self.window_drag_status.clone())
        .pointer_insets(Edges {
            right: super::resize_handle_theme::spacious_target_half_thickness(cx),
            ..Edges::default()
        })
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
            .w(layout.width)
            .h(appearance.top_height())
            .bg(gpui_color(if window.is_window_active() {
                appearance.colors.title_bar_background
            } else {
                appearance.colors.title_bar_inactive_background
            }))
            .child(drag_region)
            .into_any_element()
    }

    fn sidebar_rows(&self, cx: &App) -> Vec<WorkspaceRowViewModel> {
        let active_workspace_id = self.workspaces.active_workspace_id();
        self.workspaces
            .iter()
            .map(|workspace| {
                let (tab_count, pane_count) = workspace.payload().read(cx).aggregate_counts(cx);
                let (path, directory_tooltip) = directory_labels(
                    workspace.location(),
                    workspace.local_display_directory(),
                    workspace.remote_display_directory(),
                    &self.local_home_directory_path,
                );
                let (available, tooltip) = match workspace.availability() {
                    DirectoryAvailability::Available => (true, directory_tooltip),
                    DirectoryAvailability::Unavailable { reason } => {
                        (false, format!("{directory_tooltip}: {reason}"))
                    }
                };
                WorkspaceRowViewModel {
                    workspace_id: workspace.id(),
                    name: workspace.name().to_owned().into(),
                    path: path.into(),
                    machine: match workspace.location() {
                        WorkspaceLocation::Local => None,
                        WorkspaceLocation::Remote { key, .. } => Some(
                            key.destination()
                                .as_str()
                                .rsplit_once('@')
                                .map_or(key.destination().as_str(), |(_, host)| host)
                                .to_owned()
                                .into(),
                        ),
                    },
                    tooltip: tooltip.into(),
                    pinned: workspace.pinned_directory().is_some(),
                    remote_connection_phase: workspace
                        .remote_connection_state()
                        .map(RemoteConnectionState::phase),
                    available,
                    tab_count,
                    pane_count,
                    active: workspace.id() == active_workspace_id,
                }
            })
            .collect()
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
        let active_tab_manager = self.workspaces.active_workspace().payload().clone();
        let workspace = self.workspaces.active_workspace();
        let sidebar_layout = self.sidebar.read(cx).layout();
        let chrome = WorkspaceChromeLayout::resolve(
            sidebar_layout,
            workspace.name(),
            workspace.pinned_directory().is_some(),
            window,
            cx,
        );
        active_tab_manager.update(cx, |manager, cx| {
            manager.set_sidebar_layout(
                sidebar_layout.visible,
                sidebar_layout.width,
                chrome.width,
                cx,
            );
        });
        let rows = self.sidebar_rows(cx);
        let remote_unavailable = self.remote_workspace_unavailable_reason.clone();
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_rows(rows, remote_unavailable, cx)
        });
        let content = div()
            .id("workspace-manager")
            .debug_selector(|| "workspace-manager".to_owned())
            .key_context(TERMINAL_KEY_CONTEXT)
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .font(super::appearance::chrome(cx).regular.clone())
            .bg(gpui_color(super::appearance::chrome(cx).colors.background))
            .on_action(cx.listener(Self::on_switch_workspace))
            .on_action(cx.listener(Self::on_new_workspace))
            .on_action(cx.listener(Self::on_new_remote_workspace))
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
            .on_action(cx.listener(Self::forward_active_terminal_action::<CreateTab>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateTab1>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateTab2>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateTab3>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateTab4>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateTab5>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateTab6>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateTab7>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateTab8>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ActivateTab9>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ClosePane>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<CloseTab>))
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
            .child(active_tab_manager)
            .children(self.remote_workspace_flow.iter().cloned())
            .children(self.remote_pin_picker.iter().cloned())
            .when(self.sidebar.read(cx).layout().visible, |root| {
                root.child(self.sidebar.clone())
            })
            .child(self.sidebar.read(cx).render_resize_handle(
                self.sidebar.downgrade(),
                chrome.width,
                super::appearance::chrome(cx).top_height(),
            ))
            .child(self.render_top_left_chrome(chrome, manager.clone(), window, cx));
        let content = content.child(self.transient.picker.clone());
        ModalLayer::new(TooltipLayer::new(content))
    }
}

fn workspace_activation_shortcut(
    index: usize,
    presentation: &crate::desktop_profile::DesktopPresentation,
) -> Option<&'static str> {
    Some(match index {
        0 => presentation.shortcut(&ActivateWorkspace1),
        1 => presentation.shortcut(&ActivateWorkspace2),
        2 => presentation.shortcut(&ActivateWorkspace3),
        3 => presentation.shortcut(&ActivateWorkspace4),
        4 => presentation.shortcut(&ActivateWorkspace5),
        5 => presentation.shortcut(&ActivateWorkspace6),
        6 => presentation.shortcut(&ActivateWorkspace7),
        7 => presentation.shortcut(&ActivateWorkspace8),
        8 => presentation.shortcut(&ActivateWorkspace9),
        _ => return None,
    })
}

fn classify_remote_workspace_restart_failure(
    error: RemoteTabManagerLifecycleError,
) -> RemoteWorkspaceReconnectFailure {
    match error {
        RemoteTabManagerLifecycleError::Revalidation(
            crate::terminal::RemoteChannelRevalidationError::DirectoryUnavailable,
        ) => RemoteWorkspaceReconnectFailure::DirectoryUnavailable,
        RemoteTabManagerLifecycleError::Revalidation(
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
            "SpaceTerm can’t access the selected remote directory. Check its permissions or reopen the Remote Workspace."
                .to_owned(),
        )),
        RemoteWorkspaceReconnectFailure::IdentityChanged => Some((
            "Remote Directory Changed",
            "The selected remote path now resolves to a different directory. Reopen the Remote Workspace to review it."
                .to_owned(),
        )),
    }
}

pub(super) fn gpui_color(color: Color) -> gpui::Rgba {
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

fn directory_labels(
    location: &WorkspaceLocation,
    local_directory: Option<&std::path::Path>,
    remote_directory: Option<&RemoteDirectory>,
    local_home: &std::path::Path,
) -> (String, String) {
    match (location, local_directory, remote_directory) {
        (WorkspaceLocation::Local, Some(directory), None) => (
            compact_home_path(directory, local_home),
            directory.display().to_string(),
        ),
        (
            WorkspaceLocation::Remote {
                key,
                remote_user,
                remote_home_identity,
                ..
            },
            None,
            Some(directory),
        ) => {
            let destination = key.destination().as_str();
            let origin = if destination.contains('@') {
                destination.to_owned()
            } else {
                format!("{}@{destination}", remote_user.as_str())
            };
            let compact_directory = compact_remote_home_path(directory, remote_home_identity);
            (
                compact_directory,
                format!("{origin}:{}", directory.as_str()),
            )
        }
        _ => {
            unreachable!("a Workspace must own exactly one local or remote directory")
        }
    }
}

fn compact_remote_home_path(
    directory: &RemoteDirectory,
    home: &crate::domain::RemoteDirectoryIdentity,
) -> String {
    if directory.is_home_spelling(home) {
        return "~".to_owned();
    }
    directory
        .as_str()
        .strip_prefix(home.as_str())
        .filter(|relative| relative.starts_with('/'))
        .map(|relative| format!("~{relative}"))
        .unwrap_or_else(|| directory.as_str().to_owned())
}

/// The account facts a Remote Pane presents: who is logged in, and the home its paths shorten to.
fn remote_machine(account: &RemoteWorkspaceAccount) -> crate::terminal::metadata::RemoteMachine {
    crate::terminal::metadata::RemoteMachine::new(
        Some(account.user()),
        Some(account.home_identity().as_str()),
    )
}

fn initial_home_directory(
    path: PathBuf,
    local_filesystem: &LocalFilesystemAuthority,
) -> (ValidatedLocalDirectory, Option<String>) {
    #[cfg(test)]
    let _ = local_filesystem;
    #[cfg(test)]
    return (
        ValidatedLocalDirectory::new(path, LocalDirectoryIdentity::for_test(0)),
        None,
    );
    #[cfg(not(test))]
    match local_filesystem.validate_directory(&path) {
        Ok(directory) => (directory, None),
        Err(error) => (
            ValidatedLocalDirectory::new(path, LocalDirectoryIdentity::unavailable()),
            Some(error.to_string()),
        ),
    }
}

#[cfg(test)]
impl WorkspaceManager {
    pub(crate) fn workspace_count(&self) -> usize {
        self.workspaces.len()
    }

    pub(crate) fn assert_application_capabilities(
        &self,
        expected: &crate::app::ApplicationCapabilities,
        _: &App,
    ) {
        assert!(
            self.local_filesystem
                .same_source(&expected.local_filesystem)
        );
        self.pane_construction
            .assert_application_capabilities(expected);
    }
}

#[cfg(test)]
#[path = "workspace_manager/tests.rs"]
mod tests;
