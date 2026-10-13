#[path = "support/operation_ledger.rs"]
mod support;
use spaceterm_core::authority::*;
use spaceterm_core::identity::*;
use spaceterm_core::operation_ledger::model::*;
use spaceterm_core::operation_ledger::projection::validate_transaction_projection;
use spaceterm_core::operation_ledger::*;
use spaceterm_core::operations::*;
use spaceterm_core::organization::*;
use spaceterm_core::retirement::*;
use spaceterm_core::storage::*;
use support::*;

#[test]
fn admission_produces_no_execution_permit_before_durable_receipt() {
    let mut h = Harness::new();
    h.register(binding(1));
    let AdmissionPreparation::Prepared(p) = h.prepare(binding(1), 1, &create()).unwrap() else {
        panic!("proposal")
    };
    assert_eq!(
        h.ledger
            .lookup(&binding(1), op(binding(1), 1), None)
            .unwrap(),
        OperationLookup::NeverAdmitted
    );
    assert!(matches!(h.commit(p), TransitionResolution::Admitted(_)));
}
#[test]
fn not_applied_admission_changes_no_durable_state() {
    let mut h = Harness::new();
    h.register(binding(1));
    let before = h.rows.recovery();
    let AdmissionPreparation::Prepared(p) = h.prepare(binding(1), 1, &create()).unwrap() else {
        panic!("proposal")
    };
    h.ledger
        .resolve_commit(p, CommitDisposition::NotApplied(StoreFailure::Io))
        .unwrap();
    assert_eq!(h.rows.recovery(), before);
    h.admit(binding(1), 1, &create());
}
#[test]
fn final_outcome_and_organization_writes_share_one_transaction() {
    let mut h = Harness::new();
    h.register(binding(1));
    let permit = h.admit(binding(1), 1, &create());
    let context = h.rows.reduction(permit.binding(), true);
    let p = h
        .ledger
        .prepare_completion(
            &binding(1),
            permit.binding().id,
            permit.binding().digest,
            compact(&Outcome::Applied),
            context.resulting_organization(),
            context,
        )
        .unwrap();
    let mut transaction = parts(p.projection());
    transaction
        .organization_writes
        .push(OrganizationWrite::DeleteWorkspace(
            WorkspaceId::new(server(), 1).unwrap(),
        ));
    validate_transaction_projection(&h.ledger, &transaction).unwrap();
    struct Validator<'a>(&'a OperationLedger);
    impl DurabilityValidator for Validator<'_> {
        fn validate_transaction_projection(
            &self,
            parts: &TransactionParts,
        ) -> Result<(), StoreFailure> {
            validate_transaction_projection(self.0, parts)
                .map_err(|_| StoreFailure::InvalidTransaction)
        }
        fn validate_recovery(&self, recovered: &RecoveredState) -> Result<(), StoreFailure> {
            spaceterm_core::operation_ledger::recovery::validate_recovered_state(recovered)
                .map_err(|_| StoreFailure::InvalidDurableState)
        }
    }
    let tx = DurableTransaction::new_with_validation(transaction.clone(), &Validator(&h.ledger))
        .unwrap();
    assert_eq!(tx.parts().organization_writes.len(), 1);
    assert!(tx.parts().operation_writes.iter().any(|w| matches!(w, OperationWrite::Ledger(v) if matches!(v.as_ref(), LedgerWrite::CompleteOperation { .. }))));
    transaction
        .operation_writes
        .retain(|v| !matches!(v, OperationWrite::ReductionBasis(_)));
    assert!(validate_transaction_projection(&h.ledger, &transaction).is_err());
    h.commit(p);
}
#[test]
fn not_applied_completion_keeps_pending_identity_and_reservation() {
    let mut h = Harness::new();
    h.register(binding(1));
    let permit = h.admit(binding(1), 1, &create());
    let context = h.rows.reduction(permit.binding(), false);
    let p = h
        .ledger
        .prepare_completion(
            &binding(1),
            permit.binding().id,
            permit.binding().digest,
            compact(&Outcome::Failed(OperationFailure::TargetDeleted)),
            context.resulting_organization(),
            context,
        )
        .unwrap();
    h.ledger
        .resolve_commit(p, CommitDisposition::NotApplied(StoreFailure::Io))
        .unwrap();
    assert_eq!(
        h.ledger
            .lookup(&binding(1), op(binding(1), 1), None)
            .unwrap(),
        OperationLookup::Pending
    );
    h.complete(binding(1), 1, &Outcome::Applied, false);
}
#[test]
fn conflict_completion_preserves_field_version_and_original_result() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &rename("rejected draft"));
    let outcome = Outcome::Conflict(FieldConflict::WorkspaceName {
        workspace: WorkspaceId::new(server(), 1).unwrap(),
        current_value: WorkspaceName {
            mode: NameMode::Custom,
            display: NameText::new("current".into()).unwrap(),
        },
        current_version: FieldVersion::new(7),
    });
    h.complete(binding(1), 1, &outcome, false);
    h.rows.organization = OrganizationRevision::new(99);
    h.rows.store = StoreRevision::new(99);
    h.reopen();
    assert_eq!(
        h.ledger
            .lookup(&binding(1), op(binding(1), 1), None)
            .unwrap(),
        OperationLookup::Completed {
            result: outcome,
            publication_revision: OrganizationRevision::new(0)
        }
    );
}
#[test]
fn only_one_prepared_transaction_is_allowed() {
    let mut h = Harness::new();
    let p = h
        .ledger
        .prepare_register_stream(binding(1), h.rows.context())
        .unwrap();
    assert_class(
        h.ledger
            .prepare_register_stream(binding(2), h.rows.context()),
        LedgerFailureClass::PreparationBusy,
    );
    h.commit(p);
}
#[test]
fn foreign_or_stale_transition_cannot_install_state() {
    let mut h = Harness::new();
    let mut other = Harness::new();
    let p = h
        .ledger
        .prepare_register_stream(binding(1), h.rows.context())
        .unwrap();
    assert_class(
        other.ledger.resolve_commit(
            p,
            CommitDisposition::Durable(CommitReceipt {
                store_revision: StoreRevision::new(1),
                organization_revision: OrganizationRevision::new(0),
            }),
        ),
        LedgerFailureClass::InvalidTransition,
    );
    h.ledger.install_recovery(h.rows.recovery()).unwrap();
    let p = h
        .ledger
        .prepare_register_stream(binding(1), h.rows.context())
        .unwrap();
    h.ledger.install_recovery(h.rows.recovery()).unwrap();
    assert_class(
        h.ledger
            .resolve_commit(p, CommitDisposition::NotApplied(StoreFailure::Io)),
        LedgerFailureClass::InvalidTransition,
    );
}
#[test]
fn receipt_revision_mismatch_closes_admission() {
    let mut h = Harness::new();
    let p = h
        .ledger
        .prepare_register_stream(binding(1), h.rows.context())
        .unwrap();
    assert_class(
        h.ledger.resolve_commit(
            p,
            CommitDisposition::Durable(CommitReceipt {
                store_revision: StoreRevision::new(2),
                organization_revision: OrganizationRevision::new(0),
            }),
        ),
        LedgerFailureClass::InvalidDurableState,
    );
    assert_class(
        h.ledger
            .prepare_register_stream(binding(2), h.rows.context()),
        LedgerFailureClass::RecoveryRequired,
    );
}
#[test]
fn indeterminate_admission_closes_all_write_paths() {
    let mut h = Harness::new();
    h.register(binding(1));
    let AdmissionPreparation::Prepared(p) = h.prepare(binding(1), 1, &create()).unwrap() else {
        panic!("proposal")
    };
    assert!(matches!(
        h.ledger
            .resolve_commit(p, CommitDisposition::Indeterminate)
            .unwrap(),
        TransitionResolution::RecoveryRequired
    ));
    assert_class(
        h.prepare(binding(1), 1, &create()),
        LedgerFailureClass::RecoveryRequired,
    );
    assert_class(
        h.ledger
            .prepare_register_stream(binding(2), h.rows.context()),
        LedgerFailureClass::RecoveryRequired,
    );
    assert_class(
        h.ledger
            .prepare_acknowledgement(&binding(1), 0, h.rows.context()),
        LedgerFailureClass::RecoveryRequired,
    );
    assert_class(
        h.ledger.prepare_retirement(
            &[binding(1)],
            StreamRetirementReason::Expired,
            h.rows.context(),
        ),
        LedgerFailureClass::RecoveryRequired,
    );
    assert_class(
        h.ledger
            .resume_recovered_atomic(&binding(1), op(binding(1), 1), &create()),
        LedgerFailureClass::RecoveryRequired,
    );
}
#[test]
fn indeterminate_completion_and_acknowledgement_return_no_success() {
    let mut h = Harness::new();
    h.register(binding(1));
    let permit = h.admit(binding(1), 1, &create());
    let context = h.rows.reduction(permit.binding(), true);
    let p = h
        .ledger
        .prepare_completion(
            &binding(1),
            permit.binding().id,
            permit.binding().digest,
            compact(&Outcome::Applied),
            context.resulting_organization(),
            context,
        )
        .unwrap();
    h.rows.apply(p.projection());
    assert!(matches!(
        h.ledger
            .resolve_commit(p, CommitDisposition::Indeterminate)
            .unwrap(),
        TransitionResolution::RecoveryRequired
    ));
    assert_eq!(
        h.ledger
            .lookup(&binding(1), op(binding(1), 1), None)
            .unwrap(),
        OperationLookup::Pending
    );
    h.ledger.install_recovery(h.rows.recovery()).unwrap();
    let p = h
        .ledger
        .prepare_acknowledgement(&binding(1), 1, h.rows.context())
        .unwrap()
        .unwrap();
    assert!(matches!(
        h.ledger
            .resolve_commit(p, CommitDisposition::Indeterminate)
            .unwrap(),
        TransitionResolution::RecoveryRequired
    ));
    assert!(matches!(
        h.ledger
            .lookup(&binding(1), op(binding(1), 1), None)
            .unwrap(),
        OperationLookup::Completed { .. }
    ));
}
#[test]
fn shutdown_blocks_new_admission_but_allows_started_completion() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &create());
    h.ledger.close_admissions();
    assert_class(
        h.prepare(binding(1), 2, &create()),
        LedgerFailureClass::AdmissionsClosed,
    );
    h.complete(binding(1), 1, &Outcome::Applied, false);
    h.ack(binding(1), 1);
    h.ledger.install_recovery(h.rows.recovery()).unwrap();
    assert_class(
        h.prepare(binding(1), 2, &create()),
        LedgerFailureClass::AdmissionsClosed,
    );
}
#[test]
fn external_admission_waits_for_durable_prerequisite_and_preserves_acceptance_revision() {
    let mut h = Harness::new();
    h.register(binding(1));
    let permit = h.admit(binding(1), 1, &retirement_command(binding(1)));
    assert!(!permit.permits_external_effects());
    let context = h.rows.reduction(permit.binding(), false);
    assert_class(
        h.ledger.prepare_completion(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            compact(&Outcome::Applied),
            context.resulting_organization(),
            context,
        ),
        LedgerFailureClass::InvalidExternalProgress,
    );
    let context = h.rows.reduction(permit.binding(), true);
    let progress = retirement_progress(
        permit.binding(),
        context.resulting_organization(),
        RetirementCleanupState::AwaitingEvidence,
    );
    let p = h
        .ledger
        .prepare_external_progress(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            progress,
            context,
        )
        .unwrap();
    assert!(matches!(
        h.commit(p),
        TransitionResolution::ExternalProgressRecorded
    ));
    h.rows.organization = OrganizationRevision::new(7);
    h.rows.store = StoreRevision::new(h.rows.store.get() + 1);
    assert_class(
        h.ledger.prepare_completion(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            compact(&Outcome::Applied),
            OrganizationRevision::new(1),
            h.rows.context(),
        ),
        LedgerFailureClass::InvalidExternalProgress,
    );
    let progress = retirement_progress(
        permit.binding(),
        OrganizationRevision::new(1),
        RetirementCleanupState::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
    );
    let p = h
        .ledger
        .prepare_external_progress(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            progress,
            h.rows.context(),
        )
        .unwrap();
    h.commit(p);
    let p = h
        .ledger
        .prepare_completion(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            compact(&Outcome::Applied),
            OrganizationRevision::new(1),
            h.rows.context(),
        )
        .unwrap();
    h.commit(p);
    assert!(
        matches!(h.ledger.lookup(&binding(1), op(binding(1), 1), None).unwrap(), OperationLookup::Completed { publication_revision, .. } if publication_revision.get() == 1)
    );
}
#[test]
fn retired_stream_allows_started_external_progress_and_completion_but_no_new_execution() {
    use spaceterm_core::worktrees::intent::*;
    for (closed, retired) in [(true, false), (false, true), (true, true)] {
        let mut h = Harness::new();
        h.register(binding(1));
        let command = Command::CreateWorktree {
            scope: RepositoryScope {
                server: server(),
                repository: RepositoryId::new(server(), 1).unwrap(),
            },
            branch: BranchName::new("new".into()).unwrap(),
        };
        let permit = h.admit(binding(1), 1, &command);
        for phase in [ExternalIntentPhase::Prepared, ExternalIntentPhase::Started] {
            let progress = git_progress(
                permit.binding(),
                phase,
                h.rows.store.checked_successor().unwrap(),
            );
            let p = h
                .ledger
                .prepare_external_progress(
                    &binding(1),
                    op(binding(1), 1),
                    permit.binding().digest,
                    progress,
                    h.rows.context(),
                )
                .unwrap();
            let mut tx = parts(p.projection());
            tx.operation_writes.push(OperationWrite::GitIntent(
                if phase == ExternalIntentPhase::Prepared {
                    GitIntentMutation::Insert(Box::new(git_intent(
                        permit.binding(),
                        GitIntentPhase::Prepared,
                    )))
                } else {
                    GitIntentMutation::Advance {
                        binding: permit.binding(),
                        expected_phase: GitIntentPhase::Prepared,
                        next: GitIntentPhase::Started,
                        result: None,
                    }
                },
            ));
            validate_transaction_projection(&h.ledger, &tx).unwrap();
            h.commit(p);
        }
        if retired {
            h.retire(binding(1), StreamRetirementReason::PairingRevoked);
        }
        if closed {
            h.ledger.close_admissions();
        }
        let progress = git_progress(
            permit.binding(),
            ExternalIntentPhase::ExternalFinished,
            h.rows.store.checked_successor().unwrap(),
        );
        let p = h
            .ledger
            .prepare_external_progress(
                &binding(1),
                op(binding(1), 1),
                permit.binding().digest,
                progress,
                h.rows.context(),
            )
            .unwrap();
        let mut tx = parts(p.projection());
        tx.operation_writes
            .push(OperationWrite::GitIntent(GitIntentMutation::Advance {
                binding: permit.binding(),
                expected_phase: GitIntentPhase::Started,
                next: GitIntentPhase::ExternalFinished,
                result: git_intent(permit.binding(), GitIntentPhase::ExternalFinished)
                    .parts()
                    .external_result
                    .clone(),
            }));
        validate_transaction_projection(&h.ledger, &tx).unwrap();
        h.commit(p);
        let context = h.rows.reduction(permit.binding(), true);
        let p = h
            .ledger
            .prepare_completion(
                &binding(1),
                permit.binding().id,
                permit.binding().digest,
                compact(&Outcome::CreatedWorktree {
                    worktree: WorktreeId::new(server(), 1).unwrap(),
                }),
                context.resulting_organization(),
                context,
            )
            .unwrap();
        let mut tx = parts(p.projection());
        tx.operation_writes
            .push(OperationWrite::GitIntent(GitIntentMutation::Complete {
                binding: permit.binding(),
            }));
        validate_transaction_projection(&h.ledger, &tx).unwrap();
        h.commit(p);
        if retired {
            assert!(matches!(
                h.prepare(binding(1), 2, &command).unwrap(),
                AdmissionPreparation::Existing(OperationLookup::AcknowledgedRetired)
            ));
        } else {
            assert_class(
                h.prepare(binding(1), 2, &command),
                LedgerFailureClass::AdmissionsClosed,
            );
        }
    }
}
fn quiesce(h: &mut Harness, revoked: bool) -> LedgerFailureClass {
    if revoked {
        h.retire(binding(1), StreamRetirementReason::PairingRevoked);
        LedgerFailureClass::Retired
    } else {
        h.ledger.close_admissions();
        LedgerFailureClass::AdmissionsClosed
    }
}
fn external_progress_parts(
    h: &Harness,
    binding: OperationBinding,
    progress: PendingExternalProgress,
    context: TransactionContext,
) -> TransactionParts {
    let Some(RecoveredOperationRecord::Pending(old)) = h
        .ledger
        .operation(&StreamBinding::from_stream(binding.id.stream()), binding.id)
        .unwrap()
    else {
        panic!("pending operation")
    };
    let mut record = old.as_ref().clone();
    record.progress_recorded_at = context.expected_store().checked_successor().unwrap();
    record.external_progress = Some(progress);
    parts(
        &LedgerProjection::new(
            context,
            vec![LedgerWrite::RecordExternalProgress {
                previous_progress_store_revision: old.progress_recorded_at,
                record,
            }],
        )
        .unwrap(),
    )
}
#[test]
fn quiescence_rejects_preparing_and_starting_git_with_matching_prerequisites() {
    use spaceterm_core::worktrees::intent::*;
    for revoked in [false, true] {
        for starting in [false, true] {
            let mut h = Harness::new();
            h.register(binding(1));
            let command = Command::CreateWorktree {
                scope: RepositoryScope {
                    server: server(),
                    repository: RepositoryId::new(server(), 1).unwrap(),
                },
                branch: BranchName::new("new".into()).unwrap(),
            };
            let permit = h.admit(binding(1), 1, &command);
            if starting {
                let p = h
                    .ledger
                    .prepare_external_progress(
                        &binding(1),
                        permit.binding().id,
                        permit.binding().digest,
                        git_progress(
                            permit.binding(),
                            ExternalIntentPhase::Prepared,
                            h.rows.store.checked_successor().unwrap(),
                        ),
                        h.rows.context(),
                    )
                    .unwrap();
                let mut tx = parts(p.projection());
                tx.operation_writes
                    .push(OperationWrite::GitIntent(GitIntentMutation::Insert(
                        Box::new(git_intent(permit.binding(), GitIntentPhase::Prepared)),
                    )));
                validate_transaction_projection(&h.ledger, &tx).unwrap();
                h.commit(p);
            }
            let expected = quiesce(&mut h, revoked);
            let before = h.ledger.retained();
            let progress = git_progress(
                permit.binding(),
                if starting {
                    ExternalIntentPhase::Started
                } else {
                    ExternalIntentPhase::Prepared
                },
                h.rows.store.checked_successor().unwrap(),
            );
            let mut tx =
                external_progress_parts(&h, permit.binding(), progress.clone(), h.rows.context());
            tx.operation_writes
                .push(OperationWrite::GitIntent(if starting {
                    GitIntentMutation::Advance {
                        binding: permit.binding(),
                        expected_phase: GitIntentPhase::Prepared,
                        next: GitIntentPhase::Started,
                        result: None,
                    }
                } else {
                    GitIntentMutation::Insert(Box::new(git_intent(
                        permit.binding(),
                        GitIntentPhase::Prepared,
                    )))
                }));
            assert_class(validate_transaction_projection(&h.ledger, &tx), expected);
            assert_class(
                h.ledger.prepare_external_progress(
                    &binding(1),
                    permit.binding().id,
                    permit.binding().digest,
                    progress,
                    h.rows.context(),
                ),
                expected,
            );
            assert_eq!(h.ledger.retained(), before);
            // Unstarted work can still settle as cancellation using its reservation.
            let context = h.rows.reduction(permit.binding(), false);
            let p = h
                .ledger
                .prepare_completion(
                    &binding(1),
                    permit.binding().id,
                    permit.binding().digest,
                    compact(&Outcome::Failed(OperationFailure::TargetDeleted)),
                    context.resulting_organization(),
                    context,
                )
                .unwrap();
            let mut tx = parts(p.projection());
            if starting {
                tx.operation_writes
                    .push(OperationWrite::GitIntent(GitIntentMutation::Complete {
                        binding: permit.binding(),
                    }));
            }
            validate_transaction_projection(&h.ledger, &tx).unwrap();
            h.commit(p);
        }
    }
}
#[test]
fn quiescence_rejects_first_retirement_acceptance() {
    for revoked in [false, true] {
        let mut h = Harness::new();
        h.register(binding(1));
        let permit = h.admit(binding(1), 1, &retirement_command(binding(1)));
        let expected = quiesce(&mut h, revoked);
        let context = h.rows.reduction(permit.binding(), true);
        let progress = retirement_progress(
            permit.binding(),
            context.resulting_organization(),
            RetirementCleanupState::AwaitingEvidence,
        );
        let tx = external_progress_parts(&h, permit.binding(), progress.clone(), context);
        assert_class(validate_transaction_projection(&h.ledger, &tx), expected);
        assert_class(
            h.ledger.prepare_external_progress(
                &binding(1),
                permit.binding().id,
                permit.binding().digest,
                progress,
                context,
            ),
            expected,
        );
    }
}
#[test]
fn quiescence_allows_first_git_retirement_of_already_started_work() {
    for revoked in [false, true] {
        let mut h = Harness::new();
        h.register(binding(1));
        let scope = RepositoryScope {
            server: server(),
            repository: RepositoryId::new(server(), 1).unwrap(),
        };
        let worktree = WorktreeId::new(server(), 1).unwrap();
        let permit = h.admit(
            binding(1),
            1,
            &Command::RemoveWorktreeConfirmed {
                scope,
                worktree,
                assessment: AssessmentRef {
                    operation: op(binding(1), 99),
                    digest: RequestDigest::from_bytes([9; 32]),
                },
                force: false,
            },
        );
        let mut reference = None;
        for phase in [ExternalIntentPhase::Prepared, ExternalIntentPhase::Started] {
            let progress = git_progress(
                permit.binding(),
                phase,
                h.rows.store.checked_successor().unwrap(),
            );
            if let ExternalProgressState::RecordedIntent { reference: v, .. } = progress.state() {
                reference = Some(*v);
            }
            let p = h
                .ledger
                .prepare_external_progress(
                    &binding(1),
                    permit.binding().id,
                    permit.binding().digest,
                    progress,
                    h.rows.context(),
                )
                .unwrap();
            h.commit(p);
        }
        quiesce(&mut h, revoked);
        let context = h.rows.reduction(permit.binding(), true);
        let retirement = retirement_progress(
            permit.binding(),
            context.resulting_organization(),
            RetirementCleanupState::AwaitingEvidence,
        );
        let ExternalProgressState::AcceptedRetirement(retirement) = retirement.state() else {
            panic!("retirement")
        };
        let mut retained = retirement.parts().clone();
        retained.target = AssessmentTarget::RemoveWorktree { scope, worktree };
        retained.completion = RetirementCompletion::ContinueGitSettlement;
        let pane = retained.panes[0];
        retained.panes[0] = RetirementPane::new(
            pane.workspace(),
            pane.tab(),
            pane.pane(),
            Some(worktree),
            pane.invalidation_generation(),
        )
        .unwrap();
        let session = retained.sessions[0];
        retained.sessions[0] = RetiredSession::new(
            session.session(),
            RetirementReason::WorktreeRemoval,
            session.cleanup(),
        )
        .unwrap();
        let progress = PendingExternalProgress::new(PendingExternalProgressParts {
            binding: permit.binding(),
            effect: ExternalEffectKind::GitWrite,
            state: ExternalProgressState::RecordedIntent {
                reference: reference.unwrap(),
                retirement: Some(Box::new(RetirementDescriptor::new(retained).unwrap())),
            },
        })
        .unwrap();
        let tx = external_progress_parts(&h, permit.binding(), progress.clone(), context);
        validate_transaction_projection(&h.ledger, &tx).unwrap();
        let prepared = h
            .ledger
            .prepare_external_progress(
                &binding(1),
                permit.binding().id,
                permit.binding().digest,
                progress,
                context,
            )
            .unwrap();
        h.commit(prepared);
        assert_eq!(
            h.ledger
                .lookup(
                    &binding(1),
                    permit.binding().id,
                    Some(permit.binding().digest)
                )
                .unwrap(),
            OperationLookup::Pending
        );
    }
}
#[test]
fn quiescence_allows_accepted_retirement_cleanup_and_completion() {
    for revoked in [false, true] {
        let mut h = Harness::new();
        h.register(binding(1));
        let permit = h.admit(binding(1), 1, &retirement_command(binding(1)));
        let context = h.rows.reduction(permit.binding(), true);
        let revision = context.resulting_organization();
        let p = h
            .ledger
            .prepare_external_progress(
                &binding(1),
                permit.binding().id,
                permit.binding().digest,
                retirement_progress(
                    permit.binding(),
                    revision,
                    RetirementCleanupState::AwaitingEvidence,
                ),
                context,
            )
            .unwrap();
        h.commit(p);
        quiesce(&mut h, revoked);
        let p = h
            .ledger
            .prepare_external_progress(
                &binding(1),
                permit.binding().id,
                permit.binding().digest,
                retirement_progress(
                    permit.binding(),
                    revision,
                    RetirementCleanupState::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
                ),
                h.rows.context(),
            )
            .unwrap();
        validate_transaction_projection(&h.ledger, &parts(p.projection())).unwrap();
        h.commit(p);
        let p = h
            .ledger
            .prepare_completion(
                &binding(1),
                permit.binding().id,
                permit.binding().digest,
                compact(&Outcome::Applied),
                revision,
                h.rows.context(),
            )
            .unwrap();
        validate_transaction_projection(&h.ledger, &parts(p.projection())).unwrap();
        h.commit(p);
        assert_eq!(
            h.ledger
                .lookup(&binding(1), permit.binding().id, None)
                .unwrap(),
            OperationLookup::Completed {
                result: Outcome::Applied,
                publication_revision: revision,
            }
        );
    }
}
#[test]
fn external_progress_rejects_skipped_or_regressed_phase_and_rewritten_retirement() {
    let mut h = Harness::new();
    h.register(binding(1));
    let command = Command::CreateWorktree {
        scope: RepositoryScope {
            server: server(),
            repository: RepositoryId::new(server(), 1).unwrap(),
        },
        branch: BranchName::new("new".into()).unwrap(),
    };
    let permit = h.admit(binding(1), 1, &command);
    let progress = git_progress(
        permit.binding(),
        ExternalIntentPhase::Started,
        h.rows.store.checked_successor().unwrap(),
    );
    assert_class(
        h.ledger.prepare_external_progress(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            progress,
            h.rows.context(),
        ),
        LedgerFailureClass::InvalidExternalProgress,
    );
}
#[test]
fn projection_rejects_conflicting_writes_and_preadmission_allocation() {
    let mut h = Harness::new();
    h.register(binding(1));
    let AdmissionPreparation::Prepared(p) = h.prepare(binding(1), 1, &create()).unwrap() else {
        panic!("proposal")
    };
    let mut tx = parts(p.projection());
    validate_transaction_projection(&h.ledger, &tx).unwrap();
    tx.resulting_high_water.workspace += 1;
    assert!(validate_transaction_projection(&h.ledger, &tx).is_err());
    tx = parts(p.projection());
    tx.operation_writes.push(tx.operation_writes[0].clone());
    assert!(validate_transaction_projection(&h.ledger, &tx).is_err());
}
#[test]
fn compound_completion_reserves_dedicated_capacity_atomically() {
    let mut h = Harness::new();
    h.register(binding(1));
    let permit = h.admit(binding(1), 1, &create());
    let context = h.rows.reduction(permit.binding(), false);
    let mut p = h
        .ledger
        .prepare_completion(
            &binding(1),
            permit.binding().id,
            permit.binding().digest,
            compact(&Outcome::Applied),
            context.resulting_organization(),
            context,
        )
        .unwrap();
    let stream = StreamRecord::new(
        binding(2),
        StreamPurpose::SelfRevocation,
        StreamStatus::Active,
        0,
        0,
        Some(OutcomeReservation::full(
            ReservationClass::DedicatedSelfRevocation,
        )),
    )
    .unwrap();
    h.ledger
        .extend_prepared(&mut p, vec![LedgerWrite::RegisterStream(stream)])
        .unwrap();
    h.commit(p);
    assert!(
        h.rows.streams[&binding(2).stream().key()]
            .dormant_reservation
            .is_some()
    );
}

#[test]
fn started_git_cannot_complete_before_durable_external_finish() {
    let mut h = Harness::new();
    h.register(binding(1));
    let command = Command::CreateWorktree {
        scope: RepositoryScope {
            server: server(),
            repository: RepositoryId::new(server(), 1).unwrap(),
        },
        branch: BranchName::new("new".into()).unwrap(),
    };
    let permit = h.admit(binding(1), 1, &command);
    for phase in [ExternalIntentPhase::Prepared, ExternalIntentPhase::Started] {
        let progress = git_progress(
            permit.binding(),
            phase,
            h.rows.store.checked_successor().unwrap(),
        );
        let p = h
            .ledger
            .prepare_external_progress(
                &binding(1),
                op(binding(1), 1),
                permit.binding().digest,
                progress,
                h.rows.context(),
            )
            .unwrap();
        h.commit(p);
    }
    let context = h.rows.reduction(permit.binding(), false);
    assert_class(
        h.ledger.prepare_completion(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            compact(&Outcome::Failed(OperationFailure::Runtime(
                FailureClass::RuntimeUnavailable,
            ))),
            context.resulting_organization(),
            context,
        ),
        LedgerFailureClass::InvalidExternalProgress,
    );
}

#[test]
fn failed_recovery_blocks_late_durable_receipt_and_cannot_be_cleared_by_sync() {
    let mut h = Harness::new();
    let p = h
        .ledger
        .prepare_register_stream(binding(1), h.rows.context())
        .unwrap();
    let mut bad = h.rows.recovery();
    bad.format_version += 1;
    assert!(h.ledger.install_recovery(bad).is_err());
    assert_class(
        h.ledger.resolve_commit(
            p,
            CommitDisposition::Durable(CommitReceipt {
                store_revision: StoreRevision::new(1),
                organization_revision: OrganizationRevision::new(0),
            }),
        ),
        LedgerFailureClass::RecoveryRequired,
    );
    assert_class(
        h.ledger
            .prepare_register_stream(binding(1), h.rows.context()),
        LedgerFailureClass::RecoveryRequired,
    );
}
#[test]
fn external_phase_projection_requires_matching_git_prerequisite() {
    let mut h = Harness::new();
    h.register(binding(1));
    let command = Command::CreateWorktree {
        scope: RepositoryScope {
            server: server(),
            repository: RepositoryId::new(server(), 1).unwrap(),
        },
        branch: BranchName::new("new".into()).unwrap(),
    };
    let permit = h.admit(binding(1), 1, &command);
    let progress = git_progress(
        permit.binding(),
        ExternalIntentPhase::Prepared,
        h.rows.store.checked_successor().unwrap(),
    );
    let p = h
        .ledger
        .prepare_external_progress(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            progress,
            h.rows.context(),
        )
        .unwrap();
    let mut transaction = parts(p.projection());
    assert!(validate_transaction_projection(&h.ledger, &transaction).is_err());
    transaction.operation_writes.push(OperationWrite::GitIntent(
        spaceterm_core::worktrees::intent::GitIntentMutation::Insert(Box::new(git_intent(
            permit.binding(),
            spaceterm_core::worktrees::intent::GitIntentPhase::Prepared,
        ))),
    ));
    validate_transaction_projection(&h.ledger, &transaction).unwrap();
}

#[test]
fn retirement_progress_cannot_rebind_batch_or_rewrite_final_cleanup() {
    let mut h = Harness::new();
    h.register(binding(1));
    let permit = h.admit(binding(1), 1, &retirement_command(binding(1)));
    let context = h.rows.reduction(permit.binding(), true);
    let original = retirement_progress(
        permit.binding(),
        context.resulting_organization(),
        RetirementCleanupState::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
    );
    let p = h
        .ledger
        .prepare_external_progress(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            original.clone(),
            context,
        )
        .unwrap();
    h.commit(p);
    let ExternalProgressState::AcceptedRetirement(descriptor) = original.state() else {
        panic!("accepted retirement")
    };
    let mut parts = descriptor.parts().clone();
    parts.batch = RetirementBatchId::new(server(), ServerEpoch::new(1), 999).unwrap();
    let changed = PendingExternalProgress::new(PendingExternalProgressParts {
        binding: permit.binding(),
        effect: ExternalEffectKind::SessionLifecycle,
        state: ExternalProgressState::AcceptedRetirement(Box::new(
            RetirementDescriptor::new(parts).unwrap(),
        )),
    })
    .unwrap();
    assert_class(
        h.ledger.prepare_external_progress(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            changed,
            h.rows.context(),
        ),
        LedgerFailureClass::InvalidExternalProgress,
    );
    let regressed = retirement_progress(
        permit.binding(),
        OrganizationRevision::new(1),
        RetirementCleanupState::AwaitingEvidence,
    );
    assert_class(
        h.ledger.prepare_external_progress(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            regressed,
            h.rows.context(),
        ),
        LedgerFailureClass::InvalidExternalProgress,
    );
}

#[test]
fn finished_git_can_settle_cleanup_failure_without_claiming_removal_success() {
    use spaceterm_core::worktrees::types::*;
    let mut h = Harness::new();
    h.register(binding(1));
    let scope = RepositoryScope {
        server: server(),
        repository: RepositoryId::new(server(), 1).unwrap(),
    };
    let worktree = WorktreeId::new(server(), 1).unwrap();
    let command = Command::RemoveWorktreeConfirmed {
        scope,
        worktree,
        assessment: AssessmentRef {
            operation: op(binding(1), 99),
            digest: RequestDigest::from_bytes([9; 32]),
        },
        force: false,
    };
    let permit = h.admit(binding(1), 1, &command);
    let progress = git_progress(
        permit.binding(),
        ExternalIntentPhase::Prepared,
        h.rows.store.checked_successor().unwrap(),
    );
    let p = h
        .ledger
        .prepare_external_progress(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            progress,
            h.rows.context(),
        )
        .unwrap();
    h.commit(p);
    let context = h.rows.reduction(permit.binding(), true);
    let old = retirement_progress(
        permit.binding(),
        context.resulting_organization(),
        RetirementCleanupState::Failed(FailureClass::RuntimeUnavailable),
    );
    let ExternalProgressState::AcceptedRetirement(old) = old.state() else {
        panic!("retirement")
    };
    let mut retained = old.parts().clone();
    retained.target = AssessmentTarget::RemoveWorktree { scope, worktree };
    retained.completion = RetirementCompletion::ContinueGitSettlement;
    let pane = retained.panes[0];
    retained.panes[0] = RetirementPane::new(
        pane.workspace(),
        pane.tab(),
        pane.pane(),
        Some(worktree),
        pane.invalidation_generation(),
    )
    .unwrap();
    let session = retained.sessions[0];
    retained.sessions[0] = RetiredSession::new(
        session.session(),
        RetirementReason::WorktreeRemoval,
        session.cleanup(),
    )
    .unwrap();
    let retirement = Box::new(RetirementDescriptor::new(retained).unwrap());
    for phase in [
        ExternalIntentPhase::Started,
        ExternalIntentPhase::ExternalFinished,
    ] {
        let context = if phase == ExternalIntentPhase::Started {
            context
        } else {
            h.rows.context()
        };
        let basic = git_progress(
            permit.binding(),
            phase,
            h.rows.store.checked_successor().unwrap(),
        );
        let ExternalProgressState::RecordedIntent { reference, .. } = basic.state() else {
            panic!("intent")
        };
        let progress = PendingExternalProgress::new(PendingExternalProgressParts {
            binding: permit.binding(),
            effect: ExternalEffectKind::GitWrite,
            state: ExternalProgressState::RecordedIntent {
                reference: *reference,
                retirement: Some(retirement.clone()),
            },
        })
        .unwrap();
        let p = h
            .ledger
            .prepare_external_progress(
                &binding(1),
                op(binding(1), 1),
                permit.binding().digest,
                progress,
                context,
            )
            .unwrap();
        h.commit(p);
    }
    let context = h.rows.reduction(permit.binding(), false);
    let success = Outcome::Git(
        GitMutationOutcome::new(GitMutationOutcomeParts::Removed {
            worktree,
            closed_tabs: 1,
            ended_sessions: 1,
            revision: context.resulting_organization(),
        })
        .unwrap(),
    );
    assert_class(
        h.ledger.prepare_completion(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            compact(&success),
            context.resulting_organization(),
            context,
        ),
        LedgerFailureClass::InvalidExternalProgress,
    );
    let outcome = Outcome::Git(
        GitMutationOutcome::new(GitMutationOutcomeParts::Failed {
            server: server(),
            failure: GitFailure::Unchecked,
            ended_sessions: 1,
            external: ExternalDisposition::NoEffectEstablished,
        })
        .unwrap(),
    );
    let p = h
        .ledger
        .prepare_completion(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            compact(&outcome),
            context.resulting_organization(),
            context,
        )
        .unwrap();
    h.commit(p);
}
