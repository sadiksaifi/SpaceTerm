use spaceterm_core::{
    operation_ledger::canonical::CanonicalCodec,
    operation_ledger_contracts::*,
    operations::*,
    storage::*,
    worktrees::{queries::GitQueryResult, *},
};
use spaceterm_server::{
    api::*,
    contracts::*,
    git::{
        authority::{GitCompletionPlan, GitOrganizationPort},
        writes::*,
    },
};
use std::sync::atomic::Ordering;
#[allow(dead_code)]
#[path = "git_support/mod.rs"]
mod git_support;
use git_support::{Fixture, native, physical, run_git};

#[tokio::test]
async fn real_actor_git_create_retains_original_outcome_and_consumes_its_allocation() {
    let fixture = Fixture::new().await;
    let repository = fixture.inventory().await;
    let command = fixture.preview(repository, "created").await;
    let operation = fixture.operation(2);
    assert!(matches!(
        fixture
            .git
            .submit(fixture.git_principal, operation, command.clone())
            .await
            .unwrap(),
        GitSubmission::Pending(_)
    ));
    let outcome = fixture.git.wait(operation).await.unwrap();
    assert!(matches!(
        outcome.parts(),
        GitMutationOutcomeParts::Created { .. }
    ));
    assert!(fixture.directory.0.join("created").is_dir());
    assert_eq!(
        fixture
            .git
            .submit(fixture.git_principal, operation, command)
            .await
            .unwrap(),
        GitSubmission::Retained(outcome.clone())
    );
    assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 1);
    let (directory, stream) = fixture.stop().await;
    let mut store = physical::reopen(&directory.0).unwrap();
    let recovered = store.recover().unwrap();
    assert!(recovered.git_intents.is_empty() && recovered.git_allocations.is_empty());
    let record = recovered
        .ledger
        .unwrap()
        .operations()
        .iter()
        .find_map(|entry| match entry {
            RecoveredOperationRecord::Completed(record)
                if record.binding.id
                    == OperationId::new(stream, OperationSequence::new(2).unwrap()) =>
            {
                Some(record.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        record.outcome.outcome(&CanonicalCodec).unwrap(),
        Outcome::Git(outcome)
    );
}

#[tokio::test]
async fn real_actor_started_requires_the_serialized_prestarted_barrier() {
    let fixture = Fixture::new().await;
    let repository = fixture.inventory().await;
    let reservation = fixture.reserve(repository, "barrier").await;
    fixture
        .handle
        .persist_prepared_git(&reservation)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .handle
            .persist_started_git(&reservation)
            .await
            .unwrap_err()
            .class,
        ServerFailureClass::StaleAuthority
    );
    fixture
        .handle
        .barrier_and_revalidate_git(&reservation)
        .await
        .unwrap();
    let gate = native::StoreGate::new();
    let block = gate.clone();
    fixture.faults.barrier(
        spaceterm_host::storage::testing::FaultPoint::AfterCommit,
        move || block.block(),
    );
    let handle = fixture.handle.clone();
    let started = tokio::spawn(async move {
        handle
            .persist_started_git(&reservation)
            .await
            .map(|receipt| (receipt, reservation))
    });
    gate.entered().await;
    assert!(
        !started.is_finished(),
        "the physically committed Started prerequisite has not returned its durable permit"
    );
    assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 0);
    gate.release();
    let (_, reservation) = started.await.unwrap().unwrap();
    let mut intent = reservation.intent().parts().clone();
    intent.phase = GitIntentPhase::Started;
    let completion = fixture
        .handle
        .complete_git(GitCompletionPlan {
            intent: GitWriteIntent::new(intent).unwrap(),
            external: RetainedExternalResult {
                disposition: ExternalDisposition::NoEffectEstablished,
                failure: Some(GitFailure::WriteFailed),
                ended_sessions: 0,
            },
            recovery: None,
        })
        .await
        .unwrap();
    assert!(matches!(
        completion.disposition,
        CommitDisposition::Durable(_)
    ));
    GitOrganizationPort::release(&fixture.handle, reservation).await;
    fixture.stop().await;
}

#[tokio::test]
async fn definite_prepared_failure_settles_original_admission_without_git() {
    let fixture = Fixture::new().await;
    let repository = fixture.inventory().await;
    let reservation = fixture.reserve(repository, "not-started").await;
    fixture.faults.fail_once(
        spaceterm_host::storage::testing::FaultPoint::BeforeCommit,
        0,
    );
    assert!(
        fixture
            .handle
            .persist_prepared_git(&reservation)
            .await
            .is_err()
    );
    let completion = fixture
        .handle
        .complete_git(GitCompletionPlan {
            intent: reservation.intent().clone(),
            external: RetainedExternalResult {
                disposition: ExternalDisposition::NoEffectEstablished,
                failure: Some(GitFailure::StorageFailure),
                ended_sessions: 0,
            },
            recovery: None,
        })
        .await
        .unwrap();
    assert!(matches!(
        completion.disposition,
        CommitDisposition::Durable(_)
    ));
    assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 0);
    GitOrganizationPort::release(&fixture.handle, reservation).await;
    let (directory, _) = fixture.stop().await;
    let mut store = physical::reopen(&directory.0).unwrap();
    let recovered = store.recover().unwrap();
    assert!(recovered.git_intents.is_empty() && recovered.git_allocations.is_empty());
}

#[tokio::test]
async fn real_actor_git_remove_closes_affected_membership_only_after_external_success() {
    let fixture = Fixture::new().await;
    let repository = fixture.inventory().await;
    let create = fixture.preview(repository, "linked").await;
    fixture
        .git
        .submit(fixture.git_principal, fixture.operation(2), create)
        .await
        .unwrap();
    let created = fixture.git.wait(fixture.operation(2)).await.unwrap();
    let GitMutationOutcomeParts::Created { worktree, .. } = created.parts() else {
        panic!("created")
    };
    let tab_request = CanonicalOperationRequest::new(
        fixture.operation(3),
        Command::CreateTab {
            workspace: fixture.workspace,
            source: TabCreationSource::Worktree(*worktree),
        },
        &CanonicalCodec,
    )
    .unwrap();
    assert!(matches!(
        fixture
            .handle
            .submit(fixture.principal, tab_request)
            .await
            .unwrap(),
        OperationResponse::Completed {
            outcome: Outcome::CreatedWithControl(_),
            ..
        }
    ));
    let GitQueryResult::Assessment(page) = fixture
        .git
        .query(
            fixture.git_principal,
            GitQuery::AssessRemove {
                worktree: *worktree,
            },
        )
        .await
        .unwrap()
    else {
        panic!("remove assessment")
    };
    assert_eq!(page.tabs.len(), 1);
    let remove = GitCommand::RemoveConfirmed {
        assessment: page.token,
    };
    fixture
        .git
        .submit(fixture.git_principal, fixture.operation(4), remove.clone())
        .await
        .unwrap();
    let outcome = fixture.git.wait(fixture.operation(4)).await.unwrap();
    assert!(matches!(
        outcome.parts(),
        GitMutationOutcomeParts::Removed {
            closed_tabs: 1,
            ended_sessions: 0,
            ..
        }
    ));
    assert_eq!(fixture.handle.snapshot().state().tabs.len(), 1);
    assert!(!fixture.directory.0.join("linked").exists());
    assert_eq!(
        fixture
            .git
            .submit(fixture.git_principal, fixture.operation(4), remove)
            .await
            .unwrap(),
        GitSubmission::Retained(outcome)
    );
    assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 2);
    fixture.stop().await;
}

async fn crash_at_intent_phase(name: &str, phase: GitIntentPhase) {
    let variable = format!("SPACETERM_TEST_{name}");
    if let Some(path) = std::env::var_os(&variable) {
        let fixture = Fixture::reopen(physical::Directory(path.into())).await;
        let repository = fixture.inventory().await;
        let reservation = fixture.reserve(repository, name).await;
        if phase == GitIntentPhase::Started {
            fixture
                .handle
                .persist_prepared_git(&reservation)
                .await
                .unwrap();
            fixture
                .handle
                .barrier_and_revalidate_git(&reservation)
                .await
                .unwrap();
        }
        fixture.faults.barrier(
            spaceterm_host::storage::testing::FaultPoint::AfterCommit,
            || std::process::exit(73),
        );
        if phase == GitIntentPhase::Prepared {
            fixture
                .handle
                .persist_prepared_git(&reservation)
                .await
                .unwrap();
        } else {
            fixture
                .handle
                .persist_started_git(&reservation)
                .await
                .unwrap();
        }
        panic!("physical intent crash barrier did not fire");
    }
    let fixture = Fixture::new().await;
    fixture.inventory().await;
    let (directory, _) = fixture.stop().await;
    let test = match phase {
        GitIntentPhase::Prepared => "crash_after_prepared_recovers_without_write",
        _ => "crash_after_started_before_spawn_requires_reconciliation",
    };
    let path = directory.0.clone();
    let child = tokio::task::spawn_blocking(move || {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([test, "--exact", "--nocapture"])
            .env(variable, path)
            .status()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(child.code(), Some(73));
    let binding = {
        let mut store = physical::reopen(&directory.0).unwrap();
        let recovered = store.recover().unwrap();
        assert_eq!(recovered.git_intents.len(), 1);
        assert_eq!(recovered.git_intents[0].phase(), phase);
        recovered.git_intents[0].binding()
    };
    let restarted = Fixture::reopen(directory).await;
    assert_eq!(restarted.runner.writes.load(Ordering::SeqCst), 0);
    assert!(!restarted.directory.0.join(name).exists());
    let result = restarted
        .handle
        .lookup_state(restarted.principal, binding)
        .await
        .unwrap();
    assert!(
        matches!(result, OperationLookup::Completed { result: Outcome::Git(ref outcome), .. }
        if matches!(outcome.parts(), GitMutationOutcomeParts::Recovered { observation: RecoveryObservation::PreStateObserved, .. }))
    );
    let (directory, _) = restarted.stop().await;
    let mut store = physical::reopen(&directory.0).unwrap();
    let recovered = store.recover().unwrap();
    assert!(recovered.git_intents.is_empty() && recovered.git_allocations.is_empty());
}
#[tokio::test]
async fn crash_after_prepared_recovers_without_write() {
    crash_at_intent_phase("crash-prepared", GitIntentPhase::Prepared).await;
}
#[tokio::test]
async fn crash_after_started_before_spawn_requires_reconciliation() {
    crash_at_intent_phase("crash-started", GitIntentPhase::Started).await;
}
#[tokio::test]
async fn restoration_reconciles_unattached_inventory_before_a_client_query() {
    let fixture = Fixture::new().await;
    let scope = fixture.inventory().await;
    let (directory, _) = fixture.stop().await;
    let root = directory.0.join("repository");
    run_git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "outside",
            directory.0.join("outside").to_str().unwrap(),
        ],
    );
    let restored = Fixture::reopen(directory).await;
    let snapshot = restored.handle.snapshot();
    assert_eq!(snapshot.state().worktrees.len(), 2);
    assert!(
        snapshot
            .state()
            .worktrees
            .iter()
            .all(|v| v.repository == scope.repository)
    );
    assert_eq!(restored.runner.writes.load(Ordering::SeqCst), 0);
    restored.stop().await;
}

#[tokio::test]
async fn crash_after_git_before_completion_reconciles_same_identity() {
    let variable = "SPACETERM_TEST_GIT_AFTER_WRITE";
    if let Some(path) = std::env::var_os(variable) {
        let fixture = Fixture::reopen(physical::Directory(path.into())).await;
        let repository = fixture.inventory().await;
        let command = fixture.preview(repository, "after-write").await;
        *fixture.runner.after_write.lock().unwrap() = Some(Box::new(|| std::process::exit(74)));
        fixture
            .git
            .submit(fixture.git_principal, fixture.operation(2), command)
            .await
            .unwrap();
        fixture.git.wait(fixture.operation(2)).await.unwrap();
        panic!("native write crash barrier did not fire");
    }
    let fixture = Fixture::new().await;
    fixture.inventory().await;
    let (directory, _) = fixture.stop().await;
    let path = directory.0.clone();
    let status = tokio::task::spawn_blocking(move || {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "crash_after_git_before_completion_reconciles_same_identity",
                "--exact",
                "--nocapture",
            ])
            .env(variable, path)
            .status()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(status.code(), Some(74));
    let (binding, reserved) = {
        let mut store = physical::reopen(&directory.0).unwrap();
        let recovered = store.recover().unwrap();
        let intent = &recovered.git_intents[0];
        assert_eq!(intent.phase(), GitIntentPhase::Started);
        (intent.binding(), intent.parts().action.worktree())
    };
    assert!(directory.0.join("after-write").is_dir());
    let restored = Fixture::reopen(directory).await;
    assert_eq!(restored.runner.writes.load(Ordering::SeqCst), 0);
    assert!(
        restored
            .handle
            .snapshot()
            .state()
            .worktrees
            .iter()
            .any(|v| v.id == reserved && !v.missing)
    );
    assert!(
        matches!(restored.handle.lookup_state(restored.principal, binding).await.unwrap(), OperationLookup::Completed { result: Outcome::Git(ref outcome), .. }
        if matches!(outcome.parts(), GitMutationOutcomeParts::Recovered { observation: RecoveryObservation::DesiredStateObserved, .. }))
    );
    restored.stop().await;
}

#[tokio::test]
async fn indeterminate_final_commit_does_not_repeat_external_write() {
    let fixture = Fixture::new().await;
    let repository = fixture.inventory().await;
    let command = fixture.preview(repository, "indeterminate").await;
    let request = CanonicalOperationRequest::new(
        fixture.operation(2),
        Command::Worktree(command.clone()),
        &CanonicalCodec,
    )
    .unwrap();
    let binding = request.binding();
    let faults = fixture.faults.clone();
    *fixture.runner.after_write.lock().unwrap() = Some(Box::new(move || {
        faults.fail_once(
            spaceterm_host::storage::testing::FaultPoint::DatabaseSync,
            0,
        )
    }));
    fixture
        .git
        .submit(fixture.git_principal, fixture.operation(2), command)
        .await
        .unwrap();
    assert_eq!(
        fixture.git.wait(fixture.operation(2)).await.unwrap_err(),
        GitFailure::IndeterminateDurability
    );
    assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 1);
    assert!(fixture.directory.0.join("indeterminate").is_dir());
    assert!(fixture.server.wait().await.is_err());
    let restored = Fixture::reopen(fixture.directory).await;
    assert_eq!(restored.runner.writes.load(Ordering::SeqCst), 0);
    assert!(
        matches!(restored.handle.lookup_state(restored.principal, binding).await.unwrap(), OperationLookup::Completed { result: Outcome::Git(ref outcome), .. }
        if matches!(outcome.parts(), GitMutationOutcomeParts::Recovered { observation: RecoveryObservation::DesiredStateObserved, .. }))
    );
    restored.stop().await;
}

#[tokio::test]
async fn real_actor_new_affected_tab_invalidates_the_exact_removal_scope() {
    let fixture = Fixture::new().await;
    let worktree = fixture.create_linked("scope", 2).await;
    fixture.create_tab(worktree, 3).await;
    let GitQueryResult::Assessment(page) = fixture
        .git
        .query(fixture.git_principal, GitQuery::AssessRemove { worktree })
        .await
        .unwrap()
    else {
        panic!("assessment")
    };
    fixture.create_tab(worktree, 4).await;
    assert_eq!(
        fixture
            .git
            .submit(
                fixture.git_principal,
                fixture.operation(5),
                GitCommand::RemoveConfirmed {
                    assessment: page.token
                }
            )
            .await
            .unwrap_err(),
        GitFailure::StaleAssessment
    );
    assert_eq!(fixture.handle.snapshot().state().tabs.len(), 3);
    assert!(fixture.directory.0.join("scope").is_dir());
    assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 1);
    fixture.stop().await;
}

#[tokio::test]
async fn reserved_removal_blocks_a_new_pin_outside_its_affected_workspaces() {
    use spaceterm_core::authority::FieldVersion;
    use spaceterm_core::organization::ServerDirectory;
    let fixture = Fixture::new().await;
    let worktree = fixture.create_linked("pin-gate", 2).await;
    let reservation = fixture.reserve_remove(worktree, 3).await;
    let before = fixture.handle.snapshot();
    let workspace = &before.state().workspaces[0];
    let request = CanonicalOperationRequest::new(
        fixture.operation(4),
        Command::SetPinnedDirectory {
            workspace: workspace.id,
            expected_version: workspace.pinned_directory.version,
            value: Some(
                ServerDirectory::new(
                    fixture.stream.server(),
                    fixture
                        .directory
                        .0
                        .join("pin-gate")
                        .as_os_str()
                        .as_encoded_bytes()
                        .to_vec(),
                )
                .unwrap(),
            ),
        },
        &CanonicalCodec,
    )
    .unwrap();
    assert!(matches!(
        fixture
            .handle
            .submit(fixture.principal, request)
            .await
            .unwrap(),
        OperationResponse::Completed {
            outcome: Outcome::Failed(OperationFailure::BusyRetiring),
            ..
        }
    ));
    assert_eq!(
        fixture.handle.snapshot().state().workspaces[0].pinned_directory,
        workspace.pinned_directory
    );
    assert_eq!(workspace.pinned_directory.version, FieldVersion::new(1));
    fixture
        .handle
        .complete_git(GitCompletionPlan {
            intent: reservation.intent().clone(),
            external: RetainedExternalResult {
                disposition: ExternalDisposition::NoEffectEstablished,
                failure: Some(GitFailure::StaleAssessment),
                ended_sessions: 0,
            },
            recovery: None,
        })
        .await
        .unwrap();
    GitOrganizationPort::release(&fixture.handle, reservation).await;
    fixture.stop().await;
}

#[tokio::test]
async fn rejected_prestarted_git_restores_only_a_fresh_runtime_context() {
    let fixture = Fixture::new().await;
    let worktree = fixture.create_linked("restore-fence", 2).await;
    let pane = fixture.create_tab(worktree, 3).await;
    let session = fixture.start_pane(pane, 4).await;
    let old = fixture
        .handle
        .runtime_access(fixture.principal, session)
        .await
        .unwrap()
        .authority;
    let reservation = fixture.reserve_remove(worktree, 6).await;
    fixture
        .handle
        .persist_prepared_git(&reservation)
        .await
        .unwrap();
    fixture
        .handle
        .barrier_and_revalidate_git(&reservation)
        .await
        .unwrap();
    fixture
        .handle
        .complete_git(GitCompletionPlan {
            intent: reservation.intent().clone(),
            external: RetainedExternalResult {
                disposition: ExternalDisposition::NoEffectEstablished,
                failure: Some(GitFailure::StaleAssessment),
                ended_sessions: 0,
            },
            recovery: None,
        })
        .await
        .unwrap();
    GitOrganizationPort::release(&fixture.handle, reservation).await;
    let current = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Ok(access) = fixture
                .handle
                .runtime_access(fixture.principal, session)
                .await
            {
                break access.authority;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(current.parts().context.get() > old.parts().context.get() + 1);
    assert_eq!(current.parts().grant, old.parts().grant);
    assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 1);
    fixture.stop().await;
}

#[tokio::test]
async fn real_actor_started_removal_retires_its_exact_owned_session_before_deletion() {
    let fixture = Fixture::new().await;
    let worktree = fixture.create_linked("live-removal", 2).await;
    let pane = fixture.create_tab(worktree, 3).await;
    let session = fixture.start_pane(pane, 4).await;
    let GitQueryResult::Assessment(page) = fixture
        .git
        .query(fixture.git_principal, GitQuery::AssessRemove { worktree })
        .await
        .unwrap()
    else {
        panic!("assessment")
    };
    fixture
        .git
        .submit(
            fixture.git_principal,
            fixture.operation(6),
            GitCommand::RemoveConfirmed {
                assessment: page.token,
            },
        )
        .await
        .unwrap();
    let outcome = fixture.git.wait(fixture.operation(6)).await.unwrap();
    assert!(matches!(
        outcome.parts(),
        GitMutationOutcomeParts::Removed {
            closed_tabs: 1,
            ended_sessions: 1,
            ..
        }
    ));
    assert!(
        !fixture
            .handle
            .snapshot()
            .runtime()
            .iter()
            .any(|v| v.pane == session.pane)
    );
    assert!(!fixture.directory.0.join("live-removal").exists());
    assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn started_retirement_survives_initiating_client_revocation_without_new_authority() {
    let fixture = Fixture::new().await;
    let worktree = fixture.create_linked("revoked-retirement", 2).await;
    let pane = fixture.create_tab(worktree, 3).await;
    let session = fixture.start_pane(pane, 4).await;
    let reservation = fixture.reserve_remove(worktree, 6).await;
    fixture
        .handle
        .persist_prepared_git(&reservation)
        .await
        .unwrap();
    fixture
        .handle
        .barrier_and_revalidate_git(&reservation)
        .await
        .unwrap();
    fixture
        .handle
        .persist_started_git(&reservation)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match fixture
                .handle
                .revoke_clients(vec![fixture.stream.client()])
                .await
            {
                Ok(_) => break,
                Err(error) if error.class == ServerFailureClass::Capacity => {
                    tokio::task::yield_now().await
                }
                Err(error) => panic!("revocation failed: {:?}", error.class),
            }
        }
    })
    .await
    .unwrap();
    let report = fixture
        .handle
        .retire_started_git(&reservation)
        .await
        .unwrap();
    assert_eq!(report.cleaned, vec![session]);
    let mut intent = reservation.intent().parts().clone();
    intent.phase = GitIntentPhase::Started;
    fixture
        .handle
        .complete_git(GitCompletionPlan {
            intent: GitWriteIntent::new(intent).unwrap(),
            external: RetainedExternalResult {
                disposition: ExternalDisposition::NoEffectEstablished,
                failure: Some(GitFailure::WriteFailed),
                ended_sessions: 1,
            },
            recovery: None,
        })
        .await
        .unwrap();
    GitOrganizationPort::release(&fixture.handle, reservation).await;
    assert!(
        fixture
            .handle
            .snapshot()
            .runtime()
            .iter()
            .all(|v| v.control == spaceterm_core::organization::ControlRecord::Unclaimed)
    );
    assert!(fixture.directory.0.join("revoked-retirement").exists());
    fixture.stop().await;
}

#[tokio::test]
async fn crash_after_allocation_before_prepared_burns_ids_and_settles_original_pending() {
    let variable = "SPACETERM_TEST_GIT_AFTER_ALLOCATION";
    if let Some(path) = std::env::var_os(variable) {
        let fixture = Fixture::reopen(physical::Directory(path.into())).await;
        let scope = fixture.inventory().await;
        fixture.reserve(scope, "allocation-crash").await;
        std::process::exit(75);
    }
    let fixture = Fixture::new().await;
    fixture.inventory().await;
    let (directory, _) = fixture.stop().await;
    let path = directory.0.clone();
    let status = tokio::task::spawn_blocking(move || {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "crash_after_allocation_before_prepared_burns_ids_and_settles_original_pending",
                "--exact",
                "--nocapture",
            ])
            .env(variable, path)
            .status()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(status.code(), Some(75));
    let (binding, marks) = {
        let mut store = physical::reopen(&directory.0).unwrap();
        let recovered = store.recover().unwrap();
        assert!(recovered.git_intents.is_empty());
        assert_eq!(recovered.git_allocations.len(), 1);
        (recovered.git_allocations[0].binding(), recovered.high_water)
    };
    let restored = Fixture::reopen(directory).await;
    assert_eq!(restored.runner.writes.load(Ordering::SeqCst), 0);
    assert!(matches!(
        restored
            .handle
            .lookup_state(restored.principal, binding)
            .await
            .unwrap(),
        OperationLookup::Completed {
            result: Outcome::Failed(OperationFailure::StaleAuthority),
            ..
        }
    ));
    let (directory, _) = restored.stop().await;
    let mut store = physical::reopen(&directory.0).unwrap();
    let recovered = store.recover().unwrap();
    assert_eq!(recovered.high_water, marks);
    assert!(recovered.git_allocations.is_empty());
}

#[tokio::test]
async fn restoration_preserves_missing_referenced_worktree_identity_and_membership() {
    let fixture = Fixture::new().await;
    let worktree = fixture.create_linked("missing", 2).await;
    let pane = fixture.create_tab(worktree, 3).await;
    let tab = fixture
        .handle
        .snapshot()
        .state()
        .panes
        .iter()
        .find(|v| v.id == pane)
        .unwrap()
        .tab;
    let (directory, _) = fixture.stop().await;
    run_git(
        &directory.0.join("repository"),
        &[
            "worktree",
            "remove",
            directory.0.join("missing").to_str().unwrap(),
        ],
    );
    let restored = Fixture::reopen(directory).await;
    let snapshot = restored.handle.snapshot();
    assert!(
        snapshot
            .state()
            .worktrees
            .iter()
            .any(|v| v.id == worktree && v.missing)
    );
    assert!(
        snapshot
            .state()
            .tabs
            .iter()
            .any(|v| v.id == tab && v.worktree == Some(worktree))
    );
    assert!(snapshot.state().panes.iter().any(|v| v.id == pane));
    assert_eq!(restored.runner.writes.load(Ordering::SeqCst), 0);
    restored.stop().await;
}

#[tokio::test]
async fn failed_restoration_inventory_keeps_last_committed_membership() {
    let fixture = Fixture::new().await;
    let worktree = fixture.create_linked("unavailable", 2).await;
    fixture.create_tab(worktree, 3).await;
    let before = fixture.handle.snapshot();
    let (directory, _) = fixture.stop().await;
    std::fs::rename(
        directory.0.join("repository/.git"),
        directory.0.join("repository/.git-unavailable"),
    )
    .unwrap();
    let restored = Fixture::reopen(directory).await;
    assert_eq!(restored.handle.snapshot().state().tabs, before.state().tabs);
    assert_eq!(
        restored.handle.snapshot().state().worktrees,
        before.state().worktrees
    );
    assert_eq!(restored.runner.writes.load(Ordering::SeqCst), 0);
    restored.stop().await;
}

#[tokio::test]
async fn started_retirement_preserves_a_later_end_all_phase_and_first_cleanup_reason() {
    use spaceterm_core::organization::{EndReason, SessionPhase};
    let fixture = Fixture::new().await;
    let worktree = fixture.create_linked("end-all-transfer", 2).await;
    let pane = fixture.create_tab(worktree, 3).await;
    let session = fixture.start_pane(pane, 4).await;
    let reservation = fixture.reserve_remove(worktree, 6).await;
    fixture
        .handle
        .persist_prepared_git(&reservation)
        .await
        .unwrap();
    fixture
        .handle
        .barrier_and_revalidate_git(&reservation)
        .await
        .unwrap();
    fixture
        .handle
        .persist_started_git(&reservation)
        .await
        .unwrap();
    let assessed = CanonicalOperationRequest::new(
        fixture.operation(7),
        Command::AssessEndAllSessions {
            server: fixture.stream.server(),
        },
        &CanonicalCodec,
    )
    .unwrap();
    let OperationResponse::Completed {
        outcome: Outcome::Assessment(assessment),
        ..
    } = fixture.submit_when_available(assessed).await
    else {
        panic!("end all assessment")
    };
    let request = CanonicalOperationRequest::new(
        fixture.operation(8),
        Command::EndAllSessions {
            server: fixture.stream.server(),
            assessment: assessment.reference(),
        },
        &CanonicalCodec,
    )
    .unwrap();
    fixture.submit_when_available(request).await;
    let retired = fixture
        .handle
        .retire_started_git(&reservation)
        .await
        .unwrap();
    assert_eq!(retired.cleaned, vec![session]);
    let mut intent = reservation.intent().parts().clone();
    intent.phase = GitIntentPhase::Started;
    fixture
        .handle
        .complete_git(GitCompletionPlan {
            intent: GitWriteIntent::new(intent).unwrap(),
            external: RetainedExternalResult {
                disposition: ExternalDisposition::NoEffectEstablished,
                failure: Some(GitFailure::WriteFailed),
                ended_sessions: 1,
            },
            recovery: None,
        })
        .await
        .unwrap();
    GitOrganizationPort::release(&fixture.handle, reservation).await;
    assert!(
        fixture
            .handle
            .snapshot()
            .runtime()
            .iter()
            .filter(|v| v.pane == pane)
            .all(|v| v.phase
                == SessionPhase::Ended {
                    reason: EndReason::EndAllSessions
                })
    );
    fixture.stop().await;
}

#[tokio::test]
async fn assessment_cannot_cross_pairing_client_server_or_epoch() {
    use spaceterm_core::identity::{ClientId, PairingId, ServerId};
    let fixture = Fixture::new().await;
    let repository = fixture.inventory().await;
    let command = fixture.preview(repository, "scope-binding").await;
    let mut crossed = Vec::new();
    let mut parts = command.assessment().parts().clone();
    parts.pairing = PairingId::new(
        fixture.stream.server(),
        fixture.stream.pairing().local() + 1,
    )
    .unwrap();
    crossed.push(parts);
    let mut parts = command.assessment().parts().clone();
    parts.client =
        ClientId::new(fixture.stream.server(), fixture.stream.client().local() + 1).unwrap();
    crossed.push(parts);
    let mut parts = command.assessment().parts().clone();
    let foreign = ServerId::new([98; 16]);
    parts.server = foreign;
    parts.pairing = PairingId::new(foreign, fixture.stream.pairing().local()).unwrap();
    parts.client = ClientId::new(foreign, fixture.stream.client().local()).unwrap();
    crossed.push(parts);
    let mut parts = command.assessment().parts().clone();
    parts.epoch = parts.epoch.checked_successor().unwrap();
    crossed.push(parts);
    for parts in crossed {
        let command = GitCommand::CreateConfirmed {
            assessment: GitAssessmentToken::new(parts).unwrap(),
        };
        let failure = fixture
            .git
            .submit(fixture.git_principal, fixture.operation(2), command)
            .await
            .unwrap_err();
        assert!(matches!(
            failure,
            GitFailure::Unauthorized | GitFailure::WrongServer | GitFailure::StaleAssessment
        ));
    }
    assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 0);
    assert!(!fixture.directory.0.join("scope-binding").exists());
    fixture.stop().await;
}
