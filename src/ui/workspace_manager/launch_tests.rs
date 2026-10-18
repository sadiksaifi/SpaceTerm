//! Normal launch of the account-local server in the Workspace Window.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use spaceterm_server::hierarchy::{Axis, Inventory, Pane, Tab, Workspace};

use super::*;
use crate::local_server::ServerError;
use crate::ui::shared_workspaces::testing::{ScriptedEdits, ScriptedPanes};
use crate::ui::shared_workspaces::{SharedEditSource, SharedPaneSessions};
use crate::ui::workspace_manager::launch::{
    LAUNCH_AUTHORIZE_SELECTOR, LAUNCH_RETRY_SELECTOR, LAUNCH_SELECTOR, LAUNCH_START_SELECTOR,
    LaunchOutcome, SharedServerConnection, SharedServerConnector,
};
use crate::ui::workspace_manager::launch_testing::ScriptedLaunch;

const WORKSPACE: [u8; 32] = [31; 32];
const TAB: [u8; 32] = [32; 32];
const PANE: [u8; 32] = [33; 32];

fn inventory() -> Inventory {
    Inventory {
        repositories: None,
        revision: 1,
        close: None,
        workspaces: vec![Workspace {
            id: WORKSPACE,
            name: "Default".to_owned(),
            name_version: 0,
            pin_version: 0,
            pinned_directory: None,
            root_tab: TAB,
            tabs: vec![Tab {
                worktree: None,
                worktree_membership_fixed: false,
                worktree_repository: None,
                id: TAB,
                root_pane: PANE,
                panes: vec![Pane {
                    id: PANE,
                    source: None,
                    axis: Axis::Right,
                    starting_directory: PathBuf::from("/server/home"),
                    confirmed_directory: None,
                    creator: None,
                }],
            }],
        }],
    }
}

/// Connects every ready server to one scripted inventory and its scripted Panes.
struct ScriptedConnector {
    edits: Rc<ScriptedEdits>,
    panes: Rc<ScriptedPanes>,
}

impl SharedServerConnector for ScriptedConnector {
    fn connect(
        &self,
        _: crate::domain::ValidatedLocalDirectory,
        _: LocalFilesystemAuthority,
        panes: &PaneConstruction,
        _: &mut App,
    ) -> Option<SharedServerConnection> {
        Some(SharedServerConnection {
            edits: SharedEditSource::new(Rc::clone(&self.edits) as Rc<_>),
            sessions: SharedPaneSessions::with_opener(Rc::clone(&self.panes) as Rc<_>, panes),
            retained: None,
        })
    }
}

struct LaunchHarness {
    manager: Entity<WorkspaceManager>,
    launch: Arc<ScriptedLaunch>,
    edits: Rc<ScriptedEdits>,
    panes: Rc<ScriptedPanes>,
}

impl LaunchHarness {
    fn shared_rows(&self, cx: &mut VisualTestContext) -> usize {
        self.manager.read_with(cx, |manager, _| {
            manager
                .workspaces
                .iter()
                .filter(|workspace| manager.is_shared_workspace(workspace.id()))
                .count()
        })
    }

    fn workspace_count(&self, cx: &mut VisualTestContext) -> usize {
        self.manager
            .read_with(cx, |manager, _| manager.workspaces.len())
    }

    fn click(&self, selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} was not rendered"));
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    fn launch_title(&self, cx: &mut VisualTestContext) -> Option<&'static str> {
        self.manager
            .read_with(cx, |manager, _| manager.launch_presentation())
            .map(|presentation| presentation.title)
    }
}

/// A Workspace Window composed with a scripted account server, before launch answers.
fn launch_harness(
    answers: Vec<Result<LaunchOutcome, ServerError>>,
    cx: &mut TestAppContext,
) -> (LaunchHarness, &mut VisualTestContext) {
    launch_harness_choosing(answers, [], cx)
}

/// A launch harness whose System Directory Selection answers `selections` in order.
fn launch_harness_choosing(
    answers: Vec<Result<LaunchOutcome, ServerError>>,
    selections: impl IntoIterator<
        Item = Result<Option<PathBuf>, crate::directory_selection::DirectoryChooserError>,
    >,
    cx: &mut TestAppContext,
) -> (LaunchHarness, &mut VisualTestContext) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let launch = Arc::new(ScriptedLaunch::default());
    for answer in answers {
        launch.answer(answer);
    }
    let session_factory = WorkspaceTerminalSessionFactory::new_local(
        Rc::new(TestTerminalSessionFactory::new(
            TestTerminalSessionRecords::default(),
        )),
        crate::terminal::testing::test_local_directory(std::env::temp_dir()),
    );
    let panes = ScriptedPanes::new(session_factory);
    let edits = Rc::new(ScriptedEdits::new(Some(inventory())));
    let connector = Rc::new(ScriptedConnector {
        edits: Rc::clone(&edits),
        panes: Rc::clone(&panes),
    });
    let adapters = LocalServerAdapters {
        launch: Arc::clone(&launch) as Arc<_>,
        connector,
    };
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records).with_fallback_title("zsh"));
    let (manager, cx) = cx.add_window_view(|window, cx| {
        WorkspaceManager::new_with_adapters(
            session_factory,
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                local_server: Some(adapters),
                directory_selection: Rc::new(ScriptedDirectorySelection::new(selections)),
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
    (
        LaunchHarness {
            manager,
            launch,
            edits,
            panes,
        },
        cx,
    )
}

/// Reads the scripted server's inventory, as the window's poll does.
fn sync(harness: &LaunchHarness, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        harness
            .manager
            .update(cx, |manager, cx| manager.sync_shared_workspaces(window, cx));
    });
    cx.run_until_parked();
}

fn ready() -> Result<LaunchOutcome, ServerError> {
    Ok(LaunchOutcome::Ready)
}

#[gpui::test]
fn launch_presents_the_server_workspace_and_opens_its_pane_without_a_local_one(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = launch_harness(vec![ready()], cx);
    cx.run_until_parked();
    assert_eq!(
        harness.launch_title(cx),
        Some(launch::OPENING_TITLE),
        "the window says Workspaces are on the way rather than No Workspaces"
    );
    sync(&harness, cx);

    assert_eq!(harness.workspace_count(cx), 1, "no desktop-owned Workspace");
    assert_eq!(harness.shared_rows(cx), 1);
    assert_eq!(harness.launch_title(cx), None);
    assert_eq!(
        harness.panes.pane(PANE).selections(),
        1,
        "a new client's first view opens its Focused Pane for the person"
    );
    assert_eq!(harness.launch.steps(), ["run"]);
}

#[gpui::test]
fn a_stopped_server_stays_stopped_until_start_server(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::Stopped), ready()], cx);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));
    assert_eq!(harness.launch.steps(), ["run"], "launch never starts it");
    assert_eq!(harness.workspace_count(cx), 0);

    harness.click(LAUNCH_START_SELECTOR, cx);
    assert_eq!(harness.launch.steps(), ["run", "start", "run"]);
    sync(&harness, cx);
    assert_eq!(harness.shared_rows(cx), 1);
    assert_eq!(harness.launch_title(cx), None);
}

#[gpui::test]
fn a_refused_start_explains_itself_and_retries_launch_only_when_asked(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::Stopped), ready()], cx);
    cx.run_until_parked();
    harness.launch.refuse_start(ServerError::NotInstalled);
    harness.click(LAUNCH_START_SELECTOR, cx);
    assert_eq!(harness.launch.steps(), ["run", "start"]);
    assert_eq!(harness.launch_title(cx), Some("Server Not Installed"));

    harness.click(LAUNCH_RETRY_SELECTOR, cx);
    assert_eq!(harness.launch.steps(), ["run", "start", "run"]);
    sync(&harness, cx);
    assert_eq!(harness.shared_rows(cx), 1);
}

#[gpui::test]
fn startup_authorization_runs_only_from_its_explicit_step(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(
        vec![Ok(LaunchOutcome::AuthorizationRequired), ready()],
        cx,
    );
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::AUTHORIZATION_TITLE));
    assert_eq!(harness.launch.steps(), ["run"]);

    harness.launch.refuse_authorization();
    harness.click(LAUNCH_AUTHORIZE_SELECTOR, cx);
    assert_eq!(
        harness.launch.steps(),
        ["run", "authorize"],
        "a cancelled authorization runs nothing further"
    );
    let detail = harness
        .manager
        .read_with(cx, |manager, _| manager.launch_presentation())
        .expect("the authorization step stays")
        .detail;
    assert!(detail.starts_with("Authorization did not complete, so nothing changed."));

    harness.click(LAUNCH_AUTHORIZE_SELECTOR, cx);
    assert_eq!(
        harness.launch.steps(),
        ["run", "authorize", "authorize", "run"]
    );
    sync(&harness, cx);
    assert_eq!(harness.shared_rows(cx), 1);
}

#[gpui::test]
fn return_runs_the_launch_screens_default_action(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::Stopped), ready()], cx);
    cx.run_until_parked();
    assert!(cx.debug_bounds(LAUNCH_SELECTOR).is_some());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(harness.launch.steps(), ["run", "start", "run"]);
}

#[gpui::test]
fn removing_the_final_server_workspace_leaves_the_empty_view(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![ready()], cx);
    cx.run_until_parked();
    sync(&harness, cx);
    assert_eq!(harness.shared_rows(cx), 1);

    harness.edits.set_inventory(Inventory {
        revision: 2,
        workspaces: Vec::new(),
        ..inventory()
    });
    sync(&harness, cx);

    assert_eq!(
        harness.workspace_count(cx),
        0,
        "no desktop-owned Workspace replaces the server's"
    );
    assert!(cx.debug_bounds("empty-workspace").is_some());
}

#[gpui::test]
fn new_workspace_asks_the_server_rather_than_creating_desktop_work(cx: &mut TestAppContext) {
    use spaceterm_server::hierarchy::Mutation;

    let (harness, cx) = launch_harness(vec![ready()], cx);
    cx.run_until_parked();
    sync(&harness, cx);
    cx.dispatch_action(NewWorkspace);
    cx.run_until_parked();

    assert_eq!(
        harness.edits.submitted(),
        vec![Mutation::CreateWorkspace {
            name: "Default (2)".to_owned(),
            directory: None,
        }],
        "the next free default name, as desktop Workspaces were named"
    );
    assert_eq!(harness.workspace_count(cx), 1, "nothing appears before the server answers");
}

#[gpui::test]
fn open_local_directory_creates_a_server_workspace_rooted_there(cx: &mut TestAppContext) {
    use spaceterm_server::hierarchy::Mutation;

    let chosen = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("st-launch-open-{}", std::process::id()));
    std::fs::create_dir_all(&chosen).unwrap();
    let (harness, cx) = launch_harness_choosing(vec![ready()], [Ok(Some(chosen.clone()))], cx);
    cx.run_until_parked();
    sync(&harness, cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.open_local_directory(String::new(), window, cx);
            manager.choose_local_pin_directory(
                PinTarget::NewLocalWorkspace {
                    name: String::new(),
                },
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();

    assert_eq!(
        harness.edits.submitted(),
        vec![Mutation::CreateWorkspace {
            name: chosen.file_name().unwrap().to_string_lossy().into_owned(),
            directory: Some(chosen.clone()),
        }]
    );
    assert_eq!(harness.workspace_count(cx), 1);
    std::fs::remove_dir_all(chosen).unwrap();
}
