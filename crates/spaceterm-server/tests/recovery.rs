#[path = "support/job_a.rs"]
mod support;
use spaceterm_core::{authority::*, organization::*, storage::*};
use spaceterm_server::{api::*, contracts::ServerFailureClass, service::RecoveredServer};
use std::sync::atomic::Ordering;
use support::*;

#[tokio::test]
async fn recovery_opens_no_listener_before_validation_and_epoch_commit() {
    let fixture = Fixture::new();
    let db = fixture.db.clone();
    let host = fixture.host.clone();
    let gate = StoreGate::new();
    db.lock().unwrap().gate = Some(gate.clone());
    let task = tokio::spawn(RecoveredServer::recover(
        fixture.config,
        fixture.dependencies,
    ));
    gate.entered().await;
    assert!(!task.is_finished());
    assert_eq!(
        db.lock().unwrap().transactions[0].resulting_epoch,
        ServerEpoch::new(2)
    );
    gate.release();
    let server = task.await.unwrap().unwrap();
    assert_eq!(server.handle().phase(), ServicePhase::Ready);
    assert_eq!(server.handle().snapshot().epoch(), ServerEpoch::new(2));
    stop(&server.handle(), host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn locked_or_unknown_schema_never_replaces_identity() {
    for error in [StoreFailure::Locked, StoreFailure::UnknownSchema] {
        let fixture = Fixture::new();
        let db = fixture.db.clone();
        db.lock().unwrap().recovery_failure = Some(error);
        let result = RecoveredServer::recover(fixture.config, fixture.dependencies).await;
        assert!(result.is_err());
        let db = db.lock().unwrap();
        assert!(db.transactions.is_empty());
        assert_eq!(db.recovered.organization.server, server());
    }
}

#[tokio::test]
async fn failed_validation_never_commits_epoch_or_launches_shells() {
    let fixture = Fixture::new();
    fixture.validator.rejected.store(true, Ordering::SeqCst);
    let db = fixture.db.clone();
    let factory = fixture.factory.clone();
    assert!(
        RecoveredServer::recover(fixture.config, fixture.dependencies)
            .await
            .is_err()
    );
    assert!(db.lock().unwrap().transactions.is_empty());
    assert!(factory.starts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn native_startup_rejection_precedes_storage_and_credentials() {
    let fixture = Fixture::new();
    fixture.host.reject_start.store(true, Ordering::SeqCst);
    let db = fixture.db.clone();
    let error = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .err()
        .unwrap();
    assert_eq!(error.class, ServerFailureClass::NativeUnavailable);
    assert!(db.lock().unwrap().log.is_empty());
}

#[tokio::test]
async fn restart_restores_organization_without_launching_shells() {
    let fixture = Fixture::new();
    let factory = fixture.factory.clone();
    let host = fixture.host.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let snapshot = server.handle().snapshot();
    assert_eq!(snapshot.state().panes.len(), 1);
    assert_eq!(snapshot.runtime()[0].control, ControlRecord::Unclaimed);
    assert_eq!(
        snapshot.runtime()[0].phase,
        SessionPhase::Ended {
            reason: EndReason::ServerRestart
        }
    );
    assert!(factory.starts.lock().unwrap().is_empty());
    stop(&server.handle(), host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn indeterminate_commit_closes_admission_and_retains_original_identity() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let db = fixture.db.clone();
    let codec = fixture.codec.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let operation = request(
        &codec,
        1,
        1,
        spaceterm_core::operations::Command::RenameWorkspace {
            workspace: workspace(),
            expected_version: FieldVersion::new(1),
            draft: NameDraft::Custom(NameText::new("new".into()).unwrap()),
        },
    );
    let binding = operation.binding();
    db.lock().unwrap().next = Some(CommitDisposition::Indeterminate);
    let error = handle.submit(principal, operation).await.unwrap_err();
    assert_eq!(error.class, ServerFailureClass::RecoveryRequired);
    let result = server.wait().await.unwrap_err();
    assert_eq!(result.class, ServerFailureClass::RecoveryRequired);
    let db = db.lock().unwrap();
    assert_eq!(db.transactions.len(), 2);
    assert!(db.transactions[1].organization_writes.is_empty());
    assert!(
        matches!(&db.transactions[1].operation_writes[0],OperationWrite::Ledger(v) if matches!(v.as_ref(),spaceterm_core::operation_ledger_contracts::LedgerWrite::AdmitOperation(v) if v.binding==binding))
    );
}

fn retained_native_retirement() -> spaceterm_core::retirement::NativeRetirementDescriptor {
    use spaceterm_core::{native_management::*, operations::RequestDigest, retirement::*};
    let session = SessionKey {
        epoch: ServerEpoch::new(1),
        pane: pane(),
        generation: SessionGeneration::new(1),
    };
    NativeRetirementDescriptor::new(NativeRetirementDescriptorParts {
        request: NativeManagementRequestRef::new(
            server(),
            [7; 16],
            RequestDigest::from_bytes([8; 32]),
        )
        .unwrap(),
        batch: RetirementBatchId::new(server(), ServerEpoch::new(1), 1).unwrap(),
        action: NativeRetirementAction::Stop,
        acceptance_revision: OrganizationRevision::new(1),
        panes: vec![
            RetirementPane::new(workspace(), tab(), pane(), None, SessionGeneration::new(2))
                .unwrap(),
        ],
        sessions: vec![
            RetiredSession::new(
                session,
                RetirementReason::Stop,
                RetirementCleanupState::AwaitingEvidence,
            )
            .unwrap(),
        ],
    })
    .unwrap()
}

#[tokio::test]
async fn previous_instance_cleanup_requires_exact_session_reason_and_unique_confirmation() {
    use spaceterm_core::retirement::*;
    let descriptor = retained_native_retirement();
    let session = descriptor.sessions()[0].session();
    let correct = SessionCleanupObservation::new(
        session,
        RetirementReason::Stop,
        CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
    )
    .unwrap();
    let wrong_session = SessionCleanupObservation::new(
        SessionKey {
            generation: SessionGeneration::new(2),
            ..session
        },
        RetirementReason::Stop,
        correct.result(),
    )
    .unwrap();
    let wrong_reason =
        SessionCleanupObservation::new(session, RetirementReason::EndAllSessions, correct.result())
            .unwrap();
    let failed = SessionCleanupObservation::new(
        session,
        RetirementReason::Stop,
        CleanupResult::Failed(FailureClass::RuntimeFault),
    )
    .unwrap();
    for evidence in [
        vec![],
        vec![correct, correct],
        vec![wrong_session],
        vec![wrong_reason],
        vec![failed],
    ] {
        let fixture = Fixture::new();
        fixture
            .db
            .lock()
            .unwrap()
            .recovered
            .native_retirements
            .push(descriptor.clone());
        *fixture.host.cleanup.lock().unwrap() = evidence;
        let db = fixture.db.clone();
        let host = fixture.host.clone();
        let factory = fixture.factory.clone();
        let error = RecoveredServer::recover(fixture.config, fixture.dependencies)
            .await
            .err()
            .unwrap();
        assert_eq!(error.class, ServerFailureClass::CleanupIncomplete);
        assert_eq!(*host.requests.lock().unwrap(), vec![vec![session]]);
        assert!(db.lock().unwrap().transactions.is_empty());
        assert!(factory.starts.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn cleanup_evidence_alone_cannot_leave_retained_native_retirement_unsettled() {
    use spaceterm_core::retirement::*;
    let fixture = Fixture::new();
    let descriptor = retained_native_retirement();
    fixture.host.cleanup.lock().unwrap().push(
        SessionCleanupObservation::new(
            descriptor.sessions()[0].session(),
            RetirementReason::Stop,
            CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
        )
        .unwrap(),
    );
    fixture
        .db
        .lock()
        .unwrap()
        .recovered
        .native_retirements
        .push(descriptor);
    let db = fixture.db.clone();
    let result = RecoveredServer::recover(fixture.config, fixture.dependencies).await;
    assert_eq!(
        result.err().map(|error| error.class),
        Some(ServerFailureClass::RecoveryRequired)
    );
    assert!(db.lock().unwrap().transactions.is_empty());
}

#[tokio::test]
async fn runtime_owner_ceiling_is_exact_and_rejected_owner_cleanup_is_joined() {
    use spaceterm_core::{identity::PaneId, retirement::*, runtime_authority::*};
    use spaceterm_server::{
        adapters::terminal::RuntimeCleanupPort, runtime_registry::RuntimeRegistry,
    };
    use std::sync::{Arc, Mutex, atomic::AtomicBool};
    let fixture = Fixture::new();
    let cleanup: Arc<dyn RuntimeCleanupPort> = fixture.factory.clone();
    let mut registry = RuntimeRegistry::new(64, cleanup).unwrap();
    for n in 1..=65 {
        let session = SessionKey {
            epoch: ServerEpoch::new(2),
            pane: PaneId::new(support::server(), n).unwrap(),
            generation: SessionGeneration::new(1),
        };
        let authority = RuntimeAuthority::new(RuntimeAuthorityParts {
            session,
            grant: None,
            connection: None,
            context: ContextGeneration::new(1),
            focused_pane: false,
            terminal_input_focus: false,
            native_effects_eligible: false,
        })
        .unwrap();
        let terminal = SessionTerminalResult::new(
            session,
            RuntimeTerminalStatus::Retired {
                reason: RetirementReason::ServerRecovery,
            },
        )
        .unwrap();
        let confirmed = SessionCleanupObservation::new(
            session,
            RetirementReason::ServerRecovery,
            CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
        )
        .unwrap();
        let lifecycle = if n == 65 {
            RuntimeLifecycle::Running
        } else {
            RuntimeLifecycle::Finished(RuntimeCompletion::new(terminal, confirmed).unwrap())
        };
        let owner = Box::new(Runtime {
            released: fixture.factory.released.clone(),
            release_notice: fixture.factory.release_notice.clone(),
            state: Arc::new(Mutex::new(RuntimeState {
                authority,
                lifecycle,
                directory: None,
                close_work: None,
                cleanup: (n < 65).then_some(confirmed),
                retire_calls: 0,
            })),
            retirement_gate: Arc::new(Mutex::new(None)),
            barrier_gate: Arc::new(Mutex::new(None)),
            cleanup_failed: Arc::new(AtomicBool::new(false)),
        });
        if n <= 64 {
            assert!(registry.has_capacity(0));
            registry.insert(owner, authority).unwrap();
        } else {
            assert!(!registry.has_capacity(0));
            let gate = Gate::new();
            *fixture.factory.abandon_gate.lock().unwrap() = Some(gate.clone());
            assert_eq!(
                registry.insert(owner, authority).unwrap_err().class,
                ServerFailureClass::Capacity
            );
            let closed = tokio::spawn(registry.close());
            gate.entered().await;
            assert!(!closed.is_finished());
            assert_eq!(fixture.factory.abandoned.load(Ordering::SeqCst), 1);
            gate.release();
            closed.await.unwrap().unwrap();
            break;
        }
    }
}

#[tokio::test]
async fn malformed_durable_receipt_poisoning_finishes_recovery_shutdown_without_guessing() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let db = fixture.db.clone();
    let codec = fixture.codec.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    db.lock().unwrap().next = Some(CommitDisposition::Durable(CommitReceipt {
        store_revision: StoreRevision::new(999),
        organization_revision: OrganizationRevision::new(1),
    }));
    let operation = request(
        &codec,
        1,
        1,
        spaceterm_core::operations::Command::RenameWorkspace {
            workspace: workspace(),
            expected_version: FieldVersion::new(1),
            draft: NameDraft::Custom(NameText::new("not-published".into()).unwrap()),
        },
    );
    assert_eq!(
        handle.submit(principal, operation).await.unwrap_err().class,
        ServerFailureClass::RecoveryRequired
    );
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), server.wait())
            .await
            .unwrap()
            .unwrap_err()
            .class,
        ServerFailureClass::RecoveryRequired
    );
    assert_eq!(db.lock().unwrap().transactions.len(), 2);
}

#[tokio::test]
async fn one_retiring_owner_does_not_stall_other_retained_runtime_observations() {
    use spaceterm_core::{
        identity::PaneId, operation_ledger_contracts::CompactOutcome, operations::*, retirement::*,
        runtime_authority::*,
    };
    use spaceterm_server::runtime_registry::{OwnedRetirement, RuntimeRegistry};
    use std::sync::{Arc, Mutex, atomic::AtomicBool};
    let fixture = Fixture::new();
    let mut registry = RuntimeRegistry::new(2, fixture.factory.clone()).unwrap();
    let cleanup_gate = Gate::new();
    let mut states = Vec::new();
    for n in 1..=2 {
        let session = SessionKey {
            epoch: ServerEpoch::new(2),
            pane: PaneId::new(support::server(), n).unwrap(),
            generation: SessionGeneration::new(1),
        };
        let authority = RuntimeAuthority::new(RuntimeAuthorityParts {
            session,
            grant: None,
            connection: None,
            context: ContextGeneration::new(1),
            focused_pane: false,
            terminal_input_focus: false,
            native_effects_eligible: false,
        })
        .unwrap();
        let state = Arc::new(Mutex::new(RuntimeState {
            authority,
            lifecycle: RuntimeLifecycle::Running,
            directory: None,
            close_work: None,
            cleanup: None,
            retire_calls: 0,
        }));
        states.push(state.clone());
        registry
            .insert(
                Box::new(Runtime {
                    released: fixture.factory.released.clone(),
                    release_notice: fixture.factory.release_notice.clone(),
                    state,
                    retirement_gate: Arc::new(Mutex::new((n == 1).then(|| cleanup_gate.clone()))),
                    barrier_gate: Arc::new(Mutex::new(None)),
                    cleanup_failed: Arc::new(AtomicBool::new(false)),
                }),
                authority,
            )
            .unwrap();
    }
    let first = states[0].lock().unwrap().authority.session();
    let binding = OperationBinding {
        id: operation(1, 1),
        digest: RequestDigest::from_bytes([20; 32]),
    };
    let descriptor = RetirementDescriptor::new(RetirementDescriptorParts {
        batch: RetirementBatchId::new(support::server(), first.epoch, 1).unwrap(),
        binding,
        assessment: AssessmentRef {
            operation: binding.id,
            digest: binding.digest,
        },
        acceptance_revision: OrganizationRevision::new(2),
        target: AssessmentTarget::EndAllSessions(support::server()),
        panes: vec![
            RetirementPane::new(workspace(), tab(), pane(), None, SessionGeneration::new(2))
                .unwrap(),
        ],
        sessions: vec![
            RetiredSession::new(
                first,
                RetirementReason::EndAllSessions,
                RetirementCleanupState::AwaitingEvidence,
            )
            .unwrap(),
        ],
        completion: RetirementCompletion::AppliedAtAcceptanceRevision,
        eventual_result: CompactOutcome::new(&Outcome::Applied, fixture.codec.as_ref()).unwrap(),
    })
    .unwrap();
    let mut replies = registry
        .retire(OwnedRetirement::Operation(Arc::new(descriptor)), &[])
        .unwrap();
    cleanup_gate.entered().await;
    let second = states[1].lock().unwrap().authority.session();
    let terminal = SessionTerminalResult::new(
        second,
        RuntimeTerminalStatus::OrdinaryExit {
            exit_status: Some(0),
        },
    )
    .unwrap();
    let cleanup = SessionCleanupObservation::new(
        second,
        RetirementReason::OrdinaryExit,
        CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
    )
    .unwrap();
    {
        let mut state = states[1].lock().unwrap();
        state.lifecycle =
            RuntimeLifecycle::Finished(RuntimeCompletion::new(terminal, cleanup).unwrap());
        state.cleanup = Some(cleanup);
    }
    let inspected = tokio::spawn(registry.inspect());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !registry.observations().iter().any(|(session, facts)| {
            *session == second && facts.lifecycle.terminal_result() == Some(terminal)
        }) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!inspected.is_finished());
    cleanup_gate.release();
    replies.pop().unwrap().1.await.unwrap().unwrap();
    inspected.await.unwrap().unwrap();
    registry.close().await.unwrap();
}

#[tokio::test]
async fn confirmed_runtime_cleanup_retains_capacity_until_the_owner_task_is_joined() {
    use spaceterm_core::{retirement::*, runtime_authority::*};
    use spaceterm_server::runtime_registry::RuntimeRegistry;
    let fixture = Fixture::new();
    let session = SessionKey {
        epoch: ServerEpoch::new(2),
        pane: pane(),
        generation: SessionGeneration::new(1),
    };
    let authority = RuntimeAuthority::new(RuntimeAuthorityParts {
        session,
        grant: None,
        connection: None,
        context: ContextGeneration::new(1),
        focused_pane: false,
        terminal_input_focus: false,
        native_effects_eligible: false,
    })
    .unwrap();
    let terminal = SessionTerminalResult::new(
        session,
        RuntimeTerminalStatus::OrdinaryExit {
            exit_status: Some(0),
        },
    )
    .unwrap();
    let cleanup = SessionCleanupObservation::new(
        session,
        RetirementReason::OrdinaryExit,
        CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
    )
    .unwrap();
    let barrier = Gate::new();
    let owner = Box::new(Runtime {
        released: fixture.factory.released.clone(),
        release_notice: fixture.factory.release_notice.clone(),
        state: std::sync::Arc::new(std::sync::Mutex::new(RuntimeState {
            authority,
            lifecycle: RuntimeLifecycle::Finished(
                RuntimeCompletion::new(terminal, cleanup).unwrap(),
            ),
            directory: None,
            close_work: None,
            cleanup: Some(cleanup),
            retire_calls: 0,
        })),
        retirement_gate: std::sync::Arc::new(std::sync::Mutex::new(None)),
        barrier_gate: std::sync::Arc::new(std::sync::Mutex::new(Some(barrier.clone()))),
        cleanup_failed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });
    let mut registry = RuntimeRegistry::new(1, fixture.factory.clone()).unwrap();
    registry.insert(owner, authority).unwrap();
    let mut next = authority.parts();
    next.context = ContextGeneration::new(2);
    let applied = registry
        .transition(session, RuntimeAuthority::new(next).unwrap())
        .unwrap();
    barrier.entered().await;
    let joined = tokio::spawn(registry.release(session).unwrap().unwrap());
    assert!(registry.sessions().is_empty());
    assert!(registry.joins_pending());
    assert!(!registry.has_capacity(0));
    assert!(!joined.is_finished());
    barrier.release();
    applied.await.unwrap().unwrap();
    joined.await.unwrap().unwrap();
    assert!(!registry.has_capacity(0));
    registry.joined(session);
    assert!(!registry.joins_pending());
    assert!(registry.has_capacity(0));
    assert_eq!(
        fixture
            .factory
            .released
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    registry.close().await.unwrap();
}
