use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_core::{
    authority::*, identity::*, operation_ledger::canonical::CanonicalCodec,
    operation_ledger_contracts::*, operations::*, organization::*, reducer::*, retirement::*,
    storage::*, worktrees::*,
};
use spaceterm_protocol::{ProtocolError, authentication::SigningKey};
use spaceterm_server::adapters::core::{CommitCursor, CorePorts, ReductionRequest};
use std::sync::Arc;

#[allow(dead_code)]
#[path = "store_pairing_composition/support.rs"]
mod physical;

struct Key(Ed25519KeyPair);
impl Key {
    fn pin(&self) -> spaceterm_protocol::authentication::ServerPin {
        spaceterm_protocol::authentication::ServerPin {
            identity: [11; 16],
            public_key: self.public_key(),
        }
    }
    fn new(seed: u8) -> Self {
        Self(Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
    }
}
impl SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
struct Clock;
impl spaceterm_server::contracts::ServerClock for Clock {
    fn now(
        &self,
    ) -> Result<
        spaceterm_core::observations::MonotonicTick,
        spaceterm_server::contracts::ServerFailure,
    > {
        Ok(spaceterm_core::observations::MonotonicTick::new(1))
    }
}

struct Fixture {
    _directory: physical::Directory,
    storage: physical::Fixture,
    ports: CorePorts,
    cursor: CommitCursor,
    intent: GitWriteIntent,
    allocation: GitAllocationReservation,
    original: OrganizationSnapshot,
}
impl Fixture {
    fn new() -> Self {
        let directory = physical::Directory::new();
        let home = ServerDirectory::new(
            physical::identity().server,
            directory.0.as_os_str().as_encoded_bytes().to_vec(),
        )
        .unwrap();
        let mut storage = physical::Fixture::new(&directory.0);
        storage.enroll(42);
        let recovered = storage.recovered();
        let stream = recovered.pairing.as_ref().unwrap().parts().authorizations[0]
            .parts()
            .clients[0]
            .parts()
            .ordinary;
        let operation = OperationId::new(stream, OperationSequence::new(1).unwrap());
        let mut reducer = OrganizationReducer::from_recovered(
            RetainedOrganization::new(
                recovered.organization.clone(),
                recovered.high_water,
                home.clone(),
            ),
            recovered.store_revision,
            recovered.last_epoch,
            OrganizationLimits::default(),
        )
        .unwrap();
        let command = Command::CreateWorkspace {
            server: home.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: home.server(),
            },
            name: NameDraft::Automatic,
        };
        let binding = CanonicalOperationRequest::new(operation, command.clone(), &CanonicalCodec)
            .unwrap()
            .binding();
        let mut context = ReductionContext {
            principal: MutationPrincipal {
                pairing: stream.pairing(),
                client: stream.client(),
            },
            operation,
            request_digest: binding.digest,
            now: ReducerTick::new(1),
            facts: ReductionFacts::default(),
        };
        context.facts.directories = Some(DirectoryValidation {
            binding,
            basis: reducer.directory_basis(&command).unwrap(),
            home: DirectoryFact {
                directory: home.clone(),
                available: true,
                physically_inside: None,
            },
            pin: None,
            source: None,
            requested: None,
            worktree: None,
            worktree_bare: false,
        });
        let candidate = reducer.prepare_command(&command, &context).unwrap();
        seed(&mut storage, &mut reducer, candidate);
        let tab = reducer.snapshot().state().tabs[0].id;
        let main = ServerDirectory::new(home.server(), b"/fixture/main".to_vec()).unwrap();
        let linked = ServerDirectory::new(home.server(), b"/fixture/linked".to_vec()).unwrap();
        let candidate = reducer
            .prepare_git_inventory_reconciliation(
                &GitInventoryPatch {
                    repository: None,
                    directory: main.clone(),
                    expected_inventory_version: None,
                    worktrees: vec![
                        GitInventoryWorktree {
                            id: None,
                            directory: main.clone(),
                            main: true,
                            missing: false,
                            listed: true,
                        },
                        GitInventoryWorktree {
                            id: None,
                            directory: linked.clone(),
                            main: false,
                            missing: false,
                            listed: true,
                        },
                    ],
                },
                &[GitInventoryMembership {
                    tab,
                    expected_version: FieldVersion::new(1),
                    destination: Some(1),
                }],
                &context,
            )
            .unwrap();
        seed(&mut storage, &mut reducer, candidate);
        let recovered = storage.recovered();
        let mut ports = CorePorts::production(physical::identity(), home, Arc::new(Clock)).unwrap();
        ports.ledger.recover(&recovered).unwrap();
        let reduction = ports
            .reducer
            .recover(&recovered, recovered.last_epoch, &[])
            .unwrap();
        let mut cursor = CommitCursor::from_recovered(&recovered);
        let mut parts = cursor.parts().unwrap();
        reduction.append_to(&mut parts).unwrap();
        commit(&mut storage, &mut ports, &mut cursor, parts);
        let original = ports.reducer.snapshot().as_ref().clone();
        let workspace = original.state().workspaces[0].id;
        let repository = original.state().repositories[0].id;
        let target = original
            .state()
            .worktrees
            .iter()
            .find(|v| !v.main)
            .unwrap()
            .id;
        let main_id = original
            .state()
            .worktrees
            .iter()
            .find(|v| v.main)
            .unwrap()
            .id;
        let token = GitAssessmentToken::new(GitAssessmentTokenParts {
            server: stream.server(),
            epoch: cursor.epoch,
            pairing: stream.pairing(),
            client: stream.client(),
            kind: GitAssessmentKind::Remove,
            random: [29; 32],
        })
        .unwrap();
        let request = CanonicalOperationRequest::new(
            operation,
            Command::Worktree(GitCommand::RemoveConfirmed { assessment: token }),
            &CanonicalCodec,
        )
        .unwrap();
        let binding = request.binding();
        let mut parts = cursor.parts().unwrap();
        ports
            .ledger
            .prepare_admission(
                &request,
                PendingKind::External {
                    effect: ExternalEffectKind::GitWrite,
                },
                cursor.context().unwrap(),
            )
            .unwrap()
            .append_to(&mut parts)
            .unwrap();
        commit(&mut storage, &mut ports, &mut cursor, parts);
        let fallback = WorkspaceFallbackPlan {
            workspace,
            worktree: main_id,
            directory: main,
        };
        let reduction = ports
            .reducer
            .prepare(
                ReductionRequest::ReserveGitAllocation {
                    binding,
                    repository: false,
                    worktree: false,
                    fallbacks: std::slice::from_ref(&fallback),
                },
                cursor,
            )
            .unwrap();
        let allocation = reduction
            .auxiliary_writes
            .iter()
            .find_map(|write| match write {
                OperationWrite::GitAllocation(GitAllocationMutation::Reserve(value)) => {
                    Some(value.as_ref().clone())
                }
                _ => None,
            })
            .unwrap();
        let mut parts = cursor.parts().unwrap();
        reduction.append_to(&mut parts).unwrap();
        commit(&mut storage, &mut ports, &mut cursor, parts);
        let runtime = original.runtime()[0];
        let intent = GitWriteIntent::new(GitWriteIntentParts {
            binding,
            repository: RepositoryLocator::new(RepositoryLocatorParts {
                scope: RepositoryScope {
                    server: stream.server(),
                    repository,
                },
                main_root: original.state().repositories[0].directory.clone(),
                common_directory: ServerDirectory::new(
                    stream.server(),
                    b"/fixture/main/.git".to_vec(),
                )
                .unwrap(),
            })
            .unwrap(),
            action: RetainedGitAction::Remove {
                worktree: target,
                directory: linked,
                head: None,
                approval: ApprovedRemoval::Clean,
            },
            phase: GitIntentPhase::Prepared,
            assessment_digest: GitAssessmentDigest::from_bytes([30; 32]),
            affected: RetainedRemovalScope::new(RetainedRemovalScopeParts {
                server: stream.server(),
                tabs: original.state().tabs.clone(),
                panes: vec![RetainedAffectedPane {
                    hierarchy: AssessedPane {
                        workspace,
                        tab,
                        pane: runtime.pane,
                        session_generation: runtime.session_generation,
                        control_generation: runtime.control_generation,
                        context_generation: ContextGeneration::new(1),
                    },
                    session: None,
                    work_generation: CloseWorkGeneration::new(0),
                    startup_reserved: false,
                }],
                membership_versions: original
                    .state()
                    .retained
                    .as_ref()
                    .unwrap()
                    .tab_membership_versions()
                    .to_vec(),
                fallbacks: vec![fallback],
            })
            .unwrap(),
            allocation: binding,
            external_result: None,
        })
        .unwrap();
        let mut fixture = Self {
            _directory: directory,
            storage,
            ports,
            cursor,
            intent,
            allocation,
            original,
        };
        fixture.progress(
            GitIntentPhase::Prepared,
            vec![
                OperationWrite::GitRepository(GitRepositoryMutation::Put(
                    fixture.intent.parts().repository.clone(),
                )),
                OperationWrite::GitIntent(GitIntentMutation::Insert(Box::new(
                    fixture.intent.clone(),
                ))),
            ],
        );
        fixture.progress(
            GitIntentPhase::Started,
            vec![OperationWrite::GitIntent(GitIntentMutation::Advance {
                binding,
                expected_phase: GitIntentPhase::Prepared,
                next: GitIntentPhase::Started,
                result: None,
            })],
        );
        let mut parts = fixture.intent.parts().clone();
        parts.phase = GitIntentPhase::Started;
        fixture.intent = GitWriteIntent::new(parts).unwrap();
        fixture
    }
    fn progress(&mut self, phase: GitIntentPhase, writes: Vec<OperationWrite>) {
        let mut parts = self.cursor.parts().unwrap();
        parts.operation_writes.extend(writes);
        self.ports
            .ledger
            .prepare_external_progress(
                self.intent.binding(),
                progress(
                    &self.intent,
                    phase,
                    None,
                    self.cursor.store.checked_successor().unwrap(),
                ),
                self.cursor.context().unwrap(),
            )
            .unwrap()
            .append_to(&mut parts)
            .unwrap();
        commit(&mut self.storage, &mut self.ports, &mut self.cursor, parts);
    }
    fn retire(&mut self) -> RetirementDescriptor {
        let pane = self.original.runtime()[0];
        let tab = &self.original.state().tabs[0];
        let descriptor = RetirementDescriptor::new(RetirementDescriptorParts {
            batch: RetirementBatchId::new(self.cursor.server, self.cursor.epoch, 1).unwrap(),
            binding: self.intent.binding(),
            assessment: AssessmentRef {
                operation: self.intent.binding().id,
                digest: self.intent.binding().digest,
            },
            acceptance_revision: self.cursor.organization.checked_successor().unwrap(),
            target: AssessmentTarget::RemoveWorktree {
                scope: self.intent.parts().repository.scope(),
                worktree: self.intent.parts().action.worktree(),
            },
            panes: vec![
                RetirementPane::new(
                    tab.workspace,
                    tab.id,
                    pane.pane,
                    tab.worktree,
                    pane.session_generation.checked_successor().unwrap(),
                )
                .unwrap(),
            ],
            sessions: vec![],
            completion: RetirementCompletion::ContinueGitSettlement,
            eventual_result: CompactOutcome::new(&Outcome::Applied, &CanonicalCodec).unwrap(),
        })
        .unwrap();
        let reduction = self
            .ports
            .reducer
            .prepare(
                ReductionRequest::StartedGitRetirement {
                    intent: &self.intent,
                    descriptor: &descriptor,
                },
                self.cursor,
            )
            .unwrap();
        assert_eq!(reduction.retirement.as_ref(), Some(&descriptor));
        let mut parts = self.cursor.parts().unwrap();
        reduction.append_to(&mut parts).unwrap();
        let basis = OperationReductionBasis::new(OperationReductionBasisParts {
            binding: self.intent.binding(),
            base: self.cursor.organization,
            resulting: parts.resulting_organization_revision,
        })
        .unwrap();
        parts
            .operation_writes
            .push(OperationWrite::ReductionBasis(basis));
        self.ports
            .ledger
            .prepare_external_progress(
                self.intent.binding(),
                progress(
                    &self.intent,
                    GitIntentPhase::Started,
                    Some(descriptor.clone()),
                    self.cursor.store,
                ),
                TransactionContext::new(
                    self.cursor.store,
                    self.cursor.organization,
                    parts.resulting_organization_revision,
                    Some(basis),
                )
                .unwrap(),
            )
            .unwrap()
            .append_to(&mut parts)
            .unwrap();
        commit(&mut self.storage, &mut self.ports, &mut self.cursor, parts);
        descriptor
    }
    fn settle(&mut self, external: RetainedExternalResult, revoked: &[ClientId]) -> Outcome {
        let retirement = self
            .storage
            .recovered()
            .ledger
            .unwrap()
            .operations()
            .iter()
            .find_map(|entry| match entry {
                RecoveredOperationRecord::Pending(record)
                    if record.binding == self.intent.binding() =>
                {
                    match record.external_progress.as_ref().unwrap().state() {
                        ExternalProgressState::RecordedIntent { retirement, .. } => {
                            retirement.as_deref().cloned()
                        }
                        _ => None,
                    }
                }
                _ => None,
            });
        let mut parts = self.cursor.parts().unwrap();
        parts
            .operation_writes
            .push(OperationWrite::GitIntent(GitIntentMutation::Advance {
                binding: self.intent.binding(),
                expected_phase: GitIntentPhase::Started,
                next: GitIntentPhase::ExternalFinished,
                result: Some(external.clone()),
            }));
        self.ports
            .ledger
            .prepare_external_progress(
                self.intent.binding(),
                progress(
                    &self.intent,
                    GitIntentPhase::ExternalFinished,
                    retirement,
                    self.cursor.store.checked_successor().unwrap(),
                ),
                self.cursor.context().unwrap(),
            )
            .unwrap()
            .append_to(&mut parts)
            .unwrap();
        commit(&mut self.storage, &mut self.ports, &mut self.cursor, parts);
        let mut intent = self.intent.parts().clone();
        intent.phase = GitIntentPhase::ExternalFinished;
        intent.external_result = Some(external.clone());
        self.intent = GitWriteIntent::new(intent).unwrap();
        let reduction = self
            .ports
            .reducer
            .prepare(
                ReductionRequest::SettleStartedGit {
                    intent: &self.intent,
                    result: &external,
                    revoked_clients: revoked,
                },
                self.cursor,
            )
            .unwrap();
        let outcome = reduction.outcome.clone();
        let mut parts = self.cursor.parts().unwrap();
        reduction.append_to(&mut parts).unwrap();
        let basis = OperationReductionBasis::new(OperationReductionBasisParts {
            binding: self.intent.binding(),
            base: self.cursor.organization,
            resulting: parts.resulting_organization_revision,
        })
        .unwrap();
        parts
            .operation_writes
            .push(OperationWrite::ReductionBasis(basis));
        self.ports
            .ledger
            .prepare_completion(
                self.intent.binding(),
                &outcome,
                parts.resulting_organization_revision,
                TransactionContext::new(
                    self.cursor.store,
                    self.cursor.organization,
                    parts.resulting_organization_revision,
                    Some(basis),
                )
                .unwrap(),
            )
            .unwrap()
            .append_to(&mut parts)
            .unwrap();
        commit(&mut self.storage, &mut self.ports, &mut self.cursor, parts);
        outcome
    }
}
fn progress(
    intent: &GitWriteIntent,
    phase: GitIntentPhase,
    retirement: Option<RetirementDescriptor>,
    recorded_at: StoreRevision,
) -> PendingExternalProgress {
    PendingExternalProgress::new(PendingExternalProgressParts {
        binding: intent.binding(),
        effect: ExternalEffectKind::GitWrite,
        state: ExternalProgressState::RecordedIntent {
            reference: ExternalIntentReference::new(ExternalIntentReferenceParts {
                binding: intent.binding(),
                scope: ExternalIntentScope::Git(intent.parts().repository.scope()),
                recorded_at,
                phase: match phase {
                    GitIntentPhase::Prepared => ExternalIntentPhase::Prepared,
                    GitIntentPhase::Started => ExternalIntentPhase::Started,
                    GitIntentPhase::ExternalFinished => ExternalIntentPhase::ExternalFinished,
                },
            })
            .unwrap(),
            retirement: retirement.map(Box::new),
        },
    })
    .unwrap()
}
fn seed(
    storage: &mut physical::Fixture,
    reducer: &mut OrganizationReducer,
    candidate: spaceterm_core::reducer::PreparedReduction,
) {
    let mut parts = CommitCursor::from_recovered(&storage.recovered())
        .parts()
        .unwrap();
    parts.resulting_organization_revision = candidate.resulting_organization_revision();
    parts.resulting_high_water = candidate.writes().resulting_high_water();
    parts.organization_writes = candidate.writes().organization_writes().to_vec();
    parts.operation_writes = candidate
        .writes()
        .operation_writes()
        .iter()
        .filter(|v| !matches!(v, OperationWrite::ReductionBasis(_)))
        .cloned()
        .collect();
    let transaction =
        DurableTransaction::new_with_validation(parts, &physical::services()).unwrap();
    let CommitDisposition::Durable(receipt) = storage.store.commit(&transaction) else {
        panic!("seed")
    };
    reducer.apply_committed(candidate, &receipt).unwrap();
    storage.ledger.observe_store_commit(receipt).unwrap();
}
fn commit(
    storage: &mut physical::Fixture,
    ports: &mut CorePorts,
    cursor: &mut CommitCursor,
    parts: TransactionParts,
) {
    let transaction =
        DurableTransaction::new_with_validation(parts, ports.validator.as_ref()).unwrap();
    let disposition = storage.store.commit(&transaction);
    let CommitDisposition::Durable(receipt) = disposition else {
        panic!("commit: {disposition:?} at {}", cursor.store.get())
    };
    cursor.install(transaction.parts(), receipt).unwrap();
    ports.reducer.committed(receipt).unwrap();
    ports.ledger.committed(receipt).unwrap();
}
#[test]
fn started_git_reduction_settles_membership_intent_and_original_outcome_atomically() {
    let mut fixture = Fixture::new();
    fixture.retire();
    let retired = fixture.storage.recovered();
    assert_eq!(retired.organization.tabs, fixture.original.state().tabs);
    assert_eq!(retired.organization.panes, fixture.original.state().panes);
    let outcome = fixture.settle(
        RetainedExternalResult {
            disposition: ExternalDisposition::DesiredStateObserved,
            failure: None,
            ended_sessions: 0,
        },
        &[],
    );
    let recovered = fixture.storage.recovered();
    assert!(recovered.git_intents.is_empty() && recovered.git_allocations.is_empty());
    assert_eq!(
        recovered.organization.tabs[0].id,
        fixture.allocation.parts().fallbacks[0].tab
    );
    assert_eq!(
        recovered.organization.panes[0].id,
        fixture.allocation.parts().fallbacks[0].pane
    );
    assert!(
        matches!(outcome, Outcome::Git(ref result) if matches!(result.parts(), GitMutationOutcomeParts::Removed { revision, closed_tabs: 1, .. } if *revision == recovered.organization.revision))
    );
    let entry = recovered
        .ledger
        .unwrap()
        .operations()
        .iter()
        .find_map(|entry| match entry {
            RecoveredOperationRecord::Completed(record)
                if record.binding == fixture.intent.binding() =>
            {
                Some(record.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(entry.outcome.outcome(&CanonicalCodec).unwrap(), outcome);
    assert_eq!(entry.outcome_revision, recovered.organization.revision);
}
#[test]
fn failed_started_git_keeps_original_ended_membership_and_abandons_reserved_ids() {
    let mut fixture = Fixture::new();
    fixture.retire();
    let outcome = fixture.settle(
        RetainedExternalResult {
            disposition: ExternalDisposition::NoEffectEstablished,
            failure: Some(GitFailure::WriteFailed),
            ended_sessions: 0,
        },
        &[],
    );
    let recovered = fixture.storage.recovered();
    assert_eq!(recovered.organization.tabs, fixture.original.state().tabs);
    assert_eq!(recovered.organization.panes, fixture.original.state().panes);
    assert!(recovered.git_intents.is_empty() && recovered.git_allocations.is_empty());
    assert!(
        matches!(outcome, Outcome::Git(ref result) if matches!(result.parts(), GitMutationOutcomeParts::Failed { failure: GitFailure::WriteFailed, .. }))
    );
    assert_eq!(
        recovered.high_water,
        fixture.allocation.parts().resulting_high_water
    );
    assert!(matches!(
        fixture.ports.reducer.snapshot().runtime()[0].phase,
        SessionPhase::Ended {
            reason: EndReason::WorktreeRemoval
        }
    ));
}
#[test]
fn revoked_started_git_creator_receives_no_usable_fallback_grant() {
    let mut fixture = Fixture::new();
    fixture.retire();
    fixture.settle(
        RetainedExternalResult {
            disposition: ExternalDisposition::DesiredStateObserved,
            failure: None,
            ended_sessions: 0,
        },
        &[fixture.intent.binding().id.stream().client()],
    );
    assert_eq!(
        fixture.ports.reducer.snapshot().runtime()[0].control,
        ControlRecord::Unclaimed
    );
    assert_eq!(
        fixture.ports.reducer.snapshot().runtime()[0].phase,
        SessionPhase::NotStarted
    );
}

#[test]
fn recovery_rejects_git_retirement_outside_the_original_intent_hierarchy() {
    let mut fixture = Fixture::new();
    fixture.retire();
    let mut recovered = fixture.storage.recovered();
    let ledger = recovered.ledger.as_mut().unwrap();
    let record = ledger
        .operations
        .iter_mut()
        .find_map(|entry| match entry {
            RecoveredOperationRecord::Pending(record)
                if record.binding == fixture.intent.binding() =>
            {
                Some(record.as_mut())
            }
            _ => None,
        })
        .unwrap();
    let progress = record.external_progress.as_ref().unwrap();
    let ExternalProgressState::RecordedIntent {
        reference,
        retirement: Some(descriptor),
    } = progress.state()
    else {
        panic!("retirement")
    };
    let mut descriptor = descriptor.parts().clone();
    let fallback = &fixture.allocation.parts().fallbacks[0];
    descriptor.panes = vec![
        RetirementPane::new(
            fallback.plan.workspace,
            fallback.tab,
            fallback.pane,
            Some(fixture.intent.parts().action.worktree()),
            SessionGeneration::new(1),
        )
        .unwrap(),
    ];
    record.external_progress = Some(
        PendingExternalProgress::new(PendingExternalProgressParts {
            binding: fixture.intent.binding(),
            effect: ExternalEffectKind::GitWrite,
            state: ExternalProgressState::RecordedIntent {
                reference: *reference,
                retirement: Some(Box::new(RetirementDescriptor::new(descriptor).unwrap())),
            },
        })
        .unwrap(),
    );
    assert!(
        recovered
            .validate_with(fixture.ports.validator.as_ref())
            .is_err()
    );
}
