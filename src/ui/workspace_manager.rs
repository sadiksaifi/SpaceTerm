use super::chrome_icons::IconRole;
use super::pane_lifecycle::{PaneConstruction, PaneLifecycleDependencies};
use super::workspace_chrome::{
    TOGGLE_SIZE, WorkspaceChromeIdentity, WorkspaceChromeLayout, WorkspaceChromeStatus,
    WorkspaceChromeStatusHosts,
};
use super::workspace_sidebar::{
    OUTSIDE_PANE_LABEL, SERVER_WORKSPACE_BADGE, SidebarEvent, WorkspaceMenuCommand,
    WorkspaceRowViewModel, WorkspaceSidebar, WorktreeMenuCommand, remote_connection_status,
};
#[cfg(test)]
use super::workspace_sidebar::{SIDEBAR_MAXIMUM_WIDTH, TERMINAL_CONTENT_MINIMUM_WIDTH};
use crate::platform::terminal_accessibility::TerminalAccessibilityAdapterFactory;
use crate::terminal::native_services::NativeServiceAdapters;
use crate::ui::appearance::gpui_color;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;

use super::chrome_typography::{ChromeTextStyleExt, TextRole};
use super::directory_picker::{
    DirectoryPicker, DirectoryPickerEvent, DirectorySelection, DirectorySource,
    LocalDirectorySource, ServerDirectorySource,
};
use super::terminal_focus::{TerminalFocusBlocker, TerminalFocusCoordinator, WorkspaceFocusOwners};
use super::workspace_creation::WorkspaceCreation;
use super::{
    ActivateTab1, ActivateTab2, ActivateTab3, ActivateTab4, ActivateTab5, ActivateTab6,
    ActivateTab7, ActivateTab8, ActivateTab9, ActivateWorkspace1, ActivateWorkspace2,
    ActivateWorkspace3, ActivateWorkspace4, ActivateWorkspace5, ActivateWorkspace6,
    ActivateWorkspace7, ActivateWorkspace8, ActivateWorkspace9, ClosePane, CloseTab,
    CloseTerminalFind, CloseWorkspace, CopySelection, CreateTab, FindNext, FindPrevious,
    FocusNextPane, FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, FocusPreviousPane,
    MoveTabLeft, MoveTabRight, NewWorkspace, NewWorktree, NextTab, NextWorktree,
    OpenLocalDirectory, OpenTerminalFind, PreviousTab, PreviousWorktree, ScrollPageDown,
    ScrollPageUp, ScrollToBottom, ScrollToTop, ShowRepositoryStatus, SplitDown, SplitRight,
    SwitchWorkspace, TERMINAL_KEY_CONTEXT, TabManager, TabManagerEvent, TogglePaneZoom,
    ToggleSidebar, ToggleSidebarFocus,
};
use crate::appearance::Color;
use crate::close_confirmation::{
    ApplicationPaneFacts, CloseConfirmation, CloseHierarchy, CloseScope, CloseTarget,
};
use crate::directory_selection::SystemDirectorySelection;
use crate::domain::{
    PaneId, RemoteConnectionPhase, TabId, ValidatedLocalDirectory,
    WorkspaceCollection, WorkspaceEntry, WorkspaceError, WorkspaceId,
};
use crate::platform::local_filesystem::{LocalFilesystemAuthority, LocalFilesystemError};
use crate::platform::window_movement::{
    OperatingSystemWindowDragError, OperatingSystemWindowDragPlatform, WindowMoveStart,
};
use crate::settings::sidebar::SidebarState;
use crate::terminal::{
    NativeServiceOrigin, NativeServiceStatus, SelectionCopy, TerminalKeyInputAdapterFactory,
    WorkspaceTerminalSessionFactory,
};
use gpui::prelude::*;

use gpui::{
    Action, AnyElement, App, Context, Edges, Entity, FocusHandle, Pixels, Render, Task, WeakEntity,
    Window, div,
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, AnchoredAlignment, AnchoredPlacement,
    AnchoredPlacementConfig, ButtonVariant, ComboBox, ComboBoxAccessory, ComboBoxCommand,
    ComboBoxCopy, ComboBoxHandle, ComboBoxItem, CustomIconName, Icon, IconButton, IconName,
    ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId, ModalLayer,
    Tooltip, WindowDragRegion, WindowDragRegionEvent, WindowDragRegionResponse,
    WindowDragRegionStatus, window_combo_box_is_open, window_modal_is_open,
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
    /// The account-local server this window launches and presents. It owns local Workspaces.
    pub(crate) local_server: launch::LocalServerAdapters,
    /// Each presented server's directory channel, which answers questions about its own paths.
    pub(crate) server_directories: super::directory_picker::ServerDirectories,
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
    /// A new Workspace on the paired server at `key`, pinned to a directory there and named
    /// `name` unless blank.
    NewServerWorkspace { key: ServerKey, name: String },
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

/// The server and server identity of a detached Workspace the Workspace Switcher offers.
type DetachedWorkspace = (ServerKey, spaceterm_server::hierarchy::Id);

/// What one Workspace Switcher result chooses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SwitcherChoice {
    /// A Workspace this window presents, which choosing activates.
    Presented(WorkspaceId),
    /// A server Workspace this client has detached, which choosing attaches and activates.
    Detached(DetachedWorkspace),
}

/// A Workspace a person asked to open on a paired server's directory, waiting for that server's
/// edits or its answer about an existing pin.
#[derive(Clone)]
pub(super) struct PendingServerDirectory {
    key: ServerKey,
    name: String,
    directory: PathBuf,
    /// Where navigation stood when the person chose the directory. Navigation while the server
    /// answers wins over an existing pinned Workspace and over opening a new one.
    navigation: NavigationMark,
    /// What held the keyboard once the choice settled, after any chooser restored focus. `None`
    /// until then. A later sidebar edit, Terminal Find, chooser or other owner wins as navigation
    /// does.
    keyboard: Option<Option<FocusHandle>>,
    /// Whether the person moved the keyboard from that owner since, even if it came back.
    keyboard_moved: bool,
}

/// Where a person's explicit navigation stands in this window: Workspace activations and the
/// Active Workspace, then that Workspace's Tab activations, Active Tab, and Focused Pane changes.
/// Any change, even one returning to the same place, is newer navigation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NavigationMark {
    activations: u64,
    workspace: Option<WorkspaceId>,
    within: Option<(u64, crate::domain::TabId, u64)>,
}

/// One question to the owning server about the directory a person chose in Open Remote
/// Directory: which attached Workspace there, if any, is already pinned to it.
struct ServerDirectoryMatch {
    directory: PathBuf,
    /// The Workspaces asked about, with their pins, in sidebar order.
    pins: Vec<(WorkspaceId, PathBuf)>,
    server: crate::server_connection::ServerConnection,
    /// The position in `pins` the server matched, once it answers.
    answer: Option<Result<Option<usize>, super::directory_picker::DirectorySourceError>>,
    _task: Task<()>,
}

impl ServerDirectoryMatch {
    fn asks(
        &self,
        directory: &PathBuf,
        pins: &[(WorkspaceId, PathBuf)],
        server: &crate::server_connection::ServerConnection,
    ) -> bool {
        self.directory == *directory && self.pins == pins && self.server.same_handle(server)
    }
}

/// How long a Reveal waits for its server to publish what it shows.
const REVEAL_LIFETIME: std::time::Duration = std::time::Duration::from_secs(5);

/// A Reveal from a Focused Pane, waiting for its server's publication.
#[derive(Clone, Copy)]
struct PendingReveal {
    key: ServerKey,
    /// The origin Pane's Workspace.
    workspace_id: WorkspaceId,
    /// Where navigation in that Workspace stood when the Reveal arrived.
    navigation: (u64, TabId, u64),
    target: spaceterm_server::hierarchy::Placement,
    deadline: std::time::Instant,
}

pub(crate) struct WorkspaceManager {
    window_appearance: super::appearance_runtime::WindowAppearanceOwner,
    window_traffic_lights: super::appearance_runtime::WindowTrafficLightOwner,
    transient: WorkspaceTransientUi,
    workspace_switcher: ComboBoxHandle<SwitcherChoice, WorkspaceCreation>,
    /// The Workspace Open Remote Directory is opening, until its server takes it.
    pub(super) pending_server_directory: Option<PendingServerDirectory>,
    /// The owning server's answer to whether a Workspace there already uses the directory the
    /// person chose to open.
    server_directory_match: Option<ServerDirectoryMatch>,
    server_directories: super::directory_picker::ServerDirectories,
    sidebar: Entity<WorkspaceSidebar>,
    local_filesystem: LocalFilesystemAuthority,
    workspaces: WorkspaceCollection<Entity<TabManager>>,
    pane_construction: PaneConstruction,
    local_home_directory_path: PathBuf,
    directory_selection: Rc<dyn SystemDirectorySelection>,
    pin_picker: Option<Entity<DirectoryPicker>>,
    pin_operation: u64,
    /// Counts Workspace activations, so a deferred completion can tell that navigation happened
    /// since it began, even navigation that returned to the same Workspace.
    activation_epoch: u64,
    /// The server whose creation this window presents without opening it, while the reading
    /// right after that creation's submission runs, because newer navigation superseded the
    /// person's original choice.
    quiet_creation: Option<ServerKey>,
    /// Follows the keyboard owner a pending Open Remote Directory settled on.
    server_directory_keyboard: Option<gpui::Subscription>,
    operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
    window_drag_status: WindowDragRegionStatus,
    update_control: Entity<super::updates::UpdateControl>,
    pending_final_tab_closes: BTreeSet<WorkspaceId>,
    /// A Reveal that waits for its server to publish what it shows.
    pending_reveal: Option<PendingReveal>,
    close_empty_after_shared_removal: bool,
    close_confirmation: CloseConfirmation,
    /// Whether the Workspace a shared close names held the keyboard when the person asked for
    /// it. It is kept by confirmation generation, and a Review hands it to the fresh confirmation,
    /// because each confirmation takes the keyboard before an approved close moves it.
    shared_close_keyboard: SharedCloseKeyboard,
    /// Reads every server again once the window's modal closes, while a reading waits on it.
    modal_reread: Option<gpui::Subscription>,
    sidebar_repositories: repository::SidebarRepositories,
    sidebar_worktrees: worktrees::SidebarWorktrees,
    shared_repositories: shared_repositories::SharedWorkspaceRepositories,
    /// Each presented SpaceTerm Server's Workspaces, their edit state, and the answers still to
    /// present. Adopting a server adds its Workspaces beside the others and changes none of them.
    shared: ServerBindings,
    /// Follows the desktop's paired connections while the window lives.
    _direct_servers: Option<crate::ui::direct_servers::DirectChanges>,
    /// Each presented paired route's Pane opener and the connection it follows.
    direct_routes: direct_servers::DirectRoutes,
    /// Where the keyboard rests while the window presents no Workspace.
    empty_focus: FocusHandle,
    /// Eases the empty stage's top edge as the band above it fills or empties, on the sidebar's
    /// curve. Created at the first frame, so a window opens at rest.
    empty_band: std::cell::RefCell<Option<super::sidebar_motion::SidebarMotion>>,
    /// Normal launch of the account-local server this window presents.
    launch: launch::LocalLaunchState,
    /// This client's one Workspace order across every server, kept across launches.
    display_order: display_order::DisplayOrder,
    /// The view the last closed Workspace Window left, which this window presents as its
    /// Workspaces arrive.
    restoration: crate::ui::remembered_view::Restoration,
    /// This window's keystrokes, followed only while a remembered choice is held.
    person_keys: Option<gpui::Subscription>,
}

impl WorkspaceManager {
    pub(crate) fn new_with_adapters(
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
            local_server,
            server_directories,
        } = adapters;
        let pane_construction = PaneConstruction::new(
            key_input_adapter_factory,
            accessibility_adapter_factory,
            native_service_adapters,
            lifecycle_dependencies,
        );
        // The server owns this window's local Workspaces, which arrive once it is ready.
        let workspaces = WorkspaceCollection::empty();
        cx.on_app_quit(|manager, cx| {
            manager.save_shared_arrangements(cx);
            for shared in manager.shared.iter_mut() {
                shared.flush_client_views();
            }
            manager.flush_display_order();
            // The direct registry outlives this window's interest in it only until quit.
            manager._direct_servers = None;
            async {}
        })
        .detach();
        cx.on_release(|manager, cx| {
            manager.release_sidebar_repositories(cx);
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
        cx.observe_window_activation(window, |_, window, cx| {
            if window.is_window_active()
                && let Some(store) =
                    crate::ui::repository_status_store::InstalledRepositoryStatus::store(cx)
            {
                store.update(cx, |store, cx| store.window_activated(cx));
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
            pending_server_directory: None,
            server_directory_match: None,
            server_directories,
            sidebar,
            workspaces,
            local_filesystem,
            pane_construction,
            local_home_directory_path,
            directory_selection,
            pin_picker: None,
            pin_operation: 0,
            activation_epoch: 0,
            quiet_creation: None,
            server_directory_keyboard: None,
            operating_system_window_drag_platform,
            window_drag_status: WindowDragRegionStatus::new(),
            update_control: cx.new(|cx| super::updates::UpdateControl::new(window, cx)),
            pending_final_tab_closes: BTreeSet::new(),
            pending_reveal: None,
            close_empty_after_shared_removal: false,
            close_confirmation: CloseConfirmation::default(),
            shared_close_keyboard: SharedCloseKeyboard::default(),
            modal_reread: None,
            sidebar_repositories: repository::SidebarRepositories::default(),
            sidebar_worktrees: worktrees::SidebarWorktrees::default(),
            shared_repositories: shared_repositories::SharedWorkspaceRepositories::default(),
            shared: ServerBindings::default(),
            _direct_servers: None,
            direct_routes: direct_servers::DirectRoutes::default(),
            empty_focus: cx.focus_handle(),
            empty_band: std::cell::RefCell::default(),
            launch: launch::LocalLaunchState::new(local_server),
            display_order: display_order::DisplayOrder::default(),
            restoration: crate::ui::remembered_view::Restoration::none(),
            person_keys: None,
        };
        manager.follow_display_order(window, cx);
        manager.run_local_launch(None, window, cx);
        manager.follow_direct_servers(window, cx);
        manager
    }

    fn create_tab_manager(
        session_factory: WorkspaceTerminalSessionFactory,
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
            let mut manager = TabManager::with_pane_construction(
                session_factory,
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
                TabManagerEvent::RevealRequested {
                    tab_id,
                    origin,
                    target,
                } => {
                    workspace_manager.request_reveal(
                        workspace_id,
                        (*tab_id, *origin),
                        *target,
                        window,
                        cx,
                    );
                }
                TabManagerEvent::PresentationChanged => {
                    // A shared Workspace's directory is the server's, and the sidebar shows what
                    // the server publishes. A server path never becomes this Workspace's own
                    // directory, which local pinning and file actions would read as authority.
                    if !workspace_manager.is_shared_workspace(workspace_id)
                        && let Some(crate::domain::CurrentDirectory::Local(directory)) =
                            tab_manager.read(cx).automatic_directory(cx)
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
        manager
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
                // Another computer's Workspace carries the globe its sidebar row shows.
                let remote = self
                    .shared
                    .owner(workspace.id())
                    .and_then(|key| self.shared.origin(key))
                    .is_some_and(|origin| matches!(origin, ServerOrigin::Direct(_)));
                let icon = if remote {
                    IconName::Globe
                } else {
                    IconName::Terminal
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
            Some(&self.chrome_identity(workspace_id, cx)),
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
    fn resize_sidebar(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.resize(width, window, cx));
    }

    fn scroll_active_workspace_into_view(&self, cx: &mut App) {
        if let Some(workspace_id) = self.workspaces.active_workspace_id() {
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.reveal_workspace(workspace_id, cx));
        }
    }

    /// Opens a chosen local directory as a Workspace the account server owns.
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
        self.open_shared_directory_workspace(directory, name, window, cx)
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
        // Only a server on this machine can hold a local directory.
        let existing = self.shared.local().and_then(|shared| {
            let filesystem = shared.sessions().local_filesystem()?;
            self.workspaces.iter().find_map(|entry| {
                let pin = shared
                    .server_workspace(entry.id())?
                    .pinned_directory
                    .as_deref()?;
                filesystem
                    .validate_directory(pin)
                    .ok()
                    .filter(|pinned| pinned.identity() == directory.identity())
                    .map(|_| entry.id())
            })
        });
        let opened = match existing {
            Some(workspace_id) => self.activate_workspace(workspace_id, window, cx),
            None => {
                self.create_shared_workspace(name, Some(directory.path().to_owned()), window, cx)
            }
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
            // A paired server's Workspace never takes a directory on this computer.
            PinTarget::NewServerWorkspace { .. } => PinApplication::Failed,
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
            Some(directory),
            window,
            cx,
        )
    }

    fn apply_directory_pin(
        &mut self,
        workspace_id: WorkspaceId,
        pin: Option<ValidatedLocalDirectory>,
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
        self.synchronize_tab_manager_layouts(window, cx);

        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
        true
    }

    fn show_pin_error(window: &mut Window, cx: &mut Context<Self>) {
        let _ = Alert::new(
            ModalId::new("workspace-pin-directory-unavailable"),
            "Directory unavailable",
            "Couldn\u{2019}t Pin Directory",
            "Check that the directory exists.",
            vec![ModalAction::new(
                (),
                "OK",
                ModalActionRole::Cancel,
                "workspace-pin-error-ok",
            )],
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
        let placeholder = match &target {
            PinTarget::Workspace(_) => "Pin to Directory",
            PinTarget::NewLocalWorkspace { .. } => {
                crate::keybindings::Command::OpenLocalDirectory.label()
            }
            PinTarget::NewServerWorkspace { .. } => "Open Remote Directory",
        };
        // Another computer's server Workspace browses that server's own directories. Nothing on
        // this computer reads or validates them.
        let server = match &target {
            PinTarget::Workspace(workspace_id)
                if self.is_shared_workspace(*workspace_id)
                    && !self.is_local_server_workspace(*workspace_id) =>
            {
                let Some(source) = self.owning_directory_source(*workspace_id, cx) else {
                    Self::show_shared_refusal(
                        "change the pinned directory",
                        super::shared_workspaces::SharedRefusalReason::Offline,
                        window,
                        cx,
                    );
                    self.transient.end_pin();
                    return;
                };
                Some(source)
            }
            PinTarget::NewServerWorkspace { key, .. } => {
                let Some(source) = self.server_directory_source(*key, cx) else {
                    Self::show_shared_refusal(
                        "open a remote directory",
                        super::shared_workspaces::SharedRefusalReason::Offline,
                        window,
                        cx,
                    );
                    self.transient.end_pin();
                    return;
                };
                Some(source)
            }
            _ => None,
        };
        let remote = server.is_some();
        let source: Rc<dyn DirectorySource> = match server {
            Some(source) => Rc::new(source),
            None => Rc::new(LocalDirectorySource::new(
                self.local_filesystem.clone(),
                self.local_home_directory_path.clone(),
                cx.background_executor().clone(),
            )),
        };
        let system_selection = (!remote).then(|| {
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
                    DirectoryPickerEvent::Confirmed(selection) => {
                        let current = manager.pin_operation == operation;
                        // A local selection pins only a local target, and a server path pins
                        // only that server's Workspace.
                        let application = match (current, selection.clone(), target.clone()) {
                            (
                                true,
                                DirectorySelection::Pin(directory),
                                target,
                            ) if !remote => {
                                manager.apply_local_pin_target(target, directory, window, cx)
                            }
                            (
                                true,
                                DirectorySelection::Server(directory),
                                PinTarget::Workspace(workspace_id),
                            ) if remote => PinApplication::from_applied(
                                manager.pin_server_directory(workspace_id, directory, window, cx),
                            ),
                            (
                                true,
                                DirectorySelection::Server(directory),
                                PinTarget::NewServerWorkspace { key, name },
                            ) if remote => {
                                manager.pin_operation = manager.pin_operation.wrapping_add(1);
                                manager.transient.end_pin();
                                manager.open_server_directory(key, name, directory, window, cx);
                                PinApplication::Applied
                            }
                            _ => PinApplication::Failed,
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
                                    // A directory still waiting for its server keeps the owner
                                    // this restoration chose, not the closing chooser.
                                    if manager
                                        .pending_server_directory
                                        .as_ref()
                                        .is_some_and(|pending| pending.keyboard.is_none())
                                    {
                                        manager.settle_server_directory_keyboard(window, cx);
                                    }
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

    /// Opens Open Remote Directory on the saved paired server `route`: the Directory Picker
    /// browses that server, and the chosen directory opens a Workspace there named `name` unless
    /// blank.
    fn open_remote_directory(
        &mut self,
        route: crate::direct_connections::DirectRouteId,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.server_key(&super::ServerTarget::Direct(route)) else {
            Self::show_shared_refusal(
                "open a remote directory",
                super::shared_workspaces::SharedRefusalReason::Offline,
                window,
                cx,
            );
            return;
        };
        self.choose_directory(PinTarget::NewServerWorkspace { key, name }, window, cx);
    }

    /// Opens a Workspace on the paired server at `key` pinned to `directory` there, named `name`
    /// unless blank, or activates the Workspace already pinned to that directory. Only that
    /// server compares its directories, and nothing reads this computer's.
    pub(super) fn open_server_directory(
        &mut self,
        key: ServerKey,
        name: String,
        directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.server_directory_match = None;
        self.pending_server_directory = Some(PendingServerDirectory {
            key,
            name,
            directory,
            navigation: self.navigation(cx),
            keyboard: None,
            keyboard_moved: false,
        });
        self.server_directory_keyboard = None;
        // A chooser still open restores focus as it closes, which is not the person's intent, so
        // the keyboard owner settles after that.
        if self.pin_picker.is_none() {
            self.settle_server_directory_keyboard(window, cx);
        }
        self.continue_server_directory(window, cx);
    }

    /// Records what holds the keyboard for the pending Open Remote Directory, and follows the
    /// person moving it elsewhere before the server answers.
    fn settle_server_directory_keyboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keyboard = window.focused(cx);
        let Some(pending) = self.pending_server_directory.as_mut() else {
            return;
        };
        pending.keyboard = Some(keyboard.clone());
        pending.keyboard_moved = false;
        self.server_directory_keyboard = keyboard.map(|owner| {
            cx.on_blur(&owner, window, |manager, window, _| {
                // Leaving SpaceTerm for another application moves no keyboard owner here.
                if window.is_window_active()
                    && let Some(pending) = manager.pending_server_directory.as_mut()
                {
                    pending.keyboard_moved = true;
                }
            })
        });
    }

    /// Whether the pending Open Remote Directory still reflects the person's latest intent: no
    /// newer navigation, and the keyboard where it settled, never moved since.
    fn server_directory_intent_kept(&self, window: &Window, cx: &App) -> bool {
        let Some(pending) = &self.pending_server_directory else {
            return false;
        };
        self.navigation(cx) == pending.navigation
            && pending
                .keyboard
                .as_ref()
                .is_none_or(|keyboard| !pending.keyboard_moved && window.focused(cx) == *keyboard)
    }

    /// Asks the pending Workspace's server for it once that server's edits are ready, or
    /// activates the Workspace already pinned to the chosen directory there. Until then the
    /// request waits; a server still finishing its first reading never drops it.
    fn continue_server_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use super::shared_workspaces::{SharedRefusalReason, SharedSubmission};

        let Some(PendingServerDirectory {
            key,
            name,
            directory,
            ..
        }) = self.pending_server_directory.clone()
        else {
            return;
        };
        let Some(shared) = self.shared.get(key) else {
            // The server stopped being presented, so nothing reaches it.
            self.finish_server_directory();
            Self::show_shared_refusal(
                "open a remote directory",
                SharedRefusalReason::Offline,
                window,
                cx,
            );
            self.sync_terminal_focus_blocker(window, cx);
            cx.notify();
            return;
        };
        if !shared.accepts_edits() {
            return;
        }
        let server_name = self
            .shared
            .origin(key)
            .and_then(ServerOrigin::remote_name)
            .unwrap_or_default();
        // A server path compares only with that server's own pins, and only that server can
        // tell whether two of its paths name one directory, such as through a symbolic link.
        let pins: Vec<(WorkspaceId, PathBuf)> = self
            .workspaces
            .iter()
            .filter_map(|entry| {
                let pin = shared
                    .server_workspace(entry.id())?
                    .pinned_directory
                    .clone()?;
                Some((entry.id(), pin))
            })
            .collect();
        let existing = if pins.is_empty() {
            None
        } else {
            match self.match_server_directory(key, &directory, &pins, window, cx) {
                None => return,
                Some(Ok(matched)) => matched.or_else(|| {
                    pins.iter()
                        .find(|(_, pin)| *pin == directory)
                        .map(|(workspace_id, _)| *workspace_id)
                }),
                Some(Err(())) => {
                    self.finish_server_directory();
                    Self::show_shared_unavailable_with_title(
                        "Couldn\u{2019}t Open Workspace",
                        &format!(
                            "Couldn\u{2019}t check for an existing Workspace on \u{201C}{server_name}\u{201D}. Try again."
                        ),
                        window,
                        cx,
                    );
                    self.sync_terminal_focus_blocker(window, cx);
                    cx.notify();
                    return;
                }
            }
        };
        let intent_kept = self.server_directory_intent_kept(window, cx);
        let refusal = match existing {
            // The Workspace already exists, so nothing is created. Later navigation or keyboard
            // intent wins: a person who moved elsewhere while the server answered stays there.
            Some(workspace_id) => {
                if intent_kept && self.activate_workspace(workspace_id, window, cx) {
                    self.focus(window, cx);
                    self.scroll_active_workspace_into_view(cx);
                }
                None
            }
            None => match self.shared.get_mut(key) {
                Some(shared) => match shared.create_workspace(name, Some(directory)) {
                    SharedSubmission::Submitted => None,
                    // Readiness was just checked, so only a publication arriving between the
                    // check and the submission refuses it this way; the next reading retries.
                    SharedSubmission::Refused(
                        SharedRefusalReason::Offline | SharedRefusalReason::Pending,
                    ) => return,
                    SharedSubmission::Refused(reason) => Some(reason),
                },
                None => return,
            },
        };
        self.finish_server_directory();
        if let Some(reason) = refusal {
            Self::show_shared_refusal("create a Workspace", reason, window, cx);
        } else if existing.is_none() {
            // The server still creates the Workspace the person asked for. After newer
            // navigation it joins the sidebar without taking the selection or the keyboard.
            if !intent_kept {
                self.quiet_creation = Some(key);
            }
            self.sync_shared_workspaces(key, window, cx);
            self.quiet_creation = None;
        }
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    /// Where the person's explicit navigation stands now.
    fn navigation(&self, cx: &App) -> NavigationMark {
        NavigationMark {
            activations: self.activation_epoch,
            workspace: self.workspaces.active_workspace_id(),
            within: self
                .active_tab_manager()
                .map(|manager| manager.read(cx).navigation(cx)),
        }
    }

    fn finish_server_directory(&mut self) {
        self.pending_server_directory = None;
        self.server_directory_match = None;
        self.server_directory_keyboard = None;
    }

    /// The owning server's answer to which of `pins` names `directory`, or `None` while it is
    /// asked. A new question replaces an answer to a different one; the question is always the
    /// current choice and the current pins, so a changed list is asked again.
    fn match_server_directory(
        &mut self,
        key: ServerKey,
        directory: &PathBuf,
        pins: &[(WorkspaceId, PathBuf)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Result<Option<WorkspaceId>, ()>> {
        let Some(client) = self.server_client(key, cx) else {
            return Some(Err(()));
        };
        let server = client.server.clone();
        if let Some(pending) = self
            .server_directory_match
            .as_ref()
            .filter(|pending| pending.asks(directory, pins, &server))
        {
            return pending.answer.as_ref().map(|answer| {
                answer
                    .as_ref()
                    .map(|index| index.and_then(|index| pins.get(index)).map(|(id, _)| *id))
                    .map_err(|_| ())
            });
        }
        // The question goes to this exact presented connection and stops once it's replaced.
        let available: Rc<dyn Fn() -> bool> = Rc::new(move || client.availability().0);
        let source = ServerDirectorySource::browsing_through(
            (self.server_directories)(&server),
            None,
            available,
        );
        let question = source.match_directory(
            directory.clone(),
            pins.iter().map(|(_, pin)| pin.clone()).collect(),
        );
        let asked = (directory.clone(), pins.to_vec(), server.clone());
        let task = cx.spawn_in(window, async move |manager, cx| {
            let answer = question.await;
            drop(source);
            let _ = manager.update_in(cx, |manager, window, cx| {
                if let Some(pending) = manager.server_directory_match.as_mut()
                    && pending.asks(&asked.0, &asked.1, &asked.2)
                {
                    pending.answer = Some(answer);
                    manager.continue_server_directory(window, cx);
                }
            });
        });
        self.server_directory_match = Some(ServerDirectoryMatch {
            directory: directory.clone(),
            pins: pins.to_vec(),
            server,
            answer: None,
            _task: task,
        });
        None
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
        if self.transient.local_selection_pending {
            return;
        }
        // A shared Workspace's draft is fenced to the pin version the chooser opened on.
        let version = match &target {
            PinTarget::Workspace(workspace_id) => self
                .shared_pin_version(*workspace_id)
                .map(|version| (*workspace_id, version)),
            PinTarget::NewLocalWorkspace { .. } | PinTarget::NewServerWorkspace { .. } => None,
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

    fn local_home_directory(&self) -> Result<ValidatedLocalDirectory, LocalFilesystemError> {
        self.local_filesystem
            .validate_directory(&self.local_home_directory_path)
    }

    fn home_directory_alert() -> Alert<()> {
        Alert::new(
            ModalId::new("workspace-home-directory-unavailable"),
            "Home directory unavailable",
            "Home Directory Unavailable",
            "Check that your home directory exists.",
            vec![ModalAction::new(
                (),
                "OK",
                ModalActionRole::Cancel,
                "workspace-home-error-ok",
            )],
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
        let open_pane = !self.sidebar_holds_keyboard(window, cx);
        self.activate_workspace_focusing(workspace_id, open_pane, window, cx)
    }

    /// Whether the sidebar, its rename field or its menu holds the keyboard.
    fn sidebar_holds_keyboard(&self, window: &Window, cx: &App) -> bool {
        let sidebar = self.sidebar.read(cx);
        sidebar.is_focused(window) || sidebar.rename_is_focused(window) || sidebar.menu_open()
    }

    fn activate_workspace_focusing(
        &mut self,
        workspace_id: WorkspaceId,
        open_pane: bool,
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
        self.activation_epoch = self.activation_epoch.wrapping_add(1);

        self.synchronize_tab_manager_layout(workspace_id, window, cx);
        if previous_workspace_id != Some(workspace_id) {
            if let Some(previous_manager) = &previous_manager {
                previous_manager.update(cx, |manager, cx| manager.deactivate(cx));
            }
            self.sidebar.update(cx, |sidebar, cx| {
                sidebar.cancel_rename(cx);
            });
        }
        if !open_pane {
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
            CloseScope::Workspace => format!("Running commands in its {count} {noun} stop."),
            CloseScope::Pane | CloseScope::Window | CloseScope::Tab => {
                format!("Running commands in {count} {noun} stop.")
            }
            _ => format!("Close {count} {noun}? Running commands in these Panes stop."),
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
        // Keeping the window also keeps the client windowed after Close All Windows.
        if !confirmed && target == CloseTarget::Window {
            crate::app::dismissal_cancelled(cx);
        }
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
            CloseTarget::Window => self.close_window(window, cx),
        }
    }

    pub(crate) fn request_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            self.remember_view(window, cx);
            return true;
        }
        self.present_close_confirmation(CloseTarget::Window, window, cx);
        false
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

    /// Closes a Workspace. `activate_next` moves the keyboard into the Workspace that takes its
    /// place; nobody chose that Workspace, so its Focused Pane is not selected either way. A close
    /// the server published passes `false`. Nothing replaces a final Workspace: the window
    /// presents its empty view instead.
    fn close_workspace_activating(
        &mut self,
        workspace_id: WorkspaceId,
        activate_next: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.end_pin_for_closing(workspace_id, window, cx);
        let was_active = self.workspaces.active_workspace_id() == Some(workspace_id);
        let held_keyboard = self
            .workspaces
            .workspace(workspace_id)
            .is_some_and(|entry| entry.payload().read(cx).holds_keyboard(window, cx));
        let closed_manager = match self.workspaces.remove_workspace(workspace_id) {
            Ok(closed_manager) => closed_manager,
            Err(error) => {
                Self::report_workspace_error("close", error);
                return;
            }
        };
        closed_manager.update(cx, |manager, cx| manager.close_all(cx));
        self.pending_final_tab_closes.remove(&workspace_id);
        if let Some(shared) = self.shared.owning_mut(workspace_id) {
            shared.forget(workspace_id);
        }
        match self.workspaces.active_workspace_id() {
            Some(active_workspace_id) => {
                self.synchronize_tab_manager_layout(active_workspace_id, window, cx);
                if was_active && let Some(active_manager) = self.active_tab_manager() {
                    if !activate_next
                        || self.sidebar.read(cx).is_focused(window)
                        || self.sidebar.read(cx).rename_is_focused(window)
                    {
                        active_manager.update(cx, |manager, cx| manager.activate_without_focus(cx));
                    } else {
                        active_manager
                            .update(cx, |manager, cx| manager.activate_in_place(window, cx));
                    }
                }
            }
            // The empty view takes the keyboard that was in the final Workspace.
            None if held_keyboard || was_active => self.empty_focus.focus(window, cx),
            None => {}
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

    /// Closes a Workspace whose final Tab closed, as a confirmed close of the Workspace would.
    fn close_workspace_for_final_tab(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspaces.workspace(workspace_id).is_none() {
            self.pending_final_tab_closes.remove(&workspace_id);
            return;
        }
        self.close_workspace_activating(workspace_id, true, window, cx);
    }

    /// Ends a directory choice for a Workspace that is closing, so it can't pin a Workspace that
    /// no longer exists.
    fn end_pin_for_closing(
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
                if self.activate_workspace_focusing(workspace_id, focus_pane, window, cx) {
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
                self.record_display_order();
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
            SidebarEvent::FocusPane => {
                if let Some(manager) = self.active_tab_manager() {
                    manager.update(cx, |manager, cx| manager.activate(window, cx));
                }
            }
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
            // A Workspace of another computer's server reconnects by connecting to that server.
            // The person asked, so the system may ask for this device's key.
            WorkspaceMenuCommand::Reconnect => {
                let route = self
                    .shared
                    .owner(workspace_id)
                    .and_then(|key| self.shared.origin(key))
                    .and_then(|origin| match origin {
                        ServerOrigin::Local => None,
                        ServerOrigin::Direct(direct) => Some(direct.route),
                    });
                if let Some(route) = route
                    && let Some(direct) = crate::ui::direct_servers::installed(cx)
                {
                    direct.resume(&route);
                }
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
            WorkspaceCreation::Local => {
                self.create_shared_workspace(name, None, window, cx);
            }
            WorkspaceCreation::OpenLocalDirectory => self.open_local_directory(name, window, cx),
            WorkspaceCreation::Remote(route) => {
                self.create_workspace_on(&super::ServerTarget::Direct(route), name, window, cx)
            }
            WorkspaceCreation::OpenRemoteDirectory(route) => {
                self.open_remote_directory(route, name, window, cx)
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
        // A missing descendant handler lets the action reach this ancestor. Forward only from
        // outside the active Tab; an unavailable operation then terminates on its second pass.
        if self
            .active_tab_manager()
            .is_none_or(|manager| manager.read(cx).holds_keyboard(window, cx))
        {
            return;
        }
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
            status: WorkspaceChromeStatus::resolve(self.server_connection_phase(workspace_id, cx)),
            ..chrome_identity(workspace)
        }
    }

    /// How the connection to another computer's server that owns `workspace_id` stands, or
    /// `None` for a Workspace of this computer's server. Reading it reaches no computer.
    fn server_connection_phase(
        &self,
        workspace_id: WorkspaceId,
        cx: &App,
    ) -> Option<RemoteConnectionPhase> {
        use crate::direct_connections::DirectRouteStatus;

        let key = self.shared.owner(workspace_id)?;
        match self.shared.origin(key)? {
            ServerOrigin::Local => None,
            ServerOrigin::Direct(direct) => {
                let status = crate::ui::direct_servers::installed(cx)?.status(&direct.route)?;
                Some(match status {
                    DirectRouteStatus::Connected => RemoteConnectionPhase::Connected,
                    DirectRouteStatus::Connecting | DirectRouteStatus::Reconnecting => {
                        RemoteConnectionPhase::Reconnecting
                    }
                    DirectRouteStatus::Refused(_) | DirectRouteStatus::Unavailable(_) => {
                        RemoteConnectionPhase::Failed
                    }
                    DirectRouteStatus::Disconnected | DirectRouteStatus::NeedsInteraction => {
                        RemoteConnectionPhase::Disconnected
                    }
                })
            }
        }
    }

    /// The Active Workspace's chrome identity, or `None` while the window presents no Workspace.
    fn active_chrome_identity(&self, cx: &App) -> Option<WorkspaceChromeIdentity> {
        self.workspaces
            .active_workspace_id()
            .map(|workspace_id| self.chrome_identity(workspace_id, cx))
    }

    /// The Active Workspace's identity, shown in the top-left chrome only while the sidebar is
    /// hidden. The empty view has none, so the chooser shows only its glyph.
    fn workspace_chrome_identity(&self, cx: &App) -> Option<(WorkspaceChromeIdentity, Tooltip)> {
        let workspace = self.workspaces.active_workspace()?;
        let remote_status = self
            .server_connection_phase(workspace.id(), cx)
            .and_then(remote_connection_status);
        let shared = self.shared_row_facts(workspace.id());
        let path = match &shared {
            // The directory belongs to the server, so the chrome shows the server's own.
            Some(shared) => compact_home_path(&shared.directory, &self.local_home_directory_path),
            None => {
                directory_labels(
                    workspace.local_display_directory(),
                    &self.local_home_directory_path,
                )
                .0
            }
        };
        let tooltip_detail = remote_status
            .map(|status| format!("{path}: {status}"))
            .unwrap_or_else(|| path.clone());

        // A shared Workspace's Repository Status is what the owning server read.
        let repository = match &shared {
            Some(_) => self.shared_chip_line(workspace.id(), cx),
            None => self.chip_repository_line(workspace.id(), cx),
        }
        .map(|line| format!("\n{line}"))
        .unwrap_or_default();
        Some((
            self.chrome_identity(workspace.id(), cx),
            Tooltip::new("workspace-switcher-tooltip", "Choose Workspace")
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
        let creation_rows = switcher_creation_rows(&self.remote_creation_servers(cx), presentation);
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
            || "Choose Workspace".to_owned(),
            |(identity, _, _)| identity.accessibility_name(),
        );
        let chooser = ComboBox::with_commands(
            "workspace-switcher",
            switcher_accessibility_name,
            self.workspaces
                .active_workspace_id()
                .map(SwitcherChoice::Presented),
            "Choose Workspace",
            self.workspace_switcher_items(cx),
            move |_| {
                creation_rows
                    .iter()
                    .map(|row| {
                        let creation = row.creation;
                        let mut command = ComboBoxCommand::new(creation, row.label.clone())
                            .leading_icon(move |foreground, size| {
                                Icon::custom(creation.icon(), size, foreground).into_any_element()
                            })
                            .debug_selector(format!("workspace-switcher-{}", row.selector));
                        if row.starts_group {
                            command = command.starts_group();
                        }
                        if let Some(shortcut) = row.shortcut.clone() {
                            command = command.shortcut(shortcut);
                        }
                        if !row.available {
                            command = command
                                .disabled(true)
                                .description(super::workspace_creation::REMOTE_UNAVAILABLE)
                                .trailing(ComboBoxAccessory::Status("Unavailable".into()));
                        }
                        command
                    })
                    .collect()
            },
        )
        .handle(self.workspace_switcher.clone())
        // Beside a shown sidebar, and always without a Workspace, the switcher is a Ghost icon
        // button like the sidebar toggle.
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
        // The creation rows always remain, so the switcher never shows an empty state. Without a
        // Workspace to choose it says so above them.
        .copy(
            ComboBoxCopy::new("Workspace name", "Filter or create\u{2026}")
                .no_match("No Workspaces match")
                .commands_heading("Create")
                .no_choices("No Workspaces to attach."),
        )
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
            Tooltip::new("workspace-switcher-tooltip", "Choose Workspace")
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
                    SwitcherChoice::Detached((key, server)) => {
                        manager.attach_detached_workspace(key, server, window, cx);
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
        let drag_region =
            WindowDragRegion::new("workspace-top-chrome-drag-region", "Move Window", content)
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
                    workspace.local_display_directory(),
                    &self.local_home_directory_path,
                );
                let mut path: gpui::SharedString = path.into();
                let mut tooltip = directory_tooltip;
                // A shared Workspace's directory, pin, Worktrees, and Repository Status all live
                // on the owning server. The row shows what the server read and published; this
                // client reads none of it.
                let shared = self.shared_row_facts(workspace.id());
                let mut repository = match &shared {
                    // Another computer's directory is shown as that computer reports it: this
                    // computer's home directory says nothing about it.
                    Some(shared) if shared.machine.is_some() => {
                        path = shared.directory.display().to_string().into();
                        tooltip = format!(
                            "{SERVER_WORKSPACE_BADGE} on {}: {}",
                            shared.machine.as_deref().unwrap_or_default(),
                            shared.directory.display()
                        );
                        self.shared_sidebar_badge(workspace.id(), cx)
                    }
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
                if let Some(section) = worktrees.as_ref() {
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
                            let mut tooltip = format!(
                                "Worktree {}: {}",
                                worktree.name, worktree.directory_tooltip
                            );
                            if let Some(pane) = &worktree.outside_pane_directory {
                                tooltip.push_str(&format!("\n{OUTSIDE_PANE_LABEL}: {pane}"));
                            }
                            tooltip
                        }
                        None => section
                            .repository
                            .as_ref()
                            .map_or_else(String::new, |root| format!("Repository: {root}")),
                    };
                    if let Some(pinned) = pinned {
                        tooltip.push_str(&format!("\nPinned Directory: {}", pinned.display()));
                    }
                    match shared.as_ref().map(|shared| shared.machine.as_ref()) {
                        Some(Some(machine)) => {
                            tooltip = format!("{SERVER_WORKSPACE_BADGE} on {machine}\n{tooltip}");
                        }
                        Some(None) => {
                            tooltip = format!("{SERVER_WORKSPACE_BADGE}\n{tooltip}");
                        }
                        None => {}
                    }
                }
                WorkspaceRowViewModel {
                    workspace_id: workspace.id(),
                    name: workspace.name().to_owned().into(),
                    path,
                    // Another computer's server Workspace names that computer.
                    machine: shared
                        .as_ref()
                        .and_then(|shared| shared.machine.clone())
                        .map(Into::into),
                    tooltip: tooltip.into(),
                    pinned: shared.as_ref().map_or_else(
                        || workspace.pinned_directory().is_some(),
                        |shared| shared.pinned,
                    ),
                    remote_connection_phase: self.server_connection_phase(workspace.id(), cx),
                    repository,
                    worktrees,
                    creates_worktrees: self.creates_worktrees(workspace.id(), cx),
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
    /// The name of another computer's server, or `None` for the account-local one.
    machine: Option<String>,
}

/// Shared server Workspaces: adopting them, presenting what the server publishes, submitting the
/// edits a person asks for, and refusing the operations the server path does not support yet.
impl WorkspaceManager {
    /// Presents the Workspaces the account-local server's shared edit channel publishes.
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
        self.present_server_workspaces(ServerOrigin::Local, edits, sessions, window, cx);
    }

    /// Presents the Workspaces one server's shared edit channel publishes, beside the Workspaces
    /// of every other presented server. A server already presented keeps its binding.
    pub(crate) fn present_server_workspaces(
        &mut self,
        origin: ServerOrigin,
        edits: super::shared_workspaces::SharedEditSource,
        sessions: super::shared_workspaces::SharedPaneSessions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ServerKey {
        use super::shared_workspaces::SharedWorkspaces;

        let first = self.shared.is_empty();
        let (key, inserted) = self
            .shared
            .insert(origin, SharedWorkspaces::new(edits, sessions));
        if !inserted {
            return key;
        }
        if first {
            self.follow_shared_pull_requests(cx);
            if let Some(handle) = window.window_handle().downcast::<Self>() {
                cx.set_global(super::shared_workspaces::SharedWorkspaceHost(handle));
            }
        }
        // Reads the server's inventory and this window's edits after each change either one
        // hints, starting at once. Nothing reads on a timer: every change the window shows is
        // hinted after it is visible, so a reading never misses one.
        let reads = self
            .shared
            .get(key)
            .map(super::shared_workspaces::SharedWorkspaces::subscribe)
            .map(|changes| {
                cx.spawn_in(window, async move |manager, cx| {
                    while changes.recv().await.is_ok() {
                        if manager
                            .update_in(cx, |manager, window, cx| {
                                manager.sync_shared_workspaces(key, window, cx);
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
            });
        if let Some(reads) = reads {
            self.shared.follow(key, reads);
        }
        cx.notify();
        key
    }

    /// Stops presenting one server's Workspaces, as when the person removes its paired route. The
    /// Workspaces and their Terminal Sessions keep running on that server, and nothing is asked
    /// of it: closing the local Panes drops their subscriptions.
    pub(crate) fn withdraw_server_workspaces(
        &mut self,
        key: ServerKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if key == ServerKey::LOCAL {
            return;
        }
        self.save_shared_arrangements(cx);
        let presented: Vec<WorkspaceId> = self
            .workspaces
            .iter()
            .map(|entry| entry.id())
            .filter(|&workspace_id| self.shared.owner(workspace_id) == Some(key))
            .collect();
        for workspace_id in presented {
            self.remove_workspace_presentation(workspace_id, window, cx);
        }
        let parked: Vec<_> = self
            .workspaces
            .parked_ids()
            .filter(|&id| self.shared.owner(id) == Some(key))
            .collect();
        for id in parked {
            if let Ok(manager) = self.workspaces.discard_parked_workspace(id) {
                manager.update(cx, |manager, cx| manager.close_all(cx));
            }
            self.pending_final_tab_closes.remove(&id);
        }
        if let Some(mut withdrawn) = self.shared.remove(key) {
            withdrawn.flush_client_views();
        }
        // A Workspace still waiting for this server says it couldn't open rather than vanishing.
        if self
            .pending_server_directory
            .as_ref()
            .is_some_and(|pending| pending.key == key)
        {
            self.continue_server_directory(window, cx);
        }
        self.present_shared_repositories(cx);
        cx.notify();
    }

    /// Reads one server's answer and its inventory, then presents both.
    fn sync_shared_workspaces(
        &mut self,
        key: ServerKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ready = self
            .shared
            .get_mut(key)
            .map(|shared| shared.prepare_client_views());
        if !matches!(ready, Some(Ok(true))) {
            self.report_client_view_error(window, cx);
            self.reread_after_modal(window, cx);
            return;
        }
        // Binding a provisional Pane may replace its focus handle during reconciliation. Read
        // keyboard ownership first, so that replacement cannot erase the initiating close intent.
        let keyboard_before_reconciliation = window.focused(cx);
        let Some(poll) = self
            .shared
            .get_mut(key)
            .map(super::shared_workspaces::SharedWorkspaces::poll)
        else {
            return;
        };
        self.reconcile_parked_shared_workspaces(key, window, cx);
        // Approved closes already moved presentation. Published membership never replaces newer focus.
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
            self.apply_shared_inventory(key, &inventory, removal, window, cx);
            self.settle_remembered_view(key, window, cx);
        }
        // A new client's first view is the person opening SpaceTerm to work, so it opens the
        // Active Tab and its Focused Pane's shell. Later launches restore views passively, and
        // so does a window reopened with the view the last one left, which nobody chose here.
        if let Some(workspace_id) = poll
            .initial
            .filter(|_| !self.restores_remembered_view())
            .and_then(|server| self.shared.get(key)?.local(server))
            && self.activate_workspace(workspace_id, window, cx)
        {
            self.focus(window, cx);
        }
        // The client that asked for a creation opens it, once, after the server published it.
        if let Some(accepted) = poll.accepted {
            self.open_accepted_shared(key, &accepted, window, cx);
        }
        self.present_reveal(window, cx);
        // Each answer below stays with this reading until its alert is admitted, and counts as
        // presented once the person answers that alert.
        if !spaceterm_ui::window_modal_is_open(window, cx)
            && let Some((conflict, notice)) = self
                .shared
                .get(key)
                .and_then(super::shared_workspaces::SharedWorkspaces::next_conflict)
            && self.present_shared_conflict(conflict, &notice, window, cx)
            && let Some(shared) = self.shared.get_mut(key)
        {
            let _ = shared.take_conflict();
        }
        while let Some((refusal, notice)) = self
            .shared
            .get(key)
            .and_then(super::shared_workspaces::SharedWorkspaces::next_refusal)
            && self.present_shared_refusal(refusal, Some(&notice), window, cx)
            && let Some(shared) = self.shared.get_mut(key)
        {
            let _ = shared.take_refusal();
        }
        if let Some(notice) = self
            .shared
            .get(key)
            .and_then(super::shared_workspaces::SharedWorkspaces::next_forgotten)
            && self.present_forgotten_outcomes(&notice, window, cx)
            && let Some(shared) = self.shared.get_mut(key)
        {
            let _ = shared.take_forgotten();
        }
        while let Some((server_target, local, notice)) = self
            .shared
            .get(key)
            .and_then(super::shared_workspaces::SharedWorkspaces::next_stale_close)
            && self.present_stale_shared_close(server_target, local, Some(&notice), window, cx)
            && let Some(shared) = self.shared.get_mut(key)
        {
            let _ = shared.take_stale_close();
        }
        self.revalidate_shared_close(key, window, cx);
        if !spaceterm_ui::window_modal_is_open(window, cx)
            && self.close_confirmation.pending().is_none()
            && let Some(intent) = self.shared.get(key).and_then(|shared| shared.take_close_intent())
            // The local presentation must still name the exact creation this intent followed.
            && self.shared_close_target(intent.local, cx) == Some(intent.server)
        {
            let held_keyboard = intent.keyboard.is_some()
                && intent.keyboard == keyboard_before_reconciliation
                && self.close_target_is_active(intent.local, cx);
            self.shared_close_keyboard.reviewed = Some(held_keyboard);
            self.request_shared_close(intent.local, window, cx);
            self.shared_close_keyboard.reviewed = None;
        }
        // Each server's reading may present the Worktree it created or change the one under
        // review; both look up their own Workspace's server.
        self.open_created_server_worktree(window, cx);
        self.revalidate_server_removal(window, cx);
        // A Workspace Open Remote Directory is opening may be asked for now.
        self.continue_server_directory(window, cx);
        // New Workspace chosen with no Workspace Window open waits for this reading.
        self.create_pending_workspace(key, window, cx);
        // A form presents its own Create's answer, so a failure reaches here only when no
        // surface that asked for it presents it.
        while let Some((failure, notice)) = self
            .shared
            .get(key)
            .and_then(super::shared_workspaces::SharedWorkspaces::next_worktree_failure)
            && self.server_worktree_operation_failure(
                &failure.operation,
                failure.error,
                &notice,
                window,
                cx,
            )
            && let Some(shared) = self.shared.get_mut(key)
        {
            let _ = shared.take_worktree_failure();
        }
        self.save_shared_arrangements(cx);
        self.report_client_view_error(window, cx);
        self.read_refusal_offer(cx);
        // Membership, connection, and Git facts all decide what each view presents, including
        // Workspaces nobody is looking at.
        if poll.changed {
            self.present_shared_repositories(cx);
            // Metadata alone can change without a membership revision.
            if let Some(inventory) = self.shared.get(key).and_then(|s| s.latest_inventory()) {
                self.present_shared_metadata(key, inventory, cx);
            }
        }
        if std::mem::take(&mut self.close_empty_after_shared_removal)
            && self.workspaces.active_workspace_id().is_none()
            && self.workspaces.parked_ids().next().is_none()
        {
            for (_, shared) in self.shared.iter() {
                shared.edits().retain_until_settled(cx);
            }
            if cx.windows().len() == 1 {
                crate::status_menu::show_in_dock(cx, false);
            }
            self.close_window(window, cx);
            return;
        }
        self.reread_after_modal(window, cx);
    }

    /// Presents the Terminal Metadata `inventory` publishes for every Pane of its server's
    /// Workspaces this window shows, active or not, painted or not. The inventory names each
    /// Pane's Session run, so metadata from a run that ended never replaces the current one's.
    fn present_shared_metadata(
        &self,
        key: ServerKey,
        inventory: &spaceterm_server::hierarchy::Inventory,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::terminal_pane::ServerSessionRun;

        let runs = inventory.close.as_ref().map(|close| {
            close
                .sessions
                .iter()
                .filter_map(|pane| {
                    let session = pane.session?;
                    Some((
                        pane.id,
                        ServerSessionRun {
                            boot: close.boot,
                            session,
                        },
                    ))
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        });
        let published = inventory
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| &tab.panes)
            .filter_map(|pane| {
                let run = *runs.as_ref()?.get(&pane.id)?;
                Some((pane.id, (run, pane.presentation.as_deref()?)))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let lookup = |pane| published.get(&pane).copied();
        for entry in self.workspaces.iter() {
            if self.shared.owner(entry.id()) != Some(key) {
                continue;
            }
            entry.payload().clone().update(cx, |manager, cx| {
                manager.present_shared_metadata(&lookup, cx);
            });
        }
    }

    fn save_shared_arrangements(&mut self, cx: &App) {
        for key in self.shared.keys() {
            let workspaces = self
                .workspaces
                .iter()
                .filter(|entry| self.shared.owner(entry.id()) == Some(key))
                .filter_map(|entry| entry.payload().read(cx).shared_arrangement(cx))
                .collect();
            if let Some(shared) = self.shared.get_mut(key) {
                shared.save_arrangements(spaceterm_server::client_view::ClientView { workspaces });
            }
        }
    }

    /// A reading defers what an open modal would cover, and an error notice is itself a modal.
    /// While one is open, every presented server is read again once it closes.
    fn reread_after_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_reread.is_some() || !spaceterm_ui::window_modal_is_open(window, cx) {
            return;
        }
        self.modal_reread =
            spaceterm_ui::when_window_modal_closes(window, cx, |manager, window, cx| {
                manager.modal_reread = None;
                for key in manager.shared.keys() {
                    manager.sync_shared_workspaces(key, window, cx);
                }
            });
    }

    fn report_client_view_error(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let notice = self
            .shared
            .iter_mut()
            .find_map(|shared| shared.take_view_error());
        // The retained Workspace order says it couldn't be read or saved the same way.
        let notice = notice.or_else(|| self.take_display_order_error());
        if let Some(notice) = notice
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
        key: ServerKey,
        inventory: &spaceterm_server::hierarchy::Inventory,
        removal: super::shared_workspaces::SharedRemoval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::shared_workspaces::SharedWorkspacePlan;

        let Some(plan) = self.shared.get(key).map(|shared| shared.plan(inventory)) else {
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
                    self.present_shared_workspace(key, &facts, workspace, window, cx);
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
                    if let Some(shared) = self.shared.get_mut(key) {
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
        // Workspaces this inventory just presented take their places in this client's order.
        self.reconcile_display_order(key);
        // Views this inventory just created, including a reattached Workspace's, present its
        // metadata now. A later poll may carry the same inventory and report no change.
        self.present_shared_metadata(key, inventory, cx);
        self.synchronize_tab_manager_layouts(window, cx);
        cx.notify();
    }

    /// Presents one server Workspace this window has not seen before. It joins the sidebar and
    /// never becomes the Active Workspace, because another client's creation is membership here.
    fn present_shared_workspace(
        &mut self,
        key: ServerKey,
        facts: &super::shared_workspaces::SharedWorkspaceFacts,
        workspace: &spaceterm_server::hierarchy::Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::shared_workspaces::SharedTabs;

        let Some(shared) = self.shared.get(key) else {
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
        let Some((session_factory, root_state)) = sessions.open(root_tab.root_pane) else {
            return;
        };
        // The Workspace's own directory stays this client's home: it is launch scaffolding the
        // server-backed factory ignores, and the sidebar shows the server's directory instead.
        let Ok(directory) = self.local_home_directory() else {
            Self::show_home_directory_unavailable(window, cx);
            return;
        };
        let active_workspace_id = self.workspaces.active_workspace_id();
        // The empty view or launch screen held the keyboard, which moves to the Workspace that
        // takes its place, without asking the server for its Pane.
        let empty_held_keyboard =
            active_workspace_id.is_none() && self.empty_focus.is_focused(window);
        let active_manager = self.active_tab_manager();
        let window_drag_platform = Rc::clone(&self.operating_system_window_drag_platform);
        let sidebar_visible = self.sidebar.read(cx).layout().visible;
        let sidebar_width = self.sidebar.read(cx).layout().width;
        let pane_construction = sessions.panes().clone();
        let created = self
            .workspaces
            .create_local_workspace(directory, |workspace_id, _| {
                Self::create_tab_manager(
                    session_factory,
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
        if empty_held_keyboard {
            manager.update(cx, |manager, cx| manager.focus(window, cx));
        }
        if let Some(shared) = self.shared.get_mut(key) {
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
            .get(key)
            .and_then(|shared| shared.saved_arrangement(workspace.id))
            .cloned()
        {
            manager.update(cx, |manager, cx| {
                manager.restore_shared_arrangement(&saved, workspace, cx)
            });
        }
        self.restore_remembered_workspace(key, workspace_id, workspace.id, window, cx);
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

        let Some(shared) = self.shared.local_mut() else {
            // The launch screen already says why the server is not ready, and offers its step.
            if self.workspaces.active_workspace_id().is_none() {
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
        } else {
            self.sync_shared_workspaces(ServerKey::LOCAL, window, cx);
        }
        cx.notify();
        submission == SharedSubmission::Submitted
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

        let Some(shared) = self.shared.owning_mut(workspace_id) else {
            return false;
        };
        let submission = shared.rename(workspace_id, name.to_owned(), observed_version);
        if let SharedSubmission::Refused(reason) = submission {
            Self::show_shared_refusal("rename the Workspace", reason, window, cx);
        } else if let Some(key) = self.shared.owner(workspace_id) {
            self.sync_shared_workspaces(key, window, cx);
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
        pin: Option<&ValidatedLocalDirectory>,
        observed_version: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        use super::shared_workspaces::SharedSubmission;

        let owner = self.shared.owner(workspace_id)?;
        let directory = match pin {
            // A local directory names a directory on this machine, which only the account-local
            // server shares. Another server's Workspace never receives one.
            Some(_) if owner != ServerKey::LOCAL => {
                self.transient.end_pin();
                Self::show_shared_unavailable(REMOTE_LOCAL_PIN_REFUSED, window, cx);
                return Some(false);
            }
            Some(directory) => Some(directory.path().to_owned()),
            None => None,
        };
        self.pin_operation = self.pin_operation.wrapping_add(1);
        self.transient.end_pin();
        // `observed_version` is the pin version the chooser opened on. An immediate Unpin has no
        // chooser, so it carries none and is fenced to the version its row showed.
        let shared = self.shared.get_mut(owner)?;
        let submission = shared.pin(workspace_id, directory, observed_version);
        if let SharedSubmission::Refused(reason) = submission {
            Self::show_shared_refusal("change the pinned directory", reason, window, cx);
        } else {
            self.sync_shared_workspaces(owner, window, cx);
        }
        cx.notify();
        Some(submission == SharedSubmission::Submitted)
    }

    /// Asks another computer's server to pin its Workspace to `directory`, a path that server
    /// validated on its own filesystem. Returns whether the request reached the server.
    fn pin_server_directory(
        &mut self,
        workspace_id: WorkspaceId,
        directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use super::shared_workspaces::SharedSubmission;

        let observed_version = self.transient.pin_version(workspace_id);
        let Some(owner) = self
            .shared
            .owner(workspace_id)
            .filter(|owner| *owner != ServerKey::LOCAL)
        else {
            return false;
        };
        self.pin_operation = self.pin_operation.wrapping_add(1);
        self.transient.end_pin();
        let Some(shared) = self.shared.get_mut(owner) else {
            return false;
        };
        let submission = shared.pin(workspace_id, Some(directory), observed_version);
        if let SharedSubmission::Refused(reason) = submission {
            Self::show_shared_refusal("change the pinned directory", reason, window, cx);
        } else {
            self.sync_shared_workspaces(owner, window, cx);
        }
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
        submission == SharedSubmission::Submitted
    }

    /// The edit writer of the server that owns a shared Workspace. It reaches that server, and
    /// only it, over the route this client admitted for it.
    pub(super) fn owning_client(
        &self,
        workspace_id: WorkspaceId,
        cx: &mut App,
    ) -> Option<crate::ui::server_worktrees::Client> {
        self.server_client(self.shared.owner(workspace_id)?, cx)
    }

    /// The edit writer of the server presented at `key`.
    fn server_client(
        &self,
        key: ServerKey,
        cx: &mut App,
    ) -> Option<crate::ui::server_worktrees::Client> {
        use crate::ui::server_worktrees::Client;

        match self.shared.origin(key)? {
            ServerOrigin::Local => Client::get(cx),
            ServerOrigin::Direct(direct) => Client::direct(&direct.route, cx),
        }
    }

    /// Browses the directories of the server on another computer that owns `workspace_id`, or
    /// `None` while that server is unreachable.
    fn owning_directory_source(
        &self,
        workspace_id: WorkspaceId,
        cx: &mut App,
    ) -> Option<ServerDirectorySource> {
        self.server_directory_source(self.shared.owner(workspace_id)?, cx)
    }

    /// Browses the directories of the server on another computer presented at `key`, or `None`
    /// while that server is unreachable.
    fn server_directory_source(
        &self,
        key: ServerKey,
        cx: &mut App,
    ) -> Option<ServerDirectorySource> {
        let machine = self
            .shared
            .origin(key)
            .and_then(ServerOrigin::remote_name)?;
        let client = self.server_client(key, cx)?;
        if !client.availability().0 {
            return None;
        }
        let server = client.server.clone();
        let available: Rc<dyn Fn() -> bool> = Rc::new(move || client.availability().0);
        Some(ServerDirectorySource::browsing(
            server,
            Some(machine),
            available,
        ))
    }

    /// Asks to close a Pane, Tab, or Workspace a SpaceTerm Server owns, returning whether the
    /// target is shared.
    ///
    /// Captures the exact server targets before applying contextual confirmation. Idle own work
    /// closes directly; other clients' work and Workspace removal always require review.
    fn request_shared_close(
        &mut self,
        target: CloseTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use super::shared_workspaces::{
            SharedCloseIntent, SharedCloseReview, SharedCloseUnavailable,
        };

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
            // A target this window can no longer name closes nothing.
            return true;
        };
        if self.request_worktree_close_fallback(target, server_target, window, cx) {
            return true;
        }
        // A Review hands over what the person's first request held; any other request reads it
        // now, before a confirmation takes the keyboard.
        let held_keyboard = self
            .shared_close_keyboard
            .reviewed
            .take()
            .unwrap_or_else(|| {
                self.workspaces
                    .workspace(workspace_id)
                    .is_some_and(|entry| entry.payload().read(cx).holds_keyboard(window, cx))
            });
        let Some(shared) = self.shared.owning(workspace_id) else {
            return true;
        };
        match shared.defer_close(SharedCloseIntent {
            server: server_target,
            local: target,
            keyboard: held_keyboard.then(|| window.focused(cx)).flatten(),
        }) {
            Ok(true) => return true,
            Ok(false) => {}
            Err(reason) => {
                Self::show_shared_refusal(Self::shared_close_operation(target), reason, window, cx);
                return true;
            }
        }
        let approval = match shared.close_approval(server_target) {
            Ok(approval) => approval,
            Err(SharedCloseUnavailable::Edits(reason)) => {
                Self::show_shared_refusal(Self::shared_close_operation(target), reason, window, cx);
                return true;
            }
            Err(SharedCloseUnavailable::Missing) => {
                self.present_shared_refusal(
                    super::shared_workspaces::SharedRefusal::Missing,
                    None,
                    window,
                    cx,
                );
                return true;
            }
            Err(SharedCloseUnavailable::Unreported) => {
                Self::show_shared_unavailable_with_title(
                    "Couldn\u{2019}t Close Yet",
                    "Wait for SpaceTerm to check the affected Terminal Sessions, then try again.",
                    window,
                    cx,
                );
                return true;
            }
        };
        let direct = approval.direct_close(|pane, live_session| {
            self.workspaces
                .workspace(workspace_id)
                .is_some_and(|entry| {
                    entry
                        .payload()
                        .read(cx)
                        .shared_pane_closes_directly(pane, live_session, cx)
                })
        });
        if let Some(approval) = direct {
            use super::shared_workspaces::SharedSubmission;

            let key = self.shared.owner(workspace_id);
            let submission = self
                .shared
                .owning_mut(workspace_id)
                .map(|shared| shared.close(approval, target));
            if let Some(SharedSubmission::Refused(reason)) = submission {
                Self::show_shared_refusal(Self::shared_close_operation(target), reason, window, cx);
            }
            if submission == Some(SharedSubmission::Submitted) {
                self.prepare_shared_close_keyboard(target, held_keyboard, cx);
            }
            if let Some(key) = key {
                self.sync_shared_workspaces(key, window, cx);
            }
            // A reviewed close that needs no confirmation now still moves the keyboard the
            // person's first request held.
            if held_keyboard && submission == Some(SharedSubmission::Submitted) {
                self.focus(window, cx);
            }
            return true;
        }
        let Some(window_handle) = window.window_handle().downcast::<Self>() else {
            return true;
        };
        let Some(generation) = self.close_confirmation.begin(target) else {
            return true;
        };
        self.shared_close_keyboard.pending = Some((generation, held_keyboard));
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
        // A Review opens this beneath the notice's held sidebar rename, which it keeps holding.
        let presented = self.present_keeping_sidebar_rename(
            alert,
            move |outcome, cx| {
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
            },
            |_, _| {},
            window,
            cx,
        );
        match presented {
            Ok(presentation) => {
                if let Some(shared) = self.shared.owning_mut(workspace_id) {
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
                self.shared_close_keyboard.take(generation);
                eprintln!("failed to present the shared close confirmation: {error}");
            }
        }
        true
    }

    fn prepare_shared_close_keyboard(
        &self,
        target: CloseTarget,
        held_keyboard: bool,
        cx: &mut App,
    ) {
        if let Some(entry) =
            close_target_workspace(target).and_then(|id| self.workspaces.workspace(id))
        {
            entry.payload().update(cx, |manager, cx| {
                manager.prepare_shared_close_keyboard(held_keyboard, cx);
            });
        }
    }

    /// Later navigation wins over the keyboard intent of a deferred close.
    fn close_target_is_active(&self, target: CloseTarget, cx: &App) -> bool {
        let Some(workspace_id) = close_target_workspace(target) else {
            return false;
        };
        if self.workspaces.active_workspace_id() != Some(workspace_id) {
            return false;
        }
        let Some(entry) = self.workspaces.workspace(workspace_id) else {
            return false;
        };
        let active = entry.payload().read(cx).active_terminal_identity(cx);
        match target {
            CloseTarget::Tab { tab_id, .. } => active.0 == tab_id,
            CloseTarget::Pane {
                tab_id, pane_id, ..
            } => active == (tab_id, pane_id),
            _ => true,
        }
    }

    /// What a refused shared close says it did not do.
    const fn shared_close_operation(target: CloseTarget) -> &'static str {
        match target {
            CloseTarget::Pane { .. } => "close the Pane",
            CloseTarget::Tab { .. } => "close the Tab",
            CloseTarget::Worktree { .. } => "close the Worktree\u{2019}s Tabs",
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

        let workspace_id = match target {
            CloseTarget::Workspace(workspace_id)
            | CloseTarget::Tab { workspace_id, .. }
            | CloseTarget::Pane { workspace_id, .. }
            | CloseTarget::Worktree { workspace_id, .. } => workspace_id,
            CloseTarget::Window => return None,
        };
        let shared = self.shared.owning(workspace_id)?;
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
            CloseTarget::Worktree {
                workspace_id,
                worktree_id,
            } => {
                let workspace = shared.server_workspace(workspace_id)?;
                let path = self.shared_worktree_root(workspace_id, worktree_id)?;
                let anchor = workspace
                    .tabs
                    .iter()
                    .find(|tab| tab.worktree.as_ref() == Some(&path))?;
                Some(ServerCloseTarget::Worktree {
                    workspace: workspace.id,
                    tab: anchor.id,
                })
            }
            CloseTarget::Window => None,
        }
    }

    /// Settles a shared close confirmation. A confirmed close asks the server to end exactly what
    /// the confirmation showed, and only while that is still what this client last read.
    ///
    /// An approved close hides what it ends at once. When the closing Workspace held the keyboard
    /// before the confirmation took it, the keyboard moves to what remains, as a close that needs
    /// no confirmation does. The confirmation would otherwise return it to a Pane that is gone.
    /// A sidebar rename held beneath the confirmation gets the keyboard back unless the close
    /// removed its Workspace, which ends the rename and moves the keyboard to what remains.
    fn settle_shared_close(
        &mut self,
        generation: u64,
        target: CloseTarget,
        confirmed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::shared_workspaces::{SharedCloseState, SharedSubmission};

        let held_keyboard = self.shared_close_keyboard.take(generation);

        // The review lives with the server that owns what it would close.
        let review = self.shared.keys().into_iter().find_map(|key| {
            let review = self.shared.get_mut(key)?.end_review(generation)?;
            Some((key, review))
        });
        if !self
            .close_confirmation
            .settle_approval(generation, target, confirmed)
        {
            return;
        }
        let Some((key, review)) = review else {
            return;
        };
        let Some(shared) = self.shared.get_mut(key) else {
            return;
        };
        match shared.close_state(&review.approval) {
            SharedCloseState::Current => {
                let renaming = self.sidebar.read(cx).is_renaming();
                let submission = shared.close(review.approval, review.target);
                if let SharedSubmission::Refused(reason) = submission {
                    Self::show_shared_refusal(
                        Self::shared_close_operation(target),
                        reason,
                        window,
                        cx,
                    );
                }
                if submission == SharedSubmission::Submitted {
                    self.prepare_shared_close_keyboard(target, held_keyboard, cx);
                }
                self.sync_shared_workspaces(key, window, cx);
                let rename_removed = renaming && !self.sidebar.read(cx).is_renaming();
                if (held_keyboard || rename_removed) && submission == SharedSubmission::Submitted {
                    self.focus(window, cx);
                }
            }
            SharedCloseState::Changed => {
                self.present_shared_close_changed(
                    review.approval.changed_title(),
                    Some(target),
                    held_keyboard,
                    None,
                    window,
                    cx,
                );
            }
            SharedCloseState::Missing => {
                self.present_shared_close_missing(&review.approval, window, cx);
            }
        }
        cx.notify();
    }

    /// Withdraws a shared close confirmation that another client's change made stale, and says
    /// why. The person never approves Sessions or Panes they were not shown.
    fn revalidate_shared_close(
        &mut self,
        key: ServerKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::shared_workspaces::SharedCloseState;

        let Some(shared) = self.shared.get_mut(key) else {
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
        let held_keyboard = self.shared_close_keyboard.take(generation);
        if let Some(presentation) = review.presentation.as_ref() {
            let _ = presentation.dismiss(window, cx);
        }
        match state {
            SharedCloseState::Missing => {
                self.present_shared_close_missing(&review.approval, window, cx);
            }
            _ => {
                self.present_shared_close_changed(
                    review.approval.changed_title(),
                    Some(review.target),
                    held_keyboard,
                    None,
                    window,
                    cx,
                );
            }
        }
    }

    /// Explains that what a close would end changed after it was approved or shown, so nothing
    /// was closed. Review opens a fresh confirmation of what the server holds now; nothing asks
    /// again on its own. Review keeps `held_keyboard`, whether the closing Workspace held the
    /// keyboard when the person asked, so the approved fresh close moves it as the first would.
    /// Returns whether the alert was admitted. `notice` records the presentation of an unseen
    /// outcome.
    fn present_shared_close_changed(
        &mut self,
        title: &'static str,
        target: Option<CloseTarget>,
        held_keyboard: bool,
        notice: Option<&super::shared_workspaces::OutcomeNotice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let message = if target.is_some() {
            "Nothing was closed. Review the updated Terminal Sessions before closing."
        } else {
            "Nothing was closed. Check the Workspace before closing it again."
        };
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
        let lifecycle = notice.map(|notice| notice.lifecycle());
        let alert = Alert::new(
            ModalId::new("shared-close-changed"),
            title,
            title,
            message,
            actions,
        )
        .intent(AlertIntent::Warning);
        let result = self.present_keeping_sidebar_rename(
            alert,
            move |outcome, cx| {
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
                        manager.shared_close_keyboard.reviewed = Some(held_keyboard);
                        manager.request_close(target, window, cx);
                        manager.shared_close_keyboard.reviewed = None;
                    });
                });
            },
            move |event, cx| {
                if let Some(lifecycle) = &lifecycle {
                    lifecycle(event, cx);
                }
            },
            window,
            cx,
        );
        if let Err(error) = &result {
            eprintln!("failed to present the changed shared close: {error}");
        }
        result.is_ok()
    }

    /// Presents a notice about an earlier operation's outcome, or the confirmation its Review
    /// opens. A person may be typing in the sidebar by the time it arrives, so the modal returns
    /// the keyboard to that draft when it closes rather than ending it.
    fn present_keeping_sidebar_rename<A: Clone + Eq + 'static>(
        &mut self,
        alert: Alert<A>,
        on_result: impl FnOnce(AlertOutcome<A>, &mut App) + 'static,
        on_lifecycle: impl Fn(&spaceterm_ui::ModalLifecycleEvent, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<spaceterm_ui::ModalPresentationHandle, spaceterm_ui::ModalPresentationError> {
        let sidebar = self.sidebar.clone();
        let held = sidebar.update(cx, |sidebar, _| sidebar.hold_rename_for_modal(window));
        let release = sidebar.downgrade();
        let window_handle = window.window_handle();
        let result = alert.present_with_lifecycle(
            window,
            cx,
            move |outcome, cx| {
                if held {
                    let _ = window_handle.update(cx, |_, window, _| {
                        // The modal returns focus as it next draws, so the hold ends after that
                        // frame.
                        window.on_next_frame(move |window, _| {
                            window.on_next_frame(move |window, cx| {
                                let _ = release.update(cx, |sidebar, cx| {
                                    sidebar.release_rename_hold(window, cx);
                                });
                            });
                        });
                    });
                }
                on_result(outcome, cx);
            },
            on_lifecycle,
        );
        if held && result.is_err() {
            sidebar.update(cx, |sidebar, cx| sidebar.release_rename_hold(window, cx));
        }
        result
    }

    /// Presents a close the server refused because what it would end changed first. Review is
    /// offered only to the process that asked, and only while its target is still here.
    /// Returns whether the alert was admitted.
    fn present_stale_shared_close(
        &mut self,
        server_target: spaceterm_server::hierarchy::CloseTarget,
        local: Option<CloseTarget>,
        notice: Option<&super::shared_workspaces::OutcomeNotice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let local =
            local.filter(|local| self.shared_close_target(*local, cx) == Some(server_target));
        // The refused close already moved the keyboard, so a Review keeps what holds it now.
        let held_keyboard = local
            .and_then(close_target_workspace)
            .and_then(|workspace_id| self.workspaces.workspace(workspace_id))
            .is_some_and(|entry| entry.payload().read(cx).holds_keyboard(window, cx));
        self.present_shared_close_changed(
            super::shared_workspaces::shared_close_changed_title(server_target),
            local,
            held_keyboard,
            notice,
            window,
            cx,
        )
    }

    /// Explains that another client already removed what a close confirmation named.
    fn present_shared_close_missing(
        &mut self,
        approval: &super::shared_workspaces::SharedCloseApproval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let alert = Alert::new(
            ModalId::new("shared-close-missing"),
            approval.missing_title(),
            approval.missing_title(),
            "Another client already closed it.",
            vec![ModalAction::new(
                (),
                "OK",
                ModalActionRole::Cancel,
                SHARED_CLOSE_DISMISS_SELECTOR,
            )],
        )
        .intent(AlertIntent::Informational);
        let _ = self.present_keeping_sidebar_rename(alert, |_, _| {}, |_, _| {}, window, cx);
    }

    /// Presents a refused same-field edit: the server's value, the draft it rejected, and the
    /// choice to ask again with that draft. Nothing retries on its own.
    fn present_shared_conflict(
        &mut self,
        conflict: super::shared_workspaces::SharedWorkspaceConflict,
        notice: &super::shared_workspaces::OutcomeNotice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let name = self.workspaces.workspace(conflict.workspace).map_or_else(
            || "this Workspace".to_owned(),
            |entry| entry.name().to_owned(),
        );
        let detail = format!(
            "Another client changed the {} of {name}. Current value: {}\nYour change: {}",
            conflict.field.label(),
            conflict.authoritative,
            conflict.draft.display(),
        );
        let owner = cx.weak_entity();
        let window_handle = window.window_handle();
        let alert = Alert::new(
            ModalId::new("shared-workspace-conflict"),
            "Change not applied",
            conflict.field.title(),
            detail,
            vec![
                ModalAction::new(
                    true,
                    conflict.field.retry_label(),
                    ModalActionRole::Affirmative,
                    SHARED_CONFLICT_RETRY_SELECTOR,
                )
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    false,
                    "Keep Current Value",
                    ModalActionRole::Cancel,
                    SHARED_CONFLICT_KEEP_SELECTOR,
                ),
            ],
        )
        .intent(AlertIntent::Warning);
        let result = self.present_keeping_sidebar_rename(
            alert,
            move |outcome, cx| {
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
            },
            notice.lifecycle(),
            window,
            cx,
        );
        if let Err(error) = result {
            eprintln!("failed to present the shared change conflict: {error}");
            return false;
        }
        true
    }

    /// Asks the server again with the preserved draft and the version shown in the conflict.
    fn retry_shared_edit(
        &mut self,
        conflict: &super::shared_workspaces::SharedWorkspaceConflict,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::shared_workspaces::SharedSubmission;

        let Some(shared) = self.shared.owning_mut(conflict.workspace) else {
            return;
        };
        if let SharedSubmission::Refused(reason) = shared.ask_again(conflict) {
            Self::show_shared_refusal("apply the change", reason, window, cx);
        }
        cx.notify();
    }

    /// Presents a shared edit the server refused for a reason no retry repairs.
    /// Returns whether the alert was admitted.
    fn present_shared_refusal(
        &mut self,
        refusal: super::shared_workspaces::SharedRefusal,
        notice: Option<&super::shared_workspaces::OutcomeNotice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let alert = Alert::new(
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
        .intent(AlertIntent::Warning);
        let lifecycle = notice.map(|notice| notice.lifecycle());
        self.present_keeping_sidebar_rename(
            alert,
            |_, _| {},
            move |event, cx| {
                if let Some(lifecycle) = &lifecycle {
                    lifecycle(event, cx);
                }
            },
            window,
            cx,
        )
        .is_ok()
    }

    /// Says that more refused changes and failed Worktree operations went unseen than this client
    /// keeps. Returns whether the alert was admitted.
    fn present_forgotten_outcomes(
        &mut self,
        notice: &super::shared_workspaces::OutcomeNotice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let alert = Alert::new(
            ModalId::new("shared-outcomes-forgotten"),
            "Some Results Weren\u{2019}t Shown",
            "Some Results Weren\u{2019}t Shown",
            format!(
                "More than {} changes or Worktree operations failed before SpaceTerm could show \
                 them, and the earliest are no longer available. Review your Workspaces and \
                 Worktrees.",
                spaceterm_server::hierarchy::UNSEEN_OUTCOMES
            ),
            vec![ModalAction::new(
                (),
                "OK",
                ModalActionRole::Cancel,
                "shared-outcomes-forgotten-ok",
            )],
        )
        .intent(AlertIntent::Warning);
        self.present_keeping_sidebar_rename(alert, |_, _| {}, notice.lifecycle(), window, cx)
            .is_ok()
    }

    /// Explains a shared edit this client could not submit. Nothing was sent, and nothing waits.
    fn show_shared_refusal(
        _operation: &str,
        reason: super::shared_workspaces::SharedRefusalReason,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let detail = match reason {
            super::shared_workspaces::SharedRefusalReason::Offline => "Reconnect, then try again.",
            super::shared_workspaces::SharedRefusalReason::Pending => {
                "Wait for the current changes to finish, then try again."
            }
            super::shared_workspaces::SharedRefusalReason::Value => {
                "Enter a shorter name or directory, then try again."
            }
        };
        Self::show_shared_unavailable_with_title("Couldn\u{2019}t Make Change", detail, window, cx);
    }

    /// Returns whether the alert was presented over the active Settings Window.
    fn show_shared_unavailable(detail: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        Self::show_shared_unavailable_with_title("Shared Change Unavailable", detail, window, cx)
    }

    fn show_shared_unavailable_with_title(
        title: &'static str,
        detail: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let detail = detail.to_owned();
        let alert = move || {
            Alert::new(
                ModalId::new("shared-workspace-unavailable"),
                "Shared change unavailable",
                title,
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
        self.shared.owner(workspace_id).is_some()
    }

    /// Whether the account-local server owns the Workspace. Only that server shares this
    /// computer's filesystem, so only its Workspaces take local directories and Worktree writes.
    pub(super) fn is_local_server_workspace(&self, workspace_id: WorkspaceId) -> bool {
        self.shared.owner(workspace_id) == Some(ServerKey::LOCAL)
    }

    /// Opens what one accepted edit of this client's own created, once the server published it.
    ///
    /// Only the client that asked for the creation does this. A Workspace, Tab, or Pane another
    /// client created joins the presentation without moving anything here.
    fn open_accepted_shared(
        &mut self,
        key: ServerKey,
        accepted: &super::shared_workspaces::SharedAcceptance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use spaceterm_server::hierarchy::Mutation;

        if self.is_worktree_close_fallback(&accepted.mutation) {
            return;
        }

        match &accepted.mutation {
            Mutation::CreateWorkspace { .. } => {
                let Some(workspace_id) = self
                    .shared
                    .get(key)
                    .and_then(|shared| shared.local(accepted.target))
                else {
                    return;
                };
                if self.quiet_creation != Some(key)
                    && self.activate_workspace(workspace_id, window, cx)
                {
                    self.focus(window, cx);
                    self.scroll_active_workspace_into_view(cx);
                }
            }
            Mutation::CreateTab { workspace, .. }
            | Mutation::SplitPane { workspace, .. }
            | Mutation::Worktree(spaceterm_server::worktree::Operation::OpenTab {
                workspace,
                ..
            }) => {
                let Some(manager) = self
                    .shared
                    .get(key)
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
            // Ended Panes stay where they are. Each one presents Session Ended until someone
            // starts it, so nothing here moves the keyboard or starts a shell.
            Mutation::EndAllSessions { .. } => {}
        }
    }

    /// Records a Reveal from the Focused Pane `origin` of the Active Workspace, and shows it once
    /// its server publishes the target. A Reveal from any other Pane is dropped.
    fn request_reveal(
        &mut self,
        workspace_id: WorkspaceId,
        origin: (TabId, PaneId),
        target: spaceterm_server::hierarchy::Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self
            .workspaces
            .active_workspace()
            .filter(|workspace| workspace.id() == workspace_id)
        else {
            return;
        };
        let manager = workspace.payload().read(cx);
        if manager.active_terminal_identity(cx) != origin {
            return;
        }
        let navigation = manager.navigation(cx);
        let Some(key) = self.shared.owner(workspace_id) else {
            return;
        };
        self.pending_reveal = Some(PendingReveal {
            key,
            workspace_id,
            navigation,
            target,
            deadline: std::time::Instant::now() + REVEAL_LIFETIME,
        });
        self.present_reveal(window, cx);
    }

    /// Shows the pending Reveal's Pane once it is presented here. Any newer selection in the
    /// origin's Workspace, or leaving that Workspace, drops it instead.
    fn present_reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(reveal) = self.pending_reveal else {
            return;
        };
        let navigation = self
            .workspaces
            .active_workspace()
            .filter(|workspace| workspace.id() == reveal.workspace_id)
            .map(|workspace| workspace.payload().read(cx).navigation(cx));
        if navigation != Some(reveal.navigation) || std::time::Instant::now() >= reveal.deadline {
            self.pending_reveal = None;
            return;
        }
        let Some(workspace_id) = self
            .shared
            .get(reveal.key)
            .and_then(|shared| shared.local(reveal.target.workspace))
        else {
            return;
        };
        let Some(manager) = self
            .workspaces
            .workspace(workspace_id)
            .map(|entry| entry.payload().clone())
        else {
            return;
        };
        if !manager
            .read(cx)
            .presents_shared_pane(reveal.target.pane, cx)
        {
            return;
        }
        self.pending_reveal = None;
        if workspace_id != reveal.workspace_id {
            if !self.activate_workspace(workspace_id, window, cx) {
                return;
            }
            self.focus(window, cx);
            self.scroll_active_workspace_into_view(cx);
        }
        manager.update(cx, |manager, cx| {
            manager.open_accepted_shared(reveal.target.pane, window, cx);
        });
    }

    /// Removes a Workspace the server no longer has.
    ///
    /// The server already removed it, so this needs no confirmation here. It ends nothing on the
    /// server. An optimistic removal parks its presentation and keeps the edit owner alive until
    /// the Original Operation settles, so a refused close can restore the same views.
    fn remove_shared_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        removal: super::shared_workspaces::SharedRemoval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .shared
            .owning(workspace_id)
            .is_some_and(|shared| shared.optimistically_closing(workspace_id))
        {
            if self.workspaces.parked_ids().any(|id| id == workspace_id) {
                return;
            }
            let held_keyboard = self
                .workspaces
                .workspace(workspace_id)
                .is_some_and(|entry| entry.payload().read(cx).holds_keyboard(window, cx));
            if let Some(entry) = self.workspaces.workspace(workspace_id) {
                entry
                    .payload()
                    .update(cx, |manager, cx| manager.deactivate(cx));
            }
            if self.workspaces.park_workspace(workspace_id).is_err() {
                return;
            }
            // The person's close removed this Workspace from view, so its rename ends unsent.
            self.sidebar.update(cx, |sidebar, cx| {
                sidebar.cancel_rename_for(workspace_id, cx)
            });
            if let Some(manager) = self.active_tab_manager() {
                manager.update(cx, |manager, cx| {
                    if held_keyboard {
                        manager.activate_in_place(window, cx);
                    } else {
                        manager.activate_without_focus(cx);
                    }
                });
            } else if held_keyboard {
                self.empty_focus.focus(window, cx);
            }
            self.sync_terminal_focus_blocker(window, cx);
            cx.notify();
            return;
        }
        let confirmed = self
            .shared
            .owning(workspace_id)
            .and_then(|shared| shared.facts(workspace_id))
            .is_some_and(|facts| facts.confirmed);
        if let Some(shared) = self.shared.owning_mut(workspace_id) {
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
        self.close_empty_after_shared_removal |= confirmed;
        // The keyboard that was in the removed Workspace rests in the Workspace that takes its
        // place, without asking the server for a Pane there. Typing anywhere else, such as in
        // the sidebar, stays where it is.
        if !own && held_keyboard {
            self.focus(window, cx);
        }
    }

    fn reconcile_parked_shared_workspaces(
        &mut self,
        key: ServerKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parked: Vec<_> = self
            .workspaces
            .parked_ids()
            .filter(|id| self.shared.owner(*id) == Some(key))
            .collect();
        for id in parked {
            let Some(shared) = self.shared.get(key) else {
                continue;
            };
            let Some(inventory) = shared.latest_inventory() else {
                continue;
            };
            let exists = shared
                .facts(id)
                .is_some_and(|facts| inventory.workspaces.iter().any(|w| w.id == facts.server));
            if exists && shared.optimistically_closing(id) {
                continue;
            }
            if exists {
                let was_empty = self.workspaces.active_workspace_id().is_none();
                let restore_keyboard = was_empty && self.empty_focus.is_focused(window);
                if self.workspaces.restore_workspace(id).is_ok() && was_empty {
                    if let Some(manager) = self.active_tab_manager() {
                        manager.update(cx, |manager, cx| manager.activate_without_focus(cx));
                    }
                    if restore_keyboard {
                        self.focus(window, cx);
                    }
                }
            } else {
                if let Ok(manager) = self.workspaces.discard_parked_workspace(id) {
                    manager.update(cx, |manager, cx| manager.close_all(cx));
                }
                if let Some(shared) = self.shared.get_mut(key) {
                    shared.forget(id);
                }
                self.pending_final_tab_closes.remove(&id);
                self.close_empty_after_shared_removal = true;
            }
        }
    }

    /// The pin version a shared Workspace shows now, which its chooser keeps while it is open.
    fn shared_pin_version(&self, workspace_id: WorkspaceId) -> Option<u64> {
        self.shared.owning(workspace_id)?.pin_version(workspace_id)
    }

    fn shared_row_facts(&self, workspace_id: WorkspaceId) -> Option<SharedRowFacts> {
        let key = self.shared.owner(workspace_id)?;
        let shared = self.shared.get(key)?;
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
                status: shared.row_status(
                    facts.server,
                    self.shared_inventory(workspace_id)
                        .map(|inventory| inventory.main_root.as_path()),
                ),
                detaches: shared.attachments_changeable(),
            },
            directory: directory.to_owned(),
            pinned: facts.pinned(),
            machine: self.shared.origin(key).and_then(ServerOrigin::remote_name),
        })
    }

    /// Whether the account-local server, which creates this window's new Workspaces, is
    /// reachable.
    fn shared_reachable(&self) -> bool {
        self.shared.local().is_none_or(|shared| {
            super::shared_workspaces::SharedRefusalReason::from(shared.editability())
                != super::shared_workspaces::SharedRefusalReason::Offline
        })
    }

    /// The detached server Workspaces the Workspace Switcher offers after the presented ones.
    ///
    /// Only a connected server's detached Workspaces are offered, and each one attaches when
    /// chosen. None takes an activation Shortcut, because those follow the sidebar's order.
    fn detached_switcher_items(&self) -> Vec<ComboBoxItem<SwitcherChoice>> {
        self.shared
            .iter()
            .flat_map(|(key, shared)| self.detached_server_items(key, shared))
            .collect()
    }

    /// One server's detached Workspaces. Another computer's carry its server name, so two
    /// servers' Workspaces with one name stay distinguishable.
    fn detached_server_items(
        &self,
        key: ServerKey,
        shared: &super::shared_workspaces::SharedWorkspaces,
    ) -> Vec<ComboBoxItem<SwitcherChoice>> {
        let changeable = shared.attachments_changeable();
        let destination = self.shared.origin(key).and_then(ServerOrigin::remote_name);
        shared
            .available_workspaces()
            .into_iter()
            .filter(|available| available.local.is_none())
            .map(|available| {
                let state = if changeable {
                    DETACHED_DESCRIPTION
                } else {
                    DETACHED_UNAVAILABLE_DESCRIPTION
                };
                let description = match &destination {
                    Some(destination) => format!("{state} \u{b7} {destination}"),
                    None => state.to_owned(),
                };
                let keywords: Vec<String> = std::iter::once("Detached".to_owned())
                    .chain(destination.clone())
                    .collect();
                ComboBoxItem::new(
                    SwitcherChoice::Detached((key, available.server)),
                    available.name,
                )
                .description(description)
                .keywords(keywords)
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
        key: ServerKey,
        server: spaceterm_server::hierarchy::Id,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shared) = self.shared.get_mut(key) else {
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
            .get(key)
            .and_then(|shared| shared.latest_inventory())
            .cloned()
        {
            self.apply_shared_inventory(
                key,
                &inventory,
                super::shared_workspaces::SharedRemoval::Published,
                window,
                cx,
            );
        }
        let Some(workspace_id) = self.shared.get(key).and_then(|shared| shared.local(server))
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
        let Some(shared) = self.shared.owning_mut(workspace_id) else {
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
        if let Some(shared) = self.shared.owning_mut(workspace_id) {
            shared.forget(workspace_id);
        }
        self.pending_final_tab_closes.remove(&workspace_id);
        // Ends a pin chooser open for the Workspace; a shared Workspace has no remote runtime.
        self.end_pin_for_closing(workspace_id, window, cx);
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
                        active_manager
                            .update(cx, |manager, cx| manager.activate_in_place(window, cx));
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
const DETACHED_UNAVAILABLE_DESCRIPTION: &str = "Detached. Reopen SpaceTerm to attach.";

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
                "Couldn\u{2019}t Attach Workspace",
                "The Workspace keeps running.",
            ),
            (Self::Attach, Uncertain) => (
                "Attachment not confirmed",
                "Couldn\u{2019}t Confirm Attachment",
                "Quit and reopen SpaceTerm before you attach or detach more Workspaces. This Workspace keeps running.",
            ),
            (Self::Detach, Unchanged) => (
                "Workspace not detached",
                "Couldn\u{2019}t Detach Workspace",
                "The Workspace stays here.",
            ),
            (Self::Detach, Uncertain) => (
                "Detach not confirmed",
                "Couldn\u{2019}t Confirm Detach",
                "This Workspace might return when SpaceTerm reopens. Quit and reopen SpaceTerm before you attach or detach more Workspaces.",
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

/// Why a directory on this computer never becomes another computer's server Workspace pin.
const REMOTE_LOCAL_PIN_REFUSED: &str =
    "Choose a directory on the computer where this Workspace runs.";

impl Render for WorkspaceManager {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_sidebar_repositories(cx);
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
        // Without usable server state the launch screen takes the window, and nothing offers to
        // create or arrange Workspaces that cannot exist yet.
        let recovery = self.startup_recovery();
        let identity = self.active_chrome_identity(cx);
        let resting_chrome =
            WorkspaceChromeLayout::resolve(sidebar_layout, identity.as_ref(), window, cx);
        let chrome = WorkspaceChromeLayout::presented(
            sidebar_layout,
            sidebar_frame,
            identity.as_ref(),
            window,
            cx,
        );
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
                (!recovery).then_some(empty::EmptySidebar {
                    reveal: sidebar_frame.reveal,
                    // As the sidebar is drawn, from its leading edge.
                    edge: sidebar_frame.between(-sidebar_layout.width, gpui::px(0.0))
                        + sidebar_layout.width,
                    top_chrome_width: chrome.width,
                    controls_width: chrome.controls_width(),
                    controls_bottom: WorkspaceChromeLayout::controls_bottom(cx),
                }),
                update_control,
                close,
                window,
                cx,
            ),
        };
        let rows = self.sidebar_rows(cx);
        let remote_servers = self.remote_creation_servers(cx);

        let rows_changed = self
            .sidebar
            .update(cx, |sidebar, cx| sidebar.set_rows(rows, remote_servers, cx));
        // The New Worktree Command is available only where it can act, so its menu item disables
        // elsewhere.
        let actions = WorkspaceActions {
            new_worktree: self
                .workspaces
                .active_workspace_id()
                .is_some_and(|workspace_id| self.creates_worktrees(workspace_id, cx)),
            step_worktrees: self.steps_worktrees(cx),
            workspaces: !recovery,
        };
        let content = Self::workspace_action_scope(actions, cx)
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
            .when(!recovery, |root| {
                root.child(Self::render_top_left_chrome_surface(chrome, window, cx))
            })
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
            .when(!recovery && sidebar_frame.reveal > 0.0, |root| {
                root.child(self.render_sidebar(
                    sidebar_layout.width,
                    sidebar_frame,
                    rows_changed,
                    window,
                    cx,
                ))
            })
            .when(!recovery, |root| {
                root.child(
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
                .child(self.render_top_left_chrome(
                    chrome,
                    manager.clone(),
                    window,
                    cx,
                ))
            });
        let transients = Self::workspace_action_scope(actions, cx)
            .absolute()
            .inset_0()
            .children(self.pin_picker.iter().cloned())
            .children(self.active_directory_recovery(cx))
            .children(super::application_commands::overlay(window, cx));
        let presentation = ModalLayer::new(super::window_shell::render(content, window, cx))
            .transient(transients)
            .into_any_element();
        self.person_input_scope(presentation, cx)
    }
}

impl WorkspaceManager {
    /// The sidebar below the top chrome, sliding in from the window's leading edge as it shows.
    ///
    /// It is a cached view, so a Pane update does not redraw it. It draws uncached while
    /// assistive technology reads the window, because GPUI republishes no accessibility nodes
    /// from a reused view. It also draws uncached in a frame that changed its rows, because GPUI
    /// applies a notification issued while a frame renders only to the next frame.
    fn render_sidebar(
        &self,
        width: Pixels,
        frame: super::sidebar_motion::SidebarFrame,
        rows_changed: bool,
        window: &Window,
        cx: &App,
    ) -> impl IntoElement {
        let appearance = super::appearance::chrome(cx);
        let sidebar = if rows_changed || window.is_a11y_active() {
            self.sidebar.clone().into_any_element()
        } else {
            gpui::AnyView::from(self.sidebar.clone())
                .cached(gpui::StyleRefinement::default().size_full())
                .into_any_element()
        };
        div()
            .absolute()
            .top(
                super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx)
                    .top_chrome_height(appearance.top_height()),
            )
            .bottom_0()
            .left(frame.between(-width, gpui::px(0.0)))
            .w(width)
            .child(sidebar)
    }
}

/// The Workspace Commands the window can act on this frame. An unavailable Command has no route,
/// so its menu item disables.
#[derive(Clone, Copy)]
struct WorkspaceActions {
    new_worktree: bool,
    step_worktrees: bool,
    /// Switching, creating, and closing Workspaces, and the sidebar that lists them.
    workspaces: bool,
}

impl WorkspaceManager {
    /// Keeps ordinary content and complete transient owners on the same action routes while leaving
    /// the active modal outside those routes.
    fn workspace_action_scope(actions: WorkspaceActions, cx: &Context<Self>) -> gpui::Div {
        div()
            .key_context(TERMINAL_KEY_CONTEXT)
            .when(actions.new_worktree, |scope| {
                scope.on_action(cx.listener(Self::on_new_worktree))
            })
            .when(actions.step_worktrees, |scope| {
                scope
                    .on_action(cx.listener(Self::on_next_worktree))
                    .on_action(cx.listener(Self::on_previous_worktree))
            })
            .when(actions.workspaces, |scope| {
                scope
                    .on_action(cx.listener(Self::on_switch_workspace))
                    .on_action(cx.listener(Self::on_new_workspace))
                    .on_action(cx.listener(Self::on_open_local_directory))
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
            })
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

/// The collapsed title-bar identity a Workspace presents, before its Active Worktree.
fn chrome_identity<T>(workspace: &WorkspaceEntry<T>) -> WorkspaceChromeIdentity {
    WorkspaceChromeIdentity {
        name: workspace.name().to_owned(),
        worktree: None,
        pinned: workspace.pinned_directory().is_some(),
        status: None,
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

/// The compact directory a row shows and its full spelling for the tooltip.
fn directory_labels(
    local_directory: Option<&std::path::Path>,
    local_home: &std::path::Path,
) -> (String, String) {
    let directory = local_directory.unwrap_or(local_home);
    (
        compact_home_path(directory, local_home),
        directory.display().to_string(),
    )
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

/// Whether a shared close's Workspace held the keyboard when the person asked for the close.
#[derive(Default)]
struct SharedCloseKeyboard {
    /// The open confirmation's generation and its answer.
    pending: Option<(u64, bool)>,
    /// The answer a Review hands to the fresh request it makes.
    reviewed: Option<bool>,
}

impl SharedCloseKeyboard {
    /// Takes the answer for confirmation `generation`. Another generation's answer is false.
    fn take(&mut self, generation: u64) -> bool {
        match self.pending {
            Some((pending, held)) if pending == generation => {
                self.pending = None;
                held
            }
            _ => false,
        }
    }
}

/// The Workspace a close target belongs to.
const fn close_target_workspace(target: CloseTarget) -> Option<WorkspaceId> {
    match target {
        CloseTarget::Workspace(workspace_id)
        | CloseTarget::Tab { workspace_id, .. }
        | CloseTarget::Pane { workspace_id, .. }
        | CloseTarget::Worktree { workspace_id, .. } => Some(workspace_id),
        CloseTarget::Window => None,
    }
}

/// One creation row the Workspace Switcher offers.
struct SwitcherCreationRow {
    creation: WorkspaceCreation,
    label: gpui::SharedString,
    selector: String,
    shortcut: Option<gpui::SharedString>,
    starts_group: bool,
    available: bool,
}

/// The Workspace Switcher's creation rows: the local ones, then New Workspace and Open Directory
/// on each saved paired server.
fn switcher_creation_rows(
    servers: &[super::workspace_creation::RemoteCreationServer],
    presentation: &crate::desktop_profile::DesktopPresentation,
) -> Vec<SwitcherCreationRow> {
    let local = WorkspaceCreation::LOCAL
        .into_iter()
        .map(|creation| SwitcherCreationRow {
            creation,
            label: creation.label(None),
            selector: creation.selector(0),
            shortcut: creation.shortcut(presentation),
            starts_group: false,
            available: true,
        });
    let remote = servers.iter().enumerate().flat_map(|(position, server)| {
        [
            WorkspaceCreation::Remote(server.route),
            WorkspaceCreation::OpenRemoteDirectory(server.route),
        ]
        .into_iter()
        .map(move |creation| SwitcherCreationRow {
            creation,
            label: creation.label(Some(server.display_name().as_ref())),
            selector: creation.selector(position),
            shortcut: None,
            starts_group: position == 0 && matches!(creation, WorkspaceCreation::Remote(_)),
            available: server.available,
        })
    });
    local.chain(remote).collect()
}

mod empty;
mod launch;
#[cfg(test)]
pub(crate) use launch::testing as launch_testing;
pub(crate) use launch::{LocalServerAdapters, resume_waiting_launches, start_local_server};
#[path = "workspace_manager/repository.rs"]
mod repository;
mod server_bindings;
use server_bindings::ServerBindings;
pub(crate) use server_bindings::{ServerKey, ServerOrigin};
mod desktop_requests;
pub(crate) use desktop_requests::serve as serve_desktop_requests;
mod direct_servers;
mod display_order;
mod remembered;
mod shared_repositories;
mod worktrees;

#[cfg(test)]
#[path = "workspace_manager/tests.rs"]
pub(crate) mod tests;
