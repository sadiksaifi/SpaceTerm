use super::chrome_icons::IconRole;
use super::pane_lifecycle::{PaneConstruction, PaneLifecycleDependencies};
use super::workspace_chrome::{
    TOGGLE_SIZE, WorkspaceChromeIdentity, WorkspaceChromeLayout, WorkspaceChromeStatus,
    WorkspaceChromeStatusHosts,
};
#[cfg(test)]
use super::workspace_sidebar::{
    SIDEBAR_MAXIMUM_WIDTH, SIDEBAR_ROW_HEIGHT, TERMINAL_CONTENT_MINIMUM_WIDTH,
};
use super::workspace_sidebar::{
    SidebarEvent, WorkspaceMenuCommand, WorkspaceRowViewModel, WorkspaceSidebar,
    remote_connection_status,
};
use crate::platform::terminal_accessibility::TerminalAccessibilityAdapterFactory;
use crate::ssh::remote_account::RemoteWorkspaceAccount;
use crate::terminal::native_services::NativeServiceAdapters;
use crate::ui::appearance::gpui_color;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::chrome_typography::{ChromeTextStyleExt, TextRole};
use super::directory_picker::{
    DirectoryPicker, DirectoryPickerEvent, DirectorySource, LocalDirectorySource,
    RemoteDirectorySource,
};
use super::remote_workspace_flow::{
    ConnectedControlConnection, RemoteWorkspaceAliasPin, RemoteWorkspaceConnectContext,
    RemoteWorkspaceConnectionProgress, RemoteWorkspaceFlow, RemoteWorkspaceFlowBackend,
    RemoteWorkspaceFlowBackendError, RemoteWorkspaceFlowBackendFactory,
    RemoteWorkspaceFlowCompletion, RemoteWorkspaceFlowCompletionHandle, RemoteWorkspaceFlowEvent,
    RemoteWorkspaceStart,
};
use super::tab_manager::{PreparedTabManagerRemoteRestart, RemoteTabManagerLifecycleError};
use super::terminal_focus::{TerminalFocusBlocker, TerminalFocusCoordinator, WorkspaceFocusOwners};
use super::workspace_creation::WorkspaceCreation;
use super::{
    ActivateTab1, ActivateTab2, ActivateTab3, ActivateTab4, ActivateTab5, ActivateTab6,
    ActivateTab7, ActivateTab8, ActivateTab9, ActivateWorkspace1, ActivateWorkspace2,
    ActivateWorkspace3, ActivateWorkspace4, ActivateWorkspace5, ActivateWorkspace6,
    ActivateWorkspace7, ActivateWorkspace8, ActivateWorkspace9, ClosePane, CloseTab,
    CloseTerminalFind, CloseWorkspace, CopySelection, CreateTab, FindNext, FindPrevious,
    FocusNextPane, FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, FocusPreviousPane,
    MoveTabLeft, MoveTabRight, NewRemoteWorkspace, NewWorkspace, NextTab, OpenLocalDirectory,
    OpenRemoteDirectory, OpenTerminalFind, PreviousTab, RemoteChildLaunchUnavailable,
    ScrollPageDown, ScrollPageUp, ScrollToBottom, ScrollToTop, ShowRepositoryStatus, SplitDown,
    SplitRight, SwitchWorkspace, TERMINAL_KEY_CONTEXT, TabManager, TabManagerEvent, TogglePaneZoom,
    ToggleSidebar, ToggleSidebarFocus, WORKSPACE_SIDEBAR_DEFAULT_WIDTH,
};
use crate::appearance::Color;
use crate::close_confirmation::{
    ApplicationCloseFacts, ApplicationPaneFacts, CloseConfirmation, CloseHierarchy, CloseTarget,
};
use crate::directory_selection::SystemDirectorySelection;
use crate::domain::{
    CloseWorkspaceOutcome, DirectoryAvailability, FinalTabCloseOutcome, LocalDirectoryIdentity,
    PinnedDirectory, RemoteConnectionReduction, RemoteConnectionState, RemoteDirectory,
    RemoteWorkspaceTarget, ValidatedLocalDirectory, WorkspaceCollection, WorkspaceEntry,
    WorkspaceError, WorkspaceId, WorkspaceLocation,
};
use crate::platform::local_filesystem::{LocalFilesystemAuthority, LocalFilesystemError};
use crate::platform::window_movement::{
    OperatingSystemWindowDragError, OperatingSystemWindowDragPlatform,
};
use crate::ssh::live_connection::{ControlConnectionObserver, ControlConnectionTerminalState};
use crate::ssh::process::TransientSshErrorOutput;
use crate::terminal::metadata::RemoteTerminalMetadataContext;
use crate::terminal::{
    NativeServiceOrigin, NativeServiceStatus, PreparedWorkspaceTerminalLaunch, SelectionCopy,
    TerminalKeyInputAdapterFactory, TerminalSessionFactory, WorkspaceTerminalSessionFactory,
};
use gpui::prelude::*;

use gpui::{
    Action, AnyElement, App, Context, Edges, Entity, Pixels, Render, Task, WeakEntity, Window, div,
    px,
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, AnchoredAlignment, AnchoredPlacement,
    AnchoredPlacementConfig, ButtonVariant, ComboBox, ComboBoxAccessory, ComboBoxCommand,
    ComboBoxCopy, ComboBoxHandle, ComboBoxItem, CustomIconName, Icon, IconButton, IconName,
    ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId, ModalLayer,
    ProgressCancelDecision, ProgressCancellation, ProgressDialog, ProgressDialogHandle,
    ProgressDialogOutcome, ProgressDialogUpdate, ProgressState, Tooltip, WindowDragRegion,
    WindowDragRegionEvent, WindowDragRegionResponse, WindowDragRegionStatus,
    window_combo_box_is_open, window_modal_is_open,
};

#[cfg(test)]
const CHROME_DIVIDER_SIZE: f32 = super::control_theme::resize_handle::VISIBLE_THICKNESS;

fn sidebar_toggle_presentation(sidebar_visible: bool) -> (CustomIconName, &'static str) {
    if sidebar_visible {
        (CustomIconName::PanelLeft, "Hide Sidebar")
    } else {
        (CustomIconName::PanelRight, "Show Sidebar")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloseConfirmationAction {
    Confirm,
    Cancel,
}

struct RemoteWorkspaceRuntime {
    generation: u64,
    control_connection: Option<ConnectedControlConnection>,
    lifecycle: Option<ControlConnectionObserver>,
    alias_pin: Option<RemoteWorkspaceAliasPin>,
    /// Lends the connection's reader to Repository Status while the connection lives.
    repository_reader: Option<crate::ui::repository_status_store::RemoteReaderLease>,
}

impl RemoteWorkspaceRuntime {
    fn new(
        generation: u64,
        control_connection: ConnectedControlConnection,
        lifecycle: ControlConnectionObserver,
        alias_pin: Option<RemoteWorkspaceAliasPin>,
    ) -> Self {
        Self {
            generation,
            control_connection: Some(control_connection),
            lifecycle: Some(lifecycle),
            alias_pin,
            repository_reader: None,
        }
    }

    /// Drops the connection and withdraws its reader, so Repository Status reads the machine
    /// through another Workspace's live connection.
    fn drop_connection(&mut self) {
        self.repository_reader.take();
        self.control_connection.take();
    }

    fn close(&mut self) {
        self.lifecycle.take();
        self.drop_connection();
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
    control_connection: ConnectedControlConnection,
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

/// Owns directory selection and its target.
struct WorkspaceTransientUi {
    pin_target: Option<PinTarget>,
    local_selection_pending: bool,
}

/// What a directory chosen through the Directory Picker or System Directory Selection pins.
#[derive(Clone, Debug, Eq, PartialEq)]
enum PinTarget {
    /// An existing Workspace, whose Pinned Directory changes.
    Workspace(WorkspaceId),
    /// A new Local Workspace pinned from its first Terminal Session, named `name` unless blank.
    NewLocalWorkspace { name: String },
}

/// How applying a chosen directory to a pin target ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PinApplication {
    Applied,
    /// The directory could not be applied; the chooser that supplied it can retry.
    Failed,
    /// The attempt ended with an alert already presented to the user.
    Reported,
}

impl PinApplication {
    const fn from_applied(applied: bool) -> Self {
        if applied { Self::Applied } else { Self::Failed }
    }
}

/// The outcome of one System Directory Selection for a local pin.
enum LocalPinSelection {
    Chosen(Result<ValidatedLocalDirectory, LocalFilesystemError>),
    Cancelled,
    Failed,
}

pub(crate) struct WorkspaceManager {
    window_appearance: super::appearance_runtime::WindowAppearanceOwner,
    window_traffic_lights: super::appearance_runtime::WindowTrafficLightOwner,
    transient: WorkspaceTransientUi,
    workspace_switcher: ComboBoxHandle<WorkspaceId, WorkspaceCreation>,
    remote_workspace_name: Option<String>,
    sidebar: Entity<WorkspaceSidebar>,
    local_filesystem: LocalFilesystemAuthority,
    workspaces: WorkspaceCollection<Entity<TabManager>>,
    session_factory: Rc<dyn TerminalSessionFactory>,
    pane_construction: PaneConstruction,
    local_home_directory_path: PathBuf,
    local_home_identity: LocalDirectoryIdentity,
    directory_selection: Rc<dyn SystemDirectorySelection>,
    remote_workspace_backend: Option<Arc<dyn RemoteWorkspaceFlowBackend>>,
    remote_workspace_unavailable_reason: Option<String>,
    remote_workspace_flow: Option<Entity<RemoteWorkspaceFlow>>,
    pin_picker: Option<Entity<DirectoryPicker>>,
    pin_operation: u64,
    remote_workspace_runtimes: BTreeMap<WorkspaceId, RemoteWorkspaceRuntime>,
    remote_workspace_activation_task: Option<Task<()>>,
    remote_workspace_focus_restore_pending: bool,
    remote_workspace_reconnect: Option<RemoteWorkspaceReconnectAttempt>,
    operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
    window_drag_status: WindowDragRegionStatus,
    update_control: Entity<super::updates::UpdateControl>,
    pending_final_tab_closes: BTreeSet<WorkspaceId>,
    close_confirmation: CloseConfirmation,
    sidebar_repositories: repository::SidebarRepositories,
    sidebar_worktrees: worktrees::SidebarWorktrees,
}

impl WorkspaceManager {
    pub(crate) fn new_with_adapters(
        session_factory: Rc<dyn TerminalSessionFactory>,
        local_home_directory_path: PathBuf,
        adapters: WorkspaceManagerAdapters,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut window_appearance = super::appearance_runtime::WindowAppearanceOwner::default();
        window_appearance.apply(window, cx);
        let mut window_traffic_lights =
            super::appearance_runtime::WindowTrafficLightOwner::workspace();
        window_traffic_lights.apply(window, cx);
        cx.observe_global_in::<super::appearance_runtime::InstalledAppearance>(
            window,
            |manager, window, cx| {
                manager.window_appearance.apply(window, cx);
                manager.window_traffic_lights.apply(window, cx);
                cx.notify();
            },
        )
        .detach();
        let WorkspaceManagerAdapters {
            local_filesystem,
            key_input: key_input_adapter_factory,
            accessibility: accessibility_adapter_factory,
            native_services: native_service_adapters,
            lifecycle: lifecycle_dependencies,
            directory_selection,
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
        cx.on_release(|manager, cx| {
            manager.release_sidebar_repositories(cx);
            manager.release_sidebar_worktrees(cx);
        })
        .detach();
        let sidebar = cx.new(|cx| WorkspaceSidebar::new(window, cx));
        cx.subscribe_in(
            &sidebar,
            window,
            |manager, _, event: &SidebarEvent, window, cx| {
                manager.handle_sidebar_event(event.clone(), window, cx);
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
            if window.is_window_active()
                && let Some(store) =
                    crate::ui::repository_status_store::InstalledRepositoryStatus::store(cx)
            {
                store.update(cx, |store, cx| store.window_activated(cx));
            }
            if window.is_window_active() {
                Self::worktrees_window_activated(cx);
            }
            cx.notify();
        })
        .detach();

        Self {
            window_appearance,
            window_traffic_lights,
            transient: WorkspaceTransientUi {
                pin_target: None,
                local_selection_pending: false,
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
            directory_selection,
            remote_workspace_backend,
            remote_workspace_unavailable_reason: remote_unavailable_reason,
            remote_workspace_flow: None,
            pin_picker: None,
            pin_operation: 0,
            remote_workspace_runtimes: BTreeMap::new(),
            remote_workspace_activation_task: None,
            remote_workspace_focus_restore_pending: false,
            remote_workspace_reconnect: None,
            operating_system_window_drag_platform,
            window_drag_status: WindowDragRegionStatus::new(),
            update_control: cx.new(super::updates::UpdateControl::new),
            pending_final_tab_closes: BTreeSet::new(),
            close_confirmation: CloseConfirmation::default(),
            sidebar_repositories: repository::SidebarRepositories::default(),
            sidebar_worktrees: worktrees::SidebarWorktrees::default(),
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
    ) -> Result<Entity<TabManager>, crate::terminal::TerminalSessionChannelUnavailable> {
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

    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only a desktop Services Adapter queries Services state"
        )
    )]
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

    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only a desktop Services Adapter queries Services state"
        )
    )]
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

    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only a desktop Services Adapter queries Services state"
        )
    )]
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
            picker: self
                .pin_picker
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
            context_menu: self.sidebar.read(cx).menu_open(),
            sidebar: self.sidebar.read(cx).is_focused(window),
        })
    }

    fn workspace_switcher_items(&self, cx: &App) -> Vec<ComboBoxItem<WorkspaceId>> {
        let presentation = crate::desktop_profile::DesktopPresentation::get(cx);
        self.workspaces
            .iter()
            .enumerate()
            .map(|(index, workspace)| {
                let icon = match workspace.location() {
                    WorkspaceLocation::Local => IconName::Terminal,
                    WorkspaceLocation::Remote { .. } => IconName::Globe,
                };
                let item = ComboBoxItem::new(workspace.id(), workspace.name().to_owned())
                    .leading_icon(move |foreground, size| {
                        div()
                            .child(Icon::new(icon, size, foreground))
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

    pub(crate) fn open_workspace_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window_modal_is_open(window, cx) {
            return;
        }
        if let Some(picker) = self.pin_picker.take() {
            picker.update(cx, |picker, cx| picker.cancel(window, cx));
            self.transient.pin_target = None;
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
            &chrome_identity(workspace),
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
        self.create_pinned_local_workspace(name, None, window, cx);
    }

    /// Creates and activates a Local Workspace whose Terminal Sessions start in `pin`, including
    /// its first, or at the local home directory when there is no pin.
    fn create_pinned_local_workspace(
        &mut self,
        name: Option<String>,
        pin: Option<ValidatedLocalDirectory>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let previous_manager = self.workspaces.active_workspace().payload().clone();
        let pane_construction = self.pane_construction.clone();
        let window_drag_platform = Rc::clone(&self.operating_system_window_drag_platform);
        let sidebar_visible = self.sidebar.read(cx).layout().visible;
        let sidebar_width = self.sidebar.read(cx).layout().width;
        let directory = match self.local_home_directory() {
            Ok(directory) => directory,
            Err(_) => {
                Self::show_home_directory_unavailable(window, cx);
                return false;
            }
        };
        let mut session_factory = WorkspaceTerminalSessionFactory::new_local_with_authority(
            Rc::clone(&self.session_factory),
            directory.clone(),
            self.local_filesystem.clone(),
        );
        session_factory.set_pinned_directory(pin.clone().map(PinnedDirectory::Local));
        let launch_factory = if pin.is_some() {
            match session_factory.for_source_directory(None) {
                Ok(launch_factory) => launch_factory,
                Err(_) => return false,
            }
        } else {
            session_factory.clone()
        };
        let prepared_launch = match launch_factory.prepare_child_launch() {
            Ok(prepared_launch) => prepared_launch,
            Err(error) => unreachable!("Local initial launch preparation is infallible: {error}"),
        };
        let result = self
            .workspaces
            .create_local_workspace(directory, |workspace_id, _| {
                Self::create_tab_manager_with_prepared_launch(
                    session_factory,
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
            });
        let workspace_id = match result {
            Ok(workspace_id) => workspace_id,
            Err(error) => {
                Self::report_workspace_error("create", error);
                return false;
            }
        };
        if let Some(pin) = pin
            && let Err(error) = self
                .workspaces
                .set_pinned_directory(workspace_id, Some(PinnedDirectory::Local(pin)))
        {
            Self::report_workspace_error("pin", error);
        }
        if let Some(name) = name
            && let Err(error) = self.workspaces.rename_workspace(workspace_id, name)
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
        true
    }

    /// Activates the Local Workspace already pinned to `directory`, or creates one pinned to it.
    fn open_local_directory_workspace(
        &mut self,
        directory: ValidatedLocalDirectory,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PinApplication {
        let Ok(directory) = self.local_filesystem.revalidate_directory(&directory) else {
            return PinApplication::Failed;
        };
        let opened = match self
            .workspaces
            .local_workspace_pinned_to(&directory.identity())
        {
            Some(workspace_id) => self.activate_workspace(workspace_id, window, cx),
            None if self.local_home_directory().is_err() => {
                // The failure is not about the chosen directory, so end the attempt before
                // alerting.
                if let Some(picker) = self.pin_picker.take() {
                    picker.update(cx, |picker, cx| picker.cancel(window, cx));
                }
                self.transient.pin_target = None;
                Self::show_home_directory_unavailable(window, cx);
                return PinApplication::Reported;
            }
            None => self.create_pinned_local_workspace(Some(name), Some(directory), window, cx),
        };
        // A failure keeps the operation current so the still-open picker can retry.
        if opened {
            self.pin_operation = self.pin_operation.wrapping_add(1);
            self.transient.pin_target = None;
        }
        PinApplication::from_applied(opened)
    }

    /// Pins a validated local directory to `target`, creating the Workspace a new target names.
    fn apply_local_pin_target(
        &mut self,
        target: PinTarget,
        directory: ValidatedLocalDirectory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PinApplication {
        match target {
            PinTarget::Workspace(workspace_id) => PinApplication::from_applied(
                self.apply_validated_local_pin(workspace_id, directory, window, cx),
            ),
            PinTarget::NewLocalWorkspace { name } => {
                self.open_local_directory_workspace(directory, name, window, cx)
            }
        }
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

    /// Opens the Directory Picker for the target's machine. A local target also offers System
    /// Directory Selection from the picker.
    fn open_pin_picker(&mut self, target: PinTarget, window: &mut Window, cx: &mut Context<Self>) {
        let (location, placeholder) = match &target {
            PinTarget::Workspace(workspace_id) => {
                let Some(location) = self
                    .workspaces
                    .workspace(*workspace_id)
                    .map(|workspace| workspace.location().clone())
                else {
                    return;
                };
                (location, "Pin to Directory")
            }
            PinTarget::NewLocalWorkspace { .. } => (
                WorkspaceLocation::Local,
                crate::keybindings::Command::OpenLocalDirectory.label(),
            ),
        };
        let (source, remote_generation): (Rc<dyn DirectorySource>, Option<u64>) = match location {
            WorkspaceLocation::Local => (
                Rc::new(LocalDirectorySource::new(
                    self.local_filesystem.clone(),
                    self.local_home_directory_path.clone(),
                    cx.background_executor().clone(),
                )),
                None,
            ),
            WorkspaceLocation::Remote { key, .. } => {
                let PinTarget::Workspace(workspace_id) = target else {
                    unreachable!("only an existing Workspace can be remote");
                };
                let Some(runtime) = self.remote_workspace_runtimes.get(&workspace_id) else {
                    return;
                };
                let Some(control_connection) = runtime.control_connection.as_ref() else {
                    Self::show_pin_error(window, cx);
                    return;
                };
                let host = key.destination().host().to_owned();
                (
                    Rc::new(RemoteDirectorySource::new(
                        control_connection.provider(),
                        &host,
                    )),
                    Some(runtime.generation),
                )
            }
        };
        let system_selection = remote_generation.is_none().then(|| {
            gpui::SharedString::from(
                crate::desktop_profile::DesktopPresentation::get(cx)
                    .wording()
                    .system_directory_selection,
            )
        });
        let operation = self.pin_operation;
        let picker = cx.new(|cx| {
            let picker = DirectoryPicker::new(source, placeholder, window, cx);
            match system_selection {
                Some(label) => picker.with_system_selection(label),
                None => picker,
            }
        });
        cx.subscribe_in(
            &picker,
            window,
            move |manager, picker, event: &DirectoryPickerEvent, window, cx| {
                if manager.pin_picker.as_ref() != Some(picker) {
                    return;
                }
                match event {
                    DirectoryPickerEvent::Confirmed(pinned) => {
                        let current = manager.pin_operation == operation
                            && remote_generation.is_none_or(|generation| {
                                let PinTarget::Workspace(workspace_id) = &target else {
                                    return false;
                                };
                                manager
                                    .remote_workspace_runtimes
                                    .get(workspace_id)
                                    .is_some_and(|runtime| {
                                        runtime.generation == generation
                                            && runtime.control_connection.is_some()
                                    })
                            });
                        let application = match (current, pinned.clone(), target.clone()) {
                            (false, _, _)
                            | (
                                _,
                                PinnedDirectory::Remote { .. },
                                PinTarget::NewLocalWorkspace { .. },
                            ) => PinApplication::Failed,
                            (true, PinnedDirectory::Local(directory), target) => {
                                manager.apply_local_pin_target(target, directory, window, cx)
                            }
                            (true, remote, PinTarget::Workspace(workspace_id)) => {
                                PinApplication::from_applied(manager.apply_directory_pin(
                                    workspace_id,
                                    Some(remote),
                                    window,
                                    cx,
                                ))
                            }
                        };
                        // A reported failure already closed the picker.
                        if application == PinApplication::Reported {
                            return;
                        }
                        let applied = application == PinApplication::Applied;
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
                                    manager.pin_picker = None;
                                    manager.sync_terminal_focus_blocker(window, cx);
                                    manager.focus(window, cx);
                                    cx.notify();
                                });
                            }
                        });
                    }
                    DirectoryPickerEvent::SystemSelectionRequested => {
                        manager.pin_picker = None;
                        manager.choose_local_pin_directory(target.clone(), window, cx);
                    }
                    DirectoryPickerEvent::Dismissed => {
                        manager.pin_picker = None;
                        manager.transient.pin_target = None;
                    }
                    DirectoryPickerEvent::StateChanged => {}
                }
                manager.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            },
        )
        .detach();
        self.pin_picker = Some(picker.clone());
        picker.update(cx, |picker, cx| {
            picker.open(window, cx);
        });
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    fn choose_pin_directory(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.choose_directory(PinTarget::Workspace(workspace_id), window, cx);
    }

    /// Opens Open Local Directory, which creates a Local Workspace named `name` unless blank.
    fn open_local_directory(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.choose_directory(PinTarget::NewLocalWorkspace { name }, window, cx);
    }

    /// Chooses the directory `target` pins through the Directory Picker.
    fn choose_directory(&mut self, target: PinTarget, window: &mut Window, cx: &mut Context<Self>) {
        if spaceterm_ui::window_menu_is_open(window, cx) {
            let manager = cx.entity();
            window.defer(cx, move |window, cx| {
                spaceterm_ui::dismiss_active_menu(window, cx);
                manager.update(cx, |manager, cx| {
                    manager.choose_directory(target, window, cx)
                });
            });
            return;
        }
        if self.transient.local_selection_pending
            || (matches!(target, PinTarget::NewLocalWorkspace { .. })
                && self.remote_workspace_flow.is_some())
        {
            return;
        }

        self.transient.pin_target = Some(target.clone());
        self.pin_operation = self.pin_operation.wrapping_add(1);
        self.open_pin_picker(target, window, cx);
    }

    /// Selects a local directory for `target` through System Directory Selection.
    fn choose_local_pin_directory(
        &mut self,
        target: PinTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.dismiss_editing(window, cx);
        });
        self.transient.local_selection_pending = true;
        let operation = self.pin_operation;
        let selection = self.directory_selection.choose(cx);
        let filesystem = self.local_filesystem.clone();
        cx.spawn_in(window, async move |manager, cx| {
            let selection = match selection.await {
                Ok(Some(path)) => LocalPinSelection::Chosen(
                    cx.background_executor()
                        .spawn(async move { filesystem.validate_directory(&path) })
                        .await,
                ),
                Ok(None) => LocalPinSelection::Cancelled,
                Err(_) => LocalPinSelection::Failed,
            };
            let _ = manager.update_in(cx, |manager, window, cx| {
                manager.finish_local_pin_selection(target, operation, selection, window, cx);
            });
        })
        .detach();
    }

    fn finish_local_pin_selection(
        &mut self,
        target: PinTarget,
        operation: u64,
        selection: LocalPinSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transient.local_selection_pending = false;
        if self.pin_operation != operation || self.transient.pin_target.as_ref() != Some(&target) {
            return;
        }
        self.transient.pin_target = None;
        let pinned = match selection {
            LocalPinSelection::Cancelled => None,
            LocalPinSelection::Chosen(Ok(directory)) => {
                Some(self.apply_local_pin_target(target, directory, window, cx))
            }
            LocalPinSelection::Chosen(Err(_)) | LocalPinSelection::Failed => {
                Some(PinApplication::Failed)
            }
        };
        // The Directory Picker handed off without restoring focus, so focus returns to the
        // Workspace on every outcome, before an error alert that restores it on dismissal.
        self.focus(window, cx);
        if pinned == Some(PinApplication::Failed) {
            Self::show_pin_error(window, cx);
        }
        cx.notify();
    }

    fn present_remote_workspace_flow(
        &mut self,
        name: String,
        start: RemoteWorkspaceStart,
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
        if !flow.update(cx, |flow, cx| flow.open(start, window, cx)) {
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
        if completion.pinned_directory().is_some()
            && let Some(workspace_id) = self.workspaces.remote_workspace_pinned_to(
                completion.destination(),
                completion.physical_directory(),
            )
        {
            // The existing Workspace already owns a connection to this directory.
            drop(completion);
            self.acknowledge_remote_workspace_activation(flow, handle, window, cx);
            self.activate_workspace(workspace_id, window, cx);
            return;
        }
        let mut terminal_factory = WorkspaceTerminalSessionFactory::new_remote(
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
            completion.terminal_session_channels(),
        );
        terminal_factory.set_pinned_directory(completion.pinned_directory());
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
        revalidation: Result<(), crate::terminal::TerminalSessionChannelRevalidationError>,
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
        if let Some(pinned) = completion.pinned_directory()
            && let Err(error) = self
                .workspaces
                .set_pinned_directory(workspace_id, Some(pinned))
        {
            Self::report_workspace_error("pin", error);
        }
        if let Some(name) = self.remote_workspace_name.take()
            && let Err(error) = self.workspaces.rename_workspace(workspace_id, name)
        {
            Self::report_workspace_error("rename", error);
        }
        let (control_connection, _, _, _, _, _, lifecycle) = completion.into_parts();
        let replaced = self.remote_workspace_runtimes.insert(
            workspace_id,
            RemoteWorkspaceRuntime::new(1, control_connection, lifecycle, alias_pin),
        );
        debug_assert!(
            replaced.is_none(),
            "a new Remote Workspace owns one runtime"
        );
        self.lend_repository_reader(workspace_id, cx);
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
            runtime.drop_connection();
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
                let mut control_connection = connection.await.map_err(|error| {
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
                let provider = control_connection.provider();
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
                let channels = control_connection
                    .bind_terminal_session_channels(account.login_shell())
                    .map_err(|_| RemoteWorkspaceReconnectFailure::ConnectionFailed {
                        detail: None,
                    })?;
                let lifecycle = control_connection
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
                    .await
                    .map_err(classify_remote_workspace_restart_failure)?;
                if work_cancelled.load(Ordering::Acquire) {
                    return Err(RemoteWorkspaceReconnectFailure::Cancelled);
                }
                Ok(PreparedRemoteWorkspaceReconnect {
                    control_connection,
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
                        prepared.control_connection,
                        prepared.lifecycle,
                        alias_pin,
                    ),
                );
                self.lend_repository_reader(workspace_id, cx);
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

    /// Lends the Workspace's Control Connection reader to Repository Status for its machine.
    fn lend_repository_reader(&mut self, workspace_id: WorkspaceId, cx: &App) {
        let Some(store) = crate::ui::repository_status_store::InstalledRepositoryStatus::store(cx)
        else {
            return;
        };
        let Some(WorkspaceLocation::Remote { key, .. }) = self
            .workspaces
            .workspace(workspace_id)
            .map(|workspace| workspace.location())
        else {
            return;
        };
        let machine = crate::repository_status::RemoteMachineKey::new(key.destination().as_str());
        let Some(runtime) = self.remote_workspace_runtimes.get_mut(&workspace_id) else {
            return;
        };
        let Some(reader) = runtime
            .control_connection
            .as_ref()
            .and_then(ConnectedControlConnection::repository_reader)
        else {
            return;
        };
        runtime.repository_reader = Some(store.read(cx).remote_readers().lend(machine, reader));
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

    fn local_home_directory(&self) -> Result<ValidatedLocalDirectory, LocalFilesystemError> {
        self.local_filesystem
            .validate_directory(&self.local_home_directory_path)
    }

    fn show_home_directory_unavailable(window: &mut Window, cx: &mut Context<Self>) {
        let manager = cx.weak_entity();
        let window_handle = window.window_handle();
        let _ = Alert::new(
            ModalId::new("workspace-home-directory-unavailable"),
            "Home directory unavailable",
            "Home Directory Unavailable",
            "A workspace could not be created. Check that your home directory exists and is accessible, then try again.",
            vec![ModalAction::new((), "OK", ModalActionRole::Cancel, "workspace-home-error-ok")],
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
            || self.sidebar.read(cx).menu_open();
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
        }
    }

    fn request_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.should_close_window(window, cx) {
            window.remove_window();
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

    pub(crate) fn application_close_facts(&self, cx: &App) -> ApplicationCloseFacts {
        self.close_hierarchy(cx).application_close_facts()
    }

    pub(crate) fn application_pane_facts(&self, cx: &App) -> Vec<ApplicationPaneFacts> {
        self.close_hierarchy(cx).application_pane_facts().collect()
    }

    pub(crate) const fn has_pending_close_confirmation(&self) -> bool {
        self.close_confirmation.pending().is_some()
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
        if self.transient.pin_target == Some(PinTarget::Workspace(workspace_id)) {
            self.pin_operation = self.pin_operation.wrapping_add(1);
            self.transient.pin_target = None;
            if let Some(picker) = self.pin_picker.take() {
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
            SidebarEvent::Move {
                workspace_id,
                position,
            } => {
                if let Err(error) = self.workspaces.move_workspace(workspace_id, position) {
                    Self::report_workspace_error("move", error);
                }
            }
            SidebarEvent::Create(WorkspaceCreation::Local) => {
                self.create_local_workspace(window, cx)
            }
            SidebarEvent::Create(creation) => {
                // The creation menu returns focus to its trigger on close, so return it to
                // the terminal before a chooser captures its cancel-restore target.
                self.focus(window, cx);
                self.start_workspace_creation(creation, String::new(), window, cx)
            }
            SidebarEvent::ActivateWorktree {
                workspace_id,
                worktree_id,
                focus_pane,
            } => {
                if self.activate_workspace(workspace_id, window, cx)
                    && self.open_worktree(workspace_id, worktree_id, focus_pane, window, cx)
                    && focus_pane
                {
                    self.focus(window, cx);
                }
            }
            SidebarEvent::SetWorktreesExpanded {
                workspace_id,
                expanded,
            } => self.set_worktrees_expanded(workspace_id, expanded),
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
                self.choose_pin_directory(workspace_id, window, cx)
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
        self.run_workspace_creation(WorkspaceCreation::Local, window, cx);
    }

    fn on_new_remote_workspace(
        &mut self,
        _: &NewRemoteWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_workspace_creation(WorkspaceCreation::Remote, window, cx);
    }

    fn on_open_local_directory(
        &mut self,
        _: &OpenLocalDirectory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_workspace_creation(WorkspaceCreation::OpenLocalDirectory, window, cx);
    }

    fn on_open_remote_directory(
        &mut self,
        _: &OpenRemoteDirectory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_workspace_creation(WorkspaceCreation::OpenRemoteDirectory, window, cx);
    }

    /// Runs a creation Command. An open Workspace Switcher runs it with its query as the name.
    fn run_workspace_creation(
        &mut self,
        creation: WorkspaceCreation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window_combo_box_is_open(window, cx) {
            self.workspace_switcher.run_command(&creation, window, cx);
            return;
        }
        if creation != WorkspaceCreation::Local {
            self.sidebar.update(cx, |sidebar, cx| {
                sidebar.dismiss_editing(window, cx);
            });
        }
        self.start_workspace_creation(creation, String::new(), window, cx);
    }

    /// Starts `creation` for a Workspace named `name` unless it is blank.
    fn start_workspace_creation(
        &mut self,
        creation: WorkspaceCreation,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match creation {
            WorkspaceCreation::Local => self.create_named_local_workspace(Some(name), window, cx),
            WorkspaceCreation::Remote => {
                self.present_remote_workspace_flow(name, RemoteWorkspaceStart::Home, window, cx)
            }
            WorkspaceCreation::OpenLocalDirectory => self.open_local_directory(name, window, cx),
            WorkspaceCreation::OpenRemoteDirectory => self.present_remote_workspace_flow(
                name,
                RemoteWorkspaceStart::ChosenDirectory,
                window,
                cx,
            ),
        }
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
    fn workspace_chrome_identity(&self, cx: &App) -> (WorkspaceChromeIdentity, Tooltip) {
        let workspace = self.workspaces.active_workspace();
        let remote_connection_phase = workspace
            .remote_connection_state()
            .map(RemoteConnectionState::phase);
        let remote_status = remote_connection_phase.and_then(remote_connection_status);
        let (path, _) = directory_labels(
            workspace.location(),
            workspace.local_display_directory(),
            workspace.remote_display_directory(),
            &self.local_home_directory_path,
        );
        let tooltip_detail = match workspace.availability() {
            DirectoryAvailability::Unavailable { reason } => format!("{path}: {reason}"),
            DirectoryAvailability::Available => remote_status
                .map(|status| format!("{path}: {status}"))
                .unwrap_or_else(|| path.clone()),
        };

        let repository = self
            .chip_repository_line(workspace.id(), cx)
            .map(|line| format!("\n{line}"))
            .unwrap_or_default();
        (
            chrome_identity(workspace),
            Tooltip::new("workspace-switcher-tooltip", "Switch Workspace")
                .detail(format!(
                    "{}\n{tooltip_detail}{repository}",
                    workspace.name()
                ))
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
        let frame = super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx);
        let top_chrome_height = frame.top_chrome_height(appearance.top_height());
        let chrome_icon_size = appearance.icons.metrics(IconRole::Chrome).glyph_size;
        let switcher_colors = appearance.host_colors(spaceterm_ui::ControlHost::TitleBar);
        let switcher_foreground = gpui_color(switcher_colors.text);
        let switcher_icon_foreground = gpui_color(switcher_colors.icon);
        let placeholder_color = gpui_color(
            appearance
                .host_colors(spaceterm_ui::ControlHost::Floating)
                .text_placeholder,
        );
        let sidebar_visible = self.sidebar.read(cx).layout().visible;
        let (toggle_icon, toggle_label) =
            sidebar_toggle_presentation(self.sidebar.read(cx).layout().visible);
        let drag_manager = manager.clone();
        let toggle_manager = manager.clone();
        let close_owner = manager.clone();
        let combo_lifecycle_manager = manager.clone();
        let accept_manager = manager.clone();
        let combo_lifecycle_window = window.window_handle();
        let presentation = crate::desktop_profile::DesktopPresentation::get(cx);
        let remote_unavailable_reason = self.remote_workspace_unavailable_reason.clone();
        let creation_shortcuts =
            WorkspaceCreation::ALL.map(|creation| creation.shortcut(presentation));
        let collapsed_identity = (!sidebar_visible).then(|| self.workspace_chrome_identity(cx));
        let switcher_surface =
            super::tab_manager::active_tab_surface(appearance, window.is_window_active());
        let title_bar_host =
            appearance.control_host_background(spaceterm_ui::ControlHost::TitleBar);
        let switcher_status_hosts = WorkspaceChromeStatusHosts::new(
            switcher_surface
                .fill
                .map_or(title_bar_host, |fill| fill.source_over(title_bar_host)),
            switcher_surface
                .hover_fill
                .or(switcher_surface.fill)
                .map_or(title_bar_host, |fill| fill.source_over(title_bar_host)),
        );
        let switcher_accessibility_name = collapsed_identity.as_ref().map_or_else(
            || "Switch Workspace".to_owned(),
            |(identity, _)| identity.accessibility_name(),
        );
        let chooser = ComboBox::with_commands(
            "workspace-switcher",
            switcher_accessibility_name,
            Some(self.workspaces.active_workspace_id()),
            "Switch Workspace",
            self.workspace_switcher_items(cx),
            move |_| {
                WorkspaceCreation::ALL
                    .into_iter()
                    .zip(creation_shortcuts.clone())
                    .map(|(creation, shortcut)| {
                        let mut command = ComboBoxCommand::new(creation, creation.label())
                            .leading_icon(move |foreground, size| {
                                Icon::custom(creation.icon(), size, foreground).into_any_element()
                            })
                            .debug_selector(format!("workspace-switcher-{}", creation.selector()));
                        if creation.starts_group() {
                            command = command.starts_group();
                        }
                        if let Some(shortcut) = shortcut {
                            command = command.shortcut(shortcut);
                        }
                        if creation.is_remote()
                            && let Some(reason) = &remote_unavailable_reason
                        {
                            command = command
                                .disabled(true)
                                .description(reason.clone())
                                .trailing(ComboBoxAccessory::Status("Unavailable".into()));
                        }
                        command
                    })
                    .collect()
            },
        )
        .handle(self.workspace_switcher.clone())
        .when(sidebar_visible, |chooser| chooser.ghost_trigger())
        .when(!sidebar_visible, |chooser| {
            let normal = spaceterm_ui::ButtonPaint::new(
                gpui_color(switcher_surface.fill.unwrap_or(Color::rgba(0))),
                switcher_foreground,
                gpui_color(switcher_surface.rim.unwrap_or(Color::rgba(0))),
            );
            let hovered = spaceterm_ui::ButtonPaint::new(
                gpui_color(
                    switcher_surface
                        .hover_fill
                        .or(switcher_surface.fill)
                        .unwrap_or(Color::rgba(0)),
                ),
                switcher_foreground,
                gpui_color(
                    switcher_surface
                        .hover_rim
                        .or(switcher_surface.rim)
                        .unwrap_or(Color::rgba(0)),
                ),
            );
            chooser.trigger_surface(spaceterm_ui::ButtonVariantStyle::new(
                normal, hovered, hovered, normal,
            ))
        })
        .input_leading(move |size| {
            Icon::custom(CustomIconName::FilterCircle, size, placeholder_color).into_any_element()
        })
        .copy(ComboBoxCopy::new(
            "Workspace name",
            "Filter or create...",
            "No Workspaces",
            "No matching Workspaces",
        ))
        .menu_with_filter_header()
        // The chooser takes the top chrome's icon size so its glyph keeps one size across sidebar
        // states.
        .icon_trigger(move |_, _| {
            // The same selector the chip's glyph carries: they are the one chooser icon in its two
            // states, so a test can hold them to one size.
            div()
                .debug_selector(|| "workspace-switcher-icon".to_owned())
                .flex_shrink_0()
                .child(Icon::custom(
                    CustomIconName::RectangleStack,
                    chrome_icon_size,
                    switcher_icon_foreground,
                ))
                .into_any_element()
        })
        .placement(AnchoredPlacementConfig::new(
            AnchoredPlacement::Bottom,
            AnchoredAlignment::End,
        ))
        .debug_selector("workspace-switcher")
        .tooltip(
            Tooltip::new("workspace-switcher-tooltip", "Switch Workspace")
                .debug_selector("workspace-switcher-tooltip")
                .shortcut(presentation.shortcut(&SwitchWorkspace).unwrap_or_default()),
        )
        .when_some(collapsed_identity, |chooser, (identity, tooltip)| {
            chooser
                .custom_trigger(identity.render(
                    switcher_foreground,
                    appearance,
                    switcher_status_hosts,
                ))
                .custom_trigger_height(frame.top_chip_height(appearance.top_height()))
                .full_width(true)
                .tooltip(
                    tooltip.shortcut(presentation.shortcut(&SwitchWorkspace).unwrap_or_default()),
                )
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
            let _ = accept_manager.update(cx, |manager, cx| {
                manager.activate_workspace(*acceptance.item_id(), window, cx);
                manager.sync_terminal_focus_blocker(window, cx);
            });
        })
        .on_command(move |activation, window, cx| {
            let _ = manager.update(cx, |manager, cx| {
                let name = activation.query().trim().to_owned();
                manager.start_workspace_creation(*activation.command(), name, window, cx);
                manager.sync_terminal_focus_blocker(window, cx);
            });
        });
        let close: spaceterm_ui::WindowCloseHandler = Rc::new(move |window, cx| {
            let _ = close_owner.update(cx, |manager, cx| manager.request_window_close(window, cx));
        });
        let leading_controls = spaceterm_ui::ClientWindowControls::new(close)
            .side(spaceterm_ui::WindowControlSide::Left)
            .surface_color(gpui_color(appearance.colors.title_bar_background));
        let content = div().relative().size_full().child(
            layout.render_controls(
                IconButton::new("toggle-sidebar-button", toggle_label, move |foreground| {
                    Icon::custom(toggle_icon, chrome_icon_size, foreground).into_any_element()
                })
                .variant(ButtonVariant::Ghost)
                .size(TOGGLE_SIZE)
                .preserve_ancestor_hover()
                .debug_selector("toggle-sidebar-button")
                .tooltip(
                    Tooltip::new("toggle-sidebar-tooltip", toggle_label)
                        .shortcut(presentation.shortcut(&ToggleSidebar).unwrap_or_default())
                        .debug_selector("toggle-sidebar-tooltip"),
                )
                .on_activate(move |_, window, cx| {
                    let _ = toggle_manager.update(cx, |manager, cx| {
                        manager.toggle_sidebar(window, cx);
                    });
                }),
                chooser,
                leading_controls,
                cx,
            ),
        );
        let drag_region = WindowDragRegion::new(
            "workspace-top-chrome-drag-region",
            "Move Operating-System Window from Workspace chrome",
            content,
        )
        .middle_activation(matches!(
            window.window_decorations(),
            gpui::Decorations::Client { .. }
        ))
        .status(self.window_drag_status.clone())
        .pointer_insets(Edges {
            right: super::control_theme::resize_handle::spacious_target_half_thickness(cx),
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

        spaceterm_ui::ControlHost::TitleBar
            .mount(
                div()
                    .id("workspace-top-chrome")
                    .role(gpui::accesskit::Role::Group)
                    .a11y_synthetic_children(publish_children_in_place)
                    .debug_selector(|| "workspace-top-chrome".to_owned())
                    .absolute()
                    .top_0()
                    .left_0()
                    .w(layout.width)
                    .h(top_chrome_height)
                    .child(drag_region),
            )
            .into_any_element()
    }

    /// The title-bar surface behind the top-left chrome. It paints beneath the Tab manager so it
    /// cannot hide the mark at the Tab strip's start.
    fn render_top_left_chrome_surface(
        layout: WorkspaceChromeLayout,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let appearance = super::appearance::chrome(cx);
        let background = if window.is_window_active() {
            appearance.colors.title_bar_background
        } else {
            appearance.colors.title_bar_inactive_background
        };
        let frame = super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx);
        div()
            .debug_selector(|| "workspace-top-chrome-surface".to_owned())
            .absolute()
            .top_0()
            .left_0()
            .w(layout.width)
            .h(frame.top_chrome_height(appearance.top_height()))
            .bg(gpui_color(
                appearance.surface(crate::appearance::SurfaceRole::Base, background),
            ))
            .into_any_element()
    }

    fn sidebar_rows(&self, cx: &App) -> Vec<WorkspaceRowViewModel> {
        let active_workspace_id = self.workspaces.active_workspace_id();
        self.workspaces
            .iter()
            .map(|workspace| {
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
                        WorkspaceLocation::Remote { key, .. } => {
                            Some(key.destination().host().to_owned().into())
                        }
                    },
                    tooltip: tooltip.into(),
                    pinned: workspace.pinned_directory().is_some(),
                    remote_connection_phase: workspace
                        .remote_connection_state()
                        .map(RemoteConnectionState::phase),
                    available,
                    repository: self.sidebar_badge(workspace.id(), cx),
                    worktrees: self.worktree_section(workspace.id(), cx),
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
        self.sync_sidebar_repositories(cx);
        self.sync_sidebar_worktrees(cx);
        let activity = super::appearance::window_activity(window);
        activity.mount(activity.with_scope(|| self.render_chrome(window, cx)))
    }
}

impl WorkspaceManager {
    fn render_chrome(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        debug_assert!(self.workspaces.len() > 0);
        self.sync_terminal_focus_blocker(window, cx);
        let manager = cx.entity().downgrade();
        let active_tab_manager = self.workspaces.active_workspace().payload().clone();
        let workspace = self.workspaces.active_workspace();
        let sidebar_layout = self.sidebar.read(cx).layout();
        let chrome =
            WorkspaceChromeLayout::resolve(sidebar_layout, &chrome_identity(workspace), window, cx);
        let update_control = gpui::AnyView::from(self.update_control.clone());
        let close_owner = cx.weak_entity();
        let close: spaceterm_ui::WindowCloseHandler = Rc::new(move |window, cx| {
            let _ = close_owner.update(cx, |manager, cx| manager.request_window_close(window, cx));
        });
        active_tab_manager.update(cx, |manager, cx| {
            manager.set_sidebar_layout(
                sidebar_layout.visible,
                sidebar_layout.width,
                chrome.width,
                cx,
            );
            manager.set_trailing_accessory(Some(update_control), cx);
            manager.set_window_close_handler(close);
        });
        let rows = self.sidebar_rows(cx);
        let remote_unavailable = self.remote_workspace_unavailable_reason.clone();
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_rows(rows, remote_unavailable, cx)
        });
        let content = Self::workspace_action_scope(cx)
            .id("workspace-manager")
            .bg(gpui_color(super::appearance::chrome(cx).surface(
                crate::appearance::SurfaceRole::Sheet,
                super::appearance::chrome(cx).colors.background,
            )))
            .debug_selector(|| "workspace-manager".to_owned())
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .chrome_text(
                super::appearance::chrome(cx)
                    .typography
                    .style(TextRole::Body),
            )
            .role(gpui::accesskit::Role::Group)
            .a11y_synthetic_children(publish_in_layout_order)
            .child(Self::render_top_left_chrome_surface(chrome, window, cx))
            .child(
                div()
                    .id("workspace-tab-manager")
                    .role(gpui::accesskit::Role::Group)
                    .a11y_synthetic_children(publish_children_in_place)
                    .relative()
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .child(active_tab_manager),
            )
            .when(self.sidebar.read(cx).layout().visible, |root| {
                root.child(self.sidebar.clone())
            })
            .child(
                self.sidebar.read(cx).render_resize_handle(
                    self.sidebar.downgrade(),
                    chrome.width,
                    super::workspace_frame::WorkspaceFrame::for_appearance(
                        super::appearance::chrome(cx),
                        cx,
                    )
                    .top_chrome_height(super::appearance::chrome(cx).top_height()),
                ),
            )
            .child(self.render_top_left_chrome(chrome, manager.clone(), window, cx));
        let transients = Self::workspace_action_scope(cx)
            .absolute()
            .inset_0()
            .children(self.remote_workspace_flow.iter().cloned())
            .children(self.pin_picker.iter().cloned());
        ModalLayer::new(super::window_shell::render(content, window, cx)).transient(transients)
    }
}

impl WorkspaceManager {
    /// Keeps ordinary content and complete transient owners on the same action routes while leaving
    /// the active modal outside those routes.
    fn workspace_action_scope(cx: &Context<Self>) -> gpui::Div {
        div()
            .key_context(TERMINAL_KEY_CONTEXT)
            .on_action(cx.listener(Self::on_switch_workspace))
            .on_action(cx.listener(Self::on_new_workspace))
            .on_action(cx.listener(Self::on_new_remote_workspace))
            .on_action(cx.listener(Self::on_open_local_directory))
            .on_action(cx.listener(Self::on_open_remote_directory))
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
            .on_action(cx.listener(Self::forward_active_terminal_action::<NextTab>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<PreviousTab>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<MoveTabRight>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<MoveTabLeft>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ScrollPageUp>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ScrollPageDown>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ScrollToTop>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ScrollToBottom>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ClosePane>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<CloseTab>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<SplitRight>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<SplitDown>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<FocusPaneLeft>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<FocusPaneRight>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<FocusPaneUp>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<FocusPaneDown>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<FocusPreviousPane>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<FocusNextPane>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<TogglePaneZoom>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<ShowRepositoryStatus>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<OpenTerminalFind>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<FindNext>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<FindPrevious>))
            .on_action(cx.listener(Self::forward_active_terminal_action::<CloseTerminalFind>))
    }
}

/// Reads the main window in its presented layout: the top-left chrome, the sidebar and its resize
/// handle, then the Tab manager. Paint order differs so the handle stays above the Tab manager and
/// below the top-left chrome for pointer input.
fn publish_in_layout_order(builder: &mut gpui::A11ySubtreeBuilder) {
    let node = builder.parent_node();
    let mut children = node.children().to_vec();
    if let [tab_manager, .., top_left_chrome] = children.as_mut_slice() {
        std::mem::swap(tab_manager, top_left_chrome);
    }
    node.set_children(children);
    publish_children_in_place(builder);
}

/// Withdraws a layout container from the platform tree, which then presents its children in the
/// container's place.
fn publish_children_in_place(builder: &mut gpui::A11ySubtreeBuilder) {
    builder
        .parent_node()
        .set_role(gpui::accesskit::Role::GenericContainer);
}

fn workspace_activation_shortcut(
    index: usize,
    presentation: &crate::desktop_profile::DesktopPresentation,
) -> Option<gpui::SharedString> {
    match index {
        0 => presentation.shortcut(&ActivateWorkspace1),
        1 => presentation.shortcut(&ActivateWorkspace2),
        2 => presentation.shortcut(&ActivateWorkspace3),
        3 => presentation.shortcut(&ActivateWorkspace4),
        4 => presentation.shortcut(&ActivateWorkspace5),
        5 => presentation.shortcut(&ActivateWorkspace6),
        6 => presentation.shortcut(&ActivateWorkspace7),
        7 => presentation.shortcut(&ActivateWorkspace8),
        8 => presentation.shortcut(&ActivateWorkspace9),
        _ => None,
    }
}

fn classify_remote_workspace_restart_failure(
    error: RemoteTabManagerLifecycleError,
) -> RemoteWorkspaceReconnectFailure {
    match error {
        RemoteTabManagerLifecycleError::Revalidation(
            crate::terminal::TerminalSessionChannelRevalidationError::DirectoryUnavailable,
        ) => RemoteWorkspaceReconnectFailure::DirectoryUnavailable,
        RemoteTabManagerLifecycleError::Revalidation(
            crate::terminal::TerminalSessionChannelRevalidationError::IdentityChanged,
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

/// The collapsed title-bar identity a Workspace presents.
fn chrome_identity<T>(workspace: &WorkspaceEntry<T>) -> WorkspaceChromeIdentity {
    WorkspaceChromeIdentity {
        name: workspace.name().to_owned(),
        pinned: workspace.pinned_directory().is_some(),
        status: WorkspaceChromeStatus::resolve(
            matches!(workspace.availability(), DirectoryAvailability::Available),
            workspace
                .remote_connection_state()
                .map(RemoteConnectionState::phase),
        ),
    }
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

#[path = "workspace_manager/repository.rs"]
mod repository;
mod worktrees;

#[cfg(test)]
#[path = "workspace_manager/tests.rs"]
pub(crate) mod tests;
