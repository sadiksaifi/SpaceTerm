use super::chrome_icons::IconRole;
use super::pane_lifecycle::{PaneConstruction, PaneLifecycleDependencies};
use super::workspace_chrome::{
    TOGGLE_SIZE, WorkspaceChromeIdentity, WorkspaceChromeLayout, WorkspaceChromeStatus,
    WorkspaceChromeStatusHosts,
};
use super::workspace_sidebar::{
    SERVER_WORKSPACE_BADGE, SidebarEvent, WorkspaceMenuCommand, WorkspaceRowViewModel,
    WorkspaceSidebar, WorktreeMenuCommand, remote_connection_status,
};
#[cfg(test)]
use super::workspace_sidebar::{
    SIDEBAR_MAXIMUM_WIDTH, SIDEBAR_ROW_HEIGHT, TERMINAL_CONTENT_MINIMUM_WIDTH,
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
    DirectoryPicker, DirectoryPickerEvent, DirectorySelection, DirectorySource,
    LocalDirectorySource, RemoteDirectorySource,
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
    MoveTabLeft, MoveTabRight, NewRemoteWorkspace, NewWorkspace, NewWorktree, NextTab,
    NextWorktree, OpenLocalDirectory, OpenRemoteDirectory, OpenTerminalFind, PreviousTab,
    PreviousWorktree, RemoteChildLaunchUnavailable, ScrollPageDown, ScrollPageUp, ScrollToBottom,
    ScrollToTop, ShowRepositoryStatus, SplitDown, SplitRight, SwitchWorkspace,
    TERMINAL_KEY_CONTEXT, TabManager, TabManagerEvent, TogglePaneZoom, ToggleSidebar,
    ToggleSidebarFocus, WORKSPACE_SIDEBAR_DEFAULT_WIDTH,
};
use crate::appearance::Color;
use crate::close_confirmation::{
    ApplicationCloseFacts, ApplicationPaneFacts, CloseConfirmation, CloseHierarchy, CloseScope,
    CloseTarget,
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
    OperatingSystemWindowDragError, OperatingSystemWindowDragPlatform, WindowMoveStart,
};
use crate::settings::sidebar::SidebarState;
use crate::ssh::live_connection::{ControlConnectionObserver, ControlConnectionTerminalState};
use crate::ssh::process::TransientSshErrorOutput;
use crate::terminal::metadata::RemoteTerminalMetadataContext;
use crate::terminal::{
    NativeServiceOrigin, NativeServiceStatus, PreparedWorkspaceTerminalLaunch, SelectionCopy,
    TerminalKeyInputAdapterFactory, TerminalSessionFactory, WorkspaceTerminalSessionFactory,
};
use gpui::prelude::*;

use gpui::{
    Action, AnyElement, App, Context, Edges, Entity, FocusHandle, Pixels, Render, Task, WeakEntity,
    Window, div, px,
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
    /// The account-local server this window launches and presents. Without one, the window
    /// starts with a desktop-owned local Workspace.
    pub(crate) local_server: Option<launch::LocalServerAdapters>,
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

/// Owns directory selection, its target, and the field version its draft began at.
struct WorkspaceTransientUi {
    pin_target: Option<PinTarget>,
    /// The Workspace whose pin version a chooser opened on, for a Workspace a SpaceTerm Server
    /// owns. It lives exactly as long as `pin_target`, so a publication while the chooser is open
    /// conflicts instead of being silently overwritten.
    pin_draft_version: Option<(WorkspaceId, u64)>,
    local_selection_pending: bool,
}

impl WorkspaceTransientUi {
    /// Begins one directory selection, keeping the pin version a shared Workspace showed.
    fn begin_pin(&mut self, target: PinTarget, version: Option<(WorkspaceId, u64)>) {
        self.pin_target = Some(target);
        self.pin_draft_version = version;
    }

    /// Ends the current directory selection and forgets the version it began at.
    fn end_pin(&mut self) {
        self.pin_target = None;
        self.pin_draft_version = None;
    }

    /// The pin version the open chooser began at, for `workspace_id` alone. Only a Workspace a
    /// SpaceTerm Server owns has one.
    fn pin_version(&self, workspace_id: WorkspaceId) -> Option<u64> {
        self.pin_draft_version
            .filter(|(owner, _)| *owner == workspace_id)
            .map(|(_, version)| version)
    }
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

/// The server Workspaces a Workspace Window presents beside its own.
type SharedWorkspaceBinding = super::shared_workspaces::SharedWorkspaces;

/// The server identity of a detached Workspace the Workspace Switcher offers.
type DetachedWorkspace = spaceterm_server::hierarchy::Id;

/// What one Workspace Switcher result chooses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SwitcherChoice {
    /// A Workspace this window presents, which choosing activates.
    Presented(WorkspaceId),
    /// A server Workspace this client has detached, which choosing attaches and activates.
    Detached(DetachedWorkspace),
}

pub(crate) struct WorkspaceManager {
    window_appearance: super::appearance_runtime::WindowAppearanceOwner,
    window_traffic_lights: super::appearance_runtime::WindowTrafficLightOwner,
    transient: WorkspaceTransientUi,
    workspace_switcher: ComboBoxHandle<SwitcherChoice, WorkspaceCreation>,
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
    shared_repositories: shared_repositories::SharedWorkspaceRepositories,
    /// The adopted SpaceTerm Server's Workspaces, their edit state, and the answers still to
    /// present. Adopting adds them beside this window's own Workspaces and changes none of them.
    shared: Option<SharedWorkspaceBinding>,
    /// Reads the server's inventory and edit state while the window lives.
    _shared_poll: Option<Task<()>>,
    /// Where the keyboard rests while the window presents no Workspace.
    empty_focus: FocusHandle,
    /// Normal launch of the account-local server, while this window presents it.
    launch: Option<launch::LocalLaunchState>,
}

impl WorkspaceManager {
    pub(crate) fn new_with_adapters(
        session_factory: Rc<dyn TerminalSessionFactory>,
        local_home_directory_path: PathBuf,
        adapters: WorkspaceManagerAdapters,
        default_sidebar: SidebarState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let sidebar_visible = default_sidebar.is_expanded();
        let mut window_appearance = super::appearance_runtime::WindowAppearanceOwner::workspace();
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
        cx.observe_global_in::<super::background_image_runtime::BackgroundImageRuntime>(
            window,
            |manager, window, cx| manager.window_appearance.apply(window, cx),
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
            local_server,
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
        let mut workspaces = if local_server.is_some() {
            // The server owns this window's Workspaces, which arrive once it is ready.
            WorkspaceCollection::empty()
        } else {
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
                        sidebar_visible,
                        sidebar_width: px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
                        operating_system_window_drag_platform: Rc::clone(
                            &initial_window_drag_platform,
                        ),
                        pane_construction: pane_construction.clone(),
                    },
                    window,
                    cx,
                )
            })
        };
        if let Some((reason, workspace_id)) =
            initial_directory_error.zip(workspaces.active_workspace_id())
        {
            let _ = workspaces.set_directory_unavailable(workspace_id, reason);
        }
        cx.on_app_quit(|manager, cx| {
            manager.save_shared_arrangements(cx);
            if let Some(shared) = manager.shared.as_mut() {
                shared.flush_client_views();
            }
            async {}
        })
        .detach();
        cx.on_release(|manager, cx| {
            manager.release_sidebar_repositories(cx);
            manager.release_sidebar_worktrees(cx);
            manager.save_shared_arrangements(cx);
        })
        .detach();
        let sidebar = cx.new(|cx| WorkspaceSidebar::new(sidebar_visible, window, cx));
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

        let mut manager = Self {
            window_appearance,
            window_traffic_lights,
            transient: WorkspaceTransientUi {
                pin_target: None,
                pin_draft_version: None,
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
            shared_repositories: shared_repositories::SharedWorkspaceRepositories::default(),
            shared: None,
            _shared_poll: None,
            empty_focus: cx.focus_handle(),
            launch: local_server.map(launch::LocalLaunchState::new),
        };
        manager.run_local_launch(None, window, cx);
        manager
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
                    // A shared Workspace's directory is the server's, and the sidebar shows what
                    // the server publishes. A server path never becomes this Workspace's own
                    // directory, which local pinning and file actions would read as authority.
                    if !workspace_manager.is_shared_workspace(workspace_id)
                        && let Some(directory) = tab_manager.read(cx).automatic_directory(cx)
                    {
                        let _ = workspace_manager
                            .workspaces
                            .update_automatic_directory(workspace_id, directory);
                    }
                    workspace_manager.sync_terminal_focus_blocker(window, cx);
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
        // The empty view holds the keyboard while this window presents no Workspace.
        let Some(manager) = self.active_tab_manager() else {
            self.empty_focus.focus(window, cx);
            return;
        };
        manager.update(cx, |manager, cx| {
            manager.set_parent_focus_blocker(None, cx);
            manager.focus(window, cx);
        });
    }

    /// The Active Workspace's Tab manager, absent while this window presents no Workspace.
    fn active_tab_manager(&self) -> Option<Entity<TabManager>> {
        self.workspaces
            .active_workspace()
            .map(|workspace| workspace.payload().clone())
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
        let blocker = self.terminal_focus_blocker(window, cx);
        let Some(workspace) = self.workspaces.active_workspace() else {
            return NativeServiceStatus::default();
        };
        let workspace_id = workspace.id();
        workspace.payload().update(cx, |manager, cx| {
            manager.set_parent_focus_blocker(blocker, cx);
            manager.native_service_status(workspace_id, window, cx)
        })
    }

    fn native_service_target(
        &self,
        origin: NativeServiceOrigin,
        cx: &App,
    ) -> Option<Entity<super::TerminalPane>> {
        if self.workspaces.active_workspace_id() != Some(origin.workspace_id()) {
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
                .is_some_and(|picker| picker.read(cx).blocks_terminal_input())
                || self.active_directory_recovery(cx).is_some(),
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

    fn active_directory_recovery(
        &self,
        cx: &App,
    ) -> Option<Entity<super::server_pane::DirectoryRecovery>> {
        self.active_tab_manager()
            .and_then(|manager| manager.read(cx).directory_recovery(cx))
    }

    /// The presented Workspaces in sidebar order, then the detached ones the server offers.
    fn workspace_switcher_items(&self, cx: &App) -> Vec<ComboBoxItem<SwitcherChoice>> {
        let presentation = crate::desktop_profile::DesktopPresentation::get(cx);
        let mut items: Vec<_> = self
            .workspaces
            .iter()
            .enumerate()
            .map(|(index, workspace)| {
                let icon = match workspace.location() {
                    WorkspaceLocation::Local => IconName::Terminal,
                    WorkspaceLocation::Remote { .. } => IconName::Globe,
                };
                let item = ComboBoxItem::new(
                    SwitcherChoice::Presented(workspace.id()),
                    workspace.name().to_owned(),
                )
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
            .collect();
        items.extend(self.detached_switcher_items());
        items
    }

    pub(crate) fn open_workspace_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window_modal_is_open(window, cx) {
            return;
        }
        if let Some(picker) = self.pin_picker.take() {
            picker.update(cx, |picker, cx| picker.cancel(window, cx));
            self.transient.end_pin();
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
        if let Some(manager) = self.active_tab_manager() {
            manager.update(cx, |manager, cx| {
                manager.set_parent_focus_blocker(blocker, cx);
            });
        }
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
                let response = match self
                    .operating_system_window_drag_platform
                    .interaction_started(window)
                {
                    Ok(WindowMoveStart::Started) => {
                        WindowDragRegionResponse::OperatingSystemWindowMoveStarted
                    }
                    Ok(WindowMoveStart::AwaitMovement) => WindowDragRegionResponse::Continue,
                    Err(error) => {
                        Self::report_operating_system_window_drag_error("begin", error);
                        WindowDragRegionResponse::Continue
                    }
                };
                self.sync_terminal_focus_blocker(window, cx);
                response
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
            &self.chrome_identity(workspace_id, cx),
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
            .position(|workspace| Some(workspace.id()) == id)
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
        let previous_manager = self.active_tab_manager();
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
        if let Some(previous_manager) = &previous_manager {
            previous_manager.update(cx, |manager, cx| manager.deactivate(cx));
        }
        next_manager.update(cx, |manager, cx| manager.activate(window, cx));
        Self::hand_over_tab_strip(previous_manager.as_ref(), &next_manager, cx);
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
        if self.launch.is_some() {
            return self.open_shared_directory_workspace(directory, name, window, cx);
        }
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
                self.transient.end_pin();
                Self::show_home_directory_unavailable(window, cx);
                return PinApplication::Reported;
            }
            None => self.create_pinned_local_workspace(Some(name), Some(directory), window, cx),
        };
        // A failure keeps the operation current so the still-open picker can retry.
        if opened {
            self.pin_operation = self.pin_operation.wrapping_add(1);
            self.transient.end_pin();
        }
        PinApplication::from_applied(opened)
    }

    /// Opens a chosen directory as a Workspace the account server owns: the presented Workspace
    /// already pinned there, or a new one whose first Pane starts there.
    fn open_shared_directory_workspace(
        &mut self,
        directory: ValidatedLocalDirectory,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PinApplication {
        let existing = self.workspaces.iter().map(WorkspaceEntry::id).find(|id| {
            self.shared
                .as_ref()
                .and_then(|shared| shared.server_workspace(*id))
                .is_some_and(|workspace| {
                    workspace.pinned_directory.as_deref() == Some(directory.path())
                })
        });
        let opened = match existing {
            Some(workspace_id) => self.activate_workspace(workspace_id, window, cx),
            None => self.create_shared_workspace(
                name,
                Some(directory.path().to_owned()),
                window,
                cx,
            ),
        };
        if opened {
            self.pin_operation = self.pin_operation.wrapping_add(1);
            self.transient.end_pin();
            return PinApplication::Applied;
        }
        // The refusal was presented, so the chooser ends rather than offering a retry.
        if let Some(picker) = self.pin_picker.take() {
            picker.update(cx, |picker, cx| picker.cancel(window, cx));
        }
        self.transient.end_pin();
        PinApplication::Reported
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
        // The version a shared draft began at is read here, before anything ends the chooser:
        // every completion route clears that state as it finishes.
        let observed_version = self.transient.pin_version(workspace_id);
        // A shared Workspace's Pinned Directory belongs to the server, which decides it for every
        // client. Nothing is pinned locally first.
        if let Some(applied) =
            self.pin_shared_workspace(workspace_id, pin.as_ref(), observed_version, window, cx)
        {
            return applied;
        }
        if self
            .workspaces
            .set_pinned_directory(workspace_id, pin.clone())
            .is_err()
        {
            return false;
        }
        self.pin_operation = self.pin_operation.wrapping_add(1);
        self.transient.end_pin();
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
        if self.active_directory_recovery(cx).is_some() {
            return;
        }
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
                        let DirectorySelection::Pin(pinned) = pinned else {
                            picker.update(cx, |picker, cx| picker.activation_failed(window, cx));
                            return;
                        };
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
                        manager.transient.end_pin();
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

        // A shared Workspace's draft is fenced to the pin version the chooser opened on.
        let version = match &target {
            PinTarget::Workspace(workspace_id) => self
                .shared_pin_version(*workspace_id)
                .map(|version| (*workspace_id, version)),
            PinTarget::NewLocalWorkspace { .. } => None,
        };
        self.transient.begin_pin(target.clone(), version);
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
        // The selection is applied while the chooser's state still stands, because applying a
        // shared pin reads the version that chooser opened on. Every outcome ends it below.
        let pinned = match selection {
            LocalPinSelection::Cancelled => None,
            LocalPinSelection::Chosen(Ok(directory)) => {
                Some(self.apply_local_pin_target(target, directory, window, cx))
            }
            LocalPinSelection::Chosen(Err(_)) | LocalPinSelection::Failed => {
                Some(PinApplication::Failed)
            }
        };
        self.transient.end_pin();
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
        let previous_manager = self.active_tab_manager();
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
        previous_workspace_id: Option<WorkspaceId>,
        previous_manager: Option<Entity<TabManager>>,
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
        if previous_workspace_id != Some(workspace_id)
            && let Some(previous_manager) = &previous_manager
        {
            previous_manager.update(cx, |manager, cx| manager.deactivate(cx));
        }
        let blocker = self.non_modal_terminal_focus_blocker(window, cx);
        next_manager.update(cx, |manager, cx| {
            manager.set_parent_focus_blocker(blocker, cx);
            manager.activate(window, cx);
        });
        Self::hand_over_tab_strip(previous_manager.as_ref(), &next_manager, cx);
    }

    /// Starts `next`'s Tab strip from the shape of `previous`'s, whose place it takes. The empty
    /// view has no Tab strip to hand over.
    fn hand_over_tab_strip(
        previous: Option<&Entity<TabManager>>,
        next: &Entity<TabManager>,
        cx: &mut App,
    ) {
        let Some(previous) = previous.filter(|previous| *previous != next) else {
            return;
        };
        if let Some(shape) = previous.read(cx).tab_strip_shape(cx) {
            next.update(cx, |manager, cx| manager.arrive_from(shape, cx));
        }
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

    fn home_directory_alert() -> Alert<()> {
        Alert::new(
            ModalId::new("workspace-home-directory-unavailable"),
            "Home directory unavailable",
            "Home Directory Unavailable",
            "A workspace could not be created. Check that your home directory exists and is accessible, then try again.",
            vec![ModalAction::new((), "OK", ModalActionRole::Cancel, "workspace-home-error-ok")],
        )
        .intent(AlertIntent::Warning)
    }

    fn show_home_directory_unavailable(window: &mut Window, cx: &mut Context<Self>) {
        let manager = cx.weak_entity();
        let window_handle = window.window_handle();
        let _ = Self::home_directory_alert().present(window, cx, move |_, cx| {
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
        let previous_manager = self.active_tab_manager();
        if let Err(error) = self.workspaces.activate_workspace(workspace_id) {
            Self::report_workspace_error("activate", error);
            return false;
        }

        self.synchronize_tab_manager_layout(workspace_id, window, cx);
        let preserve_sidebar_focus = self.sidebar.read(cx).is_focused(window)
            || self.sidebar.read(cx).rename_is_focused(window)
            || self.sidebar.read(cx).menu_open();
        if previous_workspace_id != Some(workspace_id) {
            if let Some(previous_manager) = &previous_manager {
                previous_manager.update(cx, |manager, cx| manager.deactivate(cx));
            }
            self.sidebar.update(cx, |sidebar, cx| {
                sidebar.cancel_rename(cx);
            });
        }
        if preserve_sidebar_focus {
            next_manager.update(cx, |manager, cx| manager.activate_without_focus(cx));
        } else {
            next_manager.update(cx, |manager, cx| manager.activate(window, cx));
        }
        Self::hand_over_tab_strip(previous_manager.as_ref(), &next_manager, cx);
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

    /// The close facts of every Pane this window owns.
    ///
    /// A Pane a SpaceTerm Server owns is absent: closing this window or quitting SpaceTerm only
    /// drops its subscription, and the shell keeps running on the server. Warning about work that
    /// survives would be untrue, and nothing here can end it.
    fn close_hierarchy(&self, cx: &App) -> CloseHierarchy {
        let mut hierarchy = CloseHierarchy::default();
        for workspace in self
            .workspaces
            .iter()
            .filter(|workspace| !self.is_shared_workspace(workspace.id()))
        {
            let manager = workspace.payload().read(cx);
            for (tab, pane, terminal) in manager.terminal_panes(cx) {
                hierarchy.insert(
                    workspace.id(),
                    manager.tab_worktree(tab),
                    tab,
                    pane,
                    terminal.close_facts(),
                );
            }
        }
        hierarchy
    }

    fn request_close(&mut self, target: CloseTarget, window: &mut Window, cx: &mut Context<Self>) {
        // A SpaceTerm Server owns a shared Workspace's Tabs, Panes, and Sessions, so closing one
        // asks the server to end them for every client. Nothing closes only here.
        if self.request_shared_close(target, window, cx) {
            return;
        }
        if self.close_confirmation.pending().is_some() {
            return;
        }
        let Some(requires_confirmation) = self.close_target_requires_confirmation(target, cx)
        else {
            return;
        };
        // Kill Workspace ends every Pane in the Workspace at once, so it always asks first. A
        // smaller close asks only when it would stop running work.
        if !requires_confirmation && target.scope() != CloseScope::Workspace {
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
        let message = match scope {
            CloseScope::Workspace => format!(
                "Kill this Workspace and close its {count} {noun}? Running commands in these \
                 Panes will stop."
            ),
            _ => format!("Close {count} {noun}? Running commands in these Panes will stop."),
        };
        let result = Alert::new(
            ModalId::new("close-confirmation"),
            scope.title(),
            scope.title(),
            message,
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
            CloseTarget::Worktree {
                workspace_id,
                worktree_id,
            } => {
                self.close_worktree_tabs(workspace_id, worktree_id, window, cx);
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

    #[cfg(test)]
    pub(crate) fn sidebar_visible(&self, cx: &App) -> bool {
        self.sidebar.read(cx).layout().visible
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
        self.close_workspace_activating(workspace_id, true, window, cx);
    }

    /// Closes a Workspace. `activate_next` opens the Workspace that takes its place for the
    /// person, which selects that Workspace's Focused Pane; a close the server published passes
    /// `false`, because nobody asked for a Pane there.
    fn close_workspace_activating(
        &mut self,
        workspace_id: WorkspaceId,
        activate_next: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The server owns this window's local work, so nothing desktop-owned replaces a final
        // Workspace: the window presents its empty view instead.
        if self.launch.is_some() && self.workspaces.len() == 1 {
            self.remove_workspace_presentation(workspace_id, window, cx);
            self.remote_workspace_runtimes.remove(&workspace_id);
            self.debug_assert_remote_runtime_invariants();
            return;
        }
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
        let was_active = self.workspaces.active_workspace_id() == Some(workspace_id);
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
        // Closing never empties the collection: a final Workspace is replaced.
        if let Some(active_workspace_id) = self.workspaces.active_workspace_id() {
            self.synchronize_tab_manager_layout(active_workspace_id, window, cx);
        }

        if was_active && let Some(active_manager) = self.active_tab_manager() {
            if !activate_next
                || self.sidebar.read(cx).is_focused(window)
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
        // Closing another Workspace leaves the sidebar where it was scrolled.
        if was_active {
            self.scroll_active_workspace_into_view(cx);
        }

        cx.notify();
    }

    fn close_workspace_for_final_tab(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_remote_workspace_close(workspace_id, window, cx);
        let was_active = self.workspaces.active_workspace_id() == Some(workspace_id);
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
                self.synchronize_tab_manager_layout(active_workspace_id, window, cx);

                if was_active && let Some(active_manager) = self.active_tab_manager() {
                    if self.sidebar.read(cx).is_focused(window)
                        || self.sidebar.read(cx).rename_is_focused(window)
                    {
                        active_manager.update(cx, |manager, cx| manager.activate_without_focus(cx));
                    } else {
                        active_manager.update(cx, |manager, cx| manager.activate(window, cx));
                    }
                }
                debug_assert_eq!(
                    Some(active_workspace_id),
                    self.workspaces.active_workspace_id()
                );
                self.pending_final_tab_closes.remove(&workspace_id);
                self.sidebar.update(cx, |sidebar, cx| {
                    sidebar.cancel_rename_for(workspace_id, cx)
                });
                self.sync_terminal_focus_blocker(window, cx);
                if was_active {
                    self.scroll_active_workspace_into_view(cx);
                }

                cx.notify();
            }
            FinalTabCloseOutcome::CloseOperatingSystemWindow {
                workspace_id: final_workspace_id,
            } => {
                debug_assert_eq!(final_workspace_id, workspace_id);
                if let Some(manager) = self.active_tab_manager() {
                    manager.update(cx, |manager, cx| manager.close_all(cx));
                }
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
            self.transient.end_pin();
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
                if self.activate_workspace(workspace_id, window, cx) {
                    // An expanded row stands for the Tabs outside its Worktrees.
                    if self
                        .worktree_section(workspace_id, cx)
                        .is_some_and(|section| section.expanded)
                    {
                        self.show_unscoped_tabs(workspace_id, focus_pane, window, cx);
                    }
                    if focus_pane {
                        self.focus(window, cx);
                    }
                }
            }
            SidebarEvent::Command {
                workspace_id,
                command,
            } => self.perform_workspace_menu_command(workspace_id, command, window, cx),
            SidebarEvent::Rename {
                workspace_id,
                name,
                version,
            } => {
                // A shared Workspace's name belongs to the server. The sidebar keeps showing the
                // server's name until the server accepts this one, and the draft stays fenced to
                // the version it was started from.
                if self.rename_shared_workspace(workspace_id, &name, version, window, cx) {
                    return;
                }
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
            SidebarEvent::WorktreeCommand {
                workspace_id,
                worktree_id,
                command,
            } => match command {
                WorktreeMenuCommand::NewTab => {
                    if self.activate_workspace(workspace_id, window, cx)
                        && self.new_worktree_tab(workspace_id, worktree_id, window, cx)
                    {
                        self.focus(window, cx);
                    }
                }
                WorktreeMenuCommand::CopyPath => {
                    self.copy_worktree_path(workspace_id, worktree_id, cx)
                }
                WorktreeMenuCommand::CloseTabs => self.request_close(
                    CloseTarget::Worktree {
                        workspace_id,
                        worktree_id,
                    },
                    window,
                    cx,
                ),
                WorktreeMenuCommand::Remove => {
                    self.request_worktree_removal(workspace_id, worktree_id, window, cx);
                }
            },
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
                // A secondary click doesn't activate its row's Workspace, so the new Tab does.
                if self.activate_workspace(workspace_id, window, cx)
                    && let Some(workspace) = self.workspaces.workspace(workspace_id)
                {
                    workspace
                        .payload()
                        .update(cx, |manager, cx| manager.create_tab(window, cx));
                    self.focus(window, cx);
                }
                self.sync_terminal_focus_blocker(window, cx);
                cx.notify();
            }
            WorkspaceMenuCommand::NewWorktree => {
                self.new_worktree(workspace_id, window, cx);
            }
            WorkspaceMenuCommand::Reconnect => {
                self.start_remote_workspace_reconnect(workspace_id, window, cx)
            }
            WorkspaceMenuCommand::Detach => self.detach_shared_workspace(workspace_id, window, cx),
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

    fn on_new_worktree(&mut self, _: &NewWorktree, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(workspace_id) = self.workspaces.active_workspace_id() {
            self.new_worktree(workspace_id, window, cx);
        }
    }

    fn on_next_worktree(&mut self, _: &NextWorktree, window: &mut Window, cx: &mut Context<Self>) {
        self.step_worktree(true, window, cx);
    }

    fn on_previous_worktree(
        &mut self,
        _: &PreviousWorktree,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_worktree(false, window, cx);
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
            // The account server owns local work, so a new local Workspace is the server's.
            WorkspaceCreation::Local if self.launch.is_some() => {
                self.create_shared_workspace(name, None, window, cx);
            }
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
            WorkspaceCreation::Server => {
                self.create_shared_workspace(name, None, window, cx);
            }
        }
    }

    fn on_close_workspace(
        &mut self,
        _: &CloseWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The empty view has no Workspace to close.
        if let Some(workspace_id) = self.workspaces.active_workspace_id() {
            self.request_close(CloseTarget::Workspace(workspace_id), window, cx);
        }
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

    /// A Workspace's collapsed title-bar identity, naming its Active Worktree when its row
    /// discloses Worktrees.
    fn chrome_identity(&self, workspace_id: WorkspaceId, cx: &App) -> WorkspaceChromeIdentity {
        let Some(workspace) = self.workspaces.workspace(workspace_id) else {
            return WorkspaceChromeIdentity {
                name: String::new(),
                worktree: None,
                pinned: false,
                status: None,
            };
        };
        // A shared Workspace's Worktrees and pin live on the owning server, which lists the
        // Worktrees and publishes the pin.
        let shared = self.shared_row_facts(workspace_id);
        WorkspaceChromeIdentity {
            worktree: self.worktree_section(workspace_id, cx).and_then(|section| {
                section
                    .rows()
                    .find(|row| row.active)
                    .map(|row| row.name.to_string())
            }),
            pinned: shared.as_ref().map_or_else(
                || workspace.pinned_directory().is_some(),
                |shared| shared.pinned,
            ),
            ..chrome_identity(workspace)
        }
    }

    /// The Active Workspace's chrome identity, or an empty one while the window presents no
    /// Workspace.
    fn active_chrome_identity(&self, cx: &App) -> WorkspaceChromeIdentity {
        match self.workspaces.active_workspace_id() {
            Some(workspace_id) => self.chrome_identity(workspace_id, cx),
            None => WorkspaceChromeIdentity {
                name: String::new(),
                worktree: None,
                pinned: false,
                status: None,
            },
        }
    }

    /// The Active Workspace's identity, shown in the top-left chrome only while the sidebar is
    /// hidden. The empty view has none, so the chooser shows only its glyph.
    fn workspace_chrome_identity(&self, cx: &App) -> Option<(WorkspaceChromeIdentity, Tooltip)> {
        let workspace = self.workspaces.active_workspace()?;
        let remote_connection_phase = workspace
            .remote_connection_state()
            .map(RemoteConnectionState::phase);
        let remote_status = remote_connection_phase.and_then(remote_connection_status);
        let shared = self.shared_row_facts(workspace.id());
        let (path, _) = match &shared {
            // The directory belongs to the server, so the chrome shows the server's own.
            Some(shared) => (
                compact_home_path(&shared.directory, &self.local_home_directory_path),
                String::new(),
            ),
            None => directory_labels(
                workspace.location(),
                workspace.local_display_directory(),
                workspace.remote_display_directory(),
                &self.local_home_directory_path,
            ),
        };
        let tooltip_detail = match workspace.availability() {
            DirectoryAvailability::Unavailable { reason } => format!("{path}: {reason}"),
            DirectoryAvailability::Available => remote_status
                .map(|status| format!("{path}: {status}"))
                .unwrap_or_else(|| path.clone()),
        };

        // A shared Workspace's Repository Status is what the owning server read.
        let repository = match &shared {
            Some(_) => self.shared_chip_line(workspace.id(), cx),
            None => self.chip_repository_line(workspace.id(), cx),
        }
        .map(|line| format!("\n{line}"))
        .unwrap_or_default();
        Some((
            self.chrome_identity(workspace.id(), cx),
            Tooltip::new("workspace-switcher-tooltip", "Switch Workspace")
                .detail(format!(
                    "{}\n{tooltip_detail}{repository}",
                    workspace.name()
                ))
                .debug_selector("workspace-switcher-tooltip"),
        ))
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
        // The collapsed identity eases in beside the glyph as the sidebar hides, and out as it
        // shows.
        let presence = layout.identity_presence();
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
        let creation_rows = WorkspaceCreation::rows(self.offers_server_creation_row());
        let creation_shortcuts: Vec<_> = creation_rows
            .iter()
            .map(|creation| creation.shortcut(presentation))
            .collect();
        let collapsed_identity = (presence > 0.0)
            .then(|| self.workspace_chrome_identity(cx))
            .flatten()
            .map(|(identity, tooltip)| {
                let motion = layout.identity_motion(&identity, window, cx);
                (identity, tooltip, motion)
            });
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
            |(identity, _, _)| identity.accessibility_name(),
        );
        let chooser = ComboBox::with_commands(
            "workspace-switcher",
            switcher_accessibility_name,
            self.workspaces
                .active_workspace_id()
                .map(SwitcherChoice::Presented),
            "Switch Workspace",
            self.workspace_switcher_items(cx),
            move |_| {
                creation_rows
                    .clone()
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
        .when(presence == 0.0, |chooser| chooser.ghost_trigger())
        .when(presence > 0.0, |chooser| {
            // The chip's surface fades with the identity it holds.
            let fade = |color: Option<Color>| {
                let color = gpui_color(color.unwrap_or(Color::rgba(0)));
                gpui::Rgba {
                    a: color.a * presence,
                    ..color
                }
            };
            let normal = spaceterm_ui::ButtonPaint::new(
                fade(switcher_surface.fill),
                switcher_foreground,
                fade(switcher_surface.rim),
            );
            let hovered = spaceterm_ui::ButtonPaint::new(
                fade(switcher_surface.hover_fill.or(switcher_surface.fill)),
                switcher_foreground,
                fade(switcher_surface.hover_rim.or(switcher_surface.rim)),
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
        .when_some(
            collapsed_identity,
            |chooser, (identity, tooltip, motion)| {
                chooser
                    .custom_trigger(identity.render(
                        switcher_foreground,
                        appearance,
                        switcher_status_hosts,
                        motion,
                    ))
                    .custom_trigger_height(frame.top_chip_height(appearance.top_height()))
                    .full_width(motion.is_none())
                    .tooltip(
                        tooltip
                            .shortcut(presentation.shortcut(&SwitchWorkspace).unwrap_or_default()),
                    )
            },
        )
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
                match *acceptance.item_id() {
                    SwitcherChoice::Presented(workspace_id) => {
                        manager.activate_workspace(workspace_id, window, cx);
                    }
                    SwitcherChoice::Detached(server) => {
                        manager.attach_detached_workspace(server, window, cx);
                    }
                }
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
                let mut path: gpui::SharedString = path.into();
                let mut tooltip = tooltip;
                // A shared Workspace's directory, pin, Worktrees, and Repository Status all live
                // on the owning server. The row shows what the server read and published; this
                // client reads none of it.
                let shared = self.shared_row_facts(workspace.id());
                let mut repository = match &shared {
                    Some(shared) => {
                        path =
                            compact_home_path(&shared.directory, &self.local_home_directory_path)
                                .into();
                        tooltip =
                            format!("{SERVER_WORKSPACE_BADGE}: {}", shared.directory.display());
                        self.shared_sidebar_badge(workspace.id(), cx)
                    }
                    None => self.sidebar_badge(workspace.id(), cx),
                };
                let worktrees = self.worktree_section(workspace.id(), cx);
                // A disclosing row describes the Workspace. Its directory and branch belong to
                // its Worktrees, and a collapsed row shows its Active Worktree's.
                if let Some(section) = worktrees.as_ref().filter(|_| available) {
                    let pinned = match &shared {
                        Some(shared) => shared.pinned.then(|| shared.directory.clone()),
                        None => workspace
                            .local_display_directory()
                            .filter(|_| workspace.pinned_directory().is_some())
                            .map(std::path::Path::to_path_buf),
                    };
                    let active = section
                        .rows()
                        .find(|worktree| worktree.active)
                        .filter(|_| !section.expanded);
                    tooltip = match active {
                        Some(worktree) => {
                            path = worktree.directory.clone();
                            repository = worktree.repository.clone();
                            format!("Worktree {}: {}", worktree.name, worktree.directory_tooltip)
                        }
                        None => section
                            .repository
                            .as_ref()
                            .map_or_else(String::new, |root| format!("Repository: {root}")),
                    };
                    if let Some(pinned) = pinned {
                        tooltip.push_str(&format!("\nPinned Directory: {}", pinned.display()));
                    }
                    if shared.is_some() {
                        tooltip = format!("{SERVER_WORKSPACE_BADGE}\n{tooltip}");
                    }
                }
                WorkspaceRowViewModel {
                    workspace_id: workspace.id(),
                    name: workspace.name().to_owned().into(),
                    path,
                    machine: match workspace.location() {
                        WorkspaceLocation::Local => None,
                        WorkspaceLocation::Remote { key, .. } => {
                            Some(key.destination().host().to_owned().into())
                        }
                    },
                    tooltip: tooltip.into(),
                    pinned: shared.as_ref().map_or_else(
                        || workspace.pinned_directory().is_some(),
                        |shared| shared.pinned,
                    ),
                    remote_connection_phase: workspace
                        .remote_connection_state()
                        .map(RemoteConnectionState::phase),
                    available,
                    repository,
                    worktrees,
                    creates_worktrees: available && self.creates_worktrees(workspace.id(), cx),
                    active: Some(workspace.id()) == active_workspace_id,
                    shared: shared.map(|shared| shared.row),
                }
            })
            .collect()
    }
}

/// What a shared Workspace's sidebar row overrides, when a SpaceTerm Server owns the Workspace.
struct SharedRowFacts {
    row: super::workspace_sidebar::SharedWorkspaceRow,
    /// The directory the server reports. It names no directory this client may read, so the row
    /// only ever displays it.
    directory: PathBuf,
    pinned: bool,
}

/// Shared server Workspaces: adopting them, presenting what the server publishes, submitting the
/// edits a person asks for, and refusing the operations the server path does not support yet.
impl WorkspaceManager {
    /// Presents the Workspaces one shared edit channel publishes.
    ///
    /// The server this reaches is chosen by construction, so the local SpaceTerm Server and a
    /// scripted one follow the same path through this window.
    pub(crate) fn present_shared_workspaces(
        &mut self,
        edits: super::shared_workspaces::SharedEditSource,
        sessions: super::shared_workspaces::SharedPaneSessions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::shared_workspaces::SharedWorkspaces;

        if self.shared.is_some() {
            return;
        }
        self.shared = Some(SharedWorkspaces::new(edits, sessions));
        self.follow_shared_pull_requests(cx);
        if let Some(handle) = window.window_handle().downcast::<Self>() {
            cx.set_global(super::shared_workspaces::SharedWorkspaceHost(handle));
        }
        self._shared_poll = Some(cx.spawn_in(window, async move |manager, cx| {
            loop {
                cx.background_executor()
                    .timer(super::shared_workspaces::POLL_INTERVAL)
                    .await;
                if manager
                    .update_in(cx, |manager, window, cx| {
                        manager.sync_shared_workspaces(window, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
        cx.notify();
    }

    /// Reads the server's answer and its inventory, then presents both.
    fn sync_shared_workspaces(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ready = self
            .shared
            .as_mut()
            .map(|shared| shared.prepare_client_views());
        if !matches!(ready, Some(Ok(true))) {
            self.report_client_view_error(window, cx);
            return;
        }
        let Some(poll) = self
            .shared
            .as_mut()
            .map(super::shared_workspaces::SharedWorkspaces::poll)
        else {
            return;
        };
        // The removal that answers this client's own close takes the keyboard to what remains.
        let removal = if poll.accepted.as_ref().is_some_and(|accepted| {
            matches!(
                accepted.mutation,
                spaceterm_server::hierarchy::Mutation::Close { .. }
            )
        }) {
            super::shared_workspaces::SharedRemoval::OwnClose
        } else {
            super::shared_workspaces::SharedRemoval::Published
        };
        if let Some(inventory) = poll.inventory {
            self.apply_shared_inventory(&inventory, removal, window, cx);
        }
        // A new client's first view is the person opening SpaceTerm to work, so it opens the
        // Active Tab and its Focused Pane's shell. Later launches restore views passively.
        if let Some(workspace_id) = poll
            .initial
            .and_then(|server| self.shared.as_ref()?.local(server))
            && self.activate_workspace(workspace_id, window, cx)
        {
            self.focus(window, cx);
        }
        // The client that asked for a creation opens it, once, after the server published it.
        if let Some(accepted) = poll.accepted {
            self.open_accepted_shared(&accepted, window, cx);
        }
        if let Some(conflict) = self
            .shared
            .as_mut()
            .and_then(super::shared_workspaces::SharedWorkspaces::take_conflict)
        {
            self.present_shared_conflict(conflict, window, cx);
        }
        if let Some(refusal) = self
            .shared
            .as_mut()
            .and_then(super::shared_workspaces::SharedWorkspaces::take_refusal)
        {
            Self::present_shared_refusal(refusal, window, cx);
        }
        if let Some((server_target, local)) = self
            .shared
            .as_mut()
            .and_then(super::shared_workspaces::SharedWorkspaces::take_stale_close)
        {
            self.present_stale_shared_close(server_target, local, window, cx);
        }
        self.revalidate_shared_close(window, cx);
        self.open_created_server_worktree(window, cx);
        self.revalidate_server_removal(window, cx);
        if let Some((operation, error)) =
            self.shared.as_mut().and_then(|s| s.take_worktree_failure())
        {
            use spaceterm_server::worktree::Operation;
            let presented = match &operation {
                Operation::Create { path, .. } => {
                    self.worktree_creation_failure_presented(path, error, cx)
                }
                Operation::SetLocation { .. } => true,
                Operation::Remove { .. } | Operation::OpenTab { .. } => false,
            };
            if !presented {
                self.server_worktree_operation_failure(&operation, error, window, cx);
            }
        }
        self.save_shared_arrangements(cx);
        self.report_client_view_error(window, cx);
        // Membership, connection, and Git facts all decide what each view presents, including
        // Workspaces nobody is looking at.
        if poll.changed {
            self.present_shared_repositories(cx);
        }
    }

    fn save_shared_arrangements(&mut self, cx: &App) {
        let Some(shared) = self.shared.as_ref() else {
            return;
        };
        let workspaces = self
            .workspaces
            .iter()
            .filter(|entry| shared.is_shared(entry.id()))
            .filter_map(|entry| entry.payload().read(cx).shared_arrangement(cx))
            .collect();
        if let Some(shared) = self.shared.as_mut() {
            shared.save_arrangements(spaceterm_server::client_view::ClientView { workspaces });
        }
    }

    fn report_client_view_error(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(notice) = self
            .shared
            .as_mut()
            .and_then(|shared| shared.take_view_error())
            && !super::settings_window::present_over_active_settings(cx, move |window, cx| {
                notice.present(window, cx)
            })
        {
            notice.present(window, cx);
        }
    }

    /// Applies one published inventory. Every membership change the server made reaches this
    /// window here, including the answer to this client's own accepted edit.
    fn apply_shared_inventory(
        &mut self,
        inventory: &spaceterm_server::hierarchy::Inventory,
        removal: super::shared_workspaces::SharedRemoval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::shared_workspaces::SharedWorkspacePlan;

        let Some(plan) = self.shared.as_ref().map(|shared| shared.plan(inventory)) else {
            return;
        };
        for step in plan {
            match step {
                SharedWorkspacePlan::Create { facts } => {
                    let Some(workspace) = inventory
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.id == facts.server)
                    else {
                        continue;
                    };
                    self.present_shared_workspace(&facts, workspace, window, cx);
                }
                SharedWorkspacePlan::Update { local, facts } => {
                    let Some(workspace) = inventory
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.id == facts.server)
                    else {
                        continue;
                    };
                    // The server's name is authoritative, including when it rejected this
                    // client's own draft.
                    if self
                        .workspaces
                        .workspace(local)
                        .is_some_and(|entry| entry.name() != facts.name)
                        && let Err(error) =
                            self.workspaces.rename_workspace(local, facts.name.clone())
                    {
                        Self::report_workspace_error("rename the shared", error);
                    }
                    if let Some(shared) = self.shared.as_mut() {
                        shared.register(local, facts);
                    }
                    if let Some(manager) = self
                        .workspaces
                        .workspace(local)
                        .map(|entry| entry.payload().clone())
                    {
                        manager.update(cx, |manager, cx| {
                            manager.reconcile_shared(workspace, removal, window, cx);
                        });
                    }
                }
                // The server no longer has the Workspace, so neither does this window. Leaving
                // it behind would turn server data into desktop-owned work, and an inactive
                // Pane's connection may never report its own end.
                SharedWorkspacePlan::Removed { local } => {
                    self.remove_shared_workspace(local, removal, window, cx);
                }
            }
        }
        self.synchronize_tab_manager_layouts(window, cx);
        cx.notify();
    }

    /// Presents one server Workspace this window has not seen before. It joins the sidebar and
    /// never becomes the Active Workspace, because another client's creation is membership here.
    fn present_shared_workspace(
        &mut self,
        facts: &super::shared_workspaces::SharedWorkspaceFacts,
        workspace: &spaceterm_server::hierarchy::Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::shared_workspaces::SharedTabs;

        let Some(shared) = self.shared.as_ref() else {
            return;
        };
        let sessions = shared.sessions().clone();
        let edits = shared.edits();
        let Some(root_tab) = workspace
            .tabs
            .iter()
            .find(|tab| tab.id == workspace.root_tab)
        else {
            return;
        };
        let Some((session_factory, prepared_launch, root_state)) =
            sessions.open(root_tab.root_pane)
        else {
            return;
        };
        // The Workspace's own directory stays this client's home: it is launch scaffolding the
        // server-backed factory ignores, and the sidebar shows the server's directory instead.
        let Ok(directory) = self.local_home_directory() else {
            Self::show_home_directory_unavailable(window, cx);
            return;
        };
        let active_workspace_id = self.workspaces.active_workspace_id();
        let active_manager = self.active_tab_manager();
        let window_drag_platform = Rc::clone(&self.operating_system_window_drag_platform);
        let sidebar_visible = self.sidebar.read(cx).layout().visible;
        let sidebar_width = self.sidebar.read(cx).layout().width;
        let pane_construction = sessions.panes().clone();
        let created = self
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
        let workspace_id = match created {
            Ok(workspace_id) => workspace_id,
            Err(error) => {
                Self::report_workspace_error("present the shared", error);
                return;
            }
        };
        if let Err(error) = self
            .workspaces
            .rename_workspace(workspace_id, facts.name.clone())
        {
            Self::report_workspace_error("name the shared", error);
        }
        // Creating a Workspace activates it. The Active Workspace is this client's own view
        // state, so it returns to where a person left it and nothing they type moves. The empty
        // view had none, so the new Workspace stays active there.
        if let Some(active_workspace_id) = active_workspace_id
            && let Err(error) = self.workspaces.activate_workspace(active_workspace_id)
        {
            Self::report_workspace_error("restore the Active", error);
        }
        let Some(manager) = self
            .workspaces
            .workspace(workspace_id)
            .map(|entry| entry.payload().clone())
        else {
            return;
        };
        manager.update(cx, |manager, cx| {
            manager.deactivate(cx);
            manager.adopt_shared(
                SharedTabs::new(edits, sessions, facts.server),
                root_tab.id,
                root_tab.root_pane,
                root_state,
                window,
                cx,
            );
        });
        // The Active Workspace keeps the Tab strip, because it never changed. Leaving the empty
        // view presents the new Workspace without selecting a Pane, which would ask the server
        // for control nobody requested.
        active_manager
            .as_ref()
            .unwrap_or(&manager)
            .update(cx, |manager, cx| manager.activate_without_focus(cx));
        if let Some(shared) = self.shared.as_mut() {
            shared.register(workspace_id, facts.clone());
        }
        self.synchronize_tab_manager_layout(workspace_id, window, cx);
        if let Some(manager) = self
            .workspaces
            .workspace(workspace_id)
            .map(|entry| entry.payload().clone())
        {
            manager.update(cx, |manager, cx| {
                manager.reconcile_shared(
                    workspace,
                    super::shared_workspaces::SharedRemoval::Published,
                    window,
                    cx,
                );
            });
        }
        cx.notify();
        if let Some(saved) = self
            .shared
            .as_ref()
            .and_then(|shared| shared.saved_arrangement(workspace.id))
            .cloned()
        {
            manager.update(cx, |manager, cx| {
                manager.restore_shared_arrangement(&saved, workspace, cx)
            });
        }
    }

    /// Asks the server for a Workspace it owns and every connected client presents.
    /// Returns whether the request reached the server. A refusal is presented here.
    fn create_shared_workspace(
        &mut self,
        name: String,
        directory: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use super::shared_workspaces::{SharedRefusalReason, SharedSubmission};

        let name = match name.trim() {
            "" => self.next_shared_workspace_name(directory.as_deref()),
            name => name.to_owned(),
        };
        let Some(shared) = self.shared.as_mut() else {
            // The launch screen already says why the server is not ready, and offers its step.
            if self.workspaces.active_workspace_id().is_none() && self.launch.is_some() {
                self.focus(window, cx);
            } else {
                Self::show_shared_refusal(
                    "create a Workspace",
                    SharedRefusalReason::Offline,
                    window,
                    cx,
                );
            }
            return false;
        };
        let submission = shared.create_workspace(name, directory);
        if let SharedSubmission::Refused(reason) = submission {
            Self::show_shared_refusal("create a Workspace", reason, window, cx);
        }
        cx.notify();
        submission == SharedSubmission::Submitted
    }

    /// The name a new shared Workspace takes when a person supplied none: the chosen directory's
    /// name, or the next default name no presented Workspace uses, as desktop Workspaces were
    /// named. The server keeps whatever name it receives.
    fn next_shared_workspace_name(&self, directory: Option<&std::path::Path>) -> String {
        if let Some(name) = directory
            .and_then(std::path::Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
        {
            return name;
        }
        let names: BTreeSet<String> = self
            .shared
            .as_ref()
            .and_then(|shared| shared.latest_inventory())
            .into_iter()
            .flat_map(|inventory| &inventory.workspaces)
            .map(|workspace| workspace.name.clone())
            .collect();
        (1..)
            .map(|number| match number {
                1 => "Default".to_owned(),
                number => format!("Default ({number})"),
            })
            .find(|name| !names.contains(name))
            .unwrap_or_default()
    }

    /// Asks the server to rename a shared Workspace, and reports whether the Workspace was one.
    fn rename_shared_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        name: &str,
        observed_version: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use super::shared_workspaces::SharedSubmission;

        let Some(shared) = self
            .shared
            .as_mut()
            .filter(|shared| shared.is_shared(workspace_id))
        else {
            return false;
        };
        if let SharedSubmission::Refused(reason) =
            shared.rename(workspace_id, name.to_owned(), observed_version)
        {
            Self::show_shared_refusal("rename the Workspace", reason, window, cx);
        }
        cx.notify();
        true
    }

    /// Asks the server to pin or unpin a shared Workspace's directory. It reports `None` when the
    /// Workspace is this window's own, so the local pin applies instead.
    ///
    /// The chosen directory is a local choice a person made with local authority, and it reaches
    /// the server as the value it should hold. The server's own answer is never read back as local
    /// authority.
    fn pin_shared_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        pin: Option<&PinnedDirectory>,
        observed_version: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        use super::shared_workspaces::SharedSubmission;

        if !self
            .shared
            .as_ref()
            .is_some_and(|shared| shared.is_shared(workspace_id))
        {
            return None;
        }
        let directory = match pin {
            Some(PinnedDirectory::Local(directory)) => Some(directory.path().to_owned()),
            // A Remote Workspace's directory is never a shared server Workspace's directory.
            Some(PinnedDirectory::Remote { .. }) => return Some(false),
            None => None,
        };
        self.pin_operation = self.pin_operation.wrapping_add(1);
        self.transient.end_pin();
        // `observed_version` is the pin version the chooser opened on. An immediate Unpin has no
        // chooser, so it carries none and is fenced to the version its row showed.
        let shared = self.shared.as_mut()?;
        let submission = shared.pin(workspace_id, directory, observed_version);
        if let SharedSubmission::Refused(reason) = submission {
            Self::show_shared_refusal("change the pinned directory", reason, window, cx);
        }
        cx.notify();
        Some(submission == SharedSubmission::Submitted)
    }

    /// Asks to close a Pane, Tab, or Workspace a SpaceTerm Server owns, returning whether the
    /// target is shared.
    ///
    /// The close ends Terminal Sessions for every client, including Panes another client
    /// controls, so it always opens a Close Confirmation. The confirmation captures exactly which
    /// Panes and Sessions the server reports; a confirmed close asks the server to end only those.
    fn request_shared_close(
        &mut self,
        target: CloseTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use super::shared_workspaces::{SharedCloseReview, SharedCloseUnavailable};

        let workspace_id = match target {
            CloseTarget::Workspace(workspace_id)
            | CloseTarget::Tab { workspace_id, .. }
            | CloseTarget::Pane { workspace_id, .. }
            | CloseTarget::Worktree { workspace_id, .. } => workspace_id,
            CloseTarget::Window => return false,
        };
        if !self.is_shared_workspace(workspace_id) {
            return false;
        }
        if self.close_confirmation.pending().is_some() {
            return true;
        }
        let Some(server_target) = self.shared_close_target(target, cx) else {
            // A shared Workspace lists no Worktrees, and a Tab or Pane this window cannot name on
            // the server has already left it.
            return true;
        };
        let Some(shared) = self.shared.as_ref() else {
            return true;
        };
        let approval = match shared.close_approval(server_target) {
            Ok(approval) => approval,
            Err(SharedCloseUnavailable::Edits(reason)) => {
                Self::show_shared_refusal(Self::shared_close_operation(target), reason, window, cx);
                return true;
            }
            Err(SharedCloseUnavailable::Missing) => {
                Self::present_shared_refusal(
                    super::shared_workspaces::SharedRefusal::Missing,
                    window,
                    cx,
                );
                return true;
            }
            Err(SharedCloseUnavailable::Unreported) => {
                Self::show_shared_unavailable(
                    "The server has not reported which Terminal Sessions this would end yet, so \
                     SpaceTerm cannot show what you would approve. Nothing was closed.",
                    window,
                    cx,
                );
                return true;
            }
        };
        let Some(window_handle) = window.window_handle().downcast::<Self>() else {
            return true;
        };
        let Some(generation) = self.close_confirmation.begin(target) else {
            return true;
        };
        let mut alert = Alert::new(
            ModalId::new("shared-close-confirmation"),
            approval.title(),
            approval.title(),
            approval.message(),
            vec![
                ModalAction::new(
                    CloseConfirmationAction::Confirm,
                    approval.confirm_label(),
                    ModalActionRole::Affirmative,
                    SHARED_CLOSE_CONFIRM_SELECTOR,
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    CloseConfirmationAction::Cancel,
                    "Cancel",
                    ModalActionRole::Cancel,
                    SHARED_CLOSE_CANCEL_SELECTOR,
                ),
            ],
        )
        .intent(AlertIntent::Critical);
        if let Some(detail) = approval.detail() {
            alert = alert.detail(detail);
        }
        let presented = alert.present(window, cx, move |outcome, cx| {
            let confirmed = matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: CloseConfirmationAction::Confirm,
                    ..
                }
            );
            let _ = window_handle.update(cx, |manager, window, cx| {
                manager.settle_shared_close(generation, target, confirmed, window, cx);
            });
        });
        match presented {
            Ok(presentation) => {
                if let Some(shared) = self.shared.as_mut() {
                    shared.begin_review(SharedCloseReview {
                        generation,
                        target,
                        approval,
                        presentation: Some(presentation),
                    });
                }
            }
            Err(error) => {
                self.close_confirmation.presentation_failed(generation);
                eprintln!("failed to present the shared close confirmation: {error}");
            }
        }
        true
    }

    /// What a refused shared close says it did not do.
    const fn shared_close_operation(target: CloseTarget) -> &'static str {
        match target {
            CloseTarget::Pane { .. } => "close the Pane",
            CloseTarget::Tab { .. } => "close the Tab",
            _ => "kill the Workspace",
        }
    }

    /// The server identities a local close target names, read from this window's own map.
    fn shared_close_target(
        &self,
        target: CloseTarget,
        cx: &App,
    ) -> Option<spaceterm_server::hierarchy::CloseTarget> {
        use spaceterm_server::hierarchy::CloseTarget as ServerCloseTarget;

        let shared = self.shared.as_ref()?;
        let tab_manager = |workspace_id| {
            self.workspaces
                .workspace(workspace_id)
                .map(|entry| entry.payload().read(cx))
        };
        match target {
            CloseTarget::Workspace(workspace_id) => Some(ServerCloseTarget::Workspace {
                workspace: shared.facts(workspace_id)?.server,
            }),
            CloseTarget::Tab {
                workspace_id,
                tab_id,
            } => Some(ServerCloseTarget::Tab {
                workspace: shared.facts(workspace_id)?.server,
                tab: tab_manager(workspace_id)?.shared_server_tab(tab_id)?,
            }),
            CloseTarget::Pane {
                workspace_id,
                tab_id,
                pane_id,
            } => {
                let manager = tab_manager(workspace_id)?;
                Some(ServerCloseTarget::Pane {
                    workspace: shared.facts(workspace_id)?.server,
                    tab: manager.shared_server_tab(tab_id)?,
                    pane: manager.shared_server_pane(tab_id, pane_id, cx)?,
                })
            }
            CloseTarget::Worktree { .. } | CloseTarget::Window => None,
        }
    }

    /// Settles a shared close confirmation. A confirmed close asks the server to end exactly what
    /// the confirmation showed, and only while that is still what this client last read.
    fn settle_shared_close(
        &mut self,
        generation: u64,
        target: CloseTarget,
        confirmed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::shared_workspaces::{SharedCloseState, SharedSubmission};

        let review = self
            .shared
            .as_mut()
            .and_then(|shared| shared.end_review(generation));
        if !self
            .close_confirmation
            .settle_approval(generation, target, confirmed)
        {
            return;
        }
        let (Some(review), Some(shared)) = (review, self.shared.as_mut()) else {
            return;
        };
        match shared.close_state(&review.approval) {
            SharedCloseState::Current => {
                if let SharedSubmission::Refused(reason) =
                    shared.close(review.approval, review.target)
                {
                    Self::show_shared_refusal(
                        Self::shared_close_operation(target),
                        reason,
                        window,
                        cx,
                    );
                }
            }
            SharedCloseState::Changed => {
                Self::present_shared_close_changed(
                    review.approval.changed_title(),
                    Some(target),
                    window,
                    cx,
                );
            }
            SharedCloseState::Missing => {
                Self::present_shared_close_missing(&review.approval, window, cx);
            }
        }
        cx.notify();
    }

    /// Withdraws a shared close confirmation that another client's change made stale, and says
    /// why. The person never approves Sessions or Panes they were not shown.
    fn revalidate_shared_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use super::shared_workspaces::SharedCloseState;

        let Some(shared) = self.shared.as_mut() else {
            return;
        };
        let Some((generation, state)) = shared
            .review()
            .map(|review| (review.generation, shared.close_state(&review.approval)))
            .filter(|(_, state)| *state != SharedCloseState::Current)
        else {
            return;
        };
        let Some(review) = shared.end_review(generation) else {
            return;
        };
        // The pending confirmation ends here, so its own deferred answer settles nothing.
        self.close_confirmation.presentation_failed(generation);
        if let Some(presentation) = review.presentation.as_ref() {
            let _ = presentation.dismiss(window, cx);
        }
        match state {
            SharedCloseState::Missing => {
                Self::present_shared_close_missing(&review.approval, window, cx);
            }
            _ => Self::present_shared_close_changed(
                review.approval.changed_title(),
                Some(review.target),
                window,
                cx,
            ),
        }
    }

    /// Explains that what a close would end changed after it was approved or shown, so nothing
    /// was closed. Review opens a fresh confirmation of what the server holds now; nothing asks
    /// again on its own.
    fn present_shared_close_changed(
        title: &'static str,
        target: Option<CloseTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = "A Pane was added or removed, or a Terminal Session started or ended, \
                       after the confirmation opened. Nothing was closed.";
        let mut actions = Vec::new();
        if target.is_some() {
            actions.push(
                ModalAction::new(
                    true,
                    "Review",
                    ModalActionRole::Affirmative,
                    SHARED_CLOSE_REVIEW_SELECTOR,
                )
                .with_emphasis(ModalActionEmphasis::Prominent),
            );
        }
        actions.push(ModalAction::new(
            false,
            if target.is_some() { "Cancel" } else { "OK" },
            ModalActionRole::Cancel,
            SHARED_CLOSE_DISMISS_SELECTOR,
        ));
        let manager = cx.weak_entity();
        let window_handle = window.window_handle();
        let result = Alert::new(
            ModalId::new("shared-close-changed"),
            title,
            title,
            message,
            actions,
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, move |outcome, cx| {
            let reviewed = matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            );
            let Some(target) = target.filter(|_| reviewed) else {
                return;
            };
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = manager.update(cx, |manager, cx| {
                    manager.request_close(target, window, cx);
                });
            });
        });
        if let Err(error) = result {
            eprintln!("failed to present the changed shared close: {error}");
        }
    }

    /// Presents a close the server refused because what it would end changed first. Review is
    /// offered only to the process that asked, and only while its target is still here.
    fn present_stale_shared_close(
        &mut self,
        server_target: spaceterm_server::hierarchy::CloseTarget,
        local: Option<CloseTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let local =
            local.filter(|local| self.shared_close_target(*local, cx) == Some(server_target));
        Self::present_shared_close_changed(
            super::shared_workspaces::shared_close_changed_title(server_target),
            local,
            window,
            cx,
        );
    }

    /// Explains that another client already removed what a close confirmation named.
    fn present_shared_close_missing(
        approval: &super::shared_workspaces::SharedCloseApproval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = Alert::new(
            ModalId::new("shared-close-missing"),
            approval.missing_title(),
            approval.missing_title(),
            "Another client removed it while the confirmation was open. Nothing else changed.",
            vec![ModalAction::new(
                (),
                "OK",
                ModalActionRole::Cancel,
                SHARED_CLOSE_DISMISS_SELECTOR,
            )],
        )
        .intent(AlertIntent::Informational)
        .present(window, cx, |_, _| {});
    }

    /// Presents a refused same-field edit: the server's value, the draft it rejected, and the
    /// choice to ask again with that draft. Nothing retries on its own.
    fn present_shared_conflict(
        &mut self,
        conflict: super::shared_workspaces::SharedWorkspaceConflict,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self.workspaces.workspace(conflict.workspace).map_or_else(
            || "this Workspace".to_owned(),
            |entry| entry.name().to_owned(),
        );
        let detail = format!(
            "Another client changed the {} of {name} first, so the server kept its value and your \
             change was not applied.\n\nOn the server: {}\nYour change: {}",
            conflict.field.label(),
            conflict.authoritative,
            conflict.draft.display(),
        );
        let owner = cx.weak_entity();
        let window_handle = window.window_handle();
        let result = Alert::new(
            ModalId::new("shared-workspace-conflict"),
            "Change not applied",
            conflict.field.title(),
            detail,
            vec![
                ModalAction::new(
                    true,
                    "Try Again",
                    ModalActionRole::Affirmative,
                    SHARED_CONFLICT_RETRY_SELECTOR,
                )
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    false,
                    "Keep the Server\u{2019}s Value",
                    ModalActionRole::Cancel,
                    SHARED_CONFLICT_KEEP_SELECTOR,
                ),
            ],
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, move |outcome, cx| {
            if !matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            ) {
                return;
            }
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = owner.update(cx, |manager, cx| {
                    manager.retry_shared_edit(&conflict, window, cx);
                });
            });
        });
        if let Err(error) = result {
            eprintln!("failed to present the shared change conflict: {error}");
        }
    }

    /// Asks the server again with the preserved draft and the version shown in the conflict.
    fn retry_shared_edit(
        &mut self,
        conflict: &super::shared_workspaces::SharedWorkspaceConflict,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::shared_workspaces::SharedSubmission;

        let Some(shared) = self.shared.as_mut() else {
            return;
        };
        if let SharedSubmission::Refused(reason) = shared.ask_again(conflict) {
            Self::show_shared_refusal("apply the change", reason, window, cx);
        }
        cx.notify();
    }

    /// Presents a shared edit the server refused for a reason no retry repairs.
    fn present_shared_refusal(
        refusal: super::shared_workspaces::SharedRefusal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = Alert::new(
            ModalId::new("shared-workspace-refused"),
            "Change not applied",
            refusal.title(),
            refusal.detail(),
            vec![ModalAction::new(
                (),
                "OK",
                ModalActionRole::Cancel,
                "shared-workspace-refused-ok",
            )],
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, |_, _| {});
    }

    /// Explains a shared edit this client could not submit. Nothing was sent, and nothing waits.
    fn show_shared_refusal(
        operation: &str,
        reason: super::shared_workspaces::SharedRefusalReason,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let detail = format!(
            "SpaceTerm did not {operation} on the server. {}",
            reason.explanation()
        );
        Self::show_shared_unavailable(&detail, window, cx);
    }

    /// Returns whether the alert was presented over the active Settings Window.
    fn show_shared_unavailable(detail: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let detail = detail.to_owned();
        let alert = move || {
            Alert::new(
                ModalId::new("shared-workspace-unavailable"),
                "Shared change unavailable",
                "Shared Change Unavailable",
                detail.clone(),
                vec![ModalAction::new(
                    (),
                    "OK",
                    ModalActionRole::Cancel,
                    "shared-workspace-unavailable-ok",
                )],
            )
            .intent(AlertIntent::Warning)
        };
        let over_settings = alert.clone();
        let answered =
            super::settings_window::present_over_active_settings(cx, move |window, cx| {
                let _ = over_settings().present(window, cx, |_, _| {});
            });
        if !answered {
            let _ = alert().present(window, cx, |_, _| {});
        }
        answered
    }

    /// Whether a SpaceTerm Server owns the Workspace, which changes what this window may read
    /// and do with it.
    pub(super) fn is_shared_workspace(&self, workspace_id: WorkspaceId) -> bool {
        self.shared
            .as_ref()
            .is_some_and(|shared| shared.is_shared(workspace_id))
    }

    /// Opens what one accepted edit of this client's own created, once the server published it.
    ///
    /// Only the client that asked for the creation does this. A Workspace, Tab, or Pane another
    /// client created joins the presentation without moving anything here.
    fn open_accepted_shared(
        &mut self,
        accepted: &super::shared_workspaces::SharedAcceptance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use spaceterm_server::hierarchy::Mutation;

        match &accepted.mutation {
            Mutation::CreateWorkspace { .. } => {
                let Some(workspace_id) = self
                    .shared
                    .as_ref()
                    .and_then(|shared| shared.local(accepted.target))
                else {
                    return;
                };
                if self.activate_workspace(workspace_id, window, cx) {
                    self.focus(window, cx);
                    self.scroll_active_workspace_into_view(cx);
                }
            }
            Mutation::CreateTab { workspace }
            | Mutation::SplitPane { workspace, .. }
            | Mutation::Worktree(spaceterm_server::worktree::Operation::OpenTab {
                workspace,
                ..
            }) => {
                let Some(manager) = self
                    .shared
                    .as_ref()
                    .and_then(|shared| shared.local(*workspace))
                    .and_then(|workspace_id| self.workspaces.workspace(workspace_id))
                    .map(|entry| entry.payload().clone())
                else {
                    return;
                };
                let target = accepted.target;
                manager.update(cx, |manager, cx| {
                    manager.open_accepted_shared(target, window, cx);
                });
            }
            // A rename or a pin changes no membership, so there is nothing to open.
            Mutation::RenameWorkspace { .. } | Mutation::PinWorkspace { .. } => {}
            // The published removal already moved the keyboard to what remains.
            Mutation::Close { .. } | Mutation::Worktree(_) => {}
        }
    }

    /// Removes a Workspace the server no longer has.
    ///
    /// The server already removed it, so this needs no confirmation here. It ends nothing on the
    /// server: closing the local Panes drops their subscriptions. A final Workspace leaves the
    /// empty view.
    fn remove_shared_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        removal: super::shared_workspaces::SharedRemoval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(shared) = self.shared.as_mut() {
            shared.forget(workspace_id);
        }
        self.pending_final_tab_closes.remove(&workspace_id);
        let own = removal == super::shared_workspaces::SharedRemoval::OwnClose;
        let held_keyboard = self
            .workspaces
            .workspace(workspace_id)
            .is_some_and(|entry| entry.payload().read(cx).holds_keyboard(window, cx));
        // Opening whatever takes its place selects that Workspace's Focused Pane. Another
        // client's publication is nobody's request for a Pane; this client's own Kill Workspace
        // is, as a local close is.
        self.close_workspace_activating(workspace_id, own, window, cx);
        // The keyboard that was in the removed Workspace rests in the Workspace that takes its
        // place, without asking the server for a Pane there. Typing anywhere else, such as in
        // the sidebar, stays where it is.
        if !own && held_keyboard {
            self.focus(window, cx);
        }
    }

    /// The pin version a shared Workspace shows now, which its chooser keeps while it is open.
    fn shared_pin_version(&self, workspace_id: WorkspaceId) -> Option<u64> {
        self.shared.as_ref()?.pin_version(workspace_id)
    }

    fn shared_row_facts(&self, workspace_id: WorkspaceId) -> Option<SharedRowFacts> {
        let shared = self.shared.as_ref()?;
        let facts = shared.facts(workspace_id)?;
        let editability = shared.editability();
        // A Pane's confirmed directory changes without a membership revision, so the row reads
        // the latest inventory.
        let directory = shared
            .server_workspace(workspace_id)
            .and_then(super::shared_workspaces::workspace_directory)
            .unwrap_or_else(|| facts.displayed_directory());
        Some(SharedRowFacts {
            row: super::workspace_sidebar::SharedWorkspaceRow {
                name_version: facts.name_version,
                edits: editability.available(),
                edits_reason: editability.explanation(),
                status: shared
                    .worktree_status(
                        facts.server,
                        self.shared_inventory(workspace_id)
                            .map(|inventory| inventory.main_root.as_path()),
                    )
                    .or_else(|| editability.status()),
                detaches: shared.attachments_changeable(),
            },
            directory: directory.to_owned(),
            pinned: facts.pinned(),
        })
    }

    /// Whether creation offers a separate server row. Launched windows create local Workspaces
    /// on the account server through the ordinary rows instead.
    fn offers_server_creation_row(&self) -> bool {
        self.shared.is_some() && self.launch.is_none()
    }

    /// Whether the server holding this window's shared Workspaces is reachable.
    fn shared_reachable(&self) -> bool {
        self.shared.as_ref().is_none_or(|shared| {
            super::shared_workspaces::SharedRefusalReason::from(shared.editability())
                != super::shared_workspaces::SharedRefusalReason::Offline
        })
    }

    /// The detached server Workspaces the Workspace Switcher offers after the presented ones.
    ///
    /// Only a connected server's detached Workspaces are offered, and each one attaches when
    /// chosen. None takes an activation Shortcut, because those follow the sidebar's order.
    fn detached_switcher_items(&self) -> Vec<ComboBoxItem<SwitcherChoice>> {
        let Some(shared) = self.shared.as_ref() else {
            return Vec::new();
        };
        let changeable = shared.attachments_changeable();
        shared
            .available_workspaces()
            .into_iter()
            .filter(|available| available.local.is_none())
            .map(|available| {
                ComboBoxItem::new(SwitcherChoice::Detached(available.server), available.name)
                    .description(if changeable {
                        DETACHED_DESCRIPTION
                    } else {
                        DETACHED_UNAVAILABLE_DESCRIPTION
                    })
                    .keywords(["Detached"])
                    .disabled(!changeable)
                    .leading_icon(|foreground, size| {
                        div()
                            .child(Icon::new(IconName::Terminal, size, foreground))
                            .into_any_element()
                    })
                    .debug_selector(detached_switcher_selector(available.server))
            })
            .collect()
    }

    /// Attaches a detached server Workspace and activates it, as choosing a presented Workspace
    /// activates that one.
    ///
    /// Attaching presents what the server holds now. It reinstates no Pane control this client
    /// released when it detached; selecting a Pane asks for control afresh, as it does anywhere.
    fn attach_detached_workspace(
        &mut self,
        server: spaceterm_server::hierarchy::Id,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shared) = self.shared.as_mut() else {
            return;
        };
        if let Err(failure) = shared.attach(server) {
            Self::present_attachment_failure(AttachmentChange::Attach, failure, window, cx);
            return;
        }
        // The inventory revision did not change, so this window presents the attachment now
        // rather than waiting for the server's next publication.
        if let Some(inventory) = self
            .shared
            .as_ref()
            .and_then(|shared| shared.latest_inventory())
            .cloned()
        {
            self.apply_shared_inventory(
                &inventory,
                super::shared_workspaces::SharedRemoval::Published,
                window,
                cx,
            );
        }
        let Some(workspace_id) = self.shared.as_ref().and_then(|shared| shared.local(server))
        else {
            return;
        };
        if self.activate_workspace(workspace_id, window, cx) {
            self.focus(window, cx);
        }
    }

    /// Stops presenting a shared Workspace in this window.
    ///
    /// Detach Workspace removes only this client's Workspace Attachment. The Workspace and its
    /// Terminal Sessions keep running on the server for every client, and this client gives up
    /// control of its Panes. It needs no server, so it works offline, and it confirms nothing
    /// because nothing ends. Detaching the final Workspace leaves the empty view: nothing
    /// replaces it here, and nothing is created on the server.
    fn detach_shared_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::shared_workspaces::SharedAttachmentFailure;

        if !self.is_shared_workspace(workspace_id) {
            return;
        }
        // The person's arrangement is saved first, so attaching the Workspace again restores it.
        self.save_shared_arrangements(cx);
        let Some(shared) = self.shared.as_mut() else {
            return;
        };
        let outcome = shared.detach(workspace_id);
        if outcome == Err(SharedAttachmentFailure::Unchanged) {
            Self::present_attachment_failure(
                AttachmentChange::Detach,
                SharedAttachmentFailure::Unchanged,
                window,
                cx,
            );
            return;
        }
        // A write SpaceTerm cannot confirm still stops this presentation: the person asked for
        // that, and the Panes' control may already be released.
        self.remove_workspace_presentation(workspace_id, window, cx);
        if let Err(failure) = outcome {
            Self::present_attachment_failure(AttachmentChange::Detach, failure, window, cx);
        }
    }

    /// Removes one Workspace's presentation without replacing it. Closing a shared Workspace's
    /// local Panes drops their subscriptions and ends nothing on the server.
    fn remove_workspace_presentation(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(shared) = self.shared.as_mut() {
            shared.forget(workspace_id);
        }
        self.pending_final_tab_closes.remove(&workspace_id);
        // Ends a pin chooser open for the Workspace; a shared Workspace has no remote runtime.
        self.begin_remote_workspace_close(workspace_id, window, cx);
        let was_active = self.workspaces.active_workspace_id() == Some(workspace_id);
        let held_keyboard = self
            .workspaces
            .workspace(workspace_id)
            .is_some_and(|entry| entry.payload().read(cx).holds_keyboard(window, cx));
        let removed = match self.workspaces.remove_workspace(workspace_id) {
            Ok(removed) => removed,
            Err(error) => {
                Self::report_workspace_error("detach", error);
                return;
            }
        };
        removed.update(cx, |manager, cx| manager.close_all(cx));
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.cancel_rename_for(workspace_id, cx)
        });
        match self.workspaces.active_workspace_id() {
            // The Workspace that takes the detached one's place opens as a closed Workspace's
            // neighbor does. The keyboard stays in the sidebar when it is there.
            Some(active_workspace_id) if was_active => {
                self.synchronize_tab_manager_layout(active_workspace_id, window, cx);
                if let Some(active_manager) = self.active_tab_manager() {
                    if self.sidebar.read(cx).is_focused(window)
                        || self.sidebar.read(cx).rename_is_focused(window)
                    {
                        active_manager.update(cx, |manager, cx| manager.activate_without_focus(cx));
                    } else {
                        active_manager.update(cx, |manager, cx| manager.activate(window, cx));
                    }
                }
                self.scroll_active_workspace_into_view(cx);
            }
            Some(_) => {
                if held_keyboard {
                    self.focus(window, cx);
                }
            }
            // The empty view takes the keyboard, so its explanation and action are next.
            None => self.empty_focus.focus(window, cx),
        }
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    /// Says that an attachment change failed or could not be confirmed. Every Workspace keeps
    /// running on its server whatever this client saved.
    fn present_attachment_failure(
        change: AttachmentChange,
        failure: super::shared_workspaces::SharedAttachmentFailure,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (label, title, detail) = change.failure(failure);
        let _ = Alert::new(
            ModalId::new("shared-attachment-failed"),
            label,
            title,
            detail.to_owned(),
            vec![ModalAction::new(
                (),
                "OK",
                ModalActionRole::Cancel,
                "shared-attachment-failed-ok",
            )],
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, |_, _| {});
    }
}

/// What a detached Workspace's Workspace Switcher result says beneath its name.
const DETACHED_DESCRIPTION: &str = "Detached";
/// The same result while this client cannot change its attachments.
const DETACHED_UNAVAILABLE_DESCRIPTION: &str =
    "Detached. Attaching is unavailable until SpaceTerm reopens.";

/// A detached Workspace's Workspace Switcher result, named by its server identity's leading bytes.
fn detached_switcher_selector(server: spaceterm_server::hierarchy::Id) -> String {
    let prefix: String = server[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("workspace-switcher-detached-{prefix}")
}

/// Which attachment change a person asked for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AttachmentChange {
    Attach,
    Detach,
}

impl AttachmentChange {
    /// The accessibility label, title, and detail of a failed or unconfirmed change.
    const fn failure(
        self,
        failure: super::shared_workspaces::SharedAttachmentFailure,
    ) -> (&'static str, &'static str, &'static str) {
        use super::shared_workspaces::SharedAttachmentFailure::{Uncertain, Unchanged};

        match (self, failure) {
            (Self::Attach, Unchanged) => (
                "Workspace not attached",
                "Workspace Not Attached",
                "SpaceTerm could not save the attachment on this device, so nothing changed. \
                 The Workspace keeps running on the server.",
            ),
            (Self::Attach, Uncertain) => (
                "Attachment not confirmed",
                "Attachment Not Confirmed",
                "SpaceTerm cannot confirm whether this device saved the attachment. The \
                 Workspace keeps running on the server. SpaceTerm makes no more attachment \
                 changes until you quit and reopen it.",
            ),
            (Self::Detach, Unchanged) => (
                "Workspace not detached",
                "Workspace Not Detached",
                "SpaceTerm could not save the change on this device, so the Workspace stays \
                 here. It keeps running on the server.",
            ),
            (Self::Detach, Uncertain) => (
                "Detach not confirmed",
                "Detach Not Confirmed",
                "SpaceTerm removed the Workspace from this window but cannot confirm that this \
                 device saved the change, so it may return when SpaceTerm reopens. It keeps \
                 running on the server. SpaceTerm makes no more attachment changes until you \
                 quit and reopen it.",
            ),
        }
    }
}

const SHARED_CLOSE_CONFIRM_SELECTOR: &str = "shared-close-confirm";
const SHARED_CLOSE_CANCEL_SELECTOR: &str = "shared-close-cancel";
const SHARED_CLOSE_REVIEW_SELECTOR: &str = "shared-close-review";
const SHARED_CLOSE_DISMISS_SELECTOR: &str = "shared-close-dismiss";
const SHARED_CONFLICT_RETRY_SELECTOR: &str = "shared-workspace-conflict-retry";
const SHARED_CONFLICT_KEEP_SELECTOR: &str = "shared-workspace-conflict-keep";

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
        self.sync_terminal_focus_blocker(window, cx);
        let manager = cx.entity().downgrade();
        let active_tab_manager = self.active_tab_manager();
        let sidebar_layout = self.sidebar.read(cx).layout();
        let sidebar_frame = self.sidebar.update(cx, |sidebar, cx| sidebar.present(cx));
        if sidebar_frame.moving {
            window.request_animation_frame();
        }
        let identity = self.active_chrome_identity(cx);
        let resting_chrome = WorkspaceChromeLayout::resolve(sidebar_layout, &identity, window, cx);
        let chrome =
            WorkspaceChromeLayout::presented(sidebar_layout, sidebar_frame, &identity, window, cx);
        let update_control = gpui::AnyView::from(self.update_control.clone());
        let close_owner = cx.weak_entity();
        let close: spaceterm_ui::WindowCloseHandler = Rc::new(move |window, cx| {
            let _ = close_owner.update(cx, |manager, cx| manager.request_window_close(window, cx));
        });
        let stage = match active_tab_manager {
            Some(active_tab_manager) => {
                active_tab_manager.update(cx, |manager, cx| {
                    manager.set_sidebar_layout(
                        sidebar_layout.visible,
                        sidebar_layout.width,
                        chrome.width,
                        cx,
                    );
                    manager.present_sidebar(sidebar_frame.reveal, cx);
                    manager.set_trailing_accessory(Some(update_control), cx);
                    manager.set_window_close_handler(close);
                });
                active_tab_manager.into_any_element()
            }
            None => self.render_empty_workspace(
                empty::EmptySidebar {
                    width: sidebar_layout.width,
                    reveal: sidebar_frame.reveal,
                    top_chrome_width: chrome.width,
                },
                update_control,
                close,
                window,
                cx,
            ),
        };
        let rows = self.sidebar_rows(cx);
        let remote_unavailable = self.remote_workspace_unavailable_reason.clone();
        let creates_server_workspaces = self.offers_server_creation_row();
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_rows(rows, remote_unavailable, creates_server_workspaces, cx)
        });
        // The New Worktree Command is available only where it can act, so its menu item disables
        // elsewhere.
        let new_worktree = self
            .workspaces
            .active_workspace_id()
            .is_some_and(|workspace_id| self.creates_worktrees(workspace_id, cx));
        let step_worktrees = self.steps_worktrees(cx);
        let content = Self::workspace_action_scope(new_worktree, step_worktrees, cx)
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
                    .child(stage),
            )
            .when(sidebar_frame.reveal > 0.0, |root| {
                root.child(self.sidebar.clone())
            })
            .child(
                self.sidebar.read(cx).render_resize_handle(
                    self.sidebar.downgrade(),
                    resting_chrome.width,
                    chrome.width,
                    !sidebar_frame.moving,
                    super::workspace_frame::WorkspaceFrame::for_appearance(
                        super::appearance::chrome(cx),
                        cx,
                    )
                    .top_chrome_height(super::appearance::chrome(cx).top_height()),
                ),
            )
            .child(self.render_top_left_chrome(chrome, manager.clone(), window, cx));
        let transients = Self::workspace_action_scope(new_worktree, step_worktrees, cx)
            .absolute()
            .inset_0()
            .children(self.remote_workspace_flow.iter().cloned())
            .children(self.pin_picker.iter().cloned())
            .children(self.active_directory_recovery(cx));
        ModalLayer::new(super::window_shell::render(content, window, cx)).transient(transients)
    }
}

impl WorkspaceManager {
    /// Keeps ordinary content and complete transient owners on the same action routes while leaving
    /// the active modal outside those routes.
    fn workspace_action_scope(
        new_worktree: bool,
        step_worktrees: bool,
        cx: &Context<Self>,
    ) -> gpui::Div {
        div()
            .key_context(TERMINAL_KEY_CONTEXT)
            .when(new_worktree, |scope| {
                scope.on_action(cx.listener(Self::on_new_worktree))
            })
            .when(step_worktrees, |scope| {
                scope
                    .on_action(cx.listener(Self::on_next_worktree))
                    .on_action(cx.listener(Self::on_previous_worktree))
            })
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

/// The collapsed title-bar identity a Workspace presents, before its Active Worktree.
fn chrome_identity<T>(workspace: &WorkspaceEntry<T>) -> WorkspaceChromeIdentity {
    WorkspaceChromeIdentity {
        name: workspace.name().to_owned(),
        worktree: None,
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

mod empty;
mod launch;
pub(crate) use launch::LocalServerAdapters;
#[cfg(test)]
pub(crate) use launch::testing as launch_testing;
#[path = "workspace_manager/repository.rs"]
mod repository;
mod shared_repositories;
mod worktrees;

#[cfg(test)]
#[path = "workspace_manager/tests.rs"]
pub(crate) mod tests;
