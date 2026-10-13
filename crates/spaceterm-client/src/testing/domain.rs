use super::*;
use spaceterm_core::{
    operation_ledger::{
        admission::{command_pending_kind, reservation_class},
        canonical::CanonicalCodec,
        model::{AdmissionPreparation, OperationLedger, TransitionResolution},
    },
    operation_ledger_contracts::*,
    organization_retained::*,
    reducer::*,
    remote_access::RemoteAccessPolicy,
    storage::*,
};

pub(super) struct Domain {
    pub reducer: OrganizationReducer,
    ledger: OperationLedger,
    intents: BTreeMap<OperationId, StartupIntent>,
    connections: BTreeMap<ClientId, ActiveConnection>,
}
struct ActiveConnection {
    binding: ConnectionBinding,
    contexts: BTreeMap<PaneId, ContextGeneration>,
}
impl Domain {
    pub fn new(server: ServerId) -> Result<Self, ClientFailure> {
        let organization = OrganizationState {
            server,
            revision: OrganizationRevision::new(0),
            settings: ServerSettingsRecord {
                worktree_location: Versioned {
                    version: FieldVersion::new(1),
                    value: WorktreeLocationTemplate::new(
                        "~/.worktrees/{repository}/{branch}".into(),
                    )
                    .map_err(|_| invalid_peer())?,
                },
            },
            workspaces: vec![],
            tabs: vec![],
            panes: vec![],
            repositories: vec![],
            worktrees: vec![],
            retained: Some(
                OrganizationRetainedFacts::new(OrganizationRetainedParts {
                    server,
                    workspace_names: vec![],
                    creation_defaults: vec![],
                    pane_generations: vec![],
                    tab_membership_versions: vec![],
                    repository_inventory_versions: vec![],
                    worktree_inventory: vec![],
                    remote_access: Versioned {
                        version: FieldVersion::new(1),
                        value: RemoteAccessPolicy::disabled(server),
                    },
                })
                .map_err(|_| invalid_peer())?,
            ),
        };
        let retained = RetainedOrganization::new(
            organization,
            IdentityHighWater {
                pairing: 1,
                client: 64,
                operation_stream: 128,
                ..IdentityHighWater::default()
            },
            ServerDirectory::new(server, b"/fixture".to_vec()).map_err(|_| invalid_peer())?,
        );
        Ok(Self {
            reducer: OrganizationReducer::from_recovered(
                retained,
                StoreRevision::new(0),
                ServerEpoch::new(1),
                OrganizationLimits::default(),
            )
            .map_err(|_| invalid_peer())?,
            ledger: OperationLedger::empty(server, LedgerLimits::production_v1())
                .map_err(|_| invalid_peer())?,
            intents: BTreeMap::new(),
            connections: BTreeMap::new(),
        })
    }
    pub fn activate(&mut self, binding: ConnectionBinding) -> Result<(), ClientFailure> {
        if binding.generation.get() == 0
            || binding.client.server() != self.reducer.snapshot().state().server
        {
            return Err(stale());
        }
        if let Some(active) = self.connections.get(&binding.client) {
            if active.binding == binding {
                return Ok(());
            }
            if active.binding.generation >= binding.generation {
                return Err(stale());
            }
        }
        self.intents
            .retain(|id, _| id.stream().client() != binding.client);
        self.connections.insert(
            binding.client,
            ActiveConnection {
                binding,
                contexts: BTreeMap::new(),
            },
        );
        Ok(())
    }
    pub fn deactivate(&mut self, binding: ConnectionBinding) {
        if self
            .connections
            .get(&binding.client)
            .is_some_and(|active| active.binding == binding)
        {
            self.connections.remove(&binding.client);
            self.intents
                .retain(|id, _| id.stream().client() != binding.client);
        }
    }
    pub fn update_context(
        &mut self,
        binding: ConnectionBinding,
        pane: PaneId,
        context: ContextGeneration,
    ) -> Result<(), ClientFailure> {
        self.runtime(pane)?;
        let active = self
            .connections
            .get_mut(&binding.client)
            .filter(|active| active.binding == binding)
            .ok_or_else(stale)?;
        if context.get() == 0 || active.contexts.get(&pane).is_some_and(|old| *old > context) {
            return Err(stale());
        }
        if active.contexts.len() >= 4096 && !active.contexts.contains_key(&pane) {
            return Err(resource_limit());
        }
        active.contexts.insert(pane, context);
        Ok(())
    }
    fn context(
        &self,
        resulting: OrganizationRevision,
        binding: Option<OperationBinding>,
    ) -> Result<TransactionContext, ClientFailure> {
        let base = self.reducer.snapshot().state().revision;
        let basis = binding
            .map(|binding| {
                OperationReductionBasis::new(OperationReductionBasisParts {
                    binding,
                    base,
                    resulting,
                })
                .map_err(|_| invalid_peer())
            })
            .transpose()?;
        TransactionContext::new(self.reducer.store_revision(), base, resulting, basis)
            .map_err(|_| invalid_peer())
    }
    fn commit_ledger(
        &mut self,
        prepared: spaceterm_core::operation_ledger::model::PreparedLedgerTransition,
    ) -> Result<TransitionResolution, ClientFailure> {
        let receipt = CommitReceipt {
            store_revision: prepared.projection().resulting_store(),
            organization_revision: prepared.projection().resulting_organization(),
        };
        let result = self
            .ledger
            .resolve_commit(prepared, CommitDisposition::Durable(receipt))
            .map_err(|_| invalid_peer())?;
        self.reducer
            .observe_ledger_commit(&receipt)
            .map_err(|_| invalid_peer())?;
        Ok(result)
    }
    pub fn register(&mut self, stream: OperationStreamId) -> Result<(), ClientFailure> {
        let context = self.context(self.reducer.snapshot().state().revision, None)?;
        let prepared = self
            .ledger
            .prepare_register_stream(StreamBinding::from_stream(stream), context)
            .map_err(|_| invalid_peer())?;
        self.commit_ledger(prepared)?;
        Ok(())
    }
    pub fn cancel_intent(&mut self, intent: StartupIntent) {
        self.intents.remove(&intent.id());
    }
    pub fn next_operation(&self, stream: OperationStreamId) -> Result<OperationId, ClientFailure> {
        let header = self
            .ledger
            .stream(&StreamBinding::from_stream(stream))
            .map_err(|_| stale())?;
        if header.status != StreamStatus::Active {
            return Err(stale());
        }
        Ok(OperationId::new(
            stream,
            OperationSequence::new(
                header
                    .admitted_through
                    .checked_add(1)
                    .ok_or_else(exhausted)?,
            )
            .map_err(|_| exhausted())?,
        ))
    }
    pub fn lookup(&self, binding: OperationBinding) -> Result<OperationLookup, ClientFailure> {
        self.ledger
            .lookup(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id,
                Some(binding.digest),
            )
            .map_err(|_| invalid_peer())
    }
    pub fn acknowledge(
        &mut self,
        stream: OperationStreamId,
        through: u64,
    ) -> Result<(), ClientFailure> {
        let context = self.context(self.reducer.snapshot().state().revision, None)?;
        if let Some(prepared) = self
            .ledger
            .prepare_acknowledgement(&StreamBinding::from_stream(stream), through, context)
            .map_err(|_| invalid_peer())?
        {
            self.commit_ledger(prepared)?;
        }
        Ok(())
    }
    pub fn retire_stream(&mut self, stream: OperationStreamId) -> Result<(), ClientFailure> {
        let context = self.context(self.reducer.snapshot().state().revision, None)?;
        let prepared = self
            .ledger
            .prepare_retirement(
                &[StreamBinding::from_stream(stream)],
                StreamRetirementReason::ExplicitRetirement,
                context,
            )
            .map_err(|_| invalid_peer())?;
        self.commit_ledger(prepared)?;
        Ok(())
    }
    pub fn event(&mut self, event: &OrganizationEvent) -> Result<(), ClientFailure> {
        let prepared = self
            .reducer
            .prepare_event(event)
            .map_err(|_| invalid_peer())?;
        let receipt = CommitReceipt {
            store_revision: prepared
                .expected_store_revision()
                .checked_successor()
                .ok_or_else(exhausted)?,
            organization_revision: prepared.resulting_organization_revision(),
        };
        self.reducer
            .apply_committed(prepared, &receipt)
            .map_err(|_| invalid_peer())?;
        Ok(())
    }
    pub fn command(
        &mut self,
        request: &CanonicalOperationRequest,
        dispatch: Option<ConnectionBinding>,
    ) -> Result<OperationLookup, ClientFailure> {
        let command = request.command();
        let binding = request.binding();
        // External native/Git effects are not simulated by ordinary fake mutations.
        if command_pending_kind(command) != PendingKind::AtomicMutation {
            return Err(ClientFailure::new(ClientFailureClass::MissingCapability));
        }
        reservation_class(StreamPurpose::Ordinary, command).map_err(|_| invalid_peer())?;
        let stream = StreamBinding::from_stream(binding.id.stream());
        let context = self.context(self.reducer.snapshot().state().revision, None)?;
        match self
            .ledger
            .prepare_admission(
                &stream,
                binding.id,
                command,
                PendingKind::AtomicMutation,
                context,
            )
            .map_err(|_| invalid_peer())?
        {
            AdmissionPreparation::Existing(lookup) => return Ok(lookup),
            AdmissionPreparation::Prepared(prepared) => {
                if !matches!(
                    self.commit_ledger(prepared)?,
                    TransitionResolution::Admitted(_)
                ) {
                    return Err(invalid_peer());
                }
            }
        }
        let snapshot = self.reducer.snapshot();
        let (reduction, outcome) = match command {
            Command::CompareAcquire(request) => {
                self.control(spaceterm_core::control_policy::compare_acquire(
                    snapshot.epoch(),
                    stream.client(),
                    self.runtime(request.pane)?,
                    *request,
                ))
            }
            Command::TakeOver(request) => self.control(spaceterm_core::control_policy::take_over(
                snapshot.epoch(),
                stream.client(),
                self.runtime(request.pane)?,
                *request,
            )),
            Command::ReleaseControl(request) => {
                self.control(spaceterm_core::control_policy::release_exact(
                    snapshot.epoch(),
                    stream.client(),
                    self.runtime(request.grant.pane())?,
                    *request,
                ))
            }
            Command::BeginStartup(intent) => {
                let outcome = if self
                    .startup_context(intent.fence().session().pane, stream.client(), dispatch)
                    .is_ok_and(|context| {
                        spaceterm_core::startup_policy::validate_explicit_use(*intent, context)
                            .is_ok()
                    }) {
                    self.intents.insert(intent.id(), *intent);
                    Outcome::StartupAwaitingMeasurement {
                        intent: intent.id(),
                    }
                } else {
                    Outcome::Failed(OperationFailure::StaleAuthority)
                };
                Ok((None, outcome))
            }
            Command::CancelStartup { intent } => {
                self.intents.remove(intent);
                Ok((None, Outcome::Applied))
            }
            Command::CompleteStartup(measurement) => {
                self.complete_startup((*measurement).into(), stream.client(), dispatch)
            }
            Command::CompleteMeasuredStartup(measurement) => {
                self.complete_startup(*measurement, stream.client(), dispatch)
            }
            _ => {
                let mut context = ReductionContext {
                    principal: MutationPrincipal {
                        pairing: stream.pairing(),
                        client: stream.client(),
                    },
                    operation: binding.id,
                    request_digest: binding.digest,
                    now: ReducerTick::new(1),
                    facts: ReductionFacts::default(),
                };
                if matches!(
                    command,
                    Command::CreateWorkspace { .. }
                        | Command::CreateTab { .. }
                        | Command::CreatePane { .. }
                        | Command::SetPinnedDirectory { .. }
                        | Command::RepairPaneDirectory { .. }
                ) {
                    let basis = self
                        .reducer
                        .directory_basis(command)
                        .map_err(|_| invalid_peer())?;
                    let fact = |directory| DirectoryFact {
                        directory,
                        available: true,
                        physically_inside: basis.worktree.as_ref().map(|v| v.worktree.id),
                    };
                    context.facts.directories = Some(DirectoryValidation {
                        binding,
                        home: fact(self.reducer.retained().server_home().clone()),
                        pin: basis.pin.as_ref().and_then(|v| v.value.clone()).map(fact),
                        source: basis.source.as_ref().map(|v| fact(v.directory.clone())),
                        requested: basis.requested.clone().map(fact),
                        worktree: basis
                            .worktree
                            .as_ref()
                            .map(|v| fact(v.worktree.directory.clone())),
                        worktree_bare: false,
                        basis,
                    });
                }
                let reduction = self
                    .reducer
                    .prepare_command(command, &context)
                    .map_err(|_| invalid_peer())?;
                let Resolution::Completed(outcome) = reduction.resolution() else {
                    return Err(invalid_peer());
                };
                let outcome = outcome.clone();
                Ok((Some(reduction), outcome))
            }
        }?;
        let revision = reduction
            .as_ref()
            .map(|r| r.resulting_organization_revision())
            .unwrap_or(snapshot.state().revision);
        let context = self.context(revision, Some(binding))?;
        let prepared = self
            .ledger
            .prepare_completion(
                &stream,
                binding.id,
                binding.digest,
                CompactOutcome::new(&outcome, &CanonicalCodec).map_err(|_| invalid_peer())?,
                revision,
                context,
            )
            .map_err(|_| invalid_peer())?;
        let receipt = CommitReceipt {
            store_revision: prepared.projection().resulting_store(),
            organization_revision: revision,
        };
        // This is the test-only atomic commit boundary, not native durability evidence.
        self.ledger
            .resolve_commit(prepared, CommitDisposition::Durable(receipt))
            .map_err(|_| invalid_peer())?;
        if let Some(reduction) = reduction {
            self.reducer
                .apply_committed(reduction, &receipt)
                .map_err(|_| invalid_peer())?;
        } else {
            self.reducer
                .observe_ledger_commit(&receipt)
                .map_err(|_| invalid_peer())?;
        }
        if let Outcome::SessionStarted(session) = outcome {
            self.event(&OrganizationEvent::RuntimeStarted { session })?;
        }
        self.lookup(binding)
    }
    fn complete_startup(
        &mut self,
        measurement: MeasuredStartupMeasurement,
        client: ClientId,
        dispatch: Option<ConnectionBinding>,
    ) -> Result<(Option<PreparedReduction>, Outcome), ClientFailure> {
        let Some(intent) = self.intents.remove(&measurement.intent) else {
            return Ok((None, Outcome::Failed(OperationFailure::StaleAuthority)));
        };
        if !self
            .startup_context(measurement.fence.session().pane, client, dispatch)
            .is_ok_and(|context| {
                spaceterm_core::startup_policy::validate_measurement(
                    intent,
                    measurement,
                    context,
                    spaceterm_core::geometry::SUPPORTED_GRID_LIMITS,
                )
                .is_ok()
            })
        {
            Ok((None, Outcome::Failed(OperationFailure::StaleAuthority)))
        } else {
            let runtime = self.runtime(measurement.fence.session().pane)?;
            let session = SessionKey {
                generation: runtime
                    .session_generation
                    .checked_successor()
                    .ok_or_else(exhausted)?,
                ..measurement.fence.session()
            };
            let reservation = StartupReservation::new(
                runtime.pane,
                runtime.session_generation,
                runtime.control_generation,
                runtime.control,
                session,
            )
            .map_err(|_| invalid_peer())?;
            let reduction = self
                .reducer
                .prepare_event(&OrganizationEvent::StartupReserved(reservation))
                .map_err(|_| invalid_peer())?;
            let Resolution::Completed(result) = reduction.resolution() else {
                return Err(invalid_peer());
            };
            let outcome = if result == &Outcome::Applied {
                Outcome::SessionStarted(session)
            } else {
                result.clone()
            };
            Ok((Some(reduction), outcome))
        }
    }
    fn runtime(&self, pane: PaneId) -> Result<PaneRuntimeRecord, ClientFailure> {
        self.reducer
            .snapshot()
            .runtime()
            .iter()
            .find(|record| record.pane == pane)
            .copied()
            .ok_or_else(stale)
    }
    fn startup_context(
        &self,
        pane: PaneId,
        client: ClientId,
        dispatch: Option<ConnectionBinding>,
    ) -> Result<spaceterm_core::startup_policy::StartupContext, ClientFailure> {
        let active = self.connections.get(&client).ok_or_else(stale)?;
        if dispatch.is_some_and(|binding| binding != active.binding) {
            return Err(stale());
        }
        let snapshot = self.reducer.snapshot();
        let pane_id = pane;
        let pane = snapshot
            .state()
            .panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .ok_or_else(stale)?;
        Ok(spaceterm_core::startup_policy::StartupContext {
            epoch: snapshot.epoch(),
            runtime: self.runtime(pane.id)?,
            tab: pane.tab,
            connection: active.binding,
            context: active
                .contexts
                .get(&pane.id)
                .copied()
                .unwrap_or(ContextGeneration::new(1)),
            admissions_open: true,
        })
    }
    fn control(
        &self,
        transition: Result<
            spaceterm_core::control_policy::ControlTransition,
            spaceterm_core::control_policy::ControlPolicyError,
        >,
    ) -> Result<(Option<PreparedReduction>, Outcome), ClientFailure> {
        match transition {
            Ok(transition) => {
                let event = OrganizationEvent::ControlChanged {
                    pane: transition.next().pane,
                    expected: transition.expected().control,
                    replacement: transition.next().control,
                };
                let reduction = self
                    .reducer
                    .prepare_event(&event)
                    .map_err(|_| invalid_peer())?;
                let outcome = transition
                    .granted()
                    .map(Outcome::ControlGranted)
                    .unwrap_or(Outcome::Applied);
                Ok((Some(reduction), outcome))
            }
            Err(_) => Ok((None, Outcome::Failed(OperationFailure::StaleAuthority))),
        }
    }
}
