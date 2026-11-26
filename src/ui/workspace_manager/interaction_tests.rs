use super::*;

#[gpui::test]
fn rapid_idle_tab_closes_park_the_final_workspace_and_restore_the_refused_tab(
    cx: &mut TestAppContext,
) {
    use spaceterm_core::hierarchy::{ConfirmedControl, Mutation, Outcome};
    let (harness, cx) = close_harness(cx);
    let mut inventory = close_inventory(
        2,
        &[
            (CLOSE_ROOT_TAB, &[(CLOSE_ROOT_PANE, None)]),
            (CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, None)]),
        ],
    );
    inventory.close.as_mut().unwrap().controls = [CLOSE_ROOT_PANE, CLOSE_SECOND_PANE]
        .into_iter()
        .map(|pane| ConfirmedControl {
            pane,
            session: None,
            owner: [1; 32],
            epoch: 1,
        })
        .collect();
    harness.publish(inventory.clone(), cx);
    harness.panes.pane(CLOSE_SECOND_PANE).reconnect();
    settle_panes(cx);
    harness.detach_other(cx);
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
            [WORKSPACE_ROW]
        );
    });
    assert_eq!(harness.rows(cx), 0);
    assert_eq!(
        harness.closes().len(),
        1,
        "the second direct close waits for the first reply"
    );
    assert!(cx.debug_bounds("modal-action-close-confirm").is_none());

    harness.edits.answer(Outcome::Accepted {
        target: CLOSE_ROOT_TAB,
    });
    inventory.workspaces[0].tabs.remove(0);
    inventory.workspaces[0].root_tab = CLOSE_SECOND_TAB;
    inventory.revision += 1;
    inventory
        .close
        .as_mut()
        .unwrap()
        .sessions
        .retain(|pane| pane.id == CLOSE_SECOND_PANE);
    inventory
        .close
        .as_mut()
        .unwrap()
        .controls
        .retain(|control| control.pane == CLOSE_SECOND_PANE);
    harness.publish(inventory, cx);
    assert_eq!(cx.windows().len(), 1);
    assert_eq!(harness.rows(cx), 0);
    assert_eq!(harness.closes().len(), 2);
    assert!(harness.closes().iter().all(|mutation| matches!(mutation, Mutation::Close { confirmation, .. } if !confirmation.removes_workspace)));

    harness.edits.answer(Outcome::StaleConfirmation);
    harness.sync(cx);
    assert_eq!(harness.rows(cx), 1);
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
    click("modal-action-close-review", cx);
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
    use spaceterm_core::hierarchy::Outcome;
    let (harness, cx) = attachment_harness(cx);
    cx.update(|_, cx| {
        let source = harness.manager.update(cx, |manager, _| {
            let binding = manager.binding.local_mut().unwrap();
            for index in 0..12 {
                let workspace = if index % 2 == 0 {
                    WorkspaceId::new(1)
                } else {
                    WorkspaceId::new(2)
                };
                assert_eq!(
                    binding.rename(workspace, format!("queued-draft-{index}-end"), Some(0)),
                    crate::ui::workspaces::WorkspaceSubmission::Submitted
                );
            }
            binding.edits()
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
        click("modal-action-workspace-conflict-keep", cx);
    }
    harness.sync(cx);
    assert!(
        cx.debug_bounds("modal-action-workspace-conflict-keep")
            .is_none()
    );
    assert_eq!(
        harness.edits.submitted().len(),
        12,
        "review never retries a draft automatically"
    );
}

#[gpui::test]
fn disconnect_drops_a_parked_remote_presentation_without_closing_other_machines(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = MultiMachineHarness::new(cx);
    let (remote, edits) = harness.present_direct("penguin", [9; 32], &[("Far", 1)], cx);
    let mut inventory = core_inventory(900, &[("Far", 1)]);
    inventory.close = Some(spaceterm_core::hierarchy::CloseInventory {
        controls: Vec::new(),
        boot: [1; 32],
        sessions: vec![spaceterm_core::hierarchy::ConfirmedPane {
            id: [101; 32],
            session: None,
        }],
    });
    edits.set_inventory(inventory);
    let far = harness.named("Far", cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.sync_workspaces(remote, window, cx);
            manager.request_core_close(
                crate::close_confirmation::CloseTarget::KillWorkspace(far),
                window,
                cx,
            );
        });
    });
    click("modal-action-close-confirm", cx);
    harness.manager.read_with(cx, |manager, _| {
        assert_eq!(manager.workspaces.parked_ids().collect::<Vec<_>>(), [far]);
    });
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.withdraw_core_workspaces(remote, window, cx);
        });
    });
    harness.manager.read_with(cx, |manager, _| {
        assert!(manager.workspaces.parked_ids().next().is_none());
        assert!(manager.binding.get(remote).is_none());
    });
    assert!(harness.names(cx).is_empty());
}

#[gpui::test]
fn a_refused_workspace_close_restores_its_views_without_overwriting_newer_focus(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = close_harness(cx);
    let original = harness.manager.read_with(cx, |manager, _| {
        manager
            .workspaces
            .workspace(WORKSPACE_ROW)
            .unwrap()
            .payload()
            .entity_id()
    });
    cx.dispatch_action(crate::ui::KillWorkspace);
    cx.run_until_parked();
    click("modal-action-close-confirm", cx);
    assert_eq!(
        harness.rows(cx),
        0,
        "approved close hides the Workspace before its reply"
    );
    right_click("workspace-row-2-active", cx);
    click("workspace-menu-row-rename", cx);
    harness
        .edits
        .answer(spaceterm_core::hierarchy::Outcome::StaleConfirmation);
    harness.sync(cx);
    assert_eq!(harness.rows(cx), 1);
    assert_eq!(
        harness.manager.read_with(cx, |manager, _| manager
            .workspaces
            .workspace(WORKSPACE_ROW)
            .unwrap()
            .payload()
            .entity_id()),
        original
    );
    // Dismiss the explicit stale-approval notice, which must return to the newer editor.
    click("modal-action-close-dismiss", cx);
    assert!(close_rename_is_focused(&harness, cx));
}

/// Another client splits the Workspace while Kill Workspace is open. The person reviews
/// the change and approves the fresh confirmation, so the keyboard their first request held moves
/// at once to the Workspace that takes its place, and the server's answer leaves it there.
#[gpui::test]
fn a_reviewed_workspace_close_moves_the_keyboard_to_the_rendered_survivor(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::Outcome;

    let (harness, cx) = close_harness(cx);
    assert!(close_terminal_is_focused(&harness, cx));
    cx.dispatch_action(crate::ui::KillWorkspace);
    redraw(cx);
    harness.publish(
        close_inventory(
            2,
            &[
                (
                    CLOSE_ROOT_TAB,
                    &[
                        (CLOSE_ROOT_PANE, Some([31; 32])),
                        (CLOSE_SPLIT_PANE, Some([32; 32])),
                        (CLOSE_NEW_PANE, None),
                    ],
                ),
                (CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, None)]),
            ],
        ),
        cx,
    );
    assert!(
        cx.debug_bounds("modal-action-close-review").is_some(),
        "the stale confirmation is withdrawn with a Review"
    );

    click("modal-action-close-review", cx);
    redraw(cx);
    click("modal-action-close-confirm", cx);
    redraw(cx);
    assert_eq!(harness.closes().len(), 1, "the fresh review is approved");
    assert_eq!(harness.rows(cx), 0);
    assert!(
        close_terminal_is_focused(&harness, cx),
        "the surviving Workspace's Pane takes the keyboard before the server answers"
    );

    harness.edits.answer(Outcome::Accepted {
        target: CLOSE_WORKSPACE,
    });
    harness.publish(close_remaining(3), cx);
    assert!(close_terminal_is_focused(&harness, cx));
}

#[gpui::test]
fn a_reviewed_close_keeps_a_newer_rename_through_confirmation_and_acceptance(
    cx: &mut TestAppContext,
) {
    use spaceterm_core::hierarchy::{Mutation, Outcome};

    let (harness, cx) = close_harness(cx);
    cx.dispatch_action(crate::ui::KillWorkspace);
    cx.run_until_parked();
    click("modal-action-close-confirm", cx);
    right_click("workspace-row-2-active", cx);
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
    click("modal-action-close-review", cx);
    // Let the old notice finish its deferred focus and rename-hold cleanup while the new
    // confirmation remains open.
    for _ in 0..3 {
        redraw(cx);
    }
    assert!(cx.debug_bounds("modal-action-close-confirm").is_some());
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
    click("modal-action-close-confirm", cx);
    assert_eq!(harness.rows(cx), 0);
    assert!(close_rename_is_focused(&harness, cx));
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
        target: CLOSE_WORKSPACE,
    });
    harness.publish(close_remaining(2), cx);
    assert!(close_rename_is_focused(&harness, cx));
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
    use spaceterm_core::hierarchy::Mutation;

    let (harness, cx) = close_harness(cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            for name in ["Logs", "Notes"] {
                assert!(manager.rename_workspace(WORKSPACE_ROW, name, Some(0), window, cx));
            }
        });
    });
    assert_eq!(
        harness.edits.submitted().len(),
        1,
        "the second edit waits behind the first"
    );
    right_click("workspace-row-1-active", cx);
    click("workspace-menu-row-rename", cx);
    cx.simulate_keystrokes("d r a f t");
    cx.run_until_parked();

    harness.edits.set_connected(false);
    harness.sync(cx);
    assert!(
        cx.debug_bounds("modal-action-workspace-refused-ok")
            .is_some(),
        "the discarded edit is explained"
    );
    click("modal-action-workspace-refused-ok", cx);

    assert!(close_rename_is_focused(&harness, cx));
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
        cx.debug_bounds("modal-action-workspace-unavailable-ok")
            .is_none(),
        "the unsent draft is not refused"
    );
    assert_eq!(
        harness.edits.submitted(),
        vec![Mutation::RenameWorkspace {
            workspace: CLOSE_WORKSPACE,
            version: 0,
            name: "Logs".to_owned(),
        }]
    );
}

#[gpui::test]
fn close_frames_hide_retired_pane_status_and_completion_preserves_sidebar_editing(
    cx: &mut TestAppContext,
) {
    use crate::terminal::core_session::CorePaneStatus;
    let (harness, cx) = close_harness(cx);
    harness.panes.pane(CLOSE_SECOND_PANE).reconnect();
    settle_panes(cx);
    cx.dispatch_action(crate::ui::CloseTab);
    cx.run_until_parked();
    click("modal-action-close-confirm", cx);
    assert_eq!(close_counts(&harness, cx), (1, 1));
    for status in [
        CorePaneStatus::Takeover,
        CorePaneStatus::TakeoverReconnecting,
        CorePaneStatus::Ended,
    ] {
        harness.panes.pane(CLOSE_ROOT_PANE).set(status);
        settle_panes(cx);
        assert!(cx.debug_bounds("pane-takeover-1").is_none());
        assert!(cx.debug_bounds("pane-notice-1").is_none());
        assert_eq!(close_counts(&harness, cx), (1, 1));
    }
    right_click("workspace-row-2-inactive", cx);
    click("workspace-menu-row-rename", cx);
    harness
        .edits
        .answer(spaceterm_core::hierarchy::Outcome::Accepted {
            target: CLOSE_ROOT_TAB,
        });
    harness.publish(
        close_inventory(2, &[(CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, None)])]),
        cx,
    );
    assert!(close_rename_is_focused(&harness, cx));
}

#[gpui::test]
fn final_workspace_close_dismisses_on_publication_without_acknowledgement(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::Inventory;
    let (harness, cx) = close_harness(cx);
    harness.detach_other(cx);
    cx.dispatch_action(crate::ui::KillWorkspace);
    cx.run_until_parked();
    click("modal-action-close-confirm", cx);
    assert_eq!(harness.rows(cx), 0);
    // Publish directly: after this synchronization the Workspace Window must already be gone.
    harness.edits.set_inventory(Inventory {
        revision: 2,
        repositories: None,
        workspaces: Vec::new(),
        close: None,
    });
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.sync_workspaces(MachineKey::LOCAL, window, cx)
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
    let (harness, cx) = pane_harness(cx);
    let pane = harness.panes.pane(FOCUS_PANE);
    pane.set(crate::terminal::core_session::CorePaneStatus::Restored);
    settle_panes(cx);
    let before = pane.selections();
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.toggle_sidebar_focus(window, cx);
            manager.handle_sidebar_event(
                SidebarEvent::Activate {
                    workspace_id: WORKSPACE_ROW,
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
                    workspace_id: WORKSPACE_ROW,
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
    let (harness, cx) = close_harness(cx);
    harness
        .panes
        .pane(CLOSE_SECOND_PANE)
        .set(crate::terminal::core_session::CorePaneStatus::Takeover);
    settle_panes(cx);
    cx.dispatch_action(crate::ui::ActivateTab2);
    cx.dispatch_action(crate::ui::CloseTab);
    cx.run_until_parked();
    assert!(harness.closes().is_empty());
    assert!(cx.debug_bounds("modal-action-close-confirm").is_some());
}

#[gpui::test]
fn unavailable_commands_on_a_replacement_screen_terminate_without_redispatch(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = pane_harness(cx);
    harness
        .panes
        .pane(FOCUS_PANE)
        .set(crate::terminal::core_session::CorePaneStatus::Takeover);
    settle_panes(cx);
    for action in [
        crate::ui::OpenTerminalFind.boxed_clone(),
        crate::ui::FindNext.boxed_clone(),
        crate::ui::ShowRepositoryStatus.boxed_clone(),
    ] {
        cx.update(|window, cx| window.dispatch_action(action, cx));
        cx.run_until_parked();
    }
    assert_eq!(harness.panes.pane(FOCUS_PANE).take_overs(), 0);
    assert!(cx.debug_bounds("pane-takeover-1").is_some());
}

#[gpui::test]
fn closing_an_idle_own_tab_submits_without_repetitive_confirmation(cx: &mut TestAppContext) {
    let (harness, cx) = close_harness(cx);
    let mut inventory = close_published();
    let control = spaceterm_core::hierarchy::ConfirmedControl {
        pane: CLOSE_SECOND_PANE,
        session: None,
        owner: [1; 32],
        epoch: 7,
    };
    inventory.close.as_mut().unwrap().controls = vec![control.clone()];
    harness.publish(inventory, cx);
    harness.panes.pane(CLOSE_SECOND_PANE).reconnect();
    settle_panes(cx);
    cx.dispatch_action(crate::ui::ActivateTab2);
    cx.run_until_parked();
    cx.dispatch_action(crate::ui::CloseTab);
    cx.run_until_parked();
    assert_eq!(harness.closes().len(), 1, "idle own Tab closes directly");
    assert!(matches!(harness.closes().as_slice(),
        [spaceterm_core::hierarchy::Mutation::Close { confirmation, .. }]
            if confirmation.own_controls.as_deref() == Some(&[control][..])
    ));
    assert!(cx.debug_bounds("modal-action-close-confirm").is_none());
}

#[gpui::test]
fn an_idle_own_tab_with_a_reconnecting_pane_requires_explicit_confirmation(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = close_harness(cx);
    harness.panes.pane(CLOSE_SECOND_PANE).reconnect();
    settle_panes(cx);
    cx.dispatch_action(crate::ui::ActivateTab2);
    cx.run_until_parked();
    cx.dispatch_action(crate::ui::CloseTab);
    cx.run_until_parked();
    assert!(harness.closes().is_empty());
    assert!(cx.debug_bounds("modal-action-close-confirm").is_some());
    click("modal-action-close-confirm", cx);
    assert!(matches!(harness.closes().as_slice(),
        [spaceterm_core::hierarchy::Mutation::Close { confirmation, .. }]
            if confirmation.own_controls.is_none()
    ));
}

const PENDING_CLOSE_TAB: [u8; 32] = [80; 32];
const PENDING_CLOSE_PANE: [u8; 32] = [81; 32];

fn pending_close_inventory() -> spaceterm_core::hierarchy::Inventory {
    close_inventory(
        2,
        &[
            (
                CLOSE_ROOT_TAB,
                &[
                    (CLOSE_ROOT_PANE, Some([31; 32])),
                    (CLOSE_SPLIT_PANE, Some([32; 32])),
                ],
            ),
            (CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, None)]),
            (PENDING_CLOSE_TAB, &[(PENDING_CLOSE_PANE, Some([82; 32]))]),
        ],
    )
}

#[gpui::test]
fn closing_a_pending_tab_waits_for_its_creation_then_reviews_authoritative_sessions(
    cx: &mut TestAppContext,
) {
    use spaceterm_core::hierarchy::{CloseTarget, Mutation, Outcome};
    let (harness, cx) = close_harness(cx);
    click("create-tab-button", cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    assert!(
        cx.debug_bounds("modal-action-workspace-refused-ok")
            .is_none(),
        "a provisional Tab is awaiting creation, not removed by another client"
    );
    assert!(cx.debug_bounds("modal-action-close-confirm").is_none());
    assert_eq!(harness.edits.submitted().len(), 1);

    harness.edits.answer(Outcome::Accepted {
        target: PENDING_CLOSE_TAB,
    });
    harness.publish(pending_close_inventory(), cx);
    assert!(
        cx.debug_bounds("modal-action-close-confirm").is_some(),
        "the exact created Tab must be reviewed using the Session the server reports"
    );
    assert!(harness.closes().is_empty());
    click("modal-action-close-confirm", cx);
    assert_eq!(
        harness.closes(),
        [close_mutation(
            CloseTarget::Tab {
                workspace: CLOSE_WORKSPACE,
                tab: PENDING_CLOSE_TAB
            },
            &[(PENDING_CLOSE_PANE, Some([82; 32]))],
            WorkspaceCloseRemoves::Tab,
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

/// A Tab this client just created reports no Terminal Session until its shell starts. A close
/// asked for meanwhile waits for the shell, then reviews the Session it started, so the review
/// never goes stale on the shell's own start and reports the Tab as changed elsewhere.
#[gpui::test]
fn closing_a_new_tab_while_its_shell_starts_should_review_the_started_session(
    cx: &mut TestAppContext,
) {
    use spaceterm_core::hierarchy::{CloseTarget, Outcome};
    let (harness, cx) = close_harness(cx);
    let starting = |session| {
        let mut inventory = pending_close_inventory();
        if let Some(close) = inventory.close.as_mut()
            && let Some(pane) = close
                .sessions
                .iter_mut()
                .find(|pane| pane.id == PENDING_CLOSE_PANE)
        {
            pane.session = session;
        }
        inventory
    };
    click("create-tab-button", cx);
    harness.edits.answer(Outcome::Accepted {
        target: PENDING_CLOSE_TAB,
    });
    harness.publish(starting(None), cx);

    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    let waited = cx.debug_bounds("modal-action-close-confirm").is_none();
    // The shell starts: the Pane connects before the server reports its Session.
    harness.panes.pane(PENDING_CLOSE_PANE).reconnect();
    cx.executor()
        .advance_clock(crate::ui::workspace_manager::CLOSE_START_POLL * 2);
    redraw(cx);
    let waited_for_report = cx.debug_bounds("modal-action-close-confirm").is_none();
    let mut started = starting(Some([82; 32]));
    started.revision += 1;
    harness.publish(started, cx);
    cx.executor()
        .advance_clock(crate::ui::workspace_manager::CLOSE_START_POLL * 2);
    redraw(cx);
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    let changed = tree.find("Tab Changed on Another Client").is_some();
    let reviewed = cx.debug_bounds("modal-action-close-confirm").is_some();
    if reviewed {
        click("modal-action-close-confirm", cx);
    }

    assert_eq!(
        (waited, waited_for_report, changed, reviewed),
        (true, true, false, true),
        "the close must wait for the starting shell and its reported Session, then review it \
         without a stale notice"
    );
    assert_eq!(
        harness.closes(),
        [close_mutation(
            CloseTarget::Tab {
                workspace: CLOSE_WORKSPACE,
                tab: PENDING_CLOSE_TAB
            },
            &[(PENDING_CLOSE_PANE, Some([82; 32]))],
            WorkspaceCloseRemoves::Tab,
        )],
        "the close must end exactly the Session the shell started"
    );
}

/// A new Tab whose shell ended before it reported a Terminal Session has nothing about to
/// change, so a close reviews what it ends at once.
#[gpui::test]
fn closing_a_new_tab_whose_shell_ended_should_review_at_once(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::Outcome;
    let (harness, cx) = close_harness(cx);
    click("create-tab-button", cx);
    harness.edits.answer(Outcome::Accepted {
        target: PENDING_CLOSE_TAB,
    });
    let mut inventory = pending_close_inventory();
    if let Some(close) = inventory.close.as_mut() {
        for pane in &mut close.sessions {
            pane.session = None;
        }
    }
    harness.publish(inventory, cx);
    harness
        .panes
        .pane(PENDING_CLOSE_PANE)
        .set(crate::terminal::core_session::CorePaneStatus::Ended);
    redraw(cx);

    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);

    assert!(
        cx.debug_bounds("modal-action-close-confirm").is_some(),
        "a close must review a new Tab whose shell ended without waiting"
    );
}

#[gpui::test]
fn a_pending_tab_close_preserves_later_tab_selection_and_terminal_find(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::{CloseTarget, Outcome};
    let (harness, cx) = close_harness(cx);
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
    click("modal-action-close-confirm", cx);
    assert_eq!(
        harness.closes(),
        [close_mutation(
            CloseTarget::Tab {
                workspace: CLOSE_WORKSPACE,
                tab: PENDING_CLOSE_TAB
            },
            &[(PENDING_CLOSE_PANE, Some([82; 32]))],
            WorkspaceCloseRemoves::Tab,
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
    use spaceterm_core::hierarchy::Outcome;
    let (harness, cx) = close_harness(cx);
    click("create-tab-button", cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    harness.edits.answer(Outcome::Limit);
    harness.sync(cx);
    assert!(harness.closes().is_empty());
    assert!(cx.debug_bounds("modal-action-close-confirm").is_none());
    assert_eq!(close_counts(&harness, cx), (2, 3));
}

#[gpui::test]
fn disconnect_during_creation_cancels_the_unapproved_close_and_keeps_the_original_creation(
    cx: &mut TestAppContext,
) {
    use spaceterm_core::hierarchy::Outcome;
    let (harness, cx) = close_harness(cx);
    click("create-tab-button", cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    let original = harness.edits.submitted();
    harness.edits.set_connected(false);
    harness.sync(cx);
    assert_eq!(harness.edits.submitted(), original);
    click("modal-action-workspace-refused-ok", cx);
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
    assert!(cx.debug_bounds("modal-action-close-confirm").is_none());
}

#[gpui::test]
fn an_unresolved_creation_outcome_blocks_its_close_until_that_same_creation_settles(
    cx: &mut TestAppContext,
) {
    use spaceterm_core::hierarchy::Outcome;
    let (harness, cx) = close_harness(cx);
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
    assert!(cx.debug_bounds("modal-action-close-confirm").is_none());
    harness.edits.answer(Outcome::Accepted {
        target: PENDING_CLOSE_TAB,
    });
    harness.sync(cx);
    assert_eq!(harness.edits.submitted(), original);
    assert!(cx.debug_bounds("modal-action-close-confirm").is_some());
}

#[gpui::test]
fn changed_session_facts_after_pending_creation_require_explicit_review_again(
    cx: &mut TestAppContext,
) {
    use spaceterm_core::hierarchy::Outcome;
    let (harness, cx) = close_harness(cx);
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
    assert!(cx.debug_bounds("modal-action-close-confirm").is_none());
    assert!(cx.debug_bounds("modal-action-close-review").is_some());
    assert!(
        harness.closes().is_empty(),
        "changed facts never renew destructive consent"
    );
}

#[gpui::test]
fn confirming_a_tab_close_opens_the_replacement_dormant_pane_once(cx: &mut TestAppContext) {
    use crate::terminal::core_session::CorePaneStatus;
    use spaceterm_core::hierarchy::Outcome;
    let (harness, cx) = close_harness(cx);
    let replacement = harness.panes.pane(CLOSE_SECOND_PANE);
    replacement.set(CorePaneStatus::Restored);
    settle_panes(cx);
    assert_eq!(replacement.selections(), 0);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    click("modal-action-close-confirm", cx);
    assert_eq!(
        replacement.selections(),
        1,
        "confirmation preserves the initiating keyboard intent and opens the Focused Pane"
    );
    harness.edits.answer(Outcome::Accepted {
        target: CLOSE_ROOT_TAB,
    });
    harness.publish(
        close_inventory(2, &[(CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, None)])]),
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
    use crate::terminal::core_session::CorePaneStatus;
    use spaceterm_core::hierarchy::ConfirmedControl;
    let (harness, cx) = close_harness(cx);
    let mut idle = close_inventory(
        2,
        &[
            (CLOSE_ROOT_TAB, &[(CLOSE_ROOT_PANE, None)]),
            (CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, None)]),
        ],
    );
    idle.close.as_mut().unwrap().controls = vec![ConfirmedControl {
        pane: CLOSE_ROOT_PANE,
        session: None,
        owner: [1; 32],
        epoch: 1,
    }];
    harness.publish(idle, cx);
    let replacement = harness.panes.pane(CLOSE_SECOND_PANE);
    replacement.set(CorePaneStatus::Restored);
    settle_panes(cx);
    assert_eq!(replacement.selections(), 0);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    assert!(cx.debug_bounds("modal-action-close-confirm").is_none());
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
    use spaceterm_core::hierarchy::{ConfirmedControl, Mutation, Outcome};
    let (harness, cx) = close_harness(cx);
    let mut idle = close_inventory(
        2,
        &[
            (CLOSE_ROOT_TAB, &[(CLOSE_ROOT_PANE, None)]),
            (CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, None)]),
        ],
    );
    let grant = ConfirmedControl {
        pane: CLOSE_ROOT_PANE,
        session: None,
        owner: [1; 32],
        epoch: 1,
    };
    idle.close.as_mut().unwrap().controls = vec![grant.clone()];
    harness.publish(idle.clone(), cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    let original = harness.closes();
    assert!(
        matches!(&original[..], [Mutation::Close { confirmation, .. }]
        if confirmation.own_controls.as_deref() == Some(&[grant]))
    );
    idle.close.as_mut().unwrap().controls[0].epoch = 2;
    // The server rejects admission against the changed grant even with unchanged membership
    // and Session facts. The client restores the exact target and waits for explicit Review.
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(idle, cx);
    assert_eq!(harness.closes(), original);
    assert!(cx.debug_bounds("modal-action-close-review").is_some());
    assert!(cx.debug_bounds("modal-action-close-confirm").is_none());
    assert!(
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
            .find("Tab Changed on Another Client")
            .is_some()
    );
    harness.sync(cx);
    assert_eq!(harness.closes(), original);
}

/// The server refuses this client's own direct close for work in its unchanged Pane, as after
/// Enter and Close in quick succession, or after a line in a shell without shell integration
/// that nothing proved finished, such as a quiet `/bin/sleep` that already ended or a raw
/// `read -k` after it. The client cannot tell which, so it claims no running command and blames
/// no other client: the neutral confirmation opens, and only its answer closes anything.
#[gpui::test]
fn an_own_close_refused_for_work_asks_the_neutral_confirmation(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::{ConfirmedControl, Mutation, Outcome};
    let (harness, cx) = close_harness(cx);
    let mut idle = close_inventory(
        2,
        &[
            (CLOSE_ROOT_TAB, &[(CLOSE_ROOT_PANE, None)]),
            (CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, None)]),
        ],
    );
    idle.close.as_mut().unwrap().controls = vec![ConfirmedControl {
        pane: CLOSE_ROOT_PANE,
        session: None,
        owner: [1; 32],
        epoch: 1,
    }];
    harness.publish(idle.clone(), cx);
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    let original = harness.closes();
    assert!(
        matches!(&original[..], [Mutation::Close { confirmation, .. }]
        if confirmation.own_controls.is_some())
    );
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(idle.clone(), cx);
    redraw(cx);
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    assert!(tree.find("Close Tab?").is_some());
    assert!(tree.find("Command Running in Tab").is_none());
    assert!(tree.find("Tab Changed on Another Client").is_none());
    assert!(cx.debug_bounds("modal-action-close-confirm").is_some());
    assert_eq!(
        harness.closes(),
        original,
        "nothing closes before an answer"
    );

    // Cancel leaves the work alone, and a later Close asks again only if the server refuses.
    click("modal-action-close-cancel", cx);
    harness.sync(cx);
    assert_eq!(harness.closes(), original, "Cancel closes nothing");

    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(idle, cx);
    redraw(cx);
    click("modal-action-close-confirm", cx);
    let closes = harness.closes();
    assert!(
        matches!(closes.last(), Some(Mutation::Close { confirmation, .. })
            if confirmation.own_controls.is_none()),
        "the person's answer sends a reviewed close: {closes:?}"
    );
}

/// The Session id of the last key a Pane's Terminal Session received.
fn last_key_session(records: &TestTerminalSessionRecords) -> Option<usize> {
    records
        .commands()
        .into_iter()
        .rev()
        .find(|call| matches!(call.command, RecordedCommand::Key(_)))
        .map(|call| call.session_id)
}

/// How many keys every Terminal Session received.
fn key_count(records: &TestTerminalSessionRecords) -> usize {
    records
        .commands()
        .iter()
        .filter(|call| matches!(call.command, RecordedCommand::Key(_)))
        .count()
}

#[gpui::test]
fn cmd_w_keeps_the_tab_selected_until_core_admits_the_close(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::{CloseTarget as MachineCloseTarget, ConfirmedControl};
    use spaceterm_core::hierarchy::{Mutation, Outcome};

    let records = TestTerminalSessionRecords::default();
    let (harness, cx) = close_harness_recording(records.clone(), cx);
    let mut idle = close_inventory(
        2,
        &[
            (CLOSE_ROOT_TAB, &[(CLOSE_ROOT_PANE, Some([31; 32]))]),
            (CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, Some([33; 32]))]),
        ],
    );
    idle.close.as_mut().unwrap().controls = vec![ConfirmedControl {
        pane: CLOSE_ROOT_PANE,
        session: Some([31; 32]),
        owner: [1; 32],
        epoch: 1,
    }];
    harness.publish(idle.clone(), cx);
    harness.panes.pane(CLOSE_SECOND_PANE).reconnect();
    settle_panes(cx);
    focus_active_workspace(&harness.manager, cx);
    let identity = |cx: &mut VisualTestContext| {
        harness.manager.read_with(cx, |manager, cx| {
            manager
                .active_tab_manager()
                .expect("an active Workspace")
                .read(cx)
                .active_terminal_identity(cx)
        })
    };
    let original = identity(cx);
    cx.simulate_keystrokes("a");
    let closing_session = last_key_session(&records).expect("the closing shell reads keys");

    let line = harness.panes.pane(CLOSE_ROOT_PANE).hold_line_submission();
    cx.simulate_keystrokes("enter cmd-w cmd-w");
    redraw(cx);
    assert_eq!(
        identity(cx),
        original,
        "the Pane and Tab remain selected while Enter is queued"
    );
    assert!(harness.closes().is_empty(), "Close cannot overtake Enter");
    drop(line);
    redraw(cx);
    assert_eq!(
        identity(cx),
        original,
        "the Tab stays selected while Core checks the line"
    );
    assert_eq!(harness.closes().len(), 1);
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(idle.clone(), cx);
    settle_panes(cx);
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    assert!(tree.find("Close Pane?").is_some());
    assert!(tree.find_text("This is the only Pane in its Tab, so the Tab closes for every client. This ends 1 Terminal Session.").is_some());
    assert_eq!(
        identity(cx),
        original,
        "confirmation keeps the Tab selected"
    );

    click("modal-action-close-cancel", cx);
    settle_panes(cx);
    assert_eq!(
        identity(cx),
        original,
        "Cancel keeps the refused Tab selected"
    );
    assert!(close_terminal_is_focused(&harness, cx));
    let keys = key_count(&records);
    cx.simulate_keystrokes("x");
    assert_eq!(
        key_count(&records),
        keys + 1,
        "the next key reaches a shell"
    );
    assert_eq!(
        last_key_session(&records),
        Some(closing_session),
        "the next key reaches the shell the person asked to close"
    );
    assert_eq!(harness.closes().len(), 1, "Cancel closes nothing");

    cx.simulate_keystrokes("cmd-w");
    redraw(cx);
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(idle, cx);
    settle_panes(cx);
    click("modal-action-close-confirm", cx);
    settle_panes(cx);
    assert_ne!(
        identity(cx).0,
        original.0,
        "Confirm closes the Tab immediately"
    );
    let closes = harness.closes();
    assert_eq!(closes.len(), 3, "Confirm submits one reviewed close");
    let Some(Mutation::Close {
        target,
        confirmation,
    }) = closes.last()
    else {
        panic!("the reviewed close is submitted: {closes:?}");
    };
    assert!(matches!(target, MachineCloseTarget::Pane { tab, pane, .. }
        if *tab == CLOSE_ROOT_TAB && *pane == CLOSE_ROOT_PANE));
    assert!(confirmation.own_controls.is_none());
    assert_eq!(confirmation.panes.len(), 1);
    assert_eq!(confirmation.panes[0].id, CLOSE_ROOT_PANE);
    assert_eq!(confirmation.panes[0].session, Some([31; 32]));
}

#[gpui::test]
fn rapid_cmd_w_closes_each_idle_tab_in_order(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::{ConfirmedControl, Mutation, Outcome};
    let (harness, cx) = close_harness(cx);
    let third_tab = [44; 32];
    let third_pane = [45; 32];
    let last_tab = [46; 32];
    let last_pane = [47; 32];
    let tabs: &[([u8; 32], &[WorkspaceClosePane])] = &[
        (CLOSE_ROOT_TAB, &[(CLOSE_ROOT_PANE, Some([31; 32]))]),
        (CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, Some([33; 32]))]),
        (third_tab, &[(third_pane, Some([35; 32]))]),
        (last_tab, &[(last_pane, None)]),
    ];
    let inventory = |start| {
        let mut inventory = close_inventory(2 + start as u64, &tabs[start..]);
        inventory.close.as_mut().unwrap().controls = tabs[start..]
            .iter()
            .map(|(_, panes)| ConfirmedControl {
                pane: panes[0].0,
                session: panes[0].1,
                owner: [1; 32],
                epoch: 1,
            })
            .collect();
        inventory
    };
    harness.publish(inventory(0), cx);
    for pane in [CLOSE_SECOND_PANE, third_pane] {
        harness.panes.pane(pane).reconnect();
    }
    settle_panes(cx);
    focus_active_workspace(&harness.manager, cx);
    let original = close_identity(&harness, cx);
    cx.simulate_keystrokes("cmd-w cmd-w cmd-w");
    redraw(cx);
    assert_eq!(close_identity(&harness, cx), original);

    for (index, (tab, panes)) in tabs[..3].iter().enumerate() {
        let closes = harness.closes();
        assert_eq!(closes.len(), index + 1);
        assert!(
            matches!(closes.last(), Some(Mutation::Close { target: spaceterm_core::hierarchy::CloseTarget::Pane { tab: closed, .. }, .. }) if closed == tab)
        );
        harness
            .edits
            .answer(Outcome::Accepted { target: panes[0].0 });
        harness.publish(inventory(index + 1), cx);
    }
    assert_eq!(harness.closes().len(), 3);
    assert!(cx.debug_bounds("modal-action-close-confirm").is_none());
}

/// Cancel restores keyboard focus to the Pane under confirmation.
#[gpui::test]
fn cancel_after_a_refused_own_pane_close_preserves_that_pane(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::{CloseTarget as MachineCloseTarget, Mutation, Outcome};

    let records = TestTerminalSessionRecords::default();
    let (harness, cx) = close_harness_recording(records.clone(), cx);
    let (inventory, _) = close_published_controlling_root();
    harness.publish(inventory.clone(), cx);
    harness.panes.pane(CLOSE_SPLIT_PANE).reconnect();
    settle_panes(cx);
    focus_active_workspace(&harness.manager, cx);
    let identity = |cx: &mut VisualTestContext| {
        harness.manager.read_with(cx, |manager, cx| {
            manager
                .active_tab_manager()
                .expect("an active Workspace")
                .read(cx)
                .active_terminal_identity(cx)
        })
    };
    let original = identity(cx);
    cx.simulate_keystrokes("a");
    let closing_session = last_key_session(&records).expect("the closing shell reads keys");

    cx.simulate_keystrokes("cmd-w");
    redraw(cx);
    assert_eq!(
        identity(cx),
        original,
        "the original split stays focused until Core answers"
    );
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(inventory.clone(), cx);
    settle_panes(cx);
    assert!(cx.debug_bounds("modal-action-close-confirm").is_some());

    click("modal-action-close-cancel", cx);
    settle_panes(cx);
    assert_eq!(
        identity(cx),
        original,
        "Cancel keeps the refused Pane focused"
    );
    assert!(close_terminal_is_focused(&harness, cx));
    let keys = key_count(&records);
    cx.simulate_keystrokes("x");
    assert_eq!(
        key_count(&records),
        keys + 1,
        "the next key reaches a shell"
    );
    assert_eq!(last_key_session(&records), Some(closing_session));

    cx.simulate_keystrokes("cmd-w");
    redraw(cx);
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(inventory, cx);
    settle_panes(cx);
    click("modal-action-close-confirm", cx);
    let closes = harness.closes();
    assert!(
        matches!(closes.last(), Some(Mutation::Close { target: MachineCloseTarget::Pane { pane, .. }, confirmation })
            if *pane == CLOSE_ROOT_PANE && confirmation.own_controls.is_none()
                && confirmation.panes.iter().map(|p| (p.id, p.session)).eq([(CLOSE_ROOT_PANE, Some([31; 32]))])),
        "the reviewed close ends exactly the refused Pane's Session: {closes:?}"
    );
}

const REMOTE_WORKSPACE: [u8; 32] = [61; 32];

/// Presents a paired Machine whose one Workspace runs a live Session of its own beside this
/// computer's Workspaces, then returns to this computer's Workspace.
fn present_paired_machine(
    harness: &WorkspaceCloseHarness,
    cx: &mut VisualTestContext,
) -> (
    Rc<crate::ui::workspaces::testing::ScriptedEdits>,
    MachineKey,
) {
    use crate::ui::workspaces::{DirectMachine, WorkspaceEditSource, WorkspacePaneSessions};

    let mut remote = close_inventory(1, &[([62; 32], &[([63; 32], Some([64; 32]))])]);
    remote.workspaces.truncate(1);
    remote.workspaces[0].id = REMOTE_WORKSPACE;
    remote.workspaces[0].name = "Remote".to_owned();
    remote
        .close
        .as_mut()
        .unwrap()
        .sessions
        .retain(|pane| pane.id != CLOSE_OTHER_PANE);
    let remote_edits = Rc::new(crate::ui::workspaces::testing::ScriptedEdits::new(Some(
        remote,
    )));
    let route = named_route("penguin");
    let remote_key = cx.update(|window, cx| {
        crate::ui::core_worktrees::testing::present_direct(
            route,
            Rc::clone(&remote_edits),
            crate::ui::core_worktrees::testing::ScriptedReads::new("~/{branch}"),
            cx,
        );
        harness.manager.update(cx, |manager, cx| {
            let key = manager.present_core_workspaces(
                MachineOrigin::Direct(DirectMachine {
                    route,
                    name: "penguin".to_owned().into(),
                }),
                WorkspaceEditSource::new(Rc::clone(&remote_edits) as Rc<_>),
                WorkspacePaneSessions::with_opener(
                    crate::ui::workspaces::testing::ScriptedPanes::new(core_session_factory())
                        as Rc<_>,
                    &PaneConstruction::testing(),
                ),
                window,
                cx,
            );
            manager.sync_workspaces(key, window, cx);
            manager.attach_detached_workspace(key, REMOTE_WORKSPACE, window, cx);
            manager.activate_workspace(WORKSPACE_ROW, window, cx);
            key
        })
    });
    settle_panes(cx);
    (remote_edits, remote_key)
}

/// The person asks to kill the paired Machine's Workspace, whose confirmation then holds the
/// window and its keyboard.
fn open_paired_kill(
    harness: &WorkspaceCloseHarness,
    remote_key: MachineKey,
    cx: &mut VisualTestContext,
) {
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            let remote = manager
                .binding
                .get(remote_key)
                .and_then(|binding| binding.local(REMOTE_WORKSPACE))
                .expect("the paired Workspace is presented");
            manager.request_close(CloseTarget::KillWorkspace(remote), window, cx);
        });
    });
    settle_panes(cx);
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    assert!(
        tree.find("Kill Workspace \u{201C}Remote\u{201D}?")
            .is_some()
    );
}

/// A paired Machine's Kill Workspace confirmation is open when this computer's Core refuses an
/// own direct close for work in its unchanged Pane. The refusal waits with its exact target until
/// that confirmation is answered, and then opens the neutral review of what it would end, never
/// the generic Changed notice or nothing at all.
#[gpui::test]
fn an_own_close_refused_beneath_another_machines_confirmation_reviews_once_it_closes(
    cx: &mut TestAppContext,
) {
    use spaceterm_core::hierarchy::{CloseTarget as MachineCloseTarget, ConfirmedControl};
    use spaceterm_core::hierarchy::{Mutation, Outcome};

    let records = TestTerminalSessionRecords::default();
    let (harness, cx) = close_harness_recording(records.clone(), cx);
    let mut idle = close_inventory(
        2,
        &[
            (CLOSE_ROOT_TAB, &[(CLOSE_ROOT_PANE, Some([31; 32]))]),
            (CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, Some([33; 32]))]),
        ],
    );
    idle.close.as_mut().unwrap().controls = vec![ConfirmedControl {
        pane: CLOSE_ROOT_PANE,
        session: Some([31; 32]),
        owner: [1; 32],
        epoch: 1,
    }];
    harness.publish(idle.clone(), cx);
    harness.panes.pane(CLOSE_SECOND_PANE).reconnect();
    settle_panes(cx);
    let (remote_edits, remote_key) = present_paired_machine(&harness, cx);
    focus_active_workspace(&harness.manager, cx);
    let identity = |cx: &mut VisualTestContext| {
        harness.manager.read_with(cx, |manager, cx| {
            manager
                .active_tab_manager()
                .expect("an active Workspace")
                .read(cx)
                .active_terminal_identity(cx)
        })
    };
    let original = identity(cx);
    cx.simulate_keystrokes("a");
    let closing_session = last_key_session(&records).expect("the closing shell reads keys");

    // The own direct close goes out, then the person asks to kill the paired Workspace.
    cx.simulate_keystrokes("cmd-w");
    redraw(cx);
    assert_eq!(harness.closes().len(), 1);
    open_paired_kill(&harness, remote_key, cx);

    // This computer's Core refuses the own close while that confirmation is open.
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(idle.clone(), cx);
    settle_panes(cx);
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    assert!(
        tree.find("Kill Workspace \u{201C}Remote\u{201D}?")
            .is_some()
    );
    assert!(tree.find("Close Pane?").is_none());

    // Cancel ends the paired review, and the own refusal's neutral review opens in its place.
    click("modal-action-close-cancel", cx);
    settle_panes(cx);
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    assert!(
        tree.find("Kill Workspace \u{201C}Remote\u{201D}?")
            .is_none()
    );
    assert!(
        tree.find("Close Pane?").is_some(),
        "the refusal is reviewed"
    );
    assert!(tree.find("Pane Changed on Another Client").is_none());
    assert!(tree.find("Tab Changed on Another Client").is_none());
    assert_eq!(
        identity(cx),
        original,
        "the refused Tab is selected beneath the review"
    );
    assert!(
        remote_edits
            .submitted()
            .iter()
            .all(|mutation| !matches!(mutation, Mutation::Close { .. })),
        "Cancel kills nothing"
    );

    // Cancel of the own review returns the keyboard to the refused shell.
    click("modal-action-close-cancel", cx);
    settle_panes(cx);
    assert_eq!(identity(cx), original);
    let keys = key_count(&records);
    cx.simulate_keystrokes("x");
    assert_eq!(
        key_count(&records),
        keys + 1,
        "the next key reaches a shell"
    );
    assert_eq!(last_key_session(&records), Some(closing_session));
    assert_eq!(harness.closes().len(), 1, "nothing more closed");

    // A later review confirms exactly the refused Pane's Session.
    cx.simulate_keystrokes("cmd-w");
    redraw(cx);
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(idle, cx);
    settle_panes(cx);
    click("modal-action-close-confirm", cx);
    let closes = harness.closes();
    assert!(
        matches!(closes.last(), Some(Mutation::Close { target: MachineCloseTarget::Pane { tab, pane, .. }, confirmation })
            if *tab == CLOSE_ROOT_TAB && *pane == CLOSE_ROOT_PANE
                && confirmation.own_controls.is_none()
                && confirmation.panes.iter().map(|p| (p.id, p.session)).eq([(CLOSE_ROOT_PANE, Some([31; 32]))])),
        "{closes:?}"
    );
}

#[gpui::test]
fn a_close_in_a_creation_burst_follows_only_the_last_requested_tab(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::{CloseTarget, Outcome};
    let (harness, cx) = close_harness(cx);
    for _ in 0..3 {
        click("create-tab-button", cx);
    }
    cx.dispatch_action(crate::ui::CloseTab);
    redraw(cx);
    let mut authority = close_published();
    for index in 0..3u8 {
        let id = [80 + 2 * index; 32];
        let pane = [81 + 2 * index; 32];
        let mut created = pending_close_inventory();
        let mut tab = created.workspaces[0].tabs.pop().unwrap();
        tab.id = id;
        tab.root_pane = pane;
        tab.panes[0].id = pane;
        authority.workspaces[0].tabs.push(tab);
        authority
            .close
            .as_mut()
            .unwrap()
            .sessions
            .push(spaceterm_core::hierarchy::ConfirmedPane {
                id: pane,
                session: None,
            });
        authority.revision += 1;
        harness.edits.answer(Outcome::Accepted { target: id });
        harness.publish(authority.clone(), cx);
        assert!(harness.closes().is_empty());
        // The last Tab's shell is still starting, so its review waits for the shell.
        assert!(cx.debug_bounds("modal-action-close-confirm").is_none());
    }
    // A shell that does not start in time leaves the review to name what the Tab ends now.
    cx.executor()
        .advance_clock(crate::ui::workspace_manager::CLOSE_START_LIMIT * 2);
    redraw(cx);
    click("modal-action-close-confirm", cx);
    assert_eq!(
        harness.closes(),
        [close_mutation(
            CloseTarget::Tab {
                workspace: CLOSE_WORKSPACE,
                tab: [84; 32]
            },
            &[([85; 32], None)],
            WorkspaceCloseRemoves::Tab,
        )]
    );
}

/// The Workspace's Tab and Pane presenting the keyboard's place.
fn close_identity(
    harness: &WorkspaceCloseHarness,
    cx: &mut VisualTestContext,
) -> (crate::domain::TabId, crate::domain::PaneId) {
    harness.manager.read_with(cx, |manager, cx| {
        manager
            .active_tab_manager()
            .expect("an active Workspace")
            .read(cx)
            .active_terminal_identity(cx)
    })
}

/// Two single-Pane Tabs, both running a shell, in which this client controls the Root Pane's.
fn close_idle_tabs() -> spaceterm_core::hierarchy::Inventory {
    let mut idle = close_inventory(
        2,
        &[
            (CLOSE_ROOT_TAB, &[(CLOSE_ROOT_PANE, Some([31; 32]))]),
            (CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, Some([33; 32]))]),
        ],
    );
    idle.close.as_mut().unwrap().controls = vec![spaceterm_core::hierarchy::ConfirmedControl {
        pane: CLOSE_ROOT_PANE,
        session: Some([31; 32]),
        owner: [1; 32],
        epoch: 1,
    }];
    idle
}

/// How a person answers the fresh confirmation a changed close's Review opens.
#[derive(Clone, Copy)]
enum ReviewAnswer {
    Cancel,
    /// Another change arrives before the answer, and the person reviews and confirms again.
    ConfirmAfterAnotherChange,
}

/// `inventory` with the Root Pane running Terminal Session `session` under this client's grant
/// `epoch`, at the next revision.
fn replace_root_session(
    mut inventory: spaceterm_core::hierarchy::Inventory,
    session: [u8; 32],
    epoch: u64,
) -> spaceterm_core::hierarchy::Inventory {
    inventory.revision += 1;
    let live = inventory.close.as_mut().unwrap();
    for pane in &mut live.sessions {
        if pane.id == CLOSE_ROOT_PANE {
            pane.session = Some(session);
        }
    }
    live.controls[0].session = Some(session);
    live.controls[0].epoch = epoch;
    inventory
}

/// An own direct close of the Root Pane is refused for work while a paired Machine's
/// confirmation holds the window. Before that confirmation closes, `changed` is published. The
/// refusal proved only what it would end then, so its review never presents other work: it ends
/// as a changed close that names no other client, and nothing closes. Its Review opens a fresh
/// confirmation of what the close ends now and submits nothing until the person confirms, even
/// though this client controls that idle work.
fn a_queued_refusal_never_reviews_changed_work(
    changed: spaceterm_core::hierarchy::Inventory,
    answer: ReviewAnswer,
    cx: &mut TestAppContext,
) {
    use spaceterm_core::hierarchy::{CloseTarget as MachineCloseTarget, Mutation, Outcome};

    let (harness, cx) = close_harness(cx);
    let idle = close_idle_tabs();
    harness.publish(idle.clone(), cx);
    harness.panes.pane(CLOSE_SECOND_PANE).reconnect();
    settle_panes(cx);
    let (remote_edits, remote_key) = present_paired_machine(&harness, cx);
    focus_active_workspace(&harness.manager, cx);

    cx.simulate_keystrokes("cmd-w");
    redraw(cx);
    assert_eq!(harness.closes().len(), 1);
    open_paired_kill(&harness, remote_key, cx);
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(idle, cx);
    settle_panes(cx);
    assert!(
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
            .find("Close Pane?")
            .is_none(),
        "the refusal waits for the paired confirmation"
    );

    harness.publish(changed.clone(), cx);
    settle_panes(cx);
    click("modal-action-close-cancel", cx);
    settle_panes(cx);
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    assert!(
        tree.find("Close Pane?").is_none(),
        "the refusal never reviews work it did not prove"
    );
    assert!(
        tree.find("Pane Changed").is_some(),
        "the refusal ends as a changed close"
    );
    assert!(
        tree.find("Pane Changed on Another Client").is_none(),
        "the change may be this client's own"
    );
    assert!(
        cx.debug_bounds("modal-action-close-review").is_some(),
        "Review asks again from the person's own request"
    );

    click("modal-action-close-review", cx);
    settle_panes(cx);
    assert!(
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
            .find("Close Pane?")
            .is_some(),
        "Review opens a fresh confirmation"
    );
    assert_eq!(harness.closes().len(), 1, "Review closes nothing by itself");

    match answer {
        ReviewAnswer::Cancel => {
            click("modal-action-close-cancel", cx);
            settle_panes(cx);
            assert!(
                spaceterm_ui::a11y_testing::A11yTree::read(cx)
                    .find("Close Pane?")
                    .is_none()
            );
            assert_eq!(harness.closes().len(), 1, "Cancel closes nothing");
        }
        ReviewAnswer::ConfirmAfterAnotherChange => {
            // A change between the Review and the answer makes that confirmation stale too.
            let current = replace_root_session(changed, [36; 32], 3);
            harness.publish(current, cx);
            settle_panes(cx);
            let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
            assert!(tree.find("Close Pane?").is_none(), "the stale review ends");
            assert!(cx.debug_bounds("modal-action-close-review").is_some());
            assert_eq!(harness.closes().len(), 1, "the stale review closes nothing");

            click("modal-action-close-review", cx);
            settle_panes(cx);
            assert!(
                spaceterm_ui::a11y_testing::A11yTree::read(cx)
                    .find("Close Pane?")
                    .is_some(),
                "Review opens a fresh confirmation again"
            );
            assert_eq!(harness.closes().len(), 1);
            click("modal-action-close-confirm", cx);
            settle_panes(cx);
            let closes = harness.closes();
            assert_eq!(closes.len(), 2, "{closes:?}");
            assert!(
                matches!(closes.last(), Some(Mutation::Close { target: MachineCloseTarget::Pane { tab, pane, .. }, confirmation })
                    if *tab == CLOSE_ROOT_TAB && *pane == CLOSE_ROOT_PANE
                        && confirmation.own_controls.is_none()
                        && confirmation.panes.iter().map(|p| (p.id, p.session)).eq([(CLOSE_ROOT_PANE, Some([36; 32]))])),
                "the confirmed close ends exactly what the fresh confirmation showed: {closes:?}"
            );
        }
    }
    assert!(
        remote_edits
            .submitted()
            .iter()
            .all(|mutation| !matches!(mutation, Mutation::Close { .. })),
        "Cancel kills nothing"
    );
}

/// The Root Pane starts a new Terminal Session in the same Pane while its refusal waits.
fn close_replaced_root() -> spaceterm_core::hierarchy::Inventory {
    replace_root_session(close_idle_tabs(), [35; 32], 2)
}

#[gpui::test]
fn a_queued_refusal_never_reviews_a_replacement_session_in_the_same_pane(cx: &mut TestAppContext) {
    a_queued_refusal_never_reviews_changed_work(close_replaced_root(), ReviewAnswer::Cancel, cx);
}

#[gpui::test]
fn reviewing_a_replacement_session_confirms_only_what_the_fresh_confirmation_shows(
    cx: &mut TestAppContext,
) {
    a_queued_refusal_never_reviews_changed_work(
        close_replaced_root(),
        ReviewAnswer::ConfirmAfterAnotherChange,
        cx,
    );
}

/// The Root Pane's Tab gains a split while its refusal waits, so closing that Pane no longer
/// closes the Tab.
fn close_split_root() -> spaceterm_core::hierarchy::Inventory {
    let mut split = close_inventory(
        3,
        &[
            (
                CLOSE_ROOT_TAB,
                &[
                    (CLOSE_ROOT_PANE, Some([31; 32])),
                    (CLOSE_SPLIT_PANE, Some([32; 32])),
                ],
            ),
            (CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, Some([33; 32]))]),
        ],
    );
    split.close.as_mut().unwrap().controls = close_idle_tabs().close.unwrap().controls;
    split
}

#[gpui::test]
fn a_queued_refusal_never_reviews_after_a_split_joins_its_tab(cx: &mut TestAppContext) {
    a_queued_refusal_never_reviews_changed_work(close_split_root(), ReviewAnswer::Cancel, cx);
}

#[gpui::test]
fn reviewing_after_a_split_joins_confirms_only_what_the_fresh_confirmation_shows(
    cx: &mut TestAppContext,
) {
    a_queued_refusal_never_reviews_changed_work(
        close_split_root(),
        ReviewAnswer::ConfirmAfterAnotherChange,
        cx,
    );
}

/// A split's refusal waits for another Machine's confirmation. Cancel in the subsequent close
/// confirmation returns the keyboard to the original split.
#[gpui::test]
fn a_refused_split_close_beneath_another_machines_confirmation_returns_its_keyboard(
    cx: &mut TestAppContext,
) {
    use spaceterm_core::hierarchy::{CloseTarget as MachineCloseTarget, Mutation, Outcome};

    let records = TestTerminalSessionRecords::default();
    let (harness, cx) = close_harness_recording(records.clone(), cx);
    let (inventory, _) = close_published_controlling_root();
    harness.publish(inventory.clone(), cx);
    harness.panes.pane(CLOSE_SPLIT_PANE).reconnect();
    settle_panes(cx);
    let (remote_edits, remote_key) = present_paired_machine(&harness, cx);
    focus_active_workspace(&harness.manager, cx);
    let original = close_identity(&harness, cx);
    cx.simulate_keystrokes("a");
    let closing_session = last_key_session(&records).expect("the closing shell reads keys");

    cx.simulate_keystrokes("cmd-w");
    redraw(cx);
    assert_eq!(harness.closes().len(), 1);
    assert_eq!(close_identity(&harness, cx), original);
    open_paired_kill(&harness, remote_key, cx);
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(inventory.clone(), cx);
    settle_panes(cx);
    assert!(
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
            .find("Close Pane?")
            .is_none()
    );

    // The paired confirmation's Cancel opens the refusal's neutral review over the refused Pane.
    click("modal-action-close-cancel", cx);
    settle_panes(cx);
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    assert!(
        tree.find("Close Pane?").is_some(),
        "the refusal is reviewed"
    );
    assert!(tree.find("Pane Changed on Another Client").is_none());
    assert_eq!(close_identity(&harness, cx), original);

    // The review's Cancel returns the keyboard to the refused Pane's shell.
    click("modal-action-close-cancel", cx);
    settle_panes(cx);
    assert_eq!(close_identity(&harness, cx), original);
    assert!(close_terminal_is_focused(&harness, cx));
    let keys = key_count(&records);
    cx.simulate_keystrokes("x");
    assert_eq!(
        key_count(&records),
        keys + 1,
        "the next key reaches a shell"
    );
    assert_eq!(last_key_session(&records), Some(closing_session));
    assert_eq!(harness.closes().len(), 1, "Cancel closes nothing");
    assert!(
        remote_edits
            .submitted()
            .iter()
            .all(|mutation| !matches!(mutation, Mutation::Close { .. })),
        "Cancel kills nothing"
    );

    // A later review confirms exactly the refused Pane's Session.
    cx.simulate_keystrokes("cmd-w");
    redraw(cx);
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(inventory, cx);
    settle_panes(cx);
    click("modal-action-close-confirm", cx);
    let closes = harness.closes();
    assert!(
        matches!(closes.last(), Some(Mutation::Close { target: MachineCloseTarget::Pane { tab, pane, .. }, confirmation })
            if *tab == CLOSE_ROOT_TAB && *pane == CLOSE_ROOT_PANE
                && confirmation.own_controls.is_none()
                && confirmation.panes.iter().map(|p| (p.id, p.session)).eq([(CLOSE_ROOT_PANE, Some([31; 32]))])),
        "{closes:?}"
    );
}

/// A refusal keeps a Tab the person selects while the direct close is pending.
#[gpui::test]
fn a_close_refusal_keeps_the_tab_a_person_selected_while_waiting(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::Outcome;

    let records = TestTerminalSessionRecords::default();
    let (harness, cx) = close_harness_recording(records.clone(), cx);
    let idle = close_idle_tabs();
    harness.publish(idle.clone(), cx);
    harness.panes.pane(CLOSE_SECOND_PANE).reconnect();
    settle_panes(cx);
    focus_active_workspace(&harness.manager, cx);
    let original = close_identity(&harness, cx);
    cx.simulate_keystrokes("a");
    let closing_session = last_key_session(&records).expect("the closing shell reads keys");
    let closing_keys = |records: &TestTerminalSessionRecords| {
        records
            .commands()
            .iter()
            .filter(|call| {
                call.session_id == closing_session
                    && matches!(call.command, RecordedCommand::Key(_))
            })
            .count()
    };

    cx.simulate_keystrokes("cmd-w");
    redraw(cx);
    assert_eq!(harness.closes().len(), 1);
    assert_eq!(close_identity(&harness, cx), original);
    click("tab-item-2-inactive", cx);
    settle_panes(cx);
    let successor = close_identity(&harness, cx);
    assert_ne!(successor.0, original.0);

    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(idle, cx);
    settle_panes(cx);
    assert!(
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
            .find("Close Pane?")
            .is_some(),
        "the refusal is reviewed"
    );
    assert_eq!(
        close_identity(&harness, cx),
        successor,
        "the refusal keeps the chosen Tab"
    );

    click("modal-action-close-cancel", cx);
    settle_panes(cx);
    assert_eq!(close_identity(&harness, cx), successor);
    assert!(
        close_terminal_is_focused(&harness, cx),
        "the keyboard returns to the chosen Tab's Pane"
    );
    let keys = closing_keys(&records);
    cx.simulate_keystrokes("x");
    assert_eq!(
        closing_keys(&records),
        keys,
        "the next key never reaches the shell the person asked to close"
    );
    assert_eq!(harness.closes().len(), 1, "Cancel closes nothing");
}

/// Cancel keeps a split the person selects while the direct close is pending.
#[gpui::test]
fn a_close_refusal_keeps_the_pane_a_person_focused_while_waiting(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::Outcome;

    let records = TestTerminalSessionRecords::default();
    let (harness, cx) = close_harness_recording(records.clone(), cx);
    let (inventory, _) = close_published_controlling_root();
    harness.publish(inventory.clone(), cx);
    harness.panes.pane(CLOSE_SPLIT_PANE).reconnect();
    settle_panes(cx);
    focus_active_workspace(&harness.manager, cx);
    let original = close_identity(&harness, cx);

    cx.simulate_keystrokes("cmd-w");
    redraw(cx);
    assert_eq!(harness.closes().len(), 1);
    assert_eq!(close_identity(&harness, cx), original);
    cx.dispatch_action(crate::ui::FocusPaneRight);
    settle_panes(cx);
    let successor = close_identity(&harness, cx);
    assert_eq!(successor.0, original.0);
    assert_ne!(successor.1, original.1);
    cx.simulate_keystrokes("b");
    let chosen_session = last_key_session(&records).expect("the chosen shell reads keys");

    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(inventory, cx);
    settle_panes(cx);
    assert!(
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
            .find("Close Pane?")
            .is_some(),
        "the refusal is reviewed"
    );
    assert_eq!(
        close_identity(&harness, cx),
        successor,
        "the refusal keeps the chosen Pane"
    );

    click("modal-action-close-cancel", cx);
    settle_panes(cx);
    assert_eq!(close_identity(&harness, cx), successor);
    let keys = key_count(&records);
    cx.simulate_keystrokes("x");
    assert_eq!(
        key_count(&records),
        keys + 1,
        "the next key reaches a shell"
    );
    assert_eq!(last_key_session(&records), Some(chosen_session));
    assert_eq!(harness.closes().len(), 1, "Cancel closes nothing");
}

#[gpui::test]
fn a_direct_close_of_a_live_idle_pane_opens_the_replacement_after_acceptance(
    cx: &mut TestAppContext,
) {
    use crate::terminal::core_session::CorePaneStatus;
    use spaceterm_core::hierarchy::Outcome;

    let (harness, cx) = close_harness(cx);
    let mut idle = close_idle_tabs();
    idle.close.as_mut().unwrap().sessions[1].session = None;
    harness.publish(idle, cx);
    let replacement = harness.panes.pane(CLOSE_SECOND_PANE);
    replacement.set(CorePaneStatus::Restored);
    settle_panes(cx);
    focus_active_workspace(&harness.manager, cx);
    let original = close_identity(&harness, cx);
    cx.simulate_keystrokes("cmd-w");
    redraw(cx);
    assert_eq!(harness.closes().len(), 1);
    assert_eq!(close_identity(&harness, cx), original);
    assert_eq!(replacement.selections(), 0);

    harness.edits.answer(Outcome::Accepted {
        target: CLOSE_ROOT_PANE,
    });
    harness.publish(
        close_inventory(3, &[(CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, None)])]),
        cx,
    );
    assert_ne!(close_identity(&harness, cx).0, original.0);
    assert_eq!(replacement.selections(), 1);
    assert!(cx.debug_bounds("modal-action-close-confirm").is_none());
}

#[gpui::test]
fn a_direct_close_acceptance_does_not_open_a_tab_for_an_unrelated_removal(cx: &mut TestAppContext) {
    use crate::terminal::core_session::CorePaneStatus;
    use spaceterm_core::hierarchy::{ConfirmedControl, Outcome};

    let (harness, cx) = close_harness(cx);
    let remaining_tab = [44; 32];
    let remaining_pane = [45; 32];
    let mut inventory = close_inventory(
        2,
        &[
            (CLOSE_ROOT_TAB, &[(CLOSE_ROOT_PANE, Some([31; 32]))]),
            (CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, Some([33; 32]))]),
            (remaining_tab, &[(remaining_pane, None)]),
        ],
    );
    inventory.close.as_mut().unwrap().controls = vec![ConfirmedControl {
        pane: CLOSE_SECOND_PANE,
        session: Some([33; 32]),
        owner: [1; 32],
        epoch: 1,
    }];
    harness.publish(inventory, cx);
    harness.panes.pane(CLOSE_SECOND_PANE).reconnect();
    let replacement = harness.panes.pane(remaining_pane);
    replacement.set(CorePaneStatus::Restored);
    settle_panes(cx);
    focus_active_workspace(&harness.manager, cx);
    click("tab-close-button-2", cx);
    assert_eq!(harness.closes().len(), 1);

    // This client's inactive Tab closes and another client removes the Active Tab in the
    // same publication. The dormant Pane must stay dormant.
    harness.edits.answer(Outcome::Accepted {
        target: CLOSE_SECOND_TAB,
    });
    harness.publish(
        close_inventory(3, &[(remaining_tab, &[(remaining_pane, None)])]),
        cx,
    );
    assert_eq!(replacement.selections(), 0);
}

/// The shell the person asked to close exits while the neutral confirmation of a refused own
/// close is open, as the native `read -k` case ends with a signal. The confirmation gives way to
/// one notice that names no other client, and that notice presents the refusal's outcome, so no
/// later reading presents that outcome again as a second notice. Its Review opens one fresh
/// confirmation of what the close ends now, and only its Confirm closes.
#[gpui::test]
fn a_shell_exit_under_a_refused_close_reviews_once_without_another_notice(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::{CloseTarget as MachineCloseTarget, ConfirmedControl};
    use spaceterm_core::hierarchy::{Mutation, Outcome};

    let (harness, cx) = close_harness(cx);
    let mut idle = close_inventory(
        2,
        &[
            (CLOSE_ROOT_TAB, &[(CLOSE_ROOT_PANE, Some([31; 32]))]),
            (CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, Some([33; 32]))]),
        ],
    );
    idle.close.as_mut().unwrap().controls = vec![ConfirmedControl {
        pane: CLOSE_ROOT_PANE,
        session: Some([31; 32]),
        owner: [1; 32],
        epoch: 1,
    }];
    harness.publish(idle.clone(), cx);
    harness.panes.pane(CLOSE_SECOND_PANE).reconnect();
    settle_panes(cx);
    focus_active_workspace(&harness.manager, cx);

    cx.simulate_keystrokes("cmd-w");
    redraw(cx);
    assert_eq!(harness.closes().len(), 1);
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(idle.clone(), cx);
    settle_panes(cx);
    assert!(
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
            .find("Close Pane?")
            .is_some(),
        "the refusal is reviewed"
    );

    // The shell exits: its Terminal Session ends and the Pane stays.
    let mut ended = idle;
    ended.revision += 1;
    let live = ended.close.as_mut().unwrap();
    live.sessions[0].session = None;
    live.controls[0].session = None;
    harness
        .panes
        .pane(CLOSE_ROOT_PANE)
        .set(crate::terminal::core_session::CorePaneStatus::Ended);
    harness.publish(ended, cx);
    settle_panes(cx);
    // Later readings find the refusal's outcome still presented.
    harness.sync(cx);
    harness.sync(cx);
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    assert!(
        tree.find("Close Pane?").is_none(),
        "the stale confirmation ends"
    );
    assert!(tree.find("Pane Changed").is_some());
    assert!(
        tree.find("Pane Changed on Another Client").is_none(),
        "a shell's own exit is no other client's change"
    );
    assert!(
        tree.find("Check the Workspace before closing it again.")
            .is_none(),
        "the refusal's outcome is presented once"
    );

    click("modal-action-close-review", cx);
    settle_panes(cx);
    harness.sync(cx);
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    assert!(
        tree.find("Close Pane?").is_some(),
        "Review opens the fresh confirmation at once"
    );
    assert!(
        tree.find("Pane Changed").is_none()
            && tree
                .find("Check the Workspace before closing it again.")
                .is_none(),
        "no other notice comes before or over it"
    );
    assert!(
        harness.edits.unseen().is_empty(),
        "answering the notice recorded the refusal's outcome as seen"
    );
    assert_eq!(harness.closes().len(), 1, "Review closes nothing by itself");

    click("modal-action-close-confirm", cx);
    settle_panes(cx);
    let closes = harness.closes();
    assert_eq!(closes.len(), 2, "{closes:?}");
    assert!(
        matches!(closes.last(), Some(Mutation::Close { target: MachineCloseTarget::Pane { tab, pane, .. }, confirmation })
            if *tab == CLOSE_ROOT_TAB && *pane == CLOSE_ROOT_PANE
                && confirmation.own_controls.is_none()
                && confirmation.panes.iter().map(|p| (p.id, p.session)).eq([(CLOSE_ROOT_PANE, None)])),
        "the confirmed close ends exactly what the fresh confirmation showed: {closes:?}"
    );
    assert!(
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
            .find("Check the Workspace before closing it again.")
            .is_none()
    );
}

/// A Review of a new Tab waits for its starting shell. Meanwhile the person asks to kill a
/// paired Machine's Workspace, whose confirmation still holds the window when the shell starts.
/// The Review waits for that confirmation instead of ending, and its Cancel opens one fresh
/// confirmation of the Tab with the Session its shell started. Nothing closes before that
/// confirmation's own Confirm, and the kill's answer approves nothing of it.
#[gpui::test]
fn a_review_held_for_a_starting_shell_waits_for_another_confirmation(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::{CloseTarget as MachineCloseTarget, Mutation, Outcome};

    let (harness, cx) = close_harness(cx);
    let (remote_edits, remote_key) = present_paired_machine(&harness, cx);
    let starting = |session| {
        let mut inventory = pending_close_inventory();
        if let Some(close) = inventory.close.as_mut()
            && let Some(pane) = close
                .sessions
                .iter_mut()
                .find(|pane| pane.id == PENDING_CLOSE_PANE)
        {
            pane.session = session;
        }
        inventory
    };
    click("create-tab-button", cx);
    harness.edits.answer(Outcome::Accepted {
        target: PENDING_CLOSE_TAB,
    });
    harness.publish(starting(None), cx);
    focus_active_workspace(&harness.manager, cx);
    let (tab_id, _) = close_identity(&harness, cx);
    let target = cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            let workspace_id = manager
                .workspaces
                .active_workspace_id()
                .expect("an active Workspace");
            let target = CloseTarget::Tab {
                workspace_id,
                tab_id,
            };
            manager.request_core_close_as(target, CloseRequest::review(true), window, cx);
            target
        })
    });
    redraw(cx);
    assert!(
        cx.debug_bounds("modal-action-close-confirm").is_none(),
        "the Review waits for the starting shell"
    );

    open_paired_kill(&harness, remote_key, cx);

    // The shell starts and reports its Session while the kill's confirmation is open.
    harness.panes.pane(PENDING_CLOSE_PANE).reconnect();
    let mut started = starting(Some([82; 32]));
    started.revision += 1;
    harness.publish(started, cx);
    cx.executor()
        .advance_clock(crate::ui::workspace_manager::CLOSE_START_POLL * 2);
    redraw(cx);
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    assert!(
        tree.find("Kill Workspace \u{201C}Remote\u{201D}?")
            .is_some()
    );
    assert!(tree.find("Close Tab?").is_none());
    assert!(harness.closes().is_empty());

    click("modal-action-close-cancel", cx);
    settle_panes(cx);
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    assert!(
        tree.find("Kill Workspace \u{201C}Remote\u{201D}?")
            .is_none()
    );
    assert!(
        tree.find("Close Tab?").is_some(),
        "the waiting Review opens its fresh confirmation once the window is free"
    );
    assert!(
        harness.closes().is_empty(),
        "Cancel of the kill closes nothing"
    );
    assert!(
        remote_edits
            .submitted()
            .iter()
            .all(|mutation| !matches!(mutation, Mutation::Close { .. })),
        "Cancel kills nothing"
    );
    assert_eq!(
        cx.update(|_, cx| harness
            .manager
            .read(cx)
            .close_confirmation
            .pending()
            .map(|p| p.target)),
        Some(target),
        "the fresh confirmation is the Review's own"
    );

    click("modal-action-close-confirm", cx);
    settle_panes(cx);
    harness.sync(cx);
    assert_eq!(
        harness.closes(),
        [close_mutation(
            MachineCloseTarget::Tab {
                workspace: CLOSE_WORKSPACE,
                tab: PENDING_CLOSE_TAB
            },
            &[(PENDING_CLOSE_PANE, Some([82; 32]))],
            WorkspaceCloseRemoves::Tab,
        )],
        "the confirmed close ends exactly the Session the shell started"
    );
    assert!(
        cx.debug_bounds("modal-action-close-confirm").is_none(),
        "the Review asks once"
    );
    assert!(
        close_terminal_is_focused(&harness, cx),
        "the keyboard the person's request held moves to what remains"
    );
}

/// A Review of a new Tab's Pane waits for its starting shell. The shell starts and the server
/// reports its Terminal Session idle under this client's control, so the Pane alone would close
/// without asking, but the wait has not looked again yet. The person presses Command-W again.
/// The repeat joins the waiting Review instead of closing directly: one fresh confirmation opens,
/// nothing closes before its Confirm, and the wait opens no second one.
#[gpui::test]
fn a_repeat_close_joins_the_review_waiting_for_a_started_shell(cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::{CloseTarget as MachineCloseTarget, ConfirmedControl};
    use spaceterm_core::hierarchy::{Mutation, Outcome};

    let (harness, cx) = close_harness(cx);
    let inventory = |session: Option<[u8; 32]>| {
        let mut inventory = pending_close_inventory();
        let close = inventory.close.as_mut().expect("close facts");
        close
            .sessions
            .iter_mut()
            .find(|pane| pane.id == PENDING_CLOSE_PANE)
            .expect("the new Pane")
            .session = session;
        close.controls = vec![ConfirmedControl {
            pane: PENDING_CLOSE_PANE,
            session,
            owner: [1; 32],
            epoch: 1,
        }];
        inventory
    };
    click("create-tab-button", cx);
    harness.edits.answer(Outcome::Accepted {
        target: PENDING_CLOSE_TAB,
    });
    harness.publish(inventory(None), cx);
    focus_active_workspace(&harness.manager, cx);
    let (tab_id, pane_id) = close_identity(&harness, cx);
    let target = cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            let workspace_id = manager
                .workspaces
                .active_workspace_id()
                .expect("an active Workspace");
            let target = CloseTarget::Pane {
                workspace_id,
                tab_id,
                pane_id,
            };
            manager.request_core_close_as(target, CloseRequest::review(true), window, cx);
            target
        })
    });
    redraw(cx);
    assert!(
        cx.debug_bounds("modal-action-close-confirm").is_none(),
        "the Review waits for the starting shell"
    );

    // The shell starts and the server reports its Session idle under this client's control,
    // before the wait looks again.
    harness.panes.pane(PENDING_CLOSE_PANE).reconnect();
    let mut started = inventory(Some([82; 32]));
    started.revision += 1;
    harness.publish(started, cx);
    redraw(cx);
    assert!(
        cx.update(|_, cx| { !harness.manager.read(cx).close_shell_starting(target, cx) }),
        "the shell no longer starts"
    );

    cx.simulate_keystrokes("cmd-w");
    redraw(cx);
    assert!(
        harness.closes().is_empty(),
        "the repeat never closes directly"
    );
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    assert!(
        tree.find("Close Pane?").is_some(),
        "the repeat opens the waiting Review's fresh confirmation"
    );
    assert_eq!(
        cx.update(|_, cx| harness
            .manager
            .read(cx)
            .close_confirmation
            .pending()
            .map(|p| p.target)),
        Some(target)
    );

    // The wait's own look finds nothing left to resume.
    cx.executor()
        .advance_clock(crate::ui::workspace_manager::CLOSE_START_POLL * 4);
    redraw(cx);
    assert!(harness.closes().is_empty());

    click("modal-action-close-confirm", cx);
    settle_panes(cx);
    harness.sync(cx);
    cx.executor()
        .advance_clock(crate::ui::workspace_manager::CLOSE_START_POLL * 4);
    redraw(cx);
    let closes = harness.closes();
    assert_eq!(closes.len(), 1, "{closes:?}");
    assert!(
        matches!(closes.last(), Some(Mutation::Close { target: MachineCloseTarget::Pane { tab, pane, .. }, confirmation })
            if *tab == PENDING_CLOSE_TAB && *pane == PENDING_CLOSE_PANE
                && confirmation.panes.iter().map(|p| (p.id, p.session)).eq([(PENDING_CLOSE_PANE, Some([82; 32]))])),
        "the Confirm ends exactly the Session the shell started: {closes:?}"
    );
    assert!(
        cx.debug_bounds("modal-action-close-confirm").is_none(),
        "the Review asks once"
    );
}

/// A Core restart fences a close the person confirmed: the new boot returns the same Pane with no
/// Terminal Session, and this client already controls it again. The confirmation named the old
/// boot's Session, so the retry opens a fresh confirmation instead of closing directly, and only
/// its Confirm ends exactly what it shows. The refused Tab returns with the keyboard, so that
/// Confirm moves the keyboard to the Tab that remains, and the next key reaches its shell.
#[gpui::test]
fn a_confirmed_close_fenced_by_a_restart_asks_again(cx: &mut TestAppContext) {
    confirm_a_close_fenced_by_a_restart(false, cx);
}

/// A person who clicks the terminal of the Tab a confirmed close opened chose that Tab. A Core
/// restart fences the close, and the retry's fresh confirmation never takes the keyboard back:
/// its Confirm leaves the next key in the Tab the person chose.
#[gpui::test]
fn a_close_fenced_by_a_restart_keeps_the_tab_a_person_chose(cx: &mut TestAppContext) {
    confirm_a_close_fenced_by_a_restart(true, cx);
}

/// With the first Tab active, VoiceOver focuses the second Tab's strip item without selecting it,
/// opens that Tab's menu and chooses Close Tab. Another client runs its shell, so the close asks
/// first. Confirming removes the strip item the confirmation took the keyboard from, so the
/// keyboard moves to the Tab that remains, and the next key reaches its shell.
#[gpui::test]
fn confirming_an_inactive_tab_close_from_its_focused_strip_item_keeps_typing_in_the_active_tab(
    cx: &mut TestAppContext,
) {
    use gpui::accesskit::Action;
    use spaceterm_core::hierarchy::CloseTarget as MachineCloseTarget;
    use spaceterm_core::hierarchy::Mutation;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    let records = TestTerminalSessionRecords::default();
    let (harness, cx) = close_harness_recording(records.clone(), cx);
    let running = close_inventory(
        2,
        &[
            (CLOSE_ROOT_TAB, &[(CLOSE_ROOT_PANE, Some([31; 32]))]),
            (CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, Some([33; 32]))]),
        ],
    );
    harness.publish(running, cx);
    harness.panes.pane(CLOSE_SECOND_PANE).reconnect();
    settle_panes(cx);
    focus_active_workspace(&harness.manager, cx);
    let active = close_identity(&harness, cx);
    cx.simulate_keystrokes("a");
    let active_session = last_key_session(&records).expect("the active Tab's shell reads keys");

    let tree = A11yTree::read(cx);
    perform(cx, tree.with_role("Tab")[1], Action::Focus);
    let tree = A11yTree::read(cx);
    perform(cx, tree.with_role("Tab")[1], Action::ShowContextMenu);
    assert_eq!(
        close_identity(&harness, cx),
        active,
        "the menu selects nothing"
    );
    click("tab-menu-row-close", cx);
    redraw(cx);
    assert!(
        cx.debug_bounds("modal-action-close-confirm").is_some(),
        "another client's work asks first"
    );
    click("modal-action-close-confirm", cx);
    settle_panes(cx);

    let closes = harness.closes();
    assert!(
        matches!(closes.as_slice(), [Mutation::Close { target: MachineCloseTarget::Tab { tab, .. }, .. }]
            if *tab == CLOSE_SECOND_TAB),
        "the Confirm closes the second Tab: {closes:?}"
    );
    assert_eq!(close_identity(&harness, cx), active);
    assert!(
        close_terminal_is_focused(&harness, cx),
        "the keyboard moves to the Tab that remains"
    );
    let keys = key_count(&records);
    cx.simulate_keystrokes("x");
    assert_eq!(
        key_count(&records),
        keys + 1,
        "the next key reaches a shell"
    );
    assert_eq!(
        last_key_session(&records),
        Some(active_session),
        "the next key reaches the active Tab's shell"
    );
}

/// Confirms closing the second Tab, whose shell another client runs, then fences that close with
/// a Core restart and confirms the retry's fresh confirmation. With `chooses_survivor`, the person
/// clicks the terminal of the Tab the first close opened before the restart's answer arrives.
fn confirm_a_close_fenced_by_a_restart(chooses_survivor: bool, cx: &mut TestAppContext) {
    use spaceterm_core::hierarchy::{CloseTarget as MachineCloseTarget, ConfirmedControl};
    use spaceterm_core::hierarchy::{Mutation, Outcome};

    let records = TestTerminalSessionRecords::default();
    let (harness, cx) = close_harness_recording(records.clone(), cx);
    let running = close_inventory(
        2,
        &[
            (CLOSE_ROOT_TAB, &[(CLOSE_ROOT_PANE, Some([31; 32]))]),
            (CLOSE_SECOND_TAB, &[(CLOSE_SECOND_PANE, Some([33; 32]))]),
        ],
    );
    harness.publish(running.clone(), cx);
    harness.panes.pane(CLOSE_SECOND_PANE).reconnect();
    settle_panes(cx);
    focus_active_workspace(&harness.manager, cx);
    let survivor = close_identity(&harness, cx);
    cx.simulate_keystrokes("a");
    let survivor_session = last_key_session(&records).expect("the Root Tab's shell reads keys");
    cx.dispatch_action(crate::ui::NextTab);
    settle_panes(cx);
    let closing = close_identity(&harness, cx);
    assert_ne!(closing.0, survivor.0);
    assert!(close_terminal_is_focused(&harness, cx));

    cx.simulate_keystrokes("cmd-w");
    redraw(cx);
    assert!(
        cx.debug_bounds("modal-action-close-confirm").is_some(),
        "another client's work asks first"
    );
    click("modal-action-close-confirm", cx);
    settle_panes(cx);
    assert_eq!(harness.closes().len(), 1);
    assert_eq!(
        close_identity(&harness, cx),
        survivor,
        "the approved close opens the neighbor"
    );
    if chooses_survivor {
        click(format!("pane-surface-{}", survivor.1.get()).leak(), cx);
        settle_panes(cx);
        assert_eq!(close_identity(&harness, cx), survivor);
    }

    // The Core restarted before it handled the close. The new boot already started a shell this
    // client controls in the Root Tab, which the close never named.
    let mut restarted = running;
    restarted.revision += 1;
    let live = restarted.close.as_mut().unwrap();
    live.boot = [77; 32];
    let survivor_shell = Some([88; 32]);
    for pane in &mut live.sessions {
        pane.session = (pane.id == CLOSE_ROOT_PANE)
            .then_some(survivor_shell)
            .flatten();
    }
    live.controls = vec![
        ConfirmedControl {
            pane: CLOSE_SECOND_PANE,
            session: None,
            owner: [1; 32],
            epoch: 1,
        },
        ConfirmedControl {
            pane: CLOSE_ROOT_PANE,
            session: survivor_shell,
            owner: [1; 32],
            epoch: 1,
        },
    ];
    harness.edits.answer(Outcome::StaleConfirmation);
    harness.publish(restarted, cx);
    settle_panes(cx);
    harness.sync(cx);
    assert_eq!(
        harness.closes().len(),
        1,
        "the new boot never takes the old confirmation"
    );
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);
    assert!(tree.find("Close Pane?").is_some(), "the retry asks again");
    assert!(tree.find("Pane Changed on Another Client").is_none());
    assert_eq!(
        close_identity(&harness, cx),
        if chooses_survivor { survivor } else { closing },
        "the refused Tab returns selected unless the person chose another"
    );

    click("modal-action-close-confirm", cx);
    settle_panes(cx);
    let closes = harness.closes();
    assert_eq!(closes.len(), 2, "{closes:?}");
    assert!(
        matches!(closes.last(), Some(Mutation::Close { target: MachineCloseTarget::Pane { tab, pane, .. }, confirmation })
            if *tab == CLOSE_SECOND_TAB && *pane == CLOSE_SECOND_PANE
                && confirmation.boot == [77; 32]
                && confirmation.own_controls.is_none()
                && confirmation.panes.iter().map(|p| (p.id, p.session)).eq([(CLOSE_SECOND_PANE, None)])),
        "the Confirm ends exactly what the fresh confirmation showed: {closes:?}"
    );
    assert_eq!(
        close_identity(&harness, cx),
        survivor,
        "the confirmed close opens the neighbor again"
    );
    assert!(
        close_terminal_is_focused(&harness, cx),
        "the keyboard the confirmed close held moves to what remains"
    );
    let keys = key_count(&records);
    cx.simulate_keystrokes("x");
    assert_eq!(
        key_count(&records),
        keys + 1,
        "the next key reaches a shell"
    );
    assert_eq!(
        last_key_session(&records),
        Some(survivor_session),
        "the next key reaches the Root Tab's shell"
    );
}
