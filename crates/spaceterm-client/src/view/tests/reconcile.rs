//! Reconciliation of attached Workspaces against validated replicas.

use spaceterm_core::organization::{PaneCreation, SplitAxis, SplitEdge};

use super::{
    A, B, Org, attach, attached, chain, metrics, observe, offline, pane, ratios, ready,
    ready_client, reconcile, restored, retained_workspace, shape, single, space, synchronizing,
    tab, worktree, ws,
};
use crate::view::{
    ActivationCause, ArrangementEdit, AttachmentRemoval, ClientViewState, InvalidationReason,
    PersistedView, ProjectionInvalidation, ReplicaFacts, ReplicaReadiness, RestorationFilter,
    ServerViewStatus, SplitId, TabStep, ViewAvailability, ViewChange, ViewError, ViewResource,
    ZoomState,
};

fn deleted(byte: u8, workspace: u64, tab_local: u64, pane_local: u64) -> ProjectionInvalidation {
    ProjectionInvalidation {
        workspace: ws(byte, workspace),
        tab: tab(byte, tab_local),
        pane: pane(byte, pane_local),
        reason: InvalidationReason::TargetDeleted,
    }
}

fn layout_changed(state: &ClientViewState, byte: u8, workspace: u64, tab_local: u64) -> ViewChange {
    ViewChange::LayoutChanged {
        workspace: ws(byte, workspace),
        tab: tab(byte, tab_local),
        layout: state
            .workspace(ws(byte, workspace))
            .unwrap()
            .arrangement(tab(byte, tab_local))
            .unwrap()
            .layout_revision(),
    }
}

fn tab_order(state: &ClientViewState, byte: u8, workspace: u64) -> Vec<u64> {
    state
        .workspace(ws(byte, workspace))
        .unwrap()
        .tab_order()
        .iter()
        .map(|tab| tab.local())
        .collect()
}

fn split_id(
    state: &ClientViewState,
    byte: u8,
    workspace: u64,
    tab_local: u64,
    index: usize,
) -> SplitId {
    let view = state.workspace(ws(byte, workspace)).unwrap();
    SplitId::new(ratios(view, tab(byte, tab_local))[index].0).unwrap()
}

#[test]
fn remote_tab_creation_appends_without_activation() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100).add_tab(1, 11, 110, None);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    let snapshot = org.snapshot();
    state.move_tab(ws(A, 1), tab(A, 11), 0, &snapshot).unwrap();
    assert_eq!(tab_order(&state, A, 1), vec![11, 10]);

    // Another client creates two Tabs in one publication.
    org.add_tab(1, 12, 120, None)
        .add_tab(1, 13, 130, None)
        .publish();
    let update = reconcile(&mut state, &org, &[], ready(A)).unwrap();
    assert_eq!(
        update.changes,
        vec![
            ViewChange::TabAdded {
                workspace: ws(A, 1),
                tab: tab(A, 12),
            },
            ViewChange::TabAdded {
                workspace: ws(A, 1),
                tab: tab(A, 13),
            },
        ]
    );
    assert!(update.invalidations.is_empty());
    let view = state.workspace(ws(A, 1)).unwrap();
    assert_eq!(tab_order(&state, A, 1), vec![11, 10, 12, 13]);
    assert_eq!(view.active_tab(), Some(tab(A, 10)));
    assert_eq!(view.activation_history(), &[tab(A, 10)]);
    assert_eq!(
        view.arrangement(tab(A, 12)).unwrap().focused_pane(),
        Some(pane(A, 120))
    );
    assert_eq!(state.active_workspace(), Some(ws(A, 1)));
}

#[test]
fn inactive_tabs_and_workspaces_reconcile_membership() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .add_tab(1, 11, 110, None)
        .split(11, 111, 110, SplitAxis::Horizontal, SplitEdge::After)
        .split(11, 112, 110, SplitAxis::Vertical, SplitEdge::After)
        .add_tab(1, 12, 120, None)
        .add_workspace(2, 20, 200);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    attach(&mut state, &org, ready(A), 2).unwrap();
    assert_eq!(
        shape(state.workspace(ws(A, 1)).unwrap(), tab(A, 11)),
        "H(V(110,112),111)"
    );

    // Inactive Tabs 11 and 12 of inactive Workspace 1 change.
    org.delete_pane(111).delete_tab(12).publish().split(
        11,
        113,
        110,
        SplitAxis::Horizontal,
        SplitEdge::After,
    );
    let accepted = [org.accepted(113, 110, SplitAxis::Horizontal, SplitEdge::After)];
    let update = reconcile(&mut state, &org, &accepted, ready(A)).unwrap();
    assert_eq!(
        update.changes,
        vec![
            ViewChange::TabRemoved {
                workspace: ws(A, 1),
                tab: tab(A, 12),
            },
            ViewChange::PanesRemoved {
                workspace: ws(A, 1),
                tab: tab(A, 11),
                panes: vec![pane(A, 111)],
            },
            ViewChange::PanesAdded {
                workspace: ws(A, 1),
                tab: tab(A, 11),
                panes: vec![pane(A, 113)],
            },
            layout_changed(&state, A, 1, 11),
        ]
    );
    assert_eq!(
        update.invalidations,
        vec![deleted(A, 1, 12, 120), deleted(A, 1, 11, 111)]
    );
    let view = state.workspace(ws(A, 1)).unwrap();
    assert_eq!(shape(view, tab(A, 11)), "V(H(110,113),112)");
    assert_eq!(tab_order(&state, A, 1), vec![10, 11]);
    assert_eq!(view.active_tab(), Some(tab(A, 10)));
    assert_eq!(
        view.arrangement(tab(A, 11)).unwrap().focused_pane(),
        Some(pane(A, 110))
    );
    assert_eq!(state.active_workspace(), Some(ws(A, 2)));
}

#[test]
fn resync_preserves_surviving_personal_order_ratios_focus_and_zoom() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .split(10, 101, 100, SplitAxis::Horizontal, SplitEdge::After)
        .split(10, 102, 101, SplitAxis::Horizontal, SplitEdge::After)
        .add_tab(1, 11, 110, None)
        .add_tab(1, 12, 120, None);
    let snapshot = org.snapshot();
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    assert_eq!(
        shape(state.workspace(ws(A, 1)).unwrap(), tab(A, 10)),
        "H(100,H(101,102))"
    );

    // Personal order, ratios, focus and zoom.
    let workspace = ws(A, 1);
    state.move_tab(workspace, tab(A, 12), 0, &snapshot).unwrap();
    let outer = split_id(&state, A, 1, 10, 0);
    let inner = split_id(&state, A, 1, 10, 1);
    for (split, ratio) in [(outer, 0.3), (inner, 0.7)] {
        state
            .arrange(
                workspace,
                tab(A, 10),
                ArrangementEdit::Resize {
                    split,
                    available: space(),
                    metrics: metrics(),
                    ratio,
                },
            )
            .unwrap();
    }
    state
        .arrange(workspace, tab(A, 10), ArrangementEdit::Focus(pane(A, 101)))
        .unwrap();
    state
        .arrange(workspace, tab(A, 10), ArrangementEdit::ToggleZoom)
        .unwrap();
    let personal = ratios(state.workspace(workspace).unwrap(), tab(A, 10));
    assert_eq!(personal[0], (outer.get(), 0.3));

    // After a revision gap, the resynchronized snapshot has lost Pane 102,
    // gained Pane 103 and Tab 13, and carries no accepted placements.
    org.delete_pane(102)
        .split(10, 103, 100, SplitAxis::Vertical, SplitEdge::After)
        .add_tab(1, 13, 130, None)
        .publish()
        .publish();
    let update = reconcile(&mut state, &org, &[], ready(A)).unwrap();
    assert_eq!(
        update.changes,
        vec![
            ViewChange::PanesRemoved {
                workspace,
                tab: tab(A, 10),
                panes: vec![pane(A, 102)],
            },
            ViewChange::PanesAdded {
                workspace,
                tab: tab(A, 10),
                panes: vec![pane(A, 103)],
            },
            layout_changed(&state, A, 1, 10),
            ViewChange::TabAdded {
                workspace,
                tab: tab(A, 13),
            },
        ]
    );
    assert_eq!(update.invalidations, vec![deleted(A, 1, 10, 102)]);

    let view = state.workspace(workspace).unwrap();
    assert_eq!(tab_order(&state, A, 1), vec![12, 10, 11, 13]);
    assert_eq!(view.active_tab(), Some(tab(A, 10)));
    assert_eq!(shape(view, tab(A, 10)), "H(V(100,103),101)");
    let arrangement = view.arrangement(tab(A, 10)).unwrap();
    assert_eq!(arrangement.focused_pane(), Some(pane(A, 101)));
    assert_eq!(arrangement.zoom(), ZoomState::Zoomed(pane(A, 101)));
    // The surviving Split keeps its identity and ratio; only the new Split
    // gets a fresh identity and the default ratio.
    assert_eq!(ratios(view, tab(A, 10)), vec![(outer.get(), 0.3), (3, 0.5)]);
}

#[test]
fn two_independent_client_views_reconcile_without_copying_personal_state() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .split(10, 101, 100, SplitAxis::Horizontal, SplitEdge::After)
        .add_tab(1, 11, 110, None);
    let snapshot = org.snapshot();
    let first_facts = ready_client(A, 1, 1);
    let second_facts = ready_client(A, 2, 2);
    let mut first = ClientViewState::new();
    let mut second = ClientViewState::new();
    first
        .attach(ws(A, 1), &observe(&snapshot, &[], first_facts))
        .unwrap();
    second
        .attach(ws(A, 1), &observe(&snapshot, &[], second_facts))
        .unwrap();

    // Only the first client rearranges.
    let workspace = ws(A, 1);
    first.move_tab(workspace, tab(A, 11), 0, &snapshot).unwrap();
    let split = split_id(&first, A, 1, 10, 0);
    first
        .arrange(
            workspace,
            tab(A, 10),
            ArrangementEdit::Resize {
                split,
                available: space(),
                metrics: metrics(),
                ratio: 0.25,
            },
        )
        .unwrap();
    first
        .arrange(workspace, tab(A, 10), ArrangementEdit::Focus(pane(A, 101)))
        .unwrap();
    first
        .arrange(workspace, tab(A, 10), ArrangementEdit::ToggleZoom)
        .unwrap();

    // A shared creation and deletion reach both clients.
    org.add_tab(1, 12, 120, None).publish().split(
        10,
        102,
        100,
        SplitAxis::Vertical,
        SplitEdge::After,
    );
    let accepted = [org.accepted(102, 100, SplitAxis::Vertical, SplitEdge::After)];
    let mut intermediate = org.clone();
    intermediate.delete_tab(11).publish();
    reconcile(&mut first, &org, &accepted, first_facts).unwrap();
    reconcile(&mut second, &org, &accepted, second_facts).unwrap();
    reconcile(&mut first, &intermediate, &[], first_facts).unwrap();
    reconcile(&mut second, &intermediate, &[], second_facts).unwrap();

    let personal = first.workspace(workspace).unwrap();
    let default = second.workspace(workspace).unwrap();
    assert_eq!(shape(personal, tab(A, 10)), "H(V(100,102),101)");
    assert_eq!(shape(default, tab(A, 10)), "H(V(100,102),101)");
    assert_eq!(tab_order(&first, A, 1), vec![10, 12]);
    assert_eq!(tab_order(&second, A, 1), vec![10, 12]);
    assert_eq!(ratios(personal, tab(A, 10))[0].1, 0.25);
    assert_eq!(ratios(default, tab(A, 10))[0].1, 0.5);
    let personal_tab = personal.arrangement(tab(A, 10)).unwrap();
    let default_tab = default.arrangement(tab(A, 10)).unwrap();
    assert_eq!(personal_tab.focused_pane(), Some(pane(A, 101)));
    assert_eq!(personal_tab.zoom(), ZoomState::Zoomed(pane(A, 101)));
    assert_eq!(default_tab.focused_pane(), Some(pane(A, 100)));
    assert_eq!(default_tab.zoom(), ZoomState::Restored);

    // The second client's view equals a fresh attach of the same organization.
    let mut fresh = ClientViewState::new();
    let final_snapshot = intermediate.snapshot();
    fresh
        .attach(workspace, &observe(&final_snapshot, &[], second_facts))
        .unwrap();
    assert_eq!(
        shape(fresh.workspace(workspace).unwrap(), tab(A, 10)),
        shape(default, tab(A, 10))
    );
    assert_ne!(first.attachments()[0], second.attachments()[0]);
}

#[test]
fn retained_or_incomplete_inventory_does_not_delete_attachments() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .split(10, 101, 100, SplitAxis::Horizontal, SplitEdge::After)
        .add_tab(1, 11, 110, None)
        .add_workspace(2, 20, 200);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    attach(&mut state, &org, ready(A), 2).unwrap();
    let before = state.clone();

    org.delete_workspace(2)
        .delete_tab(11)
        .delete_pane(101)
        .publish();
    for facts in [offline(A), synchronizing(A)] {
        let update = reconcile(&mut state, &org, &[], facts).unwrap();
        assert!(update.is_empty());
        assert_eq!(update.revision, before.revision());
        assert_eq!(state, before);
    }
    // A connected Wave 1 replica without retained creation facts is not Ready.
    let legacy = org.legacy_snapshot();
    let update = state
        .reconcile(&observe(&legacy, &[], synchronizing(A)))
        .unwrap();
    assert!(update.is_empty());
    assert_eq!(state, before);

    // Retained targets stay navigable offline, and nothing new attaches.
    state.activate_workspace(ws(A, 1)).unwrap();
    state.activate_tab(ws(A, 1), tab(A, 11)).unwrap();
    state
        .arrange(ws(A, 1), tab(A, 10), ArrangementEdit::Focus(pane(A, 101)))
        .unwrap();
    assert_eq!(
        attach(&mut state, &org, offline(A), 1).unwrap_err(),
        ViewError::NotReady
    );
    assert_eq!(attached(&state), vec![(A, 1), (A, 2)]);

    // Only a Ready observation applies the deletions.
    reconcile(&mut state, &org, &[], ready(A)).unwrap();
    assert_eq!(attached(&state), vec![(A, 1)]);
    assert_eq!(tab_order(&state, A, 1), vec![10]);
}

#[test]
fn repeated_revision_is_idempotent() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .publish()
        .split(10, 101, 100, SplitAxis::Horizontal, SplitEdge::Before)
        .add_tab(1, 11, 110, None);
    let mut state = ClientViewState::new();
    let mut base = Org::new(A);
    base.add_workspace(1, 10, 100);
    attach(&mut state, &base, ready(A), 1).unwrap();

    let accepted = [org.accepted(101, 100, SplitAxis::Horizontal, SplitEdge::Before)];
    let first = reconcile(&mut state, &org, &accepted, ready(A)).unwrap();
    assert!(!first.is_empty());
    let after = state.clone();
    assert_eq!(
        shape(state.workspace(ws(A, 1)).unwrap(), tab(A, 10)),
        "H(101,100)"
    );

    // The same publication, with or without its placements, changes nothing.
    for placements in [&accepted[..], &[][..]] {
        let update = reconcile(&mut state, &org, placements, ready(A)).unwrap();
        assert!(update.is_empty());
        assert_eq!(update.revision, after.revision());
        assert_eq!(state, after);
    }
}

#[test]
fn older_observation_cannot_restore_deleted_targets() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .add_tab(1, 11, 110, None)
        .add_workspace(2, 20, 200);
    let old = org.clone();
    let mut state = ClientViewState::new();
    attach(&mut state, &old, ready(A), 1).unwrap();
    attach(&mut state, &old, ready(A), 2).unwrap();

    org.delete_tab(11).delete_workspace(2).publish();
    reconcile(&mut state, &org, &[], ready(A)).unwrap();
    assert_eq!(attached(&state), vec![(A, 1)]);
    assert_eq!(tab_order(&state, A, 1), vec![10]);

    // The older publication can neither restore the Tab nor reattach.
    let before = state.clone();
    assert_eq!(
        reconcile(&mut state, &old, &[], ready(A)).unwrap_err(),
        ViewError::StaleObservation
    );
    assert_eq!(
        attach(&mut state, &old, ready(A), 2).unwrap_err(),
        ViewError::StaleObservation
    );
    assert_eq!(state, before);

    // A new Server epoch is newer even at the same revision; a later revision
    // of the old epoch is still older.
    let mut restarted = org.clone();
    restarted.restart();
    reconcile(&mut state, &restarted, &[], ready(A)).unwrap();
    let mut old_epoch = org.clone();
    old_epoch.add_tab(1, 12, 120, None).publish().publish();
    assert_eq!(
        reconcile(&mut state, &old_epoch, &[], ready(A)).unwrap_err(),
        ViewError::StaleObservation
    );
    assert_eq!(tab_order(&state, A, 1), vec![10]);
}

#[test]
fn incompatible_server_does_not_disable_compatible_server_view() {
    let mut first = Org::new(A);
    first.add_workspace(1, 10, 100);
    let mut second = Org::new(B);
    second
        .add_workspace(1, 10, 100)
        .add_tab(1, 11, 110, None)
        .add_workspace(2, 20, 200);
    let mut state = ClientViewState::new();
    attach(&mut state, &first, ready(A), 1).unwrap();
    attach(&mut state, &second, ready(B), 1).unwrap();

    // Server B becomes incompatible: its retained view stays navigable.
    let incompatible = ReplicaFacts {
        connection: None,
        readiness: ReplicaReadiness::Retained,
        status: ServerViewStatus::Incompatible,
        ..ready(B)
    };
    second.delete_workspace(1).publish();
    assert!(
        reconcile(&mut state, &second, &[], incompatible)
            .unwrap()
            .is_empty()
    );
    assert_eq!(attached(&state), vec![(A, 1), (B, 1)]);
    state.activate_tab(ws(B, 1), tab(B, 11)).unwrap();
    assert_eq!(
        attach(&mut state, &second, incompatible, 2).unwrap_err(),
        ViewError::NotReady
    );

    // Server A keeps reconciling and attaching.
    first
        .add_tab(1, 11, 110, None)
        .add_workspace(2, 20, 200)
        .publish();
    let update = reconcile(&mut state, &first, &[], ready(A)).unwrap();
    assert_eq!(
        update.changes,
        vec![ViewChange::TabAdded {
            workspace: ws(A, 1),
            tab: tab(A, 11),
        }]
    );
    attach(&mut state, &first, ready(A), 2).unwrap();
    assert_eq!(attached(&state), vec![(A, 1), (B, 1), (A, 2)]);
    state.activate_workspace(ws(B, 1)).unwrap();
    assert_eq!(state.active_workspace(), Some(ws(B, 1)));
}

#[test]
fn worktree_navigation_filters_personal_order_without_owning_membership() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .add_tab(1, 11, 110, Some(1))
        .add_tab(1, 12, 120, Some(2))
        .add_tab(1, 13, 130, Some(1));
    let snapshot = org.snapshot();
    let workspace = ws(A, 1);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    let view = state.workspace(workspace).unwrap();
    assert_eq!(
        view.tabs_in_worktree(&snapshot, Some(worktree(A, 1))),
        vec![tab(A, 11), tab(A, 13)]
    );
    assert_eq!(view.tabs_in_worktree(&snapshot, None), vec![tab(A, 10)]);
    assert_eq!(view.active_worktree(&snapshot), None);

    // Moving within Worktree 1 reuses only that Worktree's slots.
    let update = state.move_tab(workspace, tab(A, 13), 0, &snapshot).unwrap();
    assert_eq!(
        update.changes,
        vec![ViewChange::TabsReordered { workspace }]
    );
    assert_eq!(tab_order(&state, A, 1), vec![10, 13, 12, 11]);
    assert_eq!(
        state
            .step_tab(workspace, tab(A, 11), TabStep::Next, &snapshot)
            .unwrap()
            .changes,
        Vec::new()
    );

    // Worktree navigation picks the first Tab, then the most recent one.
    let update = state
        .activate_worktree(workspace, Some(worktree(A, 1)), &snapshot)
        .unwrap();
    assert_eq!(
        update.changes,
        vec![ViewChange::TabActivated {
            workspace,
            tab: Some(tab(A, 13)),
            cause: ActivationCause::Navigation,
        }]
    );
    state
        .activate_adjacent_tab(workspace, TabStep::Next, &snapshot)
        .unwrap();
    assert_eq!(
        state.workspace(workspace).unwrap().active_tab(),
        Some(tab(A, 11))
    );
    state.activate_worktree(workspace, None, &snapshot).unwrap();
    state
        .activate_worktree(workspace, Some(worktree(A, 1)), &snapshot)
        .unwrap();
    let view = state.workspace(workspace).unwrap();
    assert_eq!(view.active_tab(), Some(tab(A, 11)));
    assert_eq!(view.active_worktree(&snapshot), Some(worktree(A, 1)));

    // Membership comes from the replica: moving Tab 11 to Worktree 2 changes
    // the filtered sequences before any reconcile.
    org.set_worktree(11, Some(2)).publish();
    let moved = org.snapshot();
    let view = state.workspace(workspace).unwrap();
    assert_eq!(
        view.tabs_in_worktree(&moved, Some(worktree(A, 1))),
        vec![tab(A, 13)]
    );
    assert_eq!(
        view.tabs_in_worktree(&moved, Some(worktree(A, 2))),
        vec![tab(A, 12), tab(A, 11)]
    );
    assert_eq!(view.active_worktree(&moved), Some(worktree(A, 2)));

    // The retained view holds no Worktree membership.
    let decoded =
        PersistedView::decode(state.persisted().unwrap().encode().unwrap().as_bytes()).unwrap();
    let (relaunched, _) =
        ClientViewState::restore(Some(&decoded), &RestorationFilter::new()).unwrap();
    assert_eq!(
        relaunched
            .workspace(workspace)
            .unwrap()
            .tabs_in_worktree(&moved, Some(worktree(A, 2))),
        vec![tab(A, 12), tab(A, 11)]
    );

    // Another Server's snapshot or Worktree is rejected or empty.
    let mut other = Org::new(B);
    other.add_workspace(1, 10, 100);
    let foreign = other.snapshot();
    assert!(
        state
            .workspace(workspace)
            .unwrap()
            .tabs_in_worktree(&foreign, None)
            .is_empty()
    );
    assert_eq!(
        state
            .activate_worktree(workspace, Some(worktree(B, 1)), &moved)
            .unwrap_err(),
        ViewError::WrongServer
    );
    assert_eq!(
        state
            .move_tab(workspace, tab(A, 13), 0, &foreign)
            .unwrap_err(),
        ViewError::WrongServer
    );
}

#[test]
fn restored_view_keeps_the_old_worktree_fallback_from_a_retained_replica() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .add_tab(1, 11, 110, Some(1))
        .add_tab(1, 12, 120, Some(1));
    let workspace = ws(A, 1);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    state.activate_tab(workspace, tab(A, 11)).unwrap();
    assert_eq!(
        state.workspace(workspace).unwrap().activation_history(),
        &[tab(A, 10), tab(A, 11)]
    );

    // Relaunch. The retained document holds no Worktree membership; the
    // retained replica supplies it while offline, with no change or effect.
    let saved =
        PersistedView::decode(state.persisted().unwrap().encode().unwrap().as_bytes()).unwrap();
    let (mut relaunched, _) =
        ClientViewState::restore(Some(&saved), &RestorationFilter::new()).unwrap();
    let revision = relaunched.revision();
    let update = reconcile(&mut relaunched, &org, &[], offline(A)).unwrap();
    assert!(update.is_empty());
    assert_eq!(update.revision, revision);
    assert_eq!(relaunched.persisted().unwrap(), saved);

    // An older publication in which Tab 12 sat in another Worktree.
    let mut stale = org.clone();
    stale.set_worktree(12, Some(2));

    // While this client was away, another client closed the Active Tab. The
    // fallback stays in its old Worktree instead of following activation
    // history to Tab 10.
    org.delete_tab(11).publish();
    let update = reconcile(&mut relaunched, &org, &[], ready(A)).unwrap();
    assert_eq!(
        update.changes,
        vec![
            ViewChange::TabRemoved {
                workspace,
                tab: tab(A, 11),
            },
            ViewChange::TabActivated {
                workspace,
                tab: Some(tab(A, 12)),
                cause: ActivationCause::Restoration,
            },
        ]
    );

    // A retained replica older than the reconciled publication cannot
    // replace the Worktree facts the fallback uses.
    org.add_tab(1, 13, 130, Some(1)).publish();
    reconcile(&mut relaunched, &org, &[], ready(A)).unwrap();
    assert_eq!(tab_order(&relaunched, A, 1), vec![10, 12, 13]);
    assert!(
        reconcile(&mut relaunched, &stale, &[], offline(A))
            .unwrap()
            .is_empty()
    );
    org.delete_tab(12).publish();
    reconcile(&mut relaunched, &org, &[], ready(A)).unwrap();
    assert_eq!(
        relaunched.workspace(workspace).unwrap().active_tab(),
        Some(tab(A, 13))
    );
}

#[test]
fn resource_limited_workspace_restores_limited_until_membership_fits() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 1).chain(10, 1, 2, 4095);
    let workspace = ws(A, 1);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    org.split(10, 4097, 4096, SplitAxis::Horizontal, SplitEdge::After)
        .publish();
    reconcile(&mut state, &org, &[], ready(A)).unwrap();
    let limited = ViewAvailability::ResourceLimited {
        resource: ViewResource::TabPanes,
    };
    assert_eq!(state.workspace(workspace).unwrap().availability(), limited);

    // After relaunch the pruned arrangement is still not presented as
    // complete, offline or before any Ready reconcile.
    let saved =
        PersistedView::decode(state.persisted().unwrap().encode().unwrap().as_bytes()).unwrap();
    let (mut relaunched, update) =
        ClientViewState::restore(Some(&saved), &RestorationFilter::new()).unwrap();
    assert!(update.is_empty());
    assert_eq!(
        relaunched.workspace(workspace).unwrap().availability(),
        limited
    );
    assert!(
        reconcile(&mut relaunched, &org, &[], offline(A))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        relaunched.workspace(workspace).unwrap().availability(),
        limited
    );

    // The same publication confirms the limit without repeating the
    // transition or its invalidations.
    assert!(
        reconcile(&mut relaunched, &org, &[], ready(A))
            .unwrap()
            .is_empty()
    );

    // Membership fits again: the Workspace becomes complete.
    org.delete_pane(4097).publish();
    let update = reconcile(&mut relaunched, &org, &[], ready(A)).unwrap();
    assert_eq!(
        update.changes,
        vec![ViewChange::AvailabilityChanged {
            workspace,
            availability: ViewAvailability::Available,
        }]
    );
    assert_eq!(
        relaunched.workspace(workspace).unwrap().availability(),
        ViewAvailability::Available
    );
}

#[test]
fn deleted_active_tab_and_focus_fall_back_as_restoration() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .split(10, 101, 100, SplitAxis::Horizontal, SplitEdge::After)
        .add_tab(1, 11, 110, None)
        .add_tab(1, 12, 120, None);
    let workspace = ws(A, 1);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    state.activate_tab(workspace, tab(A, 11)).unwrap();
    state
        .arrange(workspace, tab(A, 10), ArrangementEdit::Focus(pane(A, 101)))
        .unwrap();
    state
        .arrange(workspace, tab(A, 10), ArrangementEdit::ToggleZoom)
        .unwrap();

    org.delete_tab(11).delete_pane(101).publish();
    let update = reconcile(&mut state, &org, &[], ready(A)).unwrap();
    assert_eq!(
        update.changes,
        vec![
            ViewChange::TabRemoved {
                workspace,
                tab: tab(A, 11),
            },
            ViewChange::TabActivated {
                workspace,
                tab: Some(tab(A, 12)),
                cause: ActivationCause::Restoration,
            },
            ViewChange::PanesRemoved {
                workspace,
                tab: tab(A, 10),
                panes: vec![pane(A, 101)],
            },
            ViewChange::PaneFocused {
                workspace,
                tab: tab(A, 10),
                pane: pane(A, 100),
                cause: ActivationCause::Restoration,
            },
            ViewChange::ZoomChanged {
                workspace,
                tab: tab(A, 10),
                zoom: ZoomState::Restored,
            },
            layout_changed(&state, A, 1, 10),
        ]
    );
    assert_eq!(
        update.invalidations,
        vec![deleted(A, 1, 11, 110), deleted(A, 1, 10, 101)]
    );
}

#[test]
fn every_represented_tab_removed_restarts_from_defaults() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100);
    let workspace = ws(A, 1);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();

    // Another client replaces the only Tab with two new ones.
    org.add_tab(1, 11, 110, None)
        .add_tab(1, 12, 120, None)
        .set_root_tab(1, 12)
        .delete_tab(10)
        .publish();
    let update = reconcile(&mut state, &org, &[], ready(A)).unwrap();
    assert_eq!(
        update.changes,
        vec![
            ViewChange::TabRemoved {
                workspace,
                tab: tab(A, 10),
            },
            ViewChange::TabAdded {
                workspace,
                tab: tab(A, 11),
            },
            ViewChange::TabAdded {
                workspace,
                tab: tab(A, 12),
            },
            ViewChange::TabActivated {
                workspace,
                tab: Some(tab(A, 12)),
                cause: ActivationCause::Restoration,
            },
        ]
    );
    assert_eq!(update.invalidations, vec![deleted(A, 1, 10, 100)]);
    assert_eq!(tab_order(&state, A, 1), vec![11, 12]);
}

#[test]
fn resource_limit_marks_only_affected_workspace_and_recovers() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 50_000)
        .add_workspace(2, 20, 1)
        .chain(20, 1, 2, 4095)
        .add_tab(2, 21, 9_000, None);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    attach(&mut state, &org, ready(A), 2).unwrap();

    // Tab 20 grows past the per-Tab limit while Workspace 1 gains a Tab.
    // Pane 1 and Tab 21 are deleted in the same publication; Pane 2 becomes
    // Tab 20's Initial Root Pane.
    org.split(20, 4097, 4096, SplitAxis::Horizontal, SplitEdge::After)
        .split(20, 4098, 4097, SplitAxis::Horizontal, SplitEdge::After)
        .relink(2, PaneCreation::Initial)
        .set_root_pane(20, 2)
        .delete_pane(1)
        .delete_tab(21)
        .add_tab(1, 11, 50_001, None)
        .publish();
    let update = reconcile(&mut state, &org, &[], ready(A)).unwrap();
    let limited = ViewAvailability::ResourceLimited {
        resource: ViewResource::TabPanes,
    };
    let affected = state.workspace(ws(A, 2)).unwrap();
    assert_eq!(affected.availability(), limited);
    assert_eq!(affected.tab_order(), &[tab(A, 20)]);
    // The pruned last accepted arrangement survives without new Panes.
    let arrangement = affected.arrangement(tab(A, 20)).unwrap();
    assert_eq!(arrangement.pane_count(), 4095);
    assert!(!arrangement.contains_pane(pane(A, 1)));
    assert!(!arrangement.contains_pane(pane(A, 4097)));
    assert!(update.changes.contains(&ViewChange::AvailabilityChanged {
        workspace: ws(A, 2),
        availability: limited,
    }));
    assert!(update.changes.contains(&ViewChange::TabAdded {
        workspace: ws(A, 1),
        tab: tab(A, 11),
    }));
    // Removed targets are deleted and every remaining represented Pane loses
    // its content eligibility.
    let limited_panes = update
        .invalidations
        .iter()
        .filter(|invalidation| invalidation.reason == InvalidationReason::ResourceLimited)
        .count();
    assert_eq!(limited_panes, 4095);
    assert!(update.invalidations.contains(&deleted(A, 2, 20, 1)));
    assert!(update.invalidations.contains(&deleted(A, 2, 21, 9_000)));
    assert!(
        update
            .invalidations
            .iter()
            .all(|invalidation| invalidation.workspace == ws(A, 2))
    );
    let unaffected = state.workspace(ws(A, 1)).unwrap();
    assert_eq!(unaffected.availability(), ViewAvailability::Available);

    // Navigation and Detach elsewhere remain available.
    state.activate_workspace(ws(A, 1)).unwrap();
    state.activate_tab(ws(A, 1), tab(A, 11)).unwrap();

    // Repeating the same publication changes nothing.
    assert!(
        reconcile(&mut state, &org, &[], ready(A))
            .unwrap()
            .is_empty()
    );

    // Membership fits again: the Workspace becomes complete and represents
    // the Pane it could not hold while limited.
    org.delete_pane(4098).publish();
    let update = reconcile(&mut state, &org, &[], ready(A)).unwrap();
    assert_eq!(
        update.changes,
        vec![
            ViewChange::AvailabilityChanged {
                workspace: ws(A, 2),
                availability: ViewAvailability::Available,
            },
            ViewChange::PanesAdded {
                workspace: ws(A, 2),
                tab: tab(A, 20),
                panes: vec![pane(A, 4097)],
            },
            layout_changed(&state, A, 2, 20),
        ]
    );
    assert!(update.invalidations.is_empty());
    let recovered = state.workspace(ws(A, 2)).unwrap();
    assert_eq!(recovered.availability(), ViewAvailability::Available);
    assert_eq!(
        recovered.arrangement(tab(A, 20)).unwrap().pane_count(),
        4096
    );
}

#[test]
fn view_wide_limits_mark_the_growing_workspace_and_recover_after_detach() {
    // 8,191 retained Tabs of Server B leave room for one more Tab.
    let tabs: Vec<_> = (1..=8191).map(|local| (local, single(B, local))).collect();
    let mut state = restored(1, 1, vec![retained_workspace(B, 1, 1, tabs)]);
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100);
    attach(&mut state, &org, ready(A), 1).unwrap();

    org.add_tab(1, 11, 110, None).publish();
    let update = reconcile(&mut state, &org, &[], ready(A)).unwrap();
    let limited = ViewAvailability::ResourceLimited {
        resource: ViewResource::Tabs,
    };
    assert_eq!(state.workspace(ws(A, 1)).unwrap().availability(), limited);
    assert_eq!(tab_order(&state, A, 1), vec![10]);
    assert_eq!(
        update.invalidations,
        vec![ProjectionInvalidation {
            workspace: ws(A, 1),
            tab: tab(A, 10),
            pane: pane(A, 100),
            reason: InvalidationReason::ResourceLimited,
        }]
    );
    let removed = state.workspace(ws(B, 1)).unwrap().attachment();
    assert_eq!(
        state.workspace(ws(B, 1)).unwrap().availability(),
        ViewAvailability::Available
    );

    // Detaching the large Workspace makes room; the next reconcile recovers.
    let plan = state.prepare_detach(ws(B, 1)).unwrap();
    let update = state.apply_detach(plan).unwrap();
    assert_eq!(
        update.changes[0],
        ViewChange::AttachmentRemoved {
            attachment: removed,
            reason: AttachmentRemoval::Detached,
        }
    );
    let update = reconcile(&mut state, &org, &[], ready(A)).unwrap();
    assert_eq!(
        update.changes,
        vec![
            ViewChange::AvailabilityChanged {
                workspace: ws(A, 1),
                availability: ViewAvailability::Available,
            },
            ViewChange::TabAdded {
                workspace: ws(A, 1),
                tab: tab(A, 11),
            },
        ]
    );
    assert_eq!(tab_order(&state, A, 1), vec![10, 11]);

    // The Pane total behaves the same way.
    let tabs: Vec<_> = (1..=16)
        .map(|local| {
            let count = if local == 16 { 4095 } else { 4096 };
            (local, chain(B, local * 10_000, count))
        })
        .collect();
    let mut state = restored(1, 1, vec![retained_workspace(B, 1, 1, tabs)]);
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100);
    attach(&mut state, &org, ready(A), 1).unwrap();
    org.split(10, 101, 100, SplitAxis::Horizontal, SplitEdge::After)
        .publish();
    reconcile(&mut state, &org, &[], ready(A)).unwrap();
    assert_eq!(
        state.workspace(ws(A, 1)).unwrap().availability(),
        ViewAvailability::ResourceLimited {
            resource: ViewResource::Panes,
        }
    );
    assert_eq!(
        state
            .workspace(ws(A, 1))
            .unwrap()
            .arrangement(tab(A, 10))
            .unwrap()
            .pane_count(),
        1
    );
}

#[test]
fn personal_edits_report_navigation_and_keep_revision_when_unchanged() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .split(10, 101, 100, SplitAxis::Horizontal, SplitEdge::After);
    let workspace = ws(A, 1);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();

    let update = state
        .arrange(workspace, tab(A, 10), ArrangementEdit::Focus(pane(A, 101)))
        .unwrap();
    assert_eq!(
        update.changes,
        vec![ViewChange::PaneFocused {
            workspace,
            tab: tab(A, 10),
            pane: pane(A, 101),
            cause: ActivationCause::Navigation,
        }]
    );
    let revision = state.revision();
    let update = state
        .arrange(workspace, tab(A, 10), ArrangementEdit::Focus(pane(A, 101)))
        .unwrap();
    assert!(update.is_empty());
    assert_eq!(state.revision(), revision);

    // A failed edit changes nothing.
    let before = state.clone();
    assert_eq!(
        state
            .arrange(workspace, tab(A, 10), ArrangementEdit::Focus(pane(A, 999)))
            .unwrap_err(),
        ViewError::TargetMissing
    );
    assert_eq!(
        state
            .arrange(workspace, tab(B, 10), ArrangementEdit::ToggleZoom)
            .unwrap_err(),
        ViewError::WrongServer
    );
    assert_eq!(
        state
            .arrange(ws(A, 9), tab(A, 10), ArrangementEdit::ToggleZoom)
            .unwrap_err(),
        ViewError::TargetMissing
    );
    assert_eq!(state, before);

    let update = state
        .arrange(workspace, tab(A, 10), ArrangementEdit::ToggleZoom)
        .unwrap();
    // Zoom changes the presented layout as well.
    assert_eq!(
        update.changes,
        vec![
            ViewChange::ZoomChanged {
                workspace,
                tab: tab(A, 10),
                zoom: ZoomState::Zoomed(pane(A, 101)),
            },
            layout_changed(&state, A, 1, 10),
        ]
    );
    let split = split_id(&state, A, 1, 10, 0);
    let update = state
        .arrange(
            workspace,
            tab(A, 10),
            ArrangementEdit::Resize {
                split,
                available: space(),
                metrics: metrics(),
                ratio: 0.4,
            },
        )
        .unwrap();
    assert_eq!(update.changes, vec![layout_changed(&state, A, 1, 10)]);
}

#[test]
fn growth_cannot_push_a_later_unchanged_workspace_past_a_view_limit() {
    // 8,190 retained Tabs of Server B plus Workspaces 1 and 2 of Server A
    // fill the view exactly.
    let tabs: Vec<_> = (1..=8190).map(|local| (local, single(B, local))).collect();
    let mut state = restored(1, 1, vec![retained_workspace(B, 1, 1, tabs)]);
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100).add_workspace(2, 20, 200);
    attach(&mut state, &org, ready(A), 1).unwrap();
    attach(&mut state, &org, ready(A), 2).unwrap();

    // Only Workspace 1 grows. Workspace 2 reserves its unchanged size, so the
    // growing Workspace is the one marked Resource Limited.
    org.add_tab(1, 11, 110, None).publish();
    let update = reconcile(&mut state, &org, &[], ready(A)).unwrap();
    assert_eq!(
        state.workspace(ws(A, 1)).unwrap().availability(),
        ViewAvailability::ResourceLimited {
            resource: ViewResource::Tabs,
        }
    );
    assert_eq!(
        state.workspace(ws(A, 2)).unwrap().availability(),
        ViewAvailability::Available
    );
    assert!(
        update
            .invalidations
            .iter()
            .all(|invalidation| invalidation.workspace == ws(A, 1))
    );
    let represented: usize = state
        .workspaces()
        .iter()
        .map(|view| view.tab_order().len())
        .sum();
    assert_eq!(represented, 8192);
}
