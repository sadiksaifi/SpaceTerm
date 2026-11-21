//! Normal launch of the account-local server in the Workspace Window.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use spaceterm_core::hierarchy::{Inventory, Pane, Side, Tab, Workspace};

use super::*;
use crate::machine_connection::CoreError;
use crate::ui::workspace_manager::launch::{
    LAUNCH_AUTHORIZE_SELECTOR, LAUNCH_LOGIN_ITEMS_SELECTOR, LAUNCH_RETRY_SELECTOR, LAUNCH_SELECTOR,
    LAUNCH_START_SELECTOR, LaunchOutcome, WorkspaceCoreConnector, WorkspaceMachineConnection,
};
use crate::ui::workspace_manager::launch_testing::ScriptedLaunch;
use crate::ui::workspaces::testing::{ScriptedEdits, ScriptedPanes};
use crate::ui::workspaces::{WorkspaceEditSource, WorkspacePaneSessions};

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
                worktree_membership: spaceterm_core::hierarchy::WorktreeMembership::Following,
                worktree_repository: None,
                id: TAB,
                root_pane: PANE,
                panes: vec![Pane {
                    id: PANE,
                    source: None,
                    side: Side::Right,
                    starting_directory: PathBuf::from("/server/home"),
                    current_directory: None,
                    repository_directory: None,
                    presentation: None,
                    observers: Default::default(),
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

impl WorkspaceCoreConnector for ScriptedConnector {
    fn connect(
        &self,
        _: LocalFilesystemAuthority,
        panes: &PaneConstruction,
        _: &mut App,
    ) -> Option<WorkspaceMachineConnection> {
        Some(WorkspaceMachineConnection {
            edits: WorkspaceEditSource::new(Rc::clone(&self.edits) as Rc<_>),
            sessions: WorkspacePaneSessions::with_opener(Rc::clone(&self.panes) as Rc<_>, panes),
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
    fn rows(&self, cx: &mut VisualTestContext) -> usize {
        self.manager.read_with(cx, |manager, _| {
            manager
                .workspaces
                .iter()
                .filter(|workspace| manager.has_workspace(workspace.id()))
                .count()
        })
    }

    fn workspace_count(&self, cx: &mut VisualTestContext) -> usize {
        self.manager
            .read_with(cx, |manager, _| manager.workspaces.len())
    }

    fn authoritative_inventory(&self, cx: &mut VisualTestContext) -> Inventory {
        self.manager.read_with(cx, |manager, _| {
            manager
                .binding
                .local()
                .expect("the local server is presented")
                .latest_inventory()
                .expect("the server published its inventory")
                .clone()
        })
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
    answers: Vec<Result<LaunchOutcome, CoreError>>,
    cx: &mut TestAppContext,
) -> (LaunchHarness, &mut VisualTestContext) {
    launch_harness_choosing(answers, [], cx)
}

/// A launch harness whose System Directory Selection answers `selections` in order.
fn launch_harness_choosing(
    answers: Vec<Result<LaunchOutcome, CoreError>>,
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
    answers: Vec<Result<LaunchOutcome, CoreError>>,
    edits: ScriptedEdits,
    selections: impl IntoIterator<
        Item = Result<Option<PathBuf>, crate::directory_selection::DirectoryChooserError>,
    >,
    cx: &mut TestAppContext,
) -> (LaunchHarness, &mut VisualTestContext) {
    launch_harness_restoring(answers, edits, selections, None, cx)
}

/// A launch harness whose window presents `remembered` as it opens, as reopening SpaceTerm does
/// before launch answers.
fn launch_harness_restoring(
    answers: Vec<Result<LaunchOutcome, CoreError>>,
    edits: ScriptedEdits,
    selections: impl IntoIterator<
        Item = Result<Option<PathBuf>, crate::directory_selection::DirectoryChooserError>,
    >,
    remembered: Option<crate::ui::remembered_view::WindowView>,
    cx: &mut TestAppContext,
) -> (LaunchHarness, &mut VisualTestContext) {
    let launch = ScriptedLaunch::default();
    for answer in answers {
        launch.answer(answer);
    }
    scripted_harness(
        launch,
        edits,
        selections,
        remembered,
        launch::LaunchIntent::Passive,
        cx,
    )
}

/// A launch harness whose background item reads answer `off` in order before launch begins.
fn launch_harness_reading_background_item(
    answers: Vec<Result<LaunchOutcome, CoreError>>,
    off: impl IntoIterator<Item = bool>,
    cx: &mut TestAppContext,
) -> (LaunchHarness, &mut VisualTestContext) {
    let launch = ScriptedLaunch::default();
    for answer in answers {
        launch.answer(answer);
    }
    launch.background_item(off);
    scripted_harness(
        launch,
        ScriptedEdits::new(Some(inventory())),
        [],
        None,
        launch::LaunchIntent::Passive,
        cx,
    )
}

fn scripted_harness(
    launch: ScriptedLaunch,
    edits: ScriptedEdits,
    selections: impl IntoIterator<
        Item = Result<Option<PathBuf>, crate::directory_selection::DirectoryChooserError>,
    >,
    remembered: Option<crate::ui::remembered_view::WindowView>,
    intent: launch::LaunchIntent,
    cx: &mut TestAppContext,
) -> (LaunchHarness, &mut VisualTestContext) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let launch = Arc::new(launch);
    let session_factory = WorkspaceTerminalSessionFactory::new_core(Rc::new(
        TestTerminalSessionFactory::new(TestTerminalSessionRecords::default()),
    ));
    let panes = ScriptedPanes::new(session_factory);
    let edits = Rc::new(edits);
    let connector = Rc::new(ScriptedConnector {
        edits: Rc::clone(&edits),
        panes: Rc::clone(&panes),
    });
    let adapters = LocalCoreAdapters {
        launch: Arc::clone(&launch) as Arc<_>,
        connector,
        intent,
    };
    let (manager, cx) = cx.add_window_view(|window, cx| {
        let mut manager = WorkspaceManager::new_with_adapters(
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                local_core: adapters,
                local_filesystem: crate::runtime::local_filesystem(),
                directory_selection: Rc::new(ScriptedDirectorySelection::new(selections)),
                ..workspace_adapters()
            },
            SidebarState::Expanded,
            window,
            cx,
        );
        // Opening focuses the window's content, then presents what the last window left.
        manager.focus(window, cx);
        if let Some(view) = remembered {
            manager.restore_remembered(view, window, cx);
        }
        manager
    });
    cx.update(|window, _| window.activate_window());
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

/// Reads the scripted server's inventory, as the window does when its connection publishes one.
fn sync(harness: &LaunchHarness, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.sync_workspaces(MachineKey::LOCAL, window, cx)
        });
    });
    cx.run_until_parked();
}

fn ready() -> Result<LaunchOutcome, CoreError> {
    Ok(LaunchOutcome::Ready)
}

#[gpui::test]
fn launch_presents_the_workspace_and_opens_its_core_pane(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![ready()], cx);
    // The window reads the ready server at once, without waiting for a poll interval.
    cx.run_until_parked();

    assert_eq!(harness.workspace_count(cx), 1, "no desktop-owned Workspace");
    assert_eq!(harness.rows(cx), 1);
    assert_eq!(harness.launch_title(cx), None);
    assert_eq!(
        harness.panes.pane(PANE).selections(),
        1,
        "a new client's first view opens its Focused Pane for the person"
    );
    assert_eq!(harness.launch.steps(), ["run"]);
}

#[gpui::test]
fn the_window_says_workspaces_are_on_the_way_until_the_core_answers(cx: &mut TestAppContext) {
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
    assert_eq!(harness.rows(cx), 1);
    cx.executor().advance_clock(launch::LAUNCH_QUIET_PERIOD);
    cx.run_until_parked();
    assert!(cx.debug_bounds(LAUNCH_SELECTOR).is_none());
}

#[gpui::test]
fn a_retry_within_the_quiet_period_keeps_saying_what_it_does(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Err(CoreError::NotInstalled)], cx);
    cx.run_until_parked();
    assert_eq!(
        harness.launch_title(cx),
        Some("SpaceTerm Core Not Installed")
    );

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
fn a_launch_during_a_core_operation_says_so_without_asking_the_person_to_act(
    cx: &mut TestAppContext,
) {
    // The operation, such as an update, has not finished when the window looks again.
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::ServiceBusy)], cx);
    cx.run_until_parked();

    assert_eq!(harness.launch.steps(), ["run", "wait_for_service", "run"]);
    let presentation = harness
        .manager
        .read_with(cx, |manager, _| manager.launch_presentation())
        .expect("the launch screen stays until the server answers");
    assert_eq!(presentation.title, launch::BUSY_TITLE);
    assert_eq!(
        presentation.detail.as_ref(),
        "Another SpaceTerm Core operation is running. Your Workspaces open when it finishes."
    );
    assert!(presentation.progress.is_some());
    assert!(
        presentation.actions.is_empty(),
        "nothing to dismiss or retry"
    );
    assert!(cx.debug_bounds(LAUNCH_SELECTOR).is_some());
    assert!(cx.debug_bounds(LAUNCH_RETRY_SELECTOR).is_none());
}

#[gpui::test]
fn a_launch_during_a_core_operation_opens_the_workspaces_once_it_ends(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::ServiceBusy), ready()], cx);
    cx.run_until_parked();

    assert_eq!(harness.launch.steps(), ["run", "wait_for_service", "run"]);
    assert_eq!(harness.launch_title(cx), None);
    assert_eq!(harness.rows(cx), 1);
}

/// An operation that outlasts the server's bounded wait is the one failure the wait reports.
#[gpui::test]
fn an_operation_that_outlasts_the_wait_offers_try_again(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Err(CoreError::ServiceBusy), ready()], cx);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some("SpaceTerm Core Busy"));

    harness.click(LAUNCH_RETRY_SELECTOR, cx);
    sync(&harness, cx);
    assert_eq!(harness.launch.steps(), ["run", "run"]);
    assert_eq!(harness.rows(cx), 1);
}

#[gpui::test]
fn a_stopped_core_stays_stopped_until_start_core(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::Stopped), ready()], cx);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));
    assert_eq!(harness.launch.steps(), ["run"], "launch never starts it");
    assert_eq!(harness.workspace_count(cx), 0);

    harness.click(LAUNCH_START_SELECTOR, cx);
    assert_eq!(harness.launch.steps(), ["run", "start", "run"]);
    sync(&harness, cx);
    assert_eq!(harness.rows(cx), 1);
    assert_eq!(harness.launch_title(cx), None);
}

/// Start in the Workspace Window tells every open Settings window that this computer's
/// SpaceTerm Core changed outside Settings, so none keeps showing it stopped.
#[gpui::test]
fn start_core_in_the_workspace_window_reaches_open_settings(cx: &mut TestAppContext) {
    use crate::ui::settings_window::local_operations::{CompletedOperation, LocalCoreOperations};
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::Stopped), ready()], cx);
    cx.run_until_parked();
    let heard = Rc::new(std::cell::Cell::new(0));
    let _subscription = cx.update(|_, cx| {
        let operations = LocalCoreOperations::get(cx);
        let heard = Rc::clone(&heard);
        cx.subscribe(&operations, move |_, operation, _| {
            if matches!(operation, CompletedOperation::Elsewhere) {
                heard.set(heard.get() + 1);
            }
        })
    });

    harness.click(LAUNCH_START_SELECTOR, cx);
    sync(&harness, cx);

    assert_eq!(harness.launch.steps(), ["run", "start", "run"]);
    assert_eq!(
        heard.get(),
        1,
        "the Start's launch reached the running Core once"
    );
}

/// A launch harness whose window opens for the person's own launch of SpaceTerm.
fn foreground_harness(
    launch: ScriptedLaunch,
    cx: &mut TestAppContext,
) -> (LaunchHarness, &mut VisualTestContext) {
    foreground_harness_with(launch, ScriptedEdits::new(Some(inventory())), cx)
}

/// [`foreground_harness`] presenting `edits`.
fn foreground_harness_with(
    launch: ScriptedLaunch,
    edits: ScriptedEdits,
    cx: &mut TestAppContext,
) -> (LaunchHarness, &mut VisualTestContext) {
    scripted_harness(
        launch,
        edits,
        [],
        None,
        launch::LaunchIntent::Foreground,
        cx,
    )
}

fn no_workspaces() -> Inventory {
    Inventory {
        workspaces: Vec::new(),
        ..inventory()
    }
}

#[gpui::test]
fn the_persons_launch_creates_one_workspace_when_none_is_attached(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::Mutation;
    let launch = ScriptedLaunch::default();
    launch.answer(ready());
    let (harness, cx) =
        foreground_harness_with(launch, ScriptedEdits::new(Some(no_workspaces())), cx);
    sync(&harness, cx);
    sync(&harness, cx);
    assert_eq!(
        harness.edits.submitted(),
        vec![Mutation::CreateWorkspace {
            name: String::new(),
            directory: None,
            control: spaceterm_core::hierarchy::CreateControl::Creator,
        }],
        "one launch creates one Workspace"
    );
}

/// A cold launch presents this computer's inventory before its server can take a creation.
/// The landing waits without an alert, then creates exactly one Workspace once it can.
#[gpui::test]
fn a_cold_launch_creates_its_workspace_once_the_core_can_take_it(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::Mutation;
    let launch = ScriptedLaunch::default();
    launch.answer(ready());
    let edits = ScriptedEdits::new(Some(no_workspaces()));
    edits.set_connected(false);
    let (harness, cx) = foreground_harness_with(launch, edits, cx);
    sync(&harness, cx);
    sync(&harness, cx);
    assert!(
        harness.edits.submitted().is_empty(),
        "nothing is asked of a server that cannot take it"
    );
    assert!(
        !cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)),
        "the wait is not a failure"
    );

    harness.edits.set_connected(true);
    sync(&harness, cx);
    sync(&harness, cx);
    assert_eq!(
        harness.edits.submitted(),
        vec![Mutation::CreateWorkspace {
            name: String::new(),
            directory: None,
            control: spaceterm_core::hierarchy::CreateControl::Creator,
        }],
        "the launch creates one Workspace once"
    );
}

/// The person's own creation while the landing waits replaces the creation the launch owed.
#[gpui::test]
fn a_creation_the_person_chose_replaces_the_one_a_cold_launch_owed(cx: &mut TestAppContext) {
    let launch = ScriptedLaunch::default();
    launch.answer(ready());
    let edits = ScriptedEdits::new(Some(no_workspaces()));
    edits.set_connected(false);
    let (harness, cx) = foreground_harness_with(launch, edits, cx);
    sync(&harness, cx);
    harness.edits.set_connected(true);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.run_workspace_creation(
                crate::ui::workspace_creation::WorkspaceCreation::Local,
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    sync(&harness, cx);
    sync(&harness, cx);
    assert_eq!(
        harness.edits.submitted().len(),
        1,
        "only the person's own creation is asked for"
    );
}

#[gpui::test]
fn a_reopened_window_with_nothing_attached_creates_nothing(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness_with(
        vec![ready()],
        ScriptedEdits::new(Some(no_workspaces())),
        [],
        cx,
    );
    sync(&harness, cx);
    sync(&harness, cx);
    assert!(harness.edits.submitted().is_empty());
}

#[gpui::test]
fn the_persons_launch_opens_the_focused_pane_once(cx: &mut TestAppContext) {
    let launch = ScriptedLaunch::default();
    launch.answer(ready());
    let (harness, cx) = foreground_harness(launch, cx);
    sync(&harness, cx);
    sync(&harness, cx);
    assert_eq!(harness.panes.pane(PANE).selections(), 1);
    assert!(harness.edits.submitted().is_empty(), "nothing is created");
}

#[gpui::test]
fn the_persons_launch_starts_core_once_and_later_launches_keep_a_stop(cx: &mut TestAppContext) {
    let launch = ScriptedLaunch::default();
    // Another operation holds the service first, so the same launch runs again after the wait.
    launch.answer(Ok(LaunchOutcome::ServiceBusy));
    launch.answer(ready());
    launch.answer(Ok(LaunchOutcome::Stopped));
    let (harness, cx) = foreground_harness(launch, cx);
    sync(&harness, cx);
    assert_eq!(harness.rows(cx), 1);
    assert_eq!(
        harness.launch.intents(),
        [
            launch::LaunchIntent::Foreground,
            launch::LaunchIntent::Foreground
        ]
    );

    // A Stop chosen later stays in force when this window launches again.
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.run_local_launch(None, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        harness.launch.intents().last(),
        Some(&launch::LaunchIntent::Passive)
    );
}

#[gpui::test]
fn a_reopened_window_launches_passively(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::Stopped)], cx);
    cx.run_until_parked();
    assert_eq!(harness.launch.intents(), [launch::LaunchIntent::Passive]);
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));
}

#[gpui::test]
fn starting_core_shows_its_loader_at_once(cx: &mut TestAppContext) {
    let launch = ScriptedLaunch::default();
    // The run starts SpaceTerm Core and has not answered yet.
    launch.start_core();
    let (harness, cx) = foreground_harness(launch, cx);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::STARTING_TITLE));
    assert!(
        cx.debug_bounds(LAUNCH_SELECTOR).is_some(),
        "starting SpaceTerm Core shows inside the quiet period"
    );
    assert!(cx.debug_bounds(LAUNCH_START_SELECTOR).is_none());
}

/// SpaceTerm Core is off in Login Items, so the service refuses launch's start. The window says so
/// and opens that list: once the person turns it on and comes back, launch runs again.
#[gpui::test]
fn a_core_off_in_login_items_opens_that_list_until_turned_on(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Err(CoreError::LoginItemOff), ready()], cx);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::LOGIN_ITEM_OFF_TITLE));
    assert!(cx.debug_bounds(LAUNCH_START_SELECTOR).is_none());
    assert!(cx.debug_bounds(LAUNCH_RETRY_SELECTOR).is_none());

    harness.click(LAUNCH_LOGIN_ITEMS_SELECTOR, cx);
    assert_eq!(harness.launch.steps(), ["run", "open_login_items"]);

    // The person turns SpaceTerm Core on in System Settings and comes back.
    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    sync(&harness, cx);
    assert_eq!(harness.launch.steps(), ["run", "open_login_items", "run"]);
    assert_eq!(harness.rows(cx), 1);
}

/// The person turns SpaceTerm Core off in Login Items while its Workspaces are open. macOS stops
/// it, so the stage says what launch says and offers that list in place of the Tabs, instead of
/// reconnecting to a server that can't answer. Once the person turns it on and comes back, launch
/// runs again and the Tabs and their Panes return without being asked.
#[gpui::test]
fn a_core_turned_off_in_login_items_while_open_says_so_until_turned_on(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    // The connection loss reads the item off.
    let (harness, cx) = launch_harness_reading_background_item(vec![ready(), ready()], [true], cx);
    cx.run_until_parked();
    sync(&harness, cx);
    assert!(cx.debug_bounds("tab-bar").is_some());

    harness.edits.fail_attempt(CoreError::NotRunning);
    sync(&harness, cx);
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    let tree = A11yTree::read(cx);
    let stage = tree.node(launch::LOGIN_ITEM_OFF_TITLE).clone();
    assert_eq!(
        stage["aria"]["description"],
        "Workspaces on This Computer open when you turn it on."
    );
    assert!(cx.debug_bounds("tab-bar").is_none(), "no Tab reconnects");
    assert!(cx.debug_bounds(super::empty::RECONNECT_SELECTOR).is_none());
    assert!(
        cx.debug_bounds(LAUNCH_START_SELECTOR).is_none(),
        "Start can't run a service macOS keeps off"
    );
    assert!(
        !cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)),
        "the stage says it, so no alert repeats it"
    );
    assert_eq!(harness.launch.steps(), ["run"], "the read changes nothing");

    harness.click(LAUNCH_LOGIN_ITEMS_SELECTOR, cx);
    assert_eq!(harness.launch.steps(), ["run", "open_login_items"]);

    // The person turns SpaceTerm Core on in System Settings and comes back.
    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert_eq!(harness.launch.steps(), ["run", "open_login_items", "run"]);
    harness.edits.reconnect();
    sync(&harness, cx);
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(
        A11yTree::read(cx)
            .find(launch::LOGIN_ITEM_OFF_TITLE)
            .is_none()
    );
    assert!(cx.debug_bounds("tab-bar").is_some(), "the Tabs return");
}

/// The person stopped SpaceTerm Core, then turned it off in Login Items. The service refuses the
/// Start chosen after that, and the window says the item is off and offers that list. Once the
/// person turns it on and comes back, the window reads launch again and offers Start. The refused
/// Start never runs by itself, so the Stop stays until the person chooses Start.
#[gpui::test]
fn a_start_refused_for_login_items_never_runs_by_itself(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(
        vec![
            Ok(LaunchOutcome::Stopped),
            Ok(LaunchOutcome::Stopped),
            ready(),
        ],
        cx,
    );
    cx.run_until_parked();
    harness.launch.refuse_start(CoreError::LoginItemOff);

    harness.click(LAUNCH_START_SELECTOR, cx);

    assert_eq!(harness.launch.steps(), ["run", "start"]);
    assert_eq!(harness.launch_title(cx), Some(launch::LOGIN_ITEM_OFF_TITLE));
    harness.click(LAUNCH_LOGIN_ITEMS_SELECTOR, cx);
    assert_eq!(harness.launch.steps(), ["run", "start", "open_login_items"]);

    // The person turns SpaceTerm Core on in System Settings and comes back.
    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert_eq!(
        harness.launch.steps(),
        ["run", "start", "open_login_items", "run"],
        "launch reads again and starts nothing"
    );
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));
    assert!(cx.debug_bounds(LAUNCH_START_SELECTOR).is_some());

    harness.click(LAUNCH_START_SELECTOR, cx);
    sync(&harness, cx);
    assert_eq!(
        harness.launch.steps(),
        ["run", "start", "open_login_items", "run", "start", "run"]
    );
    assert_eq!(harness.rows(cx), 1);
}

/// An item macOS blocked when launch installed it and an item the person turned off read the
/// same to the service, so the window says the same about both on every run. A Retry never turns
/// a blocked item into one the person "turned off".
#[gpui::test]
fn an_item_off_in_login_items_reads_the_same_on_every_run(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(
        vec![Err(CoreError::LoginItemOff), Err(CoreError::LoginItemOff)],
        cx,
    );
    cx.run_until_parked();
    let first = harness
        .manager
        .read_with(cx, |manager, _| manager.launch_presentation());

    let manager = harness.manager.clone();
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| manager.run_local_launch(None, window, cx));
    });
    cx.run_until_parked();

    assert_eq!(harness.launch.steps(), ["run", "run"]);
    let second = harness
        .manager
        .read_with(cx, |manager, _| manager.launch_presentation());
    assert_eq!(second, first);
    assert_eq!(
        second.map(|presentation| presentation.title),
        Some(launch::LOGIN_ITEM_OFF_TITLE)
    );
}

/// The person stopped SpaceTerm Core and turned it off in Login Items while another computer's
/// Workspace is active. Start from this computer's row can't say so on the stage, so its alert
/// offers Open Login Items rather than only OK.
#[gpui::test]
fn a_start_refused_by_login_items_behind_another_machines_workspace_offers_that_list(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = ready_harness(cx);
    present_paired_machine(&harness, inventory(), cx);
    let manager = harness.manager.clone();
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            let remote = manager
                .workspaces
                .iter()
                .map(|workspace| workspace.id())
                .find(|id| manager.binding.owner(*id) != Some(MachineKey::LOCAL))
                .expect("the paired machine's Workspace");
            manager.activate_workspace(remote, window, cx);
        });
    });
    harness.edits.set_connected(false);
    harness.edits.set_error(CoreError::Stopped);
    sync(&harness, cx);
    harness.launch.refuse_start(CoreError::LoginItemOff);

    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.run_launch_action(launch::LaunchAction::Start, window, cx);
        });
    });
    cx.run_until_parked();

    assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    let action: &'static str = Box::leak(
        format!("modal-action-{}", launch::LAUNCH_ALERT_ACTION_SELECTOR).into_boxed_str(),
    );
    harness.click(action, cx);
    assert_eq!(harness.launch.steps(), ["run", "start", "open_login_items"]);
}

/// A paired server presented while the local server is stopped makes this window the presenting
/// one. Starting the local server afterward still adds its Workspaces here, beside the paired
/// server's.
#[gpui::test]
fn a_local_machine_started_after_a_paired_machine_joins_the_same_window(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::Stopped), ready()], cx);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));

    present_paired_machine(&harness, inventory(), cx);
    assert_eq!(
        harness.rows(cx),
        1,
        "the paired server's Workspace is presented"
    );

    // The person starts the local server from outside this window.
    cx.cx
        .update(crate::ui::workspace_manager::resume_waiting_launches);
    cx.run_until_parked();
    sync(&harness, cx);

    assert_eq!(harness.launch.steps(), ["run", "run"]);
    assert_eq!(
        harness.rows(cx),
        2,
        "the local server's Workspace joins the window already presenting the paired server"
    );
}

/// Connects a paired server publishing `inventory` to the window, as a saved connection does.
fn present_paired_machine(
    harness: &LaunchHarness,
    inventory: Inventory,
    cx: &mut VisualTestContext,
) {
    let remote = Rc::new(ScriptedEdits::new(Some(inventory)));
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            let key = manager.present_core_workspaces(
                MachineOrigin::Direct(crate::ui::workspaces::DirectMachine {
                    route: [9; 32],
                    name: "penguin".into(),
                }),
                WorkspaceEditSource::new(Rc::clone(&remote) as Rc<_>),
                WorkspacePaneSessions::with_opener(
                    Rc::clone(&harness.panes) as Rc<_>,
                    &PaneConstruction::testing(),
                ),
                window,
                cx,
            );
            manager.sync_workspaces(key, window, cx);
        });
    });
    cx.run_until_parked();
}

/// The controls that arrange or create Workspaces, which need usable server state.
const WORKSPACE_CONTROLS: [&str; 4] = [
    "workspace-sidebar",
    "workspace-sidebar-resize-handle",
    "toggle-sidebar-button",
    "workspace-switcher",
];

fn workspace_controls_presented(cx: &mut VisualTestContext) -> Vec<&'static str> {
    WORKSPACE_CONTROLS
        .into_iter()
        .filter(|selector| cx.debug_bounds(selector).is_some())
        .collect()
}

fn creation_available(cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| window.is_action_available(&NewWorkspace, cx))
}

/// Without usable server state the launch screen takes the whole window beneath its Window
/// Controls. Nothing offers to create or arrange Workspaces, and Server Settings stays offered.
#[gpui::test]
fn startup_without_usable_core_state_presents_recovery_across_the_window(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Err(CoreError::RuntimeUnavailable), ready()], cx);
    cx.run_until_parked();
    assert_eq!(
        harness.launch_title(cx),
        Some("Couldn\u{2019}t Verify SpaceTerm Core")
    );
    assert_eq!(workspace_controls_presented(cx), Vec::<&str>::new());
    assert!(!creation_available(cx), "New Workspace has nowhere to go");
    assert!(cx.debug_bounds(launch::LAUNCH_SETTINGS_SELECTOR).is_some());
    let viewport = cx.update(|window, _| window.viewport_size());
    let launch = cx
        .debug_bounds(LAUNCH_SELECTOR)
        .expect("recovery is presented");
    let stage = cx
        .debug_bounds("empty-workspace-stage")
        .expect("the stage is presented");
    assert_eq!(stage.size, viewport, "the stage fills the window");
    assert!(
        launch.origin.x < gpui::px(16.0) && launch.size.width > viewport.width - gpui::px(32.0),
        "recovery spans the window rather than sitting beside a sidebar: {launch:?}"
    );

    // Recovery holds while launch runs again, then the Workspaces bring their controls back.
    harness.click(LAUNCH_RETRY_SELECTOR, cx);
    sync(&harness, cx);
    assert_eq!(harness.rows(cx), 1);
    assert_eq!(workspace_controls_presented(cx), WORKSPACE_CONTROLS);
    assert!(creation_available(cx));
}

/// A build without a server package and a package that failed verification need different
/// remedies, so each names its own cause. Trying again can't add a missing package, so only a
/// failed verification offers Try Again.
#[gpui::test]
fn a_missing_core_package_and_a_failed_verification_explain_themselves_differently(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = launch_harness(
        vec![
            Err(CoreError::RuntimeUnavailable),
            Err(CoreError::CandidateMissing),
        ],
        cx,
    );
    cx.run_until_parked();
    let presentation = |cx: &mut VisualTestContext| {
        harness
            .manager
            .read_with(cx, |manager, _| manager.launch_presentation())
            .expect("the failure is presented")
    };
    let unverified = presentation(cx);
    assert_eq!(unverified.title, "Couldn\u{2019}t Verify SpaceTerm Core");
    assert_eq!(unverified.detail, "Reinstall SpaceTerm, then try again.");
    assert!(
        !unverified.detail.contains("include"),
        "{}",
        unverified.detail
    );
    assert!(cx.debug_bounds(launch::LAUNCH_SETTINGS_SELECTOR).is_some());

    harness.click(LAUNCH_RETRY_SELECTOR, cx);
    let missing = presentation(cx);
    assert_eq!(missing.title, "SpaceTerm Core Not Included");
    assert_eq!(
        missing.detail,
        "This SpaceTerm build doesn\u{2019}t include SpaceTerm Core. Reinstall SpaceTerm, then try again."
    );
    assert!(cx.debug_bounds(LAUNCH_RETRY_SELECTOR).is_none());
    assert!(cx.debug_bounds(launch::LAUNCH_SETTINGS_SELECTOR).is_some());
    assert_eq!(
        harness.launch.steps(),
        ["run", "run"],
        "nothing starts or activates"
    );
}

/// This computer's failed server never hides another server's Workspaces or the controls that
/// arrange them.
#[gpui::test]
fn a_failed_local_machine_keeps_another_machines_workspaces_presented(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Err(CoreError::CandidateMissing)], cx);
    cx.run_until_parked();
    assert_eq!(workspace_controls_presented(cx), Vec::<&str>::new());

    present_paired_machine(&harness, inventory(), cx);
    assert_eq!(harness.rows(cx), 1);
    assert_eq!(
        workspace_controls_presented(cx).len(),
        WORKSPACE_CONTROLS.len()
    );
    assert!(creation_available(cx));
}

/// Another server's inventory is usable state even before any of its Workspaces is attached, so
/// the window keeps the switcher that attaches and creates them beside the local failure.
#[gpui::test]
fn another_machines_inventory_without_attached_workspaces_keeps_workspace_controls(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = launch_harness(vec![Err(CoreError::CandidateMissing)], cx);
    cx.run_until_parked();

    present_paired_machine(
        &harness,
        Inventory {
            workspaces: Vec::new(),
            ..inventory()
        },
        cx,
    );
    assert_eq!(harness.workspace_count(cx), 0);
    assert_eq!(
        workspace_controls_presented(cx).len(),
        WORKSPACE_CONTROLS.len()
    );
    assert!(creation_available(cx));
    assert_eq!(
        harness.launch_title(cx),
        Some("SpaceTerm Core Not Included"),
        "the stage still explains this computer's server"
    );
}

#[gpui::test]
fn a_refused_start_explains_itself_and_retries_launch_only_when_asked(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::Stopped), ready()], cx);
    cx.run_until_parked();
    harness.launch.refuse_start(CoreError::NotInstalled);
    harness.click(LAUNCH_START_SELECTOR, cx);
    assert_eq!(harness.launch.steps(), ["run", "start"]);
    assert_eq!(
        harness.launch_title(cx),
        Some("SpaceTerm Core Not Installed")
    );

    harness.click(LAUNCH_RETRY_SELECTOR, cx);
    assert_eq!(harness.launch.steps(), ["run", "start", "run"]);
    sync(&harness, cx);
    assert_eq!(harness.rows(cx), 1);
}

#[gpui::test]
fn startup_authorization_runs_only_from_its_explicit_step(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::AuthorizationRequired), ready()], cx);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::AUTHORIZATION_TITLE));
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.launch_presentation())
            .expect("the authorization step is presented")
            .detail,
        "Enter an administrator password so your account\u{2019}s services keep running after you \
         log out. This applies to every service your account runs and stays on if you uninstall SpaceTerm \
         Core.",
        "launch explains authorization as Settings does"
    );
    assert_eq!(harness.launch.steps(), ["run"]);

    harness
        .launch
        .refuse_authorization(CoreError::AuthorizationRequired);
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
    assert!(detail.starts_with("Authorization didn\u{2019}t finish."));

    harness.click(LAUNCH_AUTHORIZE_SELECTOR, cx);
    assert_eq!(
        harness.launch.steps(),
        ["run", "authorize", "authorize", "run"]
    );
    sync(&harness, cx);
    assert_eq!(harness.rows(cx), 1);
}

/// An authorization that failed for another reason, before the prompt or after the system may
/// already have changed the account's startup policy, names its failure and claims nothing about
/// what changed.
fn assert_authorization_failure_explains_itself(
    error: CoreError,
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
        CoreError::ServiceUnavailable,
        "Couldn\u{2019}t Open Workspaces",
        cx,
    );
}

#[gpui::test]
fn an_authorization_that_fails_before_its_prompt_is_not_a_cancellation(cx: &mut TestAppContext) {
    assert_authorization_failure_explains_itself(
        CoreError::StorageUnavailable,
        "SpaceTerm Core Storage Unavailable",
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
        (
            CoreError::IncompatibleProtocol(
                spaceterm_core::protocol::version::Incompatible::ListenerTooOld,
            ),
            "SpaceTerm Core Is Too Old",
        ),
        (
            CoreError::IncompatibleProtocol(
                spaceterm_core::protocol::version::Incompatible::CallerTooOld,
            ),
            "This SpaceTerm Is Too Old",
        ),
        (CoreError::IncompatibleState, "Incompatible SpaceTerm Core"),
        (
            CoreError::StorageUnavailable,
            "SpaceTerm Core Storage Unavailable",
        ),
        (CoreError::UnsafeStorage, "Unsafe SpaceTerm Core Storage"),
    ] {
        harness.edits.set_error(error);
        sync(&harness, cx);
        assert_eq!(harness.launch_title(cx), Some(title));
        assert!(cx.debug_bounds(LAUNCH_RETRY_SELECTOR).is_some());
        assert!(cx.debug_bounds(launch::LAUNCH_SETTINGS_SELECTOR).is_some());
    }
    // The connection retries an interruption by itself, so the window keeps opening.
    harness.edits.set_error(CoreError::NotRunning);
    sync(&harness, cx);
    assert_eq!(harness.launch_title(cx), Some(launch::OPENING_TITLE));
    assert_eq!(
        harness.launch.steps(),
        ["run"],
        "no implicit Start or runtime activation"
    );
    assert!(harness.edits.submitted().is_empty());
}

/// This computer's Machine, blocked because its Core is too old for this SpaceTerm.
const LOCAL_MACHINE: crate::updates::machines::MachineId = [41; 32];

fn too_old_core(
    configure: impl FnOnce(&mut spaceterm_core::update::facts::UpdateFacts),
) -> crate::updates::machines::MachineUpdate {
    use spaceterm_core::update::facts::{UpdateCheck, UpdateFacts, Version};
    let mut facts = UpdateFacts {
        revision: 1,
        platform: spaceterm_core::update::facts::HOST_PLATFORM,
        running: Version::parse("0.11.4").unwrap(),
        pending: None,
        sessions: 2,
        operation: None,
        check: UpdateCheck::Never,
        skipped: None,
        last_failure: None,
    };
    configure(&mut facts);
    crate::updates::machines::MachineUpdate {
        detailed: None,
        machine: LOCAL_MACHINE,
        name: "This Mac".into(),
        local: true,
        link: crate::updates::machines::MachineLink::Incompatible(
            spaceterm_core::protocol::version::Incompatible::ListenerTooOld,
        ),
        facts: Some(facts),
        boot: None,
        capabilities: spaceterm_core::protocol::capability::Capabilities::fake(
            &spaceterm_core::protocol::capability::Capability::ALL,
        ),
        push: None,
        bundled: None,
        failed_push: None,
        asked: None,
    }
}

fn pending_0_14() -> spaceterm_core::update::facts::PendingVersion {
    spaceterm_core::update::facts::PendingVersion {
        version: spaceterm_core::update::facts::Version::parse("0.14.0").unwrap(),
        protocols: spaceterm_core::update::facts::Protocols::CURRENT,
        breaking: true,
        installed_at: crate::updates::now() - 60,
        overdue_since: None,
        stranded: Vec::new(),
    }
}

/// Shared update store whose Machines the test scripts.
fn scripted_machines(
    cx: &mut TestAppContext,
) -> Rc<crate::updates::machines::testing::ScriptedMachines> {
    let machines = Rc::new(crate::updates::machines::testing::ScriptedMachines::default());
    cx.update(|cx| {
        crate::updates::machines::store::MachineUpdates::install(machines.clone(), cx);
    });
    machines
}

fn refused() -> Result<LaunchOutcome, CoreError> {
    Err(CoreError::IncompatibleProtocol(
        spaceterm_core::protocol::version::Incompatible::ListenerTooOld,
    ))
}

/// A refused server keeps running. Launch names the side that is too old, shows the Core's
/// download in text beside its progress, and offers Restart Core only once it is downloaded.
#[gpui::test]
fn a_refused_core_names_the_older_side_and_shows_its_download(cx: &mut TestAppContext) {
    use spaceterm_core::update::facts::{Operation, Version};
    use spaceterm_ui::a11y_testing::A11yTree;
    let machines = scripted_machines(cx);
    let (harness, cx) = launch_harness(vec![refused()], cx);
    cx.run_until_parked();
    let presentation = harness
        .manager
        .read_with(cx, |manager, _| manager.launch_presentation())
        .expect("the refusal is presented");
    assert_eq!(presentation.title, "SpaceTerm Core Is Too Old");
    assert!(
        presentation
            .detail
            .ends_with(" is too old for this SpaceTerm."),
        "{}",
        presentation.detail
    );
    assert!(
        cx.debug_bounds(launch::LAUNCH_RESTART_CORE_SELECTOR)
            .is_none()
    );

    machines.emit(crate::updates::machines::MachineUpdatesEvent::Machines(
        vec![too_old_core(|facts| {
            facts.operation = Some(Operation::Installing {
                version: Version::parse("0.14.0").unwrap(),
                received: 1,
                total: 2,
            });
        })],
    ));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("server-launch-progress-label").is_some(),
        "the download shows in text beside the ring"
    );
    A11yTree::read(cx).text("Downloading SpaceTerm Core 0.14.0\u{2026}");
    assert!(
        cx.debug_bounds(launch::LAUNCH_RESTART_CORE_SELECTOR)
            .is_none(),
        "nothing is downloaded yet, so nothing restarts"
    );

    machines.emit(crate::updates::machines::MachineUpdatesEvent::Changed(
        Box::new(too_old_core(|facts| facts.pending = Some(pending_0_14()))),
    ));
    cx.run_until_parked();
    assert!(cx.debug_bounds("server-launch-progress-label").is_none());
    assert!(
        cx.debug_bounds(launch::LAUNCH_RESTART_CORE_SELECTOR)
            .is_some()
    );
    assert_eq!(
        harness.launch.steps(),
        ["run"],
        "nothing starts or activates"
    );
}

/// Restart Core asks the local Core for a fresh preview and confirms it, also after a start into
/// the kept version failed. Only the approved preview executes.
#[gpui::test]
fn the_launch_restart_core_asks_first_and_restarts_only_the_previewed_target(
    cx: &mut TestAppContext,
) {
    use spaceterm_core::update::facts::{FailureKind, StartFailure, UpdateFailure};
    const CONFIRM: &str = "modal-action-update-confirm-proceed";
    const CANCEL: &str = "modal-action-update-confirm-cancel";
    let machines = scripted_machines(cx);
    let (harness, cx) = launch_harness(vec![refused()], cx);
    machines.emit(crate::updates::machines::MachineUpdatesEvent::Machines(
        vec![too_old_core(|facts| {
            facts.pending = Some(pending_0_14());
            facts.last_failure = Some(UpdateFailure {
                id: 1,
                at: 0,
                version: pending_0_14().version,
                kind: FailureKind::Start(StartFailure::PortInUse),
            });
        })],
    ));
    cx.run_until_parked();

    harness.click(launch::LAUNCH_RESTART_CORE_SELECTOR, cx);
    assert_eq!(
        machines.asked(),
        vec![(
            LOCAL_MACHINE,
            crate::updates::machines::MachineAction::Restart
        )],
        "the Core previews the restart first"
    );
    assert!(cx.debug_bounds(CONFIRM).is_some(), "the confirmation asks");
    assert!(
        machines.executed().is_empty(),
        "nothing runs before approval"
    );

    harness.click(CANCEL, cx);
    assert!(machines.executed().is_empty(), "Cancel runs nothing");

    harness.click(launch::LAUNCH_RESTART_CORE_SELECTOR, cx);
    harness.click(CONFIRM, cx);
    let executed = machines.executed();
    assert_eq!(executed.len(), 1, "one restart");
    assert_eq!(executed[0].machine, LOCAL_MACHINE);
    assert_eq!(
        executed[0].action,
        crate::updates::machines::MachineAction::Restart
    );
    assert_eq!(executed[0].target, Some(pending_0_14().version));
}

/// A reconnection after the first admission can be refused too, such as by a replaced server.
/// That refusal presents the same blocked screen and starts nothing.
#[gpui::test]
fn a_refusal_after_admission_presents_the_blocked_screen(cx: &mut TestAppContext) {
    let edits = ScriptedEdits::new(None);
    edits.set_connected(false);
    let (harness, cx) = launch_harness_with(vec![ready()], edits, [], cx);
    cx.run_until_parked();
    sync(&harness, cx);
    harness.edits.set_error(CoreError::IncompatibleProtocol(
        spaceterm_core::protocol::version::Incompatible::ListenerTooOld,
    ));
    sync(&harness, cx);
    assert_eq!(harness.launch_title(cx), Some("SpaceTerm Core Is Too Old"));
    assert!(
        cx.debug_bounds(launch::LAUNCH_RESTART_CORE_SELECTOR)
            .is_none()
    );
    assert_eq!(
        harness.launch.steps(),
        ["run"],
        "nothing starts or activates"
    );
}

/// A Stop between admission and the first inventory offers Start SpaceTerm Core, which opens the
/// Workspaces once the server answers.
#[gpui::test]
fn a_stop_before_inventory_offers_start_core(cx: &mut TestAppContext) {
    let edits = ScriptedEdits::new(None);
    edits.set_connected(false);
    let (harness, cx) = launch_harness_with(vec![ready(), ready()], edits, [], cx);
    cx.run_until_parked();
    harness.edits.set_error(CoreError::Stopped);
    sync(&harness, cx);
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));

    harness.click(LAUNCH_START_SELECTOR, cx);
    harness.edits.set_inventory(inventory());
    harness.edits.set_connected(true);
    sync(&harness, cx);

    assert_eq!(harness.launch.steps(), ["run", "start", "run"]);
    assert_eq!(harness.launch_title(cx), None);
    assert_eq!(harness.rows(cx), 1);
}

#[gpui::test]
fn removing_the_final_core_workspace_closes_its_window(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![ready()], cx);
    cx.run_until_parked();
    sync(&harness, cx);
    assert_eq!(harness.rows(cx), 1);

    harness.edits.set_inventory(Inventory {
        revision: 2,
        workspaces: Vec::new(),
        ..inventory()
    });
    sync(&harness, cx);

    assert!(
        cx.windows().is_empty(),
        "removing the final Workspace dismisses its Window without creating desktop work"
    );
}

#[gpui::test]
fn new_workspace_presents_a_pending_creation_without_core_authority(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::Mutation;

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
            control: spaceterm_core::hierarchy::CreateControl::Creator,
        }],
        "the server allocates the automatic name"
    );
    assert_eq!(
        harness.workspace_count(cx),
        2,
        "the pending Workspace appears before the server answers"
    );
    assert_ne!(active_core_workspace(&harness, cx).unwrap(), WORKSPACE);
    assert_eq!(harness.authoritative_inventory(cx), inventory());
    assert_eq!(
        harness.panes.opened().len(),
        1,
        "a pending creation cannot subscribe to a server Pane"
    );
}

/// New Workspace chosen with no Workspace Window opens one that has no attached Workspaces yet.
/// It creates one Workspace once the server presents them, where opening SpaceTerm creates none.
#[gpui::test]
fn new_workspace_without_a_window_creates_once_the_core_presents_its_workspaces(
    cx: &mut TestAppContext,
) {
    use spaceterm_core::hierarchy::Mutation;

    let (harness, cx) = launch_harness_with(vec![ready()], ScriptedEdits::new(None), [], cx);
    cx.run_until_parked();
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.create_workspace_when_presented(window, cx);
        });
    });
    cx.run_until_parked();
    assert!(
        harness.edits.submitted().is_empty(),
        "the server has not presented its Workspaces yet"
    );

    harness.edits.set_inventory(Inventory {
        workspaces: Vec::new(),
        ..inventory()
    });
    sync(&harness, cx);
    sync(&harness, cx);

    assert_eq!(
        harness.edits.submitted(),
        vec![Mutation::CreateWorkspace {
            name: String::new(),
            directory: None,
            control: spaceterm_core::hierarchy::CreateControl::Creator,
        }],
        "one request creates one Workspace"
    );
}

#[gpui::test]
fn open_local_directory_creates_a_core_workspace_rooted_there(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::Mutation;

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
            control: spaceterm_core::hierarchy::CreateControl::Creator,
        }]
    );
    assert_eq!(harness.workspace_count(cx), 2);
    assert_eq!(harness.authoritative_inventory(cx), inventory());
    assert_eq!(harness.panes.opened().len(), 1);
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
    let adapters = LocalCoreAdapters {
        launch: Arc::clone(&launch) as Arc<_>,
        connector: Rc::new(ScriptedConnector {
            edits: Rc::clone(&harness.edits),
            panes: Rc::clone(&harness.panes),
        }),
        intent: launch::LaunchIntent::Passive,
    };
    let second = cx.add_window(|window, cx| {
        WorkspaceManager::new_with_adapters(
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                local_core: adapters,
                ..workspace_adapters()
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
    assert_eq!(harness.rows(cx), 1, "the first window keeps them");
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
fn workspace_row_status_drawn(harness: &LaunchHarness, cx: &mut VisualTestContext) -> bool {
    let id = harness.manager.read_with(cx, |manager, _| {
        manager
            .workspaces
            .iter()
            .next()
            .expect("one Workspace row")
            .id()
    });
    let selector: &'static str =
        Box::leak(format!("workspace-row-status-{}", id.get()).into_boxed_str());
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.debug_bounds(selector).is_some()
}

/// The status line 2 of the window's only Workspace row shows in place of its directory.
fn workspace_row_status(
    harness: &LaunchHarness,
    cx: &mut VisualTestContext,
) -> Option<&'static str> {
    harness.manager.read_with(cx, |manager, _| {
        let id = manager
            .workspaces
            .iter()
            .next()
            .expect("one Workspace row")
            .id();
        manager
            .workspace_row_facts(id)
            .and_then(|facts| facts.row.status)
            .map(|status| status.text)
    })
}

/// Stopping This Mac's SpaceTerm Core while one of its Workspaces is active follows the No-Tabs
/// rule. Its row says Stopped in the quiet color, and the stage is one empty Pane offering Start
/// SpaceTerm Core, with no Tab and no New Tab. No update control shows, so the Pane rises to the
/// window's top.
#[gpui::test]
fn a_core_stopped_in_session_leaves_an_empty_pane_that_offers_start(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![ready(), ready()], cx);
    cx.run_until_parked();
    sync(&harness, cx);
    assert!(cx.debug_bounds("create-tab-button").is_some());

    harness.edits.set_connected(false);
    harness.edits.set_error(CoreError::Stopped);
    sync(&harness, cx);
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    assert_eq!(workspace_row_status(&harness, cx), Some("Stopped"));
    assert!(cx.debug_bounds("tab-bar").is_none());
    assert!(cx.debug_bounds("create-tab-button").is_none());
    let stage = cx
        .debug_bounds("empty-workspace-stage")
        .expect("the stage renders");
    let pane = cx
        .debug_bounds("empty-workspace-pane")
        .expect("the stage draws an empty Pane");
    let inset = cx.update(|_, cx| {
        crate::ui::workspace_frame::WorkspaceFrame::for_appearance(
            crate::ui::appearance::chrome(cx),
            cx,
        )
        .window_edge_inset()
    });
    assert_eq!(pane.top() - stage.top(), inset);

    harness.click(LAUNCH_START_SELECTOR, cx);
    assert_eq!(harness.launch.steps(), ["run", "start", "run"]);
}

/// A stopped Workspace's row menu offers Start SpaceTerm Core, as its empty Pane does.
#[gpui::test]
fn a_stopped_workspace_row_offers_start_core(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![ready(), ready()], cx);
    cx.run_until_parked();
    sync(&harness, cx);
    let id = harness.manager.read_with(cx, |manager, _| {
        manager
            .workspaces
            .iter()
            .next()
            .expect("one Workspace row")
            .id()
    });
    let row: &'static str =
        Box::leak(format!("workspace-row-{}-active", id.get()).into_boxed_str());
    right_click(row, cx);
    assert!(
        cx.debug_bounds("workspace-menu-row-start-core").is_none(),
        "a running server needs no Start"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    harness.edits.set_connected(false);
    harness.edits.set_error(CoreError::Stopped);
    sync(&harness, cx);
    right_click(row, cx);
    click("workspace-menu-row-start-core", cx);

    assert_eq!(harness.launch.steps(), ["run", "start", "run"]);
}

#[gpui::test]
fn a_tab_creation_leaves_the_workspace_rows_as_they_are(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::Mutation;
    let (harness, cx) = ready_harness(cx);
    assert!(!workspace_row_status_drawn(&harness, cx));

    harness.click("create-tab-button", cx);
    sync(&harness, cx);
    assert!(matches!(
        harness.edits.submitted().as_slice(),
        [Mutation::CreateTab { .. }]
    ));
    assert!(
        !workspace_row_status_drawn(&harness, cx),
        "a waiting Tab creation changes nothing a Workspace row shows"
    );
}

#[gpui::test]
fn a_pending_rename_presents_its_name_without_a_saving_label(cx: &mut TestAppContext) {
    let (harness, cx) = ready_harness(cx);
    harness.manager.update(cx, |manager, cx| {
        let id = manager.workspaces.iter().next().expect("one row").id();
        let binding = manager.binding.local_mut().expect("Workspaces");
        let _ = binding.rename(id, "Logs".to_owned(), None);
        cx.notify();
    });
    sync(&harness, cx);
    assert_eq!(
        harness.manager.read_with(cx, |manager, _| {
            manager.workspaces.iter().next().unwrap().name().to_owned()
        }),
        "Logs"
    );
    assert!(!workspace_row_status_drawn(&harness, cx));
    assert_eq!(harness.authoritative_inventory(cx), inventory());
}

#[gpui::test]
fn not_connected_appears_only_once_the_connection_is_lost(cx: &mut TestAppContext) {
    let (harness, cx) = ready_harness(cx);
    // The connection published its inventory and is still finishing its handshake.
    harness.edits.set_connected(false);
    sync(&harness, cx);
    assert!(
        !workspace_row_status_drawn(&harness, cx),
        "a connection still being established has not been lost"
    );

    harness.edits.set_error(CoreError::NotRunning);
    sync(&harness, cx);
    assert!(workspace_row_status_drawn(&harness, cx));
}

fn created_names(harness: &LaunchHarness) -> Vec<String> {
    use spaceterm_core::hierarchy::Mutation;
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
}

#[gpui::test]
fn blank_and_cancelled_creation_names_stay_automatic_on_the_machine(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::Outcome;

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
    assert_eq!(harness.workspace_count(cx), 2);
    assert_ne!(active_core_workspace(&harness, cx).unwrap(), WORKSPACE);
    assert_eq!(harness.authoritative_inventory(cx), inventory());
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
    cx.simulate_keystrokes("cmd-n cmd-o");
    cx.run_until_parked();

    assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    assert!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.pin_picker.is_none())
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
    cx.simulate_keystrokes("cmd-n cmd-o");
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
    use spaceterm_core::hierarchy::{Mutation, Outcome};

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

#[gpui::test]
fn a_refused_first_workspace_creation_keeps_the_window_and_its_notice(cx: &mut TestAppContext) {
    let empty = Inventory {
        revision: 1,
        repositories: None,
        close: None,
        workspaces: Vec::new(),
    };
    let (harness, cx) = launch_harness_with(vec![ready()], ScriptedEdits::new(Some(empty)), [], cx);
    cx.run_until_parked();
    sync(&harness, cx);
    assert_eq!(harness.workspace_count(cx), 0);
    cx.dispatch_action(crate::ui::NewWorkspace);
    cx.run_until_parked();
    assert_eq!(
        harness.workspace_count(cx),
        1,
        "creation appears before acknowledgement"
    );
    harness
        .edits
        .answer(spaceterm_core::hierarchy::Outcome::Limit);
    sync(&harness, cx);
    assert_eq!(
        cx.windows().len(),
        1,
        "a refused creation must leave its empty window open"
    );
    assert_eq!(harness.workspace_count(cx), 0);
    let action = cx
        .debug_bounds("modal-action-workspace-refused-ok")
        .expect("the refusal remains readable");
    cx.simulate_click(action.center(), Modifiers::none());
    cx.run_until_parked();
    cx.dispatch_action(crate::ui::NewWorkspace);
    cx.run_until_parked();
    assert_eq!(
        created_names(&harness).len(),
        2,
        "the empty window remains usable after dismissal"
    );
    assert_eq!(harness.workspace_count(cx), 1);
}

fn active_core_workspace(harness: &LaunchHarness, cx: &mut VisualTestContext) -> Option<[u8; 32]> {
    harness.manager.read_with(cx, |manager, _| {
        let local = manager.workspaces.active_workspace_id()?;
        manager
            .binding
            .local()?
            .facts(local)
            .map(|facts| facts.core)
    })
}

#[gpui::test]
fn the_new_workspace_menu_creates_and_opens_a_machine_workspace(cx: &mut TestAppContext) {
    let (harness, cx) = ready_harness(cx);
    click_new_workspace_menu("new-workspace-menu-create-local", cx);
    assert_eq!(created_names(&harness), [""]);
    assert_eq!(
        harness.workspace_count(cx),
        2,
        "the pending Workspace appears before the server answers"
    );
    let pending = harness.manager.read_with(cx, |manager, _| {
        manager.workspaces.active_workspace_id().unwrap()
    });
    assert_ne!(active_core_workspace(&harness, cx).unwrap(), WORKSPACE);
    assert_eq!(harness.authoritative_inventory(cx), inventory());
    assert_eq!(harness.panes.opened().len(), 1);

    accept_creation(&harness, None, cx);

    assert_eq!(harness.workspace_count(cx), 2);
    assert_eq!(
        harness.manager.read_with(cx, |manager, _| {
            manager.workspaces.active_workspace_id()
        }),
        Some(pending),
        "acceptance binds the pending Workspace without replacing its local identity"
    );
    assert_eq!(active_core_workspace(&harness, cx), Some(CREATED));
    assert_eq!(
        harness.panes.pane([63; 32]).created_starts(),
        1,
        "the person's own creation starts its Pane after the server binds it"
    );
    assert_eq!(
        harness.panes.pane([63; 32]).selections(),
        0,
        "binding a creation must not select it again to acquire control or start a retained shell"
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
    assert_eq!(harness.workspace_count(cx), 2);
    assert_ne!(active_core_workspace(&harness, cx).unwrap(), WORKSPACE);
    accept_creation(&harness, None, cx);
    assert_eq!(harness.workspace_count(cx), 2);
    assert_eq!(active_core_workspace(&harness, cx), Some(CREATED));
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
    use spaceterm_core::hierarchy::Mutation;

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
            control: spaceterm_core::hierarchy::CreateControl::Creator,
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
    assert_eq!(harness.rows(cx), 1);

    cx.cx
        .update(crate::ui::workspace_manager::resume_waiting_launches);
    cx.run_until_parked();
    assert_eq!(harness.launch.steps(), ["run", "run"]);
}

/// A Start from the status menu reaches a window that launched while the server was stopped, and
/// a Stop from there leaves it waiting.
#[gpui::test]
fn a_status_menu_start_resumes_a_window_waiting_on_a_stopped_machine(cx: &mut TestAppContext) {
    use spaceterm_core::service::ServiceStatus;

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
    assert_eq!(harness.rows(cx), 1);
}

/// Stopping the server from Settings while its Workspaces are open leaves each Pane saying so,
/// with Start SpaceTerm Core where retrying could never connect.
fn stopped_in_session(cx: &mut TestAppContext) -> (LaunchHarness, &mut VisualTestContext) {
    stopped_in_session_as(
        crate::terminal::core_session::CorePaneStatus::Failed(CoreError::Stopped),
        cx,
    )
}

/// Like `stopped_in_session`, with the Pane left in `status` when the server stopped.
fn stopped_in_session_as(
    status: crate::terminal::core_session::CorePaneStatus,
    cx: &mut TestAppContext,
) -> (LaunchHarness, &mut VisualTestContext) {
    let (harness, cx) = ready_harness(cx);
    harness.launch.answer(ready());
    harness.panes.pane(PANE).set(status);
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    (harness, cx)
}

#[gpui::test]
fn a_pane_stopped_in_session_offers_start_core_instead_of_retry(cx: &mut TestAppContext) {
    let (harness, cx) = stopped_in_session(cx);
    assert!(cx.debug_bounds("pane-start-server-1").is_some());
    assert!(cx.debug_bounds("pane-retry-1").is_none());

    harness.click("pane-start-server-1", cx);

    assert_eq!(harness.launch.steps(), ["run", "start", "run"]);
}

/// Once Start SpaceTerm Core in a Pane starts the server, a Pane it stopped connects again, so the
/// server says what became of it. Nothing takes control or starts a shell on the person's behalf.
fn assert_start_reconnects(
    status: crate::terminal::core_session::CorePaneStatus,
    cx: &mut TestAppContext,
) {
    let (harness, cx) = stopped_in_session_as(status, cx);
    let pane = harness.panes.pane(PANE);

    harness.click("pane-start-server-1", cx);

    assert_eq!(
        (pane.retries(), pane.take_overs(), pane.restored_starts()),
        (1, 0, 0)
    );
}

#[gpui::test]
fn starting_the_core_from_a_pane_reconnects_its_stopped_pane(cx: &mut TestAppContext) {
    assert_start_reconnects(
        crate::terminal::core_session::CorePaneStatus::Failed(CoreError::Stopped),
        cx,
    );
}

/// A Pane this window did not control offers Take Over again once the server answers, rather
/// than waiting on a connection that never retries.
#[gpui::test]
fn starting_the_core_reconnects_a_stopped_take_over_pane_without_taking_control(
    cx: &mut TestAppContext,
) {
    assert_start_reconnects(
        crate::terminal::core_session::CorePaneStatus::TakeoverFailed(CoreError::Stopped),
        cx,
    );
}

#[gpui::test]
fn a_refused_start_while_presenting_explains_itself(cx: &mut TestAppContext) {
    let (harness, cx) = stopped_in_session(cx);
    harness.launch.refuse_start(CoreError::NotInstalled);

    harness.click("pane-start-server-1", cx);

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

/// A launch that runs again but has not answered yet, as when a person retries.
fn launching_after_stop(
    answers: Vec<Result<LaunchOutcome, CoreError>>,
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
    assert_eq!(harness.rows(cx), 1);
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
    assert_eq!(harness.rows(cx), 0);
}

const HIDDEN_TAB: [u8; 32] = [34; 32];
const HIDDEN_PANE: [u8; 32] = [35; 32];
const BOOT: [u8; 32] = [36; 32];

/// What a server publishes for a shell running `command` in `directory`.
fn running(
    revision: u64,
    command: &str,
    directory: &str,
) -> spaceterm_core::pane::metadata::Snapshot {
    use spaceterm_core::pane::metadata::{
        Command, CommandState, DirectoryProvenance, Freshness, Progress, PromptZone, Snapshot,
        TitleGlyph, TitleProvenance,
    };
    Snapshot {
        revision,
        shell_name: "sh".to_owned(),
        title: "Terminal".to_owned(),
        title_provenance: TitleProvenance::Fallback,
        directory: directory.to_owned(),
        directory_provenance: DirectoryProvenance::Osc7,
        user: Some("tester".to_owned()),
        host: Some("server".to_owned()),
        home: Some("/home/tester".to_owned()),
        freshness: Freshness::Live,
        finished_commands: 0,
        prompt_zone: PromptZone::CommandOutput,
        command: Some(Command {
            line: command.to_owned(),
            state: CommandState::Running,
        }),
        title_activity: false,
        title_glyph: TitleGlyph::Reported,
        progress: Progress::None,
    }
}

/// The server's Workspace with a second Tab this client never opens, whose Pane runs `session`
/// and published `presentation`.
fn with_hidden_tab(
    revision: u64,
    session: Option<[u8; 32]>,
    presentation: Option<spaceterm_core::pane::metadata::Snapshot>,
) -> Inventory {
    use spaceterm_core::hierarchy::{CloseInventory, ConfirmedPane};
    let mut inventory = inventory();
    inventory.revision = revision;
    inventory.workspaces[0].tabs.push(Tab {
        worktree: None,
        worktree_membership: spaceterm_core::hierarchy::WorktreeMembership::Following,
        worktree_repository: None,
        id: HIDDEN_TAB,
        root_pane: HIDDEN_PANE,
        panes: vec![Pane {
            id: HIDDEN_PANE,
            source: None,
            side: Side::Right,
            starting_directory: PathBuf::from("/server/home"),
            current_directory: None,
            repository_directory: None,
            presentation: presentation.map(Box::new),
            observers: Default::default(),
            confirmed_directory: None,
            creator: None,
        }],
    });
    inventory.close = Some(CloseInventory {
        controls: Vec::new(),
        boot: BOOT,
        sessions: vec![
            ConfirmedPane {
                id: PANE,
                session: None,
            },
            ConfirmedPane {
                id: HIDDEN_PANE,
                session,
            },
        ],
    });
    inventory
}

/// Each Tab the strip publishes, as its name and the place it names.
fn tab_identities(cx: &mut VisualTestContext) -> Vec<(String, String)> {
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    tree.with_role("Tab")
        .iter()
        .map(|tab| {
            (
                tab["aria"]["label"].as_str().unwrap_or_default().to_owned(),
                tab["aria"]["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect()
}

#[gpui::test]
fn a_tab_this_client_never_opened_names_what_its_machine_shell_runs(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![ready()], cx);
    cx.run_until_parked();
    harness.edits.set_inventory(with_hidden_tab(
        2,
        Some([41; 32]),
        Some(running(3, "cargo test", "/srv/api")),
    ));
    sync(&harness, cx);

    assert_eq!(
        tab_identities(cx),
        [
            ("Terminal".to_owned(), String::new()),
            ("cargo test".to_owned(), "api".to_owned()),
        ]
    );
    assert_eq!(
        harness.panes.pane(HIDDEN_PANE).selections(),
        0,
        "presenting the Tab neither selects nor claims its Pane"
    );

    harness.edits.set_inventory(with_hidden_tab(
        3,
        Some([41; 32]),
        Some(running(4, "cargo build", "/srv/web")),
    ));
    sync(&harness, cx);
    assert_eq!(
        tab_identities(cx)[1],
        ("cargo build".to_owned(), "web".to_owned()),
        "a metadata-only publication reaches the hidden Tab"
    );

    let mut completed = running(5, "cargo build", "/srv/web");
    completed.command.as_mut().unwrap().state =
        spaceterm_core::pane::metadata::CommandState::Finished {
            exit_status: Some(0),
            duration_ms: 100,
        };
    harness
        .edits
        .set_inventory(with_hidden_tab(4, Some([41; 32]), Some(completed)));
    sync(&harness, cx);
    assert_eq!(tab_identities(cx)[1], ("sh".to_owned(), "web".to_owned()));
    assert_eq!(harness.panes.pane(HIDDEN_PANE).selections(), 0);
}

#[gpui::test]
fn a_new_core_session_replaces_the_former_sessions_metadata(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness(vec![ready()], cx);
    cx.run_until_parked();
    harness.edits.set_inventory(with_hidden_tab(
        2,
        Some([41; 32]),
        Some(running(9, "cargo test", "/srv/api")),
    ));
    sync(&harness, cx);

    // The new Session numbers its metadata from its own start, below the former Session's.
    harness.edits.set_inventory(with_hidden_tab(
        3,
        Some([42; 32]),
        Some(running(1, "make", "/srv/web")),
    ));
    sync(&harness, cx);
    assert_eq!(tab_identities(cx)[1], ("make".to_owned(), "web".to_owned()));

    // A Pane whose Session ended presents nothing of it.
    harness.edits.set_inventory(with_hidden_tab(4, None, None));
    sync(&harness, cx);
    assert_eq!(
        tab_identities(cx)[1],
        ("Terminal".to_owned(), String::new())
    );
}

const OTHER_WORKSPACE: [u8; 32] = [37; 32];
const OTHER_TAB: [u8; 32] = [38; 32];
const OTHER_PANE: [u8; 32] = [39; 32];
const OTHER_HIDDEN_TAB: [u8; 32] = [40; 32];
const OTHER_HIDDEN_PANE: [u8; 32] = [43; 32];

/// The server's first Workspace, and a second one a new client does not attach, whose second Tab
/// runs `cargo test` in `/srv/api`.
fn with_detached_workspace() -> Inventory {
    use spaceterm_core::hierarchy::ConfirmedPane;
    let pane = |id, presentation: Option<spaceterm_core::pane::metadata::Snapshot>| Pane {
        id,
        source: None,
        side: Side::Right,
        starting_directory: PathBuf::from("/server/home"),
        current_directory: None,
        repository_directory: None,
        presentation: presentation.map(Box::new),
        observers: Default::default(),
        confirmed_directory: None,
        creator: None,
    };
    let tab = |id, root_pane, pane| Tab {
        worktree: None,
        worktree_membership: spaceterm_core::hierarchy::WorktreeMembership::Following,
        worktree_repository: None,
        id,
        root_pane,
        panes: vec![pane],
    };
    let mut inventory = with_hidden_tab(2, None, None);
    inventory.workspaces.push(Workspace {
        id: OTHER_WORKSPACE,
        name: "Other".to_owned(),
        name_version: 0,
        pin_version: 0,
        pinned_directory: None,
        root_tab: OTHER_TAB,
        tabs: vec![
            tab(OTHER_TAB, OTHER_PANE, pane(OTHER_PANE, None)),
            tab(
                OTHER_HIDDEN_TAB,
                OTHER_HIDDEN_PANE,
                pane(
                    OTHER_HIDDEN_PANE,
                    Some(running(5, "cargo test", "/srv/api")),
                ),
            ),
        ],
    });
    let close = inventory.close.as_mut().expect("close facts");
    close.sessions.push(ConfirmedPane {
        id: OTHER_PANE,
        session: None,
    });
    close.sessions.push(ConfirmedPane {
        id: OTHER_HIDDEN_PANE,
        session: Some([44; 32]),
    });
    inventory
}

#[gpui::test]
fn a_reattached_workspace_names_what_its_hidden_tab_runs_from_an_unchanged_inventory(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = launch_harness(vec![ready()], cx);
    cx.run_until_parked();
    harness.edits.set_inventory(with_detached_workspace());
    sync(&harness, cx);
    let attach = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            harness.manager.update(cx, |manager, cx| {
                manager.attach_detached_workspace(MachineKey::LOCAL, OTHER_WORKSPACE, window, cx);
            });
        });
        cx.run_until_parked();
    };
    attach(cx);
    let other = harness.manager.read_with(cx, |manager, _| {
        manager
            .binding
            .get(MachineKey::LOCAL)
            .and_then(|binding| binding.local(OTHER_WORKSPACE))
            .expect("the attached Workspace is presented")
    });
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.detach_workspace(other, window, cx);
        });
    });
    cx.run_until_parked();

    attach(cx);
    // The server publishes nothing new, so each poll reports the same inventory.
    sync(&harness, cx);

    assert_eq!(
        tab_identities(cx),
        [
            ("Terminal".to_owned(), String::new()),
            ("cargo test".to_owned(), "api".to_owned()),
        ]
    );
    assert_eq!(
        harness.panes.pane(OTHER_HIDDEN_PANE).selections(),
        0,
        "presenting the hidden Tab neither selects nor claims its Pane"
    );
}

/// The root Pane runs a Session that reports `percent` through the inventory.
fn with_root_progress(revision: u64, metadata_revision: u64, percent: u8) -> Inventory {
    use spaceterm_core::hierarchy::{CloseInventory, ConfirmedPane};
    let mut inventory = inventory();
    inventory.revision = revision;
    let mut presentation = running(metadata_revision, "cargo build", "/srv/api");
    presentation.progress = spaceterm_core::pane::metadata::Progress::Normal(percent);
    inventory.workspaces[0].tabs[0].panes[0].presentation = Some(Box::new(presentation));
    inventory.close = Some(CloseInventory {
        controls: Vec::new(),
        boot: BOOT,
        sessions: vec![ConfirmedPane {
            id: PANE,
            session: Some([45; 32]),
        }],
    });
    inventory
}

/// What each Terminal Session status in the window says.
fn session_statuses(cx: &mut VisualTestContext) -> Vec<String> {
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    tree.with_role("Status")
        .iter()
        .filter(|node| node["aria"]["label"] == "Terminal Session status")
        .map(|node| {
            node["aria"]["value"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

#[gpui::test]
fn a_pane_another_client_controls_keeps_showing_its_sessions_progress(cx: &mut TestAppContext) {
    use crate::terminal::core_session::CorePaneStatus;
    let (harness, cx) = launch_harness(vec![ready()], cx);
    cx.run_until_parked();
    harness.edits.set_inventory(with_root_progress(2, 3, 40));
    sync(&harness, cx);

    harness.panes.pane(PANE).set(CorePaneStatus::Takeover);
    cx.run_until_parked();
    harness.edits.set_inventory(with_root_progress(3, 4, 60));
    sync(&harness, cx);
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("pane-takeover-1").is_some(),
        "typing still belongs to the other client"
    );
    assert!(
        session_statuses(cx).contains(&"Busy, 60%".to_owned()),
        "the server's live progress still shows: {:?}",
        session_statuses(cx)
    );
}

#[gpui::test]
fn observer_caption_renders_names_and_clears_without_a_control_change(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;
    let (harness, cx) = launch_harness(vec![ready()], cx);
    cx.run_until_parked();
    assert!(cx.debug_bounds("pane-caption-observers-1").is_none());
    for (names, others, expected) in [
        (vec!["Build CLI"], 0, "Observed by Build CLI"),
        (
            vec!["Build CLI", "Review CLI"],
            2,
            "Observed by Build CLI and 3 others",
        ),
    ] {
        let mut value = serde_json::to_value(inventory()).unwrap();
        value["workspaces"][0]["tabs"][0]["panes"][0]["observers"] =
            serde_json::json!({ "names": names, "others": others });
        harness
            .edits
            .set_inventory(serde_json::from_value(value).unwrap());
        sync(&harness, cx);
        assert!(cx.debug_bounds("pane-caption-observers-1").is_some());
        assert!(A11yTree::read(cx).find(expected).is_some(), "{expected}");
    }
    harness.edits.set_inventory(inventory());
    sync(&harness, cx);
    assert!(cx.debug_bounds("pane-caption-observers-1").is_none());
    assert!(
        A11yTree::read(cx)
            .find("Observed by Build CLI and 3 others")
            .is_none()
    );
}

/// A window reopened with the keyboard in its sidebar keeps it there while launch connects to
/// this computer's server and its Workspaces arrive. Neither the launch screen nor the first
/// Workspace takes the keyboard, and no Pane is opened for the person.
#[gpui::test]
fn a_reopened_window_keeps_the_keyboard_in_its_sidebar_through_launch(cx: &mut TestAppContext) {
    use crate::ui::remembered_view::{SidebarView, WindowView};

    let remembered = WindowView {
        placement: None,
        sidebar: SidebarView {
            visible: true,
            width: gpui::px(240.0),
            focused: true,
            focus_visible: true,
        },
        active_workspace: None,
        workspaces: std::collections::BTreeMap::new(),
    };
    let (harness, cx) = launch_harness_restoring(
        vec![ready()],
        ScriptedEdits::new(Some(inventory())),
        [],
        Some(remembered),
        cx,
    );
    cx.run_until_parked();

    assert_eq!(harness.rows(cx), 1);
    assert!(cx.update(|window, cx| {
        harness
            .manager
            .read(cx)
            .sidebar
            .read(cx)
            .has_visible_focus(window)
    }));
    assert_eq!(harness.panes.pane(PANE).selections(), 0);
}

const WORK: [u8; 32] = [41; 32];
const WORK_ROOT_TAB: [u8; 32] = [42; 32];
const WORK_ROOT_PANE: [u8; 32] = [43; 32];
const WORK_TAB: [u8; 32] = [44; 32];
const WORK_TAB_ROOT: [u8; 32] = [45; 32];
const WORK_SPLIT: [u8; 32] = [46; 32];

fn server_pane(id: [u8; 32], source: Option<[u8; 32]>) -> Pane {
    Pane {
        id,
        source,
        side: Side::Right,
        starting_directory: PathBuf::from("/server/home"),
        current_directory: None,
        repository_directory: None,
        presentation: None,
        observers: Default::default(),
        confirmed_directory: None,
        creator: None,
    }
}

fn server_tab(id: [u8; 32], panes: Vec<Pane>) -> Tab {
    Tab {
        worktree: None,
        worktree_membership: spaceterm_core::hierarchy::WorktreeMembership::Following,
        worktree_repository: None,
        id,
        root_pane: panes[0].id,
        panes,
    }
}

/// The Default Workspace and a second Workspace, Work, whose second Tab is split in two.
fn two_workspaces() -> Inventory {
    let mut inventory = inventory();
    inventory.workspaces.push(Workspace {
        id: WORK,
        name: "Work".to_owned(),
        name_version: 0,
        pin_version: 0,
        pinned_directory: None,
        root_tab: WORK_ROOT_TAB,
        tabs: vec![
            server_tab(WORK_ROOT_TAB, vec![server_pane(WORK_ROOT_PANE, None)]),
            server_tab(
                WORK_TAB,
                vec![
                    server_pane(WORK_TAB_ROOT, None),
                    server_pane(WORK_SPLIT, Some(WORK_TAB_ROOT)),
                ],
            ),
        ],
    });
    inventory
}

fn work_split() -> spaceterm_core::client_view::Selection {
    spaceterm_core::client_view::Selection {
        workspace: WORK,
        tab: WORK_TAB,
        pane: WORK_SPLIT,
    }
}

/// The person's launch of SpaceTerm after an earlier run that left `attached` attached and
/// `saved` as this computer's selection. `before` runs once the window is open and before this
/// computer's Workspaces arrive.
fn relaunch<'a>(
    saved: Option<spaceterm_core::client_view::Selection>,
    attached: &[[u8; 32]],
    before: impl FnOnce(&mut WorkspaceManager, &mut Window, &mut Context<WorkspaceManager>),
    cx: &'a mut TestAppContext,
) -> (LaunchHarness, &'a mut VisualTestContext) {
    let launch = ScriptedLaunch::default();
    launch.answer(ready());
    let (harness, cx) = foreground_harness_with(launch, ScriptedEdits::new(None), cx);
    sync(&harness, cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            let binding = manager
                .binding
                .get_mut(MachineKey::LOCAL)
                .expect("this computer's server is presented");
            binding.attach_for_test(attached);
            if let Some(saved) = saved {
                binding.retain_selection_for_test(saved);
            }
            before(manager, window, cx);
        });
    });
    harness.edits.set_inventory(two_workspaces());
    sync(&harness, cx);
    (harness, cx)
}

/// What the window presents of its Active Workspace, by server identity.
fn presented_selection(
    harness: &LaunchHarness,
    cx: &mut VisualTestContext,
) -> Option<spaceterm_core::client_view::Selection> {
    harness.manager.read_with(cx, |manager, cx| {
        manager.active_tab_manager()?.read(cx).selection(cx)
    })
}

/// A full relaunch returns to the split Pane of a second Tab in a second Workspace, and starts
/// only that Pane's shell for the person.
#[gpui::test]
fn the_persons_launch_reopens_the_saved_workspace_tab_and_split(cx: &mut TestAppContext) {
    let (harness, cx) = relaunch(Some(work_split()), &[WORKSPACE, WORK], |_, _, _| {}, cx);

    assert_eq!(harness.rows(cx), 2);
    assert_eq!(presented_selection(&harness, cx), Some(work_split()));
    assert_eq!(harness.panes.pane(WORK_SPLIT).selections(), 1);
    for other in [PANE, WORK_ROOT_PANE, WORK_TAB_ROOT] {
        assert_eq!(harness.panes.pane(other).selections(), 0);
    }
    assert!(harness.edits.submitted().is_empty(), "nothing is created");
    sync(&harness, cx);
    assert_eq!(
        harness.panes.pane(WORK_SPLIT).selections(),
        1,
        "the launch opens the Pane once"
    );
}

/// What the window presents from this computer becomes the next launch's selection, and another
/// choice the person makes replaces it.
#[gpui::test]
fn the_presented_selection_is_retained_for_the_next_launch(cx: &mut TestAppContext) {
    let (harness, cx) = relaunch(Some(work_split()), &[WORKSPACE, WORK], |_, _, _| {}, cx);
    let saved = |harness: &LaunchHarness, cx: &mut VisualTestContext| {
        harness
            .manager
            .read_with(cx, |manager, _| manager.binding.local()?.saved_selection())
    };
    assert_eq!(saved(&harness, cx), Some(work_split()));

    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            let default = manager
                .binding
                .local()
                .and_then(|binding| binding.local(WORKSPACE))
                .expect("the Default Workspace is presented");
            manager.activate_workspace(default, window, cx);
            manager.save_arrangements(cx);
        });
    });
    assert_eq!(
        saved(&harness, cx),
        Some(spaceterm_core::client_view::Selection {
            workspace: WORKSPACE,
            tab: TAB,
            pane: PANE,
        })
    );
}

/// A saved Tab that is gone leaves its Workspace's Root Tab, and a saved Workspace this client
/// detached, or none at all, leaves the window's own Active Workspace. Each opens one shell.
#[gpui::test]
fn a_saved_selection_that_is_gone_falls_back_without_attaching_anything(cx: &mut TestAppContext) {
    let gone_tab = spaceterm_core::client_view::Selection {
        tab: [99; 32],
        ..work_split()
    };
    let (harness, cx) = relaunch(Some(gone_tab), &[WORKSPACE, WORK], |_, _, _| {}, cx);
    assert_eq!(
        presented_selection(&harness, cx),
        Some(spaceterm_core::client_view::Selection {
            workspace: WORK,
            tab: WORK_ROOT_TAB,
            pane: WORK_ROOT_PANE,
        })
    );
    assert_eq!(harness.panes.pane(WORK_ROOT_PANE).selections(), 1);
    assert_eq!(harness.panes.pane(WORK_SPLIT).selections(), 0);
}

#[gpui::test]
fn a_saved_selection_in_a_detached_workspace_leaves_it_detached(cx: &mut TestAppContext) {
    let (harness, cx) = relaunch(Some(work_split()), &[WORKSPACE], |_, _, _| {}, cx);
    assert_eq!(harness.rows(cx), 1, "Work stays detached");
    assert_eq!(harness.panes.pane(PANE).selections(), 1);
    assert_eq!(harness.panes.opens(WORK_SPLIT), 0);
}

#[gpui::test]
fn a_launch_without_a_saved_selection_opens_the_active_workspace(cx: &mut TestAppContext) {
    let (harness, cx) = relaunch(None, &[WORKSPACE, WORK], |_, _, _| {}, cx);
    assert_eq!(harness.rows(cx), 2);
    assert_eq!(harness.panes.pane(PANE).selections(), 1);
    assert_eq!(harness.panes.pane(WORK_SPLIT).selections(), 0);
}

/// The person's input while this computer's Workspaces are on the way is their choice, so the
/// saved selection never takes the view or the keyboard from it.
#[gpui::test]
fn a_choice_before_the_workspaces_arrive_keeps_the_saved_selection_away(cx: &mut TestAppContext) {
    let (harness, cx) = relaunch(
        Some(work_split()),
        &[WORKSPACE, WORK],
        |manager, _, cx| manager.person_input(cx),
        cx,
    );
    assert_eq!(
        presented_selection(&harness, cx).map(|selection| selection.workspace),
        Some(WORKSPACE)
    );
    assert_eq!(harness.panes.pane(WORK_SPLIT).selections(), 0);
}

/// A window that opens for anything but the person's own launch never opens a saved selection.
#[gpui::test]
fn a_passive_launch_leaves_the_saved_selection_unopened(cx: &mut TestAppContext) {
    let (harness, cx) = launch_harness_with(vec![ready()], ScriptedEdits::new(None), [], cx);
    sync(&harness, cx);
    cx.update(|_, cx| {
        harness.manager.update(cx, |manager, _| {
            let binding = manager
                .binding
                .get_mut(MachineKey::LOCAL)
                .expect("this computer's server is presented");
            binding.attach_for_test(&[WORKSPACE, WORK]);
            binding.retain_selection_for_test(work_split());
        });
    });
    harness.edits.set_inventory(two_workspaces());
    sync(&harness, cx);
    assert_eq!(harness.panes.pane(WORK_SPLIT).selections(), 0);
    assert_ne!(
        presented_selection(&harness, cx).map(|selection| selection.workspace),
        Some(WORK)
    );
    // The default it presents is nobody's choice, so the next launch still opens the split.
    sync(&harness, cx);
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.binding.local()?.saved_selection()),
        Some(work_split())
    );
}

/// Input before the Workspaces arrive that leaves the keyboard in the sidebar keeps it there:
/// the late landing neither opens the saved selection nor takes the keyboard.
#[gpui::test]
fn a_late_landing_leaves_the_keyboard_where_early_input_put_it(cx: &mut TestAppContext) {
    let (harness, cx) = relaunch(
        Some(work_split()),
        &[WORKSPACE, WORK],
        |manager, window, cx| {
            manager.sidebar.update(cx, |sidebar, cx| {
                sidebar.take_keyboard(true, window, cx);
            });
            manager.person_input(cx);
        },
        cx,
    );
    assert_eq!(harness.panes.pane(WORK_SPLIT).selections(), 0);
    assert!(
        cx.update(|window, cx| harness.manager.read(cx).sidebar.read(cx).is_focused(window)),
        "the sidebar keeps the keyboard"
    );
}

/// A first launch on this computer, whose attachments the server's first inventory initializes
/// with a Workspace the server already holds: input that leaves the keyboard in the sidebar
/// before that inventory arrives keeps it there, and the first view opens no Pane.
#[gpui::test]
fn a_first_view_leaves_the_keyboard_where_early_input_put_it(cx: &mut TestAppContext) {
    let launch = ScriptedLaunch::default();
    launch.answer(ready());
    let (harness, cx) = foreground_harness_with(launch, ScriptedEdits::new(None), cx);
    sync(&harness, cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.sidebar.update(cx, |sidebar, cx| {
                sidebar.take_keyboard(true, window, cx);
            });
            manager.person_input(cx);
        });
    });
    harness.edits.set_inventory(two_workspaces());
    sync(&harness, cx);
    assert!(
        cx.update(|window, cx| harness.manager.read(cx).sidebar.read(cx).is_focused(window)),
        "the sidebar keeps the keyboard"
    );
    for pane in [PANE, WORK_ROOT_PANE, WORK_TAB_ROOT, WORK_SPLIT] {
        assert!(
            harness.panes.opens(pane) == 0 || harness.panes.pane(pane).selections() == 0,
            "{pane:?} is not selected"
        );
    }
    assert!(harness.edits.submitted().is_empty(), "nothing is created");
    sync(&harness, cx);
    assert!(
        cx.update(|window, cx| harness.manager.read(cx).sidebar.read(cx).is_focused(window)),
        "a later reading leaves it there too"
    );
}

/// Without early input, the same first view opens the server's first Workspace and its Pane.
#[gpui::test]
fn a_first_view_without_early_input_opens_the_first_workspace(cx: &mut TestAppContext) {
    let launch = ScriptedLaunch::default();
    launch.answer(ready());
    let (harness, cx) = foreground_harness_with(launch, ScriptedEdits::new(None), cx);
    sync(&harness, cx);
    harness.edits.set_inventory(two_workspaces());
    sync(&harness, cx);
    assert_eq!(harness.panes.pane(PANE).selections(), 1);
    assert!(harness.edits.submitted().is_empty(), "nothing is created");
}

/// Runs the window's start queue for a while.
fn run_start_queue(cx: &mut VisualTestContext) {
    for _ in 0..20 {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(16));
        cx.run_until_parked();
    }
}

/// After a relaunch, SpaceTerm Core restarts. Every retained Pane, including the ones nobody
/// opened in this run, has a worker that waits for a background start turn, and the window
/// gives those turns at most three at once. Only the Pane the person opened is selected.
#[gpui::test]
fn retained_panes_nobody_opened_take_bounded_background_turns_after_a_restart(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = relaunch(Some(work_split()), &[WORKSPACE, WORK], |_, _, _| {}, cx);
    run_start_queue(cx);
    let panes = [PANE, WORK_ROOT_PANE, WORK_TAB_ROOT, WORK_SPLIT];
    for pane in panes {
        assert_eq!(harness.panes.opens(pane), 1, "{pane:?} has one worker");
    }

    // SpaceTerm Core restarts, and each worker sees its Pane restored without a shell.
    for pane in panes {
        let pane = harness.panes.pane(pane);
        pane.admitted();
        pane.await_turn();
    }
    let turns = |cx: &mut VisualTestContext| {
        run_start_queue(cx);
        panes
            .into_iter()
            .filter(|pane| harness.panes.pane(*pane).holds_turn())
            .collect::<Vec<_>>()
    };
    let first = turns(cx);
    assert_eq!(first.len(), 3, "three background starts run at once");
    for pane in &first {
        harness.panes.pane(*pane).finish_turn();
    }
    let rest = turns(cx);
    assert_eq!(rest.len(), 1, "the last Pane gets the next turn");
    assert!(!first.contains(&rest[0]));
    assert_eq!(harness.panes.pane(WORK_SPLIT).selections(), 1);
    for pane in [PANE, WORK_ROOT_PANE, WORK_TAB_ROOT] {
        assert_eq!(harness.panes.pane(pane).selections(), 0);
    }
}

/// This computer's Machine as Machine updates read it, with `link`.
fn local_machine(
    link: crate::updates::machines::MachineLink,
) -> crate::updates::machines::MachineUpdate {
    crate::updates::machines::MachineUpdate {
        link,
        ..too_old_core(|_| {})
    }
}

/// Machine updates that follow this computer's Core, as the application installs them.
fn following_local_core(
    cx: &mut TestAppContext,
) -> impl Fn(crate::updates::machines::MachineLink, &mut VisualTestContext) + use<> {
    let machines = scripted_machines(cx);
    cx.update(crate::ui::workspace_manager::follow_local_core_runs);
    move |link, cx: &mut VisualTestContext| {
        machines.emit(crate::updates::machines::MachineUpdatesEvent::Changed(
            Box::new(local_machine(link)),
        ));
        cx.run_until_parked();
    }
}

/// A window that opened while this computer's Core was deliberately stopped, as a replacement
/// window does, opens its Workspaces once Machine updates confirm the Core answers again, as
/// after a Start from the command line. Reading it stopped or unreachable keeps the Stop, and
/// a later answer from the same run resumes nothing again.
#[gpui::test]
fn a_command_line_start_opens_a_window_that_opened_while_stopped(cx: &mut TestAppContext) {
    use crate::updates::machines::MachineLink;
    let read = following_local_core(cx);
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::Stopped), ready()], cx);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));

    read(MachineLink::Stopped, cx);
    read(MachineLink::Offline { since: 1 }, cx);
    assert_eq!(
        harness.launch.steps(),
        ["run"],
        "a stopped Core stays stopped"
    );
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));

    read(MachineLink::Connected, cx);
    assert_eq!(
        harness.launch.steps(),
        ["run", "connect"],
        "it only connects, with no Start of its own"
    );
    assert_eq!(harness.launch_title(cx), None);
    assert_eq!(harness.rows(cx), 1);
    assert!(harness.panes.was_opened(PANE), "its retained Pane opens");

    read(MachineLink::Connected, cx);
    assert_eq!(harness.launch.steps(), ["run", "connect"]);
}

/// This computer's Machine as Machine updates read it, with `link` and the facts of Core run
/// `boot`.
fn local_run(
    link: crate::updates::machines::MachineLink,
    boot: &str,
) -> crate::updates::machines::MachineUpdate {
    crate::updates::machines::MachineUpdate {
        boot: Some(boot.into()),
        ..local_machine(link)
    }
}

/// Machine updates that follow this computer's Core, as the application installs them, and
/// read Core run `boot` answering before any window opens.
fn following_local_run(
    boot: &str,
    cx: &mut TestAppContext,
) -> (
    Rc<crate::updates::machines::testing::ScriptedMachines>,
    impl Fn(crate::updates::machines::MachineUpdate, &mut VisualTestContext) + use<>,
) {
    use crate::updates::machines::{MachineLink, MachineUpdatesEvent};
    let machines = scripted_machines(cx);
    cx.update(crate::ui::workspace_manager::follow_local_core_runs);
    machines.emit(MachineUpdatesEvent::Changed(Box::new(local_run(
        MachineLink::Connected,
        boot,
    ))));
    cx.run_until_parked();
    let emitter = Rc::clone(&machines);
    (machines, move |update, cx: &mut VisualTestContext| {
        emitter.emit(MachineUpdatesEvent::Changed(Box::new(update)));
        cx.run_until_parked();
    })
}

/// A window that reopens while this computer's Core is deliberately stopped reads the Stop
/// itself, even when Machine updates never read the Core stopped because a Start from the
/// command line came first. The window waits past the Core run it saw answering, so another
/// answer of that run resumes nothing, and the new run's first answer opens its retained Pane
/// with one connection and no Start of its own.
#[gpui::test]
fn a_window_that_read_the_stop_itself_opens_when_a_new_core_run_answers(cx: &mut TestAppContext) {
    use crate::updates::machines::MachineLink;
    let (_machines, read) = following_local_run("before-stop", cx);
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::Stopped), ready()], cx);
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));

    read(local_run(MachineLink::Connected, "before-stop"), cx);
    read(local_machine(MachineLink::Connected), cx);
    assert_eq!(
        harness.launch.steps(),
        ["run"],
        "the run it saw, or a reading of no run, keeps the Stop"
    );
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));

    read(local_run(MachineLink::Connected, "after-start"), cx);
    assert_eq!(
        harness.launch.steps(),
        ["run", "connect"],
        "it only connects, with no Start of its own"
    );
    assert_eq!(harness.launch.intents(), [launch::LaunchIntent::Passive]);
    assert_eq!(harness.launch_title(cx), None);
    assert_eq!(harness.rows(cx), 1);
    assert!(harness.panes.was_opened(PANE), "its retained Pane opens");

    read(local_run(MachineLink::Connected, "after-start"), cx);
    read(
        local_run(MachineLink::Offline { since: 1 }, "after-start"),
        cx,
    );
    read(local_run(MachineLink::Connected, "after-start"), cx);
    assert_eq!(
        harness.launch.steps(),
        ["run", "connect"],
        "once per Core run"
    );
}

/// A new Core run that Machine updates read while the window's launch was still reading the
/// Stop resumes that window once, whether the reading arrives before the Stop applies, with no
/// later reading, or after it. The iterations order the two both ways.
#[gpui::test(iterations = 20)]
fn a_core_run_read_while_the_launch_read_the_stop_resumes_at_once(cx: &mut TestAppContext) {
    use crate::updates::machines::{MachineLink, MachineUpdatesEvent};
    let (machines, read) = following_local_run("before-stop", cx);
    let events = machines
        .events()
        .expect("the store follows the scripted Machines");
    let launch = ScriptedLaunch::default();
    launch.answer(Ok(LaunchOutcome::Stopped));
    launch.answer(ready());
    launch.during_next_run(move || {
        let _ = events.try_send(MachineUpdatesEvent::Changed(Box::new(local_run(
            MachineLink::Connected,
            "after-start",
        ))));
    });
    let (harness, cx) = scripted_harness(
        launch,
        ScriptedEdits::new(Some(inventory())),
        [],
        None,
        launch::LaunchIntent::Passive,
        cx,
    );
    cx.run_until_parked();
    assert_eq!(
        harness.launch.steps(),
        ["run", "connect"],
        "it only connects, with no Start of its own"
    );
    assert_eq!(harness.launch_title(cx), None);
    assert!(harness.panes.was_opened(PANE), "its retained Pane opens");

    read(local_run(MachineLink::Connected, "after-start"), cx);
    assert_eq!(
        harness.launch.steps(),
        ["run", "connect"],
        "once per Core run"
    );
}

/// A Core run that answers and then exits before the waiting window connects is never started
/// or installed by that window: its connection only reads, and the window keeps saying the Core
/// is stopped. Another answer of the gone run connects nothing again, so nothing spins. A later
/// run that a Stop from the command line ends first leaves the window as it was too, and the
/// next run that still runs opens its retained Pane, one connection per run.
#[gpui::test]
fn a_core_run_gone_before_the_window_connects_is_never_started(cx: &mut TestAppContext) {
    use crate::updates::machines::MachineLink;
    use spaceterm_core::protocol::version::Incompatible;
    let (_machines, read) = following_local_run("before-stop", cx);
    let (harness, cx) = launch_harness(
        vec![
            Ok(LaunchOutcome::Stopped),
            Err(CoreError::NotRunning),
            Ok(LaunchOutcome::Stopped),
            ready(),
        ],
        cx,
    );
    cx.run_until_parked();
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));

    read(local_run(MachineLink::Connected, "gone"), cx);
    assert_eq!(
        harness.launch.steps(),
        ["run", "connect"],
        "the gone run is never started"
    );
    assert_eq!(harness.launch.intents(), [launch::LaunchIntent::Passive]);
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));
    assert!(!harness.panes.was_opened(PANE));

    read(local_run(MachineLink::Connected, "gone"), cx);
    read(
        local_run(
            MachineLink::Incompatible(Incompatible::ListenerTooOld),
            "gone",
        ),
        cx,
    );
    assert_eq!(
        harness.launch.steps(),
        ["run", "connect"],
        "the gone run connects once"
    );

    read(local_run(MachineLink::Connected, "stopped-again"), cx);
    assert_eq!(harness.launch.steps(), ["run", "connect", "connect"]);
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));

    read(local_run(MachineLink::Connected, "after-start"), cx);
    assert_eq!(
        harness.launch.steps(),
        ["run", "connect", "connect", "connect"],
        "only connections, with no Start of its own"
    );
    assert_eq!(harness.launch_title(cx), None);
    assert!(harness.panes.was_opened(PANE), "its retained Pane opens");
}

/// A window whose connection finds the running Core refuses it says so, as launch does, and
/// starts nothing.
#[gpui::test]
fn a_core_run_that_refuses_the_connection_says_it_is_incompatible(cx: &mut TestAppContext) {
    use crate::updates::machines::MachineLink;
    use spaceterm_core::protocol::version::Incompatible;
    let (_machines, read) = following_local_run("before-stop", cx);
    let (harness, cx) = launch_harness(
        vec![
            Ok(LaunchOutcome::Stopped),
            Err(CoreError::IncompatibleProtocol(
                Incompatible::ListenerTooOld,
            )),
        ],
        cx,
    );
    cx.run_until_parked();

    read(local_run(MachineLink::Connected, "after-start"), cx);
    assert_eq!(harness.launch.steps(), ["run", "connect"]);
    assert!(harness.manager.read_with(cx, |manager, _| matches!(
        manager.launch.state,
        launch::LaunchState::Incompatible(Incompatible::ListenerTooOld)
    )));
}

/// A window that Machine updates resume after reading the Core stopped and answering again only
/// connects, and a Core gone by then leaves it stopped, with nothing started. The next Start
/// from the command line connects it.
#[gpui::test]
fn a_command_line_start_gone_before_an_open_window_connects_starts_nothing(
    cx: &mut TestAppContext,
) {
    use crate::updates::machines::MachineLink;
    let read = following_local_core(cx);
    let (harness, cx) = launch_harness(
        vec![
            Ok(LaunchOutcome::Stopped),
            Err(CoreError::NotRunning),
            ready(),
        ],
        cx,
    );
    cx.run_until_parked();

    read(MachineLink::Stopped, cx);
    read(MachineLink::Connected, cx);
    assert_eq!(harness.launch.steps(), ["run", "connect"]);
    assert_eq!(harness.launch_title(cx), Some(launch::STOPPED_TITLE));

    read(MachineLink::Connected, cx);
    assert_eq!(harness.launch.steps(), ["run", "connect"]);

    read(MachineLink::Stopped, cx);
    read(MachineLink::Connected, cx);
    assert_eq!(harness.launch.steps(), ["run", "connect", "connect"]);
    assert_eq!(harness.launch_title(cx), None);
    assert!(harness.panes.was_opened(PANE), "its retained Pane opens");
}

/// A reading that names no Core run, as Machine updates list before the Core's first answer,
/// resumes no launch, even after an unreachable reading.
#[gpui::test]
fn a_core_that_answers_again_without_a_stop_resumes_nothing(cx: &mut TestAppContext) {
    use crate::updates::machines::MachineLink;
    let read = following_local_core(cx);
    let (harness, cx) = launch_harness(vec![Ok(LaunchOutcome::Stopped), ready()], cx);
    cx.run_until_parked();

    read(MachineLink::Offline { since: 1 }, cx);
    read(MachineLink::Connected, cx);
    assert_eq!(harness.launch.steps(), ["run"]);
}

/// A window that stayed open through a Stop and a Start from the command line reconnects every
/// Pane the Stop left waiting, in its Active Workspace and in a background attached Workspace,
/// including Panes another client controlled. Nothing takes control, starts a shell or runs a
/// Start, and nothing reconnects while the Core is still stopped or after it already answered.
#[gpui::test]
fn a_command_line_start_reconnects_the_stopped_panes_of_an_open_window(cx: &mut TestAppContext) {
    use crate::terminal::core_session::CorePaneStatus;
    use crate::updates::machines::MachineLink;
    let read = following_local_core(cx);
    let (harness, cx) = ready_harness(cx);
    harness.edits.set_inventory(with_detached_workspace());
    sync(&harness, cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.attach_detached_workspace(MachineKey::LOCAL, OTHER_WORKSPACE, window, cx);
        });
    });
    cx.run_until_parked();
    let active = harness
        .manager
        .read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
    let panes = [harness.panes.pane(PANE), harness.panes.pane(OTHER_PANE)];
    for pane in &panes {
        pane.set(CorePaneStatus::TakeoverFailed(CoreError::Stopped));
    }
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let counts = |panes: &[std::sync::Arc<crate::ui::workspaces::testing::ScriptedPane>; 2]| {
        panes
            .iter()
            .map(|pane| (pane.retries(), pane.take_overs(), pane.restored_starts()))
            .collect::<Vec<_>>()
    };

    read(MachineLink::Stopped, cx);
    read(MachineLink::Offline { since: 1 }, cx);
    assert_eq!(counts(&panes), [(0, 0, 0); 2], "the Core is still stopped");

    read(MachineLink::Connected, cx);
    assert_eq!(counts(&panes), [(1, 0, 0); 2]);
    assert_eq!(harness.launch.steps(), ["run"], "no Start of its own");
    assert_eq!(
        harness.manager.read_with(cx, |manager, _| {
            manager.workspaces.active_workspace_id()
        }),
        active,
        "the Active Workspace stays"
    );

    read(MachineLink::Connected, cx);
    assert_eq!(counts(&panes), [(1, 0, 0); 2], "once per Core run");
}
