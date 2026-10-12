use super::support::*;

fn assess(
    reducer: &mut OrganizationReducer,
    target: AssessmentTarget,
    sequence: u64,
) -> CloseAssessment {
    let command = match target {
        AssessmentTarget::Close(target) => Command::AssessClose { target },
        AssessmentTarget::KillWorkspace(workspace) => Command::AssessKillWorkspace { workspace },
        AssessmentTarget::EndAllSessions(server) => Command::AssessEndAllSessions { server },
        AssessmentTarget::RemoveWorktree { .. } => panic!("external assessment needs Git policy"),
    };
    let before = reducer.snapshot();
    let committed = apply(reducer, command, sequence);
    assert!(committed.publication.is_none());
    assert!(committed.effects.is_empty());
    assert_eq!(reducer.snapshot(), before);
    match committed.resolution {
        Resolution::Completed(Outcome::Assessment(value)) => *value,
        _ => panic!("assessment did not complete"),
    }
}

#[test]
fn inactive_close_assessment_has_exact_counts_without_publication() {
    let mut reducer = reducer();
    let (_, tab, pane) = create(&mut reducer, 1);
    apply(
        &mut reducer,
        Command::CreatePane {
            tab,
            source: pane,
            axis: SplitAxis::Vertical,
            edge: SplitEdge::Before,
        },
        2,
    );
    let target = AssessmentTarget::Close(CloseTarget::Tab(tab));
    let value = assess(&mut reducer, target, 3);
    let summary = value.summary();
    assert_eq!(summary.target(), target);
    assert_eq!(summary.workspace_count(), 1);
    assert_eq!(summary.tab_count(), 1);
    assert_eq!(summary.pane_count(), 2);
    assert_eq!(summary.live_session_count(), 0);
    assert!(!summary.requires_confirmation());
    assert_eq!(value.reference().operation, context(3).operation);
    assert_eq!(value.reference().digest, context(3).request_digest);
    assert_eq!(value.epoch(), reducer.snapshot().epoch());
}

#[test]
fn confirmed_close_uses_one_cascade_and_leaves_empty_organization() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let target = CloseTarget::Pane(pane);
    let value = assess(&mut reducer, AssessmentTarget::Close(target), 2);
    let before = reducer.snapshot();
    let marks = reducer.retained().high_water();
    let command = Command::CloseConfirmed {
        target,
        assessment: value.reference(),
    };
    let prepared = reducer.prepare_command(&command, &context(3)).unwrap();
    assert_eq!(reducer.snapshot(), before);
    assert_eq!(
        prepared.resulting_organization_revision(),
        before.state().revision.checked_successor().unwrap()
    );
    let result = commit(&mut reducer, prepared);
    assert_eq!(result.resolution, Resolution::Completed(Outcome::Applied));
    let delta = result.publication.as_ref().unwrap();
    assert_eq!(replay(before, delta), reducer.snapshot());
    assert!(reducer.snapshot().state().workspaces.is_empty());
    assert!(reducer.snapshot().state().tabs.is_empty());
    assert!(reducer.snapshot().state().panes.is_empty());
    assert_eq!(reducer.retained().high_water(), marks);
    assert_eq!(result.effects.len(), 2);
    assert!(
        matches!(result.effects[0], OrganizationEffect::CancelStartupIntents { pane: id } if id == pane)
    );
    assert!(
        matches!(result.effects[1], OrganizationEffect::InvalidatePaneRuntime { pane: id, clear_content: true, clear_approvals: true, .. } if id == pane)
    );
    let repeated = apply(&mut reducer, command, 4);
    assert_eq!(
        repeated.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::TargetDeleted))
    );
    assert!(repeated.publication.is_none() && repeated.effects.is_empty());
}

fn validate(
    reducer: &OrganizationReducer,
    value: CloseAssessment,
    now: u64,
) -> Result<(), AssessmentFailure> {
    reducer.validate_assessment(
        &context(1).principal,
        &value.reference(),
        &value.summary().target(),
        ReducerTick::new(now),
    )
}

#[test]
fn assessment_binds_owner_intent_epoch_and_deadline() {
    let mut reducer = reducer();
    let (workspace, _, pane) = create(&mut reducer, 1);
    let value = assess(
        &mut reducer,
        AssessmentTarget::Close(CloseTarget::Pane(pane)),
        2,
    );
    assert_eq!(validate(&reducer, value, 10), Ok(()));
    assert_eq!(validate(&reducer, value, 60_009), Ok(()));
    assert_eq!(
        validate(&reducer, value, 60_010),
        Err(AssessmentFailure::AssessmentExpired)
    );
    assert_eq!(
        validate(&reducer, value, 9),
        Err(AssessmentFailure::StaleAssessment)
    );
    let mut principal = context(1).principal;
    principal.client = ClientId::new(SERVER, 2).unwrap();
    assert_eq!(
        reducer.validate_assessment(
            &principal,
            &value.reference(),
            &value.summary().target(),
            ReducerTick::new(10)
        ),
        Err(AssessmentFailure::StaleAssessment)
    );
    principal = context(1).principal;
    principal.pairing = PairingId::new(SERVER, 2).unwrap();
    assert_eq!(
        reducer.validate_assessment(
            &principal,
            &value.reference(),
            &value.summary().target(),
            ReducerTick::new(10)
        ),
        Err(AssessmentFailure::StaleAssessment)
    );
    assert_eq!(
        reducer.validate_assessment(
            &context(1).principal,
            &value.reference(),
            &AssessmentTarget::KillWorkspace(workspace),
            ReducerTick::new(10)
        ),
        Err(AssessmentFailure::StaleAssessment)
    );
    let mut handle = value.reference();
    handle.digest = RequestDigest::from_bytes([9; 32]);
    assert_eq!(
        reducer.validate_assessment(
            &context(1).principal,
            &handle,
            &value.summary().target(),
            ReducerTick::new(10)
        ),
        Err(AssessmentFailure::StaleAssessment)
    );
    let restored = OrganizationReducer::from_recovered(
        reducer.retained(),
        reducer.store_revision(),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    assert_eq!(
        validate(&restored, value, 10),
        Err(AssessmentFailure::StaleAssessment)
    );
}

#[test]
fn new_pane_in_assessed_tab_invalidates_close_even_when_only_one_pane_is_targeted() {
    for whole_tab in [false, true] {
        let mut reducer = reducer();
        let (_, tab, pane) = create(&mut reducer, 1);
        let target = if whole_tab {
            CloseTarget::Tab(tab)
        } else {
            CloseTarget::Pane(pane)
        };
        let value = assess(&mut reducer, AssessmentTarget::Close(target), 2);
        apply(
            &mut reducer,
            Command::CreatePane {
                tab,
                source: pane,
                axis: SplitAxis::Horizontal,
                edge: SplitEdge::After,
            },
            3,
        );
        let before = reducer.snapshot();
        let result = apply(
            &mut reducer,
            Command::CloseConfirmed {
                target,
                assessment: value.reference(),
            },
            4,
        );
        assert_eq!(
            result.resolution,
            Resolution::Completed(Outcome::Failed(OperationFailure::StaleAssessment))
        );
        assert!(result.publication.is_none() && result.effects.is_empty());
        assert_eq!(reducer.snapshot(), before);
    }
}

#[test]
fn new_tab_changes_final_child_cascade_and_invalidates_close() {
    let mut reducer = reducer();
    let (workspace, _, pane) = create(&mut reducer, 1);
    let value = assess(
        &mut reducer,
        AssessmentTarget::Close(CloseTarget::Pane(pane)),
        2,
    );
    apply(
        &mut reducer,
        Command::CreateTab {
            workspace,
            source: TabCreationSource::Pane(pane),
        },
        3,
    );
    assert_eq!(
        validate(&reducer, value, 10),
        Err(AssessmentFailure::StaleAssessment)
    );
}

#[test]
fn unrelated_workspace_change_and_field_only_edits_preserve_hierarchy_assessment() {
    let mut reducer = reducer();
    let (workspace, _, pane) = create(&mut reducer, 1);
    let value = assess(
        &mut reducer,
        AssessmentTarget::Close(CloseTarget::Pane(pane)),
        2,
    );
    let (other, _, other_pane) = create(&mut reducer, 3);
    apply(
        &mut reducer,
        Command::CreateTab {
            workspace: other,
            source: TabCreationSource::Pane(other_pane),
        },
        4,
    );
    apply(
        &mut reducer,
        Command::RenameWorkspace {
            workspace,
            expected_version: FieldVersion::new(1),
            draft: NameDraft::Custom(NameText::new("named".into()).unwrap()),
        },
        5,
    );
    apply(
        &mut reducer,
        Command::SetPinnedDirectory {
            workspace,
            expected_version: FieldVersion::new(1),
            value: Some(directory(b"/pin")),
        },
        6,
    );
    apply(
        &mut reducer,
        Command::RepairPaneDirectory {
            pane,
            expected_version: FieldVersion::new(1),
            value: directory(b"/repair"),
        },
        7,
    );
    assert_eq!(validate(&reducer, value, 10), Ok(()));
    let result = apply(
        &mut reducer,
        Command::CloseConfirmed {
            target: CloseTarget::Pane(pane),
            assessment: value.reference(),
        },
        8,
    );
    assert_eq!(result.resolution, Resolution::Completed(Outcome::Applied));
    assert_eq!(reducer.snapshot().state().workspaces.len(), 1);
    assert_eq!(reducer.snapshot().state().workspaces[0].id, other);
}

#[test]
fn assessment_is_published_to_registry_and_consumed_only_after_durable_acceptance() {
    let mut reducer = reducer();
    let (_, tab, pane) = create(&mut reducer, 1);
    apply(
        &mut reducer,
        Command::CreatePane {
            tab,
            source: pane,
            axis: SplitAxis::Horizontal,
            edge: SplitEdge::After,
        },
        2,
    );
    let command = Command::AssessClose {
        target: CloseTarget::Pane(pane),
    };
    let prepared = reducer.prepare_command(&command, &context(3)).unwrap();
    let value = match prepared.resolution() {
        Resolution::Completed(Outcome::Assessment(value)) => **value,
        _ => panic!("assessment did not complete"),
    };
    assert_eq!(
        validate(&reducer, value, 10),
        Err(AssessmentFailure::StaleAssessment)
    );
    commit(&mut reducer, prepared);
    assert_eq!(validate(&reducer, value, 10), Ok(()));
    let command = Command::CloseConfirmed {
        target: CloseTarget::Pane(pane),
        assessment: value.reference(),
    };
    let unapplied = reducer.prepare_command(&command, &context(4)).unwrap();
    drop(unapplied);
    assert_eq!(validate(&reducer, value, 10), Ok(()));
    let invalid = reducer.prepare_command(&command, &context(4)).unwrap();
    let receipt = CommitReceipt {
        store_revision: StoreRevision::new(999),
        organization_revision: invalid.resulting_organization_revision(),
    };
    assert!(matches!(
        reducer.apply_committed(invalid, &receipt),
        Err(ReducerFailure::InvalidCommitReceipt)
    ));
    assert_eq!(validate(&reducer, value, 10), Ok(()));
    let prepared = reducer.prepare_command(&command, &context(4)).unwrap();
    commit(&mut reducer, prepared);
    assert_eq!(
        validate(&reducer, value, 10),
        Err(AssessmentFailure::StaleAssessment)
    );
    assert_eq!(reducer.snapshot().state().panes.len(), 1);
    reducer
        .retained()
        .validate(OrganizationLimits::default())
        .unwrap();
}

#[test]
fn expired_historical_assessment_does_not_issue_new_authority() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let command = Command::AssessClose {
        target: CloseTarget::Pane(pane),
    };
    let value = assess(
        &mut reducer,
        AssessmentTarget::Close(CloseTarget::Pane(pane)),
        2,
    );
    let mut retry = context(2);
    retry.now = ReducerTick::new(60_011);
    assert!(matches!(
        reducer.prepare_command(&command, &retry),
        Err(ReducerFailure::InvalidFacts)
    ));
    let mut close = context(3);
    close.now = retry.now;
    let result = reducer
        .prepare_command(
            &Command::CloseConfirmed {
                target: CloseTarget::Pane(pane),
                assessment: value.reference(),
            },
            &close,
        )
        .unwrap();
    assert_eq!(
        result.resolution(),
        &Resolution::Completed(Outcome::Failed(OperationFailure::AssessmentExpired))
    );
    assert_eq!(
        validate(&reducer, value, 60_011),
        Err(AssessmentFailure::AssessmentExpired)
    );
}

#[test]
fn assessment_limits_prune_expired_records_without_refreshing_handles() {
    for dimension in 0..3 {
        let mut limits = OrganizationLimits::default();
        match dimension {
            0 => limits.assessments = 1,
            1 => limits.assessments_per_client = 1,
            _ => limits.assessment_bytes = 1_000,
        }
        let mut reducer = OrganizationReducer::from_recovered(
            empty(),
            StoreRevision::new(0),
            ServerEpoch::new(9),
            limits,
        )
        .unwrap();
        let (_, _, pane) = create(&mut reducer, 1);
        let value = assess(
            &mut reducer,
            AssessmentTarget::Close(CloseTarget::Pane(pane)),
            2,
        );
        let rejected = apply(
            &mut reducer,
            Command::AssessClose {
                target: CloseTarget::Pane(pane),
            },
            3,
        );
        assert_eq!(
            rejected.resolution,
            Resolution::Completed(Outcome::Failed(OperationFailure::Capacity))
        );
        assert!(rejected.publication.is_none() && rejected.effects.is_empty());
        assert_eq!(validate(&reducer, value, 10), Ok(()));
        let mut later = context(4);
        later.now = ReducerTick::new(60_010);
        let prepared = reducer
            .prepare_command(
                &Command::AssessClose {
                    target: CloseTarget::Pane(pane),
                },
                &later,
            )
            .unwrap();
        let newer = match prepared.resolution() {
            Resolution::Completed(Outcome::Assessment(value)) => **value,
            _ => panic!("expired record did not release capacity"),
        };
        commit(&mut reducer, prepared);
        assert_eq!(
            validate(&reducer, value, 60_010),
            Err(AssessmentFailure::StaleAssessment)
        );
        assert_eq!(validate(&reducer, newer, 60_010), Ok(()));
    }
}

#[test]
fn assessment_registry_transition_invalidates_prepared_close_without_publication() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let value = assess(
        &mut reducer,
        AssessmentTarget::Close(CloseTarget::Pane(pane)),
        2,
    );
    let prepared = reducer
        .prepare_command(
            &Command::CloseConfirmed {
                target: CloseTarget::Pane(pane),
                assessment: value.reference(),
            },
            &context(3),
        )
        .unwrap();
    assess(
        &mut reducer,
        AssessmentTarget::Close(CloseTarget::Pane(pane)),
        4,
    );
    let receipt = CommitReceipt {
        store_revision: reducer.store_revision().checked_successor().unwrap(),
        organization_revision: prepared.resulting_organization_revision(),
    };
    assert!(matches!(
        reducer.apply_committed(prepared, &receipt),
        Err(ReducerFailure::StalePreparation)
    ));
    assert_eq!(validate(&reducer, value, 10), Ok(()));
}

#[test]
fn kill_includes_unattached_work_and_leaves_repository_records_unmodified() {
    let mut reducer = OrganizationReducer::from_recovered(
        with_worktrees(empty()),
        StoreRevision::new(0),
        ServerEpoch::new(9),
        OrganizationLimits::default(),
    )
    .unwrap();
    let (workspace, _, pane) = create(&mut reducer, 1);
    apply(
        &mut reducer,
        Command::CreateTab {
            workspace,
            source: TabCreationSource::Worktree(WorktreeId::new(SERVER, 2).unwrap()),
        },
        2,
    );
    let (other, _, _) = create(&mut reducer, 3);
    let value = assess(&mut reducer, AssessmentTarget::KillWorkspace(workspace), 4);
    assert!(value.summary().requires_confirmation());
    assert_eq!(value.summary().tab_count(), 2);
    assert_eq!(value.summary().pane_count(), 2);
    let before = reducer.snapshot();
    let result = apply(
        &mut reducer,
        Command::KillWorkspaceConfirmed {
            workspace,
            assessment: value.reference(),
        },
        5,
    );
    assert_eq!(
        replay(before.clone(), result.publication.as_ref().unwrap()),
        reducer.snapshot()
    );
    assert_eq!(
        reducer.snapshot().state().repositories,
        before.state().repositories
    );
    assert_eq!(
        reducer.snapshot().state().worktrees,
        before.state().worktrees
    );
    assert_eq!(reducer.snapshot().state().workspaces[0].id, other);
    assert!(
        result
            .effects
            .iter()
            .all(|v| !matches!(v, OrganizationEffect::RegisterCreatedPane { .. }))
    );
    let rejected = apply(
        &mut reducer,
        Command::RenameWorkspace {
            workspace,
            expected_version: FieldVersion::new(1),
            draft: NameDraft::Automatic,
        },
        6,
    );
    assert_eq!(
        rejected.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::TargetDeleted))
    );
    let rejected = apply(
        &mut reducer,
        Command::CloseConfirmed {
            target: CloseTarget::Pane(pane),
            assessment: value.reference(),
        },
        7,
    );
    assert_eq!(
        rejected.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::TargetDeleted))
    );
}

#[test]
fn kill_and_end_all_assessments_are_distinct_and_always_require_confirmation() {
    let mut reducer = reducer();
    let (workspace, _, _) = create(&mut reducer, 1);
    let kill = assess(&mut reducer, AssessmentTarget::KillWorkspace(workspace), 2);
    let end_all = assess(&mut reducer, AssessmentTarget::EndAllSessions(SERVER), 3);
    assert!(kill.summary().requires_confirmation() && end_all.summary().requires_confirmation());
    let before = reducer.snapshot();
    let wrong = apply(
        &mut reducer,
        Command::KillWorkspaceConfirmed {
            workspace,
            assessment: end_all.reference(),
        },
        4,
    );
    assert_eq!(
        wrong.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::StaleAssessment))
    );
    assert_eq!(reducer.snapshot(), before);
    assert_eq!(validate(&reducer, kill, 10), Ok(()));
}

#[test]
fn git_fallback_reservation_blocks_ordinary_deletion_until_settlement_or_abandonment() {
    let mut reducer = OrganizationReducer::from_recovered(
        with_worktrees(empty()),
        StoreRevision::new(0),
        ServerEpoch::new(9),
        OrganizationLimits::default(),
    )
    .unwrap();
    let (workspace, _, pane) = create(&mut reducer, 1);
    let value = assess(
        &mut reducer,
        AssessmentTarget::Close(CloseTarget::Pane(pane)),
        2,
    );
    let prepared = reducer
        .prepare_git_allocation(
            &GitAllocationRequest {
                repository: false,
                worktree: false,
                fallbacks: vec![spaceterm_core::worktrees::intent::WorkspaceFallbackPlan {
                    workspace,
                    directory: directory(b"/fallback"),
                    worktree: WorktreeId::new(SERVER, 1).unwrap(),
                }],
            },
            &context(3),
        )
        .unwrap();
    commit(&mut reducer, prepared);
    let before = reducer.snapshot();
    let rejected = apply(
        &mut reducer,
        Command::CloseConfirmed {
            target: CloseTarget::Pane(pane),
            assessment: value.reference(),
        },
        4,
    );
    assert_eq!(
        rejected.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
    );
    assert_eq!(reducer.snapshot(), before);
    let rejected = apply(&mut reducer, Command::AssessKillWorkspace { workspace }, 5);
    assert_eq!(
        rejected.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
    );
    let abandoned = reducer
        .prepare_abandon_git_allocation(context(3).binding())
        .unwrap();
    commit(&mut reducer, abandoned);
    assert_eq!(validate(&reducer, value, 10), Ok(()));
}

#[test]
fn assessment_deadline_and_deletion_generation_exhaustion_have_no_effects() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let before = reducer.retained();
    let mut late = context(2);
    late.now = ReducerTick::new(u64::MAX);
    assert!(matches!(
        reducer.prepare_command(
            &Command::AssessClose {
                target: CloseTarget::Pane(pane)
            },
            &late
        ),
        Err(ReducerFailure::CounterExhausted)
    ));
    assert_eq!(reducer.retained(), before);
    let mut org = before.organization().clone();
    let mut retained_facts = org.retained.as_ref().unwrap().parts().clone();
    retained_facts.pane_generations[0] = PaneGenerationHighWater::new(
        pane,
        SessionGeneration::new(u64::MAX),
        ControlGeneration::new(1),
    );
    org.retained = Some(OrganizationRetainedFacts::new(retained_facts).unwrap());
    let mut reducer = OrganizationReducer::from_recovered(
        RetainedOrganization::new(org, before.high_water(), before.server_home().clone()),
        reducer.store_revision(),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let value = assess(
        &mut reducer,
        AssessmentTarget::Close(CloseTarget::Pane(pane)),
        3,
    );
    let before = reducer.snapshot();
    assert!(matches!(
        reducer.prepare_command(
            &Command::CloseConfirmed {
                target: CloseTarget::Pane(pane),
                assessment: value.reference()
            },
            &context(4)
        ),
        Err(ReducerFailure::CounterExhausted)
    ));
    assert_eq!(reducer.snapshot(), before);
    assert_eq!(validate(&reducer, value, 10), Ok(()));
}

#[test]
fn worktree_tabs_close_is_workspace_scoped_and_preserves_repository_inventory() {
    let mut reducer = OrganizationReducer::from_recovered(
        with_worktrees(empty()),
        StoreRevision::new(0),
        ServerEpoch::new(9),
        OrganizationLimits::default(),
    )
    .unwrap();
    let (workspace, _, pane) = create(&mut reducer, 1);
    let (other, _, _) = create(&mut reducer, 2);
    let worktree = WorktreeId::new(SERVER, 2).unwrap();
    for (owner, sequence) in [(workspace, 3), (workspace, 4), (other, 5)] {
        apply(
            &mut reducer,
            Command::CreateTab {
                workspace: owner,
                source: TabCreationSource::Worktree(worktree),
            },
            sequence,
        );
    }
    let target = CloseTarget::WorktreeTabs {
        workspace,
        worktree,
    };
    let value = assess(&mut reducer, AssessmentTarget::Close(target), 6);
    assert_eq!(value.summary().tab_count(), 2);
    assert_eq!(value.summary().pane_count(), 2);
    let before = reducer.snapshot();
    let result = apply(
        &mut reducer,
        Command::CloseConfirmed {
            target,
            assessment: value.reference(),
        },
        7,
    );
    assert_eq!(
        replay(before.clone(), result.publication.as_ref().unwrap()),
        reducer.snapshot()
    );
    assert!(
        reducer
            .snapshot()
            .state()
            .panes
            .iter()
            .any(|v| v.id == pane)
    );
    assert_eq!(
        reducer
            .snapshot()
            .state()
            .tabs
            .iter()
            .filter(|v| v.worktree == Some(worktree))
            .count(),
        1
    );
    assert_eq!(
        reducer.snapshot().state().worktrees,
        before.state().worktrees
    );
    assert_eq!(
        reducer.snapshot().state().repositories,
        before.state().repositories
    );
}

#[test]
fn worktree_membership_change_invalidates_assessment_without_runtime_change() {
    let mut reducer = OrganizationReducer::from_recovered(
        with_worktrees(empty()),
        StoreRevision::new(0),
        ServerEpoch::new(9),
        OrganizationLimits::default(),
    )
    .unwrap();
    let (_, tab, pane) = create(&mut reducer, 1);
    let value = assess(
        &mut reducer,
        AssessmentTarget::Close(CloseTarget::Pane(pane)),
        2,
    );
    let patch = WorktreeOrganizationPatch {
        binding: context(3).binding(),
        scope: RepositoryScope {
            server: SERVER,
            repository: RepositoryId::new(SERVER, 1).unwrap(),
        },
        expected_inventory_version: Some(FieldVersion::new(1)),
        repositories: vec![],
        worktrees: vec![],
        memberships: vec![WorktreeMembershipEdit {
            tab,
            expected_version: FieldVersion::new(1),
            worktree: Some(WorktreeId::new(SERVER, 2).unwrap()),
        }],
        settlement: None,
    };
    let prepared = reducer
        .prepare_event(&OrganizationEvent::WorktreeOrganizationChanged(Box::new(
            patch,
        )))
        .unwrap();
    commit(&mut reducer, prepared);
    assert_eq!(
        validate(&reducer, value, 10),
        Err(AssessmentFailure::StaleAssessment)
    );
}

#[test]
fn assessment_client_quota_counts_distinct_streams_of_the_same_stable_client() {
    let limits = OrganizationLimits {
        assessments_per_client: 1,
        ..OrganizationLimits::default()
    };
    let mut reducer = OrganizationReducer::from_recovered(
        empty(),
        StoreRevision::new(0),
        ServerEpoch::new(9),
        limits,
    )
    .unwrap();
    let (_, _, pane) = create(&mut reducer, 1);
    let value = assess(
        &mut reducer,
        AssessmentTarget::Close(CloseTarget::Pane(pane)),
        2,
    );
    let mut caller = context(1);
    caller.operation = OperationId::new(
        OperationStreamId::new(
            OperationStreamKey::new(SERVER, 2).unwrap(),
            caller.principal.pairing,
            caller.principal.client,
        )
        .unwrap(),
        OperationSequence::new(1).unwrap(),
    );
    let prepared = reducer
        .prepare_command(
            &Command::AssessClose {
                target: CloseTarget::Pane(pane),
            },
            &caller,
        )
        .unwrap();
    let result = commit(&mut reducer, prepared);
    assert_eq!(
        result.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::Capacity))
    );
    assert_eq!(validate(&reducer, value, 10), Ok(()));
    caller.principal.client = ClientId::new(SERVER, 2).unwrap();
    caller.operation = OperationId::new(
        OperationStreamId::new(
            OperationStreamKey::new(SERVER, 3).unwrap(),
            caller.principal.pairing,
            caller.principal.client,
        )
        .unwrap(),
        OperationSequence::new(1).unwrap(),
    );
    let prepared = reducer
        .prepare_command(
            &Command::AssessClose {
                target: CloseTarget::Pane(pane),
            },
            &caller,
        )
        .unwrap();
    let result = commit(&mut reducer, prepared);
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::Assessment(_))
    ));
}

#[test]
fn confirmed_source_deletion_preserves_root_succession_and_surviving_default() {
    let mut reducer = reducer();
    let (_, tab, a) = create(&mut reducer, 1);
    apply(
        &mut reducer,
        Command::CreatePane {
            tab,
            source: a,
            axis: SplitAxis::Horizontal,
            edge: SplitEdge::After,
        },
        2,
    );
    let b = reducer
        .snapshot()
        .state()
        .panes
        .iter()
        .find(|v| v.id != a)
        .unwrap()
        .id;
    apply(
        &mut reducer,
        Command::CreatePane {
            tab,
            source: b,
            axis: SplitAxis::Vertical,
            edge: SplitEdge::After,
        },
        3,
    );
    let c = reducer
        .snapshot()
        .state()
        .panes
        .iter()
        .find(|v| v.id != a && v.id != b)
        .unwrap()
        .id;
    apply(
        &mut reducer,
        Command::CreatePane {
            tab,
            source: a,
            axis: SplitAxis::Vertical,
            edge: SplitEdge::After,
        },
        4,
    );
    let d = reducer
        .snapshot()
        .state()
        .panes
        .iter()
        .find(|v| v.id != a && v.id != b && v.id != c)
        .unwrap()
        .id;
    let value = assess(
        &mut reducer,
        AssessmentTarget::Close(CloseTarget::Pane(a)),
        5,
    );
    let before = reducer.snapshot();
    let result = apply(
        &mut reducer,
        Command::CloseConfirmed {
            target: CloseTarget::Pane(a),
            assessment: value.reference(),
        },
        6,
    );
    assert_eq!(
        replay(before, result.publication.as_ref().unwrap()),
        reducer.snapshot()
    );
    let snapshot = reducer.snapshot();
    assert_eq!(snapshot.state().tabs[0].root_pane, b);
    let default = snapshot
        .state()
        .retained
        .as_ref()
        .unwrap()
        .creation_defaults()
        .iter()
        .find(|v| v.tab() == tab)
        .unwrap();
    assert_eq!(default.replay_order(), &[d, b, c]);
    assert_eq!(
        snapshot
            .state()
            .panes
            .iter()
            .find(|v| v.id == b)
            .unwrap()
            .creation,
        PaneCreation::SplitOf {
            source: d,
            axis: SplitAxis::Horizontal,
            edge: SplitEdge::After
        }
    );
    assert_eq!(
        snapshot
            .state()
            .panes
            .iter()
            .find(|v| v.id == c)
            .unwrap()
            .creation,
        PaneCreation::SplitOf {
            source: b,
            axis: SplitAxis::Vertical,
            edge: SplitEdge::After
        }
    );
    reducer
        .retained()
        .validate(OrganizationLimits::default())
        .unwrap();
}
