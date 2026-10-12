#![allow(dead_code)]
// Test-only store boundary: independent row interpreter, never a clone of the
// ledger's candidate state. Reopen reconstructs fresh policy from durable rows.
use std::collections::BTreeMap;

use spaceterm_core::authority::*;
use spaceterm_core::identity::*;
use spaceterm_core::operation_ledger::canonical::CanonicalCodec;
use spaceterm_core::operation_ledger::model::*;
use spaceterm_core::operation_ledger::*;
use spaceterm_core::operations::*;
use spaceterm_core::organization::*;
use spaceterm_core::storage::*;

pub fn server() -> ServerId {
    ServerId::new([1; 16])
}
pub fn binding(key: u64) -> StreamBinding {
    binding_for(key, 1, 1)
}
pub fn binding_for(key: u64, pairing: u64, client: u64) -> StreamBinding {
    StreamBinding::from_stream(
        OperationStreamId::new(
            OperationStreamKey::new(server(), key).unwrap(),
            PairingId::new(server(), pairing).unwrap(),
            ClientId::new(server(), client).unwrap(),
        )
        .unwrap(),
    )
}
pub fn op(binding: StreamBinding, sequence: u64) -> OperationId {
    OperationId::new(binding.stream(), OperationSequence::new(sequence).unwrap())
}
pub fn create() -> Command {
    Command::CreateWorkspace {
        server: server(),
        source: WorkspaceCreationSource::ServerHome { server: server() },
        name: NameDraft::Automatic,
    }
}
pub fn rename(value: &str) -> Command {
    Command::RenameWorkspace {
        workspace: WorkspaceId::new(server(), 1).unwrap(),
        expected_version: FieldVersion::new(1),
        draft: NameDraft::Custom(NameText::new(value.to_owned()).unwrap()),
    }
}
pub fn safety() -> Command {
    Command::RevokePairing {
        pairing: PairingId::new(server(), 1).unwrap(),
    }
}
pub fn compact(outcome: &Outcome) -> CompactOutcome {
    CompactOutcome::new(outcome, &CanonicalCodec).unwrap()
}
pub fn limits() -> LedgerLimits {
    LedgerLimits::new(12, 4, 4, 12, 1, 2).unwrap()
}

pub struct DurableRows {
    pub streams: BTreeMap<OperationStreamKey, StreamRecord>,
    pub entries: BTreeMap<OperationId, RecoveredOperationRecord>,
    pub store: StoreRevision,
    pub organization: OrganizationRevision,
    pub limits: LedgerLimits,
}
impl DurableRows {
    pub fn new(limits: LedgerLimits) -> Self {
        Self {
            streams: BTreeMap::new(),
            entries: BTreeMap::new(),
            store: StoreRevision::new(0),
            organization: OrganizationRevision::new(0),
            limits,
        }
    }
    pub fn recovery(&self) -> LedgerRecovery {
        LedgerRecovery {
            format_version: LEDGER_FORMAT_VERSION,
            quota_policy_version: LEDGER_QUOTA_POLICY_VERSION,
            limits: self.limits,
            streams: self.streams.values().cloned().collect(),
            operations: self.entries.values().cloned().collect(),
        }
    }
    pub fn context(&self) -> TransactionContext {
        TransactionContext::new(self.store, self.organization, self.organization, None).unwrap()
    }
    pub fn reduction(&self, binding: OperationBinding, change: bool) -> TransactionContext {
        let resulting = if change {
            self.organization.checked_successor().unwrap()
        } else {
            self.organization
        };
        let basis = OperationReductionBasis::new(OperationReductionBasisParts {
            binding,
            base: self.organization,
            resulting,
        })
        .unwrap();
        TransactionContext::new(self.store, self.organization, resulting, Some(basis)).unwrap()
    }
    pub fn apply(&mut self, projection: &LedgerProjection) -> CommitDisposition {
        assert_eq!(projection.expected_store(), self.store);
        assert_eq!(projection.base_organization(), self.organization);
        for write in projection.writes() {
            match write {
                LedgerWrite::RegisterStream(v) => {
                    self.streams
                        .entry(v.binding.stream().key())
                        .or_insert_with(|| v.clone());
                }
                LedgerWrite::AdmitOperation(v) => {
                    let header = self.streams.get_mut(&v.binding.id.stream().key()).unwrap();
                    assert_eq!(header.admitted_through + 1, v.binding.id.sequence().get());
                    header.admitted_through += 1;
                    header.dormant_reservation = None;
                    assert!(
                        self.entries
                            .insert(
                                v.binding.id,
                                RecoveredOperationRecord::Pending(Box::new(v.clone()))
                            )
                            .is_none()
                    );
                }
                LedgerWrite::RecordExternalProgress {
                    previous_progress_store_revision,
                    record,
                } => {
                    assert!(
                        matches!(self.entries.get(&record.binding.id), Some(RecoveredOperationRecord::Pending(old)) if old.progress_recorded_at == *previous_progress_store_revision)
                    );
                    self.entries.insert(
                        record.binding.id,
                        RecoveredOperationRecord::Pending(Box::new(record.clone())),
                    );
                }
                LedgerWrite::CompleteOperation { record, .. } => {
                    assert!(
                        matches!(self.entries.get(&record.binding.id), Some(RecoveredOperationRecord::Pending(old)) if old.binding == record.binding && old.reservation == record.reservation)
                    );
                    self.entries.insert(
                        record.binding.id,
                        RecoveredOperationRecord::Completed(record.clone()),
                    );
                }
                LedgerWrite::AcknowledgePrefix {
                    binding,
                    previous,
                    through,
                } => {
                    assert_eq!(
                        self.streams[&binding.stream().key()].acknowledged_through,
                        *previous
                    );
                    let ids: Vec<_> = self
                        .entries
                        .keys()
                        .filter(|id| {
                            id.stream() == binding.stream() && id.sequence().get() <= *through
                        })
                        .copied()
                        .collect();
                    for id in ids {
                        assert!(matches!(
                            self.entries.remove(&id),
                            Some(RecoveredOperationRecord::Completed(_))
                        ));
                    }
                    self.streams
                        .get_mut(&binding.stream().key())
                        .unwrap()
                        .acknowledged_through = *through;
                }
                LedgerWrite::RetireStream { binding, reason } => {
                    self.streams
                        .get_mut(&binding.stream().key())
                        .unwrap()
                        .status = StreamStatus::Retired(*reason);
                }
            }
        }
        self.store = projection.resulting_store();
        self.organization = projection.resulting_organization();
        CommitDisposition::Durable(CommitReceipt {
            store_revision: self.store,
            organization_revision: self.organization,
        })
    }
}
pub struct Harness {
    pub ledger: OperationLedger,
    pub rows: DurableRows,
}
impl Harness {
    pub fn new() -> Self {
        Self::with_limits(limits())
    }
    pub fn with_limits(limits: LedgerLimits) -> Self {
        Self {
            ledger: OperationLedger::empty(server(), limits).unwrap(),
            rows: DurableRows::new(limits),
        }
    }
    pub fn commit(&mut self, prepared: PreparedLedgerTransition) -> TransitionResolution {
        let disposition = self.rows.apply(prepared.projection());
        self.ledger.resolve_commit(prepared, disposition).unwrap()
    }
    pub fn register(&mut self, binding: StreamBinding) {
        let p = self
            .ledger
            .prepare_register_stream(binding, self.rows.context())
            .unwrap();
        assert!(matches!(
            self.commit(p),
            TransitionResolution::StreamRegistered
        ));
    }
    pub fn register_purpose(&mut self, binding: StreamBinding, purpose: StreamPurpose) {
        let p = self
            .ledger
            .prepare_register_stream_with_purpose(binding, purpose, self.rows.context())
            .unwrap();
        self.commit(p);
    }
    pub fn prepare(
        &mut self,
        binding: StreamBinding,
        n: u64,
        command: &Command,
    ) -> Result<AdmissionPreparation, LedgerFailure> {
        self.ledger.prepare_admission(
            &binding,
            op(binding, n),
            command,
            spaceterm_core::operation_ledger::admission::command_pending_kind(command),
            self.rows.context(),
        )
    }
    pub fn admit(&mut self, binding: StreamBinding, n: u64, command: &Command) -> ExecutionPermit {
        let AdmissionPreparation::Prepared(p) = self.prepare(binding, n, command).unwrap() else {
            panic!("expected new admission")
        };
        let TransitionResolution::Admitted(permit) = self.commit(p) else {
            panic!("expected permit")
        };
        permit
    }
    pub fn complete(&mut self, binding: StreamBinding, n: u64, outcome: &Outcome, change: bool) {
        let digest = match self.rows.entries.get(&op(binding, n)).unwrap() {
            RecoveredOperationRecord::Pending(p) => p.binding.digest,
            _ => panic!("expected pending"),
        };
        let context = self.rows.reduction(
            OperationBinding {
                id: op(binding, n),
                digest,
            },
            change,
        );
        let p = self
            .ledger
            .prepare_completion(
                &binding,
                op(binding, n),
                digest,
                compact(outcome),
                context.resulting_organization(),
                context,
            )
            .unwrap();
        assert!(matches!(self.commit(p), TransitionResolution::Completed));
    }
    pub fn ack(&mut self, binding: StreamBinding, n: u64) {
        if let Some(p) = self
            .ledger
            .prepare_acknowledgement(&binding, n, self.rows.context())
            .unwrap()
        {
            self.commit(p);
        }
    }
    pub fn retire(&mut self, binding: StreamBinding, reason: StreamRetirementReason) {
        let p = self
            .ledger
            .prepare_retirement(&[binding], reason, self.rows.context())
            .unwrap();
        self.commit(p);
    }
    pub fn reopen(&mut self) {
        self.ledger = OperationLedger::recover(server(), self.rows.recovery()).unwrap();
    }
}
pub fn assert_class<T: std::fmt::Debug>(
    result: Result<T, LedgerFailure>,
    expected: LedgerFailureClass,
) {
    assert_eq!(result.unwrap_err().class, expected);
}

pub fn retirement_command(b: StreamBinding) -> Command {
    Command::EndAllSessions {
        server: server(),
        assessment: AssessmentRef {
            operation: op(b, 99),
            digest: RequestDigest::from_bytes([9; 32]),
        },
    }
}
pub fn retirement_progress(
    binding: OperationBinding,
    revision: OrganizationRevision,
    cleanup: spaceterm_core::retirement::RetirementCleanupState,
) -> PendingExternalProgress {
    use spaceterm_core::retirement::*;
    let pane = PaneId::new(server(), 1).unwrap();
    let descriptor = RetirementDescriptor::new(RetirementDescriptorParts {
        batch: RetirementBatchId::new(server(), ServerEpoch::new(1), binding.id.sequence().get())
            .unwrap(),
        binding,
        assessment: AssessmentRef {
            operation: op(StreamBinding::from_stream(binding.id.stream()), 99),
            digest: RequestDigest::from_bytes([9; 32]),
        },
        acceptance_revision: revision,
        target: AssessmentTarget::EndAllSessions(server()),
        panes: vec![
            RetirementPane::new(
                WorkspaceId::new(server(), 1).unwrap(),
                TabId::new(server(), 1).unwrap(),
                pane,
                None,
                SessionGeneration::new(2),
            )
            .unwrap(),
        ],
        sessions: vec![
            RetiredSession::new(
                SessionKey {
                    epoch: ServerEpoch::new(1),
                    pane,
                    generation: SessionGeneration::new(1),
                },
                RetirementReason::EndAllSessions,
                cleanup,
            )
            .unwrap(),
        ],
        completion: RetirementCompletion::AppliedAtAcceptanceRevision,
        eventual_result: compact(&Outcome::Applied),
    })
    .unwrap();
    PendingExternalProgress::new(PendingExternalProgressParts {
        binding,
        effect: ExternalEffectKind::SessionLifecycle,
        state: ExternalProgressState::AcceptedRetirement(Box::new(descriptor)),
    })
    .unwrap()
}
pub fn git_progress(
    binding: OperationBinding,
    phase: ExternalIntentPhase,
    store: StoreRevision,
) -> PendingExternalProgress {
    let reference = ExternalIntentReference::new(ExternalIntentReferenceParts {
        binding,
        scope: ExternalIntentScope::Git(RepositoryScope {
            server: server(),
            repository: RepositoryId::new(server(), 1).unwrap(),
        }),
        phase,
        recorded_at: store,
    })
    .unwrap();
    PendingExternalProgress::new(PendingExternalProgressParts {
        binding,
        effect: ExternalEffectKind::GitWrite,
        state: ExternalProgressState::RecordedIntent {
            reference,
            retirement: None,
        },
    })
    .unwrap()
}
pub fn parts(projection: &LedgerProjection) -> TransactionParts {
    let marks = IdentityHighWater {
        workspace: 100,
        tab: 100,
        pane: 100,
        repository: 100,
        worktree: 100,
        pairing: 100,
        client: 100,
        operation_stream: 100,
        creation_ordinal: 100,
    };
    let mut parts = TransactionParts {
        server: server(),
        expected_store_revision: projection.expected_store(),
        resulting_store_revision: projection.resulting_store(),
        expected_organization_revision: projection.base_organization(),
        resulting_organization_revision: projection.resulting_organization(),
        expected_epoch: ServerEpoch::new(1),
        resulting_epoch: ServerEpoch::new(1),
        expected_high_water: marks,
        resulting_high_water: marks,
        organization_writes: Vec::new(),
        operation_writes: Vec::new(),
    };
    if let Some(basis) = projection.context().operation_basis() {
        parts
            .operation_writes
            .push(OperationWrite::ReductionBasis(basis));
    }
    projection.append_to(&mut parts).unwrap();
    parts
}
pub fn git_intent(
    binding: OperationBinding,
    phase: spaceterm_core::worktrees::intent::GitIntentPhase,
) -> spaceterm_core::worktrees::intent::GitWriteIntent {
    use spaceterm_core::worktrees::intent::*;
    use spaceterm_core::worktrees::types::*;
    let directory = ServerDirectory::new(server(), b"/repo".to_vec()).unwrap();
    GitWriteIntent::new(GitWriteIntentParts {
        binding,
        repository: RepositoryLocator::new(RepositoryLocatorParts {
            scope: RepositoryScope {
                server: server(),
                repository: RepositoryId::new(server(), 1).unwrap(),
            },
            main_root: directory.clone(),
            common_directory: directory.clone(),
        })
        .unwrap(),
        action: RetainedGitAction::Create {
            worktree: WorktreeId::new(server(), 1).unwrap(),
            directory,
            branch: WorktreeBranchChoice::Existing {
                name: BranchName::new("main".into()).unwrap(),
            },
            target: CreationTargetObservation::Absent,
            template_version: None,
        },
        phase,
        assessment_digest: GitAssessmentDigest::from_bytes([1; 32]),
        affected: RetainedRemovalScope::empty(server()),
        allocation: binding,
        external_result: if phase == GitIntentPhase::ExternalFinished {
            Some(RetainedExternalResult {
                disposition: ExternalDisposition::DesiredStateObserved,
                failure: None,
                ended_sessions: 0,
            })
        } else {
            None
        },
    })
    .unwrap()
}
