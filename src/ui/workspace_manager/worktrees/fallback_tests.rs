use super::*;
use spaceterm_server::hierarchy::{
    CloseInventory, CloseTarget as ServerCloseTarget, ConfirmedPane, Mutation, Outcome,
};
use spaceterm_server::worktree::{Error, Operation};
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
            let owner = manager.shared.owner(harness.shared).unwrap();
            manager.sync_shared_workspaces(owner, window, cx);
        });
    });
    redraw(cx);
}

fn request(harness: &Harness, cx: &mut VisualTestContext) {
    let (_, feature) = worktree_ids(harness, cx);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.request_shared_close(
                CloseTarget::Worktree {
                    workspace_id: harness.shared,
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
        path: path.into(),
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
    let (harness, _, _, cx) = ssh_harness(cx, initial.clone());
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
        cx.debug_bounds("modal-action-shared-close-confirm")
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
    let active_server_tab = harness.manager.read_with(cx, |manager, cx| {
        let tabs = manager
            .workspaces
            .workspace(harness.shared)
            .unwrap()
            .payload()
            .read(cx);
        tabs.shared_server_tab(tabs.active_terminal_identity(cx).0)
    });
    assert_eq!(
        active_server_tab,
        Some(FEATURE_TAB),
        "fallback membership does not activate it"
    );
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_some()
    );
    click("modal-action-shared-close-confirm", cx);
    let target = ServerCloseTarget::Worktree {
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
    let (harness, _, _, cx) = ssh_harness(cx, initial.clone());
    request(&harness, cx);
    harness
        .edits
        .answer(Outcome::WorktreeFailed(Error::Location));
    sync(&harness, initial, cx);
    assert_eq!(harness.edits.submitted(), [expected_open(MAIN_ROOT)]);
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
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

#[gpui::test]
fn worktree_fallback_requires_accepted_membership_even_when_a_tab_is_published(
    cx: &mut TestAppContext,
) {
    let initial = only_feature();
    let (harness, _, _, cx) = ssh_harness(cx, initial.clone());
    request(&harness, cx);
    let published = accepted_inventory(initial);
    sync(&harness, published.clone(), cx);
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );
    assert_eq!(harness.edits.submitted(), [expected_open(MAIN_ROOT)]);
    harness.edits.answer(Outcome::Accepted { target: [99; 32] });
    sync(&harness, published, cx);
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
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
    let (harness, _, _, cx) = ssh_harness(cx, initial);
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
        cx.debug_bounds("modal-action-shared-close-confirm")
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
    let (harness, _, _, cx) = ssh_harness(cx, initial.clone());
    request(&harness, cx);
    let mut accepted = accepted_inventory(initial);
    accepted.workspaces[0].tabs[0].worktree = Some(EXTRA_ROOT.into());
    harness.edits.answer(Outcome::Accepted { target: [80; 32] });
    sync(&harness, accepted, cx);
    assert_eq!(harness.edits.submitted(), [expected_open(MAIN_ROOT)]);
    assert!(
        cx.debug_bounds("modal-action-shared-close-confirm")
            .is_none()
    );
}

#[test]
fn worktree_fallback_destination_prefers_main_and_excludes_bare_missing_and_target() {
    use crate::ui::workspace_manager::worktrees::fallback::destination;
    let mut listing = WorktreeInventory {
        main_root: MAIN_ROOT.into(),
        current: None,
        worktrees: vec![
            record(EXTRA_ROOT, "extra", false),
            record(MAIN_ROOT, "main", false),
            record(FEATURE_ROOT, "feature", false),
        ],
    };
    assert_eq!(
        destination(&listing, Path::new(FEATURE_ROOT)),
        Some(Path::new(MAIN_ROOT))
    );
    listing.worktrees[1].missing = true;
    assert_eq!(
        destination(&listing, Path::new(FEATURE_ROOT)),
        Some(Path::new(EXTRA_ROOT))
    );
    listing.worktrees[0].head = WorktreeHead::Bare;
    assert_eq!(destination(&listing, Path::new(FEATURE_ROOT)), None);
    listing.worktrees[1].missing = false;
    assert_eq!(
        destination(&listing, Path::new(MAIN_ROOT)),
        Some(Path::new(FEATURE_ROOT))
    );
}
