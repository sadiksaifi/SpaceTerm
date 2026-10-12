#[path = "support/operation_ledger.rs"]
mod support;
use spaceterm_core::authority::*;
use spaceterm_core::identity::*;
use spaceterm_core::operation_ledger::model::*;
use spaceterm_core::operation_ledger::recovery::{validate_recovered_state, validate_recovery};
use spaceterm_core::operation_ledger::*;
use spaceterm_core::operations::*;
use spaceterm_core::organization::*;
use spaceterm_core::retirement::*;
use spaceterm_core::storage::*;
use spaceterm_core::worktrees::intent::*;
use support::*;

#[test]
fn recover_completed_outcome_and_acknowledged_prefix_after_commit_before_reply() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &create());
    h.complete(binding(1), 1, &Outcome::Applied, true);
    h.reopen();
    assert_eq!(
        h.ledger
            .lookup(&binding(1), op(binding(1), 1), None)
            .unwrap(),
        OperationLookup::Completed {
            result: Outcome::Applied,
            publication_revision: OrganizationRevision::new(1)
        }
    );
    h.ack(binding(1), 1);
    h.reopen();
    assert_eq!(
        h.ledger
            .lookup(&binding(1), op(binding(1), 1), None)
            .unwrap(),
        OperationLookup::AcknowledgedRetired
    );
}
#[test]
fn recovered_atomic_waits_for_original_matching_request_and_issues_one_permit() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &create());
    h.reopen();
    assert!(matches!(
        h.ledger.recovery_tasks().next(),
        Some(RecoveryTask::AwaitOriginalAtomicRequest(_))
    ));
    assert_class(
        h.ledger
            .resume_recovered_atomic(&binding(1), op(binding(1), 1), &rename("wrong")),
        LedgerFailureClass::IdentityReused,
    );
    h.ledger
        .resume_recovered_atomic(&binding(1), op(binding(1), 1), &create())
        .unwrap();
    assert_class(
        h.ledger
            .resume_recovered_atomic(&binding(1), op(binding(1), 1), &create()),
        LedgerFailureClass::InvalidTransition,
    );
}
#[test]
fn recovered_external_operation_requires_reconciliation_not_execution() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &retirement_command(binding(1)));
    h.reopen();
    assert!(
        matches!(h.ledger.recovery_tasks().next(), Some(RecoveryTask::ReconcileExternal(v)) if matches!(v.external_progress.as_ref().unwrap().state(), ExternalProgressState::AwaitingIntent))
    );
    assert_class(
        h.ledger.resume_recovered_atomic(
            &binding(1),
            op(binding(1), 1),
            &retirement_command(binding(1)),
        ),
        LedgerFailureClass::InvalidTransition,
    );
    assert!(matches!(
        h.prepare(binding(1), 1, &retirement_command(binding(1)))
            .unwrap(),
        AdmissionPreparation::Existing(OperationLookup::Pending)
    ));
}
#[test]
fn recovery_rejects_holes_duplicates_wrong_binding_and_invalid_outcomes() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &create());
    h.admit(binding(1), 2, &create());
    h.complete(binding(1), 2, &Outcome::Applied, true);
    let recovery = h.rows.recovery();
    let mut invalid = recovery.clone();
    invalid.operations.remove(0);
    assert!(validate_recovery(server(), &invalid).is_err());
    invalid = recovery.clone();
    invalid.operations.push(invalid.operations[0].clone());
    assert!(validate_recovery(server(), &invalid).is_err());
    invalid = recovery.clone();
    if let RecoveredOperationRecord::Pending(v) = &mut invalid.operations[0] {
        v.binding.id = op(binding_for(1, 2, 2), 1);
    }
    assert!(validate_recovery(server(), &invalid).is_err());
    invalid = recovery.clone();
    if let RecoveredOperationRecord::Completed(v) = &mut invalid.operations[1] {
        v.outcome = CompactOutcome::from_encoded(vec![1]).unwrap();
    }
    assert!(validate_recovery(server(), &invalid).is_err());
    invalid = recovery;
    invalid.streams[0].acknowledged_through = 1;
    assert!(validate_recovery(server(), &invalid).is_err());
}
#[test]
fn recovery_rejects_quota_and_header_bounds_plus_one() {
    let mut h = Harness::new();
    h.register(binding(1));
    for n in 1..=3 {
        h.admit(binding(1), n, &create());
    }
    let mut recovery = h.rows.recovery();
    recovery.limits = LedgerLimits::new(12, 4, 3, 12, 1, 2).unwrap();
    assert_class(
        validate_recovery(server(), &recovery),
        LedgerFailureClass::QuotaExceeded,
    );
    let mut recovery = LedgerRecovery::empty(LedgerLimits::new(1, 1, 2, 2, 1, 1).unwrap());
    for n in 1..=2 {
        recovery.streams.push(
            StreamRecord::new(
                binding(n),
                StreamPurpose::Ordinary,
                StreamStatus::Retired(StreamRetirementReason::Expired),
                0,
                0,
                None,
            )
            .unwrap(),
        );
    }
    assert_class(
        validate_recovery(server(), &recovery),
        LedgerFailureClass::QuotaExceeded,
    );
}
#[test]
fn recovery_validation_is_bounded_for_large_high_water() {
    let mut recovery = LedgerRecovery::empty(limits());
    recovery.streams.push(
        StreamRecord::new(
            binding(1),
            StreamPurpose::Ordinary,
            StreamStatus::Active,
            u64::MAX,
            u64::MAX - 1,
            None,
        )
        .unwrap(),
    );
    assert_class(
        validate_recovery(server(), &recovery),
        LedgerFailureClass::InvalidDurableState,
    );
    recovery.streams[0].acknowledged_through = 0;
    assert_class(
        validate_recovery(server(), &recovery),
        LedgerFailureClass::QuotaExceeded,
    );
    recovery.streams[0].acknowledged_through = u64::MAX;
    validate_recovery(server(), &recovery).unwrap();
}
#[test]
fn failed_recovery_leaves_admission_closed() {
    let mut h = Harness::new();
    h.register(binding(1));
    let mut bad = h.rows.recovery();
    bad.format_version += 1;
    assert!(h.ledger.install_recovery(bad).is_err());
    assert_class(
        h.prepare(binding(1), 1, &create()),
        LedgerFailureClass::RecoveryRequired,
    );
    h.ledger.install_recovery(h.rows.recovery()).unwrap();
    h.admit(binding(1), 1, &create());
}
#[test]
fn uncertain_admission_absent_after_recovery_can_retry_original_identity() {
    let mut h = Harness::new();
    h.register(binding(1));
    let AdmissionPreparation::Prepared(p) = h.prepare(binding(1), 1, &create()).unwrap() else {
        panic!("proposal")
    };
    h.ledger
        .resolve_commit(p, CommitDisposition::Indeterminate)
        .unwrap();
    h.ledger.install_recovery(h.rows.recovery()).unwrap();
    h.admit(binding(1), 1, &create());
}
#[test]
fn uncertain_completion_recovers_pending_or_completed_without_duplicate_effect() {
    for applied in [false, true] {
        let mut h = Harness::new();
        h.register(binding(1));
        let permit = h.admit(binding(1), 1, &create());
        let context = h.rows.reduction(permit.binding(), true);
        let p = h
            .ledger
            .prepare_completion(
                &binding(1),
                op(binding(1), 1),
                permit.binding().digest,
                compact(&Outcome::Applied),
                context.resulting_organization(),
                context,
            )
            .unwrap();
        if applied {
            h.rows.apply(p.projection());
        }
        h.ledger
            .resolve_commit(p, CommitDisposition::Indeterminate)
            .unwrap();
        h.ledger.install_recovery(h.rows.recovery()).unwrap();
        assert!(matches!(
            h.prepare(binding(1), 1, &create()).unwrap(),
            AdmissionPreparation::Existing(_)
        ));
        assert!(
            h.ledger
                .resume_recovered_atomic(&binding(1), op(binding(1), 1), &create())
                .is_err()
        );
        if applied {
            assert!(matches!(
                h.ledger
                    .lookup(&binding(1), op(binding(1), 1), None)
                    .unwrap(),
                OperationLookup::Completed { .. }
            ));
        } else {
            h.complete(binding(1), 1, &Outcome::Applied, true);
        }
    }
}
#[test]
fn uncertain_completion_rejects_invented_acknowledgement_prefix() {
    for external in [false, true] {
        for applied in [false, true] {
            let mut h = Harness::new();
            h.register(binding(1));
            let command = if external {
                retirement_command(binding(1))
            } else {
                create()
            };
            let permit = h.admit(binding(1), 1, &command);
            if external {
                let context = h.rows.reduction(permit.binding(), true);
                let progress = retirement_progress(
                    permit.binding(),
                    context.resulting_organization(),
                    RetirementCleanupState::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
                );
                let p = h
                    .ledger
                    .prepare_external_progress(
                        &binding(1),
                        permit.binding().id,
                        permit.binding().digest,
                        progress,
                        context,
                    )
                    .unwrap();
                h.commit(p);
            }
            let before = h.ledger.retained();
            let context = if external {
                h.rows.context()
            } else {
                h.rows.reduction(permit.binding(), true)
            };
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
            if applied {
                h.rows.apply(p.projection());
            }
            h.ledger
                .resolve_commit(p, CommitDisposition::Indeterminate)
                .unwrap();
            let mut bad = h.rows.recovery();
            bad.streams[0].acknowledged_through = 1;
            bad.operations.clear();
            // A standalone inventory cannot prove that the client acknowledged.
            validate_recovery(server(), &bad).unwrap();
            assert_class(
                h.ledger.install_recovery(bad),
                LedgerFailureClass::InvalidDurableState,
            );
            assert!(h.ledger.requires_recovery());
            assert_eq!(h.ledger.retained(), before);
            assert_class(
                h.prepare(binding(1), 2, &command),
                LedgerFailureClass::RecoveryRequired,
            );
            h.ledger.install_recovery(h.rows.recovery()).unwrap();
            assert_eq!(h.ledger.retained(), h.rows.recovery());
        }
    }
}
#[test]
fn unrelated_uncertain_transaction_cannot_compact_unacknowledged_records() {
    for completed in [false, true] {
        let mut h = Harness::new();
        h.register(binding(1));
        h.admit(binding(1), 1, &create());
        if completed {
            h.complete(binding(1), 1, &Outcome::Applied, true);
        }
        let before = h.ledger.retained();
        let p = h
            .ledger
            .prepare_register_stream(binding(2), h.rows.context())
            .unwrap();
        h.ledger
            .resolve_commit(p, CommitDisposition::Indeterminate)
            .unwrap();
        let mut bad = h.rows.recovery();
        bad.streams[0].acknowledged_through = 1;
        bad.operations.clear();
        assert_class(
            h.ledger.install_recovery(bad),
            LedgerFailureClass::InvalidDurableState,
        );
        assert!(h.ledger.requires_recovery());
        assert_eq!(h.ledger.retained(), before);
        h.ledger.install_recovery(h.rows.recovery()).unwrap();
        assert_eq!(h.ledger.retained(), before);
    }
}
#[test]
fn dropped_acknowledgement_recovers_applied_projection_and_restores_writes() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &create());
    h.complete(binding(1), 1, &Outcome::Applied, true);
    let p = h
        .ledger
        .prepare_acknowledgement(&binding(1), 1, h.rows.context())
        .unwrap()
        .unwrap();
    h.rows.apply(p.projection());
    drop(p);
    assert_class(
        h.prepare(binding(1), 2, &create()),
        LedgerFailureClass::PreparationBusy,
    );
    h.ledger.install_recovery(h.rows.recovery()).unwrap();
    assert!(!h.ledger.requires_recovery());
    assert_eq!(
        h.ledger.stream(&binding(1)).unwrap().acknowledged_through,
        1
    );
    assert_eq!(
        h.ledger
            .lookup(&binding(1), op(binding(1), 1), None)
            .unwrap(),
        OperationLookup::AcknowledgedRetired
    );
    h.admit(binding(1), 2, &create());
}
#[test]
fn outstanding_acknowledgement_accepts_only_its_exact_stream_and_prefix() {
    for dropped in [false, true] {
        for applied in [false, true] {
            let mut h = Harness::new();
            for b in [binding(1), binding(2)] {
                h.register(b);
                for n in 1..=3 {
                    h.admit(b, n, &create());
                    h.complete(b, n, &Outcome::Applied, true);
                }
            }
            let p = h
                .ledger
                .prepare_acknowledgement(&binding(1), 2, h.rows.context())
                .unwrap()
                .unwrap();
            if applied {
                h.rows.apply(p.projection());
            }
            if dropped {
                drop(p);
            } else {
                h.ledger
                    .resolve_commit(p, CommitDisposition::Indeterminate)
                    .unwrap();
            }
            for (b, through) in [(binding(1), 1), (binding(1), 3), (binding(2), 2)] {
                let mut bad = h.rows.recovery();
                bad.streams
                    .iter_mut()
                    .find(|v| v.binding == b)
                    .unwrap()
                    .acknowledged_through = through;
                bad.operations.retain(|v| {
                    let id = match v {
                        RecoveredOperationRecord::Pending(v) => v.binding.id,
                        RecoveredOperationRecord::Completed(v) => v.binding.id,
                    };
                    id.stream() != b.stream() || id.sequence().get() > through
                });
                assert_class(
                    h.ledger.install_recovery(bad),
                    LedgerFailureClass::InvalidDurableState,
                );
                assert!(h.ledger.requires_recovery());
            }
            h.ledger.install_recovery(h.rows.recovery()).unwrap();
            assert!(!h.ledger.requires_recovery());
            assert_eq!(h.ledger.retained(), h.rows.recovery());
            h.ack(binding(1), 2);
        }
    }
}
#[test]
fn not_applied_acknowledgement_cannot_authorize_recovery_compaction() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &create());
    h.complete(binding(1), 1, &Outcome::Applied, true);
    let before = h.ledger.retained();
    let p = h
        .ledger
        .prepare_acknowledgement(&binding(1), 1, h.rows.context())
        .unwrap()
        .unwrap();
    h.ledger
        .resolve_commit(p, CommitDisposition::NotApplied(StoreFailure::Io))
        .unwrap();
    let mut bad = h.rows.recovery();
    bad.streams[0].acknowledged_through = 1;
    bad.operations.clear();
    assert_class(
        h.ledger.install_recovery(bad),
        LedgerFailureClass::InvalidDurableState,
    );
    assert!(h.ledger.requires_recovery());
    assert_eq!(h.ledger.retained(), before);
    h.ledger.install_recovery(h.rows.recovery()).unwrap();
    h.ack(binding(1), 1);
}
#[test]
fn retired_headers_and_original_pairing_comparison_survive_recovery() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &create());
    h.retire(binding(1), StreamRetirementReason::PairingRevoked);
    h.reopen();
    assert_class(
        h.ledger
            .resume_recovered_atomic(&binding(1), op(binding(1), 1), &create()),
        LedgerFailureClass::Retired,
    );
    assert_class(
        h.ledger
            .lookup(&binding_for(1, 2, 2), op(binding_for(1, 2, 2), 1), None),
        LedgerFailureClass::ScopeMismatch,
    );
}
#[test]
fn install_recovery_rejects_regressed_external_progress() {
    let mut h = Harness::new();
    h.register(binding(1));
    let permit = h.admit(binding(1), 1, &retirement_command(binding(1)));
    let old = h.rows.recovery();
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
    h.commit(p);
    assert_class(
        h.ledger.install_recovery(old),
        LedgerFailureClass::InvalidDurableState,
    );
}
#[test]
fn error_and_debug_output_omit_sensitive_request_and_outcome_values() {
    let mut h = Harness::new();
    h.register(binding(1));
    let secret = "sensitive-path-credential-sentinel";
    let request = rename(secret);
    let permit = h.admit(binding(1), 1, &request);
    let context = h.rows.reduction(permit.binding(), false);
    let outcome = compact(&Outcome::Conflict(FieldConflict::WorktreeLocation {
        server: server(),
        current_value: WorktreeLocationTemplate::new(secret.into()).unwrap(),
        current_version: FieldVersion::new(1),
    }));
    let p = h
        .ledger
        .prepare_completion(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            outcome.clone(),
            context.resulting_organization(),
            context,
        )
        .unwrap();
    let error = h.prepare(binding(1), 2, &request).unwrap_err();
    let formatted = format!(
        "{error} {error:?} {p:?} {:?} {:?} {:?}",
        h.ledger,
        outcome,
        h.rows.recovery()
    );
    assert!(!formatted.contains(secret));
    assert!(!formatted.contains(&format!("{:?}", permit.binding().digest.bytes())));
}

fn bundle(h: &Harness) -> RecoveredState {
    let tx = parts(
        &LedgerProjection::new(
            h.rows.context(),
            vec![LedgerWrite::RetireStream {
                binding: binding(1),
                reason: StreamRetirementReason::Expired,
            }],
        )
        .unwrap(),
    );
    RecoveredState {
        organization: OrganizationState {
            server: server(),
            revision: h.rows.organization,
            settings: ServerSettingsRecord {
                worktree_location: Versioned {
                    value: WorktreeLocationTemplate::new("../{branch}".into()).unwrap(),
                    version: FieldVersion::new(1),
                },
            },
            workspaces: vec![],
            tabs: vec![],
            panes: vec![],
            worktrees: vec![],
            repositories: vec![],
            retained: None,
        },
        store_revision: h.rows.store,
        last_epoch: ServerEpoch::new(1),
        high_water: tx.resulting_high_water,
        streams: vec![],
        operations: vec![],
        ledger: Some(h.rows.recovery()),
        pairing: None,
        git_repositories: vec![],
        git_intents: vec![],
        git_allocations: vec![],
        native_retirements: vec![],
    }
}
#[test]
fn borrowed_bundle_validator_checks_git_phase_and_original_intent_binding() {
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
    h.commit(p);
    let mut recovered = bundle(&h);
    assert!(validate_recovered_state(&recovered).is_err());
    recovered
        .git_intents
        .push(git_intent(permit.binding(), GitIntentPhase::Prepared));
    validate_recovered_state(&recovered).unwrap();
    recovered.git_intents[0] = git_intent(permit.binding(), GitIntentPhase::Started);
    assert!(validate_recovered_state(&recovered).is_err());
}
#[test]
fn adversarial_schedules_preserve_prefixes_results_and_single_admission_ownership() {
    use std::collections::BTreeMap;
    for seed in 1..=24u64 {
        let mut random = seed;
        let mut h = Harness::new();
        h.register(binding(1));
        h.register(binding(2));
        let mut permits = BTreeMap::<OperationId, u32>::new();
        let mut history = BTreeMap::<OperationId, OrganizationRevision>::new();
        let mut prefixes = BTreeMap::<u64, (u64, u64)>::new();
        for step in 0..220 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let b = binding(1 + ((random >> 24) & 1));
            let action = (random >> 32) % 7;
            match action {
                0 | 1 => {
                    let next = h.rows.streams[&b.stream().key()].admitted_through + 1;
                    if let Ok(AdmissionPreparation::Prepared(p)) = h.prepare(b, next, &create()) {
                        match (random >> 40) % 4 {
                            0 => {
                                h.ledger
                                    .resolve_commit(
                                        p,
                                        CommitDisposition::NotApplied(StoreFailure::Io),
                                    )
                                    .unwrap();
                            }
                            1 => {
                                if random & 4 != 0 {
                                    h.rows.apply(p.projection());
                                }
                                h.ledger
                                    .resolve_commit(p, CommitDisposition::Indeterminate)
                                    .unwrap();
                                h.ledger.install_recovery(h.rows.recovery()).unwrap();
                            }
                            _ => {
                                let TransitionResolution::Admitted(permit) = h.commit(p) else {
                                    panic!("permit")
                                };
                                *permits.entry(permit.binding().id).or_default() += 1;
                            }
                        }
                    }
                }
                2 => {
                    let pending = h.rows.entries.iter().find_map(|(id, v)| {
                        if id.stream() == b.stream()
                            && matches!(v, RecoveredOperationRecord::Pending(_))
                        {
                            Some(*id)
                        } else {
                            None
                        }
                    });
                    if let Some(id) = pending {
                        h.complete(b, id.sequence().get(), &Outcome::Applied, true);
                        history.insert(id, h.rows.organization);
                    }
                }
                3 => {
                    let header = &h.rows.streams[&b.stream().key()];
                    let mut through = header.acknowledged_through;
                    for n in through + 1..=header.admitted_through {
                        if matches!(
                            h.rows.entries.get(&op(b, n)),
                            Some(RecoveredOperationRecord::Completed(_))
                        ) {
                            through = n;
                        } else {
                            break;
                        }
                    }
                    h.ack(b, through);
                }
                4 => h.reopen(),
                5 if step > 180 => h.retire(b, StreamRetirementReason::Expired),
                _ => {
                    let header = &h.rows.streams[&b.stream().key()];
                    if header.admitted_through > 0 {
                        assert!(!matches!(
                            h.prepare(b, header.admitted_through, &create()).unwrap(),
                            AdmissionPreparation::Prepared(_)
                        ));
                    }
                }
            }
            validate_recovery(server(), &h.rows.recovery()).unwrap();
            assert!(permits.values().all(|count| *count <= 1));
            for k in 1..=2 {
                let header = &h.rows.streams[&binding(k).stream().key()];
                let previous = prefixes.entry(k).or_default();
                assert!(
                    header.admitted_through >= previous.0
                        && header.acknowledged_through >= previous.1
                );
                *previous = (header.admitted_through, header.acknowledged_through);
            }
            for (id, revision) in &history {
                if let OperationLookup::Completed {
                    result,
                    publication_revision,
                } = h
                    .ledger
                    .lookup(&StreamBinding::from_stream(id.stream()), *id, None)
                    .unwrap()
                {
                    assert_eq!(result, Outcome::Applied);
                    assert_eq!(&publication_revision, revision);
                }
            }
            assert_eq!(
                h.rows.entries.len(),
                h.rows
                    .streams
                    .values()
                    .map(|v| (v.admitted_through - v.acknowledged_through) as usize)
                    .sum::<usize>()
            );
        }
    }
}

#[test]
fn recovered_atomic_cannot_misclassify_an_external_request_as_execution_authority() {
    let mut h = Harness::new();
    h.register(binding(1));
    let command = retirement_command(binding(1));
    h.admit(binding(1), 1, &command);
    let mut recovery = h.rows.recovery();
    if let RecoveredOperationRecord::Pending(v) = &mut recovery.operations[0] {
        v.kind = PendingKind::AtomicMutation;
        v.external_progress = None;
    }
    let mut ledger = OperationLedger::recover(server(), recovery).unwrap();
    assert_class(
        ledger.resume_recovered_atomic(&binding(1), op(binding(1), 1), &command),
        LedgerFailureClass::InvalidTransition,
    );
}

#[test]
fn borrowed_bundle_validator_binds_enrollment_claim_and_original_request_digest() {
    use spaceterm_core::pairing::*;
    let mut h = Harness::new();
    let claim = RequestDigest::from_bytes([4; 32]);
    h.register_purpose(
        binding(1),
        StreamPurpose::Enrollment {
            claim_digest: claim,
        },
    );
    let ticket = EnrollmentTicketId::new(EnrollmentTicketParts {
        server: server(),
        nonce: [1; 16],
    })
    .unwrap();
    let command = Command::Pairing(PairingCommand::Enroll {
        ticket: ticket.clone(),
        claim_digest: claim,
    });
    let original = spaceterm_core::operation_ledger::canonical::CanonicalCodec
        .canonical_request_digest(&binding(1), op(binding(1), 1), &command)
        .unwrap();
    let reservation = |claim_digest, enrollment_request_digest| {
        EnrollmentReservation::new(EnrollmentReservationParts {
            ticket: ticket.clone(),
            target: PairingTarget::new(PairingTargetParts {
                server: server(),
                device: DeviceId::new([2; 16]),
                pairing: binding(1).pairing(),
            })
            .unwrap(),
            keys: PairingKeys::new(PairingKeysParts {
                device_identity_public_key: [3; 32],
                pairing_authentication_public_key: [4; 32],
            })
            .unwrap(),
            initial_client: binding(1).client(),
            enrollment_stream: binding(1).stream(),
            claim_digest,
            authorization_digest: RequestDigest::from_bytes([6; 32]),
            enrollment_request_digest,
            issuer: EnrollmentIssuer::AccountOwner {
                native_request: [7; 16],
                native_request_digest: RequestDigest::from_bytes([8; 32]),
            },
            issued_epoch: ServerEpoch::new(1),
            issued_revision: StoreRevision::new(1),
            issued_at_millis: 0,
            expires_at_millis: 1000,
            state: EnrollmentReservationState::Reserved,
        })
        .unwrap()
    };
    let pairing = |reservation| {
        PairingRecovery::new(PairingRecoveryParts {
            authorizations: vec![],
            enrollments: vec![reservation],
        })
        .unwrap()
    };
    let mut recovered = bundle(&h);
    recovered.pairing = Some(pairing(reservation(claim, original)));
    validate_recovered_state(&recovered).unwrap();
    recovered.pairing = Some(pairing(reservation(
        RequestDigest::from_bytes([9; 32]),
        original,
    )));
    assert_class(
        validate_recovered_state(&recovered),
        LedgerFailureClass::PurposeMismatch,
    );
    h.admit(binding(1), 1, &command);
    recovered = bundle(&h);
    recovered.pairing = Some(pairing(reservation(
        claim,
        RequestDigest::from_bytes([9; 32]),
    )));
    assert_class(
        validate_recovered_state(&recovered),
        LedgerFailureClass::IdentityReused,
    );
}
#[test]
fn borrowed_bundle_validator_rejects_future_and_unallocated_historical_results() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &create());
    h.complete(
        binding(1),
        1,
        &Outcome::CreatedPane {
            pane: PaneId::new(server(), 200).unwrap(),
        },
        true,
    );
    let mut recovered = bundle(&h);
    assert_class(
        validate_recovered_state(&recovered),
        LedgerFailureClass::InvalidDurableState,
    );
    recovered.high_water.pane = 200;
    validate_recovered_state(&recovered).unwrap();
    recovered.store_revision = StoreRevision::new(0);
    assert_class(
        validate_recovered_state(&recovered),
        LedgerFailureClass::InvalidDurableState,
    );
}

#[test]
fn uncertain_completion_cannot_recover_a_changed_original_outcome() {
    let mut h = Harness::new();
    h.register(binding(1));
    let permit = h.admit(binding(1), 1, &create());
    let context = h.rows.reduction(permit.binding(), true);
    let p = h
        .ledger
        .prepare_completion(
            &binding(1),
            op(binding(1), 1),
            permit.binding().digest,
            compact(&Outcome::Applied),
            context.resulting_organization(),
            context,
        )
        .unwrap();
    h.rows.apply(p.projection());
    h.ledger
        .resolve_commit(p, CommitDisposition::Indeterminate)
        .unwrap();
    let mut bad = h.rows.recovery();
    if let RecoveredOperationRecord::Completed(v) = &mut bad.operations[0] {
        v.outcome = compact(&Outcome::Failed(OperationFailure::TargetDeleted));
    }
    assert_class(
        h.ledger.install_recovery(bad),
        LedgerFailureClass::InvalidDurableState,
    );
}
