use super::*;

#[gpui::test]
fn rapid_idle_tab_closes_park_the_final_workspace_and_restore_the_refused_tab(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::{ConfirmedControl, Mutation, Outcome};
    let (harness, cx) = shared_close_harness(cx);
    let mut inventory = shared_close_inventory(
        2,
        &[
            (SHARED_CLOSE_ROOT_TAB, &[(SHARED_CLOSE_ROOT_PANE, None)]),
            (SHARED_CLOSE_SECOND_TAB, &[(SHARED_CLOSE_SECOND_PANE, None)]),
        ],
    );
    inventory.close.as_mut().unwrap().controls = Some(
        [SHARED_CLOSE_ROOT_PANE, SHARED_CLOSE_SECOND_PANE]
            .into_iter()
            .map(|pane| ConfirmedControl {
                pane,
                session: None,
                owner: [1; 32],
                epoch: 1,
            })
            .collect(),
    );
    harness.publish(inventory.clone(), cx);
    harness.panes.pane(SHARED_CLOSE_SECOND_PANE).reconnect();
    settle_shared_panes(cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.close_workspace_activating(WorkspaceId::new(1), false, window, cx);
        })
    });
    let manager = harness.manager.read_with(cx, |manager, _| {
        manager.workspaces.active().payload().clone()
    });
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    let surviving_view = manager.read_with(cx, |manager, _| manager.active_tab_view().entity_id());
    let surviving_tab = manager.read_with(cx, |manager, cx| manager.active_terminal_identity(cx).0);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    assert_eq!(
        cx.windows().len(),
        1,
        "combined optimistic removal cannot dismiss the rollback host"
    );
    harness.manager.read_with(cx, |workspace, _| {
        assert_eq!(
            workspace.workspaces.parked_ids().collect::<Vec<_>>(),
            [SHARED_WORKSPACE_ROW]
        );
    });
    assert_eq!(harness.shared_rows(cx), 0);
    assert_eq!(
        harness.closes().len(),
        1,
        "the second direct close waits for the first reply"
    );
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );

    harness.edits.answer(Outcome::Accepted {
        target: SHARED_CLOSE_ROOT_TAB,
    });
    inventory.workspaces[0].tabs.remove(0);
    inventory.workspaces[0].root_tab = SHARED_CLOSE_SECOND_TAB;
    inventory.revision += 1;
    inventory
        .close
        .as_mut()
        .unwrap()
        .sessions
        .retain(|pane| pane.id == SHARED_CLOSE_SECOND_PANE);
    inventory
        .close
        .as_mut()
        .unwrap()
        .controls
        .as_mut()
        .unwrap()
        .retain(|control| control.pane == SHARED_CLOSE_SECOND_PANE);
    harness.publish(inventory, cx);
    assert_eq!(cx.windows().len(), 1);
    assert_eq!(harness.shared_rows(cx), 0);
    assert_eq!(harness.closes().len(), 2);
    assert!(harness.closes().iter().all(|mutation| matches!(mutation, Mutation::Close { confirmation, .. } if !confirmation.removes_workspace)));

    harness.edits.answer(Outcome::StaleConfirmation);
    harness.sync(cx);
    assert_eq!(harness.shared_rows(cx), 1);
    assert_eq!(cx.windows().len(), 1);
    harness.manager.read_with(cx, |workspace, _| {
        assert!(workspace.workspaces.parked_ids().next().is_none());
        assert_eq!(
            workspace.workspaces.active().payload().entity_id(),
            manager.entity_id()
        );
    });
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.tab_ids()),
        [surviving_tab]
    );
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.active_tab_view().entity_id()),
        surviving_view
    );
    assert_eq!(harness.closes().len(), 2);
    click("modal-action-shared-close-review", cx);
    assert!(
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
            .find("Kill Workspace")
            .is_some()
    );
    assert_eq!(
        harness.closes().len(),
        2,
        "Workspace removal waits for fresh explicit consent"
    );
}

#[gpui::test]
fn batched_conflict_notices_are_reviewed_one_at_a_time_beyond_the_modal_queue_limit(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::Outcome;
    let (harness, cx) = attachment_harness(cx);
    cx.update(|_, cx| {
        let source = harness.manager.update(cx, |manager, _| {
            let shared = manager.shared.local_mut().unwrap();
            for index in 0..12 {
                let workspace = if index % 2 == 0 {
                    WorkspaceId::new(2)
                } else {
                    WorkspaceId::new(3)
                };
                assert_eq!(
                    shared.rename(workspace, format!("queued-draft-{index}-end"), Some(0)),
                    crate::ui::shared_workspaces::SharedSubmission::Submitted
                );
            }
            shared.edits()
        });
        let mut inventory = attachment_inventory();
        inventory.revision += 1;
        for (index, workspace) in inventory.workspaces.iter_mut().enumerate() {
            workspace.name = format!("Server value {index}");
            workspace.name_version = 1;
        }
        harness.edits.set_inventory(inventory);
        for _ in 0..12 {
            harness.edits.answer(Outcome::Conflict);
            source.snapshot();
        }
    });
    for index in 0..12 {
        harness.sync(cx);
        let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
        let expected = format!("queued-draft-{index}-end");
        assert!(
            tree.in_order()
                .iter()
                .any(|node| node["aria"].to_string().contains(&expected)),
            "every rejected draft must get its own visible review: {expected}"
        );
        // Another reading while the notice is open must leave the next draft retained.
        harness.sync(cx);
        let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
        assert!(
            tree.in_order()
                .iter()
                .any(|node| node["aria"].to_string().contains(&expected))
        );
        click("modal-action-shared-workspace-conflict-keep", cx);
    }
    harness.sync(cx);
    assert!(
        cx.debug_bounds("modal-action-shared-workspace-conflict-keep")
            .is_none()
    );
    assert_eq!(
        harness.edits.submitted().len(),
        12,
        "review never retries a draft automatically"
    );
}

#[gpui::test]
fn disconnect_drops_a_parked_remote_presentation_without_closing_other_servers(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = MachineGroupsHarness::new(cx);
    let (remote, edits) = harness.present_ssh("penguin", &[("Far", 1)], cx);
    let mut inventory = machine_inventory(900, &[("Far", 1)]);
    inventory.close = Some(spaceterm_server::hierarchy::CloseInventory {
        controls: None,
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

/// Another client splits the shared Workspace while Kill Workspace is open. The person reviews
/// the change and approves the fresh confirmation, so the keyboard their first request held moves
/// at once to the Workspace that takes its place, and the server's answer leaves it there.
#[gpui::test]
fn a_reviewed_workspace_close_moves_the_keyboard_to_the_rendered_survivor(cx: &mut TestAppContext) {
    use spaceterm_server::hierarchy::{Inventory, Outcome};

    let (harness, cx) = shared_close_harness(cx);
    assert!(shared_close_terminal_is_focused(&harness, cx));
    cx.dispatch_action(crate::ui::CloseWorkspace);
    redraw(cx);
    harness.publish(
        shared_close_inventory(
            2,
            &[
                (
                    SHARED_CLOSE_ROOT_TAB,
                    &[
                        (SHARED_CLOSE_ROOT_PANE, Some([31; 32])),
                        (SHARED_CLOSE_SPLIT_PANE, Some([32; 32])),
                        (SHARED_CLOSE_NEW_PANE, None),
                    ],
                ),
                (SHARED_CLOSE_SECOND_TAB, &[(SHARED_CLOSE_SECOND_PANE, None)]),
            ],
        ),
        cx,
    );
    assert!(
        cx.debug_bounds("modal-action-shared-close-review")
            .is_some(),
        "the stale confirmation is withdrawn with a Review"
    );

    click("modal-action-shared-close-review", cx);
    redraw(cx);
    click("modal-action-shared-close-confirm", cx);
    redraw(cx);
    assert_eq!(harness.closes().len(), 1, "the fresh review is approved");
    assert_eq!(harness.shared_rows(cx), 0);
    assert!(
        shared_close_terminal_is_focused(&harness, cx),
        "the surviving Workspace's Pane takes the keyboard before the server answers"
    );

    harness.edits.answer(Outcome::Accepted {
        target: SHARED_CLOSE_WORKSPACE,
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
    assert!(shared_close_terminal_is_focused(&harness, cx));
}

#[gpui::test]
fn a_reviewed_close_keeps_a_newer_rename_through_confirmation_and_acceptance(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::{Inventory, Mutation, Outcome};

    let (harness, cx) = shared_close_harness(cx);
    cx.dispatch_action(crate::ui::CloseWorkspace);
    cx.run_until_parked();
    click("modal-action-shared-close-confirm", cx);
    right_click("workspace-row-1-active", cx);
    click("workspace-menu-row-rename", cx);
    cx.simulate_keystrokes("d r a f t");
    cx.run_until_parked();
    let input = harness.manager.read_with(cx, |manager, cx| {
        manager
            .sidebar
            .read(cx)
            .rename_input()
            .expect("the newer draft")
            .clone()
    });
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.sync(cx);
    click("modal-action-shared-close-review", cx);
    // Let the old notice finish its deferred focus and rename-hold cleanup while the new
    // confirmation remains open.
    for _ in 0..3 {
        redraw(cx);
    }
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_some()
    );
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        "draft"
    );
    assert!(
        harness
            .manager
            .read_with(cx, |manager, cx| manager.sidebar.read(cx).is_renaming()),
        "the fresh confirmation retains the draft after the notice releases its hold"
    );
    click("modal-action-shared-close-confirm", cx);
    assert_eq!(harness.shared_rows(cx), 0);
    assert!(shared_close_rename_is_focused(&harness, cx));
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        "draft"
    );
    assert_eq!(
        harness.closes().len(),
        2,
        "only explicit fresh consent submits a second close"
    );
    assert!(
        harness
            .edits
            .submitted()
            .iter()
            .all(|mutation| matches!(mutation, Mutation::Close { .. })),
        "the newer draft is never submitted as a blur"
    );
    harness.edits.answer(Outcome::Accepted {
        target: SHARED_CLOSE_WORKSPACE,
    });
    harness.publish(
        Inventory {
            revision: 2,
            repositories: None,
            close: None,
            workspaces: Vec::new(),
        },
        cx,
    );
    assert!(shared_close_rename_is_focused(&harness, cx));
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        "draft"
    );
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
    let mut inventory = shared_close_published();
    let control = spaceterm_server::hierarchy::ConfirmedControl {
        pane: SHARED_CLOSE_SECOND_PANE,
        session: None,
        owner: [1; 32],
        epoch: 7,
    };
    inventory.close.as_mut().unwrap().controls = Some(vec![control.clone()]);
    harness.publish(inventory, cx);
    harness.panes.pane(SHARED_CLOSE_SECOND_PANE).reconnect();
    settle_shared_panes(cx);
    cx.dispatch_action(crate::ui::ActivateTab2);
    cx.run_until_parked();
    cx.dispatch_action(crate::ui::CloseTab);
    cx.run_until_parked();
    assert_eq!(harness.closes().len(), 1, "idle own Tab closes directly");
    assert!(matches!(harness.closes().as_slice(),
        [spaceterm_server::hierarchy::Mutation::Close { confirmation, .. }]
            if confirmation.own_controls.as_deref() == Some(&[control][..])
    ));
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );
}

#[gpui::test]
fn an_idle_own_tab_on_an_older_server_requires_explicit_confirmation(cx: &mut TestAppContext) {
    let (harness, cx) = shared_close_harness(cx);
    harness.panes.pane(SHARED_CLOSE_SECOND_PANE).reconnect();
    settle_shared_panes(cx);
    cx.dispatch_action(crate::ui::ActivateTab2);
    cx.run_until_parked();
    cx.dispatch_action(crate::ui::CloseTab);
    cx.run_until_parked();
    assert!(harness.closes().is_empty());
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_some()
    );
    click("modal-action-shared-close-confirm", cx);
    assert!(matches!(harness.closes().as_slice(),
        [spaceterm_server::hierarchy::Mutation::Close { confirmation, .. }]
            if confirmation.own_controls.is_none()
    ));
}

const PENDING_CLOSE_TAB: [u8; 32] = [80; 32];
const PENDING_CLOSE_PANE: [u8; 32] = [81; 32];

fn pending_close_inventory() -> spaceterm_server::hierarchy::Inventory {
    shared_close_inventory(
        2,
        &[
            (
                SHARED_CLOSE_ROOT_TAB,
                &[
                    (SHARED_CLOSE_ROOT_PANE, Some([31; 32])),
                    (SHARED_CLOSE_SPLIT_PANE, Some([32; 32])),
                ],
            ),
            (SHARED_CLOSE_SECOND_TAB, &[(SHARED_CLOSE_SECOND_PANE, None)]),
            (PENDING_CLOSE_TAB, &[(PENDING_CLOSE_PANE, Some([82; 32]))]),
        ],
    )
}

#[gpui::test]
fn closing_a_pending_tab_waits_for_its_creation_then_reviews_authoritative_sessions(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::{CloseTarget, Mutation, Outcome};
    let (harness, cx) = shared_close_harness(cx);
    click("create-tab-button", cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    assert!(
        cx.debug_bounds("modal-action-shared-workspace-refused-ok")
            .is_none(),
        "a provisional Tab is awaiting creation, not removed by another client"
    );
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );
    assert_eq!(harness.edits.submitted().len(), 1);

    harness.edits.answer(Outcome::Accepted {
        target: PENDING_CLOSE_TAB,
    });
    harness.publish(pending_close_inventory(), cx);
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_some(),
        "the exact created Tab must be reviewed using the Session the server reports"
    );
    assert!(harness.closes().is_empty());
    click("modal-action-shared-close-confirm", cx);
    assert_eq!(
        harness.closes(),
        [shared_close_mutation(
            CloseTarget::Tab {
                workspace: SHARED_CLOSE_WORKSPACE,
                tab: PENDING_CLOSE_TAB
            },
            &[(PENDING_CLOSE_PANE, Some([82; 32]))],
            SharedCloseRemoves::Tab,
        )]
    );
    harness.sync(cx);
    assert_eq!(
        harness.closes().len(),
        1,
        "settlement must not submit a duplicate close"
    );
    assert!(matches!(
        harness.edits.submitted()[0],
        Mutation::CreateTab { .. }
    ));
}

#[gpui::test]
fn a_pending_tab_close_preserves_later_tab_selection_and_terminal_find(cx: &mut TestAppContext) {
    use spaceterm_server::hierarchy::{CloseTarget, Outcome};
    let (harness, cx) = shared_close_harness(cx);
    click("create-tab-button", cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    cx.dispatch_action(crate::ui::ActivateTab1);
    cx.dispatch_action(crate::ui::OpenTerminalFind);
    redraw(cx);
    let find_focus = cx.update(|window, cx| window.focused(cx).expect("Find holds the keyboard"));
    let selected = harness.manager.read_with(cx, |manager, cx| {
        manager
            .workspaces
            .active()
            .payload()
            .read(cx)
            .active_terminal_identity(cx)
    });

    harness.edits.answer(Outcome::Accepted {
        target: PENDING_CLOSE_TAB,
    });
    harness.publish(pending_close_inventory(), cx);
    click("modal-action-shared-close-confirm", cx);
    assert_eq!(
        harness.closes(),
        [shared_close_mutation(
            CloseTarget::Tab {
                workspace: SHARED_CLOSE_WORKSPACE,
                tab: PENDING_CLOSE_TAB
            },
            &[(PENDING_CLOSE_PANE, Some([82; 32]))],
            SharedCloseRemoves::Tab,
        )]
    );
    assert_eq!(
        harness.manager.read_with(cx, |manager, cx| {
            manager
                .workspaces
                .active()
                .payload()
                .read(cx)
                .active_terminal_identity(cx)
        }),
        selected,
        "closing the original target must preserve the Tab selected afterwards"
    );
    assert!(cx.update(|window, _| find_focus.is_focused(window)));
}

#[gpui::test]
fn a_refused_creation_discards_its_pending_close_without_targeting_a_surviving_tab(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::Outcome;
    let (harness, cx) = shared_close_harness(cx);
    click("create-tab-button", cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    harness.edits.answer(Outcome::Limit);
    harness.sync(cx);
    assert!(harness.closes().is_empty());
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );
    assert_eq!(shared_close_counts(&harness, cx), (2, 3));
}

#[gpui::test]
fn disconnect_during_creation_cancels_the_unapproved_close_and_keeps_the_original_creation(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::Outcome;
    let (harness, cx) = shared_close_harness(cx);
    click("create-tab-button", cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    let original = harness.edits.submitted();
    harness.edits.set_connected(false);
    harness.sync(cx);
    assert_eq!(harness.edits.submitted(), original);
    click("modal-action-shared-workspace-refused-ok", cx);
    harness.edits.set_connected(true);
    harness.edits.answer(Outcome::Accepted {
        target: PENDING_CLOSE_TAB,
    });
    harness.publish(pending_close_inventory(), cx);
    assert_eq!(
        harness.edits.submitted(),
        original,
        "reconnection resolves the admitted creation without resubmitting it or its cancelled close"
    );
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );
}

#[gpui::test]
fn an_unresolved_creation_outcome_blocks_its_close_until_that_same_creation_settles(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::Outcome;
    let (harness, cx) = shared_close_harness(cx);
    click("create-tab-button", cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    let original = harness.edits.submitted();
    // Publishing membership alone cannot settle a lost creation reply or authorize its close.
    harness.publish(pending_close_inventory(), cx);
    harness.edits.answer(Outcome::Pending);
    harness.sync(cx);
    harness.sync(cx);
    assert_eq!(harness.edits.submitted(), original);
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );
    harness.edits.answer(Outcome::Accepted {
        target: PENDING_CLOSE_TAB,
    });
    harness.sync(cx);
    assert_eq!(harness.edits.submitted(), original);
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_some()
    );
}

#[gpui::test]
fn changed_session_facts_after_pending_creation_require_explicit_review_again(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::Outcome;
    let (harness, cx) = shared_close_harness(cx);
    click("create-tab-button", cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    harness.edits.answer(Outcome::Accepted {
        target: PENDING_CLOSE_TAB,
    });
    harness.publish(pending_close_inventory(), cx);
    let mut changed = pending_close_inventory();
    changed
        .close
        .as_mut()
        .unwrap()
        .sessions
        .iter_mut()
        .find(|pane| pane.id == PENDING_CLOSE_PANE)
        .unwrap()
        .session = Some([83; 32]);
    harness.publish(changed, cx);
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );
    assert!(
        cx.debug_bounds("modal-action-shared-close-review")
            .is_some()
    );
    assert!(
        harness.closes().is_empty(),
        "changed facts never renew destructive consent"
    );
}

#[gpui::test]
fn confirming_a_tab_close_opens_the_replacement_dormant_pane_once(cx: &mut TestAppContext) {
    use crate::terminal::server_session::DevelopmentPaneStatus;
    use spaceterm_server::hierarchy::Outcome;
    let (harness, cx) = shared_close_harness(cx);
    let replacement = harness.panes.pane(SHARED_CLOSE_SECOND_PANE);
    replacement.set(DevelopmentPaneStatus::Restored);
    settle_shared_panes(cx);
    assert_eq!(replacement.selections(), 0);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    click("modal-action-shared-close-confirm", cx);
    assert_eq!(
        replacement.selections(),
        1,
        "confirmation preserves the initiating keyboard intent and opens the Focused Pane"
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
    assert_eq!(
        replacement.selections(),
        1,
        "settlement must not repeat first-use selection"
    );
}

#[gpui::test]
fn directly_closing_a_tab_opens_the_replacement_dormant_pane_once(cx: &mut TestAppContext) {
    use crate::terminal::server_session::DevelopmentPaneStatus;
    use spaceterm_server::hierarchy::ConfirmedControl;
    let (harness, cx) = shared_close_harness(cx);
    let mut idle = shared_close_inventory(
        2,
        &[
            (SHARED_CLOSE_ROOT_TAB, &[(SHARED_CLOSE_ROOT_PANE, None)]),
            (SHARED_CLOSE_SECOND_TAB, &[(SHARED_CLOSE_SECOND_PANE, None)]),
        ],
    );
    idle.close.as_mut().unwrap().controls = Some(vec![ConfirmedControl {
        pane: SHARED_CLOSE_ROOT_PANE,
        session: None,
        owner: [1; 32],
        epoch: 1,
    }]);
    harness.publish(idle, cx);
    let replacement = harness.panes.pane(SHARED_CLOSE_SECOND_PANE);
    replacement.set(DevelopmentPaneStatus::Restored);
    settle_shared_panes(cx);
    assert_eq!(replacement.selections(), 0);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );
    assert_eq!(harness.closes().len(), 1);
    assert_eq!(
        replacement.selections(),
        1,
        "direct close opens the replacement without another keypress"
    );
}

#[gpui::test]
fn a_changed_control_grant_refuses_the_original_direct_close_without_renewing_it(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::hierarchy::{ConfirmedControl, Mutation, Outcome};
    let (harness, cx) = shared_close_harness(cx);
    let mut idle = shared_close_inventory(
        2,
        &[
            (SHARED_CLOSE_ROOT_TAB, &[(SHARED_CLOSE_ROOT_PANE, None)]),
            (SHARED_CLOSE_SECOND_TAB, &[(SHARED_CLOSE_SECOND_PANE, None)]),
        ],
    );
    let grant = ConfirmedControl {
        pane: SHARED_CLOSE_ROOT_PANE,
        session: None,
        owner: [1; 32],
        epoch: 1,
    };
    idle.close.as_mut().unwrap().controls = Some(vec![grant.clone()]);
    harness.publish(idle.clone(), cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    let original = harness.closes();
    assert!(
        matches!(&original[..], [Mutation::Close { confirmation, .. }]
        if confirmation.own_controls.as_deref() == Some(&[grant]))
    );
    idle.close.as_mut().unwrap().controls.as_mut().unwrap()[0].epoch = 2;
    // The server rejects admission against the changed grant even with unchanged membership
    // and Session facts. The client restores the exact target and waits for explicit Review.
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(idle, cx);
    assert_eq!(harness.closes(), original);
    assert!(
        cx.debug_bounds("modal-action-shared-close-review")
            .is_some()
    );
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );
    harness.sync(cx);
    assert_eq!(harness.closes(), original);
}

#[gpui::test]
fn a_close_in_a_creation_burst_follows_only_the_last_requested_tab(cx: &mut TestAppContext) {
    use spaceterm_server::hierarchy::{CloseTarget, Outcome};
    let (harness, cx) = shared_close_harness(cx);
    for _ in 0..3 {
        click("create-tab-button", cx);
    }
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    let mut authority = shared_close_published();
    for index in 0..3u8 {
        let id = [80 + 2 * index; 32];
        let pane = [81 + 2 * index; 32];
        let mut created = pending_close_inventory();
        let mut tab = created.workspaces[0].tabs.pop().unwrap();
        tab.id = id;
        tab.root_pane = pane;
        tab.panes[0].id = pane;
        authority.workspaces[0].tabs.push(tab);
        authority.close.as_mut().unwrap().sessions.push(
            spaceterm_server::hierarchy::ConfirmedPane {
                id: pane,
                session: None,
            },
        );
        authority.revision += 1;
        harness.edits.answer(Outcome::Accepted { target: id });
        harness.publish(authority.clone(), cx);
        assert!(harness.closes().is_empty());
        assert_eq!(
            cx.debug_bounds("modal-action-shared-close-confirm")
                .is_some(),
            index == 2
        );
    }
    click("modal-action-shared-close-confirm", cx);
    assert_eq!(
        harness.closes(),
        [shared_close_mutation(
            CloseTarget::Tab {
                workspace: SHARED_CLOSE_WORKSPACE,
                tab: [84; 32]
            },
            &[([85; 32], None)],
            SharedCloseRemoves::Tab,
        )]
    );
}
