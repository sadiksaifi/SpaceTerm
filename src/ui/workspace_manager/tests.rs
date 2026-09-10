use crate::domain::RemoteConnectionPhase;
use crate::ssh::remote_account::RemoteWorkspaceAccount;
use crate::ui::WORKSPACE_SIDEBAR_MINIMUM_WIDTH;
use crate::ui::workspace_sidebar::COLLAPSED_TOP_CHROME_MAXIMUM_WIDTH;
use gpui::MouseButton;
use spaceterm_ui::MenuLifecycleEvent;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, MouseDownEvent, MouseUpEvent, ScrollDelta,
    ScrollWheelEvent, TestAppContext, TouchPhase, VisualTestContext, point,
};
use spaceterm_ui::{
    Alert, Dialog, DialogCloseDecision, DialogInitialFocus, ModalAction, ModalActionRole, ModalId,
    ModalPresentationHandle, TextDirection,
};

use super::*;
use crate::domain::{PaneId, TabId};

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
        .try_send(SessionEvent::Exited(SessionExit::Success))
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
    click("new-remote-workspace-button", cx);
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
        .try_send(SessionEvent::Exited(SessionExit::Success))
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
fn sidebar_rows_should_keep_counts_and_pin_below_name_and_hide_machine_when_narrow(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    click("new-remote-workspace-button", cx);
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
    let counts = cx.debug_bounds("workspace-counts-2").unwrap();
    assert!(machine.left() - name.right() >= px(8.0));
    assert!(counts.top() >= name.bottom());
    assert!(path.right() + px(8.0) <= counts.left());
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
    assert_eq!(
        name.right(),
        row.right() - px(SIDEBAR_ROW_HORIZONTAL_PADDING)
    );
    let counts = cx.debug_bounds("workspace-counts-2").unwrap();
    let pin = cx.debug_bounds("workspace-row-pin-2").unwrap();
    assert!(counts.right() <= row.right());
    assert!(pin.right() <= counts.left());
    assert_eq!(row.size.height, px(SIDEBAR_ROW_HEIGHT));
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
        (visible_icon.unicode(), visible_label),
        (IconName::PanelLeft.unicode(), "Close Sidebar")
    );
    let (hidden_icon, hidden_label) = sidebar_toggle_presentation(false);
    assert_eq!(
        (hidden_icon.unicode(), hidden_label),
        (IconName::PanelRight.unicode(), "Open Sidebar")
    );
}

#[test]
fn remote_connection_status_should_only_replace_the_path_while_unhealthy() {
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
use crate::platform::ssh_askpass::{AskPassPromptKind, AskPassRequest, AskPassResult};
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::ssh::command::{SshCommandContext, ValidatedRemoteShellCommand};
use crate::ssh::destination::SshHostAlias;
use crate::ssh::host_config::HostDiscovery;
use crate::ssh::managed_hosts::ManagedSshHost;
use crate::terminal::testing::{
    RecordedSessionCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
};
use crate::terminal::{SessionEvent, SessionExit};
use crate::ui::remote_directory_picker::{
    RemoteDirectoryExactPathState, RemoteDirectoryListing, RemoteDirectoryProvider,
    RemoteDirectoryProviderError,
};
use crate::ui::remote_workspace_flow::{
    RemoteWorkspaceAliasPin, RemoteWorkspaceAliasPinError, RemoteWorkspaceConnectContext,
    RemoteWorkspaceFlowBackendError, RemoteWorkspaceFlowStage, RemoteWorkspaceSessionOwner,
};
use crate::ui::ssh_askpass_dialog::GpuiAskPassPresenter;
use crate::ui::ssh_host_form::ManagedHostFormBackendError;

#[derive(Default)]
struct TestRemoteWorkspaceFlowBackend {
    connections: Mutex<
        VecDeque<
            gpui::Task<Result<RemoteWorkspaceConnectedSession, RemoteWorkspaceFlowBackendError>>,
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
    ) -> gpui::Task<Result<RemoteWorkspaceConnectedSession, RemoteWorkspaceFlowBackendError>> {
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
        gpui::Task::ready(
            RemoteWorkspaceAccount::new(
                "tester".to_owned(),
                crate::domain::RemoteDirectoryIdentity::new("/home/tester".to_owned()).unwrap(),
                "/bin/zsh".to_owned(),
            )
            .map_err(|_| RemoteDirectoryProviderError::InvalidResponse),
        )
    }

    fn list_directories(
        &self,
        _: RemoteDirectory,
    ) -> gpui::Task<Result<RemoteDirectoryListing, RemoteDirectoryProviderError>> {
        gpui::Task::ready(Ok(RemoteDirectoryListing::new(Vec::new())))
    }

    fn probe_exact_path(
        &self,
        _: RemoteDirectory,
    ) -> gpui::Task<Result<RemoteDirectoryExactPathState, RemoteDirectoryProviderError>> {
        gpui::Task::ready(Ok(RemoteDirectoryExactPathState::ReadableDirectory))
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
    ) -> gpui::Task<Result<RemoteDirectoryListing, RemoteDirectoryProviderError>> {
        gpui::Task::ready(Err(RemoteDirectoryProviderError::Other))
    }

    fn probe_exact_path(
        &self,
        _: RemoteDirectory,
    ) -> gpui::Task<Result<RemoteDirectoryExactPathState, RemoteDirectoryProviderError>> {
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
    revalidation_tasks:
        Mutex<VecDeque<gpui::Task<Result<(), crate::terminal::RemoteChannelRevalidationError>>>>,
    available: Arc<AtomicBool>,
    destination: crate::domain::SshDestination,
}

impl crate::terminal::RemoteTerminalChannelProvider for TestRemoteChannelProvider {
    fn is_ready(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }

    fn revalidate(
        &self,
        _: RemoteDirectory,
        _: Option<crate::domain::RemoteDirectoryIdentity>,
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
        _: &RemoteDirectory,
    ) -> Result<
        crate::ssh::command::PreparedSshPaneChannelCommand,
        crate::terminal::RemoteChannelUnavailable,
    > {
        self.preparations.fetch_add(1, Ordering::AcqRel);
        if !self.available.load(Ordering::Acquire) {
            return Err(crate::terminal::RemoteChannelUnavailable);
        }
        Ok(SshCommandContext::new(
            crate::ssh::command::OpenSshExecutable::for_test(),
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

    fn bind_terminal_channels(
        &self,
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
    revalidation: gpui::Task<Result<(), crate::terminal::RemoteChannelRevalidationError>>,
    provider: Arc<dyn RemoteDirectoryProvider>,
) -> RemoteCompletionFixture {
    let destination = crate::domain::SshDestination::new(destination.to_owned()).unwrap();
    let directory = RemoteDirectory::new(directory.to_owned()).unwrap();
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
            _lifecycle_senders: vec![runtime_lifecycle_sender.clone(), session_lifecycle_sender],
            alias: None,
            alias_pin_error: false,
        }),
        provider,
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
    let directory = RemoteDirectory::new("~/src".to_owned()).unwrap();
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
            _lifecycle_senders: vec![runtime_lifecycle_sender.clone(), session_lifecycle_sender],
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
                local_filesystem: LocalFilesystemAuthority::testing(),
                key_input: Rc::new(GpuiTerminalKeyInputAdapterFactory::default()),
                accessibility: Rc::new(crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default()),
                native_services,
                lifecycle: PaneLifecycleDependencies::testing(),
                directory_selection: Rc::new(GpuiDirectorySelection),
            permission_recovery: None,
                window_drag: Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
                remote_workspace: test_remote_backend_factory(),
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
                local_filesystem: LocalFilesystemAuthority::testing(),
                key_input: Rc::new(GpuiTerminalKeyInputAdapterFactory::default()),
                accessibility: factory.clone(),
                native_services: crate::terminal::native_services::testing::adapters(),
                lifecycle: PaneLifecycleDependencies::testing(),
                directory_selection: Rc::new(GpuiDirectorySelection),
                permission_recovery: None,
                window_drag: Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
                remote_workspace: test_remote_backend_factory(),
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
            .any(|(presented, _)| !presented)
    );
    assert!(
        records[3]
            .borrow()
            .hierarchy
            .iter()
            .any(|(presented, order)| *presented && *order == 0)
    );
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
        WorkspaceManager::new_with_remote_workspace_backend_factory(
            session_factory,
            std::env::temp_dir(),
            test_remote_backend_factory(),
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
    &mut VisualTestContext,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    cx.update(|cx| {
        crate::app::init(
            cx,
            Rc::new(
                crate::platform::application_menu::testing::RecordingApplicationMenuAdapter::default(),
            ),
        );
    });
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records.clone()).with_fallback_title("zsh"));
    let (manager, cx) = cx.add_window_view(|window, cx| {
        WorkspaceManager::new_with_remote_workspace_backend_factory(
            session_factory,
            std::env::temp_dir(),
            test_remote_backend_factory(),
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
        WorkspaceManager::new_with_remote_workspace_backend_factory(
            session_factory,
            std::env::temp_dir(),
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
    provider: Arc<dyn RemoteDirectoryProvider>,
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
    provider: Arc<dyn RemoteDirectoryProvider>,
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
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records).with_fallback_title("zsh"));
    let platform = Rc::new(RecordingOperatingSystemWindowDragPlatform::default());
    let injected_platform = Rc::clone(&platform);
    let (manager, cx) = cx.add_window_view(move |window, cx| {
        WorkspaceManager::new_with_operating_system_window_drag_platform(
            session_factory,
            std::env::temp_dir(),
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
    let directory_selection_fallback: Rc<dyn SystemDirectorySelection> =
        Rc::new(ScriptedDirectorySelection::new(selections));
    let (manager, cx) = cx.add_window_view(|window, cx| {
        WorkspaceManager::new_with_directory_selection_fallback(
            session_factory,
            std::env::temp_dir(),
            directory_selection_fallback,
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

fn open_directory_picker(manager: &Entity<WorkspaceManager>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.open_pin_directory_picker(manager.workspaces.active_workspace_id(), window, cx);
        })
    });
    cx.run_until_parked();
}

fn open_workspace_switcher(cx: &mut VisualTestContext) {
    cx.simulate_keystrokes("cmd-k");
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
                completion.terminal_channels(),
            );
            let prepared = terminal_factory.prepare_child_launch().unwrap();
            manager
                .try_create_remote_workspace(completion, terminal_factory, prepared, window, cx)
                .unwrap_or_else(|_| panic!("direct Remote Workspace installation failed"));
        });
    });
    cx.run_until_parked();
}

fn choose_with_directory_selection_fallback(
    manager: &Entity<WorkspaceManager>,
    cx: &mut VisualTestContext,
) {
    open_directory_picker(manager, cx);
    click("directory-picker-directory-selection", cx);
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
        WorkspaceManager::new_with_remote_workspace_backend_factory(
            session_factory,
            std::env::temp_dir(),
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
    cx.update(|window, cx| presentation.dismiss(window, cx).unwrap());
    cx.run_until_parked();
    assert!(!cx.update(|window, cx| window_modal_is_open(window, cx)));
}

#[gpui::test]
fn directory_picker_reentry_should_not_steal_focus_from_an_active_modal(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.present_directory_picker(window, cx)
        });
    });
    cx.run_until_parked();
    let presentation = present_test_alert(&manager, "directory-picker-modal-priority", cx);
    cx.run_until_parked();

    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.open_pin_directory_picker(manager.workspaces.active_workspace_id(), window, cx);
            manager.open_pin_directory_picker(manager.workspaces.active_workspace_id(), window, cx);
        });
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let focus_contained = cx.update(|window, cx| {
        let manager = manager.read(cx);
        window_modal_is_open(window, cx)
            && !manager
                .transient
                .picker
                .read(cx)
                .path_input_is_focused(window, cx)
    });

    assert!(focus_contained);
    assert!(cx.debug_bounds("modal-surface-1").is_some());

    cx.update(|window, cx| {
        presentation
            .dismiss(window, cx)
            .expect("directory-picker modal should dismiss")
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("command-palette-panel").is_some());
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .transient
            .picker
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
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let first_records = TestTerminalSessionRecords::default();
    let second_records = TestTerminalSessionRecords::default();
    let first_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(first_records.clone()).with_fallback_title("zsh"));
    let second_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(second_records.clone()).with_fallback_title("zsh"));
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
fn cancelled_directory_selection_fallback_should_leave_hierarchy_unchanged(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager_with_picker([Ok(None)], cx);

    choose_with_directory_selection_fallback(&manager, cx);

    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        1
    );
    assert_eq!(records.starts().len(), 1);
}

#[gpui::test]
fn titlebar_button_and_command_k_should_each_block_terminal_input(cx: &mut TestAppContext) {
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
                manager.transient.picker.read(cx).is_open(),
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
        (false, false, None, true, 2, 2)
    );
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
        WorkspaceManager::new_with_remote_workspace_backend_factory(
            session_factory,
            std::env::temp_dir(),
            factory,
            window,
            cx,
        )
    });
    cx.update(|window, cx| manager.update(cx, |manager, cx| manager.focus(window, cx)));
    cx.run_until_parked();
    open_workspace_switcher_for_creation(cx);

    click("workspace-switcher-create-remote", cx);
    cx.simulate_keystrokes("cmd-shift-n");
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
    click("new-remote-workspace-button", cx);
    assert_eq!(create_calls.load(Ordering::Acquire), 0);
    assert!(manager.read_with(cx, |manager, _| manager.remote_workspace_flow.is_none()));
    assert_eq!(records.starts().len(), 1);
}

#[gpui::test]
fn pin_directory_selection_should_present_the_picker_without_the_panel(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);

    open_directory_picker(&manager, cx);

    assert_eq!(
        cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.transient.picker.read(cx).is_open(),
                window_combo_box_is_open(window, cx),
                manager.terminal_focus_blocker(window, cx),
            )
        }),
        (true, false, Some(TerminalFocusBlocker::Modal))
    );
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
    let (session, closes, _, _, _) = reconnect_session_with_provider("work", provider);
    let backend =
        TestRemoteWorkspaceFlowBackend::with_connections([gpui::Task::ready(Ok(session))]);
    let (manager, _, cx) = workspace_manager_with_remote_backend(backend, cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let flow = open_remote_workspace_flow(&manager, cx);
    cx.update(|window, cx| {
        flow.update(cx, |flow, cx| {
            flow.select_destination_for_test(
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
        WorkspaceManager::new_with_remote_workspace_backend_factory(
            session_factory,
            std::env::temp_dir(),
            factory,
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
    let (workspace_id, tab_manager, pane_host) = manager.read_with(cx, |manager, cx| {
        let workspace = manager.workspaces.active_workspace();
        let tab_manager = workspace.payload().clone();
        let pane_host = tab_manager.read(cx).active_pane_host();
        (workspace.id(), tab_manager, pane_host)
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
        tab_manager.read_with(cx, |manager, _| manager.active_pane_host().entity_id()),
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
    let status_selector: &'static str =
        Box::leak(format!("workspace-row-remote-status-{}", workspace_id.get()).into_boxed_str());
    assert!(cx.debug_bounds(status_selector).is_some());

    cx.simulate_keystrokes("cmd-b");
    redraw(cx);
    assert!(
        cx.debug_bounds("workspace-chip-remote-status").is_none(),
        "the collapsed chip conveys connection state through its globe and tooltip"
    );
    cx.simulate_keystrokes("cmd-b");
    redraw(cx);

    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    assert_eq!(records.starts().len(), starts_before_disconnect);
    assert_eq!(pane_host.read_with(cx, |host, _| host.pane_count()), 1);
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
    let (workspace_id, tab_manager, pane_host, starts_before_reconnect) =
        manager.read_with(cx, |manager, cx| {
            let workspace = manager.workspaces.active_workspace();
            let tab_manager = workspace.payload().clone();
            let pane_host = tab_manager.read(cx).active_pane_host();
            (
                workspace.id(),
                tab_manager,
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
        tab_manager.entity_id()
    );
    assert_eq!(
        tab_manager.read_with(cx, |manager, _| manager.active_pane_host().entity_id()),
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
            "The selected remote path now resolves to a different directory. Reopen the Remote Workspace to review it.".to_owned()
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
    let provider: Arc<dyn RemoteDirectoryProvider> = Arc::new(BlockingRemoteProvider {
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
    let provider: Arc<dyn RemoteDirectoryProvider> = Arc::new(BlockingRemoteProvider {
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
    let provider: Arc<dyn RemoteDirectoryProvider> = Arc::new(TestRemoteProvider::connected(
        crate::domain::RemoteDirectoryIdentity::new("/home/tester/src".to_owned()).unwrap(),
    ));
    let (session, closes, _, revalidations, _) = reconnect_session_with_provider_and_revalidation(
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
        assert_eq!(closes.load(Ordering::Acquire), 1);
        assert_eq!(
            flow.read_with(cx, |flow, _| flow.stage()),
            RemoteWorkspaceFlowStage::ConnectionError
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
    let (completion, closes, _, _) = remote_completion("work", "~/src", "/home/tester/src", true);
    let flow = open_remote_workspace_flow(&manager, cx);
    emit_remote_workspace_completion(&flow, completion, cx);

    manager.update(cx, |manager, _| manager.close_remote_runtimes());
    manager.update(cx, |manager, _| manager.close_remote_runtimes());

    assert_eq!(closes.load(Ordering::Acquire), 1);
}

#[gpui::test]
fn unavailable_initial_remote_channel_should_close_completion_and_offer_retry(
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
fn dismissing_pin_picker_should_restore_terminal_without_opening_creation_panel(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
    open_directory_picker(&manager, cx);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.transient.picker.read(cx).is_open(),
                window_combo_box_is_open(window, cx),
                manager.terminal_focus_blocker(window, cx),
            )
        }),
        (false, false, None)
    );
}

#[gpui::test]
fn failed_pin_keeps_picker_focus_and_escape_restores_terminal(cx: &mut TestAppContext) {
    let project = temporary_directory("activation-panel-origin");
    fs::create_dir_all(&project).unwrap();
    let (manager, records, cx) = workspace_manager_with_picker([Ok(Some(project.clone()))], cx);
    open_directory_picker(&manager, cx);
    // The picker retains its valid background authority; activation independently fails.
    manager.update(cx, |manager, _| {
        manager.local_filesystem = LocalFilesystemAuthority::testing_with_failure(
            crate::platform::local_filesystem::LocalFilesystemError::Capacity,
        );
    });
    click("directory-picker-directory-selection", cx);
    cx.run_until_parked();
    assert_eq!(records.starts().len(), 1);
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        1
    );
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .transient
            .picker
            .read(cx)
            .path_input_is_focused(window, cx)
    }));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.transient.picker.read(cx).is_open(),
                window_combo_box_is_open(window, cx),
                manager.terminal_focus_blocker(window, cx),
            )
        }),
        (false, false, None)
    );
    fs::remove_dir_all(project).unwrap();
}

#[gpui::test]
fn escape_should_close_a_picker_that_no_panel_opened(cx: &mut TestAppContext) {
    let (manager, _, cx) = workspace_manager(cx);

    open_directory_picker(&manager, cx);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert_eq!(
        cx.update(|window, cx| {
            let manager = manager.read(cx);
            (
                manager.transient.picker.read(cx).is_open(),
                window_combo_box_is_open(window, cx),
                manager.terminal_focus_blocker(window, cx),
            )
        }),
        (false, false, None)
    );
}

#[gpui::test]
fn directory_picker_should_block_parent_shortcuts_and_keep_path_focus(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-n");
    open_directory_picker(&manager, cx);
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

    cx.simulate_keystrokes("cmd-n cmd-shift-n");
    assert!(manager.read_with(cx, |manager, _| manager.remote_workspace_flow.is_none()));
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        baseline.0
    );

    cx.simulate_keystrokes("cmd-k");
    let focus_state = cx.update(|window, cx| {
        let manager = manager.read(cx);
        (
            window_combo_box_is_open(window, cx),
            manager
                .transient
                .picker
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
fn unavailable_pinned_directory_should_block_children_and_recover_when_restored(
    cx: &mut TestAppContext,
) {
    let root = temporary_directory("availability");
    let project = root.join("project");
    let parked = root.join("parked");
    fs::create_dir_all(&project).unwrap();
    let (manager, records, cx) = workspace_manager_with_picker([Ok(Some(project.clone()))], cx);
    choose_with_directory_selection_fallback(&manager, cx);
    assert_eq!(records.starts().len(), 1);
    assert!(!manager.read_with(cx, |manager, cx| {
        manager.transient.picker.read(cx).is_open()
    }));

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

    fs::rename(&parked, &project).unwrap();
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    assert_eq!(records.starts().len(), 2);
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
fn unusable_directory_selection_should_not_pin_the_workspace(cx: &mut TestAppContext) {
    let missing = temporary_directory("missing");
    let (manager, records, cx) = workspace_manager_with_picker([Ok(Some(missing))], cx);

    choose_with_directory_selection_fallback(&manager, cx);

    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.len()),
        1
    );
    assert_eq!(records.starts().len(), 1);
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
    assert!(manager.read_with(cx, |manager, cx| {
        manager.sidebar.read(cx).menu_target().is_none()
    }));

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
fn open_workspace_switcher_should_remove_a_workspace_after_its_final_session_exits(
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

    click("workspace-switcher", cx);
    cx.simulate_keystrokes("a l p h a");
    cx.run_until_parked();
    assert!(cx.debug_bounds("workspace-switcher-result-1").is_some());

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
    let remote = cx.debug_bounds("new-remote-workspace-button").unwrap();
    let local = cx.debug_bounds("new-local-workspace-button").unwrap();
    assert!(remote.left() < local.left());
    assert_eq!(remote.center().y, local.center().y);
    assert!(remote.left() >= button.left());
    assert!(local.right() <= button.right());
    for (icon_selector, target) in [
        ("new-remote-workspace-icon", remote),
        ("new-local-workspace-icon", local),
    ] {
        let icon = cx.debug_bounds(icon_selector).unwrap();
        assert_eq!(icon.size, gpui::size(px(18.0), px(18.0)));
        assert_eq!(target.size, gpui::size(px(28.0), px(28.0)));
        assert_eq!(icon.center(), target.center());
    }
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

fn press_return(cx: &mut VisualTestContext) {
    let keystroke = Keystroke::parse("enter").unwrap_or_default();
    cx.simulate_event(KeyDownEvent {
        keystroke: keystroke.clone(),
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
            .any(|call| call.session_id == 1
                && matches!(call.command, RecordedSessionCommand::Key(_)))
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
        .expect("the focused split Pane session was not started")
        .try_send(SessionEvent::Screen(Arc::new(screen)))
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
        .expect("the focused split Pane session was not started")
        .try_send(SessionEvent::Exited(SessionExit::Success))
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
fn native_window_and_application_close_should_share_the_pending_coordinator(
    cx: &mut TestAppContext,
) {
    let (manager, _, cx) = workspace_manager(cx);
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
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.request_application_quit(window, cx)
        });
    });
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .close_confirmation
            .pending()
            .map(|pending| pending.target)),
        Some(CloseTarget::Window)
    );
    click("modal-action-close-confirmation-cancel", cx);

    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.request_application_quit(window, cx)
        });
    });
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .close_confirmation
            .pending()
            .map(|pending| pending.target)),
        Some(CloseTarget::Application)
    );
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
fn command_q_and_quit_action_should_cancel_safely_and_confirm_once(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager_with_application_actions(cx);
    redraw(cx);

    cx.simulate_keystrokes("cmd-q");
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .close_confirmation
            .pending()
            .map(|pending| pending.target)),
        Some(CloseTarget::Application)
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(manager.read_with(cx, |manager, _| {
        manager.close_confirmation.pending().is_none()
    }));
    assert!(records.dropped_session_ids().is_empty());

    cx.dispatch_action(crate::app::QuitApplication);
    cx.run_until_parked();
    assert_eq!(
        manager.read_with(cx, |manager, _| manager
            .close_confirmation
            .pending()
            .map(|pending| pending.target)),
        Some(CloseTarget::Application)
    );
    cx.simulate_keystrokes("cmd-.");
    cx.run_until_parked();
    assert!(manager.read_with(cx, |manager, _| {
        manager.close_confirmation.pending().is_none()
    }));
    assert!(records.dropped_session_ids().is_empty());

    cx.dispatch_action(crate::app::QuitApplication);
    cx.run_until_parked();
    click("modal-action-close-confirmation-confirm", cx);

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

    assert_eq!(platform.counts(), (1, 1, 1, 0));
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
fn sidebar_should_render_one_divider_across_top_chrome_and_body(cx: &mut TestAppContext) {
    let (_manager, _records, cx) = workspace_manager(cx);

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
            gpui::size(px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH), px(TOP_CHROME_HEIGHT)),
            px(0.0),
            px(TOP_CHROME_HEIGHT),
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
        gpui::size(px(16.0), px(TOP_CHROME_HEIGHT))
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
        (px(WORKSPACE_SIDEBAR_DEFAULT_WIDTH + 80.0), (0, 0, 0, 0),)
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
        collapsed_top_chrome_width(
            manager.read(cx).workspaces.active_workspace().name(),
            window,
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
    let requested_width = chrome.size.width + px(40.0);

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
    assert_eq!(chooser.right(), toggle.left());

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
    assert_eq!(platform.counts(), (0, 0, 0, 0));
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
    assert!(chip.right() <= chooser.left());

    click("workspace-switcher", cx);

    assert!(!manager.read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().visible));
    assert!(cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    let panel = cx
        .debug_bounds("combo-box-panel")
        .expect("Workspace chooser popup");
    assert_eq!(panel.top(), chooser.bottom() + px(4.0));
}

#[gpui::test]
fn sidebar_new_workspace_button_should_create_local_immediately(cx: &mut TestAppContext) {
    let (manager, records, cx) = workspace_manager(cx);
    click("new-local-workspace-button", cx);
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

    let sidebar = cx
        .debug_bounds("workspace-sidebar")
        .expect("the Workspace sidebar should render");
    let panel = cx
        .debug_bounds("combo-box-panel")
        .expect("the New Workspace ComboBox panel should render");
    assert_eq!(
        panel.size.width,
        sidebar.size.width - px(SIDEBAR_ROW_HORIZONTAL_PADDING * 2.0)
    );
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
        assert_eq!(row.size.height, px(30.0));
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
    let (manager, records, cx) = workspace_manager_with_picker([Ok(Some(directory.clone()))], cx);
    assert!(cx.debug_bounds("workspace-row-pin-1").is_none());
    choose_with_directory_selection_fallback(&manager, cx);
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
    let (manager, _, cx) = workspace_manager(cx);

    click("toggle-sidebar-button", cx);
    cx.simulate_keystrokes("cmd-n");
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
        chip.right() <= chooser.left(),
        "the chip overlapped the Workspace chooser: {chip:?} {chooser:?}"
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
    assert_eq!(
        (
            chrome.size.width,
            spacer.size.width,
            tab_bar.origin.x,
            divider.center().x,
        ),
        (
            px(COLLAPSED_TOP_CHROME_MAXIMUM_WIDTH),
            px(COLLAPSED_TOP_CHROME_MAXIMUM_WIDTH),
            root.origin.x + px(COLLAPSED_TOP_CHROME_MAXIMUM_WIDTH),
            root.origin.x + px(COLLAPSED_TOP_CHROME_MAXIMUM_WIDTH),
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
    assert!(chrome.size.width < px(COLLAPSED_TOP_CHROME_MAXIMUM_WIDTH));
    assert_eq!(
        (spacer.size.width, tab_bar.origin.x),
        (chrome.size.width, root.origin.x + chrome.size.width)
    );
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
            .shape_line("Default".into(), px(WORKSPACE_CHIP_TEXT_SIZE), &[run], None)
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
    assert!(label.right() <= chooser.left());
    assert!(chooser.right() <= toggle.left() && toggle.right() <= chrome.right());
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
    let (manager, records, cx) = workspace_manager(cx);
    let inactive_sender = records
        .event_sender(1)
        .expect("the initial Workspace terminal session must have started");
    cx.simulate_keystrokes("cmd-n");
    cx.run_until_parked();
    click("toggle-sidebar-button", cx);

    inactive_sender
        .try_send(SessionEvent::Exited(SessionExit::Success))
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
fn sidebar_footer_should_have_no_top_divider(cx: &mut TestAppContext) {
    let (_, _, cx) = workspace_manager(cx);
    assert!(cx.debug_bounds("workspace-sidebar-footer").is_some());
    assert!(
        cx.debug_bounds("workspace-sidebar-footer-divider")
            .is_none()
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
        collapsed_top_chrome_width(
            manager.read(cx).workspaces.active_workspace().name(),
            window,
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

#[gpui::test]
fn command_n_and_local_choice_should_create_and_activate_a_home_workspace(cx: &mut TestAppContext) {
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
            manager.sidebar.read(cx).menu_target(),
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
            manager.sidebar.read(cx).menu_target(),
            manager.sidebar.read(cx).is_focused(window),
            manager.terminal_focus_blocker(window, cx),
        )
    });
    assert_eq!(dismissed, (None, true, Some(TerminalFocusBlocker::Sidebar)));
}

#[gpui::test]
fn workspace_menu_closure_recomputes_remaining_focus_owners(cx: &mut TestAppContext) {
    let (manager, _records, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    for resizing in [true, false] {
        cx.update(|window, cx| {
            manager.update(cx, |manager, cx| {
                let workspace_id = manager.workspaces.active_workspace_id();
                manager.sidebar.update(cx, |sidebar, cx| {
                    sidebar.set_resizing_for_test(resizing);
                    sidebar.handle_menu_lifecycle(
                        workspace_id,
                        MenuLifecycleEvent::Opened,
                        window,
                        cx,
                    );
                    sidebar.handle_menu_lifecycle(
                        workspace_id,
                        MenuLifecycleEvent::Closed(spaceterm_ui::MenuCloseReason::Escape),
                        window,
                        cx,
                    );
                });
            });
        });
        cx.run_until_parked();
        assert_eq!(
            cx.update(|window, cx| manager
                .read(cx)
                .workspaces
                .active_workspace()
                .payload()
                .read(cx)
                .focused_terminal_has_input_focus(window, cx)),
            !resizing
        );
    }
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
    cx.update(|window, _| sidebar_focus.focus(window));
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
#[cfg(all(test, target_os = "macos", feature = "macos-native-tests"))]
mod macos_adapter_tests {
    include!("../../platform/macos_adapter_tests/workspace_manager.rs");
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
fn remote_pin_change_and_unpin_should_preserve_sessions_and_source_directory(
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
fn remote_directory_picker_should_pin_its_target_and_keep_the_connection(cx: &mut TestAppContext) {
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
            manager.open_pin_directory_picker(WorkspaceId::new(2), window, cx)
        })
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-shift-n");
    cx.run_until_parked();
    assert!(manager.read_with(cx, |manager, _| manager.remote_workspace_flow.is_none()));
    click("remote-directory-picker-confirm", cx);
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

    assert!(cx.has_pending_prompt());
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

    assert!(cx.has_pending_prompt());
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

    assert!(!cx.has_pending_prompt());
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
        assert!(matched.bottom() <= local.top());
        assert!(local.bottom() <= remote.top());
    }
}

#[gpui::test]
fn command_p_should_no_longer_open_workspace_search(cx: &mut TestAppContext) {
    let (_, _, cx) = workspace_manager(cx);
    cx.simulate_keystrokes("cmd-p");
    cx.run_until_parked();
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(cx.debug_bounds("command-palette-panel").is_none());
}

#[gpui::test]
fn sidebar_remote_creation_should_open_host_selection_and_restore_focus_on_cancel(
    cx: &mut TestAppContext,
) {
    let (manager, records, cx) = workspace_manager(cx);
    click("new-remote-workspace-button", cx);
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
        click("workspace-switcher-create-remote", cx);
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
    let marker = cx.debug_bounds("workspace-switcher-active-marker").unwrap();
    let first = cx.debug_bounds("workspace-switcher-result-1").unwrap();
    assert!(first.contains(&marker.center()));
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id()),
        WorkspaceId::new(1)
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    open_workspace_switcher(cx);
    let marker = cx.debug_bounds("workspace-switcher-active-marker").unwrap();
    let second = cx.debug_bounds("workspace-switcher-result-2").unwrap();
    assert!(second.contains(&marker.center()));
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.workspaces.active_workspace_id()),
        WorkspaceId::new(2)
    );
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
    manager.read_with(cx, |manager, cx| {
        let items = manager.workspace_switcher_items(cx);
        for (index, item) in items.iter().take(9).enumerate() {
            assert_eq!(
                item.shortcut_text(),
                Some(format!("Ctrl+{}", index + 1).as_str())
            );
        }
        assert_eq!(items[9].shortcut_text(), None);
    });
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.close_workspace(WorkspaceId::new(1), window, cx)
        })
    });
    cx.run_until_parked();
    manager.read_with(cx, |manager, cx| {
        let items = manager.workspace_switcher_items(cx);
        assert_eq!(
            items[0].id(),
            &WorkspaceSwitcherChoice::Workspace(WorkspaceId::new(2))
        );
        assert_eq!(items[0].shortcut_text(), Some("Ctrl+1"));
        assert_eq!(items[8].shortcut_text(), Some("Ctrl+9"));
    });
}

#[gpui::test]
fn workspace_creation_and_switcher_buttons_should_show_hover_tooltips(cx: &mut TestAppContext) {
    let (_, _, cx) = workspace_manager(cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    for (button, tooltip) in [
        (
            "new-remote-workspace-button",
            "new-remote-workspace-tooltip",
        ),
        ("new-local-workspace-button", "new-local-workspace-tooltip"),
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
            .is_some()
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
            .any(|call| matches!(call.command, RecordedSessionCommand::Key(_)))
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
    assert!(cx.update(|window, cx| {
        manager
            .read(cx)
            .workspaces
            .active_workspace()
            .payload()
            .read(cx)
            .focused_terminal_has_input_focus(window, cx)
    }));
    cx.simulate_keystrokes("x");
    assert!(
        records
            .commands()
            .iter()
            .any(|call| matches!(call.command, RecordedSessionCommand::Key(_)))
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
            .any(|call| matches!(call.command, RecordedSessionCommand::Key(_)))
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
            .focused_terminal_has_input_focus(window, cx)
    }));
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
            .is_some()
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
