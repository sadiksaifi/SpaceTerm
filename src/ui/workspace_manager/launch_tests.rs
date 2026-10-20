//! Normal launch of the account-local server in the Workspace Window.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Entity, Modifiers, TestAppContext, VisualTestContext};
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
    launch_harness_with(
        answers,
        ScriptedEdits::new(Some(inventory())),
        selections,
        cx,
    )
}

/// A launch harness presenting `edits`, whose System Directory Selection answers `selections`.
fn launch_harness_with(
    answers: Vec<Result<LaunchOutcome, ServerError>>,
    edits: ScriptedEdits,
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
    let edits = Rc::new(edits);
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
                local_server: adapters,
                local_filesystem: crate::runtime::local_filesystem(),
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
    // The window reads the ready server at once, without waiting for a poll interval.
    cx.run_until_parked();

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
fn the_window_says_workspaces_are_on_the_way_until_the_server_answers(cx: &mut TestAppContext) {
    // No answer arrives, as while the server is still starting.
    let (harness, cx) = launch_harness(Vec::new(), cx);
    cx.run_until_parked();
    assert!(
        cx.debug_bounds(LAUNCH_SELECTOR).is_none(),
        "a launch still inside its quiet period says nothing yet"
    );
    assert!(cx.debug_bounds("empty-workspace").is_none());

    cx.executor().advance_clock(launch::LAUNCH_QUIET_PERIOD);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::OPENING_TITLE));
    assert!(cx.debug_bounds(LAUNCH_SELECTOR).is_some());
    assert!(
        cx.debug_bounds("empty-workspace").is_none(),
        "never No Workspaces while they are on the way"
    );
}

#[gpui::test]
fn a_launch_that_opens_within_its_quiet_period_never_shows_the_launch_screen(
    cx: &mut TestAppContext,
) {
    // The server is admitted at once, but its first inventory arrives a moment later.
    let edits = ScriptedEdits::new(None);
    let (harness, cx) = launch_harness_with(vec![ready()], edits, [], cx);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::OPENING_TITLE));
    assert!(
        cx.debug_bounds(LAUNCH_SELECTOR).is_none(),
        "an opening that has not taken long yet draws no spinner or message"
    );
    assert!(
        cx.debug_bounds("empty-workspace").is_none(),
        "never No Workspaces while they are on the way"
    );

    cx.executor().advance_clock(launch::LAUNCH_QUIET_PERIOD / 2);
    harness.edits.set_inventory(inventory());
    sync(&harness, cx);
    assert_eq!(harness.launch_title(cx), None);
    assert_eq!(harness.shared_rows(cx), 1);
    cx.executor().advance_clock(launch::LAUNCH_QUIET_PERIOD);
    cx.run_until_parked();
    assert!(cx.debug_bounds(LAUNCH_SELECTOR).is_none());
}

#[gpui::test]
fn a_retry_within_the_quiet_period_keeps_saying_what_it_does(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Err(ServerError::NotInstalled)], cx);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some("Server Not Installed"));

    // Try Again runs launch, and no answer arrives yet.
    harness.click(LAUNCH_RETRY_SELECTOR, cx);
    assert_eq!(harness.launch.steps(), ["run", "run"]);
    assert_eq!(harness.launch_title(cx), Some(launch::OPENING_TITLE));
    assert!(
        cx.debug_bounds(LAUNCH_SELECTOR).is_some(),
        "a screen the person already sees never goes blank while launch runs again"
    );
}

#[gpui::test]
fn a_launch_that_needs_the_person_explains_itself_at_once(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::Stopped)], cx);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));
    assert!(
        cx.debug_bounds(LAUNCH_START_SELECTOR).is_some(),
        "a screen that asks for an action never waits behind the quiet period"
    );
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
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::AuthorizationRequired), ready()], cx);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::AUTHORIZATION_TITLE));
    assert_eq!(harness.launch.steps(), ["run"]);

    harness
        .launch
        .refuse_authorization(ServerError::AuthorizationRequired);
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

/// An authorization that failed for another reason, before the prompt or after the system may
/// already have changed the account's startup policy, names its failure and claims nothing about
/// what changed.
fn assert_authorization_failure_explains_itself(
    error: ServerError,
    title: &str,
    cx: &mut TestAppContext,
) {
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::AuthorizationRequired)], cx);
    cx.run_until_parked();
    harness.launch.refuse_authorization(error);

    harness.click(LAUNCH_AUTHORIZE_SELECTOR, cx);
    let presentation = harness
        .manager
        .read_with(cx, |manager, _| manager.launch_presentation())
        .expect("the launch screen stays");

    assert_eq!(harness.launch.steps(), ["run", "authorize"]);
    assert_eq!(presentation.title, title);
    assert_eq!(presentation.detail, error.to_string());
    assert!(!presentation.detail.contains("nothing changed"));
    assert!(cx.debug_bounds(LAUNCH_RETRY_SELECTOR).is_some());
}

#[gpui::test]
fn an_authorization_whose_result_is_unconfirmed_reports_its_failure(cx: &mut TestAppContext) {
    // The system may have enabled startup before the service could confirm it.
    assert_authorization_failure_explains_itself(
        ServerError::ServiceUnavailable,
        "Couldn\u{2019}t Open Workspaces",
        cx,
    );
}

#[gpui::test]
fn an_authorization_that_fails_before_its_prompt_is_not_a_cancellation(cx: &mut TestAppContext) {
    assert_authorization_failure_explains_itself(
        ServerError::StorageUnavailable,
        "Server Storage Unavailable",
        cx,
    );
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
fn a_failure_before_inventory_explains_itself_and_offers_recovery(cx: &mut TestAppContext) {
    let edits = ScriptedEdits::new(None);
    edits.set_connected(false);
    let (harness, cx) = launch_harness_with(vec![ready()], edits, [], cx);
    cx.run_until_parked();
    sync(&harness, cx);
    assert_eq!(harness.launch_title(cx), Some(launch::OPENING_TITLE));
    for (error, title) in [
        (ServerError::IncompatibleProtocol, "Incompatible Server"),
        (ServerError::IncompatibleState, "Incompatible Server"),
        (
            ServerError::StorageUnavailable,
            "Server Storage Unavailable",
        ),
        (ServerError::UnsafeStorage, "Unsafe Server Storage"),
    ] {
        harness.edits.set_error(error);
        sync(&harness, cx);
        assert_eq!(harness.launch_title(cx), Some(title));
        assert!(cx.debug_bounds(LAUNCH_RETRY_SELECTOR).is_some());
        assert!(cx.debug_bounds(launch::LAUNCH_SETTINGS_SELECTOR).is_some());
    }
    // The connection retries an interruption by itself, so the window keeps opening.
    harness.edits.set_error(ServerError::NotRunning);
    sync(&harness, cx);
    assert_eq!(harness.launch_title(cx), Some(launch::OPENING_TITLE));
    // The incompatible refusal only read whether an update is offered.
    assert_eq!(
        harness.launch.steps(),
        ["run", "update_offered"],
        "no implicit Start or runtime activation"
    );
    assert!(harness.edits.submitted().is_empty());
}

/// A refused server keeps running. Launch names the refusal without guessing which side is
/// older, and offers the update journey only when a verified included build can update it.
#[gpui::test]
fn a_refused_server_offers_an_update_only_from_a_verified_build(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(
        vec![
            Err(ServerError::IncompatibleProtocol),
            Err(ServerError::IncompatibleProtocol),
        ],
        cx,
    );
    cx.run_until_parked();
    // Without a verified build, the refusal names no side and offers no update.
    let presentation = harness
        .manager
        .read_with(cx, |manager, _| manager.launch_presentation())
        .expect("the refusal is presented");
    assert!(
        presentation
            .detail
            .contains("SpaceTerm and the server need versions that work together")
    );
    assert!(cx.debug_bounds(launch::LAUNCH_UPDATE_SELECTOR).is_none());

    harness.launch.offer_update();
    harness.click(LAUNCH_RETRY_SELECTOR, cx);
    let presentation = harness
        .manager
        .read_with(cx, |manager, _| manager.launch_presentation())
        .expect("the refusal is presented");
    assert_eq!(presentation.title, "Incompatible Server");
    assert!(
        presentation
            .detail
            .contains("Terminal Sessions keep running")
    );
    assert!(presentation.detail.contains("SpaceTerm asks first"));
    assert!(cx.debug_bounds(launch::LAUNCH_UPDATE_SELECTOR).is_some());
    assert!(cx.debug_bounds(LAUNCH_RETRY_SELECTOR).is_some());
    assert_eq!(
        harness.launch.steps(),
        ["run", "update_offered", "run", "update_offered"],
        "reading the offer starts, stages, and activates nothing"
    );
}

/// Admission can succeed before Pane negotiation refuses a required capability. That refusal
/// reads the same verified offer, once, and offers the same update journey.
#[gpui::test]
fn a_refusal_after_admission_offers_the_verified_update(cx: &mut TestAppContext) {
    let edits = ScriptedEdits::new(None);
    edits.set_connected(false);
    let (harness, cx) = launch_harness_with(vec![ready()], edits, [], cx);
    harness.launch.offer_update();
    cx.run_until_parked();
    sync(&harness, cx);
    assert_eq!(harness.launch.steps(), ["run"]);

    harness
        .edits
        .set_error(ServerError::MissingRequiredCapability);
    sync(&harness, cx);
    assert_eq!(harness.launch_title(cx), Some("Incompatible Server"));
    assert!(cx.debug_bounds(launch::LAUNCH_UPDATE_SELECTOR).is_some());
    sync(&harness, cx);
    assert_eq!(
        harness.launch.steps(),
        ["run", "update_offered"],
        "one read per refusal, and nothing starts or activates"
    );
}

#[gpui::test]
fn a_missing_capability_without_a_build_names_no_update(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Err(ServerError::MissingRequiredCapability)], cx);
    cx.run_until_parked();
    let presentation = harness
        .manager
        .read_with(cx, |manager, _| manager.launch_presentation())
        .expect("the refusal is presented");
    assert_eq!(presentation.title, "Incompatible Server");
    assert!(
        presentation
            .detail
            .contains("doesn\u{2019}t support a feature this SpaceTerm requires")
    );
    assert!(
        presentation
            .detail
            .contains("the server needs a version that supports it")
    );
    assert!(cx.debug_bounds(launch::LAUNCH_UPDATE_SELECTOR).is_none());
    assert!(cx.debug_bounds(launch::LAUNCH_SETTINGS_SELECTOR).is_some());
}

/// A Stop between admission and the first inventory offers Start Server, which opens the
/// Workspaces once the server answers.
#[gpui::test]
fn a_stop_before_inventory_offers_start_server(cx: &mut TestAppContext) {
    let edits = ScriptedEdits::new(None);
    edits.set_connected(false);
    let (harness, cx) = launch_harness_with(vec![ready(), ready()], edits, [], cx);
    cx.run_until_parked();
    harness.edits.set_error(ServerError::Stopped);
    sync(&harness, cx);
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));

    harness.click(LAUNCH_START_SELECTOR, cx);
    harness.edits.set_inventory(inventory());
    harness.edits.set_connected(true);
    sync(&harness, cx);

    assert_eq!(harness.launch.steps(), ["run", "start", "run"]);
    assert_eq!(harness.launch_title(cx), None);
    assert_eq!(harness.shared_rows(cx), 1);
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
            name: String::new(),
            directory: None,
        }],
        "the server allocates the automatic name"
    );
    assert_eq!(
        harness.workspace_count(cx),
        1,
        "nothing appears before the server answers"
    );
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
            name: String::new(),
            directory: Some(chosen.clone()),
        }]
    );
    assert_eq!(harness.workspace_count(cx), 1);
    std::fs::remove_dir_all(chosen).unwrap();
}

/// One device presents its server in one Workspace Window. A second window that finishes launch
/// brings the first one forward instead of presenting the same Workspaces again.
#[gpui::test]
fn a_second_launched_window_brings_the_presenting_one_forward(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![ready()], cx);
    cx.run_until_parked();
    sync(&harness, cx);
    let first = cx.update(|window, _| window.window_handle().window_id());
    let launch = Arc::new(ScriptedLaunch::default());
    launch.answer(ready());
    let adapters = LocalServerAdapters {
        launch: Arc::clone(&launch) as Arc<_>,
        connector: Rc::new(ScriptedConnector {
            edits: Rc::clone(&harness.edits),
            panes: Rc::clone(&harness.panes),
        }),
    };
    let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
        TestTerminalSessionFactory::new(TestTerminalSessionRecords::default())
            .with_fallback_title("zsh"),
    );
    let second = cx.add_window(|window, cx| {
        WorkspaceManager::new_with_adapters(
            session_factory,
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                local_server: adapters,
                ..workspace_adapters(test_remote_backend_factory())
            },
            SidebarState::Expanded,
            window,
            cx,
        )
    });
    second
        .update(cx, |_, window, _| window.activate_window())
        .expect("the second window is open");
    cx.run_until_parked();

    assert_eq!(
        second
            .read_with(cx, |manager, _| manager.workspaces.len())
            .expect("the second window is open"),
        0,
        "a second window must not present the same server's Workspaces"
    );
    assert_eq!(harness.shared_rows(cx), 1, "the first window keeps them");
    assert_eq!(
        cx.update(|_, cx| cx.active_window().map(|active| active.window_id())),
        Some(first),
        "the Workspace Window presenting them comes forward"
    );
}

/// The ready launch harness with its server's Workspace presented and focused.
fn ready_harness(cx: &mut TestAppContext) -> (LaunchHarness, &mut VisualTestContext) {
    let (harness, cx) = launch_harness(vec![ready()], cx);
    cx.run_until_parked();
    sync(&harness, cx);
    (harness, cx)
}

/// Whether the window's only Workspace row replaces its directory with a shared status.
fn shared_row_status_drawn(harness: &LaunchHarness, cx: &mut VisualTestContext) -> bool {
    let id = harness.manager.read_with(cx, |manager, _| {
        manager
            .workspaces
            .iter()
            .next()
            .expect("one Workspace row")
            .id()
    });
    let selector: &'static str =
        Box::leak(format!("workspace-row-shared-status-{}", id.get()).into_boxed_str());
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.debug_bounds(selector).is_some()
}

#[gpui::test]
fn a_tab_creation_leaves_the_workspace_rows_as_they_are(cx: &mut TestAppContext) {
    use spaceterm_server::hierarchy::Mutation;
    let (harness, cx) = ready_harness(cx);
    assert!(!shared_row_status_drawn(&harness, cx));

    harness.click("create-tab-button", cx);
    sync(&harness, cx);
    assert!(matches!(
        harness.edits.submitted().as_slice(),
        [Mutation::CreateTab { .. }]
    ));
    assert!(
        !shared_row_status_drawn(&harness, cx),
        "a waiting Tab creation changes nothing a Workspace row shows"
    );
}

#[gpui::test]
fn a_pending_rename_says_saving_on_its_own_row(cx: &mut TestAppContext) {
    let (harness, cx) = ready_harness(cx);
    harness.manager.update(cx, |manager, cx| {
        let id = manager.workspaces.iter().next().expect("one row").id();
        let shared = manager.shared.as_mut().expect("shared Workspaces");
        let _ = shared.rename(id, "Logs".to_owned(), None);
        cx.notify();
    });
    sync(&harness, cx);
    assert!(shared_row_status_drawn(&harness, cx));
}

#[gpui::test]
fn not_connected_appears_only_once_the_connection_is_lost(cx: &mut TestAppContext) {
    let (harness, cx) = ready_harness(cx);
    // The connection published its inventory and is still finishing its handshake.
    harness.edits.set_connected(false);
    sync(&harness, cx);
    assert!(
        !shared_row_status_drawn(&harness, cx),
        "a connection still being established has not been lost"
    );

    harness.edits.set_error(ServerError::NotRunning);
    sync(&harness, cx);
    assert!(shared_row_status_drawn(&harness, cx));
}

fn created_names(harness: &LaunchHarness) -> Vec<String> {
    use spaceterm_server::hierarchy::Mutation;
    harness
        .edits
        .submitted()
        .into_iter()
        .filter_map(|mutation| match mutation {
            Mutation::CreateWorkspace { name, .. } => Some(name),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn the_switcher_creates_a_local_workspace_with_its_query_as_the_name(cx: &mut TestAppContext) {
    let (harness, cx) = ready_harness(cx);
    open_workspace_switcher_for_creation(cx);
    click("workspace-switcher-create-local", cx);

    assert_eq!(created_names(&harness), ["fresh workspace"]);
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn the_creation_shortcut_takes_the_switcher_name_instead_of_the_highlight(cx: &mut TestAppContext) {
    let (harness, cx) = ready_harness(cx);
    open_workspace_switcher_for_creation(cx);
    cx.simulate_keystrokes("down cmd-n");
    cx.run_until_parked();

    assert_eq!(created_names(&harness), ["fresh workspace"]);
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
    assert!(
        harness
            .manager
            .read_with(cx, |manager, _| { manager.remote_workspace_flow.is_none() })
    );
}

#[gpui::test]
fn blank_and_cancelled_creation_names_stay_automatic_on_the_server(cx: &mut TestAppContext) {
    use spaceterm_server::hierarchy::Outcome;

    let (harness, cx) = ready_harness(cx);
    open_workspace_switcher_for_creation(cx);
    cx.simulate_keystrokes("escape cmd-n");
    cx.run_until_parked();
    harness.edits.answer(Outcome::Accepted { target: [41; 32] });
    sync(&harness, cx);
    open_workspace_switcher(cx);
    cx.simulate_keystrokes("space space cmd-n");
    cx.run_until_parked();

    assert_eq!(created_names(&harness), ["", ""]);
}

#[gpui::test]
fn a_named_creation_never_switches_to_an_existing_match(cx: &mut TestAppContext) {
    let (harness, cx) = ready_harness(cx);
    open_workspace_switcher(cx);
    cx.simulate_keystrokes("space D e f a u l t space cmd-n");
    cx.run_until_parked();

    assert_eq!(created_names(&harness), ["Default"]);
    assert_eq!(harness.workspace_count(cx), 1);
}

#[gpui::test]
fn creation_shortcuts_never_escape_a_modal(cx: &mut TestAppContext) {
    let (harness, cx) = ready_harness(cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |_, cx| {
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
    assert!(
        harness
            .manager
            .read_with(cx, |manager, _| { manager.remote_workspace_flow.is_none() })
    );
    assert!(created_names(&harness).is_empty());
}

#[gpui::test]
fn creation_shortcuts_never_escape_the_switcher_input_context_menu(cx: &mut TestAppContext) {
    let (harness, cx) = ready_harness(cx);
    open_workspace_switcher_for_creation(cx);
    let input = cx.debug_bounds("combo-box-input").unwrap().center();
    cx.simulate_mouse_down(input, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(input, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.update(|window, cx| spaceterm_ui::window_menu_is_open(window, cx)));
    cx.simulate_keystrokes("cmd-n cmd-shift-n");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| spaceterm_ui::window_menu_is_open(window, cx)));
    assert!(created_names(&harness).is_empty());

    cx.simulate_keystrokes("escape cmd-n");
    cx.run_until_parked();
    assert_eq!(created_names(&harness), ["fresh workspace"]);
}

#[gpui::test]
fn the_switcher_filter_creates_a_workspace_named_by_it(cx: &mut TestAppContext) {
    let (harness, cx) = ready_harness(cx);
    open_workspace_switcher(cx);
    let icon = cx.debug_bounds("combo-box-input-leading").unwrap();
    let input = cx.debug_bounds("combo-box-input").unwrap();
    assert!(icon.right() <= input.left());
    assert!(input.size.width > px(0.0));
    cx.simulate_keystrokes("f i l t e r enter");
    cx.run_until_parked();

    assert_eq!(created_names(&harness), ["filter"]);
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

const CREATED: [u8; 32] = [61; 32];

/// The server accepts this client's creation and publishes the new Workspace beside the first.
fn accept_creation(harness: &LaunchHarness, pinned: Option<PathBuf>, cx: &mut VisualTestContext) {
    use spaceterm_server::hierarchy::{Mutation, Outcome};

    let Some(Mutation::CreateWorkspace { name, .. }) = harness.edits.submitted().pop() else {
        panic!("a creation must have been submitted");
    };
    let mut published = inventory();
    let mut created = published.workspaces[0].clone();
    created.id = CREATED;
    created.name = if name.is_empty() {
        "Default (2)".into()
    } else {
        name
    };
    created.pinned_directory = pinned;
    created.root_tab = [62; 32];
    created.tabs[0].id = [62; 32];
    created.tabs[0].root_pane = [63; 32];
    created.tabs[0].panes[0].id = [63; 32];
    published.workspaces.push(created);
    published.revision = 2;
    harness.edits.set_inventory(published);
    harness.edits.answer(Outcome::Accepted { target: CREATED });
    sync(harness, cx);
}

fn active_server_workspace(
    harness: &LaunchHarness,
    cx: &mut VisualTestContext,
) -> Option<[u8; 32]> {
    harness.manager.read_with(cx, |manager, _| {
        let local = manager.workspaces.active_workspace_id()?;
        manager
            .shared
            .as_ref()?
            .facts(local)
            .map(|facts| facts.server)
    })
}

#[gpui::test]
fn the_new_workspace_menu_creates_and_opens_a_server_workspace(cx: &mut TestAppContext) {
    let (harness, cx) = ready_harness(cx);
    click_new_workspace_menu("new-workspace-menu-create-local", cx);
    assert_eq!(created_names(&harness), [""]);
    assert_eq!(
        harness.workspace_count(cx),
        1,
        "nothing appears before the server answers"
    );

    accept_creation(&harness, None, cx);

    assert_eq!(harness.workspace_count(cx), 2);
    assert_eq!(active_server_workspace(&harness, cx), Some(CREATED));
    assert_eq!(
        harness.panes.pane([63; 32]).selections(),
        1,
        "the person's own creation opens its Pane"
    );
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn switcher_keyboard_acceptance_creates_exactly_one_workspace_without_a_picker(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = ready_harness(cx);
    open_workspace_switcher_for_creation(cx);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert_eq!(created_names(&harness), ["fresh workspace"]);
    assert!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.pin_picker.is_none())
    );
    accept_creation(&harness, None, cx);
    assert_eq!(harness.workspace_count(cx), 2);
    assert_eq!(active_server_workspace(&harness, cx), Some(CREATED));
}

#[gpui::test]
fn the_collapsed_workspace_name_creates_a_named_workspace_and_stays_collapsed(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = ready_harness(cx);
    click("toggle-sidebar-button", cx);
    click("workspace-chip-label", cx);
    cx.simulate_keystrokes("f r e s h space w o r k s p a c e");
    cx.run_until_parked();
    click("workspace-switcher-create-local", cx);

    assert_eq!(created_names(&harness), ["fresh workspace"]);
    assert!(
        !harness
            .manager
            .read_with(cx, |manager, cx| manager.sidebar.read(cx).layout().visible)
    );
    assert!(!cx.update(|window, cx| window_combo_box_is_open(window, cx)));
}

#[gpui::test]
fn open_local_directory_from_the_sidebar_names_by_directory_or_creates_nothing(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::Mutation;

    let project = temporary_directory("launch-open-local-sidebar");
    std::fs::create_dir_all(&project).unwrap();
    let (harness, cx) = ready_harness(cx);
    click_new_workspace_menu("new-workspace-menu-open-local-directory", cx);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(harness.edits.submitted().is_empty());
    assert!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.pin_picker.is_none())
    );

    click_new_workspace_menu("new-workspace-menu-open-local-directory", cx);
    confirm_directory_picker_path(&format!("{}/", project.to_str().unwrap()), cx);

    assert_eq!(
        harness.edits.submitted(),
        vec![Mutation::CreateWorkspace {
            name: String::new(),
            directory: Some(project.clone()),
        }]
    );
    assert!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.pin_picker.is_none())
    );
    std::fs::remove_dir_all(project).unwrap();
}

#[gpui::test]
fn open_local_directory_takes_the_switcher_query_as_its_name(cx: &mut TestAppContext) {
    let project = temporary_directory("launch-open-local-query");
    std::fs::create_dir_all(&project).unwrap();
    let (harness, cx) = ready_harness(cx);
    open_workspace_switcher_for_creation(cx);
    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    assert!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.pin_picker.is_some())
    );
    confirm_directory_picker_path(&format!("{}/", project.to_str().unwrap()), cx);

    assert_eq!(created_names(&harness), ["fresh workspace"]);
    std::fs::remove_dir_all(project).unwrap();
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
mod unix_adapter_tests {
    include!("../../platform/unix_adapter_tests/workspace_manager_launch.rs");
}

/// Starting the server from Settings opens the Workspaces of a window waiting on it, and leaves
/// a window that already presents them alone.
#[gpui::test]
fn a_service_start_elsewhere_resumes_only_a_waiting_launch(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::Stopped), ready()], cx);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));

    cx.cx
        .update(crate::ui::workspace_manager::resume_waiting_launches);
    cx.run_until_parked();
    assert_eq!(
        harness.launch.steps(),
        ["run", "run"],
        "no Start of its own"
    );
    assert_eq!(harness.shared_rows(cx), 1);

    cx.cx
        .update(crate::ui::workspace_manager::resume_waiting_launches);
    cx.run_until_parked();
    assert_eq!(harness.launch.steps(), ["run", "run"]);
}

/// A Start from the status menu reaches a window that launched while the server was stopped, and
/// a Stop from there leaves it waiting.
#[gpui::test]
fn a_status_menu_start_resumes_a_window_waiting_on_a_stopped_server(cx: &mut TestAppContext) {
    use spaceterm_server::service::ServiceStatus;

    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::Stopped), ready()], cx);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));

    let stopped = ServiceStatus {
        installed: true,
        enabled: false,
        running: false,
        stopped: true,
    };
    cx.cx
        .update(|cx| crate::status_menu::operation_succeeded(cx, stopped));
    cx.run_until_parked();
    assert_eq!(
        harness.launch.steps(),
        ["run"],
        "a stopped server stays stopped"
    );

    let running = ServiceStatus {
        enabled: true,
        running: true,
        stopped: false,
        ..stopped
    };
    cx.cx
        .update(|cx| crate::status_menu::operation_succeeded(cx, running));
    cx.run_until_parked();
    assert_eq!(
        harness.launch.steps(),
        ["run", "run"],
        "no Start of its own"
    );
    assert_eq!(harness.shared_rows(cx), 1);
}

/// Stopping the server from Settings while its Workspaces are open leaves each Pane saying so,
/// with Start Server where retrying could never connect.
fn stopped_in_session(cx: &mut TestAppContext) -> (LaunchHarness, &mut VisualTestContext) {
    stopped_in_session_as(
        crate::terminal::server_session::DevelopmentPaneStatus::Failed(ServerError::Stopped),
        cx,
    )
}

/// Like `stopped_in_session`, with the Pane left in `status` when the server stopped.
fn stopped_in_session_as(
    status: crate::terminal::server_session::DevelopmentPaneStatus,
    cx: &mut TestAppContext,
) -> (LaunchHarness, &mut VisualTestContext) {
    let (harness, cx) = ready_harness(cx);
    harness.launch.answer(ready());
    harness.panes.pane(PANE).set(status);
    cx.executor()
        .advance_clock(crate::ui::shared_workspaces::POLL_INTERVAL * 3);
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    (harness, cx)
}

#[gpui::test]
fn a_pane_stopped_in_session_offers_start_server_instead_of_retry(cx: &mut TestAppContext) {
    let (harness, cx) = stopped_in_session(cx);
    assert!(cx.debug_bounds("shared-pane-start-server-1").is_some());
    assert!(cx.debug_bounds("shared-pane-retry-1").is_none());

    harness.click("shared-pane-start-server-1", cx);

    assert_eq!(harness.launch.steps(), ["run", "start", "run"]);
}

/// Once Start Server in a Pane starts the server, a Pane it stopped connects again, so the
/// server says what became of it. Nothing takes control or starts a shell on the person's behalf.
fn assert_start_reconnects(
    status: crate::terminal::server_session::DevelopmentPaneStatus,
    cx: &mut TestAppContext,
) {
    let (harness, cx) = stopped_in_session_as(status, cx);
    let pane = harness.panes.pane(PANE);

    harness.click("shared-pane-start-server-1", cx);

    assert_eq!(
        (pane.retries(), pane.take_overs(), pane.restored_starts()),
        (1, 0, 0)
    );
}

#[gpui::test]
fn starting_the_server_from_a_pane_reconnects_its_stopped_pane(cx: &mut TestAppContext) {
    assert_start_reconnects(
        crate::terminal::server_session::DevelopmentPaneStatus::Failed(ServerError::Stopped),
        cx,
    );
}

/// A Pane this window did not control offers Take Over again once the server answers, rather
/// than waiting on a connection that never retries.
#[gpui::test]
fn starting_the_server_reconnects_a_stopped_take_over_pane_without_taking_control(
    cx: &mut TestAppContext,
) {
    assert_start_reconnects(
        crate::terminal::server_session::DevelopmentPaneStatus::TakeoverFailed(
            ServerError::Stopped,
        ),
        cx,
    );
}

#[gpui::test]
fn a_refused_start_while_presenting_explains_itself(cx: &mut TestAppContext) {
    let (harness, cx) = stopped_in_session(cx);
    harness.launch.refuse_start(ServerError::NotInstalled);

    harness.click("shared-pane-start-server-1", cx);

    assert_eq!(harness.launch.steps(), ["run", "start"]);
    assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    assert_eq!(
        harness.panes.pane(PANE).retries(),
        0,
        "the server is still stopped"
    );
}

/// Starting the server from Settings reconnects the Panes a presenting window shows stopped,
/// without running launch again, taking control, or starting a shell.
#[gpui::test]
fn a_service_start_elsewhere_reconnects_the_stopped_panes_of_a_presenting_window(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = stopped_in_session(cx);
    let pane = harness.panes.pane(PANE);

    cx.cx
        .update(crate::ui::workspace_manager::resume_waiting_launches);
    cx.run_until_parked();

    assert_eq!(harness.launch.steps(), ["run"]);
    assert_eq!(
        (pane.retries(), pane.take_overs(), pane.restored_starts()),
        (1, 0, 0)
    );
}

/// A Pane refused while an update was resolving connects again once the update finishes, as a
/// stopped Pane does after Start, whether the server then runs or stays stopped. The server
/// says what became of it; nothing takes control, starts a shell, or starts the server.
fn assert_update_reconnects(
    status: crate::terminal::server_session::DevelopmentPaneStatus,
    finished: fn(&mut App),
    retries: usize,
    cx: &mut TestAppContext,
) {
    let (harness, cx) = stopped_in_session_as(status, cx);
    let pane = harness.panes.pane(PANE);

    cx.cx.update(finished);
    cx.run_until_parked();

    assert_eq!(harness.launch.steps(), ["run"]);
    assert_eq!(
        (pane.retries(), pane.take_overs(), pane.restored_starts()),
        (retries, 0, 0)
    );
}

#[gpui::test]
fn a_finished_update_reconnects_a_pane_refused_while_it_resolved(cx: &mut TestAppContext) {
    assert_update_reconnects(
        crate::terminal::server_session::DevelopmentPaneStatus::Failed(
            ServerError::ActivationUnresolved,
        ),
        crate::ui::workspace_manager::resume_waiting_launches,
        1,
        cx,
    );
}

#[gpui::test]
fn a_finished_update_reconnects_a_refused_take_over_pane_without_taking_control(
    cx: &mut TestAppContext,
) {
    assert_update_reconnects(
        crate::terminal::server_session::DevelopmentPaneStatus::TakeoverFailed(
            ServerError::ActivationUnresolved,
        ),
        crate::ui::workspace_manager::resume_waiting_launches,
        1,
        cx,
    );
}

/// A Return leaves the server stopped, so nothing resumes launch, yet the refused Pane still
/// connects again and then says the server is stopped.
#[gpui::test]
fn a_return_that_keeps_the_server_stopped_reconnects_a_refused_pane(cx: &mut TestAppContext) {
    assert_update_reconnects(
        crate::terminal::server_session::DevelopmentPaneStatus::Failed(
            ServerError::ActivationUnresolved,
        ),
        crate::ui::workspace_manager::update_resolved,
        1,
        cx,
    );
}

/// Unrelated refusals still wait for the person's Retry.
#[gpui::test]
fn a_finished_update_leaves_an_unrelated_refusal_alone(cx: &mut TestAppContext) {
    assert_update_reconnects(
        crate::terminal::server_session::DevelopmentPaneStatus::Failed(
            ServerError::AuthenticationRefused,
        ),
        crate::ui::workspace_manager::resume_waiting_launches,
        0,
        cx,
    );
}

/// A launch that runs again but has not answered yet, as when a person retries.
fn launching_after_stop(
    answers: Vec<Result<LaunchOutcome, ServerError>>,
    cx: &mut TestAppContext,
) -> (LaunchHarness, &mut VisualTestContext) {
    let (harness, cx) = launch_harness(
        std::iter::once(Ok(LaunchOutcome::Stopped))
            .chain(answers)
            .collect(),
        cx,
    );
    cx.run_until_parked();
    let manager = harness.manager.clone();
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| manager.run_local_launch(None, window, cx));
    });
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.launch.state),
        launch::LaunchState::Launching,
        "the second launch has not answered yet"
    );
    (harness, cx)
}

/// A service started elsewhere while this window's launch is still answering, such as a
/// Restart finishing before a launch that observed it stopped, is not lost. Launch runs once
/// more and opens the Workspaces, and a server that stays stopped still asks for Start.
#[gpui::test]
fn a_service_start_during_launch_runs_launch_again_when_it_finishes(cx: &mut TestAppContext) {
    let (harness, cx) = launching_after_stop(vec![Ok(LaunchOutcome::Stopped), ready()], cx);
    cx.cx
        .update(crate::ui::workspace_manager::resume_waiting_launches);
    cx.run_until_parked();
    assert_eq!(
        harness.launch.steps(),
        ["run", "run", "run"],
        "no Start of its own"
    );
    assert_eq!(harness.shared_rows(cx), 1);
}

#[gpui::test]
fn a_service_start_during_launch_keeps_a_deliberate_stop(cx: &mut TestAppContext) {
    let (harness, cx) = launching_after_stop(
        vec![Ok(LaunchOutcome::Stopped), Ok(LaunchOutcome::Stopped)],
        cx,
    );
    cx.cx
        .update(crate::ui::workspace_manager::resume_waiting_launches);
    cx.run_until_parked();
    assert_eq!(harness.launch.steps(), ["run", "run", "run"]);
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));
    assert_eq!(harness.shared_rows(cx), 0);
}
