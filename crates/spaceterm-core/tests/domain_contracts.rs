mod support;

use spaceterm_core::authority::*;
use spaceterm_core::control::*;
use spaceterm_core::geometry::*;
use spaceterm_core::identity::*;
use spaceterm_core::operations::*;
use spaceterm_core::organization::*;
use spaceterm_core::pairing::*;
use spaceterm_core::storage::*;

use support::{create_command, operation, organization, server};

#[test]
fn canonical_digest_has_a_retained_encoding_fixture() {
    let digest = RequestDigest::for_command(&create_command()).unwrap();
    assert_eq!(
        digest.bytes(),
        [
            88, 150, 78, 73, 62, 56, 230, 78, 12, 83, 108, 167, 154, 159, 231, 169, 53, 53, 94,
            251, 23, 161, 30, 143, 202, 197, 235, 160, 149, 244, 143, 150
        ]
    );
}

#[test]
fn retries_bind_original_identity_to_exact_contents() {
    let first = OperationRequest::new(operation(1), create_command()).unwrap();
    let retry = OperationRequest::new(operation(1), create_command()).unwrap();
    assert_eq!(first.binding(), retry.binding());
    let changed = OperationRequest::new(
        operation(1),
        Command::CreateWorkspace {
            server: server(),
            source: WorkspaceCreationSource::ServerHome { server: server() },
            name: NameDraft::Custom(NameText::new("draft".to_owned()).unwrap()),
        },
    )
    .unwrap();
    assert_eq!(
        first.verify_binding(changed.binding()),
        Err(RequestError::IdentityReused)
    );
    assert_eq!(
        OperationSequence::new(u64::MAX)
            .unwrap()
            .checked_successor(),
        None
    );
}

#[test]
fn creation_cannot_silently_use_a_different_server_or_claimed_client() {
    let other = ServerId::new([8; 16]);
    assert_eq!(
        OperationRequest::new(
            operation(1),
            Command::CreatePane {
                tab: TabId::new(server(), 1).unwrap(),
                source: PaneId::new(other, 1).unwrap(),
                axis: SplitAxis::Horizontal,
                edge: SplitEdge::After,
            }
        ),
        Err(RequestError::WrongScope)
    );
    assert_eq!(
        OperationRequest::new(
            operation(1),
            Command::CompareAcquire(CompareAcquireRequest {
                epoch: ServerEpoch::new(1),
                pane: PaneId::new(server(), 1).unwrap(),
                client: ClientId::new(server(), 2).unwrap(),
                expected_generation: ControlGeneration::new(0),
            })
        ),
        Err(RequestError::WrongScope)
    );
}

fn intent() -> StartupIntent {
    let id = operation(1);
    let pane = PaneId::new(server(), 1).unwrap();
    let grant = ControlGrant::new(pane, id.stream().client(), ControlGeneration::new(1)).unwrap();
    StartupIntent::new(
        id,
        EffectFence::new(
            SessionKey {
                epoch: ServerEpoch::new(1),
                pane,
                generation: SessionGeneration::new(0),
            },
            grant,
            ConnectionBinding::new(grant.client(), ConnectionGeneration::new(1)),
            ContextGeneration::new(1),
            None,
        )
        .unwrap(),
        StartupSource::ExplicitSelection,
    )
    .unwrap()
}

#[test]
fn measured_startup_completes_only_the_original_explicit_intent_and_fence() {
    let intent = intent();
    let mut measurement = StartupMeasurement {
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
    assert!(intent.matches_measurement(&measurement));
    measurement.intent = operation(2);
    assert!(!intent.matches_measurement(&measurement));
    measurement.intent = intent.id();
    let fence = intent.fence();
    measurement.fence = EffectFence::new(
        fence.session(),
        fence.grant(),
        ConnectionBinding::new(fence.grant().client(), ConnectionGeneration::new(2)),
        fence.context(),
        None,
    )
    .unwrap();
    assert!(!intent.matches_measurement(&measurement));
}

#[test]
fn startup_deserialization_cannot_rebind_an_explicit_intent() {
    let intent = intent();
    let mut value = serde_json::to_value(intent).unwrap();
    let other_client = ClientId::new(server(), 2).unwrap();
    value["id"]["stream"]["client"] = serde_json::to_value(other_client).unwrap();
    assert!(serde_json::from_value::<StartupIntent>(value).is_err());
}

#[test]
fn organization_requires_surviving_membership_and_roots() {
    let mut state = organization();
    assert_eq!(state.validate(), Ok(()));
    state.tabs[0].root_pane = PaneId::new(server(), 2).unwrap();
    assert_eq!(state.validate(), Err(OrganizationError::InvalidMembership));
}

#[test]
fn creation_topology_cannot_retain_deleted_sources_or_cycles() {
    let mut state = organization();
    state.panes[0].creation = PaneCreation::SplitOf {
        source: PaneId::new(server(), 2).unwrap(),
        axis: SplitAxis::Horizontal,
        edge: SplitEdge::After,
    };
    assert_eq!(state.validate(), Err(OrganizationError::InvalidTopology));
    let mut second = state.panes[0].clone();
    second.id = PaneId::new(server(), 2).unwrap();
    second.creation_ordinal = CreationOrdinal::new(4);
    second.creation = PaneCreation::SplitOf {
        source: state.panes[0].id,
        axis: SplitAxis::Vertical,
        edge: SplitEdge::Before,
    };
    state.panes.push(second);
    assert_eq!(state.validate(), Err(OrganizationError::InvalidTopology));
}

#[test]
fn normalized_topology_is_bounded_by_surviving_panes_not_creation_ancestry() {
    let mut state = organization();
    let mut second = state.panes[0].clone();
    second.id = PaneId::new(server(), 2).unwrap();
    second.creation_ordinal = CreationOrdinal::new(4);
    second.creation = PaneCreation::SplitOf {
        source: state.panes[0].id,
        axis: SplitAxis::Vertical,
        edge: SplitEdge::After,
    };
    state.panes.push(second);
    assert_eq!(state.validate(), Ok(()));
    state.panes.remove(0);
    state.panes[0].creation = PaneCreation::Initial;
    state.tabs[0].root_pane = state.panes[0].id;
    assert_eq!(state.validate(), Ok(()));
}

#[test]
fn duplicate_creation_ordinals_and_foreign_directories_fail_validation() {
    let mut state = organization();
    state.panes[0].creation_ordinal = state.tabs[0].creation_ordinal;
    assert_eq!(state.validate(), Err(OrganizationError::Duplicate));
    state = organization();
    state.panes[0].starting_directory =
        ServerDirectory::new(ServerId::new([8; 16]), b"/work".to_vec()).unwrap();
    assert_eq!(state.validate(), Err(OrganizationError::WrongServer));
}

#[test]
fn empty_organization_is_valid_and_has_no_implicit_replacement_workspace() {
    let mut state = organization();
    state.workspaces.clear();
    state.tabs.clear();
    state.panes.clear();
    assert!(OrganizationSnapshot::new(ServerEpoch::new(1), state, vec![]).is_ok());
}

#[test]
fn runtime_publication_requires_exact_session_and_grant_generations() {
    let state = organization();
    let record = PaneRuntimeRecord {
        pane: state.panes[0].id,
        session_generation: SessionGeneration::new(2),
        phase: SessionPhase::Running {
            generation: SessionGeneration::new(1),
        },
        control_generation: ControlGeneration::new(0),
        control: ControlRecord::Unclaimed,
    };
    assert_eq!(
        OrganizationSnapshot::new(ServerEpoch::new(1), state.clone(), vec![record]),
        Err(OrganizationError::InvalidRuntime)
    );
    let record = PaneRuntimeRecord {
        phase: SessionPhase::NotStarted,
        control: ControlRecord::Held {
            client: operation(1).stream().client(),
            generation: ControlGeneration::new(1),
        },
        ..record
    };
    assert_eq!(
        OrganizationSnapshot::new(ServerEpoch::new(1), state, vec![record]),
        Err(OrganizationError::InvalidRuntime)
    );
}

#[test]
fn delta_rejects_cross_server_membership_inside_a_record() {
    let mut tab = organization().tabs.remove(0);
    tab.workspace = WorkspaceId::new(ServerId::new([8; 16]), 1).unwrap();
    assert_eq!(
        OrganizationDelta::new(
            server(),
            ServerEpoch::new(1),
            OrganizationRevision::new(1),
            OrganizationRevision::new(2),
            vec![OrganizationWrite::PutTab(tab)],
            vec![]
        ),
        Err(OrganizationError::WrongServer)
    );
}

#[test]
fn directory_values_are_bounded_opaque_bytes_and_never_error_details() {
    let opaque = ServerDirectory::new(server(), b"/work/\xff".to_vec()).unwrap();
    assert_eq!(opaque.bytes(), b"/work/\xff");
    assert!(!format!("{opaque:?}").contains("/work"));
    assert_eq!(
        ServerDirectory::new(server(), b"/secret\0".to_vec()),
        Err(OrganizationError::InvalidDirectory)
    );
    assert_eq!(
        ServerDirectory::new(server(), vec![b'a'; MAX_DIRECTORY_BYTES + 1]),
        Err(OrganizationError::InvalidDirectory)
    );
    let mut encoded = serde_json::to_value(opaque).unwrap();
    encoded["bytes"] = serde_json::json!([0]);
    assert!(serde_json::from_value::<ServerDirectory>(encoded).is_err());
}

fn transaction_parts() -> TransactionParts {
    TransactionParts {
        server: server(),
        expected_store_revision: StoreRevision::new(10),
        resulting_store_revision: StoreRevision::new(11),
        expected_organization_revision: OrganizationRevision::new(3),
        resulting_organization_revision: OrganizationRevision::new(3),
        expected_epoch: ServerEpoch::new(1),
        resulting_epoch: ServerEpoch::new(1),
        expected_high_water: IdentityHighWater {
            workspace: 1,
            tab: 1,
            pane: 1,
            creation_ordinal: 3,
            pairing: 1,
            client: 1,
            operation_stream: 1,
            ..IdentityHighWater::default()
        },
        resulting_high_water: IdentityHighWater {
            workspace: 1,
            tab: 1,
            pane: 1,
            creation_ordinal: 3,
            pairing: 1,
            client: 1,
            operation_stream: 1,
            ..IdentityHighWater::default()
        },
        organization_writes: vec![],
        operation_writes: vec![],
    }
}

#[test]
fn ledger_only_commit_does_not_advance_organization_or_field_versions() {
    let mut parts = transaction_parts();
    parts
        .operation_writes
        .push(OperationWrite::RetireAcknowledgedPrefix {
            stream: operation(1).stream(),
            through: operation(1).sequence(),
        });
    let transaction = DurableTransaction::new(parts).unwrap();
    assert!(transaction.accepts_receipt(CommitReceipt {
        store_revision: StoreRevision::new(11),
        organization_revision: OrganizationRevision::new(3)
    }));
    assert!(!transaction.accepts_receipt(CommitReceipt {
        store_revision: StoreRevision::new(10),
        organization_revision: OrganizationRevision::new(3)
    }));
}

#[test]
fn durable_transaction_rejects_counter_wrap_regression_and_unpublished_writes() {
    let mut parts = transaction_parts();
    parts.expected_store_revision = StoreRevision::new(u64::MAX);
    assert_eq!(DurableTransaction::new(parts), Err(StoreFailure::Exhausted));
    parts = transaction_parts();
    parts.expected_high_water.pane = 100;
    parts.resulting_high_water.pane = 99;
    assert_eq!(
        DurableTransaction::new(parts),
        Err(StoreFailure::InvalidDurableState)
    );
    parts = transaction_parts();
    parts
        .organization_writes
        .push(OrganizationWrite::PutPane(organization().panes.remove(0)));
    assert_eq!(
        DurableTransaction::new(parts),
        Err(StoreFailure::InvalidDurableState)
    );
}

fn recovered() -> RecoveredState {
    RecoveredState {
        ledger: None,
        pairing: None,
        git_repositories: vec![],
        git_intents: vec![],
        git_allocations: vec![],
        native_retirements: vec![],
        organization: organization(),
        store_revision: StoreRevision::new(1),
        last_epoch: ServerEpoch::new(1),
        high_water: IdentityHighWater {
            workspace: 1,
            tab: 1,
            pane: 1,
            creation_ordinal: 3,
            pairing: 1,
            client: 1,
            operation_stream: 1,
            ..IdentityHighWater::default()
        },
        streams: vec![],
        operations: vec![],
    }
}

#[test]
fn recovery_never_rebuilds_allocation_marks_from_surviving_entities() {
    let mut state = recovered();
    assert_eq!(state.validate(), Ok(()));
    state.high_water.pane = 0;
    assert_eq!(state.validate(), Err(StoreFailure::InvalidDurableState));
    state = recovered();
    state.high_water.pane = 99;
    assert_eq!(state.validate(), Ok(()));
}

#[test]
fn recovery_requires_every_unacknowledged_sequence_and_retains_acknowledged_prefixes() {
    let mut state = recovered();
    state.streams.push(DurableStreamState {
        id: operation(1).stream(),
        admitted_through: 2,
        acknowledged_through: 0,
    });
    let record = DurableOperationRecord {
        binding: OperationRequest::new(operation(1), create_command())
            .unwrap()
            .binding(),
        state: DurableOperationState::Completed {
            result: Outcome::Applied,
            publication_revision: OrganizationRevision::new(1),
        },
    };
    state.operations.push(record.clone());
    assert_eq!(state.validate(), Err(StoreFailure::InvalidDurableState));
    state.streams[0].admitted_through = 1;
    assert_eq!(state.validate(), Ok(()));
    state.streams[0].acknowledged_through = 1;
    assert_eq!(state.validate(), Err(StoreFailure::InvalidDurableState));
    state.operations.clear();
    assert_eq!(state.validate(), Ok(()));
}

#[test]
fn pending_recovery_validates_canonical_identity_binding_before_reconciliation() {
    let mut state = recovered();
    state.streams.push(DurableStreamState {
        id: operation(1).stream(),
        admitted_through: 1,
        acknowledged_through: 0,
    });
    state.operations.push(DurableOperationRecord {
        binding: OperationRequest::new(operation(1), create_command())
            .unwrap()
            .binding(),
        state: DurableOperationState::Pending {
            command: create_command(),
        },
    });
    assert_eq!(state.validate(), Ok(()));
    state.operations[0].binding.digest = RequestDigest::from_bytes([0; 32]);
    assert_eq!(state.validate(), Err(StoreFailure::InvalidDurableState));
}

#[test]
fn recovered_completed_results_cannot_claim_other_server_entities() {
    let mut state = recovered();
    state.streams.push(DurableStreamState {
        id: operation(1).stream(),
        admitted_through: 1,
        acknowledged_through: 0,
    });
    state.operations.push(DurableOperationRecord {
        binding: OperationRequest::new(operation(1), create_command())
            .unwrap()
            .binding(),
        state: DurableOperationState::Completed {
            result: Outcome::CreatedPane {
                pane: PaneId::new(ServerId::new([8; 16]), 1).unwrap(),
            },
            publication_revision: OrganizationRevision::new(1),
        },
    });
    assert_eq!(state.validate(), Err(StoreFailure::InvalidDurableState));
}

fn pending_revocation() -> PendingSelfRevocation {
    PendingSelfRevocation::new(PendingParts {
        original_server: server(),
        original_pairing: operation(1).stream().pairing(),
        device: DeviceId::new([9; 16]),
        client: operation(1).stream().client(),
        operation: operation(1),
        route: RevocationRouteReference::new([1; 16]),
        credential: CredentialReference::new([2; 16]),
        notice_dismissed: true,
    })
    .unwrap()
}

#[test]
fn old_revocation_receipt_cannot_clean_up_a_new_pairing_or_different_operation() {
    let task = pending_revocation();
    let mut receipt = RevocationReceipt {
        server: task.server(),
        pairing: task.pairing(),
        device: task.device(),
        operation: task.operation(),
        revision: StoreRevision::new(2),
        receipt_digest: RequestDigest::from_bytes([3; 32]),
    };
    assert!(task.matches_receipt(&receipt));
    assert!(task.notice_dismissed());
    receipt.pairing = PairingId::new(server(), 2).unwrap();
    assert!(!task.matches_receipt(&receipt));
    receipt.pairing = task.pairing();
    receipt.operation = operation(2);
    assert!(!task.matches_receipt(&receipt));
}

#[test]
fn pending_revocation_deserialization_cannot_redirect_original_pairing() {
    let mut encoded = serde_json::to_value(pending_revocation()).unwrap();
    encoded["original_pairing"] =
        serde_json::to_value(PairingId::new(server(), 2).unwrap()).unwrap();
    assert!(serde_json::from_value::<PendingSelfRevocation>(encoded).is_err());
}

#[test]
fn a_persisted_stream_handle_cannot_be_rebound_to_a_second_client() {
    let mut state = recovered();
    let stream = operation(1).stream();
    state.high_water.client = 2;
    state.streams.push(DurableStreamState {
        id: stream,
        admitted_through: 0,
        acknowledged_through: 0,
    });
    state.streams.push(DurableStreamState {
        id: OperationStreamId::new(
            stream.key(),
            stream.pairing(),
            ClientId::new(server(), 2).unwrap(),
        )
        .unwrap(),
        admitted_through: 0,
        acknowledged_through: 0,
    });
    assert_eq!(state.validate(), Err(StoreFailure::InvalidDurableState));
}

#[test]
fn historical_creation_results_must_remain_below_retained_allocation_marks() {
    let mut state = recovered();
    state.streams.push(DurableStreamState {
        id: operation(1).stream(),
        admitted_through: 1,
        acknowledged_through: 0,
    });
    state.operations.push(DurableOperationRecord {
        binding: OperationRequest::new(operation(1), create_command())
            .unwrap()
            .binding(),
        state: DurableOperationState::Completed {
            result: Outcome::CreatedPane {
                pane: PaneId::new(server(), 2).unwrap(),
            },
            publication_revision: OrganizationRevision::new(1),
        },
    });
    assert_eq!(state.validate(), Err(StoreFailure::InvalidDurableState));
    // The created Pane was later deleted. Its historical result is still valid;
    // recovering it does not require or recreate a surviving Pane record.
    state.high_water.pane = 2;
    assert_eq!(state.validate(), Ok(()));
}

#[test]
fn transaction_rejects_entity_writes_without_corresponding_retained_marks() {
    let mut parts = transaction_parts();
    parts.resulting_organization_revision = OrganizationRevision::new(4);
    let mut pane = organization().panes.remove(0);
    pane.id = PaneId::new(server(), 2).unwrap();
    pane.creation_ordinal = CreationOrdinal::new(4);
    parts
        .organization_writes
        .push(OrganizationWrite::PutPane(pane));
    assert_eq!(
        DurableTransaction::new(parts.clone()),
        Err(StoreFailure::InvalidDurableState)
    );
    parts.resulting_high_water.pane = 2;
    parts.resulting_high_water.creation_ordinal = 4;
    assert!(DurableTransaction::new(parts).is_ok());
}

#[test]
fn beginning_startup_cannot_alias_another_operation_as_its_intent() {
    assert_eq!(
        OperationRequest::new(operation(2), Command::BeginStartup(intent())),
        Err(RequestError::WrongScope)
    );
}

#[test]
fn server_status_serialization_preserves_optional_machine_facts_and_validates_attachment() {
    use spaceterm_core::{management::*, remote_access::RemoteAccessPolicy};
    use std::num::NonZeroU64;
    let status = ServerStatus::new(ServerStatusParts {
        runtime: Default::default(),
        server: server(),
        epoch: ServerEpoch::new(3),
        revision: NonZeroU64::MIN,
        running: None,
        approved: None,
        staged: None,
        native_verification: NativeVerification::Unverified,
        capabilities: vec![],
        remote_access: Versioned {
            value: RemoteAccessPolicy::disabled(server()),
            version: FieldVersion::new(1),
        },
        listeners: vec![],
        progress: vec![],
    })
    .unwrap();
    // The older flattened record remains readable without local default offers.
    let legacy = serde_json::to_vec(status.parts()).unwrap();
    let decoded: ServerStatus = serde_json::from_slice(&legacy).unwrap();
    assert_eq!(decoded, status);
    assert!(decoded.machine_facts().is_none());
    let status = status
        .with_machine_facts(ServerMachineFacts {
            platform: ServerPlatform::Linux,
            bind_scope_availability: MachineFactsAvailability::Available,
            bind_scopes: vec![ServerBindScopeOffer {
                scope: ServerBindScope::AllInterfaces,
                endpoints: vec![
                    "0.0.0.0:47232".parse().unwrap(),
                    "[::]:47232".parse().unwrap(),
                ],
            }],
            ..Default::default()
        })
        .unwrap();
    let recovered: ServerStatus =
        serde_json::from_slice(&serde_json::to_vec(&status).unwrap()).unwrap();
    assert_eq!(recovered, status);
    let mut invalid = serde_json::to_value(&status).unwrap();
    invalid["machine_facts"]["bind_scopes"][0]["endpoints"][1] = serde_json::json!("[::1]:47232");
    assert!(serde_json::from_value::<ServerStatus>(invalid).is_err());
}
