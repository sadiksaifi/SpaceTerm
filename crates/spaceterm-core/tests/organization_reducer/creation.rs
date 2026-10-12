use super::support::*;

#[test]
fn create_workspace_commits_initial_hierarchy_roots_and_creator_grant() {
    let mut reducer = reducer();
    let command = Command::CreateWorkspace {
        server: SERVER,
        source: WorkspaceCreationSource::ServerHome { server: SERVER },
        name: NameDraft::Automatic,
    };
    let context = facts(&reducer, &command, 1);
    let before = reducer.snapshot();
    let prepared = reducer.prepare_command(&command, &context).unwrap();
    assert_eq!(reducer.snapshot(), before);
    assert_eq!(prepared.writes().resulting_high_water().creation_ordinal, 3);
    let committed = commit(&mut reducer, prepared);
    let snapshot = reducer.snapshot();
    let workspace = &snapshot.state().workspaces[0];
    let tab = &snapshot.state().tabs[0];
    let pane = &snapshot.state().panes[0];
    assert_eq!(
        (workspace.root_tab, tab.root_pane, pane.tab),
        (tab.id, pane.id, tab.id)
    );
    assert_eq!(snapshot.runtime()[0].phase, SessionPhase::NotStarted);
    assert_eq!(
        snapshot.runtime()[0].control,
        ControlRecord::Held {
            client: context.principal.client,
            generation: ControlGeneration::new(1)
        }
    );
    match committed.resolution {
        Resolution::Completed(Outcome::CreatedWithControl(created)) => {
            assert_eq!(created.control().epoch(), ServerEpoch::new(9))
        }
        _ => panic!("missing exact grant provenance"),
    }
    assert!(matches!(
        committed.effects.as_slice(),
        [OrganizationEffect::RegisterCreatedPane { .. }]
    ));
    reducer
        .retained()
        .validate(OrganizationLimits::default())
        .unwrap();
}

#[test]
fn independent_tab_creates_allocate_distinct_server_wide_ids() {
    let mut reducer = reducer();
    let (workspace, _, source) = create(&mut reducer, 1);
    let command = Command::CreateTab {
        workspace,
        source: TabCreationSource::Pane(source),
    };
    apply(&mut reducer, command.clone(), 2);
    apply(&mut reducer, command, 3);
    let snapshot = reducer.snapshot();
    assert_eq!(
        snapshot
            .state()
            .tabs
            .iter()
            .map(|v| v.id.local())
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(reducer.retained().high_water().pane, 3);
    assert!(
        snapshot
            .runtime()
            .iter()
            .all(|v| v.phase == SessionPhase::NotStarted)
    );
}
#[test]
fn failed_final_allocation_leaves_all_marks_and_records_unchanged() {
    let base = empty();
    let mut marks = base.high_water();
    marks.pane = u64::MAX;
    let retained = RetainedOrganization::new(
        base.organization().clone(),
        marks,
        base.server_home().clone(),
    );
    let reducer = OrganizationReducer::from_recovered(
        retained,
        StoreRevision::new(0),
        ServerEpoch::new(1),
        OrganizationLimits::default(),
    )
    .unwrap();
    let command = Command::CreateWorkspace {
        server: SERVER,
        source: WorkspaceCreationSource::ServerHome { server: SERVER },
        name: NameDraft::Automatic,
    };
    assert!(matches!(
        reducer.prepare_command(&command, &facts(&reducer, &command, 1)),
        Err(ReducerFailure::CounterExhausted)
    ));
    assert_eq!(reducer.retained().high_water(), marks);
    assert!(reducer.snapshot().state().workspaces.is_empty());
}
#[test]
fn stale_directory_validation_rejects_before_allocation() {
    let mut reducer = reducer();
    let (workspace, _, source) = create(&mut reducer, 1);
    let command = Command::CreateTab {
        workspace,
        source: TabCreationSource::Pane(source),
    };
    let stale = facts(&reducer, &command, 2);
    apply(
        &mut reducer,
        Command::SetPinnedDirectory {
            workspace,
            expected_version: FieldVersion::new(1),
            value: Some(directory(b"/pinned")),
        },
        3,
    );
    let before = reducer.retained();
    let prepared = reducer.prepare_command(&command, &stale).unwrap();
    assert!(matches!(
        prepared.resolution(),
        Resolution::Completed(Outcome::Failed(OperationFailure::StaleValidation))
    ));
    assert_eq!(
        prepared.writes().resulting_high_water(),
        before.high_water()
    );
}
#[test]
fn unavailable_explicit_pin_never_falls_back() {
    let mut reducer = reducer();
    let (workspace, _, source) = create(&mut reducer, 1);
    apply(
        &mut reducer,
        Command::SetPinnedDirectory {
            workspace,
            expected_version: FieldVersion::new(1),
            value: Some(directory(b"/pinned")),
        },
        2,
    );
    let command = Command::CreateTab {
        workspace,
        source: TabCreationSource::Pane(source),
    };
    let mut context = facts(&reducer, &command, 3);
    context
        .facts
        .directories
        .as_mut()
        .unwrap()
        .pin
        .as_mut()
        .unwrap()
        .available = false;
    let prepared = reducer.prepare_command(&command, &context).unwrap();
    assert!(matches!(
        prepared.resolution(),
        Resolution::Completed(Outcome::Failed(OperationFailure::DirectoryUnavailable))
    ));
    assert_eq!(
        prepared.writes().resulting_high_water(),
        reducer.retained().high_water()
    );
}
#[test]
fn creation_rejects_foreign_server_and_cross_tab_source() {
    let mut reducer = reducer();
    let (_, first_tab, first_pane) = create(&mut reducer, 1);
    let (_, other_tab, other_pane) = create(&mut reducer, 2);
    let command = Command::CreatePane {
        tab: first_tab,
        source: other_pane,
        axis: SplitAxis::Horizontal,
        edge: SplitEdge::After,
    };
    let result = reducer.prepare_command(&command, &context(3)).unwrap();
    assert!(matches!(
        result.resolution(),
        Resolution::Completed(Outcome::Failed(OperationFailure::WrongMembership))
    ));
    let foreign = TabId::new(ServerId::new([7; 16]), other_tab.local()).unwrap();
    let command = Command::CreatePane {
        tab: foreign,
        source: first_pane,
        axis: SplitAxis::Horizontal,
        edge: SplitEdge::After,
    };
    assert!(matches!(
        reducer.prepare_command(&command, &context(4)),
        Err(ReducerFailure::WrongServer)
    ));
}
#[test]
fn organization_limits_reject_bounds_plus_one_without_allocating() {
    let limits = OrganizationLimits {
        workspaces: 1,
        ..Default::default()
    };
    let mut reducer = OrganizationReducer::from_recovered(
        empty(),
        StoreRevision::new(0),
        ServerEpoch::new(1),
        limits,
    )
    .unwrap();
    create(&mut reducer, 1);
    let marks = reducer.retained().high_water();
    let result = apply(
        &mut reducer,
        Command::CreateWorkspace {
            server: SERVER,
            source: WorkspaceCreationSource::ServerHome { server: SERVER },
            name: NameDraft::Automatic,
        },
        2,
    );
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::Capacity))
    ));
    assert_eq!(reducer.retained().high_water(), marks);
    assert!(result.publication.is_none());
    assert!(result.effects.is_empty());
}
#[test]
fn deleted_ids_are_not_reused_after_recovery() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let retained = reducer
        .retained()
        .preview_removal(&[pane], OrganizationLimits::default())
        .unwrap();
    let mut recovered = OrganizationReducer::from_recovered(
        retained,
        StoreRevision::new(2),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let (workspace, tab, new_pane) = create(&mut recovered, 2);
    assert_eq!(
        (workspace.local(), tab.local(), new_pane.local()),
        (2, 2, 2)
    );
}

#[test]
fn git_identity_reservation_advances_marks_before_external_work_without_publication() {
    let mut reducer = reducer();
    let request = GitAllocationRequest {
        repository: true,
        worktree: true,
        fallbacks: vec![],
    };
    let prepared = reducer
        .prepare_git_allocation(&request, &context(1))
        .unwrap();
    assert_eq!(reducer.retained().high_water().repository, 0);
    assert_eq!(prepared.writes().resulting_high_water().repository, 1);
    assert!(prepared.writes().organization_writes().is_empty());
    let committed = commit(&mut reducer, prepared);
    assert!(committed.publication.is_none() && committed.effects.is_empty());
    assert_eq!(reducer.retained().git_allocations().len(), 1);
}

#[test]
fn pin_source_home_and_worktree_directory_precedence() {
    let mut base = reducer();
    let (workspace, _, source) = create(&mut base, 1);
    let mut reducer = OrganizationReducer::from_recovered(
        with_worktrees(base.retained()),
        StoreRevision::new(1),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let worktree = WorktreeId::new(SERVER, 2).unwrap();
    let command = Command::CreateTab {
        workspace,
        source: TabCreationSource::Worktree(worktree),
    };
    apply(&mut reducer, command, 2);
    let tab = reducer.snapshot().state().tabs[1].id;
    let pane = reducer.snapshot().state().tabs[1].root_pane;
    apply(
        &mut reducer,
        Command::SetPinnedDirectory {
            workspace,
            expected_version: FieldVersion::new(1),
            value: Some(directory(b"/outside")),
        },
        3,
    );
    let command = Command::CreatePane {
        tab,
        source: pane,
        axis: SplitAxis::Horizontal,
        edge: SplitEdge::After,
    };
    let mut context = facts(&reducer, &command, 4);
    context
        .facts
        .directories
        .as_mut()
        .unwrap()
        .pin
        .as_mut()
        .unwrap()
        .physically_inside = None;
    let prepared = reducer.prepare_command(&command, &context).unwrap();
    commit(&mut reducer, prepared);
    assert!(
        reducer
            .snapshot()
            .state()
            .panes
            .last()
            .unwrap()
            .starting_directory
            == directory(b"/repo/feature")
    );
    let command = Command::CreateTab {
        workspace,
        source: TabCreationSource::Pane(source),
    };
    apply(&mut reducer, command, 5);
    assert!(
        reducer
            .snapshot()
            .state()
            .panes
            .last()
            .unwrap()
            .starting_directory
            == directory(b"/outside")
    );
}
#[test]
fn create_tab_uses_explicit_workspace_source_and_worktree() {
    let mut base = reducer();
    let (workspace, _, _) = create(&mut base, 1);
    let mut reducer = OrganizationReducer::from_recovered(
        with_worktrees(base.retained()),
        StoreRevision::new(1),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let worktree = WorktreeId::new(SERVER, 2).unwrap();
    apply(
        &mut reducer,
        Command::CreateTab {
            workspace,
            source: TabCreationSource::Worktree(worktree),
        },
        2,
    );
    assert_eq!(reducer.snapshot().state().tabs[1].worktree, Some(worktree));
}

#[test]
fn reserved_git_ids_can_be_consumed_after_later_allocations_but_not_twice() {
    use spaceterm_core::worktrees::intent::GitIntentPhase;
    let mut reducer = reducer();
    let context = context(1);
    let prepared = reducer
        .prepare_git_allocation(
            &GitAllocationRequest {
                repository: true,
                worktree: true,
                fallbacks: vec![],
            },
            &context,
        )
        .unwrap();
    let reservation = match prepared.resolution() {
        Resolution::AllocationReserved(v) => (**v).clone(),
        _ => panic!("reservation rejected"),
    };
    commit(&mut reducer, prepared);
    create(&mut reducer, 2);
    let repository = reservation.parts().repository.unwrap();
    let worktree = reservation.parts().worktree.unwrap();
    let patch = WorktreeOrganizationPatch {
        binding: context.binding(),
        scope: RepositoryScope {
            server: SERVER,
            repository,
        },
        expected_inventory_version: None,
        repositories: vec![RepositoryRecord {
            id: repository,
            directory: directory(b"/repo"),
        }],
        worktrees: vec![(
            WorktreeRecord {
                id: worktree,
                repository,
                directory: directory(b"/repo/main"),
                main: true,
                missing: false,
            },
            true,
        )],
        memberships: vec![],
        settlement: Some(GitOrganizationSettlement {
            reservation,
            phase: GitIntentPhase::Started,
            remove_tabs: vec![],
            creator_still_authorized: true,
        }),
    };
    let before = reducer.snapshot();
    let event = OrganizationEvent::WorktreeOrganizationChanged(Box::new(patch));
    let prepared = reducer.prepare_event(&event).unwrap();
    let result = commit(&mut reducer, prepared);
    assert_eq!(reducer.retained().git_allocations().len(), 0);
    assert_eq!(reducer.snapshot().state().worktrees[0].id, worktree);
    assert_eq!(
        replay(before, result.publication.as_ref().unwrap()),
        reducer.snapshot()
    );
    let repeated = reducer.prepare_event(&event).unwrap();
    assert!(matches!(
        repeated.resolution(),
        Resolution::Completed(Outcome::Failed(OperationFailure::StaleValidation))
    ));
}
#[test]
fn started_git_fallback_settles_during_native_barrier_without_grant_for_revoked_creator() {
    use spaceterm_core::{native_management::*, retirement::*, worktrees::intent::*};
    let mut base = reducer();
    let (workspace, _, _) = create(&mut base, 1);
    let retained = with_worktrees(base.retained());
    let mut org = retained.organization().clone();
    org.tabs[0].worktree = Some(WorktreeId::new(SERVER, 2).unwrap());
    let retained =
        RetainedOrganization::new(org, retained.high_water(), retained.server_home().clone());
    let mut reducer = OrganizationReducer::from_recovered(
        retained,
        StoreRevision::new(1),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let context = context(2);
    let request = GitAllocationRequest {
        repository: false,
        worktree: false,
        fallbacks: vec![WorkspaceFallbackPlan {
            workspace,
            worktree: WorktreeId::new(SERVER, 1).unwrap(),
            directory: directory(b"/repo/main"),
        }],
    };
    let prepared = reducer.prepare_git_allocation(&request, &context).unwrap();
    let reservation = match prepared.resolution() {
        Resolution::AllocationReserved(v) => (**v).clone(),
        _ => panic!("reservation rejected"),
    };
    commit(&mut reducer, prepared);
    let native = NativeRetirementDescriptor::new(NativeRetirementDescriptorParts {
        request: NativeManagementRequestRef::new(
            SERVER,
            [1; 16],
            RequestDigest::from_bytes([7; 32]),
        )
        .unwrap(),
        batch: RetirementBatchId::new(SERVER, ServerEpoch::new(10), 1).unwrap(),
        action: NativeRetirementAction::Stop,
        acceptance_revision: OrganizationRevision::new(1),
        panes: vec![],
        sessions: vec![],
    })
    .unwrap();
    let retained = reducer.retained().with_native_retirements(vec![native]);
    reducer = OrganizationReducer::from_recovered(
        retained,
        StoreRevision::new(2),
        ServerEpoch::new(11),
        OrganizationLimits::default(),
    )
    .unwrap();
    let source = reducer.snapshot().state().panes[0].id;
    let blocked = apply(
        &mut reducer,
        Command::CreateTab {
            workspace,
            source: TabCreationSource::Pane(source),
        },
        3,
    );
    assert!(matches!(
        blocked.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
    ));
    let removed_tab = reducer.snapshot().state().tabs[0].clone();
    let patch = WorktreeOrganizationPatch {
        binding: context.binding(),
        scope: RepositoryScope {
            server: SERVER,
            repository: RepositoryId::new(SERVER, 1).unwrap(),
        },
        expected_inventory_version: Some(FieldVersion::new(1)),
        repositories: vec![],
        worktrees: vec![],
        memberships: vec![],
        settlement: Some(GitOrganizationSettlement {
            reservation,
            phase: GitIntentPhase::Started,
            remove_tabs: vec![removed_tab],
            creator_still_authorized: false,
        }),
    };
    let prepared = reducer
        .prepare_event(&OrganizationEvent::WorktreeOrganizationChanged(Box::new(
            patch,
        )))
        .unwrap();
    let before = reducer.snapshot();
    let committed = commit(&mut reducer, prepared);
    assert_eq!(reducer.snapshot().state().workspaces[0].id, workspace);
    assert_eq!(
        reducer.snapshot().state().tabs[0].worktree,
        Some(WorktreeId::new(SERVER, 1).unwrap())
    );
    assert_eq!(
        reducer.snapshot().runtime()[0].control,
        ControlRecord::Unclaimed
    );
    assert_eq!(
        reducer.snapshot().runtime()[0].phase,
        SessionPhase::NotStarted
    );
    assert!(committed.effects.is_empty());
    assert_eq!(
        replay(before, committed.publication.as_ref().unwrap()),
        reducer.snapshot()
    );
}

#[test]
fn abandoned_git_reservations_never_make_ids_reusable() {
    let mut reducer = reducer();
    let request = GitAllocationRequest {
        repository: true,
        worktree: true,
        fallbacks: vec![],
    };
    let first_context = context(1);
    let prepared = reducer
        .prepare_git_allocation(&request, &first_context)
        .unwrap();
    commit(&mut reducer, prepared);
    let marks = reducer.retained().high_water();
    let abandoned = reducer
        .prepare_abandon_git_allocation(first_context.binding())
        .unwrap();
    let result = commit(&mut reducer, abandoned);
    assert!(result.publication.is_none() && result.effects.is_empty());
    assert!(reducer.retained().git_allocations().is_empty());
    assert_eq!(reducer.retained().high_water(), marks);
    let second = reducer
        .prepare_git_allocation(&request, &context(2))
        .unwrap();
    assert_eq!(second.writes().resulting_high_water().repository, 2);
    assert_eq!(second.writes().resulting_high_water().worktree, 2);
}
#[test]
fn recovery_rejects_two_reservations_for_the_same_absent_id() {
    use spaceterm_core::worktrees::intent::*;
    let mut reducer = reducer();
    let request = GitAllocationRequest {
        repository: true,
        worktree: false,
        fallbacks: vec![],
    };
    let prepared = reducer
        .prepare_git_allocation(&request, &context(1))
        .unwrap();
    commit(&mut reducer, prepared);
    let retained = reducer.retained();
    let first = retained.git_allocations()[0].clone();
    let mut duplicate = first.parts().clone();
    duplicate.binding = context(2).binding();
    let duplicated = retained.with_git_allocations(vec![
        first,
        GitAllocationReservation::new(duplicate).unwrap(),
    ]);
    assert_eq!(
        duplicated.validate(OrganizationLimits::default()),
        Err(ReducerFailure::InvalidDurableState(
            DurableInvariant::Reservation
        ))
    );
}

#[test]
fn recovery_rejects_reserved_ordinals_that_collide_with_surviving_records() {
    use spaceterm_core::worktrees::intent::*;
    let mut reducer = reducer();
    let (workspace, _, _) = create(&mut reducer, 1);
    create(&mut reducer, 2);
    let retained = with_worktrees(reducer.retained());
    let mut before = retained.high_water();
    before.creation_ordinal = 4;
    let mut after = before;
    after.tab += 1;
    after.pane += 1;
    after.creation_ordinal += 2;
    let reservation = GitAllocationReservation::new(GitAllocationReservationParts {
        binding: context(3).binding(),
        expected_high_water: before,
        resulting_high_water: after,
        repository: None,
        worktree: None,
        fallbacks: vec![GitReservedFallback {
            plan: WorkspaceFallbackPlan {
                workspace,
                worktree: WorktreeId::new(SERVER, 1).unwrap(),
                directory: directory(b"/repo/main"),
            },
            tab: TabId::new(SERVER, after.tab).unwrap(),
            pane: PaneId::new(SERVER, after.pane).unwrap(),
            tab_creation_ordinal: CreationOrdinal::new(5),
            pane_creation_ordinal: CreationOrdinal::new(6),
        }],
    })
    .unwrap();
    let corrupt = RetainedOrganization::new(
        retained.organization().clone(),
        after,
        retained.server_home().clone(),
    )
    .with_git_allocations(vec![reservation]);
    assert_eq!(
        corrupt.validate(OrganizationLimits::default()),
        Err(ReducerFailure::InvalidDurableState(
            DurableInvariant::Reservation
        ))
    );
    assert!(matches!(
        OrganizationReducer::from_recovered(
            corrupt,
            StoreRevision::new(2),
            ServerEpoch::new(10),
            OrganizationLimits::default(),
        ),
        Err(ReducerFailure::InvalidDurableState(
            DurableInvariant::Reservation
        ))
    ));
}

#[test]
fn creation_is_blocked_in_a_pending_worktree_retirement() {
    use spaceterm_core::{operation_ledger_contracts::CompactOutcome, retirement::*};
    let mut base = reducer();
    let (workspace, tab, pane) = create(&mut base, 1);
    let retained = with_worktrees(base.retained());
    let mut org = retained.organization().clone();
    let worktree = WorktreeId::new(SERVER, 2).unwrap();
    org.tabs[0].worktree = Some(worktree);
    let mut retained_facts = org.retained.as_ref().unwrap().parts().clone();
    retained_facts.pane_generations[0] =
        PaneGenerationHighWater::new(pane, SessionGeneration::new(1), ControlGeneration::new(1));
    org.retained = Some(OrganizationRetainedFacts::new(retained_facts).unwrap());
    // This fixture stands in for the ledger's validated pending projection. The
    // reducer treats the compact result as opaque; no codec or cleanup is claimed.
    let pending = RetirementDescriptor::new(RetirementDescriptorParts {
        batch: RetirementBatchId::new(SERVER, ServerEpoch::new(9), 1).unwrap(),
        binding: context(3).binding(),
        assessment: AssessmentRef {
            operation: context(2).operation,
            digest: context(2).request_digest,
        },
        acceptance_revision: OrganizationRevision::new(1),
        target: AssessmentTarget::RemoveWorktree {
            scope: RepositoryScope {
                server: SERVER,
                repository: RepositoryId::new(SERVER, 1).unwrap(),
            },
            worktree,
        },
        panes: vec![
            RetirementPane::new(
                workspace,
                tab,
                pane,
                Some(worktree),
                SessionGeneration::new(1),
            )
            .unwrap(),
        ],
        sessions: vec![],
        completion: RetirementCompletion::ContinueGitSettlement,
        eventual_result: CompactOutcome::from_encoded(vec![1]).unwrap(),
    })
    .unwrap();
    let retained =
        RetainedOrganization::new(org, retained.high_water(), retained.server_home().clone())
            .with_pending_retirements(vec![pending]);
    let mut reducer = OrganizationReducer::from_recovered(
        retained,
        StoreRevision::new(1),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let result = apply(
        &mut reducer,
        Command::CreatePane {
            tab,
            source: pane,
            axis: SplitAxis::Vertical,
            edge: SplitEdge::After,
        },
        4,
    );
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
    ));
    assert!(result.publication.is_none() && result.effects.is_empty());
}

fn inventory_patch(
    repository: Option<RepositoryId>,
    version: Option<FieldVersion>,
) -> GitInventoryPatch {
    GitInventoryPatch {
        repository,
        directory: directory(b"/repo"),
        expected_inventory_version: version,
        worktrees: vec![GitInventoryWorktree {
            id: None,
            directory: directory(b"/repo/external"),
            main: repository.is_none(),
            missing: false,
            listed: true,
        }],
    }
}

#[test]
fn git_inventory_discovery_allocates_repository_and_multiple_worktrees_without_write_intent() {
    let mut reducer = reducer();
    let mut inventory = inventory_patch(None, None);
    for value in [b"/repo/second".as_slice(), b"/repo/third"] {
        inventory.worktrees.push(GitInventoryWorktree {
            id: None,
            directory: directory(value),
            main: false,
            missing: false,
            listed: true,
        });
    }
    let before = reducer.retained();
    let snapshot = reducer.snapshot();
    let prepared = reducer
        .prepare_git_inventory(&inventory, &context(1))
        .unwrap();
    assert_eq!(reducer.retained(), before);
    let mut expected_marks = before.high_water();
    expected_marks.repository = 1;
    expected_marks.worktree = 3;
    assert_eq!(prepared.writes().resulting_high_water(), expected_marks);
    assert!(
        prepared
            .writes()
            .operation_writes()
            .iter()
            .all(|write| { matches!(write, OperationWrite::ReductionBasis(_)) })
    );
    let result = commit(&mut reducer, prepared);
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::Applied)
    ));
    assert!(result.effects.is_empty());
    assert!(reducer.retained().git_allocations().is_empty());
    assert_eq!(reducer.snapshot().state().repositories.len(), 1);
    assert_eq!(reducer.snapshot().state().worktrees.len(), 3);
    assert_eq!(reducer.retained().high_water(), expected_marks);
    assert_eq!(
        replay(snapshot, result.publication.as_ref().unwrap()),
        reducer.snapshot()
    );
}

#[test]
fn git_inventory_discovers_worktrees_in_a_retained_repository_without_clearing_membership() {
    let mut base = reducer();
    create(&mut base, 1);
    let retained = with_worktrees(base.retained());
    let mut org = retained.organization().clone();
    org.tabs[0].worktree = Some(WorktreeId::new(SERVER, 2).unwrap());
    org.worktrees[1].missing = true;
    let retained =
        RetainedOrganization::new(org, retained.high_water(), retained.server_home().clone());
    let mut reducer = OrganizationReducer::from_recovered(
        retained,
        StoreRevision::new(1),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let before = reducer.snapshot();
    let repository = RepositoryId::new(SERVER, 1).unwrap();
    let mut inventory = inventory_patch(Some(repository), Some(FieldVersion::new(1)));
    inventory.worktrees.push(GitInventoryWorktree {
        id: None,
        directory: directory(b"/repo/another"),
        main: false,
        missing: false,
        listed: true,
    });
    let prepared = reducer
        .prepare_git_inventory(&inventory, &context(2))
        .unwrap();
    let result = commit(&mut reducer, prepared);
    let after = reducer.snapshot();
    assert_eq!(after.state().tabs, before.state().tabs);
    assert_eq!(after.state().panes, before.state().panes);
    assert_eq!(after.state().workspaces, before.state().workspaces);
    assert_eq!(after.runtime(), before.runtime());
    assert_eq!(after.state().worktrees[..2], before.state().worktrees);
    assert_eq!(after.state().worktrees.len(), 4);
    assert_eq!(reducer.retained().high_water().worktree, 4);
    assert_eq!(reducer.retained().high_water().repository, 1);
    assert_eq!(
        after
            .state()
            .retained
            .as_ref()
            .unwrap()
            .repository_inventory_versions()[0]
            .version(),
        FieldVersion::new(2)
    );
    assert!(result.effects.is_empty());
    assert_eq!(replay(before, result.publication.as_ref().unwrap()), after);
}

#[test]
fn git_inventory_cannot_insert_reserved_or_forgotten_identities() {
    let mut reducer = reducer();
    let prepared = reducer
        .prepare_git_allocation(
            &GitAllocationRequest {
                repository: true,
                worktree: true,
                fallbacks: vec![],
            },
            &context(1),
        )
        .unwrap();
    commit(&mut reducer, prepared);
    let reserved = reducer.retained();
    let missing_repository = inventory_patch(Some(RepositoryId::new(SERVER, 1).unwrap()), None);
    let prepared = reducer
        .prepare_git_inventory(&missing_repository, &context(2))
        .unwrap();
    assert!(matches!(
        prepared.resolution(),
        Resolution::Completed(Outcome::Failed(OperationFailure::TargetDeleted))
    ));
    drop(prepared);
    assert_eq!(reducer.retained(), reserved);
    let prepared = reducer
        .prepare_git_inventory(&inventory_patch(None, None), &context(3))
        .unwrap();
    commit(&mut reducer, prepared);
    assert_eq!(reducer.snapshot().state().repositories[0].id.local(), 2);
    assert_eq!(reducer.snapshot().state().worktrees[0].id.local(), 2);
    assert_eq!(
        reducer.retained().git_allocations(),
        reserved.git_allocations()
    );
    let mut missing_worktree = inventory_patch(
        Some(RepositoryId::new(SERVER, 2).unwrap()),
        Some(FieldVersion::new(1)),
    );
    missing_worktree.worktrees[0].id = Some(WorktreeId::new(SERVER, 1).unwrap());
    let prepared = reducer
        .prepare_git_inventory(&missing_worktree, &context(4))
        .unwrap();
    assert!(matches!(
        prepared.resolution(),
        Resolution::Completed(Outcome::Failed(OperationFailure::TargetDeleted))
    ));
    drop(prepared);

    // Forget unreferenced inventory while retaining its allocation history.
    let retained = reducer.retained();
    let mut org = retained.organization().clone();
    org.repositories.clear();
    org.worktrees.clear();
    let mut facts = org.retained.as_ref().unwrap().parts().clone();
    facts.repository_inventory_versions.clear();
    facts.worktree_inventory.clear();
    org.retained = Some(OrganizationRetainedFacts::new(facts).unwrap());
    let forgotten =
        RetainedOrganization::new(org, retained.high_water(), retained.server_home().clone())
            .with_git_allocations(retained.git_allocations().to_vec());
    reducer = OrganizationReducer::from_recovered(
        forgotten,
        StoreRevision::new(2),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let stale = inventory_patch(
        Some(RepositoryId::new(SERVER, 2).unwrap()),
        Some(FieldVersion::new(1)),
    );
    let prepared = reducer.prepare_git_inventory(&stale, &context(5)).unwrap();
    assert!(matches!(
        prepared.resolution(),
        Resolution::Completed(Outcome::Failed(OperationFailure::TargetDeleted))
    ));
    drop(prepared);
    let prepared = reducer
        .prepare_git_inventory(&inventory_patch(None, None), &context(6))
        .unwrap();
    commit(&mut reducer, prepared);
    assert_eq!(reducer.snapshot().state().repositories[0].id.local(), 3);
    assert_eq!(reducer.snapshot().state().worktrees[0].id.local(), 3);
    let mut stale_worktree = inventory_patch(
        Some(RepositoryId::new(SERVER, 3).unwrap()),
        Some(FieldVersion::new(1)),
    );
    stale_worktree.worktrees[0].id = Some(WorktreeId::new(SERVER, 2).unwrap());
    let prepared = reducer
        .prepare_git_inventory(&stale_worktree, &context(7))
        .unwrap();
    assert!(matches!(
        prepared.resolution(),
        Resolution::Completed(Outcome::Failed(OperationFailure::TargetDeleted))
    ));
}

#[test]
fn git_inventory_rejects_stale_versions_without_allocations_or_publication() {
    let retained = with_worktrees(empty());
    let mut marks = retained.high_water();
    marks.worktree = u64::MAX;
    let retained = RetainedOrganization::new(
        retained.organization().clone(),
        marks,
        retained.server_home().clone(),
    );
    let mut reducer = OrganizationReducer::from_recovered(
        retained,
        StoreRevision::new(0),
        ServerEpoch::new(9),
        OrganizationLimits::default(),
    )
    .unwrap();
    let before = reducer.retained();
    let stale = inventory_patch(
        Some(RepositoryId::new(SERVER, 1).unwrap()),
        Some(FieldVersion::new(2)),
    );
    let prepared = reducer.prepare_git_inventory(&stale, &context(1)).unwrap();
    assert_eq!(
        prepared.writes().resulting_high_water(),
        before.high_water()
    );
    let result = commit(&mut reducer, prepared);
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::StaleValidation))
    ));
    assert!(result.publication.is_none() && result.effects.is_empty());
    assert_eq!(reducer.retained(), before);
}

#[test]
fn git_inventory_failed_last_allocation_and_quota_leave_all_marks_unchanged() {
    let base = empty();
    let mut exhausted_marks = base.high_water();
    exhausted_marks.worktree = u64::MAX;
    let exhausted = RetainedOrganization::new(
        base.organization().clone(),
        exhausted_marks,
        base.server_home().clone(),
    );
    let reducer = OrganizationReducer::from_recovered(
        exhausted.clone(),
        StoreRevision::new(0),
        ServerEpoch::new(9),
        OrganizationLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        reducer.prepare_git_inventory(&inventory_patch(None, None), &context(1)),
        Err(ReducerFailure::CounterExhausted)
    ));
    assert_eq!(reducer.retained(), exhausted);

    let mut reducer = OrganizationReducer::from_recovered(
        empty(),
        StoreRevision::new(0),
        ServerEpoch::new(9),
        OrganizationLimits {
            worktrees: 1,
            ..OrganizationLimits::default()
        },
    )
    .unwrap();
    let prepared = reducer
        .prepare_git_inventory(&inventory_patch(None, None), &context(1))
        .unwrap();
    commit(&mut reducer, prepared);
    let before = reducer.retained();
    let over_limit = inventory_patch(
        Some(RepositoryId::new(SERVER, 1).unwrap()),
        Some(FieldVersion::new(1)),
    );
    let prepared = reducer
        .prepare_git_inventory(&over_limit, &context(2))
        .unwrap();
    assert!(matches!(
        prepared.resolution(),
        Resolution::Completed(Outcome::Failed(OperationFailure::Capacity))
    ));
    assert_eq!(
        prepared.writes().resulting_high_water(),
        before.high_water()
    );
    let result = commit(&mut reducer, prepared);
    assert!(result.publication.is_none() && result.effects.is_empty());
    assert_eq!(reducer.retained(), before);
}

#[test]
fn git_inventory_unchanged_partial_reads_preserve_versions_and_directory_validation() {
    let mut base = reducer();
    let (workspace, _, _) = create(&mut base, 1);
    let mut reducer = OrganizationReducer::from_recovered(
        with_worktrees(base.retained()),
        StoreRevision::new(1),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let inventory = GitInventoryPatch {
        repository: Some(RepositoryId::new(SERVER, 1).unwrap()),
        directory: directory(b"/repo"),
        expected_inventory_version: Some(FieldVersion::new(1)),
        worktrees: vec![GitInventoryWorktree {
            id: Some(WorktreeId::new(SERVER, 1).unwrap()),
            directory: directory(b"/repo/main"),
            main: true,
            missing: false,
            listed: true,
        }],
    };
    let command = Command::CreateTab {
        workspace,
        source: TabCreationSource::Worktree(WorktreeId::new(SERVER, 1).unwrap()),
    };
    let validated = facts(&reducer, &command, 4);
    let before = reducer.retained();
    let snapshot = reducer.snapshot();
    for sequence in 2..=3 {
        let prepared = reducer
            .prepare_git_inventory(&inventory, &context(sequence))
            .unwrap();
        assert_eq!(
            prepared.writes().resulting_high_water(),
            before.high_water()
        );
        let result = commit(&mut reducer, prepared);
        assert!(matches!(
            result.resolution,
            Resolution::Completed(Outcome::Applied)
        ));
        assert!(result.publication.is_none() && result.effects.is_empty());
        assert_eq!(reducer.retained(), before);
        assert_eq!(reducer.snapshot(), snapshot);
    }
    let prepared = reducer.prepare_command(&command, &validated).unwrap();
    let result = commit(&mut reducer, prepared);
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::CreatedWithControl(_))
    ));
}

#[test]
fn git_inventory_reversed_read_order_preserves_versions_and_directory_validation() {
    let mut base = reducer();
    let (workspace, _, _) = create(&mut base, 1);
    let mut reducer = OrganizationReducer::from_recovered(
        with_worktrees(base.retained()),
        StoreRevision::new(1),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let inventory = GitInventoryPatch {
        repository: Some(RepositoryId::new(SERVER, 1).unwrap()),
        directory: directory(b"/repo"),
        expected_inventory_version: Some(FieldVersion::new(1)),
        worktrees: vec![
            GitInventoryWorktree {
                id: Some(WorktreeId::new(SERVER, 2).unwrap()),
                directory: directory(b"/repo/feature"),
                main: false,
                missing: false,
                listed: true,
            },
            GitInventoryWorktree {
                id: Some(WorktreeId::new(SERVER, 1).unwrap()),
                directory: directory(b"/repo/main"),
                main: true,
                missing: false,
                listed: true,
            },
        ],
    };
    let command = Command::CreateTab {
        workspace,
        source: TabCreationSource::Worktree(WorktreeId::new(SERVER, 2).unwrap()),
    };
    let validated = facts(&reducer, &command, 3);
    let before = reducer.retained();
    let snapshot = reducer.snapshot();
    let prepared = reducer
        .prepare_git_inventory(&inventory, &context(2))
        .unwrap();
    assert_eq!(
        prepared.writes().resulting_high_water(),
        before.high_water()
    );
    let result = commit(&mut reducer, prepared);
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::Applied)
    ));
    assert!(result.publication.is_none() && result.effects.is_empty());
    assert_eq!(reducer.retained(), before);
    assert_eq!(reducer.snapshot(), snapshot);
    let prepared = reducer.prepare_command(&command, &validated).unwrap();
    let result = commit(&mut reducer, prepared);
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::CreatedWithControl(_))
    ));
}

#[test]
fn git_inventory_unchanged_first_repository_preserves_versions_and_directory_validation() {
    let mut base = reducer();
    let (workspace, _, _) = create(&mut base, 1);
    let mut reducer = OrganizationReducer::from_recovered(
        with_worktrees(base.retained()),
        StoreRevision::new(1),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let second = GitInventoryPatch {
        repository: None,
        directory: directory(b"/other"),
        expected_inventory_version: None,
        worktrees: vec![GitInventoryWorktree {
            id: None,
            directory: directory(b"/other/main"),
            main: true,
            missing: false,
            listed: true,
        }],
    };
    let prepared = reducer.prepare_git_inventory(&second, &context(2)).unwrap();
    commit(&mut reducer, prepared);
    let inventory = GitInventoryPatch {
        repository: Some(RepositoryId::new(SERVER, 1).unwrap()),
        directory: directory(b"/repo"),
        expected_inventory_version: Some(FieldVersion::new(1)),
        worktrees: vec![],
    };
    let command = Command::CreateTab {
        workspace,
        source: TabCreationSource::Worktree(WorktreeId::new(SERVER, 1).unwrap()),
    };
    let validated = facts(&reducer, &command, 4);
    let before = reducer.retained();
    let snapshot = reducer.snapshot();
    let prepared = reducer
        .prepare_git_inventory(&inventory, &context(3))
        .unwrap();
    assert_eq!(
        prepared.writes().resulting_high_water(),
        before.high_water()
    );
    let result = commit(&mut reducer, prepared);
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::Applied)
    ));
    assert!(result.publication.is_none() && result.effects.is_empty());
    assert_eq!(reducer.retained(), before);
    assert_eq!(reducer.snapshot(), snapshot);
    let prepared = reducer.prepare_command(&command, &validated).unwrap();
    let result = commit(&mut reducer, prepared);
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::CreatedWithControl(_))
    ));
}

#[test]
fn git_inventory_existing_records_are_noops_until_availability_or_listing_changes() {
    let mut reducer = reducer();
    let prepared = reducer
        .prepare_git_inventory(&inventory_patch(None, None), &context(1))
        .unwrap();
    commit(&mut reducer, prepared);
    let before = reducer.retained();
    let mut inventory = inventory_patch(
        Some(RepositoryId::new(SERVER, 1).unwrap()),
        Some(FieldVersion::new(1)),
    );
    inventory.worktrees[0].id = Some(WorktreeId::new(SERVER, 1).unwrap());
    inventory.worktrees[0].main = true;
    let prepared = reducer
        .prepare_git_inventory(&inventory, &context(2))
        .unwrap();
    let result = commit(&mut reducer, prepared);
    assert_eq!(reducer.retained(), before);
    assert!(result.publication.is_none() && result.effects.is_empty());
    inventory.worktrees[0].missing = true;
    inventory.worktrees[0].listed = false;
    let snapshot = reducer.snapshot();
    let prepared = reducer
        .prepare_git_inventory(&inventory, &context(3))
        .unwrap();
    let result = commit(&mut reducer, prepared);
    let after = reducer.retained();
    assert_eq!(after.high_water(), before.high_water());
    assert!(after.organization().worktrees[0].missing);
    let facts = after.organization().retained.as_ref().unwrap();
    assert!(!facts.worktree_inventory()[0].listed());
    assert_eq!(
        facts.worktree_inventory()[0].version(),
        FieldVersion::new(2)
    );
    assert_eq!(
        facts.repository_inventory_versions()[0].version(),
        FieldVersion::new(2)
    );
    assert_eq!(
        replay(snapshot, result.publication.as_ref().unwrap()),
        reducer.snapshot()
    );
}
