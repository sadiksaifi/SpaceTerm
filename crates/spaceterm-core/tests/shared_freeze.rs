#[expect(
    dead_code,
    reason = "shared Wave 1 command fixture is unused by this constructor suite"
)]
mod support;

use spaceterm_core::authority::*;
use spaceterm_core::control::*;
use spaceterm_core::geometry::*;
use spaceterm_core::identity::*;
use spaceterm_core::observations::*;
use spaceterm_core::operation_ledger_contracts::*;
use spaceterm_core::operations::*;
use spaceterm_core::organization::*;
use spaceterm_core::organization_retained::*;
use spaceterm_core::pairing::*;
use spaceterm_core::remote_access::*;
use spaceterm_core::storage::*;
use spaceterm_core::worktrees::*;
use support::{operation, organization, server};

fn session() -> SessionKey {
    SessionKey {
        epoch: ServerEpoch::new(3),
        pane: PaneId::new(server(), 1).unwrap(),
        generation: SessionGeneration::new(2),
    }
}
fn grant() -> ControlGrant {
    ControlGrant::new(
        session().pane,
        operation(1).stream().client(),
        ControlGeneration::new(7),
    )
    .unwrap()
}
fn parts() -> TransactionParts {
    TransactionParts {
        server: server(),
        expected_store_revision: StoreRevision::new(1),
        resulting_store_revision: StoreRevision::new(2),
        expected_organization_revision: OrganizationRevision::new(1),
        resulting_organization_revision: OrganizationRevision::new(1),
        expected_epoch: ServerEpoch::new(3),
        resulting_epoch: ServerEpoch::new(3),
        expected_high_water: IdentityHighWater::default(),
        resulting_high_water: IdentityHighWater::default(),
        organization_writes: vec![],
        operation_writes: vec![],
    }
}

#[test]
fn original_grant_epoch_survives_outcome_serialization() {
    let original = GrantedControl::new(ServerEpoch::new(3), grant()).unwrap();
    let outcome = Outcome::ControlGranted(original);
    let recovered: Outcome =
        serde_json::from_slice(&serde_json::to_vec(&outcome).unwrap()).unwrap();
    assert_eq!(recovered.grant_provenance(), Some(original));
}
#[test]
fn legacy_grant_result_never_acquires_a_guessed_epoch() {
    assert_eq!(Outcome::ControlAcquired(grant()).grant_provenance(), None);
}
#[test]
fn actual_grant_provenance_rejects_comparison_zero() {
    let zero =
        ControlGrant::new(session().pane, grant().client(), ControlGeneration::new(0)).unwrap();
    assert_eq!(
        GrantedControl::new(ServerEpoch::new(3), zero),
        Err(FenceError::WrongBinding)
    );
}
#[test]
fn deserialization_cannot_bypass_actual_grant_epoch_validation() {
    let original = GrantedControl::new(ServerEpoch::new(3), grant()).unwrap();
    let mut json = serde_json::to_value(original).unwrap();
    json["epoch"] = 0.into();
    assert!(serde_json::from_value::<GrantedControl>(json).is_err());
}
#[test]
fn creation_result_cannot_release_a_different_created_pane() {
    let control = GrantedControl::new(ServerEpoch::new(3), grant()).unwrap();
    assert_eq!(
        CreatedOrganization::new(
            CreatedEntities::Pane(PaneId::new(server(), 2).unwrap()),
            control
        ),
        Err(RequestError::WrongScope)
    );
}
#[test]
fn stale_retained_work_does_not_become_fresh_on_delivery() {
    let observed = CloseWorkObservation::new(
        session(),
        RuntimeObservationSequence::new(1).unwrap(),
        CloseWorkGeneration::new(0),
        MonotonicTick::new(100),
        CloseWorkState::Idle,
    )
    .unwrap();
    assert!(!observed.is_fresh(MonotonicTick::new(200), 50));
}
#[test]
fn future_work_tick_is_not_fresh() {
    let observed = CloseWorkObservation::new(
        session(),
        RuntimeObservationSequence::new(1).unwrap(),
        CloseWorkGeneration::new(0),
        MonotonicTick::new(100),
        CloseWorkState::Idle,
    )
    .unwrap();
    assert!(!observed.is_fresh(MonotonicTick::new(99), u64::MAX));
}
#[test]
fn directory_confirmation_rejects_foreign_server_scope() {
    let directory = ServerDirectory::new(ServerId::new([8; 16]), b"/work".to_vec()).unwrap();
    assert_eq!(
        ConfirmedDirectoryObservation::new(
            session(),
            RuntimeObservationSequence::new(1).unwrap(),
            MonotonicTick::new(100),
            directory
        ),
        Err(ObservationError::WrongSession)
    );
}
#[test]
fn remote_enablement_rejects_ninth_endpoint() {
    let endpoints = (1..=9)
        .map(|port| format!("127.0.0.1:{port}").parse().unwrap())
        .collect();
    assert_eq!(
        RemoteAccessPolicy::enabled(server(), endpoints),
        Err(RemoteAccessError::TooManyEndpoints)
    );
}
#[test]
fn remote_enablement_rejects_duplicate_explicit_endpoint() {
    let endpoint = "[::]:0".parse().unwrap();
    assert_eq!(
        RemoteAccessPolicy::enabled(server(), vec![endpoint, endpoint]),
        Err(RemoteAccessError::DuplicateEndpoint)
    );
}
#[test]
fn remote_enablement_keeps_ipv4_and_ipv6_wildcards_distinct() {
    let v4 = "0.0.0.0:0".parse().unwrap();
    let v6 = "[::]:0".parse().unwrap();
    assert_eq!(
        RemoteAccessPolicy::enabled(server(), vec![v4, v6])
            .unwrap()
            .endpoints(),
        &[v4, v6]
    );
}
#[test]
fn creation_default_rejects_replay_before_its_source() {
    let mut state = organization();
    let first = state.panes[0].clone();
    let second = PaneRecord {
        id: PaneId::new(server(), 2).unwrap(),
        creation_ordinal: CreationOrdinal::new(4),
        creation: PaneCreation::SplitOf {
            source: first.id,
            axis: SplitAxis::Horizontal,
            edge: SplitEdge::After,
        },
        ..first.clone()
    };
    state.panes.push(second.clone());
    let invalid = TabCreationDefault::new(first.tab, vec![second.id, first.id]).unwrap();
    assert_eq!(
        invalid.validate(&state.panes),
        Err(OrganizationError::InvalidTopology)
    );
}
#[test]
fn topology_replay_order_need_not_follow_retained_creation_ordinals() {
    let mut state = organization();
    let first = state.panes[0].clone();
    let second = PaneRecord {
        id: PaneId::new(server(), 2).unwrap(),
        creation_ordinal: CreationOrdinal::new(4),
        creation: PaneCreation::Initial,
        ..first.clone()
    };
    state.panes[0].creation = PaneCreation::SplitOf {
        source: second.id,
        axis: SplitAxis::Horizontal,
        edge: SplitEdge::Before,
    };
    state.panes.push(second.clone());
    let normalized = TabCreationDefault::new(first.tab, vec![second.id, first.id]).unwrap();
    assert_eq!(normalized.validate(&state.panes), Ok(()));
}
#[test]
fn measured_startup_reservation_requires_exact_generation_successor() {
    let intent = StartupIntent::new(
        operation(1),
        EffectFence::new(
            session(),
            grant(),
            ConnectionBinding::new(grant().client(), ConnectionGeneration::new(1)),
            ContextGeneration::new(1),
            None,
        )
        .unwrap(),
        StartupSource::StartSession,
    )
    .unwrap();
    let measurement = StartupMeasurement {
        intent: intent.id(),
        fence: intent.fence(),
        grid: GridSize::new(
            24,
            80,
            CellPixels::new(8, 16).unwrap(),
            SUPPORTED_GRID_LIMITS,
        )
        .unwrap(),
    };
    let skipped = SessionKey {
        generation: SessionGeneration::new(4),
        ..session()
    };
    assert_eq!(
        MeasuredStartupReservation::new(intent, measurement, skipped),
        Err(StartupError::InvalidReservation)
    );
}
#[test]
fn startup_reservation_does_not_accept_unclaimed_control() {
    assert_eq!(
        StartupReservation::new(
            session().pane,
            SessionGeneration::new(1),
            ControlGeneration::new(7),
            ControlRecord::Unclaimed,
            session()
        ),
        Err(OrganizationError::InvalidRuntime)
    );
}
#[test]
fn kill_workspace_assessment_always_requires_confirmation() {
    assert_eq!(
        AssessmentSummary::new(
            AssessmentTarget::KillWorkspace(WorkspaceId::new(server(), 1).unwrap()),
            1,
            1,
            1,
            0,
            false
        ),
        Err(RequestError::WrongScope)
    );
}
#[test]
fn worktree_tabs_target_rejects_a_foreign_worktree() {
    let command = Command::AssessClose {
        target: CloseTarget::WorktreeTabs {
            workspace: WorkspaceId::new(server(), 1).unwrap(),
            worktree: WorktreeId::new(ServerId::new([8; 16]), 1).unwrap(),
        },
    };
    assert_eq!(
        command.validate_scope(operation(1).stream()),
        Err(RequestError::WrongScope)
    );
}
#[test]
fn new_ledger_writes_have_no_default_validation_bypass() {
    let mut proposed = parts();
    proposed
        .operation_writes
        .push(OperationWrite::Ledger(Box::new(
            LedgerWrite::RetireStream {
                binding: StreamBinding::from_stream(operation(1).stream()),
                reason: StreamRetirementReason::ExplicitRetirement,
            },
        )));
    assert_eq!(
        DurableTransaction::new(proposed),
        Err(StoreFailure::RecoveryRequired)
    );
}
#[test]
fn compact_outcome_admission_is_lower_than_result_reservation() {
    assert_eq!(
        CompactOutcome::from_encoded(vec![1; MAX_COMPACT_OUTCOME_BYTES + 1])
            .unwrap_err()
            .class,
        LedgerFailureClass::InvalidCompactOutcome
    );
}
#[test]
fn pairing_keys_cannot_collapse_device_and_authentication_roles() {
    assert_eq!(
        PairingKeys::new(PairingKeysParts {
            device_identity_public_key: [1; 32],
            pairing_authentication_public_key: [1; 32]
        }),
        Err(PairingContractError::WrongBinding)
    );
}
#[test]
fn git_confirmation_rejects_prestartup_epoch() {
    assert_eq!(
        GitAssessmentToken::new(GitAssessmentTokenParts {
            server: server(),
            epoch: ServerEpoch::new(0),
            pairing: operation(1).stream().pairing(),
            client: operation(1).stream().client(),
            kind: GitAssessmentKind::Remove,
            random: [1; 32]
        }),
        Err(GitFailure::InvalidResponse)
    );
}

#[test]
fn original_split_placement_is_not_replaced_by_its_normalized_creation_link() {
    let original = organization().panes.remove(0);
    let pane = PaneRecord {
        id: PaneId::new(server(), 2).unwrap(),
        creation_ordinal: CreationOrdinal::new(4),
        creation: PaneCreation::Initial,
        ..original.clone()
    };
    let delta = OrganizationDelta::new(
        server(),
        ServerEpoch::new(3),
        OrganizationRevision::new(1),
        OrganizationRevision::new(2),
        vec![
            OrganizationWrite::DeletePane(original.id),
            OrganizationWrite::PutPane(pane.clone()),
        ],
        vec![],
    )
    .unwrap();
    let placement = AcceptedPaneCreation::new(
        &delta,
        pane.tab,
        pane.id,
        pane.creation_ordinal,
        PaneCreation::SplitOf {
            source: original.id,
            axis: SplitAxis::Vertical,
            edge: SplitEdge::After,
        },
    )
    .unwrap();
    let delta = delta.with_accepted_creations(vec![placement]).unwrap();
    assert_eq!(delta.accepted_creations()[0].source(), original.id);
}

#[test]
fn original_split_placement_cannot_be_retagged_to_another_publication() {
    let mut pane = organization().panes.remove(0);
    let source = pane.id;
    pane.id = PaneId::new(server(), 2).unwrap();
    let delta = OrganizationDelta::new(
        server(),
        ServerEpoch::new(3),
        OrganizationRevision::new(1),
        OrganizationRevision::new(2),
        vec![OrganizationWrite::PutPane(pane.clone())],
        vec![],
    )
    .unwrap();
    let placement = AcceptedPaneCreation::new(
        &delta,
        pane.tab,
        pane.id,
        pane.creation_ordinal,
        PaneCreation::SplitOf {
            source,
            axis: SplitAxis::Horizontal,
            edge: SplitEdge::After,
        },
    )
    .unwrap();
    let later = OrganizationDelta::new(
        server(),
        ServerEpoch::new(3),
        OrganizationRevision::new(2),
        OrganizationRevision::new(3),
        vec![OrganizationWrite::PutPane(pane)],
        vec![],
    )
    .unwrap();
    assert_eq!(
        later.with_accepted_creations(vec![placement]),
        Err(OrganizationError::InvalidRevision)
    );
}

#[test]
fn legacy_operation_slot_cannot_bypass_new_outcome_validation() {
    let mut proposed = parts();
    proposed.resulting_high_water = IdentityHighWater {
        pane: 1,
        client: 1,
        pairing: 1,
        operation_stream: 1,
        ..Default::default()
    };
    proposed
        .operation_writes
        .push(OperationWrite::Put(Box::new(DurableOperationRecord {
            binding: OperationBinding {
                id: operation(1),
                digest: RequestDigest::from_bytes([1; 32]),
            },
            state: DurableOperationState::Completed {
                result: Outcome::ControlGranted(
                    GrantedControl::new(ServerEpoch::new(3), grant()).unwrap(),
                ),
                publication_revision: OrganizationRevision::new(1),
            },
        })));
    assert_eq!(
        DurableTransaction::new(proposed),
        Err(StoreFailure::RecoveryRequired)
    );
}

#[test]
fn legacy_recovery_slot_cannot_install_a_new_outcome_without_domain_validation() {
    let recovered = RecoveredState {
        organization: organization(),
        store_revision: StoreRevision::new(2),
        last_epoch: ServerEpoch::new(3),
        high_water: IdentityHighWater {
            workspace: 1,
            tab: 1,
            pane: 1,
            client: 1,
            pairing: 1,
            operation_stream: 1,
            creation_ordinal: 3,
            ..Default::default()
        },
        streams: vec![DurableStreamState {
            id: operation(1).stream(),
            admitted_through: 1,
            acknowledged_through: 0,
        }],
        operations: vec![DurableOperationRecord {
            binding: OperationBinding {
                id: operation(1),
                digest: RequestDigest::from_bytes([1; 32]),
            },
            state: DurableOperationState::Completed {
                result: Outcome::ControlGranted(
                    GrantedControl::new(ServerEpoch::new(3), grant()).unwrap(),
                ),
                publication_revision: OrganizationRevision::new(1),
            },
        }],
        ledger: None,
        pairing: None,
        git_repositories: vec![],
        git_intents: vec![],
        git_allocations: vec![],
        native_retirements: vec![],
    };
    assert_eq!(recovered.validate(), Err(StoreFailure::RecoveryRequired));
}

fn removal_intent_parts(target: WorktreeId, tab_worktree: WorktreeId) -> GitWriteIntentParts {
    let binding = OperationBinding {
        id: operation(1),
        digest: RequestDigest::from_bytes([1; 32]),
    };
    let scope = RepositoryScope {
        server: server(),
        repository: RepositoryId::new(server(), 1).unwrap(),
    };
    let directory = ServerDirectory::new(server(), b"/work".to_vec()).unwrap();
    let mut tabs = Vec::new();
    let mut panes = Vec::new();
    for local in 1..=2 {
        let tab = TabRecord {
            id: TabId::new(server(), local).unwrap(),
            workspace: WorkspaceId::new(server(), local).unwrap(),
            root_pane: PaneId::new(server(), local).unwrap(),
            worktree: Some(tab_worktree),
            creation_ordinal: CreationOrdinal::new(local),
        };
        panes.push(RetainedAffectedPane {
            hierarchy: AssessedPane {
                workspace: tab.workspace,
                tab: tab.id,
                pane: tab.root_pane,
                session_generation: SessionGeneration::new(0),
                control_generation: ControlGeneration::new(0),
                context_generation: ContextGeneration::new(0),
            },
            session: None,
            startup_reserved: false,
            work_generation: CloseWorkGeneration::new(0),
        });
        tabs.push(tab);
    }
    GitWriteIntentParts {
        binding,
        repository: RepositoryLocator::new(RepositoryLocatorParts {
            scope,
            main_root: directory.clone(),
            common_directory: directory.clone(),
        })
        .unwrap(),
        action: RetainedGitAction::Remove {
            worktree: target,
            directory,
            head: None,
            approval: ApprovedRemoval::Missing,
        },
        phase: GitIntentPhase::Prepared,
        assessment_digest: GitAssessmentDigest::from_bytes([2; 32]),
        affected: RetainedRemovalScope::new(RetainedRemovalScopeParts {
            server: server(),
            tabs,
            membership_versions: vec![],
            panes,
            fallbacks: vec![],
        })
        .unwrap(),
        allocation: binding,
        external_result: None,
    }
}
#[test]
fn checked_removal_intent_rejects_another_servers_target() {
    let parts = removal_intent_parts(
        WorktreeId::new(ServerId::new([8; 16]), 1).unwrap(),
        WorktreeId::new(server(), 2).unwrap(),
    );
    assert_eq!(GitWriteIntent::new(parts), Err(GitFailure::WrongServer));
}
#[test]
fn removal_intent_deserialization_rechecks_the_targets_server() {
    let target = WorktreeId::new(server(), 1).unwrap();
    let value = GitWriteIntent::new(removal_intent_parts(target, target)).unwrap();
    let mut json = serde_json::to_value(value).unwrap();
    json["action"]["Remove"]["worktree"] =
        serde_json::to_value(WorktreeId::new(ServerId::new([8; 16]), 2).unwrap()).unwrap();
    assert!(serde_json::from_value::<GitWriteIntent>(json).is_err());
}
#[test]
fn removal_intent_preserves_exact_worktree_scope_across_workspaces() {
    let target = WorktreeId::new(server(), 1).unwrap();
    let value = GitWriteIntent::new(removal_intent_parts(target, target)).unwrap();
    assert_eq!(value.parts().affected.parts().tabs.len(), 2);
}
#[test]
fn removal_scope_rejects_a_pane_linked_to_another_tabs_workspace() {
    let target = WorktreeId::new(server(), 1).unwrap();
    let mut scope = removal_intent_parts(target, target)
        .affected
        .parts()
        .clone();
    scope.panes[0].hierarchy.tab = scope.tabs[1].id;
    assert_eq!(
        RetainedRemovalScope::new(scope),
        Err(GitFailure::InvalidResponse)
    );
}

#[test]
fn measured_grid_keeps_valid_cells_and_zeros_unrepresentable_pty_extents() {
    let pixels = CellPixels::new(64, 16).unwrap();
    let grid = MeasuredGrid::new(24, 1024, pixels, SUPPORTED_GRID_LIMITS).unwrap();
    assert_eq!(
        (grid.rows(), grid.columns(), grid.cell_pixels()),
        (24, 1024, pixels)
    );
    assert_eq!(grid.pty_pixels(), (0, 384));
    assert_eq!(
        GridSize::new(24, 1024, pixels, SUPPORTED_GRID_LIMITS),
        Err(GridError::Overflow)
    );
}
#[test]
fn measured_grid_rejects_cell_limits_even_when_pty_pixels_fall_back() {
    assert_eq!(
        MeasuredGrid::new(
            1024,
            2048,
            CellPixels::new(64, 64).unwrap(),
            SUPPORTED_GRID_LIMITS
        ),
        Err(GridError::TooManyCells)
    );
}
#[test]
fn measured_startup_reservation_accepts_pixel_overflow_without_changing_legacy_measurement() {
    let fence = EffectFence::new(
        session(),
        grant(),
        ConnectionBinding::new(grant().client(), ConnectionGeneration::new(1)),
        ContextGeneration::new(1),
        None,
    )
    .unwrap();
    let intent = StartupIntent::new(operation(1), fence, StartupSource::StartSession).unwrap();
    let grid = MeasuredGrid::new(
        24,
        1024,
        CellPixels::new(64, 16).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    let measurement = MeasuredStartupMeasurement {
        intent: intent.id(),
        fence,
        grid,
    };
    let next = SessionKey {
        generation: session().generation.checked_successor().unwrap(),
        ..session()
    };
    let reservation = MeasuredStartupReservation::new(intent, measurement, next).unwrap();
    assert_eq!(reservation.grid().pty_pixels(), (0, 384));
}

#[test]
fn creation_preview_requires_the_drafts_exact_template_version() {
    let draft = WorktreeCreateDraft::new(
        RepositoryScope {
            server: server(),
            repository: RepositoryId::new(server(), 1).unwrap(),
        },
        WorktreeBranchChoice::Existing {
            name: BranchName::new("topic".into()).unwrap(),
        },
        WorktreeLocationChoice::ServerTemplate {
            expected_version: FieldVersion::new(7),
        },
    )
    .unwrap();
    let directory = ServerDirectory::new(server(), b"/work/topic".to_vec()).unwrap();
    assert_eq!(
        WorktreeCreationPreview::new(draft.clone(), directory.clone(), Some(FieldVersion::new(8))),
        Err(GitFailure::StaleAssessment)
    );
    assert!(WorktreeCreationPreview::new(draft, directory, Some(FieldVersion::new(7))).is_ok());
}
#[test]
fn explicit_creation_location_cannot_be_replaced_in_the_preview() {
    let directory = ServerDirectory::new(server(), b"/work/topic".to_vec()).unwrap();
    let draft = WorktreeCreateDraft::new(
        RepositoryScope {
            server: server(),
            repository: RepositoryId::new(server(), 1).unwrap(),
        },
        WorktreeBranchChoice::Existing {
            name: BranchName::new("topic".into()).unwrap(),
        },
        WorktreeLocationChoice::Explicit(directory),
    )
    .unwrap();
    assert_eq!(
        WorktreeCreationPreview::new(
            draft,
            ServerDirectory::new(server(), b"/elsewhere".to_vec()).unwrap(),
            None
        ),
        Err(GitFailure::InvalidDirectory)
    );
}

#[test]
fn creation_template_preview_rejects_stale_settings_on_confirmation_comparison() {
    let draft = WorktreeCreateDraft::new(
        RepositoryScope {
            server: server(),
            repository: RepositoryId::new(server(), 1).unwrap(),
        },
        WorktreeBranchChoice::New {
            name: BranchName::new("topic".into()).unwrap(),
            base: BranchReference::Remote(BranchName::new("origin/main".into()).unwrap()),
        },
        WorktreeLocationChoice::ServerTemplate {
            expected_version: FieldVersion::new(7),
        },
    )
    .unwrap();
    let preview = WorktreeCreationPreview::new(
        draft,
        ServerDirectory::new(server(), b"/work/topic".to_vec()).unwrap(),
        Some(FieldVersion::new(7)),
    )
    .unwrap();
    assert_eq!(
        preview.validate_template_version(FieldVersion::new(8)),
        Err(GitFailure::StaleAssessment)
    );
}
#[test]
fn measured_grid_deserialization_checks_dimensions_without_rejecting_pixel_overflow() {
    let grid = MeasuredGrid::new(
        24,
        1024,
        CellPixels::new(64, 16).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    let mut json = serde_json::to_value(grid).unwrap();
    assert_eq!(
        serde_json::from_value::<MeasuredGrid>(json.clone())
            .unwrap()
            .pty_pixels(),
        (0, 384)
    );
    json["rows"] = 0.into();
    assert!(serde_json::from_value::<MeasuredGrid>(json).is_err());
}
