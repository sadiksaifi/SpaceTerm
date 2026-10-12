use spaceterm_client::{
    failure::ClientFailureClass,
    limits::ClientLimits,
    replica::{
        OrganizationCursor, OrganizationResidency, PublicationStamp, Replica, ReplicaState,
        ReplicaUpdate, SnapshotPart,
    },
};
use spaceterm_core::{
    authority::{FieldVersion, OrganizationRevision, ServerEpoch},
    identity::ServerId,
    organization::{
        OrganizationDelta, OrganizationSnapshot, OrganizationState, ServerSettingsRecord,
        Versioned, WorktreeLocationTemplate,
    },
    organization_retained::{OrganizationRetainedFacts, OrganizationRetainedParts},
    remote_access::RemoteAccessPolicy,
};
use std::sync::Arc;

fn snapshot(server: ServerId, epoch: u64, revision: u64) -> OrganizationSnapshot {
    let retained = OrganizationRetainedFacts::new(OrganizationRetainedParts {
        server,
        workspace_names: vec![],
        creation_defaults: vec![],
        pane_generations: vec![],
        tab_membership_versions: vec![],
        repository_inventory_versions: vec![],
        worktree_inventory: vec![],
        remote_access: Versioned {
            version: FieldVersion::new(1),
            value: RemoteAccessPolicy::disabled(server),
        },
    })
    .unwrap();
    OrganizationSnapshot::new(
        ServerEpoch::new(epoch),
        OrganizationState {
            server,
            revision: OrganizationRevision::new(revision),
            settings: ServerSettingsRecord {
                worktree_location: Versioned {
                    version: FieldVersion::new(1),
                    value: WorktreeLocationTemplate::new("{branch}".into()).unwrap(),
                },
            },
            workspaces: vec![],
            tabs: vec![],
            panes: vec![],
            repositories: vec![],
            worktrees: vec![],
            retained: Some(retained),
        },
        vec![],
    )
    .unwrap()
}

fn replica(limits: ClientLimits) -> Replica {
    Replica::new(
        ServerId::new([1; 16]),
        Arc::new(OrganizationResidency::new(limits).unwrap()),
    )
}

#[test]
fn revision_gap_requests_one_resync_and_preserves_last_valid_replica() {
    let server = ServerId::new([1; 16]);
    let mut replica = replica(ClientLimits::default());
    let token = replica.begin_resync(ServerEpoch::new(1)).unwrap();
    assert_eq!(
        replica
            .install_snapshot(token, snapshot(server, 1, 5))
            .unwrap(),
        ReplicaUpdate::Installed
    );
    let delta = OrganizationDelta::new(
        server,
        ServerEpoch::new(1),
        OrganizationRevision::new(6),
        OrganizationRevision::new(7),
        vec![],
        vec![],
    )
    .unwrap();
    assert_eq!(
        replica.apply_delta(&delta).unwrap(),
        ReplicaUpdate::ResyncRequired
    );
    let state = replica.subscribe();
    assert!(
        matches!(&*state.borrow(), ReplicaState::Stale { retained, .. } if retained.cursor.revision.get() == 5)
    );
}

#[test]
fn late_resync_snapshot_cannot_replace_newer_base() {
    let server = ServerId::new([1; 16]);
    let mut replica = replica(ClientLimits::default());
    let old = replica.begin_resync(ServerEpoch::new(1)).unwrap();
    let current = replica.begin_resync(ServerEpoch::new(1)).unwrap();
    replica
        .install_snapshot(current, snapshot(server, 1, 8))
        .unwrap();
    assert_eq!(
        replica
            .install_snapshot(old, snapshot(server, 1, 5))
            .unwrap(),
        ReplicaUpdate::Ignored
    );
    assert_eq!(
        replica
            .subscribe()
            .borrow()
            .retained()
            .unwrap()
            .cursor
            .revision
            .get(),
        8
    );
}

#[test]
fn snapshot_is_installed_atomically_after_all_chunks() {
    let server = ServerId::new([1; 16]);
    let mut replica = replica(ClientLimits::default());
    let token = replica.begin_resync(ServerEpoch::new(1)).unwrap();
    let part = |index| SnapshotPart {
        stamp: PublicationStamp {
            request: 1,
            request_generation: token.get(),
            transfer: 1,
            index,
            total: 2,
            cursor: OrganizationCursor {
                server,
                epoch: ServerEpoch::new(1),
                revision: OrganizationRevision::new(5),
            },
        },
        state: snapshot(server, 1, 5).state().clone(),
        runtime: vec![],
    };
    assert_eq!(
        replica.stage_snapshot(token, part(0)).unwrap(),
        ReplicaUpdate::Staged
    );
    assert!(replica.subscribe().borrow().retained().is_none());
    assert_eq!(
        replica.stage_snapshot(token, part(1)).unwrap(),
        ReplicaUpdate::Installed
    );
    assert_eq!(
        replica
            .subscribe()
            .borrow()
            .retained()
            .unwrap()
            .cursor
            .revision
            .get(),
        5
    );
}

#[test]
fn wrong_server_snapshot_cannot_replace_a_valid_replica() {
    let server = ServerId::new([1; 16]);
    let mut replica = replica(ClientLimits::default());
    let token = replica.begin_resync(ServerEpoch::new(1)).unwrap();
    replica
        .install_snapshot(token, snapshot(server, 1, 5))
        .unwrap();
    let token = replica.begin_resync(ServerEpoch::new(1)).unwrap();
    assert_eq!(
        replica
            .install_snapshot(token, snapshot(ServerId::new([2; 16]), 1, 6))
            .unwrap_err()
            .class(),
        ClientFailureClass::InvalidPeer
    );
    assert_eq!(
        replica
            .subscribe()
            .borrow()
            .retained()
            .unwrap()
            .cursor
            .revision
            .get(),
        5
    );
}

#[test]
fn retained_snapshot_leases_are_counted_in_residency() {
    let server = ServerId::new([1; 16]);
    let limits = ClientLimits {
        organization_server_bytes: 20_480,
        organization_registry_bytes: 20_480,
        ..ClientLimits::default()
    };
    let mut replica = replica(limits);
    let token = replica.begin_resync(ServerEpoch::new(1)).unwrap();
    replica
        .install_snapshot(token, snapshot(server, 1, 1))
        .unwrap();
    let mut held = vec![replica.subscribe().borrow().retained().unwrap().clone()];
    let mut updates = 0;
    loop {
        let token = replica.begin_resync(ServerEpoch::new(1)).unwrap();
        match replica.install_snapshot(token, snapshot(server, 1, 2 + updates)) {
            Ok(_) => {
                updates += 1;
                assert!(updates < 20);
                held.push(replica.subscribe().borrow().retained().unwrap().clone());
            }
            Err(error) => {
                assert_eq!(error.class(), ClientFailureClass::ResourceLimit);
                break;
            }
        }
    }
    assert!(updates > 0);
    drop(held);
    let token = replica.begin_resync(ServerEpoch::new(1)).unwrap();
    assert_eq!(
        replica
            .install_snapshot(token, snapshot(server, 1, 20))
            .unwrap(),
        ReplicaUpdate::Installed
    );
}

fn populated_snapshot(server: ServerId, revision: u64) -> OrganizationSnapshot {
    use spaceterm_core::{
        authority::{ControlGeneration, CreationOrdinal, SessionGeneration},
        identity::{PaneId, TabId, WorkspaceId},
        organization::{
            ControlRecord, NameMode, NameText, PaneCreation, PaneRecord, PaneRuntimeRecord,
            ServerDirectory, SessionPhase, TabRecord, WorkspaceName, WorkspaceRecord,
        },
        organization_retained::{
            PaneGenerationHighWater, TabCreationDefault, TabMembershipVersion, WorkspaceNameBasis,
        },
    };
    let workspace = WorkspaceId::new(server, 1).unwrap();
    let tab = TabId::new(server, 1).unwrap();
    let pane = PaneId::new(server, 1).unwrap();
    let mut state = snapshot(server, 1, revision).state().clone();
    let name = NameText::new("original".into()).unwrap();
    state.workspaces.push(WorkspaceRecord {
        id: workspace,
        root_tab: tab,
        creation_ordinal: CreationOrdinal::new(1),
        name: Versioned {
            value: WorkspaceName {
                mode: NameMode::Automatic,
                display: name.clone(),
            },
            version: FieldVersion::new(2),
        },
        pinned_directory: Versioned {
            value: None,
            version: FieldVersion::new(1),
        },
    });
    state.tabs.push(TabRecord {
        id: tab,
        workspace,
        root_pane: pane,
        worktree: None,
        creation_ordinal: CreationOrdinal::new(2),
    });
    state.panes.push(PaneRecord {
        id: pane,
        tab,
        creation_ordinal: CreationOrdinal::new(3),
        creation: PaneCreation::Initial,
        starting_directory: ServerDirectory::new(server, b"/server-only".to_vec()).unwrap(),
        confirmed_restoration_directory: Versioned {
            value: None,
            version: FieldVersion::new(1),
        },
    });
    let mut retained = state.retained.as_ref().unwrap().parts().clone();
    retained
        .workspace_names
        .push(WorkspaceNameBasis::new(workspace, name.clone(), name));
    retained
        .creation_defaults
        .push(TabCreationDefault::new(tab, vec![pane]).unwrap());
    retained.pane_generations.push(PaneGenerationHighWater::new(
        pane,
        SessionGeneration::new(0),
        ControlGeneration::new(0),
    ));
    retained
        .tab_membership_versions
        .push(TabMembershipVersion::new(tab, FieldVersion::new(1)));
    state.retained = Some(OrganizationRetainedFacts::new(retained).unwrap());
    OrganizationSnapshot::new(
        ServerEpoch::new(1),
        state,
        vec![PaneRuntimeRecord {
            pane,
            session_generation: SessionGeneration::new(0),
            control_generation: ControlGeneration::new(0),
            control: ControlRecord::Unclaimed,
            phase: SessionPhase::NotStarted,
        }],
    )
    .unwrap()
}

#[test]
fn delta_validation_failure_preserves_last_valid_replica() {
    use spaceterm_core::organization::{NameMode, NameText, OrganizationWrite, WorkspaceName};
    let server = ServerId::new([1; 16]);
    let base = populated_snapshot(server, 5);
    let mut replica = replica(ClientLimits::default());
    let token = replica.begin_resync(ServerEpoch::new(1)).unwrap();
    replica.install_snapshot(token, base.clone()).unwrap();
    let mut record = base.state().workspaces[0].clone();
    record.name = Versioned {
        version: FieldVersion::new(1),
        value: WorkspaceName {
            mode: NameMode::Custom,
            display: NameText::new("regressed".into()).unwrap(),
        },
    };
    let delta = OrganizationDelta::new(
        server,
        ServerEpoch::new(1),
        OrganizationRevision::new(5),
        OrganizationRevision::new(6),
        vec![OrganizationWrite::PutWorkspace(record)],
        vec![],
    )
    .unwrap();
    assert_eq!(
        replica.apply_delta(&delta).unwrap_err().class(),
        ClientFailureClass::InvalidPeer
    );
    let state = replica.subscribe();
    assert!(
        matches!(&*state.borrow(),ReplicaState::Stale { retained,.. } if retained.organization == base)
    );
}

#[test]
fn duplicate_ids_or_invalid_roots_reject_snapshot_without_partial_installation() {
    use spaceterm_core::identity::TabId;
    let server = ServerId::new([1; 16]);
    for duplicate in [false, true] {
        let base = populated_snapshot(server, 5);
        let mut invalid = base.state().clone();
        if duplicate {
            invalid.workspaces.push(invalid.workspaces[0].clone());
        } else {
            invalid.workspaces[0].root_tab = TabId::new(server, 99).unwrap();
        }
        let mut replica = replica(ClientLimits::default());
        let token = replica.begin_resync(ServerEpoch::new(1)).unwrap();
        let part = SnapshotPart {
            stamp: PublicationStamp {
                request: 1,
                request_generation: token.get(),
                transfer: 1,
                index: 0,
                total: 1,
                cursor: OrganizationCursor {
                    server,
                    epoch: ServerEpoch::new(1),
                    revision: OrganizationRevision::new(5),
                },
            },
            state: invalid,
            runtime: base.runtime().to_vec(),
        };
        assert_eq!(
            replica.stage_snapshot(token, part).unwrap_err().class(),
            ClientFailureClass::InvalidPeer
        );
        assert!(replica.subscribe().borrow().retained().is_none());
    }
}

#[test]
fn inactive_and_unattached_workspace_changes_are_published() {
    use spaceterm_core::organization::{NameMode, NameText, OrganizationWrite, WorkspaceName};
    let server = ServerId::new([1; 16]);
    let base = populated_snapshot(server, 5);
    let mut replica = replica(ClientLimits::default());
    let token = replica.begin_resync(ServerEpoch::new(1)).unwrap();
    replica.install_snapshot(token, base.clone()).unwrap();
    let mut updates = replica.subscribe();
    updates.borrow_and_update();
    let mut record = base.state().workspaces[0].clone();
    record.name = Versioned {
        version: FieldVersion::new(3),
        value: WorkspaceName {
            mode: NameMode::Custom,
            display: NameText::new("updated".into()).unwrap(),
        },
    };
    let delta = OrganizationDelta::new(
        server,
        ServerEpoch::new(1),
        OrganizationRevision::new(5),
        OrganizationRevision::new(6),
        vec![OrganizationWrite::PutWorkspace(record)],
        vec![],
    )
    .unwrap();
    replica.apply_delta(&delta).unwrap();
    assert!(updates.has_changed().unwrap());
    assert_eq!(
        updates
            .borrow()
            .retained()
            .unwrap()
            .organization
            .state()
            .workspaces[0]
            .name
            .value
            .display
            .as_str(),
        "updated"
    );
}

#[test]
fn multipart_delta_installs_only_the_complete_validated_candidate() {
    use spaceterm_core::organization::{NameMode, NameText, OrganizationWrite, WorkspaceName};
    let server = ServerId::new([1; 16]);
    let base = populated_snapshot(server, 5);
    let mut replica = replica(ClientLimits::default());
    let token = replica.begin_resync(ServerEpoch::new(1)).unwrap();
    replica.install_snapshot(token, base.clone()).unwrap();
    let mut workspace = base.state().workspaces[0].clone();
    workspace.name = Versioned {
        version: FieldVersion::new(3),
        value: WorkspaceName {
            mode: NameMode::Custom,
            display: NameText::new("new name".into()).unwrap(),
        },
    };
    let first = OrganizationDelta::new(
        server,
        ServerEpoch::new(1),
        OrganizationRevision::new(5),
        OrganizationRevision::new(6),
        vec![OrganizationWrite::PutWorkspace(workspace)],
        vec![],
    )
    .unwrap();
    let second = OrganizationDelta::new(
        server,
        ServerEpoch::new(1),
        OrganizationRevision::new(5),
        OrganizationRevision::new(6),
        vec![],
        vec![],
    )
    .unwrap();
    let stamp = |index| PublicationStamp {
        request: 0,
        request_generation: 0,
        transfer: 3,
        index,
        total: 2,
        cursor: OrganizationCursor {
            server,
            epoch: ServerEpoch::new(1),
            revision: OrganizationRevision::new(6),
        },
    };
    assert_eq!(
        replica.stage_delta(stamp(0), first).unwrap(),
        ReplicaUpdate::Staged
    );
    assert_eq!(
        replica
            .subscribe()
            .borrow()
            .retained()
            .unwrap()
            .organization,
        base
    );
    assert_eq!(
        replica.stage_delta(stamp(1), second).unwrap(),
        ReplicaUpdate::Installed
    );
    assert_eq!(
        replica
            .subscribe()
            .borrow()
            .retained()
            .unwrap()
            .organization
            .state()
            .workspaces[0]
            .name
            .value
            .display
            .as_str(),
        "new name"
    );
}

#[test]
fn zero_live_session_or_control_generation_is_invalid_peer_state() {
    use spaceterm_core::{
        authority::ControlGeneration,
        identity::ClientId,
        organization::{ControlRecord, SessionPhase},
    };
    let server = ServerId::new([1; 16]);
    for held in [false, true] {
        let base = populated_snapshot(server, 5);
        let mut runtime = base.runtime().to_vec();
        if held {
            runtime[0].control = ControlRecord::Held {
                client: ClientId::new(server, 1).unwrap(),
                generation: ControlGeneration::new(0),
            };
        } else {
            runtime[0].phase = SessionPhase::Running {
                generation: runtime[0].session_generation,
            };
        }
        let malformed =
            OrganizationSnapshot::new(ServerEpoch::new(1), base.state().clone(), runtime).unwrap();
        let mut replica = replica(ClientLimits::default());
        let token = replica.begin_resync(ServerEpoch::new(1)).unwrap();
        assert_eq!(
            replica
                .install_snapshot(token, malformed)
                .unwrap_err()
                .class(),
            ClientFailureClass::InvalidPeer
        );
        assert!(replica.subscribe().borrow().retained().is_none());
    }
}

#[test]
fn every_non_live_phase_requires_a_new_generation_to_revive() {
    use spaceterm_core::{
        authority::SessionGeneration,
        organization::{EndReason, FailureClass, OrganizationWrite, SessionPhase},
        organization_retained::{PaneGenerationHighWater, RetainedOrganizationWrite},
    };
    let server = ServerId::new([1; 16]);
    for phase in [
        SessionPhase::NotStarted,
        SessionPhase::DirectoryUnavailable,
        SessionPhase::Ended {
            reason: EndReason::EndAllSessions,
        },
        SessionPhase::Failed {
            class: FailureClass::RuntimeFault,
        },
    ] {
        for starting in [false, true] {
            for generation in [7, 8] {
                let base = populated_snapshot(server, 5);
                let mut state = base.state().clone();
                let mut runtime = base.runtime().to_vec();
                runtime[0].phase = phase;
                runtime[0].session_generation = SessionGeneration::new(7);
                let high_water = |generation| {
                    PaneGenerationHighWater::new(
                        runtime[0].pane,
                        SessionGeneration::new(generation),
                        runtime[0].control_generation,
                    )
                };
                let mut retained = state.retained.as_ref().unwrap().parts().clone();
                retained.pane_generations[0] = high_water(7);
                state.retained = Some(OrganizationRetainedFacts::new(retained).unwrap());
                let base =
                    OrganizationSnapshot::new(ServerEpoch::new(1), state, runtime.clone()).unwrap();
                let mut replica = replica(ClientLimits::default());
                let token = replica.begin_resync(ServerEpoch::new(1)).unwrap();
                replica.install_snapshot(token, base.clone()).unwrap();
                let high_water = high_water(generation);
                runtime[0].session_generation = SessionGeneration::new(generation);
                runtime[0].phase = if starting {
                    SessionPhase::Starting {
                        generation: runtime[0].session_generation,
                    }
                } else {
                    SessionPhase::Running {
                        generation: runtime[0].session_generation,
                    }
                };
                let delta = OrganizationDelta::new(
                    server,
                    ServerEpoch::new(1),
                    OrganizationRevision::new(5),
                    OrganizationRevision::new(6),
                    vec![OrganizationWrite::Retained(
                        RetainedOrganizationWrite::SetPaneGenerationHighWater(high_water),
                    )],
                    runtime,
                )
                .unwrap();
                let result = replica.apply_delta(&delta);
                if generation == 7 {
                    assert_eq!(
                        result.unwrap_err().class(),
                        ClientFailureClass::InvalidPeer,
                        "phase {phase:?}"
                    );
                    assert_eq!(
                        replica
                            .subscribe()
                            .borrow()
                            .retained()
                            .unwrap()
                            .organization,
                        base
                    );
                } else {
                    assert_eq!(result.unwrap(), ReplicaUpdate::Installed);
                }
            }
        }
    }
}

#[test]
fn starting_to_running_keeps_the_reserved_generation() {
    use spaceterm_core::{
        authority::SessionGeneration, organization::SessionPhase,
        organization_retained::PaneGenerationHighWater,
    };
    let server = ServerId::new([1; 16]);
    let base = populated_snapshot(server, 5);
    let mut state = base.state().clone();
    let mut runtime = base.runtime().to_vec();
    runtime[0].session_generation = SessionGeneration::new(7);
    runtime[0].phase = SessionPhase::Starting {
        generation: SessionGeneration::new(7),
    };
    let mut retained = state.retained.as_ref().unwrap().parts().clone();
    retained.pane_generations[0] = PaneGenerationHighWater::new(
        runtime[0].pane,
        runtime[0].session_generation,
        runtime[0].control_generation,
    );
    state.retained = Some(OrganizationRetainedFacts::new(retained).unwrap());
    let base = OrganizationSnapshot::new(ServerEpoch::new(1), state, runtime.clone()).unwrap();
    let mut replica = replica(ClientLimits::default());
    let token = replica.begin_resync(ServerEpoch::new(1)).unwrap();
    replica.install_snapshot(token, base).unwrap();
    runtime[0].phase = SessionPhase::Running {
        generation: SessionGeneration::new(7),
    };
    let delta = OrganizationDelta::new(
        server,
        ServerEpoch::new(1),
        OrganizationRevision::new(5),
        OrganizationRevision::new(6),
        vec![],
        runtime,
    )
    .unwrap();
    assert_eq!(
        replica.apply_delta(&delta).unwrap(),
        ReplicaUpdate::Installed
    );
}

fn inventory_snapshot(server: ServerId, revision: u64) -> OrganizationSnapshot {
    use spaceterm_core::{
        identity::{RepositoryId, WorktreeId},
        organization::{RepositoryRecord, ServerDirectory, WorktreeRecord},
        organization_retained::{
            RepositoryInventoryVersion, TabMembershipVersion, WorktreeInventoryFacts,
        },
    };
    let base = populated_snapshot(server, revision);
    let mut state = base.state().clone();
    let repository = RepositoryId::new(server, 1).unwrap();
    state.repositories.push(RepositoryRecord {
        id: repository,
        directory: ServerDirectory::new(server, b"/repo".to_vec()).unwrap(),
    });
    for id in 1..=2 {
        state.worktrees.push(WorktreeRecord {
            id: WorktreeId::new(server, id).unwrap(),
            repository,
            directory: ServerDirectory::new(server, format!("/repo/{id}").into_bytes()).unwrap(),
            main: id == 1,
            missing: false,
        });
    }
    let mut retained = state.retained.as_ref().unwrap().parts().clone();
    retained.tab_membership_versions[0] =
        TabMembershipVersion::new(state.tabs[0].id, FieldVersion::new(5));
    retained
        .repository_inventory_versions
        .push(RepositoryInventoryVersion::new(
            repository,
            FieldVersion::new(5),
        ));
    retained.worktree_inventory = state
        .worktrees
        .iter()
        .map(|w| WorktreeInventoryFacts::new(w.id, true, FieldVersion::new(5)))
        .collect();
    state.retained = Some(OrganizationRetainedFacts::new(retained).unwrap());
    OrganizationSnapshot::new(base.epoch(), state, base.runtime().to_vec()).unwrap()
}

#[test]
fn retained_comparison_versions_cannot_move_backwards() {
    use spaceterm_core::{
        organization::OrganizationWrite,
        organization_retained::{
            RepositoryInventoryVersion, RetainedOrganizationWrite, TabMembershipVersion,
            WorktreeInventoryFacts,
        },
    };
    let server = ServerId::new([1; 16]);
    let base = inventory_snapshot(server, 5);
    let writes = [
        RetainedOrganizationWrite::SetTabMembershipVersion(TabMembershipVersion::new(
            base.state().tabs[0].id,
            FieldVersion::new(4),
        )),
        RetainedOrganizationWrite::SetRepositoryInventoryVersion(RepositoryInventoryVersion::new(
            base.state().repositories[0].id,
            FieldVersion::new(4),
        )),
        RetainedOrganizationWrite::SetWorktreeInventoryFacts(WorktreeInventoryFacts::new(
            base.state().worktrees[0].id,
            true,
            FieldVersion::new(4),
        )),
    ];
    for write in writes {
        let mut replica = replica(ClientLimits::default());
        let token = replica.begin_resync(base.epoch()).unwrap();
        replica.install_snapshot(token, base.clone()).unwrap();
        let delta = OrganizationDelta::new(
            server,
            base.epoch(),
            OrganizationRevision::new(5),
            OrganizationRevision::new(6),
            vec![OrganizationWrite::Retained(write)],
            vec![],
        )
        .unwrap();
        assert_eq!(
            replica.apply_delta(&delta).unwrap_err().class(),
            ClientFailureClass::InvalidPeer
        );
        assert_eq!(
            replica
                .subscribe()
                .borrow()
                .retained()
                .unwrap()
                .organization,
            base
        );
    }
}

#[test]
fn membership_and_inventory_value_changes_require_advancing_their_versions() {
    use spaceterm_core::{
        organization::OrganizationWrite,
        organization_retained::{
            RepositoryInventoryVersion, RetainedOrganizationWrite, TabMembershipVersion,
            WorktreeInventoryFacts,
        },
    };
    let server = ServerId::new([1; 16]);
    let base = inventory_snapshot(server, 5);
    for field in ["membership", "repository", "worktree", "listing"] {
        for advance in [false, true] {
            let mut replica = replica(ClientLimits::default());
            let token = replica.begin_resync(base.epoch()).unwrap();
            replica.install_snapshot(token, base.clone()).unwrap();
            let version = FieldVersion::new(if advance { 6 } else { 5 });
            let mut writes = vec![];
            if field == "membership" {
                let mut tab = base.state().tabs[0].clone();
                tab.worktree = Some(base.state().worktrees[0].id);
                writes.push(OrganizationWrite::PutTab(tab.clone()));
                writes.push(OrganizationWrite::Retained(
                    RetainedOrganizationWrite::SetTabMembershipVersion(TabMembershipVersion::new(
                        tab.id, version,
                    )),
                ));
            } else {
                let mut worktree = base.state().worktrees[0].clone();
                if field != "listing" {
                    worktree.missing = true;
                }
                writes.push(OrganizationWrite::PutWorktree(worktree.clone()));
                writes.push(OrganizationWrite::Retained(
                    RetainedOrganizationWrite::SetWorktreeInventoryFacts(
                        WorktreeInventoryFacts::new(
                            worktree.id,
                            field != "listing",
                            if field == "repository" {
                                FieldVersion::new(6)
                            } else {
                                version
                            },
                        ),
                    ),
                ));
                writes.push(OrganizationWrite::Retained(
                    RetainedOrganizationWrite::SetRepositoryInventoryVersion(
                        RepositoryInventoryVersion::new(
                            worktree.repository,
                            if field == "repository" {
                                version
                            } else {
                                FieldVersion::new(6)
                            },
                        ),
                    ),
                ));
            }
            let delta = OrganizationDelta::new(
                server,
                base.epoch(),
                OrganizationRevision::new(5),
                OrganizationRevision::new(6),
                writes,
                vec![],
            )
            .unwrap();
            let result = replica.apply_delta(&delta);
            if advance {
                assert_eq!(result.unwrap(), ReplicaUpdate::Installed, "{field}");
            } else {
                assert_eq!(
                    result.unwrap_err().class(),
                    ClientFailureClass::InvalidPeer,
                    "{field}"
                );
                assert_eq!(
                    replica
                        .subscribe()
                        .borrow()
                        .retained()
                        .unwrap()
                        .organization,
                    base
                );
            }
        }
    }
}

#[test]
fn equivalent_reordered_resync_after_reserved_id_insertion_is_accepted() {
    use spaceterm_core::{
        authority::CreationOrdinal,
        identity::{PaneId, TabId},
        organization::OrganizationWrite,
        organization_retained::{
            PaneGenerationHighWater, RetainedOrganizationWrite, TabCreationDefault,
            TabMembershipVersion,
        },
    };
    let server = ServerId::new([1; 16]);
    let original = inventory_snapshot(server, 5);
    let add_tab = |base: &OrganizationSnapshot, id, ordinal| {
        let mut tab = base.state().tabs[0].clone();
        let mut pane = base.state().panes[0].clone();
        let mut runtime = base.runtime()[0];
        tab.id = TabId::new(server, id).unwrap();
        tab.root_pane = PaneId::new(server, id).unwrap();
        tab.creation_ordinal = CreationOrdinal::new(ordinal);
        pane.id = tab.root_pane;
        pane.tab = tab.id;
        pane.creation_ordinal = CreationOrdinal::new(ordinal + 1);
        runtime.pane = pane.id;
        let writes = vec![
            OrganizationWrite::PutTab(tab.clone()),
            OrganizationWrite::PutPane(pane.clone()),
            OrganizationWrite::Retained(RetainedOrganizationWrite::PutCreationDefault(
                TabCreationDefault::new(tab.id, vec![pane.id]).unwrap(),
            )),
            OrganizationWrite::Retained(RetainedOrganizationWrite::SetTabMembershipVersion(
                TabMembershipVersion::new(tab.id, FieldVersion::new(1)),
            )),
            OrganizationWrite::Retained(RetainedOrganizationWrite::SetPaneGenerationHighWater(
                PaneGenerationHighWater::new(
                    pane.id,
                    runtime.session_generation,
                    runtime.control_generation,
                ),
            )),
        ];
        OrganizationDelta::new(
            server,
            base.epoch(),
            base.state().revision,
            OrganizationRevision::new(base.state().revision.get() + 1),
            writes,
            vec![runtime],
        )
        .unwrap()
    };
    let mut replica = replica(ClientLimits::default());
    let token = replica.begin_resync(original.epoch()).unwrap();
    replica.install_snapshot(token, original.clone()).unwrap();
    replica.apply_delta(&add_tab(&original, 3, 4)).unwrap();
    let before = replica
        .subscribe()
        .borrow()
        .retained()
        .unwrap()
        .organization
        .clone();
    replica.apply_delta(&add_tab(&before, 2, 6)).unwrap();
    let installed = replica
        .subscribe()
        .borrow()
        .retained()
        .unwrap()
        .organization
        .clone();
    assert_eq!(
        installed
            .state()
            .tabs
            .iter()
            .map(|t| t.id.local())
            .collect::<Vec<_>>(),
        [1, 3, 2]
    );
    let mut reordered = installed.state().clone();
    reordered.tabs.sort_by_key(|r| r.id);
    reordered.panes.sort_by_key(|r| r.id);
    reordered.worktrees.reverse();
    let mut runtime = installed.runtime().to_vec();
    runtime.sort_by_key(|r| r.pane);
    let resync = OrganizationSnapshot::new(installed.epoch(), reordered, runtime).unwrap();
    let token = replica.begin_resync(installed.epoch()).unwrap();
    assert_eq!(
        replica.install_snapshot(token, resync).unwrap(),
        ReplicaUpdate::Installed
    );
}
