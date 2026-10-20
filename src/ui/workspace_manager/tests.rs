use crate::directory_selection::GpuiDirectorySelection;
use crate::domain::RemoteConnectionPhase;
use crate::ssh::remote_account::RemoteWorkspaceAccount;
use crate::terminal::GpuiTerminalKeyInputAdapterFactory;
use crate::ui::drag_and_drop::finish_slides;
use crate::ui::workspace_frame::WorkspaceFrame;
use crate::ui::workspace_sidebar::SIDEBAR_ROW_SELECTION_INSET_Y;
use crate::ui::{
    TOP_CHROME_HEIGHT, WORKSPACE_SIDEBAR_DEFAULT_WIDTH, WORKSPACE_SIDEBAR_MINIMUM_WIDTH,
};
use gpui::MouseButton;
use gpui::px;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    DivInspectorState, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, MouseDownEvent,
    MouseUpEvent, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, VisualTestContext,
    point,
};
use spaceterm_ui::{
    Alert, Dialog, DialogCloseDecision, DialogInitialFocus, ModalAction, ModalActionRole, ModalId,
    ModalPresentationHandle, TextDirection,
};

use super::*;
use crate::domain::PresentedWorkspace;
use crate::domain::{PaneId, TabId};

fn workspace_frame(cx: &mut VisualTestContext) -> WorkspaceFrame {
    cx.update(|_, cx| WorkspaceFrame::for_appearance(crate::ui::appearance::chrome(cx), cx))
}

fn frame_space(cx: &mut VisualTestContext) -> Pixels {
    workspace_frame(cx).space()
}

/// The height of the top chrome, which carries the frame's top space in its own height.
fn top_chrome_height(cx: &mut VisualTestContext) -> Pixels {
    cx.update(|_, cx| {
        let appearance = crate::ui::appearance::chrome(cx);
        WorkspaceFrame::for_appearance(appearance, cx).top_chrome_height(appearance.top_height())
    })
}

/// The air between a row's content and the row's trailing edge.
fn row_trailing_padding(cx: &mut VisualTestContext) -> Pixels {
    cx.update(|_, cx| {
        let appearance = crate::ui::appearance::chrome(cx);
        WorkspaceFrame::for_appearance(appearance, cx).sidebar_chip_trailing_inset()
            + appearance.spacing(crate::ui::workspace_sidebar::SIDEBAR_ROW_CHIP_PADDING)
    })
}

#[gpui::test]
fn sidebar_should_follow_the_local_root_in_background_and_promote_on_close(
    cx: &mut TestAppContext,
) {
    use crate::domain::CurrentDirectory;
    let (manager, records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-d");
    cx.run_until_parked();
    assert_eq!(records.starts().len(), 2);
    records.report_directory(
        2,
        Some(CurrentDirectory::Local(PathBuf::from("/other/child"))),
    );
    records.report_directory(
        1,
        Some(CurrentDirectory::Local(PathBuf::from("/projects/api"))),
    );
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active()
            .name()
            .to_owned()),
        "api"
    );

    new_desktop_workspace(cx);
    cx.run_until_parked();
    records.report_directory(
        1,
        Some(CurrentDirectory::Local(PathBuf::from("/projects/server"))),
    );
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .workspace(WorkspaceId::new(1))
            .unwrap()
            .name()
            .to_owned()),
        "server"
    );
    records.report_directory(1, None);
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .workspace(WorkspaceId::new(1))
            .unwrap()
            .name()
            .to_owned()),
        "server"
    );

    records
        .event_sender(1)
        .unwrap()
        .try_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success))
        .unwrap();
    cx.run_until_parked();
    let identity = manager.read_with(cx, |manager, _| {
        let workspace = manager.workspaces.workspace(WorkspaceId::new(1)).unwrap();
        (
            workspace.name().to_owned(),
            workspace.local_display_directory().unwrap().to_path_buf(),
        )
    });
    assert_eq!(
        identity,
        ("child".to_owned(), PathBuf::from("/other/child"))
    );
}

#[gpui::test]
fn sidebar_should_follow_remote_root_across_tabs_pins_and_custom_names(cx: &mut TestAppContext) {
    use crate::domain::CurrentDirectory;
    let (manager, records, cx) = workspace_manager(cx);
    let (completion, _, _, _) = remote_completion("deploy@staging", "~/", "/home/tester", true);
    click_new_workspace_menu("new-workspace-menu-create-remote", cx);
    let flow = manager.read_with(cx, |manager, _| {
        manager.remote_workspace_flow.clone().unwrap()
    });
    emit_remote_workspace_completion(&flow, completion, cx);
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    assert_eq!(records.starts().len(), 3);
    records.report_directory(
        3,
        Some(CurrentDirectory::Remote(
            RemoteDirectory::new("/srv/other".into()).unwrap(),
        )),
    );
    records.report_directory(
        2,
        Some(CurrentDirectory::Remote(
            RemoteDirectory::new("/srv/api".into()).unwrap(),
        )),
    );
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active()
            .name()
            .to_owned()),
        "api"
    );
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.apply_directory_pin(
                WorkspaceId::new(2),
                Some(PinnedDirectory::Remote {
                    directory: RemoteDirectory::new("/srv/pinned".into()).unwrap(),
                    identity: crate::domain::RemoteDirectoryIdentity::new("/srv/pinned".into())
                        .unwrap(),
                }),
                window,
                cx,
            )
        })
    });
    records.report_directory(
        2,
        Some(CurrentDirectory::Remote(
            RemoteDirectory::new("/srv/latest".into()).unwrap(),
        )),
    );
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active()
            .name()
            .to_owned()),
        "pinned"
    );
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.apply_directory_pin(WorkspaceId::new(2), None, window, cx)
        })
    });
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active()
            .name()
            .to_owned()),
        "latest"
    );

    manager.update(cx, |manager, _| {
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(2), "Release".into())
            .unwrap()
    });
    records
        .event_sender(2)
        .unwrap()
        .try_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success))
        .unwrap();
    cx.run_until_parked();
    let identity = manager.read_with(cx, |manager, _| {
        let workspace = manager.workspaces.active();
        (
            workspace.name().to_owned(),
            workspace
                .remote_display_directory()
                .unwrap()
                .as_str()
                .to_owned(),
        )
    });
    assert_eq!(identity, ("Release".to_owned(), "/srv/other".to_owned()));
}

#[gpui::test]
fn sidebar_rows_should_keep_the_directory_and_pin_below_name_and_leave_the_machine_to_its_heading(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    click_new_workspace_menu("new-workspace-menu-create-remote", cx);
    let flow = manager.read_with(cx, |manager, _| {
        manager.remote_workspace_flow.clone().unwrap()
    });
    let (completion, _, _, _) =
        remote_completion("deploy@staging-production", "~/", "/home/tester", true);
    emit_remote_workspace_completion(&flow, completion, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager
                .workspaces
                .rename_workspace(WorkspaceId::new(2), "Production".into())
                .unwrap();
            manager.apply_directory_pin(
                WorkspaceId::new(2),
                Some(PinnedDirectory::Remote {
                    directory: RemoteDirectory::new("/srv/very/long/path/to/application".into())
                        .unwrap(),
                    identity: crate::domain::RemoteDirectoryIdentity::new(
                        "/srv/very/long/path/to/application".into(),
                    )
                    .unwrap(),
                }),
                window,
                cx,
            );
            manager.set_sidebar_layout(true, px(420.0), window, cx);
        })
    });
    cx.run_until_parked();
    let name = cx.debug_bounds("workspace-row-name-2").unwrap();
    let path = cx.debug_bounds("workspace-row-path-2").unwrap();
    let pin = cx.debug_bounds("workspace-row-pin-2").unwrap();
    // The machine's heading names it, so its row doesn't repeat it.
    assert!(
        cx.debug_bounds("machine-heading-deploy@staging-production")
            .is_some()
    );
    assert!(cx.debug_bounds("workspace-machine-2").is_none());
    assert!(path.top() >= name.bottom());
    assert!(pin.right() <= path.left());
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager
                .workspaces
                .rename_workspace(
                    WorkspaceId::new(2),
                    "A very long production workspace name".into(),
                )
                .unwrap();
            manager.set_sidebar_layout(true, px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH), window, cx);
        })
    });
    cx.run_until_parked();
    let row = cx.debug_bounds("workspace-row-2-active").unwrap();
    let name = cx.debug_bounds("workspace-row-name-2").unwrap();
    // Canvas children removed this frame can leave historical GPUI debug bounds behind.
    // The name occupying all available width verifies the machine and its gap are gone.
    assert_eq!(name.right(), row.right() - row_trailing_padding(cx));
    let path = cx.debug_bounds("workspace-row-path-2").unwrap();
    let pin = cx.debug_bounds("workspace-row-pin-2").unwrap();
    assert!(path.right() <= row.right());
    assert!(pin.right() <= path.left());
    assert_eq!(row.size.height, px(SIDEBAR_ROW_HEIGHT));
}

#[gpui::test]
fn sidebar_rows_end_with_the_branch_and_let_a_pull_request_open_in_the_browser(
    cx: &mut TestAppContext,
) {
    use crate::repository_status::{
        ChangeState, Freshness, PullRequest, RepositoryHead, RepositoryKey, RepositoryMachine,
        RepositoryRoot, RepositoryStatus, RepositoryView,
    };

    let (manager, _records, cx) = workspace_manager(cx);
    let status = |pull_request: Option<PullRequest>| {
        RepositoryView::Repository(Arc::new(RepositoryStatus {
            key: RepositoryKey {
                machine: RepositoryMachine::Local,
                root: RepositoryRoot::Local(PathBuf::from("/projects/api")),
            },
            head: RepositoryHead::Branch("main".into()),
            commit: Some("a1b2c3d".into()),
            upstream: None,
            operation: None,
            changes: ChangeState::NotCounted,
            freshness: Freshness::Current,
            read_at: Instant::now(),
            pull_request,
            read_failure: None,
        }))
    };
    manager.update(cx, |manager, cx| {
        manager.present_sidebar_repository(WorkspaceId::new(1), status(None), cx);
    });
    cx.run_until_parked();
    let branch = cx.debug_bounds("workspace-row-branch-1").unwrap();
    let path = cx.debug_bounds("workspace-row-path-1").unwrap();
    let row = cx.debug_bounds("workspace-row-1-active").unwrap();
    // The directory leads line 2 and the branch ends it.
    assert!(path.left() < branch.left());
    assert!(path.right() <= branch.left());
    assert_eq!(branch.right(), row.right() - row_trailing_padding(cx));
    assert!(cx.debug_bounds("workspace-counts-1").is_none());

    manager.update(cx, |manager, cx| {
        manager.present_sidebar_repository(
            WorkspaceId::new(1),
            status(Some(PullRequest {
                number: 478,
                title: "Add Repository Status".into(),
                draft: true,
                url: "https://github.com/acme/api/pull/478".into(),
                head: "main".into(),
                base: "release".into(),
            })),
            cx,
        );
    });
    cx.run_until_parked();
    let number = cx
        .debug_bounds("workspace-row-pull-request-1")
        .expect("the Pull Request number replaces the branch");
    let path = cx.debug_bounds("workspace-row-path-1").unwrap();
    assert!(path.right() <= number.left());
    assert_eq!(number.right(), row.right() - row_trailing_padding(cx));
    let active = manager.read_with(cx, |manager, _| manager.workspaces.active_id());
    cx.simulate_click(number.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://github.com/acme/api/pull/478")
    );
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_id()),
        active
    );
}

#[gpui::test]
fn sidebar_rows_contain_semantic_text_in_both_densities(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);

    for density in [
        crate::appearance::ChromeDensity::Compact,
        crate::appearance::ChromeDensity::Comfortable,
    ] {
        let mut preferences = crate::appearance::AppearancePreferences::default();

        preferences.window.density = density;
        let resolved = crate::appearance::ThemeCatalog::default()
            .resolve(
                crate::appearance::AppearanceGeneration::INITIAL,
                &preferences,
                crate::appearance::SystemAppearance::unavailable()
                    .with_composition(crate::appearance::CompositionCapabilities::new(true, true)),
                &crate::appearance::AvailableFonts::default(),
            )
            .expect("built-in appearance should resolve");
        let (active, inactive) =
            crate::ui::appearance::ChromeAppearance::prepare_variants(&resolved.chrome);
        cx.update(|window, cx| {
            cx.set_global(crate::ui::appearance::InstalledChrome {
                active: Arc::new(active),
                inactive: Arc::new(inactive),
            });
            window.refresh();
        });
        cx.run_until_parked();

        for id in [1, 2] {
            let row_selector: &'static str = format!(
                "workspace-row-{id}-{}",
                if id == 2 { "active" } else { "inactive" }
            )
            .leak();
            let name_selector: &'static str = format!("workspace-row-name-{id}").leak();
            let detail_selector: &'static str = format!("workspace-row-path-{id}").leak();
            let row = cx.debug_bounds(row_selector).expect("Workspace row");
            let name = cx.debug_bounds(name_selector).expect("Workspace name");
            let detail = cx.debug_bounds(detail_selector).expect("Workspace detail");
            assert!(
                name.top() >= row.top() && detail.bottom() <= row.bottom(),
                "{density:?} row {id} must contain its semantic line boxes: row={row:?}, name={name:?}, detail={detail:?}"
            );
        }
        let first = cx
            .debug_bounds("workspace-row-1-inactive")
            .expect("first Workspace row");
        let second = cx
            .debug_bounds("workspace-row-2-active")
            .expect("second Workspace row");
        assert!(
            first.bottom() <= second.top(),
            "{density:?} adjacent Workspace targets must not overlap: first={first:?}, second={second:?}"
        );
    }
}

#[test]
fn remote_home_labels_should_ignore_trailing_separators_without_changing_tooltip_paths() {
    let location = WorkspaceLocation::Remote {
        key: RemoteWorkspaceTarget::new(
            crate::domain::SshDestination::new("build".into()).unwrap(),
            crate::domain::RemoteDirectoryIdentity::new("/home/tester".into()).unwrap(),
        ),
        remote_user: crate::domain::RemoteUser::new("tester".into()).unwrap(),
        remote_directory: RemoteDirectory::new("~/".into()).unwrap(),
        remote_home_identity: crate::domain::RemoteDirectoryIdentity::new("/home/tester".into())
            .unwrap(),
        connection_state: RemoteConnectionState::connected(1),
    };
    for (path, expected) in [
        ("/home/tester/", "~"),
        ("/home/tester///", "~"),
        ("~///", "~"),
        ("/home/tester/src/", "~/src/"),
        ("/home/tester-other/", "/home/tester-other/"),
        ("/", "/"),
    ] {
        let directory = RemoteDirectory::new(path.into()).unwrap();
        assert_eq!(
            directory_labels(&location, None, Some(&directory), Path::new("/Users/local")),
            (expected.to_owned(), format!("tester@build:{path}")),
        );
    }
}

#[test]
fn remote_directory_labels_should_include_account_destination_and_compact_home() {
    let location = WorkspaceLocation::Remote {
        key: RemoteWorkspaceTarget::new(
            crate::domain::SshDestination::new("build-01".into()).unwrap(),
            crate::domain::RemoteDirectoryIdentity::new("/home/tester/src".into()).unwrap(),
        ),
        remote_user: crate::domain::RemoteUser::new("tester".into()).unwrap(),
        remote_directory: RemoteDirectory::new("/home/tester/src".into()).unwrap(),
        remote_home_identity: crate::domain::RemoteDirectoryIdentity::new("/home/tester".into())
            .unwrap(),
        connection_state: RemoteConnectionState::connected(1),
    };
    let directory = RemoteDirectory::new("/home/tester/src".into()).unwrap();

    assert_eq!(
        directory_labels(&location, None, Some(&directory), Path::new("/Users/local")),
        (
            "~/src".to_owned(),
            "tester@build-01:/home/tester/src".to_owned(),
        )
    );
}

#[test]
fn remote_directory_labels_should_preserve_an_explicit_destination_user() {
    let location = WorkspaceLocation::Remote {
        key: RemoteWorkspaceTarget::new(
            crate::domain::SshDestination::new("admin@build-01".into()).unwrap(),
            crate::domain::RemoteDirectoryIdentity::new("/srv/project".into()).unwrap(),
        ),
        remote_user: crate::domain::RemoteUser::new("tester".into()).unwrap(),
        remote_directory: RemoteDirectory::new("/srv/project".into()).unwrap(),
        remote_home_identity: crate::domain::RemoteDirectoryIdentity::new("/home/tester".into())
            .unwrap(),
        connection_state: RemoteConnectionState::connected(1),
    };
    let directory = RemoteDirectory::new("/srv/project".into()).unwrap();

    assert_eq!(
        directory_labels(&location, None, Some(&directory), Path::new("/Users/local")),
        (
            "/srv/project".to_owned(),
            "admin@build-01:/srv/project".to_owned(),
        )
    );
}

#[test]
fn sidebar_toggle_should_describe_the_action_for_each_visibility_state() {
    let (visible_icon, visible_label) = sidebar_toggle_presentation(true);
    assert_eq!(
        (visible_icon, visible_label),
        (CustomIconName::PanelLeft, "Hide Sidebar")
    );
    let (hidden_icon, hidden_label) = sidebar_toggle_presentation(false);
    assert_eq!(
        (hidden_icon, hidden_label),
        (CustomIconName::PanelRight, "Show Sidebar")
    );
}

#[test]
fn remote_connection_phases_have_expected_status_labels() {
    assert_eq!(
        remote_connection_status(RemoteConnectionPhase::Connected),
        None
    );
    assert_eq!(
        remote_connection_status(RemoteConnectionPhase::Reconnecting),
        Some("Reconnecting…")
    );
    assert_eq!(
        remote_connection_status(RemoteConnectionPhase::Disconnected),
        Some("Disconnected")
    );
    assert_eq!(
        remote_connection_status(RemoteConnectionPhase::Failed),
        Some("Connection failed")
    );
    assert_eq!(
        remote_connection_status(RemoteConnectionPhase::Closing),
        Some("Closing…")
    );
}

type RemoteCompletionFixture = (
    RemoteWorkspaceFlowCompletion,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<AtomicBool>,
    async_channel::Sender<crate::ssh::live_connection::ControlConnectionTerminalState>,
);
use crate::directory_selection::ScriptedDirectorySelection;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::ssh::askpass::prompt::{AskPassPromptKind, AskPassRequest, AskPassResult};
use crate::ssh::command::ValidatedRemoteShellCommand;
use crate::ssh::destination::SshHostAlias;
use crate::ssh::host_config::HostDiscovery;
use crate::ssh::managed_hosts::ManagedSshHost;
use crate::terminal::testing::{
    RecordedCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
};
use crate::terminal::{TerminalSessionEvent, TerminalSessionExit};
use crate::ui::askpass_dialog::GpuiAskPassPresenter;
use crate::ui::directory_picker::{
    DirectoryListing, ExactPathState, RemoteDirectoryProvider, RemoteDirectoryProviderError,
};
use crate::ui::remote_workspace_flow::{
    ControlConnectionOwner, RemoteWorkspaceAliasPin, RemoteWorkspaceAliasPinError,
    RemoteWorkspaceConnectContext, RemoteWorkspaceFlowBackendError, RemoteWorkspaceFlowStage,
};
use crate::ui::ssh_host_form::ManagedHostFormBackendError;

#[derive(Default)]
struct TestRemoteWorkspaceFlowBackend {
    connections: Mutex<
        VecDeque<gpui::Task<Result<ConnectedControlConnection, RemoteWorkspaceFlowBackendError>>>,
    >,
    connect_calls: AtomicUsize,
}

impl TestRemoteWorkspaceFlowBackend {
    fn with_connections(
        connections: impl IntoIterator<
            Item = gpui::Task<Result<ConnectedControlConnection, RemoteWorkspaceFlowBackendError>>,
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

    fn save_managed_host(
        &self,
        _: ManagedSshHost,
    ) -> gpui::Task<Result<(), ManagedHostFormBackendError>> {
        gpui::Task::ready(Ok(()))
    }

    fn connect(
        &self,
        _: crate::domain::SshDestination,
        _: RemoteWorkspaceConnectContext,
    ) -> gpui::Task<Result<ConnectedControlConnection, RemoteWorkspaceFlowBackendError>> {
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
    identity: Result<crate::domain::RemoteDirectoryIdentity, RemoteDirectoryProviderError>,
}

impl TestRemoteProvider {
    fn failing() -> Self {
        Self {
            account_available: false,
            identity: Err(RemoteDirectoryProviderError::Other),
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
            identity: Err(RemoteDirectoryProviderError::PermissionDenied),
        }
    }
}

impl RemoteDirectoryProvider for TestRemoteProvider {
    fn discover_account(
        &self,
    ) -> gpui::Task<Result<RemoteWorkspaceAccount, RemoteDirectoryProviderError>> {
        if !self.account_available {
            return gpui::Task::ready(Err(RemoteDirectoryProviderError::Other));
        }
        gpui::Task::ready(Ok(test_remote_account()))
    }

    fn list_directories(
        &self,
        _: RemoteDirectory,
    ) -> gpui::Task<Result<DirectoryListing, RemoteDirectoryProviderError>> {
        gpui::Task::ready(Ok(DirectoryListing::new(Vec::new())))
    }

    fn probe_exact_path(
        &self,
        _: RemoteDirectory,
    ) -> gpui::Task<Result<ExactPathState, RemoteDirectoryProviderError>> {
        gpui::Task::ready(Ok(ExactPathState::ReadableDirectory))
    }

    fn create_directory_recursively(
        &self,
        _: RemoteDirectory,
    ) -> gpui::Task<Result<(), RemoteDirectoryProviderError>> {
        gpui::Task::ready(Err(RemoteDirectoryProviderError::Other))
    }

    fn validate_physical_identity(
        &self,
        _: RemoteDirectory,
    ) -> gpui::Task<Result<crate::domain::RemoteDirectoryIdentity, RemoteDirectoryProviderError>>
    {
        gpui::Task::ready(self.identity.clone())
    }
}

struct BlockingRemoteProvider {
    account:
        Mutex<Option<gpui::Task<Result<RemoteWorkspaceAccount, RemoteDirectoryProviderError>>>>,
    identity: Mutex<
        Option<
            gpui::Task<
                Result<crate::domain::RemoteDirectoryIdentity, RemoteDirectoryProviderError>,
            >,
        >,
    >,
    account_calls: Arc<AtomicUsize>,
    identity_calls: Arc<AtomicUsize>,
}

impl RemoteDirectoryProvider for BlockingRemoteProvider {
    fn discover_account(
        &self,
    ) -> gpui::Task<Result<RemoteWorkspaceAccount, RemoteDirectoryProviderError>> {
        self.account_calls.fetch_add(1, Ordering::AcqRel);
        self.account
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| gpui::Task::ready(Err(RemoteDirectoryProviderError::Other)))
    }

    fn list_directories(
        &self,
        _: RemoteDirectory,
    ) -> gpui::Task<Result<DirectoryListing, RemoteDirectoryProviderError>> {
        gpui::Task::ready(Err(RemoteDirectoryProviderError::Other))
    }

    fn probe_exact_path(
        &self,
        _: RemoteDirectory,
    ) -> gpui::Task<Result<ExactPathState, RemoteDirectoryProviderError>> {
        gpui::Task::ready(Err(RemoteDirectoryProviderError::Other))
    }

    fn create_directory_recursively(
        &self,
        _: RemoteDirectory,
    ) -> gpui::Task<Result<(), RemoteDirectoryProviderError>> {
        gpui::Task::ready(Err(RemoteDirectoryProviderError::Other))
    }

    fn validate_physical_identity(
        &self,
        _: RemoteDirectory,
    ) -> gpui::Task<Result<crate::domain::RemoteDirectoryIdentity, RemoteDirectoryProviderError>>
    {
        self.identity_calls.fetch_add(1, Ordering::AcqRel);
        self.identity
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| gpui::Task::ready(Err(RemoteDirectoryProviderError::Other)))
    }
}

fn test_remote_account() -> RemoteWorkspaceAccount {
    RemoteWorkspaceAccount::from_validated_login_shell(
        "tester".to_owned(),
        crate::domain::RemoteDirectoryIdentity::new("/home/tester".to_owned()).unwrap(),
        crate::ssh::command::ValidatedRemoteLoginShell::new("/bin/zsh".to_owned()).unwrap(),
    )
    .unwrap()
}

struct TestTerminalSessionChannelProvider {
    connection: crate::ssh::testing::SshConnectionFixture,
    preparations: Arc<AtomicUsize>,
    revalidations: Arc<AtomicUsize>,
    revalidation_tasks: Mutex<
        VecDeque<gpui::Task<Result<(), crate::terminal::TerminalSessionChannelRevalidationError>>>,
    >,
    available: Arc<AtomicBool>,
}

impl crate::terminal::TerminalSessionChannelProvider for TestTerminalSessionChannelProvider {
    fn is_ready(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }

    fn revalidate(
        &self,
        _: RemoteDirectory,
        _: Option<crate::domain::RemoteDirectoryIdentity>,
    ) -> gpui::Task<Result<(), crate::terminal::TerminalSessionChannelRevalidationError>> {
        self.revalidations.fetch_add(1, Ordering::AcqRel);
        self.revalidation_tasks
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| gpui::Task::ready(Ok(())))
    }

    fn prepare(
        &self,
        _: &RemoteDirectory,
    ) -> Result<
        crate::ssh::command::PreparedSshTerminalSessionChannelCommand,
        crate::terminal::TerminalSessionChannelUnavailable,
    > {
        self.preparations.fetch_add(1, Ordering::AcqRel);
        if !self.available.load(Ordering::Acquire) {
            return Err(crate::terminal::TerminalSessionChannelUnavailable);
        }
        Ok(self.connection.prepare_terminal_session_channel(
            ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
        ))
    }
}

struct TestControlConnectionOwner {
    closes: Arc<AtomicUsize>,
    channels: Arc<dyn crate::terminal::TerminalSessionChannelProvider>,
    lifecycle: Option<crate::ssh::live_connection::ControlConnectionObserver>,
    _lifecycle_senders:
        Vec<async_channel::Sender<crate::ssh::live_connection::ControlConnectionTerminalState>>,
    alias: Option<crate::ssh::alias_usage::ActiveSshAliasLease>,
    alias_pin_error: bool,
}

impl ControlConnectionOwner for TestControlConnectionOwner {
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

    fn bind_terminal_session_channels(
        &self,
        _: &crate::ssh::command::ValidatedRemoteLoginShell,
    ) -> Result<
        Arc<dyn crate::terminal::TerminalSessionChannelProvider>,
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
    revalidation: gpui::Task<Result<(), crate::terminal::TerminalSessionChannelRevalidationError>>,
) -> RemoteCompletionFixture {
    remote_completion_with_provider(
        destination,
        directory,
        physical,
        available,
        revalidation,
        Arc::new(TestRemoteProvider::failing()),
    )
}

fn remote_completion_with_provider(
    destination: &str,
    directory: &str,
    physical: &str,
    available: bool,
    revalidation: gpui::Task<Result<(), crate::terminal::TerminalSessionChannelRevalidationError>>,
    provider: Arc<dyn RemoteDirectoryProvider>,
) -> RemoteCompletionFixture {
    let destination = crate::domain::SshDestination::new(destination.to_owned()).unwrap();
    let directory = RemoteDirectory::new(directory.to_owned()).unwrap();
    let physical = crate::domain::RemoteDirectoryIdentity::new(physical.to_owned()).unwrap();
    let preparations = Arc::new(AtomicUsize::new(0));
    let revalidations = Arc::new(AtomicUsize::new(0));
    let availability = Arc::new(AtomicBool::new(available));
    let channels: Arc<dyn crate::terminal::TerminalSessionChannelProvider> =
        Arc::new(TestTerminalSessionChannelProvider {
            connection: crate::ssh::testing::SshConnectionFixture::new(destination.clone()),
            preparations: Arc::clone(&preparations),
            revalidations: Arc::clone(&revalidations),
            revalidation_tasks: Mutex::new(VecDeque::from([revalidation])),
            available: Arc::clone(&availability),
        });
    let closes = Arc::new(AtomicUsize::new(0));
    let (runtime_lifecycle_sender, runtime_lifecycle) =
        crate::ssh::live_connection::ControlConnectionObserver::controlled();
    let (control_connection_lifecycle_sender, control_connection_lifecycle) =
        crate::ssh::live_connection::ControlConnectionObserver::controlled();
    let control_connection = ConnectedControlConnection::new(
        Box::new(TestControlConnectionOwner {
            closes: Arc::clone(&closes),
            channels: Arc::clone(&channels),
            lifecycle: Some(control_connection_lifecycle),
            _lifecycle_senders: vec![
                runtime_lifecycle_sender.clone(),
                control_connection_lifecycle_sender,
            ],
            alias: None,
            alias_pin_error: false,
        }),
        provider,
    );
    let account = test_remote_account();
    (
        crate::ui::remote_workspace_flow::completion(
            control_connection,
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
    let directory = RemoteDirectory::new("~/src".to_owned()).unwrap();
    let physical =
        crate::domain::RemoteDirectoryIdentity::new("/home/tester/src".to_owned()).unwrap();
    let preparations = Arc::new(AtomicUsize::new(0));
    let revalidations = Arc::new(AtomicUsize::new(0));
    let channels: Arc<dyn crate::terminal::TerminalSessionChannelProvider> =
        Arc::new(TestTerminalSessionChannelProvider {
            connection: crate::ssh::testing::SshConnectionFixture::new(destination.clone()),
            preparations,
            revalidations,
            revalidation_tasks: Mutex::new(VecDeque::from([gpui::Task::ready(Ok(()))])),
            available: Arc::new(AtomicBool::new(true)),
        });
    let closes = Arc::new(AtomicUsize::new(0));
    let (runtime_lifecycle_sender, runtime_lifecycle) =
        crate::ssh::live_connection::ControlConnectionObserver::controlled();
    let (control_connection_lifecycle_sender, control_connection_lifecycle) =
        crate::ssh::live_connection::ControlConnectionObserver::controlled();
    let control_connection = ConnectedControlConnection::new(
        Box::new(TestControlConnectionOwner {
            closes: Arc::clone(&closes),
            channels: Arc::clone(&channels),
            lifecycle: Some(control_connection_lifecycle),
            _lifecycle_senders: vec![
                runtime_lifecycle_sender.clone(),
                control_connection_lifecycle_sender,
            ],
            alias: Some(lease),
            alias_pin_error,
        }),
        Arc::new(TestRemoteProvider::failing()),
    );
    let account = test_remote_account();
    (
        crate::ui::remote_workspace_flow::completion(
            control_connection,
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

#[gpui::test]
fn native_service_factory_reaches_initial_and_new_hierarchy(cx: &mut TestAppContext) {
    use crate::terminal::native_services::file_preview::{FilePreviewFactory, FilePreviewPanel};

    struct CountingFilePreviewFactory {
        created: Rc<std::cell::Cell<usize>>,
        delegate: Rc<dyn FilePreviewFactory>,
    }

    impl FilePreviewFactory for CountingFilePreviewFactory {
        fn create(&self) -> Box<dyn FilePreviewPanel> {
            self.created.set(self.created.get() + 1);
            self.delegate.create()
        }
    }

    cx.update(crate::ui::init).unwrap();
    let created = Rc::new(std::cell::Cell::new(0));
    let mut native_services = crate::terminal::native_services::testing::adapters();
    native_services.file_preview = Rc::new(CountingFilePreviewFactory {
        created: Rc::clone(&created),
        delegate: Rc::clone(&native_services.file_preview),
    });
    let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(TestTerminalSessionFactory::new(
        TestTerminalSessionRecords::default(),
    ));
    let (manager, cx) = cx.add_window_view(|window, cx| {
        new_desktop_manager(
            session_factory,
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                native_services,
                ..workspace_adapters(test_remote_backend_factory())
            },
            SidebarState::Expanded,
            window,
            cx,
        )
    });
    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.focus(window, cx)));
    cx.run_until_parked();
    assert_eq!(created.get(), 1);
    for (shortcut, expected) in [("cmd-d", 2), ("cmd-t", 3)] {
        cx.simulate_keystrokes(shortcut);
        cx.run_until_parked();
        assert_eq!(created.get(), expected, "{shortcut}");
    }
    new_desktop_workspace(cx);
    assert_eq!(created.get(), 4, "new Workspace");
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            let workspace_id = manager.workspaces.active_id();
            manager.close_workspace(workspace_id, window, cx);
            let final_workspace_id = manager.workspaces.active_id();
            manager.close_workspace(final_workspace_id, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(created.get(), 4, "nothing replaces the final Workspace");
}

#[gpui::test]
fn accessibility_factory_reaches_initial_and_new_workspaces_tabs_and_split_panes(
    cx: &mut TestAppContext,
) {
    cx.update(crate::ui::init).unwrap();
    let factory = Rc::new(
        crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default(),
    );
    let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(TestTerminalSessionFactory::new(
        TestTerminalSessionRecords::default(),
    ));
    let (manager, cx) = cx.add_window_view(|window, cx| {
        new_desktop_manager(
            session_factory,
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                accessibility: factory.clone(),
                ..workspace_adapters(test_remote_backend_factory())
            },
            SidebarState::Expanded,
            window,
            cx,
        )
    });
    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.focus(window, cx)));
    cx.run_until_parked();
    assert_eq!(factory.records.borrow().len(), 1);
    for (shortcut, expected) in [("cmd-d", 2), ("cmd-t", 3)] {
        cx.simulate_keystrokes(shortcut);
        cx.run_until_parked();
        assert_eq!(factory.records.borrow().len(), expected, "{shortcut}");
    }
    new_desktop_workspace(cx);
    assert_eq!(factory.records.borrow().len(), 4, "new Workspace");
    let records = factory.records.borrow();
    assert!(
        records[0]
            .borrow()
            .hierarchy
            .iter()
            .any(|presented| !presented)
    );
    assert!(
        records[3]
            .borrow()
            .hierarchy
            .iter()
            .any(|presented| *presented)
    );
}

fn workspace_adapters(
    remote_workspace: Arc<dyn RemoteWorkspaceFlowBackendFactory>,
) -> WorkspaceManagerAdapters {
    WorkspaceManagerAdapters {
        local_filesystem: LocalFilesystemAuthority::testing(),
        key_input: Rc::new(GpuiTerminalKeyInputAdapterFactory::default()),
        accessibility: Rc::new(
            crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default(),
        ),
        native_services: crate::terminal::native_services::testing::adapters(),
        lifecycle: PaneLifecycleDependencies::testing(),
        directory_selection: Rc::new(GpuiDirectorySelection),
        window_drag: Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
        remote_workspace,
        local_server: crate::ui::launch_testing::unanswered(),
    }
}

/// Adds and activates one more desktop-owned Workspace in the window under test, as a person
/// opening another Workspace does.
fn new_desktop_workspace(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        let manager = window
            .root::<WorkspaceManager>()
            .flatten()
            .expect("the window presents a Workspace manager");
        manager.update(cx, |manager, cx| {
            manager.present_desktop_workspace(window, cx);
        });
    });
    cx.run_until_parked();
}

/// A Workspace Window presenting one desktop-owned Workspace at `home_directory`.
///
/// Production desktop-owned Workspaces are Remote Workspaces; these tests exercise the window's
/// own behavior around any desktop-owned Workspace without opening an SSH connection.
fn new_desktop_manager(
    session_factory: Rc<dyn TerminalSessionFactory>,
    home_directory: PathBuf,
    adapters: WorkspaceManagerAdapters,
    sidebar: SidebarState,
    window: &mut Window,
    cx: &mut Context<WorkspaceManager>,
) -> WorkspaceManager {
    let mut manager = WorkspaceManager::new_with_adapters(
        session_factory,
        home_directory,
        adapters,
        sidebar,
        window,
        cx,
    );
    manager.present_desktop_workspace(window, cx);
    manager
}

impl WorkspaceManager {
    /// Presents and activates one desktop-owned Workspace at this window's home directory.
    pub(crate) fn present_desktop_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> WorkspaceId {
        let directory = ValidatedLocalDirectory::new(
            self.local_home_directory_path.clone(),
            self.local_home_identity.clone(),
        );
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            Rc::clone(&self.session_factory),
            directory.clone(),
        );
        let prepared_launch = session_factory
            .prepare_child_launch()
            .expect("a local launch reserves no channel");
        let sidebar_visible = self.sidebar.read(cx).layout().visible;
        let sidebar_width = self.sidebar.read(cx).layout().width;
        let window_drag = Rc::clone(&self.operating_system_window_drag_platform);
        let pane_construction = self.pane_construction.clone();
        let previous = self.workspaces.active_workspace_id();
        let workspace_id = self
            .workspaces
            .create_local_workspace(directory, |workspace_id, _| {
                Self::create_tab_manager_with_prepared_launch(
                    session_factory,
                    prepared_launch,
                    TabManagerCreation {
                        workspace_id,
                        sidebar_visible,
                        sidebar_width,
                        operating_system_window_drag_platform: window_drag,
                        pane_construction,
                    },
                    window,
                    cx,
                )
            })
            .expect("the test Workspace is created");
        // Creation activates the Workspace in the collection; the window then opens it as a
        // person's activation does, from the Workspace that was active.
        if let Some(previous) = previous {
            self.workspaces
                .activate_workspace(previous)
                .expect("the previous Workspace remains");
        }
        self.arrange_by_machine();
        self.activate_workspace(workspace_id, window, cx);
        workspace_id
    }
}

fn workspace_manager(
    cx: &mut TestAppContext,
) -> (
    Entity<WorkspaceManager>,
    TestTerminalSessionRecords,
    &mut VisualTestContext,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
    let (manager, cx) = cx.add_window_view(|window, cx| {
        new_desktop_manager(
            session_factory,
            std::env::temp_dir(),
            workspace_adapters(test_remote_backend_factory()),
            SidebarState::Expanded,
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| manager.focus(window, cx));
        let close_manager = manager.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            close_manager
                .update(cx, |manager, cx| manager.should_close_window(window, cx))
                .unwrap_or(true)
        });
    });
    cx.run_until_parked();
    (manager, records, cx)
}

fn workspace_manager_with_application_actions(
    cx: &mut TestAppContext,
) -> (
    Entity<WorkspaceManager>,
    TestTerminalSessionRecords,
    Rc<crate::platform::application_quit::testing::RecordingApplicationQuitAdapter>,
    &mut VisualTestContext,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let application_quit = Rc::new(
        crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
    );
    cx.update(|cx| {
        crate::app::init(
            cx,
            Rc::new(
                crate::platform::application_menu::testing::RecordingApplicationMenuAdapter::default(),
            ),
            application_quit.clone(),
        )
        .unwrap();
    });
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
    let (manager, cx) = cx.add_window_view(|window, cx| {
        new_desktop_manager(
            session_factory,
            std::env::temp_dir(),
            workspace_adapters(test_remote_backend_factory()),
            SidebarState::Expanded,
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        window.activate_window();
        manager.update(cx, |manager, cx| manager.focus(window, cx));
    });
    cx.run_until_parked();
    (manager, records, application_quit, cx)
}

fn workspace_manager_with_remote_backend(
    backend: Arc<TestRemoteWorkspaceFlowBackend>,
    cx: &mut TestAppContext,
) -> (
    Entity<WorkspaceManager>,
    TestTerminalSessionRecords,
    &mut VisualTestContext,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
    let factory: Arc<dyn RemoteWorkspaceFlowBackendFactory> =
        Arc::new(TestRemoteWorkspaceFlowBackendFactory { backend });
    let (manager, cx) = cx.add_window_view(move |window, cx| {
        new_desktop_manager(
            session_factory,
            std::env::temp_dir(),
            workspace_adapters(factory),
            SidebarState::Expanded,
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

fn reconnect_control_connection(
    destination: &str,
    physical: &str,
) -> (
    ConnectedControlConnection,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    async_channel::Sender<crate::ssh::live_connection::ControlConnectionTerminalState>,
) {
    let physical = crate::domain::RemoteDirectoryIdentity::new(physical.to_owned()).unwrap();
    reconnect_control_connection_with_provider(
        destination,
        Arc::new(TestRemoteProvider::connected(physical)),
    )
}

fn reconnect_control_connection_with_provider(
    destination: &str,
    provider: Arc<dyn RemoteDirectoryProvider>,
) -> (
    ConnectedControlConnection,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    async_channel::Sender<crate::ssh::live_connection::ControlConnectionTerminalState>,
) {
    reconnect_control_connection_with_provider_and_revalidation(
        destination,
        provider,
        VecDeque::new(),
    )
}

fn reconnect_control_connection_with_provider_and_revalidation(
    destination: &str,
    provider: Arc<dyn RemoteDirectoryProvider>,
    revalidation_tasks: VecDeque<
        gpui::Task<Result<(), crate::terminal::TerminalSessionChannelRevalidationError>>,
    >,
) -> (
    ConnectedControlConnection,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    async_channel::Sender<crate::ssh::live_connection::ControlConnectionTerminalState>,
) {
    let destination = crate::domain::SshDestination::new(destination.to_owned()).unwrap();
    let preparations = Arc::new(AtomicUsize::new(0));
    let revalidations = Arc::new(AtomicUsize::new(0));
    let channels: Arc<dyn crate::terminal::TerminalSessionChannelProvider> =
        Arc::new(TestTerminalSessionChannelProvider {
            connection: crate::ssh::testing::SshConnectionFixture::new(destination.clone()),
            preparations: Arc::clone(&preparations),
            revalidations: Arc::clone(&revalidations),
            revalidation_tasks: Mutex::new(revalidation_tasks),
            available: Arc::new(AtomicBool::new(true)),
        });
    let closes = Arc::new(AtomicUsize::new(0));
    let (lifecycle_sender, lifecycle) =
        crate::ssh::live_connection::ControlConnectionObserver::controlled();
    (
        ConnectedControlConnection::new(
            Box::new(TestControlConnectionOwner {
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
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records).with_fallback_title("zsh"));
    let platform = Rc::new(RecordingOperatingSystemWindowDragPlatform::default());
    let injected_platform = Rc::clone(&platform);
    let (manager, cx) = cx.add_window_view(move |window, cx| {
        new_desktop_manager(
            session_factory,
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                window_drag: injected_platform,
                ..workspace_adapters(test_remote_backend_factory())
            },
            SidebarState::Expanded,
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

fn workspace_manager_with_directory_selection(
    selections: impl IntoIterator<
        Item = Result<Option<PathBuf>, crate::directory_selection::DirectoryChooserError>,
    >,
    cx: &mut TestAppContext,
) -> (
    Entity<WorkspaceManager>,
    TestTerminalSessionRecords,
    &mut VisualTestContext,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
    let directory_selection: Rc<dyn SystemDirectorySelection> =
        Rc::new(ScriptedDirectorySelection::new(selections));
    let (manager, cx) = cx.add_window_view(|window, cx| {
        new_desktop_manager(
            session_factory,
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                directory_selection,
                ..workspace_adapters(test_remote_backend_factory())
            },
            SidebarState::Expanded,
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

/// Pins the active Local Workspace through the Directory Picker's hand-off to System Directory
/// Selection.
fn choose_pin_directory(manager: &Entity<WorkspaceManager>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.choose_pin_directory(manager.workspaces.active_id(), window, cx);
        })
    });
    cx.run_until_parked();
    click("directory-picker-confirm-menu", cx);
    click(
        "command-palette-primary-menu-directory-picker-system-selection",
        cx,
    );
}

fn open_workspace_switcher(cx: &mut VisualTestContext) {
    cx.simulate_keystrokes("cmd-shift-k");
    cx.run_until_parked();
}

fn open_workspace_switcher_for_creation(cx: &mut VisualTestContext) {
    open_workspace_switcher(cx);
    cx.simulate_keystrokes("f r e s h space w o r k s p a c e");
    cx.run_until_parked();
}

fn open_remote_workspace_flow(
    manager: &Entity<WorkspaceManager>,
    cx: &mut VisualTestContext,
) -> Entity<RemoteWorkspaceFlow> {
    open_workspace_switcher_for_creation(cx);
    let count = manager.read_with(cx, |manager, _| manager.workspaces.len());
    cx.simulate_keystrokes("space");
    for digit in count.to_string().chars() {
        cx.simulate_keystrokes(&digit.to_string());
    }
    cx.run_until_parked();
    click("workspace-switcher-create-remote", cx);
    manager.read_with(cx, |manager, _| {
        manager
            .remote_workspace_flow
            .as_ref()
            .expect("Remote Workspace must create its flow")
            .clone()
    })
}

fn emit_remote_workspace_completion(
    flow: &Entity<RemoteWorkspaceFlow>,
    completion: RemoteWorkspaceFlowCompletion,
    cx: &mut VisualTestContext,
) {
    cx.update(|_, cx| {
        flow.update(cx, |flow, cx| {
            crate::ui::remote_workspace_flow::emit_completion(flow, completion, cx);
        });
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_id());
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
            let terminal_factory = WorkspaceTerminalSessionFactory::new_remote(
                Rc::clone(&manager.session_factory),
                ValidatedLocalDirectory::new(
                    manager.local_home_directory_path.clone(),
                    manager.local_home_identity.clone(),
                ),
                RemoteTerminalMetadataContext::new(
                    completion.destination().clone(),
                    completion.initial_directory().clone(),
                )
                .with_machine(remote_machine(completion.account())),
                completion.physical_directory().clone(),
                completion.account().login_shell().name().to_owned(),
                completion.terminal_session_channels(),
            );
            let prepared = terminal_factory.prepare_child_launch().unwrap();
            manager
                .try_create_remote_workspace(completion, terminal_factory, prepared, window, cx)
                .unwrap_or_else(|_| panic!("direct Remote Workspace installation failed"));
        });
    });
    cx.run_until_parked();
}

#[gpui::test]
fn application_rtl_locale_installation_should_mirror_production_modal_footer(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| crate::ui::init_with_text_direction(cx, TextDirection::RightToLeft))
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records).with_fallback_title("zsh"));
    let (manager, cx) = cx.add_window_view(|window, cx| {
        new_desktop_manager(
            session_factory,
            std::env::temp_dir(),
            workspace_adapters(test_remote_backend_factory()),
            SidebarState::Expanded,
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
    let policy_is_rtl = cx.update(|_, cx| {
        *cx.global::<spaceterm_ui::ModalDesktopPolicy>()
            == spaceterm_ui::ModalDesktopPolicy::mac_os()
                .with_text_direction(TextDirection::RightToLeft)
    });

    assert!(
        policy_is_rtl && save.left() < cancel.left(),
        "RTL production bounds were save={save:?}, cancel={cancel:?}"
    );
}

#[gpui::test]
fn workspace_modal_mount_blocks_and_restores_terminal_input(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let view = manager.read_with(cx, |manager, cx| {
        manager
            .workspaces
            .active()
            .payload()
            .read(cx)
            .active_tab_view()
    });
    let focused_pane = view.read_with(cx, |view, _| view.focused_pane_id());
    let focused_before =
        cx.update(|window, cx| view.read(cx).focused_terminal_has_input_focus(window, cx));

    let presentation = present_test_alert(&manager, "root-layer-alert", cx);
    cx.run_until_parked();
    let modal_state = cx.update(|window, cx| {
        let view = view.read(cx);
        (
            view.focused_pane_id(),
            view.focused_terminal_has_input_focus(window, cx),
        )
    });

    assert!(cx.debug_bounds("spaceterm-modal-root").is_some());
    let selector: &'static str =
        format!("modal-surface-{}", presentation.presentation_id().value()).leak();
    assert!(cx.debug_bounds(selector).is_some());
    assert_eq!((focused_before, modal_state), (true, (focused_pane, false)));

    cx.update(|window, cx| {
        presentation
            .dismiss(window, cx)
            .expect("root integration modal should dismiss")
    });
    cx.run_until_parked();
    let restored = cx.update(|window, cx| {
        let view = view.read(cx);
        (
            view.focused_pane_id(),
            view.focused_terminal_has_input_focus(window, cx),
        )
    });

    assert_eq!(restored, (focused_pane, true));
}

#[gpui::test]
fn workspace_switcher_reentry_should_not_steal_focus_from_an_active_modal(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    open_workspace_switcher(cx);
    let presentation = present_test_alert(&manager, "workspace-switcher-modal-priority", cx);
    cx.run_until_parked();
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.open_workspace_switcher(window, cx);
            manager.open_workspace_switcher(window, cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.update(|window, cx| window_modal_is_open(window, cx)));
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(
        cx.debug_bounds("modal-action-acknowledge-keyboard-focus")
            .is_some()
    );
    cx.update(|window, cx| presentation.dismiss(window, cx).unwrap());
    cx.run_until_parked();
    assert!(!cx.update(|window, cx| window_modal_is_open(window, cx)));
}

#[gpui::test]
fn queued_modals_should_preserve_focused_pane_and_restore_terminal_input_focus(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let view = manager.read_with(cx, |manager, cx| {
        manager
            .workspaces
            .active()
            .payload()
            .read(cx)
            .active_tab_view()
    });
    let focused_pane = view.read_with(cx, |view, _| view.focused_pane_id());
    let before = cx.update(|window, cx| {
        (
            view.read(cx).focused_pane_id(),
            view.read(cx).focused_terminal_has_input_focus(window, cx),
        )
    });
    let command_count = records.commands().len();

    let first = present_test_alert(&manager, "queued-modal-first", cx);
    cx.run_until_parked();
    let active = cx.update(|window, cx| {
        (
            view.read(cx).focused_pane_id(),
            view.read(cx).focused_terminal_has_input_focus(window, cx),
        )
    });
    let second = present_test_alert(&manager, "queued-modal-second", cx);
    cx.run_until_parked();
    let queued = cx.update(|window, cx| {
        (
            view.read(cx).focused_pane_id(),
            view.read(cx).focused_terminal_has_input_focus(window, cx),
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
            view.read(cx).focused_pane_id(),
            view.read(cx).focused_terminal_has_input_focus(window, cx),
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
            view.read(cx).focused_pane_id(),
            view.read(cx).focused_terminal_has_input_focus(window, cx),
        )
    });
    let focus_reports = records
        .commands()
        .into_iter()
        .skip(command_count)
        .filter_map(|call| match call.command {
            RecordedCommand::Focus(focused) => Some(focused),
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
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let first_records = TestTerminalSessionRecords::default();
    let second_records = TestTerminalSessionRecords::default();
    let first_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(first_records.clone()).with_fallback_title("zsh"));
    let second_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(second_records.clone()).with_fallback_title("zsh"));
    let first = cx.add_window(|window, cx| {
        new_desktop_manager(
            first_factory,
            PathBuf::from("/Users/first"),
            workspace_adapters(test_remote_backend_factory()),
            SidebarState::Expanded,
            window,
            cx,
        )
    });
    let second = cx.add_window(|window, cx| {
        new_desktop_manager(
            second_factory,
            PathBuf::from("/Users/second"),
            workspace_adapters(test_remote_backend_factory()),
            SidebarState::Expanded,
            window,
            cx,
        )
    });

    let first_view = first
        .update(cx, |manager, window, cx| {
            window.activate_window();
            manager.focus(window, cx);
            manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .active_tab_view()
        })
        .expect("first Operating-System Window should remain available");
    cx.run_until_parked();
    let first_focused_pane = first_view.read_with(cx, |view, _| view.focused_pane_id());
    let first_before = first
        .update(cx, |_, window, cx| {
            first_view
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

    let second_view = second
        .update(cx, |manager, window, cx| {
            window.activate_window();
            manager.focus(window, cx);
            manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .active_tab_view()
        })
        .expect("second Operating-System Window should remain available");
    cx.run_until_parked();
    let second_focused_pane = second_view.read_with(cx, |view, _| view.focused_pane_id());
    let second_before = second
        .update(cx, |_, window, cx| {
            second_view
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
            let view = first_view.read(cx);
            (
                view.focused_pane_id(),
                view.focused_terminal_has_input_focus(window, cx),
            )
        })
        .expect("first Operating-System Window should remain available");
    let second_blocked = second
        .update(cx, |_, window, cx| {
            let view = second_view.read(cx);
            (
                view.focused_pane_id(),
                view.focused_terminal_has_input_focus(window, cx),
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
            let view = first_view.read(cx);
            (
                view.focused_pane_id(),
                view.focused_terminal_has_input_focus(window, cx),
            )
        })
        .expect("first Operating-System Window should remain available");
    let second_still_blocked = second
        .update(cx, |_, window, cx| {
            let view = second_view.read(cx);
            (
                view.focused_pane_id(),
                view.focused_terminal_has_input_focus(window, cx),
            )
        })
        .expect("second Operating-System Window should remain available");
    let first_focus_reports = first_records
        .commands()
        .into_iter()
        .skip(first_command_count)
        .filter_map(|call| match call.command {
            RecordedCommand::Focus(focused) => Some(focused),
            _ => None,
        })
        .collect::<Vec<_>>();
    let second_focus_reports_while_blocked = second_records
        .commands()
        .into_iter()
        .skip(second_command_count)
        .filter_map(|call| match call.command {
            RecordedCommand::Focus(focused) => Some(focused),
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
            let view = second_view.read(cx);
            (
                view.focused_pane_id(),
                view.focused_terminal_has_input_focus(window, cx),
            )
        })
        .expect("second Operating-System Window should remain available");
    let second_focus_reports = second_records
        .commands()
        .into_iter()
        .skip(second_command_count)
        .filter_map(|call| match call.command {
            RecordedCommand::Focus(focused) => Some(focused),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        (second_restored, second_focus_reports),
        ((second_focused_pane, true), vec![false, true])
    );
}

#[gpui::test]
fn cancelled_directory_selection_fallback_should_leave_hierarchy_unchanged(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager_with_directory_selection([Ok(None)], cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let view = manager.read_with(cx, |manager, cx| {
        manager
            .workspaces
            .active()
            .payload()
            .read(cx)
            .active_tab_view()
    });
    let terminal_focused = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| view.read(cx).focused_terminal_has_input_focus(window, cx))
    };
    assert!(terminal_focused(cx));

    choose_pin_directory(&manager, cx);

    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        1
    );
    assert_eq!(records.starts().len(), 1);
    assert!(
        terminal_focused(cx),
        "cancelling System Directory Selection left the terminal without focus"
    );
}

#[gpui::test]
fn titlebar_button_and_command_shift_k_should_each_block_terminal_input(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    click("workspace-switcher", cx);
    assert_eq!(
        cx.update(|window, cx| manager.read(cx).terminal_focus_blocker(window, cx)),
        Some(TerminalFocusBlocker::CommandPalette)
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    open_workspace_switcher(cx);
    assert_eq!(
        cx.update(|window, cx| manager.read(cx).terminal_focus_blocker(window, cx)),
        Some(TerminalFocusBlocker::CommandPalette)
    );
}

#[gpui::test]
fn hiding_the_sidebar_should_keep_the_top_combo_box_and_its_focus_blocker(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    click("workspace-switcher", cx);

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();

    assert_eq!(
        cx.update(|window, cx| {
            (
                manager.read(cx).sidebar.read(cx).layout().visible,
                window_combo_box_is_open(window, cx),
                manager.read(cx).terminal_focus_blocker(window, cx),
            )
        }),
        (false, true, Some(TerminalFocusBlocker::CommandPalette))
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|window, cx| manager.read(cx).terminal_focus_blocker(window, cx)),
        None
    );
}

#[gpui::test]
fn top_combo_box_available_remote_should_open_one_flow_and_keep_terminal_input_blocked(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    open_workspace_switcher_for_creation(cx);

    click("workspace-switcher-create-remote", cx);

    assert_eq!(
        cx.update(|window, cx| {
            let manager = manager.read(cx);
            let flow = manager
                .remote_workspace_flow
                .as_ref()
                .expect("the available Remote Workspace source should open its flow")
                .read(cx);
            (
                window_combo_box_is_open(window, cx),
                flow.stage(),
                flow.owns_first_responder(window, cx),
                manager.terminal_focus_blocker(window, cx),
                manager
                    .workspaces
                    .active()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
                manager.workspaces.len(),
                records.starts().len(),
            )
        }),
        (
            false,
            RemoteWorkspaceFlowStage::HostSelection,
            true,
            Some(TerminalFocusBlocker::CommandPalette),
            false,
            1,
            1,
        )
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert_eq!(
        cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.remote_workspace_flow.is_none(),
                manager.terminal_focus_blocker(window, cx),
                manager
                    .workspaces
                    .active()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
                manager.workspaces.len(),
                records.starts().len(),
            )
        }),
        (true, None, true, 1, 1)
    );
}

#[gpui::test]
fn top_combo_box_unavailable_remote_should_reject_acceptance_and_keep_terminal_input_blocked(
    cx: &mut TestAppContext,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
    let create_calls = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn RemoteWorkspaceFlowBackendFactory> =
        Arc::new(UnavailableTestRemoteWorkspaceFlowBackendFactory {
            create_calls: Arc::clone(&create_calls),
        });
    let (manager, cx) = cx.add_window_view(move |window, cx| {
        new_desktop_manager(
            session_factory,
            std::env::temp_dir(),
            workspace_adapters(factory),
            SidebarState::Expanded,
            window,
            cx,
        )
    });
    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.focus(window, cx)));
    cx.run_until_parked();
    open_workspace_switcher_for_creation(cx);

    click("workspace-switcher-create-remote", cx);
    click("workspace-switcher-open-remote-directory", cx);
    cx.simulate_keystrokes("cmd-shift-n");
    cx.simulate_keystrokes("cmd-shift-o");
    cx.run_until_parked();

    assert_eq!(
        cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                create_calls.load(Ordering::Acquire),
                window_combo_box_is_open(window, cx),
                manager.remote_workspace_flow.is_none(),
                manager.terminal_focus_blocker(window, cx),
                manager
                    .workspaces
                    .active()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
                manager.workspaces.len(),
                records.starts().len(),
            )
        }),
        (
            0,
            true,
            true,
            Some(TerminalFocusBlocker::CommandPalette),
            false,
            1,
            1,
        )
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert_eq!(
        cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                window_combo_box_is_open(window, cx),
                manager.terminal_focus_blocker(window, cx),
                manager
                    .workspaces
                    .active()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        }),
        (false, None, true)
    );
    click_new_workspace_menu("new-workspace-menu-create-remote", cx);
    // A disabled row keeps the menu open.
    click("new-workspace-menu-open-remote-directory", cx);
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("cmd-shift-n");
    cx.simulate_keystrokes("cmd-shift-o");
    cx.run_until_parked();
    assert_eq!(create_calls.load(Ordering::Acquire), 0);
    assert!(manager.read_with(cx, |manager, _| manager.remote_workspace_flow.is_none()));
    assert_eq!(records.starts().len(), 1);
}

#[gpui::test]
fn choosing_remote_workspace_should_strictly_replace_the_panel_and_restore_focus_on_escape(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);

    open_workspace_switcher_for_creation(cx);
    click("workspace-switcher-create-remote", cx);
    let flow = manager.read_with(cx, |manager, _| {
        manager
            .remote_workspace_flow
            .as_ref()
            .expect("Remote Workspace must create its flow")
            .clone()
    });

    let opened = cx.update(|window, cx| {
        let manager = manager.read(cx);
        (
            window_combo_box_is_open(window, cx),
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
            manager.remote_workspace_flow.is_none(),
            manager.terminal_focus_blocker(window, cx),
            manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .focused_terminal_is_focused(window, cx),
        )
    });
    assert_eq!(escaped, (true, None, true));

    open_workspace_switcher_for_creation(cx);
    click("workspace-switcher-create-remote", cx);
    let reopened = manager.read_with(cx, |manager, _| {
        manager
            .remote_workspace_flow
            .as_ref()
            .expect("Remote Workspace should create a fresh flow")
            .clone()
    });
    assert_ne!(reopened, flow);
    assert_eq!(
        reopened.read_with(cx, |flow, _| flow.stage()),
        RemoteWorkspaceFlowStage::HostSelection
    );
}

#[gpui::test]
fn replacing_the_ssh_host_picker_should_release_terminal_input_and_allow_reopening(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    cx.simulate_keystrokes("cmd-shift-k cmd-shift-n cmd-shift-k");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(!active_terminal_has_input_focus(&manager, cx));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    click("terminal-pane", cx);
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    assert!(
        records
            .commands()
            .iter()
            .any(|call| matches!(call.command, RecordedCommand::Key(_))),
        "the terminal must accept input after the replacement switcher closes"
    );

    cx.simulate_keystrokes("cmd-shift-n");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .remote_workspace_flow
            .as_ref()
            .is_some_and(|flow| flow.read(cx).owns_first_responder(window, cx))
    }));
    assert!(cx.debug_bounds("command-palette-panel").is_some());
}

#[gpui::test]
fn deactivated_remote_creation_should_restore_actions_after_releasing_its_flow(
    cx: &mut TestAppContext,
) {
    let pending_account = cx.executor().spawn(async { std::future::pending().await });
    let provider = Arc::new(BlockingRemoteProvider {
        account: Mutex::new(Some(pending_account)),
        identity: Mutex::new(None),
        account_calls: Arc::new(AtomicUsize::new(0)),
        identity_calls: Arc::new(AtomicUsize::new(0)),
    });
    let (control_connection, closes, _, _, _) =
        reconnect_control_connection_with_provider("work", provider);
    let backend = TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(
        control_connection,
    ))]);
    let (manager, _, cx) = workspace_manager_with_remote_backend(backend, cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let flow = open_remote_workspace_flow(&manager, cx);
    cx.update(|window, cx| {
        flow.update(cx, |flow, cx| {
            crate::ui::remote_workspace_flow::select_host_destination(
                flow,
                crate::domain::SshDestination::new("work".to_owned()).unwrap(),
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert_eq!(
        flow.read_with(cx, |flow, _| flow.stage()),
        RemoteWorkspaceFlowStage::PreparingHome
    );

    cx.deactivate_window();
    cx.run_until_parked();

    assert_eq!(closes.load(Ordering::Acquire), 1);
    assert_eq!(
        flow.read_with(cx, |flow, _| flow.stage()),
        RemoteWorkspaceFlowStage::Cancelled
    );
    assert!(manager.read_with(cx, |manager, _| manager.remote_workspace_flow.is_none()));
    assert_eq!(
        cx.update(|window, cx| manager.read(cx).terminal_focus_blocker(window, cx)),
        None
    );
    assert!(manager.read_with(cx, |manager, _| {
        manager.remote_workspace_focus_restore_pending
    }));

    let cancelled_flow_id = flow.entity_id();
    drop(flow);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    assert!(manager.read_with(cx, |manager, _| {
        !manager.remote_workspace_focus_restore_pending
    }));
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }));
    assert!(cx.update(|window, cx| { window.is_action_available(&NewWorkspace, cx) }));

    open_workspace_switcher_for_creation(cx);
    assert!(cx.update(|window, cx| { window_combo_box_is_open(window, cx) }));
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.remote_workspace_focus_restore_pending = true;
            manager.restore_remote_workspace_focus_after_activation(window, cx);
        });
    });
    assert!(cx.update(|window, cx| {
        let manager = manager.read(cx);
        !manager.remote_workspace_focus_restore_pending
            && window_combo_box_is_open(window, cx)
            && !manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .focused_terminal_is_focused(window, cx)
    }));
    click("workspace-switcher-create-remote", cx);
    let reopened = manager.read_with(cx, |manager, _| {
        manager
            .remote_workspace_flow
            .as_ref()
            .expect("Remote Workspace should create a fresh flow")
            .clone()
    });
    assert_ne!(reopened.entity_id(), cancelled_flow_id);
}

#[gpui::test]
fn backend_construction_failure_should_disable_remote_instead_of_leaving_an_inert_source(
    cx: &mut TestAppContext,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records).with_fallback_title("zsh"));
    let create_calls = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn RemoteWorkspaceFlowBackendFactory> =
        Arc::new(FailingTestRemoteWorkspaceFlowBackendFactory {
            create_calls: Arc::clone(&create_calls),
        });
    let (manager, cx) = cx.add_window_view(move |window, cx| {
        new_desktop_manager(
            session_factory,
            std::env::temp_dir(),
            workspace_adapters(factory),
            SidebarState::Expanded,
            window,
            cx,
        )
    });
    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.focus(window, cx)));
    cx.run_until_parked();

    open_workspace_switcher_for_creation(cx);
    click("workspace-switcher-create-remote", cx);
    cx.run_until_parked();

    assert_eq!(create_calls.load(Ordering::Acquire), 1);
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
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

    let (workspace_id, destination, directory, physical, runtime_count, has_alias_pin) = manager
        .read_with(cx, |manager, _| {
            let workspace = manager.workspaces.active();
            (
                workspace.id(),
                workspace
                    .remote_workspace_key()
                    .expect("active Workspace must be remote")
                    .destination()
                    .as_str()
                    .to_owned(),
                workspace
                    .remote_starting_directory()
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
    assert_eq!(remote.local_home().path(), std::env::temp_dir());

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
                .active()
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
    let (workspace_id, tab_manager, view) = manager.read_with(cx, |manager, cx| {
        let workspace = manager.workspaces.active();
        let tab_manager = workspace.payload().clone();
        let view = tab_manager.read(cx).active_tab_view();
        (workspace.id(), tab_manager, view)
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
        tab_manager.entity_id()
    );
    assert_eq!(
        tab_manager.read_with(cx, |manager, _| manager.active_tab_view().entity_id()),
        view.entity_id()
    );
    assert_eq!(
        view.read_with(cx, |host, cx| (
            host.remote_disconnected_generation(),
            host.focused_terminal_remote_state(cx),
            host.pane_count(),
        )),
        (Some(1), (true, true), 1)
    );
    let status_selector: &'static str =
        Box::leak(format!("workspace-row-remote-status-{}", workspace_id.get()).into_boxed_str());
    assert!(cx.debug_bounds(status_selector).is_some());

    cx.simulate_keystrokes("cmd-b");
    redraw(cx);
    assert!(
        cx.debug_bounds("workspace-switcher-status-disconnected")
            .is_some(),
        "the collapsed identity must keep the Active Remote Workspace's disconnected state visible"
    );
    let label = cx
        .debug_bounds("workspace-chip-label")
        .expect("collapsed identity name");
    let dot = cx
        .debug_bounds("workspace-switcher-status-disconnected")
        .expect("disconnected status dot");
    let switcher = cx
        .debug_bounds("workspace-switcher")
        .expect("collapsed Workspace switcher");
    assert!(
        dot.left() >= label.right() && dot.right() <= switcher.right(),
        "the status dot must trail the name inside the collapsed switcher"
    );
    assert!(
        (dot.center().y - label.center().y).abs() <= px(1.0),
        "the status dot must sit on the name's vertical center"
    );
    cx.simulate_keystrokes("cmd-b");
    redraw(cx);

    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    assert_eq!(records.starts().len(), starts_before_disconnect);
    assert_eq!(view.read_with(cx, |host, _| host.pane_count()), 1);
}

#[gpui::test]
fn workspace_menu_should_offer_reconnect_only_after_disconnect_or_failure(cx: &mut TestAppContext) {
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_id());
    let row_selector: &'static str =
        Box::leak(format!("workspace-row-{}-active", workspace_id.get()).into_boxed_str());
    redraw(cx);

    right_click(row_selector, cx);
    assert!(
        cx.debug_bounds("workspace-menu-row-reconnect").is_none(),
        "Connected Workspaces must not show an unavailable Reconnect action"
    );
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
    click("modal-action-remote-workspace-reconnect-error-ok", cx);
    redraw(cx);
    click("toggle-sidebar-button", cx);
    assert!(
        cx.debug_bounds("workspace-switcher-status-failed")
            .is_some(),
        "the collapsed identity must keep the Active Remote Workspace's failed state visible"
    );
    click("toggle-sidebar-button", cx);
    redraw(cx);
    right_click(row_selector, cx);
    click("workspace-menu-row-reconnect", cx);
    assert_eq!(backend.connect_calls.load(Ordering::Acquire), 2);
}

#[gpui::test]
fn reconnect_authentication_should_promote_queued_askpass_and_restore_progress_after_close(
    cx: &mut TestAppContext,
) {
    let (_connection_sender, connection_receiver) = async_channel::bounded(1);
    let pending = cx.update(|cx| {
        cx.background_executor()
            .spawn(async move { connection_receiver.recv().await.unwrap() })
    });
    let backend = TestRemoteWorkspaceFlowBackend::with_connections([pending]);
    let (manager, _, cx) = workspace_manager_with_remote_backend(backend, cx);
    let workspace_id = create_disconnected_remote_workspace(&manager, cx);
    let outcomes = Rc::new(RefCell::new(Vec::new()));
    let askpass = cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.start_remote_workspace_reconnect(workspace_id, window, cx)
        });
        let askpass = cx.new(|_| GpuiAskPassPresenter::default());
        let captured = Rc::clone(&outcomes);
        askpass
            .update(cx, |presenter, cx| {
                presenter.present(
                    55,
                    AskPassRequest::new(
                        "root@example.test's password:".to_owned(),
                        AskPassPromptKind::Secret,
                    )
                    .unwrap(),
                    Box::new(move |result| {
                        captured
                            .borrow_mut()
                            .push(matches!(result, AskPassResult::Cancelled));
                    }),
                    window,
                    cx,
                )
            })
            .unwrap();
        askpass
    });
    cx.run_until_parked();
    let original = manager.read_with(cx, |manager, _| {
        manager
            .remote_workspace_reconnect
            .as_ref()
            .and_then(|attempt| attempt.progress.as_ref())
            .map(ProgressDialogHandle::presentation_id)
            .unwrap()
    });
    assert!(cx.debug_bounds("ssh-askpass-secret-input").is_none());

    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            assert!(manager.apply_remote_workspace_reconnect_progress(
                workspace_id,
                2,
                RemoteWorkspaceConnectionProgress::Authenticating,
                window,
                cx,
            ));
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("ssh-askpass-secret-input").is_some());
    assert!(manager.read_with(cx, |manager, _| {
        manager
            .remote_workspace_reconnect
            .as_ref()
            .is_some_and(|attempt| attempt.progress.is_none())
    }));

    cx.update(|window, cx| {
        askpass.update(cx, |presenter, cx| presenter.cancel_owner(55, window, cx));
    });
    cx.run_until_parked();
    assert_eq!(outcomes.borrow().as_slice(), [true]);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            assert!(manager.apply_remote_workspace_reconnect_progress(
                workspace_id,
                2,
                RemoteWorkspaceConnectionProgress::Connecting,
                window,
                cx,
            ));
        });
    });
    cx.run_until_parked();
    let restored = manager.read_with(cx, |manager, _| {
        manager
            .remote_workspace_reconnect
            .as_ref()
            .and_then(|attempt| attempt.progress.as_ref())
            .map(ProgressDialogHandle::presentation_id)
            .unwrap()
    });
    assert_ne!(restored, original);

    click("modal-action-remote-workspace-reconnect-cancel", cx);
    assert!(manager.read_with(cx, |manager, _| {
        manager.remote_workspace_reconnect.is_none()
    }));
}

#[gpui::test]
fn reconnect_should_atomically_restart_the_same_workspace_tab_and_pane(cx: &mut TestAppContext) {
    let (new_control_connection, new_closes, new_preparations, new_revalidations, _new_lifecycle) =
        reconnect_control_connection("work", "/home/tester/src");
    let backend = TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(
        new_control_connection,
    ))]);
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
    let (workspace_id, tab_manager, view, starts_before_reconnect) =
        manager.read_with(cx, |manager, cx| {
            let workspace = manager.workspaces.active();
            let tab_manager = workspace.payload().clone();
            let view = tab_manager.read(cx).active_tab_view();
            (workspace.id(), tab_manager, view, records.starts().len())
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
        tab_manager.entity_id()
    );
    assert_eq!(
        tab_manager.read_with(cx, |manager, _| manager.active_tab_view().entity_id()),
        view.entity_id()
    );
    assert_eq!(
        view.read_with(cx, |host, cx| (
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
    let (new_control_connection, new_closes, _, _, new_lifecycle) =
        reconnect_control_connection("work", "/home/tester/src");
    new_lifecycle
        .try_send(ControlConnectionTerminalState::Failed)
        .unwrap();
    let backend = TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(
        new_control_connection,
    ))]);
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_id());
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
    let (mut text_context, rendered_text) = RecordingRenderedText::context(cx);
    let cx = &mut text_context;
    let (new_control_connection, new_closes, new_preparations, new_revalidations, _) =
        reconnect_control_connection("work", "/home/tester/replaced");
    let backend = TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(
        new_control_connection,
    ))]);
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
    let (workspace_id, view, starts) = manager.read_with(cx, |manager, cx| {
        let workspace = manager.workspaces.active();
        (
            workspace.id(),
            workspace.payload().read(cx).active_tab_view(),
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
        view.read_with(cx, |host, cx| host.focused_terminal_remote_state(cx)),
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
            "The selected remote path now resolves to a different directory. Reopen the Remote Workspace to review it.".to_owned()
        ))
    );
    assert_rendered_text(
        "modal-header-title",
        "Remote Directory Changed",
        &rendered_text,
        cx,
    );
}

#[gpui::test]
fn reconnect_directory_unavailable_should_remain_disconnected_with_actionable_alert(
    cx: &mut TestAppContext,
) {
    let (mut text_context, rendered_text) = RecordingRenderedText::context(cx);
    let cx = &mut text_context;
    let (new_control_connection, new_closes, _, _, _) = reconnect_control_connection_with_provider(
        "work",
        Arc::new(TestRemoteProvider::directory_unavailable()),
    );
    let backend = TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(
        new_control_connection,
    ))]);
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_id());
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
            "SpaceTerm can’t access the selected remote directory. Check its permissions or reopen the Remote Workspace.".to_owned()
        ))
    );
    assert_rendered_text(
        "modal-header-title",
        "Remote Directory Unavailable",
        &rendered_text,
        cx,
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
    let (mut new_control_connection, new_closes, _, _, _) =
        reconnect_control_connection("work", "/home/tester/src");
    let (pending_sender, pending_receiver) = async_channel::bounded(1);
    let pending = cx.update(|cx| {
        cx.background_executor()
            .spawn(async move { pending_receiver.recv().await.unwrap() })
    });
    let backend = TestRemoteWorkspaceFlowBackend::with_connections([pending]);
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
    let (workspace_id, tab_manager, starts) = manager.read_with(cx, |manager, _| {
        let workspace = manager.workspaces.active();
        (
            workspace.id(),
            workspace.payload().clone(),
            records.starts().len(),
        )
    });
    assert_eq!(
        tab_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
        (1, 2)
    );

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
    let account = test_remote_account();
    let channels = new_control_connection
        .bind_terminal_session_channels(account.login_shell())
        .unwrap();
    let prepared_lifecycle = new_control_connection.take_lifecycle_observer().unwrap();
    let factory = manager.read_with(cx, |manager, _| {
        WorkspaceTerminalSessionFactory::new_remote(
            Rc::clone(&manager.session_factory),
            ValidatedLocalDirectory::new(
                manager.local_home_directory_path.clone(),
                manager.local_home_identity.clone(),
            ),
            RemoteTerminalMetadataContext::new(
                crate::domain::SshDestination::new("work".into()).unwrap(),
                RemoteDirectory::new("~/src".into()).unwrap(),
            )
            .with_machine(remote_machine(&account)),
            crate::domain::RemoteDirectoryIdentity::new("/home/tester/src".into()).unwrap(),
            account.login_shell().name().to_owned(),
            channels,
        )
    });
    let preparation = tab_manager.update(cx, |manager, cx| {
        manager.prepare_remote_restart(factory, 2, cx)
    });
    let prepared = Rc::new(RefCell::new(None));
    let result = prepared.clone();
    cx.update(|_, cx| {
        cx.spawn(async move |_| {
            *result.borrow_mut() = Some(preparation.await);
        })
        .detach()
    });
    cx.run_until_parked();
    let restart = prepared
        .borrow_mut()
        .take()
        .expect("real restart preparation must complete")
        .unwrap();
    assert_eq!(
        tab_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
        (1, 2)
    );
    cx.update(|window, cx| {
        tab_manager
            .read(cx)
            .active_tab_view()
            .update(cx, |host, cx| {
                host.close_pane_authorized(host.focused_pane_id(), window, cx);
            })
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.finish_remote_workspace_reconnect(
                workspace_id,
                2,
                Ok(PreparedRemoteWorkspaceReconnect {
                    control_connection: new_control_connection,
                    lifecycle: prepared_lifecycle,
                    restart,
                    remote_user: account.remote_user().clone(),
                }),
                window,
                cx,
            )
        })
    });
    // A cancelled task drops its future only when the executor runs it.
    cx.run_until_parked();
    drop(pending_sender);
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
        tab_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
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
    let (late_control_connection, late_closes, _, _, _) =
        reconnect_control_connection("work", "/home/tester/src");
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_id());
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

    assert!(sender.try_send(Ok(late_control_connection)).is_err());
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
fn cancelling_while_account_discovery_is_blocked_should_close_control_connection_immediately(
    cx: &mut TestAppContext,
) {
    let (account_sender, account_receiver) = async_channel::bounded(1);
    let account_task = cx.update(|cx| {
        cx.background_executor()
            .spawn(async move { account_receiver.recv().await.unwrap() })
    });
    let account_calls = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn RemoteDirectoryProvider> = Arc::new(BlockingRemoteProvider {
        account: Mutex::new(Some(account_task)),
        identity: Mutex::new(Some(gpui::Task::ready(Ok(
            crate::domain::RemoteDirectoryIdentity::new("/home/tester/src".to_owned()).unwrap(),
        )))),
        account_calls: Arc::clone(&account_calls),
        identity_calls: Arc::new(AtomicUsize::new(0)),
    });
    let (control_connection, closes, _, _, _) =
        reconnect_control_connection_with_provider("work", provider);
    let backend = TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(
        control_connection,
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
fn closing_workspace_while_identity_validation_is_blocked_should_close_control_connection_immediately(
    cx: &mut TestAppContext,
) {
    let (identity_sender, identity_receiver) = async_channel::bounded(1);
    let identity_task = cx.update(|cx| {
        cx.background_executor()
            .spawn(async move { identity_receiver.recv().await.unwrap() })
    });
    let identity_calls = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn RemoteDirectoryProvider> = Arc::new(BlockingRemoteProvider {
        account: Mutex::new(Some(gpui::Task::ready(Ok(test_remote_account())))),
        identity: Mutex::new(Some(identity_task)),
        account_calls: Arc::new(AtomicUsize::new(0)),
        identity_calls: Arc::clone(&identity_calls),
    });
    let (control_connection, closes, _, _, _) =
        reconnect_control_connection_with_provider("work", provider);
    let backend = TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(
        control_connection,
    ))]);
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
fn application_cleanup_while_restart_preparation_is_blocked_should_abort_and_close_control_connection(
    cx: &mut TestAppContext,
) {
    let (revalidation_sender, revalidation_receiver) = async_channel::bounded(1);
    let revalidation_task = cx.update(|cx| {
        cx.background_executor()
            .spawn(async move { revalidation_receiver.recv().await.unwrap() })
    });
    let provider: Arc<dyn RemoteDirectoryProvider> = Arc::new(TestRemoteProvider::connected(
        crate::domain::RemoteDirectoryIdentity::new("/home/tester/src".to_owned()).unwrap(),
    ));
    let (control_connection, closes, _, revalidations, _) =
        reconnect_control_connection_with_provider_and_revalidation(
            "work",
            provider,
            VecDeque::from([revalidation_task]),
        );
    let backend = TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(
        control_connection,
    ))]);
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_id());
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
    let (mut text_context, rendered_text) = RecordingRenderedText::context(cx);
    let cx = &mut text_context;
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
    assert_rendered_text(
        "modal-alert-message",
        "SpaceTerm couldn’t restore the remote connection. OpenSSH reported:\n\nssh: Permission denied (publickey).",
        &rendered_text,
        cx,
    );
}

#[gpui::test]
fn closing_workspace_during_reconnect_should_close_late_control_connection_and_prevent_resurrection(
    cx: &mut TestAppContext,
) {
    let (late_control_connection, late_closes, _, _, _) =
        reconnect_control_connection("work", "/home/tester/src");
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_id());
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

    assert!(sender.try_send(Ok(late_control_connection)).is_err());
    cx.run_until_parked();
    assert_eq!(late_closes.load(Ordering::Acquire), 1);
    assert!(manager.read_with(cx, |manager, _| {
        manager.workspaces.workspace(workspace_id).is_none()
    }));
}

#[gpui::test]
fn remote_child_identity_failure_preserves_connection_and_restores_focus_after_alert(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    let flow = open_remote_workspace_flow(&manager, cx);
    let (completion, _, _, _) = remote_completion("work", "~/src", "/home/tester/src", true);
    emit_remote_workspace_completion(&flow, completion, cx);
    let (workspace_id, tab_manager) = manager.read_with(cx, |manager, _| {
        let workspace = manager.workspaces.active();
        (workspace.id(), workspace.payload().clone())
    });

    tab_manager.update(cx, |_, cx| {
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
        tab_manager
            .read(cx)
            .focused_terminal_has_input_focus(window, cx)
    }));
    assert!(
        cx.debug_bounds("modal-action-remote-workspace-reconnect-error-ok")
            .is_some()
    );
    click("modal-action-remote-workspace-reconnect-error-ok", cx);
    assert!(
        cx.update(|window, cx| { tab_manager.read(cx).focused_terminal_is_focused(window, cx) })
    );
}

#[gpui::test]
fn failed_flow_activation_should_close_connection_and_offer_retry(cx: &mut TestAppContext) {
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
            RemoteWorkspaceFlowStage::ConnectionError,
            Some(TerminalFocusBlocker::Modal),
        )
    );
    assert_eq!(records.starts().len(), 1);
    assert_eq!(preparations.load(Ordering::Acquire), 0);
    assert_eq!(closes.load(Ordering::Acquire), 1);
}

#[gpui::test]
fn remote_revalidation_failures_should_close_completion_without_mutation(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);

    for error in [
        crate::terminal::TerminalSessionChannelRevalidationError::ConnectionUnavailable,
        crate::terminal::TerminalSessionChannelRevalidationError::DirectoryUnavailable,
        crate::terminal::TerminalSessionChannelRevalidationError::IdentityChanged,
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
        assert_eq!(closes.load(Ordering::Acquire), 1);
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::ConnectionError
        );

        cx.update(|window, cx| {
            flow.update(cx, |flow, cx| {
                crate::ui::remote_workspace_flow::cancel(flow, window, cx);
            });
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
        first_flow.update(cx, |flow, cx| {
            crate::ui::remote_workspace_flow::cancel(flow, window, cx);
        });
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
    assert_ne!(current_flow, first_flow);
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
fn matching_remote_destinations_should_create_independent_workspaces(cx: &mut TestAppContext) {
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
                .active()
                .remote_starting_directory()
                .unwrap()
                .as_str()
                .to_owned(),
            manager.remote_workspace_runtimes.len(),
        )
    });
    assert_eq!(state, (3, "/home/tester/src".to_owned(), 2));
    assert_eq!(records.starts().len(), 3);
    assert_eq!(first_preparations.load(Ordering::Acquire), 1);
    assert_eq!(duplicate_preparations.load(Ordering::Acquire), 1);
    assert_eq!(first_closes.load(Ordering::Acquire), 0);
    assert_eq!(duplicate_closes.load(Ordering::Acquire), 0);
}

#[gpui::test]
fn independent_workspace_creation_during_revalidation_should_preserve_both_launches(
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
                .active()
                .remote_starting_directory()
                .map(|directory| directory.as_str().to_owned()),
        )),
        (3, 2, Some("~/src".to_owned()))
    );
    assert_eq!(records.starts().len(), 3);
    assert_eq!(pending_preparations.load(Ordering::Acquire), 1);
    assert_eq!(pending_closes.load(Ordering::Acquire), 0);
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_id());
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_id());

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
    assert_eq!(closes.load(Ordering::Acquire), 1);
    assert!(!aliases.is_active(&alias));
    assert_eq!(
        flow.read_with(cx, |flow, _| flow.stage()),
        RemoteWorkspaceFlowStage::ConnectionError
    );

    cx.update(|window, cx| {
        flow.update(cx, |flow, cx| {
            crate::ui::remote_workspace_flow::cancel(flow, window, cx);
        });
    });
    cx.run_until_parked();

    assert_eq!(closes.load(Ordering::Acquire), 1);
    assert!(!aliases.is_active(&alias));
    assert!(aliases.begin_mutation([alias]).is_ok());
}

#[gpui::test]
fn repeated_remote_runtime_cleanup_closes_once(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    let (completion, closes, _, _) = remote_completion("work", "~/src", "/home/tester/src", true);
    let flow = open_remote_workspace_flow(&manager, cx);
    emit_remote_workspace_completion(&flow, completion, cx);

    manager.update(cx, |manager, _| manager.close_remote_runtimes());
    manager.update(cx, |manager, _| manager.close_remote_runtimes());

    assert_eq!(closes.load(Ordering::Acquire), 1);
}

#[gpui::test]
fn unavailable_initial_terminal_session_channel_should_close_completion_and_offer_retry(
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
    assert_eq!(closes.load(Ordering::Acquire), 1);

    availability.store(true, Ordering::Release);
    assert_eq!(
        flow.read_with(cx, |flow, _| flow.stage()),
        RemoteWorkspaceFlowStage::ConnectionError
    );
}

#[gpui::test]
fn failed_pin_activation_should_report_the_error_without_changing_the_hierarchy(
    cx: &mut TestAppContext,
) {
    let project = temporary_directory("activation-failure");
    fs::create_dir_all(&project).unwrap();
    let (manager, records, cx) =
        workspace_manager_with_directory_selection([Ok(Some(project.clone()))], cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            let workspace_id = manager.workspaces.active_id();
            manager.transient.pin_target = Some(PinTarget::Workspace(workspace_id));
            manager.choose_local_pin_directory(PinTarget::Workspace(workspace_id), window, cx);
            // Selection validation retains its valid authority; activation independently fails.
            manager.local_filesystem = LocalFilesystemAuthority::testing_with_failure(
                crate::platform::local_filesystem::LocalFilesystemError::Capacity,
            );
        })
    });
    cx.run_until_parked();

    assert_eq!(records.starts().len(), 1);
    assert!(manager.read_with(cx, |manager, _| {
        manager.workspaces.active().pinned_directory().is_none()
    }));
    assert!(
        cx.debug_bounds("modal-action-workspace-pin-error-ok")
            .is_some()
    );
    click("modal-action-workspace-pin-error-ok", cx);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|window, cx| manager.read(cx).terminal_focus_blocker(window, cx)),
        None
    );
    fs::remove_dir_all(project).unwrap();
}

#[gpui::test]
fn a_local_workspace_should_pin_the_directory_typed_in_the_directory_picker(
    cx: &mut TestAppContext,
) {
    let project = temporary_directory("local-picker");
    fs::create_dir_all(&project).unwrap();
    let (manager, _, cx) = workspace_manager_with_directory_selection([], cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.choose_pin_directory(manager.workspaces.active_id(), window, cx);
        })
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("command-palette-query-prefix").is_none(),
        "a local path should not name a machine"
    );

    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input(&format!("{}/", project.to_str().unwrap()));
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();

    let pinned = manager.read_with(cx, |manager, _| {
        match manager.workspaces.active().pinned_directory() {
            Some(PinnedDirectory::Local(directory)) => Some(directory.path().to_owned()),
            _ => None,
        }
    });
    assert_eq!(pinned, Some(project.clone()));
    assert!(manager.read_with(cx, |manager, _| manager.pin_picker.is_none()));
    fs::remove_dir_all(project).unwrap();
}

fn confirm_directory_picker_path(path: &str, cx: &mut VisualTestContext) {
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input(path);
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
}

#[gpui::test]
fn open_remote_directory_should_start_the_remote_flow_at_a_chosen_directory(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-shift-o");
    cx.run_until_parked();
    let flow = manager
        .read_with(cx, |manager, _| manager.remote_workspace_flow.clone())
        .expect("Open Remote Directory should present the Remote Workspace flow");
    assert_eq!(
        flow.read_with(cx, |flow, _| (
            flow.stage(),
            crate::ui::remote_workspace_flow::start(flow)
        )),
        (
            RemoteWorkspaceFlowStage::HostSelection,
            RemoteWorkspaceStart::ChosenDirectory
        )
    );
}

#[gpui::test]
fn open_remote_directory_should_return_to_the_picker_after_a_failed_launch_and_then_create(
    cx: &mut TestAppContext,
) {
    let provider = Arc::new(TestRemoteProvider::connected(
        crate::domain::RemoteDirectoryIdentity::new("/home/tester/src".to_owned()).unwrap(),
    ));
    let (control_connection, closes, _, _, _) =
        reconnect_control_connection_with_provider_and_revalidation(
            "work",
            provider,
            VecDeque::from([gpui::Task::ready(Err(
                crate::terminal::TerminalSessionChannelRevalidationError::DirectoryUnavailable,
            ))]),
        );
    let backend = TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(
        control_connection,
    ))]);
    let (manager, records, cx) = workspace_manager_with_remote_backend(backend, cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-shift-o");
    cx.run_until_parked();
    let flow = manager.read_with(cx, |manager, _| {
        manager.remote_workspace_flow.clone().unwrap()
    });
    cx.update(|window, cx| {
        flow.update(cx, |flow, cx| {
            crate::ui::remote_workspace_flow::select_host_destination(
                flow,
                crate::domain::SshDestination::new("work".to_owned()).unwrap(),
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert_eq!(
        flow.read_with(cx, |flow, _| flow.stage()),
        RemoteWorkspaceFlowStage::ChoosingDirectory
    );

    confirm_directory_picker_path("~/src/", cx);

    assert_eq!(
        flow.read_with(cx, |flow, _| flow.stage()),
        RemoteWorkspaceFlowStage::ChoosingDirectory
    );
    assert!(cx.debug_bounds("command-palette-panel").is_some());
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        1
    );
    assert_eq!(closes.load(Ordering::Acquire), 0);

    confirm_directory_picker_path("~/src/", cx);

    manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.len(), 2);
        assert!(manager.remote_workspace_flow.is_none());
        match manager.workspaces.active().pinned_directory() {
            Some(PinnedDirectory::Remote {
                directory,
                identity,
            }) => {
                assert_eq!(directory.as_str(), "~/src/");
                assert_eq!(identity.as_str(), "/home/tester/src");
            }
            other => panic!("expected a Remote Pinned Directory, got {other:?}"),
        }
    });
    assert!(cx.debug_bounds("command-palette-panel").is_none());
    assert_eq!(records.starts().len(), 2);
    assert_eq!(closes.load(Ordering::Acquire), 0);
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }));
}

#[gpui::test]
fn open_remote_directory_should_pin_the_new_workspace_and_reuse_it_for_the_same_directory(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    let mut closes = Vec::new();
    for _ in 0..2 {
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                assert!(manager.activate_workspace(WorkspaceId::new(1), window, cx));
            })
        });
        let flow = open_remote_workspace_flow(&manager, cx);
        let (completion, completion_closes, _, _) =
            remote_completion("work", "~/src", "/home/tester/src", true);
        emit_remote_workspace_completion(
            &flow,
            crate::ui::remote_workspace_flow::pinned(completion),
            cx,
        );
        closes.push(completion_closes);
    }

    manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.len(), 2);
        assert_eq!(manager.workspaces.active_id(), WorkspaceId::new(2));
        assert!(manager.remote_workspace_flow.is_none());
        assert_eq!(manager.remote_workspace_runtimes.len(), 1);
        match manager.workspaces.active().pinned_directory() {
            Some(PinnedDirectory::Remote {
                directory,
                identity,
            }) => {
                assert_eq!(directory.as_str(), "~/src");
                assert_eq!(identity.as_str(), "/home/tester/src");
            }
            other => panic!("expected a Remote Pinned Directory, got {other:?}"),
        }
    });
    let starts = records.starts();
    assert_eq!(starts.len(), 2);
    assert_eq!(
        starts
            .last()
            .unwrap()
            .remote_launch_plan()
            .unwrap()
            .remote_directory()
            .as_str(),
        "~/src"
    );
    assert_eq!(closes[0].load(Ordering::Acquire), 0);
    assert_eq!(closes[1].load(Ordering::Acquire), 1);
}

#[gpui::test]
fn unusable_directory_selection_should_not_pin_the_workspace(cx: &mut TestAppContext) {
    let missing = temporary_directory("missing");
    let (manager, records, cx) =
        workspace_manager_with_directory_selection([Ok(Some(missing))], cx);

    choose_pin_directory(&manager, cx);

    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        1
    );
    assert_eq!(records.starts().len(), 1);
    assert!(manager.read_with(cx, |manager, _| {
        manager.workspaces.active().pinned_directory().is_none()
    }));
    assert!(
        cx.debug_bounds("modal-action-workspace-pin-error-ok")
            .is_some()
    );
}

#[gpui::test]
fn workspace_switcher_should_open_and_block_terminal_input_focus(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);

    click("workspace-switcher", cx);

    let state = cx.update(|window, cx| {
        let manager = manager.read(cx);
        (
            window_combo_box_is_open(window, cx),
            manager.terminal_focus_blocker(window, cx),
            manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .focused_terminal_is_focused(window, cx),
        )
    });
    assert_eq!(
        state,
        (true, Some(TerminalFocusBlocker::CommandPalette), false)
    );
    assert!(cx.debug_bounds("combo-box-panel").is_some());
}

#[gpui::test]
fn workspace_switcher_should_replace_an_open_workspace_context_menu(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    right_click("workspace-row-1-active", cx);
    assert!(cx.debug_bounds("menu-panel-0").is_some());

    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.open_workspace_switcher(window, cx);
        });
    });
    cx.run_until_parked();

    redraw(cx);
    assert!(cx.debug_bounds("menu-panel-0").is_none());
    assert!(cx.debug_bounds("combo-box-panel").is_some());
    assert!(manager.read_with(cx, |manager, cx| { !manager.sidebar.read(cx).menu_open() }));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    let restored = cx.update(|window, cx| {
        let manager = manager.read(cx);
        (
            manager.sidebar.read(cx).is_focused(window),
            manager.terminal_focus_blocker(window, cx),
        )
    });
    assert_eq!(
        restored,
        (false, None),
        "closing the replacement palette returns focus to the Pane the menu displaced"
    );
}

#[gpui::test]
fn workspace_switcher_from_inline_rename_should_restore_sidebar_focus(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    right_click("workspace-row-1-active", cx);
    click("workspace-menu-row-rename", cx);
    assert!(cx.update(|window, cx| manager.read(cx).sidebar.read(cx).rename_is_focused(window)));

    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.open_workspace_switcher(window, cx);
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    let restored = cx.update(|window, cx| {
        let manager = manager.read(cx);
        (
            !manager.sidebar.read(cx).is_renaming(),
            manager.sidebar.read(cx).is_focused(window),
            manager.terminal_focus_blocker(window, cx),
        )
    });
    assert_eq!(restored, (true, true, Some(TerminalFocusBlocker::Sidebar)));
}

#[gpui::test]
fn workspace_switcher_escape_should_restore_terminal_focus(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    let terminal_was_focused = cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    });

    click("workspace-switcher", cx);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    let restored = cx.update(|window, cx| {
        let manager = manager.read(cx);
        (
            window_combo_box_is_open(window, cx),
            manager.terminal_focus_blocker(window, cx),
            manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .focused_terminal_is_focused(window, cx),
        )
    });
    assert_eq!(
        (terminal_was_focused, restored),
        (true, (false, None, true))
    );
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn open_workspace_switcher_should_remove_a_workspace_after_its_final_terminal_session_exits(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    let inactive_sender = records
        .event_sender(1)
        .expect("the initial Workspace Terminal Session must have started");
    new_desktop_workspace(cx);
    cx.run_until_parked();
    manager.update(cx, |manager, cx| {
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(1), "Alpha Workspace".to_owned())
            .expect("the inactive Workspace must remain owned");
        cx.notify();
    });

    click("workspace-switcher", cx);
    cx.simulate_keystrokes("a l p h a");
    cx.run_until_parked();
    assert!(cx.debug_bounds("workspace-switcher-result-1").is_some());

    inactive_sender
        .try_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success))
        .expect("the inactive shell exit must be delivered");
    cx.run_until_parked();

    let state = manager.read_with(cx, |manager, _| {
        (
            manager.workspaces.workspace(WorkspaceId::new(1)).is_none(),
            manager.workspaces.active_id(),
        )
    });
    assert_eq!(state, (true, WorkspaceId::new(2)));
    assert!(
        cx.update(|window, cx| window_combo_box_is_open(window, cx)),
        "the open switcher stays open without the removed Workspace"
    );
}

#[gpui::test]
fn workspace_switcher_selection_should_activate_the_matching_workspace(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
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

    click("workspace-switcher", cx);
    cx.simulate_keystrokes("a l p h a enter");
    cx.run_until_parked();

    let state = cx.update(|window, cx| {
        let manager = manager.read(cx);
        (
            manager.workspaces.active_id(),
            window_combo_box_is_open(window, cx),
            manager.terminal_focus_blocker(window, cx),
            manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .focused_terminal_is_focused(window, cx),
        )
    });
    assert_eq!(state, (WorkspaceId::new(1), false, None, true));
}

#[gpui::test]
fn sidebar_should_keep_creation_actions_at_the_bottom_without_a_header(cx: &mut TestAppContext) {
    let (_, _, cx) = workspace_manager(cx);
    assert!(cx.debug_bounds("workspace-sidebar-header").is_none());
    assert!(cx.debug_bounds("search-workspaces-button").is_none());
    let sidebar = cx.debug_bounds("workspace-sidebar").unwrap();
    let row = cx.debug_bounds("workspace-row-1-active").unwrap();
    let list = cx.debug_bounds("workspace-list").unwrap();
    let button = cx.debug_bounds("workspace-sidebar-footer").unwrap();
    assert_eq!(row.top(), sidebar.top());
    assert!(row.bottom() < list.bottom());
    assert_eq!(button.top(), list.bottom());
    assert_eq!(button.bottom(), sidebar.bottom());
    let settings = cx.debug_bounds("open-settings-button").unwrap();
    let create = cx.debug_bounds("new-workspace-button").unwrap();
    assert_eq!(settings.center().y, create.center().y);
    assert!(create.right() <= button.right());
    // Settings is application scoped and the menu opposite it adds a Workspace, so the odd one
    // out stands alone at the leading end and the creation menu sits at the other.
    assert!(settings.left() >= button.left());
    assert!(settings.right() < create.left());
    assert!(
        settings.right() - button.left() < create.left() - settings.right(),
        "settings should stand apart from the creation menu, got {settings:?} beside {create:?}"
    );
    // Both footer glyphs share one visual extent while their buttons keep the same hit target.
    for (icon_selector, target) in [
        ("open-settings-icon", settings),
        ("new-workspace-icon", create),
    ] {
        let icon = cx.debug_bounds(icon_selector).unwrap();
        assert_eq!(icon.size, gpui::size(px(15.0), px(15.0)));
        assert_eq!(target.size, gpui::size(px(28.0), px(28.0)));
        assert_eq!(icon.center(), target.center());
    }
}

#[gpui::test]
fn the_workspace_chooser_glyph_should_keep_one_size_across_sidebar_states(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    assert!(manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().visible));

    let expected = cx.update(|_, cx| {
        crate::ui::appearance::chrome(cx)
            .icons
            .metrics(crate::ui::chrome_icons::IconRole::Chrome)
            .glyph_size
    });
    let opened = cx.debug_bounds("workspace-switcher-icon").unwrap();
    assert_eq!(
        opened.size,
        gpui::size(expected, expected),
        "the chooser should take the top chrome's icon size, got {opened:?}"
    );

    click("toggle-sidebar-button", cx);
    let closed = cx.debug_bounds("workspace-switcher-icon").unwrap();

    assert_eq!(
        closed.size, opened.size,
        "the chooser's glyph should not resize with the sidebar, got {closed:?} then {opened:?}"
    );
}

#[gpui::test]
fn sidebar_settings_button_should_request_the_application_settings_action(cx: &mut TestAppContext) {
    let (_, _, cx) = workspace_manager(cx);
    let requests = std::rc::Rc::new(std::cell::Cell::new(0usize));
    cx.update(|_, cx| {
        let requests = requests.clone();
        cx.on_action(
            move |_: &crate::ui::settings_window::OpenSettings, _: &mut gpui::App| {
                requests.set(requests.get() + 1);
            },
        );
    });

    click("open-settings-button", cx);
    cx.run_until_parked();

    assert_eq!(requests.get(), 1, "the cog should ask for Settings");
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

fn click_new_workspace_menu(entry: &'static str, cx: &mut VisualTestContext) {
    click("new-workspace-button", cx);
    click(entry, cx);
}

fn press_return(cx: &mut VisualTestContext) {
    let keystroke = Keystroke::parse("enter").unwrap_or_default();
    cx.simulate_event(KeyDownEvent {
        keystroke: keystroke.clone(),
        prefer_character_input: false,
        is_held: false,
    });
    cx.simulate_event(KeyUpEvent { keystroke });
    cx.run_until_parked();
}

fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

fn active_tab_manager(
    manager: &Entity<WorkspaceManager>,
    cx: &VisualTestContext,
) -> (WorkspaceId, Entity<TabManager>) {
    manager.read_with(cx, |manager, _| {
        let workspace = manager.workspaces.active();
        (workspace.id(), workspace.payload().clone())
    })
}

#[gpui::test]
fn caption_close_should_confirm_its_owning_pane_and_restore_focus_after_cancel(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    redraw(cx);
    cx.simulate_keystrokes("cmd-d");
    redraw(cx);
    let (workspace_id, tab_manager) = active_tab_manager(&manager, cx);
    // The unfocused Pane reveals its controls under the pointer before they take a click.
    let pane = cx.debug_bounds("pane-surface-1").unwrap();
    cx.simulate_mouse_move(pane.center(), None, Modifiers::none());
    crate::ui::settle_hover(cx);
    click("pane-close-1", cx);
    redraw(cx);
    let pending = manager.read_with(cx, |manager, _| {
        manager.close_confirmation.pending().unwrap()
    });
    assert_eq!(
        pending.target,
        CloseTarget::Pane {
            workspace_id,
            tab_id: TabId::new(1),
            pane_id: PaneId::new(1)
        }
    );
    assert_eq!(
        tab_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
        (1, 2)
    );
    assert!(records.dropped_session_ids().is_empty());
    assert!(
        cx.debug_bounds("modal-action-close-confirmation-cancel-keyboard-focus")
            .is_some()
    );
    press_return(cx);
    redraw(cx);
    assert!(manager.read_with(cx, |manager, _| {
        manager.close_confirmation.pending().is_none()
    }));
    cx.simulate_keystrokes("a");
    assert!(
        records
            .commands()
            .iter()
            .any(|call| call.session_id == 1 && matches!(call.command, RecordedCommand::Key(_)))
    );
    click("pane-close-1", cx);
    redraw(cx);
    click("modal-action-close-confirmation-confirm", cx);
    redraw(cx);
    assert_eq!(
        tab_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
        (1, 1)
    );
    assert_eq!(records.dropped_session_ids(), vec![1]);
}

#[gpui::test]
fn risky_pane_close_should_follow_keyboard_focus_and_confirm_once(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    redraw(cx);
    cx.simulate_keystrokes("cmd-d");
    redraw(cx);
    let (_, tab_manager) = active_tab_manager(&manager, cx);
    assert_eq!(
        tab_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
        (1, 2)
    );

    cx.simulate_keystrokes("cmd-alt-w");
    cx.run_until_parked();
    let first_pending = manager.read_with(cx, |manager, _| {
        manager
            .close_confirmation
            .pending()
            .expect("risky close should present one confirmation")
    });
    cx.simulate_keystrokes("cmd-alt-w");
    cx.run_until_parked();

    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .close_confirmation
            .pending()
            .expect("duplicate request must keep the original confirmation")
            .generation),
        first_pending.generation
    );
    assert_eq!(
        tab_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
        (1, 2)
    );
    assert!(records.dropped_session_ids().is_empty());
    assert!(
        cx.debug_bounds("modal-action-close-confirmation-cancel-keyboard-focus")
            .is_some()
    );

    press_return(cx);
    redraw(cx);
    assert!(manager.read_with(cx, |manager, _| {
        manager.close_confirmation.pending().is_none()
    }));
    assert_eq!(
        tab_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
        (1, 2)
    );

    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.request_close(first_pending.target, window, cx)
        });
    });
    redraw(cx);
    assert!(manager.read_with(cx, |manager, _| {
        manager.close_confirmation.pending().is_some()
    }));
    assert!(
        cx.debug_bounds("modal-action-close-confirmation-cancel-keyboard-focus")
            .is_some()
    );
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("modal-action-close-confirmation-confirm-keyboard-focus")
            .is_some()
    );
    assert_eq!(
        tab_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
        (1, 2)
    );
    assert!(records.dropped_session_ids().is_empty());
    press_return(cx);

    assert_eq!(
        (
            manager.read_with(cx, |manager, _| manager.close_confirmation.pending()),
            tab_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
            records.dropped_session_ids(),
        ),
        (None, (1, 1), vec![2])
    );
}

#[gpui::test]
fn prompt_metadata_should_close_a_pane_without_confirmation(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    redraw(cx);
    cx.simulate_keystrokes("cmd-d");
    redraw(cx);
    let (_, tab_manager) = active_tab_manager(&manager, cx);
    let mut metadata = crate::terminal::metadata::MetadataTracker::new(
        crate::local_path::LocalPathSemantics::Posix,
        "/Users/test",
        "zsh",
        Default::default(),
        Instant::now(),
    );
    assert!(metadata.apply_semantic_prompt("A", Instant::now()));
    let mut screen =
        (*crate::terminal::ScreenSnapshot::empty(crate::local_path::LocalPathSemantics::Posix))
            .clone();
    screen.metadata = metadata.snapshot();
    records
        .event_sender(2)
        .expect("the focused split Pane's Terminal Session was not started")
        .try_send(TerminalSessionEvent::Screen(Arc::new(screen)))
        .unwrap();
    cx.run_until_parked();

    cx.simulate_keystrokes("cmd-alt-w");
    cx.run_until_parked();

    assert!(manager.read_with(cx, |manager, _| {
        manager.close_confirmation.pending().is_none()
    }));
    assert_eq!(
        tab_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
        (1, 1)
    );
    assert_eq!(records.dropped_session_ids(), vec![2]);
}

/// Cmd+W asks to Kill Workspace, which confirms even when no Pane is running work.
#[gpui::test]
fn command_w_should_confirm_kill_workspace_even_when_its_panes_are_idle(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    redraw(cx);
    let mut metadata = crate::terminal::metadata::MetadataTracker::new(
        crate::local_path::LocalPathSemantics::Posix,
        "/Users/test",
        "zsh",
        Default::default(),
        Instant::now(),
    );
    assert!(metadata.apply_semantic_prompt("A", Instant::now()));
    let mut screen =
        (*crate::terminal::ScreenSnapshot::empty(crate::local_path::LocalPathSemantics::Posix))
            .clone();
    screen.metadata = metadata.snapshot();
    records
        .event_sender(1)
        .expect("the Workspace's Terminal Session was not started")
        .try_send(TerminalSessionEvent::Screen(Arc::new(screen)))
        .unwrap();
    cx.run_until_parked();
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_id());

    cx.simulate_keystrokes("cmd-w");
    redraw(cx);

    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .close_confirmation
            .pending()
            .map(|pending| pending.target)),
        Some(CloseTarget::Workspace(workspace_id))
    );
    assert!(
        cx.debug_bounds("modal-action-close-confirmation-cancel-keyboard-focus")
            .is_some()
    );
    assert!(records.dropped_session_ids().is_empty());
    click("modal-action-close-confirmation-confirm", cx);
    redraw(cx);
    assert_eq!(records.dropped_session_ids(), vec![1]);
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        0,
        "nothing replaces the final Workspace"
    );
}

#[gpui::test]
fn automatic_terminal_exit_should_bypass_close_confirmation(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    redraw(cx);
    cx.simulate_keystrokes("cmd-d");
    redraw(cx);
    let (_, tab_manager) = active_tab_manager(&manager, cx);

    records
        .event_sender(2)
        .expect("the focused split Pane's Terminal Session was not started")
        .try_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success))
        .unwrap();
    cx.run_until_parked();

    assert!(manager.read_with(cx, |manager, _| {
        manager.close_confirmation.pending().is_none()
    }));
    assert_eq!(
        tab_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
        (1, 1)
    );
    assert_eq!(records.dropped_session_ids(), vec![2]);
}

#[gpui::test]
fn confirmed_stale_pane_identity_should_not_close_its_successor(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    redraw(cx);
    cx.simulate_keystrokes("cmd-d");
    redraw(cx);
    let (workspace_id, tab_manager) = active_tab_manager(&manager, cx);
    let (tab_id, pane_id) =
        tab_manager.read_with(cx, |manager, cx| manager.active_terminal_identity(cx));

    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.request_close(
                CloseTarget::Pane {
                    workspace_id,
                    tab_id,
                    pane_id,
                },
                window,
                cx,
            );
        });
        tab_manager.update(cx, |manager, cx| {
            manager.close_pane_authorized(tab_id, pane_id, window, cx)
        });
    });
    cx.run_until_parked();
    click("modal-action-close-confirmation-confirm", cx);

    assert_eq!(
        tab_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
        (1, 1)
    );
    assert_eq!(records.dropped_session_ids(), vec![2]);
}

#[gpui::test]
fn native_application_quit_should_not_compete_with_an_in_app_close_confirmation(
    cx: &mut TestAppContext,
) {
    let (manager, _, application_quit, cx) = workspace_manager_with_application_actions(cx);
    redraw(cx);

    assert!(!cx.update(|window, cx| manager.update(cx, |manager, cx| {
        manager.should_close_window(window, cx)
    })));
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .close_confirmation
            .pending()
            .map(|pending| pending.target)),
        Some(CloseTarget::Window)
    );
    cx.simulate_keystrokes("cmd-q");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .close_confirmation
            .pending()
            .map(|pending| pending.target)),
        Some(CloseTarget::Window)
    );
    assert!(!cx.has_pending_prompt());
    // Command-Q closes windows; it asks the application to quit only from the status menu.
    assert_eq!(application_quit.requests(), 0);
    click("modal-action-close-confirmation-cancel", cx);
}

#[gpui::test]
fn direct_multi_pane_tab_close_should_use_one_aggregate_confirmation(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    redraw(cx);
    cx.simulate_keystrokes("cmd-d");
    redraw(cx);
    cx.simulate_keystrokes("cmd-t");
    redraw(cx);
    cx.simulate_keystrokes("cmd-1");
    redraw(cx);
    let (workspace_id, tab_manager) = active_tab_manager(&manager, cx);

    cx.simulate_keystrokes("cmd-shift-w");
    cx.run_until_parked();

    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .close_confirmation
            .pending()
            .map(|pending| pending.target)),
        Some(CloseTarget::Tab {
            workspace_id,
            tab_id: TabId::new(1),
        })
    );
    assert_eq!(
        tab_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
        (2, 3)
    );
    assert!(records.dropped_session_ids().is_empty());

    click("modal-action-close-confirmation-confirm", cx);

    assert_eq!(
        tab_manager.read_with(cx, |manager, cx| manager.aggregate_counts(cx)),
        (1, 1)
    );
    assert_eq!(records.dropped_session_ids(), vec![1, 2]);
}

#[gpui::test]
fn direct_multi_tab_workspace_close_should_use_one_aggregate_confirmation(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    redraw(cx);
    cx.simulate_keystrokes("cmd-t");
    redraw(cx);

    cx.dispatch_action(CloseWorkspace);
    cx.run_until_parked();

    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .close_confirmation
            .pending()
            .map(|pending| pending.target)),
        Some(CloseTarget::Workspace(WorkspaceId::new(1)))
    );
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        1
    );
    assert!(records.dropped_session_ids().is_empty());

    click("modal-action-close-confirmation-confirm", cx);

    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        0
    );
    assert_eq!(records.dropped_session_ids(), vec![1, 2]);
    assert_eq!(records.session_count(), 2);
    // The empty stage remains, with nothing desktop-owned in it.
    assert!(cx.debug_bounds("empty-workspace-stage").is_some());
}

#[gpui::test]
fn native_window_close_should_cancel_then_remove_only_after_confirmation(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    redraw(cx);

    assert!(!cx.simulate_close());
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .close_confirmation
            .pending()
            .map(|pending| pending.target)),
        Some(CloseTarget::Window)
    );
    click("modal-action-close-confirmation-cancel", cx);
    assert_eq!(cx.windows().len(), 1);
    assert!(records.dropped_session_ids().is_empty());

    assert!(!cx.simulate_close());
    click("modal-action-close-confirmation-confirm", cx);

    assert!(cx.windows().is_empty());
}

#[gpui::test]
fn client_window_controls_remain_operable_during_close_confirmation(cx: &mut TestAppContext) {
    for layout in [
        gpui::WindowButtonLayout {
            left: [None; 3],
            right: [
                Some(gpui::WindowButton::Close),
                Some(gpui::WindowButton::Minimize),
                Some(gpui::WindowButton::Maximize),
            ],
        },
        gpui::WindowButtonLayout {
            left: [
                Some(gpui::WindowButton::Close),
                Some(gpui::WindowButton::Minimize),
                Some(gpui::WindowButton::Maximize),
            ],
            right: [None; 3],
        },
    ] {
        let (manager, records, cx) = workspace_manager(cx);
        cx.simulate_button_layout(Some(layout));
        cx.simulate_decorations(gpui::Decorations::Client {
            tiling: gpui::Tiling::default(),
        });
        let alert = present_test_alert(&manager, "window-close-under-alert", cx);
        redraw(cx);
        let alert_focus = cx.update(|window, cx| window.focused(cx).unwrap());
        click("window-close", cx);
        assert!(cx.update(|window, _| alert_focus.is_focused(window)));
        assert!(cx.debug_bounds("modal-action-acknowledge").is_some());
        assert!(
            cx.debug_bounds("modal-action-close-confirmation-cancel")
                .is_none()
        );
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .close_confirmation
                .pending()
                .map(|pending| pending.target)),
            Some(CloseTarget::Window)
        );
        cx.update(|window, cx| alert.dismiss(window, cx).unwrap());
        redraw(cx);
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("modal-action-close-confirmation-confirm-keyboard-focus")
                .is_some()
        );
        let pending = manager.read_with(cx, |manager, _| manager.close_confirmation.pending());
        let focused = cx.update(|window, cx| window.focused(cx).unwrap());
        click("window-minimize", cx);
        click("window-maximize", cx);
        assert_eq!(
            cx.window_requests(),
            [
                gpui::TestWindowRequest::Minimize,
                gpui::TestWindowRequest::Zoom
            ]
        );
        assert!(cx.update(|window, _| focused.is_focused(window)));
        click("toggle-sidebar-button", cx);
        assert!(manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().visible));
        click("window-close", cx);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager.close_confirmation.pending()),
            pending
        );
        assert!(cx.update(|window, _| focused.is_focused(window)));
        assert_eq!(cx.windows().len(), 1);
        assert!(records.dropped_session_ids().is_empty());
        click("modal-action-close-confirmation-cancel", cx);
        assert_eq!(cx.windows().len(), 1);
        assert!(records.dropped_session_ids().is_empty());
        click("window-close", cx);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .close_confirmation
                .pending()
                .map(|pending| pending.target)),
            Some(CloseTarget::Window),
            "the second native close activation must reopen confirmation after cancellation"
        );
        click("modal-action-close-confirmation-confirm", cx);
        assert!(
            manager.read_with(cx, |manager, _| manager
                .close_confirmation
                .pending()
                .is_none()),
            "confirmation must consume the pending close request"
        );
        assert!(cx.windows().is_empty());
    }
}

#[gpui::test]
fn command_q_closes_the_window_through_its_own_confirmation_without_quitting(
    cx: &mut TestAppContext,
) {
    let (manager, records, application_quit, cx) = workspace_manager_with_application_actions(cx);
    redraw(cx);
    cx.simulate_keystrokes("cmd-q");
    cx.run_until_parked();

    // The window's running work asks first, exactly as its close button does.
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .close_confirmation
            .pending()
            .map(|pending| pending.target)),
        Some(CloseTarget::Window)
    );
    assert_eq!(application_quit.requests(), 0);
    click("modal-action-close-confirmation-cancel", cx);
    assert!(records.dropped_session_ids().is_empty());
    assert_eq!(application_quit.requests(), 0);
}

/// A Quit confirmed while a status-menu Stop or Restart runs waits for it, then checks the
/// client's work again: a Pane opened during the wait was never covered by that confirmation.
#[gpui::test]
fn quit_waiting_for_a_server_operation_asks_again_about_new_work(cx: &mut TestAppContext) {
    let (_, records, application_quit, cx) = workspace_manager_with_application_actions(cx);
    redraw(cx);
    cx.cx.update(crate::status_menu::begin_operation);
    cx.dispatch_action(crate::app::QuitApplication);
    cx.run_until_parked();
    cx.simulate_prompt_answer("Quit SpaceTerm");
    cx.run_until_parked();
    assert_eq!(
        application_quit.confirmations(),
        0,
        "the Stop is still running"
    );

    cx.simulate_keystrokes("cmd-d");
    redraw(cx);
    // The operation ends from the application's event loop, outside any window.
    cx.cx.update(crate::status_menu::end_operation);
    cx.run_until_parked();

    assert!(
        cx.has_pending_prompt(),
        "the new Pane needs its own confirmation"
    );
    assert_eq!(application_quit.confirmations(), 0);
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert!(records.dropped_session_ids().is_empty());
    assert_eq!(application_quit.confirmations(), 0);
}

#[gpui::test]
fn quit_action_should_cancel_safely_and_confirm_once(cx: &mut TestAppContext) {
    let (manager, records, application_quit, cx) = workspace_manager_with_application_actions(cx);
    redraw(cx);
    cx.dispatch_action(crate::app::QuitApplication);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    assert_eq!(application_quit.requests(), 1);
    assert_eq!(
        application_quit.simulate_native_request(),
        crate::platform::application_quit::ApplicationQuitDecision::Cancel
    );
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert!(!cx.has_pending_prompt());
    assert!(records.dropped_session_ids().is_empty());

    cx.dispatch_action(crate::app::QuitApplication);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert!(records.dropped_session_ids().is_empty());

    assert_eq!(
        application_quit.simulate_native_request(),
        crate::platform::application_quit::ApplicationQuitDecision::Cancel
    );
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Quit SpaceTerm");
    cx.run_until_parked();

    assert_eq!(application_quit.requests(), 4);
    assert_eq!(application_quit.confirmations(), 1);
    assert!(manager.read_with(cx, |manager, _| {
        manager.close_confirmation.pending().is_none()
    }));
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
fn chrome_mouse_down_handoff_should_restore_terminal_focus_without_a_pointer_release(
    cx: &mut TestAppContext,
) {
    let (manager, platform, cx) = workspace_manager_with_operating_system_window_drag_platform(cx);
    platform.handoff_on_press();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    // Both Chrome owners use the same native adapter but retain their own interaction status.
    for selector in ["workspace-top-chrome-drag-region", "tab-bar-drag-region"] {
        let bounds = cx
            .debug_bounds(selector)
            .expect("Chrome drag region must be rendered");
        let press = bounds.center();
        cx.simulate_mouse_down(press, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        assert!(
            cx.update(|window, cx| {
                manager
                    .read(cx)
                    .workspaces
                    .active()
                    .payload()
                    .read(cx)
                    .focused_terminal_has_input_focus(window, cx)
            }),
            "Terminal Input Focus did not recover after {selector}; native counts: {:?}",
            platform.counts()
        );
        assert!(cx.update(|window, cx| {
            manager
                .update(cx, |manager, cx| manager.native_service_status(window, cx))
                .capabilities
                .return_text
        }));
    }

    assert_eq!(platform.counts(), (2, 2, 2));
}

#[gpui::test]
fn workspace_chrome_should_forward_threshold_crossing_and_double_activation_to_platform_policy(
    cx: &mut TestAppContext,
) {
    let (_manager, platform, cx) = workspace_manager_with_operating_system_window_drag_platform(cx);
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

    assert_eq!(platform.counts(), (1, 1, 1));
    assert_eq!(
        cx.window_requests(),
        vec![gpui::TestWindowRequest::TitlebarDoubleClick {
            is_resizable: true,
            is_minimizable: true
        }]
    );
}

#[gpui::test]
fn workspace_top_chrome_should_restore_after_release_outside_its_hitbox(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let command_count = records.commands().len();
    let chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("Workspace top chrome must be rendered")
        .center();
    let outside = cx
        .debug_bounds("tab-manager-content")
        .expect("Tab content must be rendered")
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
            .active()
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
            RecordedCommand::Focus(focused) => Some((call.session_id, focused)),
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
fn sidebar_should_share_one_resize_edge_across_top_chrome_and_body(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);
    let chrome_height = top_chrome_height(cx);

    let root = cx
        .debug_bounds("workspace-manager")
        .expect("the Workspace manager was not rendered");
    let chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("the fixed top-left chrome was not rendered");
    let sidebar = cx
        .debug_bounds("workspace-sidebar")
        .expect("the Workspace sidebar was not rendered");
    let active_row = cx
        .debug_bounds("workspace-row-1-active")
        .expect("the Active Workspace row was not rendered");
    let divider = cx
        .debug_bounds("workspace-sidebar-resize-handle-divider")
        .expect("the unified sidebar divider was not rendered");
    let content = cx
        .debug_bounds("tab-manager-content")
        .expect("the active Tab content was not rendered");

    assert_eq!(
        (
            chrome.size,
            sidebar.origin.x,
            sidebar.origin.y,
            sidebar.size.width,
            active_row.origin.x,
            active_row.size.width,
            divider.center().x,
            divider.origin.y,
            divider.size,
            content.origin.x,
        ),
        (
            gpui::size(px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH), chrome_height),
            px(0.0),
            chrome_height,
            px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            px(0.0),
            px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            sidebar.origin.x + sidebar.size.width,
            root.origin.y,
            gpui::size(px(CHROME_DIVIDER_SIZE), root.size.height),
            px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
        )
    );
}

#[gpui::test]
fn sidebar_divider_hover_should_preserve_full_height_hairline_geometry(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);
    let root = cx
        .debug_bounds("workspace-manager")
        .expect("the Workspace manager was not rendered");
    let hitbox = cx
        .debug_bounds("workspace-sidebar-resize-handle-hitbox")
        .expect("the regular sidebar divider hitbox was not rendered");
    let spacious_hitbox = cx
        .debug_bounds("workspace-sidebar-resize-handle-spacious-hitbox")
        .expect("the spacious top-chrome hitbox was not rendered");
    assert_eq!(hitbox.size.width, px(8.0));
    assert_eq!(
        spacious_hitbox.size,
        gpui::size(px(16.0), top_chrome_height(cx))
    );
    let workspace_drag_target = cx
        .debug_bounds("workspace-top-chrome-drag-region-hitbox")
        .expect("the Workspace chrome drag target was not rendered");
    let window_drag_target = cx
        .debug_bounds("tab-bar-drag-region-hitbox")
        .expect("the Tab chrome drag target was not rendered");
    assert_eq!(
        (workspace_drag_target.right(), window_drag_target.left()),
        (spacious_hitbox.left(), spacious_hitbox.right())
    );
    let expected = gpui::size(px(CHROME_DIVIDER_SIZE), root.size.height);
    let top = spacious_hitbox.center();
    let body = point(hitbox.center().x, root.origin.y + root.size.height / 2.0);

    cx.simulate_mouse_move(top, None, Modifiers::none());
    cx.run_until_parked();
    let top_geometry = cx
        .debug_bounds("workspace-sidebar-resize-handle-divider")
        .expect("the hovered sidebar divider was not rendered")
        .size;
    cx.simulate_mouse_move(body, None, Modifiers::none());
    cx.run_until_parked();
    let body_geometry = cx
        .debug_bounds("workspace-sidebar-resize-handle-divider")
        .expect("the hovered sidebar divider was not rendered")
        .size;

    assert_eq!((top_geometry, body_geometry), (expected, expected));
}

#[gpui::test]
fn sidebar_resize_should_reveal_its_paintless_handle_to_keyboard_focus(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);
    assert!(
        cx.debug_bounds("workspace-sidebar-resize-handle-keyboard-focus-indicator")
            .is_none(),
        "the idle sidebar edge should stay paintless"
    );

    let mut indicator = None;
    for _ in 0..64 {
        cx.update(|window, cx| window.focus_next(cx));
        cx.run_until_parked();
        indicator = cx.debug_bounds("workspace-sidebar-resize-handle-keyboard-focus-indicator");
        if indicator.is_some() {
            break;
        }
    }
    let indicator = indicator.expect("the sidebar resize tab stop should reveal its indicator");
    let root = cx
        .debug_bounds("workspace-manager")
        .expect("the Workspace manager was rendered");
    assert_eq!(indicator.size.width, px(CHROME_DIVIDER_SIZE));
    assert!(
        indicator.top() >= root.top() && indicator.bottom() <= root.bottom(),
        "the keyboard focus indicator escaped the Workspace frame"
    );
}

#[gpui::test]
fn workspace_frame_omits_structural_divider_elements(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);

    for selector in [
        "workspace-top-chrome-bottom-divider",
        "tab-bar-divider",
        "workspace-sidebar-footer-divider",
    ] {
        assert!(
            cx.debug_bounds(selector).is_none(),
            "{selector} should no longer be rendered"
        );
    }
    // The sidebar edge keeps its resize layout and target, while the sidebar and the content stage
    // paint one continuous base surface instead of a separator.
    let base = cx.update(|_, cx| crate::ui::appearance::chrome(cx).colors.panel_background);
    let stage: &'static str = format!("tab-manager-stage-surface-{:08x}", base.rgba_hex()).leak();
    assert!(
        cx.debug_bounds(stage).is_some(),
        "the content stage should paint the sidebar's base surface"
    );
    assert!(
        cx.debug_bounds("workspace-sidebar-resize-handle-hitbox")
            .is_some()
    );
}

/// The distances are taken between painted bounds, not between layout properties.
#[gpui::test]
fn content_stage_should_space_the_active_pane_with_one_measurement(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    let space = frame_space(cx);
    let window_edge_inset = workspace_frame(cx).window_edge_inset();
    let chrome_height = top_chrome_height(cx);

    for sidebar_visible in [true, false] {
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                if manager.sidebar.read(cx).layout().visible != sidebar_visible {
                    manager.toggle_sidebar(window, cx);
                }
            });
        });
        cx.run_until_parked();

        let root = cx
            .debug_bounds("workspace-manager")
            .expect("the Workspace manager was rendered");
        let content = cx
            .debug_bounds("tab-manager-content")
            .expect("the active Tab content was rendered");
        let pane = cx
            .debug_bounds("pane-surface-1")
            .expect("the floating Pane surface was rendered");
        let content_left = if sidebar_visible {
            root.origin.x + px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH)
        } else {
            root.origin.x
        };

        let (leading_neighbour, leading_inset) = if sidebar_visible {
            (
                cx.debug_bounds("workspace-row-selection-1")
                    .expect("the Active Workspace chip was rendered")
                    .right(),
                space,
            )
        } else {
            (root.left(), window_edge_inset)
        };
        assert_eq!(
            (
                content.origin.x,
                content.origin.y,
                pane.left() - leading_neighbour,
                pane.top() - content.top(),
                root.right() - pane.right(),
                root.bottom() - pane.bottom(),
            ),
            (
                content_left,
                root.origin.y + chrome_height,
                leading_inset,
                px(0.0),
                window_edge_inset,
                window_edge_inset,
            ),
            "sidebar visible: {sidebar_visible}"
        );
    }
}

#[gpui::test]
fn first_tab_chip_should_keep_one_space_from_the_workspace_identity(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    let space = frame_space(cx);

    for sidebar_visible in [true, false] {
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                if manager.sidebar.read(cx).layout().visible != sidebar_visible {
                    manager.toggle_sidebar(window, cx);
                }
            });
        });
        cx.run_until_parked();

        let switcher = cx
            .debug_bounds("workspace-switcher")
            .expect("the Workspace chooser was rendered");
        let chip = cx
            .debug_bounds("tab-item-1-chip")
            .expect("the Active Tab chip was rendered");
        assert_eq!(
            chip.left() - switcher.right(),
            space,
            "the identity and the first Tab should leave one visible space \
             (sidebar visible: {sidebar_visible})"
        );

        if sidebar_visible {
            let pane = cx
                .debug_bounds("pane-surface-1")
                .expect("the floating Pane surface was rendered");
            assert_eq!(
                chip.left(),
                pane.left(),
                "the first Tab and the Pane beneath it should share one leading vertical"
            );
            // The identity area above the sidebar stops where a selected row's chip stops.
            let row_chip = cx
                .debug_bounds("workspace-row-selection-1")
                .expect("the Active Workspace chip was rendered");
            assert_eq!(switcher.right(), row_chip.right());
        }
    }
}

#[gpui::test]
fn workspace_switcher_uses_ghost_then_active_tab_surface(cx: &mut TestAppContext) {
    let (_, _, cx) = workspace_manager(cx);
    let surface = |selector: &'static str, cx: &mut VisualTestContext| {
        let bounds = cx.debug_bounds(selector).unwrap();
        cx.update(|window, _| {
            let bounds = bounds.scale(window.scale_factor());
            window
                .painted_quads()
                .into_iter()
                .find(|quad| quad.bounds.intersect(&quad.content_mask.bounds) == bounds)
                .map(|quad| quad.background)
        })
    };
    let resting = surface("workspace-switcher", cx);
    let switcher = cx.debug_bounds("workspace-switcher").unwrap();
    cx.simulate_mouse_move(switcher.center(), None, Modifiers::none());
    crate::ui::settle_hover(cx);
    assert_ne!(
        surface("workspace-switcher", cx),
        resting,
        "expanded switcher needs Ghost hover feedback"
    );

    click("toggle-sidebar-button", cx);
    cx.simulate_mouse_move(point(px(0.0), px(200.0)), None, Modifiers::none());
    crate::ui::settle_hover(cx);
    let tab = surface("tab-item-1-chip", cx).expect("active Tab surface");
    assert_eq!(
        surface("workspace-switcher", cx),
        Some(tab),
        "collapsed switcher must share active Tab material"
    );
}

/// The collapsed switcher matches the Tab surface height across densities.
#[gpui::test]
fn collapsed_workspace_switcher_should_match_tab_height(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);
    click("toggle-sidebar-button", cx);
    assert!(cx.debug_bounds("workspace-switcher-chip").is_none());

    for spacing_scale in [1.0, 1.25] {
        cx.update(|window, cx| {
            let appearance = crate::ui::appearance::ChromeAppearance {
                spacing_scale,
                ..crate::ui::appearance::ChromeAppearance::default()
            };
            cx.set_global(crate::ui::appearance::InstalledChrome::single(Arc::new(
                appearance,
            )));
            window.refresh();
        });
        cx.run_until_parked();
        let workspace = cx.debug_bounds("workspace-chip").unwrap();
        let tab = cx.debug_bounds("tab-item-1-chip").unwrap();
        let switcher = cx.debug_bounds("workspace-switcher").unwrap();
        assert_eq!(
            (switcher.top(), switcher.bottom()),
            (tab.top(), tab.bottom()),
            "switcher and Tab surface edges must align at spacing scale {spacing_scale}"
        );
        assert_eq!(workspace.center().y, tab.center().y);
        assert!(workspace.top() >= tab.top() && workspace.bottom() <= tab.bottom());
    }
}

#[gpui::test]
fn collapsed_workspace_name_should_fit_at_fractional_scales(cx: &mut TestAppContext) {
    let name = "Measured Workspace";
    let (mut text_context, _) = RecordingRenderedText::context_with_measurement(cx, name, px(27.0));
    let (manager, _records, cx) = workspace_manager(&mut text_context);
    manager.update(cx, |manager, cx| {
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(1), name.to_owned())
            .unwrap();
        cx.notify();
    });
    cx.update(|window, cx| {
        window.toggle_fullscreen();
        manager.update(cx, |manager, cx| manager.toggle_sidebar(window, cx));
    });
    let mut failures = Vec::new();
    for scale in [1.25, 1.5, 1.75] {
        cx.simulate_scale_factor_change(scale);
        cx.run_until_parked();
        let measured = cx.update(|window, cx| {
            crate::ui::appearance::chrome(cx).typography.measure(
                TextRole::BodyEmphasis,
                name,
                window,
            )
        });
        assert_eq!(measured, px(27.0));
        let label = cx
            .debug_bounds("workspace-chip-label")
            .expect("collapsed name");
        if label.size.width < measured {
            failures.push((scale, name, measured, label));
        }
    }
    assert!(
        failures.is_empty(),
        "complete collapsed switcher must fit its name: {failures:?}"
    );
}

#[gpui::test]
fn workspace_popup_name_should_fit_at_fractional_scales(cx: &mut TestAppContext) {
    let name = "Widest Workspace without a shortcut";
    let (mut text_context, _) =
        RecordingRenderedText::context_with_measurement(cx, name, px(266.9));
    let (manager, _records, cx) = workspace_manager(&mut text_context);
    for _ in 0..9 {
        new_desktop_workspace(cx);
    }
    manager.update(cx, |manager, cx| {
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(10), name.to_owned())
            .unwrap();
        cx.notify();
    });
    cx.run_until_parked();
    click("workspace-switcher", cx);
    let mut failures = Vec::new();
    for scale in [1.25, 1.5, 1.75] {
        cx.simulate_scale_factor_change(scale);
        cx.run_until_parked();
        let measured = cx.update(|window, cx| {
            crate::ui::appearance::chrome(cx)
                .typography
                .measure(TextRole::Body, name, window)
        });
        assert_eq!(measured, px(266.9));
        assert!(cx.debug_bounds("combo-box-row-9-shortcut").is_none());
        let label = cx
            .debug_bounds("combo-box-row-9-label")
            .expect("tenth Workspace label");
        if label.size.width < measured {
            failures.push((scale, measured, label));
        }
    }
    assert!(
        failures.is_empty(),
        "complete Workspace popup must fit its widest row below the width limit: {failures:?}"
    );
}

/// Draws the frame 30ms into a sidebar motion and reports the sidebar, the stage, the Pane, and
/// whether the collapsed Workspace identity shows.
fn sidebar_motion_frame(
    cx: &mut VisualTestContext,
) -> (
    Option<gpui::Bounds<Pixels>>,
    gpui::Bounds<Pixels>,
    gpui::Bounds<Pixels>,
    bool,
) {
    cx.run_until_parked();
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(30));
    cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();
    sidebar_presentation(cx)
}

fn sidebar_presentation(
    cx: &mut VisualTestContext,
) -> (
    Option<gpui::Bounds<Pixels>>,
    gpui::Bounds<Pixels>,
    gpui::Bounds<Pixels>,
    bool,
) {
    (
        cx.debug_bounds("workspace-sidebar"),
        // A moving stage is laid out where it rests and drawn where the sidebar leaves it.
        cx.debug_bounds("tab-manager-moving-stage")
            .or_else(|| cx.debug_bounds("tab-manager-content"))
            .unwrap(),
        cx.debug_bounds("pane-surface-1").unwrap(),
        cx.debug_bounds("workspace-chip-label").is_some(),
    )
}

#[gpui::test]
fn toggling_the_sidebar_should_slide_it_while_the_terminal_resizes_once(cx: &mut TestAppContext) {
    use crate::ui::drag_and_drop::AnimatePresence;

    let (manager, records, cx) = workspace_manager(cx);
    let resizes = || {
        records
            .commands()
            .iter()
            .filter(|call| matches!(call.command, RecordedCommand::Resize(_)))
            .count()
    };
    let (Some(sidebar), stage, pane, false) = sidebar_presentation(cx) else {
        panic!("the sidebar must start shown");
    };
    let before = resizes();
    cx.update(|_, cx| cx.set_global(AnimatePresence));

    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.toggle_sidebar(window, cx)));
    let hiding = sidebar_motion_frame(cx);
    let hiding_resizes = resizes();
    finish_slides(cx);
    let hidden = sidebar_presentation(cx);
    let hidden_resizes = resizes();
    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.toggle_sidebar(window, cx)));
    let showing = sidebar_motion_frame(cx);
    finish_slides(cx);
    let shown = sidebar_presentation(cx);

    // The collapsed Workspace name eases in as the sidebar slides out, and out as it slides in.
    let (Some(hiding_sidebar), hiding_stage, hiding_pane, true) = hiding else {
        panic!("the sidebar must still show beside the easing Workspace name: {hiding:?}");
    };
    assert!(
        hiding_sidebar.left() < sidebar.left() && hiding_sidebar.right() > px(0.0),
        "the sidebar must be sliding out: {hiding_sidebar:?} from {sidebar:?}"
    );
    assert!(
        hiding_stage.left() < stage.left() && hiding_stage.left() > hidden.1.left(),
        "the stage must follow the sidebar: {hiding_stage:?} between {stage:?} and {:?}",
        hidden.1
    );
    assert_eq!(
        (hiding_pane.right(), hiding_resizes),
        (hidden.2.right(), hidden_resizes),
        "the Pane must keep its trailing edge while the Terminal takes its final size when the \
         motion starts"
    );
    assert_eq!((hidden.0, hidden.3), (None, true));
    assert_eq!(hidden_resizes, before + 1, "the Terminal must resize once");

    let (Some(showing_sidebar), showing_stage, showing_pane, true) = showing else {
        panic!("the sidebar must slide in beside the easing Workspace name: {showing:?}");
    };
    assert!(
        showing_sidebar.left() < sidebar.left() && showing_stage.left() < stage.left(),
        "the sidebar and stage must be sliding in: {showing:?}"
    );
    assert_eq!(
        showing_pane.right(),
        pane.right(),
        "the Pane must keep its trailing edge while the sidebar slides in"
    );
    assert_eq!(shown, (Some(sidebar), stage, pane, false));
    assert_eq!(resizes(), before + 2);
}

#[gpui::test]
fn every_frame_of_a_sidebar_toggle_should_keep_the_terminal_size(cx: &mut TestAppContext) {
    toggle_the_sidebar_frame_by_frame(None, None, false, cx);
}

#[gpui::test]
fn every_frame_of_a_sidebar_toggle_should_keep_the_terminal_size_beside_a_fractional_width(
    cx: &mut TestAppContext,
) {
    toggle_the_sidebar_frame_by_frame(None, Some(px(251.3)), false, cx);
}

#[gpui::test]
fn every_frame_of_a_sidebar_toggle_should_keep_the_terminal_size_at_a_fractional_scale(
    cx: &mut TestAppContext,
) {
    toggle_the_sidebar_frame_by_frame(Some(1.25), Some(px(251.3)), false, cx);
}

#[gpui::test]
fn every_frame_of_a_sidebar_toggle_should_keep_split_terminal_sizes_at_a_fractional_scale(
    cx: &mut TestAppContext,
) {
    toggle_the_sidebar_frame_by_frame(Some(1.75), Some(px(251.3)), true, cx);
}

/// Toggles the sidebar twice, drawing every frame, and asserts each Terminal resizes once per
/// toggle.
fn toggle_the_sidebar_frame_by_frame(
    scale: Option<f32>,
    sidebar_width: Option<Pixels>,
    split: bool,
    cx: &mut TestAppContext,
) {
    use crate::ui::drag_and_drop::AnimatePresence;

    let (manager, records, cx) = workspace_manager(cx);
    if let Some(scale) = scale {
        cx.simulate_scale_factor_change(scale);
    }
    // A width where the stage's fractional edges round across a Terminal cell boundary.
    cx.simulate_resize(gpui::size(px(804.0), px(600.0)));
    if let Some(width) = sidebar_width {
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.sidebar.update(cx, |sidebar, cx| {
                    sidebar.set_layout(true, width, window, cx)
                })
            })
        });
    }
    if split {
        cx.simulate_keystrokes("cmd-d");
    }
    cx.run_until_parked();
    let resizes = || {
        records
            .commands()
            .into_iter()
            .filter_map(|call| match call.command {
                RecordedCommand::Resize(geometry) => Some(geometry),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let before = resizes().len();
    cx.update(|_, cx| cx.set_global(AnimatePresence));

    for _ in 0..2 {
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.toggle_sidebar(window, cx))
        });
        cx.run_until_parked();
        for _ in 0..60 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(8));
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
        }
    }

    let after = resizes();
    let terminals = if split { 2 } else { 1 };
    assert_eq!(
        after.len(),
        before + 2 * terminals,
        "each Terminal must resize once per toggle at scale {scale:?} beside a {sidebar_width:?} \
         sidebar: {:?}",
        &after[before..]
    );
}

#[gpui::test]
fn a_closing_sidebar_should_not_take_input(cx: &mut TestAppContext) {
    use crate::ui::drag_and_drop::AnimatePresence;

    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|_, cx| cx.set_global(AnimatePresence));
    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.toggle_sidebar(window, cx)));
    sidebar_motion_frame(cx);
    let row = cx
        .debug_bounds("workspace-row-1-active")
        .expect("the closing sidebar still draws its rows");
    let position = point(row.right() - px(12.0), row.center().y);
    assert!(
        position.x > px(0.0),
        "the row must still be on screen: {row:?}"
    );

    cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    let menu = cx.debug_bounds("workspace-menu-row-rename");
    finish_slides(cx);

    let focus = cx.update(|window, cx| {
        let manager = manager.read(cx);
        (
            manager.sidebar.read(cx).is_focused(window),
            manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .focused_terminal_is_focused(window, cx),
        )
    });
    assert_eq!(
        (menu, focus),
        (None, (false, true)),
        "a press on a closing sidebar must reach neither its rows nor its menu"
    );
}

#[gpui::test]
fn a_toggle_during_a_resize_drag_should_end_the_drag(cx: &mut TestAppContext) {
    use crate::ui::drag_and_drop::AnimatePresence;

    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|_, cx| cx.set_global(AnimatePresence));
    let start = cx
        .debug_bounds("workspace-sidebar-resize-handle-hitbox")
        .expect("the shown sidebar has a resize handle")
        .center();
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.toggle_sidebar(window, cx)));
    sidebar_motion_frame(cx);

    cx.simulate_mouse_move(
        point(start.x + px(30.0), start.y),
        MouseButton::Left,
        Modifiers::none(),
    );
    finish_slides(cx);
    let during = manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().visible);
    cx.simulate_mouse_up(
        point(start.x + px(30.0), start.y),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.run_until_parked();
    let after = manager.read_with(cx, |manager, cx| {
        let sidebar = manager.sidebar.read(cx);
        (sidebar.layout().visible, sidebar.is_resizing())
    });

    assert_eq!(
        (during, after),
        (false, (false, false)),
        "moving the held pointer must not undo the toggle"
    );
}

#[gpui::test]
fn split_panes_should_stretch_across_the_stage_while_the_sidebar_slides(cx: &mut TestAppContext) {
    use crate::ui::drag_and_drop::AnimatePresence;

    let (manager, records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-d");
    cx.run_until_parked();
    let resizes = || {
        records
            .commands()
            .iter()
            .filter(|call| matches!(call.command, RecordedCommand::Resize(_)))
            .count()
    };
    let panes = |cx: &mut VisualTestContext| {
        ["pane-surface-1", "pane-surface-2"].map(|selector| cx.debug_bounds(selector).unwrap())
    };
    let shown = panes(cx);
    let before = resizes();
    cx.update(|_, cx| cx.set_global(AnimatePresence));

    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.toggle_sidebar(window, cx)));
    let (_, hiding_stage, _, _) = sidebar_motion_frame(cx);
    let hiding = panes(cx);
    finish_slides(cx);
    let hidden = panes(cx);

    assert!(
        hiding[0].left() < shown[0].left() && hiding[0].left() > hidden[0].left(),
        "the leading Pane must move with the stage {hiding_stage:?}: {hiding:?}"
    );
    assert!(
        hiding[0].size.width > shown[0].size.width && hiding[0].size.width < hidden[0].size.width,
        "the leading Pane must widen toward its final width: {hiding:?} from {shown:?} to \
         {hidden:?}"
    );
    assert_eq!(
        hiding[1].right(),
        hidden[1].right(),
        "the trailing Pane must keep its trailing edge"
    );
    assert_eq!(resizes(), before + 2, "each Terminal must resize once");
}

#[gpui::test]
fn an_unzoom_while_the_sidebar_slides_should_not_clip_the_stretched_panes(cx: &mut TestAppContext) {
    use crate::ui::drag_and_drop::AnimatePresence;

    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.toggle_sidebar(window, cx)));
    cx.simulate_keystrokes("cmd-d cmd-shift-enter");
    cx.run_until_parked();
    cx.update(|_, cx| cx.set_global(AnimatePresence));

    let frame = |cx: &mut VisualTestContext| {
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(30));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
    };
    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.toggle_sidebar(window, cx)));
    frame(cx);
    cx.simulate_keystrokes("cmd-shift-enter");
    frame(cx);

    for selector in ["pane-surface-1", "pane-surface-2"] {
        let surface = cx.debug_bounds(selector).unwrap();
        cx.update(|window, _| {
            let surface = surface.scale(window.scale_factor());
            // The surface's fill and rim are quads at its bounds, each clipped to the part it
            // draws. Together they reach both sides unless something else clips them.
            let shown = window
                .painted_quads()
                .into_iter()
                .filter(|quad| quad.bounds == surface)
                .map(|quad| quad.bounds.intersect(&quad.content_mask.bounds))
                .filter(|shown| shown.size.width.0 > 0.0 && shown.size.height.0 > 0.0)
                .map(|shown| (shown.left(), shown.right()))
                .reduce(|(left, right), (l, r)| (left.min(l), right.max(r)))
                .expect("the Pane surface should be painted");
            assert_eq!(
                shown,
                (surface.left(), surface.right()),
                "{selector} must not be clipped to its place while the stage stretches"
            );
        });
    }
}

#[gpui::test]
fn the_workspace_glyph_should_glide_while_the_sidebar_slides(cx: &mut TestAppContext) {
    use crate::ui::drag_and_drop::AnimatePresence;

    let (manager, _records, cx) = workspace_manager(cx);
    let glyph = |cx: &mut VisualTestContext| {
        cx.debug_bounds("workspace-switcher-icon")
            .expect("the Workspace glyph shows in every frame")
            .left()
    };
    let expanded = glyph(cx);
    cx.update(|_, cx| cx.set_global(AnimatePresence));

    for hidden in [true, false] {
        let from = glyph(cx);
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| manager.toggle_sidebar(window, cx))
        });
        cx.run_until_parked();
        let mut frames = vec![from];
        let mut labelled = Vec::new();
        for _ in 0..60 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(8));
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
            frames.push(glyph(cx));
            labelled.push(cx.debug_bounds("workspace-chip-label").is_some());
        }
        let to = *frames.last().unwrap();
        let travel = (to - from).abs();
        let largest_step = frames
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(px(0.0), Pixels::max);
        assert!(
            travel > px(0.0) && largest_step < travel / 4.0,
            "the glyph must glide from {from:?} to {to:?}, not jump: {frames:?}"
        );
        assert_eq!(
            (labelled[0], *labelled.last().unwrap()),
            (true, hidden),
            "the Workspace name must ease in or out as the sidebar moves: {labelled:?}"
        );
    }
    assert_eq!(glyph(cx), expanded);
}

/// The Workspace identity keeps its glyph in both sidebar states.
#[gpui::test]
fn workspace_identity_should_keep_its_icon_when_the_sidebar_is_hidden(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    let expanded_strip = cx
        .debug_bounds("tab-items")
        .expect("the Tab strip was rendered");
    assert!(cx.debug_bounds("workspace-switcher-icon").is_some());

    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| manager.toggle_sidebar(window, cx));
    });
    cx.run_until_parked();

    let icon = cx
        .debug_bounds("workspace-switcher-icon")
        .expect("the collapsed Workspace chip should keep the Workspace glyph");
    let label = cx
        .debug_bounds("workspace-chip-label")
        .expect("the collapsed Workspace name was rendered");
    let strip = cx
        .debug_bounds("tab-items")
        .expect("the Tab strip was rendered");
    assert!(icon.size.width > px(0.0));
    assert!(
        icon.right() <= label.left(),
        "the glyph should lead the Workspace name, got {icon:?} and {label:?}"
    );
    assert_eq!(
        strip.size.height, expanded_strip.size.height,
        "collapsing the sidebar should not move the Tab row"
    );
    assert_eq!(strip.top(), expanded_strip.top());
}

#[gpui::test]
fn sidebar_resize_target_should_track_scaled_top_chrome_in_both_layout_states(
    cx: &mut TestAppContext,
) {
    let (_manager, _records, cx) = workspace_manager(cx);
    let appearance = crate::ui::appearance::ChromeAppearance {
        spacing_scale: 1.25,
        ..crate::ui::appearance::ChromeAppearance::default()
    };
    cx.update(|window, cx| {
        cx.set_global(crate::ui::appearance::InstalledChrome::single(Arc::new(
            appearance,
        )));
        window.refresh();
    });
    cx.run_until_parked();

    let expanded_chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("the expanded top-left Chrome was not rendered");
    let expanded_target = cx
        .debug_bounds("workspace-sidebar-resize-handle-spacious-hitbox")
        .expect("the expanded spacious resize target was not rendered");

    click("toggle-sidebar-button", cx);

    let collapsed_chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("the collapsed top-left Chrome was not rendered");
    let collapsed_target = cx
        .debug_bounds("workspace-sidebar-resize-handle-spacious-hitbox")
        .expect("the collapsed spacious resize target was not rendered");

    assert_eq!(
        (expanded_target.size.height, collapsed_target.size.height,),
        (expanded_chrome.size.height, collapsed_chrome.size.height,)
    );
    assert!(expanded_chrome.size.height > px(TOP_CHROME_HEIGHT));
}

#[gpui::test]
fn dragging_sidebar_divider_at_top_chrome_edges_should_not_move_window(cx: &mut TestAppContext) {
    let (manager, platform, cx) = workspace_manager_with_operating_system_window_drag_platform(cx);

    for leading_edge in [true, false] {
        for top_edge in [true, false] {
            let hitbox = cx
                .debug_bounds("workspace-sidebar-resize-handle-spacious-hitbox")
                .expect("the spacious top-chrome hitbox was not rendered");
            let start_x = if leading_edge {
                hitbox.left() + px(0.5)
            } else {
                hitbox.right() - px(0.5)
            };
            let start_y = if top_edge {
                hitbox.top() + px(0.5)
            } else {
                hitbox.bottom() - px(0.5)
            };
            let start = point(start_x, start_y);
            let destination = point(start.x + px(20.0), start.y);

            cx.simulate_mouse_move(start, None, Modifiers::none());
            cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
            cx.simulate_mouse_move(
                point(start.x + px(12.0), start.y),
                MouseButton::Left,
                Modifiers::none(),
            );
            cx.simulate_mouse_move(destination, MouseButton::Left, Modifiers::none());
            cx.simulate_mouse_up(destination, MouseButton::Left, Modifiers::none());
            cx.run_until_parked();
        }
    }

    assert_eq!(
        (
            manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().width),
            platform.counts(),
        ),
        (px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH + 80.0), (0, 0, 0),)
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

    let layout = manager.read_with(cx, |manager, cx| {
        (
            manager.sidebar.read(cx).layout().visible,
            manager.sidebar.read(cx).layout().width,
        )
    });
    let chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("the persistent top-left chrome was not rendered");
    let sidebar = cx
        .debug_bounds("workspace-sidebar")
        .expect("the resized Workspace sidebar was not rendered");
    let content = cx
        .debug_bounds("tab-manager-content")
        .expect("the active Tab content was not rendered");
    let tab_bar = cx
        .debug_bounds("tab-bar")
        .expect("the Tab bar was not rendered");
    assert_eq!(
        (
            layout,
            chrome.size.width,
            sidebar.size.width,
            content.origin.x,
            tab_bar.origin.x,
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
        manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().width),
        maximum
    );
}

#[gpui::test]
fn sidebar_resize_interaction_should_block_then_restore_terminal_focus(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    let start = cx
        .debug_bounds("workspace-sidebar-resize-handle-hitbox")
        .expect("the shared sidebar handle was rendered")
        .center();

    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    let active = cx.update(|window, cx| {
        let manager = manager.read(cx);
        (
            manager.sidebar.read(cx).is_resizing(),
            manager.terminal_focus_blocker(window, cx),
            manager
                .workspaces
                .active()
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
            manager.sidebar.read(cx).is_resizing(),
            manager.terminal_focus_blocker(window, cx),
            manager
                .workspaces
                .active()
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
        .debug_bounds("workspace-sidebar-resize-handle-hitbox")
        .expect("the unified sidebar handle was rendered")
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
            manager.sidebar.read(cx).layout().width,
            manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .focused_terminal_is_focused(window, cx),
        )
    });
    assert_eq!(state, (px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH), true));
}

#[gpui::test]
fn dragging_sidebar_below_minimum_should_collapse_it_at_the_minimum_width(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    let collapsed_width = cx.update(|window, cx| {
        WorkspaceChromeLayout::collapsed_width(
            &chrome_identity(manager.read(cx).workspaces.active()),
            window,
            cx,
        )
    });
    let root = cx
        .debug_bounds("workspace-manager")
        .expect("the Workspace manager was not rendered");

    drag_to(
        "workspace-sidebar-resize-handle",
        root.origin.x + px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH - 20.0),
        cx,
    );

    let layout = manager.read_with(cx, |manager, cx| {
        (
            manager.sidebar.read(cx).layout().visible,
            manager.sidebar.read(cx).layout().width,
        )
    });
    let chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("the persistent top-left chrome was not rendered");
    let content = cx
        .debug_bounds("tab-manager-content")
        .expect("the active Tab content was not rendered");
    let tab_bar = cx
        .debug_bounds("tab-bar")
        .expect("the Tab bar was not rendered");
    assert_eq!(
        (
            layout,
            chrome.size.width,
            content.origin.x,
            tab_bar.origin.x,
        ),
        (
            (false, px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH)),
            collapsed_width,
            root.origin.x,
            root.origin.x + collapsed_width,
        )
    );
}

#[gpui::test]
fn dragging_the_collapsed_handle_should_reopen_from_the_top_chrome_edge(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    click("toggle-sidebar-button", cx);
    let root = cx
        .debug_bounds("workspace-manager")
        .expect("the Workspace manager was not rendered");
    let chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("the collapsed top-left chrome was not rendered");
    let requested_width = (chrome.size.width + px(40.0)).max(px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH));

    drag_to(
        "workspace-sidebar-resize-handle",
        root.origin.x + requested_width,
        cx,
    );

    assert_eq!(
        manager.read_with(cx, |manager, cx| {
            (
                manager.sidebar.read(cx).layout().visible,
                manager.sidebar.read(cx).layout().width,
            )
        }),
        (true, requested_width)
    );
}

#[gpui::test]
fn collapsed_handle_should_preserve_the_remembered_width_when_dragged_left(
    cx: &mut TestAppContext,
) {
    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.resize_sidebar(px(320.0), window, cx);
        });
    });
    click("toggle-sidebar-button", cx);
    let root = cx
        .debug_bounds("workspace-manager")
        .expect("the Workspace manager was not rendered");
    let chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("the collapsed top-left chrome was not rendered");

    drag_to(
        "workspace-sidebar-resize-handle",
        root.origin.x + chrome.size.width - px(20.0),
        cx,
    );
    assert_eq!(
        manager.read_with(cx, |manager, cx| {
            (
                manager.sidebar.read(cx).layout().visible,
                manager.sidebar.read(cx).layout().width,
            )
        }),
        (false, px(320.0)),
        "dragging the collapsed edge inward must retain its remembered expanded width"
    );
    click("toggle-sidebar-button", cx);

    assert_eq!(
        manager.read_with(cx, |manager, cx| {
            (
                manager.sidebar.read(cx).layout().visible,
                manager.sidebar.read(cx).layout().width,
            )
        }),
        (true, px(320.0))
    );
}

#[gpui::test]
fn escape_should_restore_the_collapsed_sidebar_and_its_remembered_width(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.resize_sidebar(px(320.0), window, cx);
        });
    });
    click("toggle-sidebar-button", cx);
    let root = cx
        .debug_bounds("workspace-manager")
        .expect("the Workspace manager was not rendered");
    let handle = cx
        .debug_bounds("workspace-sidebar-resize-handle-hitbox")
        .expect("the collapsed sidebar handle was rendered");

    cx.simulate_mouse_down(handle.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        point(root.origin.x + px(240.0), handle.center().y),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_keystrokes("escape");
    cx.simulate_mouse_up(handle.center(), MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(
        manager.read_with(cx, |manager, cx| {
            (
                manager.sidebar.read(cx).layout().visible,
                manager.sidebar.read(cx).layout().width,
            )
        }),
        (false, px(320.0))
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
    let sidebar_visible =
        manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().visible);
    assert!(
        !sidebar_visible,
        "the sidebar resize did not collapse the body"
    );
    let terminal = cx
        .debug_bounds("tab-manager-content")
        .expect("the Terminal Session content was rendered")
        .center();
    cx.simulate_mouse_move(terminal, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(terminal, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(records.pointer_count(), 0);
}

#[gpui::test]
fn selected_workspace_should_use_an_inset_chip_without_row_separators(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
    new_desktop_workspace(cx);
    cx.run_until_parked();

    let first_row = cx
        .debug_bounds("workspace-row-1-inactive")
        .expect("the inactive Workspace row was not rendered");
    let second_row = cx
        .debug_bounds("workspace-row-2-inactive")
        .expect("the second inactive Workspace row was not rendered");
    let third_row = cx
        .debug_bounds("workspace-row-3-active")
        .expect("the Active Workspace row was not rendered");
    let selection = cx
        .debug_bounds("workspace-row-selection-3")
        .expect("the Active Workspace selection was not rendered");

    let space = frame_space(cx);
    let window_edge = workspace_frame(cx).window_edge();
    assert_eq!(
        selection,
        gpui::bounds(
            point(
                third_row.origin.x + window_edge + space,
                third_row.origin.y + px(SIDEBAR_ROW_SELECTION_INSET_Y),
            ),
            gpui::size(
                third_row.size.width - window_edge - space - space,
                third_row.size.height - px(SIDEBAR_ROW_SELECTION_INSET_Y * 2.0),
            ),
        ),
        "the selected Workspace material should float inside its row"
    );
    let pane = cx
        .debug_bounds("pane-surface-1")
        .expect("the floating Pane surface was rendered");
    let root = cx
        .debug_bounds("workspace-manager")
        .expect("the Workspace manager was rendered");
    assert_eq!(
        (
            selection.left() - root.left() - window_edge,
            pane.left() - selection.right(),
        ),
        (space, space),
        "the selected Workspace chip should rest on equal visible air"
    );
    for row in 1..=3 {
        let selector: &'static str = format!("workspace-row-divider-{row}").leak();
        assert!(
            cx.debug_bounds(selector).is_none(),
            "Workspace row {row} should rest on the base surface without a separator"
        );
    }
    assert_eq!(
        (first_row.size, second_row.size),
        (third_row.size, third_row.size)
    );
}

#[gpui::test]
fn top_workspace_chooser_should_open_below_its_icon_without_dragging_the_window(
    cx: &mut TestAppContext,
) {
    let (manager, platform, cx) = workspace_manager_with_operating_system_window_drag_platform(cx);
    let chooser = cx
        .debug_bounds("workspace-switcher")
        .expect("the Workspace chooser should be in the top chrome");
    let toggle = cx
        .debug_bounds("toggle-sidebar-button")
        .expect("sidebar toggle");
    assert_eq!(chooser.size, toggle.size);
    let chrome = cx.debug_bounds("workspace-top-chrome").unwrap();
    assert_eq!(toggle.left(), chrome.left() + frame_space(cx));
    assert!(toggle.right() < chooser.left());
    let tabs = cx.debug_bounds("tab-bar").unwrap();
    assert_eq!(tabs.left() - chooser.right(), frame_space(cx));

    click("workspace-switcher", cx);

    let panel = cx
        .debug_bounds("combo-box-panel")
        .expect("Workspace chooser popup");
    assert_eq!(panel.top(), chooser.bottom() + px(4.0));
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        1
    );
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert_eq!(platform.counts(), (0, 0, 0));
}

#[gpui::test]
fn top_workspace_chooser_should_remain_available_with_the_sidebar_collapsed(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    click("toggle-sidebar-button", cx);
    let chooser = cx.debug_bounds("workspace-switcher").expect("top chooser");
    let chip = cx
        .debug_bounds("workspace-chip")
        .expect("collapsed Workspace chip");
    assert!(chooser.left() <= chip.left() && chip.right() <= chooser.right());

    click("workspace-switcher", cx);

    assert!(!manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().visible));
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    let panel = cx
        .debug_bounds("combo-box-panel")
        .expect("Workspace chooser popup");
    assert_eq!(panel.top(), chooser.bottom() + px(4.0));
}

/// Disabled parity while Remote is unavailable is covered by
/// `top_combo_box_unavailable_remote_should_reject_acceptance_and_keep_terminal_input_blocked`.
#[gpui::test]
fn sidebar_new_workspace_menu_should_mirror_switcher_creation_rows(cx: &mut TestAppContext) {
    use crate::desktop_profile::testing_presentation;
    use crate::ui::workspace_creation::WorkspaceCreation;

    // The shared descriptor source both surfaces build from.
    assert_eq!(
        WorkspaceCreation::ALL.map(WorkspaceCreation::label),
        [
            "Local Workspace",
            "Remote Workspace",
            "Open Local Directory…",
            "Open Remote Directory…",
        ]
    );
    let presentation = cx.update(|_| testing_presentation());
    assert_eq!(
        WorkspaceCreation::ALL.map(|creation| creation.shortcut(&presentation)),
        [
            Some("Primary+N".into()),
            Some("Primary+Shift+N".into()),
            Some("Primary+O".into()),
            Some("Primary+Shift+O".into()),
        ]
    );

    let (_, _, cx) = workspace_manager(cx);
    open_workspace_switcher_for_creation(cx);
    let switcher_rows = WorkspaceCreation::ALL.map(|creation| {
        // Debug selectors are static; leaking four test strings is harmless.
        let selector: &'static str = format!("workspace-switcher-{}", creation.selector()).leak();
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing switcher creation row: {selector}"))
    });
    assert!(switcher_rows.is_sorted_by_key(|row| row.top()));
    assert!(
        cx.debug_bounds("workspace-switcher-open-local-directory-group-separator")
            .is_some(),
        "the Open rows should start a separate switcher group"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    click("new-workspace-button", cx);
    assert!(cx.update(|window, cx| spaceterm_ui::window_menu_is_open(window, cx)));
    let [local, remote, open_local, open_remote] = WorkspaceCreation::ALL.map(|creation| {
        // Debug selectors are static; leaking four test strings is harmless.
        let selector: &'static str = format!("new-workspace-menu-{}", creation.selector()).leak();
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing creation row: {selector}"))
    });
    assert!([local, remote, open_local, open_remote].is_sorted_by_key(|row| row.top()));
    assert!(
        open_local.top() - remote.bottom() > remote.top() - local.bottom(),
        "a separator should divide the New rows from the Open rows"
    );
    assert_eq!(
        open_remote.top() - open_local.bottom(),
        remote.top() - local.bottom()
    );
}

#[gpui::test]
fn top_chrome_buttons_should_toggle_sidebar_and_present_the_new_workspace_combo_box(
    cx: &mut TestAppContext,
) {
    let (manager, _records, cx) = workspace_manager(cx);

    click("toggle-sidebar-button", cx);
    assert!(!manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().visible));

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    click("workspace-switcher", cx);

    assert_eq!(
        cx.update(|window, cx| {
            (
                manager.read(cx).workspaces.len(),
                window_combo_box_is_open(window, cx),
            )
        }),
        (1, true)
    );

    let panel = cx
        .debug_bounds("combo-box-panel")
        .expect("the New Workspace ComboBox panel should render");
    let workspace = cx
        .debug_bounds("workspace-manager")
        .expect("the Workspace root should render");
    assert!(panel.left() >= workspace.left() && panel.right() <= workspace.right());
    assert!(panel.size.width >= px(240.0) && panel.size.width <= px(420.0));
    let remote_label = cx
        .debug_bounds("combo-box-row-2-label")
        .expect("the Remote Workspace label should render");
    let remote_shortcut = cx
        .debug_bounds("combo-box-row-2-shortcut")
        .expect("the Remote Workspace shortcut should render");
    assert!(remote_shortcut.left() - remote_label.right() >= px(24.0));
    let chooser = cx
        .debug_bounds("workspace-switcher")
        .expect("the top chooser should render");
    assert_eq!(panel.top(), chooser.bottom() + px(4.0));
    assert_eq!(
        cx.debug_bounds("combo-box-input-row")
            .expect("the compact ComboBox input row should render")
            .size
            .height,
        px(28.0)
    );
    cx.simulate_keystrokes("f r e s h");
    cx.run_until_parked();
    for selector in [
        "workspace-switcher-create-local",
        "workspace-switcher-create-remote",
    ] {
        let row = cx
            .debug_bounds(selector)
            .expect("the compact Workspace source row should render");
        // Selector rows share the chrome control height with the trigger and the filter above them.
        assert_eq!(row.size.height, px(28.0));
        assert_eq!(
            row.left() - panel.left(),
            panel.right() - row.right(),
            "{selector} should have equal left and right insets"
        );
    }
}

#[gpui::test]
fn workspace_pin_indicator_should_track_explicit_pin_state(cx: &mut TestAppContext) {
    let directory = temporary_directory("pinned-directory");
    fs::create_dir_all(&directory).unwrap();
    let (manager, records, cx) =
        workspace_manager_with_directory_selection([Ok(Some(directory.clone()))], cx);
    assert!(cx.debug_bounds("workspace-row-pin-1").is_none());
    choose_pin_directory(&manager, cx);
    assert!(cx.debug_bounds("workspace-row-pin-1").is_some());
    assert_eq!(records.starts().len(), 1);
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        1
    );
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.apply_directory_pin(WorkspaceId::new(1), None, window, cx);
        })
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("workspace-row-pin-1").is_none());
    assert_eq!(records.starts().len(), 1);
    fs::remove_dir_all(directory).unwrap();
}

#[gpui::test]
fn the_workspace_chip_should_appear_only_while_the_sidebar_is_hidden(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);

    assert!(
        cx.debug_bounds("workspace-chip").is_none(),
        "the sidebar already answers which Workspace is active"
    );

    click("toggle-sidebar-button", cx);

    assert!(!manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().visible));
    assert!(
        cx.debug_bounds("workspace-chip").is_some(),
        "nothing named the Active Workspace once the sidebar closed"
    );

    cx.simulate_keystrokes("cmd-b");
    redraw(cx);
    // The unified keyed ResizeHandle persists across visibility changes, so the GPUI test
    // inspector needs one more refresh to retire selectors from the preceding frame.
    redraw(cx);

    assert!(
        cx.debug_bounds("workspace-chip").is_none(),
        "the chip outlived the sidebar it stands in for"
    );
}

#[gpui::test]
fn the_workspace_chip_should_follow_the_active_workspace(cx: &mut TestAppContext) {
    let (mut text_context, rendered_text) = RecordingRenderedText::context(cx);
    let cx = &mut text_context;
    let (manager, _, cx) = workspace_manager(cx);

    click("toggle-sidebar-button", cx);
    new_desktop_workspace(cx);
    manager.update(cx, |manager, cx| {
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(2), "New Active Workspace".into())
            .unwrap();
        cx.notify();
    });
    redraw(cx);

    let chip = cx
        .debug_bounds("workspace-chip")
        .expect("the chip was not rendered for the new Active Workspace");
    let chooser = cx
        .debug_bounds("workspace-switcher")
        .expect("the Workspace chooser was not rendered");

    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        2
    );
    assert!(
        chooser.left() <= chip.left() && chip.right() <= chooser.right(),
        "the chip escaped the Workspace chooser: {chip:?} {chooser:?}"
    );
    assert_rendered_text(
        "workspace-chip-label",
        "New Active Workspace",
        &rendered_text,
        cx,
    );
}

#[gpui::test]
fn collapsed_top_chrome_should_ignore_a_larger_resized_sidebar_width(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    manager.update(cx, |manager, cx| {
        manager
            .workspaces
            .rename_workspace(
                WorkspaceId::new(1),
                "A Workspace Name That Must Be Truncated".to_owned(),
            )
            .expect("the Active Workspace should be renamed");
        cx.notify();
    });
    let root = cx
        .debug_bounds("workspace-manager")
        .expect("the Workspace manager was not rendered");
    drag_to(
        "workspace-sidebar-resize-handle",
        root.origin.x + px(320.0),
        cx,
    );

    click("toggle-sidebar-button", cx);

    let chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("the collapsed top-left chrome was not rendered");
    let spacer = cx
        .debug_bounds("tab-manager-top-spacer")
        .expect("the collapsed top-left spacer was not rendered");
    let tab_bar = cx
        .debug_bounds("tab-bar")
        .expect("the Tab bar was not rendered");
    let divider = cx
        .debug_bounds("workspace-sidebar-resize-handle-divider")
        .expect("the collapsed sidebar divider was not rendered");
    let collapsed_width = cx.update(|window, cx| {
        WorkspaceChromeLayout::collapsed_width(
            &WorkspaceChromeIdentity {
                name: "A Workspace Name That Must Be Truncated".to_owned(),
                worktree: None,
                pinned: false,
                status: None,
            },
            window,
            cx,
        )
    });
    assert_eq!(
        (
            chrome.size.width,
            spacer.size.width,
            tab_bar.origin.x,
            divider.center().x,
        ),
        (
            collapsed_width,
            collapsed_width,
            root.origin.x + collapsed_width,
            root.origin.x + collapsed_width,
        )
    );
}

#[gpui::test]
fn collapsed_top_chrome_should_fit_a_short_workspace_name(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    manager.update(cx, |manager, cx| {
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(1), "A".to_owned())
            .expect("the Active Workspace should be renamed");
        cx.notify();
    });

    click("toggle-sidebar-button", cx);

    let root = cx
        .debug_bounds("workspace-manager")
        .expect("the Workspace manager was not rendered");
    let chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("the collapsed top-left chrome was not rendered");
    let spacer = cx
        .debug_bounds("tab-manager-top-spacer")
        .expect("the collapsed top-left spacer was not rendered");
    let tab_bar = cx
        .debug_bounds("tab-bar")
        .expect("the Tab bar was not rendered");
    assert!(chrome.size.width < px(212.0));
    assert_eq!(
        (spacer.size.width, tab_bar.origin.x),
        (chrome.size.width, root.origin.x + chrome.size.width)
    );
}

#[gpui::test]
fn collapsed_workspace_switcher_should_stop_at_the_tab_item_maximum(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    manager.update(cx, |manager, cx| {
        manager
            .workspaces
            .rename_workspace(
                WorkspaceId::new(1),
                "A Workspace Name That Must Be Truncated".to_owned(),
            )
            .expect("the Active Workspace should be renamed");
        cx.notify();
    });

    click("toggle-sidebar-button", cx);

    let switcher = cx
        .debug_bounds("workspace-switcher")
        .expect("the collapsed Workspace switcher was not rendered");
    let expected = cx.update(|_, cx| {
        let appearance = crate::ui::appearance::chrome(cx);
        appearance.spacing(super::super::tab_manager::TAB_ITEM_MAXIMUM_WIDTH)
    });
    assert_eq!(switcher.size.width, expected);
}

#[gpui::test]
fn collapsed_top_chrome_should_fit_a_short_name_with_its_pin_indicator(cx: &mut TestAppContext) {
    let directory = temporary_directory("collapsed-pin");
    fs::create_dir_all(&directory).unwrap();
    let (manager, _, cx) =
        workspace_manager_with_directory_selection([Ok(Some(directory.clone()))], cx);
    choose_pin_directory(&manager, cx);
    manager.update(cx, |manager, cx| {
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(1), "A".into())
            .unwrap();
        cx.notify();
    });
    click("toggle-sidebar-button", cx);
    let pinned_chrome = cx.debug_bounds("workspace-top-chrome").unwrap();
    let label = cx.debug_bounds("workspace-chip-label").unwrap();
    let pin = cx.debug_bounds("workspace-chip-pin").unwrap();
    let switcher_icon = cx.debug_bounds("workspace-switcher-icon").unwrap();
    assert_eq!(pin.left() - switcher_icon.right(), px(8.0));
    assert_eq!(label.left() - pin.right(), px(5.0));
    let chooser = cx.debug_bounds("workspace-switcher").unwrap();
    assert!(label.size.width > px(0.0));
    assert!(pin.right() <= label.left() && label.right() <= chooser.right());
    assert_eq!(
        cx.debug_bounds("tab-manager-top-spacer")
            .unwrap()
            .size
            .width,
        pinned_chrome.size.width
    );
    assert_eq!(
        cx.debug_bounds("workspace-sidebar-resize-handle-divider")
            .unwrap()
            .center()
            .x,
        pinned_chrome.right()
    );

    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.apply_directory_pin(WorkspaceId::new(1), None, window, cx);
        });
    });
    redraw(cx);
    let unpinned_chrome = cx.debug_bounds("workspace-top-chrome").unwrap();
    assert_eq!(
        pinned_chrome.size.width - unpinned_chrome.size.width,
        px(17.0)
    );
    assert!(cx.debug_bounds("workspace-chip-label").unwrap().size.width >= label.size.width);
    fs::remove_dir_all(directory).unwrap();
}

#[gpui::test]
fn collapsed_top_chrome_should_preserve_the_default_label_beside_both_actions(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    manager.update(cx, |manager, cx| {
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(1), "Default".to_owned())
            .expect("the Active Workspace should be renamed");
        cx.notify();
    });
    let name_width = cx.update(|window, _| {
        let run = window.text_style().to_run("Default".len());
        window
            .text_system()
            .shape_line("Default".into(), px(12.0), &[run], None)
            .width
    });

    click("toggle-sidebar-button", cx);

    let label = cx
        .debug_bounds("workspace-chip-label")
        .expect("collapsed Workspace label");
    let chooser = cx
        .debug_bounds("workspace-switcher")
        .expect("Workspace chooser");
    let toggle = cx
        .debug_bounds("toggle-sidebar-button")
        .expect("sidebar toggle");
    let chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("collapsed top chrome");
    assert!(
        label.size.width >= name_width,
        "Default needs {name_width:?}, but the collapsed label received {:?}",
        label.size.width
    );
    assert!(chooser.left() <= label.left() && label.right() <= chooser.right());
    assert!(toggle.right() <= chooser.left() && chooser.right() <= chrome.right());
}

#[gpui::test]
fn collapsed_workspace_switcher_should_open_from_each_part_without_dragging(
    cx: &mut TestAppContext,
) {
    let (manager, platform, cx) = workspace_manager_with_operating_system_window_drag_platform(cx);
    let expanded_toggle = cx.debug_bounds("toggle-sidebar-button").unwrap();
    click("toggle-sidebar-button", cx);
    assert_eq!(
        cx.debug_bounds("toggle-sidebar-button").unwrap(),
        expanded_toggle
    );

    let chooser = cx.debug_bounds("workspace-switcher").unwrap();
    let switcher_icon = cx.debug_bounds("workspace-switcher-icon").unwrap();
    assert!(cx.debug_bounds("workspace-chip-icon").is_none());
    let label = cx.debug_bounds("workspace-chip-label").unwrap();
    let tabs = cx.debug_bounds("tab-bar").unwrap();
    assert_eq!(expanded_toggle.left(), frame_space(cx));
    assert_eq!(expanded_toggle.size, gpui::size(px(28.0), px(28.0)));
    assert_eq!(chooser.left(), expanded_toggle.right() + frame_space(cx));
    // Ten pixels of content padding inside the one-pixel trigger border.
    assert_eq!(switcher_icon.left() - chooser.left(), px(11.0));
    let trailing_inset = chooser.right() - label.right();
    assert!(trailing_inset >= px(11.0) && trailing_inset < px(12.0));
    assert_eq!(label.left() - switcher_icon.right(), px(8.0));
    assert_eq!(tabs.left() - chooser.right(), frame_space(cx));

    for position in [
        switcher_icon.center(),
        label.center(),
        point(chooser.left() + px(2.0), chooser.center().y),
        point(chooser.right() - px(2.0), chooser.center().y),
        point(chooser.center().x, chooser.top() + px(2.0)),
    ] {
        cx.simulate_mouse_move(position, None, Modifiers::none());
        cx.simulate_click(position, Modifiers::none());
        cx.run_until_parked();
        assert!(
            cx.update(|window, cx| window_combo_box_is_open(window, cx)),
            "click at {position:?} did not open chooser {chooser:?}"
        );
        assert!(cx.debug_bounds("workspace-switcher-create-local").is_some());
        assert!(
            cx.debug_bounds("workspace-switcher-create-remote")
                .is_some()
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.update(|window, cx| {
            manager
                .read(cx)
                .workspaces
                .active()
                .payload()
                .read(cx)
                .focused_terminal_is_focused(window, cx)
        }));
    }
    assert_eq!(platform.counts(), (0, 0, 0));
}

#[gpui::test]
fn collapsed_remote_switcher_should_show_the_name_without_a_workspace_icon(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    let flow = open_remote_workspace_flow(&manager, cx);
    let (completion, _, _, _) = remote_completion("work", "~", "/home/tester", true);
    emit_remote_workspace_completion(&flow, completion, cx);
    click("toggle-sidebar-button", cx);

    assert!(cx.debug_bounds("workspace-chip-icon").is_none());
    let switcher_icon = cx.debug_bounds("workspace-switcher-icon").unwrap();
    let label = cx.debug_bounds("workspace-chip-label").unwrap();
    assert_eq!(label.left() - switcher_icon.right(), px(8.0));
    let chrome = cx.debug_bounds("workspace-top-chrome").unwrap();
    assert_eq!(cx.debug_bounds("tab-bar").unwrap().left(), chrome.right());
    assert_eq!(
        cx.debug_bounds("workspace-sidebar-resize-handle-divider")
            .unwrap()
            .center()
            .x,
        chrome.right()
    );
    click("workspace-chip-label", cx);
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn collapsed_chrome_geometry_survives_combo_box_theme_replacement(cx: &mut TestAppContext) {
    use spaceterm_ui::{ComboBoxMetrics, ComboBoxPaint, ComboBoxTheme};

    let (manager, _, cx) = workspace_manager(cx);
    manager.update(cx, |manager, cx| {
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(1), "A".into())
            .unwrap();
        cx.notify();
    });
    click("toggle-sidebar-button", cx);
    let original_chrome = cx.debug_bounds("workspace-top-chrome").unwrap();
    let original_label = cx.debug_bounds("workspace-chip-label").unwrap();
    cx.update(|window, cx| {
        let color = gpui::rgb(0x222222);
        cx.set_global(ComboBoxTheme::new(
            ComboBoxPaint::new(
                color, color, color, color, color, color, color, color, color,
            ),
            ComboBoxMetrics::new(px(240.0), px(40.0)).trigger_shape(px(7.0)),
        ));
        window.refresh();
    });
    redraw(cx);

    let chrome = cx.debug_bounds("workspace-top-chrome").unwrap();
    assert_eq!(chrome.size.width, original_chrome.size.width);
    assert_eq!(
        cx.debug_bounds("workspace-chip-label").unwrap().size.width,
        original_label.size.width
    );
    assert_eq!(
        cx.debug_bounds("tab-manager-top-spacer")
            .unwrap()
            .size
            .width,
        chrome.size.width
    );
    assert_eq!(
        cx.debug_bounds("workspace-sidebar-resize-handle-divider")
            .unwrap()
            .center()
            .x,
        chrome.right()
    );
}

#[gpui::test]
fn workspace_created_while_collapsed_should_share_the_active_top_chrome_width(
    cx: &mut TestAppContext,
) {
    let (_, _, cx) = workspace_manager(cx);
    click("toggle-sidebar-button", cx);

    new_desktop_workspace(cx);
    cx.run_until_parked();

    let chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("the collapsed top-left chrome was not rendered");
    let spacer = cx
        .debug_bounds("tab-manager-top-spacer")
        .expect("the new Workspace top-left spacer was not rendered");
    assert_eq!(spacer.size.width, chrome.size.width);
}

#[gpui::test]
fn collapsed_top_chrome_should_fit_after_pinning_changes_name(cx: &mut TestAppContext) {
    let directory = temporary_directory("pinned-directory-with-a-long-name");
    fs::create_dir_all(&directory).unwrap();
    let (manager, _, cx) = workspace_manager(cx);
    click("toggle-sidebar-button", cx);
    let name = directory
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            let directory = manager
                .local_filesystem
                .validate_directory(&directory)
                .unwrap();
            manager.apply_directory_pin(
                WorkspaceId::new(1),
                Some(PinnedDirectory::Local(directory)),
                window,
                cx,
            );
        })
    });
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active()
            .name()
            .to_owned()),
        name
    );
    let chrome = cx.debug_bounds("workspace-top-chrome").unwrap();
    let spacer = cx.debug_bounds("tab-manager-top-spacer").unwrap();
    assert_eq!(spacer.size.width, chrome.size.width);
    fs::remove_dir_all(directory).unwrap();
}

#[gpui::test]
fn collapsed_top_chrome_should_preserve_name_after_inactive_shell_exit(cx: &mut TestAppContext) {
    let (mut text_context, rendered_text) = RecordingRenderedText::context(cx);
    let cx = &mut text_context;
    let (manager, records, cx) = workspace_manager(cx);
    let inactive_sender = records
        .event_sender(1)
        .expect("the initial Workspace Terminal Session must have started");
    new_desktop_workspace(cx);
    manager.update(cx, |manager, cx| {
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(2), "Surviving Active Workspace".into())
            .unwrap();
        cx.notify();
    });
    cx.run_until_parked();
    click("toggle-sidebar-button", cx);

    inactive_sender
        .try_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success))
        .expect("the inactive shell exit must be delivered");
    cx.run_until_parked();

    let chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("the collapsed top-left chrome was not rendered");
    let spacer = cx
        .debug_bounds("tab-manager-top-spacer")
        .expect("the active Tab manager spacer was not rendered");
    assert_eq!(
        (
            manager.read_with(cx, |manager, _| manager.workspaces.active_id()),
            spacer.size.width,
        ),
        (WorkspaceId::new(2), chrome.size.width)
    );
    assert_rendered_text(
        "workspace-chip-label",
        "Surviving Active Workspace",
        &rendered_text,
        cx,
    );
}

#[gpui::test]
fn cmd_n_should_create_a_local_workspace_without_the_panel(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);

    new_desktop_workspace(cx);
    cx.run_until_parked();

    assert_eq!(
        cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.workspaces.len(),
                manager.workspaces.active_id(),
                window_combo_box_is_open(window, cx),
            )
        }),
        (2, WorkspaceId::new(2), false)
    );
}

#[gpui::test]
fn workspace_list_should_scroll_vertically_with_the_mouse_wheel(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    for _ in 0..24 {
        new_desktop_workspace(cx);
    }

    manager.read_with(cx, |manager, cx| {
        manager
            .sidebar
            .read(cx)
            .scroll_handle()
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

    let offset = manager.read_with(cx, |manager, cx| {
        manager.sidebar.read(cx).scroll_handle().offset().y
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
        new_desktop_workspace(cx);
    }
    manager.read_with(cx, |manager, cx| {
        manager
            .sidebar
            .read(cx)
            .scroll_handle()
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
fn workspace_list_publishes_a_named_scroll_bar_before_it_is_revealed(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (_manager, _records, cx) = workspace_manager(cx);
    for _ in 0..24 {
        new_desktop_workspace(cx);
    }
    let tree = A11yTree::read(cx);
    assert_eq!(tree.node("Workspace list")["aria"]["role"], "ScrollBar");
    assert_eq!(tree.with_role("ScrollBar").len(), 1);
}

fn workspace_order(manager: &Entity<WorkspaceManager>, cx: &mut VisualTestContext) -> Vec<u64> {
    manager.read_with(cx, |manager, _| {
        manager
            .workspaces
            .iter()
            .map(|workspace| workspace.id().get())
            .collect()
    })
}

#[gpui::test]
fn a_workspace_row_drag_released_on_its_first_move_should_land(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
    new_desktop_workspace(cx);
    cx.run_until_parked();
    let first = cx.debug_bounds("workspace-row-1-inactive").unwrap();
    let release =
        cx.debug_bounds("workspace-row-3-active").unwrap().center() + point(px(0.0), px(4.0));

    cx.simulate_mouse_move(first.center(), None, Modifiers::none());
    cx.simulate_mouse_down(first.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(release, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    cx.simulate_mouse_up(release, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(workspace_order(&manager, cx), vec![2, 3, 1]);
}

#[gpui::test]
fn a_workspace_row_released_just_below_the_list_should_land_at_its_end(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
    new_desktop_workspace(cx);
    cx.run_until_parked();
    let first = cx.debug_bounds("workspace-row-1-inactive").unwrap();
    let list = cx.debug_bounds("workspace-list").unwrap();
    let footer = cx.debug_bounds("workspace-sidebar-footer").unwrap();
    assert!(footer.top() >= list.bottom());
    // In line with the rows, but over the footer below the list.
    let below = point(first.center().x, footer.center().y);

    cx.simulate_mouse_move(first.center(), None, Modifiers::none());
    cx.simulate_mouse_down(first.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        first.center() + point(px(0.0), px(8.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.run_until_parked();
    cx.simulate_mouse_move(below, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    cx.simulate_mouse_up(below, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(workspace_order(&manager, cx), vec![2, 3, 1]);
}

#[gpui::test]
fn dragging_a_workspace_row_should_open_a_gap_and_settle_there_on_release(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
    new_desktop_workspace(cx);
    cx.run_until_parked();
    let first = cx.debug_bounds("workspace-row-1-inactive").unwrap();
    let second = cx.debug_bounds("workspace-row-2-inactive").unwrap();
    let third = cx.debug_bounds("workspace-row-3-active").unwrap();
    let origin = |selector: &'static str, cx: &mut VisualTestContext| {
        cx.debug_bounds(selector).map(|row| row.origin)
    };

    cx.simulate_mouse_move(first.center(), None, Modifiers::none());
    cx.simulate_mouse_down(first.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        first.center() + point(px(0.0), px(8.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.run_until_parked();
    cx.simulate_mouse_move(
        second.center() + point(px(0.0), px(4.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
    finish_slides(cx);
    let between = origin("workspace-row-2-inactive", cx);
    // Beside the rows, where the lifted copy stays in line with them.
    let beside = point(px(0.0), px(4.0));
    let release = third.center() + point(px(30.0), px(4.0));
    cx.simulate_mouse_move(release, MouseButton::Left, Modifiers::none());
    finish_slides(cx);
    let landing = gpui::Bounds::new(third.origin, first.size);
    let dashed = cx
        .debug_bounds("workspace-row-selection-1-landing")
        .is_some();
    let during = (
        workspace_order(&manager, cx),
        cx.debug_bounds("workspace-row-1-inactive"),
        origin("workspace-row-3-active", cx),
        cx.debug_bounds("workspace-row-preview-1-inactive"),
    );
    cx.simulate_mouse_up(release, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(between, Some(first.origin));
    assert_eq!(
        during,
        (
            vec![1, 2, 3],
            Some(landing),
            Some(second.origin),
            Some(gpui::Bounds::new(third.origin + beside, first.size)),
        ),
        "a dragged row must show its faded face where it lands, open a gap there, and hold its \
         lifted copy in line with the rows"
    );
    assert!(
        dashed,
        "a dragged row's faded face must edge its chip with a dashed line"
    );
    assert_eq!(
        (
            workspace_order(&manager, cx),
            manager.read_with(cx, |manager, _| manager.workspaces.active_id()),
        ),
        (vec![2, 3, 1], WorkspaceId::new(3))
    );
    assert!(cx.debug_bounds("drag-preview").is_none());
    // The moved row starts where its copy was released and settles into its new place.
    assert_eq!(
        origin("workspace-row-1-inactive", cx),
        Some(third.origin + beside)
    );
    finish_slides(cx);
    assert_eq!(
        [
            origin("workspace-row-2-inactive", cx),
            origin("workspace-row-3-active", cx),
            origin("workspace-row-1-inactive", cx),
        ],
        [Some(first.origin), Some(second.origin), Some(third.origin)]
    );

    // Position shortcuts follow the presented order.
    cx.simulate_keystrokes("ctrl-1");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_id()),
        WorkspaceId::new(2)
    );
}

#[gpui::test]
fn a_closed_workspace_row_should_leave_a_gap_that_closes(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
    cx.run_until_parked();
    cx.update(|_, cx| cx.set_global(crate::ui::drag_and_drop::AnimatePresence));
    let first = cx.debug_bounds("workspace-row-1-inactive").unwrap();
    let second = cx.debug_bounds("workspace-row-2-active").unwrap();

    right_click("workspace-row-1-inactive", cx);
    click("workspace-menu-row-close", cx);
    click("modal-action-close-confirmation-confirm", cx);
    let closing = cx
        .debug_bounds("workspace-row-2-active")
        .map(|row| row.origin);
    crate::ui::drag_and_drop::finish_slides(cx);
    let closed = cx
        .debug_bounds("workspace-row-2-active")
        .map(|row| row.origin);

    assert_eq!(
        (closing, closed),
        (Some(second.origin), Some(first.origin)),
        "the row after a closed one must start in its own place and close the gap"
    );
}

#[gpui::test]
fn selecting_another_workspace_should_move_the_selection_from_the_old_row(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
    finish_slides(cx);
    cx.update(|_, cx| cx.set_global(crate::ui::drag_and_drop::AnimatePresence));
    let first = cx.debug_bounds("workspace-row-1-inactive").unwrap();
    let second = cx.debug_bounds("workspace-row-2-active").unwrap();
    let selection = cx.debug_bounds("workspace-row-selection-2").unwrap().origin;
    let chip = |cx: &mut VisualTestContext| {
        cx.debug_bounds("workspace-row-selection-1")
            .expect("the selected row's chip was not rendered")
            .origin
    };

    click("workspace-row-1-inactive", cx);
    let leaving = chip(cx);
    finish_slides(cx);
    let landed = chip(cx);

    assert_eq!(
        (leaving, landed),
        (selection, selection + (first.origin - second.origin)),
        "the selection must leave from the old row and land on the new one"
    );
}

#[gpui::test]
fn switching_to_a_workspace_with_fewer_tabs_should_start_its_tab_strip_from_the_old_one(
    cx: &mut TestAppContext,
) {
    let (_manager, _records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-t cmd-t");
    new_desktop_workspace(cx);
    finish_slides(cx);
    click("workspace-row-1-inactive", cx);
    finish_slides(cx);
    cx.update(|_, cx| cx.set_global(crate::ui::drag_and_drop::AnimatePresence));
    let first = cx.debug_bounds("tab-item-1-inactive").unwrap().origin;
    let third = cx.debug_bounds("tab-item-3-active").unwrap().origin;
    let selection = cx.debug_bounds("tab-item-3-chip").unwrap().origin;
    let create = cx.debug_bounds("create-tab-button").unwrap().origin;
    let strip = |cx: &mut VisualTestContext| {
        (
            cx.debug_bounds("tab-item-1-chip")
                .expect("the arriving Tab's chip was not rendered")
                .origin,
            cx.debug_bounds("create-tab-button").unwrap().origin,
        )
    };

    click("workspace-row-2-inactive", cx);
    let (leaving, closing) = strip(cx);
    finish_slides(cx);
    let (landed, closed) = strip(cx);

    assert_eq!(
        (leaving, closing, landed),
        (selection, create, selection + (first - third)),
        "the selection must leave from the old active Tab, and the strip's end from the old end"
    );
    assert!(
        closed.x < create.x,
        "the strip must close the place of the Tabs it lacks, got {closed:?} from {create:?}"
    );
}

#[gpui::test]
fn switching_to_a_workspace_with_an_overflowing_tab_strip_should_keep_its_tabs_in_place(
    cx: &mut TestAppContext,
) {
    let (_manager, _records, cx) = workspace_manager(cx);
    for _ in 0..13 {
        cx.simulate_keystrokes("cmd-t");
    }
    new_desktop_workspace(cx);
    finish_slides(cx);
    click("workspace-row-1-inactive", cx);
    finish_slides(cx);
    let strip = cx.debug_bounds("tab-items").unwrap();
    let rest = ["tab-item-13-inactive", "tab-item-14-active"]
        .map(|selector| cx.debug_bounds(selector).map(|tab| tab.origin));
    click("workspace-row-2-inactive", cx);
    finish_slides(cx);
    cx.update(|_, cx| cx.set_global(crate::ui::drag_and_drop::AnimatePresence));
    let selection = cx.debug_bounds("tab-item-1-chip").unwrap().origin;
    let tabs = |cx: &mut VisualTestContext| {
        ["tab-item-13-inactive", "tab-item-14-active"]
            .map(|selector| cx.debug_bounds(selector).map(|tab| tab.origin))
    };

    click("workspace-row-1-inactive", cx);
    let arriving = tabs(cx);
    let leaving = cx.debug_bounds("tab-item-14-chip").unwrap().origin;
    finish_slides(cx);

    assert!(
        rest[1].is_some_and(|last| strip.right() < last.x + strip.size.width),
        "the strip must overflow for this test, got {rest:?} in {strip:?}"
    );
    assert_eq!(
        (arriving, leaving, tabs(cx)),
        (rest, selection, rest),
        "an overflowing strip's Tabs must keep their places while the selection leaves from the \
         old one"
    );
}

#[gpui::test]
fn a_new_workspace_should_start_its_tab_strip_from_a_scrolled_old_one(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);
    for _ in 0..13 {
        cx.simulate_keystrokes("cmd-t");
    }
    finish_slides(cx);
    cx.update(|_, cx| cx.set_global(crate::ui::drag_and_drop::AnimatePresence));
    let selection = cx.debug_bounds("tab-item-14-chip").unwrap().origin;
    let create = cx.debug_bounds("create-tab-button").unwrap().origin;

    new_desktop_workspace(cx);
    cx.run_until_parked();
    let leaving = (
        cx.debug_bounds("tab-item-1-chip")
            .expect("the new Workspace's Tab chip was not rendered")
            .origin,
        cx.debug_bounds("create-tab-button").unwrap().origin,
    );

    assert_eq!(
        leaving,
        (selection, create),
        "the selection and the strip's end must leave from where a scrolled strip drew them"
    );
}

#[gpui::test]
fn switching_workspaces_while_a_dropped_tab_settles_should_move_the_selection_from_where_it_is_drawn(
    cx: &mut TestAppContext,
) {
    let (_manager, _records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-t cmd-t");
    finish_slides(cx);
    cx.update(|_, cx| cx.set_global(crate::ui::drag_and_drop::AnimatePresence));
    let first = cx.debug_bounds("tab-item-1-inactive").unwrap();
    let third = cx.debug_bounds("tab-item-3-active").unwrap();
    cx.simulate_mouse_move(third.center(), None, Modifiers::none());
    cx.simulate_mouse_down(third.center(), MouseButton::Left, Modifiers::none());
    for position in [
        third.center() - point(px(8.0), px(0.0)),
        first.center() + point(px(30.0), px(0.0)),
    ] {
        cx.simulate_mouse_move(position, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
    }
    finish_slides(cx);
    let release = first.center() + point(px(30.0), px(0.0));
    cx.simulate_mouse_up(release, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    let drawn = cx.debug_bounds("tab-item-3-chip").unwrap().origin;

    new_desktop_workspace(cx);
    cx.run_until_parked();
    let leaving = cx
        .debug_bounds("tab-item-1-chip")
        .expect("the new Workspace's Tab chip was not rendered")
        .origin;

    assert_eq!(
        leaving, drawn,
        "the selection must leave from where the settling Tab drew it"
    );
}

#[gpui::test]
fn a_new_workspace_should_start_its_tab_strip_from_the_old_one(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-t cmd-t");
    finish_slides(cx);
    cx.update(|_, cx| cx.set_global(crate::ui::drag_and_drop::AnimatePresence));
    let first = cx.debug_bounds("tab-item-1-inactive").unwrap().origin;
    let third = cx.debug_bounds("tab-item-3-active").unwrap().origin;
    let selection = cx.debug_bounds("tab-item-3-chip").unwrap().origin;
    let chip = |cx: &mut VisualTestContext| {
        cx.debug_bounds("tab-item-1-chip")
            .expect("the new Workspace's Tab chip was not rendered")
            .origin
    };

    new_desktop_workspace(cx);
    cx.run_until_parked();
    let leaving = chip(cx);
    finish_slides(cx);
    let landed = chip(cx);

    assert_eq!(
        (leaving, landed),
        (selection, selection + (first - third)),
        "the selection must leave from the old Workspace's active Tab"
    );
}

#[gpui::test]
fn switching_to_a_workspace_with_more_tabs_should_grow_the_tabs_the_old_strip_lacked(
    cx: &mut TestAppContext,
) {
    let (_manager, _records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-t cmd-t");
    new_desktop_workspace(cx);
    finish_slides(cx);
    cx.update(|_, cx| cx.set_global(crate::ui::drag_and_drop::AnimatePresence));
    let selection = cx.debug_bounds("tab-item-1-chip").unwrap().origin;
    let create = cx.debug_bounds("create-tab-button").unwrap().origin;
    let end = |cx: &mut VisualTestContext| cx.debug_bounds("create-tab-button").unwrap().origin;

    click("workspace-row-1-inactive", cx);
    let leaving = cx.debug_bounds("tab-item-3-chip").unwrap().origin;
    let growing = end(cx);
    finish_slides(cx);
    let grown = end(cx);

    assert_eq!(
        (leaving, growing),
        (selection, create),
        "the selection must leave from the old active Tab, and the strip's end from the old end"
    );
    assert!(
        create.x < grown.x,
        "the Tabs the old strip lacked must grow in, got {grown:?} from {create:?}"
    );
}

#[gpui::test]
fn switching_back_and_forth_quickly_should_start_each_tab_strip_from_the_old_end(
    cx: &mut TestAppContext,
) {
    let (_manager, _records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-t cmd-t");
    new_desktop_workspace(cx);
    finish_slides(cx);
    cx.update(|_, cx| cx.set_global(crate::ui::drag_and_drop::AnimatePresence));
    let end = |cx: &mut VisualTestContext| cx.debug_bounds("create-tab-button").unwrap().origin.x;
    let later = |cx: &mut VisualTestContext| {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(60));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
    };

    click("workspace-row-1-inactive", cx);
    later(cx);
    click("workspace-row-2-inactive", cx);
    later(cx);
    let left = end(cx);
    click("workspace-row-1-inactive", cx);
    let arrived = end(cx);

    assert!(
        (arrived - left).abs() <= px(1.0),
        "the Tab strip must start from the old strip's end at {left:?}, got {arrived:?}"
    );
}

#[gpui::test]
fn escape_should_cancel_a_workspace_row_drag_and_return_every_row_to_its_place(
    cx: &mut TestAppContext,
) {
    let (manager, _records, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
    new_desktop_workspace(cx);
    cx.run_until_parked();
    let first = cx.debug_bounds("workspace-row-1-inactive").unwrap();
    let third = cx.debug_bounds("workspace-row-3-active").unwrap();
    let release = third.center() + point(px(0.0), px(4.0));

    cx.simulate_mouse_move(first.center(), None, Modifiers::none());
    cx.simulate_mouse_down(first.center(), MouseButton::Left, Modifiers::none());
    for position in [first.center() + point(px(0.0), px(8.0)), release] {
        cx.simulate_mouse_move(position, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
    }
    finish_slides(cx);
    let opened = cx.debug_bounds("workspace-row-3-active").unwrap().origin != third.origin;
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let cancelled = cx.debug_bounds("drag-preview").is_some();
    cx.simulate_mouse_up(release, MouseButton::Left, Modifiers::none());
    finish_slides(cx);

    assert_eq!(
        (
            opened,
            cancelled,
            workspace_order(&manager, cx),
            ["workspace-row-1-inactive", "workspace-row-3-active"]
                .map(|selector| cx.debug_bounds(selector).map(|row| row.origin)),
        ),
        (
            true,
            false,
            vec![1, 2, 3],
            [Some(first.origin), Some(third.origin)]
        )
    );
}

#[gpui::test]
fn a_single_motion_should_move_a_workspace_row_on_release(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
    new_desktop_workspace(cx);
    cx.run_until_parked();
    let first = cx.debug_bounds("workspace-row-1-inactive").unwrap();
    let third = cx.debug_bounds("workspace-row-3-active").unwrap();
    let release = third.center() + point(px(0.0), px(4.0));

    cx.simulate_mouse_move(first.center(), None, Modifiers::none());
    cx.simulate_mouse_down(first.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(release, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(release, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(workspace_order(&manager, cx), vec![2, 3, 1]);
}

#[gpui::test]
fn escape_should_cancel_only_the_drag_in_progress(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-t");
    new_desktop_workspace(cx);
    cx.simulate_keystrokes("cmd-t");
    cx.simulate_keystrokes("ctrl-1");
    cx.run_until_parked();
    let (_, hidden_tabs) = active_tab_manager(&manager, cx);
    let first = cx.debug_bounds("tab-item-1-inactive").unwrap();
    let second = cx.debug_bounds("tab-item-2-active").unwrap();
    let past_second = second.center() + point(px(4.0), px(0.0));

    // A Tab drag in the first Workspace ends while that Workspace is hidden.
    cx.simulate_mouse_move(first.center(), None, Modifiers::none());
    cx.simulate_mouse_down(first.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(past_second, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-2");
    cx.run_until_parked();
    cx.simulate_mouse_up(past_second, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    let (_, visible_tabs) = active_tab_manager(&manager, cx);

    // Escape during a drag in the second Workspace cancels that drag alone.
    cx.simulate_mouse_move(first.center(), None, Modifiers::none());
    cx.simulate_mouse_down(first.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(past_second, MouseButton::Left, Modifiers::none());
    finish_slides(cx);
    let opened = cx.debug_bounds("tab-item-2-active").unwrap().origin != second.origin;
    cx.simulate_keystrokes("escape");
    finish_slides(cx);
    let cancelled = (
        visible_tabs.read_with(cx, |tabs, _| tabs.dragged_tab()),
        cx.debug_bounds("tab-item-2-active").unwrap().origin != second.origin,
    );
    cx.simulate_mouse_up(past_second, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    // The hidden Tab bar never saw its release, so its drag landed nowhere.
    assert_eq!(
        (
            opened,
            cancelled,
            visible_tabs.read_with(cx, |tabs, _| tabs.tab_ids()),
            hidden_tabs.read_with(cx, |tabs, _| tabs.tab_ids()),
        ),
        (
            true,
            (None, false),
            vec![TabId::new(1), TabId::new(2)],
            vec![TabId::new(1), TabId::new(2)],
        )
    );
}

#[gpui::test]
fn workspace_scrollbar_thumb_should_drag_the_list(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    for _ in 0..24 {
        new_desktop_workspace(cx);
    }
    manager.update(cx, |manager, cx| {
        manager
            .sidebar
            .read(cx)
            .scroll_handle()
            .set_offset(point(px(0.0), px(0.0)));
        manager
            .sidebar
            .update(cx, |sidebar, cx| sidebar.reveal_scrollbar(cx));
    });
    cx.run_until_parked();

    let thumb = cx
        .debug_bounds("workspace-scrollbar-thumb-hitbox")
        .expect("the Workspace scrollbar hitbox was not rendered");
    let list = cx
        .debug_bounds("workspace-list")
        .expect("the Workspace list was not rendered");
    let before = manager.read_with(cx, |manager, cx| {
        manager.sidebar.read(cx).scroll_handle().offset().y
    });
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

    let state = manager.read_with(cx, |manager, cx| {
        manager.sidebar.read(cx).scroll_handle().offset().y
    });
    assert!(
        state < before,
        "the Workspace list did not finish a scrollbar drag: {state:?}"
    );
}

#[gpui::test]
fn closing_an_inactive_workspace_should_keep_the_sidebar_where_it_was_scrolled(
    cx: &mut TestAppContext,
) {
    let (manager, _records, cx) = workspace_manager(cx);
    for _ in 0..24 {
        new_desktop_workspace(cx);
    }
    finish_slides(cx);
    manager.update(cx, |manager, cx| {
        manager
            .sidebar
            .read(cx)
            .scroll_handle()
            .set_offset(point(px(0.0), px(0.0)));
    });
    cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();

    right_click("workspace-row-2-inactive", cx);
    click("workspace-menu-row-close", cx);
    if cx
        .debug_bounds("modal-action-close-confirmation-confirm")
        .is_some()
    {
        click("modal-action-close-confirmation-confirm", cx);
    }
    finish_slides(cx);

    let state = manager.read_with(cx, |manager, cx| {
        (
            manager.workspaces.len(),
            manager.sidebar.read(cx).scroll_handle().offset().y,
        )
    });
    assert_eq!(
        state,
        (24, px(0.0)),
        "closing an inactive Workspace must not scroll the sidebar back to the active one"
    );
}

#[gpui::test]
fn creating_workspaces_should_scroll_the_active_workspace_into_view(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    for _ in 0..24 {
        new_desktop_workspace(cx);
    }

    let state = manager.read_with(cx, |manager, cx| {
        (
            manager.workspaces.len(),
            manager.workspaces.active_id(),
            manager.sidebar.read(cx).scroll_handle().offset().y,
        )
    });
    assert!(
        state.0 == 25 && state.1 == WorkspaceId::new(25) && state.2 < px(0.0),
        "the Active Workspace was not revealed; state was {state:?}"
    );
    redraw(cx);
    let viewport = cx.debug_bounds("workspace-list").unwrap();
    let active = cx
        .debug_bounds("workspace-row-25-active")
        .expect("the new active row must be mounted");
    assert!(active.top() >= viewport.top() && active.bottom() <= viewport.bottom());
    assert!(active.left() >= viewport.left() && active.right() <= viewport.right());
}

#[gpui::test]
fn overflowing_workspace_list_should_not_cover_the_creation_footer(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);
    for _ in 0..24 {
        new_desktop_workspace(cx);
    }

    let sidebar = cx
        .debug_bounds("workspace-sidebar")
        .expect("the Workspace sidebar was not rendered");
    let list = cx
        .debug_bounds("workspace-list")
        .expect("the overflowing Workspace list was not rendered");
    let button = cx
        .debug_bounds("workspace-sidebar-footer")
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
fn command_b_should_collapse_the_top_chrome_and_expand_terminal_content(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    let collapsed_width = cx.update(|window, cx| {
        WorkspaceChromeLayout::collapsed_width(
            &chrome_identity(manager.read(cx).workspaces.active()),
            window,
            cx,
        )
    });
    let expanded_chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("the fixed top-left chrome was not rendered");

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();

    let hidden_state =
        manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().visible);
    let collapsed_chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("the fixed top-left chrome must remain rendered");
    let content = cx
        .debug_bounds("tab-manager-content")
        .expect("the active Tab content was not rendered");
    assert_eq!(
        (
            hidden_state,
            expanded_chrome.size.width,
            collapsed_chrome.size.width,
            content.origin.x,
        ),
        (
            false,
            px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH),
            collapsed_width,
            px(0.0),
        )
    );
}

#[gpui::test]
fn command_shift_e_should_toggle_focus_and_reveal_a_hidden_sidebar(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);

    cx.simulate_keystrokes("cmd-shift-e");
    cx.run_until_parked();
    let sidebar_focused =
        cx.update(|window, cx| manager.read(cx).sidebar.read(cx).is_focused(window));

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    let hidden_state = cx.update(|window, cx| {
        let workspace_manager = manager.read(cx);
        (
            workspace_manager.sidebar.read(cx).layout().visible,
            workspace_manager.sidebar.read(cx).is_focused(window),
            workspace_manager
                .workspaces
                .active()
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
            manager.sidebar.read(cx).layout().visible,
            manager.sidebar.read(cx).is_focused(window),
        )
    });

    assert_eq!(
        (sidebar_focused, hidden_state, revealed_state),
        (true, (false, false, true), (true, true))
    );
}

/// Whether the Active Workspace chip paints the accent fill that marks a focused sidebar.
fn active_row_is_emphasized(cx: &mut VisualTestContext) -> bool {
    let chip = cx
        .debug_bounds("workspace-row-selection-1")
        .expect("the Active Workspace chip was rendered");
    cx.update(|window, cx| {
        let accent = crate::ui::appearance::chrome(cx)
            .host_colors(spaceterm_ui::ControlHost::Panel)
            .primary_background;
        let fill = gpui::Background::from(crate::ui::appearance::gpui_color(accent));
        let bounds = chip.scale(window.scale_factor());
        window
            .painted_quads()
            .iter()
            .any(|quad| quad.bounds == bounds && quad.background == fill)
    })
}

#[gpui::test]
fn a_focused_sidebar_emphasizes_the_active_workspace(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert!(!active_row_is_emphasized(cx), "the Pane has focus");

    cx.simulate_keystrokes("cmd-shift-e");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| manager.read(cx).sidebar.read(cx).is_focused(window)));
    assert!(
        active_row_is_emphasized(cx),
        "a focused sidebar paints its selection in the accent color"
    );

    cx.simulate_keystrokes("cmd-shift-e");
    cx.run_until_parked();
    assert!(!active_row_is_emphasized(cx), "focus returned to the Pane");
}

#[gpui::test]
fn a_secondary_click_should_leave_focus_in_the_pane(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    right_click("workspace-row-1-active", cx);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!cx.update(|window, cx| manager.read(cx).sidebar.read(cx).is_focused(window)));
    assert!(active_terminal_has_input_focus(&manager, cx));
    assert!(
        !active_row_is_emphasized(cx),
        "only the keyboard emphasizes the sidebar selection"
    );
}

#[gpui::test]
fn command_n_creates_and_activates_a_home_workspace(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);

    new_desktop_workspace(cx);
    cx.run_until_parked();

    let state = manager.read_with(cx, |manager, _| {
        (
            manager.workspaces.len(),
            manager.workspaces.active_id(),
            manager
                .workspaces
                .active()
                .local_home_directory()
                .unwrap()
                .to_path_buf(),
            records.dropped_session_ids(),
            records
                .starts()
                .into_iter()
                .map(|start| {
                    start
                        .local_working_directory()
                        .expect("Local Workspace starts must remain local")
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
            std::env::temp_dir(),
            Vec::new(),
            vec![std::env::temp_dir(), std::env::temp_dir()],
        )
    );
}

#[gpui::test]
fn control_number_should_activate_workspaces_by_position(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
    new_desktop_workspace(cx);
    cx.run_until_parked();

    cx.simulate_keystrokes("ctrl-1");
    cx.run_until_parked();
    let first_state = cx.update(|window, cx| {
        let manager = manager.read(cx);
        (
            manager.workspaces.active_id(),
            manager
                .workspaces
                .active()
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
            manager.workspaces.active_id(),
            manager.sidebar.read(cx).is_focused(window),
            manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .focused_terminal_is_focused(window, cx),
        )
    });

    cx.simulate_keystrokes("ctrl-9");
    cx.run_until_parked();
    let unavailable_state = manager.read_with(cx, |manager, _| manager.workspaces.active_id());

    assert_eq!(first_state, (WorkspaceId::new(1), true));
    assert_eq!(second_state, (WorkspaceId::new(2), true, false));
    assert_eq!(unavailable_state, WorkspaceId::new(2));
}

#[gpui::test]
fn clicking_an_inactive_workspace_should_restore_its_focused_pane(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-d");
    new_desktop_workspace(cx);
    cx.run_until_parked();

    click("workspace-row-1-inactive", cx);

    let state = cx.update(|window, cx| {
        let manager = manager.read(cx);
        let active_workspace = manager.workspaces.active().payload().read(cx);
        (
            manager.workspaces.active_id(),
            manager.sidebar.read(cx).is_focused(window),
            manager.terminal_focus_blocker(window, cx),
            active_workspace.aggregate_counts(cx),
            active_workspace.focused_terminal_is_focused(window, cx),
        )
    });
    assert_eq!(state, (WorkspaceId::new(1), false, None, (1, 2), true,));
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
            manager.sidebar.read(cx).is_focused(window),
            manager.terminal_focus_blocker(window, cx),
            manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .focused_terminal_is_focused(window, cx),
        )
    });
    assert_eq!(state, (false, None, true));
}

#[gpui::test]
fn right_clicking_an_inactive_workspace_should_open_its_menu_without_activating_it(
    cx: &mut TestAppContext,
) {
    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    new_desktop_workspace(cx);
    cx.run_until_parked();

    right_click("workspace-row-1-inactive", cx);

    // The menu acts on the first Workspace without activating it.
    let state = cx.update(|window, cx| {
        let manager = manager.read(cx);
        (
            manager.workspaces.active_id(),
            manager.sidebar.read(cx).is_focused(window),
            manager.sidebar.read(cx).menu_open(),
            manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .focused_terminal_is_focused(window, cx),
        )
    });
    assert_eq!(state, (WorkspaceId::new(2), false, true, false));
    assert!(cx.debug_bounds("menu-panel-0").is_some());

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let dismissed = cx.update(|window, cx| {
        let manager = manager.read(cx);
        (
            manager.sidebar.read(cx).menu_open(),
            manager.sidebar.read(cx).is_focused(window),
            manager.terminal_focus_blocker(window, cx),
        )
    });
    assert_eq!(dismissed, (false, false, None));
    assert!(active_terminal_has_input_focus(&manager, cx));
}

#[gpui::test]
fn workspace_menu_closure_recomputes_remaining_focus_owners(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    right_click("workspace-row-1-active", cx);
    assert!(manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).menu_open()));
    assert!(!active_terminal_has_input_focus(&manager, cx));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).menu_open()));
    assert_eq!(
        cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.sidebar.read(cx).is_focused(window),
                manager.terminal_focus_blocker(window, cx),
                manager.workspaces.active_id(),
            )
        }),
        (false, None, WorkspaceId::new(1))
    );
    assert!(active_terminal_has_input_focus(&manager, cx));
}

#[gpui::test]
fn tab_shortcuts_should_create_and_activate_tabs_while_sidebar_is_focused(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);

    cx.simulate_keystrokes("cmd-shift-e");
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();

    let created_state = cx.update(|window, cx| {
        let manager = manager.read(cx);
        (
            manager.sidebar.read(cx).is_focused(window),
            manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .aggregate_counts(cx),
        )
    });

    cx.simulate_keystrokes("cmd-shift-e");
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-1");
    cx.run_until_parked();

    assert_eq!((created_state.0, created_state.1), (false, (2, 2)));
    assert!(cx.debug_bounds("tab-item-1-active").is_some());
    assert!(cx.debug_bounds("tab-item-2-inactive").is_some());
}

#[gpui::test]
fn close_pane_shortcut_from_sidebar_should_leave_the_empty_view_after_the_final_pane(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);

    cx.simulate_keystrokes("cmd-shift-e");
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-alt-w");
    cx.run_until_parked();
    click("modal-action-close-confirmation-confirm", cx);

    let state = manager.read_with(cx, |manager, _| {
        (
            manager.workspaces.len(),
            records.dropped_session_ids(),
            records.session_count(),
        )
    });
    assert_eq!(state, (0, vec![1], 1));
    assert_eq!(cx.windows().len(), 1, "the window stays open");
    // The empty stage remains, with nothing desktop-owned in it.
    assert!(cx.debug_bounds("empty-workspace-stage").is_some());
}

#[gpui::test]
fn duplicate_final_tab_close_requests_should_close_the_workspace_once(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    let tab_manager = manager.read_with(cx, |manager, _| {
        manager.workspaces.active().payload().clone()
    });

    tab_manager.update(cx, |_, cx| {
        cx.emit(TabManagerEvent::FinalTabCloseRequested {
            final_tab_id: crate::domain::TabId::new(1),
        });
        cx.emit(TabManagerEvent::FinalTabCloseRequested {
            final_tab_id: crate::domain::TabId::new(1),
        });
    });
    cx.run_until_parked();

    assert_eq!(records.dropped_session_ids(), vec![1]);
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        0
    );
    assert_eq!(cx.windows().len(), 1);
}

#[gpui::test]
fn scroll_shortcuts_from_sidebar_focus_use_the_active_tabs_focused_pane(cx: &mut TestAppContext) {
    let profile = crate::desktop_profile::default_keymap::profile(
        crate::platform::keyboard_layout::testing::us(),
        vec![],
    )
    .unwrap();
    crate::ui::assert_scroll_shortcuts_from_sidebar_focus(profile, cx);
}

pub(crate) fn assert_scroll_shortcuts_from_sidebar_focus(
    profile: crate::keybindings::KeymapProfile,
    cx: &mut TestAppContext,
) {
    use crate::keybindings::{Command, KeybindingPreferences};
    use crate::terminal::ScrollbackMovement;
    let (manager, records, cx) = workspace_manager(cx);
    let keymap = profile.resolve(&KeybindingPreferences::default());
    cx.update(|_, cx| {
        cx.clear_key_bindings();
        cx.bind_keys(
            keymap
                .key_bindings()
                .into_iter()
                .chain(profile.control_bindings().iter().cloned())
                .chain(profile.fixed_bindings().iter().cloned()),
        );
        crate::keybindings::runtime::install(profile, cx);
    });
    let shortcut = |command| keymap.shortcut(command).unwrap().to_string();
    cx.simulate_keystrokes(&shortcut(Command::CreateTab));
    cx.run_until_parked();
    cx.simulate_keystrokes(&shortcut(Command::SplitRight));
    cx.run_until_parked();
    assert_eq!(records.starts().len(), 3);
    for (command, movement) in [
        (Command::ScrollPageUp, ScrollbackMovement::PageUp),
        (Command::ScrollPageDown, ScrollbackMovement::PageDown),
        (Command::ScrollToTop, ScrollbackMovement::Top),
        (Command::ScrollToBottom, ScrollbackMovement::Bottom),
    ] {
        cx.simulate_keystrokes(&shortcut(Command::ToggleSidebarFocus));
        cx.run_until_parked();
        assert!(cx.update(|window, cx| manager.read(cx).sidebar.read(cx).is_focused(window)));
        let before = records.commands().len();
        cx.simulate_keystrokes(&shortcut(command));
        cx.run_until_parked();
        let calls = records.commands();
        let calls = calls[before..]
            .iter()
            .filter(|call| !matches!(call.command, RecordedCommand::Focus(_)))
            .map(|call| (call.session_id, &call.command))
            .collect::<Vec<_>>();
        assert_eq!(
            calls,
            vec![(3, &RecordedCommand::ScrollScrollback(movement))],
            "{command:?} must move only the Active Tab's Focused Pane and send no terminal input"
        );
        assert!(cx.update(|window, cx| !manager.read(cx).sidebar.read(cx).is_focused(window)));
    }
}

#[gpui::test]
fn pane_shortcuts_should_operate_on_the_active_tab_while_sidebar_is_focused(
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
            manager.sidebar.read(cx).is_focused(window),
            manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .aggregate_counts(cx),
            records.dropped_session_ids(),
        )
    });
    assert_eq!((state.0, state.1, state.2), (false, (1, 2), Vec::new()));
}

#[gpui::test]
fn inline_rename_contains_fixed_chrome_line_height_in_both_densities(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);
    right_click("workspace-row-1-active", cx);
    click("workspace-menu-row-rename", cx);

    for density in [
        crate::appearance::ChromeDensity::Compact,
        crate::appearance::ChromeDensity::Comfortable,
    ] {
        let line_height = cx.update(|window, cx| {
            let mut preferences = crate::appearance::AppearancePreferences::default();

            preferences.window.density = density;
            let resolved = crate::appearance::ThemeCatalog::default()
                .resolve(
                    crate::appearance::AppearanceGeneration::INITIAL,
                    &preferences,
                    crate::appearance::SystemAppearance::unavailable().with_composition(
                        crate::appearance::CompositionCapabilities::new(true, true),
                    ),
                    &crate::appearance::AvailableFonts::default(),
                )
                .expect("built-in appearance should resolve");
            let (active, inactive) =
                crate::ui::appearance::ChromeAppearance::prepare_variants(&resolved.chrome);
            let line_height = active
                .typography
                .style(crate::ui::chrome_typography::TextRole::Navigation)
                .line_height;
            cx.set_global(crate::ui::appearance::InstalledChrome {
                active: Arc::new(active),
                inactive: Arc::new(inactive),
            });
            window.refresh();
            line_height
        });
        cx.run_until_parked();

        let frame = cx
            .debug_bounds("workspace-rename-input-1")
            .expect("inline rename frame");
        let input = cx
            .debug_bounds("workspace-rename-input")
            .expect("inline rename input");
        assert!(
            frame.size.height >= line_height
                && input.top() >= frame.top()
                && input.bottom() <= frame.bottom(),
            "{density:?} inline rename must contain its {line_height:?} Navigation line: frame={frame:?}, input={input:?}"
        );
    }
}

#[gpui::test]
fn workspace_context_menu_should_target_new_tab_and_rename_commands(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);

    right_click("workspace-row-1-active", cx);
    click("workspace-menu-row-new-tab", cx);
    let workspace_detail = manager.read_with(cx, |manager, cx| {
        manager
            .workspaces
            .active()
            .payload()
            .read(cx)
            .aggregate_counts(cx)
    });

    right_click("workspace-row-1-active", cx);
    click("workspace-menu-row-rename", cx);
    cx.simulate_keystrokes("cmd-a D e v enter");
    cx.run_until_parked();
    let name = manager.read_with(cx, |manager, _| {
        manager.workspaces.active().name().to_owned()
    });

    assert_eq!((workspace_detail, name), ((2, 2), "Dev".to_owned()));
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
            manager.sidebar.read(cx).rename_is_focused(window),
            manager.sidebar.read(cx).is_focused(window),
            manager.sidebar.read(cx).is_renaming(),
        )
    });
    cx.simulate_keystrokes("cmd-a D e v enter");
    cx.run_until_parked();

    let rename_state = manager.read_with(cx, |manager, cx| {
        (
            manager.workspaces.active_id(),
            manager.workspaces.active().name().to_owned(),
            !manager.sidebar.read(cx).is_renaming(),
        )
    });
    assert_eq!(focus_state, (true, false, true));
    assert_eq!(rename_state, (WorkspaceId::new(1), "Dev".to_owned(), true));
}

#[gpui::test]
fn inline_rename_frame_should_resolve_inside_the_sidebar_control_host(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);
    let mut appearance = cx.update(|_, cx| crate::ui::appearance::chrome(cx).clone());
    appearance.control_colors.input_background = Color::rgb(0xcc2233);
    appearance.panel_controls.colors.input_background = Color::rgb(0x228844);
    let window_background = gpui_color(appearance.control_colors.input_background);
    let panel_background = gpui_color(appearance.panel_controls.colors.input_background);
    assert_ne!(window_background, panel_background);
    cx.update(|window, cx| {
        assert_eq!(
            crate::ui::control_theme::replace_uniform_control_catalog(cx, &appearance),
            Ok(spaceterm_ui::ControlThemeReplacement::Applied)
        );
        cx.set_global(crate::ui::appearance::InstalledChrome::single(Arc::new(
            appearance,
        )));
        window.refresh();
    });
    cx.run_until_parked();
    right_click("workspace-row-1-active", cx);
    click("workspace-menu-row-rename", cx);

    let observed = Rc::new(RefCell::new(Vec::<DivInspectorState>::new()));
    let styles = Rc::clone(&observed);
    cx.update(|window, cx| {
        cx.register_inspector_element(move |_, _| {
            let styles = Rc::clone(&styles);
            move |_, state: &DivInspectorState, _, _| {
                styles.borrow_mut().push(state.clone());
                gpui::Empty
            }
        });
        cx.set_inspector_renderer(Box::new(|inspector, window, cx| {
            div()
                .children(inspector.render_inspector_states(window, cx))
                .into_any_element()
        }));
        window.toggle_inspector(cx);
    });
    cx.run_until_parked();
    let bounds = cx
        .debug_bounds("workspace-rename-input-1")
        .expect("inline rename frame must render");
    cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
    cx.run_until_parked();
    for _ in 0..16 {
        if let Some(background) = observed
            .borrow()
            .iter()
            .rev()
            .find(|style| style.bounds == bounds)
            .map(|style| style.base_style.background.clone())
        {
            assert_eq!(
                background,
                Some(panel_background.into()),
                "inline rename frame must use Panel input paint, not Window input paint"
            );
            return;
        }
        cx.simulate_event(ScrollWheelEvent {
            position: bounds.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(36.0))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();
    }
    panic!("inspector did not expose the inline rename frame");
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
            .sidebar
            .read(cx)
            .rename_input()
            .expect("rename editor should remain active")
            .read(cx)
            .value()
            .to_owned()),
        "Dev"
    );
    right_click("workspace-rename-input", cx);
    assert!(manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).is_renaming()));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    let state_before_submit = cx.update(|window, cx| {
        let manager = manager.read(cx);
        (
            manager.sidebar.read(cx).is_renaming(),
            manager.sidebar.read(cx).rename_is_focused(window),
            manager.workspaces.active().name().to_owned(),
        )
    });
    assert_eq!(
        state_before_submit,
        (true, true, "Default".to_owned()),
        "dismissing the owned menu must not commit or destroy the editor"
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let state_after_submit = manager.read_with(cx, |manager, cx| {
        (
            !manager.sidebar.read(cx).is_renaming(),
            manager.workspaces.active().name().to_owned(),
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
            .sidebar
            .read(cx)
            .rename_input()
            .expect("rename editor should remain active")
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
            manager.sidebar.read(cx).is_renaming(),
            manager.sidebar.read(cx).rename_is_focused(window),
            manager.workspaces.active().name().to_owned(),
        )
    });
    assert_eq!(
        state_before_submit,
        (true, true, "Default".to_owned()),
        "activating the owned menu must not commit or destroy the editor"
    );

    cx.simulate_keystrokes("O p s enter");
    cx.run_until_parked();
    let state_after_submit = manager.read_with(cx, |manager, cx| {
        (
            !manager.sidebar.read(cx).is_renaming(),
            manager.workspaces.active().name().to_owned(),
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

    let sidebar_focus =
        manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).focus_handle());
    cx.update(|window, cx| sidebar_focus.focus(window, cx));
    cx.run_until_parked();

    let rename_state = manager.read_with(cx, |manager, cx| {
        (
            manager.workspaces.active().name().to_owned(),
            !manager.sidebar.read(cx).is_renaming(),
        )
    });
    assert_eq!(rename_state, ("Dev".to_owned(), true));
}

#[gpui::test]
fn activating_another_workspace_should_cancel_the_previous_inline_rename(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
    cx.run_until_parked();
    click("workspace-row-1-inactive", cx);
    right_click("workspace-row-1-active", cx);
    click("workspace-menu-row-rename", cx);

    assert!(manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).is_renaming()));
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
            manager.workspaces.active_id(),
            !manager.sidebar.read(cx).is_renaming(),
            first.name().to_owned(),
            first.payload().read(cx).aggregate_counts(cx),
            second.payload().read(cx).aggregate_counts(cx),
            records.dropped_session_ids(),
        )
    });
    assert_eq!(
        (state.0, state.1, state.2, state.3, state.4, state.5,),
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
fn explicitly_closing_the_final_workspace_should_leave_the_empty_view(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);

    right_click("workspace-row-1-active", cx);
    click("workspace-menu-row-close", cx);
    click("modal-action-close-confirmation-confirm", cx);

    let state = manager.read_with(cx, |manager, _| {
        (
            manager.workspaces.len(),
            records.dropped_session_ids(),
            records.session_count(),
        )
    });
    assert_eq!(
        state,
        (0, vec![1], 1),
        "nothing replaces the final Workspace"
    );
    assert_eq!(cx.windows().len(), 1);
    // The empty stage remains, with nothing desktop-owned in it.
    assert!(cx.debug_bounds("empty-workspace-stage").is_some());
}

#[gpui::test]
fn inactive_shell_exit_should_close_its_workspace_without_stealing_activation(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    let inactive_sender = records
        .event_sender(1)
        .expect("the initial Workspace Terminal Session must have started");
    new_desktop_workspace(cx);
    cx.run_until_parked();

    assert!(active_terminal_has_input_focus(&manager, cx));
    inactive_sender
        .try_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success))
        .expect("the inactive shell exit must be delivered");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_id()),
        WorkspaceId::new(2)
    );
    assert!(active_terminal_has_input_focus(&manager, cx));
    new_desktop_workspace(cx);
    cx.run_until_parked();

    let state = manager.read_with(cx, |manager, _| {
        (
            manager.workspaces.len(),
            manager.workspaces.active_id(),
            records.dropped_session_ids(),
        )
    });
    assert_eq!(state, (2, WorkspaceId::new(3), vec![1]));
}
#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
mod unix_adapter_tests {
    include!("../../platform/unix_adapter_tests/workspace_manager.rs");
}

#[gpui::test]
fn remote_pin_change_and_unpin_should_preserve_terminal_sessions_and_source_directory(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    let provider = Arc::new(TestRemoteProvider::connected(
        crate::domain::RemoteDirectoryIdentity::new("/srv/physical".into()).unwrap(),
    ));
    let (completion, closes, _, _, _, _) = remote_completion_with_provider(
        "work",
        "~",
        "/home/tester",
        true,
        gpui::Task::ready(Ok(())),
        provider,
    );
    let flow = open_remote_workspace_flow(&manager, cx);
    emit_remote_workspace_completion(&flow, completion, cx);
    for (directory, count) in [("/srv/frontend", 2), ("/srv/backend", 3)] {
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.apply_directory_pin(
                    WorkspaceId::new(2),
                    Some(PinnedDirectory::Remote {
                        directory: RemoteDirectory::new(directory.into()).unwrap(),
                        identity: crate::domain::RemoteDirectoryIdentity::new(
                            "/srv/physical".into(),
                        )
                        .unwrap(),
                    }),
                    window,
                    cx,
                )
            })
        });
        cx.run_until_parked();
        assert_eq!(records.starts().len(), count);
        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();
        assert_eq!(
            records
                .starts()
                .last()
                .unwrap()
                .remote_launch_plan()
                .unwrap()
                .remote_directory()
                .as_str(),
            directory
        );
    }
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.apply_directory_pin(WorkspaceId::new(2), None, window, cx);
        })
    });
    assert_eq!(records.starts().len(), 4);
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    assert_eq!(
        records
            .starts()
            .last()
            .unwrap()
            .remote_launch_plan()
            .unwrap()
            .remote_directory()
            .as_str(),
        "/srv/backend"
    );
    assert!(records.dropped_session_ids().is_empty());
    assert_eq!(closes.load(Ordering::Acquire), 0);
}

#[gpui::test]
fn directory_picker_should_pin_its_target_and_keep_the_connection(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    let provider = Arc::new(TestRemoteProvider::connected(
        crate::domain::RemoteDirectoryIdentity::new("/home/tester".into()).unwrap(),
    ));
    let (completion, closes, _, _, _, _) = remote_completion_with_provider(
        "work",
        "~",
        "/home/tester",
        true,
        gpui::Task::ready(Ok(())),
        provider,
    );
    let flow = open_remote_workspace_flow(&manager, cx);
    emit_remote_workspace_completion(&flow, completion, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.choose_pin_directory(WorkspaceId::new(2), window, cx)
        })
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-shift-n");
    cx.run_until_parked();
    assert!(manager.read_with(cx, |manager, _| manager.remote_workspace_flow.is_none()));
    click("directory-picker-confirm", cx);
    assert!(manager.read_with(cx, |manager, _| {
        manager
            .workspaces
            .workspace(WorkspaceId::new(1))
            .unwrap()
            .pinned_directory()
            .is_none()
    }));
    assert_eq!(
        manager.read_with(cx, |manager, _| {
            match manager
                .workspaces
                .workspace(WorkspaceId::new(2))
                .unwrap()
                .pinned_directory()
            {
                Some(PinnedDirectory::Remote { directory, .. }) => {
                    Some(directory.as_str().to_owned())
                }
                _ => None,
            }
        }),
        Some("~/".to_owned())
    );
    assert_eq!(records.starts().len(), 2);
    assert_eq!(closes.load(Ordering::Acquire), 0);
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.remote_workspace_runtimes.len()),
        1
    );
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }));
}

#[gpui::test]
fn unavailable_home_should_allow_closing_a_workspace_without_replacement(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
    cx.run_until_parked();
    let missing_home = temporary_directory("missing-home");
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.local_home_directory_path = missing_home;
            manager.close_workspace(WorkspaceId::new(2), window, cx);
        });
    });
    cx.run_until_parked();

    assert!(!cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.len(), 1);
        assert_eq!(manager.workspaces.active_id(), WorkspaceId::new(1));
    });
    assert_eq!(records.session_count(), 2);
    assert_eq!(records.dropped_session_ids(), vec![2]);
}

#[gpui::test]
fn workspace_switcher_should_list_local_and_remote_workspaces_and_activate_remote(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    let flow = open_remote_workspace_flow(&manager, cx);
    let (completion, _, _, _) = remote_completion("work", "~", "/home/tester", true);
    emit_remote_workspace_completion(&flow, completion, cx);
    cx.simulate_keystrokes("ctrl-1");
    cx.run_until_parked();
    open_workspace_switcher(cx);
    assert!(cx.debug_bounds("workspace-switcher-result-1").is_some());
    assert!(cx.debug_bounds("workspace-switcher-result-2").is_some());
    cx.simulate_keystrokes("f r e s h enter");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_id()),
        WorkspaceId::new(2)
    );
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn workspace_switcher_should_not_expose_the_workspace_path_as_a_description(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    manager.update(cx, |manager, _| {
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(1), "Alpha".to_owned())
            .unwrap();
        manager
            .workspaces
            .update_automatic_directory(
                WorkspaceId::new(1),
                crate::terminal::metadata::CurrentDirectory::Local(PathBuf::from(
                    "/project/Ångström",
                )),
            )
            .unwrap();
    });

    let item = manager.read_with(cx, |manager, cx| {
        manager.workspace_switcher_items(cx).remove(0)
    });

    assert_eq!(item.label(), "Alpha");
    assert_eq!(item.description_text(), None);
}

#[gpui::test]
fn workspace_switcher_should_not_match_workspace_paths(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
    cx.run_until_parked();
    manager.update(cx, |manager, _| {
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(1), "Alpha".to_owned())
            .unwrap();
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(2), "Beta".to_owned())
            .unwrap();
        manager
            .workspaces
            .update_automatic_directory(
                WorkspaceId::new(1),
                crate::terminal::metadata::CurrentDirectory::Local(PathBuf::from(
                    "/project/Ångström",
                )),
            )
            .unwrap();
    });
    open_workspace_switcher(cx);

    cx.simulate_input("project");
    cx.run_until_parked();

    assert!(cx.debug_bounds("workspace-switcher-result-1").is_none());
    assert!(cx.debug_bounds("workspace-switcher-result-2").is_none());
}

#[gpui::test]
fn workspace_switcher_should_preserve_empty_order_and_show_fuzzy_ranking(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
    cx.run_until_parked();
    manager.update(cx, |manager, _| {
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(1), "remote operation".to_owned())
            .unwrap();
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(2), "projects".to_owned())
            .unwrap();
    });
    open_workspace_switcher(cx);
    let first = cx.debug_bounds("workspace-switcher-result-1").unwrap();
    let second = cx.debug_bounds("workspace-switcher-result-2").unwrap();
    assert!(first.top() < second.top());

    cx.simulate_input("ro");
    cx.run_until_parked();

    let first = cx.debug_bounds("workspace-switcher-result-1").unwrap();
    let second = cx.debug_bounds("workspace-switcher-result-2").unwrap();
    assert!(second.top() < first.top());
}

#[gpui::test]
fn workspace_switcher_should_append_creation_actions_for_empty_and_matching_queries(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    manager.update(cx, |manager, _| {
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(1), "Alpha".into())
            .unwrap()
    });
    open_workspace_switcher(cx);
    for query in ["", "space", "a l p h a"] {
        cx.simulate_keystrokes(query);
        cx.run_until_parked();
        let matched = cx.debug_bounds("workspace-switcher-result-1").unwrap();
        let local = cx.debug_bounds("workspace-switcher-create-local").unwrap();
        let remote = cx.debug_bounds("workspace-switcher-create-remote").unwrap();
        let separator = cx
            .debug_bounds("workspace-switcher-create-local-group-separator")
            .expect("the creation group must be separated from matching Workspaces");
        assert!(matched.bottom() <= separator.top());
        assert!(separator.bottom() <= local.top());
        assert!(local.bottom() <= remote.top());
    }
}

#[gpui::test]
fn workspace_switcher_menu_sizes_to_its_rows_and_keeps_shortcuts_clear(cx: &mut TestAppContext) {
    let (_manager, _, cx) = workspace_manager(cx);
    open_workspace_switcher(cx);

    let panel = cx.debug_bounds("combo-box-panel").unwrap();
    let remote_label = cx.debug_bounds("combo-box-row-2-label").unwrap();
    let remote_shortcut = cx.debug_bounds("combo-box-row-2-shortcut").unwrap();

    assert!(panel.size.width > px(240.0));
    assert!(panel.size.width <= px(420.0));
    assert!(remote_shortcut.left() - remote_label.right() >= px(24.0));
}

#[gpui::test]
fn sidebar_remote_creation_should_open_host_selection_and_restore_focus_on_cancel(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    click_new_workspace_menu("new-workspace-menu-create-remote", cx);
    let flow = manager.read_with(cx, |manager, _| {
        manager.remote_workspace_flow.clone().unwrap()
    });
    assert_eq!(
        flow.read_with(cx, |flow, _| flow.stage()),
        RemoteWorkspaceFlowStage::HostSelection
    );
    assert_eq!(records.starts().len(), 1);
    assert!(cx.update(|window, cx| flow.read(cx).owns_first_responder(window, cx)));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(manager.read_with(cx, |manager, _| manager.remote_workspace_flow.is_none()));
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }));
}

#[gpui::test]
fn creation_shortcut_should_accept_remote_switcher_name_instead_of_highlight(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    open_workspace_switcher_for_creation(cx);
    cx.simulate_keystrokes("end home");
    cx.run_until_parked();
    let local_row = cx.debug_bounds("workspace-switcher-create-local").unwrap();
    let remote_row = cx.debug_bounds("workspace-switcher-create-remote").unwrap();
    cx.update(|window, _| {
        let quads = window.painted_quads();
        let background = |bounds: gpui::Bounds<Pixels>| quads.iter().find(|quad| quad.bounds == bounds.scale(window.scale_factor())).map(|quad| quad.background);
        assert_ne!(background(local_row), background(remote_row), "keyboard highlight must visibly distinguish the Local creation row before the Remote shortcut");
    });
    cx.simulate_keystrokes("cmd-shift-n");
    cx.run_until_parked();
    let flow = manager
        .read_with(cx, |manager, _| manager.remote_workspace_flow.clone())
        .expect("remote shortcut should open setup");
    assert!(cx.update(|window, cx| flow.read(cx).owns_first_responder(window, cx)));
    let (completion, _, _, _) = remote_completion("work", "~", "/home/tester", true);
    emit_remote_workspace_completion(&flow, completion, cx);
    manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.active().name(), "fresh workspace");
        assert_eq!(manager.workspaces.len(), 2);
    });
    assert_eq!(records.starts().len(), 2);
}

#[gpui::test]
fn switcher_should_preserve_explicit_remote_names_across_destinations(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
    manager.update(cx, |manager, cx| {
        let workspace_id = manager.workspaces.active_id();
        manager
            .workspaces
            .rename_workspace(workspace_id, "fresh workspace".to_owned())
            .unwrap();
        cx.notify();
    });
    for (destination, expected) in [
        ("work", "fresh workspace"),
        ("work", "fresh workspace"),
        ("other", "fresh workspace"),
        ("work", "fresh workspace"),
    ] {
        open_workspace_switcher_for_creation(cx);
        // The matching Workspaces push the creation rows below the switcher's visible rows.
        cx.simulate_keystrokes("cmd-shift-n");
        cx.run_until_parked();
        let flow = manager.read_with(cx, |manager, _| {
            manager.remote_workspace_flow.clone().unwrap()
        });
        let (completion, _, _, _) = remote_completion(destination, "~", "/home/tester", true);
        emit_remote_workspace_completion(&flow, completion, cx);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .active()
                .name()
                .to_owned()),
            expected
        );
    }
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        6
    );
    assert_eq!(records.starts().len(), 6);
}

#[gpui::test]
fn creation_shortcut_should_open_remote_setup_without_reusing_cancelled_query(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    open_workspace_switcher_for_creation(cx);
    cx.simulate_keystrokes("escape cmd-shift-n");
    cx.run_until_parked();
    let flow = manager.read_with(cx, |manager, _| {
        assert_eq!(manager.remote_workspace_name.as_deref(), Some(""));
        manager.remote_workspace_flow.clone().unwrap()
    });
    assert_eq!(
        flow.read_with(cx, |flow, _| flow.stage()),
        RemoteWorkspaceFlowStage::HostSelection
    );
    assert!(cx.update(|window, cx| flow.read(cx).owns_first_responder(window, cx)));
    cx.simulate_keystrokes("cmd-shift-n");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.remote_workspace_flow.clone()),
        Some(flow)
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(manager.read_with(cx, |manager, _| manager.remote_workspace_flow.is_none()));
    assert_eq!(records.starts().len(), 1);
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }));
}

#[gpui::test]
fn creation_shortcut_should_use_automatic_remote_name_for_blank_input(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    open_workspace_switcher(cx);
    cx.simulate_keystrokes("space cmd-shift-n");
    cx.run_until_parked();
    let flow = manager.read_with(cx, |manager, _| {
        manager.remote_workspace_flow.clone().unwrap()
    });
    let (completion, _, _, _) = remote_completion("work", "~", "/home/tester", true);
    emit_remote_workspace_completion(&flow, completion, cx);
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active()
            .name()
            .to_owned()),
        "Default (2)"
    );
}

#[gpui::test]
fn workspace_switcher_check_should_follow_active_workspace_not_keyboard_highlight(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    new_desktop_workspace(cx);
    cx.simulate_keystrokes("ctrl-1");
    cx.run_until_parked();
    open_workspace_switcher(cx);
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    let marker = cx.debug_bounds("combo-box-row-0-check").unwrap();
    let identity = cx.debug_bounds("combo-box-row-0-identity-icon").unwrap();
    let first = cx.debug_bounds("workspace-switcher-result-1").unwrap();
    assert!(first.contains(&marker.center()));
    assert!(marker.right() < identity.left());
    assert_eq!(cx.debug_bounds("workspace-switcher-active-marker"), None);
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_id()),
        WorkspaceId::new(1)
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    open_workspace_switcher(cx);
    let marker = cx.debug_bounds("combo-box-row-1-check").unwrap();
    let identity = cx.debug_bounds("combo-box-row-1-identity-icon").unwrap();
    let second = cx.debug_bounds("workspace-switcher-result-2").unwrap();
    assert!(second.contains(&marker.center()));
    assert!(marker.right() < identity.left());
    assert_eq!(cx.debug_bounds("workspace-switcher-active-marker"), None);
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_id()),
        WorkspaceId::new(2)
    );
}

#[gpui::test]
fn workspace_switcher_creation_rows_should_start_in_the_checkmark_column(cx: &mut TestAppContext) {
    let (_, _, cx) = workspace_manager(cx);
    open_workspace_switcher(cx);

    let marker = cx.debug_bounds("combo-box-row-0-check").unwrap();
    let workspace_label = cx.debug_bounds("combo-box-row-0-label").unwrap();
    for (icon, label) in [
        ("combo-box-row-1-identity-icon", "combo-box-row-1-label"),
        ("combo-box-row-2-identity-icon", "combo-box-row-2-label"),
    ] {
        let icon = cx.debug_bounds(icon).unwrap();
        let label = cx.debug_bounds(label).unwrap();
        assert!(icon.left() < marker.right(), "{icon:?} {marker:?}");
        assert!(label.left() < workspace_label.left());
    }
}

#[gpui::test]
fn workspace_activation_hints_should_follow_sidebar_order_after_closing(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    for _ in 0..9 {
        new_desktop_workspace(cx);
    }
    cx.run_until_parked();
    let hints = cx.update(|_, cx| {
        let presentation = crate::desktop_profile::DesktopPresentation::get(cx);
        [0, 8, 9].map(|index| workspace_activation_shortcut(index, presentation))
    });
    assert_eq!(hints, [Some("Ctrl+1".into()), Some("Ctrl+9".into()), None]);
    open_workspace_switcher(cx);
    assert_eq!(switcher_shortcut_row(9, cx), Some(8));
    assert_eq!(switcher_shortcut_row(10, cx), None);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.close_workspace(WorkspaceId::new(1), window, cx)
        })
    });
    cx.simulate_keystrokes("ctrl-1");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_id()),
        WorkspaceId::new(2)
    );
    open_workspace_switcher(cx);
    assert_eq!(switcher_shortcut_row(2, cx), Some(0));
    assert_eq!(switcher_shortcut_row(9, cx), Some(7));
    cx.simulate_keystrokes("end");
    cx.run_until_parked();
    assert_eq!(switcher_shortcut_row(10, cx), Some(8));
}

fn switcher_shortcut_row(workspace: u64, cx: &mut VisualTestContext) -> Option<usize> {
    let row = cx
        .debug_bounds(format!("workspace-switcher-result-{workspace}").leak())
        .expect("the Workspace switcher row should render");
    (0..12).find(|position| {
        cx.debug_bounds(format!("combo-box-row-{position}-shortcut").leak())
            .is_some_and(|shortcut| {
                row.top() <= shortcut.center().y && shortcut.center().y <= row.bottom()
            })
    })
}

#[gpui::test]
fn workspace_creation_and_switcher_buttons_should_show_hover_tooltips(cx: &mut TestAppContext) {
    let (_, _, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    for (button, tooltip) in [
        ("toggle-sidebar-button", "toggle-sidebar-tooltip"),
        ("new-workspace-button", "new-workspace-tooltip"),
        ("workspace-switcher", "workspace-switcher-tooltip"),
    ] {
        let center = cx.debug_bounds(button).unwrap().center();
        cx.simulate_mouse_move(center, None, Modifiers::default());
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds(tooltip).is_some(),
            "missing tooltip: {tooltip}"
        );
        cx.simulate_mouse_move(point(px(500.0), px(300.0)), None, Modifiers::default());
        cx.run_until_parked();
    }
}

#[gpui::test]
fn collapsed_sidebar_toggle_shows_hover_tooltip(cx: &mut TestAppContext) {
    let (_, _, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    click("toggle-sidebar-button", cx);
    cx.simulate_mouse_move(point(px(500.0), px(300.0)), None, Modifiers::default());
    cx.run_until_parked();
    let center = cx.debug_bounds("toggle-sidebar-button").unwrap().center();
    cx.simulate_mouse_move(center, None, Modifiers::default());
    cx.run_until_parked();
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    assert!(cx.debug_bounds("toggle-sidebar-tooltip").is_some());
    assert_eq!(
        crate::desktop_profile::testing_presentation()
            .shortcut(&ToggleSidebar)
            .as_deref(),
        Some("Primary+B")
    );
}

#[gpui::test]
fn sidebar_should_constrain_width_after_window_shrink_and_reopen(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    cx.simulate_resize(gpui::size(px(1000.0), px(600.0)));
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.resize_sidebar(px(420.0), window, cx)
        })
    });
    cx.run_until_parked();
    cx.simulate_resize(gpui::size(px(480.0), px(600.0)));
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("workspace-sidebar").unwrap().size.width,
        px(240.0)
    );
    assert!(cx.debug_bounds("tab-manager-content").unwrap().size.width >= px(240.0));

    cx.simulate_resize(gpui::size(px(1000.0), px(600.0)));
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.resize_sidebar(px(420.0), window, cx)
        })
    });
    cx.simulate_keystrokes("cmd-b");
    cx.simulate_resize(gpui::size(px(480.0), px(600.0)));
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("workspace-sidebar").unwrap().size.width,
        px(240.0)
    );
    assert!(cx.debug_bounds("tab-manager-content").unwrap().size.width >= px(240.0));
}

#[gpui::test]
fn sidebar_resize_cancel_should_respect_the_current_window_width(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    cx.simulate_resize(gpui::size(px(1000.0), px(600.0)));
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.resize_sidebar(px(420.0), window, cx)
        })
    });
    cx.run_until_parked();
    let start = cx
        .debug_bounds("workspace-sidebar-resize-handle-hitbox")
        .unwrap()
        .center();
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        point(start.x - px(50.0), start.y),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_resize(gpui::size(px(480.0), px(600.0)));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("tab-manager-content").unwrap().size.width >= px(240.0));
}

#[gpui::test]
fn sidebar_keyboard_should_navigate_reveal_and_stop_at_both_ends(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    for _ in 0..24 {
        new_desktop_workspace(cx);
    }
    cx.simulate_keystrokes("cmd-shift-e home up");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_id()),
        WorkspaceId::new(1)
    );
    let first = cx.debug_bounds("workspace-row-1-active").unwrap();
    let list = cx.debug_bounds("workspace-list").unwrap();
    assert!(first.top() >= list.top());
    assert!(
        cx.debug_bounds("workspace-sidebar-focus-indicator")
            .is_none(),
        "collection rows must not paint a focus ring"
    );
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_id()),
        WorkspaceId::new(2)
    );
    cx.simulate_keystrokes("end down");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_id()),
        WorkspaceId::new(25)
    );
    let last = cx.debug_bounds("workspace-row-25-active").unwrap();
    assert!(last.bottom() <= list.bottom());
    assert!(cx.update(|window, cx| {
        !manager
            .read(cx)
            .workspaces
            .active()
            .payload()
            .read(cx)
            .focused_terminal_has_input_focus(window, cx)
    }));
    assert!(
        !records
            .commands()
            .iter()
            .any(|call| matches!(call.command, RecordedCommand::Key(_)))
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }));
    assert!(cx.update(|window, cx| !manager.read(cx).sidebar.read(cx).is_focused(window)));
}

#[gpui::test]
fn hiding_sidebar_with_its_menu_open_should_restore_terminal_input(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-shift-e shift-f10");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| spaceterm_ui::window_menu_is_open(window, cx)));

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(!manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().visible));
    assert!(!cx.update(|window, cx| spaceterm_ui::window_menu_is_open(window, cx)));
    assert!(active_terminal_has_input_focus(&manager, cx));
    cx.simulate_keystrokes("x");
    assert!(
        records
            .commands()
            .iter()
            .any(|call| matches!(call.command, RecordedCommand::Key(_)))
    );
}

#[gpui::test]
fn hiding_sidebar_with_new_workspace_menu_open_should_restore_terminal_input(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    click("new-workspace-button", cx);
    assert!(cx.update(|window, cx| spaceterm_ui::window_menu_is_open(window, cx)));

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(!manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().visible));
    assert!(!cx.update(|window, cx| spaceterm_ui::window_menu_is_open(window, cx)));
    assert!(active_terminal_has_input_focus(&manager, cx));
    cx.simulate_keystrokes("x");
    assert!(
        records
            .commands()
            .iter()
            .any(|call| matches!(call.command, RecordedCommand::Key(_)))
    );
}

#[gpui::test]
fn native_text_ui_should_keep_workspace_rename_until_the_emoji_is_committed(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    cx.update(|window, cx| {
        window.activate_window();
        spaceterm_ui::TextInputFocus::set(Some(window.window_handle()), cx);
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-shift-e shift-f10 down enter");
    cx.run_until_parked();
    assert!(manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).is_renaming()));

    cx.deactivate_window();
    cx.run_until_parked();
    assert!(manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).is_renaming()));
    cx.simulate_input("🙂");
    cx.run_until_parked();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active()
            .name()
            .to_owned()),
        "🙂"
    );
    assert!(!manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).is_renaming()));
}

#[gpui::test]
fn sidebar_keyboard_rename_cancel_should_preserve_name_and_restore_sidebar_focus(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let original_name = manager.read_with(cx, |manager, _| {
        manager.workspaces.active().name().to_owned()
    });
    cx.simulate_keystrokes("cmd-shift-e shift-f10 down enter");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| manager.read(cx).sidebar.read(cx).rename_is_focused(window)));

    cx.simulate_keystrokes("cmd-a D e v escape");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active()
            .name()
            .to_owned()),
        original_name
    );
    assert!(!manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).is_renaming()));
    assert!(cx.update(|window, cx| manager.read(cx).sidebar.read(cx).is_focused(window)));
    assert!(cx.update(|window, cx| {
        !manager
            .read(cx)
            .workspaces
            .active()
            .payload()
            .read(cx)
            .focused_terminal_has_input_focus(window, cx)
    }));
    assert!(
        !records
            .commands()
            .iter()
            .any(|call| matches!(call.command, RecordedCommand::Key(_)))
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(active_terminal_has_input_focus(&manager, cx));
}

#[gpui::test]
fn sidebar_workspace_menu_should_show_an_icon_before_every_label(cx: &mut TestAppContext) {
    let (_, _, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-shift-e shift-f10");
    cx.run_until_parked();

    for row in [
        "workspace-menu-row-new-tab",
        "workspace-menu-row-rename",
        "workspace-menu-row-pin-directory",
        "workspace-menu-row-close",
    ] {
        let icon_selector: &'static str = format!("{row}-icon").leak();
        let label_selector: &'static str = format!("{row}-label").leak();
        let icon = cx
            .debug_bounds(icon_selector)
            .unwrap_or_else(|| panic!("{row} has no icon"));
        let label = cx.debug_bounds(label_selector).unwrap();
        assert!(icon.right() <= label.left(), "{row}: {icon:?} {label:?}");
    }
}

#[gpui::test]
fn sidebar_keyboard_menu_should_rename_and_restore_focus(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-shift-e shift-f10");
    cx.run_until_parked();
    assert!(cx.debug_bounds("workspace-menu-row-rename").is_some());
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("workspace-rename-input").is_some());
    cx.simulate_keystrokes("cmd-a D e v enter");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active()
            .name()
            .to_owned()),
        "Dev"
    );
    assert!(
        cx.debug_bounds("workspace-sidebar-focus-indicator")
            .is_none(),
        "restored collection focus must not add a row focus ring"
    );
    cx.simulate_keystrokes("shift-f10");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!cx.update(|window, cx| spaceterm_ui::window_menu_is_open(window, cx)));
    assert!(cx.update(|window, cx| manager.read(cx).sidebar.read(cx).is_focused(window)));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }));
}

/// The first Tab's leading inset reaches under the top-left chrome, so the mark before an inactive
/// first Tab paints in the chrome's bounds.
#[gpui::test]
fn tab_strip_start_mark_should_stay_visible_beside_the_opaque_top_chrome(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, cx| {
        let appearance = crate::ui::appearance::ChromeAppearance {
            materials: crate::appearance::SurfaceMaterials::OPAQUE,
            ..crate::ui::appearance::chrome(cx).clone()
        };
        cx.set_global(crate::ui::appearance::InstalledChrome::single(Arc::new(
            appearance,
        )));
        window.activate_window();
        window.refresh();
    });
    cx.simulate_keystrokes("cmd-t cmd-2");
    cx.run_until_parked();

    let mark = cx
        .debug_bounds("tab-separator-start-1")
        .expect("the mark before inactive Tab 1 should be drawn beside the visible sidebar");
    let chrome = cx
        .debug_bounds("workspace-top-chrome")
        .expect("Workspace top chrome must be rendered");
    assert!(
        chrome.intersects(&mark),
        "the mark should rest inside the top chrome's bounds for this check to guard its paint \
         order, got {mark:?} and {chrome:?}"
    );
    cx.update(|window, _| {
        let mark = mark.scale(window.scale_factor());
        let quads = window.painted_quads();
        let visible = |quad: &gpui::Quad| quad.bounds.intersect(&quad.content_mask.bounds);
        let mark_order = quads
            .iter()
            .find(|quad| visible(quad) == mark)
            .expect("the mark should be painted")
            .order;
        let covering = quads
            .iter()
            .filter(|quad| quad.order > mark_order && !quad.background.is_transparent())
            .map(visible)
            .filter(|bounds| bounds.intersects(&mark))
            .collect::<Vec<_>>();
        assert!(
            covering.is_empty(),
            "no surface should paint over the strip-start mark at {mark:?}, got {covering:?}"
        );
    });
}

#[gpui::test]
fn client_window_controls_follow_live_layout_on_both_sides_of_the_sidebar_toggle(
    cx: &mut TestAppContext,
) {
    let (_, _, cx) = workspace_manager(cx);
    cx.simulate_decorations(gpui::Decorations::Client {
        tiling: gpui::Tiling::default(),
    });
    for collapsed in [false, true] {
        if collapsed {
            click("toggle-sidebar-button", cx);
        }
        for style in [
            spaceterm_ui::DesktopWindowStyle::Adwaita,
            spaceterm_ui::DesktopWindowStyle::Breeze,
        ] {
            cx.update(|window, cx| {
                cx.set_global(spaceterm_ui::DesktopWindowControls {
                    style,
                    ..Default::default()
                });
                window.refresh();
            });
            let (target, diameter, gap) = match style {
                spaceterm_ui::DesktopWindowStyle::Adwaita => (34.0, 24.0, 3.0),
                spaceterm_ui::DesktopWindowStyle::Breeze => (20.0, 18.0, 4.0),
            };
            for (left, right) in [
                ([None; 3], [Some(gpui::WindowButton::Close), None, None]),
                (
                    [None; 3],
                    [
                        Some(gpui::WindowButton::Minimize),
                        Some(gpui::WindowButton::Maximize),
                        Some(gpui::WindowButton::Close),
                    ],
                ),
                (
                    [
                        Some(gpui::WindowButton::Close),
                        Some(gpui::WindowButton::Minimize),
                        Some(gpui::WindowButton::Maximize),
                    ],
                    [None; 3],
                ),
            ] {
                cx.simulate_button_layout(Some(gpui::WindowButtonLayout { left, right }));
                redraw(cx);
                let toggle = cx.debug_bounds("toggle-sidebar-button").unwrap();
                let close = cx.debug_bounds("window-close").unwrap();
                assert_eq!(close.size.width, px(target));
                if style == spaceterm_ui::DesktopWindowStyle::Adwaita {
                    cx.update(|window, _| {
                    let circle = close.inset(px((target - diameter) / 2.0)).scale(window.scale_factor());
                    assert!(
                        window
                            .painted_quads()
                            .iter()
                            .any(|quad| { quad.bounds == circle && !quad.background.is_transparent() }),
                        "the native 24px circle must retain its full size inside the 34px target"
                    );
                });
                }
                if left[0].is_some() {
                    let minimize = cx.debug_bounds("window-minimize").unwrap();
                    let maximize = cx.debug_bounds("window-maximize").unwrap();
                    assert!(close.right() < minimize.left());
                    assert!(minimize.right() < maximize.left());
                    assert!(maximize.right() < toggle.left());
                    assert_eq!(minimize.left() - close.right(), px(gap));
                } else {
                    assert!(toggle.right() < close.left());
                }
            }
        }
    }
}

fn active_terminal_has_input_focus(
    manager: &Entity<WorkspaceManager>,
    cx: &mut VisualTestContext,
) -> bool {
    cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active()
            .payload()
            .read(cx)
            .focused_terminal_has_input_focus(window, cx)
    })
}

#[derive(Default)]
struct RecordingRenderedText(Mutex<Vec<String>>, Option<(String, Pixels)>);

impl RecordingRenderedText {
    fn context_with_measurement(
        cx: &TestAppContext,
        text: &str,
        width: Pixels,
    ) -> (TestAppContext, Arc<Self>) {
        let text_system = Arc::new(Self(Mutex::default(), Some((text.to_owned(), width))));
        let context = TestAppContext::build_with_text_system(
            cx.dispatcher.clone(),
            cx.test_function_name(),
            text_system.clone(),
        );
        (context, text_system)
    }

    fn context(cx: &TestAppContext) -> (TestAppContext, Arc<Self>) {
        let text = Arc::new(Self::default());
        let context = TestAppContext::build_with_text_system(
            cx.dispatcher.clone(),
            cx.test_function_name(),
            text.clone(),
        );
        (context, text)
    }
}

impl gpui::PlatformTextSystem for RecordingRenderedText {
    fn add_fonts(&self, fonts: Vec<std::borrow::Cow<'static, [u8]>>) -> anyhow::Result<()> {
        gpui::NoopTextSystem.add_fonts(fonts)
    }
    fn all_font_names(&self) -> Vec<String> {
        gpui::NoopTextSystem.all_font_names()
    }
    fn font_id(&self, font: &gpui::Font) -> anyhow::Result<gpui::FontId> {
        gpui::NoopTextSystem.font_id(font)
    }
    fn font_metrics(&self, id: gpui::FontId) -> gpui::FontMetrics {
        gpui::NoopTextSystem.font_metrics(id)
    }
    fn typographic_bounds(
        &self,
        id: gpui::FontId,
        glyph: gpui::GlyphId,
    ) -> anyhow::Result<gpui::Bounds<f32>> {
        gpui::NoopTextSystem.typographic_bounds(id, glyph)
    }
    fn advance(&self, id: gpui::FontId, glyph: gpui::GlyphId) -> anyhow::Result<gpui::Size<f32>> {
        gpui::NoopTextSystem.advance(id, glyph)
    }
    fn glyph_for_char(&self, id: gpui::FontId, c: char) -> Option<gpui::GlyphId> {
        gpui::NoopTextSystem.glyph_for_char(id, c)
    }
    fn glyph_raster_bounds(
        &self,
        params: &gpui::RenderGlyphParams,
    ) -> anyhow::Result<gpui::Bounds<gpui::DevicePixels>> {
        gpui::NoopTextSystem.glyph_raster_bounds(params)
    }
    fn rasterize_glyph(
        &self,
        params: &gpui::RenderGlyphParams,
        bounds: gpui::Bounds<gpui::DevicePixels>,
    ) -> anyhow::Result<(gpui::Size<gpui::DevicePixels>, Vec<u8>)> {
        gpui::NoopTextSystem.rasterize_glyph(params, bounds)
    }
    fn layout_line(&self, text: &str, size: Pixels, runs: &[gpui::FontRun]) -> gpui::LineLayout {
        self.0.lock().unwrap().push(text.to_owned());
        let mut layout = gpui::NoopTextSystem.layout_line(text, size, runs);
        // The platform font boundary supplies deterministic fractional advances to layout tests.
        if let Some((label, width)) = &self.1
            && text == label
        {
            let ratio = *width / layout.width;
            for run in &mut layout.runs {
                for glyph in &mut run.glyphs {
                    glyph.position.x *= ratio;
                }
            }
            layout.width = *width;
        }
        layout
    }
    fn recommended_rendering_mode(
        &self,
        id: gpui::FontId,
        size: Pixels,
    ) -> gpui::TextRenderingMode {
        gpui::NoopTextSystem.recommended_rendering_mode(id, size)
    }
}

fn assert_rendered_text(
    selector: &'static str,
    expected: &str,
    text: &RecordingRenderedText,
    cx: &mut VisualTestContext,
) {
    text.0.lock().unwrap().clear();
    // A font reload invalidates GPUI's line cache so this observation belongs to the current mount.
    cx.update(|_, cx| cx.text_system().add_fonts(Vec::new()).unwrap());
    redraw(cx);
    assert!(
        cx.debug_bounds(selector).is_some(),
        "the expected text element must be mounted"
    );
    let lines = text.0.lock().unwrap();
    for line in expected.lines().filter(|line| !line.is_empty()) {
        assert!(
            lines.iter().any(|rendered| rendered == line),
            "the current mount must shape the exact expected text: {line:?}"
        );
    }
}

#[gpui::test]
fn workspace_rows_publish_a_tree_that_selects_on_press(cx: &mut TestAppContext) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.present_desktop_workspace(window, cx);
        });
    });
    cx.run_until_parked();

    let tree = A11yTree::read(cx);
    let list = tree.node("Workspaces");
    assert_eq!(list["aria"]["role"], "Tree");
    let rows = tree.with_role("TreeItem");
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|row| row["aria"]["level"] == 1 && row["aria"]["expanded"].is_null()),
        "Workspaces outside a repository are top-level items with nothing to expand"
    );
    let selected = |tree: &A11yTree| {
        tree.with_role("TreeItem")
            .iter()
            .map(|row| row["aria"]["selected"] == true)
            .collect::<Vec<_>>()
    };
    assert_eq!(selected(&tree), [false, true]);

    perform(cx, rows[0], Action::Click);
    let tree = A11yTree::read(cx);
    assert_eq!(selected(&tree), [true, false]);

    perform(cx, tree.with_role("TreeItem")[0], Action::ShowContextMenu);
    assert_eq!(A11yTree::read(cx).with_role("Menu").len(), 1);
}

/// The main window's controls a reader meets, keyed by the accessible name before any detail.
fn main_window_reading_order(cx: &mut VisualTestContext) -> Vec<String> {
    use spaceterm_ui::a11y_testing::A11yTree;

    const CONTROLS: [&str; 11] = [
        "Hide Sidebar",
        "Show Sidebar",
        "Switch Workspace",
        "Workspaces",
        "Settings",
        "New Workspace",
        "Resize Workspace sidebar",
        "Tabs",
        "Create Tab",
        "Pane Caption",
        "Terminal context actions",
    ];
    A11yTree::read(cx)
        .in_order()
        .into_iter()
        .filter_map(|node| node["aria"]["label"].as_str()?.split(", ").next())
        .filter(|name| CONTROLS.contains(name))
        .map(str::to_owned)
        .collect()
}

/// The roles between the window and the first node with this accessible name.
fn ancestor_roles(cx: &mut VisualTestContext, label: &str) -> Vec<String> {
    use spaceterm_ui::a11y_testing::A11yTree;

    fn walk<'a>(
        tree: &'a A11yTree,
        node: &'a serde_json::Value,
        label: &str,
        path: &mut Vec<&'a serde_json::Value>,
    ) -> bool {
        if node["aria"]["label"] == label {
            return true;
        }
        path.push(node);
        if tree
            .children(node)
            .into_iter()
            .any(|child| walk(tree, child, label, path))
        {
            return true;
        }
        path.pop();
        false
    }

    let tree = A11yTree::read(cx);
    let root = tree.in_order()[0];
    let mut path = Vec::new();
    assert!(
        walk(&tree, root, label, &mut path),
        "no node is named {label:?}"
    );
    path[1..]
        .iter()
        .map(|node| node["aria"]["role"].as_str().unwrap().to_owned())
        .collect()
}

#[gpui::test]
fn main_window_reads_in_presented_layout_order(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);

    // Assistive technology omits generic containers, so ordering adds no node a reader meets.
    for label in ["Hide Sidebar", "Workspaces", "Tabs"] {
        assert!(
            ancestor_roles(cx, label)
                .iter()
                .all(|role| role == "GenericContainer"),
            "{label}"
        );
    }

    assert_eq!(
        main_window_reading_order(cx),
        [
            "Hide Sidebar",
            "Switch Workspace",
            "Workspaces",
            "Settings",
            "New Workspace",
            "Resize Workspace sidebar",
            "Tabs",
            "Create Tab",
            "Pane Caption",
            "Terminal context actions",
        ]
    );

    cx.simulate_keystrokes("cmd-b");
    assert_eq!(
        main_window_reading_order(cx),
        [
            "Show Sidebar",
            "Switch Workspace",
            "Resize Workspace sidebar",
            "Tabs",
            "Create Tab",
            "Pane Caption",
            "Terminal context actions",
        ]
    );
}

/// The widest sidebar the window allows, the same limit a drag clamps to.
fn sidebar_maximum_width(cx: &mut VisualTestContext) -> Pixels {
    cx.update(|window, _| {
        (spaceterm_ui::content_viewport(window).size.width - px(TERMINAL_CONTENT_MINIMUM_WIDTH))
            .min(px(SIDEBAR_MAXIMUM_WIDTH))
            .max(px(WORKSPACE_SIDEBAR_MINIMUM_WIDTH))
    })
}

/// The sidebar Resize Handle's published value, minimum, and maximum.
fn sidebar_splitter_range(cx: &mut VisualTestContext) -> (f64, f64, f64) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let tree = A11yTree::read(cx);
    let splitter = tree.node("Resize Workspace sidebar");
    let number = |key: &str| {
        splitter["aria"][key]
            .as_f64()
            .unwrap_or_else(|| panic!("the sidebar splitter publishes no {key}"))
    };
    (
        number("numeric_value"),
        number("min_numeric_value"),
        number("max_numeric_value"),
    )
}

#[gpui::test]
fn an_opening_sidebar_should_publish_its_resting_width(cx: &mut TestAppContext) {
    use crate::ui::drag_and_drop::AnimatePresence;

    let (manager, _records, cx) = workspace_manager(cx);
    let width = |cx: &mut VisualTestContext| {
        manager.read_with(cx, |manager, cx| {
            f64::from(f32::from(manager.sidebar.read(cx).layout().width))
        })
    };
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    cx.update(|_, cx| cx.set_global(AnimatePresence));
    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.toggle_sidebar(window, cx)));
    sidebar_motion_frame(cx);
    let rest = width(cx);

    let published = sidebar_splitter_range(cx).0;
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    spaceterm_ui::a11y_testing::perform(
        cx,
        tree.node("Resize Workspace sidebar"),
        gpui::accesskit::Action::Increment,
    );
    cx.run_until_parked();

    assert_eq!(
        published, rest,
        "the splitter must publish the sidebar's model width"
    );
    assert!(width(cx) > rest, "Increment must widen the sidebar");
}

#[gpui::test]
fn sidebar_resize_handle_publishes_the_range_its_model_resizes_within(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    let maximum = f64::from(f32::from(sidebar_maximum_width(cx)));

    // Every width below the minimum collapses the sidebar, so the range reaches zero width.
    assert_eq!(
        sidebar_splitter_range(cx),
        (f64::from(WORKSPACE_SIDEBAR_DEFAULT_WIDTH), 0.0, maximum)
    );

    let root = cx
        .debug_bounds("workspace-manager")
        .expect("the Workspace manager was not rendered");
    drag_to(
        "workspace-sidebar-resize-handle",
        root.origin.x + px(300.0),
        cx,
    );
    let width = manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().width);
    assert_eq!(
        sidebar_splitter_range(cx),
        (f64::from(f32::from(width)), 0.0, maximum)
    );

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    let collapsed = cx
        .debug_bounds("workspace-top-chrome")
        .expect("the collapsed top-left chrome was not rendered")
        .size
        .width;
    let collapsed = f64::from(f32::from(collapsed));
    // The handle stays at the collapsed chrome's edge, but the hidden sidebar is zero wide.
    assert_eq!(
        sidebar_splitter_range(cx),
        (0.0, 0.0, maximum.max(collapsed))
    );
}

#[gpui::test]
fn sidebar_resize_handle_moves_to_the_width_assistive_technology_sets(cx: &mut TestAppContext) {
    use gpui::accesskit::{Action, ActionData};
    use spaceterm_ui::a11y_testing::{A11yTree, perform_with};

    let (manager, _records, cx) = workspace_manager(cx);
    let maximum = sidebar_maximum_width(cx);
    let set_width = |width: f64, cx: &mut VisualTestContext| {
        let tree = A11yTree::read(cx);
        perform_with(
            cx,
            tree.node("Resize Workspace sidebar"),
            Action::SetValue,
            Some(ActionData::NumericValue(width)),
        );
        cx.run_until_parked();
    };
    let layout = |cx: &mut VisualTestContext| {
        manager.read_with(cx, |manager, cx| {
            let sidebar = manager.sidebar.read(cx);
            (sidebar.layout().visible, sidebar.layout().width)
        })
    };

    set_width(300.0, cx);
    assert_eq!(layout(cx), (true, px(300.0)));
    assert_eq!(sidebar_splitter_range(cx).0, 300.0);

    set_width(10_000.0, cx);
    assert_eq!(layout(cx), (true, maximum));
    assert_eq!(sidebar_splitter_range(cx).0, f64::from(f32::from(maximum)));

    set_width(-50.0, cx);
    assert!(
        !layout(cx).0,
        "a width below the minimum collapses the sidebar"
    );
    assert!(manager.read_with(cx, |manager, cx| !manager.sidebar.read(cx).is_resizing()));
    assert_eq!(sidebar_splitter_range(cx).0, 0.0);

    // VoiceOver steps a splitter from its published value, so its first step from a hidden
    // sidebar requests a width below the minimum.
    let hidden_width = layout(cx).1;
    set_width(1.0, cx);
    assert_eq!(layout(cx), (true, hidden_width));
    assert_eq!(
        sidebar_splitter_range(cx).0,
        f64::from(f32::from(hidden_width))
    );
}

#[gpui::test]
fn tab_menu_opened_by_assistive_technology_returns_focus_to_its_tab(cx: &mut TestAppContext) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, node_id, perform};

    let (_manager, _records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-t");
    // VoiceOver moves keyboard focus with its cursor, so focus rests on the last focusable
    // control the cursor passed before it reached the Tab.
    let tree = A11yTree::read(cx);
    perform(cx, tree.node("Resize Workspace sidebar"), Action::Focus);
    let tree = A11yTree::read(cx);
    let tab = node_id(tree.with_role("Tab")[0]);
    perform(cx, tree.with_role("Tab")[0], Action::ShowContextMenu);
    assert_eq!(A11yTree::read(cx).with_role("Menu").len(), 1);

    cx.simulate_keystrokes("escape");
    let tree = A11yTree::read(cx);
    assert!(tree.with_role("Menu").is_empty());
    assert_eq!(tree.focused().map(node_id), Some(tab));
}

#[gpui::test]
fn tab_menu_opened_by_the_pointer_returns_focus_to_where_it_was(cx: &mut TestAppContext) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, node_id, perform};

    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.simulate_keystrokes("cmd-t");
    right_click("tab-item-1-inactive", cx);
    assert_eq!(A11yTree::read(cx).with_role("Menu").len(), 1);
    cx.simulate_keystrokes("escape");
    assert!(A11yTree::read(cx).with_role("Menu").is_empty());
    assert!(active_terminal_has_input_focus(&manager, cx));

    let tree = A11yTree::read(cx);
    let handle = node_id(tree.node("Resize Workspace sidebar"));
    perform(cx, tree.node("Resize Workspace sidebar"), Action::Focus);
    right_click("tab-item-1-inactive", cx);
    assert_eq!(A11yTree::read(cx).with_role("Menu").len(), 1);
    cx.simulate_keystrokes("escape");
    let tree = A11yTree::read(cx);
    assert!(tree.with_role("Menu").is_empty());
    assert_eq!(tree.focused().map(node_id), Some(handle));
}

/// Opens a Workspace whose Terminal Panes publish their accessibility nodes.
fn workspace_manager_with_terminal_nodes(
    cx: &mut TestAppContext,
) -> (
    Entity<WorkspaceManager>,
    TestTerminalSessionRecords,
    &mut VisualTestContext,
) {
    use crate::platform::accesskit_terminal_accessibility::AccessKitTerminalAccessibilityAdapterFactory;

    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
    let (manager, cx) = cx.add_window_view(|window, cx| {
        new_desktop_manager(
            session_factory,
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                accessibility: Rc::new(AccessKitTerminalAccessibilityAdapterFactory),
                ..workspace_adapters(test_remote_backend_factory())
            },
            SidebarState::Expanded,
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        window.activate_window();
        manager.update(cx, |manager, cx| manager.focus(window, cx));
    });
    cx.run_until_parked();
    (manager, records, cx)
}

#[gpui::test]
fn pane_split_accessibility_reads_between_its_panes_in_both_orientations(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (_manager, _records, cx) = workspace_manager_with_terminal_nodes(cx);
    for shortcut in ["cmd-d", "cmd-shift-d"] {
        if shortcut == "cmd-shift-d" {
            cx.simulate_keystrokes("cmd-t");
        }
        cx.simulate_keystrokes(shortcut);
        for hidden_sidebar in [false, true] {
            if hidden_sidebar {
                cx.simulate_keystrokes("cmd-b");
            }
            let tree = A11yTree::read(cx);
            let order = tree
                .in_order()
                .into_iter()
                .filter_map(|node| match node["aria"]["role"].as_str()? {
                    "Group"
                        if node["aria"]["label"]
                            .as_str()
                            .is_some_and(|label| label.starts_with("Pane Caption, ")) =>
                    {
                        Some("Pane Caption")
                    }
                    "Terminal" => Some("Terminal"),
                    "Splitter" if node["aria"]["label"] == "Resize Pane split" => {
                        Some("Resize Pane split")
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                order,
                [
                    "Pane Caption",
                    "Terminal",
                    "Resize Pane split",
                    "Pane Caption",
                    "Terminal"
                ],
                "{shortcut}, hidden sidebar: {hidden_sidebar}"
            );
        }
        cx.simulate_keystrokes("cmd-b");
    }
}

/// Children retained by the platform after it removes layout-only containers.
fn platform_children<'a>(
    tree: &'a spaceterm_ui::a11y_testing::A11yTree,
    parent: &serde_json::Value,
) -> Vec<&'a serde_json::Value> {
    tree.children(parent)
        .into_iter()
        .flat_map(|child| {
            if child["aria"]["role"] == "GenericContainer" {
                platform_children(tree, child)
            } else {
                vec![child]
            }
        })
        .collect()
}

#[gpui::test]
fn pane_split_accessibility_scopes_contents_to_its_two_sides(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::{A11yTree, node_id};

    let (_manager, _records, cx) = workspace_manager_with_terminal_nodes(cx);
    for shortcut in ["cmd-d", "cmd-shift-d"] {
        if shortcut == "cmd-shift-d" {
            cx.simulate_keystrokes("cmd-t");
        }
        cx.simulate_keystrokes(shortcut);
        for hidden_sidebar in [false, true] {
            if hidden_sidebar {
                cx.simulate_keystrokes("cmd-b");
            }
            let tree = A11yTree::read(cx);
            let splitter = tree.node("Resize Pane split");
            let parent = tree
                .in_order()
                .into_iter()
                .filter(|node| node["aria"]["role"] != "GenericContainer")
                .find(|node| {
                    platform_children(&tree, node)
                        .iter()
                        .any(|child| node_id(child) == node_id(splitter))
                })
                .expect("the splitter has a platform parent");
            assert_eq!(parent["aria"]["role"], "Group");
            let children = platform_children(&tree, parent);
            assert_eq!(
                children
                    .iter()
                    .map(|node| node["aria"]["role"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                [
                    "Group", "Group", "Terminal", "Splitter", "Group", "Group", "Terminal"
                ],
                "{shortcut}, hidden sidebar: {hidden_sidebar}"
            );
            for caption in [children[0], children[4]] {
                assert!(
                    caption["aria"]["label"]
                        .as_str()
                        .unwrap()
                        .starts_with("Pane Caption, ")
                );
            }
            for context in [children[1], children[5]] {
                assert_eq!(context["aria"]["label"], "Terminal context actions");
            }
        }
        cx.simulate_keystrokes("cmd-b");
    }
}

#[gpui::test]
fn pane_split_accessibility_keeps_nested_splitters_between_their_own_sides(
    cx: &mut TestAppContext,
) {
    use spaceterm_ui::a11y_testing::{A11yTree, node_id};

    let (_manager, _records, cx) = workspace_manager_with_terminal_nodes(cx);
    for shortcut in ["cmd-d", "cmd-shift-d", "cmd-d"] {
        cx.simulate_keystrokes(shortcut);
        cx.run_until_parked();
    }
    for hidden_sidebar in [false, true] {
        if hidden_sidebar {
            cx.simulate_keystrokes("cmd-b");
        }
        let tree = A11yTree::read(cx);
        let captions = tree
            .in_order()
            .into_iter()
            .filter(|node| {
                node["aria"]["label"]
                    .as_str()
                    .is_some_and(|label| label.starts_with("Pane Caption, "))
            })
            .collect::<Vec<_>>();
        let terminals = tree
            .in_order()
            .into_iter()
            .filter(|node| node["aria"]["role"] == "Terminal")
            .collect::<Vec<_>>();
        let contexts = tree
            .in_order()
            .into_iter()
            .filter(|node| node["aria"]["label"] == "Terminal context actions")
            .collect::<Vec<_>>();
        assert_eq!(captions.len(), 4);
        assert_eq!(terminals.len(), 4);
        assert_eq!(contexts.len(), 4);
        let groups = tree
            .in_order()
            .into_iter()
            .filter(|node| {
                node["aria"]["role"] == "Group"
                    && platform_children(&tree, node)
                        .iter()
                        .any(|child| child["aria"]["label"] == "Resize Pane split")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            groups.len(),
            3,
            "each Split retains its own platform parent"
        );
        for (index, group) in groups.iter().enumerate() {
            let children = platform_children(&tree, group);
            let splitter = children
                .iter()
                .find(|node| node["aria"]["role"] == "Splitter")
                .unwrap();
            let mut expected = vec![
                node_id(captions[index]),
                node_id(contexts[index]),
                node_id(terminals[index]),
                node_id(splitter),
            ];
            if index < 2 {
                expected.push(node_id(groups[index + 1]));
            } else {
                expected.extend([
                    node_id(captions[3]),
                    node_id(contexts[3]),
                    node_id(terminals[3]),
                ]);
            }
            assert_eq!(
                children.into_iter().map(node_id).collect::<Vec<_>>(),
                expected,
                "Split {index}, hidden sidebar: {hidden_sidebar}"
            );
        }
    }
}

/// Reports a shell prompt for one Terminal Session, so closing its Pane needs no confirmation.
fn report_idle_prompt(
    records: &TestTerminalSessionRecords,
    session_id: usize,
    cx: &mut VisualTestContext,
) {
    let mut metadata = crate::terminal::metadata::MetadataTracker::new(
        crate::local_path::LocalPathSemantics::Posix,
        "/Users/test",
        "zsh",
        Default::default(),
        Instant::now(),
    );
    assert!(metadata.apply_semantic_prompt("A", Instant::now()));
    let mut screen =
        (*crate::terminal::ScreenSnapshot::empty(crate::local_path::LocalPathSemantics::Posix))
            .clone();
    screen.metadata = metadata.snapshot();
    records
        .event_sender(session_id)
        .expect("the Terminal Session was not started")
        .try_send(TerminalSessionEvent::Screen(Arc::new(screen)))
        .unwrap();
    cx.run_until_parked();
}

/// Returns the "Close Tab" button inside the Tab at this Tab bar position.
fn tab_close_button(
    tree: &spaceterm_ui::a11y_testing::A11yTree,
    index: usize,
) -> &serde_json::Value {
    let mut pending = vec![tree.with_role("Tab")[index]];
    while let Some(node) = pending.pop() {
        if node["aria"]["label"] == "Close Tab" {
            return node;
        }
        pending.extend(tree.children(node));
    }
    panic!("Tab {index} has no Close Tab button");
}

/// Asserts that the Active Tab's terminal holds focus and no node of a closed Tab remains.
fn assert_closed_tab_left_focus_on_the_active_terminal(
    manager: &Entity<WorkspaceManager>,
    closed: &[gpui::accesskit::NodeId],
    cx: &mut VisualTestContext,
) {
    use spaceterm_ui::a11y_testing::{A11yTree, node_id};

    let tree = A11yTree::read(cx);
    assert_eq!(tree.with_role("Tab").len(), 1);
    let focused = tree.focused().expect("a node holds focus");
    assert_eq!(focused["aria"]["role"], "Terminal");
    assert!(active_terminal_has_input_focus(manager, cx));
    let remaining = tree.in_order().into_iter().map(node_id).collect::<Vec<_>>();
    for node in closed {
        assert!(
            !remaining.contains(node),
            "a node of the closed Tab remains"
        );
    }
}

#[gpui::test]
fn closing_an_inactive_tab_from_its_close_button_by_assistive_technology_keeps_focus(
    cx: &mut TestAppContext,
) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, node_id, perform};

    let (manager, records, cx) = workspace_manager_with_terminal_nodes(cx);
    cx.simulate_keystrokes("cmd-t");
    report_idle_prompt(&records, 1, cx);
    // VoiceOver moves keyboard focus with its cursor onto the button it then presses.
    let tree = A11yTree::read(cx);
    let closed = [
        node_id(tree.with_role("Tab")[0]),
        node_id(tab_close_button(&tree, 0)),
    ];
    perform(cx, tab_close_button(&tree, 0), Action::Focus);
    assert_eq!(A11yTree::read(cx).focused().map(node_id), Some(closed[1]));
    let tree = A11yTree::read(cx);
    perform(cx, tab_close_button(&tree, 0), Action::Click);

    assert!(manager.read_with(cx, |manager, _| {
        manager.close_confirmation.pending().is_none()
    }));
    assert_eq!(records.dropped_session_ids(), vec![1]);
    assert_closed_tab_left_focus_on_the_active_terminal(&manager, &closed, cx);
}

#[gpui::test]
fn closing_the_active_tab_from_its_close_button_by_assistive_technology_keeps_focus(
    cx: &mut TestAppContext,
) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, node_id, perform};

    let (manager, records, cx) = workspace_manager_with_terminal_nodes(cx);
    cx.simulate_keystrokes("cmd-t");
    report_idle_prompt(&records, 2, cx);
    let tree = A11yTree::read(cx);
    let closed = [
        node_id(tree.with_role("Tab")[1]),
        node_id(tab_close_button(&tree, 1)),
    ];
    perform(cx, tab_close_button(&tree, 1), Action::Focus);
    assert_eq!(A11yTree::read(cx).focused().map(node_id), Some(closed[1]));
    let tree = A11yTree::read(cx);
    perform(cx, tab_close_button(&tree, 1), Action::Click);

    assert!(manager.read_with(cx, |manager, _| {
        manager.close_confirmation.pending().is_none()
    }));
    assert_eq!(records.dropped_session_ids(), vec![2]);
    assert_closed_tab_left_focus_on_the_active_terminal(&manager, &closed, cx);
}

#[gpui::test]
fn cancelling_a_tab_close_pressed_by_assistive_technology_returns_focus_to_its_button(
    cx: &mut TestAppContext,
) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, node_id, perform};

    let (manager, records, cx) = workspace_manager_with_terminal_nodes(cx);
    cx.simulate_keystrokes("cmd-t");
    let tree = A11yTree::read(cx);
    let button = node_id(tab_close_button(&tree, 0));
    perform(cx, tab_close_button(&tree, 0), Action::Focus);
    let tree = A11yTree::read(cx);
    perform(cx, tab_close_button(&tree, 0), Action::Click);
    assert!(manager.read_with(cx, |manager, _| {
        manager.close_confirmation.pending().is_some()
    }));

    let tree = A11yTree::read(cx);
    perform(cx, tree.node("Cancel"), Action::Click);
    redraw(cx);
    let tree = A11yTree::read(cx);
    assert!(manager.read_with(cx, |manager, _| {
        manager.close_confirmation.pending().is_none()
    }));
    assert_eq!(tree.with_role("Tab").len(), 2);
    assert!(records.dropped_session_ids().is_empty());
    assert_eq!(tree.focused().map(node_id), Some(button));
}

#[gpui::test]
fn close_confirmation_publishes_text_before_actions_and_contains_accessibility(
    cx: &mut TestAppContext,
) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (_manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    A11yTree::read(cx);
    click("tab-close-button-1", cx);
    let tree = A11yTree::read(cx);
    let dialog = tree.node("Close Tab?");
    assert_eq!(dialog["aria"]["role"], "AlertDialog");
    assert_eq!(dialog["aria"]["modal"], true);
    let title = tree.text("Close Tab?");
    let message = tree.text("Close 1 Pane? Running commands in these Panes will stop.");
    let content = tree.descendants(dialog);
    let position = |node: &serde_json::Value| {
        content
            .iter()
            .position(|child| child["accesskit_id"] == node["accesskit_id"])
            .expect("confirmation content belongs to the dialog")
    };
    assert!(position(title) < position(message));
    for name in ["Cancel", "Close Tab"] {
        let button = content
            .iter()
            .find(|node| node["aria"]["role"] == "Button" && node["aria"]["label"] == name)
            .expect("a dialog action");
        assert!(position(message) < position(button));
        assert!(tree.exposed(button));
    }
    assert_eq!(tree.focused().unwrap()["aria"]["label"], "Cancel");
    for name in ["Tabs", "Workspaces", "Terminal context actions"] {
        assert!(!tree.exposed(tree.node(name)), "{name}");
    }
}

#[gpui::test]
fn close_confirmation_restores_accessibility_focus_after_escape_and_cancel(
    cx: &mut TestAppContext,
) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    let (manager, records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    for escape in [true, false] {
        let tree = A11yTree::read(cx);
        perform(cx, tree.node("Close Tab"), Action::Focus);
        let tree = A11yTree::read(cx);
        assert_eq!(tree.focused().unwrap()["aria"]["label"], "Close Tab");
        perform(cx, tree.node("Close Tab"), Action::Click);
        let tree = A11yTree::read(cx);
        assert_eq!(tree.focused().unwrap()["aria"]["label"], "Cancel");
        if escape {
            cx.simulate_keystrokes("escape");
        } else {
            perform(cx, tree.node("Cancel"), Action::Click);
        }
        let tree = A11yTree::read(cx);
        assert!(tree.with_role("AlertDialog").is_empty());
        assert_eq!(tree.focused().unwrap()["aria"]["label"], "Close Tab");
        assert!(tree.exposed(tree.focused().unwrap()));
        assert!(tree.exposed(tree.node("Terminal context actions")));
        assert!(manager.read_with(cx, |manager, _| {
            manager.close_confirmation.pending().is_none()
        }));
        assert!(records.dropped_session_ids().is_empty());
    }
}

#[gpui::test]
fn an_inactive_workspace_menu_should_open_its_tab_there_and_rename_it_in_place(
    cx: &mut TestAppContext,
) {
    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    new_desktop_workspace(cx);
    cx.run_until_parked();

    right_click("workspace-row-1-inactive", cx);
    click("workspace-menu-row-rename", cx);
    cx.simulate_keystrokes("cmd-a D e v enter");
    cx.run_until_parked();
    let renamed = manager.read_with(cx, |manager, _| {
        (
            manager.workspaces.active_id(),
            manager
                .workspaces
                .workspace(WorkspaceId::new(1))
                .unwrap()
                .name()
                .to_owned(),
        )
    });
    let typing_after_rename = active_terminal_has_input_focus(&manager, cx);

    right_click("workspace-row-1-inactive", cx);
    click("workspace-menu-row-new-tab", cx);
    let opened = manager.read_with(cx, |manager, cx| {
        (
            manager.workspaces.active_id(),
            manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .tab_ids()
                .len(),
        )
    });

    assert_eq!(
        renamed,
        (WorkspaceId::new(2), "Dev".to_owned()),
        "renaming leaves the active Workspace active"
    );
    assert!(
        typing_after_rename,
        "a finished rename returns typing to the Pane"
    );
    assert_eq!(
        opened,
        (WorkspaceId::new(1), 2),
        "New Tab opens in its row's Workspace and shows it"
    );
    assert!(active_terminal_has_input_focus(&manager, cx));
}

/// The pin version a shared Workspace's chooser opened at, carried through the completion route
/// the Directory Picker and System Directory Selection share.
#[gpui::test]
fn a_shared_pin_chosen_while_another_client_pins_should_submit_the_version_it_opened_at(
    cx: &mut TestAppContext,
) {
    use crate::ui::shared_workspaces::testing::{ScriptedEdits, ScriptedPanes};
    use crate::ui::shared_workspaces::{SharedEditSource, SharedPaneSessions};
    use spaceterm_server::hierarchy::{Axis, Inventory, Mutation, Pane, Tab, Workspace};

    const SERVER_WORKSPACE: [u8; 32] = [1; 32];
    const SERVER_TAB: [u8; 32] = [2; 32];
    const SERVER_PANE: [u8; 32] = [3; 32];

    // One server Workspace at `pin_version`, with the Pinned Directory it published.
    let published = |revision: u64, pin_version: u64, pinned: Option<PathBuf>| Inventory {
        repositories: None,
        revision,
        close: None,
        workspaces: vec![Workspace {
            id: SERVER_WORKSPACE,
            name: "Build".to_owned(),
            name_version: 0,
            pin_version,
            pinned_directory: pinned,
            root_tab: SERVER_TAB,
            tabs: vec![Tab {
                worktree: None,
                worktree_membership_fixed: false,
                worktree_repository: None,
                id: SERVER_TAB,
                root_pane: SERVER_PANE,
                panes: vec![Pane {
                    id: SERVER_PANE,
                    source: None,
                    axis: Axis::Right,
                    starting_directory: PathBuf::from("/server/work"),
                    confirmed_directory: None,
                    creator: None,
                }],
            }],
        }],
    };

    let chosen = temporary_directory("shared-pin-draft");
    fs::create_dir_all(&chosen).unwrap();
    let (manager, _records, cx) =
        workspace_manager_with_directory_selection([Ok(Some(chosen.clone()))], cx);
    let edits = Rc::new(ScriptedEdits::new(Some(published(1, 0, None))));
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            let session_factory = WorkspaceTerminalSessionFactory::new_local(
                Rc::new(TestTerminalSessionFactory::new(
                    TestTerminalSessionRecords::default(),
                )),
                crate::terminal::testing::test_local_directory(std::env::temp_dir()),
            );
            manager.present_shared_workspaces(
                SharedEditSource::new(Rc::clone(&edits) as Rc<_>),
                SharedPaneSessions::with_opener(
                    ScriptedPanes::new(session_factory) as Rc<_>,
                    &PaneConstruction::testing(),
                ),
                window,
                cx,
            );
            manager.sync_shared_workspaces(ServerKey::LOCAL, window, cx);
        });
    });
    cx.run_until_parked();
    let shared_workspace_id = manager
        .read_with(cx, |manager, _| {
            manager
                .workspaces
                .iter()
                .map(|workspace| workspace.id())
                .find(|id| manager.is_shared_workspace(*id))
        })
        .expect("the published Workspace should join the sidebar");

    // The person opens the chooser while the Workspace sits at pin version 0, and asks for the
    // system chooser from it.
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.choose_pin_directory(shared_workspace_id, window, cx);
        });
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.choose_local_pin_directory(
                PinTarget::Workspace(shared_workspace_id),
                window,
                cx,
            );
        });
    });

    // Another client pins the Workspace while that chooser is still open, and this client reads
    // the publication before the chooser finishes.
    edits.set_inventory(published(2, 1, Some(PathBuf::from("/server/theirs"))));
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.sync_shared_workspaces(ServerKey::LOCAL, window, cx)
        });
    });
    cx.run_until_parked();

    assert_eq!(
        edits.submitted().last(),
        Some(&Mutation::PinWorkspace {
            workspace: SERVER_WORKSPACE,
            version: 0,
            directory: Some(chosen),
        }),
        "the chooser's draft must stay fenced to the pin version it opened at, so the server \
         refuses it instead of overwriting the other client"
    );
}

/// Two servers may publish Workspaces with the same server identity. Each edit reaches the server
/// that owns the Workspace, a folder on this computer never becomes another computer's pin, and
/// disconnecting from one server leaves the other's Workspaces in place.
#[gpui::test]
fn each_servers_workspaces_should_take_edits_and_withdrawal_only_from_their_own_server(
    cx: &mut TestAppContext,
) {
    use crate::ui::shared_workspaces::testing::{ScriptedEdits, ScriptedPanes};
    use crate::ui::shared_workspaces::{SharedEditSource, SharedPaneSessions};
    use spaceterm_server::hierarchy::{Axis, Inventory, Mutation, Pane, Tab, Workspace};

    // Both servers name their only Workspace, Tab, and Pane with the same identities.
    let published = |name: &str| Inventory {
        repositories: None,
        revision: 1,
        close: None,
        workspaces: vec![Workspace {
            id: [1; 32],
            name: name.to_owned(),
            name_version: 0,
            pin_version: 0,
            pinned_directory: None,
            root_tab: [2; 32],
            tabs: vec![Tab {
                worktree: None,
                worktree_membership_fixed: false,
                worktree_repository: None,
                id: [2; 32],
                root_pane: [3; 32],
                panes: vec![Pane {
                    id: [3; 32],
                    source: None,
                    axis: Axis::Right,
                    starting_directory: PathBuf::from("/srv/work"),
                    confirmed_directory: None,
                    creator: None,
                }],
            }],
        }],
    };

    let chosen = temporary_directory("remote-server-local-pin");
    fs::create_dir_all(&chosen).unwrap();
    let (manager, _records, cx) =
        workspace_manager_with_directory_selection([Ok(Some(chosen))], cx);
    let local = Rc::new(ScriptedEdits::new(Some(published("Here"))));
    let remote = Rc::new(ScriptedEdits::new(Some(published("There"))));
    let destination = crate::domain::SshDestination::new("penguin".to_owned()).unwrap();
    let remote_key = cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            let sessions = || {
                SharedPaneSessions::with_opener(
                    ScriptedPanes::new(WorkspaceTerminalSessionFactory::new_local(
                        Rc::new(TestTerminalSessionFactory::new(
                            TestTerminalSessionRecords::default(),
                        )),
                        crate::terminal::testing::test_local_directory(std::env::temp_dir()),
                    )) as Rc<_>,
                    &PaneConstruction::testing(),
                )
            };
            manager.present_shared_workspaces(
                SharedEditSource::new(Rc::clone(&local) as Rc<_>),
                sessions(),
                window,
                cx,
            );
            let remote_key = manager.present_server_workspaces(
                ServerOrigin::Ssh(destination.clone()),
                SharedEditSource::new(Rc::clone(&remote) as Rc<_>),
                sessions(),
                window,
                cx,
            );
            manager.sync_shared_workspaces(ServerKey::LOCAL, window, cx);
            manager.sync_shared_workspaces(remote_key, window, cx);
            remote_key
        })
    });
    cx.run_until_parked();
    let named = |name: &str, cx: &mut VisualTestContext| {
        let name = name.to_owned();
        manager.read_with(cx, move |manager, _| {
            manager
                .workspaces
                .iter()
                .find(|workspace| workspace.name() == name)
                .map(|workspace| workspace.id())
        })
    };
    let here = named("Here", cx).expect("the local server's Workspace should be presented");
    let there = named("There", cx).expect("the SSH server's Workspace should be presented");

    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            assert!(manager.rename_shared_workspace(there, "Renamed", None, window, cx));
        });
    });
    assert_eq!(
        remote.submitted().last(),
        Some(&Mutation::RenameWorkspace {
            workspace: [1; 32],
            version: 0,
            name: "Renamed".to_owned(),
        }),
        "the rename must reach the server that owns the Workspace"
    );
    assert!(
        local.submitted().is_empty(),
        "the local server must not receive another server's edit"
    );

    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.choose_local_pin_directory(PinTarget::Workspace(there), window, cx);
        });
    });
    cx.run_until_parked();
    assert!(
        !remote
            .submitted()
            .iter()
            .chain(local.submitted().iter())
            .any(|mutation| matches!(mutation, Mutation::PinWorkspace { .. })),
        "a folder on this computer must never become another computer's pinned directory"
    );

    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.withdraw_server_workspaces(remote_key, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        named("There", cx),
        None,
        "disconnecting withdraws that server's Workspaces"
    );
    assert_eq!(
        named("Here", cx),
        Some(here),
        "the local server's Workspaces stay presented"
    );
}

/// One inventory at `revision` publishing each named Workspace, identified by its number below 50.
fn machine_inventory(
    revision: u64,
    workspaces: &[(&str, u8)],
) -> spaceterm_server::hierarchy::Inventory {
    use spaceterm_server::hierarchy::{Axis, Inventory, Pane, Tab, Workspace};

    Inventory {
        repositories: None,
        revision,
        close: None,
        workspaces: workspaces
            .iter()
            .map(|(name, number)| Workspace {
                id: [*number; 32],
                name: (*name).to_owned(),
                name_version: 0,
                pin_version: 0,
                pinned_directory: None,
                root_tab: [number + 50; 32],
                tabs: vec![Tab {
                    worktree: None,
                    worktree_membership_fixed: false,
                    worktree_repository: None,
                    id: [number + 50; 32],
                    root_pane: [number + 100; 32],
                    panes: vec![Pane {
                        id: [number + 100; 32],
                        source: None,
                        axis: Axis::Right,
                        starting_directory: PathBuf::from("/srv/work"),
                        confirmed_directory: None,
                        creator: None,
                    }],
                }],
            })
            .collect(),
    }
}

/// A window presenting the account-local server beside SSH servers, each with scripted edits.
struct MachineGroupsHarness {
    manager: Entity<WorkspaceManager>,
    local: Rc<crate::ui::shared_workspaces::testing::ScriptedEdits>,
    /// The revision the next publication carries, so every server reads it as new.
    revision: std::cell::Cell<u64>,
}

impl MachineGroupsHarness {
    fn new(cx: &mut TestAppContext) -> (Self, &mut VisualTestContext) {
        use crate::ui::shared_workspaces::testing::{ScriptedEdits, ScriptedPanes};
        use crate::ui::shared_workspaces::{SharedEditSource, SharedPaneSessions};

        let (manager, _records, cx) = workspace_manager(cx);
        let local = Rc::new(ScriptedEdits::new(Some(machine_inventory(1, &[]))));
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.present_shared_workspaces(
                    SharedEditSource::new(Rc::clone(&local) as Rc<_>),
                    SharedPaneSessions::with_opener(
                        ScriptedPanes::new(Self::session_factory()) as Rc<_>,
                        &PaneConstruction::testing(),
                    ),
                    window,
                    cx,
                );
                manager.sync_shared_workspaces(ServerKey::LOCAL, window, cx);
            });
        });
        cx.run_until_parked();
        (
            Self {
                manager,
                local,
                revision: std::cell::Cell::new(2),
            },
            cx,
        )
    }

    fn session_factory() -> WorkspaceTerminalSessionFactory {
        WorkspaceTerminalSessionFactory::new_local(
            Rc::new(TestTerminalSessionFactory::new(
                TestTerminalSessionRecords::default(),
            )),
            crate::terminal::testing::test_local_directory(std::env::temp_dir()),
        )
    }

    /// Presents the SSH server at `destination`, publishing `workspaces`, and attaches each one.
    fn present_ssh(
        &self,
        destination: &str,
        workspaces: &[(&str, u8)],
        cx: &mut VisualTestContext,
    ) -> (
        ServerKey,
        Rc<crate::ui::shared_workspaces::testing::ScriptedEdits>,
    ) {
        use crate::ui::shared_workspaces::testing::{ScriptedEdits, ScriptedPanes};
        use crate::ui::shared_workspaces::{SharedEditSource, SharedPaneSessions};

        let edits = Rc::new(ScriptedEdits::new(Some(machine_inventory(
            self.next_revision(),
            workspaces,
        ))));
        let destination = crate::domain::SshDestination::new(destination.to_owned()).unwrap();
        let key = cx.update(|window, cx| {
            self.manager.update(cx, |manager, cx| {
                let key = manager.present_server_workspaces(
                    ServerOrigin::Ssh(destination),
                    SharedEditSource::new(Rc::clone(&edits) as Rc<_>),
                    SharedPaneSessions::with_opener(
                        ScriptedPanes::new(Self::session_factory()) as Rc<_>,
                        &PaneConstruction::testing(),
                    ),
                    window,
                    cx,
                );
                manager.sync_shared_workspaces(key, window, cx);
                key
            })
        });
        self.attach(key, workspaces, cx);
        (key, edits)
    }

    /// Publishes `workspaces` from the server at `key` and attaches each one.
    fn publish(
        &self,
        key: ServerKey,
        edits: &crate::ui::shared_workspaces::testing::ScriptedEdits,
        workspaces: &[(&str, u8)],
        cx: &mut VisualTestContext,
    ) {
        edits.set_inventory(machine_inventory(self.next_revision(), workspaces));
        cx.update(|window, cx| {
            self.manager.update(cx, |manager, cx| {
                manager.sync_shared_workspaces(key, window, cx);
            });
        });
        self.attach(key, workspaces, cx);
    }

    /// Attaches each of `workspaces` this client does not present yet, as choosing it in the
    /// Workspace Switcher does.
    fn attach(&self, key: ServerKey, workspaces: &[(&str, u8)], cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            self.manager.update(cx, |manager, cx| {
                for (_, number) in workspaces {
                    let server = [*number; 32];
                    if manager
                        .shared
                        .get(key)
                        .is_some_and(|shared| shared.local(server).is_none())
                    {
                        manager.attach_detached_workspace(key, server, window, cx);
                    }
                }
            });
        });
        cx.run_until_parked();
    }

    fn next_revision(&self) -> u64 {
        let revision = self.revision.get();
        self.revision.set(revision + 1);
        revision
    }

    /// Every presented Workspace's name, in Workspace order.
    fn names(&self, cx: &mut VisualTestContext) -> Vec<String> {
        self.manager.read_with(cx, |manager, _| {
            manager
                .workspaces
                .iter()
                .map(|workspace| workspace.name().to_owned())
                .collect()
        })
    }

    fn named(&self, name: &str, cx: &mut VisualTestContext) -> WorkspaceId {
        let name = name.to_owned();
        self.manager.read_with(cx, move |manager, _| {
            manager
                .workspaces
                .iter()
                .find(|workspace| workspace.name() == name)
                .map(|workspace| workspace.id())
                .expect("the Workspace should be presented")
        })
    }

    fn active_name(&self, cx: &mut VisualTestContext) -> String {
        self.manager.read_with(cx, |manager, _| {
            manager.workspaces.active().name().to_owned()
        })
    }
}

/// Each machine's Workspaces stay together in Workspace order, the local ones first and then each
/// other machine's by name, so a Workspace published later joins the end of its own machine's.
#[gpui::test]
fn workspaces_from_several_servers_should_stay_grouped_by_machine(cx: &mut TestAppContext) {
    let (harness, cx) = MachineGroupsHarness::new(cx);
    harness.publish(ServerKey::LOCAL, &harness.local, &[("Here", 1)], cx);
    let (zeta, zeta_edits) = harness.present_ssh("zeta", &[("Far", 1)], cx);
    harness.publish(
        ServerKey::LOCAL,
        &harness.local,
        &[("Here", 1), ("Also Here", 2)],
        cx,
    );
    harness.present_ssh("Alpha", &[("Near", 1)], cx);
    harness.publish(zeta, &zeta_edits, &[("Far", 1), ("Farther", 2)], cx);

    assert_eq!(
        harness.names(cx),
        ["Default", "Here", "Also Here", "Near", "Far", "Farther",],
        "local Workspaces come first, then each machine's by name regardless of case, and a new \
         Workspace lands at the end of its machine's"
    );
}

/// Moving a Workspace past its machine's Workspaces leaves it at the nearer end of them.
#[gpui::test]
fn moving_a_workspace_should_stay_among_its_machines_workspaces(cx: &mut TestAppContext) {
    let (harness, cx) = MachineGroupsHarness::new(cx);
    harness.publish(ServerKey::LOCAL, &harness.local, &[("Here", 1)], cx);
    harness.present_ssh("penguin", &[("Far", 1), ("Farther", 2)], cx);
    let farther = harness.named("Farther", cx);
    let default = harness.named("Default", cx);

    let move_to = |workspace_id: WorkspaceId, position: usize, cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            harness.manager.update(cx, |manager, cx| {
                manager.handle_sidebar_event(
                    SidebarEvent::Move {
                        workspace_id,
                        position,
                    },
                    window,
                    cx,
                );
            });
        });
    };
    move_to(farther, 0, cx);
    let toward_start = harness.names(cx);
    move_to(default, 3, cx);

    assert_eq!(
        (toward_start, harness.names(cx)),
        (
            vec![
                "Default".to_owned(),
                "Here".to_owned(),
                "Farther".to_owned(),
                "Far".to_owned(),
            ],
            vec![
                "Here".to_owned(),
                "Default".to_owned(),
                "Farther".to_owned(),
                "Far".to_owned(),
            ]
        ),
        "a move past a machine's Workspaces stops at their nearer end"
    );
}

/// Position shortcuts follow the grouped order the sidebar shows, not the order Workspaces arrived.
#[gpui::test]
fn workspace_shortcuts_should_follow_the_grouped_order(cx: &mut TestAppContext) {
    let (harness, cx) = MachineGroupsHarness::new(cx);
    harness.publish(ServerKey::LOCAL, &harness.local, &[("Here", 1)], cx);
    harness.present_ssh("penguin", &[("Far", 1)], cx);
    harness.publish(
        ServerKey::LOCAL,
        &harness.local,
        &[("Here", 1), ("Also Here", 2)],
        cx,
    );

    cx.simulate_keystrokes("ctrl-3");
    cx.run_until_parked();
    let third = harness.active_name(cx);
    cx.simulate_keystrokes("ctrl-4");
    cx.run_until_parked();

    assert_eq!(
        (third, harness.active_name(cx)),
        ("Also Here".to_owned(), "Far".to_owned())
    );
}

/// Disconnecting from one server removes only its machine's Workspaces. The Active Workspace and
/// the order of the others stay as the person left them.
#[gpui::test]
fn withdrawing_one_server_should_keep_the_other_machines_workspaces_and_active_workspace(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = MachineGroupsHarness::new(cx);
    harness.publish(
        ServerKey::LOCAL,
        &harness.local,
        &[("Here", 1), ("Also Here", 2)],
        cx,
    );
    let (penguin, _) = harness.present_ssh("penguin", &[("Far", 1)], cx);
    let here = harness.named("Here", cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.handle_sidebar_event(
                SidebarEvent::Move {
                    workspace_id: here,
                    position: 2,
                },
                window,
                cx,
            );
            manager.activate_workspace(here, window, cx);
        });
    });

    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.withdraw_server_workspaces(penguin, window, cx);
        });
    });
    cx.run_until_parked();

    assert_eq!(
        (harness.names(cx), harness.active_name(cx)),
        (
            vec![
                "Default".to_owned(),
                "Also Here".to_owned(),
                "Here".to_owned(),
            ],
            "Here".to_owned()
        )
    );
}

/// An SSH registry whose every connection waits for the person to sign in, recording each
/// request it receives.
fn waiting_ssh_registry(
    cx: &mut VisualTestContext,
) -> (
    crate::ssh::server_connection::SshServerConnections,
    std::sync::mpsc::Receiver<String>,
) {
    use crate::ssh::cancellation::SshCancellationToken;
    use crate::ssh::server_connection::{
        ConnectMode, ConnectProgress, ConnectedSshServer, InteractionReason, RetainedSshConnection,
        SshConnectBackend, SshConnectFailure, SshConnectionIntentStore, SshIntentStoreError,
        SshServerConnections, SshServerContinuity, SshServerObservation,
    };
    use std::sync::{Arc, mpsc};

    struct WaitingBackend(Mutex<mpsc::Sender<String>>);
    impl SshConnectBackend for WaitingBackend {
        fn connect(
            &self,
            destination: &crate::domain::SshDestination,
            _: ConnectMode,
            _: Option<&SshServerContinuity>,
            _: &SshCancellationToken,
            _: &ConnectProgress<'_>,
        ) -> Result<ConnectedSshServer, SshConnectFailure> {
            let _ = self.0.lock().unwrap().send(destination.as_str().to_owned());
            Err(SshConnectFailure::NeedsPerson(
                InteractionReason::Authentication,
            ))
        }
        fn observe(
            &self,
            _: &crate::domain::SshDestination,
            _: Option<&SshServerContinuity>,
            _: &SshCancellationToken,
            _: &ConnectProgress<'_>,
        ) -> Result<SshServerObservation, SshConnectFailure> {
            Err(SshConnectFailure::NeedsPerson(
                InteractionReason::Authentication,
            ))
        }
    }
    struct NoIntents;
    impl SshConnectionIntentStore for NoIntents {
        fn load(&self) -> Result<Vec<RetainedSshConnection>, SshIntentStoreError> {
            Ok(Vec::new())
        }
        fn retain(&self, _: RetainedSshConnection) -> Result<(), SshIntentStoreError> {
            Ok(())
        }
        fn remove(&self, _: &crate::domain::SshDestination) -> Result<(), SshIntentStoreError> {
            Ok(())
        }
    }

    let (sender, connects) = mpsc::channel();
    let connections = SshServerConnections::new(
        Arc::new(WaitingBackend(Mutex::new(sender))),
        Arc::new(NoIntents),
    );
    let installed = connections.clone();
    cx.update(|_, cx| crate::ui::ssh_servers::install(installed, None, cx));
    (connections, connects)
}

/// Waits until the registry reports that `destination` waits for the person.
fn wait_for_person(
    connections: &crate::ssh::server_connection::SshServerConnections,
    destination: &str,
) {
    use crate::ssh::server_connection::RemoteConnectionStatus;

    let destination = crate::domain::SshDestination::new(destination.to_owned()).unwrap();
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while !matches!(
        connections.status(&destination),
        RemoteConnectionStatus::NeedsPerson { .. }
    ) {
        assert!(
            Instant::now() < deadline,
            "the connection should come to wait for the person"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn heading_names(cx: &mut VisualTestContext) -> Vec<String> {
    spaceterm_ui::a11y_testing::A11yTree::read(cx)
        .with_role("Heading")
        .iter()
        .filter_map(|node| node["aria"]["label"].as_str().map(str::to_owned))
        .collect()
}

/// One machine's Workspaces need no heading. Once another machine's server is presented, each
/// machine heads its own Workspaces, and each row names its machine to assistive technology.
#[gpui::test]
fn machine_headings_should_appear_only_once_a_second_machine_is_presented(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (harness, cx) = MachineGroupsHarness::new(cx);
    harness.publish(ServerKey::LOCAL, &harness.local, &[("Here", 1)], cx);
    redraw(cx);
    let alone = (
        cx.debug_bounds("machine-heading-This Computer").is_some(),
        heading_names(cx),
    );

    harness.present_ssh("penguin", &[("Far", 1)], cx);
    redraw(cx);
    let here = harness.named("Here", cx).get();
    let far = harness.named("Far", cx).get();
    let bounds = |selector: String, cx: &mut VisualTestContext| {
        cx.debug_bounds(selector.leak())
            .expect("the sidebar should present it")
    };
    let local = bounds("machine-heading-This Computer".to_owned(), cx);
    let first = bounds("workspace-row-1-inactive".to_owned(), cx);
    let here_row = bounds(format!("workspace-group-{here}"), cx);
    let remote = bounds("machine-heading-penguin".to_owned(), cx);
    let far_row = bounds(format!("workspace-group-{far}"), cx);

    assert_eq!(alone, (false, Vec::<String>::new()));
    assert!(
        local.bottom() <= first.top()
            && here_row.bottom() <= remote.top()
            && remote.bottom() <= far_row.top(),
        "each machine's heading stands above its own Workspaces"
    );
    assert_eq!(heading_names(cx), ["This Computer", "penguin"]);
    let far_item = A11yTree::read(cx)
        .with_role("TreeItem")
        .into_iter()
        .find(|node| node["aria"]["label"] == "Far")
        .map(|node| {
            node["aria"]["description"]
                .as_str()
                .unwrap_or("")
                .to_owned()
        });
    assert!(
        far_item.is_some_and(|description| description.starts_with("penguin")),
        "a row reached on its own names its machine"
    );
}

/// A presented SSH server that isn't connected says so in its heading. Reading the state asks
/// that server for nothing.
#[gpui::test]
fn an_ssh_machine_heading_should_show_a_connection_that_is_not_connected(cx: &mut TestAppContext) {
    let (harness, cx) = MachineGroupsHarness::new(cx);
    harness.publish(ServerKey::LOCAL, &harness.local, &[("Here", 1)], cx);
    let (connections, connects) = waiting_ssh_registry(cx);
    harness.present_ssh("penguin", &[("Far", 1)], cx);

    connections.connect(crate::domain::SshDestination::new("penguin".to_owned()).unwrap());
    wait_for_person(&connections, "penguin");
    assert_eq!(connects.try_recv().ok().as_deref(), Some("penguin"));
    redraw(cx);

    assert!(
        cx.debug_bounds("machine-heading-status-penguin").is_some(),
        "the heading should show the connection's state"
    );
    assert_eq!(
        heading_names(cx),
        ["This Computer", "penguin, Waiting for You"]
    );
    assert!(
        connects.try_recv().is_err(),
        "presenting the state asks the server for nothing"
    );
}

/// A presented SSH server with no attached Workspaces keeps its heading, with a quiet line in
/// place of its Workspaces.
#[gpui::test]
fn an_empty_ssh_server_should_keep_its_heading_and_say_it_has_no_workspaces(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = MachineGroupsHarness::new(cx);
    harness.publish(ServerKey::LOCAL, &harness.local, &[("Here", 1)], cx);
    harness.present_ssh("penguin", &[], cx);
    redraw(cx);

    let heading = cx.debug_bounds("machine-heading-penguin");
    let empty = cx.debug_bounds("machine-empty-penguin");
    assert!(
        heading
            .zip(empty)
            .is_some_and(|(heading, empty)| heading.bottom() <= empty.top()),
        "the empty server's heading should stand above its No Workspaces line"
    );
    assert!(
        cx.debug_bounds("machine-empty-This Computer").is_none(),
        "a machine with Workspaces lists them instead"
    );
    assert!(
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
            .find("No Workspaces")
            .is_some()
    );
}

/// Dragging a Workspace row past its machine's heading lands it at the nearer end of that
/// machine's Workspaces.
#[gpui::test]
fn dragging_a_workspace_past_its_machine_heading_should_stay_with_its_machine(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = MachineGroupsHarness::new(cx);
    harness.present_ssh("penguin", &[("Far", 1), ("Farther", 2)], cx);
    harness.manager.update(cx, |manager, cx| {
        manager
            .workspaces
            .activate_workspace(WorkspaceId::new(1))
            .unwrap();
        cx.notify();
    });
    redraw(cx);
    let farther = harness.named("Farther", cx).get();
    let far = harness.named("Far", cx).get();
    let row = cx
        .debug_bounds(format!("workspace-row-{farther}-inactive").leak())
        .expect("the row to drag");
    let group_start = cx
        .debug_bounds(format!("workspace-row-{far}-inactive").leak())
        .expect("the machine's first row")
        .origin;
    let top = cx
        .debug_bounds("workspace-row-1-active")
        .expect("the local row")
        .center();

    cx.simulate_mouse_move(row.center(), None, Modifiers::none());
    cx.simulate_mouse_down(row.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        row.center() - point(px(0.0), px(8.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.run_until_parked();
    cx.simulate_mouse_move(top, MouseButton::Left, Modifiers::none());
    finish_slides(cx);
    let landing = cx
        .debug_bounds(format!("workspace-row-{farther}-inactive").leak())
        .map(|face| face.origin);
    cx.simulate_mouse_up(top, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(
        landing,
        Some(group_start),
        "the dragged row's place opens at the start of its machine's Workspaces"
    );
    assert_eq!(harness.names(cx), ["Default", "Farther", "Far"]);
}

/// A machine heading's menu reaches that machine's server: Connect asks the registry as Settings
/// does, and every heading offers the Servers Settings Section.
#[gpui::test]
fn a_machine_headings_menu_should_connect_its_server(cx: &mut TestAppContext) {
    let (harness, cx) = MachineGroupsHarness::new(cx);
    let (connections, connects) = waiting_ssh_registry(cx);
    harness.present_ssh("penguin", &[("Far", 1)], cx);
    connections.connect(crate::domain::SshDestination::new("penguin".to_owned()).unwrap());
    wait_for_person(&connections, "penguin");
    let _ = connects.try_recv();
    redraw(cx);

    right_click("machine-heading-This Computer", cx);
    let local = (
        cx.debug_bounds("machine-menu-server-settings").is_some(),
        cx.debug_bounds("machine-menu-connect").is_some(),
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    right_click("machine-heading-penguin", cx);
    let remote = (
        cx.debug_bounds("machine-menu-server-settings").is_some(),
        cx.debug_bounds("machine-menu-disconnect").is_some(),
    );
    click("machine-menu-connect", cx);

    assert_eq!(local, (true, false));
    assert_eq!(remote, (true, true));
    assert_eq!(
        connects
            .recv_timeout(std::time::Duration::from_secs(5))
            .ok()
            .as_deref(),
        Some("penguin"),
        "Connect asks the registry to connect to the heading's machine"
    );
}

struct SettingsMovement;

impl crate::platform::window_movement::WindowMovementFactory for SettingsMovement {
    fn create(
        &self,
    ) -> Rc<dyn crate::platform::window_movement::OperatingSystemWindowDragPlatform> {
        Rc::new(RecordingOperatingSystemWindowDragPlatform::default())
    }
}

/// A machine heading's Server Settings opens Settings with that machine's server selected.
#[gpui::test]
fn a_machine_headings_server_settings_should_select_its_server(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (harness, cx) = MachineGroupsHarness::new(cx);
    let (connections, _connects) = waiting_ssh_registry(cx);
    for name in ["alpha", "penguin"] {
        harness.present_ssh(name, &[], cx);
        connections.connect(crate::domain::SshDestination::new(name.to_owned()).unwrap());
        wait_for_person(&connections, name);
    }
    let settings = crate::settings::Settings::load(
        crate::settings::storage::testing::MemoryStorage::with_document(
            &crate::settings::SettingsDocument::default(),
        ),
    );
    cx.update(|_, cx| {
        crate::ui::appearance_runtime::install(
            settings,
            Rc::new(crate::platform::appearance::testing::RecordingAppearancePlatform::default()),
            cx,
        )
        .expect("appearance runtime should install");
        crate::ui::settings_window::configure_window_chrome(
            Rc::new(SettingsMovement),
            Default::default(),
            None,
            cx,
        );
    });
    redraw(cx);

    right_click("machine-heading-penguin", cx);
    click("machine-menu-server-settings", cx);

    let settings_window = cx
        .windows()
        .into_iter()
        .find(|window| {
            window
                .downcast::<crate::ui::settings_window::SettingsWindow>()
                .is_some()
        })
        .expect("Settings should open");
    let mut settings_cx = VisualTestContext::from_window(settings_window, cx);
    settings_cx.run_until_parked();
    let selected: Vec<_> = A11yTree::read(&mut settings_cx)
        .with_role("ListBoxOption")
        .into_iter()
        .filter(|option| option["aria"]["selected"] == true)
        .filter_map(|option| option["aria"]["label"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        selected,
        ["Servers", "penguin"],
        "Settings presents the Servers section with the heading's server selected"
    );
}

/// A machine with only native Remote Workspaces has no presented SSH server, so its heading offers
/// no Connect that would set one up there and no Disconnect, whatever the registry reports for its
/// destination.
#[gpui::test]
fn a_machine_with_only_remote_workspaces_should_offer_no_connection_controls(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = MachineGroupsHarness::new(cx);
    // The registry reads "work" as an inactive host, whose status alone would offer Connect.
    let (_connections, _connects) = waiting_ssh_registry(cx);
    create_disconnected_remote_workspace(&harness.manager, cx);
    redraw(cx);

    assert!(
        cx.debug_bounds("machine-heading-work").is_some(),
        "the machine still heads its Workspaces"
    );
    assert!(
        cx.debug_bounds("machine-heading-work-menu").is_none(),
        "no SSH server is saved or presented there, so its heading offers nothing"
    );
}

/// A machine heading's Disconnect ends that machine's connection and leaves the others connected.
#[gpui::test]
fn a_machine_headings_menu_should_disconnect_only_its_server(cx: &mut TestAppContext) {
    use crate::ssh::server_connection::RemoteConnectionStatus;

    let (harness, cx) = MachineGroupsHarness::new(cx);
    let (connections, _connects) = waiting_ssh_registry(cx);
    for name in ["penguin", "zeta"] {
        harness.present_ssh(name, &[], cx);
        connections.connect(crate::domain::SshDestination::new(name.to_owned()).unwrap());
        wait_for_person(&connections, name);
    }
    redraw(cx);

    right_click("machine-heading-penguin", cx);
    click("machine-menu-disconnect", cx);

    let status = |name: &str| {
        connections.status(&crate::domain::SshDestination::new(name.to_owned()).unwrap())
    };
    assert!(matches!(
        status("penguin"),
        RemoteConnectionStatus::Inactive
    ));
    assert!(matches!(
        status("zeta"),
        RemoteConnectionStatus::NeedsPerson { .. }
    ));
}

/// A shared server Workspace presented in this window, with the server Pane its scripted
/// connection drives. Its Panes reach the same presentation the local Development server's do.
struct SharedPaneHarness {
    manager: Entity<WorkspaceManager>,
    panes: Rc<crate::ui::shared_workspaces::testing::ScriptedPanes>,
}

const SHARED_FOCUS_WORKSPACE: [u8; 32] = [11; 32];
const SHARED_FOCUS_TAB: [u8; 32] = [12; 32];
const SHARED_FOCUS_PANE: [u8; 32] = [13; 32];

/// The Workspace the sidebar lists second, which is the shared one every harness here publishes.
const SHARED_WORKSPACE_ROW: WorkspaceId = WorkspaceId::new(2);

/// One connected shared Workspace, opened from the sidebar so its Pane is the Focused Pane of the
/// Active Workspace and holds the keyboard.
fn shared_pane_harness(cx: &mut TestAppContext) -> (SharedPaneHarness, &mut VisualTestContext) {
    use crate::ui::shared_workspaces::testing::{ScriptedEdits, ScriptedPanes};
    use crate::ui::shared_workspaces::{SharedEditSource, SharedPaneSessions};
    use spaceterm_server::hierarchy::{Axis, Inventory, Pane, Tab, Workspace};

    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let published = Inventory {
        repositories: None,
        revision: 1,
        close: None,
        workspaces: vec![Workspace {
            id: SHARED_FOCUS_WORKSPACE,
            name: "Build".to_owned(),
            name_version: 0,
            pin_version: 0,
            pinned_directory: None,
            root_tab: SHARED_FOCUS_TAB,
            tabs: vec![Tab {
                worktree: None,
                worktree_membership_fixed: false,
                worktree_repository: None,
                id: SHARED_FOCUS_TAB,
                root_pane: SHARED_FOCUS_PANE,
                panes: vec![Pane {
                    id: SHARED_FOCUS_PANE,
                    source: None,
                    axis: Axis::Right,
                    starting_directory: PathBuf::from("/server/work"),
                    confirmed_directory: None,
                    creator: None,
                }],
            }],
        }],
    };
    let edits = Rc::new(ScriptedEdits::new(Some(published)));
    let session_factory = WorkspaceTerminalSessionFactory::new_local(
        Rc::new(TestTerminalSessionFactory::new(
            TestTerminalSessionRecords::default(),
        )),
        crate::terminal::testing::test_local_directory(std::env::temp_dir()),
    );
    let panes = ScriptedPanes::new(session_factory);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.present_shared_workspaces(
                SharedEditSource::new(Rc::clone(&edits) as Rc<_>),
                SharedPaneSessions::with_opener(
                    Rc::clone(&panes) as Rc<_>,
                    &PaneConstruction::testing(),
                ),
                window,
                cx,
            );
            manager.sync_shared_workspaces(ServerKey::LOCAL, window, cx);
        });
    });
    cx.run_until_parked();
    let harness = SharedPaneHarness { manager, panes };
    assert!(
        harness.manager.read_with(cx, |manager, _| manager
            .is_shared_workspace(SHARED_WORKSPACE_ROW)),
        "the published Workspace should join the sidebar as the second row"
    );
    // The server reaches the Pane and publishes this client's screen, as a fresh connection does.
    harness.panes.pane(SHARED_FOCUS_PANE).reconnect();
    settle_shared_panes(cx);
    // A new client's first view opens the server's Workspace and focuses its Focused Pane.
    focus_active_workspace(&harness.manager, cx);
    assert!(
        shared_terminal_is_focused(&harness, cx),
        "opening the shared Workspace should leave the keyboard in its Pane"
    );
    (harness, cx)
}

/// Returns the keyboard to the Active Workspace once its Pane's screen is connected.
fn focus_active_workspace(manager: &Entity<WorkspaceManager>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.focus(window, cx)));
    cx.run_until_parked();
}

/// Lets every shared Pane read its connection and paints the frame that follows.
fn settle_shared_panes(cx: &mut VisualTestContext) {
    cx.executor()
        .advance_clock(crate::ui::shared_workspaces::POLL_INTERVAL * 3);
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

fn shared_terminal_is_focused(harness: &SharedPaneHarness, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| {
        harness
            .manager
            .read(cx)
            .workspaces
            .active()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    })
}

fn sidebar_rename_is_focused(harness: &SharedPaneHarness, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| {
        harness
            .manager
            .read(cx)
            .sidebar
            .read(cx)
            .rename_is_focused(window)
    })
}

/// Starts the inline rename of the Workspace the sidebar lists first, which the shared Workspace
/// is not, so the keyboard sits in an editor outside the Pane.
fn start_sidebar_rename(harness: &SharedPaneHarness, cx: &mut VisualTestContext) {
    right_click("workspace-row-1-inactive", cx);
    click("workspace-menu-row-rename", cx);
    assert!(
        sidebar_rename_is_focused(harness, cx),
        "the inline rename should hold the keyboard"
    );
}

/// A reconnection is server news rather than a person's action, so it returns the keyboard only to
/// the Pane that already held it.
#[gpui::test]
fn a_shared_pane_reconnection_should_return_focus_only_to_the_pane_that_held_it(
    cx: &mut TestAppContext,
) {
    use crate::terminal::server_session::DevelopmentPaneStatus;

    let (harness, cx) = shared_pane_harness(cx);
    let pane = harness.panes.pane(SHARED_FOCUS_PANE);
    start_sidebar_rename(&harness, cx);

    pane.set(DevelopmentPaneStatus::Reconnecting);
    settle_shared_panes(cx);
    pane.reconnect();
    settle_shared_panes(cx);

    assert!(
        sidebar_rename_is_focused(&harness, cx),
        "a reconnection must leave the keyboard in the editor a person is typing in"
    );
    assert!(
        !shared_terminal_is_focused(&harness, cx),
        "a reconnection must not pull focus into the Pane's terminal"
    );

    // The Pane a person left the keyboard in gets it back, so typing returns to the shell.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    click("workspace-row-2-active", cx);
    assert!(shared_terminal_is_focused(&harness, cx));
    pane.set(DevelopmentPaneStatus::Reconnecting);
    settle_shared_panes(cx);
    pane.reconnect();
    settle_shared_panes(cx);

    assert!(
        shared_terminal_is_focused(&harness, cx),
        "the Pane that held the keyboard should keep it once its screens return"
    );
}

/// Another client's takeover replaces the Pane, so the keyboard leaves its terminal for the screen
/// that explains the change, and nothing else loses the keyboard.
#[gpui::test]
fn a_shared_pane_takeover_should_take_focus_only_from_the_pane_that_held_it(
    cx: &mut TestAppContext,
) {
    use crate::terminal::server_session::DevelopmentPaneStatus;

    let (harness, cx) = shared_pane_harness(cx);
    let pane = harness.panes.pane(SHARED_FOCUS_PANE);

    pane.set(DevelopmentPaneStatus::Takeover);
    settle_shared_panes(cx);

    assert!(
        cx.debug_bounds("shared-pane-takeover-1").is_some(),
        "another client's control should replace the Pane with its screen"
    );
    assert!(
        !shared_terminal_is_focused(&harness, cx),
        "typing must not reach a shell this client no longer controls"
    );

    // Control returns, so the screen hands the keyboard back to the terminal it replaced.
    pane.reconnect();
    settle_shared_panes(cx);
    assert!(
        shared_terminal_is_focused(&harness, cx),
        "the Pane should take the keyboard back from the screen it replaced"
    );

    // The same takeover while a person types in the sidebar leaves that editor alone.
    start_sidebar_rename(&harness, cx);
    pane.set(DevelopmentPaneStatus::Takeover);
    settle_shared_panes(cx);

    assert!(
        sidebar_rename_is_focused(&harness, cx),
        "another client's takeover must not pull the keyboard out of the rename editor"
    );
    assert!(
        cx.debug_bounds("shared-pane-takeover-1").is_some(),
        "the screen should still replace the Pane it took over"
    );
}

/// A shared Workspace whose Root Tab is split in two beside a second Tab, presented in this window
/// with the scripted server that answers its edits.
struct SharedCloseHarness {
    manager: Entity<WorkspaceManager>,
    edits: Rc<crate::ui::shared_workspaces::testing::ScriptedEdits>,
    panes: Rc<crate::ui::shared_workspaces::testing::ScriptedPanes>,
}

const SHARED_CLOSE_WORKSPACE: [u8; 32] = [21; 32];
const SHARED_CLOSE_ROOT_TAB: [u8; 32] = [22; 32];
const SHARED_CLOSE_ROOT_PANE: [u8; 32] = [23; 32];
const SHARED_CLOSE_SPLIT_PANE: [u8; 32] = [24; 32];
const SHARED_CLOSE_SECOND_TAB: [u8; 32] = [25; 32];
const SHARED_CLOSE_SECOND_PANE: [u8; 32] = [26; 32];
const SHARED_CLOSE_NEW_PANE: [u8; 32] = [27; 32];
const SHARED_CLOSE_BOOT: [u8; 32] = [29; 32];

/// One published Pane and the live Session it presents, if a shell runs.
type SharedClosePane = ([u8; 32], Option<[u8; 32]>);

/// The published inventory: each Tab and its Panes, with every Pane's live Session.
fn shared_close_inventory(
    revision: u64,
    tabs: &[([u8; 32], &[SharedClosePane])],
) -> spaceterm_server::hierarchy::Inventory {
    use spaceterm_server::hierarchy::{
        Axis, CloseInventory, ConfirmedPane, Inventory, Pane, Tab, Workspace,
    };

    let pane = |id: [u8; 32], source: Option<[u8; 32]>| Pane {
        id,
        source,
        axis: Axis::Right,
        starting_directory: PathBuf::from("/server/work"),
        confirmed_directory: None,
        creator: None,
    };
    let workspace_tabs = tabs
        .iter()
        .map(|(tab, panes)| Tab {
            worktree: None,
            worktree_membership_fixed: false,
            worktree_repository: None,
            id: *tab,
            root_pane: panes[0].0,
            panes: panes
                .iter()
                .enumerate()
                .map(|(index, (id, _))| pane(*id, (index > 0).then_some(panes[0].0)))
                .collect(),
        })
        .collect::<Vec<_>>();
    Inventory {
        repositories: None,
        revision,
        close: Some(CloseInventory {
            boot: SHARED_CLOSE_BOOT,
            sessions: tabs
                .iter()
                .flat_map(|(_, panes)| panes.iter())
                .map(|(id, session)| ConfirmedPane {
                    id: *id,
                    session: *session,
                })
                .collect(),
        }),
        workspaces: vec![Workspace {
            id: SHARED_CLOSE_WORKSPACE,
            name: "Build".to_owned(),
            name_version: 0,
            pin_version: 0,
            pinned_directory: None,
            root_tab: tabs[0].0,
            tabs: workspace_tabs,
        }],
    }
}

fn shared_close_published() -> spaceterm_server::hierarchy::Inventory {
    shared_close_inventory(
        1,
        &[
            (
                SHARED_CLOSE_ROOT_TAB,
                &[
                    (SHARED_CLOSE_ROOT_PANE, Some([31; 32])),
                    (SHARED_CLOSE_SPLIT_PANE, Some([32; 32])),
                ],
            ),
            (SHARED_CLOSE_SECOND_TAB, &[(SHARED_CLOSE_SECOND_PANE, None)]),
        ],
    )
}

/// Opens the shared Workspace from the sidebar so its Root Pane holds the keyboard.
fn shared_close_harness(cx: &mut TestAppContext) -> (SharedCloseHarness, &mut VisualTestContext) {
    use crate::ui::shared_workspaces::testing::{ScriptedEdits, ScriptedPanes};
    use crate::ui::shared_workspaces::{SharedEditSource, SharedPaneSessions};

    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let edits = Rc::new(ScriptedEdits::new(Some(shared_close_published())));
    let panes = ScriptedPanes::new(WorkspaceTerminalSessionFactory::new_local(
        Rc::new(TestTerminalSessionFactory::new(
            TestTerminalSessionRecords::default(),
        )),
        crate::terminal::testing::test_local_directory(std::env::temp_dir()),
    ));
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.present_shared_workspaces(
                SharedEditSource::new(Rc::clone(&edits) as Rc<_>),
                SharedPaneSessions::with_opener(
                    Rc::clone(&panes) as Rc<_>,
                    &PaneConstruction::testing(),
                ),
                window,
                cx,
            );
            manager.sync_shared_workspaces(ServerKey::LOCAL, window, cx);
        });
    });
    cx.run_until_parked();
    let harness = SharedCloseHarness {
        manager,
        edits,
        panes,
    };
    harness.panes.pane(SHARED_CLOSE_ROOT_PANE).reconnect();
    settle_shared_panes(cx);
    focus_active_workspace(&harness.manager, cx);
    (harness, cx)
}

impl SharedCloseHarness {
    /// The server publishes `inventory`, and the window reads it.
    fn publish(
        &self,
        inventory: spaceterm_server::hierarchy::Inventory,
        cx: &mut VisualTestContext,
    ) {
        self.edits.set_inventory(inventory);
        self.sync(cx);
    }

    fn sync(&self, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            self.manager.update(cx, |manager, cx| {
                manager.sync_shared_workspaces(ServerKey::LOCAL, window, cx)
            });
        });
        redraw(cx);
    }

    fn closes(&self) -> Vec<spaceterm_server::hierarchy::Mutation> {
        self.edits
            .submitted()
            .into_iter()
            .filter(|mutation| {
                matches!(
                    mutation,
                    spaceterm_server::hierarchy::Mutation::Close { .. }
                )
            })
            .collect()
    }

    fn shared_rows(&self, cx: &mut VisualTestContext) -> usize {
        self.manager.read_with(cx, |manager, _| {
            manager
                .workspaces
                .iter()
                .filter(|workspace| manager.is_shared_workspace(workspace.id()))
                .count()
        })
    }
}

/// The largest part of the hierarchy a shared close removes, as the server reports it.
#[derive(Clone, Copy)]
enum SharedCloseRemoves {
    Pane,
    Tab,
    Workspace,
}

fn shared_close_mutation(
    target: spaceterm_server::hierarchy::CloseTarget,
    panes: &[SharedClosePane],
    removes: SharedCloseRemoves,
) -> spaceterm_server::hierarchy::Mutation {
    use spaceterm_server::hierarchy::{CloseConfirmation, ConfirmedPane, Mutation};

    Mutation::Close {
        target,
        confirmation: CloseConfirmation {
            boot: SHARED_CLOSE_BOOT,
            panes: panes
                .iter()
                .map(|(id, session)| ConfirmedPane {
                    id: *id,
                    session: *session,
                })
                .collect(),
            removes_tab: !matches!(removes, SharedCloseRemoves::Pane),
            removes_workspace: matches!(removes, SharedCloseRemoves::Workspace),
        },
    }
}

/// Kill Workspace on a shared Workspace asks its server. Its confirmation starts on
/// Cancel, and confirming asks the server to end every Pane and Session it showed; the Workspace
/// leaves this window only when the server publishes its removal.
#[gpui::test]
fn shared_kill_workspace_should_confirm_then_end_exactly_what_it_showed_for_every_client(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::{CloseTarget as ServerCloseTarget, Inventory, Outcome};

    let (harness, cx) = shared_close_harness(cx);
    right_click("workspace-row-2-active", cx);
    click("workspace-menu-row-close", cx);
    redraw(cx);

    assert!(
        cx.debug_bounds("modal-action-shared-close-cancel-keyboard-focus")
            .is_some(),
        "a destructive confirmation should start on Cancel"
    );
    assert!(
        harness.closes().is_empty(),
        "nothing ends before confirmation"
    );

    click("modal-action-shared-close-confirm", cx);
    redraw(cx);
    assert_eq!(
        harness.closes(),
        vec![shared_close_mutation(
            ServerCloseTarget::Workspace {
                workspace: SHARED_CLOSE_WORKSPACE,
            },
            &[
                (SHARED_CLOSE_ROOT_PANE, Some([31; 32])),
                (SHARED_CLOSE_SPLIT_PANE, Some([32; 32])),
                (SHARED_CLOSE_SECOND_PANE, None),
            ],
            SharedCloseRemoves::Workspace,
        )]
    );
    assert_eq!(
        harness.shared_rows(cx),
        1,
        "the Workspace stays until the server publishes its removal"
    );

    harness.edits.answer(Outcome::Accepted {
        target: SHARED_CLOSE_WORKSPACE,
    });
    harness.publish(
        Inventory {
            repositories: None,
            revision: 2,
            workspaces: Vec::new(),
            close: None,
        },
        cx,
    );
    assert_eq!(harness.shared_rows(cx), 0);
    assert!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.workspaces.len())
            == 1,
        "this window keeps its own Workspace"
    );
    assert!(
        shared_close_terminal_is_focused(&harness, cx),
        "the Workspace that takes its place takes the keyboard, as a local close does"
    );
}

/// Cmd+W runs the retained close_workspace Command, which opens Kill Workspace on a shared
/// Workspace rather than Close Pane.
#[gpui::test]
fn shared_command_w_should_open_kill_workspace(cx: &mut TestAppContext) {
    let (harness, cx) = shared_close_harness(cx);

    cx.simulate_keystrokes("cmd-w");
    redraw(cx);

    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_some()
    );
    assert!(matches!(
        harness.manager.read_with(cx, |manager, _| manager
            .close_confirmation
            .pending()
            .map(|pending| pending.target)),
        Some(CloseTarget::Workspace(_))
    ));
    click("modal-action-shared-close-cancel", cx);
    redraw(cx);
    assert!(harness.closes().is_empty(), "Cancel ends nothing");
    assert_eq!(harness.shared_rows(cx), 1);
}

/// Closing a Tab ends Panes another client controls without taking them over first.
#[gpui::test]
fn shared_tab_close_should_end_panes_another_client_controls_without_taking_over(
    cx: &mut TestAppContext,
) {
    use crate::terminal::server_session::DevelopmentPaneStatus;
    use spaceterm_server::hierarchy::CloseTarget as ServerCloseTarget;

    let (harness, cx) = shared_close_harness(cx);
    harness
        .panes
        .pane(SHARED_CLOSE_ROOT_PANE)
        .set(DevelopmentPaneStatus::Takeover);
    settle_shared_panes(cx);

    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    click("modal-action-shared-close-confirm", cx);
    redraw(cx);

    assert_eq!(
        harness.closes(),
        vec![shared_close_mutation(
            ServerCloseTarget::Tab {
                workspace: SHARED_CLOSE_WORKSPACE,
                tab: SHARED_CLOSE_ROOT_TAB,
            },
            &[
                (SHARED_CLOSE_ROOT_PANE, Some([31; 32])),
                (SHARED_CLOSE_SPLIT_PANE, Some([32; 32])),
            ],
            SharedCloseRemoves::Tab,
        )]
    );
    assert_eq!(
        harness.panes.pane(SHARED_CLOSE_ROOT_PANE).take_overs(),
        0,
        "closing needs no control of the Pane"
    );
}

/// Another client's change while the confirmation is open withdraws it, ends nothing, and Review
/// opens a fresh confirmation of what the server holds now.
#[gpui::test]
fn shared_close_made_stale_by_another_client_should_end_nothing_until_reviewed_again(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::CloseTarget as ServerCloseTarget;

    let (harness, cx) = shared_close_harness(cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_some()
    );

    // Another client splits the Tab while this confirmation is open.
    let grown = [
        (SHARED_CLOSE_ROOT_PANE, Some([31; 32])),
        (SHARED_CLOSE_SPLIT_PANE, Some([32; 32])),
        (SHARED_CLOSE_NEW_PANE, None),
    ];
    harness.publish(
        shared_close_inventory(
            2,
            &[
                (SHARED_CLOSE_ROOT_TAB, &grown),
                (SHARED_CLOSE_SECOND_TAB, &[(SHARED_CLOSE_SECOND_PANE, None)]),
            ],
        ),
        cx,
    );

    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none(),
        "the stale confirmation is withdrawn"
    );
    assert!(
        cx.debug_bounds("modal-action-shared-close-review")
            .is_some()
    );
    assert!(harness.closes().is_empty(), "a stale approval ends nothing");

    click("modal-action-shared-close-review", cx);
    redraw(cx);
    click("modal-action-shared-close-confirm", cx);
    redraw(cx);
    assert_eq!(
        harness.closes(),
        vec![shared_close_mutation(
            ServerCloseTarget::Tab {
                workspace: SHARED_CLOSE_WORKSPACE,
                tab: SHARED_CLOSE_ROOT_TAB,
            },
            &grown,
            SharedCloseRemoves::Tab,
        )],
        "the fresh review approves the Pane the other client added"
    );
}

/// Another client closes the sibling Tab while Close Tab is open. The Tab's Panes and Sessions
/// stay the same, but closing it now removes the Workspace, so the confirmation is withdrawn and
/// only a fresh review that says so can approve it.
#[gpui::test]
fn shared_tab_close_made_final_by_a_sibling_close_should_end_nothing_until_reviewed_again(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::CloseTarget as ServerCloseTarget;

    let (harness, cx) = shared_close_harness(cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_some()
    );

    let root = [
        (SHARED_CLOSE_ROOT_PANE, Some([31; 32])),
        (SHARED_CLOSE_SPLIT_PANE, Some([32; 32])),
    ];
    harness.publish(
        shared_close_inventory(2, &[(SHARED_CLOSE_ROOT_TAB, &root)]),
        cx,
    );

    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none(),
        "a confirmation that never said the Workspace is removed is withdrawn"
    );
    assert!(harness.closes().is_empty(), "a stale approval ends nothing");

    click("modal-action-shared-close-review", cx);
    redraw(cx);
    click("modal-action-shared-close-confirm", cx);
    redraw(cx);
    assert_eq!(
        harness.closes(),
        vec![shared_close_mutation(
            ServerCloseTarget::Tab {
                workspace: SHARED_CLOSE_WORKSPACE,
                tab: SHARED_CLOSE_ROOT_TAB,
            },
            &root,
            SharedCloseRemoves::Workspace,
        )],
        "the fresh review approves the Workspace's removal"
    );
}

/// The server refuses an approval that went stale before it arrived. Nothing ends, nothing asks
/// again on its own, and Review stays a person's choice.
#[gpui::test]
fn shared_close_stale_answer_should_offer_review_without_retrying(cx: &mut TestAppContext) {
    use spaceterm_server::hierarchy::Outcome;

    let (harness, cx) = shared_close_harness(cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    click("modal-action-shared-close-confirm", cx);
    redraw(cx);
    assert_eq!(harness.closes().len(), 1);

    harness.edits.answer(Outcome::StaleConfirmation);
    harness.sync(cx);

    assert!(
        cx.debug_bounds("modal-action-shared-close-review")
            .is_some()
    );
    assert_eq!(
        harness.closes().len(),
        1,
        "a stale approval is never retried"
    );
    assert_eq!(harness.shared_rows(cx), 1);
    click("modal-action-shared-close-dismiss", cx);
    redraw(cx);
    assert_eq!(harness.closes().len(), 1);
}

/// A Tab another client closes while this confirmation is open is gone, so the confirmation says
/// so instead of asking for anything.
#[gpui::test]
fn shared_close_of_a_target_another_client_closed_should_be_withdrawn(cx: &mut TestAppContext) {
    let (harness, cx) = shared_close_harness(cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);

    harness.publish(
        shared_close_inventory(
            2,
            &[(SHARED_CLOSE_SECOND_TAB, &[(SHARED_CLOSE_SECOND_PANE, None)])],
        ),
        cx,
    );

    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );
    assert!(
        cx.debug_bounds("modal-action-shared-close-review")
            .is_none()
    );
    assert!(
        cx.debug_bounds("modal-action-shared-close-dismiss")
            .is_some()
    );
    assert!(harness.closes().is_empty());
}

/// An offline client offers no shared close and keeps nothing for later.
#[gpui::test]
fn shared_close_offline_should_close_nothing_and_say_why(cx: &mut TestAppContext) {
    let (harness, cx) = shared_close_harness(cx);
    harness.edits.set_connected(false);
    harness.sync(cx);

    cx.dispatch_action(crate::ui::ClosePane);
    redraw(cx);

    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );
    assert!(
        cx.debug_bounds("modal-action-shared-workspace-unavailable-ok")
            .is_some()
    );
    harness.edits.set_connected(true);
    harness.sync(cx);
    assert!(
        harness.closes().is_empty(),
        "nothing was queued while offline"
    );
}

/// Close Pane on a split Pane closes that Pane only, after confirmation.
#[gpui::test]
fn shared_pane_close_should_ask_the_server_for_that_pane_only(cx: &mut TestAppContext) {
    use spaceterm_server::hierarchy::CloseTarget as ServerCloseTarget;

    let (harness, cx) = shared_close_harness(cx);
    cx.dispatch_action(crate::ui::ClosePane);
    redraw(cx);
    click("modal-action-shared-close-confirm", cx);
    redraw(cx);

    assert_eq!(
        harness.closes(),
        vec![shared_close_mutation(
            ServerCloseTarget::Pane {
                workspace: SHARED_CLOSE_WORKSPACE,
                tab: SHARED_CLOSE_ROOT_TAB,
                pane: SHARED_CLOSE_ROOT_PANE,
            },
            &[(SHARED_CLOSE_ROOT_PANE, Some([31; 32]))],
            SharedCloseRemoves::Pane,
        )]
    );
}

/// Close in a retained Pane's caption reviews the close without asking the server for that Pane,
/// so neither Cancel nor the approval starts its shell first. The Pane still takes the keyboard,
/// and Cancel returns it there.
#[gpui::test]
fn shared_caption_close_should_confirm_a_retained_pane_without_starting_it(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::CloseTarget as ServerCloseTarget;

    let (harness, cx) = shared_close_harness(cx);
    // After a server restart the split Pane is retained without a shell.
    let retained = [
        (SHARED_CLOSE_ROOT_PANE, Some([31; 32])),
        (SHARED_CLOSE_SPLIT_PANE, None),
    ];
    harness.publish(
        shared_close_inventory(
            2,
            &[
                (SHARED_CLOSE_ROOT_TAB, &retained),
                (SHARED_CLOSE_SECOND_TAB, &[(SHARED_CLOSE_SECOND_PANE, None)]),
            ],
        ),
        cx,
    );
    let split = harness.panes.pane(SHARED_CLOSE_SPLIT_PANE);
    assert_eq!(split.selections(), 0);
    let close_split = |cx: &mut VisualTestContext| {
        // The unfocused Pane reveals its controls under the pointer before they take a click.
        let pane = cx.debug_bounds("pane-surface-2").unwrap();
        cx.simulate_mouse_move(pane.center(), None, Modifiers::none());
        crate::ui::settle_hover(cx);
        click("pane-close-2", cx);
        redraw(cx);
    };

    close_split(cx);
    assert!(
        cx.debug_bounds("modal-action-shared-close-cancel-keyboard-focus")
            .is_some()
    );
    assert_eq!(
        split.selections(),
        0,
        "reviewing a close must not start the retained shell"
    );

    click("modal-action-shared-close-cancel", cx);
    redraw(cx);
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );
    assert!(harness.closes().is_empty(), "Cancel ends nothing");
    assert_eq!(split.selections(), 0, "Cancel leaves no shell started");
    assert!(
        shared_close_terminal_is_focused(&harness, cx),
        "the keyboard returns to the Pane"
    );

    close_split(cx);
    click("modal-action-shared-close-confirm", cx);
    redraw(cx);
    assert_eq!(
        harness.closes(),
        vec![shared_close_mutation(
            ServerCloseTarget::Pane {
                workspace: SHARED_CLOSE_WORKSPACE,
                tab: SHARED_CLOSE_ROOT_TAB,
                pane: SHARED_CLOSE_SPLIT_PANE,
            },
            &[(SHARED_CLOSE_SPLIT_PANE, None)],
            SharedCloseRemoves::Pane,
        )],
        "the approval ends the Pane as it was reviewed"
    );
    assert_eq!(split.selections(), 0);
}

fn shared_close_terminal_is_focused(
    harness: &SharedCloseHarness,
    cx: &mut VisualTestContext,
) -> bool {
    cx.update(|window, cx| {
        harness
            .manager
            .read(cx)
            .workspaces
            .active()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    })
}

fn shared_close_counts(harness: &SharedCloseHarness, cx: &mut VisualTestContext) -> (usize, usize) {
    harness.manager.read_with(cx, |manager, cx| {
        manager
            .workspaces
            .active()
            .payload()
            .read(cx)
            .aggregate_counts(cx)
    })
}

/// The Close Pane Shortcut opens the shared confirmation on Cancel, and Escape cancels it,
/// ending nothing and returning the keyboard to the Pane.
#[gpui::test]
fn shared_close_pane_shortcut_should_confirm_and_escape_should_cancel(cx: &mut TestAppContext) {
    let (harness, cx) = shared_close_harness(cx);
    assert!(shared_close_terminal_is_focused(&harness, cx));

    cx.simulate_keystrokes("cmd-alt-w");
    redraw(cx);
    assert!(
        cx.debug_bounds("modal-action-shared-close-cancel-keyboard-focus")
            .is_some()
    );

    cx.simulate_keystrokes("escape");
    redraw(cx);
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );
    assert!(harness.closes().is_empty(), "Escape ends nothing");
    assert_eq!(shared_close_counts(&harness, cx), (2, 3));
    assert!(
        shared_close_terminal_is_focused(&harness, cx),
        "the keyboard returns to the Pane"
    );
}

/// Closing a Tab changes this window only when the server publishes it. The surviving Tab takes
/// the keyboard, and closing the final Tab removes the Workspace once the server removes it.
#[gpui::test]
fn shared_tab_closes_should_reconcile_through_publication_down_to_the_final_tab(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::{CloseTarget as ServerCloseTarget, Inventory, Outcome};

    let (harness, cx) = shared_close_harness(cx);
    harness.panes.pane(SHARED_CLOSE_SECOND_PANE).reconnect();
    settle_shared_panes(cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    click("modal-action-shared-close-confirm", cx);
    redraw(cx);
    assert_eq!(
        shared_close_counts(&harness, cx),
        (2, 3),
        "nothing closes before the server publishes"
    );

    harness.edits.answer(Outcome::Accepted {
        target: SHARED_CLOSE_ROOT_TAB,
    });
    harness.publish(
        shared_close_inventory(
            2,
            &[(SHARED_CLOSE_SECOND_TAB, &[(SHARED_CLOSE_SECOND_PANE, None)])],
        ),
        cx,
    );
    assert_eq!(shared_close_counts(&harness, cx), (1, 1));
    assert_eq!(harness.shared_rows(cx), 1);
    redraw(cx);
    assert!(
        shared_close_terminal_is_focused(&harness, cx),
        "the surviving Tab's Pane takes the keyboard after this client's own close"
    );

    // The final Tab's close is its Workspace's removal.
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    click("modal-action-shared-close-confirm", cx);
    redraw(cx);
    assert_eq!(
        harness.closes().last(),
        Some(&shared_close_mutation(
            ServerCloseTarget::Tab {
                workspace: SHARED_CLOSE_WORKSPACE,
                tab: SHARED_CLOSE_SECOND_TAB,
            },
            &[(SHARED_CLOSE_SECOND_PANE, None)],
            SharedCloseRemoves::Workspace,
        ))
    );
    harness.edits.answer(Outcome::Accepted {
        target: SHARED_CLOSE_SECOND_TAB,
    });
    harness.publish(
        Inventory {
            repositories: None,
            revision: 3,
            workspaces: Vec::new(),
            close: None,
        },
        cx,
    );
    assert_eq!(harness.shared_rows(cx), 0);
}

fn shared_close_rename_is_focused(
    harness: &SharedCloseHarness,
    cx: &mut VisualTestContext,
) -> bool {
    cx.update(|window, cx| {
        harness
            .manager
            .read(cx)
            .sidebar
            .read(cx)
            .rename_is_focused(window)
    })
}

/// Another client's closes are membership here. A person typing in the sidebar keeps typing there
/// while the server removes a Pane, a Tab, and finally the Workspace they are not typing in.
#[gpui::test]
fn another_clients_removals_should_leave_the_keyboard_where_a_person_is_typing(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::Inventory;

    let (harness, cx) = shared_close_harness(cx);
    right_click("workspace-row-1-inactive", cx);
    click("workspace-menu-row-rename", cx);
    assert!(shared_close_rename_is_focused(&harness, cx));

    harness.publish(
        shared_close_inventory(
            2,
            &[
                (SHARED_CLOSE_ROOT_TAB, &[(SHARED_CLOSE_ROOT_PANE, None)]),
                (SHARED_CLOSE_SECOND_TAB, &[(SHARED_CLOSE_SECOND_PANE, None)]),
            ],
        ),
        cx,
    );
    assert_eq!(shared_close_counts(&harness, cx), (2, 2));
    assert!(
        shared_close_rename_is_focused(&harness, cx),
        "another client's Pane close must not pull the keyboard into the remaining Pane"
    );

    harness.publish(
        shared_close_inventory(
            3,
            &[(SHARED_CLOSE_SECOND_TAB, &[(SHARED_CLOSE_SECOND_PANE, None)])],
        ),
        cx,
    );
    assert_eq!(shared_close_counts(&harness, cx), (1, 1));
    assert!(
        shared_close_rename_is_focused(&harness, cx),
        "another client's Tab close must not pull the keyboard into the Tab that remains"
    );

    harness.publish(
        Inventory {
            repositories: None,
            revision: 4,
            workspaces: Vec::new(),
            close: None,
        },
        cx,
    );
    assert_eq!(harness.shared_rows(cx), 0);
    assert!(
        shared_close_rename_is_focused(&harness, cx),
        "another client's Kill Workspace must not end a rename in this window's own Workspace"
    );
    assert_eq!(
        harness.panes.pane(SHARED_CLOSE_SECOND_PANE).selections(),
        0,
        "nobody here selected the Pane that remained, so the server is asked for nothing"
    );
}

/// The keyboard was in what another client removed, so it rests in what remains, the way a
/// split's exited shell hands it on, without asking the server for that Pane.
#[gpui::test]
fn another_clients_removal_of_where_the_keyboard_rests_should_hand_it_to_what_remains(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::Inventory;

    let (harness, cx) = shared_close_harness(cx);
    harness.panes.pane(SHARED_CLOSE_SPLIT_PANE).reconnect();
    harness.panes.pane(SHARED_CLOSE_SECOND_PANE).reconnect();
    settle_shared_panes(cx);
    click("pane-surface-2", cx);
    assert!(shared_close_terminal_is_focused(&harness, cx));
    let root_selections = harness.panes.pane(SHARED_CLOSE_ROOT_PANE).selections();

    // Another client closes the split Pane the keyboard is in.
    harness.publish(
        shared_close_inventory(
            2,
            &[
                (SHARED_CLOSE_ROOT_TAB, &[(SHARED_CLOSE_ROOT_PANE, None)]),
                (SHARED_CLOSE_SECOND_TAB, &[(SHARED_CLOSE_SECOND_PANE, None)]),
            ],
        ),
        cx,
    );
    assert!(
        shared_close_terminal_is_focused(&harness, cx),
        "the keyboard should rest in the Pane that remains rather than nowhere"
    );
    assert_eq!(
        harness.panes.pane(SHARED_CLOSE_ROOT_PANE).selections(),
        root_selections,
        "resting the keyboard is not a Pane selection"
    );

    // Another client closes the Active Tab the keyboard is in.
    harness.publish(
        shared_close_inventory(
            3,
            &[(SHARED_CLOSE_SECOND_TAB, &[(SHARED_CLOSE_SECOND_PANE, None)])],
        ),
        cx,
    );
    assert!(
        shared_close_terminal_is_focused(&harness, cx),
        "the keyboard should rest in the Tab that becomes active"
    );
    assert_eq!(
        harness.panes.pane(SHARED_CLOSE_SECOND_PANE).selections(),
        0,
        "the Tab that becomes active asks the server for nothing"
    );

    // Another client kills the Workspace the keyboard is in.
    harness.publish(
        Inventory {
            repositories: None,
            revision: 4,
            workspaces: Vec::new(),
            close: None,
        },
        cx,
    );
    assert_eq!(harness.shared_rows(cx), 0);
    assert!(
        shared_close_terminal_is_focused(&harness, cx),
        "the keyboard should rest in the Workspace that becomes active"
    );
}

const ATTACHED_BUILD: [u8; 32] = [21; 32];
const ATTACHED_BUILD_TAB: [u8; 32] = [22; 32];
const ATTACHED_BUILD_PANE: [u8; 32] = [23; 32];
const ATTACHED_DOCS: [u8; 32] = [24; 32];
const ATTACHED_DOCS_TAB: [u8; 32] = [25; 32];
const ATTACHED_DOCS_PANE: [u8; 32] = [26; 32];
/// The Workspace Switcher result of the detached Build Workspace, named by its leading bytes.
const DETACHED_BUILD_RESULT: &str = "workspace-switcher-detached-15151515";

/// Two shared Workspaces, Build and Docs, beside this window's own Workspace. A new client
/// attaches Build, and the person attaches Docs, so the sidebar lists the desktop Workspace
/// first, then Build as Workspace 2 and Docs as Workspace 3.
struct AttachmentHarness {
    manager: Entity<WorkspaceManager>,
    edits: Rc<crate::ui::shared_workspaces::testing::ScriptedEdits>,
    panes: Rc<crate::ui::shared_workspaces::testing::ScriptedPanes>,
}

fn attachment_inventory() -> spaceterm_server::hierarchy::Inventory {
    use spaceterm_server::hierarchy::{Axis, Inventory, Pane, Tab, Workspace};

    let workspace = |id, name: &str, tab, pane| Workspace {
        id,
        name: name.to_owned(),
        name_version: 0,
        pin_version: 0,
        pinned_directory: None,
        root_tab: tab,
        tabs: vec![Tab {
            worktree: None,
            worktree_membership_fixed: false,
            worktree_repository: None,
            id: tab,
            root_pane: pane,
            panes: vec![Pane {
                id: pane,
                source: None,
                axis: Axis::Right,
                starting_directory: PathBuf::from("/server/work"),
                confirmed_directory: None,
                creator: None,
            }],
        }],
    };
    Inventory {
        repositories: None,
        revision: 1,
        close: None,
        workspaces: vec![
            workspace(
                ATTACHED_BUILD,
                "Build",
                ATTACHED_BUILD_TAB,
                ATTACHED_BUILD_PANE,
            ),
            workspace(ATTACHED_DOCS, "Docs", ATTACHED_DOCS_TAB, ATTACHED_DOCS_PANE),
        ],
    }
}

fn attachment_harness(cx: &mut TestAppContext) -> (AttachmentHarness, &mut VisualTestContext) {
    use crate::ui::shared_workspaces::testing::{ScriptedEdits, ScriptedPanes};
    use crate::ui::shared_workspaces::{SharedEditSource, SharedPaneSessions};

    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let edits = Rc::new(ScriptedEdits::new(Some(attachment_inventory())));
    let panes = ScriptedPanes::new(WorkspaceTerminalSessionFactory::new_local(
        Rc::new(TestTerminalSessionFactory::new(
            TestTerminalSessionRecords::default(),
        )),
        crate::terminal::testing::test_local_directory(std::env::temp_dir()),
    ));
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.present_shared_workspaces(
                SharedEditSource::new(Rc::clone(&edits) as Rc<_>),
                SharedPaneSessions::with_opener(
                    Rc::clone(&panes) as Rc<_>,
                    &PaneConstruction::testing(),
                ),
                window,
                cx,
            );
        });
    });
    let harness = AttachmentHarness {
        manager,
        edits,
        panes,
    };
    harness.sync(cx);
    assert_eq!(
        harness.presented(cx),
        vec![1, 2],
        "a new client attaches only the server's first Workspace"
    );
    // The person attaches the other Workspace, then returns to the one they started in.
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.attach_detached_workspace(ServerKey::LOCAL, ATTACHED_DOCS, window, cx);
            manager.activate_workspace(WorkspaceId::new(1), window, cx);
        });
    });
    settle_shared_panes(cx);
    assert_eq!(harness.presented(cx), vec![1, 2, 3]);
    (harness, cx)
}

impl AttachmentHarness {
    fn sync(&self, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            self.manager.update(cx, |manager, cx| {
                manager.sync_shared_workspaces(ServerKey::LOCAL, window, cx)
            });
        });
        settle_shared_panes(cx);
    }

    /// The Workspaces this window presents, in sidebar order.
    fn presented(&self, cx: &mut VisualTestContext) -> Vec<u64> {
        workspace_order(&self.manager, cx)
    }

    fn active(&self, cx: &mut VisualTestContext) -> Option<WorkspaceId> {
        self.manager
            .read_with(cx, |manager, _| manager.workspaces.active_workspace_id())
    }

    /// Opens a row's context menu and chooses Detach Workspace.
    fn detach(&self, workspace: u64, cx: &mut VisualTestContext) {
        right_click(workspace_row(&self.manager, workspace, cx), cx);
        click("workspace-menu-row-detach", cx);
        settle_shared_panes(cx);
    }

    /// Kills the desktop Workspace through its menu and confirms, leaving the shared ones.
    fn close_desktop_workspace(&self, cx: &mut VisualTestContext) {
        right_click(workspace_row(&self.manager, 1, cx), cx);
        click("workspace-menu-row-close", cx);
        if cx
            .debug_bounds("modal-action-close-confirmation-confirm")
            .is_some()
        {
            click("modal-action-close-confirmation-confirm", cx);
        }
        settle_shared_panes(cx);
    }
}

/// The row selector a Workspace's sidebar row carries in its current state.
fn workspace_row(
    manager: &Entity<WorkspaceManager>,
    workspace: u64,
    cx: &mut VisualTestContext,
) -> &'static str {
    let active = manager.read_with(cx, |manager, _| {
        manager.workspaces.active_workspace_id() == Some(WorkspaceId::new(workspace))
    });
    let state = if active { "active" } else { "inactive" };
    format!("workspace-row-{workspace}-{state}").leak()
}

/// The accessible names of the open menu's items, in order.
fn menu_item_labels(cx: &mut VisualTestContext) -> Vec<String> {
    spaceterm_ui::a11y_testing::A11yTree::read(cx)
        .with_role("MenuItem")
        .iter()
        .map(|item| {
            item["aria"]["label"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

#[gpui::test]
fn detach_workspace_should_stop_presenting_it_here_and_offer_it_detached_in_the_switcher(
    cx: &mut TestAppContext,
) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (harness, cx) = attachment_harness(cx);

    harness.detach(2, cx);
    let presented = harness.presented(cx);
    let confirmation = cx
        .debug_bounds("modal-action-close-confirmation-confirm")
        .is_some();
    open_workspace_switcher(cx);
    let tree = A11yTree::read(cx);
    let detached = tree
        .with_role("ListBoxOption")
        .into_iter()
        .find(|option| option["aria"]["label"] == "Build")
        .cloned();

    assert_eq!(presented, vec![1, 3], "only Build stops being presented");
    assert!(
        !confirmation,
        "Detach Workspace ends nothing, so it asks for no confirmation"
    );
    assert_eq!(
        harness.edits.submitted(),
        Vec::new(),
        "detaching asks the server for nothing: no Kill and no replacement Workspace"
    );
    assert_eq!(
        harness.active(cx),
        Some(WorkspaceId::new(1)),
        "detaching an inactive Workspace leaves the Active Workspace where it was"
    );
    let detached = detached.expect("the switcher lists the detached Workspace");
    assert_eq!(detached["aria"]["description"], "Detached");
    assert!(
        detached["aria"]["keyboard_shortcut"].is_null(),
        "a detached Workspace takes no activation Shortcut"
    );
}

#[gpui::test]
fn choosing_a_detached_workspace_should_attach_and_activate_it_without_taking_over(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = attachment_harness(cx);
    harness.detach(2, cx);

    open_workspace_switcher(cx);
    click(DETACHED_BUILD_RESULT, cx);
    harness.sync(cx);
    let reattached = harness.manager.read_with(cx, |manager, _| {
        manager
            .shared
            .local()
            .and_then(|shared| shared.local(ATTACHED_BUILD))
    });
    let switcher_open = cx.update(|window, cx| window_combo_box_is_open(window, cx));

    let reattached = reattached.expect("choosing the detached Workspace attaches it");
    assert_eq!(
        harness.active(cx),
        Some(reattached),
        "choosing the detached Workspace also activates it"
    );
    assert!(!switcher_open);
    assert_eq!(
        harness.presented(cx).len(),
        3,
        "attaching presents the Workspace again beside the others"
    );
    assert_eq!(
        harness.panes.pane(ATTACHED_BUILD_PANE).take_overs(),
        0,
        "attaching reinstates no control by taking the Pane over"
    );
    assert_eq!(harness.edits.submitted(), Vec::new());
}

/// The Workspace that takes the place of one the person closed gets the keyboard, but nobody chose
/// it, so its Focused Pane is not selected and a restored Pane starts no shell.
#[gpui::test]
fn killing_the_active_workspace_should_open_its_neighbor_without_selecting_its_pane(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = attachment_harness(cx);
    let selections = || {
        [ATTACHED_BUILD_PANE, ATTACHED_DOCS_PANE].map(|pane| harness.panes.pane(pane).selections())
    };
    let before = selections();

    harness.close_desktop_workspace(cx);

    assert_eq!(harness.presented(cx), vec![2, 3]);
    assert_eq!(harness.active(cx), Some(WorkspaceId::new(2)));
    assert_eq!(selections(), before);
    assert!(
        active_terminal_has_input_focus(&harness.manager, cx),
        "the keyboard moves into the Workspace that took the closed one's place"
    );
}

/// A neighbor whose Pane waits for first use takes the keyboard on the screen in the Pane's
/// place, so Tab reaches Start New Shell and nothing starts until the person presses it.
#[gpui::test]
fn a_dormant_neighbor_should_take_the_keyboard_on_its_screen(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (harness, cx) = attachment_harness(cx);
    for pane in [ATTACHED_BUILD_PANE, ATTACHED_DOCS_PANE] {
        harness
            .panes
            .pane(pane)
            .set(crate::terminal::server_session::DevelopmentPaneStatus::Restored);
    }
    settle_shared_panes(cx);
    let selections = harness.panes.pane(ATTACHED_BUILD_PANE).selections();

    harness.close_desktop_workspace(cx);
    let landing = A11yTree::read(cx)
        .focused()
        .map(|node| node["aria"]["label"].clone());
    assert_eq!(landing, Some(serde_json::json!("Server Restarted")));
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    let focused = A11yTree::read(cx)
        .focused()
        .map(|node| node["aria"]["label"].clone());

    assert_eq!(harness.active(cx), Some(WorkspaceId::new(2)));
    assert_eq!(focused, Some(serde_json::json!("Start New Shell")));
    assert_eq!(
        harness.panes.pane(ATTACHED_BUILD_PANE).selections(),
        selections
    );
    assert_eq!(harness.panes.pane(ATTACHED_BUILD_PANE).restored_starts(), 0);
}

/// Detaching the Active Workspace opens its neighbor the same way: with the keyboard, without
/// selecting the neighbor's Focused Pane.
#[gpui::test]
fn detaching_the_active_workspace_should_open_its_neighbor_without_selecting_its_pane(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = attachment_harness(cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.activate_workspace(WorkspaceId::new(3), window, cx);
        });
    });
    settle_shared_panes(cx);
    let before = harness.panes.pane(ATTACHED_BUILD_PANE).selections();

    harness.detach(3, cx);

    assert_eq!(harness.presented(cx), vec![1, 2]);
    assert_eq!(harness.active(cx), Some(WorkspaceId::new(2)));
    assert_eq!(harness.panes.pane(ATTACHED_BUILD_PANE).selections(), before);
    assert!(active_terminal_has_input_focus(&harness.manager, cx));
}

#[gpui::test]
fn detaching_the_final_workspace_should_leave_an_empty_view_without_a_replacement(
    cx: &mut TestAppContext,
) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (harness, cx) = attachment_harness(cx);
    harness.close_desktop_workspace(cx);
    harness.detach(2, cx);
    harness.detach(3, cx);

    let count = harness
        .manager
        .read_with(cx, |manager, _| manager.workspace_count());
    let tree = A11yTree::read(cx);
    let empty = tree.node(super::empty::EMPTY_TITLE).clone();
    let focused = tree.focused().map(|node| node["aria"]["label"].clone());
    let action = tree.node(super::empty::EMPTY_ACTION).clone();

    assert_eq!(count, 0, "nothing replaces the detached final Workspace");
    assert_eq!(
        harness.edits.submitted(),
        Vec::new(),
        "the empty view asks the server to create nothing"
    );
    assert_eq!(empty["aria"]["role"], "Group");
    assert_eq!(empty["aria"]["description"], super::empty::EMPTY_DETAIL);
    assert_eq!(
        focused,
        Some(serde_json::json!(super::empty::EMPTY_TITLE)),
        "the keyboard rests on the empty view, which names itself and explains what to do"
    );
    assert_eq!(action["aria"]["role"], "Button");
    assert!(cx.debug_bounds("tab-bar").is_none(), "no Tab strip remains");
}

#[gpui::test]
fn the_empty_view_action_should_open_the_switcher_with_the_detached_workspaces(
    cx: &mut TestAppContext,
) {
    use crate::ui::workspace_creation::WorkspaceCreation;
    use spaceterm_ui::a11y_testing::A11yTree;

    let (harness, cx) = attachment_harness(cx);
    harness.close_desktop_workspace(cx);
    harness.detach(2, cx);
    harness.detach(3, cx);

    // Plain Tab moves from the empty view to its action, and Return activates it.
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    let focused = A11yTree::read(cx)
        .focused()
        .map(|node| node["aria"]["label"].clone());
    press_return(cx);
    let switcher_open = cx.update(|window, cx| window_combo_box_is_open(window, cx));
    // The switcher's creation commands share its option role, and how many of them render
    // depends on the platform's viewport, so only the remaining options are Workspaces.
    let creations = WorkspaceCreation::ALL
        .into_iter()
        .map(WorkspaceCreation::label)
        .collect::<Vec<_>>();
    let workspaces: Vec<(String, String)> = A11yTree::read(cx)
        .with_role("ListBoxOption")
        .iter()
        .map(|option| {
            let text = |key: &str| option["aria"][key].as_str().unwrap_or_default().to_owned();
            (text("label"), text("description"))
        })
        .filter(|(label, _)| !creations.contains(&label.as_str()))
        .collect();
    click(DETACHED_BUILD_RESULT, cx);
    harness.sync(cx);

    assert_eq!(focused, Some(serde_json::json!(super::empty::EMPTY_ACTION)));
    assert!(switcher_open, "the action opens the Workspace Switcher");
    assert_eq!(
        workspaces,
        [("Build", "Detached"), ("Docs", "Detached")]
            .map(|(label, description)| (label.to_owned(), description.to_owned())),
        "the switcher offers the two detached Workspaces and no other Workspace"
    );
    assert_eq!(
        harness.presented(cx).len(),
        1,
        "choosing a detached Workspace leaves the empty view"
    );
    assert!(cx.debug_bounds("empty-workspace").is_none());
    assert!(
        harness.active(cx).is_some(),
        "the attached Workspace becomes the Active Workspace"
    );
}

#[gpui::test]
fn detach_workspace_should_be_named_apart_from_kill_and_carry_no_shortcut(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (harness, cx) = attachment_harness(cx);

    right_click(workspace_row(&harness.manager, 2, cx), cx);
    let shared_menu = menu_item_labels(cx);
    let detach = A11yTree::read(cx).node("Detach Workspace").clone();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    right_click(workspace_row(&harness.manager, 1, cx), cx);
    let desktop_menu = menu_item_labels(cx);

    let position = |label: &str| shared_menu.iter().position(|item| item == label);
    assert!(
        position("Detach Workspace") < position("Kill Workspace"),
        "Detach Workspace sits apart from Kill Workspace: {shared_menu:?}"
    );
    assert!(
        detach["aria"]["keyboard_shortcut"].is_null(),
        "Detach Workspace has no default Shortcut"
    );
    assert_ne!(detach["aria"]["disabled"], true);
    assert!(
        !desktop_menu.iter().any(|item| item.starts_with("Detach")),
        "a Workspace this window owns has no attachment to detach: {desktop_menu:?}"
    );
}

#[gpui::test]
fn offline_detach_should_still_stop_presenting_while_shared_edits_stay_unavailable(
    cx: &mut TestAppContext,
) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (harness, cx) = attachment_harness(cx);
    harness.edits.set_connected(false);
    harness.sync(cx);

    right_click(workspace_row(&harness.manager, 2, cx), cx);
    let tree = A11yTree::read(cx);
    let detach_disabled = tree.node("Detach Workspace")["aria"]["disabled"].clone();
    let kill_disabled = tree.node("Kill Workspace")["aria"]["disabled"].clone();
    click("workspace-menu-row-detach", cx);
    settle_shared_panes(cx);
    let after_detach = harness.presented(cx);
    // Navigation through what this window still presents keeps working offline.
    click(workspace_row(&harness.manager, 3, cx), cx);
    let navigated = harness.active(cx);
    open_workspace_switcher(cx);
    let offered_offline = cx.debug_bounds(DETACHED_BUILD_RESULT).is_some();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    harness.edits.set_connected(true);
    harness.sync(cx);
    open_workspace_switcher(cx);
    let offered_online = cx.debug_bounds(DETACHED_BUILD_RESULT).is_some();

    assert!(
        detach_disabled.is_null() || detach_disabled == false,
        "Detach Workspace needs no server"
    );
    assert_eq!(kill_disabled, true, "Kill Workspace waits for the server");
    assert_eq!(after_detach, vec![1, 3]);
    assert_eq!(navigated, Some(WorkspaceId::new(3)));
    assert!(
        !offered_offline,
        "an unreachable server's detached Workspaces are not offered"
    );
    assert!(
        offered_online,
        "the detached Workspace is offered again once the server is reachable"
    );
    assert_eq!(harness.edits.submitted(), Vec::new());
}

#[gpui::test]
fn the_offline_empty_view_should_say_detached_workspaces_return_on_reconnect(
    cx: &mut TestAppContext,
) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (harness, cx) = attachment_harness(cx);
    harness.close_desktop_workspace(cx);
    harness.edits.set_connected(false);
    harness.sync(cx);
    harness.detach(2, cx);
    harness.detach(3, cx);

    let tree = A11yTree::read(cx);
    let description = tree.node(super::empty::EMPTY_TITLE)["aria"]["description"].clone();
    let count = harness
        .manager
        .read_with(cx, |manager, _| manager.workspace_count());

    assert_eq!(count, 0);
    assert_eq!(description, super::empty::EMPTY_OFFLINE_DETAIL);
}

/// One shared Workspace with a Tab in each of two Worktrees, its Git facts published by the
/// server, beside the local Workspace the window keeps active.
mod shared_repositories {
    use std::path::PathBuf;
    use std::rc::Rc;

    use gpui::{Entity, TestAppContext, VisualTestContext};
    use spaceterm_server::hierarchy::{Axis, Inventory, Pane, Tab, Workspace};
    use spaceterm_server::repository::{
        ChangeSummary, ChangeTotal, DirectoryFacts, GitToolStatus, Repository, RepositoryHead,
        RepositoryReadError, Snapshot, ToolVersion, WorktreeHead, WorktreeInventory,
        WorktreeRecord,
    };

    use super::*;
    use crate::ui::shared_workspaces::testing::{ScriptedEdits, ScriptedPanes};
    use crate::ui::shared_workspaces::{SharedEditSource, SharedPaneSessions};
    use crate::ui::workspace_sidebar::WorktreeRemoval;

    const WORKSPACE: [u8; 32] = [21; 32];
    const ROOT_TAB: [u8; 32] = [22; 32];
    const ROOT_PANE: [u8; 32] = [23; 32];
    const FEATURE_TAB: [u8; 32] = [24; 32];
    const FEATURE_PANE: [u8; 32] = [25; 32];
    const MAIN_ROOT: &str = "/srv/app";
    const FEATURE_ROOT: &str = "/srv/app-feature";
    const EXTRA_ROOT: &str = "/srv/app-extra";

    fn pane(id: [u8; 32], directory: &str) -> Pane {
        Pane {
            id,
            source: None,
            axis: Axis::Right,
            starting_directory: PathBuf::from(directory),
            confirmed_directory: Some(PathBuf::from(directory)),
            creator: None,
        }
    }

    fn record(root: &str, branch: &str, missing: bool) -> WorktreeRecord {
        WorktreeRecord {
            root: root.into(),
            head: WorktreeHead::Branch(branch.to_owned()),
            locked: false,
            missing,
        }
    }

    fn facts(directory: &str, branch: &str, listed: &[WorktreeRecord]) -> DirectoryFacts {
        DirectoryFacts {
            directory: directory.into(),
            repository: Some(Repository {
                root: directory.into(),
                head: RepositoryHead::Branch(branch.into()),
                commit: Some("abc1234".into()),
                upstream: None,
                operation: None,
                changes: ChangeSummary {
                    total: ChangeTotal::Exact(1),
                    modified: 1,
                    ..ChangeSummary::default()
                },
                pull_request: None,
            }),
            worktrees: Some(WorktreeInventory {
                main_root: MAIN_ROOT.into(),
                current: listed
                    .iter()
                    .position(|record| record.root == std::path::Path::new(directory)),
                worktrees: listed.to_vec(),
            }),
            failure: None,
        }
    }

    /// The server's inventory at organization revision 1, with Git facts at `revision`.
    fn published(revision: u64, main_branch: &str, feature_listed: bool) -> Inventory {
        let mut listed = vec![record(MAIN_ROOT, main_branch, false)];
        let mut directories = Vec::new();
        if feature_listed {
            listed.push(record(FEATURE_ROOT, "feature", false));
            directories.push(facts(FEATURE_ROOT, "feature", &listed));
        }
        directories.insert(0, facts(MAIN_ROOT, main_branch, &listed));
        Inventory {
            repositories: Some(Snapshot {
                boot: [1; 32],
                revision,
                git: GitToolStatus::Ready(ToolVersion {
                    major: 2,
                    minor: 47,
                    patch: 0,
                }),
                directories,
            }),
            revision: 1,
            close: None,
            workspaces: vec![Workspace {
                id: WORKSPACE,
                name: "Build".to_owned(),
                name_version: 0,
                pin_version: 0,
                pinned_directory: None,
                root_tab: ROOT_TAB,
                tabs: vec![
                    Tab {
                        worktree: Some(MAIN_ROOT.into()),
                        worktree_membership_fixed: false,
                        worktree_repository: Some(MAIN_ROOT.into()),
                        id: ROOT_TAB,
                        root_pane: ROOT_PANE,
                        panes: vec![pane(ROOT_PANE, MAIN_ROOT)],
                    },
                    Tab {
                        worktree: Some(FEATURE_ROOT.into()),
                        worktree_membership_fixed: false,
                        worktree_repository: Some(MAIN_ROOT.into()),
                        id: FEATURE_TAB,
                        root_pane: FEATURE_PANE,
                        panes: vec![pane(FEATURE_PANE, FEATURE_ROOT)],
                    },
                ],
            }],
        }
    }

    /// The server's inventory when the Workspace's Root Tab is in the feature Worktree, with the
    /// Git facts it read at `revision`.
    fn rooted_in_feature(revision: u64, directories: Vec<DirectoryFacts>) -> Inventory {
        let mut inventory = published(revision, "main", true);
        inventory.workspaces[0].root_tab = FEATURE_TAB;
        if let Some(snapshot) = &mut inventory.repositories {
            snapshot.directories = directories;
        }
        inventory
    }

    /// The server's inventory after a restart when the Workspace's only Tab is in the feature
    /// Worktree and its directory is gone. The server reads the Main Worktree the Tab saved, which
    /// lists `main_listing`, and keeps no facts from before the restart.
    fn restarted_in_removed_feature(revision: u64, main_listing: &[WorktreeRecord]) -> Inventory {
        let mut inventory = rooted_in_feature(
            revision,
            vec![
                facts(MAIN_ROOT, "main", main_listing),
                DirectoryFacts {
                    directory: FEATURE_ROOT.into(),
                    repository: None,
                    worktrees: None,
                    failure: Some(RepositoryReadError::Unavailable),
                },
            ],
        );
        inventory.workspaces[0]
            .tabs
            .retain(|tab| tab.id == FEATURE_TAB);
        inventory
    }

    struct Harness {
        manager: Entity<WorkspaceManager>,
        edits: Rc<ScriptedEdits>,
        shared: WorkspaceId,
    }

    impl Harness {
        fn publish(&self, inventory: Inventory, cx: &mut VisualTestContext) {
            self.edits.set_inventory(inventory);
            cx.update(|window, cx| {
                self.manager.update(cx, |manager, cx| {
                    manager.sync_shared_workspaces(ServerKey::LOCAL, window, cx)
                });
            });
            cx.run_until_parked();
        }

        fn row(&self, cx: &mut VisualTestContext) -> WorkspaceRowViewModel {
            cx.update(|_, cx| {
                self.manager
                    .read(cx)
                    .sidebar_rows(cx)
                    .into_iter()
                    .find(|row| row.workspace_id == self.shared)
                    .expect("the shared Workspace has a row")
            })
        }

        /// Each shared Pane's Repository Status caption, as its Pane presents it.
        fn captions(&self, cx: &mut VisualTestContext) -> Vec<Option<String>> {
            cx.update(|_, cx| {
                let manager = self.manager.read(cx);
                let tabs = manager
                    .workspaces
                    .workspace(self.shared)
                    .expect("the shared Workspace")
                    .payload()
                    .read(cx);
                tabs.terminal_panes(cx)
                    .map(|(_, _, pane)| pane.caption().repository.map(|caption| caption.branch))
                    .collect()
            })
        }
    }

    fn harness(cx: &mut TestAppContext, initial: Inventory) -> (Harness, &mut VisualTestContext) {
        let (manager, _records, cx) = workspace_manager(cx);
        let edits = Rc::new(ScriptedEdits::new(Some(initial)));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            Rc::new(
                TestTerminalSessionFactory::new(TestTerminalSessionRecords::default())
                    .server_backed(),
            ),
            crate::terminal::testing::test_local_directory(std::env::temp_dir()),
        );
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                manager.present_shared_workspaces(
                    SharedEditSource::new(Rc::clone(&edits) as Rc<_>),
                    SharedPaneSessions::with_opener(
                        ScriptedPanes::new(session_factory) as Rc<_>,
                        &PaneConstruction::testing(),
                    ),
                    window,
                    cx,
                );
                manager.sync_shared_workspaces(ServerKey::LOCAL, window, cx);
                // The person returns to the desktop's own Workspace, so this one is inactive.
                manager.activate_workspace(WorkspaceId::new(1), window, cx);
            });
        });
        cx.run_until_parked();
        let shared = manager
            .read_with(cx, |manager, _| {
                manager
                    .workspaces
                    .iter()
                    .map(|workspace| workspace.id())
                    .find(|id| manager.is_shared_workspace(*id))
            })
            .expect("the published Workspace joins the sidebar");
        (
            Harness {
                manager,
                edits,
                shared,
            },
            cx,
        )
    }

    /// The server reads Git for a Workspace nobody is looking at, and a new reading reaches its
    /// row and every Pane even though its organization revision never changes.
    #[gpui::test]
    fn an_inactive_shared_workspace_should_present_each_new_git_reading(cx: &mut TestAppContext) {
        let (harness, cx) = harness(cx, published(1, "main", true));
        let first_badge = harness
            .row(cx)
            .repository
            .map(|badge| badge.text().to_owned());
        let first_captions = harness.captions(cx);

        harness.publish(published(2, "next", true), cx);

        let active = harness
            .manager
            .read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
        let next_badge = harness
            .row(cx)
            .repository
            .map(|badge| badge.text().to_owned());
        let next_captions = harness.captions(cx);

        assert_ne!(
            active,
            Some(harness.shared),
            "the local Workspace stays active"
        );
        assert_eq!(first_badge.as_deref(), Some("main"));
        assert_eq!(next_badge.as_deref(), Some("next"));
        assert_eq!(
            first_captions,
            vec![Some("main".to_owned()), Some("feature".to_owned())]
        );
        assert_eq!(
            next_captions,
            vec![Some("next".to_owned()), Some("feature".to_owned())],
            "every Pane presents the server's reading, including one outside the Active Tab"
        );
        assert_eq!(harness.edits.submitted(), Vec::new());
    }

    /// Worktree actions follow the owning server's availability and existing Tab membership.
    #[gpui::test]
    fn shared_worktree_rows_should_follow_server_membership_and_available_actions(
        cx: &mut TestAppContext,
    ) {
        let (harness, cx) = harness(cx, published(1, "main", true));
        let section = harness
            .row(cx)
            .worktrees
            .expect("a repository with two Worktrees discloses them");
        let rows: Vec<_> = section.rows().cloned().collect();
        let creates_worktrees = harness.row(cx).creates_worktrees;

        assert_eq!(
            rows.iter()
                .map(|row| (row.name.to_string(), row.label.to_string(), row.has_tabs))
                .collect::<Vec<_>>(),
            vec![
                ("app".to_owned(), "main".to_owned(), true),
                ("app-feature".to_owned(), "feature".to_owned(), true),
            ],
            "each server Tab sits in the Worktree the server says it belongs to"
        );
        assert_eq!(rows[0].removal, WorktreeRemoval::Main);
        assert_eq!(rows[1].removal, WorktreeRemoval::Server);
        assert!(rows.iter().all(|row| !row.opens_tabs() && row.closes_tabs));
        assert!(!creates_worktrees, "New Worktree stays unavailable");
    }

    /// A Worktree removed outside SpaceTerm keeps its row while the server keeps its Tab there.
    /// When it was the Workspace's own directory, the server's failed read of it keeps its last
    /// facts, and the rows follow what git lists from a surviving Worktree: the removal, later
    /// additions, and a prune. Nothing asks the server to recreate it, and its row opens only the
    /// Tab it has.
    #[gpui::test]
    fn an_externally_removed_shared_worktree_should_stay_missing_with_its_tab(
        cx: &mut TestAppContext,
    ) {
        let both = [
            record(MAIN_ROOT, "main", false),
            record(FEATURE_ROOT, "feature", false),
        ];
        let (harness, cx) = harness(
            cx,
            rooted_in_feature(
                1,
                vec![
                    facts(MAIN_ROOT, "main", &both),
                    facts(FEATURE_ROOT, "feature", &both),
                ],
            ),
        );
        // The server keeps the removed directory's last facts, including its former listing.
        let removed_source = DirectoryFacts {
            failure: Some(RepositoryReadError::Unavailable),
            ..facts(FEATURE_ROOT, "feature", &both)
        };
        let rows = |cx: &mut VisualTestContext| {
            harness
                .row(cx)
                .worktrees
                .expect("the Workspace lists its Worktrees")
                .rows()
                .map(|row| (row.name.to_string(), row.missing, row.has_tabs))
                .collect::<Vec<_>>()
        };
        let feature_row = |cx: &mut VisualTestContext| {
            harness
                .row(cx)
                .worktrees
                .and_then(|section| {
                    section
                        .rows()
                        .find(|row| row.name.as_ref() == "app-feature")
                        .cloned()
                })
                .expect("the removed Worktree keeps its row")
        };

        harness.publish(
            rooted_in_feature(
                2,
                vec![
                    facts(
                        MAIN_ROOT,
                        "main",
                        &[
                            record(MAIN_ROOT, "main", false),
                            record(FEATURE_ROOT, "feature", true),
                        ],
                    ),
                    removed_source.clone(),
                ],
            ),
            cx,
        );
        let after_removal = rows(cx);
        let badge = harness
            .row(cx)
            .repository
            .map(|badge| badge.text().to_owned());
        harness.publish(
            rooted_in_feature(
                3,
                vec![
                    facts(
                        MAIN_ROOT,
                        "main",
                        &[
                            record(MAIN_ROOT, "main", false),
                            record(FEATURE_ROOT, "feature", true),
                            record(EXTRA_ROOT, "extra", false),
                        ],
                    ),
                    removed_source.clone(),
                ],
            ),
            cx,
        );
        let after_addition = rows(cx);
        harness.publish(
            rooted_in_feature(
                4,
                vec![
                    facts(
                        MAIN_ROOT,
                        "main",
                        &[
                            record(MAIN_ROOT, "main", false),
                            record(EXTRA_ROOT, "extra", false),
                        ],
                    ),
                    removed_source,
                ],
            ),
            cx,
        );
        let after_prune = rows(cx);
        let pruned = feature_row(cx);

        assert_eq!(
            after_removal,
            vec![
                ("app".to_owned(), false, true),
                ("app-feature".to_owned(), true, true),
            ],
            "a surviving Worktree's listing marks the Workspace's own Worktree Missing"
        );
        assert_eq!(
            badge.as_deref(),
            Some("feature"),
            "the row keeps the removed directory's last Repository Status"
        );
        assert_eq!(
            after_addition,
            vec![
                ("app".to_owned(), false, true),
                ("app-feature".to_owned(), true, true),
                ("app-extra".to_owned(), false, false),
            ],
            "a Worktree added outside SpaceTerm joins the rows"
        );
        assert_eq!(
            after_prune
                .iter()
                .find(|(name, ..)| name == "app-extra")
                .cloned(),
            Some(("app-extra".to_owned(), false, false))
        );
        assert!(pruned.missing);
        assert!(pruned.has_tabs && pruned.openable() && !pruned.opens_tabs());
        assert_eq!(pruned.removal, WorktreeRemoval::Unlisted);
        assert_eq!(harness.edits.submitted(), Vec::new());
    }

    /// A fresh window after a server restart finds the repository of a Workspace whose only Tab
    /// is in a removed Worktree through the Main Worktree the Tab saved. The window can receive
    /// the restarted server's inventory before the server reads any directory, and git can have
    /// pruned the Worktree before the restart, so the Main Worktree's listing never names it. The
    /// Tab still stays in the saved repository as a Missing Worktree, and the rows follow what git
    /// lists from the Main Worktree: an addition and a prune.
    #[gpui::test]
    fn a_fresh_window_should_find_the_repository_of_a_removed_worktree_after_a_server_restart(
        cx: &mut TestAppContext,
    ) {
        let mut unread = restarted_in_removed_feature(1, &[]);
        if let Some(snapshot) = &mut unread.repositories {
            snapshot.directories.clear();
        }
        let (harness, cx) = harness(cx, unread);
        let rows = |cx: &mut VisualTestContext| {
            let section = harness
                .row(cx)
                .worktrees
                .expect("the Workspace lists its Worktrees");
            let former = section
                .groups
                .iter()
                .filter_map(|group| group.former_repository.as_ref().map(ToString::to_string))
                .collect::<Vec<_>>();
            let rows = section
                .rows()
                .map(|row| (row.name.to_string(), row.missing, row.has_tabs))
                .collect::<Vec<_>>();
            (
                section.repository.map(|root| root.to_string()),
                former,
                rows,
            )
        };

        let before_reads = rows(cx);
        harness.publish(
            restarted_in_removed_feature(
                2,
                &[
                    record(MAIN_ROOT, "main", false),
                    record(EXTRA_ROOT, "extra", false),
                ],
            ),
            cx,
        );
        let after_restart = rows(cx);
        harness.publish(
            restarted_in_removed_feature(3, &[record(MAIN_ROOT, "main", false)]),
            cx,
        );
        let after_prune = rows(cx);
        let pruned = harness
            .row(cx)
            .worktrees
            .and_then(|section| {
                section
                    .rows()
                    .find(|row| row.name.as_ref() == "app-feature")
                    .cloned()
            })
            .expect("the removed Worktree keeps its row");

        assert_eq!(
            before_reads,
            (
                None,
                vec!["app".to_owned()],
                vec![("app-feature".to_owned(), true, true)],
            ),
            "before the server reads any directory, the Tab keeps its saved repository"
        );
        assert_eq!(
            after_restart,
            (
                Some(MAIN_ROOT.to_owned()),
                Vec::new(),
                vec![
                    ("app".to_owned(), false, false),
                    ("app-extra".to_owned(), false, false),
                    ("app-feature".to_owned(), true, true),
                ],
            ),
            "the saved Main Worktree locates the repository the pruned Worktree's Tab is in, and a \
             Worktree added outside SpaceTerm joins the rows"
        );
        assert_eq!(
            after_prune,
            (
                Some(MAIN_ROOT.to_owned()),
                Vec::new(),
                vec![
                    ("app".to_owned(), false, false),
                    ("app-feature".to_owned(), true, true),
                ],
            ),
            "a Worktree pruned later leaves the rows while the Tab stays in its repository"
        );
        assert_eq!(pruned.removal, WorktreeRemoval::Unlisted);
        assert!(pruned.has_tabs && pruned.openable() && !pruned.opens_tabs());
        assert_eq!(harness.edits.submitted(), Vec::new());
    }

    /// Activating a shared Worktree row shows its Tab, and one without Tabs opens nothing.
    #[gpui::test]
    fn activating_a_shared_worktree_should_show_its_tab_without_asking_the_server(
        cx: &mut TestAppContext,
    ) {
        let (harness, cx) = harness(cx, published(1, "main", true));
        let feature = harness
            .row(cx)
            .worktrees
            .and_then(|section| {
                section
                    .rows()
                    .find(|row| row.name.as_ref() == "app-feature")
                    .map(|row| row.worktree_id)
            })
            .expect("the feature Worktree row");

        cx.update(|window, cx| {
            harness.manager.update(cx, |manager, cx| {
                manager.handle_sidebar_event(
                    SidebarEvent::ActivateWorktree {
                        workspace_id: harness.shared,
                        worktree_id: feature,
                        focus_pane: false,
                    },
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();
        let active_worktree = harness.manager.read_with(cx, |manager, cx| {
            manager
                .workspaces
                .workspace(harness.shared)
                .and_then(|workspace| workspace.payload().read(cx).active_worktree())
        });
        let copied_root = harness.manager.read_with(cx, |manager, _| {
            manager.shared_worktree_root(harness.shared, feature)
        });

        assert_eq!(active_worktree, Some(feature));
        assert_eq!(copied_root, Some(PathBuf::from(FEATURE_ROOT)));
        assert_eq!(harness.edits.submitted(), Vec::new());
    }

    /// The shared Workspace, active, with its two Worktree rows disclosed.
    fn active_harness(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
        let (harness, cx) = harness(cx, published(1, "main", true));
        cx.update(|window, cx| {
            harness.manager.update(cx, |manager, cx| {
                manager.activate_workspace(harness.shared, window, cx);
            });
        });
        cx.run_until_parked();
        (harness, cx)
    }

    fn worktree_ids(
        harness: &Harness,
        cx: &mut VisualTestContext,
    ) -> (crate::domain::WorktreeId, crate::domain::WorktreeId) {
        let section = harness.row(cx).worktrees.expect("the Worktree rows");
        let rows: Vec<_> = section.rows().map(|row| row.worktree_id).collect();
        (rows[0], rows[1])
    }

    #[gpui::test]
    fn worktree_menu_new_tab_uses_its_recent_tabs_focused_pane(cx: &mut TestAppContext) {
        use spaceterm_server::hierarchy::Mutation;
        use spaceterm_server::worktree::Operation;

        let mut initial = published(1, "main", true);
        let mut recent = initial.workspaces[0].tabs[1].clone();
        recent.id = [70; 32];
        recent.root_pane = [71; 32];
        recent.panes[0].id = recent.root_pane;
        let mut child = pane([72; 32], "/srv/app-feature/src");
        child.source = Some(recent.root_pane);
        recent.panes.push(child);
        initial.workspaces[0].tabs.push(recent);
        let (harness, cx) = harness(cx, initial);
        cx.update(|window, cx| {
            window.activate_window();
            harness.manager.update(cx, |manager, cx| {
                manager.activate_workspace(harness.shared, window, cx);
                manager
                    .workspaces
                    .workspace(harness.shared)
                    .unwrap()
                    .payload()
                    .update(cx, |tabs, cx| {
                        tabs.open_accepted_shared([70; 32], window, cx);
                    });
            });
        });
        redraw(cx);
        cx.dispatch_action(crate::ui::FocusPaneRight);
        cx.run_until_parked();
        let (main, feature) = worktree_ids(&harness, cx);
        click(worktree_row(harness.shared, main, cx), cx);
        cx.update(|_, cx| {
            let sidebar = harness.manager.read(cx).sidebar.clone();
            sidebar.update(cx, |_, cx| {
                cx.emit(SidebarEvent::WorktreeCommand {
                    workspace_id: harness.shared,
                    worktree_id: feature,
                    command: WorktreeMenuCommand::NewTab,
                });
            });
        });
        cx.run_until_parked();

        assert_eq!(
            harness.edits.submitted(),
            [Mutation::Worktree(Operation::OpenTab {
                workspace: WORKSPACE,
                source: Some([72; 32]),
                root: MAIN_ROOT.into(),
                path: FEATURE_ROOT.into(),
            })]
        );
    }

    #[gpui::test]
    fn worktree_rows_copy_the_path_and_confirm_their_server_tabs(cx: &mut TestAppContext) {
        use spaceterm_server::hierarchy::{
            CloseInventory, CloseTarget as ServerCloseTarget, ConfirmedPane, Mutation, Outcome,
        };
        let mut initial = published(1, "main", true);
        let mut second = initial.workspaces[0].tabs[1].clone();
        second.id = [70; 32];
        second.root_pane = [71; 32];
        second.panes[0].id = second.root_pane;
        initial.workspaces[0].tabs.push(second);
        initial.close = Some(CloseInventory {
            boot: [9; 32],
            sessions: initial.workspaces[0]
                .tabs
                .iter()
                .flat_map(|t| &t.panes)
                .map(|p| ConfirmedPane {
                    id: p.id,
                    session: None,
                })
                .collect(),
        });
        let (harness, cx) = harness(cx, initial.clone());
        cx.update(|window, cx| {
            window.activate_window();
            harness.manager.update(cx, |manager, cx| {
                manager.activate_workspace(harness.shared, window, cx);
            });
        });
        redraw(cx);
        let (_, feature) = worktree_ids(&harness, cx);
        right_click(worktree_row(harness.shared, feature, cx), cx);
        click("worktree-menu-row-copy-path", cx);
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
            Some(FEATURE_ROOT.into())
        );
        right_click(worktree_row(harness.shared, feature, cx), cx);
        click("worktree-menu-row-close-tabs", cx);
        assert!(
            harness.edits.submitted().is_empty(),
            "confirmation sends nothing"
        );
        assert!(
            spaceterm_ui::a11y_testing::A11yTree::read(cx)
                .find("Close Tabs?")
                .is_some()
        );
        click("modal-action-shared-close-confirm", cx);
        let target = ServerCloseTarget::Worktree {
            workspace: WORKSPACE,
            tab: FEATURE_TAB,
        };
        assert_eq!(
            harness.edits.submitted(),
            [Mutation::Close {
                target,
                confirmation: initial.close_confirmation(target).unwrap()
            }]
        );
        assert!(
            harness
                .row(cx)
                .worktrees
                .unwrap()
                .rows()
                .any(|row| row.worktree_id == feature && row.has_tabs),
            "publication owns removal"
        );
        harness
            .edits
            .answer(Outcome::Accepted { target: WORKSPACE });
        initial.workspaces[0].tabs.retain(|tab| tab.id == ROOT_TAB);
        initial.revision += 1;
        harness.publish(initial, cx);
        assert!(
            harness
                .row(cx)
                .worktrees
                .unwrap()
                .rows()
                .any(|row| row.worktree_id == feature && !row.has_tabs),
            "the Git Worktree row remains"
        );
    }

    #[gpui::test]
    fn closing_every_worktree_tab_refuses_without_a_confirmation_or_mutation(
        cx: &mut TestAppContext,
    ) {
        let mut initial = published(1, "main", true);
        initial.workspaces[0]
            .tabs
            .retain(|tab| tab.id == FEATURE_TAB);
        initial.workspaces[0].root_tab = FEATURE_TAB;
        let (harness, cx) = harness(cx, initial);
        let (_, feature) = worktree_ids(&harness, cx);
        assert!(
            !harness
                .row(cx)
                .worktrees
                .unwrap()
                .rows()
                .find(|row| row.worktree_id == feature)
                .unwrap()
                .closes_tabs
        );
        cx.update(|window, cx| {
            harness.manager.update(cx, |manager, cx| {
                manager.handle_sidebar_event(
                    SidebarEvent::WorktreeCommand {
                        workspace_id: harness.shared,
                        worktree_id: feature,
                        command: WorktreeMenuCommand::CloseTabs,
                    },
                    window,
                    cx,
                );
            });
        });
        redraw(cx);
        assert!(
            spaceterm_ui::a11y_testing::A11yTree::read(cx)
                .find("Tabs Not Closed")
                .is_some()
        );
        assert!(
            cx.debug_bounds("modal-action-shared-close-confirm")
                .is_none()
        );
        assert!(harness.edits.submitted().is_empty());
    }

    #[gpui::test]
    fn a_disclosure_chevron_should_trail_the_workspace_name(cx: &mut TestAppContext) {
        let (harness, cx) = active_harness(cx);
        redraw(cx);
        let id = harness.shared.get();
        let name = cx
            .debug_bounds(format!("workspace-row-name-{id}").leak())
            .expect("the Workspace name");
        let disclosure = cx
            .debug_bounds(format!("workspace-disclosure-{id}-expanded").leak())
            .expect("the disclosure chevron");
        let group = cx
            .debug_bounds(format!("workspace-group-{id}").leak())
            .expect("the Workspace group");

        click(format!("workspace-disclosure-{id}-expanded").leak(), cx);
        let collapsed_name = cx
            .debug_bounds(format!("workspace-row-name-{id}").leak())
            .expect("the Workspace name");
        let collapsed = cx
            .debug_bounds(format!("workspace-disclosure-{id}-collapsed").leak())
            .expect("the collapsed disclosure chevron");
        let branch = cx
            .debug_bounds(format!("workspace-row-branch-{id}").leak())
            .expect("line 2's branch");

        assert!(
            disclosure.left() >= name.right() && disclosure.right() > group.right() - px(40.0),
            "the chevron trails the Workspace name at the row's end"
        );
        assert_eq!(
            (collapsed.center().y, branch.right()),
            (collapsed_name.center().y, collapsed.right()),
            "a collapsed row's chevron trails line 1, so line 2 runs to the row's end"
        );
    }

    #[gpui::test]
    fn a_focused_git_workspace_row_should_collapse_and_expand_from_the_keyboard(
        cx: &mut TestAppContext,
    ) {
        let (harness, cx) = active_harness(cx);
        let expanded = |cx: &mut VisualTestContext| {
            harness.manager.read_with(cx, |manager, cx| {
                manager
                    .worktree_section(harness.shared, cx)
                    .unwrap()
                    .expanded
            })
        };
        cx.update(|window, cx| {
            harness
                .manager
                .update(cx, |manager, cx| manager.toggle_sidebar_focus(window, cx));
        });
        cx.simulate_keystrokes("up");
        cx.run_until_parked();
        let mut states = vec![expanded(cx)];

        for key in ["enter", "space", "left", "left", "right", "right"] {
            cx.simulate_keystrokes(key);
            cx.run_until_parked();
            states.push(expanded(cx));
        }

        assert_eq!(states, [true, false, true, false, false, true, true]);
        assert!(
            cx.update(|window, cx| harness.manager.read(cx).sidebar.read(cx).is_focused(window)),
            "toggling keeps the keyboard in the sidebar"
        );
    }

    #[gpui::test]
    fn worktree_rows_should_match_a_two_line_workspace_row_height(cx: &mut TestAppContext) {
        let (harness, cx) = active_harness(cx);
        let (_, feature) = worktree_ids(&harness, cx);
        redraw(cx);
        let row = worktree_row(harness.shared, feature, cx);
        let worktree = cx.debug_bounds(row).expect("the feature Worktree row");

        harness.manager.update(cx, |manager, cx| {
            manager.set_worktrees_expanded(harness.shared, false);
            cx.notify();
        });
        cx.run_until_parked();
        redraw(cx);
        let workspace = cx
            .debug_bounds(format!("workspace-row-{}-active", harness.shared.get()).leak())
            .expect("the collapsed Workspace row");

        assert_eq!(worktree.size.height, workspace.size.height);
    }

    /// Arrows show the Tabs of each Worktree row they stand on, Left and Right collapse and
    /// expand the group without changing the Active Worktree, and Return moves typing to it.
    #[gpui::test]
    fn the_keyboard_should_walk_worktrees_and_return_to_the_pane(cx: &mut TestAppContext) {
        let (harness, cx) = active_harness(cx);
        let (main, feature) = worktree_ids(&harness, cx);
        let sidebar_focus = harness
            .manager
            .read_with(cx, |manager, cx| manager.sidebar.read(cx).focus_handle());
        let active_worktree = |cx: &mut VisualTestContext| {
            harness.manager.read_with(cx, |manager, cx| {
                manager
                    .workspaces
                    .workspace(harness.shared)
                    .and_then(|workspace| workspace.payload().read(cx).active_worktree())
            })
        };
        let initial = active_worktree(cx);
        cx.update(|window, cx| sidebar_focus.focus(window, cx));
        cx.run_until_parked();

        let mut steps = Vec::new();
        for keys in ["down", "left", "left", "right", "down down", "enter"] {
            cx.simulate_keystrokes(keys);
            cx.run_until_parked();
            steps.push((selected_tree_item_labels(cx), active_worktree(cx)));
        }

        let step = |selected: &str, worktree| (vec![selected.to_owned()], Some(worktree));
        assert_eq!(initial, Some(main));
        assert_eq!(
            steps,
            [
                step("app-feature", feature),
                step("Build", feature),
                step("Build", feature),
                step("Build", feature),
                step("app-feature", feature),
                step("app-feature", feature),
            ]
        );
        assert!(
            cx.update(|window, _| !sidebar_focus.is_focused(window)),
            "Return moves typing to the Worktree's Tab"
        );
        assert_eq!(harness.edits.submitted(), Vec::new());
    }

    /// A secondary click on another Worktree row opens its menu and keeps the selection, and
    /// Escape returns typing to the Pane.
    #[gpui::test]
    fn a_worktree_menu_should_keep_the_selection_and_return_typing_to_the_pane(
        cx: &mut TestAppContext,
    ) {
        let (harness, cx) = active_harness(cx);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let (_, feature) = worktree_ids(&harness, cx);
        redraw(cx);
        let selected = selected_tree_item_labels(cx);

        right_click(worktree_row(harness.shared, feature, cx), cx);
        let selected_with_menu = selected_tree_item_labels(cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert_eq!(selected_with_menu, selected);
        assert!(
            active_terminal_has_input_focus(&harness.manager, cx),
            "Escape returns typing to the Pane"
        );
    }

    fn selected_tree_item_labels(cx: &mut VisualTestContext) -> Vec<String> {
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
            .with_role("TreeItem")
            .iter()
            .filter(|item| item["aria"]["selected"] == true)
            .map(|item| {
                item["aria"]["label"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    fn worktree_row(
        workspace_id: WorkspaceId,
        worktree_id: crate::domain::WorktreeId,
        cx: &mut VisualTestContext,
    ) -> &'static str {
        ["selected", "unselected"]
            .into_iter()
            .map(|state| -> &'static str {
                format!("worktree-row-{}-{worktree_id}-{state}", workspace_id.get()).leak()
            })
            .find(|selector| cx.debug_bounds(selector).is_some())
            .unwrap_or_else(|| panic!("worktree row {worktree_id} was not rendered"))
    }
}

#[path = "launch_tests.rs"]
mod launch_tests;

/// Saved connections remembered in memory, as the private record keeps them.
#[derive(Default)]
struct RememberedConnections(Mutex<Vec<crate::ssh::server_connection::RetainedSshConnection>>);

impl RememberedConnections {
    fn active(&self, destination: &str) -> Option<bool> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .find(|record| record.destination.as_str() == destination)
            .map(|record| record.active)
    }
}

impl crate::ssh::server_connection::SshConnectionIntentStore for RememberedConnections {
    fn load(
        &self,
    ) -> Result<
        Vec<crate::ssh::server_connection::RetainedSshConnection>,
        crate::ssh::server_connection::SshIntentStoreError,
    > {
        Ok(self.0.lock().unwrap().clone())
    }
    fn retain(
        &self,
        record: crate::ssh::server_connection::RetainedSshConnection,
    ) -> Result<(), crate::ssh::server_connection::SshIntentStoreError> {
        let mut records = self.0.lock().unwrap();
        records.retain(|other| other.destination != record.destination);
        records.push(record);
        Ok(())
    }
    fn remove(
        &self,
        destination: &crate::domain::SshDestination,
    ) -> Result<(), crate::ssh::server_connection::SshIntentStoreError> {
        self.0
            .lock()
            .unwrap()
            .retain(|record| record.destination != *destination);
        Ok(())
    }
}

/// An SSH computer that connects once over a route the test can end, then answers every later
/// attempt with `later`. Requested attempts are counted.
struct ConnectsOnceThen {
    route: Arc<AtomicBool>,
    attempts: AtomicUsize,
    requested: AtomicUsize,
    first_succeeds: bool,
    later: fn() -> crate::ssh::server_connection::SshConnectFailure,
}

struct ScriptedRoute(Arc<AtomicBool>);
impl crate::ssh::server_connection::SshServerLease for ScriptedRoute {
    fn is_alive(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl crate::ssh::server_connection::SshConnectBackend for ConnectsOnceThen {
    fn connect(
        &self,
        _: &crate::domain::SshDestination,
        mode: crate::ssh::server_connection::ConnectMode,
        _: Option<&crate::ssh::server_connection::SshServerContinuity>,
        _: &crate::ssh::cancellation::SshCancellationToken,
        _: &crate::ssh::server_connection::ConnectProgress<'_>,
    ) -> Result<
        crate::ssh::server_connection::ConnectedSshServer,
        crate::ssh::server_connection::SshConnectFailure,
    > {
        if mode == crate::ssh::server_connection::ConnectMode::Requested {
            self.requested.fetch_add(1, Ordering::SeqCst);
        }
        if !self.first_succeeds || self.attempts.fetch_add(1, Ordering::SeqCst) > 0 {
            return Err((self.later)());
        }
        Ok(crate::ssh::server_connection::ConnectedSshServer {
            server: spaceterm_server::LocalServer::for_identity("spaceterm-development").unwrap(),
            continuity: crate::ssh::server_connection::SshServerContinuity {
                route: [1; 32],
                account: 501,
                build: "spaceterm-development".into(),
                server: [2; 32],
            },
            lease: Arc::new(ScriptedRoute(self.route.clone())),
            protocol_major: 1,
            facts: crate::ssh::server_connection::SshServerFacts {
                platform: ("linux".into(), "x86_64".into()),
                version: Some("1.0.0".into()),
                starts_automatically: true,
            },
        })
    }
    fn observe(
        &self,
        _: &crate::domain::SshDestination,
        _: Option<&crate::ssh::server_connection::SshServerContinuity>,
        _: &crate::ssh::cancellation::SshCancellationToken,
        _: &crate::ssh::server_connection::ConnectProgress<'_>,
    ) -> Result<
        crate::ssh::server_connection::SshServerObservation,
        crate::ssh::server_connection::SshConnectFailure,
    > {
        Err((self.later)())
    }
}

fn wait_until(mut done: impl FnMut() -> bool, cx: &mut VisualTestContext) {
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "the registry settles");
        std::thread::sleep(std::time::Duration::from_millis(5));
        cx.run_until_parked();
    }
    redraw(cx);
}

/// Connects to "penguin", presents it, ends its route so the next attempt answers `later`, and
/// checks that the heading shows `state` and offers Disconnect, which withdraws the server and
/// saves it inactive.
fn a_failing_presented_server_should_offer_disconnect(
    later: fn() -> crate::ssh::server_connection::SshConnectFailure,
    state: &str,
    cx: &mut TestAppContext,
) {
    use crate::ssh::server_connection::SshServerConnections;

    let (harness, cx) = MachineGroupsHarness::new(cx);
    let route = Arc::new(AtomicBool::new(true));
    let remembered = Arc::new(RememberedConnections::default());
    let connections = SshServerConnections::new(
        Arc::new(ConnectsOnceThen {
            route: route.clone(),
            attempts: AtomicUsize::new(0),
            requested: AtomicUsize::new(0),
            first_succeeds: true,
            later,
        }),
        remembered.clone(),
    );
    let installed = connections.clone();
    cx.update(|_, cx| crate::ui::ssh_servers::install(installed, None, cx));
    let penguin = crate::domain::SshDestination::new("penguin".to_owned()).unwrap();
    connections.connect(penguin.clone());
    wait_until(|| connections.server(&penguin).is_some(), cx);
    // The window presents the connected server as its registry binding would.
    harness.present_ssh("penguin", &[("Far", 1)], cx);

    route.store(false, Ordering::SeqCst);
    let expected = format!("penguin, {state}");
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while !heading_names(cx).contains(&expected) {
        assert!(
            Instant::now() < deadline,
            "the heading should show {expected:?}, got {:?}",
            heading_names(cx)
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        redraw(cx);
    }

    right_click("machine-heading-penguin", cx);
    click("machine-menu-disconnect", cx);
    wait_until(|| remembered.active("penguin") == Some(false), cx);
    assert!(connections.server(&penguin).is_none());
}

/// A presented server whose reconnect fails keeps its frozen Workspaces under its heading, which
/// still reconnects it quietly and offers Disconnect to end that.
#[gpui::test]
fn a_presented_server_that_fails_after_connecting_should_offer_disconnect(cx: &mut TestAppContext) {
    use crate::ssh::server_connection::{SshConnectFailure, UnavailableKind};
    a_failing_presented_server_should_offer_disconnect(
        || SshConnectFailure::Unavailable(UnavailableKind::ServiceUnavailable),
        "Unavailable",
        cx,
    );
}

/// A presented server stopped on its computer no longer reconnects, yet its heading still offers
/// Disconnect, so Remove isn't the only way to withdraw it.
#[gpui::test]
fn a_presented_server_stopped_after_connecting_should_offer_disconnect(cx: &mut TestAppContext) {
    a_failing_presented_server_should_offer_disconnect(
        || crate::ssh::server_connection::SshConnectFailure::Stopped,
        "Stopped",
        cx,
    );
}

/// After relaunch, a Saved Connection SpaceTerm keeps trying heads the Workspace Window while it
/// waits for the person, even before any of its Workspaces arrive, and its Connect asks once.
#[gpui::test]
fn a_saved_connection_waiting_after_relaunch_should_head_the_window_with_connect(
    cx: &mut TestAppContext,
) {
    use crate::ssh::server_connection::{
        InteractionReason, RetainedSshConnection, SshConnectFailure, SshServerConnections,
    };

    let (harness, cx) = MachineGroupsHarness::new(cx);
    let remembered = Arc::new(RememberedConnections::default());
    remembered.0.lock().unwrap().extend([
        RetainedSshConnection {
            destination: crate::domain::SshDestination::new("penguin".to_owned()).unwrap(),
            continuity: None,
            active: true,
        },
        RetainedSshConnection {
            destination: crate::domain::SshDestination::new("idle".to_owned()).unwrap(),
            continuity: None,
            active: false,
        },
    ]);
    let backend = Arc::new(ConnectsOnceThen {
        route: Arc::new(AtomicBool::new(true)),
        attempts: AtomicUsize::new(0),
        requested: AtomicUsize::new(0),
        first_succeeds: false,
        later: || SshConnectFailure::NeedsPerson(InteractionReason::CredentialProvider),
    });
    let connections = SshServerConnections::new(backend.clone(), remembered.clone());
    let installed = connections.clone();
    cx.update(|window, cx| {
        crate::ui::ssh_servers::install(installed, None, cx);
        harness
            .manager
            .update(cx, |manager, cx| manager.follow_ssh_servers(window, cx));
    });
    connections.resume_retained();
    let penguin = crate::domain::SshDestination::new("penguin".to_owned()).unwrap();
    wait_until(
        || {
            matches!(
                connections.status(&penguin),
                crate::ssh::server_connection::RemoteConnectionStatus::NeedsPerson { .. }
            )
        },
        cx,
    );

    assert_eq!(
        heading_names(cx),
        vec![
            "This Computer".to_owned(),
            "penguin, Waiting for You".to_owned()
        ],
        "the waiting connection has its heading; the inactive one has none"
    );
    assert!(cx.debug_bounds("machine-empty-penguin").is_some());
    assert_eq!(
        backend.requested.load(Ordering::SeqCst),
        0,
        "relaunch asks nothing"
    );

    right_click("machine-heading-penguin", cx);
    click("machine-menu-connect", cx);
    wait_until(|| backend.requested.load(Ordering::SeqCst) == 1, cx);
}
