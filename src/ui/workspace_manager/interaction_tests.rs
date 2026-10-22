use super::*;

#[gpui::test]
fn explicit_sidebar_open_selects_the_restored_focused_pane(cx: &mut TestAppContext) {
    let (harness, cx) = shared_pane_harness(cx);
    let pane = harness.panes.pane(SHARED_FOCUS_PANE);
    pane.set(crate::terminal::server_session::DevelopmentPaneStatus::Restored);
    settle_shared_panes(cx);
    let before = pane.selections();
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.toggle_sidebar_focus(window, cx);
            manager.handle_sidebar_event(
                SidebarEvent::Activate {
                    workspace_id: SHARED_WORKSPACE_ROW,
                    focus_pane: false,
                },
                window,
                cx,
            );
        });
    });
    assert_eq!(
        pane.selections(),
        before,
        "passive navigation starts no shell"
    );
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.handle_sidebar_event(SidebarEvent::FocusPane, window, cx);
        });
    });
    assert_eq!(
        pane.selections(),
        before + 1,
        "Return explicitly opens the Focused Pane"
    );
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.handle_sidebar_event(
                SidebarEvent::Activate {
                    workspace_id: SHARED_WORKSPACE_ROW,
                    focus_pane: true,
                },
                window,
                cx,
            );
        });
    });
    assert_eq!(
        pane.selections(),
        before + 2,
        "a committed row click also explicitly opens it"
    );
}

#[gpui::test]
fn an_idle_tab_controlled_by_another_client_still_requires_confirmation(cx: &mut TestAppContext) {
    let (harness, cx) = shared_close_harness(cx);
    harness
        .panes
        .pane(SHARED_CLOSE_SECOND_PANE)
        .set(crate::terminal::server_session::DevelopmentPaneStatus::Takeover);
    settle_shared_panes(cx);
    cx.dispatch_action(crate::ui::ActivateTab2);
    cx.dispatch_action(crate::ui::CloseTab);
    cx.run_until_parked();
    assert!(harness.closes().is_empty());
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_some()
    );
}

#[gpui::test]
fn unavailable_commands_on_a_replacement_screen_terminate_without_redispatch(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = shared_pane_harness(cx);
    harness
        .panes
        .pane(SHARED_FOCUS_PANE)
        .set(crate::terminal::server_session::DevelopmentPaneStatus::Takeover);
    settle_shared_panes(cx);
    for action in [
        crate::ui::OpenTerminalFind.boxed_clone(),
        crate::ui::FindNext.boxed_clone(),
        crate::ui::ShowRepositoryStatus.boxed_clone(),
    ] {
        cx.update(|window, cx| window.dispatch_action(action, cx));
        cx.run_until_parked();
    }
    assert_eq!(harness.panes.pane(SHARED_FOCUS_PANE).take_overs(), 0);
    assert!(cx.debug_bounds("shared-pane-takeover-1").is_some());
}

#[gpui::test]
fn closing_an_idle_own_tab_submits_without_repetitive_confirmation(cx: &mut TestAppContext) {
    let (harness, cx) = shared_close_harness(cx);
    harness.panes.pane(SHARED_CLOSE_SECOND_PANE).reconnect();
    settle_shared_panes(cx);
    cx.dispatch_action(crate::ui::ActivateTab2);
    cx.run_until_parked();
    cx.dispatch_action(crate::ui::CloseTab);
    cx.run_until_parked();
    assert_eq!(harness.closes().len(), 1, "idle own Tab closes directly");
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );
}
