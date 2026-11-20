use super::*;
use spaceterm_core::hierarchy::{
    CloseInventory, CloseTarget as MachineCloseTarget, ConfirmedPane, Mutation, Outcome,
};
use spaceterm_core::worktree::{Error, Operation};
use std::path::Path;

fn only_feature() -> Inventory {
    let mut inventory = published(1, "main", true);
    inventory.workspaces[0]
        .tabs
        .retain(|tab| tab.id == FEATURE_TAB);
    inventory.workspaces[0].root_tab = FEATURE_TAB;
    live_close(&mut inventory);
    inventory
}

fn live_close(inventory: &mut Inventory) {
    inventory.close = Some(CloseInventory {
        controls: Vec::new(),
        boot: [9; 32],
        sessions: inventory.workspaces[0]
            .tabs
            .iter()
            .flat_map(|tab| &tab.panes)
            .map(|pane| ConfirmedPane {
                id: pane.id,
                session: Some(pane.id),
            })
            .collect(),
    });
}

fn sync(harness: &Harness, inventory: Inventory, cx: &mut VisualTestContext) {
    harness.edits.set_inventory(inventory);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            let owner = manager.binding.owner(harness.binding).unwrap();
            manager.sync_workspaces(owner, window, cx);
        });
    });
    redraw(cx);
}

fn request(harness: &Harness, cx: &mut VisualTestContext) {
    let (_, feature) = worktree_ids(harness, cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.request_core_close(
                CloseTarget::Worktree {
                    workspace_id: harness.binding,
                    worktree_id: feature,
                },
                window,
                cx,
            );
        });
    });
    redraw(cx);
}

fn expected_open(path: &str) -> Mutation {
    Mutation::Worktree(Operation::OpenTab {
        workspace: WORKSPACE,
        source: None,
        root: MAIN_ROOT.into(),
        path: path.into(), control: spaceterm_core::hierarchy::CreateControl::Creator,
    })
}

fn accepted_inventory(mut inventory: Inventory) -> Inventory {
    let mut outside = published(1, "main", true).workspaces[0].tabs[0].clone();
    outside.id = [80; 32];
    outside.root_pane = [81; 32];
    outside.panes[0].id = outside.root_pane;
    inventory.workspaces[0].tabs.push(outside);
    inventory.revision += 1;
    live_close(&mut inventory);
    inventory
}

#[gpui::test]
fn worktree_fallback_acceptance_reviews_current_exact_targets_without_opening_the_fallback(
    cx: &mut TestAppContext,
) {
    let initial = only_feature();
    let (harness, _, _, cx) = paired_harness(cx, initial.clone());
    let feature = worktree_ids(&harness, cx).1;
    assert!(
        harness
            .row(cx)
            .worktrees
            .unwrap()
            .rows()
            .find(|row| row.worktree_id == feature)
            .unwrap()
            .closes_tabs
    );
    request(&harness, cx);
    request(&harness, cx);
    assert_eq!(harness.edits.submitted(), [expected_open(MAIN_ROOT)]);
    assert!(
        cx.debug_bounds("modal-action-close-confirm")
            .is_none()
    );

    // A later navigation owns the keyboard while membership is being established.
    click(workspace_row(&harness.manager, 1, cx), cx);
    let active = harness
        .manager
        .read_with(cx, |manager, _| manager.workspaces.active_workspace_id());
    let mut accepted = accepted_inventory(initial);
    let mut added = accepted.workspaces[0].tabs[0].clone();
    added.id = [82; 32];
    added.root_pane = [83; 32];
    added.panes[0].id = added.root_pane;
    accepted.workspaces[0].tabs.push(added);
    live_close(&mut accepted);
    harness.edits.answer(Outcome::Accepted { target: [80; 32] });
    sync(&harness, accepted.clone(), cx);

    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.workspaces.active_workspace_id()),
        active
    );
    let active_core_tab = harness.manager.read_with(cx, |manager, cx| {
        let tabs = manager
            .workspaces
            .workspace(harness.binding)
            .unwrap()
            .payload()
            .read(cx);
        tabs.core_tab(tabs.active_terminal_identity(cx).0)
    });
    assert_eq!(
        active_core_tab,
        Some(FEATURE_TAB),
        "fallback membership does not activate it"
    );
    assert!(
        cx.debug_bounds("modal-action-close-confirm")
            .is_some()
    );
    click("modal-action-close-confirm", cx);
    let target = MachineCloseTarget::Worktree {
        workspace: WORKSPACE,
        tab: FEATURE_TAB,
    };
    assert_eq!(
        harness.edits.submitted(),
        [
            expected_open(MAIN_ROOT),
            Mutation::Close {
                target,
                confirmation: accepted.close_confirmation(target).unwrap(),
            }
        ],
        "fresh approval captures the added target Tab and excludes the fallback"
    );
}

#[gpui::test]
fn worktree_fallback_failure_keeps_every_existing_tab(cx: &mut TestAppContext) {
    let initial = only_feature();
    let (harness, _, _, cx) = paired_harness(cx, initial.clone());
    request(&harness, cx);
    harness
        .edits
        .answer(Outcome::WorktreeFailed(Error::Location));
    sync(&harness, initial, cx);
    assert_eq!(harness.edits.submitted(), [expected_open(MAIN_ROOT)]);
    assert!(
        cx.debug_bounds("modal-action-close-confirm")
            .is_none()
    );
    assert!(
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
            .find("Tabs Not Closed")
            .is_some()
    );
    assert!(
        harness
            .row(cx)
            .worktrees
            .unwrap()
            .rows()
            .any(|row| row.has_tabs)
    );
}

/// The fallback's own alert waits behind another one when the window closes, so nobody saw
/// the failure. The reopened window presents it.
#[gpui::test]
fn a_fallback_failure_whose_alert_never_drew_should_reach_the_next_window(
    cx: &mut TestAppContext,
) {
    let initial = only_feature();
    let (harness, _, _, cx) = paired_harness(cx, initial.clone());
    request(&harness, cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |_, cx| {
            Alert::new(
                ModalId::new("blocking-alert"),
                "Blocking",
                "Blocking",
                "Another alert is open.",
                vec![ModalAction::new(
                    (),
                    "OK",
                    ModalActionRole::Cancel,
                    "blocking-alert-ok",
                )],
            )
            .present(window, cx, |_, _| {})
            .expect("the blocking alert opens");
        });
    });
    harness
        .edits
        .answer(Outcome::WorktreeFailed(Error::Location));
    sync(&harness, initial, cx);
    let queued = harness.edits.unseen().len();
    close_window(cx);
    let mut reopened = reopened_window(&harness.edits, cx);
    reopened.run_until_parked();

    assert_eq!(queued, 1, "the queued fallback alert has not presented the failure");
    assert!(
        reopened
            .debug_bounds("modal-action-server-worktree-operation-failed-ok")
            .is_some(),
        "the reopened window presents it"
    );
}

#[gpui::test]
fn worktree_fallback_requires_accepted_membership_even_when_a_tab_is_published(
    cx: &mut TestAppContext,
) {
    let initial = only_feature();
    let (harness, _, _, cx) = paired_harness(cx, initial.clone());
    request(&harness, cx);
    let published = accepted_inventory(initial);
    sync(&harness, published.clone(), cx);
    assert!(
        cx.debug_bounds("modal-action-close-confirm")
            .is_none()
    );
    assert_eq!(harness.edits.submitted(), [expected_open(MAIN_ROOT)]);
    harness.edits.answer(Outcome::Accepted { target: [99; 32] });
    sync(&harness, published, cx);
    assert!(
        cx.debug_bounds("modal-action-close-confirm")
            .is_none()
    );
    assert_eq!(harness.edits.submitted(), [expected_open(MAIN_ROOT)]);
}

#[gpui::test]
fn worktree_fallback_with_no_usable_destination_closes_nothing(cx: &mut TestAppContext) {
    let mut initial = only_feature();
    for directory in &mut initial.repositories.as_mut().unwrap().directories {
        for record in &mut directory.worktrees.as_mut().unwrap().worktrees {
            if record.root == Path::new(MAIN_ROOT) {
                record.missing = true;
            }
        }
    }
    let (harness, _, _, cx) = paired_harness(cx, initial);
    let feature = worktree_ids(&harness, cx).1;
    assert!(
        !harness
            .row(cx)
            .worktrees
            .unwrap()
            .rows()
            .find(|row| row.worktree_id == feature)
            .unwrap()
            .closes_tabs
    );
    request(&harness, cx);
    assert!(harness.edits.submitted().is_empty());
    assert!(
        cx.debug_bounds("modal-action-close-confirm")
            .is_none()
    );
    assert!(
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
            .find("Tabs Not Closed")
            .is_some()
    );
}

#[gpui::test]
fn worktree_fallback_never_closes_a_reassigned_original_anchor(cx: &mut TestAppContext) {
    let initial = only_feature();
    let (harness, _, _, cx) = paired_harness(cx, initial.clone());
    request(&harness, cx);
    let mut accepted = accepted_inventory(initial);
    accepted.workspaces[0].tabs[0].worktree = Some(EXTRA_ROOT.into());
    harness.edits.answer(Outcome::Accepted { target: [80; 32] });
    sync(&harness, accepted, cx);
    assert_eq!(harness.edits.submitted(), [expected_open(MAIN_ROOT)]);
    assert!(
        cx.debug_bounds("modal-action-close-confirm")
            .is_none()
    );
}

/// `inventory` with every Pane's Terminal Session under this client's control, so closing them
/// needs no confirmation while they are idle.
fn controlled(mut inventory: Inventory) -> Inventory {
    let close = inventory.close.as_mut().expect("live close facts");
    close.controls = close
        .sessions
        .iter()
        .map(|pane| spaceterm_core::hierarchy::ConfirmedControl {
            pane: pane.id,
            session: pane.session,
            owner: [1; 32],
            epoch: 1,
        })
        .collect();
    inventory
}

/// Asks to close the feature Worktree's Tabs as the Review of a changed close does.
fn request_review(harness: &Harness, cx: &mut VisualTestContext) {
    let (_, feature) = worktree_ids(harness, cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.request_core_close_as(
                CloseTarget::Worktree {
                    workspace_id: harness.binding,
                    worktree_id: feature,
                },
                crate::ui::workspace_manager::CloseRequest::review(false),
                window,
                cx,
            );
        });
    });
    redraw(cx);
}

/// How the Worktree close reaches the fallback Tab creation.
#[derive(Clone, Copy, Debug)]
enum FallbackRequests {
    /// An ordinary close of idle Tabs this client controls.
    Plain,
    /// A Review starts the creation.
    Review,
    /// An ordinary close starts the creation, and a Review joins it.
    PlainThenReview,
}

/// The feature Worktree's idle Tabs, which this client controls, are its Workspace's only Tabs,
/// so closing them first creates a Tab outside the Worktree. A plain close then ends them
/// directly. A Review, whether it starts that creation or joins one already in flight, still
/// opens a fresh confirmation of what the close ends once the Tab exists, and nothing closes
/// before its Confirm.
fn close_through_the_fallback_tab(requests: FallbackRequests, cx: &mut TestAppContext) {
    let initial = controlled(only_feature());
    let (harness, _, _, cx) = paired_harness(cx, initial.clone());
    match requests {
        FallbackRequests::Plain => request(&harness, cx),
        FallbackRequests::Review => request_review(&harness, cx),
        FallbackRequests::PlainThenReview => {
            request(&harness, cx);
            request_review(&harness, cx);
        }
    }
    assert_eq!(
        harness.edits.submitted(),
        [expected_open(MAIN_ROOT)],
        "one fallback creation"
    );
    let accepted = controlled(accepted_inventory(initial));
    harness.edits.answer(Outcome::Accepted { target: [80; 32] });
    sync(&harness, accepted.clone(), cx);
    let target = MachineCloseTarget::Worktree {
        workspace: WORKSPACE,
        tab: FEATURE_TAB,
    };
    let confirmation = accepted.close_confirmation(target).unwrap();
    if matches!(requests, FallbackRequests::Plain) {
        let submitted = harness.edits.submitted();
        assert!(
            matches!(&submitted[..], [_, Mutation::Close { target: closed, confirmation: direct }]
                if *closed == target && direct.own_controls.is_some()
                    && direct.same_targets(&confirmation)),
            "an ordinary close of idle controlled Tabs needs no confirmation: {submitted:?}"
        );
        return;
    }
    assert!(
        cx.debug_bounds("modal-action-close-confirm").is_some(),
        "the Review opens a fresh confirmation once the Tab exists"
    );
    assert_eq!(
        harness.edits.submitted(),
        [expected_open(MAIN_ROOT)],
        "nothing closes before the Confirm"
    );
    click("modal-action-close-confirm", cx);
    assert_eq!(
        harness.edits.submitted(),
        [
            expected_open(MAIN_ROOT),
            Mutation::Close {
                target,
                confirmation,
            }
        ],
        "the Confirm ends exactly what the fresh confirmation showed"
    );
}

#[gpui::test]
fn a_plain_close_through_the_fallback_tab_ends_idle_controlled_tabs_directly(
    cx: &mut TestAppContext,
) {
    close_through_the_fallback_tab(FallbackRequests::Plain, cx);
}

#[gpui::test]
fn a_review_that_creates_the_fallback_tab_waits_for_fresh_confirmation(cx: &mut TestAppContext) {
    close_through_the_fallback_tab(FallbackRequests::Review, cx);
}

#[gpui::test]
fn a_review_that_joins_the_fallback_creation_waits_for_fresh_confirmation(
    cx: &mut TestAppContext,
) {
    close_through_the_fallback_tab(FallbackRequests::PlainThenReview, cx);
}
