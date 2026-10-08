use crate::directory_selection::GpuiDirectorySelection;
use crate::domain::RemoteConnectionPhase;
use crate::ssh::remote_account::RemoteWorkspaceAccount;
use crate::terminal::GpuiTerminalKeyInputAdapterFactory;
use crate::ui::workspace_frame::WorkspaceFrame;
use crate::ui::workspace_sidebar::SIDEBAR_ROW_SELECTION_INSET_Y;
use crate::ui::{TOP_CHROME_HEIGHT, WORKSPACE_SIDEBAR_MINIMUM_WIDTH};
use gpui::MouseButton;
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
            .active_workspace()
            .name()
            .to_owned()),
        "api"
    );

    cx.simulate_keystrokes("cmd-n");
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
            .active_workspace()
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
            .active_workspace()
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
            .active_workspace()
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
        let workspace = manager.workspaces.active_workspace();
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
fn sidebar_rows_should_keep_the_directory_and_pin_below_name_and_hide_machine_when_narrow(
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
    let machine = cx.debug_bounds("workspace-machine-2").unwrap();
    let path = cx.debug_bounds("workspace-row-path-2").unwrap();
    let pin = cx.debug_bounds("workspace-row-pin-2").unwrap();
    assert!(machine.left() - name.right() >= px(8.0));
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
    let active = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
    cx.simulate_click(number.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://github.com/acme/api/pull/478")
    );
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id()),
        active
    );
}

#[gpui::test]
fn sidebar_rows_contain_semantic_text_in_both_densities(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);
    click_new_workspace_menu("new-workspace-menu-create-local", cx);

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
fn native_service_factory_reaches_initial_new_and_replacement_hierarchy(cx: &mut TestAppContext) {
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
        WorkspaceManager::new_with_adapters(
            session_factory,
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                native_services,
                ..workspace_adapters(test_remote_backend_factory())
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.focus(window, cx)));
    cx.run_until_parked();
    assert_eq!(created.get(), 1);
    for (shortcut, expected) in [("cmd-d", 2), ("cmd-t", 3), ("cmd-n", 4)] {
        cx.simulate_keystrokes(shortcut);
        cx.run_until_parked();
        assert_eq!(created.get(), expected, "{shortcut}");
    }
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            let workspace_id = manager.workspaces.active_workspace_id();
            manager.close_workspace(workspace_id, window, cx);
            let final_workspace_id = manager.workspaces.active_workspace_id();
            manager.close_workspace(final_workspace_id, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(created.get(), 5, "replacement Workspace");
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
        WorkspaceManager::new_with_adapters(
            session_factory,
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                accessibility: factory.clone(),
                ..workspace_adapters(test_remote_backend_factory())
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.focus(window, cx)));
    cx.run_until_parked();
    assert_eq!(factory.records.borrow().len(), 1);
    for (shortcut, expected) in [("cmd-d", 2), ("cmd-t", 3), ("cmd-n", 4)] {
        cx.simulate_keystrokes(shortcut);
        cx.run_until_parked();
        assert_eq!(factory.records.borrow().len(), expected, "{shortcut}");
    }
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
        WorkspaceManager::new_with_adapters(
            session_factory,
            std::env::temp_dir(),
            workspace_adapters(test_remote_backend_factory()),
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
        WorkspaceManager::new_with_adapters(
            session_factory,
            std::env::temp_dir(),
            workspace_adapters(test_remote_backend_factory()),
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
        WorkspaceManager::new_with_adapters(
            session_factory,
            std::env::temp_dir(),
            workspace_adapters(factory),
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
        WorkspaceManager::new_with_adapters(
            session_factory,
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                window_drag: injected_platform,
                ..workspace_adapters(test_remote_backend_factory())
            },
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
        WorkspaceManager::new_with_adapters(
            session_factory,
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                directory_selection,
                ..workspace_adapters(test_remote_backend_factory())
            },
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
            manager.choose_pin_directory(manager.workspaces.active_workspace_id(), window, cx);
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
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
        WorkspaceManager::new_with_adapters(
            session_factory,
            std::env::temp_dir(),
            workspace_adapters(test_remote_backend_factory()),
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
            .active_workspace()
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
            .active_workspace()
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
        WorkspaceManager::new_with_adapters(
            first_factory,
            PathBuf::from("/Users/first"),
            workspace_adapters(test_remote_backend_factory()),
            window,
            cx,
        )
    });
    let second = cx.add_window(|window, cx| {
        WorkspaceManager::new_with_adapters(
            second_factory,
            PathBuf::from("/Users/second"),
            workspace_adapters(test_remote_backend_factory()),
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
                .active_workspace()
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
                .active_workspace()
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
            .active_workspace()
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
fn top_combo_box_local_choice_should_create_without_a_directory_picker(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    open_workspace_switcher_for_creation(cx);
    click("workspace-switcher-create-local", cx);
    assert_eq!(
        cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                window_combo_box_is_open(window, cx),
                manager.terminal_focus_blocker(window, cx),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
                manager.workspaces.len(),
                records.starts().len(),
            )
        }),
        (false, None, true, 2, 2)
    );
    assert!(manager.read_with(cx, |manager, _| manager.pin_picker.is_none()));
}

#[gpui::test]
fn top_combo_box_keyboard_acceptance_should_create_exactly_one_local_workspace(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    open_workspace_switcher_for_creation(cx);

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert_eq!(
        cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.workspaces.len(),
                records.starts().len(),
                window_combo_box_is_open(window, cx),
                manager.terminal_focus_blocker(window, cx),
                manager
                    .workspaces
                    .active_workspace()
                    .payload()
                    .read(cx)
                    .focused_terminal_is_focused(window, cx),
            )
        }),
        (2, 2, false, None, true)
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
                    .active_workspace()
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
                    .active_workspace()
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
        WorkspaceManager::new_with_adapters(
            session_factory,
            std::env::temp_dir(),
            workspace_adapters(factory),
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
                    .active_workspace()
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
                    .active_workspace()
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
                .active_workspace()
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
            .active_workspace()
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
                .active_workspace()
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
        WorkspaceManager::new_with_adapters(
            session_factory,
            std::env::temp_dir(),
            workspace_adapters(factory),
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
    let (workspace_id, tab_manager, view) = manager.read_with(cx, |manager, cx| {
        let workspace = manager.workspaces.active_workspace();
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
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
            let workspace = manager.workspaces.active_workspace();
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
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
        let workspace = manager.workspaces.active_workspace();
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
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
        let workspace = manager.workspaces.active_workspace();
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
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
        let workspace = manager.workspaces.active_workspace();
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
                .active_workspace()
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
                .active_workspace()
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
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
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());

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
            let workspace_id = manager.workspaces.active_workspace_id();
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
        manager
            .workspaces
            .active_workspace()
            .pinned_directory()
            .is_none()
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
            manager.choose_pin_directory(manager.workspaces.active_workspace_id(), window, cx);
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
        match manager.workspaces.active_workspace().pinned_directory() {
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

fn active_local_pin(
    manager: &Entity<WorkspaceManager>,
    cx: &mut VisualTestContext,
) -> Option<PathBuf> {
    manager.read_with(cx, |manager, _| {
        match manager.workspaces.active_workspace().pinned_directory() {
            Some(PinnedDirectory::Local(directory)) => Some(directory.path().to_owned()),
            _ => None,
        }
    })
}

#[gpui::test]
fn open_local_directory_should_create_a_pinned_workspace_named_by_the_switcher_query(
    cx: &mut TestAppContext,
) {
    let project = temporary_directory("open-local");
    fs::create_dir_all(&project).unwrap();
    let (manager, records, cx) = workspace_manager_with_directory_selection([], cx);
    open_workspace_switcher_for_creation(cx);
    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    assert!(manager.read_with(cx, |manager, _| manager.pin_picker.is_some()));
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        1
    );

    confirm_directory_picker_path(&format!("{}/", project.to_str().unwrap()), cx);

    manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.len(), 2);
        assert_eq!(
            manager.workspaces.active_workspace_id(),
            WorkspaceId::new(2)
        );
        assert_eq!(
            manager.workspaces.active_workspace().name(),
            "fresh workspace"
        );
        assert!(manager.pin_picker.is_none());
    });
    assert_eq!(active_local_pin(&manager, cx), Some(project.clone()));
    let starts = records.starts();
    assert_eq!(starts.len(), 2);
    assert_eq!(
        starts
            .last()
            .unwrap()
            .local_working_directory()
            .unwrap()
            .path(),
        project.as_path()
    );
    fs::remove_dir_all(project).unwrap();
}

#[gpui::test]
fn open_local_directory_should_activate_the_workspace_already_pinned_to_the_directory(
    cx: &mut TestAppContext,
) {
    let project = temporary_directory("open-local-existing");
    fs::create_dir_all(&project).unwrap();
    let (manager, records, cx) = workspace_manager_with_directory_selection([], cx);
    let path = format!("{}/", project.to_str().unwrap());
    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    confirm_directory_picker_path(&path, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            assert!(manager.activate_workspace(WorkspaceId::new(1), window, cx));
        })
    });

    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    confirm_directory_picker_path(&path, cx);

    manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.len(), 2);
        assert_eq!(
            manager.workspaces.active_workspace_id(),
            WorkspaceId::new(2)
        );
    });
    assert_eq!(records.starts().len(), 2);
    fs::remove_dir_all(project).unwrap();
}

#[gpui::test]
fn open_local_directory_should_keep_the_picker_usable_after_a_failed_creation(
    cx: &mut TestAppContext,
) {
    let project = temporary_directory("open-local-retry");
    fs::create_dir_all(&project).unwrap();
    let (manager, _, cx) = workspace_manager_with_directory_selection([], cx);
    manager.update(cx, |manager, _| {
        crate::domain::set_next_workspace_id(&mut manager.workspaces, u64::MAX);
    });
    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    confirm_directory_picker_path(&format!("{}/", project.to_str().unwrap()), cx);
    manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.len(), 1);
        assert!(manager.pin_picker.is_some());
    });

    manager.update(cx, |manager, _| {
        crate::domain::set_next_workspace_id(&mut manager.workspaces, 2);
    });
    confirm_directory_picker_path(&format!("{}/", project.to_str().unwrap()), cx);

    manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.len(), 2);
        assert!(manager.pin_picker.is_none());
    });
    assert_eq!(active_local_pin(&manager, cx), Some(project.clone()));
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active_workspace()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }));
    fs::remove_dir_all(project).unwrap();
}

#[gpui::test]
fn open_local_directory_should_activate_the_pinned_workspace_when_home_is_unavailable(
    cx: &mut TestAppContext,
) {
    let project = temporary_directory("open-local-reuse-missing-home");
    fs::create_dir_all(&project).unwrap();
    let (manager, records, cx) = workspace_manager_with_directory_selection([], cx);
    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    confirm_directory_picker_path(&format!("{}/", project.to_str().unwrap()), cx);
    let pinned = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
    manager.update(cx, |manager, _| {
        manager.local_home_directory_path =
            temporary_directory("open-local-reuse-missing-home-directory");
    });
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            assert!(manager.activate_workspace(WorkspaceId::new(1), window, cx));
        })
    });

    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    confirm_directory_picker_path(&format!("{}/", project.to_str().unwrap()), cx);

    assert!(!cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.len(), 2);
        assert_eq!(manager.workspaces.active_workspace_id(), pinned);
        assert!(manager.pin_picker.is_none());
    });
    assert_eq!(records.starts().len(), 2);
    fs::remove_dir_all(project).unwrap();
}

#[gpui::test]
fn open_local_directory_should_alert_once_when_system_selection_meets_an_unavailable_home(
    cx: &mut TestAppContext,
) {
    let project = temporary_directory("open-local-system-missing-home");
    fs::create_dir_all(&project).unwrap();
    let (manager, _, cx) =
        workspace_manager_with_directory_selection([Ok(Some(project.clone()))], cx);
    manager.update(cx, |manager, _| {
        manager.local_home_directory_path =
            temporary_directory("open-local-system-missing-home-directory");
    });
    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    click("directory-picker-confirm-menu", cx);
    click(
        "command-palette-primary-menu-directory-picker-system-selection",
        cx,
    );

    assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    click("modal-action-workspace-home-error-ok", cx);

    assert!(!cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    manager.read_with(cx, |manager, _| assert_eq!(manager.workspaces.len(), 1));
    fs::remove_dir_all(project).unwrap();
}

#[gpui::test]
fn open_local_directory_should_close_the_picker_and_alert_when_home_is_unavailable(
    cx: &mut TestAppContext,
) {
    let project = temporary_directory("open-local-missing-home-project");
    fs::create_dir_all(&project).unwrap();
    let (manager, records, cx) = workspace_manager_with_directory_selection([], cx);
    manager.update(cx, |manager, _| {
        manager.local_home_directory_path = temporary_directory("open-local-missing-home");
    });
    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    confirm_directory_picker_path(&format!("{}/", project.to_str().unwrap()), cx);

    assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.len(), 1);
        assert!(manager.pin_picker.is_none());
    });
    click("modal-action-workspace-home-error-ok", cx);
    assert_eq!(records.starts().len(), 1);
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active_workspace()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }));
    fs::remove_dir_all(project).unwrap();
}

#[gpui::test]
fn sidebar_open_local_directory_should_name_the_workspace_automatically_or_create_nothing(
    cx: &mut TestAppContext,
) {
    let project = temporary_directory("open-local-sidebar");
    fs::create_dir_all(&project).unwrap();
    let (manager, records, cx) = workspace_manager_with_directory_selection([], cx);
    click_new_workspace_menu("new-workspace-menu-open-local-directory", cx);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        1
    );
    assert!(manager.read_with(cx, |manager, _| manager.pin_picker.is_none()));

    click_new_workspace_menu("new-workspace-menu-open-local-directory", cx);
    confirm_directory_picker_path(&format!("{}/", project.to_str().unwrap()), cx);

    let expected_name = project.file_name().unwrap().to_str().unwrap().to_owned();
    manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.len(), 2);
        assert_eq!(manager.workspaces.active_workspace().name(), expected_name);
    });
    assert_eq!(active_local_pin(&manager, cx), Some(project.clone()));
    assert_eq!(records.starts().len(), 2);
    fs::remove_dir_all(project).unwrap();
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
        match manager.workspaces.active_workspace().pinned_directory() {
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
            .active_workspace()
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
        assert_eq!(
            manager.workspaces.active_workspace_id(),
            WorkspaceId::new(2)
        );
        assert!(manager.remote_workspace_flow.is_none());
        assert_eq!(manager.remote_workspace_runtimes.len(), 1);
        match manager.workspaces.active_workspace().pinned_directory() {
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
fn unavailable_pinned_directory_should_block_children_and_recover_when_restored(
    cx: &mut TestAppContext,
) {
    let root = temporary_directory("availability");
    let project = root.join("project");
    let parked = root.join("parked");
    fs::create_dir_all(&project).unwrap();
    let (manager, records, cx) =
        workspace_manager_with_directory_selection([Ok(Some(project.clone()))], cx);
    choose_pin_directory(&manager, cx);
    assert_eq!(records.starts().len(), 1);

    fs::rename(&project, &parked).unwrap();
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    assert_eq!(records.starts().len(), 1);
    assert!(manager.read_with(cx, |manager, _| {
        manager
            .workspaces
            .active_workspace()
            .pinned_directory()
            .is_some()
    }));
    assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    click("modal-action-tab-start-error-ok", cx);
    cx.run_until_parked();
    assert!(!cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active_workspace()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }));

    fs::rename(&parked, &project).unwrap();
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    assert_eq!(records.starts().len(), 2);
    assert!(manager.read_with(cx, |manager, _| {
        matches!(
            manager.workspaces.active_workspace().availability(),
            crate::domain::DirectoryAvailability::Available
        )
    }));
    fs::remove_dir_all(root).unwrap();
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
        manager
            .workspaces
            .active_workspace()
            .pinned_directory()
            .is_none()
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
        (true, Some(TerminalFocusBlocker::Sidebar)),
        "closing the replacement palette must not restore the invisible menu focus owner"
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
            .active_workspace()
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
    cx.simulate_keystrokes("cmd-n");
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
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    let state = manager.read_with(cx, |manager, _| {
        (
            manager.workspaces.workspace(WorkspaceId::new(1)).is_none(),
            manager.workspaces.active_workspace_id(),
        )
    });
    assert_eq!(state, (true, WorkspaceId::new(3)));
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active_workspace()
            .name()
            .to_owned()),
        "alpha"
    );
}

#[gpui::test]
fn workspace_switcher_selection_should_activate_the_matching_workspace(cx: &mut TestAppContext) {
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

    click("workspace-switcher", cx);
    cx.simulate_keystrokes("a l p h a enter");
    cx.run_until_parked();

    let state = cx.update(|window, cx| {
        let manager = manager.read(cx);
        (
            manager.workspaces.active_workspace_id(),
            window_combo_box_is_open(window, cx),
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
        let workspace = manager.workspaces.active_workspace();
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

    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    let first_pending = manager.read_with(cx, |manager, _| {
        manager
            .close_confirmation
            .pending()
            .expect("risky close should present one confirmation")
    });
    cx.simulate_keystrokes("cmd-w");
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

    cx.simulate_keystrokes("cmd-w");
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
    assert_eq!(application_quit.requests(), 1);
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
        manager.read_with(cx, |manager, _| (
            manager.workspaces.len(),
            manager.workspaces.active_workspace_id(),
        )),
        (1, WorkspaceId::new(2))
    );
    assert_eq!(records.dropped_session_ids(), vec![1, 2]);
    assert_eq!(records.session_count(), 3);
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
fn command_q_and_quit_action_should_cancel_safely_and_confirm_once(cx: &mut TestAppContext) {
    let (manager, records, application_quit, cx) = workspace_manager_with_application_actions(cx);
    redraw(cx);
    cx.simulate_keystrokes("cmd-q");
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
        cx.simulate_keystrokes("cmd-n");
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
            manager.sidebar.read(cx).is_resizing(),
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
                .active_workspace()
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
            &chrome_identity(manager.read(cx).workspaces.active_workspace()),
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
    cx.simulate_keystrokes("cmd-n cmd-n");
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
fn sidebar_new_workspace_button_should_create_local_immediately(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    click_new_workspace_menu("new-workspace-menu-create-local", cx);
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        2
    );
    assert_eq!(records.starts().len(), 2);
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active_workspace()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }));
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
    cx.simulate_keystrokes("cmd-n");
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
fn collapsed_identity_should_keep_an_unavailable_local_workspace_visible(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    manager.update(cx, |manager, cx| {
        let workspace_id = manager.workspaces.active_workspace_id();
        manager
            .workspaces
            .set_directory_unavailable(workspace_id, "Directory unavailable".to_owned())
            .expect("the Active Workspace should remain present");
        cx.notify();
    });

    click("toggle-sidebar-button", cx);

    assert!(cx.debug_bounds("workspace-chip-label").is_some());
    assert!(
        cx.debug_bounds("workspace-switcher-status-unavailable")
            .is_some(),
        "the collapsed identity must keep the Active Workspace's unavailable state visible"
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
                .active_workspace()
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
fn collapsed_workspace_name_should_open_filter_and_create_a_named_workspace(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    click("toggle-sidebar-button", cx);
    click("workspace-chip-label", cx);
    cx.simulate_keystrokes("f r e s h space w o r k s p a c e");
    cx.run_until_parked();
    click("workspace-switcher-create-local", cx);
    assert_eq!(
        manager.read_with(cx, |manager, _| {
            manager.workspaces.active_workspace().name().to_owned()
        }),
        "fresh workspace"
    );
    assert_eq!(records.starts().len(), 2);
    assert!(!manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().visible));
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active_workspace()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }));
}

#[gpui::test]
fn workspace_created_while_collapsed_should_share_the_active_top_chrome_width(
    cx: &mut TestAppContext,
) {
    let (_, _, cx) = workspace_manager(cx);
    click("toggle-sidebar-button", cx);

    cx.simulate_keystrokes("cmd-n");
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
            .active_workspace()
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
    cx.simulate_keystrokes("cmd-n");
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
            manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id()),
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

    cx.simulate_keystrokes("cmd-n");
    cx.run_until_parked();

    assert_eq!(
        cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.workspaces.len(),
                manager.workspaces.active_workspace_id(),
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
        cx.simulate_keystrokes("cmd-n");
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
        cx.simulate_keystrokes("cmd-n");
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
        cx.simulate_keystrokes("cmd-n");
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
    cx.simulate_keystrokes("cmd-n");
    cx.simulate_keystrokes("cmd-n");
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
fn a_workspace_row_released_below_the_list_should_stay_in_place(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-n");
    cx.simulate_keystrokes("cmd-n");
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
    let marked = cx.debug_bounds("workspace-insertion-marker-3").is_some();
    cx.simulate_mouse_up(below, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(
        (marked, workspace_order(&manager, cx)),
        (false, vec![1, 2, 3])
    );
}

#[gpui::test]
fn dragging_a_workspace_row_should_mark_its_slot_and_land_there_on_release(
    cx: &mut TestAppContext,
) {
    let (manager, _records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-n");
    cx.simulate_keystrokes("cmd-n");
    cx.run_until_parked();
    let first = cx.debug_bounds("workspace-row-1-inactive").unwrap();
    let second = cx.debug_bounds("workspace-row-2-inactive").unwrap();
    let third = cx.debug_bounds("workspace-row-3-active").unwrap();

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
    cx.run_until_parked();
    let between = cx.debug_bounds("workspace-insertion-marker-2");
    let release = third.center() + point(px(0.0), px(4.0));
    cx.simulate_mouse_move(release, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    let during = (
        workspace_order(&manager, cx),
        cx.debug_bounds("workspace-row-1-inactive"),
        cx.debug_bounds("workspace-row-preview-1-inactive")
            .map(|preview| preview.size),
        cx.debug_bounds("workspace-insertion-marker-3").is_some(),
    );
    cx.simulate_mouse_up(release, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(
        during,
        (vec![1, 2, 3], Some(first), Some(first.size), true),
        "a dragged row must keep its place, lift an exact copy, and mark where it lands"
    );
    // The marker between two rows stands on the edge they share.
    assert_eq!(between.map(|marker| marker.center().y), Some(third.top()));
    assert_eq!(
        (
            workspace_order(&manager, cx),
            manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id()),
        ),
        (vec![2, 3, 1], WorkspaceId::new(3))
    );
    assert!(cx.debug_bounds("drag-preview").is_none());
    assert!(cx.debug_bounds("workspace-insertion-marker-3").is_none());
    let moved = cx
        .debug_bounds("workspace-row-1-inactive")
        .expect("the moved Workspace must render in its new place");
    assert_eq!(moved.origin, third.origin);

    // Position shortcuts follow the presented order.
    cx.simulate_keystrokes("ctrl-1");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id()),
        WorkspaceId::new(2)
    );
}

#[gpui::test]
fn escape_should_cancel_a_workspace_row_drag_without_moving_it(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-n");
    cx.simulate_keystrokes("cmd-n");
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
    let marked = cx.debug_bounds("workspace-insertion-marker-3").is_some();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let cancelled = (
        cx.debug_bounds("drag-preview").is_some(),
        cx.debug_bounds("workspace-insertion-marker-3").is_some(),
    );
    cx.simulate_mouse_up(release, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(
        (marked, cancelled, workspace_order(&manager, cx)),
        (true, (false, false), vec![1, 2, 3])
    );
    let unmoved = cx
        .debug_bounds("workspace-row-1-inactive")
        .expect("the cancelled Workspace must render in its original place");
    assert_eq!(unmoved.origin, first.origin);
}

#[gpui::test]
fn a_single_motion_should_move_a_workspace_row_on_release(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-n");
    cx.simulate_keystrokes("cmd-n");
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
    cx.simulate_keystrokes("cmd-n");
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
    cx.run_until_parked();
    let marked = cx.debug_bounds("tab-insertion-marker-2").is_some();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let cancelled = (
        visible_tabs.read_with(cx, |tabs, _| tabs.dragged_tab()),
        cx.debug_bounds("tab-insertion-marker-2").is_some(),
    );
    cx.simulate_mouse_up(past_second, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    // The hidden Tab bar never saw its release, so its drag landed nowhere.
    assert_eq!(
        (
            marked,
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
        cx.simulate_keystrokes("cmd-n");
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
fn creating_workspaces_should_scroll_the_active_workspace_into_view(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    for _ in 0..24 {
        cx.simulate_keystrokes("cmd-n");
    }

    let state = manager.read_with(cx, |manager, cx| {
        (
            manager.workspaces.len(),
            manager.workspaces.active_workspace_id(),
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
        cx.simulate_keystrokes("cmd-n");
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
            &chrome_identity(manager.read(cx).workspaces.active_workspace()),
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
fn a_secondary_click_does_not_emphasize_the_sidebar_it_focuses(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    right_click("workspace-row-1-active", cx);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| manager.read(cx).sidebar.read(cx).is_focused(window)));
    assert!(
        !active_row_is_emphasized(cx),
        "only the keyboard emphasizes the sidebar selection"
    );

    // Keyboard navigation within the focused sidebar restores the emphasis.
    cx.simulate_keystrokes("home");
    cx.run_until_parked();
    assert!(active_row_is_emphasized(cx));
}

#[gpui::test]
fn command_n_creates_and_activates_a_home_workspace(cx: &mut TestAppContext) {
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
            manager.sidebar.read(cx).is_focused(window),
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
            manager.sidebar.read(cx).is_focused(window),
            manager.sidebar.read(cx).menu_open(),
            manager
                .workspaces
                .active_workspace()
                .payload()
                .read(cx)
                .focused_terminal_is_focused(window, cx),
        )
    });
    assert_eq!(state, (WorkspaceId::new(1), false, true, false));
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
    assert_eq!(
        dismissed,
        (false, true, Some(TerminalFocusBlocker::Sidebar))
    );
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
                manager.workspaces.active_workspace_id(),
            )
        }),
        (
            true,
            Some(TerminalFocusBlocker::Sidebar),
            WorkspaceId::new(1)
        )
    );
    click("workspace-row-1-active", cx);
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

    assert_eq!((created_state.0, created_state.1), (false, (2, 2)));
    assert!(cx.debug_bounds("tab-item-1-active").is_some());
    assert!(cx.debug_bounds("tab-item-2-inactive").is_some());
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
    click("modal-action-close-confirmation-confirm", cx);

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
fn duplicate_final_tab_close_requests_should_schedule_one_operating_system_window_close(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    let tab_manager = manager.read_with(cx, |manager, _| {
        manager.workspaces.active_workspace().payload().clone()
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
    assert!(cx.windows().is_empty());
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
                .active_workspace()
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
        manager.workspaces.active_workspace().name().to_owned()
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
            manager.workspaces.active_workspace_id(),
            manager.workspaces.active_workspace().name().to_owned(),
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
    let state_after_submit = manager.read_with(cx, |manager, cx| {
        (
            !manager.sidebar.read(cx).is_renaming(),
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
    let state_after_submit = manager.read_with(cx, |manager, cx| {
        (
            !manager.sidebar.read(cx).is_renaming(),
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

    let sidebar_focus =
        manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).focus_handle());
    cx.update(|window, cx| sidebar_focus.focus(window, cx));
    cx.run_until_parked();

    let rename_state = manager.read_with(cx, |manager, cx| {
        (
            manager.workspaces.active_workspace().name().to_owned(),
            !manager.sidebar.read(cx).is_renaming(),
        )
    });
    assert_eq!(rename_state, ("Dev".to_owned(), true));
}

#[gpui::test]
fn activating_another_workspace_should_cancel_the_previous_inline_rename(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-n");
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
            manager.workspaces.active_workspace_id(),
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
fn explicitly_closing_the_final_workspace_should_replace_it_and_keep_the_window_open(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);

    right_click("workspace-row-1-active", cx);
    click("workspace-menu-row-close", cx);
    click("modal-action-close-confirmation-confirm", cx);

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
    cx.update(|window, _| window.activate_window());
    let inactive_sender = records
        .event_sender(1)
        .expect("the initial Workspace Terminal Session must have started");
    cx.simulate_keystrokes("cmd-n");
    cx.run_until_parked();

    assert!(active_terminal_has_input_focus(&manager, cx));
    inactive_sender
        .try_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success))
        .expect("the inactive shell exit must be delivered");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id()),
        WorkspaceId::new(2)
    );
    assert!(active_terminal_has_input_focus(&manager, cx));
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
#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
mod unix_adapter_tests {
    include!("../../platform/unix_adapter_tests/workspace_manager.rs");
}

#[gpui::test]
fn pin_change_and_unpin_should_only_affect_future_terminal_starts(cx: &mut TestAppContext) {
    let root = temporary_directory("pin-policy");
    let first = root.join("first");
    let second = root.join("second");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    let (manager, records, cx) = workspace_manager(cx);
    for (directory, existing_count) in [(&first, 1), (&second, 2)] {
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                let directory = manager
                    .local_filesystem
                    .validate_directory(directory)
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
        assert_eq!(records.starts().len(), existing_count);
        assert!(records.dropped_session_ids().is_empty());
        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();
        assert_eq!(
            records
                .starts()
                .last()
                .unwrap()
                .local_working_directory()
                .unwrap()
                .path(),
            directory.as_path()
        );
    }
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.apply_directory_pin(WorkspaceId::new(1), None, window, cx);
        })
    });
    assert_eq!(records.starts().len(), 3);
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    let starts = records
        .starts()
        .iter()
        .map(|start| {
            start
                .local_working_directory()
                .unwrap()
                .path()
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        starts,
        [std::env::temp_dir(), first, second.clone(), second]
    );
    assert!(records.dropped_session_ids().is_empty());
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active_workspace()
            .name()
            .to_owned()),
        "Default"
    );
    fs::remove_dir_all(root).unwrap();
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
            .active_workspace()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }));
}

#[gpui::test]
fn unavailable_home_should_reject_new_workspace_before_mutating_hierarchy(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    let missing_home = temporary_directory("missing-home");
    manager.update(cx, |manager, _| {
        manager.local_home_directory_path = missing_home
    });

    cx.simulate_keystrokes("cmd-n");
    cx.run_until_parked();

    assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.len(), 1);
        assert_eq!(
            manager.workspaces.active_workspace_id(),
            WorkspaceId::new(1)
        );
    });
    assert_eq!(records.session_count(), 1);
    assert!(records.dropped_session_ids().is_empty());
}

#[gpui::test]
fn unavailable_home_should_reject_final_workspace_replacement_before_closing(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    let missing_home = temporary_directory("missing-home");
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.local_home_directory_path = missing_home;
            manager.close_workspace(WorkspaceId::new(1), window, cx);
        });
    });
    cx.run_until_parked();

    assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.len(), 1);
        assert_eq!(
            manager.workspaces.active_workspace_id(),
            WorkspaceId::new(1)
        );
    });
    assert_eq!(records.session_count(), 1);
    assert!(records.dropped_session_ids().is_empty());
}

#[gpui::test]
fn unavailable_home_should_allow_closing_a_workspace_without_replacement(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-n");
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
        assert_eq!(
            manager.workspaces.active_workspace_id(),
            WorkspaceId::new(1)
        );
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
        manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id()),
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
    cx.simulate_keystrokes("cmd-n");
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
    cx.simulate_keystrokes("cmd-n");
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
            .active_workspace()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }));
}

#[gpui::test]
fn switcher_should_preserve_explicit_local_names_without_switching_to_the_match(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    for expected in ["fresh workspace"; 3] {
        open_workspace_switcher_for_creation(cx);
        click("workspace-switcher-create-local", cx);
        assert_eq!(
            manager.read_with(cx, |manager, _| manager
                .workspaces
                .active_workspace()
                .name()
                .to_owned()),
            expected
        );
    }
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        4
    );
    assert_eq!(records.starts().len(), 4);
}

#[gpui::test]
fn creation_shortcut_should_accept_local_switcher_name_instead_of_highlight(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    open_workspace_switcher_for_creation(cx);
    cx.simulate_keystrokes("down cmd-n");
    cx.run_until_parked();
    manager.read_with(cx, |manager, _| {
        assert_eq!(
            manager.workspaces.active_workspace().name(),
            "fresh workspace"
        );
        assert_eq!(manager.workspaces.len(), 2);
        assert!(manager.remote_workspace_flow.is_none());
    });
    assert_eq!(records.starts().len(), 2);
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active_workspace()
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
        assert_eq!(
            manager.workspaces.active_workspace().name(),
            "fresh workspace"
        );
        assert_eq!(manager.workspaces.len(), 2);
    });
    assert_eq!(records.starts().len(), 2);
}

#[gpui::test]
fn switcher_should_preserve_explicit_remote_names_across_destinations(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    open_workspace_switcher_for_creation(cx);
    click("workspace-switcher-create-local", cx);
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
                .active_workspace()
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
            .active_workspace()
            .payload()
            .read(cx)
            .focused_terminal_is_focused(window, cx)
    }));
}

#[gpui::test]
fn creation_shortcut_should_use_automatic_local_name_for_blank_and_cancelled_input(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    open_workspace_switcher_for_creation(cx);
    cx.simulate_keystrokes("escape cmd-n");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active_workspace()
            .name()
            .to_owned()),
        "Default (2)"
    );
    open_workspace_switcher(cx);
    cx.simulate_keystrokes("space space cmd-n");
    cx.run_until_parked();
    let id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
    manager.update(cx, |manager, _| {
        manager
            .workspaces
            .update_automatic_directory(
                id,
                crate::terminal::metadata::CurrentDirectory::Local(PathBuf::from(
                    "/project/automatic",
                )),
            )
            .unwrap();
    });
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active_workspace()
            .name()
            .to_owned()),
        "automatic"
    );
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
            .active_workspace()
            .name()
            .to_owned()),
        "Default (2)"
    );
}

#[gpui::test]
fn creation_shortcut_should_create_named_local_even_with_existing_match(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    manager.update(cx, |manager, _| {
        manager
            .workspaces
            .rename_workspace(WorkspaceId::new(1), "Alpha".into())
            .unwrap()
    });
    open_workspace_switcher(cx);
    cx.simulate_keystrokes("space A l p h a space cmd-n");
    cx.run_until_parked();
    let id = manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.len(), 2);
        manager.workspaces.active_workspace_id()
    });
    manager.update(cx, |manager, _| {
        manager
            .workspaces
            .update_automatic_directory(
                id,
                crate::terminal::metadata::CurrentDirectory::Local(PathBuf::from(
                    "/project/changed",
                )),
            )
            .unwrap();
    });
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active_workspace()
            .name()
            .to_owned()),
        "Alpha"
    );
}

#[gpui::test]
fn creation_shortcuts_should_not_escape_a_modal(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.update(|window, cx| {
        manager.update(cx, |_, cx| {
            Alert::new(
                ModalId::new("creation-shortcut-blocker"),
                "Shortcut blocker",
                "Blocking dialog",
                "Finish this dialog first.",
                vec![
                    ModalAction::new(
                        (),
                        "OK",
                        ModalActionRole::Affirmative,
                        "creation-shortcut-blocker-ok",
                    )
                    .default_action(true),
                ],
            )
            .present(window, cx, |_, _| {})
            .unwrap();
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-n cmd-shift-n");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    assert!(manager.read_with(cx, |manager, _| manager.remote_workspace_flow.is_none()));
    assert_eq!(records.starts().len(), 1);
}

#[gpui::test]
fn creation_shortcuts_should_not_escape_switcher_input_context_menu(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    open_workspace_switcher_for_creation(cx);
    let input = cx.debug_bounds("combo-box-input").unwrap().center();
    cx.simulate_mouse_down(input, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(input, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.update(|window, cx| spaceterm_ui::window_menu_is_open(window, cx)));
    cx.simulate_keystrokes("cmd-n cmd-shift-n");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| spaceterm_ui::window_menu_is_open(window, cx)));
    assert!(manager.read_with(cx, |manager, _| manager.remote_workspace_flow.is_none()));
    assert_eq!(records.starts().len(), 1);
    cx.simulate_keystrokes("escape cmd-n");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active_workspace()
            .name()
            .to_owned()),
        "fresh workspace"
    );
}

#[gpui::test]
fn workspace_switcher_check_should_follow_active_workspace_not_keyboard_highlight(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-n ctrl-1");
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
        manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id()),
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
        manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id()),
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
fn workspace_filter_icon_should_precede_editable_input_without_affecting_creation(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    open_workspace_switcher(cx);
    let icon = cx.debug_bounds("combo-box-input-leading").unwrap();
    let input = cx.debug_bounds("combo-box-input").unwrap();
    assert!(icon.right() <= input.left());
    assert!(input.size.width > px(0.0));
    cx.simulate_keystrokes("f i l t e r enter");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active_workspace()
            .name()
            .to_owned()),
        "filter"
    );
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn workspace_activation_hints_should_follow_sidebar_order_after_closing(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    for _ in 0..9 {
        cx.simulate_keystrokes("cmd-n");
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
        manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id()),
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
        cx.simulate_keystrokes("cmd-n");
    }
    cx.simulate_keystrokes("cmd-shift-e home up");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id()),
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
        manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id()),
        WorkspaceId::new(2)
    );
    cx.simulate_keystrokes("end down");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id()),
        WorkspaceId::new(25)
    );
    let last = cx.debug_bounds("workspace-row-25-active").unwrap();
    assert!(last.bottom() <= list.bottom());
    assert!(cx.update(|window, cx| {
        !manager
            .read(cx)
            .workspaces
            .active_workspace()
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
            .active_workspace()
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
fn sidebar_keyboard_rename_cancel_should_preserve_name_and_restore_sidebar_focus(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let original_name = manager.read_with(cx, |manager, _| {
        manager.workspaces.active_workspace().name().to_owned()
    });
    cx.simulate_keystrokes("cmd-shift-e shift-f10 down enter");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| manager.read(cx).sidebar.read(cx).rename_is_focused(window)));

    cx.simulate_keystrokes("cmd-a D e v escape");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .workspaces
            .active_workspace()
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
            .active_workspace()
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
            .active_workspace()
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
            .active_workspace()
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
            .active_workspace()
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

/// A repository whose Main Worktree is the test home, with a linked Worktree at each `linked`.
fn present_worktrees(cx: &mut VisualTestContext, linked: &[&Path]) {
    use crate::domain::RepositoryIdentity;
    use crate::worktrees::WorktreeSnapshot;
    use crate::worktrees::listing::{WorktreeHead, WorktreeRecord};

    let main = std::env::temp_dir();
    let record = |root: &Path, branch: &str| WorktreeRecord {
        root: root.to_path_buf(),
        head: WorktreeHead::Branch(branch.into()),
        locked: false,
        missing: false,
    };
    let snapshot = WorktreeSnapshot {
        repository: RepositoryIdentity::new(main.clone()),
        current: Some(0),
        common_directory: main.join(".git"),
        worktrees: std::iter::once(record(&main, "main"))
            .chain(linked.iter().map(|root| record(root, "feature/login")))
            .collect(),
    };
    let store =
        cx.update(|_, cx| crate::ui::worktree_store::InstalledWorktrees::store(cx).unwrap());
    store.update(cx, |store, cx| store.present(&main, Some(snapshot), cx));
    cx.run_until_parked();
}

#[gpui::test]
fn git_workspace_rows_should_disclose_worktrees_that_open_lazily(cx: &mut TestAppContext) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    cx.update(|cx| {
        crate::ui::worktree_store::testing::install(cx);
    });
    let (manager, records, cx) = workspace_manager(cx);
    let fixture = crate::terminal::testing::ShellResourcesFixture::new();
    let linked = fixture.path().join("shell-integration");
    present_worktrees(cx, &[&linked]);
    let items = |cx: &mut VisualTestContext| {
        A11yTree::read(cx)
            .with_role("TreeItem")
            .iter()
            .map(|item| {
                (
                    item["aria"]["label"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    item["aria"]["level"].as_u64().unwrap_or_default(),
                    item["aria"]["selected"] == true,
                )
            })
            .collect::<Vec<_>>()
    };
    let tabs = |cx: &mut VisualTestContext| {
        manager.read_with(cx, |manager, cx| {
            manager
                .workspaces
                .active_workspace()
                .payload()
                .read(cx)
                .worktree_tab_counts()
                .into_values()
                .collect::<Vec<_>>()
        })
    };

    let disclosed = items(cx);
    let tabs_before = tabs(cx);
    let tree = A11yTree::read(cx);
    let feature = tree
        .with_role("TreeItem")
        .into_iter()
        .find(|item| item["aria"]["label"] == "shell-integration")
        .expect("the linked Worktree's row");
    assert_eq!(
        (
            feature["aria"]["position_in_set"].as_u64(),
            feature["aria"]["size_of_set"].as_u64()
        ),
        (Some(2), Some(2))
    );
    perform(cx, feature, Action::Click);
    cx.run_until_parked();

    let main = std::env::temp_dir()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        disclosed,
        [
            ("Default".to_owned(), 1, false),
            (main.clone(), 2, true),
            ("shell-integration".to_owned(), 2, false),
        ],
        "two Worktrees disclose under the Workspace, and the Active Worktree carries the selection"
    );
    assert_eq!(
        tabs_before,
        [1],
        "the Root Tab belongs to the Main Worktree"
    );
    assert_eq!(
        items(cx)
            .into_iter()
            .map(|(label, _, selected)| (label, selected))
            .collect::<Vec<_>>(),
        [
            ("Default".to_owned(), false),
            (main, false),
            ("shell-integration".to_owned(), true),
        ]
    );
    assert_eq!(
        tabs(cx),
        [1, 1],
        "opening a Worktree with no Tabs opens its first Tab"
    );
    assert_eq!(
        records
            .starts()
            .last()
            .and_then(|start| start.local_working_directory())
            .map(|directory| directory.path().to_owned()),
        Some(linked)
    );
}

#[gpui::test]
fn a_repository_without_linked_worktrees_should_keep_a_plain_workspace_row(
    cx: &mut TestAppContext,
) {
    use spaceterm_ui::a11y_testing::A11yTree;

    cx.update(|cx| {
        crate::ui::worktree_store::testing::install(cx);
    });
    let (_manager, _records, cx) = workspace_manager(cx);
    present_worktrees(cx, &[]);

    let tree = A11yTree::read(cx);
    let rows = tree.with_role("TreeItem");
    assert_eq!(
        rows.iter()
            .map(|row| (
                row["aria"]["label"].as_str().unwrap_or_default(),
                row["aria"]["expanded"].is_null()
            ))
            .collect::<Vec<_>>(),
        [("Default", true)],
        "the Main Worktree alone discloses nothing"
    );
}

#[gpui::test]
fn worktree_rows_should_carry_the_directory_and_branch_that_a_collapsed_row_shows(
    cx: &mut TestAppContext,
) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    cx.update(|cx| {
        crate::ui::worktree_store::testing::install(cx);
    });
    let (manager, _records, cx) = workspace_manager(cx);
    let fixture = crate::terminal::testing::ShellResourcesFixture::new();
    let linked = fixture.path().join("shell-integration");
    present_worktrees(cx, &[&linked]);
    let (workspace_id, home) = manager.read_with(cx, |manager, _| {
        (
            manager.workspaces.active_workspace_id(),
            manager.local_home_directory_path.clone(),
        )
    });
    let main = std::env::temp_dir();
    let items = |cx: &mut VisualTestContext| {
        A11yTree::read(cx)
            .with_role("TreeItem")
            .iter()
            .map(|item| {
                (
                    item["aria"]["label"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    item["aria"]["description"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                )
            })
            .collect::<Vec<_>>()
    };

    let expanded = items(cx);
    let tree = A11yTree::read(cx);
    let feature = tree
        .with_role("TreeItem")
        .into_iter()
        .find(|item| item["aria"]["label"] == "shell-integration")
        .expect("the linked Worktree's row");
    perform(cx, feature, Action::Click);
    cx.run_until_parked();
    manager.update(cx, |manager, cx| {
        manager.set_worktrees_expanded(workspace_id, false);
        cx.notify();
    });
    cx.run_until_parked();

    let name = |path: &Path| path.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        expanded,
        [
            (
                "Default".to_owned(),
                format!("Repository: {}", compact_home_path(&main, &home)),
            ),
            (
                name(&main),
                format!("Branch main, {}, Main Worktree", main.display()),
            ),
            (
                "shell-integration".to_owned(),
                format!("Branch feature/login, {}, No Tabs", linked.display()),
            ),
        ],
        "an expanded Workspace row describes its repository, and each Worktree row its own \
         directory and branch"
    );
    assert_eq!(
        items(cx),
        [("Default".to_owned(), compact_home_path(&linked, &home))],
        "a collapsed Workspace row shows its Active Worktree's directory"
    );
}

#[gpui::test]
fn a_disclosure_chevron_should_trail_the_workspace_name(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::ui::worktree_store::testing::install(cx);
    });
    let (manager, _records, cx) = workspace_manager(cx);
    let fixture = crate::terminal::testing::ShellResourcesFixture::new();
    present_worktrees(cx, &[&fixture.path().join("shell-integration")]);
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
    redraw(cx);
    let id = workspace_id.get();
    let name = cx
        .debug_bounds(format!("workspace-row-name-{id}").leak())
        .expect("the Workspace name");
    let disclosure = cx
        .debug_bounds(format!("workspace-disclosure-{id}-expanded").leak())
        .expect("the disclosure chevron");
    let group = cx
        .debug_bounds(format!("workspace-group-{id}").leak())
        .expect("the Workspace group");

    assert!(
        disclosure.left() >= name.right() && disclosure.right() > group.right() - px(40.0),
        "the chevron trails the Workspace name at the row's end"
    );
}

#[gpui::test]
fn a_focused_git_workspace_row_should_collapse_and_expand_from_the_keyboard(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        crate::ui::worktree_store::testing::install(cx);
    });
    let (manager, _records, cx) = workspace_manager(cx);
    let fixture = crate::terminal::testing::ShellResourcesFixture::new();
    present_worktrees(cx, &[&fixture.path().join("shell-integration")]);
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
    let expanded = |cx: &mut VisualTestContext| {
        manager.read_with(cx, |manager, cx| {
            manager.worktree_section(workspace_id, cx).unwrap().expanded
        })
    };
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| manager.toggle_sidebar_focus(window, cx));
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
        cx.update(|window, cx| manager.read(cx).sidebar.read(cx).is_focused(window)),
        "toggling keeps the keyboard in the sidebar"
    );
}

#[gpui::test]
fn worktree_rows_should_match_a_two_line_workspace_row_height(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::ui::worktree_store::testing::install(cx);
    });
    let (manager, _records, cx) = workspace_manager(cx);
    let fixture = crate::terminal::testing::ShellResourcesFixture::new();
    present_worktrees(cx, &[&fixture.path().join("shell-integration")]);
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
    let linked = manager.read_with(cx, |manager, cx| {
        manager.worktree_section(workspace_id, cx).unwrap().groups[0].rows[1].worktree_id
    });
    redraw(cx);
    let id = workspace_id.get();
    let worktree = cx
        .debug_bounds(format!("worktree-row-{id}-{linked}-unselected").leak())
        .expect("the linked Worktree row");

    manager.update(cx, |manager, cx| {
        manager.set_worktrees_expanded(workspace_id, false);
        cx.notify();
    });
    cx.run_until_parked();
    redraw(cx);
    let workspace = cx
        .debug_bounds(format!("workspace-row-{id}-active").leak())
        .expect("the collapsed Workspace row");

    assert_eq!(worktree.size.height, workspace.size.height);
}

#[gpui::test]
fn worktree_rows_should_open_tabs_from_their_own_menu(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::ui::worktree_store::testing::install(cx);
    });
    let (manager, _records, cx) = workspace_manager(cx);
    let fixture = crate::terminal::testing::ShellResourcesFixture::new();
    present_worktrees(cx, &[&fixture.path().join("shell-integration")]);
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
    let linked = manager.read_with(cx, |manager, cx| {
        manager.worktree_section(workspace_id, cx).unwrap().groups[0].rows[1].worktree_id
    });
    let tabs = |cx: &mut VisualTestContext| {
        manager.read_with(cx, |manager, cx| {
            manager
                .workspaces
                .active_workspace()
                .payload()
                .read(cx)
                .worktree_tab_counts()
                .into_values()
                .collect::<Vec<_>>()
        })
    };
    let workspace_row: &'static str = format!("workspace-row-{}-active", workspace_id.get()).leak();
    let linked_row = |state: &str| -> &'static str {
        format!("worktree-row-{}-{linked}-{state}", workspace_id.get()).leak()
    };
    let new_tab_offered = |row: &'static str, cx: &mut VisualTestContext| {
        right_click(row, cx);
        let offered = cx.debug_bounds("workspace-menu-row-new-tab").is_some();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        offered
    };

    let expanded_offers_new_tab = new_tab_offered(workspace_row, cx);
    right_click(linked_row("unselected"), cx);
    let after_menu = tabs(cx);
    click("worktree-menu-row-new-tab", cx);
    let after_first = tabs(cx);
    right_click(linked_row("selected"), cx);
    click("worktree-menu-row-new-tab", cx);
    let after_second = tabs(cx);
    manager.update(cx, |manager, cx| {
        manager.set_worktrees_expanded(workspace_id, false);
        cx.notify();
    });
    cx.run_until_parked();
    let collapsed_offers_new_tab = new_tab_offered(workspace_row, cx);

    assert!(
        !expanded_offers_new_tab,
        "an expanded Workspace leaves New Tab to its Worktree rows"
    );
    assert_eq!(
        (after_menu, after_first, after_second),
        (vec![1], vec![1, 1], vec![1, 2]),
        "opening the menu leaves the Worktree unopened, and New Tab opens its first Tab, then \
         another"
    );
    assert!(
        collapsed_offers_new_tab,
        "a collapsed Workspace opens a Tab in its Active Worktree"
    );
}

#[gpui::test]
fn the_keyboard_should_walk_worktrees_and_open_one_only_on_return(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    cx.update(|cx| {
        crate::ui::worktree_store::testing::install(cx);
    });
    let (manager, _records, cx) = workspace_manager(cx);
    let fixture = crate::terminal::testing::ShellResourcesFixture::new();
    present_worktrees(cx, &[&fixture.path().join("shell-integration")]);
    let sidebar_focus =
        manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).focus_handle());
    cx.update(|window, cx| sidebar_focus.focus(window, cx));
    cx.run_until_parked();
    let state = |cx: &mut VisualTestContext| {
        let tree = A11yTree::read(cx);
        let items = tree.with_role("TreeItem");
        let selected = items
            .iter()
            .find(|item| item["aria"]["selected"] == true)
            .and_then(|item| item["aria"]["label"].as_str())
            .unwrap_or_default()
            .to_owned();
        let tab_counts = manager.read_with(cx, |manager, cx| {
            manager
                .workspaces
                .active_workspace()
                .payload()
                .read(cx)
                .worktree_tab_counts()
                .into_values()
                .collect::<Vec<_>>()
        });
        (items.len(), selected, tab_counts)
    };

    let mut steps = Vec::new();
    for keys in ["down", "left", "left", "right", "down down", "enter"] {
        cx.simulate_keystrokes(keys);
        cx.run_until_parked();
        steps.push(state(cx));
    }

    let step =
        |items: usize, selected: &str, tabs: &[usize]| (items, selected.to_owned(), tabs.to_vec());
    assert_eq!(
        steps,
        [
            step(3, "shell-integration", &[1]),
            step(3, "Default", &[1]),
            step(1, "Default", &[1]),
            step(3, "Default", &[1]),
            step(3, "shell-integration", &[1]),
            step(3, "shell-integration", &[1, 1]),
        ],
        "arrows stand on a Worktree with no Tabs without opening it, Left and Right collapse and \
         expand, and Return opens it"
    );
    assert!(
        cx.update(|window, _| !sidebar_focus.is_focused(window)),
        "Return moves focus to the new Tab"
    );
}

#[gpui::test]
fn leaving_a_repository_should_keep_worktrees_with_tabs_under_their_former_repository(
    cx: &mut TestAppContext,
) {
    use crate::ui::workspace_sidebar::{WorktreeGroup, WorktreeSection};

    cx.update(|cx| {
        crate::ui::worktree_store::testing::install(cx);
    });
    let (manager, _records, cx) = workspace_manager(cx);
    let fixture = crate::terminal::testing::ShellResourcesFixture::new();
    present_worktrees(cx, &[&fixture.path().join("shell-integration")]);
    let workspace_id = manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
    let section = |cx: &mut VisualTestContext| {
        manager.read_with(cx, |manager, cx| manager.worktree_section(workspace_id, cx))
    };
    let linked_id = section(cx).unwrap().groups[0].rows[1].worktree_id;
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.open_worktree(workspace_id, linked_id, true, window, cx)
        })
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            let root_tab = manager
                .workspaces
                .active_workspace()
                .payload()
                .read(cx)
                .tab_ids()[0];
            manager
                .workspaces
                .active_workspace()
                .payload()
                .update(cx, |tabs, cx| {
                    tabs.activate_tab_for_test(root_tab, window, cx)
                });
        })
    });

    let store =
        cx.update(|_, cx| crate::ui::worktree_store::InstalledWorktrees::store(cx).unwrap());
    store.update(cx, |store, cx| {
        store.present(&std::env::temp_dir(), None, cx)
    });
    cx.run_until_parked();

    let section = section(cx).expect("a Worktree with Tabs keeps the disclosure");
    let former = std::env::temp_dir()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        section,
        WorktreeSection {
            expanded: true,
            repository: None,
            groups: vec![WorktreeGroup {
                former_repository: Some(former.into()),
                rows: vec![crate::ui::workspace_sidebar::WorktreeRowViewModel {
                    active: false,
                    ..section.groups[0].rows[0].clone()
                }],
            }],
        }
    );
    assert_eq!(section.groups[0].rows[0].worktree_id, linked_id);
    assert_eq!(
        manager.read_with(cx, |manager, cx| manager
            .workspaces
            .active_workspace()
            .payload()
            .read(cx)
            .active_worktree()),
        None,
        "the Root Tab follows its Root Pane out of the repository"
    );
}

#[gpui::test]
fn workspace_rows_publish_a_tree_that_selects_on_press(cx: &mut TestAppContext) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| manager.create_local_workspace(window, cx));
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
        WorkspaceManager::new_with_adapters(
            session_factory,
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                accessibility: Rc::new(AccessKitTerminalAccessibilityAdapterFactory),
                ..workspace_adapters(test_remote_backend_factory())
            },
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
