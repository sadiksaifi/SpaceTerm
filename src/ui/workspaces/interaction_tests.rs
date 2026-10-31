use super::*;
use spaceterm_ui::a11y_testing::A11yTree;

fn split_inventory() -> Inventory {
    inventory(
        2,
        vec![tab(
            ROOT_TAB,
            vec![
                pane(ROOT_PANE, None, Axis::Right),
                pane(SPLIT_PANE, Some(ROOT_PANE), Axis::Right),
            ],
        )],
    )
}

#[gpui::test]
fn keyboard_navigation_focuses_an_already_visible_takeover_screen(cx: &mut TestAppContext) {
    let (harness, cx) = tab_manager(cx);
    publish(&harness, split_inventory(), cx);
    harness
        .panes
        .pane(SPLIT_PANE)
        .set(ServerPaneStatus::Takeover);
    settle(&harness, cx);
    cx.dispatch_action(crate::ui::FocusNextPane);
    cx.run_until_parked();
    assert!(
        !cx.update(|window, cx| harness
            .manager
            .read(cx)
            .focused_terminal_is_focused(window, cx)),
        "the hidden terminal must not take keyboard focus"
    );
    cx.simulate_keystrokes("tab");
    let focused = A11yTree::read(cx)
        .focused()
        .map(|node| node["aria"]["label"].clone());
    assert_eq!(focused, Some(serde_json::json!("Take Over")));
}

#[gpui::test]
fn initiating_a_split_restores_zoom_before_the_server_replies(cx: &mut TestAppContext) {
    let (harness, cx) = tab_manager(cx);
    publish(&harness, split_inventory(), cx);
    cx.dispatch_action(crate::ui::TogglePaneZoom);
    cx.run_until_parked();
    cx.dispatch_action(crate::ui::SplitDown);
    cx.run_until_parked();
    assert_eq!(
        harness.manager.read_with(cx, |manager, cx| manager
            .active_tab_view()
            .read(cx)
            .zoom_state()),
        crate::domain::ZoomState::Restored
    );
    assert!(matches!(
        harness.edits.submitted().last(),
        Some(Mutation::SplitPane {
            axis: Axis::Down,
            ..
        })
    ));
}

#[gpui::test]
fn a_split_checks_the_restored_allocation_before_submitting(cx: &mut TestAppContext) {
    let (harness, cx) = tab_manager(cx);
    // Other clients may add membership beyond this client's viewport. That publication is accepted.
    let mut panes = vec![pane(ROOT_PANE, None, Axis::Right)];
    for id in 20..35 {
        panes.push(pane([id; 32], Some(ROOT_PANE), Axis::Right));
    }
    publish(&harness, inventory(2, vec![tab(ROOT_TAB, panes)]), cx);
    cx.dispatch_action(crate::ui::TogglePaneZoom);
    cx.run_until_parked();
    let source = harness.panes.pane(ROOT_PANE);
    let selections = source.selections();
    click("pane-split-right-1", cx);
    assert!(
        harness.edits.submitted().is_empty(),
        "an initiating split must fit before requesting membership"
    );
    assert_eq!(
        pane_count(&harness, cx),
        16,
        "another client's membership stays authoritative"
    );
    assert_eq!(
        source.selections(),
        selections,
        "a refused split starts no source shell"
    );
}
