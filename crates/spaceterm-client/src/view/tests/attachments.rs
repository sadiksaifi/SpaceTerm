//! Workspace Attachments: attach, restore, detach, pairing removal, replica
//! observation validation and view-wide representation bounds.

use spaceterm_core::authority::{ConnectionBinding, ConnectionGeneration};
use spaceterm_core::organization::{SplitAxis, SplitEdge};

use super::{
    A, B, Org, attach, attached, chain, client, observe, offline, pairing, ready, ready_as,
    reconcile, restored, retained_workspace, server, single, synchronizing, tab, ws,
};
use crate::view::{
    ActivationCause, AttachmentRemoval, ClientViewState, InvalidationReason, PaneEdge,
    PanePlacement, PersistedView, PersistedViewParts, ProjectionInvalidation, ReplicaFacts,
    ReplicaObservation, ReplicaReadiness, RestorationFilter, ServerViewStatus, ViewChange,
    ViewCounter, ViewError, ViewResource, ViewRevision,
};

fn invalidation(
    byte: u8,
    workspace: u64,
    tab_local: u64,
    pane_local: u64,
    reason: InvalidationReason,
) -> ProjectionInvalidation {
    ProjectionInvalidation {
        workspace: ws(byte, workspace),
        tab: tab(byte, tab_local),
        pane: super::pane(byte, pane_local),
        reason,
    }
}

/// Server A with Workspaces 1, 2 and 3, each with one Tab and one Pane.
fn three_workspaces() -> Org {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .add_workspace(2, 20, 200)
        .add_workspace(3, 30, 300);
    org
}

#[test]
fn new_view_is_empty_and_creates_no_workspace() {
    let mut state = ClientViewState::new();
    assert!(state.attachments().is_empty());
    assert!(state.workspaces().is_empty());
    assert_eq!(state.active_workspace(), None);
    assert_eq!(state.revision(), ViewRevision::INITIAL);

    // A Ready replica with Workspaces attaches none of them.
    let org = three_workspaces();
    let update = reconcile(&mut state, &org, &[], ready(A)).unwrap();
    assert!(update.is_empty());
    assert_eq!(update.revision, ViewRevision::INITIAL);
    assert!(state.attachments().is_empty());
    assert_eq!(state.active_workspace(), None);

    // The empty view persists as an empty document and restores as empty.
    let persisted = state.persisted().unwrap();
    assert!(persisted.parts().workspaces.is_empty());
    assert_eq!(persisted.parts().active, None);
    let decoded = PersistedView::decode(persisted.encode().unwrap().as_bytes()).unwrap();
    let (restored, update) =
        ClientViewState::restore(Some(&decoded), &RestorationFilter::new()).unwrap();
    assert!(update.is_empty());
    assert!(restored.attachments().is_empty());
    assert_eq!(restored.active_workspace(), None);
}

#[test]
fn launch_restores_only_saved_attachments() {
    let org = three_workspaces();
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    attach(&mut state, &org, ready(A), 3).unwrap();
    let saved = state.persisted().unwrap().encode().unwrap();

    // Restoration needs no connectivity and adds nothing.
    let decoded = PersistedView::decode(saved.as_bytes()).unwrap();
    let (mut launched, update) =
        ClientViewState::restore(Some(&decoded), &RestorationFilter::new()).unwrap();
    assert!(update.is_empty());
    assert_eq!(launched.attachments(), state.attachments());
    assert_eq!(attached(&launched), vec![(A, 1), (A, 3)]);
    assert_eq!(launched.active_workspace(), Some(ws(A, 3)));
    assert!(launched.workspace(ws(A, 2)).is_none());
    assert_eq!(launched.revision(), state.revision());

    // An offline replica changes nothing; a Ready one attaches nothing new.
    let offline_update = reconcile(&mut launched, &org, &[], offline(A)).unwrap();
    assert!(offline_update.is_empty());
    let ready_update = reconcile(&mut launched, &org, &[], ready(A)).unwrap();
    assert!(ready_update.is_empty());
    assert_eq!(attached(&launched), vec![(A, 1), (A, 3)]);
    assert_eq!(launched.persisted().unwrap(), state.persisted().unwrap());
}

#[test]
fn remote_workspace_creation_does_not_attach_it() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    let before = state.clone();

    // Another client creates Workspace 2.
    org.add_workspace(2, 20, 200).publish();
    let update = reconcile(&mut state, &org, &[], ready(A)).unwrap();
    assert!(update.is_empty());
    assert_eq!(update.revision, before.revision());
    assert_eq!(attached(&state), vec![(A, 1)]);
    assert_eq!(state.active_workspace(), Some(ws(A, 1)));
    assert_eq!(state.persisted().unwrap(), before.persisted().unwrap());
}

#[test]
fn killed_workspace_removes_saved_attachment_without_recreation() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .add_workspace(2, 20, 200)
        .split(20, 201, 200, SplitAxis::Horizontal, SplitEdge::After);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    attach(&mut state, &org, ready(A), 2).unwrap();
    let saved = state.persisted().unwrap();
    let killed = state.workspace(ws(A, 2)).unwrap().attachment();

    org.delete_workspace(2).publish();
    let update = reconcile(&mut state, &org, &[], ready(A)).unwrap();
    assert_eq!(
        update.changes,
        vec![
            ViewChange::AttachmentRemoved {
                attachment: killed,
                reason: AttachmentRemoval::WorkspaceDeleted,
            },
            ViewChange::WorkspaceActivated {
                workspace: Some(ws(A, 1)),
                cause: ActivationCause::Restoration,
            },
        ]
    );
    assert_eq!(
        update.invalidations,
        vec![
            invalidation(A, 2, 20, 200, InvalidationReason::TargetDeleted),
            invalidation(A, 2, 20, 201, InvalidationReason::TargetDeleted),
        ]
    );
    assert_eq!(attached(&state), vec![(A, 1)]);
    assert_eq!(state.persisted().unwrap().parts().workspaces.len(), 1);

    // Re-observing the same organization and a new Workspace recreates nothing.
    assert!(
        reconcile(&mut state, &org, &[], ready(A))
            .unwrap()
            .is_empty()
    );
    org.add_workspace(3, 30, 300).publish();
    assert!(
        reconcile(&mut state, &org, &[], ready(A))
            .unwrap()
            .is_empty()
    );
    assert_eq!(attached(&state), vec![(A, 1)]);

    // The saved document from before the kill loses the attachment too.
    let (mut relaunched, _) =
        ClientViewState::restore(Some(&saved), &RestorationFilter::new()).unwrap();
    let update = reconcile(&mut relaunched, &org, &[], ready(A)).unwrap();
    assert!(update.changes.contains(&ViewChange::AttachmentRemoved {
        attachment: killed,
        reason: AttachmentRemoval::WorkspaceDeleted,
    }));
    assert_eq!(attached(&relaunched), vec![(A, 1)]);
}

#[test]
fn removed_active_workspace_falls_back_to_next_then_preceding() {
    let mut org = three_workspaces();
    let mut state = ClientViewState::new();
    for workspace in [1, 2, 3] {
        attach(&mut state, &org, ready(A), workspace).unwrap();
    }
    state.activate_workspace(ws(A, 2)).unwrap();

    // The next surviving attachment at the old position comes first.
    org.delete_workspace(2).publish();
    let update = reconcile(&mut state, &org, &[], ready(A)).unwrap();
    assert_eq!(
        update.changes.last(),
        Some(&ViewChange::WorkspaceActivated {
            workspace: Some(ws(A, 3)),
            cause: ActivationCause::Restoration,
        })
    );

    // Without a next attachment, the preceding one follows.
    let plan = state.prepare_detach(ws(A, 3)).unwrap();
    let update = state.apply_detach(plan).unwrap();
    assert_eq!(
        update.changes.last(),
        Some(&ViewChange::WorkspaceActivated {
            workspace: Some(ws(A, 1)),
            cause: ActivationCause::Restoration,
        })
    );

    // Removing an inactive attachment keeps the Active Workspace.
    attach(&mut state, &org, ready(A), 3).unwrap();
    state.activate_workspace(ws(A, 1)).unwrap();
    let plan = state.prepare_detach(ws(A, 3)).unwrap();
    let update = state.apply_detach(plan).unwrap();
    assert!(
        !update
            .changes
            .iter()
            .any(|change| matches!(change, ViewChange::WorkspaceActivated { .. }))
    );
    assert_eq!(state.active_workspace(), Some(ws(A, 1)));
}

#[test]
fn final_detach_leaves_empty_view_without_shared_effects() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .split(10, 101, 100, SplitAxis::Vertical, SplitEdge::After);
    let snapshot = org.snapshot();
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    let attachment = state.attachments()[0];

    // Preparing changes nothing.
    let before = state.clone();
    let plan = state.prepare_detach(ws(A, 1)).unwrap();
    assert_eq!(state, before);
    assert_eq!(plan.attachment(), attachment);
    assert_eq!(plan.revision(), state.revision());

    let update = state.apply_detach(plan).unwrap();
    assert_eq!(
        update.changes,
        vec![
            ViewChange::AttachmentRemoved {
                attachment,
                reason: AttachmentRemoval::Detached,
            },
            ViewChange::WorkspaceActivated {
                workspace: None,
                cause: ActivationCause::Restoration,
            },
        ]
    );
    assert_eq!(
        update.invalidations,
        vec![
            invalidation(A, 1, 10, 100, InvalidationReason::Detached),
            invalidation(A, 1, 10, 101, InvalidationReason::Detached),
        ]
    );
    assert!(state.attachments().is_empty());
    assert!(state.workspaces().is_empty());
    assert_eq!(state.active_workspace(), None);
    assert!(state.persisted().unwrap().parts().workspaces.is_empty());

    // The shared organization is untouched and is not reattached.
    assert_eq!(org.snapshot(), snapshot);
    assert!(
        reconcile(&mut state, &org, &[], ready(A))
            .unwrap()
            .is_empty()
    );
    assert!(state.attachments().is_empty());
    assert_eq!(
        state.prepare_detach(ws(A, 1)).unwrap_err(),
        ViewError::TargetMissing
    );
}

#[test]
fn detach_plan_rejects_changed_revision_or_incarnation() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100).add_tab(1, 11, 110, None);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();

    // A plan from a revision this view never reached, as after restoring an
    // older retained document, changes nothing.
    let saved = state.persisted().unwrap();
    state.activate_tab(ws(A, 1), tab(A, 11)).unwrap();
    let ahead = state.prepare_detach(ws(A, 1)).unwrap();
    let (mut older, _) = ClientViewState::restore(Some(&saved), &RestorationFilter::new()).unwrap();
    assert!(older.revision() < ahead.revision());
    assert_eq!(older.attachments(), &[ahead.attachment()]);
    let before = older.clone();
    assert_eq!(older.apply_detach(ahead).unwrap_err(), ViewError::StaleView);
    assert_eq!(older, before);

    // A plan for the old incarnation cannot remove a later reattachment. Its
    // revision is older than the view's, so only the incarnation rejects it.
    let old = state.prepare_detach(ws(A, 1)).unwrap();
    state.apply_detach(old).unwrap();
    attach(&mut state, &org, ready(A), 1).unwrap();
    let new = state.attachments()[0];
    assert_ne!(new.incarnation(), old.attachment().incarnation());
    assert!(new.incarnation() > old.attachment().incarnation());
    assert!(old.revision() < state.revision());
    let before = state.clone();
    assert_eq!(state.apply_detach(old).unwrap_err(), ViewError::StaleView);
    assert_eq!(state, before);
    assert_eq!(state.attachments(), &[new]);
}

#[test]
fn detach_plan_survives_unrelated_changes_while_its_marker_is_written() {
    let mut first = Org::new(A);
    first
        .add_workspace(1, 10, 100)
        .add_tab(1, 11, 110, None)
        .add_workspace(2, 20, 200);
    let mut second = Org::new(B);
    second.add_workspace(1, 10, 100);
    let mut state = ClientViewState::new();
    attach(&mut state, &first, ready(A), 1).unwrap();
    attach(&mut state, &first, ready(A), 2).unwrap();
    attach(&mut state, &second, ready(B), 1).unwrap();
    let plan = state.prepare_detach(ws(A, 1)).unwrap();

    // While client-core makes the marker durable, another Server reconciles,
    // another Workspace activates and the detached Workspace is navigated.
    second.add_tab(1, 11, 110, None).publish();
    assert!(
        !reconcile(&mut state, &second, &[], ready(B))
            .unwrap()
            .is_empty()
    );
    state.activate_workspace(ws(A, 2)).unwrap();
    state.activate_tab(ws(A, 1), tab(A, 11)).unwrap();
    assert!(state.revision() > plan.revision());

    // The exact attachment is still present, so the durable marker and the
    // view agree: the detach applies.
    let update = state.apply_detach(plan).unwrap();
    assert_eq!(
        update.changes,
        vec![ViewChange::AttachmentRemoved {
            attachment: plan.attachment(),
            reason: AttachmentRemoval::Detached,
        }]
    );
    assert_eq!(
        update.invalidations,
        vec![
            invalidation(A, 1, 10, 100, InvalidationReason::Detached),
            invalidation(A, 1, 11, 110, InvalidationReason::Detached),
        ]
    );
    assert_eq!(attached(&state), vec![(A, 2), (B, 1)]);
    assert_eq!(state.active_workspace(), Some(ws(A, 2)));

    // Settling the same plan again removes nothing.
    let before = state.clone();
    assert_eq!(state.apply_detach(plan).unwrap_err(), ViewError::StaleView);
    assert_eq!(state, before);
}

#[test]
fn old_pairing_removal_does_not_remove_new_pairing_attachments() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100).add_workspace(2, 20, 200);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready_as(A, 1), 1).unwrap();
    let old = state.attachments()[0];

    // After re-pairing, a Workspace still attached under the old pairing must
    // lose that attachment first.
    attach(&mut state, &org, ready_as(A, 2), 2).unwrap();
    assert_eq!(
        attach(&mut state, &org, ready_as(A, 2), 1).unwrap_err(),
        ViewError::WrongMembership
    );

    let update = state.remove_pairing(server(A), pairing(A, 1)).unwrap();
    assert_eq!(
        update.changes,
        vec![ViewChange::AttachmentRemoved {
            attachment: old,
            reason: AttachmentRemoval::PairingRemoved,
        }]
    );
    assert_eq!(
        update.invalidations,
        vec![invalidation(
            A,
            1,
            10,
            100,
            InvalidationReason::PairingRemoved
        )]
    );
    assert_eq!(attached(&state), vec![(A, 2)]);
    assert_eq!(state.active_workspace(), Some(ws(A, 2)));

    // The new pairing attaches Workspace 1 with a new incarnation.
    attach(&mut state, &org, ready_as(A, 2), 1).unwrap();
    assert!(state.attachments()[1].incarnation() > old.incarnation());

    // Settling the old removal again, or unpairing another device, keeps
    // every attachment of the new pairing.
    let before = state.clone();
    assert!(
        state
            .remove_pairing(server(A), pairing(A, 1))
            .unwrap()
            .is_empty()
    );
    assert!(
        state
            .remove_pairing(server(A), pairing(A, 9))
            .unwrap()
            .is_empty()
    );
    assert_eq!(state, before);
    assert_eq!(
        state.remove_pairing(server(B), pairing(A, 2)).unwrap_err(),
        ViewError::WrongServer
    );

    // An observation under the old pairing does not reconcile new attachments.
    org.delete_workspace(2).publish();
    assert!(
        reconcile(&mut state, &org, &[], ready_as(A, 1))
            .unwrap()
            .is_empty()
    );
    assert_eq!(attached(&state), vec![(A, 2), (A, 1)]);
    reconcile(&mut state, &org, &[], ready_as(A, 2)).unwrap();
    assert_eq!(attached(&state), vec![(A, 1)]);
}

#[test]
fn two_servers_with_equal_local_ids_remain_distinct() {
    let mut first = Org::new(A);
    first.add_workspace(1, 10, 100).add_tab(1, 11, 110, None);
    let mut second = Org::new(B);
    second.add_workspace(1, 10, 100).add_tab(1, 11, 110, None);
    let mut state = ClientViewState::new();
    attach(&mut state, &first, ready(A), 1).unwrap();
    attach(&mut state, &second, ready(B), 1).unwrap();
    assert_eq!(attached(&state), vec![(A, 1), (B, 1)]);
    assert_ne!(state.attachments()[0], state.attachments()[1]);

    // Personal changes on one Server leave the other untouched.
    state.activate_tab(ws(B, 1), tab(B, 11)).unwrap();
    assert_eq!(
        state.workspace(ws(A, 1)).unwrap().active_tab(),
        Some(tab(A, 10))
    );
    assert_eq!(
        state.workspace(ws(B, 1)).unwrap().active_tab(),
        Some(tab(B, 11))
    );
    assert_eq!(
        state.activate_tab(ws(A, 1), tab(B, 11)).unwrap_err(),
        ViewError::WrongServer
    );

    // The retained document keeps both Servers apart.
    let encoded = state.persisted().unwrap().encode().unwrap();
    let decoded = PersistedView::decode(encoded.as_bytes()).unwrap();
    let (restored_state, _) =
        ClientViewState::restore(Some(&decoded), &RestorationFilter::new()).unwrap();
    assert_eq!(restored_state.attachments(), state.attachments());
    assert_eq!(
        restored_state.workspace(ws(B, 1)).unwrap().active_tab(),
        Some(tab(B, 11))
    );

    // Deleting Workspace 1 on Server A leaves Server B's Workspace 1.
    first.delete_workspace(1).publish();
    reconcile(&mut state, &first, &[], ready(A)).unwrap();
    assert_eq!(attached(&state), vec![(B, 1)]);
    assert_eq!(state.active_workspace(), Some(ws(B, 1)));
}

#[test]
fn attach_requires_a_ready_replica_and_an_existing_workspace() {
    let org = three_workspaces();
    let mut state = ClientViewState::new();
    assert_eq!(
        attach(&mut state, &org, offline(A), 1).unwrap_err(),
        ViewError::NotReady
    );
    assert_eq!(
        attach(&mut state, &org, synchronizing(A), 1).unwrap_err(),
        ViewError::NotReady
    );
    assert_eq!(
        attach(&mut state, &org, ready(A), 9).unwrap_err(),
        ViewError::TargetMissing
    );
    let snapshot = org.snapshot();
    assert_eq!(
        state
            .attach(ws(B, 1), &observe(&snapshot, &[], ready(A)))
            .unwrap_err(),
        ViewError::WrongServer
    );
    assert_eq!(state, ClientViewState::new());

    let update = attach(&mut state, &org, ready(A), 2).unwrap();
    let attachment = state.attachments()[0];
    assert_eq!(attachment.workspace(), ws(A, 2));
    assert_eq!(attachment.pairing(), pairing(A, 1));
    assert_eq!(attachment.client(), client(A, 1));
    assert_eq!(attachment.incarnation().get(), 1);
    assert_eq!(
        update.changes,
        vec![
            ViewChange::Attached { attachment },
            ViewChange::WorkspaceActivated {
                workspace: Some(ws(A, 2)),
                cause: ActivationCause::Navigation,
            },
        ]
    );
    assert!(update.invalidations.is_empty());
    let view = state.workspace(ws(A, 2)).unwrap();
    assert_eq!(view.tab_order(), &[tab(A, 20)]);
    assert_eq!(view.active_tab(), Some(tab(A, 20)));
    assert_eq!(
        view.arrangement(tab(A, 20)).unwrap().focused_pane(),
        Some(super::pane(A, 200))
    );
}

#[test]
fn attaching_an_attached_workspace_only_activates_it() {
    let org = three_workspaces();
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready(A), 1).unwrap();
    attach(&mut state, &org, ready(A), 2).unwrap();
    let attachments = state.attachments().to_vec();

    let update = attach(&mut state, &org, ready(A), 1).unwrap();
    assert_eq!(
        update.changes,
        vec![ViewChange::WorkspaceActivated {
            workspace: Some(ws(A, 1)),
            cause: ActivationCause::Navigation,
        }]
    );
    assert_eq!(state.attachments(), attachments.as_slice());

    let revision = state.revision();
    let update = attach(&mut state, &org, ready(A), 1).unwrap();
    assert!(update.is_empty());
    assert_eq!(update.revision, revision);

    // Another stable Client of the same Server is another membership scope.
    let other_client = ReplicaFacts {
        client: client(A, 2),
        connection: Some(ConnectionBinding::new(
            client(A, 2),
            ConnectionGeneration::new(1),
        )),
        ..ready(A)
    };
    assert_eq!(
        attach(&mut state, &org, other_client, 1).unwrap_err(),
        ViewError::WrongMembership
    );
    assert_eq!(state.attachments(), attachments.as_slice());
}

#[test]
fn replica_observation_rejects_mismatched_bindings_and_stale_placements() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .add_tab(1, 11, 110, None)
        .publish()
        .split(10, 101, 100, SplitAxis::Vertical, SplitEdge::Before);
    let snapshot = org.snapshot();
    let accepted = [org.accepted(101, 100, SplitAxis::Vertical, SplitEdge::Before)];
    let error =
        |facts: ReplicaFacts| ReplicaObservation::new(&snapshot, &accepted, facts).unwrap_err();

    assert_eq!(
        error(ReplicaFacts {
            pairing: pairing(B, 1),
            ..ready(A)
        }),
        ViewError::WrongServer
    );
    assert_eq!(
        error(ReplicaFacts {
            client: client(B, 1),
            ..ready(A)
        }),
        ViewError::WrongServer
    );
    assert_eq!(
        error(ReplicaFacts {
            connection: Some(ConnectionBinding::new(
                client(B, 1),
                ConnectionGeneration::new(1)
            )),
            ..ready(A)
        }),
        ViewError::WrongServer
    );
    assert_eq!(
        error(ReplicaFacts {
            connection: Some(ConnectionBinding::new(
                client(A, 2),
                ConnectionGeneration::new(1)
            )),
            ..ready(A)
        }),
        ViewError::WrongMembership
    );
    // A connected socket alone is not Ready, and Ready needs a connection.
    assert_eq!(
        error(ReplicaFacts {
            connection: None,
            ..ready(A)
        }),
        ViewError::NotReady
    );
    assert_eq!(
        error(ReplicaFacts {
            status: ServerViewStatus::Offline,
            ..ready(A)
        }),
        ViewError::NotReady
    );

    // Ready requires the retained creation defaults; Retained does not.
    let legacy = org.legacy_snapshot();
    assert_eq!(
        ReplicaObservation::new(&legacy, &[], ready(A)).unwrap_err(),
        ViewError::InvalidCreationTopology
    );
    let retained = ReplicaObservation::new(&legacy, &[], offline(A)).unwrap();
    assert_eq!(retained.readiness(), ReplicaReadiness::Retained);
    assert_eq!(
        retained.creation_layout(tab(A, 10)).unwrap_err(),
        ViewError::InvalidCreationTopology
    );

    // Accepted placements bind the exact publication of the snapshot.
    let mut later = org.clone();
    later.publish();
    let later_snapshot = later.snapshot();
    assert_eq!(
        ReplicaObservation::new(&later_snapshot, &accepted, ready(A)).unwrap_err(),
        ViewError::StaleObservation
    );
    let mut restarted = org.clone();
    restarted.restart();
    let restarted_snapshot = restarted.snapshot();
    assert_eq!(
        ReplicaObservation::new(&restarted_snapshot, &accepted, ready(A)).unwrap_err(),
        ViewError::StaleObservation
    );
    let duplicated = [accepted[0], accepted[0]];
    assert_eq!(
        ReplicaObservation::new(&snapshot, &duplicated, ready(A)).unwrap_err(),
        ViewError::InvalidCreationTopology
    );
    let mut foreign = Org::new(B);
    foreign
        .add_workspace(1, 10, 100)
        .add_tab(1, 11, 110, None)
        .publish()
        .split(10, 101, 100, SplitAxis::Vertical, SplitEdge::Before);
    let foreign_accepted = [foreign.accepted(101, 100, SplitAxis::Vertical, SplitEdge::Before)];
    assert_eq!(
        ReplicaObservation::new(&snapshot, &foreign_accepted, ready(A)).unwrap_err(),
        ViewError::WrongServer
    );

    // A valid observation exposes its bindings and per-Tab placements.
    let observation = observe(&snapshot, &accepted, ready(A));
    assert_eq!(observation.server(), server(A));
    assert_eq!(observation.pairing(), pairing(A, 1));
    assert_eq!(observation.client(), client(A, 1));
    assert_eq!(observation.epoch(), snapshot.epoch());
    assert_eq!(observation.revision(), org.revision());
    assert_eq!(observation.status(), ServerViewStatus::Connected);
    assert_eq!(observation.accepted_creations().len(), 1);
    assert_eq!(
        observation.placements(tab(A, 10)),
        vec![PanePlacement {
            pane: super::pane(A, 101),
            source: super::pane(A, 100),
            edge: PaneEdge::from_split(SplitAxis::Vertical, SplitEdge::Before),
            ordinal: accepted[0].ordinal(),
        }]
    );
    assert!(observation.placements(tab(A, 11)).is_empty());
    assert_eq!(
        observation.creation_layout(tab(B, 10)).unwrap_err(),
        ViewError::WrongServer
    );
    assert_eq!(
        observation.creation_layout(tab(A, 99)).unwrap_err(),
        ViewError::TargetMissing
    );
    assert_eq!(
        observation.creation_layout(tab(A, 10)).unwrap().nodes.len(),
        3
    );
}

#[test]
fn attachment_count_admits_the_limit_and_rejects_one_more() {
    // 255 retained attachments of Server B leave room for exactly one more.
    let filler: Vec<_> = (1..=255)
        .map(|local| retained_workspace(B, local, local, vec![(local, single(B, local))]))
        .collect();
    let mut state = restored(1, 255, filler);
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100).add_workspace(2, 20, 200);

    attach(&mut state, &org, ready(A), 1).unwrap();
    assert_eq!(state.attachments().len(), 256);
    let before = state.clone();
    assert_eq!(
        attach(&mut state, &org, ready(A), 2).unwrap_err(),
        ViewError::ResourceLimit {
            resource: ViewResource::Attachments,
        }
    );
    assert_eq!(state, before);
}

#[test]
fn view_tab_total_admits_the_limit_and_rejects_one_more() {
    // 8,191 retained Tabs of Server B in two attachments.
    let first: Vec<_> = (1..=4096).map(|local| (local, single(B, local))).collect();
    let second: Vec<_> = (4097..=8191)
        .map(|local| (local, single(B, local)))
        .collect();
    let mut state = restored(
        1,
        2,
        vec![
            retained_workspace(B, 1, 1, first),
            retained_workspace(B, 2, 2, second),
        ],
    );
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100).add_workspace(2, 20, 200);

    attach(&mut state, &org, ready(A), 1).unwrap();
    let before = state.clone();
    assert_eq!(
        attach(&mut state, &org, ready(A), 2).unwrap_err(),
        ViewError::ResourceLimit {
            resource: ViewResource::Tabs,
        }
    );
    assert_eq!(state, before);
}

#[test]
fn view_pane_total_admits_the_limit_and_rejects_one_more() {
    // 65,535 retained Panes of Server B in sixteen Tabs.
    let tabs: Vec<_> = (1..=16)
        .map(|local| {
            let count = if local == 16 { 4095 } else { 4096 };
            (local, chain(B, local * 10_000, count))
        })
        .collect();
    let mut state = restored(1, 1, vec![retained_workspace(B, 1, 1, tabs)]);
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100).add_workspace(2, 20, 200);

    attach(&mut state, &org, ready(A), 1).unwrap();
    let before = state.clone();
    assert_eq!(
        attach(&mut state, &org, ready(A), 2).unwrap_err(),
        ViewError::ResourceLimit {
            resource: ViewResource::Panes,
        }
    );
    assert_eq!(state, before);
}

#[test]
fn tab_pane_limit_admits_the_limit_and_rejects_one_more_on_attach() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 1)
        .chain(10, 1, 2, 4095)
        .add_workspace(2, 20, 10_001)
        .chain(20, 10_001, 10_002, 4096);
    let mut state = ClientViewState::new();

    attach(&mut state, &org, ready(A), 1).unwrap();
    assert_eq!(
        state
            .workspace(ws(A, 1))
            .unwrap()
            .arrangement(tab(A, 10))
            .unwrap()
            .pane_count(),
        4096
    );
    let before = state.clone();
    assert_eq!(
        attach(&mut state, &org, ready(A), 2).unwrap_err(),
        ViewError::ResourceLimit {
            resource: ViewResource::TabPanes,
        }
    );
    assert_eq!(state, before);
}

#[test]
fn exhausted_view_counters_reject_changes_without_effects() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .add_tab(1, 11, 110, None)
        .add_workspace(2, 20, 200);

    // The view revision is checked before any effect.
    let retained = vec![retained_workspace(
        A,
        1,
        1,
        vec![(10, single(A, 100)), (11, single(A, 110))],
    )];
    let mut state = restored(u64::MAX, 1, retained);
    let before = state.clone();
    let exhausted = ViewError::IdentityExhausted {
        counter: ViewCounter::ViewRevision,
    };
    assert_eq!(
        state.activate_tab(ws(A, 1), tab(A, 11)).unwrap_err(),
        exhausted
    );
    assert_eq!(
        attach(&mut state, &org, ready(A), 2).unwrap_err(),
        exhausted
    );
    org.add_tab(1, 12, 120, None).publish();
    assert_eq!(
        reconcile(&mut state, &org, &[], ready(A)).unwrap_err(),
        exhausted
    );
    let plan = state.prepare_detach(ws(A, 1)).unwrap();
    assert_eq!(state.apply_detach(plan).unwrap_err(), exhausted);
    assert_eq!(state, before);

    // So is the attachment incarnation high-water mark.
    let empty = PersistedView::new(PersistedViewParts {
        revision: ViewRevision::new(1),
        incarnation_high_water: u64::MAX,
        workspaces: Vec::new(),
        active: None,
    })
    .unwrap();
    let (mut state, _) = ClientViewState::restore(Some(&empty), &RestorationFilter::new()).unwrap();
    let before = state.clone();
    assert_eq!(
        attach(&mut state, &org, ready(A), 2).unwrap_err(),
        ViewError::IdentityExhausted {
            counter: ViewCounter::AttachmentIncarnation,
        }
    );
    assert_eq!(state, before);
}

#[test]
fn restoration_filter_suppresses_exact_attachments_and_pairings() {
    let mut org = Org::new(A);
    org.add_workspace(1, 10, 100)
        .add_workspace(2, 20, 200)
        .add_workspace(3, 30, 300);
    let mut state = ClientViewState::new();
    attach(&mut state, &org, ready_as(A, 1), 1).unwrap();
    attach(&mut state, &org, ready_as(A, 1), 2).unwrap();
    attach(&mut state, &org, ready_as(A, 2), 3).unwrap();
    let saved = state.persisted().unwrap();
    let first = state.attachments()[0];
    let third = state.attachments()[2];

    let mut filter = RestorationFilter::new();
    filter.suppress_attachment(first);
    let (restored_state, update) = ClientViewState::restore(Some(&saved), &filter).unwrap();
    assert_eq!(attached(&restored_state), vec![(A, 2), (A, 3)]);
    assert_eq!(
        update.changes,
        vec![ViewChange::AttachmentRemoved {
            attachment: first,
            reason: AttachmentRemoval::Suppressed,
        }]
    );
    assert_eq!(
        update.invalidations,
        vec![invalidation(A, 1, 10, 100, InvalidationReason::Suppressed)]
    );

    // A pending self-unpair barrier suppresses every attachment of the pairing.
    let mut filter = RestorationFilter::new();
    filter.suppress_pairing(pairing(A, 1));
    let (restored_state, _) = ClientViewState::restore(Some(&saved), &filter).unwrap();
    assert_eq!(restored_state.attachments(), &[third]);
    assert_eq!(restored_state.active_workspace(), Some(ws(A, 3)));
}
