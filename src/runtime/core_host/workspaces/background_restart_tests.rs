//! The three-Pane restart and offline window-replacement sequence from issue #768.
use super::*;
use crate::terminal::core_session::{
    CorePaneStatus,
    push_tests::{ConnectedPane, await_status, paired_connection},
};
use crate::terminal::geometry::{
    BackingScale, CellGridSize, LogicalCellSize, LogicalSize, TerminalGeometry,
};
use crate::terminal::session::{TerminalAppearanceUpdate, TerminalSessionFactory as _};
use crate::ui::core_pane::{CorePaneScreen, CorePaneView, DormantCopy};
use spaceterm_core::direct::{DirectConnection, DirectConnector, DirectError};

struct Away;
impl DirectConnector for Away {
    fn connect(&self) -> Result<DirectConnection, DirectError> {
        Err(DirectError::Transport)
    }
}

#[test]
fn three_panes_observe_restart_after_the_window_reopens_offline() {
    let mut original = crate::runtime::directory_testing::terminal_host();
    original.prepare_initial_organization().unwrap();
    let workspace = original.state.inventory.workspaces[0].id;
    let tab = original.state.inventory.workspaces[0].tabs[0].id;
    let root = original.state.inventory.workspaces[0].tabs[0].root_pane;
    let mut source = root;
    for (sequence, axis) in [
        (1, spaceterm_core::hierarchy::Axis::Right),
        (2, spaceterm_core::hierarchy::Axis::Down),
    ] {
        let Reply::Outcome(outcome) = original.request(
            [1; 32],
            [1; 32],
            Request::Mutate {
                sequence,
                mutation: Mutation::SplitPane {
                    workspace,
                    tab,
                    source,
                    axis,
                    control: CreateControl::Creator,
                },
            },
        ) else {
            panic!("split did not return an outcome")
        };
        let Outcome::Accepted { target } = outcome.outcome else {
            panic!("split was refused")
        };
        source = target;
    }
    let panes: Vec<_> = original.state.inventory.workspaces[0].tabs[0]
        .panes
        .iter()
        .map(|pane| pane.id)
        .collect();
    assert_eq!(panes.len(), 3);
    let organization: Organization =
        serde_json::from_slice(&serde_json::to_vec(&original.state.envelope()).unwrap()).unwrap();
    let restored = || {
        let mut host = crate::runtime::directory_testing::terminal_host();
        host.restore_organization(Some(&organization));
        host
    };
    let core = ConnectedPane::with_host(paired_connection(Arc::new(Away)), original);
    let open = |start: bool| {
        panes
            .iter()
            .map(|&pane| {
                let (factory, state) = core.pane(pane);
                let started = factory
                    .start(
                        TerminalGeometry::from_viewport(
                            LogicalSize::new(640.0, 432.0),
                            LogicalCellSize::new(8.0, 18.0),
                            BackingScale::ONE,
                            CellGridSize::new(2, 2),
                        ),
                        TerminalAppearanceUpdate::builtin().unwrap(),
                    )
                    .unwrap();
                if start {
                    state.select();
                    await_status(&state, CorePaneStatus::Connected);
                }
                (started, state)
            })
            .collect::<Vec<_>>()
    };
    let window = open(true);
    assert_eq!(core.sessions().len(), 3);
    // The last split is focused. The first two Panes stay unfocused through both restarts.
    window[2].0.handle.focus(true);
    core.stop_core();
    core.restore_core(restored());
    (core.reconnect)();
    for (_, state) in &window {
        await_status(state, CorePaneStatus::Restored);
    }
    assert!(
        core.sessions().is_empty(),
        "restart started a shell without first use"
    );
    window[2].1.start_restored();
    await_status(&window[2].1, CorePaneStatus::Connected);
    assert_eq!(core.sessions().len(), 1);

    // Close the Workspace window, stop the server, then reopen the window while offline.
    drop(window);
    core.stop_core();
    let reopened = open(false);
    reopened[2].0.handle.focus(true);
    reopened[2].0.handle.focus(false);
    for (_, state) in &reopened {
        await_status(
            state,
            CorePaneStatus::Failed(CoreError::Direct(DirectError::Transport)),
        );
    }
    // Settings does not select either background Pane. The saved route owner reconnects.
    let starts = core.starts();
    core.restore_core(restored());
    (core.reconnect)();
    assert!(
        core.connection().live_link().is_some(),
        "the paired route is connected"
    );
    // The route owner continues even if a cohort of Pane claims closes its first returning
    // connection. Its next backoff step outlasts the Pane's three one-second retries.
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until
        && core.connection().live_link().is_some()
        && reopened
            .iter()
            .any(|(_, state)| state.status() != CorePaneStatus::Restored)
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    if core.connection().live_link().is_none() {
        std::thread::sleep(Duration::from_secs(4));
        (core.reconnect)();
    }
    assert!(
        core.connection().live_link().is_some(),
        "the route recovered independently of its Panes"
    );
    reopened[2].0.handle.focus(true);
    for (index, (_, state)) in reopened.iter().enumerate() {
        let until = Instant::now() + Duration::from_secs(10);
        while state.status() != CorePaneStatus::Restored && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            core.connection().live_link().is_some(),
            "the paired route stays connected"
        );
        let mut view = CorePaneView::new(state.clone());
        view.sync();
        let Some(CorePaneScreen::Restored {
            starting: false,
            unconfirmed: false,
            offered: true,
            ended,
        }) = view.screen()
        else {
            panic!(
                "Pane {index} on the connected route did not offer Start Session: status={:?}, screen={:?}, notice={:?}",
                state.status(),
                view.screen(),
                view.notice()
            );
        };
        let copy = DormantCopy::of(ended);
        assert_eq!(
            (copy.title, copy.label),
            ("Server Restarted", "Start Session")
        );
    }
    assert_eq!(
        core.starts(),
        starts,
        "reconnection submitted a shell start"
    );
    assert!(
        core.sessions().is_empty(),
        "reconnection started a shell without first use"
    );
}
