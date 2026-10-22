use super::*;

#[gpui::test]
fn disconnect_drops_a_parked_remote_presentation_without_closing_other_servers(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = MachineGroupsHarness::new(cx);
    let (remote, edits) = harness.present_ssh("penguin", &[("Far", 1)], cx);
    let mut inventory = machine_inventory(900, &[("Far", 1)]);
    inventory.close = Some(spaceterm_server::hierarchy::CloseInventory {
        boot: [1; 32],
        sessions: vec![spaceterm_server::hierarchy::ConfirmedPane {
            id: [101; 32],
            session: None,
        }],
    });
    edits.set_inventory(inventory);
    let far = harness.named("Far", cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.sync_shared_workspaces(remote, window, cx);
            manager.request_shared_close(
                crate::close_confirmation::CloseTarget::Workspace(far),
                window,
                cx,
            );
        });
    });
    click("modal-action-shared-close-confirm", cx);
    harness.manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.parked_ids().collect::<Vec<_>>(), [far]);
    });
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.withdraw_server_workspaces(remote, window, cx);
        });
    });
    harness.manager.read_with(cx, |manager, _| {
        assert!(manager.workspaces.parked_ids().next().is_none());
        assert!(manager.shared.get(remote).is_none());
    });
    assert_eq!(harness.names(cx), ["Default"]);
}

#[gpui::test]
fn a_refused_workspace_close_restores_its_views_without_overwriting_newer_focus(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = shared_close_harness(cx);
    let original = harness.manager.read_with(cx, |manager, _| {
        manager
            .workspaces
            .workspace(SHARED_WORKSPACE_ROW)
            .unwrap()
            .payload()
            .entity_id()
    });
    cx.dispatch_action(crate::ui::CloseWorkspace);
    cx.run_until_parked();
    click("modal-action-shared-close-confirm", cx);
    assert_eq!(
        harness.shared_rows(cx),
        0,
        "approved close hides the Workspace before its reply"
    );
    right_click("workspace-row-1-active", cx);
    click("workspace-menu-row-rename", cx);
    harness
        .edits
        .answer(spaceterm_server::hierarchy::Outcome::StaleConfirmation);
    harness.sync(cx);
    assert_eq!(harness.shared_rows(cx), 1);
    assert_eq!(
        harness.manager.read_with(cx, |manager, _| manager
            .workspaces
            .workspace(SHARED_WORKSPACE_ROW)
            .unwrap()
            .payload()
            .entity_id()),
        original
    );
    // Dismiss the explicit stale-approval notice, which must return to the newer editor.
    click("modal-action-shared-close-dismiss", cx);
    assert!(shared_close_rename_is_focused(&harness, cx));
}

/// A disconnect discards an edit still queued behind one the server has not answered. Its notice
/// arrives while a person types a newer rename, which an offline server would refuse, so the
/// draft stays open and unsent beneath the notice and takes the keyboard back after it.
#[gpui::test]
fn a_disconnect_notice_keeps_a_newer_rename_draft_and_its_focus(cx: &mut TestAppContext) {
    use spaceterm_server::hierarchy::Mutation;

    let (harness, cx) = shared_close_harness(cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            for name in ["Logs", "Notes"] {
                assert!(manager.rename_shared_workspace(
                    SHARED_WORKSPACE_ROW,
                    name,
                    Some(0),
                    window,
                    cx
                ));
            }
        });
    });
    assert_eq!(
        harness.edits.submitted().len(),
        1,
        "the second edit waits behind the first"
    );
    right_click("workspace-row-2-active", cx);
    click("workspace-menu-row-rename", cx);
    cx.simulate_keystrokes("d r a f t");
    cx.run_until_parked();

    harness.edits.set_connected(false);
    harness.sync(cx);
    assert!(
        cx.debug_bounds("modal-action-shared-workspace-refused-ok")
            .is_some(),
        "the discarded edit is explained"
    );
    click("modal-action-shared-workspace-refused-ok", cx);

    assert!(shared_close_rename_is_focused(&harness, cx));
    let draft = cx.update(|_, cx| {
        harness
            .manager
            .read(cx)
            .sidebar
            .read(cx)
            .rename_input()
            .map(|input| input.read(cx).value().to_owned())
    });
    assert_eq!(draft.as_deref(), Some("draft"));
    assert!(
        cx.debug_bounds("modal-action-shared-workspace-unavailable-ok")
            .is_none(),
        "the unsent draft is not refused"
    );
    assert_eq!(
        harness.edits.submitted(),
        vec![Mutation::RenameWorkspace {
            workspace: SHARED_CLOSE_WORKSPACE,
            version: 0,
            name: "Logs".to_owned(),
        }]
    );
}

#[gpui::test]
fn close_frames_hide_retired_pane_status_and_completion_preserves_sidebar_editing(
    cx: &mut TestAppContext,
) {
    use crate::terminal::server_session::DevelopmentPaneStatus;
    let (harness, cx) = shared_close_harness(cx);
    harness.panes.pane(SHARED_CLOSE_SECOND_PANE).reconnect();
    settle_shared_panes(cx);
    cx.dispatch_action(crate::ui::CloseTab);
    cx.run_until_parked();
    click("modal-action-shared-close-confirm", cx);
    assert_eq!(shared_close_counts(&harness, cx), (1, 1));
    for status in [
        DevelopmentPaneStatus::Takeover,
        DevelopmentPaneStatus::TakeoverReconnecting,
        DevelopmentPaneStatus::Ended,
    ] {
        harness.panes.pane(SHARED_CLOSE_ROOT_PANE).set(status);
        settle_shared_panes(cx);
        assert!(cx.debug_bounds("shared-pane-takeover-1").is_none());
        assert!(cx.debug_bounds("shared-pane-notice-1").is_none());
        assert_eq!(shared_close_counts(&harness, cx), (1, 1));
    }
    right_click("workspace-row-1-inactive", cx);
    click("workspace-menu-row-rename", cx);
    harness
        .edits
        .answer(spaceterm_server::hierarchy::Outcome::Accepted {
            target: SHARED_CLOSE_ROOT_TAB,
        });
    harness.publish(
        shared_close_inventory(
            2,
            &[(SHARED_CLOSE_SECOND_TAB, &[(SHARED_CLOSE_SECOND_PANE, None)])],
        ),
        cx,
    );
    assert!(shared_close_rename_is_focused(&harness, cx));
}

#[gpui::test]
fn final_workspace_close_dismisses_on_publication_without_acknowledgement(cx: &mut TestAppContext) {
    use spaceterm_server::hierarchy::Inventory;
    let (harness, cx) = shared_close_harness(cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.close_workspace_activating(WorkspaceId::new(1), false, window, cx)
        })
    });
    cx.dispatch_action(crate::ui::CloseWorkspace);
    cx.run_until_parked();
    click("modal-action-shared-close-confirm", cx);
    assert_eq!(harness.shared_rows(cx), 0);
    // Publish directly: after this synchronization the Workspace Window must already be gone.
    harness.edits.set_inventory(Inventory {
        revision: 2,
        repositories: None,
        workspaces: Vec::new(),
        close: None,
    });
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.sync_shared_workspaces(ServerKey::LOCAL, window, cx)
        })
    });
    cx.run_until_parked();
    assert!(
        cx.windows().is_empty(),
        "authoritative final close dismisses without waiting for acknowledgement"
    );
}

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
