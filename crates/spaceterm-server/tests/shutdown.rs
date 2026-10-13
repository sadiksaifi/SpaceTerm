#[path = "support/job_a.rs"]
mod support;
use spaceterm_core::{
    authority::*, control::*, operations::*, organization::*, retirement::*, runtime_authority::*,
};
use spaceterm_server::{api::*, contracts::ServerFailureClass, service::RecoveredServer};
use std::sync::atomic::Ordering;
use support::*;

#[tokio::test]
async fn stop_requires_durable_stopped_receipt() {
    let fixture = Fixture::new();
    let host = fixture.host.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    host.reject_start.store(true, Ordering::SeqCst);
    assert_eq!(
        stop(&server.handle(), host.clone())
            .await
            .unwrap_err()
            .class,
        ServerFailureClass::NativeUnavailable
    );
    assert_eq!(server.handle().phase(), ServicePhase::Ready);
    host.reject_start.store(false, Ordering::SeqCst);
    stop(&server.handle(), host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn shutdown_cancels_reads_finishes_started_writes_and_reaps_sessions() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let external = fixture.external.clone();
    let factory = fixture.factory.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    start(&handle, principal, &codec, grant).await;
    let gate = Gate::new();
    *external.gate.lock().unwrap() = Some(gate.clone());
    let stop_handle = handle.clone();
    let shutdown = tokio::spawn(async move { stop(&stop_handle, host).await });
    gate.entered().await;
    assert_eq!(external.reads_cancelled.load(Ordering::SeqCst), 1);
    assert_eq!(
        factory.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .retire_calls,
        0
    );
    assert!(!shutdown.is_finished());
    gate.release();
    let report = shutdown.await.unwrap().unwrap();
    assert!(report.cleanup_confirmed);
    assert!(report.started_writes_finished);
    server.wait().await.unwrap();
    assert_eq!(
        factory.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .retire_calls,
        1
    );
}

#[tokio::test]
async fn cleanup_failure_is_not_reported_as_completed() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    start(&handle, principal, &codec, grant).await;
    factory.cleanup_failed.store(true, Ordering::SeqCst);
    assert_eq!(
        stop(&handle, host).await.unwrap_err().class,
        ServerFailureClass::CleanupIncomplete
    );
    assert!(server.wait().await.is_err());
}

#[tokio::test]
async fn dropping_handles_and_disconnecting_retain_runtime_and_grant() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    start(&handle, principal, &codec, grant).await;
    let disposable = handle.clone();
    drop(disposable);
    handle.disconnect(principal).await.unwrap();
    assert_eq!(
        handle.snapshot().runtime()[0].control,
        ControlRecord::Held {
            client: principal.client(),
            generation: grant.generation()
        }
    );
    assert_eq!(
        factory.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .retire_calls,
        0
    );
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn disconnect_is_processed_while_store_commit_is_in_flight() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let db = fixture.db.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let gate = StoreGate::new();
    db.lock().unwrap().gate = Some(gate.clone());
    let submit_handle = handle.clone();
    let operation = request(
        &codec,
        1,
        1,
        Command::RenameWorkspace {
            workspace: workspace(),
            expected_version: FieldVersion::new(1),
            draft: NameDraft::Custom(NameText::new("new".into()).unwrap()),
        },
    );
    let task = tokio::spawn(async move { submit_handle.submit(principal, operation).await });
    gate.entered().await;
    handle.disconnect(principal).await.unwrap();
    assert!(!task.is_finished());
    gate.release();
    task.await.unwrap().unwrap();
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn takeover_completion_waits_for_worker_barrier_and_no_early_publication() {
    let fixture = Fixture::new();
    let first = fixture.principal(1, 1);
    let second = fixture.principal(2, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, first).await;
    connect(&handle, second).await;
    let grant = acquire(&handle, first, &codec).await;
    start(&handle, first, &codec, grant).await;
    let pause = Gate::new();
    *factory.barrier_gate.lock().unwrap() = Some(pause.clone());
    let task_handle = handle.clone();
    let operation = request(
        &codec,
        2,
        1,
        Command::TakeOver(TakeOverRequest {
            epoch: second.epoch(),
            pane: pane(),
            client: second.client(),
        }),
    );
    let binding = operation.binding();
    let retry = operation.clone();
    let task = tokio::spawn(async move { task_handle.submit(second, operation).await });
    pause.entered().await;
    let install = Gate::new();
    *factory.barrier_gate.lock().unwrap() = Some(install.clone());
    pause.release();
    install.entered().await;
    assert!(!task.is_finished());
    assert_eq!(
        handle.lookup(second, binding).await.unwrap(),
        OperationResponse::Pending(binding)
    );
    assert_eq!(
        handle.submit(second, retry).await.unwrap(),
        OperationResponse::Pending(binding)
    );
    assert_eq!(
        handle.snapshot().runtime()[0].control,
        ControlRecord::Held {
            client: first.client(),
            generation: grant.generation()
        }
    );
    install.release();
    let result = task.await.unwrap().unwrap();
    assert!(matches!(
        result,
        OperationResponse::Completed {
            outcome: Outcome::ControlGranted(_),
            ..
        }
    ));
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn ordinary_exit_reduces_without_subscribers() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    let session = start(&handle, principal, &codec, grant).await;
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
    factory.states.lock().unwrap()[0].lock().unwrap().lifecycle =
        RuntimeLifecycle::Finished(RuntimeCompletion::new(terminal, cleanup).unwrap());
    factory.states.lock().unwrap()[0].lock().unwrap().cleanup = Some(cleanup);
    let mut snapshots = handle.subscribe_snapshot();
    handle.refresh_runtime_observations().await.unwrap();
    while !snapshots.borrow().state().workspaces.is_empty() {
        snapshots.changed().await.unwrap();
    }
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn reconnect_reinstalls_retained_authority_after_reconciliation() {
    let fixture = Fixture::new();
    let first = fixture.principal(1, 1);
    let later = fixture.principal(1, 2);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, first).await;
    let grant = acquire(&handle, first, &codec).await;
    let session = start(&handle, first, &codec, grant).await;
    handle.disconnect(first).await.unwrap();
    connect(&handle, later).await;
    let fence = EffectFence::new(
        session,
        grant,
        later.binding(),
        ContextGeneration::new(1),
        None,
    )
    .unwrap();
    assert_eq!(
        handle
            .runtime_command(
                later,
                spaceterm_terminal::runtime_contracts::FencedRuntimeCommand {
                    fence,
                    command: spaceterm_terminal::runtime_contracts::RuntimeCommand::FocusReport(
                        true
                    )
                }
            )
            .await
            .unwrap(),
        EffectDisposition::Applied
    );
    assert_eq!(factory.starts.lock().unwrap().len(), 1);
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn disconnect_during_start_generation_commit_starts_nothing() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    let launch = fixture.launch.clone();
    let db = fixture.db.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    let old = SessionKey {
        epoch: principal.epoch(),
        pane: pane(),
        generation: SessionGeneration::new(0),
    };
    let fence = EffectFence::new(
        old,
        grant,
        principal.binding(),
        ContextGeneration::new(1),
        None,
    )
    .unwrap();
    let intent =
        StartupIntent::new(operation(1, 2), fence, StartupSource::ExplicitSelection).unwrap();
    handle
        .submit(
            principal,
            request(&codec, 1, 2, Command::BeginStartup(intent)),
        )
        .await
        .unwrap();
    let prepared = Gate::new();
    *launch.gate.lock().unwrap() = Some(prepared.clone());
    let measured = spaceterm_core::geometry::MeasuredGrid::new(
        37,
        113,
        spaceterm_core::geometry::CellPixels::new(9, 18).unwrap(),
        spaceterm_core::geometry::SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    let operation = request(
        &codec,
        1,
        3,
        Command::CompleteMeasuredStartup(MeasuredStartupMeasurement {
            intent: intent.id(),
            fence,
            grid: measured,
        }),
    );
    let task_handle = handle.clone();
    let task = tokio::spawn(async move { task_handle.submit(principal, operation).await });
    prepared.entered().await;
    let committed = StoreGate::new();
    db.lock().unwrap().gate = Some(committed.clone());
    prepared.release();
    committed.entered().await;
    handle.disconnect(principal).await.unwrap();
    let mut snapshots = handle.subscribe_snapshot();
    committed.release();
    task.await.unwrap().unwrap();
    while snapshots
        .borrow()
        .runtime()
        .iter()
        .any(|v| matches!(v.phase, SessionPhase::Starting { .. }))
    {
        snapshots.changed().await.unwrap();
    }
    assert!(factory.starts.lock().unwrap().is_empty());
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn close_publishes_deletion_at_acceptance_and_completion_does_not_delete_twice() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    let db = fixture.db.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    start(&handle, principal, &codec, grant).await;
    let gate = Gate::new();
    *factory.retirement_gate.lock().unwrap() = Some(gate.clone());
    let operation = request(
        &codec,
        1,
        4,
        Command::CloseConfirmed {
            target: CloseTarget::Workspace(workspace()),
            assessment: AssessmentRef {
                operation: operation(1, 3),
                digest: RequestDigest::from_bytes([5; 32]),
            },
        },
    );
    let binding = operation.binding();
    let result = handle.submit(principal, operation).await.unwrap();
    assert_eq!(result, OperationResponse::Pending(binding));
    gate.entered().await;
    assert!(handle.snapshot().state().workspaces.is_empty());
    let acceptance = handle.snapshot().state().revision;
    gate.release();
    loop {
        if let OperationResponse::Completed {
            revision, outcome, ..
        } = handle.lookup(principal, binding).await.unwrap()
        {
            assert_eq!(revision, acceptance);
            assert_eq!(outcome, Outcome::Applied);
            break;
        }
        tokio::task::yield_now().await;
    }
    {
        let db = db.lock().unwrap();
        let completions:Vec<_>=db.transactions.iter().filter(|p|p.operation_writes.iter().any(|v|matches!(v,spaceterm_core::storage::OperationWrite::Ledger(v) if matches!(v.as_ref(),spaceterm_core::operation_ledger_contracts::LedgerWrite::CompleteOperation{record,..} if record.binding==binding)))).collect();
        assert_eq!(completions.len(), 1);
        assert!(completions[0].organization_writes.is_empty());
    }
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn overlapping_end_all_and_stop_share_cleanup_and_preserve_latest_ended_phase() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    let db = fixture.db.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    start(&handle, principal, &codec, grant).await;
    let gate = Gate::new();
    *factory.retirement_gate.lock().unwrap() = Some(gate.clone());
    let request = request(
        &codec,
        1,
        4,
        Command::EndAllSessions {
            server: support::server(),
            assessment: AssessmentRef {
                operation: operation(1, 3),
                digest: RequestDigest::from_bytes([5; 32]),
            },
        },
    );
    let binding = request.binding();
    handle.submit(principal, request).await.unwrap();
    gate.entered().await;
    let acceptance = handle.snapshot().state().revision;
    let mut snapshots = handle.subscribe_snapshot();
    let stop_handle = handle.clone();
    let shutdown = tokio::spawn(async move { stop(&stop_handle, host).await });
    while !snapshots.borrow().runtime().iter().any(|v| {
        v.phase
            == SessionPhase::Ended {
                reason: EndReason::Stop,
            }
    }) {
        snapshots.changed().await.unwrap();
    }
    assert_eq!(
        factory.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .retire_calls,
        1
    );
    assert!(!shutdown.is_finished());
    gate.release();
    shutdown.await.unwrap().unwrap();
    server.wait().await.unwrap();
    assert_eq!(
        handle.snapshot().runtime()[0].phase,
        SessionPhase::Ended {
            reason: EndReason::Stop
        }
    );
    let db = db.lock().unwrap();
    let record = db
        .transactions
        .iter()
        .flat_map(|p| p.operation_writes.iter())
        .find_map(|v| match v {
            spaceterm_core::storage::OperationWrite::Ledger(v) => match v.as_ref() {
                spaceterm_core::operation_ledger_contracts::LedgerWrite::CompleteOperation {
                    record,
                    ..
                } if record.binding == binding => Some(record),
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    assert_eq!(record.outcome_revision, acceptance);
}

#[tokio::test]
async fn reliable_capacity_rejection_precedes_ledger_admission() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let db = fixture.db.clone();
    let host = fixture.host.clone();
    let outcomes = fixture.outcomes.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    outcomes.rejected.store(true, Ordering::SeqCst);
    let request = request(
        &codec,
        1,
        1,
        Command::RenameWorkspace {
            workspace: workspace(),
            expected_version: FieldVersion::new(1),
            draft: NameDraft::Custom(NameText::new("rejected".into()).unwrap()),
        },
    );
    assert_eq!(
        handle.submit(principal, request).await.unwrap_err().class,
        ServerFailureClass::Capacity
    );
    assert_eq!(db.lock().unwrap().transactions.len(), 1);
    assert_eq!(outcomes.publications.load(Ordering::SeqCst), 0);
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn blocked_admission_has_no_reduction_publication_or_success() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let db = fixture.db.clone();
    let host = fixture.host.clone();
    let outcomes = fixture.outcomes.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let before = handle.snapshot();
    let gate = StoreGate::new();
    db.lock().unwrap().gate = Some(gate.clone());
    let request = request(
        &codec,
        1,
        1,
        Command::RenameWorkspace {
            workspace: workspace(),
            expected_version: FieldVersion::new(1),
            draft: NameDraft::Custom(NameText::new("committed".into()).unwrap()),
        },
    );
    let submitted = handle.clone();
    let task = tokio::spawn(async move { submitted.submit(principal, request).await });
    gate.entered().await;
    assert!(!task.is_finished());
    assert_eq!(handle.snapshot().state(), before.state());
    assert_eq!(outcomes.publications.load(Ordering::SeqCst), 0);
    assert!(
        db.lock().unwrap().transactions[1]
            .organization_writes
            .is_empty()
    );
    gate.release();
    assert!(matches!(
        task.await.unwrap().unwrap(),
        OperationResponse::Completed { .. }
    ));
    assert_eq!(outcomes.publications.load(Ordering::SeqCst), 1);
    assert_eq!(
        handle.snapshot().state().revision,
        before.state().revision.checked_successor().unwrap()
    );
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn shell_directory_confirmation_survives_disconnect_and_notifies_after_commit_once() {
    use spaceterm_core::observations::*;
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let db = fixture.db.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    let observer = fixture.observer.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    start(&handle, principal, &codec, grant).await;
    handle.disconnect(principal).await.unwrap();
    let state = factory.states.lock().unwrap()[0].clone();
    let session = state.lock().unwrap().authority.session();
    let directory =
        ServerDirectory::new(support::server(), b"/test-only/confirmed".to_vec()).unwrap();
    let gate = StoreGate::new();
    db.lock().unwrap().gate = Some(gate.clone());
    state.lock().unwrap().directory = Some(
        ConfirmedDirectoryObservation::new(
            session,
            RuntimeObservationSequence::new(1).unwrap(),
            MonotonicTick::new(23),
            directory.clone(),
        )
        .unwrap(),
    );
    gate.entered().await;
    assert_eq!(observer.directories.load(Ordering::SeqCst), 0);
    assert!(
        handle.snapshot().state().panes[0]
            .confirmed_restoration_directory
            .value
            .is_none()
    );
    gate.release();
    let mut snapshots = handle.subscribe_snapshot();
    loop {
        if snapshots.borrow().state().panes[0]
            .confirmed_restoration_directory
            .value
            == Some(directory.clone())
        {
            break;
        }
        snapshots.changed().await.unwrap();
    }
    while observer.directories.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    loop {
        match handle.refresh_runtime_observations().await {
            Ok(()) => break,
            Err(error) if error.class == ServerFailureClass::Capacity => {
                tokio::task::yield_now().await
            }
            Err(error) => panic!("{error:?}"),
        }
    }
    assert_eq!(observer.directories.load(Ordering::SeqCst), 1);
    assert_eq!(
        db.lock().unwrap().recovered.organization.panes[0]
            .confirmed_restoration_directory
            .value,
        Some(directory)
    );
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn stop_neutralizes_worker_authority_while_started_writes_are_draining() {
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let external = fixture.external.clone();
    let factory = fixture.factory.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    start(&handle, principal, &codec, grant).await;
    let writes = Gate::new();
    let barrier = Gate::new();
    *external.gate.lock().unwrap() = Some(writes.clone());
    *factory.barrier_gate.lock().unwrap() = Some(barrier.clone());
    let stopped = handle.clone();
    let shutdown = tokio::spawn(async move { stop(&stopped, host).await });
    writes.entered().await;
    tokio::time::timeout(std::time::Duration::from_secs(2), barrier.entered())
        .await
        .unwrap();
    assert!(
        factory.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .authority
            .parts()
            .connection
            .is_none()
    );
    assert_eq!(
        factory.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .retire_calls,
        0
    );
    barrier.release();
    writes.release();
    shutdown.await.unwrap().unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn started_git_finishes_durable_progress_and_settlement_during_stop_without_new_admission() {
    use spaceterm_core::{
        identity::RepositoryId,
        storage::*,
        worktrees::{intent::*, types::ExternalDisposition},
    };
    let fixture = Fixture::new();
    {
        let mut db = fixture.db.lock().unwrap();
        db.recovered.high_water.repository = 1;
        db.recovered.high_water.worktree = 1;
    }
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let db = fixture.db.clone();
    let external = fixture.external.clone();
    let factory = fixture.factory.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let command = request(
        &codec,
        1,
        1,
        Command::CreateWorktree {
            scope: RepositoryScope {
                server: support::server(),
                repository: RepositoryId::new(support::server(), 1).unwrap(),
            },
            branch: BranchName::new("test".into()).unwrap(),
        },
    );
    let binding = command.binding();
    assert_eq!(
        handle.submit(principal, command).await.unwrap(),
        OperationResponse::Pending(binding)
    );
    let intent = git_intent(binding, GitIntentPhase::Prepared);
    let next_store = db
        .lock()
        .unwrap()
        .recovered
        .store_revision
        .checked_successor()
        .unwrap();
    let receipt = handle
        .record_external_progress(
            binding,
            git_progress(&intent, next_store),
            vec![OperationWrite::GitIntent(GitIntentMutation::Insert(
                Box::new(intent.clone()),
            ))],
        )
        .await
        .unwrap();
    let started = git_intent(binding, GitIntentPhase::Started);
    let receipt = handle
        .record_external_progress(
            binding,
            git_progress(
                &started,
                receipt.store_revision.checked_successor().unwrap(),
            ),
            vec![OperationWrite::GitIntent(GitIntentMutation::Advance {
                binding,
                expected_phase: GitIntentPhase::Prepared,
                next: GitIntentPhase::Started,
                result: None,
            })],
        )
        .await
        .unwrap();
    let gate = Gate::new();
    *external.gate.lock().unwrap() = Some(gate.clone());
    let stopped = handle.clone();
    let shutdown = tokio::spawn(async move { stop(&stopped, host).await });
    gate.entered().await;
    let result = RetainedExternalResult {
        disposition: ExternalDisposition::DesiredStateObserved,
        failure: None,
        ended_sessions: 0,
    };
    let mut parts = started.parts().clone();
    parts.phase = GitIntentPhase::ExternalFinished;
    parts.external_result = Some(result.clone());
    let finished = GitWriteIntent::new(parts).unwrap();
    handle
        .record_external_progress(
            binding,
            git_progress(
                &finished,
                receipt.store_revision.checked_successor().unwrap(),
            ),
            vec![OperationWrite::GitIntent(GitIntentMutation::Advance {
                binding,
                expected_phase: GitIntentPhase::Started,
                next: GitIntentPhase::ExternalFinished,
                result: Some(result.clone()),
            })],
        )
        .await
        .unwrap();
    assert_eq!(
        handle
            .settle_started_git(intent, result.clone())
            .await
            .unwrap_err()
            .class,
        ServerFailureClass::StaleAuthority
    );
    let response = handle
        .settle_started_git(finished.clone(), result.clone())
        .await
        .unwrap();
    assert!(matches!(response, OperationResponse::Completed { .. }));
    assert_eq!(
        handle.settle_started_git(finished, result).await.unwrap(),
        response
    );
    assert!(factory.starts.lock().unwrap().is_empty());
    assert_eq!(
        db.lock()
            .unwrap()
            .log
            .iter()
            .filter(|entry| **entry == "git settlement")
            .count(),
        1
    );
    gate.release();
    shutdown.await.unwrap().unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn revoked_creator_started_git_settles_to_unclaimed_without_launch() {
    use spaceterm_core::{
        identity::RepositoryId,
        storage::*,
        worktrees::{intent::*, types::ExternalDisposition},
    };
    let fixture = Fixture::new();
    {
        let mut db = fixture.db.lock().unwrap();
        db.recovered.high_water.repository = 1;
        db.recovered.high_water.worktree = 1;
    }
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let db = fixture.db.clone();
    let factory = fixture.factory.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    acquire(&handle, principal, &codec).await;
    let command = request(
        &codec,
        1,
        2,
        Command::CreateWorktree {
            scope: RepositoryScope {
                server: support::server(),
                repository: RepositoryId::new(support::server(), 1).unwrap(),
            },
            branch: BranchName::new("test".into()).unwrap(),
        },
    );
    let binding = command.binding();
    handle.submit(principal, command).await.unwrap();
    let intent = git_intent(binding, GitIntentPhase::Prepared);
    let next_store = db
        .lock()
        .unwrap()
        .recovered
        .store_revision
        .checked_successor()
        .unwrap();
    let receipt = handle
        .record_external_progress(
            binding,
            git_progress(&intent, next_store),
            vec![OperationWrite::GitIntent(GitIntentMutation::Insert(
                Box::new(intent.clone()),
            ))],
        )
        .await
        .unwrap();
    let started = git_intent(binding, GitIntentPhase::Started);
    handle
        .record_external_progress(
            binding,
            git_progress(
                &started,
                receipt.store_revision.checked_successor().unwrap(),
            ),
            vec![OperationWrite::GitIntent(GitIntentMutation::Advance {
                binding,
                expected_phase: GitIntentPhase::Prepared,
                next: GitIntentPhase::Started,
                result: None,
            })],
        )
        .await
        .unwrap();
    handle.revoke_clients(vec![client(1)]).await.unwrap();
    let result = RetainedExternalResult {
        disposition: ExternalDisposition::DesiredStateObserved,
        failure: None,
        ended_sessions: 0,
    };
    assert!(matches!(
        handle.settle_started_git(started, result).await.unwrap(),
        OperationResponse::Completed { .. }
    ));
    assert_eq!(
        handle.snapshot().runtime()[0].control,
        ControlRecord::Unclaimed
    );
    assert!(matches!(
        handle.snapshot().runtime()[0].phase,
        SessionPhase::Ended {
            reason: EndReason::ServerRestart
        }
    ));
    assert!(factory.starts.lock().unwrap().is_empty());
    assert_eq!(
        handle.lookup(principal, binding).await.unwrap_err().class,
        ServerFailureClass::Authentication
    );
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn unavailable_git_extension_requests_preserve_another_operations_staged_candidate() {
    use spaceterm_core::{
        identity::RepositoryId,
        storage::*,
        worktrees::{intent::*, types::ExternalDisposition},
    };
    let fixture = Fixture::new();
    {
        let mut db = fixture.db.lock().unwrap();
        db.recovered.high_water.repository = 1;
        db.recovered.high_water.worktree = 1;
    }
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let db = fixture.db.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let git = request(
        &codec,
        1,
        1,
        Command::CreateWorktree {
            scope: RepositoryScope {
                server: support::server(),
                repository: RepositoryId::new(support::server(), 1).unwrap(),
            },
            branch: BranchName::new("test".into()).unwrap(),
        },
    );
    let binding = git.binding();
    handle.submit(principal, git).await.unwrap();
    let prepared = git_intent(binding, GitIntentPhase::Prepared);
    let next_store = db
        .lock()
        .unwrap()
        .recovered
        .store_revision
        .checked_successor()
        .unwrap();
    let receipt = handle
        .record_external_progress(
            binding,
            git_progress(&prepared, next_store),
            vec![OperationWrite::GitIntent(GitIntentMutation::Insert(
                Box::new(prepared),
            ))],
        )
        .await
        .unwrap();
    let started = git_intent(binding, GitIntentPhase::Started);
    handle
        .record_external_progress(
            binding,
            git_progress(
                &started,
                receipt.store_revision.checked_successor().unwrap(),
            ),
            vec![OperationWrite::GitIntent(GitIntentMutation::Advance {
                binding,
                expected_phase: GitIntentPhase::Prepared,
                next: GitIntentPhase::Started,
                result: None,
            })],
        )
        .await
        .unwrap();

    let admission = StoreGate::new();
    db.lock().unwrap().gate = Some(admission.clone());
    let rename = request(
        &codec,
        1,
        2,
        Command::RenameWorkspace {
            workspace: workspace(),
            expected_version: FieldVersion::new(1),
            draft: NameDraft::Custom(NameText::new("renamed".into()).unwrap()),
        },
    );
    let retry = rename.clone();
    let before = handle.snapshot();
    let mutation_handle = handle.clone();
    let mutation = tokio::spawn(async move { mutation_handle.submit(principal, rename).await });
    admission.entered().await;
    let commit = StoreGate::new();
    db.lock().unwrap().gate = Some(commit.clone());
    admission.release();
    commit.entered().await;
    assert_eq!(handle.snapshot(), before);
    assert!(!mutation.is_finished());

    let allocation_error = handle
        .reserve_git_allocation(
            binding,
            GitAllocationNeeds {
                repository: false,
                worktree: true,
                fallbacks: vec![],
            },
        )
        .await
        .err()
        .unwrap();
    let settlement_error = handle
        .settle_started_git(
            started,
            RetainedExternalResult {
                disposition: ExternalDisposition::DesiredStateObserved,
                failure: None,
                ended_sessions: 0,
            },
        )
        .await
        .unwrap_err();
    commit.release();
    let response = mutation.await.unwrap().unwrap();
    assert_eq!(allocation_error.class, ServerFailureClass::Capacity);
    assert_eq!(settlement_error.class, ServerFailureClass::Capacity);
    let after = handle.snapshot();
    assert_eq!(
        after.state().workspaces[0].name.value.display.as_str(),
        "renamed"
    );
    assert_eq!(
        after.state().workspaces[0].name.version,
        FieldVersion::new(2)
    );
    assert_eq!(
        after.state().revision,
        before.state().revision.checked_successor().unwrap()
    );
    assert_eq!(db.lock().unwrap().recovered.organization, *after.state());
    let commits = db.lock().unwrap().transactions.len();
    assert_eq!(handle.submit(principal, retry).await.unwrap(), response);
    assert_eq!(db.lock().unwrap().transactions.len(), commits);
    assert_eq!(handle.snapshot(), after);
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn git_allocation_requires_admission_and_one_durable_reducer_reservation() {
    use spaceterm_core::{identity::RepositoryId, storage::*};
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let db = fixture.db.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let command = request(
        &codec,
        1,
        1,
        Command::CreateWorktree {
            scope: RepositoryScope {
                server: support::server(),
                repository: RepositoryId::new(support::server(), 1).unwrap(),
            },
            branch: BranchName::new("test".into()).unwrap(),
        },
    );
    let binding = command.binding();
    assert_eq!(
        handle
            .reserve_git_allocation(
                binding,
                GitAllocationNeeds {
                    repository: true,
                    worktree: true,
                    fallbacks: vec![]
                }
            )
            .await
            .err()
            .unwrap()
            .class,
        ServerFailureClass::StaleAuthority
    );
    assert_eq!(db.lock().unwrap().transactions.len(), 1);
    handle.submit(principal, command).await.unwrap();
    let reserved = handle
        .reserve_git_allocation(
            binding,
            GitAllocationNeeds {
                repository: true,
                worktree: true,
                fallbacks: vec![],
            },
        )
        .await
        .unwrap();
    assert_eq!(reserved.reservation.binding(), binding);
    assert_eq!(
        reserved.reservation.parts().resulting_high_water.repository,
        1
    );
    assert_eq!(
        reserved.reservation.parts().resulting_high_water.worktree,
        1
    );
    {
        let db = db.lock().unwrap();
        let transaction = db.transactions.last().unwrap();
        assert_eq!(
            transaction.resulting_store_revision,
            reserved.receipt.store_revision
        );
        assert_eq!(
            transaction.resulting_high_water,
            reserved.reservation.parts().resulting_high_water
        );
        assert!(matches!(
            transaction.operation_writes.as_slice(),
            [OperationWrite::GitAllocation(_)]
        ));
    }
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn remote_listener_generation_changes_only_after_its_policy_commit() {
    use spaceterm_core::{organization_retained::*, remote_access::*, storage::*};
    use spaceterm_server::contracts::{ServerFailure, ServerFuture};
    use std::sync::{Arc, Mutex, atomic::AtomicUsize};
    struct Listeners {
        opened: tokio::sync::Notify,
        applied: Mutex<Vec<(Versioned<RemoteAccessPolicy>, CommitReceipt)>>,
        closed: AtomicUsize,
    }
    impl ListenerLifecyclePort for Listeners {
        fn open(
            &self,
            _: ServerHandle,
            _: Arc<OrganizationSnapshot>,
        ) -> ServerFuture<'_, Result<(), ServerFailure>> {
            self.opened.notify_one();
            Box::pin(async { Ok(()) })
        }
        fn apply_remote_policy(
            &self,
            policy: Versioned<RemoteAccessPolicy>,
            receipt: CommitReceipt,
        ) -> ServerFuture<'_, Result<(), ServerFailure>> {
            self.applied.lock().unwrap().push((policy, receipt));
            Box::pin(async { Ok(()) })
        }
        fn close(&self) -> ServerFuture<'_, Result<(), ServerFailure>> {
            self.closed.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }
    let fixture = Fixture::new();
    {
        let mut db = fixture.db.lock().unwrap();
        let state = &mut db.recovered.organization;
        state.workspaces.clear();
        state.tabs.clear();
        state.panes.clear();
        state.retained = Some(
            OrganizationRetainedFacts::new(OrganizationRetainedParts {
                server: support::server(),
                workspace_names: vec![],
                creation_defaults: vec![],
                pane_generations: vec![],
                tab_membership_versions: vec![],
                repository_inventory_versions: vec![],
                worktree_inventory: vec![],
                remote_access: Versioned {
                    version: FieldVersion::new(1),
                    value: RemoteAccessPolicy::disabled(support::server()),
                },
            })
            .unwrap(),
        );
    }
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let db = fixture.db.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    let listeners = Arc::new(Listeners {
        opened: tokio::sync::Notify::new(),
        applied: Mutex::new(vec![]),
        closed: AtomicUsize::new(0),
    });
    let running = tokio::spawn(server.run(ListenerFactories {
        lifecycle: listeners.clone(),
    }));
    listeners.opened.notified().await;
    connect(&handle, principal).await;
    let policy = RemoteAccessPolicy::enabled(
        support::server(),
        vec![
            "127.0.0.1:1122".parse().unwrap(),
            "[::1]:1122".parse().unwrap(),
        ],
    )
    .unwrap();
    let operation = request(
        &codec,
        1,
        1,
        Command::SetRemoteAccess {
            server: support::server(),
            expected_version: FieldVersion::new(1),
            value: policy.clone(),
        },
    );
    let gate = StoreGate::new();
    db.lock().unwrap().gate = Some(gate.clone());
    let submitted = handle.clone();
    let result = tokio::spawn(async move { submitted.submit(principal, operation).await });
    gate.entered().await;
    assert!(listeners.applied.lock().unwrap().is_empty());
    gate.release();
    assert!(matches!(
        result.await.unwrap().unwrap(),
        OperationResponse::Completed { .. }
    ));
    {
        let applied = listeners.applied.lock().unwrap();
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].0.version, FieldVersion::new(2));
        assert_eq!(applied[0].0.value, policy);
        assert_eq!(
            applied[0].1.store_revision,
            db.lock().unwrap().recovered.store_revision
        );
    }
    stop(&handle, host).await.unwrap();
    running.await.unwrap().unwrap();
    assert!(listeners.closed.load(Ordering::SeqCst) > 0);
}

#[tokio::test]
async fn definite_control_not_applied_restores_only_current_owner_with_fresh_context_barrier() {
    use spaceterm_core::storage::*;
    use spaceterm_terminal::runtime_contracts::{FencedRuntimeCommand, RuntimeCommand};
    let fixture = Fixture::new();
    let first = fixture.principal(1, 1);
    let second = fixture.principal(2, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    let db = fixture.db.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, first).await;
    connect(&handle, second).await;
    let grant = acquire(&handle, first, &codec).await;
    let session = start(&handle, first, &codec, grant).await;
    let old = handle.runtime_access(first, session).await.unwrap();
    let old_fence = EffectFence::new(
        session,
        grant,
        first.binding(),
        old.authority.parts().context,
        None,
    )
    .unwrap();
    let pause = Gate::new();
    *factory.barrier_gate.lock().unwrap() = Some(pause.clone());
    let submitted = handle.clone();
    let operation = request(
        &codec,
        2,
        1,
        Command::TakeOver(TakeOverRequest {
            epoch: second.epoch(),
            pane: pane(),
            client: second.client(),
        }),
    );
    let task = tokio::spawn(async move { submitted.submit(second, operation).await });
    pause.entered().await;
    db.lock().unwrap().next = Some(CommitDisposition::NotApplied(StoreFailure::Io));
    let restore = Gate::new();
    *factory.barrier_gate.lock().unwrap() = Some(restore.clone());
    pause.release();
    restore.entered().await;
    assert!(!task.is_finished());
    assert_eq!(
        handle.snapshot().runtime()[0].control,
        ControlRecord::Held {
            client: first.client(),
            generation: grant.generation()
        }
    );
    assert_eq!(
        handle
            .runtime_access(first, session)
            .await
            .err()
            .unwrap()
            .class,
        ServerFailureClass::StaleAuthority
    );
    restore.release();
    assert_eq!(
        task.await.unwrap().unwrap_err().class,
        ServerFailureClass::StorageUnavailable
    );
    let current = handle.runtime_access(first, session).await.unwrap();
    assert!(current.authority.parts().context > old.authority.parts().context);
    let discarded = old
        .handle
        .try_submit(FencedRuntimeCommand {
            fence: old_fence,
            command: RuntimeCommand::FocusReport(true),
        })
        .unwrap()
        .await
        .unwrap();
    assert_eq!(discarded, EffectDisposition::StaleAuthority);
    assert_eq!(
        handle
            .runtime_access(second, session)
            .await
            .err()
            .unwrap()
            .class,
        ServerFailureClass::StaleAuthority
    );
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn client_focus_is_ephemeral_worker_fenced_and_cannot_start_a_session() {
    use spaceterm_terminal::runtime_contracts::{FencedRuntimeCommand, RuntimeCommand};
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    let session = start(&handle, principal, &codec, grant).await;
    let snapshot = handle.snapshot();
    let barrier = Gate::new();
    *factory.barrier_gate.lock().unwrap() = Some(barrier.clone());
    let updated = handle.clone();
    let task = tokio::spawn(async move {
        updated
            .update_client_focus(
                principal,
                ClientFocusUpdate {
                    context: ContextGeneration::new(2),
                    focused_pane: Some(pane()),
                    terminal_input_focus: true,
                    native_effects_eligible: true,
                },
            )
            .await
    });
    barrier.entered().await;
    assert!(!task.is_finished());
    assert_eq!(handle.snapshot().state(), snapshot.state());
    assert_eq!(factory.starts.lock().unwrap().len(), 1);
    let old_fence = EffectFence::new(
        session,
        grant,
        principal.binding(),
        ContextGeneration::new(1),
        None,
    )
    .unwrap();
    assert_eq!(
        handle
            .runtime_command(
                principal,
                FencedRuntimeCommand {
                    fence: old_fence,
                    command: RuntimeCommand::FocusReport(true)
                }
            )
            .await
            .unwrap_err()
            .class,
        ServerFailureClass::StaleAuthority
    );
    barrier.release();
    assert_eq!(task.await.unwrap().unwrap(), ContextGeneration::new(2));
    let access = handle.runtime_access(principal, session).await.unwrap();
    let parts = access.authority.parts();
    assert!(parts.focused_pane && parts.terminal_input_focus && parts.native_effects_eligible);
    assert_eq!(parts.context, ContextGeneration::new(2));
    assert_eq!(
        handle
            .update_client_focus(
                principal,
                ClientFocusUpdate {
                    context: ContextGeneration::new(1),
                    focused_pane: None,
                    terminal_input_focus: false,
                    native_effects_eligible: false
                }
            )
            .await
            .unwrap_err()
            .class,
        ServerFailureClass::StaleAuthority
    );
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn started_git_retirement_remains_pending_and_shares_cleanup_with_stop() {
    use spaceterm_core::{
        identity::*,
        operation_ledger_contracts::*,
        storage::*,
        worktrees::{intent::*, types::ExternalDisposition},
    };
    let fixture = Fixture::new();
    let worktree = WorktreeId::new(support::server(), 1).unwrap();
    let scope = RepositoryScope {
        server: support::server(),
        repository: RepositoryId::new(support::server(), 1).unwrap(),
    };
    {
        let mut db = fixture.db.lock().unwrap();
        db.recovered.high_water.repository = 1;
        db.recovered.high_water.worktree = 1;
        let directory = db.recovered.organization.panes[0]
            .starting_directory
            .clone();
        db.recovered
            .organization
            .repositories
            .push(RepositoryRecord {
                id: scope.repository,
                directory: directory.clone(),
            });
        db.recovered.organization.worktrees.push(WorktreeRecord {
            id: worktree,
            repository: scope.repository,
            directory,
            main: false,
            missing: false,
        });
        db.recovered.organization.tabs[0].worktree = Some(worktree);
        use spaceterm_core::{organization_retained::*, remote_access::RemoteAccessPolicy};
        db.recovered.organization.retained = Some(
            OrganizationRetainedFacts::new(OrganizationRetainedParts {
                server: support::server(),
                workspace_names: vec![WorkspaceNameBasis::new(
                    workspace(),
                    NameText::new("test".into()).unwrap(),
                    NameText::new("test".into()).unwrap(),
                )],
                creation_defaults: vec![TabCreationDefault::new(tab(), vec![pane()]).unwrap()],
                pane_generations: vec![PaneGenerationHighWater::new(
                    pane(),
                    SessionGeneration::new(0),
                    ControlGeneration::new(0),
                )],
                tab_membership_versions: vec![TabMembershipVersion::new(
                    tab(),
                    FieldVersion::new(1),
                )],
                repository_inventory_versions: vec![RepositoryInventoryVersion::new(
                    scope.repository,
                    FieldVersion::new(1),
                )],
                worktree_inventory: vec![WorktreeInventoryFacts::new(
                    worktree,
                    true,
                    FieldVersion::new(1),
                )],
                remote_access: Versioned {
                    version: FieldVersion::new(1),
                    value: RemoteAccessPolicy::disabled(support::server()),
                },
            })
            .unwrap(),
        );
    }
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let db = fixture.db.clone();
    let factory = fixture.factory.clone();
    let external = fixture.external.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    let session = start(&handle, principal, &codec, grant).await;
    let assessment = AssessmentRef {
        operation: operation(1, 4),
        digest: RequestDigest::from_bytes([14; 32]),
    };
    let command = request(
        &codec,
        1,
        4,
        Command::RemoveWorktreeConfirmed {
            scope,
            worktree,
            assessment,
            force: false,
        },
    );
    let binding = command.binding();
    handle.submit(principal, command).await.unwrap();
    handle
        .reserve_git_allocation(
            binding,
            GitAllocationNeeds {
                repository: false,
                worktree: false,
                fallbacks: vec![],
            },
        )
        .await
        .unwrap();
    let mut parts = git_intent(binding, GitIntentPhase::Prepared)
        .parts()
        .clone();
    parts.action = RetainedGitAction::Remove {
        worktree,
        directory: parts.action.directory().clone(),
        head: None,
        approval: ApprovedRemoval::Clean,
    };
    let tabs = handle.snapshot().state().tabs.clone();
    parts.affected = RetainedRemovalScope::new(RetainedRemovalScopeParts {
        server: support::server(),
        membership_versions: tabs
            .iter()
            .map(|tab| {
                spaceterm_core::organization_retained::TabMembershipVersion::new(
                    tab.id,
                    FieldVersion::new(1),
                )
            })
            .collect(),
        tabs,
        panes: vec![RetainedAffectedPane {
            hierarchy: AssessedPane {
                workspace: workspace(),
                tab: tab(),
                pane: pane(),
                session_generation: session.generation,
                control_generation: grant.generation(),
                context_generation: ContextGeneration::new(1),
            },
            session: Some(session),
            startup_reserved: false,
            work_generation: CloseWorkGeneration::new(1),
        }],
        fallbacks: vec![],
    })
    .unwrap();
    let prepared = GitWriteIntent::new(parts.clone()).unwrap();
    let next_store = db
        .lock()
        .unwrap()
        .recovered
        .store_revision
        .checked_successor()
        .unwrap();
    let receipt = handle
        .record_external_progress(
            binding,
            git_progress(&prepared, next_store),
            vec![OperationWrite::GitIntent(GitIntentMutation::Insert(
                Box::new(prepared),
            ))],
        )
        .await
        .unwrap();
    parts.phase = GitIntentPhase::Started;
    let started = GitWriteIntent::new(parts).unwrap();
    let receipt = handle
        .record_external_progress(
            binding,
            git_progress(
                &started,
                receipt.store_revision.checked_successor().unwrap(),
            ),
            vec![OperationWrite::GitIntent(GitIntentMutation::Advance {
                binding,
                expected_phase: GitIntentPhase::Prepared,
                next: GitIntentPhase::Started,
                result: None,
            })],
        )
        .await
        .unwrap();
    let descriptor = RetirementDescriptor::new(RetirementDescriptorParts {
        batch: RetirementBatchId::new(support::server(), principal.epoch(), 17).unwrap(),
        binding,
        assessment,
        acceptance_revision: handle
            .snapshot()
            .state()
            .revision
            .checked_successor()
            .unwrap(),
        target: AssessmentTarget::RemoveWorktree { scope, worktree },
        panes: vec![
            RetirementPane::new(
                workspace(),
                tab(),
                pane(),
                Some(worktree),
                session.generation.checked_successor().unwrap(),
            )
            .unwrap(),
        ],
        sessions: vec![
            RetiredSession::new(
                session,
                RetirementReason::WorktreeRemoval,
                RetirementCleanupState::AwaitingEvidence,
            )
            .unwrap(),
        ],
        completion: RetirementCompletion::ContinueGitSettlement,
        eventual_result: CompactOutcome::new(&Outcome::Applied, codec.as_ref()).unwrap(),
    })
    .unwrap();
    let reference = ExternalIntentReference::new(ExternalIntentReferenceParts {
        binding,
        scope: ExternalIntentScope::Git(scope),
        phase: ExternalIntentPhase::Started,
        recorded_at: receipt.store_revision.checked_successor().unwrap(),
    })
    .unwrap();
    let progress = PendingExternalProgress::new(PendingExternalProgressParts {
        binding,
        effect: ExternalEffectKind::GitWrite,
        state: ExternalProgressState::RecordedIntent {
            reference,
            retirement: Some(Box::new(descriptor)),
        },
    })
    .unwrap();
    let cleanup_gate = Gate::new();
    *factory.retirement_gate.lock().unwrap() = Some(cleanup_gate.clone());
    let commit_gate = StoreGate::new();
    db.lock().unwrap().gate = Some(commit_gate.clone());
    let accepted = handle.clone();
    let mut acceptance = tokio::spawn(async move {
        accepted
            .record_external_progress(binding, progress, vec![])
            .await
    });
    // A rejected projection must report the error instead of leaving the test
    // waiting for a store call that will never happen.
    tokio::select! {
        _ = commit_gate.entered() => {}
        result = &mut acceptance => panic!("retirement rejected before commit: {:?}", result.unwrap()),
    }
    assert_eq!(
        handle.snapshot().runtime()[0].session_generation,
        session.generation
    );
    assert_eq!(
        factory.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .retire_calls,
        0
    );
    {
        use spaceterm_core::organization_retained::RetainedOrganizationWrite;
        let db = db.lock().unwrap();
        let transaction = db.transactions.last().unwrap();
        assert!(
            matches!(transaction.organization_writes.as_slice(), [OrganizationWrite::Retained(
            RetainedOrganizationWrite::SetPaneGenerationHighWater(value)
        )] if value.pane() == pane() && value.session() == session.generation.checked_successor().unwrap())
        );
        assert!(transaction.operation_writes.iter().any(|write| {
            if let OperationWrite::Ledger(write) = write {
                matches!(write.as_ref(), LedgerWrite::RecordExternalProgress { record, .. }
                    if record.binding == binding && matches!(record.external_progress.as_ref().unwrap().state(),
                        ExternalProgressState::RecordedIntent { retirement: Some(_), .. }))
            } else {
                false
            }
        }));
    }
    commit_gate.release();
    acceptance.await.unwrap().unwrap();
    cleanup_gate.entered().await;
    assert_eq!(
        handle.snapshot().runtime()[0].session_generation,
        session.generation.checked_successor().unwrap()
    );
    assert_eq!(
        db.lock()
            .unwrap()
            .recovered
            .organization
            .retained
            .as_ref()
            .unwrap()
            .parts()
            .pane_generations[0]
            .session(),
        session.generation.checked_successor().unwrap()
    );
    assert_eq!(
        handle.lookup(principal, binding).await.unwrap(),
        OperationResponse::Pending(binding)
    );
    assert_eq!(handle.snapshot().state().panes.len(), 1);
    let awaited = handle.clone();
    let cleanup = tokio::spawn(async move { awaited.await_git_cleanup(binding).await });
    let writes = Gate::new();
    *external.gate.lock().unwrap() = Some(writes.clone());
    let stopped = handle.clone();
    let shutdown = tokio::spawn(async move { stop(&stopped, host).await });
    writes.entered().await;
    cleanup_gate.release();
    let evidence = cleanup.await.unwrap().unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].session(), session);
    assert_eq!(
        evidence[0].first_reason(),
        RetirementReason::WorktreeRemoval
    );
    handle
        .settle_started_git(
            started,
            RetainedExternalResult {
                disposition: ExternalDisposition::DesiredStateObserved,
                failure: None,
                ended_sessions: 1,
            },
        )
        .await
        .unwrap();
    writes.release();
    shutdown.await.unwrap().unwrap();
    assert_eq!(
        handle.snapshot().runtime()[0].phase,
        SessionPhase::Ended {
            reason: EndReason::Stop
        }
    );
    assert_eq!(
        factory.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .retire_calls,
        1
    );
    server.wait().await.unwrap();
}

#[tokio::test]
async fn end_all_settles_starting_session_before_and_after_process_creation_boundary() {
    use spaceterm_core::geometry::*;
    for created in [false, true] {
        let fixture = Fixture::new();
        let principal = fixture.principal(1, 1);
        let codec = fixture.codec.clone();
        let host = fixture.host.clone();
        let factory = fixture.factory.clone();
        let delayed = Gate::new();
        if created {
            *factory.start_gate.lock().unwrap() = Some(delayed.clone());
        } else {
            *fixture.launch.before_process.lock().unwrap() = Some(delayed.clone());
        }
        let cleanup = Gate::new();
        *factory.retirement_gate.lock().unwrap() = Some(cleanup.clone());
        let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
            .await
            .unwrap();
        let handle = server.handle();
        connect(&handle, principal).await;
        let grant = acquire(&handle, principal, &codec).await;
        let fence = EffectFence::new(
            SessionKey {
                epoch: principal.epoch(),
                pane: pane(),
                generation: SessionGeneration::new(0),
            },
            grant,
            principal.binding(),
            ContextGeneration::new(1),
            None,
        )
        .unwrap();
        let intent =
            StartupIntent::new(operation(1, 2), fence, StartupSource::ExplicitSelection).unwrap();
        handle
            .submit(
                principal,
                request(&codec, 1, 2, Command::BeginStartup(intent)),
            )
            .await
            .unwrap();
        handle
            .submit(
                principal,
                request(
                    &codec,
                    1,
                    3,
                    Command::CompleteMeasuredStartup(MeasuredStartupMeasurement {
                        intent: intent.id(),
                        fence,
                        grid: MeasuredGrid::new(
                            24,
                            80,
                            CellPixels::new(8, 16).unwrap(),
                            SUPPORTED_GRID_LIMITS,
                        )
                        .unwrap(),
                    }),
                ),
            )
            .await
            .unwrap();
        delayed.entered().await;
        let operation = request(
            &codec,
            1,
            4,
            Command::EndAllSessions {
                server: support::server(),
                assessment: AssessmentRef {
                    operation: operation(1, 4),
                    digest: RequestDigest::from_bytes([18; 32]),
                },
            },
        );
        let binding = operation.binding();
        assert_eq!(
            handle.submit(principal, operation).await.unwrap(),
            OperationResponse::Pending(binding)
        );
        let acceptance = handle.snapshot().state().revision;
        delayed.release();
        if created {
            tokio::time::timeout(std::time::Duration::from_secs(2), cleanup.entered())
                .await
                .unwrap();
            cleanup.release();
        }
        let completed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let OperationResponse::Completed { revision, .. } =
                    handle.lookup(principal, binding).await.unwrap()
                {
                    break revision;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(completed, acceptance);
        assert_eq!(
            handle.snapshot().runtime()[0].phase,
            SessionPhase::Ended {
                reason: EndReason::EndAllSessions
            }
        );
        assert_eq!(factory.starts.lock().unwrap().len(), usize::from(created));
        if created {
            assert_eq!(
                factory.states.lock().unwrap()[0]
                    .lock()
                    .unwrap()
                    .retire_calls,
                1
            );
        }
        stop(&handle, host).await.unwrap();
        server.wait().await.unwrap();
    }
}

#[tokio::test]
async fn ended_publication_and_pending_retirement_reply_wait_for_worker_effect_barrier() {
    use spaceterm_terminal::runtime_contracts::{FencedRuntimeCommand, RuntimeCommand};
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    let session = start(&handle, principal, &codec, grant).await;
    let access = handle.runtime_access(principal, session).await.unwrap();
    let fence = EffectFence::new(
        session,
        grant,
        principal.binding(),
        access.authority.parts().context,
        None,
    )
    .unwrap();
    let barrier = Gate::new();
    let cleanup = Gate::new();
    *factory.barrier_gate.lock().unwrap() = Some(barrier.clone());
    *factory.retirement_gate.lock().unwrap() = Some(cleanup.clone());
    let submitted = handle.clone();
    let operation = request(
        &codec,
        1,
        4,
        Command::EndAllSessions {
            server: support::server(),
            assessment: AssessmentRef {
                operation: operation(1, 4),
                digest: RequestDigest::from_bytes([19; 32]),
            },
        },
    );
    let task = tokio::spawn(async move { submitted.submit(principal, operation).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), barrier.entered())
        .await
        .unwrap();
    assert!(!task.is_finished());
    assert_eq!(
        handle.snapshot().runtime()[0].phase,
        SessionPhase::Running {
            generation: session.generation
        }
    );
    barrier.release();
    assert!(matches!(
        task.await.unwrap().unwrap(),
        OperationResponse::Pending(_)
    ));
    cleanup.entered().await;
    assert_eq!(
        access
            .handle
            .try_submit(FencedRuntimeCommand {
                fence,
                command: RuntimeCommand::FocusReport(true)
            })
            .unwrap()
            .await
            .unwrap(),
        EffectDisposition::StaleAuthority
    );
    cleanup.release();
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn native_directory_refusal_persists_unavailable_phase_without_fallback_or_launch() {
    use spaceterm_core::geometry::*;
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    *fixture.launch.failure.lock().unwrap() = Some(OperationFailure::DirectoryUnavailable);
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    let directory = handle.snapshot().state().panes[0]
        .restoration_directory()
        .clone();
    let fence = EffectFence::new(
        SessionKey {
            epoch: principal.epoch(),
            pane: pane(),
            generation: SessionGeneration::new(0),
        },
        grant,
        principal.binding(),
        ContextGeneration::new(1),
        None,
    )
    .unwrap();
    let intent =
        StartupIntent::new(operation(1, 2), fence, StartupSource::ExplicitSelection).unwrap();
    handle
        .submit(
            principal,
            request(&codec, 1, 2, Command::BeginStartup(intent)),
        )
        .await
        .unwrap();
    let response = handle
        .submit(
            principal,
            request(
                &codec,
                1,
                3,
                Command::CompleteMeasuredStartup(MeasuredStartupMeasurement {
                    intent: intent.id(),
                    fence,
                    grid: MeasuredGrid::new(
                        24,
                        80,
                        CellPixels::new(8, 16).unwrap(),
                        SUPPORTED_GRID_LIMITS,
                    )
                    .unwrap(),
                }),
            ),
        )
        .await
        .unwrap();
    assert!(matches!(
        response,
        OperationResponse::Completed {
            outcome: Outcome::Failed(OperationFailure::DirectoryUnavailable),
            ..
        }
    ));
    assert_eq!(
        handle.snapshot().runtime()[0].phase,
        SessionPhase::DirectoryUnavailable
    );
    assert_eq!(
        handle.snapshot().state().panes[0].restoration_directory(),
        &directory
    );
    assert!(factory.starts.lock().unwrap().is_empty());
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn subscription_drop_does_not_retire_and_close_work_keeps_original_source_tick() {
    use spaceterm_core::observations::*;
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    let db = fixture.db.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    let session = start(&handle, principal, &codec, grant).await;
    let fence = EffectFence::new(
        session,
        grant,
        principal.binding(),
        ContextGeneration::new(1),
        None,
    )
    .unwrap();
    let subscription = handle.subscribe_runtime(principal, fence).await.unwrap();
    drop(subscription);
    let observation = CloseWorkObservation::new(
        session,
        RuntimeObservationSequence::new(1).unwrap(),
        CloseWorkGeneration::new(1),
        MonotonicTick::new(17),
        CloseWorkState::Idle,
    )
    .unwrap();
    factory.states.lock().unwrap()[0].lock().unwrap().close_work = Some(observation);
    loop {
        match handle.refresh_runtime_observations().await {
            Ok(()) => break,
            Err(error) if error.class == ServerFailureClass::Capacity => {
                tokio::task::yield_now().await
            }
            Err(error) => panic!("{error:?}"),
        }
    }
    assert_eq!(
        db.lock().unwrap().close_observations.last().copied(),
        Some((observation, MonotonicTick::new(1000)))
    );
    assert_eq!(
        factory.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .retire_calls,
        0
    );
    assert_eq!(factory.starts.lock().unwrap().len(), 1);
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

struct PairingResult(spaceterm_core::pairing::PairingOutcome);
impl spaceterm_server::extensions::PairingCompletionPort for PairingResult {
    fn prepare(
        &mut self,
        _: spaceterm_server::extensions::PairingCompletionContext<'_>,
    ) -> Result<
        spaceterm_server::extensions::PreparedPairingCompletion,
        spaceterm_server::contracts::ServerFailure,
    > {
        Ok(spaceterm_server::extensions::PreparedPairingCompletion {
            writes: spaceterm_core::pairing::PairingWrites::default(),
            streams: vec![],
            outcome: self.0.clone(),
        })
    }
}

#[tokio::test]
async fn pairing_assessment_and_failure_complete_through_the_actor() {
    use spaceterm_core::pairing::*;
    use spaceterm_server::extensions::*;
    let fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let db = fixture.db.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let target = PairingTarget::new(PairingTargetParts {
        server: support::server(),
        device: principal.device(),
        pairing: principal.pairing(),
    })
    .unwrap();
    let request = request(
        &codec,
        1,
        1,
        Command::Pairing(PairingCommand::AssessUnpair {
            target: target.clone(),
        }),
    );
    let binding = request.binding();
    assert_eq!(
        handle.submit(principal, request.clone()).await.unwrap(),
        OperationResponse::Pending(binding)
    );
    let before = handle.snapshot();
    let assessment = UnpairAssessment::new(UnpairAssessmentParts {
        reference: AssessmentRef {
            operation: binding.id,
            digest: binding.digest,
        },
        target,
        authorization_version: FieldVersion::new(1),
        clients: vec![ClientRecord {
            id: client(1),
            pairing: principal.pairing(),
        }],
    })
    .unwrap();
    let response = handle
        .complete_pairing(
            request.clone(),
            PairingCompletionRequest {
                allocation: PairingAllocationRequest::None,
                revoked_clients: vec![],
                preparation: Box::new(PairingResult(PairingOutcome::UnpairAssessed(
                    assessment.clone(),
                ))),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        response,
        OperationResponse::Completed {
            binding,
            revision: before.state().revision,
            outcome: Outcome::Pairing(Box::new(PairingOutcome::UnpairAssessed(assessment))),
        }
    );
    assert_eq!(handle.snapshot(), before);
    let count = db.lock().unwrap().transactions.len();
    assert_eq!(handle.lookup(principal, binding).await.unwrap(), response);
    assert_eq!(
        handle
            .complete_pairing(
                request,
                PairingCompletionRequest {
                    allocation: PairingAllocationRequest::None,
                    revoked_clients: vec![],
                    preparation: Box::new(PairingResult(PairingOutcome::Failed(
                        PairingFailure::Cancelled
                    ))),
                }
            )
            .await
            .unwrap(),
        response
    );
    assert_eq!(db.lock().unwrap().transactions.len(), count);

    let rejected = support::request(
        &codec,
        1,
        2,
        Command::Pairing(PairingCommand::AssessUnpair {
            target: PairingTarget::new(PairingTargetParts {
                server: support::server(),
                device: principal.device(),
                pairing: principal.pairing(),
            })
            .unwrap(),
        }),
    );
    handle.submit(principal, rejected.clone()).await.unwrap();
    assert!(
        matches!(handle.complete_pairing(rejected, PairingCompletionRequest {
        allocation: PairingAllocationRequest::None,
        revoked_clients: vec![],
        preparation: Box::new(PairingResult(PairingOutcome::Failed(PairingFailure::StaleAssessment))),
    }).await.unwrap(), OperationResponse::Completed {
        outcome: Outcome::Pairing(value), ..
    } if *value == PairingOutcome::Failed(PairingFailure::StaleAssessment))
    );
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

struct BindPairingClient {
    authorization: spaceterm_core::pairing::PairingAuthorization,
    signer: std::sync::Arc<TestKey>,
}
impl spaceterm_server::extensions::PairingCompletionPort for BindPairingClient {
    fn prepare(
        &mut self,
        context: spaceterm_server::extensions::PairingCompletionContext<'_>,
    ) -> Result<
        spaceterm_server::extensions::PreparedPairingCompletion,
        spaceterm_server::contracts::ServerFailure,
    > {
        use spaceterm_core::{operation_ledger_contracts::*, pairing::*};
        use spaceterm_protocol::authentication::SigningKey;
        let client = context.allocated_client.unwrap();
        let previous = self.authorization.parts();
        let mut authorization = previous.clone();
        authorization.authorization_version = authorization
            .authorization_version
            .checked_successor()
            .unwrap();
        authorization.clients.push(client.clone());
        let claims = PairingReceiptBody::ClientBinding {
            target: PairingTarget::new(PairingTargetParts {
                server: support::server(),
                device: previous.record.device,
                pairing: previous.record.id,
            })
            .unwrap(),
            keys: previous.keys.clone(),
            client: client.clone(),
            operation: context.request.binding(),
            revision: context.cursor.store.checked_successor().unwrap(),
        };
        // Test-only receipt bytes. Exact-byte semantic validation belongs to the
        // real pairing/codec validator, not this actor-ordering fixture.
        let mut canonical_body = context.request.binding().digest.bytes().to_vec();
        canonical_body.extend(client.parts().client.id.local().to_be_bytes());
        canonical_body.extend(claims.revision().get().to_be_bytes());
        let signature = self.signer.sign(&canonical_body).unwrap().to_vec();
        let receipt = SignedPairingReceipt::new(SignedPairingReceiptParts {
            schema: 1,
            claims,
            canonical_body,
            signature,
        })
        .unwrap();
        Ok(spaceterm_server::extensions::PreparedPairingCompletion {
            writes: PairingWrites::new(PairingWritesParts {
                writes: vec![PairingWrite::PutAuthorization {
                    expected_authorization_version: Some(previous.authorization_version),
                    value: PairingAuthorization::new(authorization).unwrap(),
                }],
            })
            .unwrap(),
            streams: vec![
                StreamRecord::new(
                    StreamBinding::from_stream(client.parts().ordinary),
                    StreamPurpose::Ordinary,
                    StreamStatus::Active,
                    0,
                    0,
                    None,
                )
                .unwrap(),
                StreamRecord::new(
                    StreamBinding::from_stream(client.parts().self_revocation),
                    StreamPurpose::SelfRevocation,
                    StreamStatus::Active,
                    0,
                    0,
                    Some(OutcomeReservation::full(
                        ReservationClass::DedicatedSelfRevocation,
                    )),
                )
                .unwrap(),
            ],
            outcome: PairingOutcome::ClientBound(receipt),
        })
    }
}

#[tokio::test]
async fn additional_client_binding_reserves_streams_allocation_and_outcome_in_one_commit() {
    use spaceterm_core::{identity::*, operation_ledger_contracts::*, pairing::*, storage::*};
    use spaceterm_protocol::authentication::{PairingLookup, SigningKey};
    use spaceterm_server::extensions::*;
    for capacity_available in [false, true] {
        let fixture = Fixture::new();
        let principal = fixture.principal(1, 1);
        let codec = fixture.codec.clone();
        let host = fixture.host.clone();
        let db = fixture.db.clone();
        let signer = fixture.key.clone();
        let pairings = fixture.pairings.clone();
        let observer = fixture.observer.clone();
        let authorization = PairingAuthorization::new(PairingAuthorizationParts {
            record: PairingRecord {
                id: principal.pairing(),
                device: principal.device(),
                state: PairingState::Authorized,
            },
            authorization_version: FieldVersion::new(1),
            keys: PairingKeys::new(PairingKeysParts {
                device_identity_public_key: signer.public_key(),
                pairing_authentication_public_key: pairings.key.public_key(),
            })
            .unwrap(),
            clients: vec![
                PairingClientStreams::new(PairingClientStreamsParts {
                    client: ClientRecord {
                        id: client(1),
                        pairing: principal.pairing(),
                    },
                    ordinary: stream(1),
                    self_revocation: OperationStreamId::new(
                        OperationStreamKey::new(support::server(), 3).unwrap(),
                        principal.pairing(),
                        client(1),
                    )
                    .unwrap(),
                })
                .unwrap(),
            ],
            revocation_receipt: None,
        })
        .unwrap();
        {
            let mut db = db.lock().unwrap();
            db.recovered.high_water.operation_stream = 3;
            db.recovered.pairing = Some(
                PairingRecovery::new(PairingRecoveryParts {
                    authorizations: vec![authorization.clone()],
                    enrollments: vec![],
                })
                .unwrap(),
            );
            db.reject_stream_registration = !capacity_available;
        }
        let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
            .await
            .unwrap();
        let handle = server.handle();
        connect(&handle, principal).await;
        let target = PairingTarget::new(PairingTargetParts {
            server: support::server(),
            device: principal.device(),
            pairing: principal.pairing(),
        })
        .unwrap();
        let request = request(
            &codec,
            1,
            1,
            Command::Pairing(PairingCommand::BindAdditionalClient {
                target,
                expected_authorization_version: FieldVersion::new(1),
                claim_digest: RequestDigest::from_bytes([23; 32]),
            }),
        );
        let binding = request.binding();
        handle.submit(principal, request.clone()).await.unwrap();
        let before = db.lock().unwrap().recovered.clone();
        let snapshot = handle.snapshot();
        let gate = StoreGate::new();
        db.lock().unwrap().gate = Some(gate.clone());
        let completed = handle.clone();
        let retry = request.clone();
        let mut completion = tokio::spawn(async move {
            completed
                .complete_pairing(
                    request,
                    PairingCompletionRequest {
                        allocation: PairingAllocationRequest::AdditionalClient(principal.pairing()),
                        revoked_clients: vec![],
                        preparation: Box::new(BindPairingClient {
                            authorization,
                            signer,
                        }),
                    },
                )
                .await
        });
        if !capacity_available {
            assert_eq!(
                completion.await.unwrap().unwrap_err().class,
                ServerFailureClass::Capacity
            );
            {
                let db = db.lock().unwrap();
                assert_eq!(db.recovered, before);
                assert_eq!(
                    db.transactions.last().unwrap().resulting_store_revision,
                    before.store_revision
                );
            }
            assert_eq!(
                handle.lookup(principal, binding).await.unwrap(),
                OperationResponse::Pending(binding)
            );
            assert_eq!(handle.snapshot(), snapshot);
            // Rejection reached no physical commit, so do not gate shutdown.
            db.lock().unwrap().gate = None;
        } else {
            tokio::select! {
                _ = gate.entered() => {}
                result = &mut completion => panic!("binding failed before commit: {:?}", result.unwrap()),
            }
            assert_eq!(db.lock().unwrap().recovered, before);
            assert_eq!(
                handle.lookup(principal, binding).await.unwrap(),
                OperationResponse::Pending(binding)
            );
            assert!(
                pairings
                    .lookup(support::server().bytes(), local(1), local(3))
                    .is_err()
            );
            gate.release();
            let response = completion.await.unwrap().unwrap();
            let OperationResponse::Completed {
                outcome: Outcome::Pairing(value),
                ..
            } = &response
            else {
                panic!("pairing completion")
            };
            let PairingOutcome::ClientBound(receipt) = value.as_ref() else {
                panic!("client receipt")
            };
            let PairingReceiptBody::ClientBinding {
                client: allocated,
                operation,
                revision,
                ..
            } = &receipt.parts().claims
            else {
                panic!("binding claims")
            };
            assert_eq!(*operation, binding);
            assert_eq!(allocated.parts().client.id, client(3));
            assert_eq!(allocated.parts().ordinary.key().local(), 4);
            assert_eq!(allocated.parts().self_revocation.key().local(), 5);
            {
                let db = db.lock().unwrap();
                let transaction = db.transactions.last().unwrap();
                assert_eq!(transaction.resulting_store_revision, *revision);
                assert_eq!(transaction.resulting_high_water.client, 3);
                assert_eq!(transaction.resulting_high_water.operation_stream, 5);
                assert_eq!(
                    transaction.expected_organization_revision,
                    transaction.resulting_organization_revision
                );
                let streams: Vec<_> = transaction
                    .operation_writes
                    .iter()
                    .filter_map(|write| match write {
                        OperationWrite::Ledger(value) => match value.as_ref() {
                            LedgerWrite::RegisterStream(value) => Some(value),
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect();
                assert_eq!(streams.len(), 2);
                assert_eq!(streams[0].purpose, StreamPurpose::Ordinary);
                assert_eq!(streams[1].purpose, StreamPurpose::SelfRevocation);
                assert_eq!(
                    streams[1].dormant_reservation,
                    Some(OutcomeReservation::full(
                        ReservationClass::DedicatedSelfRevocation
                    ))
                );
                assert!(
                    transaction
                        .operation_writes
                        .iter()
                        .any(|write| matches!(write, OperationWrite::Pairing(_)))
                );
                assert!(transaction.operation_writes.iter().any(|write| matches!(write, OperationWrite::Ledger(write)
                    if matches!(write.as_ref(), LedgerWrite::CompleteOperation { record, .. } if record.binding == binding))));
                let authorization = &db
                    .recovered
                    .pairing
                    .as_ref()
                    .unwrap()
                    .parts()
                    .authorizations[0];
                assert_eq!(
                    authorization.parts().authorization_version,
                    FieldVersion::new(2)
                );
                assert_eq!(authorization.parts().clients.last().unwrap(), allocated);
            }
            assert!(
                pairings
                    .lookup(support::server().bytes(), local(1), local(3))
                    .unwrap()
                    .active
            );
            assert_eq!(observer.pairing_commits.load(Ordering::SeqCst), 1);
            let count = db.lock().unwrap().transactions.len();
            assert_eq!(handle.submit(principal, retry).await.unwrap(), response);
            assert_eq!(db.lock().unwrap().transactions.len(), count);
            assert_eq!(handle.snapshot(), snapshot);
        }
        stop(&handle, host).await.unwrap();
        server.wait().await.unwrap();
    }
}

async fn late_cleanup_releases_completed_runtime(failed: bool) {
    let mut fixture = Fixture::new();
    fixture.config.limits.runtime_owners = 1;
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    let db = fixture.db.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    let session = start(&handle, principal, &codec, grant).await;
    let terminal = SessionTerminalResult::new(
        session,
        if failed {
            RuntimeTerminalStatus::RuntimeFailed {
                class: FailureClass::RuntimeFault,
            }
        } else {
            RuntimeTerminalStatus::OrdinaryExit {
                exit_status: Some(0),
            }
        },
    )
    .unwrap();
    let state = factory.states.lock().unwrap()[0].clone();
    state.lock().unwrap().lifecycle = RuntimeLifecycle::Retiring { terminal };
    let mut snapshots = handle.subscribe_snapshot();
    loop {
        match handle.refresh_runtime_observations().await {
            Ok(()) => break,
            Err(error) if error.class == ServerFailureClass::Capacity => {
                tokio::task::yield_now().await
            }
            Err(error) => panic!("{error:?}"),
        }
    }
    while if failed {
        !matches!(
            snapshots.borrow().runtime()[0].phase,
            SessionPhase::Failed {
                class: FailureClass::RuntimeFault
            }
        )
    } else {
        !snapshots.borrow().state().panes.is_empty()
    } {
        snapshots.changed().await.unwrap();
    }
    assert_eq!(factory.released.load(Ordering::SeqCst), 0);
    assert_eq!(state.lock().unwrap().retire_calls, 0);
    let reduction = handle.snapshot();
    let commits = db.lock().unwrap().transactions.len();
    let cleanup = SessionCleanupObservation::new(
        session,
        terminal.retirement_reason(),
        CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
    )
    .unwrap();
    {
        let mut state = state.lock().unwrap();
        state.cleanup = Some(cleanup);
        state.lifecycle =
            RuntimeLifecycle::Finished(RuntimeCompletion::new(terminal, cleanup).unwrap());
    }
    loop {
        match handle.refresh_runtime_observations().await {
            Ok(()) => break,
            Err(error) if error.class == ServerFailureClass::Capacity => {
                tokio::task::yield_now().await
            }
            Err(error) => panic!("{error:?}"),
        }
    }
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while factory.released.load(Ordering::SeqCst) == 0 {
            factory.release_notice.notified().await;
        }
    })
    .await
    .expect("confirmed late cleanup must release the completed owner before shutdown");
    assert_eq!(handle.snapshot(), reduction);
    assert_eq!(db.lock().unwrap().transactions.len(), commits);
    assert_eq!(factory.abandoned.load(Ordering::SeqCst), 0);
    assert_eq!(state.lock().unwrap().retire_calls, 0);
    if failed {
        assert_eq!(
            handle.snapshot().runtime()[0].control,
            ControlRecord::Held {
                client: principal.client(),
                generation: grant.generation()
            }
        );
        let replacement = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            start_at(&handle, principal, &codec, grant, 4),
        )
        .await
        .expect("explicit restart must reclaim the one-owner capacity");
        assert_eq!(
            replacement.generation,
            session.generation.checked_successor().unwrap()
        );
        assert_eq!(factory.starts.lock().unwrap().len(), 2);
    }
    stop(&handle, host).await.unwrap();
    if !failed {
        assert!(
            db.lock()
                .unwrap()
                .transactions
                .iter()
                .filter_map(|transaction| {
                    transaction
                        .operation_writes
                        .iter()
                        .find_map(|write| match write {
                            spaceterm_core::storage::OperationWrite::NativeRetirement(
                                NativeRetirementMutation::Accept(value),
                            ) => Some(value),
                            _ => None,
                        })
                })
                .all(|retirement| retirement.sessions().is_empty())
        );
    }
    server.wait().await.unwrap();
}

#[tokio::test]
async fn ordinary_exit_releases_capacity_when_confirmed_cleanup_arrives_after_deletion() {
    late_cleanup_releases_completed_runtime(false).await;
}

#[tokio::test]
async fn runtime_failure_releases_late_cleanup_and_allows_an_explicit_restart() {
    late_cleanup_releases_completed_runtime(true).await;
}

struct RevokePairingClients {
    authorization: spaceterm_core::pairing::PairingAuthorization,
    signer: std::sync::Arc<TestKey>,
}
impl spaceterm_server::extensions::PairingCompletionPort for RevokePairingClients {
    fn prepare(
        &mut self,
        context: spaceterm_server::extensions::PairingCompletionContext<'_>,
    ) -> Result<
        spaceterm_server::extensions::PreparedPairingCompletion,
        spaceterm_server::contracts::ServerFailure,
    > {
        use spaceterm_core::pairing::*;
        use spaceterm_protocol::authentication::SigningKey;
        let previous = self.authorization.parts();
        let revision = context.cursor.store.checked_successor().unwrap();
        let canonical_body = context.request.binding().digest.bytes().to_vec();
        let receipt = SignedPairingReceipt::new(SignedPairingReceiptParts {
            schema: 1,
            claims: PairingReceiptBody::Revocation {
                receipt: RevocationReceipt {
                    server: support::server(),
                    pairing: previous.record.id,
                    device: previous.record.device,
                    operation: context.request.binding().id,
                    revision,
                    receipt_digest: RequestDigest::from_bytes([31; 32]),
                },
                authentication_key_digest: RequestDigest::from_bytes([32; 32]),
                revocation_event: [33; 16],
            },
            signature: self.signer.sign(&canonical_body).unwrap().to_vec(),
            canonical_body,
        })
        .unwrap();
        let mut value = previous.clone();
        value.authorization_version = value.authorization_version.checked_successor().unwrap();
        value.record.state = PairingState::Revoked { revision };
        value.revocation_receipt = Some(receipt.clone());
        Ok(spaceterm_server::extensions::PreparedPairingCompletion {
            writes: PairingWrites::new(PairingWritesParts {
                writes: vec![PairingWrite::PutAuthorization {
                    expected_authorization_version: Some(previous.authorization_version),
                    value: PairingAuthorization::new(value).unwrap(),
                }],
            })
            .unwrap(),
            streams: vec![],
            outcome: PairingOutcome::Revoked(receipt),
        })
    }
}

#[tokio::test]
async fn pairing_completion_releases_controls_and_retires_streams_before_final_worker_acknowledgement()
 {
    use spaceterm_core::{operation_ledger_contracts::*, pairing::*, storage::*};
    use spaceterm_protocol::authentication::PairingLookup;
    use spaceterm_server::extensions::*;
    let (fixture, authorization) = revocation_fixture();
    let first = fixture.principal(1, 1);
    let second = fixture.principal(2, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let db = fixture.db.clone();
    let factory = fixture.factory.clone();
    let pairings = fixture.pairings.clone();
    let signer = fixture.key.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, first).await;
    connect(&handle, second).await;
    let grant = acquire(&handle, first, &codec).await;
    let session = start(&handle, first, &codec, grant).await;
    let assessment = UnpairAssessment::new(UnpairAssessmentParts {
        reference: AssessmentRef {
            operation: operation(2, 1),
            digest: RequestDigest::from_bytes([34; 32]),
        },
        target: PairingTarget::new(PairingTargetParts {
            server: support::server(),
            device: first.device(),
            pairing: first.pairing(),
        })
        .unwrap(),
        authorization_version: FieldVersion::new(1),
        clients: authorization
            .parts()
            .clients
            .iter()
            .map(|client| client.parts().client)
            .collect(),
    })
    .unwrap();
    let request = request(
        &codec,
        2,
        2,
        Command::Pairing(PairingCommand::UnpairConfirmed { assessment }),
    );
    let binding = request.binding();
    handle.submit(second, request.clone()).await.unwrap();
    let before = handle.snapshot();
    let store_gate = StoreGate::new();
    db.lock().unwrap().gate = Some(store_gate.clone());
    let completed = handle.clone();
    let completion = tokio::spawn(async move {
        completed
            .complete_pairing(
                request,
                PairingCompletionRequest {
                    allocation: PairingAllocationRequest::None,
                    revoked_clients: vec![client(1), client(2)],
                    preparation: Box::new(RevokePairingClients {
                        authorization,
                        signer,
                    }),
                },
            )
            .await
    });
    store_gate.entered().await;
    assert_eq!(handle.snapshot(), before);
    assert!(
        pairings
            .lookup(support::server().bytes(), local(1), local(1))
            .unwrap()
            .active
    );
    let barrier = Gate::new();
    *factory.barrier_gate.lock().unwrap() = Some(barrier.clone());
    store_gate.release();
    barrier.entered().await;
    assert!(!completion.is_finished());
    assert_eq!(handle.snapshot(), before);
    assert!(
        !pairings
            .lookup(support::server().bytes(), local(1), local(1))
            .unwrap()
            .active
    );
    assert_eq!(
        handle
            .runtime_access(first, session)
            .await
            .err()
            .unwrap()
            .class,
        ServerFailureClass::Authentication
    );
    {
        let db = db.lock().unwrap();
        let transaction = db.transactions.last().unwrap();
        assert!(
            transaction
                .operation_writes
                .iter()
                .any(|write| matches!(write, OperationWrite::Pairing(_)))
        );
        let retired = transaction.operation_writes.iter().filter(|write| matches!(write, OperationWrite::Ledger(value)
            if matches!(value.as_ref(), LedgerWrite::RetireStream { reason: StreamRetirementReason::PairingRevoked, .. }))).count();
        assert_eq!(retired, 2);
        assert!(transaction.operation_writes.iter().any(|write| matches!(write, OperationWrite::Ledger(value)
            if matches!(value.as_ref(), LedgerWrite::CompleteOperation { record, .. } if record.binding == binding))));
    }
    barrier.release();
    assert!(
        matches!(completion.await.unwrap().unwrap(), OperationResponse::Completed { outcome: Outcome::Pairing(value), .. }
        if matches!(*value, PairingOutcome::Revoked(_)))
    );
    assert_eq!(
        handle.snapshot().runtime()[0].control,
        ControlRecord::Unclaimed
    );
    assert_eq!(
        handle.snapshot().runtime()[0].phase,
        SessionPhase::Running {
            generation: session.generation
        }
    );
    assert_eq!(
        factory.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .retire_calls,
        0
    );
    assert_eq!(factory.starts.lock().unwrap().len(), 1);
    stop(&handle, host).await.unwrap();
    server.wait().await.unwrap();
}

fn revocation_fixture() -> (Fixture, spaceterm_core::pairing::PairingAuthorization) {
    use spaceterm_core::{identity::*, pairing::*};
    use spaceterm_protocol::authentication::SigningKey;
    let fixture = Fixture::new();
    let first = fixture.principal(1, 1);
    let db = fixture.db.clone();
    let pairings = fixture.pairings.clone();
    let signer = fixture.key.clone();
    let authorization = PairingAuthorization::new(PairingAuthorizationParts {
        record: PairingRecord {
            id: first.pairing(),
            device: first.device(),
            state: PairingState::Authorized,
        },
        authorization_version: FieldVersion::new(1),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: signer.public_key(),
            pairing_authentication_public_key: pairings.key.public_key(),
        })
        .unwrap(),
        clients: [1, 2]
            .into_iter()
            .map(|n| {
                PairingClientStreams::new(PairingClientStreamsParts {
                    client: ClientRecord {
                        id: client(n),
                        pairing: first.pairing(),
                    },
                    ordinary: stream(n),
                    self_revocation: OperationStreamId::new(
                        OperationStreamKey::new(support::server(), n + 2).unwrap(),
                        first.pairing(),
                        client(n),
                    )
                    .unwrap(),
                })
                .unwrap()
            })
            .collect(),
        revocation_receipt: None,
    })
    .unwrap();
    {
        let mut db = db.lock().unwrap();
        db.recovered.high_water.operation_stream = 4;
        db.recovered.pairing = Some(
            PairingRecovery::new(PairingRecoveryParts {
                authorizations: vec![authorization.clone()],
                enrollments: vec![],
            })
            .unwrap(),
        );
    }
    (fixture, authorization)
}

#[tokio::test]
async fn pairing_revocation_fences_retained_handles_before_commit_and_never_replays_delayed_effects()
 {
    use spaceterm_core::{native_requests::*, observations::MonotonicTick, storage::*};
    use spaceterm_server::extensions::*;
    use spaceterm_terminal::runtime_contracts::{FencedRuntimeCommand, RuntimeCommand};
    for (disposition, disconnected) in [
        (None, false),
        (Some(CommitDisposition::NotApplied(StoreFailure::Io)), false),
        (Some(CommitDisposition::NotApplied(StoreFailure::Io)), true),
        (Some(CommitDisposition::Indeterminate), false),
    ] {
        let (fixture, authorization) = revocation_fixture();
        let first = fixture.principal(1, 1);
        let second = fixture.principal(2, 1);
        let reconnect = fixture.principal(1, 2);
        let codec = fixture.codec.clone();
        let host = fixture.host.clone();
        let db = fixture.db.clone();
        let factory = fixture.factory.clone();
        let signer = fixture.key.clone();
        let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
            .await
            .unwrap();
        let handle = server.handle();
        connect(&handle, first).await;
        connect(&handle, second).await;
        let grant = acquire(&handle, first, &codec).await;
        let session = start(&handle, first, &codec, grant).await;
        handle
            .update_client_focus(
                first,
                ClientFocusUpdate {
                    context: ContextGeneration::new(2),
                    focused_pane: Some(pane()),
                    terminal_input_focus: true,
                    native_effects_eligible: true,
                },
            )
            .await
            .unwrap();
        let old = handle.runtime_access(first, session).await.unwrap();
        let fence = EffectFence::new(
            session,
            grant,
            first.binding(),
            old.authority.parts().context,
            None,
        )
        .unwrap();
        let command = FencedRuntimeCommand {
            fence,
            command: RuntimeCommand::FocusReport(true),
        };
        let native = NativeRequest::new(
            fence,
            NativeRequestId::new(1).unwrap(),
            MonotonicTick::new(1),
            MonotonicTick::new(1),
            Some(MonotonicTick::new(100)),
            NativeRequestKind::ClipboardWrite(NativeText::new(String::new()).unwrap()),
        )
        .unwrap();
        let native =
            NativeCompletion::new(&native, NativeCompletionKind::ClipboardWritten).unwrap();
        // Admit both effects before revocation; execute their final checks while
        // the revocation transaction is blocked on the store worker.
        let input = old.handle.try_submit(command.clone()).unwrap();
        let completion = old.handle.complete_native(native.clone()).unwrap();
        let request = request(
            &codec,
            2,
            1,
            Command::RevokePairing {
                pairing: second.pairing(),
            },
        );
        handle.submit(second, request.clone()).await.unwrap();
        let before = handle.snapshot();
        let commits = db.lock().unwrap().transactions.len();
        let store = StoreGate::new();
        {
            let mut db = db.lock().unwrap();
            db.gate = Some(store.clone());
            db.next = disposition;
        }
        let precommit = Gate::new();
        *factory.barrier_gate.lock().unwrap() = Some(precommit.clone());
        let submitted = handle.clone();
        let task = tokio::spawn(async move {
            submitted
                .complete_pairing(
                    request,
                    PairingCompletionRequest {
                        allocation: PairingAllocationRequest::None,
                        revoked_clients: vec![client(1), client(2)],
                        preparation: Box::new(RevokePairingClients {
                            authorization,
                            signer,
                        }),
                    },
                )
                .await
        });
        let commit_preceded_barrier = tokio::select! {
            _ = precommit.entered() => false,
            _ = store.entered() => true,
        };
        if !commit_preceded_barrier {
            assert_eq!(handle.snapshot(), before);
            assert_eq!(db.lock().unwrap().transactions.len(), commits);
            assert!(!task.is_finished());
            assert!(handle.runtime_access(first, session).await.is_err());
            assert!(handle.register(first).await.is_err());
            precommit.release();
            store.entered().await;
        }
        let input_result = input.await.unwrap();
        let native_result = completion.await.unwrap();
        let retained_result = old
            .handle
            .try_submit(command.clone())
            .unwrap()
            .await
            .unwrap();
        assert_eq!(handle.snapshot(), before);
        if disconnected {
            handle.disconnect(first).await.unwrap();
        }
        store.release();
        // Let the old implementation finish too, so a red assertion leaves no
        // blocking database thread or worker barrier behind.
        if commit_preceded_barrier {
            precommit.release();
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        match disposition {
            None => {
                assert!(matches!(
                    result.unwrap(),
                    OperationResponse::Completed {
                        outcome: Outcome::Pairing(_),
                        ..
                    }
                ));
                assert!(handle.runtime_access(first, session).await.is_err());
                stop(&handle, host).await.unwrap();
                server.wait().await.unwrap();
            }
            Some(CommitDisposition::NotApplied(_)) => {
                assert_eq!(
                    result.unwrap_err().class,
                    ServerFailureClass::StorageUnavailable
                );
                let current_principal = if disconnected {
                    assert!(handle.runtime_access(first, session).await.is_err());
                    connect(&handle, reconnect).await;
                    reconnect
                } else {
                    first
                };
                let current = handle
                    .runtime_access(current_principal, session)
                    .await
                    .unwrap();
                let current_fence = EffectFence::new(
                    session,
                    grant,
                    current_principal.binding(),
                    current.authority.parts().context,
                    None,
                )
                .unwrap();
                let current_result = current
                    .handle
                    .try_submit(FencedRuntimeCommand {
                        fence: current_fence,
                        command: RuntimeCommand::FocusReport(true),
                    })
                    .unwrap()
                    .await
                    .unwrap();
                let discarded = old.handle.try_submit(command).unwrap().await.unwrap();
                stop(&handle, host).await.unwrap();
                server.wait().await.unwrap();
                if !disconnected {
                    assert!(current.authority.parts().context > old.authority.parts().context);
                }
                assert!(!current.authority.parts().native_effects_eligible);
                assert_eq!(current_result, EffectDisposition::Applied);
                assert_eq!(discarded, EffectDisposition::StaleAuthority);
            }
            Some(CommitDisposition::Indeterminate) => {
                assert_eq!(
                    result.unwrap_err().class,
                    ServerFailureClass::RecoveryRequired
                );
                assert!(handle.runtime_access(first, session).await.is_err());
                assert_eq!(
                    server.wait().await.unwrap_err().class,
                    ServerFailureClass::RecoveryRequired
                );
            }
            _ => unreachable!(),
        }
        assert!(
            !commit_preceded_barrier,
            "revocation commit entered before the final-effect barrier completed"
        );
        assert_eq!(input_result, EffectDisposition::StaleAuthority);
        assert_eq!(native_result, EffectDisposition::StaleAuthority);
        assert_eq!(retained_result, EffectDisposition::StaleAuthority);
    }
}

#[tokio::test]
async fn pairing_revocation_waits_for_cancelled_or_late_launch_owners_before_commit() {
    use spaceterm_core::geometry::*;
    use spaceterm_server::extensions::*;
    for created in [false, true] {
        let (fixture, authorization) = revocation_fixture();
        let first = fixture.principal(1, 1);
        let second = fixture.principal(2, 1);
        let codec = fixture.codec.clone();
        let host = fixture.host.clone();
        let db = fixture.db.clone();
        let factory = fixture.factory.clone();
        let signer = fixture.key.clone();
        let delayed = Gate::new();
        if created {
            *factory.start_gate.lock().unwrap() = Some(delayed.clone());
        } else {
            *fixture.launch.before_process.lock().unwrap() = Some(delayed.clone());
        }
        let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
            .await
            .unwrap();
        let handle = server.handle();
        connect(&handle, first).await;
        connect(&handle, second).await;
        let grant = acquire(&handle, first, &codec).await;
        let fence = EffectFence::new(
            SessionKey {
                epoch: first.epoch(),
                pane: pane(),
                generation: SessionGeneration::new(0),
            },
            grant,
            first.binding(),
            ContextGeneration::new(1),
            None,
        )
        .unwrap();
        let intent =
            StartupIntent::new(operation(1, 2), fence, StartupSource::ExplicitSelection).unwrap();
        handle
            .submit(first, request(&codec, 1, 2, Command::BeginStartup(intent)))
            .await
            .unwrap();
        handle
            .submit(
                first,
                request(
                    &codec,
                    1,
                    3,
                    Command::CompleteMeasuredStartup(MeasuredStartupMeasurement {
                        intent: intent.id(),
                        fence,
                        grid: MeasuredGrid::new(
                            24,
                            80,
                            CellPixels::new(8, 16).unwrap(),
                            SUPPORTED_GRID_LIMITS,
                        )
                        .unwrap(),
                    }),
                ),
            )
            .await
            .unwrap();
        delayed.entered().await;
        let request = request(
            &codec,
            2,
            1,
            Command::RevokePairing {
                pairing: first.pairing(),
            },
        );
        handle.submit(second, request.clone()).await.unwrap();
        let before = handle.snapshot();
        let store = StoreGate::new();
        db.lock().unwrap().gate = Some(store.clone());
        let barrier = Gate::new();
        *factory.barrier_gate.lock().unwrap() = Some(barrier.clone());
        let submitted = handle.clone();
        let task = tokio::spawn(async move {
            submitted
                .complete_pairing(
                    request,
                    PairingCompletionRequest {
                        allocation: PairingAllocationRequest::None,
                        revoked_clients: vec![client(1), client(2)],
                        preparation: Box::new(RevokePairingClients {
                            authorization,
                            signer,
                        }),
                    },
                )
                .await
        });
        let early_commit =
            tokio::time::timeout(std::time::Duration::from_millis(150), store.entered())
                .await
                .is_ok();
        assert_eq!(handle.snapshot(), before);
        delayed.release();
        if created {
            tokio::time::timeout(std::time::Duration::from_secs(3), barrier.entered())
                .await
                .unwrap();
            assert!(!task.is_finished());
            assert!(
                factory.states.lock().unwrap()[0]
                    .lock()
                    .unwrap()
                    .authority
                    .parts()
                    .connection
                    .is_none()
            );
            barrier.release();
        }
        if !early_commit {
            tokio::time::timeout(std::time::Duration::from_secs(3), store.entered())
                .await
                .unwrap();
        }
        store.release();
        task.await.unwrap().unwrap();
        stop(&handle, host).await.unwrap();
        server.wait().await.unwrap();
        assert!(
            !early_commit,
            "revocation committed before the affected launch owner was fenced"
        );
        assert_eq!(factory.starts.lock().unwrap().len(), usize::from(created));
    }
}

#[tokio::test]
async fn stop_acceptance_in_flight_retains_late_cleanup_for_an_already_failed_session() {
    use spaceterm_core::storage::*;
    for disposition in [
        None,
        Some(CommitDisposition::NotApplied(StoreFailure::Io)),
        Some(CommitDisposition::Indeterminate),
    ] {
        let fixture = Fixture::new();
        let principal = fixture.principal(1, 1);
        let codec = fixture.codec.clone();
        let host = fixture.host.clone();
        let db = fixture.db.clone();
        let factory = fixture.factory.clone();
        let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
            .await
            .unwrap();
        let handle = server.handle();
        connect(&handle, principal).await;
        let grant = acquire(&handle, principal, &codec).await;
        let session = start(&handle, principal, &codec, grant).await;
        let state = factory.states.lock().unwrap()[0].clone();
        let terminal = SessionTerminalResult::new(
            session,
            RuntimeTerminalStatus::RuntimeFailed {
                class: FailureClass::RuntimeFault,
            },
        )
        .unwrap();
        state.lock().unwrap().lifecycle = RuntimeLifecycle::Retiring { terminal };
        let mut snapshots = handle.subscribe_snapshot();
        while !matches!(
            snapshots.borrow().runtime()[0].phase,
            SessionPhase::Failed { .. }
        ) {
            snapshots.changed().await.unwrap();
        }
        let store = StoreGate::new();
        {
            let mut db = db.lock().unwrap();
            db.gate = Some(store.clone());
            db.next = disposition;
        }
        let stopped = handle.clone();
        let shutdown = tokio::spawn(async move { stop(&stopped, host).await });
        store.entered().await;
        {
            let db = db.lock().unwrap();
            assert!(db.transactions.last().unwrap().operation_writes.iter().any(|write| {
            matches!(write, OperationWrite::NativeRetirement(NativeRetirementMutation::Accept(descriptor))
                if descriptor.sessions().iter().any(|retained| retained.session() == session))
        }));
        }
        let cleanup = SessionCleanupObservation::new(
            session,
            terminal.retirement_reason(),
            CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
        )
        .unwrap();
        {
            let mut state = state.lock().unwrap();
            state.cleanup = Some(cleanup);
            state.lifecycle =
                RuntimeLifecycle::Finished(RuntimeCompletion::new(terminal, cleanup).unwrap());
        }
        loop {
            match handle.refresh_runtime_observations().await {
                Ok(()) => break,
                Err(error) if error.class == ServerFailureClass::Capacity => {
                    tokio::task::yield_now().await
                }
                Err(error) => panic!("{error:?}"),
            }
        }
        // If the actor incorrectly releases this owner, let its join and the
        // RuntimeJoined event finish before the acceptance transaction is durable.
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(150),
            factory.release_notice.notified(),
        )
        .await;
        let retained = factory.released.load(Ordering::SeqCst) == 0;
        handle
            .runtime_access(principal, session)
            .await
            .err()
            .unwrap();
        assert!(!shutdown.is_finished());
        store.release();
        let result = shutdown.await.unwrap();
        let joined = server.wait().await;
        if disposition == Some(CommitDisposition::Indeterminate) {
            assert_eq!(
                result.unwrap_err().class,
                ServerFailureClass::RecoveryRequired
            );
            assert_eq!(
                joined.unwrap_err().class,
                ServerFailureClass::RecoveryRequired
            );
        } else {
            assert!(
                result.is_ok(),
                "confirmed cleanup was lost during acceptance: {result:?}"
            );
            assert!(joined.is_ok(), "owner cleanup failed: {joined:?}");
        }
        assert!(
            retained,
            "an in-flight retirement must pin the exact owner until it installs or aborts"
        );
        assert_eq!(state.lock().unwrap().retire_calls, 0);
        assert_eq!(factory.abandoned.load(Ordering::SeqCst), 0);
        assert_eq!(factory.released.load(Ordering::SeqCst), 1);
        assert_eq!(factory.starts.lock().unwrap().len(), 1);
        if disposition == Some(CommitDisposition::Indeterminate) {
            assert_eq!(
                handle.snapshot().runtime()[0].phase,
                SessionPhase::Failed {
                    class: FailureClass::RuntimeFault
                }
            );
        } else {
            assert_eq!(
                handle.snapshot().runtime()[0].phase,
                SessionPhase::Ended {
                    reason: EndReason::Stop
                }
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_joins_a_long_native_git_write_before_retiring_sessions() {
    use spaceterm_host::git::{
        CancellationToken, ProgramRequest, RepositoryProgramRunner, UnixRepositoryProgramRunner,
    };
    use spaceterm_server::contracts::{
        ExternalWorkPort, RecoveryEdits, ServerFailure, ServerFuture,
    };
    use std::{fs, os::unix::fs::PermissionsExt as _, process::Stdio, sync::Arc, time::Duration};

    struct NativeWrite {
        read_cancellation: CancellationToken,
        write: tokio::sync::Mutex<Option<tokio::task::JoinHandle<Result<(), ServerFailure>>>>,
    }
    impl ExternalWorkPort for NativeWrite {
        fn cancel_reads(&self) {
            // The read token must never become the started write's token.
            self.read_cancellation.cancel();
        }
        fn finish_started_writes(&self) -> ServerFuture<'_, Result<(), ServerFailure>> {
            Box::pin(async move {
                let write = self.write.lock().await.take();
                match write {
                    Some(write) => write.await.map_err(|_| {
                        spaceterm_server::failure::failure(ServerFailureClass::CleanupIncomplete)
                    })?,
                    None => Ok(()),
                }
            })
        }
        fn reconcile<'a>(
            &'a self,
            _recovered: &'a spaceterm_core::storage::RecoveredState,
        ) -> ServerFuture<'a, Result<RecoveryEdits, ServerFailure>> {
            Box::pin(async {
                Ok(RecoveryEdits {
                    transactions: Vec::new(),
                })
            })
        }
    }
    struct ReleaseWrite(std::path::PathBuf);
    impl Drop for ReleaseWrite {
        fn drop(&mut self) {
            let _ = fs::write(&self.0, []);
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let release = ReleaseWrite(directory.path().join("release"));
    let mut init = std::process::Command::new("/usr/bin/git");
    init.args(["init", "-q"])
        .current_dir(directory.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    assert!(
        spaceterm_host::process::spawn(&mut init)
            .unwrap()
            .wait()
            .unwrap()
            .success()
    );
    let script = directory.path().join("long-write");
    fs::write(
        &script,
        b"#!/bin/sh\n: > started\nwhile [ ! -e release ]; do sleep 0.02; done\nexec /usr/bin/git -c user.name=Fixture -c user.email=fixture@invalid commit -q --allow-empty -m completed\n",
    )
    .unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();

    let mut fixture = Fixture::new();
    let principal = fixture.principal(1, 1);
    let codec = fixture.codec.clone();
    let host = fixture.host.clone();
    let factory = fixture.factory.clone();
    let external = Arc::new(NativeWrite {
        read_cancellation: CancellationToken::default(),
        write: tokio::sync::Mutex::new(None),
    });
    fixture.dependencies.external_work = external.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    connect(&handle, principal).await;
    let grant = acquire(&handle, principal, &codec).await;
    start(&handle, principal, &codec, grant).await;

    let request = ProgramRequest {
        executable: "/usr/bin/git".into(),
        arguments: ["-c", "alias.finish=!./long-write", "finish"]
            .into_iter()
            .map(Into::into)
            .collect(),
        directory: directory.path().to_owned(),
        environment: spaceterm_host::git::local_read::git_environment(
            std::path::Path::new("/usr/bin/git"),
            directory.path(),
        ),
        stdout_limit: Some(64 * 1024),
        deadline: None,
        keep_process_group_on_exit: false,
    };
    *external.write.lock().await = Some(tokio::task::spawn_blocking(move || {
        let result = UnixRepositoryProgramRunner::new().run(
            &request,
            &mut |_| {},
            &CancellationToken::default(),
        );
        match result {
            Ok(exit) if exit.success() => Ok(()),
            _ => Err(spaceterm_server::failure::failure(
                ServerFailureClass::CleanupIncomplete,
            )),
        }
    }));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !directory.path().join("started").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let shutdown = tokio::spawn(async move { stop(&handle, host).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !external.read_cancellation.is_cancelled() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!shutdown.is_finished());
    assert_eq!(
        factory.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .retire_calls,
        0
    );
    fs::write(&release.0, []).unwrap();
    let report = shutdown.await.unwrap().unwrap();
    server.wait().await.unwrap();
    assert!(report.started_writes_finished && report.cleanup_confirmed);
    assert_eq!(
        factory.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .retire_calls,
        1
    );
    let mut count = std::process::Command::new("/usr/bin/git");
    count
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(directory.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let output = spaceterm_host::process::spawn(&mut count)
        .unwrap()
        .wait_with_output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"1\n");
}
