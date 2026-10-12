use spaceterm_core::identity::*;
use spaceterm_core::operation_ledger::canonical::CanonicalCodec;
use spaceterm_core::operation_ledger::*;
use spaceterm_core::operations::*;
use spaceterm_core::organization::*;

#[test]
fn canonical_digest_binds_identity_and_roundtrips_exact_request() {
    let server = ServerId::new([1; 16]);
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server, 1).unwrap(),
        PairingId::new(server, 1).unwrap(),
        ClientId::new(server, 1).unwrap(),
    )
    .unwrap();
    let id = OperationId::new(stream, OperationSequence::new(1).unwrap());
    let command = Command::CreateWorkspace {
        server,
        source: WorkspaceCreationSource::ServerHome { server },
        name: NameDraft::Automatic,
    };
    let request = CanonicalOperationRequest::new(id, command.clone(), &CanonicalCodec).unwrap();
    let bytes = request.encode(&CanonicalCodec).unwrap();
    assert_eq!(
        CanonicalOperationRequest::decode(&bytes, &CanonicalCodec).unwrap(),
        request
    );
    let next = OperationId::new(stream, OperationSequence::new(2).unwrap());
    assert_ne!(
        CanonicalOperationRequest::new(next, command, &CanonicalCodec)
            .unwrap()
            .binding()
            .digest,
        request.binding().digest
    );
}

#[path = "support/operation_ledger.rs"]
mod support;
use spaceterm_core::authority::*;
use spaceterm_core::operation_ledger::model::*;
use support::*;

#[test]
fn unknown_stream_never_registers_on_operation_submission() {
    let mut h = Harness::new();
    assert_class(
        h.prepare(binding(1), 1, &create()),
        LedgerFailureClass::UnknownStream,
    );
    assert!(h.rows.streams.is_empty());
}
#[test]
fn stream_registration_binds_server_pairing_and_stable_client() {
    let mut h = Harness::new();
    h.register(binding(1));
    let wrong = binding_for(1, 2, 1);
    assert_class(
        h.ledger.lookup(&wrong, op(wrong, 1), None),
        LedgerFailureClass::ScopeMismatch,
    );
    assert_class(
        h.ledger.prepare_register_stream(wrong, h.rows.context()),
        LedgerFailureClass::ScopeMismatch,
    );
    let wrong = binding_for(1, 1, 2);
    assert_class(
        h.ledger.prepare_register_stream(wrong, h.rows.context()),
        LedgerFailureClass::ScopeMismatch,
    );
    assert_class(
        StreamBinding::new(
            ServerId::new([2; 16]),
            binding(1).pairing(),
            binding(1).client(),
            binding(1).stream(),
        ),
        LedgerFailureClass::ScopeMismatch,
    );
}
#[test]
fn stream_id_cannot_be_registered_under_a_later_pairing() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.retire(binding(1), StreamRetirementReason::PairingRevoked);
    h.reopen();
    assert_class(
        h.ledger
            .prepare_register_stream(binding_for(1, 2, 2), h.rows.context()),
        LedgerFailureClass::ScopeMismatch,
    );
}
#[test]
fn repeat_registration_does_not_reopen_retired_stream() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.retire(binding(1), StreamRetirementReason::ExplicitRetirement);
    h.register(binding(1));
    assert_eq!(
        h.ledger
            .lookup(&binding(1), op(binding(1), 1), None)
            .unwrap(),
        OperationLookup::AcknowledgedRetired
    );
}
#[test]
fn sequence_starts_at_one_and_gap_does_not_consume_capacity() {
    let mut h = Harness::new();
    h.register(binding(1));
    assert!(
        matches!(h.prepare(binding(1), 2, &create()).unwrap(), AdmissionPreparation::Existing(OperationLookup::SequenceGap { expected }) if expected.get() == 1)
    );
    assert_eq!(
        h.rows.streams[&binding(1).stream().key()].admitted_through,
        0
    );
    h.admit(binding(1), 1, &create());
}
#[test]
fn same_identity_same_request_returns_pending_without_second_permit() {
    let mut h = Harness::new();
    h.register(binding(1));
    {
        let _permit = h.admit(binding(1), 1, &create());
    }
    assert!(matches!(
        h.prepare(binding(1), 1, &create()).unwrap(),
        AdmissionPreparation::Existing(OperationLookup::Pending)
    ));
}
#[test]
fn same_identity_different_request_is_rejected_pending_and_completed() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &create());
    for complete in [false, true] {
        if complete {
            h.complete(binding(1), 1, &Outcome::Applied, true);
        }
        assert!(matches!(
            h.prepare(binding(1), 1, &rename("changed draft")).unwrap(),
            AdmissionPreparation::Existing(OperationLookup::IdentityReusedWithDifferentContents)
        ));
    }
}
#[test]
fn completed_create_replays_after_target_was_killed_without_execution() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &create());
    let outcome = Outcome::CreatedWorkspace {
        workspace: WorkspaceId::new(server(), 1).unwrap(),
        tab: TabId::new(server(), 1).unwrap(),
        pane: PaneId::new(server(), 1).unwrap(),
    };
    h.complete(binding(1), 1, &outcome, true);
    // An unrelated committed deletion advances the authoritative revision. The
    // ledger has no entity-existence callback and cannot allocate on lookup.
    h.rows.organization = OrganizationRevision::new(2);
    h.rows.store = StoreRevision::new(h.rows.store.get() + 1);
    h.reopen();
    assert!(
        matches!(h.prepare(binding(1), 1, &create()).unwrap(), AdmissionPreparation::Existing(OperationLookup::Completed { result, publication_revision }) if result == outcome && publication_revision.get() == 1)
    );
}
#[test]
fn acknowledged_identity_never_executes_with_same_or_changed_contents() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &create());
    h.complete(binding(1), 1, &Outcome::Applied, true);
    h.ack(binding(1), 1);
    h.reopen();
    for command in [create(), rename("new draft")] {
        assert!(matches!(
            h.prepare(binding(1), 1, &command).unwrap(),
            AdmissionPreparation::Existing(OperationLookup::AcknowledgedRetired)
        ));
    }
}
#[test]
fn maximum_sequence_never_wraps() {
    let mut recovery = LedgerRecovery::empty(limits());
    recovery.streams.push(
        StreamRecord::new(
            binding(1),
            StreamPurpose::Ordinary,
            StreamStatus::Active,
            u64::MAX,
            u64::MAX,
            None,
        )
        .unwrap(),
    );
    let mut ledger = OperationLedger::recover(server(), recovery).unwrap();
    let context = TransactionContext::new(
        StoreRevision::new(1),
        OrganizationRevision::new(0),
        OrganizationRevision::new(0),
        None,
    )
    .unwrap();
    assert!(matches!(
        ledger
            .prepare_admission(
                &binding(1),
                op(binding(1), u64::MAX),
                &create(),
                PendingKind::AtomicMutation,
                context
            )
            .unwrap(),
        AdmissionPreparation::Existing(OperationLookup::AcknowledgedRetired)
    ));
    assert_class(
        TransactionContext::new(
            StoreRevision::new(u64::MAX),
            OrganizationRevision::new(0),
            OrganizationRevision::new(0),
            None,
        ),
        LedgerFailureClass::CounterExhausted,
    );
}
#[test]
fn canonical_request_has_fixed_binary_header_and_discriminants() {
    let request =
        CanonicalOperationRequest::new(op(binding(1), 1), create(), &CanonicalCodec).unwrap();
    let mut expected = CANONICAL_REQUEST_DOMAIN.to_vec();
    expected.extend_from_slice(&1u32.to_be_bytes());
    for _ in 0..3 {
        expected.extend_from_slice(&server().bytes());
        expected.extend_from_slice(&1u64.to_be_bytes());
    }
    expected.extend_from_slice(&1u64.to_be_bytes());
    expected.extend_from_slice(&1u16.to_be_bytes());
    expected.extend_from_slice(&server().bytes());
    expected.extend_from_slice(&1u16.to_be_bytes());
    expected.extend_from_slice(&server().bytes());
    expected.extend_from_slice(&1u16.to_be_bytes());
    assert_eq!(request.encode(&CanonicalCodec).unwrap(), expected);
}
#[test]
fn canonical_digest_binds_target_version_source_axis_and_confirmation() {
    let b = binding(1);
    let id = op(b, 1);
    let pane = PaneId::new(server(), 1).unwrap();
    let tab = TabId::new(server(), 1).unwrap();
    let original = Command::CreatePane {
        tab,
        source: pane,
        axis: SplitAxis::Horizontal,
        edge: SplitEdge::Before,
    };
    let digest = CanonicalCodec
        .canonical_request_digest(&b, id, &original)
        .unwrap();
    for variant in [
        Command::CreatePane {
            tab: TabId::new(server(), 2).unwrap(),
            source: pane,
            axis: SplitAxis::Horizontal,
            edge: SplitEdge::Before,
        },
        Command::CreatePane {
            tab,
            source: PaneId::new(server(), 2).unwrap(),
            axis: SplitAxis::Horizontal,
            edge: SplitEdge::Before,
        },
        Command::CreatePane {
            tab,
            source: pane,
            axis: SplitAxis::Vertical,
            edge: SplitEdge::Before,
        },
        Command::CreatePane {
            tab,
            source: pane,
            axis: SplitAxis::Horizontal,
            edge: SplitEdge::After,
        },
    ] {
        assert_ne!(
            digest,
            CanonicalCodec
                .canonical_request_digest(&b, id, &variant)
                .unwrap()
        );
    }
    let target = CloseTarget::Pane(pane);
    let assessment = AssessmentRef {
        operation: op(b, 2),
        digest: RequestDigest::from_bytes([3; 32]),
    };
    let close = Command::CloseConfirmed { target, assessment };
    let changed = Command::CloseConfirmed {
        target,
        assessment: AssessmentRef {
            digest: RequestDigest::from_bytes([4; 32]),
            ..assessment
        },
    };
    assert_ne!(
        CanonicalCodec
            .canonical_request_digest(&b, id, &close)
            .unwrap(),
        CanonicalCodec
            .canonical_request_digest(&b, id, &changed)
            .unwrap()
    );
    let mut changed = rename("draft");
    let original = CanonicalCodec
        .canonical_request_digest(&b, id, &changed)
        .unwrap();
    if let Command::RenameWorkspace {
        expected_version, ..
    } = &mut changed
    {
        *expected_version = FieldVersion::new(2);
    }
    assert_ne!(
        original,
        CanonicalCodec
            .canonical_request_digest(&b, id, &changed)
            .unwrap()
    );
}
#[test]
fn canonical_encoding_distinguishes_options_and_delimited_values() {
    let b = binding(1);
    let id = op(b, 1);
    let workspace = WorkspaceId::new(server(), 1).unwrap();
    let absent = Command::SetPinnedDirectory {
        workspace,
        expected_version: FieldVersion::new(1),
        value: None,
    };
    let present = Command::SetPinnedDirectory {
        workspace,
        expected_version: FieldVersion::new(1),
        value: Some(ServerDirectory::new(server(), b"/a".to_vec()).unwrap()),
    };
    assert_ne!(
        CanonicalCodec
            .canonical_request_digest(&b, id, &absent)
            .unwrap(),
        CanonicalCodec
            .canonical_request_digest(&b, id, &present)
            .unwrap()
    );
    assert!(ServerDirectory::new(server(), Vec::new()).is_err());
    assert_ne!(
        CanonicalCodec
            .canonical_request_digest(&b, id, &rename("ab"))
            .unwrap(),
        CanonicalCodec
            .canonical_request_digest(&b, id, &rename("a"))
            .unwrap()
    );
}
#[test]
fn canonical_remote_policy_orders_endpoint_sets_and_preserves_ipv6_scope() {
    use spaceterm_core::remote_access::RemoteAccessPolicy;
    use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
    let a: SocketAddr = "127.0.0.1:9000".parse().unwrap();
    let b = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 9001, 7, 3));
    let command = |endpoints| Command::SetRemoteAccess {
        server: server(),
        expected_version: FieldVersion::new(1),
        value: RemoteAccessPolicy::enabled(server(), endpoints).unwrap(),
    };
    let first =
        CanonicalOperationRequest::new(op(binding(1), 1), command(vec![a, b]), &CanonicalCodec)
            .unwrap();
    let second =
        CanonicalOperationRequest::new(op(binding(1), 1), command(vec![b, a]), &CanonicalCodec)
            .unwrap();
    assert_eq!(first.binding(), second.binding());
    let decoded =
        CanonicalOperationRequest::decode(&first.encode(&CanonicalCodec).unwrap(), &CanonicalCodec)
            .unwrap();
    let Command::SetRemoteAccess { value, .. } = decoded.command() else {
        panic!("remote policy")
    };
    assert!(value.endpoints().contains(&b));
}
#[test]
fn canonical_codec_rejects_trailing_unknown_and_truncated_input() {
    let bytes = CanonicalOperationRequest::new(op(binding(1), 1), create(), &CanonicalCodec)
        .unwrap()
        .encode(&CanonicalCodec)
        .unwrap();
    for end in 0..bytes.len() {
        assert!(CanonicalOperationRequest::decode(&bytes[..end], &CanonicalCodec).is_err());
    }
    let mut changed = bytes.clone();
    changed.push(0);
    assert!(CanonicalOperationRequest::decode(&changed, &CanonicalCodec).is_err());
    changed = bytes;
    changed[CANONICAL_REQUEST_DOMAIN.len() + 3] = 2;
    assert!(CanonicalOperationRequest::decode(&changed, &CanonicalCodec).is_err());
}

#[test]
fn canonical_codec_roundtrips_every_command_and_historical_outcome_family() {
    use spaceterm_core::control::*;
    use spaceterm_core::geometry::*;
    use spaceterm_core::pairing::*;
    use spaceterm_core::remote_access::RemoteAccessPolicy;
    use spaceterm_core::worktrees::types::*;
    let b = binding(1);
    let id = op(b, 1);
    let s = server();
    let workspace = WorkspaceId::new(s, 1).unwrap();
    let tab = TabId::new(s, 1).unwrap();
    let pane = PaneId::new(s, 1).unwrap();
    let worktree = WorktreeId::new(s, 1).unwrap();
    let epoch = ServerEpoch::new(1);
    let directory = ServerDirectory::new(s, b"/nonutf8\xff".to_vec()).unwrap();
    let scope = RepositoryScope {
        server: s,
        repository: RepositoryId::new(s, 1).unwrap(),
    };
    let assessment = AssessmentRef {
        operation: op(b, 2),
        digest: RequestDigest::from_bytes([4; 32]),
    };
    let target = CloseTarget::WorktreeTabs {
        workspace,
        worktree,
    };
    let grant = ControlGrant::new(pane, b.client(), ControlGeneration::new(2)).unwrap();
    let fence = EffectFence::new(
        SessionKey {
            epoch,
            pane,
            generation: SessionGeneration::new(1),
        },
        grant,
        ConnectionBinding::new(b.client(), ConnectionGeneration::new(3)),
        ContextGeneration::new(4),
        Some(ContentRevision::new(5)),
    )
    .unwrap();
    let grid = GridSize::new(
        24,
        80,
        CellPixels::new(8, 16).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    let intent = StartupIntent::new(id, fence, StartupSource::TabOpening { tab }).unwrap();
    let pairing_target = PairingTarget::new(PairingTargetParts {
        server: s,
        device: DeviceId::new([2; 16]),
        pairing: b.pairing(),
    })
    .unwrap();
    let ticket = EnrollmentTicketId::new(EnrollmentTicketParts {
        server: s,
        nonce: [3; 16],
    })
    .unwrap();
    let keys = PairingKeys::new(PairingKeysParts {
        device_identity_public_key: [5; 32],
        pairing_authentication_public_key: [6; 32],
    })
    .unwrap();
    let unpair = UnpairAssessment::new(UnpairAssessmentParts {
        reference: assessment,
        target: pairing_target.clone(),
        authorization_version: FieldVersion::new(1),
        clients: vec![ClientRecord {
            id: b.client(),
            pairing: b.pairing(),
        }],
    })
    .unwrap();
    let mut commands = vec![
        create(),
        Command::CreateTab {
            workspace,
            source: TabCreationSource::Directory(directory.clone()),
        },
        Command::CreatePane {
            tab,
            source: pane,
            axis: SplitAxis::Vertical,
            edge: SplitEdge::After,
        },
        rename("draft"),
        Command::SetPinnedDirectory {
            workspace,
            expected_version: FieldVersion::new(1),
            value: Some(directory.clone()),
        },
        Command::RepairPaneDirectory {
            pane,
            expected_version: FieldVersion::new(1),
            value: directory.clone(),
        },
        Command::RetryPaneDirectory {
            pane,
            expected_version: FieldVersion::new(1),
        },
        Command::SetWorktreeLocation {
            server: s,
            expected_version: FieldVersion::new(1),
            template: WorktreeLocationTemplate::new("../{branch}".into()).unwrap(),
        },
        Command::AssessClose { target },
        Command::CloseConfirmed { target, assessment },
        Command::KillWorkspaceConfirmed {
            workspace,
            assessment,
        },
        Command::AssessEndAllSessions { server: s },
        Command::EndAllSessions {
            server: s,
            assessment,
        },
        Command::CompareAcquire(CompareAcquireRequest {
            epoch,
            pane,
            client: b.client(),
            expected_generation: ControlGeneration::new(1),
        }),
        Command::TakeOver(TakeOverRequest {
            epoch,
            pane,
            client: b.client(),
        }),
        Command::ReleaseControl(ReleaseControlRequest { epoch, grant }),
        Command::BeginStartup(intent),
        Command::CompleteStartup(StartupMeasurement {
            intent: id,
            fence,
            grid,
        }),
        Command::CompleteMeasuredStartup(MeasuredStartupMeasurement {
            intent: id,
            fence,
            grid: grid.into(),
        }),
        Command::CancelStartup { intent: id },
        safety(),
        Command::RefreshWorktrees { scope },
        Command::CreateWorktree {
            scope,
            branch: BranchName::new("feature".into()).unwrap(),
        },
        Command::AssessRemoveWorktree { scope, worktree },
        Command::RemoveWorktreeConfirmed {
            scope,
            worktree,
            assessment,
            force: true,
        },
        Command::AssessKillWorkspace { workspace },
        Command::SetRemoteAccess {
            server: s,
            expected_version: FieldVersion::new(1),
            value: RemoteAccessPolicy::disabled(s),
        },
    ];
    for pairing in [
        PairingCommand::IssueDirectAuthorization {
            proposal: EnrollmentProposal::new(EnrollmentProposalParts {
                server: s,
                device: DeviceId::new([2; 16]),
                keys: keys.clone(),
                claim_nonce: [7; 32],
            })
            .unwrap(),
        },
        PairingCommand::Enroll {
            ticket: ticket.clone(),
            claim_digest: RequestDigest::from_bytes([7; 32]),
        },
        PairingCommand::AssessUnpair {
            target: pairing_target.clone(),
        },
        PairingCommand::UnpairConfirmed {
            assessment: unpair.clone(),
        },
        PairingCommand::SelfRevoke {
            target: pairing_target.clone(),
        },
        PairingCommand::BindAdditionalClient {
            target: pairing_target.clone(),
            expected_authorization_version: FieldVersion::new(1),
            claim_digest: RequestDigest::from_bytes([7; 32]),
        },
    ] {
        commands.push(Command::Pairing(pairing));
    }
    for (kind, remove) in [
        (GitAssessmentKind::Create, false),
        (GitAssessmentKind::Remove, true),
    ] {
        let assessment = GitAssessmentToken::new(GitAssessmentTokenParts {
            server: s,
            epoch,
            pairing: b.pairing(),
            client: b.client(),
            kind,
            random: [8; 32],
        })
        .unwrap();
        commands.push(Command::Worktree(if remove {
            GitCommand::RemoveConfirmed { assessment }
        } else {
            GitCommand::CreateConfirmed { assessment }
        }));
    }
    for command in commands {
        let request = CanonicalOperationRequest::new(id, command.clone(), &CanonicalCodec).unwrap();
        let bytes = request.encode(&CanonicalCodec).unwrap();
        assert_eq!(
            CanonicalOperationRequest::decode(&bytes, &CanonicalCodec)
                .unwrap()
                .command(),
            &command
        );
        assert_eq!(
            CanonicalCodec
                .canonical_request_digest(&b, id, &command)
                .unwrap(),
            request.binding().digest
        );
    }
    let control = GrantedControl::new(epoch, grant).unwrap();
    let clients = PairingClientStreams::new(PairingClientStreamsParts {
        client: ClientRecord {
            id: b.client(),
            pairing: b.pairing(),
        },
        ordinary: binding(2).stream(),
        self_revocation: binding(3).stream(),
    })
    .unwrap();
    let proof = |claims| {
        SignedPairingReceipt::new(SignedPairingReceiptParts {
            schema: 1,
            claims,
            canonical_body: vec![1, 2, 3],
            signature: vec![4; 64],
        })
        .unwrap()
    };
    let enrolled = proof(PairingReceiptBody::Enrollment {
        target: pairing_target.clone(),
        keys: keys.clone(),
        client: clients.clone(),
        enrollment: OperationBinding {
            id,
            digest: RequestDigest::from_bytes([7; 32]),
        },
        revision: StoreRevision::new(1),
    });
    let bound = proof(PairingReceiptBody::ClientBinding {
        target: pairing_target,
        keys,
        client: clients,
        operation: OperationBinding {
            id,
            digest: RequestDigest::from_bytes([7; 32]),
        },
        revision: StoreRevision::new(1),
    });
    let revoked = proof(PairingReceiptBody::Revocation {
        receipt: RevocationReceipt {
            server: s,
            pairing: b.pairing(),
            device: DeviceId::new([2; 16]),
            operation: id,
            revision: StoreRevision::new(1),
            receipt_digest: RequestDigest::from_bytes([9; 32]),
        },
        authentication_key_digest: RequestDigest::from_bytes([7; 32]),
        revocation_event: [8; 16],
    });
    let mut outcomes = vec![
        Outcome::Applied,
        Outcome::CreatedWorkspace {
            workspace,
            tab,
            pane,
        },
        Outcome::CreatedTab { tab, pane },
        Outcome::CreatedPane { pane },
        Outcome::CreatedWorktree { worktree },
        Outcome::ControlAcquired(grant),
        Outcome::ControlUnavailable(ControlRecord::Held {
            client: b.client(),
            generation: ControlGeneration::new(2),
        }),
        Outcome::StartupAwaitingMeasurement { intent: id },
        Outcome::SessionStarted(fence.session()),
        Outcome::Assessed {
            assessment,
            affected_panes: 1,
            running_sessions: 1,
        },
        Outcome::Failed(OperationFailure::Runtime(FailureClass::RuntimeFault)),
        Outcome::CreatedWithControl(
            CreatedOrganization::new(
                CreatedEntities::Workspace {
                    workspace,
                    tab,
                    pane,
                },
                control,
            )
            .unwrap(),
        ),
        Outcome::ControlGranted(control),
        Outcome::Assessment(Box::new(
            CloseAssessment::new(
                assessment,
                epoch,
                AssessmentSummary::new(AssessmentTarget::Close(target), 1, 1, 1, 1, true).unwrap(),
            )
            .unwrap(),
        )),
    ];
    for conflict in [
        FieldConflict::WorkspaceName {
            workspace,
            current_value: WorkspaceName {
                mode: NameMode::Automatic,
                display: NameText::new("current".into()).unwrap(),
            },
            current_version: FieldVersion::new(1),
        },
        FieldConflict::PinnedDirectory {
            workspace,
            current_value: None,
            current_version: FieldVersion::new(1),
        },
        FieldConflict::PaneDirectory {
            pane,
            current_value: directory,
            current_version: FieldVersion::new(1),
        },
        FieldConflict::WorktreeLocation {
            server: s,
            current_value: WorktreeLocationTemplate::new("../{branch}".into()).unwrap(),
            current_version: FieldVersion::new(1),
        },
        FieldConflict::RemoteAccess {
            server: s,
            current_value: RemoteAccessPolicy::disabled(s),
            current_version: FieldVersion::new(1),
        },
    ] {
        outcomes.push(Outcome::Conflict(conflict));
    }
    for result in [
        PairingOutcome::AuthorizationReserved {
            ticket,
            enrollment: id,
            claim_digest: RequestDigest::from_bytes([7; 32]),
        },
        PairingOutcome::Enrolled(enrolled),
        PairingOutcome::ClientBound(bound),
        PairingOutcome::UnpairAssessed(unpair),
        PairingOutcome::Revoked(revoked.clone()),
        PairingOutcome::AlreadyRevoked(revoked),
        PairingOutcome::Failed(PairingFailure::RecoveryRequired),
    ] {
        outcomes.push(Outcome::Pairing(Box::new(result)));
    }
    for result in [
        GitMutationOutcomeParts::Created {
            worktree,
            revision: OrganizationRevision::new(1),
        },
        GitMutationOutcomeParts::Removed {
            worktree,
            closed_tabs: 1,
            ended_sessions: 1,
            revision: OrganizationRevision::new(1),
        },
        GitMutationOutcomeParts::Failed {
            server: s,
            failure: GitFailure::RecoveryNeedsReview,
            ended_sessions: 1,
            external: ExternalDisposition::PartialOrAmbiguous,
        },
        GitMutationOutcomeParts::Recovered {
            server: s,
            observation: RecoveryObservation::Unavailable,
            revision: None,
        },
    ] {
        outcomes.push(Outcome::Git(GitMutationOutcome::new(result).unwrap()));
    }
    for outcome in outcomes {
        let bytes = CanonicalCodec.encode_compact_outcome(&outcome).unwrap();
        assert_eq!(
            CanonicalCodec.decode_compact_outcome(&bytes).unwrap(),
            outcome
        );
    }
}
