use super::support::*;

#[test]
fn stale_preparation_or_wrong_receipt_cannot_be_applied() {
    let mut reducer = reducer();
    let (workspace, _, _) = create(&mut reducer, 1);
    let command = Command::RenameWorkspace {
        workspace,
        expected_version: FieldVersion::new(1),
        draft: NameDraft::Custom(NameText::new("draft".into()).unwrap()),
    };
    let prepared = reducer.prepare_command(&command, &context(2)).unwrap();
    let before = reducer.snapshot();
    let receipt = CommitReceipt {
        store_revision: StoreRevision::new(9),
        organization_revision: prepared.resulting_organization_revision(),
    };
    assert!(matches!(
        reducer.apply_committed(prepared, &receipt),
        Err(ReducerFailure::InvalidCommitReceipt)
    ));
    assert_eq!(reducer.snapshot(), before);
    let stale = reducer.prepare_command(&command, &context(3)).unwrap();
    reducer
        .observe_ledger_commit(&CommitReceipt {
            store_revision: StoreRevision::new(2),
            organization_revision: OrganizationRevision::new(1),
        })
        .unwrap();
    let receipt = CommitReceipt {
        store_revision: StoreRevision::new(3),
        organization_revision: stale.resulting_organization_revision(),
    };
    assert!(matches!(
        reducer.apply_committed(stale, &receipt),
        Err(ReducerFailure::StalePreparation)
    ));
}
#[test]
fn ledger_only_commit_does_not_change_organization_or_field_versions() {
    let mut reducer = reducer();
    create(&mut reducer, 1);
    let before = reducer.snapshot();
    reducer
        .observe_ledger_commit(&CommitReceipt {
            store_revision: StoreRevision::new(2),
            organization_revision: before.state().revision,
        })
        .unwrap();
    assert_eq!(reducer.snapshot(), before);
    assert_eq!(reducer.store_revision(), StoreRevision::new(2));
}
#[test]
fn not_applied_and_indeterminate_never_publish_or_consume_allocations() {
    let mut reducer = reducer();
    let command = Command::CreateWorkspace {
        server: SERVER,
        source: WorkspaceCreationSource::ServerHome { server: SERVER },
        name: NameDraft::Automatic,
    };
    let before = reducer.retained();
    let prepared = reducer
        .prepare_command(&command, &facts(&reducer, &command, 1))
        .unwrap();
    drop(prepared);
    assert_eq!(reducer.retained(), before);
    reducer.suspend_for_recovery();
    assert!(matches!(
        reducer.prepare_command(&command, &facts(&reducer, &command, 2)),
        Err(ReducerFailure::RecoveryRequired)
    ));
    assert!(matches!(
        reducer.observe_ledger_commit(&CommitReceipt {
            store_revision: StoreRevision::new(1),
            organization_revision: OrganizationRevision::new(0)
        }),
        Err(ReducerFailure::RecoveryRequired)
    ));
    assert_eq!(reducer.retained(), before);
}
#[test]
fn creation_delta_preserves_original_split_placement_and_one_revision() {
    let mut reducer = reducer();
    let (_, tab, pane) = create(&mut reducer, 1);
    let result = apply(
        &mut reducer,
        Command::CreatePane {
            tab,
            source: pane,
            axis: SplitAxis::Vertical,
            edge: SplitEdge::Before,
        },
        2,
    );
    let delta = result.publication.unwrap();
    assert_eq!(
        (delta.base(), delta.revision()),
        (OrganizationRevision::new(1), OrganizationRevision::new(2))
    );
    assert_eq!(delta.accepted_creations().len(), 1);
    assert_eq!(
        (
            delta.accepted_creations()[0].source(),
            delta.accepted_creations()[0].edge()
        ),
        (pane, SplitEdge::Before)
    );
    assert_eq!(delta.runtime().len(), 1);
}
#[test]
fn reducer_errors_and_debug_views_do_not_leak_supplied_values() {
    let directory = directory(b"/private/fixture-value");
    let facts = DirectoryFact {
        directory,
        available: false,
        physically_inside: None,
    };
    assert!(!format!("{facts:?}").contains("fixture-value"));
    assert!(
        !format!(
            "{:?}",
            ReducerFailure::InvalidDurableState(DurableInvariant::Graph)
        )
        .contains("fixture-value")
    );
}
#[test]
fn restart_restores_ended_unclaimed_panes_without_launching() {
    let mut reducer = reducer();
    create(&mut reducer, 1);
    let retained = reducer.retained();
    let restored = OrganizationReducer::from_recovered(
        retained.clone(),
        StoreRevision::new(1),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    assert_eq!(restored.retained(), retained);
    assert_eq!(
        restored.snapshot().runtime()[0].phase,
        SessionPhase::Ended {
            reason: EndReason::ServerRestart
        }
    );
    assert_eq!(
        restored.snapshot().runtime()[0].control,
        ControlRecord::Unclaimed
    );
    assert_eq!(
        restored.snapshot().runtime()[0].control_generation,
        ControlGeneration::new(1)
    );
}
#[test]
fn write_projection_rejects_a_mismatched_transaction_without_partial_append() {
    let reducer = reducer();
    let command = Command::CreateWorkspace {
        server: SERVER,
        source: WorkspaceCreationSource::ServerHome { server: SERVER },
        name: NameDraft::Automatic,
    };
    let prepared = reducer
        .prepare_command(&command, &facts(&reducer, &command, 1))
        .unwrap();
    let mut parts = TransactionParts {
        server: SERVER,
        expected_store_revision: StoreRevision::new(0),
        resulting_store_revision: StoreRevision::new(1),
        expected_organization_revision: OrganizationRevision::new(0),
        resulting_organization_revision: OrganizationRevision::new(1),
        expected_epoch: ServerEpoch::new(9),
        resulting_epoch: ServerEpoch::new(9),
        expected_high_water: reducer.retained().high_water(),
        resulting_high_water: prepared.writes().resulting_high_water(),
        organization_writes: vec![],
        operation_writes: vec![],
    };
    parts.resulting_epoch = ServerEpoch::new(10);
    assert_eq!(
        prepared.writes().append_to(&mut parts),
        Err(ReducerFailure::InvalidFacts)
    );
    assert!(parts.organization_writes.is_empty() && parts.operation_writes.is_empty());
    parts.resulting_epoch = ServerEpoch::new(9);
    prepared.writes().append_to(&mut parts).unwrap();
    assert!(
        parts
            .operation_writes
            .iter()
            .any(|v| matches!(v, OperationWrite::ReductionBasis(_)))
    );
    assert!(
        parts
            .organization_writes
            .iter()
            .any(|v| matches!(v, OrganizationWrite::Retained(_)))
    );
}

#[test]
fn preparation_from_another_reducer_with_equal_counters_cannot_replace_state() {
    let mut origin = reducer();
    let (workspace, _, _) = create(&mut origin, 1);
    let retained = origin.retained();
    let mut first = OrganizationReducer::from_recovered(
        retained.clone(),
        StoreRevision::new(1),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let mut org = retained.organization().clone();
    org.workspaces[0].name.value = WorkspaceName {
        mode: NameMode::Custom,
        display: NameText::new("other".into()).unwrap(),
    };
    let changed =
        RetainedOrganization::new(org, retained.high_water(), retained.server_home().clone());
    let mut other = OrganizationReducer::from_recovered(
        changed,
        StoreRevision::new(1),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let command = Command::RenameWorkspace {
        workspace,
        expected_version: FieldVersion::new(1),
        draft: NameDraft::Custom(NameText::new("candidate".into()).unwrap()),
    };
    let prepared = first.prepare_command(&command, &context(2)).unwrap();
    let receipt = CommitReceipt {
        store_revision: StoreRevision::new(2),
        organization_revision: prepared.resulting_organization_revision(),
    };
    assert!(matches!(
        other.apply_committed(prepared, &receipt),
        Err(ReducerFailure::StalePreparation)
    ));
    let original = first.prepare_command(&command, &context(3)).unwrap();
    commit(&mut first, original);
}

#[test]
fn delta_replay_matches_authoritative_snapshot_for_generated_creation_and_fields() {
    let mut reducer = reducer();
    let mut replica = reducer.snapshot();
    let mut seed = 520u64;
    for step in 1..=120 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let snapshot = reducer.snapshot();
        let command = if snapshot.state().workspaces.is_empty() || seed.is_multiple_of(5) {
            Command::CreateWorkspace {
                server: SERVER,
                source: WorkspaceCreationSource::ServerHome { server: SERVER },
                name: NameDraft::Automatic,
            }
        } else {
            let workspace =
                &snapshot.state().workspaces[(seed as usize) % snapshot.state().workspaces.len()];
            let tab = snapshot
                .state()
                .tabs
                .iter()
                .find(|v| v.id == workspace.root_tab)
                .unwrap();
            match seed % 4 {
                0 => Command::CreateTab {
                    workspace: workspace.id,
                    source: TabCreationSource::Pane(tab.root_pane),
                },
                1 => Command::CreatePane {
                    tab: tab.id,
                    source: tab.root_pane,
                    axis: SplitAxis::Horizontal,
                    edge: SplitEdge::Before,
                },
                2 => Command::RenameWorkspace {
                    workspace: workspace.id,
                    expected_version: workspace.name.version,
                    draft: NameDraft::Custom(NameText::new(format!("name-{step}")).unwrap()),
                },
                _ => Command::SetPinnedDirectory {
                    workspace: workspace.id,
                    expected_version: workspace.pinned_directory.version,
                    value: Some(directory(b"/shared")),
                },
            }
        };
        let result = apply(&mut reducer, command, step);
        if let Some(delta) = result.publication {
            replica = replay(replica, &delta);
        }
        assert_eq!(replica, reducer.snapshot(), "numeric seed {seed}");
        reducer
            .retained()
            .validate(OrganizationLimits::default())
            .unwrap();
    }
}

#[test]
fn worktree_patch_retains_missing_referenced_records_and_rejects_stale_inventory() {
    let mut base = reducer();
    create(&mut base, 1);
    let retained = with_worktrees(base.retained());
    let mut org = retained.organization().clone();
    org.tabs[0].worktree = Some(WorktreeId::new(SERVER, 2).unwrap());
    let mut reducer = OrganizationReducer::from_recovered(
        RetainedOrganization::new(org, retained.high_water(), retained.server_home().clone()),
        StoreRevision::new(1),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let before = reducer.snapshot();
    let mut missing = before.state().worktrees[1].clone();
    missing.missing = true;
    let patch = WorktreeOrganizationPatch {
        binding: context(2).binding(),
        scope: RepositoryScope {
            server: SERVER,
            repository: missing.repository,
        },
        expected_inventory_version: Some(FieldVersion::new(1)),
        repositories: vec![],
        worktrees: vec![(missing.clone(), false)],
        memberships: vec![],
        settlement: None,
    };
    let event = OrganizationEvent::WorktreeOrganizationChanged(Box::new(patch));
    let prepared = reducer.prepare_event(&event).unwrap();
    let committed = commit(&mut reducer, prepared);
    assert_eq!(
        replay(before.clone(), committed.publication.as_ref().unwrap()),
        reducer.snapshot()
    );
    assert_eq!(reducer.snapshot().state().tabs, before.state().tabs);
    assert_eq!(reducer.snapshot().state().worktrees[1], missing);
    let repeated = reducer.prepare_event(&event).unwrap();
    assert!(matches!(
        repeated.resolution(),
        Resolution::Completed(Outcome::Failed(OperationFailure::StaleValidation))
    ));
}

#[test]
fn observed_principal_allocation_allows_new_clients_without_minting_organization_ids() {
    let mut reducer = reducer();
    let before = reducer.snapshot();
    let mut marks = reducer.retained().high_water();
    marks.client = 9;
    marks.pairing = 9;
    marks.operation_stream = 9;
    let receipt = CommitReceipt {
        store_revision: StoreRevision::new(1),
        organization_revision: before.state().revision,
    };
    let mut invalid = marks;
    invalid.pane = 1;
    assert_eq!(
        reducer.observe_principal_commit(&receipt, invalid),
        Err(ReducerFailure::InvalidFacts)
    );
    assert_eq!(reducer.store_revision(), StoreRevision::new(0));
    reducer.observe_principal_commit(&receipt, marks).unwrap();
    assert_eq!(reducer.snapshot(), before);
    let command = Command::CreateWorkspace {
        server: SERVER,
        source: WorkspaceCreationSource::ServerHome { server: SERVER },
        name: NameDraft::Automatic,
    };
    let mut context = facts(&reducer, &command, 1);
    context.principal = MutationPrincipal {
        pairing: PairingId::new(SERVER, 9).unwrap(),
        client: ClientId::new(SERVER, 9).unwrap(),
    };
    context.operation = OperationId::new(
        OperationStreamId::new(
            OperationStreamKey::new(SERVER, 9).unwrap(),
            context.principal.pairing,
            context.principal.client,
        )
        .unwrap(),
        OperationSequence::new(1).unwrap(),
    );
    let binding = context.binding();
    context.facts.directories.as_mut().unwrap().binding = binding;
    let prepared = reducer.prepare_command(&command, &context).unwrap();
    commit(&mut reducer, prepared);
    assert_eq!(
        reducer.snapshot().runtime()[0].control,
        ControlRecord::Held {
            client: context.principal.client,
            generation: ControlGeneration::new(1)
        }
    );
}

#[test]
fn exhausted_store_organization_and_creation_counters_reject_before_effects() {
    let command = Command::CreateWorkspace {
        server: SERVER,
        source: WorkspaceCreationSource::ServerHome { server: SERVER },
        name: NameDraft::Automatic,
    };
    for dimension in 0..3 {
        let mut retained = empty();
        let mut marks = retained.high_water();
        let mut org = retained.organization().clone();
        let store_revision = if dimension == 0 {
            StoreRevision::new(u64::MAX)
        } else {
            StoreRevision::new(0)
        };
        if dimension == 1 {
            org.revision = OrganizationRevision::new(u64::MAX);
        }
        if dimension == 2 {
            marks.creation_ordinal = u64::MAX - 2;
        }
        retained = RetainedOrganization::new(org, marks, retained.server_home().clone());
        let reducer = OrganizationReducer::from_recovered(
            retained.clone(),
            store_revision,
            ServerEpoch::new(1),
            OrganizationLimits::default(),
        )
        .unwrap();
        assert!(matches!(
            reducer.prepare_command(&command, &facts(&reducer, &command, 1)),
            Err(ReducerFailure::CounterExhausted)
        ));
        assert_eq!(reducer.retained(), retained);
    }
}
